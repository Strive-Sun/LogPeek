use super::{parse_mft_enum_buffer, MftRecord};
use anyhow::{bail, Context};
use std::ffi::c_void;
use std::io;
use std::mem::size_of;
use std::ptr::{null, null_mut};
use widestring::U16CString;
use windows_sys::Win32::Foundation::{CloseHandle, GENERIC_READ, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    OPEN_EXISTING,
};
use windows_sys::Win32::System::Ioctl::{
    FSCTL_ENUM_USN_DATA, FSCTL_QUERY_USN_JOURNAL, FSCTL_READ_USN_JOURNAL, MFT_ENUM_DATA_V0,
    READ_USN_JOURNAL_DATA_V0, USN_JOURNAL_DATA_V0,
};
use windows_sys::Win32::System::IO::DeviceIoControl;

const ENUM_BUFFER_SIZE: usize = 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MftEnumeration {
    pub batches: u64,
    pub records: u64,
    pub last_reference: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UsnJournalInfo {
    pub journal_id: u64,
    pub first_usn: i64,
    pub next_usn: i64,
    pub lowest_valid_usn: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UsnReadSummary {
    pub batches: u64,
    pub records: u64,
    pub next_usn: i64,
}

struct OwnedHandle(HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        if self.0 != INVALID_HANDLE_VALUE {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
}

pub fn enumerate_mft<F>(volume: char, mut on_batch: F) -> anyhow::Result<MftEnumeration>
where
    F: FnMut(Vec<MftRecord>) -> anyhow::Result<()>,
{
    let handle = open_volume(volume)?;
    let mut input = MFT_ENUM_DATA_V0 {
        StartFileReferenceNumber: 0,
        LowUsn: 0,
        HighUsn: i64::MAX,
    };
    let mut output = vec![0_u8; ENUM_BUFFER_SIZE];
    let mut summary = MftEnumeration {
        batches: 0,
        records: 0,
        last_reference: 0,
    };
    loop {
        let mut returned = 0_u32;
        let success = unsafe {
            DeviceIoControl(
                handle.0,
                FSCTL_ENUM_USN_DATA,
                (&input as *const MFT_ENUM_DATA_V0).cast::<c_void>(),
                size_of::<MFT_ENUM_DATA_V0>() as u32,
                output.as_mut_ptr().cast::<c_void>(),
                output.len() as u32,
                &mut returned,
                null_mut(),
            )
        };
        if success == 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(38) {
                break;
            }
            return Err(error).context("FSCTL_ENUM_USN_DATA 失败");
        }
        let (next_reference, records) = parse_mft_enum_buffer(&output[..returned as usize])?;
        if next_reference <= input.StartFileReferenceNumber {
            bail!("MFT 枚举 continuation 未前进");
        }
        summary.batches += 1;
        summary.records += records.len() as u64;
        summary.last_reference = next_reference;
        input.StartFileReferenceNumber = next_reference;
        on_batch(records)?;
    }
    Ok(summary)
}

pub fn query_usn_journal(volume: char) -> anyhow::Result<UsnJournalInfo> {
    let handle = open_volume(volume)?;
    let mut data = USN_JOURNAL_DATA_V0::default();
    let mut returned = 0_u32;
    let success = unsafe {
        DeviceIoControl(
            handle.0,
            FSCTL_QUERY_USN_JOURNAL,
            null(),
            0,
            (&mut data as *mut USN_JOURNAL_DATA_V0).cast::<c_void>(),
            size_of::<USN_JOURNAL_DATA_V0>() as u32,
            &mut returned,
            null_mut(),
        )
    };
    if success == 0 {
        return Err(io::Error::last_os_error()).context("FSCTL_QUERY_USN_JOURNAL 失败");
    }
    if returned < size_of::<USN_JOURNAL_DATA_V0>() as u32 {
        bail!("USN journal 信息长度不足");
    }
    Ok(UsnJournalInfo {
        journal_id: data.UsnJournalID,
        first_usn: data.FirstUsn,
        next_usn: data.NextUsn,
        lowest_valid_usn: data.LowestValidUsn,
    })
}

pub fn read_usn_journal<F>(
    volume: char,
    start_usn: i64,
    journal_id: u64,
    target_usn: i64,
    mut on_batch: F,
) -> anyhow::Result<UsnReadSummary>
where
    F: FnMut(Vec<MftRecord>) -> anyhow::Result<()>,
{
    let handle = open_volume(volume)?;
    let mut input = READ_USN_JOURNAL_DATA_V0 {
        StartUsn: start_usn,
        ReasonMask: u32::MAX,
        ReturnOnlyOnClose: 0,
        Timeout: 0,
        BytesToWaitFor: 0,
        UsnJournalID: journal_id,
    };
    let mut output = vec![0_u8; ENUM_BUFFER_SIZE];
    let mut summary = UsnReadSummary {
        batches: 0,
        records: 0,
        next_usn: start_usn,
    };
    while input.StartUsn < target_usn {
        let mut returned = 0_u32;
        let success = unsafe {
            DeviceIoControl(
                handle.0,
                FSCTL_READ_USN_JOURNAL,
                (&input as *const READ_USN_JOURNAL_DATA_V0).cast::<c_void>(),
                size_of::<READ_USN_JOURNAL_DATA_V0>() as u32,
                output.as_mut_ptr().cast::<c_void>(),
                output.len() as u32,
                &mut returned,
                null_mut(),
            )
        };
        if success == 0 {
            return Err(io::Error::last_os_error()).context("FSCTL_READ_USN_JOURNAL 失败");
        }
        let (next, records) = parse_mft_enum_buffer(&output[..returned as usize])?;
        let next = next as i64;
        if next <= input.StartUsn {
            bail!("USN journal continuation 未前进");
        }
        summary.batches += 1;
        summary.records += records.len() as u64;
        summary.next_usn = next;
        input.StartUsn = next;
        on_batch(records)?;
    }
    Ok(summary)
}

fn open_volume(volume: char) -> anyhow::Result<OwnedHandle> {
    if !volume.is_ascii_alphabetic() {
        bail!("卷必须是盘符");
    }
    let volume_path = U16CString::from_str(format!(r"\\.\{}:", volume.to_ascii_uppercase()))?;
    let handle = unsafe {
        CreateFileW(
            volume_path.as_ptr(),
            GENERIC_READ,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            null(),
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error()).context("无法打开 NTFS 卷；需要索引服务权限");
    }
    Ok(OwnedHandle(handle))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires an elevated process or installed index service"]
    fn enumerates_the_system_volume() {
        let started = std::time::Instant::now();
        let summary = enumerate_mft('C', |_| Ok(())).unwrap();
        eprintln!(
            "MFT enumeration: {summary:?}, elapsed={:?}",
            started.elapsed()
        );
        assert!(summary.records > 0);
    }
}
