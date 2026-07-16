//! 归档读取抽象:屏蔽 zip / 裸文本差异(见技术设计 4.3)。
//! `entries()` 只读元信息(zip 仅读中央目录,不解压);
//! `open_entry()` 返回可流式读取的解压流。

mod plain;
mod zip_reader;

use serde::Serialize;
use std::io::Read;
use std::path::Path;

pub use plain::PlainReader;
pub use zip_reader::ZipArchiveReader;

/// 归档内的一个条目(仅元信息)
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArchiveEntry {
    /// 包内路径
    pub path: String,
    /// 解压后大小(中央目录声明值,仅供展示,不作安全上限依据)
    pub size: u64,
    /// 是否日志/文本
    pub is_log: bool,
    /// 是否加密条目(M1 不支持)
    pub encrypted: bool,
}

/// 统一归档读取器
pub trait ArchiveReader: Send {
    /// 列出条目(不解压内容)
    fn entries(&mut self) -> anyhow::Result<Vec<ArchiveEntry>>;
    /// 打开某个条目,返回可流式读取的内容流
    fn open_entry(&mut self, path: &str) -> anyhow::Result<Box<dyn Read + Send + '_>>;
}

/// 按路径构造合适的读取器:zip → ZipArchiveReader,其余文本 → PlainReader
pub fn open_archive(path: &Path) -> anyhow::Result<Box<dyn ArchiveReader>> {
    if is_zip(path)? {
        Ok(Box::new(ZipArchiveReader::open(path)?))
    } else {
        Ok(Box::new(PlainReader::open(path)?))
    }
}

/// 按扩展名 + 文件头判定是否 zip
pub fn is_zip(path: &Path) -> anyhow::Result<bool> {
    use std::io::Read;
    let ext_zip = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("zip"))
        .unwrap_or(false);
    // 文件头 magic:PK\x03\x04 / PK\x05\x06(空归档)
    let mut f = std::fs::File::open(path)?;
    let mut magic = [0u8; 4];
    let n = f.read(&mut magic)?;
    let head_zip = n >= 4 && magic[0] == b'P' && magic[1] == b'K';
    Ok(ext_zip || head_zip)
}

const LOG_EXTS: &[&str] = &["log", "txt", "out", "err", "trace", "json", "csv"];

/// 判定条目是否为日志/文本:扩展名优先,其次内容采样
pub fn is_log_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    if let Some(ext) = Path::new(&lower).extension().and_then(|e| e.to_str()) {
        if LOG_EXTS.contains(&ext) {
            return true;
        }
        // 已知二进制扩展名直接判否
        const BIN_EXTS: &[&str] = &["bin", "png", "jpg", "gz", "zip", "exe", "dll", "so", "o"];
        if BIN_EXTS.contains(&ext) {
            return false;
        }
    }
    false
}

/// 内容采样判定是否文本:检查前若干字节是否含 NUL 或过多不可打印字符
pub fn is_text_sample(sample: &[u8]) -> bool {
    if sample.is_empty() {
        return true;
    }
    if sample.contains(&0) {
        return false;
    }
    let non_print = sample
        .iter()
        .filter(|&&b| b < 0x09 || (b > 0x0d && b < 0x20))
        .count();
    (non_print as f64) / (sample.len() as f64) < 0.10
}
