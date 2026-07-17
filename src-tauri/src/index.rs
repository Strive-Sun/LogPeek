//! Incremental line indexing, windowed reads, decoding, and session lifecycle.

use encoding_rs::Encoding;
use serde::Serialize;
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const MAX_UNCOMPRESSED: u64 = 2 * 1024 * 1024 * 1024;
pub const MAX_LINE_BYTES: usize = 64 * 1024;
const MAX_SESSIONS: usize = 5;

static SESSION_SEQ: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LogLine {
    pub line_no: u64,
    pub content: String,
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenResult {
    pub session_id: String,
    pub entry_path: String,
    pub size: u64,
    pub indexing: bool,
    pub encoding: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexProgress {
    pub session_id: String,
    pub percent: u8,
    pub indexed_lines: u64,
    pub done: bool,
    pub failed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

pub struct Session {
    cache_path: PathBuf,
    /// Starts of complete lines, followed by the current readable boundary.
    offsets: Vec<u64>,
    encoding: &'static Encoding,
    cancel: Arc<AtomicBool>,
}

impl Session {
    pub fn line_count(&self) -> u64 {
        self.offsets.len().saturating_sub(1) as u64
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.cache_path);
    }
}

#[derive(Default)]
pub struct SessionManager {
    sessions: Mutex<HashMap<String, Arc<Mutex<Session>>>>,
    lru: Mutex<Vec<String>>,
    cache_dir: Mutex<Option<PathBuf>>,
}

impl SessionManager {
    pub fn set_cache_dir(&self, dir: PathBuf) {
        let _ = std::fs::create_dir_all(&dir);
        *self.cache_dir.lock().unwrap() = Some(dir);
    }

    fn new_cache_path(&self, session_id: &str) -> PathBuf {
        let dir = self
            .cache_dir
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_else(std::env::temp_dir);
        dir.join(format!("logpeek-{session_id}.cache"))
    }

    fn detect_encoding(sample: &[u8]) -> &'static Encoding {
        if sample.starts_with(&[0xEF, 0xBB, 0xBF]) {
            return encoding_rs::UTF_8;
        }
        if sample.starts_with(&[0xFF, 0xFE]) || sample.starts_with(&[0xFE, 0xFF]) {
            return encoding_rs::UTF_16LE;
        }
        if std::str::from_utf8(sample).is_ok() {
            encoding_rs::UTF_8
        } else {
            encoding_rs::GBK
        }
    }

    /// Create an empty session so the command can return before indexing begins.
    pub fn prepare(&self, entry_path: String, declared_size: u64) -> anyhow::Result<OpenResult> {
        if declared_size > MAX_UNCOMPRESSED {
            anyhow::bail!("file is too large; files over 2GB are not supported");
        }
        let session_id = format!("s{}", SESSION_SEQ.fetch_add(1, Ordering::SeqCst));
        let cache_path = self.new_cache_path(&session_id);
        File::create(&cache_path)?;
        let session = Session {
            cache_path,
            offsets: vec![0],
            encoding: encoding_rs::UTF_8,
            cancel: Arc::new(AtomicBool::new(false)),
        };
        self.sessions
            .lock()
            .unwrap()
            .insert(session_id.clone(), Arc::new(Mutex::new(session)));
        self.touch_lru(&session_id);

        Ok(OpenResult {
            session_id,
            entry_path,
            size: declared_size,
            indexing: true,
            encoding: encoding_rs::UTF_8.name().to_string(),
        })
    }

    /// Fill a prepared session. Flushed bytes and their offsets are published atomically.
    pub fn index<R, F>(&self, session_id: &str, declared_size: u64, mut reader: R, mut progress: F)
    where
        R: Read,
        F: FnMut(IndexProgress),
    {
        let session = {
            let map = self.sessions.lock().unwrap();
            map.get(session_id).cloned()
        };
        let Some(session) = session else { return };
        let (cache_path, cancel) = {
            let session = session.lock().unwrap();
            (session.cache_path.clone(), session.cancel.clone())
        };

        let result = (|| -> anyhow::Result<u64> {
            let mut out = BufWriter::new(File::create(&cache_path)?);
            let mut written = 0u64;
            let mut sample = Vec::with_capacity(4096);
            let mut buf = [0u8; 64 * 1024];
            let mut last_emit: Option<Instant> = None;

            loop {
                if cancel.load(Ordering::Acquire) {
                    anyhow::bail!("indexing cancelled");
                }
                let n = reader.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                if sample.len() < 4096 {
                    sample.extend_from_slice(&buf[..n.min(4096 - sample.len())]);
                }
                out.write_all(&buf[..n])?;
                let mut new_offsets = Vec::new();
                for (i, &byte) in buf[..n].iter().enumerate() {
                    if byte == b'\n' {
                        new_offsets.push(written + i as u64 + 1);
                    }
                }
                written += n as u64;
                if written > MAX_UNCOMPRESSED {
                    anyhow::bail!("uncompressed content exceeds the 2GB limit");
                }

                // A reader must never observe an offset before the corresponding bytes.
                out.flush()?;
                let indexed_lines = {
                    let mut current = session.lock().unwrap();
                    current.encoding = Self::detect_encoding(&sample);
                    current.offsets.extend(new_offsets);
                    current.line_count()
                };
                let percent = written
                    .saturating_mul(100)
                    .checked_div(declared_size)
                    .unwrap_or(0)
                    .min(99) as u8;
                let now = Instant::now();
                if last_emit
                    .map(|last| now.duration_since(last) >= Duration::from_millis(50))
                    .unwrap_or(true)
                {
                    progress(IndexProgress {
                        session_id: session_id.to_string(),
                        percent,
                        indexed_lines,
                        done: false,
                        failed: false,
                        error: None,
                    });
                    last_emit = Some(now);
                }
            }

            out.flush()?;
            let indexed_lines = {
                let mut current = session.lock().unwrap();
                if *current.offsets.last().unwrap() != written {
                    current.offsets.push(written);
                }
                current.encoding = Self::detect_encoding(&sample);
                current.line_count()
            };
            Ok(indexed_lines)
        })();

        match result {
            Ok(indexed_lines) => progress(IndexProgress {
                session_id: session_id.to_string(),
                percent: 100,
                indexed_lines,
                done: true,
                failed: false,
                error: None,
            }),
            Err(_) if cancel.load(Ordering::Acquire) => {}
            Err(error) => progress(IndexProgress {
                session_id: session_id.to_string(),
                percent: 100,
                indexed_lines: session.lock().unwrap().line_count(),
                done: true,
                failed: true,
                error: Some(error.to_string()),
            }),
        }
    }

    fn touch_lru(&self, session_id: &str) {
        let mut lru = self.lru.lock().unwrap();
        lru.retain(|s| s != session_id);
        lru.push(session_id.to_string());
        while lru.len() > MAX_SESSIONS {
            let evict = lru.remove(0);
            let removed = self.sessions.lock().unwrap().remove(&evict);
            if let Some(session) = removed {
                session
                    .lock()
                    .unwrap()
                    .cancel
                    .store(true, Ordering::Release);
            }
        }
    }

    pub fn read_lines(
        &self,
        session_id: &str,
        start: u64,
        count: u64,
    ) -> anyhow::Result<Vec<LogLine>> {
        let session = {
            let map = self.sessions.lock().unwrap();
            map.get(session_id).cloned()
        };
        let session = session.ok_or_else(|| anyhow::anyhow!("session not found: {session_id}"))?;
        self.touch_lru(session_id);
        let session = session.lock().unwrap();

        let total = session.line_count();
        if start >= total {
            return Ok(vec![]);
        }
        let end = start.saturating_add(count).min(total);
        let mut file = File::open(&session.cache_path)?;
        let mut lines = Vec::with_capacity((end - start) as usize);
        for line_no in start..end {
            let from = session.offsets[line_no as usize];
            let to = session.offsets[line_no as usize + 1];
            let len = (to - from) as usize;
            let read_len = len.min(MAX_LINE_BYTES);
            let mut raw = vec![0u8; read_len];
            file.seek(SeekFrom::Start(from))?;
            file.read_exact(&mut raw)?;
            while matches!(raw.last(), Some(b'\n') | Some(b'\r')) {
                raw.pop();
            }
            let (text, _, _) = session.encoding.decode(&raw);
            lines.push(LogLine {
                line_no: line_no + 1,
                content: text.into_owned(),
                truncated: len > MAX_LINE_BYTES,
            });
        }
        Ok(lines)
    }

    pub fn line_count(&self, session_id: &str) -> u64 {
        self.sessions
            .lock()
            .unwrap()
            .get(session_id)
            .map(|session| session.lock().unwrap().line_count())
            .unwrap_or(0)
    }

    pub fn close(&self, session_id: &str) {
        let removed = self.sessions.lock().unwrap().remove(session_id);
        if let Some(session) = removed {
            session
                .lock()
                .unwrap()
                .cancel
                .store(true, Ordering::Release);
        }
        self.lru.lock().unwrap().retain(|id| id != session_id);
    }

    #[allow(dead_code)]
    pub fn clear_all(&self) {
        let sessions = std::mem::take(&mut *self.sessions.lock().unwrap());
        for session in sessions.into_values() {
            session
                .lock()
                .unwrap()
                .cancel
                .store(true, Ordering::Release);
        }
        self.lru.lock().unwrap().clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::sync::atomic::AtomicBool;
    use std::sync::mpsc;

    #[test]
    fn publishes_readable_lines_before_indexing_finishes() {
        let manager = Arc::new(SessionManager::default());
        let open = manager.prepare("test.log".into(), 12).unwrap();
        let session_id = open.session_id.clone();
        let (tx, rx) = mpsc::channel();
        let read_while_indexing = Arc::new(AtomicBool::new(false));
        let observed = read_while_indexing.clone();
        let reader_manager = manager.clone();
        let reader_session_id = session_id.clone();
        manager.index(&session_id, 12, Cursor::new(b"one\ntwo\nlast"), |event| {
            if !event.done && event.indexed_lines > 0 {
                let lines = reader_manager
                    .read_lines(&reader_session_id, 0, 200)
                    .unwrap();
                assert!(!lines.is_empty());
                observed.store(true, Ordering::Release);
            }
            tx.send(event).unwrap();
        });

        let events: Vec<_> = rx.try_iter().collect();
        assert!(events.iter().any(|event| !event.done));
        assert!(events.last().unwrap().done);
        assert!(read_while_indexing.load(Ordering::Acquire));
        assert_eq!(manager.line_count(&session_id), 3);
        let lines = manager.read_lines(&session_id, 1, 99).unwrap();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].content, "two");
        assert_eq!(lines[1].content, "last");
    }

    #[test]
    fn read_lines_clamps_to_the_published_boundary() {
        let manager = SessionManager::default();
        let open = manager.prepare("partial.log".into(), 0).unwrap();
        assert!(manager
            .read_lines(&open.session_id, 0, 200)
            .unwrap()
            .is_empty());
    }
}
