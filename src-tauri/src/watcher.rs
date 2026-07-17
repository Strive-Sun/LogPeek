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
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WatchConfig {
    #[serde(default)]
    pub dirs: Vec<String>,
    #[serde(default = "default_suffixes")]
    pub suffixes: Vec<String>,
    #[serde(default)]
    pub show_all: bool,
}

/// 手写 Default 以复用 default_suffixes,保证内存态默认(全新安装、
/// 配置缺失)与前端默认后缀一致,避免后端把所有裸文件通知都压制。
impl Default for WatchConfig {
    fn default() -> Self {
        Self {
            dirs: Vec::new(),
            suffixes: default_suffixes(),
            show_all: false,
        }
    }
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

    /// 重命名监控目录:磁盘改名 + 更新配置中的路径 + 停掉旧监听。
    /// 返回新路径(调用方负责对新路径重建监听)。
    pub fn rename_dir(&self, old: &str, new_name: &str) -> anyhow::Result<String> {
        let src = Path::new(old);
        if !src.is_dir() {
            anyhow::bail!("目录不存在: {old}");
        }
        let name = new_name.trim();
        if name.is_empty() {
            anyhow::bail!("名称不能为空");
        }
        if name.contains('/') || name.contains('\\') {
            anyhow::bail!("名称不能包含路径分隔符");
        }
        let parent = src
            .parent()
            .ok_or_else(|| anyhow::anyhow!("无法确定父目录"))?;
        let dst = parent.join(name);
        if dst.exists() {
            anyhow::bail!("已存在同名目录: {name}");
        }
        std::fs::rename(src, &dst)?;
        let dst_str = dst.to_string_lossy().into_owned();
        {
            let mut cfg = self.config.lock().unwrap();
            for d in cfg.dirs.iter_mut() {
                if d == old {
                    *d = dst_str.clone();
                }
            }
        }
        self.watchers.lock().unwrap().remove(old);
        self.persist();
        Ok(dst_str)
    }

    pub fn set_filter(&self, suffixes: Vec<String>, show_all: bool) {
        {
            let mut cfg = self.config.lock().unwrap();
            cfg.suffixes = suffixes;
            cfg.show_all = show_all;
        }
        self.persist();
    }

    /// 读取当前持久化的后缀筛选配置(suffixes, show_all)。
    pub fn get_filter(&self) -> (Vec<String>, bool) {
        let cfg = self.config.lock().unwrap();
        (cfg.suffixes.clone(), cfg.show_all)
    }

    /// 依据当前持久化的后缀筛选,判断一个到达项是否应计入新日志通知。
    /// 语义与前端目录树筛选一致:压缩包(archive)始终通知;`show_all`
    /// 开启时不过滤;否则仅当裸文件名匹配任一配置后缀时通知。
    /// 每次调用读取最新配置,保证筛选规则变更后对后续到达即时生效。
    pub fn should_notify(&self, item: &DetectedItem) -> bool {
        if item.kind == "archive" {
            return true;
        }
        let cfg = self.config.lock().unwrap();
        if cfg.show_all {
            return true;
        }
        let lower = item.name.to_lowercase();
        cfg.suffixes
            .iter()
            .any(|s| lower.ends_with(&s.to_lowercase()))
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static DIR_SEQ: AtomicU64 = AtomicU64::new(1);

    struct FixtureDir {
        path: PathBuf,
    }

    impl FixtureDir {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "logpeek-watcher-test-{}-{}",
                std::process::id(),
                DIR_SEQ.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&path).unwrap();
            Self { path }
        }

        fn write(&self, name: &str, bytes: &[u8]) -> PathBuf {
            let path = self.path.join(name);
            std::fs::write(&path, bytes).unwrap();
            path
        }
    }

    impl Drop for FixtureDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    fn item(name: &str, kind: &str) -> DetectedItem {
        DetectedItem {
            path: name.to_string(),
            name: name.to_string(),
            kind: kind.to_string(),
            size: 1,
            source: "test".into(),
        }
    }

    #[test]
    fn classify_recognizes_zip_logs_text_samples_and_binary_files() {
        let fixture = FixtureDir::new();
        let zip = fixture.write("download.part", b"PK\x03\x04");
        let log = fixture.write("server.LOG", b"\0binary-looking extension still wins");
        let sampled = fixture.write("notes.data", b"plain sampled text\n");
        let binary = fixture.write("image.bin", &[0, 1, 2, 3]);

        assert_eq!(classify(&zip, "test").unwrap().kind, "archive");
        assert_eq!(classify(&log, "test").unwrap().kind, "file");
        assert_eq!(classify(&sampled, "test").unwrap().kind, "file");
        assert!(classify(&binary, "test").is_none());
    }

    #[test]
    fn suffix_filter_is_case_insensitive_and_show_all_bypasses_it() {
        let fixture = FixtureDir::new();
        let state = WatchState::new(fixture.path.join("config.json"));
        state.set_filter(vec![".log".into(), ".TXT".into()], false);
        assert!(state.should_notify(&item("SERVER.LOG", "file")));
        assert!(state.should_notify(&item("notes.txt", "file")));
        assert!(!state.should_notify(&item("trace.out", "file")));
        assert!(state.should_notify(&item("bundle.zip", "archive")));

        state.set_filter(vec![], true);
        assert!(state.should_notify(&item("anything.bin", "file")));
    }

    #[test]
    fn configuration_persists_directories_and_filters() {
        let fixture = FixtureDir::new();
        let watched = fixture.path.join("watched");
        std::fs::create_dir(&watched).unwrap();
        let config_path = fixture.path.join("config.json");
        {
            let state = WatchState::new(config_path.clone());
            state.add_dir(watched.to_str().unwrap()).unwrap();
            state.set_filter(vec![".trace".into()], true);
        }

        let restored = WatchState::new(config_path);
        assert_eq!(
            restored.list_dirs(),
            vec![watched.to_string_lossy().into_owned()]
        );
        assert_eq!(restored.get_filter(), (vec![".trace".into()], true));
    }

    #[test]
    fn invalid_persisted_directory_is_skipped_without_panicking() {
        let fixture = FixtureDir::new();
        let missing = fixture.path.join("missing");
        let config_path = fixture.path.join("config.json");
        let config = WatchConfig {
            dirs: vec![missing.to_string_lossy().into_owned()],
            ..WatchConfig::default()
        };
        std::fs::write(&config_path, serde_json::to_vec(&config).unwrap()).unwrap();
        let state = WatchState::new(config_path);

        assert!(state.start_watch(missing.to_str().unwrap(), |_| {}).is_ok());
        assert!(state.scan_dir(missing.to_str().unwrap()).is_empty());
    }
}
