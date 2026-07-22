//! Read-only RAR4/RAR5 support using the official UnRAR callback API.
//!
//! UnRAR source code may be used in any software to handle RAR archives
//! without limitations free of charge, but cannot be used to develop a RAR
//! compatible archiver or re-create the proprietary compression algorithm.
//! The complete required license text is shipped in resources/licenses/unrar.txt.

// unrar_sys declares output structs as `*const` even though UnRAR mutates them.
// Mutable references are intentional and match the native C API contract.
#![allow(clippy::unnecessary_mut_passed)]

use super::channel_reader::{send_error, ChannelReader, StreamMessage};
use super::{ArchiveEntry, ArchiveLimits, ArchiveReader, EntryReader};
#[cfg(any(target_os = "linux", target_os = "netbsd"))]
use std::ffi::CString;
use std::path::{Path, PathBuf};
use std::ptr;
use std::sync::mpsc::{sync_channel, SyncSender};
use std::time::Instant;

pub struct RarArchiveReader {
    path: PathBuf,
    limits: ArchiveLimits,
}

impl RarArchiveReader {
    #[cfg(test)]
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        Self::open_with_limits(path, ArchiveLimits::default())
    }

    pub fn open_with_limits(path: &Path, limits: ArchiveLimits) -> anyhow::Result<Self> {
        if !path.is_file() {
            anyhow::bail!("RAR 归档不存在: {}", path.display());
        }
        Ok(Self {
            path: path.to_path_buf(),
            limits,
        })
    }
}

struct RarHandle(*const unrar_sys::Handle);

impl Drop for RarHandle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: the handle was returned by RAROpenArchiveEx and is closed once here.
            unsafe { unrar_sys::RARCloseArchive(self.0) };
        }
    }
}

enum RarPath {
    #[cfg(any(target_os = "linux", target_os = "netbsd"))]
    Narrow(CString),
    #[cfg(not(any(target_os = "linux", target_os = "netbsd")))]
    Wide(widestring::WideCString),
}

impl RarPath {
    fn new(path: &Path) -> anyhow::Result<Self> {
        #[cfg(any(target_os = "linux", target_os = "netbsd"))]
        {
            use std::os::unix::ffi::OsStrExt;
            Ok(Self::Narrow(CString::new(path.as_os_str().as_bytes())?))
        }
        #[cfg(not(any(target_os = "linux", target_os = "netbsd")))]
        {
            Ok(Self::Wide(widestring::WideCString::from_os_str(
                path.as_os_str(),
            )?))
        }
    }

    fn open_data(&self, mode: u32) -> unrar_sys::OpenArchiveDataEx {
        match self {
            #[cfg(any(target_os = "linux", target_os = "netbsd"))]
            Self::Narrow(path) => unrar_sys::OpenArchiveDataEx::new(path.as_ptr(), mode),
            #[cfg(not(any(target_os = "linux", target_os = "netbsd")))]
            // WideCString uses the platform wchar width, but its Unix code units are
            // unsigned while libc::wchar_t (and therefore UnRAR's WCHAR) is signed.
            Self::Wide(path) => {
                unrar_sys::OpenArchiveDataEx::new(path.as_ptr().cast::<unrar_sys::WCHAR>(), mode)
            }
        }
    }
}

fn open_handle(path: &Path, mode: u32) -> anyhow::Result<(RarHandle, u32)> {
    let path = RarPath::new(path)?;
    let mut data = path.open_data(mode);
    // SAFETY: data and its path buffers remain alive for the duration of this call.
    let handle = unsafe { unrar_sys::RAROpenArchiveEx(&mut data) };
    if handle.is_null() || data.open_result != unrar_sys::ERAR_SUCCESS as u32 {
        return Err(rar_error(data.open_result as i32, "打开 RAR 归档"));
    }
    if data.flags & unrar_sys::ROADF_ENCHEADERS != 0 {
        // SAFETY: handle is valid and has not been wrapped yet.
        unsafe { unrar_sys::RARCloseArchive(handle) };
        anyhow::bail!("RAR 归档头已加密，暂不支持密码输入");
    }
    if data.flags & unrar_sys::ROADF_VOLUME != 0 {
        // SAFETY: handle is valid and has not been wrapped yet.
        unsafe { unrar_sys::RARCloseArchive(handle) };
        anyhow::bail!("RAR 分卷归档暂不支持");
    }
    Ok((RarHandle(handle), data.flags))
}

fn rar_error(code: i32, action: &str) -> anyhow::Error {
    let detail = match code {
        unrar_sys::ERAR_MISSING_PASSWORD | unrar_sys::ERAR_BAD_PASSWORD => "归档已加密或密码缺失",
        unrar_sys::ERAR_BAD_DATA | unrar_sys::ERAR_BAD_ARCHIVE => "归档已损坏",
        unrar_sys::ERAR_UNKNOWN_FORMAT => "未知 RAR 格式或算法",
        unrar_sys::ERAR_EOPEN => "无法打开归档或后续分卷",
        unrar_sys::ERAR_EREAD => "读取归档失败",
        unrar_sys::ERAR_NO_MEMORY => "解码内存不足",
        _ => "UnRAR 返回错误",
    };
    anyhow::anyhow!("{action}失败: {detail} (code {code})")
}

fn header_name(header: &unrar_sys::HeaderDataEx) -> String {
    let length = header
        .filename_w
        .iter()
        .position(|value| *value == 0)
        .unwrap_or(header.filename_w.len());
    if length > 0 {
        #[cfg(windows)]
        {
            return String::from_utf16_lossy(&header.filename_w[..length]);
        }
        #[cfg(not(windows))]
        {
            return header.filename_w[..length]
                .iter()
                .filter_map(|value| char::from_u32(*value as u32))
                .collect();
        }
    }
    let length = header
        .filename
        .iter()
        .position(|value| *value == 0)
        .unwrap_or(header.filename.len());
    let bytes = header.filename[..length]
        .iter()
        .map(|value| *value as u8)
        .collect::<Vec<_>>();
    String::from_utf8_lossy(&bytes).into_owned()
}

fn header_size(header: &unrar_sys::HeaderDataEx) -> u64 {
    (u64::from(header.unp_size_high) << 32) | u64::from(header.unp_size)
}

fn is_special(header: &unrar_sys::HeaderDataEx) -> bool {
    header.flags & unrar_sys::RHDF_DIRECTORY != 0 || header.redir_type != 0
}

impl ArchiveReader for RarArchiveReader {
    fn entries(&mut self) -> anyhow::Result<Vec<ArchiveEntry>> {
        let (handle, _) = open_handle(&self.path, unrar_sys::RAR_OM_LIST)?;
        let mut entries = Vec::new();
        let started = Instant::now();
        loop {
            super::ensure_scan_time(started, self.limits)?;
            let mut header = unrar_sys::HeaderDataEx::default();
            // SAFETY: handle is valid and header points to writable initialized storage.
            let result = unsafe { unrar_sys::RARReadHeaderEx(handle.0, &mut header) };
            if result == unrar_sys::ERAR_END_ARCHIVE {
                break;
            }
            if result != unrar_sys::ERAR_SUCCESS {
                return Err(rar_error(result, "读取 RAR 条目"));
            }
            let name = header_name(&header).replace('\\', "/");
            let split =
                header.flags & (unrar_sys::RHDF_SPLITBEFORE | unrar_sys::RHDF_SPLITAFTER) != 0;
            if split {
                anyhow::bail!("RAR 分卷归档暂不支持");
            }
            if !is_special(&header) && super::is_safe_entry_name(&name, self.limits.max_path_bytes)
            {
                if entries.len() >= self.limits.max_entries {
                    anyhow::bail!("归档条目数量超过安全上限");
                }
                entries.push(ArchiveEntry::new(
                    name,
                    header_size(&header),
                    header.flags & unrar_sys::RHDF_ENCRYPTED != 0,
                ));
            }
            // SAFETY: skipping advances the valid handle to the next header.
            let result = unsafe {
                unrar_sys::RARProcessFile(handle.0, unrar_sys::RAR_SKIP, ptr::null(), ptr::null())
            };
            if result != unrar_sys::ERAR_SUCCESS {
                return Err(rar_error(result, "跳过 RAR 条目"));
            }
        }
        super::ensure_scan_time(started, self.limits)?;
        Ok(entries)
    }

    fn open_entry(&mut self, path: &str) -> anyhow::Result<EntryReader<'_>> {
        let source = self.path.clone();
        let target = path.to_string();
        let max_decoded_bytes = self.limits.max_decoded_bytes;
        let (sender, receiver) = sync_channel(2);
        std::thread::spawn(move || {
            if let Err(error) = stream_entry(&source, &target, max_decoded_bytes, &sender) {
                send_error(&sender, error);
            }
        });
        Ok(EntryReader::Sequential(Box::new(ChannelReader::new(
            receiver,
        ))))
    }
}

struct CallbackState {
    sender: SyncSender<StreamMessage>,
    decoded: u64,
    error: Option<String>,
    max_decoded_bytes: u64,
}

extern "C" fn rar_callback(
    message: unrar_sys::UINT,
    user_data: unrar_sys::LPARAM,
    param1: unrar_sys::LPARAM,
    param2: unrar_sys::LPARAM,
) -> i32 {
    // SAFETY: user_data points to CallbackState for the synchronous process call.
    let state = unsafe { &mut *(user_data as *mut CallbackState) };
    match message {
        unrar_sys::UCM_PROCESSDATA => {
            let length = match usize::try_from(param2) {
                Ok(length) => length,
                Err(_) => {
                    state.error = Some("RAR 解码数据块大小无效".into());
                    return -1;
                }
            };
            // SAFETY: UnRAR guarantees the callback buffer is valid for `length`
            // bytes during this callback; data is copied before returning.
            let bytes = unsafe { std::slice::from_raw_parts(param1 as *const u8, length) };
            state.decoded = state.decoded.saturating_add(length as u64);
            if state.decoded > state.max_decoded_bytes {
                state.error = Some(format!(
                    "RAR 实际解码内容超过 {} 字节安全上限",
                    state.max_decoded_bytes
                ));
                return -1;
            }
            for chunk in bytes.chunks(64 * 1024) {
                if state
                    .sender
                    .send(StreamMessage::Data(chunk.to_vec()))
                    .is_err()
                {
                    state.error = Some("RAR 读取已取消".into());
                    return -1;
                }
            }
            1
        }
        unrar_sys::UCM_NEEDPASSWORD | unrar_sys::UCM_NEEDPASSWORDW => {
            state.error = Some("RAR 归档已加密，暂不支持密码输入".into());
            -1
        }
        unrar_sys::UCM_CHANGEVOLUME | unrar_sys::UCM_CHANGEVOLUMEW => {
            state.error = Some("RAR 分卷归档暂不支持".into());
            -1
        }
        _ => -1,
    }
}

fn stream_entry(
    source: &Path,
    target: &str,
    max_decoded_bytes: u64,
    sender: &SyncSender<StreamMessage>,
) -> anyhow::Result<()> {
    let (handle, _) = open_handle(source, unrar_sys::RAR_OM_EXTRACT)?;
    loop {
        let mut header = unrar_sys::HeaderDataEx::default();
        // SAFETY: handle is valid and header points to writable initialized storage.
        let result = unsafe { unrar_sys::RARReadHeaderEx(handle.0, &mut header) };
        if result == unrar_sys::ERAR_END_ARCHIVE {
            anyhow::bail!("条目不存在: {target}");
        }
        if result != unrar_sys::ERAR_SUCCESS {
            return Err(rar_error(result, "读取 RAR 条目"));
        }
        let name = header_name(&header).replace('\\', "/");
        if name == target {
            if header.flags & unrar_sys::RHDF_ENCRYPTED != 0 {
                anyhow::bail!("RAR 归档条目已加密，暂不支持密码输入");
            }
            if header.flags & (unrar_sys::RHDF_SPLITBEFORE | unrar_sys::RHDF_SPLITAFTER) != 0 {
                anyhow::bail!("RAR 分卷归档暂不支持");
            }
            if is_special(&header) {
                anyhow::bail!("RAR 特殊条目不可打开: {target}");
            }
            let mut state = Box::new(CallbackState {
                sender: sender.clone(),
                decoded: 0,
                error: None,
                max_decoded_bytes,
            });
            // SAFETY: callback and state remain valid for the synchronous process call.
            unsafe {
                unrar_sys::RARSetCallback(
                    handle.0,
                    Some(rar_callback),
                    (&mut *state as *mut CallbackState) as unrar_sys::LPARAM,
                )
            };
            // RAR_TEST decodes and verifies but never writes a user-visible file.
            // SAFETY: the callback owns all output handling and destination pointers are null.
            let result = unsafe {
                unrar_sys::RARProcessFile(handle.0, unrar_sys::RAR_TEST, ptr::null(), ptr::null())
            };
            if let Some(error) = state.error.take() {
                anyhow::bail!(error);
            }
            if result != unrar_sys::ERAR_SUCCESS {
                return Err(rar_error(result, "解码 RAR 条目"));
            }
            return Ok(());
        }
        // SAFETY: skipping advances the valid handle to the next header.
        let result = unsafe {
            unrar_sys::RARProcessFile(handle.0, unrar_sys::RAR_SKIP, ptr::null(), ptr::null())
        };
        if result != unrar_sys::ERAR_SUCCESS {
            return Err(rar_error(result, "跳过 RAR 条目"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::sync::atomic::{AtomicU64, Ordering};

    static FIXTURE_SEQ: AtomicU64 = AtomicU64::new(1);

    struct Fixture(PathBuf);

    impl Fixture {
        fn write(name: &str, bytes: &[u8]) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "logcrate-rar-test-{}-{}",
                std::process::id(),
                FIXTURE_SEQ.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&dir).unwrap();
            let path = dir.join(name);
            std::fs::write(&path, bytes).unwrap();
            Self(path)
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            if let Some(parent) = self.0.parent() {
                let _ = std::fs::remove_dir_all(parent);
            }
        }
    }

    #[test]
    fn rar4_entry_is_listed_and_streamed_without_extraction() {
        // RARLab-compatible fixture from unrar.rs, containing VERSION = "unrar-0.4.0".
        let fixture = Fixture::write(
            "version.rar",
            &[
                0x52, 0x61, 0x72, 0x21, 0x1a, 0x07, 0x00, 0xcf, 0x90, 0x73, 0x00, 0x00, 0x0d, 0x00,
                0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x0f, 0x0c, 0x74, 0x20, 0x80, 0x27, 0x00, 0x15,
                0x00, 0x00, 0x00, 0x0b, 0x00, 0x00, 0x00, 0x03, 0x45, 0xf3, 0x7d, 0xc6, 0xa4, 0x8a,
                0x07, 0x47, 0x1d, 0x33, 0x07, 0x00, 0xa4, 0x81, 0x00, 0x00, 0x56, 0x45, 0x52, 0x53,
                0x49, 0x4f, 0x4e, 0x0c, 0x00, 0x8f, 0xec, 0x8a, 0x45, 0xcc, 0x23, 0xc8, 0x48, 0x08,
                0x83, 0x62, 0xfe, 0x5f, 0xdd, 0x5c, 0x53, 0x88, 0xf0, 0x72, 0xc4, 0x3d, 0x7b, 0x00,
                0x40, 0x07, 0x00,
            ],
        );
        let mut reader = RarArchiveReader::open(&fixture.0).unwrap();
        let entries = reader.entries().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path, "VERSION");
        let mut content = String::new();
        reader
            .open_entry("VERSION")
            .unwrap()
            .read_to_string(&mut content)
            .unwrap();
        assert_eq!(content, "unrar-0.4.0");
        assert!(!fixture.0.with_file_name("VERSION").exists());
    }

    #[test]
    fn rar5_unicode_header_is_recognized() {
        let fixture = Fixture::write(
            "unicode.rar",
            &[
                0x52, 0x61, 0x72, 0x21, 0x1a, 0x07, 0x01, 0x00, 0x33, 0x92, 0xb5, 0xe5, 0x0a, 0x01,
                0x05, 0x06, 0x00, 0x05, 0x01, 0x01, 0x80, 0x80, 0x00, 0x37, 0x3c, 0xcb, 0xef, 0x1f,
                0x02, 0x02, 0x80, 0x00, 0x06, 0x80, 0x00, 0xa4, 0x83, 0x02, 0x22, 0x68, 0x55, 0x5b,
                0x00, 0x00, 0x00, 0x00, 0x80, 0x00, 0x01, 0x09, 0x66, 0x6f, 0x6f, 0xe2, 0x80, 0x94,
                0x62, 0x61, 0x72, 0x1d, 0x77, 0x56, 0x51, 0x03, 0x05, 0x04, 0x00,
            ],
        );
        let mut reader = RarArchiveReader::open(&fixture.0).unwrap();
        let entries = reader.entries().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path, "foo—bar");
    }
}
