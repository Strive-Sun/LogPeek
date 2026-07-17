//! 裸文本文件的 passthrough 读取器:把单个文件视为仅含一个条目的“归档”。

use super::{is_text_sample, ArchiveEntry, ArchiveReader, EntryReader};
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

    fn open_entry(&mut self, path: &str) -> anyhow::Result<EntryReader<'_>> {
        if path != self.name {
            anyhow::bail!("条目不存在: {path}");
        }
        let mut f = File::open(&self.path)?;
        f.seek(SeekFrom::Start(0))?;
        Ok(EntryReader::Seekable(Box::new(f)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::sync::atomic::{AtomicU64, Ordering};

    static FILE_SEQ: AtomicU64 = AtomicU64::new(1);

    struct Fixture {
        path: PathBuf,
    }

    impl Fixture {
        fn new(name: &str, bytes: &[u8]) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "logpeek-plain-test-{}-{}",
                std::process::id(),
                FILE_SEQ.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&dir).unwrap();
            let path = dir.join(name);
            std::fs::write(&path, bytes).unwrap();
            Self { path }
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            if let Some(parent) = self.path.parent() {
                let _ = std::fs::remove_dir_all(parent);
            }
        }
    }

    #[test]
    fn plain_text_is_a_single_seekable_entry() {
        let fixture = Fixture::new("sample.data", b"plain text\nsecond line");
        let mut reader = PlainReader::open(&fixture.path).unwrap();
        let entries = reader.entries().unwrap();
        assert_eq!(entries.len(), 1);
        assert!(entries[0].is_log);
        assert_eq!(entries[0].size, 22);

        let mut entry = reader.open_entry("sample.data").unwrap();
        assert!(entry.is_seekable());
        let mut content = String::new();
        entry.read_to_string(&mut content).unwrap();
        assert_eq!(content, "plain text\nsecond line");
    }

    #[test]
    fn binary_plain_entry_is_listed_but_not_marked_as_log() {
        let fixture = Fixture::new("sample.bin", &[0, 1, 2, 3]);
        let mut reader = PlainReader::open(&fixture.path).unwrap();
        let entries = reader.entries().unwrap();
        assert_eq!(entries.len(), 1);
        assert!(!entries[0].is_log);
        assert!(reader.open_entry("missing.bin").is_err());
    }
}
