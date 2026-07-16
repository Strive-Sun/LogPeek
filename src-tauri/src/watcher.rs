//! 目录监控:多目录 notify + 大小稳定检测 + 类型判定 + 配置持久化。

use crate::archive::{is_log_name, is_zip};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::channel;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// 检测到的新日志(通知前端)
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DetectedItem {
    pub path: String,
    pub name: String,
    pub kind: String, // "archive" | "file"
    pub size: u64,
    pub source: String,
}

/// 持久化配置
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct WatchConfig {
    pub dirs: Vec<String>,
    #[serde(default = "default_suffixes")]
    pub suffixes: Vec<String>,
    #[serde(default)]
    pub show_all: bool,
}

fn default_suffixes() -> Vec<String> {
    vec![".log".into(), ".txt".into(), ".out".into()]
}

pub struct WatchState {
    config_path: PathBuf,
    pub config: Mutex<WatchConfig>,
    watchers: Mutex<HashMap<String, RecommendedWatcher>>,
}

impl WatchState {
    pub fn new(config_path: PathBuf) -> Arc<Self> {
        let config = load_config(&config_path).unwrap_or_default();
        Arc::new(Self {
            config_path,
            config: Mutex::new(config),
            watchers: Mutex::new(HashMap::new()),
        })
    }

    pub fn list_dirs(&self) -> Vec<String> {
        self.config.lock().unwrap().dirs.clone()
    }

    #[allow(dead_code)]
    pub fn config(&self) -> WatchConfig {
        self.config.lock().unwrap().clone()
    }

    fn persist(&self) {
        let cfg = self.config.lock().unwrap().clone();
        if let Ok(json) = serde_json::to_string_pretty(&cfg) {
            let _ = std::fs::write(&self.config_path, json);
        }
    }

    /// 添加监控目录:校验存在性 → 持久化 →(调用方负责 emit 初次扫描)
    pub fn add_dir(&self, dir: &str) -> anyhow::Result<()> {
        let p = Path::new(dir);
        if !p.is_dir() {
            anyhow::bail!("目录不存在或不可读: {dir}");
        }
        {
            let mut cfg = self.config.lock().unwrap();
            if !cfg.dirs.iter().any(|d| d == dir) {
                cfg.dirs.push(dir.to_string());
            }
        }
        self.persist();
        Ok(())
    }

    pub fn remove_dir(&self, dir: &str) {
        {
            let mut cfg = self.config.lock().unwrap();
            cfg.dirs.retain(|d| d != dir);
        }
        self.watchers.lock().unwrap().remove(dir);
        self.persist();
    }

    pub fn set_filter(&self, suffixes: Vec<String>, show_all: bool) {
        {
            let mut cfg = self.config.lock().unwrap();
            cfg.suffixes = suffixes;
            cfg.show_all = show_all;
        }
        self.persist();
    }

    /// 注册一个目录的 notify 监听;回调在稳定检测通过后触发
    pub fn start_watch<F>(&self, dir: &str, on_detect: F) -> anyhow::Result<()>
    where
        F: Fn(DetectedItem) + Send + 'static,
    {
        let p = Path::new(dir);
        if !p.is_dir() {
            // 失效目录:跳过不阻断
            return Ok(());
        }
        let (tx, rx) = channel();
        let mut watcher: RecommendedWatcher = notify::recommended_watcher(move |res| {
            let _ = tx.send(res);
        })?;
        watcher.watch(p, RecursiveMode::NonRecursive)?;
        self.watchers
            .lock()
            .unwrap()
            .insert(dir.to_string(), watcher);

        let source = p
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(dir)
            .to_string();
        std::thread::spawn(move || {
            for event in rx.into_iter().flatten() {
                for path in event.paths {
                    if let Some(item) = stable_detect(&path, &source) {
                        on_detect(item);
                    }
                }
            }
        });
        Ok(())
    }

    /// 首次/手动扫描目录内已有文件
    pub fn scan_dir(&self, dir: &str) -> Vec<DetectedItem> {
        let p = Path::new(dir);
        let source = p
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(dir)
            .to_string();
        let mut out = vec![];
        if let Ok(rd) = std::fs::read_dir(p) {
            for e in rd.flatten() {
                let path = e.path();
                if path.is_file() {
                    if let Some(item) = classify(&path, &source) {
                        out.push(item);
                    }
                }
            }
        }
        out
    }
}

/// 大小稳定检测:轮询大小,连续稳定窗口内不变才判定到达完成
fn stable_detect(path: &Path, source: &str) -> Option<DetectedItem> {
    if !path.is_file() {
        return None;
    }
    let mut last = std::fs::metadata(path).ok()?.len();
    let mut stable = 0;
    for _ in 0..20 {
        std::thread::sleep(Duration::from_millis(500));
        let cur = std::fs::metadata(path).ok()?.len();
        if cur == last {
            stable += 1;
            if stable >= 3 {
                break;
            }
        } else {
            stable = 0;
            last = cur;
        }
    }
    classify(path, source)
}

/// 类型判定:zip 归档 / 裸文本日志 / 其余忽略
fn classify(path: &Path, source: &str) -> Option<DetectedItem> {
    let name = path.file_name()?.to_str()?.to_string();
    let size = std::fs::metadata(path).ok()?.len();
    let path_str = path.to_str()?.to_string();

    if is_zip(path).unwrap_or(false) {
        return Some(DetectedItem {
            path: path_str,
            name,
            kind: "archive".into(),
            size,
            source: source.to_string(),
        });
    }
    // 裸文本:扩展名或内容采样
    let is_text = is_log_name(&name) || sample_text(path);
    if is_text {
        return Some(DetectedItem {
            path: path_str,
            name,
            kind: "file".into(),
            size,
            source: source.to_string(),
        });
    }
    None
}

fn sample_text(path: &Path) -> bool {
    use std::io::Read;
    let mut f = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return false,
    };
    let mut buf = [0u8; 4096];
    let n = f.read(&mut buf).unwrap_or(0);
    crate::archive::is_text_sample(&buf[..n])
}

fn load_config(path: &Path) -> anyhow::Result<WatchConfig> {
    let data = std::fs::read_to_string(path)?;
    Ok(serde_json::from_str(&data)?)
}
