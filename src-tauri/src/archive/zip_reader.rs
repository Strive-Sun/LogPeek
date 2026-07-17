//! zip 归档读取器。`entries()` 仅读中央目录(不解压);
//! `open_entry()` 返回条目的解压流(Stored 直读 / Deflate 顺序解压)。

use super::{is_log_name, ArchiveEntry, ArchiveReader, EntryReader};
use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::Path;
use zip::ZipArchive;

pub struct ZipArchiveReader {
    archive: ZipArchive<BufReader<File>>,
}

impl ZipArchiveReader {
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        let file = File::open(path)?;
        let archive = ZipArchive::new(BufReader::new(file))?;
        Ok(Self { archive })
    }
}

impl ArchiveReader for ZipArchiveReader {
    fn entries(&mut self) -> anyhow::Result<Vec<ArchiveEntry>> {
        let mut out = Vec::with_capacity(self.archive.len());
        for i in 0..self.archive.len() {
            // by_index 读取的是中央目录里的元信息,不解压内容
            let entry = self.archive.by_index_raw(i)?;
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

    fn open_entry(&mut self, path: &str) -> anyhow::Result<EntryReader<'_>> {
        let stored = {
            let index = self
                .archive
                .index_for_name(path)
                .ok_or(zip::result::ZipError::FileNotFound)?;
            let entry = self.archive.by_index_raw(index)?;
            if entry.encrypted() {
                anyhow::bail!("条目已加密,M1 暂不支持: {path}");
            }
            entry.compression() == zip::CompressionMethod::Stored
        };
        if stored {
            Ok(EntryReader::Seekable(Box::new(RelativeSeek::new(
                self.archive.by_name_seek(path)?,
            )?)))
        } else {
            Ok(EntryReader::Sequential(Box::new(
                self.archive.by_name(path)?,
            )))
        }
    }
}

/// zip 2.x bounds Stored reads correctly, but reports absolute archive offsets from `seek`.
/// Normalize them to entry-relative offsets for the ArchiveReader contract.
struct RelativeSeek<R> {
    inner: R,
    base: u64,
}

impl<R: Read + Seek> RelativeSeek<R> {
    fn new(mut inner: R) -> std::io::Result<Self> {
        let base = inner.stream_position()?;
        Ok(Self { inner, base })
    }
}

impl<R: Read> Read for RelativeSeek<R> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        self.inner.read(buffer)
    }
}

impl<R: Seek> Seek for RelativeSeek<R> {
    fn seek(&mut self, position: SeekFrom) -> std::io::Result<u64> {
        self.inner
            .seek(position)?
            .checked_sub(self.base)
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::Other,
                    "zip entry seek escaped its lower boundary",
                )
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, SeekFrom, Write};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use zip::write::SimpleFileOptions;

    static FIXTURE_SEQ: AtomicU64 = AtomicU64::new(1);

    struct Fixture {
        path: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "logpeek-zip-test-{}-{}.zip",
                std::process::id(),
                FIXTURE_SEQ.fetch_add(1, Ordering::Relaxed)
            ));
            let file = File::create(&path).unwrap();
            let mut writer = zip::ZipWriter::new(file);
            writer
                .start_file(
                    "stored.log",
                    SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored),
                )
                .unwrap();
            writer.write_all(b"stored-line-1\nstored-line-2\n").unwrap();
            writer
                .start_file(
                    "deflated.log",
                    SimpleFileOptions::default()
                        .compression_method(zip::CompressionMethod::Deflated),
                )
                .unwrap();
            writer.write_all(&vec![b'x'; 256 * 1024]).unwrap();
            writer
                .start_file("image.bin", SimpleFileOptions::default())
                .unwrap();
            writer.write_all(&[0, 1, 2, 3]).unwrap();
            writer.finish().unwrap();
            Self { path }
        }

        fn encrypted() -> Self {
            let path = std::env::temp_dir().join(format!(
                "logpeek-zip-test-{}-{}.zip",
                std::process::id(),
                FIXTURE_SEQ.fetch_add(1, Ordering::Relaxed)
            ));
            let file = File::create(&path).unwrap();
            let mut writer = zip::ZipWriter::new(file);
            writer
                .start_file("secret.log", SimpleFileOptions::default())
                .unwrap();
            writer.write_all(b"secret").unwrap();
            writer.finish().unwrap();

            // Mark the test entry encrypted in both headers. The reader must reject it
            // from metadata before attempting to decode the intentionally plain payload.
            let mut bytes = std::fs::read(&path).unwrap();
            let local = bytes
                .windows(4)
                .position(|window| window == b"PK\x03\x04")
                .unwrap();
            bytes[local + 6] |= 1;
            let central = bytes
                .windows(4)
                .position(|window| window == b"PK\x01\x02")
                .unwrap();
            bytes[central + 8] |= 1;
            std::fs::write(&path, bytes).unwrap();
            Self { path }
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.path);
        }
    }

    #[test]
    fn stored_entry_is_seekable_and_bounded() {
        let fixture = Fixture::new();
        let mut archive = ZipArchiveReader::open(&fixture.path).unwrap();
        let mut entry = archive.open_entry("stored.log").unwrap();
        assert!(entry.is_seekable());
        assert_eq!(entry.seek(SeekFrom::Start(7)).unwrap(), 7);
        let mut tail = String::new();
        entry.read_to_string(&mut tail).unwrap();
        assert_eq!(tail, "line-1\nstored-line-2\n");
        assert_eq!(entry.seek(SeekFrom::End(1)).unwrap(), 28);
        let mut byte = [0u8; 1];
        assert_eq!(entry.read(&mut byte).unwrap(), 0);
    }

    #[test]
    fn listing_reads_metadata_without_creating_extracted_files() {
        let fixture = Fixture::new();
        let archive_size = std::fs::metadata(&fixture.path).unwrap().len();
        let mut archive = ZipArchiveReader::open(&fixture.path).unwrap();
        let entries = archive.entries().unwrap();
        assert_eq!(entries.len(), 3);
        assert!(entries
            .iter()
            .any(|entry| entry.path == "stored.log" && entry.is_log));
        assert!(entries
            .iter()
            .any(|entry| entry.path == "deflated.log" && entry.is_log));
        assert!(entries
            .iter()
            .any(|entry| entry.path == "image.bin" && !entry.is_log));
        assert_eq!(
            std::fs::metadata(&fixture.path).unwrap().len(),
            archive_size
        );
        assert!(!fixture.path.with_file_name("stored.log").exists());
        assert!(!fixture.path.with_file_name("deflated.log").exists());
    }

    #[test]
    fn deflated_entry_streams_without_seek_capability() {
        let fixture = Fixture::new();
        let mut archive = ZipArchiveReader::open(&fixture.path).unwrap();
        let mut entry = archive.open_entry("deflated.log").unwrap();
        assert!(!entry.is_seekable());
        assert_eq!(
            entry.seek(SeekFrom::Start(0)).unwrap_err().kind(),
            std::io::ErrorKind::Unsupported
        );
        let mut first = [0u8; 4096];
        entry.read_exact(&mut first).unwrap();
        assert!(first.iter().all(|byte| *byte == b'x'));
    }

    #[test]
    fn encrypted_entry_returns_an_explicit_error() {
        let fixture = Fixture::encrypted();
        let mut archive = ZipArchiveReader::open(&fixture.path).unwrap();
        let entries = archive.entries().unwrap();
        assert!(entries[0].encrypted);
        let error = match archive.open_entry("secret.log") {
            Ok(_) => panic!("encrypted entry unexpectedly opened"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("加密"));
    }
}
