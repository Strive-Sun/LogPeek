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
    pub detected_encoding: String,
    pub effective_encoding: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EncodingProgress {
    pub session_id: String,
    pub generation: u64,
    pub percent: u8,
    pub encoding: String,
    pub line_count: u64,
    pub done: bool,
    pub failed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

pub struct Session {
    cache_path: PathBuf,
    /// Starts of complete lines, followed by the current readable boundary.
    offsets: Vec<u64>,
    detected_encoding: Option<&'static Encoding>,
    effective_encoding: &'static Encoding,
    indexing: bool,
    encoding_generation: u64,
    cancel: Arc<AtomicBool>,
}

impl Session {
    pub fn line_count(&self) -> u64 {
        self.offsets.len().saturating_sub(1) as u64
    }
}

pub struct EncodingChange {
    session_id: String,
    session: Arc<Mutex<Session>>,
    cache_path: PathBuf,
    cancel: Arc<AtomicBool>,
    encoding: &'static Encoding,
    generation: u64,
}

impl EncodingChange {
    pub fn generation(&self) -> u64 {
        self.generation
    }
}

struct LineScanner {
    encoding: &'static Encoding,
    pending_utf16_byte: Option<u8>,
}

impl LineScanner {
    fn new(encoding: &'static Encoding) -> Self {
        Self {
            encoding,
            pending_utf16_byte: None,
        }
    }

    fn scan(&mut self, bytes: &[u8], base: u64) -> Vec<u64> {
        if self.encoding != encoding_rs::UTF_16LE && self.encoding != encoding_rs::UTF_16BE {
            return bytes
                .iter()
                .enumerate()
                .filter_map(|(index, byte)| (*byte == b'\n').then_some(base + index as u64 + 1))
                .collect();
        }

        let mut offsets = Vec::new();
        for (index, byte) in bytes.iter().copied().enumerate() {
            if let Some(first) = self.pending_utf16_byte.take() {
                let code_unit = if self.encoding == encoding_rs::UTF_16LE {
                    u16::from_le_bytes([first, byte])
                } else {
                    u16::from_be_bytes([first, byte])
                };
                if code_unit == b'\n' as u16 {
                    offsets.push(base + index as u64 + 1);
                }
            } else {
                self.pending_utf16_byte = Some(byte);
            }
        }
        offsets
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
        if sample.starts_with(&[0xFF, 0xFE]) {
            return encoding_rs::UTF_16LE;
        }
        if sample.starts_with(&[0xFE, 0xFF]) {
            return encoding_rs::UTF_16BE;
        }
        if std::str::from_utf8(sample).is_ok() {
            encoding_rs::UTF_8
        } else {
            encoding_rs::GB18030
        }
    }

    fn encoding_by_name(name: &str) -> anyhow::Result<&'static Encoding> {
        match name.trim().to_ascii_uppercase().as_str() {
            "UTF-8" | "UTF8" => Ok(encoding_rs::UTF_8),
            "GBK" => Ok(encoding_rs::GBK),
            "GB18030" => Ok(encoding_rs::GB18030),
            "UTF-16LE" | "UTF16LE" => Ok(encoding_rs::UTF_16LE),
            "UTF-16BE" | "UTF16BE" => Ok(encoding_rs::UTF_16BE),
            _ => anyhow::bail!("unsupported encoding: {name}"),
        }
    }

    fn encoding_name(encoding: &'static Encoding) -> String {
        if encoding == encoding_rs::UTF_8 {
            "UTF-8"
        } else if encoding == encoding_rs::GBK {
            "GBK"
        } else if encoding == encoding_rs::GB18030 {
            "GB18030"
        } else if encoding == encoding_rs::UTF_16LE {
            "UTF-16LE"
        } else if encoding == encoding_rs::UTF_16BE {
            "UTF-16BE"
        } else {
            encoding.name()
        }
        .to_string()
    }

    fn append_index_chunk(
        session: &Arc<Mutex<Session>>,
        output: &mut BufWriter<File>,
        scanner: &mut LineScanner,
        bytes: &[u8],
        written: &mut u64,
        max_uncompressed: u64,
    ) -> anyhow::Result<u64> {
        let next_written = written.saturating_add(bytes.len() as u64);
        if next_written > max_uncompressed {
            anyhow::bail!("uncompressed content exceeds the size limit");
        }
        output.write_all(bytes)?;
        let new_offsets = scanner.scan(bytes, *written);
        *written = next_written;
        // A reader must never observe an offset before the corresponding bytes.
        output.flush()?;
        let mut current = session.lock().unwrap();
        current.offsets.extend(new_offsets);
        Ok(current.line_count())
    }

    fn trim_line_bytes(raw: &mut Vec<u8>, encoding: &'static Encoding, first_line: bool) {
        if encoding == encoding_rs::UTF_16LE || encoding == encoding_rs::UTF_16BE {
            while raw.len() >= 2 {
                let pair = [raw[raw.len() - 2], raw[raw.len() - 1]];
                let code_unit = if encoding == encoding_rs::UTF_16LE {
                    u16::from_le_bytes(pair)
                } else {
                    u16::from_be_bytes(pair)
                };
                if code_unit != b'\n' as u16 && code_unit != b'\r' as u16 {
                    break;
                }
                raw.truncate(raw.len() - 2);
            }
            if first_line
                && ((encoding == encoding_rs::UTF_16LE && raw.starts_with(&[0xFF, 0xFE]))
                    || (encoding == encoding_rs::UTF_16BE && raw.starts_with(&[0xFE, 0xFF])))
            {
                raw.drain(..2);
            }
        } else {
            while matches!(raw.last(), Some(b'\n') | Some(b'\r')) {
                raw.pop();
            }
            if first_line && raw.starts_with(&[0xEF, 0xBB, 0xBF]) {
                raw.drain(..3);
            }
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
            detected_encoding: None,
            effective_encoding: encoding_rs::UTF_8,
            indexing: true,
            encoding_generation: 0,
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
            encoding: "Detecting".to_string(),
        })
    }

    /// Fill a prepared session. Flushed bytes and their offsets are published atomically.
    pub fn index<R, F>(&self, session_id: &str, declared_size: u64, reader: R, progress: F)
    where
        R: Read,
        F: FnMut(IndexProgress),
    {
        self.index_with_limit(
            session_id,
            declared_size,
            reader,
            MAX_UNCOMPRESSED,
            progress,
        );
    }

    fn index_with_limit<R, F>(
        &self,
        session_id: &str,
        declared_size: u64,
        mut reader: R,
        max_uncompressed: u64,
        mut progress: F,
    ) where
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
            let mut sample = [0u8; 4096];
            let sample_len = reader.read(&mut sample)?;
            let encoding = Self::detect_encoding(&sample[..sample_len]);
            let encoding_name = Self::encoding_name(encoding);
            {
                let mut current = session.lock().unwrap();
                current.detected_encoding = Some(encoding);
                current.effective_encoding = encoding;
            }
            let mut scanner = LineScanner::new(encoding);
            let mut buf = [0u8; 64 * 1024];
            let mut last_emit: Option<Instant> = None;

            if sample_len > 0 {
                let indexed_lines = Self::append_index_chunk(
                    &session,
                    &mut out,
                    &mut scanner,
                    &sample[..sample_len],
                    &mut written,
                    max_uncompressed,
                )?;
                progress(IndexProgress {
                    session_id: session_id.to_string(),
                    percent: written
                        .saturating_mul(100)
                        .checked_div(declared_size)
                        .unwrap_or(0)
                        .min(99) as u8,
                    indexed_lines,
                    done: false,
                    failed: false,
                    detected_encoding: encoding_name.clone(),
                    effective_encoding: encoding_name.clone(),
                    error: None,
                });
                last_emit = Some(Instant::now());
            }

            loop {
                if cancel.load(Ordering::Acquire) {
                    anyhow::bail!("indexing cancelled");
                }
                let n = reader.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                let indexed_lines = Self::append_index_chunk(
                    &session,
                    &mut out,
                    &mut scanner,
                    &buf[..n],
                    &mut written,
                    max_uncompressed,
                )?;
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
                        detected_encoding: encoding_name.clone(),
                        effective_encoding: encoding_name.clone(),
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
                current.indexing = false;
                current.line_count()
            };
            Ok(indexed_lines)
        })();

        match result {
            Ok(indexed_lines) => {
                let current = session.lock().unwrap();
                let detected = current.detected_encoding.unwrap_or(encoding_rs::UTF_8);
                progress(IndexProgress {
                    session_id: session_id.to_string(),
                    percent: 100,
                    indexed_lines,
                    done: true,
                    failed: false,
                    detected_encoding: Self::encoding_name(detected),
                    effective_encoding: Self::encoding_name(current.effective_encoding),
                    error: None,
                });
            }
            Err(_) if cancel.load(Ordering::Acquire) => {}
            Err(error) => {
                let mut current = session.lock().unwrap();
                current.indexing = false;
                let detected = current.detected_encoding.unwrap_or(encoding_rs::UTF_8);
                progress(IndexProgress {
                    session_id: session_id.to_string(),
                    percent: 100,
                    indexed_lines: current.line_count(),
                    done: true,
                    failed: true,
                    detected_encoding: Self::encoding_name(detected),
                    effective_encoding: Self::encoding_name(current.effective_encoding),
                    error: Some(error.to_string()),
                });
            }
        }
    }

    pub fn prepare_encoding_change(
        &self,
        session_id: &str,
        encoding_name: &str,
    ) -> anyhow::Result<EncodingChange> {
        let encoding = Self::encoding_by_name(encoding_name)?;
        let session = {
            let sessions = self.sessions.lock().unwrap();
            sessions.get(session_id).cloned()
        }
        .ok_or_else(|| anyhow::anyhow!("session not found: {session_id}"))?;
        self.touch_lru(session_id);
        let mut current = session.lock().unwrap();
        if current.indexing {
            anyhow::bail!("wait for initial indexing to finish before changing encoding");
        }
        current.encoding_generation = current.encoding_generation.saturating_add(1);
        let change = EncodingChange {
            session_id: session_id.to_string(),
            session: session.clone(),
            cache_path: current.cache_path.clone(),
            cancel: current.cancel.clone(),
            encoding,
            generation: current.encoding_generation,
        };
        drop(current);
        Ok(change)
    }

    pub fn apply_encoding_change<F>(&self, change: EncodingChange, mut progress: F)
    where
        F: FnMut(EncodingProgress),
    {
        let encoding_name = Self::encoding_name(change.encoding);
        let result = (|| -> anyhow::Result<Option<u64>> {
            let mut file = File::open(&change.cache_path)?;
            let total_bytes = file.metadata()?.len();
            let mut offsets = vec![0u64];
            let mut scanner = LineScanner::new(change.encoding);
            let mut buffer = [0u8; 64 * 1024];
            let mut read_bytes = 0u64;
            let mut last_emit: Option<Instant> = None;

            loop {
                if change.cancel.load(Ordering::Acquire) {
                    return Ok(None);
                }
                if change.session.lock().unwrap().encoding_generation != change.generation {
                    return Ok(None);
                }
                let count = file.read(&mut buffer)?;
                if count == 0 {
                    break;
                }
                offsets.extend(scanner.scan(&buffer[..count], read_bytes));
                read_bytes += count as u64;
                let now = Instant::now();
                if last_emit
                    .map(|last| now.duration_since(last) >= Duration::from_millis(50))
                    .unwrap_or(true)
                {
                    progress(EncodingProgress {
                        session_id: change.session_id.clone(),
                        generation: change.generation,
                        percent: read_bytes
                            .saturating_mul(100)
                            .checked_div(total_bytes)
                            .unwrap_or(0)
                            .min(99) as u8,
                        encoding: encoding_name.clone(),
                        line_count: offsets.len().saturating_sub(1) as u64,
                        done: false,
                        failed: false,
                        error: None,
                    });
                    last_emit = Some(now);
                }
            }

            if *offsets.last().unwrap() != total_bytes {
                offsets.push(total_bytes);
            }
            let mut current = change.session.lock().unwrap();
            if current.encoding_generation != change.generation
                || change.cancel.load(Ordering::Acquire)
            {
                return Ok(None);
            }
            current.effective_encoding = change.encoding;
            current.offsets = offsets;
            Ok(Some(current.line_count()))
        })();

        match result {
            Ok(Some(line_count)) => progress(EncodingProgress {
                session_id: change.session_id,
                generation: change.generation,
                percent: 100,
                encoding: encoding_name,
                line_count,
                done: true,
                failed: false,
                error: None,
            }),
            Ok(None) => {}
            Err(error) => {
                if change.session.lock().unwrap().encoding_generation == change.generation {
                    progress(EncodingProgress {
                        session_id: change.session_id,
                        generation: change.generation,
                        percent: 100,
                        encoding: encoding_name,
                        line_count: 0,
                        done: true,
                        failed: true,
                        error: Some(error.to_string()),
                    });
                }
            }
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
            Self::trim_line_bytes(&mut raw, session.effective_encoding, line_no == 0);
            let (text, _, _) = session.effective_encoding.decode(&raw);
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
    use std::sync::atomic::{AtomicBool, AtomicU64};
    use std::sync::mpsc;

    static CACHE_TEST_SEQ: AtomicU64 = AtomicU64::new(1);

    fn indexed_manager(bytes: Vec<u8>) -> (SessionManager, String, Vec<IndexProgress>) {
        let manager = SessionManager::default();
        let open = manager
            .prepare("encoding.log".into(), bytes.len() as u64)
            .unwrap();
        let mut events = Vec::new();
        manager.index(
            &open.session_id,
            bytes.len() as u64,
            Cursor::new(bytes),
            |event| events.push(event),
        );
        (manager, open.session_id, events)
    }

    fn utf16_bytes(text: &str, big_endian: bool, bom: bool) -> Vec<u8> {
        let mut bytes = Vec::new();
        if bom {
            bytes.extend(if big_endian {
                [0xFE, 0xFF]
            } else {
                [0xFF, 0xFE]
            });
        }
        for unit in text.encode_utf16() {
            bytes.extend(if big_endian {
                unit.to_be_bytes()
            } else {
                unit.to_le_bytes()
            });
        }
        bytes
    }

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

    #[test]
    fn closing_session_cancels_indexing_and_removes_the_cache() {
        let manager = Arc::new(SessionManager::default());
        let open = manager.prepare("cancel.log".into(), 128 * 1024).unwrap();
        let cache_path = manager
            .sessions
            .lock()
            .unwrap()
            .get(&open.session_id)
            .unwrap()
            .lock()
            .unwrap()
            .cache_path
            .clone();
        let closer = manager.clone();
        let closing_id = open.session_id.clone();
        let mut events = Vec::new();
        manager.index(
            &open.session_id,
            128 * 1024,
            Cursor::new(vec![b'x'; 128 * 1024]),
            |event| {
                events.push(event);
                closer.close(&closing_id);
            },
        );

        assert_eq!(events.len(), 1);
        assert!(!events[0].done);
        assert!(!cache_path.exists());
    }

    #[test]
    fn actual_bytes_over_the_limit_emit_a_terminal_failure() {
        let manager = SessionManager::default();
        let open = manager.prepare("limit.log".into(), 3).unwrap();
        let mut events = Vec::new();
        manager.index_with_limit(
            &open.session_id,
            3,
            Cursor::new(b"actual content"),
            3,
            |event| events.push(event),
        );

        assert_eq!(events.len(), 1);
        assert!(events[0].done);
        assert!(events[0].failed);
        assert!(events[0].error.as_deref().unwrap().contains("size limit"));
    }

    #[test]
    fn detects_and_reads_utf8_bom_and_gb18030() {
        let (utf8_manager, utf8_id, utf8_events) =
            indexed_manager(b"\xEF\xBB\xBFfirst\r\nsecond\n".to_vec());
        assert_eq!(utf8_events.last().unwrap().effective_encoding, "UTF-8");
        let utf8_lines = utf8_manager.read_lines(&utf8_id, 0, 10).unwrap();
        assert_eq!(utf8_lines[0].content, "first");
        assert_eq!(utf8_lines[1].content, "second");

        let (encoded, _, had_errors) = encoding_rs::GB18030.encode("中文😀\n第二行");
        assert!(!had_errors);
        let (gb_manager, gb_id, gb_events) = indexed_manager(encoded.into_owned());
        assert_eq!(gb_events.last().unwrap().effective_encoding, "GB18030");
        let gb_lines = gb_manager.read_lines(&gb_id, 0, 10).unwrap();
        assert_eq!(gb_lines[0].content, "中文😀");
        assert_eq!(gb_lines[1].content, "第二行");
    }

    #[test]
    fn indexes_utf16_in_both_byte_orders_and_strips_bom() {
        for (big_endian, expected_name) in [(false, "UTF-16LE"), (true, "UTF-16BE")] {
            let bytes = utf16_bytes("第一行\r\nsecond\n末行", big_endian, true);
            let (manager, session_id, events) = indexed_manager(bytes);
            assert_eq!(events.last().unwrap().effective_encoding, expected_name);
            let lines = manager.read_lines(&session_id, 0, 10).unwrap();
            assert_eq!(lines.len(), 3);
            assert_eq!(lines[0].content, "第一行");
            assert_eq!(lines[1].content, "second");
            assert_eq!(lines[2].content, "末行");
        }
    }

    #[test]
    fn manual_encoding_change_rebuilds_offsets_and_latest_generation_wins() {
        let bytes = utf16_bytes("alpha\nbeta\ngamma", false, false);
        let (manager, session_id, _) = indexed_manager(bytes);
        let stale = manager
            .prepare_encoding_change(&session_id, "GB18030")
            .unwrap();
        let current = manager
            .prepare_encoding_change(&session_id, "UTF-16LE")
            .unwrap();
        let mut stale_events = Vec::new();
        manager.apply_encoding_change(stale, |event| stale_events.push(event));
        assert!(stale_events.is_empty());

        let mut events = Vec::new();
        manager.apply_encoding_change(current, |event| events.push(event));
        assert!(events.last().unwrap().done);
        assert_eq!(events.last().unwrap().encoding, "UTF-16LE");
        let lines = manager.read_lines(&session_id, 0, 10).unwrap();
        assert_eq!(
            lines
                .iter()
                .map(|line| line.content.as_str())
                .collect::<Vec<_>>(),
            ["alpha", "beta", "gamma"]
        );
    }

    #[test]
    fn line_index_handles_empty_lines_crlf_and_tail_without_newline() {
        let (manager, session_id, _) = indexed_manager(b"\nalpha\r\n\nomega".to_vec());
        assert_eq!(manager.line_count(&session_id), 4);
        let lines = manager.read_lines(&session_id, 0, 10).unwrap();
        assert_eq!(
            lines
                .iter()
                .map(|line| line.content.as_str())
                .collect::<Vec<_>>(),
            ["", "alpha", "", "omega"]
        );
        assert_eq!(
            manager.read_lines(&session_id, 3, 99).unwrap()[0].content,
            "omega"
        );
        assert!(manager.read_lines(&session_id, 4, 1).unwrap().is_empty());
        assert!(manager.read_lines(&session_id, 0, 0).unwrap().is_empty());
    }

    #[test]
    fn oversized_line_is_truncated_and_marked() {
        let mut bytes = vec![b'x'; MAX_LINE_BYTES + 100];
        bytes.push(b'\n');
        let (manager, session_id, _) = indexed_manager(bytes);
        let lines = manager.read_lines(&session_id, 0, 1).unwrap();
        assert_eq!(lines.len(), 1);
        assert!(lines[0].truncated);
        assert_eq!(lines[0].content.len(), MAX_LINE_BYTES);
    }

    #[test]
    fn close_and_lru_eviction_remove_cache_files() {
        let cache_dir = std::env::temp_dir().join(format!(
            "logpeek-index-test-{}-{}",
            std::process::id(),
            CACHE_TEST_SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        let manager = SessionManager::default();
        manager.set_cache_dir(cache_dir.clone());

        let closed = manager.prepare("closed.log".into(), 0).unwrap();
        let closed_path = manager
            .sessions
            .lock()
            .unwrap()
            .get(&closed.session_id)
            .unwrap()
            .lock()
            .unwrap()
            .cache_path
            .clone();
        manager.close(&closed.session_id);
        assert!(!closed_path.exists());

        let first = manager.prepare("first.log".into(), 0).unwrap();
        let first_path = manager
            .sessions
            .lock()
            .unwrap()
            .get(&first.session_id)
            .unwrap()
            .lock()
            .unwrap()
            .cache_path
            .clone();
        for index in 0..MAX_SESSIONS {
            manager.prepare(format!("extra-{index}.log"), 0).unwrap();
        }
        assert_eq!(manager.line_count(&first.session_id), 0);
        assert!(!first_path.exists());
        manager.clear_all();
        let _ = std::fs::remove_dir(cache_dir);
    }
}
