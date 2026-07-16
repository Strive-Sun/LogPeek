//! zip 归档读取器。`entries()` 仅读中央目录(不解压);
//! `open_entry()` 返回条目的解压流(Stored 直读 / Deflate 顺序解压)。

use super::{is_log_name, ArchiveEntry, ArchiveReader};
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use zip::ZipArchive;

pub struct ZipArchiveReader {
    path: PathBuf,
    archive: ZipArchive<BufReader<File>>,
}

impl ZipArchiveReader {
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        let file = File::open(path)?;
        let archive = ZipArchive::new(BufReader::new(file))?;
        Ok(Self {
            path: path.to_path_buf(),
            archive,
        })
    }
}

impl ArchiveReader for ZipArchiveReader {
    fn entries(&mut self) -> anyhow::Result<Vec<ArchiveEntry>> {
        let mut out = Vec::with_capacity(self.archive.len());
        for i in 0..self.archive.len() {
            // by_index 读取的是中央目录里的元信息,不解压内容
            let entry = self.archive.by_index(i)?;
            if entry.is_dir() {
                continue;
            }
            let name = entry.name().to_string();
            let encrypted = entry.encrypted();
            out.push(ArchiveEntry {
                path: name.clone(),
                size: entry.size(),
                is_log: !encrypted && is_log_name(&name),
                encrypted,
            });
        }
        Ok(out)
    }

    fn open_entry(&mut self, path: &str) -> anyhow::Result<Box<dyn Read + Send + '_>> {
        // 校验存在性与加密;为返回 'static 流,这里重新打开归档取该条目
        {
            let entry = self.archive.by_name(path)?;
            if entry.encrypted() {
                anyhow::bail!("条目已加密,M1 暂不支持: {path}");
            }
        }
        let file = File::open(&self.path)?;
        let mut archive = ZipArchive::new(BufReader::new(file))?;
        let name = path.to_string();
        // 用一个自持有的读取器包装:持有 archive 并按名定位
        let reader = OwnedZipEntry::new(archive_take(&mut archive, &name)?);
        Ok(Box::new(reader))
    }
}

/// 把某条目完整读入内存缓冲(M1 简化:命令层会流式建索引;
/// 此处对超大条目由上层按实际字节数熔断)。返回一个可读游标。
fn archive_take(archive: &mut ZipArchive<BufReader<File>>, name: &str) -> anyhow::Result<Vec<u8>> {
    let mut entry = archive.by_name(name)?;
    let mut buf = Vec::with_capacity(entry.size().min(64 * 1024 * 1024) as usize);
    entry.read_to_end(&mut buf)?;
    Ok(buf)
}

struct OwnedZipEntry {
    buf: std::io::Cursor<Vec<u8>>,
}
impl OwnedZipEntry {
    fn new(data: Vec<u8>) -> Self {
        Self {
            buf: std::io::Cursor::new(data),
        }
    }
}
impl Read for OwnedZipEntry {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        self.buf.read(out)
    }
}
