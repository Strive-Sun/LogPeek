//! 裸文本文件的 passthrough 读取器:把单个文件视为仅含一个条目的“归档”。

use super::{is_text_sample, ArchiveEntry, ArchiveReader};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

pub struct PlainReader {
    path: PathBuf,
    name: String,
    size: u64,
}

impl PlainReader {
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        let meta = std::fs::metadata(path)?;
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("file")
            .to_string();
        Ok(Self {
            path: path.to_path_buf(),
            name,
            size: meta.len(),
        })
    }

    fn sample_is_text(&self) -> bool {
        let mut f = match File::open(&self.path) {
            Ok(f) => f,
            Err(_) => return false,
        };
        let mut buf = [0u8; 4096];
        let n = f.read(&mut buf).unwrap_or(0);
        is_text_sample(&buf[..n])
    }
}

impl ArchiveReader for PlainReader {
    fn entries(&mut self) -> anyhow::Result<Vec<ArchiveEntry>> {
        Ok(vec![ArchiveEntry {
            path: self.name.clone(),
            size: self.size,
            is_log: super::is_log_name(&self.name) || self.sample_is_text(),
            encrypted: false,
        }])
    }

    fn open_entry(&mut self, path: &str) -> anyhow::Result<Box<dyn Read + Send + '_>> {
        if path != self.name {
            anyhow::bail!("条目不存在: {path}");
        }
        let mut f = File::open(&self.path)?;
        f.seek(SeekFrom::Start(0))?;
        Ok(Box::new(f))
    }
}
