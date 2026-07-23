use super::{MftEnumeration, MftRecord, UsnJournalInfo, UsnReadSummary};
use anyhow::{anyhow, bail, Context};
use std::ffi::c_void;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::windows::io::{FromRawHandle, RawHandle};
use std::ptr::null_mut;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::sleep;
use std::time::{Duration, Instant};
use widestring::U16CString;
use windows_service::service::{ServiceAccess, ServiceState};
use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};
use windows_sys::Win32::Foundation::{
    CloseHandle, LocalFree, HANDLE, HLOCAL, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
use windows_sys::Win32::Storage::FileSystem::PIPE_ACCESS_DUPLEX;
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_BYTE,
    PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_WAIT,
};

pub const SERVICE_NAME: &str = "LogCrateIndex";
pub const PIPE_NAME: &str = r"\\.\pipe\LogCrate.Index.v2";
pub const PROTOCOL_VERSION: u16 = 2;
const MAGIC: [u8; 4] = *b"LCIX";
const HEADER_SIZE: usize = 12;
const MAX_FRAME_BODY: usize = 8 * 1024 * 1024;
const MAX_BATCH_RECORDS: usize = 131_072;
const PIPE_BUFFER_SIZE: u32 = 1024 * 1024;
const PIPE_SDDL: &str = "D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GRGW;;;IU)";

const REQUEST_HELLO: u16 = 1;
const REQUEST_ENUMERATE_MFT: u16 = 2;
const REQUEST_QUERY_USN: u16 = 3;
const REQUEST_READ_USN: u16 = 4;
const RESPONSE_HELLO: u16 = 100;
const RESPONSE_MFT_BATCH: u16 = 101;
const RESPONSE_COMPLETE: u16 = 102;
const RESPONSE_ERROR: u16 = 103;
const RESPONSE_USN_INFO: u16 = 104;
const RESPONSE_USN_BATCH: u16 = 105;
const RESPONSE_USN_COMPLETE: u16 = 106;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    Hello,
    EnumerateMft {
        volume: char,
    },
    QueryUsn {
        volume: char,
    },
    ReadUsn {
        volume: char,
        start_usn: i64,
        journal_id: u64,
        target_usn: i64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Response {
    Hello { protocol: u16 },
    MftBatch(Vec<MftRecord>),
    Complete(MftEnumeration),
    UsnInfo(UsnJournalInfo),
    UsnBatch(Vec<MftRecord>),
    UsnComplete(UsnReadSummary),
    Error { code: u32, message: String },
}

pub fn enumerate_mft_via_service<F>(volume: char, mut on_batch: F) -> anyhow::Result<MftEnumeration>
where
    F: FnMut(Vec<MftRecord>) -> anyhow::Result<()>,
{
    let mut pipe = connect_and_handshake()?;
    write_request(&mut pipe, &Request::EnumerateMft { volume })?;
    loop {
        match read_response(&mut pipe)? {
            Response::MftBatch(records) => on_batch(records)?,
            Response::Complete(summary) => return Ok(summary),
            Response::Error { code, message } => bail!("索引服务错误 {code}: {message}"),
            response => bail!("索引服务枚举响应无效: {response:?}"),
        }
    }
}

pub fn query_usn_via_service(volume: char) -> anyhow::Result<UsnJournalInfo> {
    let mut pipe = connect_and_handshake()?;
    write_request(&mut pipe, &Request::QueryUsn { volume })?;
    match read_response(&mut pipe)? {
        Response::UsnInfo(info) => Ok(info),
        Response::Error { code, message } => bail!("索引服务错误 {code}: {message}"),
        response => bail!("索引服务 USN 信息响应无效: {response:?}"),
    }
}

pub fn read_usn_via_service<F>(
    volume: char,
    start_usn: i64,
    journal_id: u64,
    target_usn: i64,
    mut on_batch: F,
) -> anyhow::Result<UsnReadSummary>
where
    F: FnMut(Vec<MftRecord>) -> anyhow::Result<()>,
{
    let mut pipe = connect_and_handshake()?;
    write_request(
        &mut pipe,
        &Request::ReadUsn {
            volume,
            start_usn,
            journal_id,
            target_usn,
        },
    )?;
    loop {
        match read_response(&mut pipe)? {
            Response::UsnBatch(records) => on_batch(records)?,
            Response::UsnComplete(summary) => return Ok(summary),
            Response::Error { code, message } => bail!("索引服务错误 {code}: {message}"),
            response => bail!("索引服务 USN 读取响应无效: {response:?}"),
        }
    }
}

fn connect_and_handshake() -> anyhow::Result<File> {
    let mut pipe = connect()?;
    write_request(&mut pipe, &Request::Hello)?;
    match read_response(&mut pipe)? {
        Response::Hello { protocol } if protocol == PROTOCOL_VERSION => Ok(pipe),
        Response::Error { code, message } => bail!("索引服务错误 {code}: {message}"),
        response => bail!("索引服务握手响应无效: {response:?}"),
    }
}

pub fn run_pipe_server(stop: &AtomicBool, once: bool) -> anyhow::Result<()> {
    loop {
        let pipe = create_server_pipe()?;
        let connected = unsafe { ConnectNamedPipe(pipe.0, null_mut()) };
        if connected == 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(535) {
                return Err(error).context("等待索引服务 named pipe 客户端失败");
            }
        }
        if stop.load(Ordering::SeqCst) {
            break;
        }
        let raw = pipe.0 as RawHandle;
        std::mem::forget(pipe);
        let mut file = unsafe { File::from_raw_handle(raw) };
        let served = serve_client(&mut file);
        let _ = file.flush();
        unsafe {
            DisconnectNamedPipe(raw as HANDLE);
        }
        drop(file);
        if let Err(error) = served {
            if once {
                return Err(error);
            }
        }
        if once {
            break;
        }
    }
    Ok(())
}

pub fn wake_pipe_server() {
    let _ = OpenOptions::new().read(true).write(true).open(PIPE_NAME);
}

fn serve_client(pipe: &mut File) -> anyhow::Result<()> {
    loop {
        let request = match read_request(pipe) {
            Ok(request) => request,
            Err(error) if is_disconnect(&error) => return Ok(()),
            Err(error) => return Err(error),
        };
        match request {
            Request::Hello => write_response(
                pipe,
                &Response::Hello {
                    protocol: PROTOCOL_VERSION,
                },
            )?,
            Request::EnumerateMft { volume } => {
                let result = super::enumerate_mft(volume, |records| {
                    write_response(pipe, &Response::MftBatch(records))
                });
                match result {
                    Ok(summary) => write_response(pipe, &Response::Complete(summary))?,
                    Err(error) => {
                        let code = error
                            .chain()
                            .find_map(|cause| cause.downcast_ref::<io::Error>())
                            .and_then(io::Error::raw_os_error)
                            .unwrap_or(1) as u32;
                        write_response(
                            pipe,
                            &Response::Error {
                                code,
                                message: format!("{error:#}"),
                            },
                        )?;
                    }
                }
            }
            Request::QueryUsn { volume } => match super::query_usn_journal(volume) {
                Ok(info) => write_response(pipe, &Response::UsnInfo(info))?,
                Err(error) => write_service_error(pipe, &error)?,
            },
            Request::ReadUsn {
                volume,
                start_usn,
                journal_id,
                target_usn,
            } => {
                let result =
                    super::read_usn_journal(volume, start_usn, journal_id, target_usn, |records| {
                        write_response(pipe, &Response::UsnBatch(records))
                    });
                match result {
                    Ok(summary) => write_response(pipe, &Response::UsnComplete(summary))?,
                    Err(error) => write_service_error(pipe, &error)?,
                }
            }
        }
    }
}

fn write_service_error(pipe: &mut File, error: &anyhow::Error) -> anyhow::Result<()> {
    let code = error
        .chain()
        .find_map(|cause| cause.downcast_ref::<io::Error>())
        .and_then(io::Error::raw_os_error)
        .unwrap_or(1) as u32;
    write_response(
        pipe,
        &Response::Error {
            code,
            message: format!("{error:#}"),
        },
    )
}

fn connect() -> anyhow::Result<File> {
    if let Ok(pipe) = open_pipe() {
        return Ok(pipe);
    }
    start_installed_service()?;
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(5) {
        match open_pipe() {
            Ok(pipe) => return Ok(pipe),
            Err(_) => sleep(Duration::from_millis(50)),
        }
    }
    open_pipe().context("LogCrate Index Service 已启动但 named pipe 未就绪")
}

fn open_pipe() -> io::Result<File> {
    OpenOptions::new().read(true).write(true).open(PIPE_NAME)
}

fn start_installed_service() -> anyhow::Result<()> {
    let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)
        .context("无法连接 Windows Service Control Manager")?;
    let service = manager
        .open_service(
            SERVICE_NAME,
            ServiceAccess::START | ServiceAccess::QUERY_STATUS,
        )
        .context("LogCrate Index Service 未安装或当前用户无权启动")?;
    let state = service.query_status()?.current_state;
    if !matches!(state, ServiceState::Running | ServiceState::StartPending) {
        service.start::<&str>(&[])?;
    }
    Ok(())
}

fn write_request(writer: &mut impl Write, request: &Request) -> anyhow::Result<()> {
    let (kind, body) = encode_request(request)?;
    write_frame(writer, kind, &body)
}

fn read_request(reader: &mut impl Read) -> anyhow::Result<Request> {
    let (kind, body) = read_frame(reader)?;
    decode_request(kind, &body)
}

fn write_response(writer: &mut impl Write, response: &Response) -> anyhow::Result<()> {
    let (kind, body) = encode_response(response)?;
    write_frame(writer, kind, &body)
}

fn read_response(reader: &mut impl Read) -> anyhow::Result<Response> {
    let (kind, body) = read_frame(reader)?;
    decode_response(kind, &body)
}

fn write_frame(writer: &mut impl Write, kind: u16, body: &[u8]) -> anyhow::Result<()> {
    if body.len() > MAX_FRAME_BODY {
        bail!("IPC 帧超过大小上限");
    }
    let mut header = [0_u8; HEADER_SIZE];
    header[..4].copy_from_slice(&MAGIC);
    header[4..6].copy_from_slice(&PROTOCOL_VERSION.to_le_bytes());
    header[6..8].copy_from_slice(&kind.to_le_bytes());
    header[8..12].copy_from_slice(&(body.len() as u32).to_le_bytes());
    writer.write_all(&header)?;
    writer.write_all(body)?;
    writer.flush()?;
    Ok(())
}

fn read_frame(reader: &mut impl Read) -> anyhow::Result<(u16, Vec<u8>)> {
    let mut header = [0_u8; HEADER_SIZE];
    reader.read_exact(&mut header)?;
    if header[..4] != MAGIC {
        bail!("IPC magic 无效");
    }
    let protocol = u16::from_le_bytes([header[4], header[5]]);
    if protocol != PROTOCOL_VERSION {
        bail!("IPC 协议版本不兼容: {protocol}");
    }
    let kind = u16::from_le_bytes([header[6], header[7]]);
    let body_len = u32::from_le_bytes(header[8..12].try_into().unwrap()) as usize;
    if body_len > MAX_FRAME_BODY {
        bail!("IPC 帧声明长度超过上限");
    }
    let mut body = vec![0_u8; body_len];
    reader.read_exact(&mut body)?;
    Ok((kind, body))
}

fn encode_request(request: &Request) -> anyhow::Result<(u16, Vec<u8>)> {
    match request {
        Request::Hello => Ok((REQUEST_HELLO, Vec::new())),
        Request::EnumerateMft { volume } if volume.is_ascii_alphabetic() => Ok((
            REQUEST_ENUMERATE_MFT,
            vec![volume.to_ascii_uppercase() as u8],
        )),
        Request::QueryUsn { volume } if volume.is_ascii_alphabetic() => {
            Ok((REQUEST_QUERY_USN, vec![volume.to_ascii_uppercase() as u8]))
        }
        Request::ReadUsn {
            volume,
            start_usn,
            journal_id,
            target_usn,
        } if volume.is_ascii_alphabetic() && start_usn <= target_usn => {
            let mut body = Vec::with_capacity(25);
            body.push(volume.to_ascii_uppercase() as u8);
            body.extend(start_usn.to_le_bytes());
            body.extend(journal_id.to_le_bytes());
            body.extend(target_usn.to_le_bytes());
            Ok((REQUEST_READ_USN, body))
        }
        Request::EnumerateMft { .. } | Request::QueryUsn { .. } | Request::ReadUsn { .. } => {
            bail!("USN 请求的卷或范围无效")
        }
    }
}

fn decode_request(kind: u16, body: &[u8]) -> anyhow::Result<Request> {
    match (kind, body) {
        (REQUEST_HELLO, []) => Ok(Request::Hello),
        (REQUEST_ENUMERATE_MFT, [volume]) if (*volume as char).is_ascii_alphabetic() => {
            Ok(Request::EnumerateMft {
                volume: (*volume as char).to_ascii_uppercase(),
            })
        }
        (REQUEST_QUERY_USN, [volume]) if (*volume as char).is_ascii_alphabetic() => {
            Ok(Request::QueryUsn {
                volume: (*volume as char).to_ascii_uppercase(),
            })
        }
        (REQUEST_READ_USN, body) if body.len() == 25 && (body[0] as char).is_ascii_alphabetic() => {
            let start_usn = i64::from_le_bytes(read_array(body, 1)?);
            let target_usn = i64::from_le_bytes(read_array(body, 17)?);
            if start_usn > target_usn {
                bail!("USN 读取范围无效");
            }
            Ok(Request::ReadUsn {
                volume: (body[0] as char).to_ascii_uppercase(),
                start_usn,
                journal_id: read_u64(body, 9)?,
                target_usn,
            })
        }
        _ => bail!("IPC 请求类型或长度无效"),
    }
}

fn encode_response(response: &Response) -> anyhow::Result<(u16, Vec<u8>)> {
    match response {
        Response::Hello { protocol } => Ok((RESPONSE_HELLO, protocol.to_le_bytes().to_vec())),
        Response::MftBatch(records) => Ok((RESPONSE_MFT_BATCH, encode_records(records)?)),
        Response::Complete(summary) => {
            let mut body = Vec::with_capacity(24);
            body.extend(summary.batches.to_le_bytes());
            body.extend(summary.records.to_le_bytes());
            body.extend(summary.last_reference.to_le_bytes());
            Ok((RESPONSE_COMPLETE, body))
        }
        Response::UsnInfo(info) => {
            let mut body = Vec::with_capacity(32);
            body.extend(info.journal_id.to_le_bytes());
            body.extend(info.first_usn.to_le_bytes());
            body.extend(info.next_usn.to_le_bytes());
            body.extend(info.lowest_valid_usn.to_le_bytes());
            Ok((RESPONSE_USN_INFO, body))
        }
        Response::UsnBatch(records) => Ok((RESPONSE_USN_BATCH, encode_records(records)?)),
        Response::UsnComplete(summary) => {
            let mut body = Vec::with_capacity(24);
            body.extend(summary.batches.to_le_bytes());
            body.extend(summary.records.to_le_bytes());
            body.extend(summary.next_usn.to_le_bytes());
            Ok((RESPONSE_USN_COMPLETE, body))
        }
        Response::Error { code, message } => {
            let message = message.as_bytes();
            let mut body = Vec::with_capacity(8 + message.len());
            body.extend(code.to_le_bytes());
            body.extend((message.len() as u32).to_le_bytes());
            body.extend(message);
            Ok((RESPONSE_ERROR, body))
        }
    }
}

fn decode_response(kind: u16, body: &[u8]) -> anyhow::Result<Response> {
    match kind {
        RESPONSE_HELLO if body.len() == 2 => Ok(Response::Hello {
            protocol: u16::from_le_bytes(body.try_into().unwrap()),
        }),
        RESPONSE_MFT_BATCH => Ok(Response::MftBatch(decode_records(body)?)),
        RESPONSE_COMPLETE if body.len() == 24 => Ok(Response::Complete(MftEnumeration {
            batches: read_u64(body, 0)?,
            records: read_u64(body, 8)?,
            last_reference: read_u64(body, 16)?,
        })),
        RESPONSE_USN_INFO if body.len() == 32 => Ok(Response::UsnInfo(UsnJournalInfo {
            journal_id: read_u64(body, 0)?,
            first_usn: i64::from_le_bytes(read_array(body, 8)?),
            next_usn: i64::from_le_bytes(read_array(body, 16)?),
            lowest_valid_usn: i64::from_le_bytes(read_array(body, 24)?),
        })),
        RESPONSE_USN_BATCH => Ok(Response::UsnBatch(decode_records(body)?)),
        RESPONSE_USN_COMPLETE if body.len() == 24 => Ok(Response::UsnComplete(UsnReadSummary {
            batches: read_u64(body, 0)?,
            records: read_u64(body, 8)?,
            next_usn: i64::from_le_bytes(read_array(body, 16)?),
        })),
        RESPONSE_ERROR if body.len() >= 8 => {
            let code = read_u32(body, 0)?;
            let length = read_u32(body, 4)? as usize;
            if length > MAX_FRAME_BODY || body.len() != 8 + length {
                bail!("IPC 错误消息长度无效");
            }
            Ok(Response::Error {
                code,
                message: String::from_utf8(body[8..].to_vec()).context("IPC 错误消息不是 UTF-8")?,
            })
        }
        _ => bail!("IPC 响应类型或长度无效"),
    }
}

fn encode_records(records: &[MftRecord]) -> anyhow::Result<Vec<u8>> {
    if records.len() > MAX_BATCH_RECORDS {
        bail!("MFT IPC 批次记录数超过上限");
    }
    let mut body = Vec::new();
    body.extend((records.len() as u32).to_le_bytes());
    for record in records {
        let name = record.name.as_bytes();
        if name.len() > u32::MAX as usize || name.len() > MAX_FRAME_BODY {
            bail!("MFT 文件名超过 IPC 上限");
        }
        body.extend(record.id.as_bytes());
        body.extend(record.parent_id.as_bytes());
        body.extend(record.usn.to_le_bytes());
        body.extend(record.attributes.to_le_bytes());
        body.extend(record.reason.to_le_bytes());
        body.extend((name.len() as u32).to_le_bytes());
        body.extend(name);
        if body.len() > MAX_FRAME_BODY {
            bail!("MFT IPC 批次超过帧大小上限");
        }
    }
    Ok(body)
}

fn decode_records(body: &[u8]) -> anyhow::Result<Vec<MftRecord>> {
    if body.len() < 4 {
        bail!("MFT IPC 批次缺少记录数");
    }
    let count = read_u32(body, 0)? as usize;
    if count > MAX_BATCH_RECORDS {
        bail!("MFT IPC 批次记录数超过上限");
    }
    let mut offset = 4;
    let mut records = Vec::with_capacity(count);
    for _ in 0..count {
        let id = super::FileId::from_bytes(read_array(body, offset)?);
        offset += 16;
        let parent_id = super::FileId::from_bytes(read_array(body, offset)?);
        offset += 16;
        let usn = i64::from_le_bytes(read_array(body, offset)?);
        offset += 8;
        let attributes = read_u32(body, offset)?;
        offset += 4;
        let reason = read_u32(body, offset)?;
        offset += 4;
        let name_len = read_u32(body, offset)? as usize;
        offset += 4;
        let end = offset
            .checked_add(name_len)
            .filter(|end| *end <= body.len())
            .ok_or_else(|| anyhow!("MFT IPC 文件名越过批次边界"))?;
        let name =
            String::from_utf8(body[offset..end].to_vec()).context("MFT IPC 文件名不是 UTF-8")?;
        offset = end;
        records.push(MftRecord {
            id,
            parent_id,
            name,
            attributes,
            reason,
            usn,
        });
    }
    if offset != body.len() {
        bail!("MFT IPC 批次包含尾随数据");
    }
    Ok(records)
}

struct OwnedPipe(HANDLE);

impl Drop for OwnedPipe {
    fn drop(&mut self) {
        if self.0 != INVALID_HANDLE_VALUE {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
}

struct OwnedSecurityDescriptor(*mut c_void);

impl Drop for OwnedSecurityDescriptor {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                LocalFree(self.0 as HLOCAL);
            }
        }
    }
}

fn create_server_pipe() -> anyhow::Result<OwnedPipe> {
    let sddl = U16CString::from_str(PIPE_SDDL)?;
    let mut descriptor = null_mut::<c_void>();
    let converted = unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &mut descriptor,
            null_mut(),
        )
    };
    if converted == 0 {
        return Err(io::Error::last_os_error()).context("创建索引服务 pipe ACL 失败");
    }
    let descriptor = OwnedSecurityDescriptor(descriptor);
    let security = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor.0,
        bInheritHandle: 0,
    };
    let name = U16CString::from_str(PIPE_NAME)?;
    let handle = unsafe {
        CreateNamedPipeW(
            name.as_ptr(),
            PIPE_ACCESS_DUPLEX,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            4,
            PIPE_BUFFER_SIZE,
            PIPE_BUFFER_SIZE,
            0,
            &security,
        )
    };
    drop(descriptor);
    if handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error()).context("创建索引服务 named pipe 失败");
    }
    Ok(OwnedPipe(handle))
}

fn read_u32(bytes: &[u8], offset: usize) -> anyhow::Result<u32> {
    Ok(u32::from_le_bytes(read_array(bytes, offset)?))
}

fn read_u64(bytes: &[u8], offset: usize) -> anyhow::Result<u64> {
    Ok(u64::from_le_bytes(read_array(bytes, offset)?))
}

fn read_array<const N: usize>(bytes: &[u8], offset: usize) -> anyhow::Result<[u8; N]> {
    bytes
        .get(offset..offset + N)
        .ok_or_else(|| anyhow!("IPC 字段越过边界"))?
        .try_into()
        .map_err(|_| anyhow!("IPC 字段长度无效"))
}

fn is_disconnect(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<io::Error>()
            .and_then(io::Error::raw_os_error)
            .is_some_and(|code| matches!(code, 109 | 232 | 233))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ntfs::FileId;
    use std::io::Cursor;

    fn record() -> MftRecord {
        MftRecord {
            id: FileId::from_u64(10),
            parent_id: FileId::from_u64(5),
            name: "调试-debug.log".into(),
            attributes: 32,
            reason: 0,
            usn: 99,
        }
    }

    #[test]
    fn protocol_round_trips_requests_and_bounded_batches() {
        let requests = [
            Request::Hello,
            Request::EnumerateMft { volume: 'd' },
            Request::QueryUsn { volume: 'D' },
            Request::ReadUsn {
                volume: 'D',
                start_usn: 10,
                journal_id: 20,
                target_usn: 30,
            },
        ];
        for request in requests {
            let mut bytes = Vec::new();
            write_request(&mut bytes, &request).unwrap();
            let decoded = read_request(&mut Cursor::new(bytes)).unwrap();
            let expected = match request {
                Request::EnumerateMft { .. } => Request::EnumerateMft { volume: 'D' },
                other => other,
            };
            assert_eq!(decoded, expected);
        }

        let responses = [
            Response::Hello {
                protocol: PROTOCOL_VERSION,
            },
            Response::MftBatch(vec![record()]),
            Response::Complete(MftEnumeration {
                batches: 2,
                records: 3,
                last_reference: 4,
            }),
            Response::UsnInfo(UsnJournalInfo {
                journal_id: 5,
                first_usn: 6,
                next_usn: 7,
                lowest_valid_usn: 4,
            }),
            Response::UsnBatch(vec![record()]),
            Response::UsnComplete(UsnReadSummary {
                batches: 8,
                records: 9,
                next_usn: 10,
            }),
            Response::Error {
                code: 5,
                message: "拒绝访问".into(),
            },
        ];
        for response in responses {
            let mut bytes = Vec::new();
            write_response(&mut bytes, &response).unwrap();
            assert_eq!(read_response(&mut Cursor::new(bytes)).unwrap(), response);
        }
    }

    #[test]
    fn protocol_rejects_oversized_unknown_and_trailing_data() {
        let mut oversized = Vec::new();
        oversized.extend(MAGIC);
        oversized.extend(PROTOCOL_VERSION.to_le_bytes());
        oversized.extend(REQUEST_HELLO.to_le_bytes());
        oversized.extend(((MAX_FRAME_BODY + 1) as u32).to_le_bytes());
        assert!(read_request(&mut Cursor::new(oversized)).is_err());

        let mut unknown = Vec::new();
        write_frame(&mut unknown, 999, &[]).unwrap();
        assert!(read_request(&mut Cursor::new(unknown)).is_err());

        let mut records = encode_records(&[record()]).unwrap();
        records.push(0);
        assert!(decode_records(&records).is_err());
    }
}
