//! 行偏移索引 + 窗口化加载 + 编码解码 + 会话生命周期(方案 A)。
//!
//! 打开条目时:把解压流一趟写入内部临时缓存文件(方案 A),
//! 同时记录每行字节偏移;查看时对缓存文件 seek 读取指定行范围。

use encoding_rs::Encoding;
use serde::Serialize;
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// 解压后大小上限:超过则拒绝(方案 A,见设计文档)
pub const MAX_UNCOMPRESSED: u64 = 2 * 1024 * 1024 * 1024;
/// 单行返回上限,超过截断
pub const MAX_LINE_BYTES: usize = 64 * 1024;

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

/// 一个查看会话:缓存文件 + 行偏移索引
pub struct Session {
    cache_path: PathBuf,
    /// 每行起始字节偏移(末尾追加文件总长以便计算最后一行长度)
    offsets: Vec<u64>,
    encoding: &'static Encoding,
}

impl Session {
    pub fn line_count(&self) -> u64 {
        self.offsets.len().saturating_sub(1) as u64
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // 关闭会话即清理临时缓存(方案 A:用完即清)
        let _ = std::fs::remove_file(&self.cache_path);
    }
}

#[derive(Default)]
pub struct SessionManager {
    sessions: Mutex<HashMap<String, Arc<Mutex<Session>>>>,
    /// LRU 顺序(最近使用在末尾)
    lru: Mutex<Vec<String>>,
    cache_dir: Mutex<Option<PathBuf>>,
}

const MAX_SESSIONS: usize = 5;

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

    /// 探测编码:BOM 优先,其次尝试 UTF-8,失败回退 GBK
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

    /// 打开条目:一趟顺序读入 reader → 写缓存文件 → 建行偏移索引(方案 A)。
    /// reader 内容超过上限则中止并报错(按实际字节数熔断)。
    pub fn open<R: Read>(
        &self,
        mut reader: R,
        entry_path: String,
        declared_size: u64,
    ) -> anyhow::Result<OpenResult> {
        if declared_size > MAX_UNCOMPRESSED {
            anyhow::bail!("文件过大,超大文件支持将在后续版本提供");
        }
        let session_id = format!("s{}", SESSION_SEQ.fetch_add(1, Ordering::SeqCst));
        let cache_path = self.new_cache_path(&session_id);

        let mut out = BufWriter::new(File::create(&cache_path)?);
        let mut offsets: Vec<u64> = vec![0];
        let mut written: u64 = 0;
        let mut sample: Vec<u8> = Vec::with_capacity(4096);
        let mut buf = [0u8; 64 * 1024];

        loop {
            let n = reader.read(&mut buf)?;
            if n == 0 {
                break;
            }
            if sample.len() < 4096 {
                sample.extend_from_slice(&buf[..n.min(4096 - sample.len())]);
            }
            out.write_all(&buf[..n])?;
            // 记录换行位置(字节偏移),兼容 \n / \r\n(\r 在读取时剥离)
            for (i, &b) in buf[..n].iter().enumerate() {
                if b == b'\n' {
                    offsets.push(written + i as u64 + 1);
                }
            }
            written += n as u64;
            if written > MAX_UNCOMPRESSED {
                drop(out);
                let _ = std::fs::remove_file(&cache_path);
                anyhow::bail!("解压后超过 2GB 上限,已中止");
            }
        }
        // 末尾补一个总长,便于取最后一行
        if *offsets.last().unwrap() != written {
            offsets.push(written);
        }
        out.flush()?;

        let encoding = Self::detect_encoding(&sample);
        let session = Session {
            cache_path,
            offsets,
            encoding,
        };
        let line_count = session.line_count();

        let mut map = self.sessions.lock().unwrap();
        map.insert(session_id.clone(), Arc::new(Mutex::new(session)));
        drop(map);
        self.touch_lru(&session_id);

        Ok(OpenResult {
            session_id,
            entry_path,
            size: written,
            indexing: line_count > 300_000,
            encoding: encoding.name().to_string(),
        })
    }

    fn touch_lru(&self, session_id: &str) {
        let mut lru = self.lru.lock().unwrap();
        lru.retain(|s| s != session_id);
        lru.push(session_id.to_string());
        while lru.len() > MAX_SESSIONS {
            let evict = lru.remove(0);
            self.sessions.lock().unwrap().remove(&evict);
        }
    }

    /// 读取指定行范围(0-based start)
    pub fn read_lines(
        &self,
        session_id: &str,
        start: u64,
        count: u64,
    ) -> anyhow::Result<Vec<LogLine>> {
        let sess = {
            let map = self.sessions.lock().unwrap();
            map.get(session_id).cloned()
        };
        let sess = sess.ok_or_else(|| anyhow::anyhow!("会话不存在: {session_id}"))?;
        self.touch_lru(session_id);
        let sess = sess.lock().unwrap();

        let total = sess.line_count();
        if start >= total {
            return Ok(vec![]);
        }
        let end = (start + count).min(total);
        let mut f = File::open(&sess.cache_path)?;
        let mut out = Vec::with_capacity((end - start) as usize);
        for ln in start..end {
            let from = sess.offsets[ln as usize];
            let to = sess.offsets[ln as usize + 1];
            let len = (to - from) as usize;
            let read_len = len.min(MAX_LINE_BYTES);
            let mut raw = vec![0u8; read_len];
            f.seek(SeekFrom::Start(from))?;
            f.read_exact(&mut raw)?;
            // 剥离行尾 \n / \r
            while matches!(raw.last(), Some(b'\n') | Some(b'\r')) {
                raw.pop();
            }
            let (text, _, _) = sess.encoding.decode(&raw);
            out.push(LogLine {
                line_no: ln + 1,
                content: text.into_owned(),
                truncated: len > MAX_LINE_BYTES,
            });
        }
        Ok(out)
    }

    pub fn line_count(&self, session_id: &str) -> u64 {
        self.sessions
            .lock()
            .unwrap()
            .get(session_id)
            .map(|s| s.lock().unwrap().line_count())
            .unwrap_or(0)
    }

    pub fn close(&self, session_id: &str) {
        self.sessions.lock().unwrap().remove(session_id);
        self.lru.lock().unwrap().retain(|s| s != session_id);
    }

    /// 进程退出兜底:清理全部会话缓存
    #[allow(dead_code)]
    pub fn clear_all(&self) {
        self.sessions.lock().unwrap().clear();
        self.lru.lock().unwrap().clear();
    }
}
