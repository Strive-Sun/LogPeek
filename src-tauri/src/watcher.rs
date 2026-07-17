//! 目录监控:多目录 notify + 大小稳定检测 + 类型判定 + 配置持久化。

use crate::archive::{is_log_name, is_zip};
use notify::event::{ModifyKind, RenameMode};
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const EVENT_BATCH_WINDOW: Duration = Duration::from_millis(200);
const RAW_EVENT_CAPACITY: usize = 1024;
const STABLE_QUEUE_CAPACITY: usize = 256;

/// 检测到的新日志(通知前端)
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DetectedItem {
    pub path: String,
    pub name: String,
    pub kind: String, // "dir" | "archive" | "file"
    pub size: u64,
    pub source: String,
    pub is_log: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum DirectoryChange {
    Upsert {
        node: DetectedItem,
    },
    Remove {
        path: String,
    },
    Rename {
        #[serde(rename = "oldPath")]
        old_path: String,
        node: DetectedItem,
    },
    Rescan {
        nodes: Vec<DetectedItem>,
    },
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DirectoryChangeBatch {
    pub watch_dir: String,
    pub changes: Vec<DirectoryChange>,
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
    watchers: Mutex<HashMap<String, WatchRegistration>>,
}

struct WatchRegistration {
    _watcher: RecommendedWatcher,
    active: Arc<AtomicBool>,
}

impl Drop for WatchRegistration {
    fn drop(&mut self) {
        self.active.store(false, Ordering::Release);
    }
}

type DetectCallback = Arc<dyn Fn(DetectedItem) + Send + Sync>;

struct StableScheduler {
    tx: SyncSender<PathBuf>,
    generations: Arc<Mutex<HashMap<PathBuf, u64>>>,
    queued: Arc<Mutex<HashSet<PathBuf>>>,
    next_generation: AtomicU64,
}

impl StableScheduler {
    fn new(source: String, on_detect: DetectCallback) -> Self {
        let (tx, rx) = sync_channel(STABLE_QUEUE_CAPACITY);
        let generations = Arc::new(Mutex::new(HashMap::new()));
        let queued = Arc::new(Mutex::new(HashSet::new()));
        let worker_generations = generations.clone();
        let worker_queued = queued.clone();
        std::thread::spawn(move || {
            stable_worker(rx, source, on_detect, worker_generations, worker_queued);
        });
        Self {
            tx,
            generations,
            queued,
            next_generation: AtomicU64::new(1),
        }
    }

    fn schedule(&self, path: PathBuf) -> bool {
        let generation = self.next_generation.fetch_add(1, Ordering::Relaxed);
        self.generations
            .lock()
            .unwrap()
            .insert(path.clone(), generation);
        let should_send = self.queued.lock().unwrap().insert(path.clone());
        if !should_send {
            return true;
        }
        match self.tx.try_send(path.clone()) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => {
                self.queued.lock().unwrap().remove(&path);
                self.generations.lock().unwrap().remove(&path);
                false
            }
        }
    }

    fn cancel(&self, path: &Path) {
        self.generations.lock().unwrap().remove(path);
        self.queued.lock().unwrap().remove(path);
    }
}

fn stable_worker(
    rx: Receiver<PathBuf>,
    source: String,
    on_detect: DetectCallback,
    generations: Arc<Mutex<HashMap<PathBuf, u64>>>,
    queued: Arc<Mutex<HashSet<PathBuf>>>,
) {
    while let Ok(path) = rx.recv() {
        loop {
            let generation = generations.lock().unwrap().get(&path).copied();
            let Some(generation) = generation else {
                queued.lock().unwrap().remove(&path);
                break;
            };
            let item = stable_detect(&path, &source);
            let latest = generations.lock().unwrap().get(&path).copied();
            if latest != Some(generation) {
                if latest.is_none() {
                    queued.lock().unwrap().remove(&path);
                    break;
                }
                continue;
            }
            generations.lock().unwrap().remove(&path);
            queued.lock().unwrap().remove(&path);
            if let Some(item) = item {
                on_detect(item);
            }
            break;
        }
    }
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
        self.stop_watch_tree(dir, false);
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
        self.stop_watch_tree(old, false);
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

    /// 按需浏览只允许访问已配置监控根目录之下的现存目录。
    pub fn is_allowed_directory(&self, dir: &str) -> bool {
        let Ok(candidate) = std::fs::canonicalize(dir) else {
            return false;
        };
        if !candidate.is_dir() {
            return false;
        }
        self.list_dirs().iter().any(|root| {
            std::fs::canonicalize(root)
                .map(|root| candidate.starts_with(root))
                .unwrap_or(false)
        })
    }

    /// 释放某个目录及后代的按需 watcher；监控根自身始终保留。
    pub fn stop_aux_watch_tree(&self, dir: &str) {
        self.stop_watch_tree(dir, true);
    }

    fn stop_watch_tree(&self, dir: &str, preserve_roots: bool) {
        let target = Path::new(dir);
        let roots = self.list_dirs();
        self.watchers.lock().unwrap().retain(|watched, _| {
            let path = Path::new(watched);
            if !path.starts_with(target) {
                return true;
            }
            preserve_roots && roots.iter().any(|root| Path::new(root) == path)
        });
    }

    /// 注册一个目录的 notify 监听。结构变化按短窗口批处理，稳定检测在独立有界队列执行。
    pub fn start_watch<F, G>(&self, dir: &str, on_detect: F, on_change: G) -> anyhow::Result<()>
    where
        F: Fn(DetectedItem) + Send + Sync + 'static,
        G: Fn(DirectoryChangeBatch) + Send + Sync + 'static,
    {
        let p = Path::new(dir);
        if !p.is_dir() {
            // 失效目录:跳过不阻断
            return Ok(());
        }
        if self.watchers.lock().unwrap().contains_key(dir) {
            return Ok(());
        }
        let (tx, rx) = sync_channel(RAW_EVENT_CAPACITY);
        let overflowed = Arc::new(AtomicBool::new(false));
        let callback_overflowed = overflowed.clone();
        let active = Arc::new(AtomicBool::new(true));
        let callback_active = active.clone();
        let mut watcher: RecommendedWatcher = notify::recommended_watcher(move |res| {
            if !callback_active.load(Ordering::Acquire) {
                return;
            }
            if matches!(tx.try_send(res), Err(TrySendError::Full(_))) {
                callback_overflowed.store(true, Ordering::Release);
            }
        })?;
        watcher.watch(p, RecursiveMode::NonRecursive)?;
        self.watchers.lock().unwrap().insert(
            dir.to_string(),
            WatchRegistration {
                _watcher: watcher,
                active: active.clone(),
            },
        );

        let source = p
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(dir)
            .to_string();
        let watch_dir = p.to_path_buf();
        let detect_active = active.clone();
        let on_detect: DetectCallback = Arc::new(move |item| {
            if detect_active.load(Ordering::Acquire) {
                on_detect(item);
            }
        });
        let on_change: Arc<dyn Fn(DirectoryChangeBatch) + Send + Sync> = Arc::new(on_change);
        std::thread::spawn(move || {
            let stable = StableScheduler::new(source.clone(), on_detect);
            while let Ok(first) = rx.recv() {
                let deadline = Instant::now() + EVENT_BATCH_WINDOW;
                let mut events = vec![];
                let mut force_rescan = overflowed.swap(false, Ordering::AcqRel);
                match first {
                    Ok(event) => events.push(event),
                    Err(_) => force_rescan = true,
                }
                loop {
                    let now = Instant::now();
                    if now >= deadline {
                        break;
                    }
                    match rx.recv_timeout(deadline - now) {
                        Ok(Ok(event)) => events.push(event),
                        Ok(Err(_)) => force_rescan = true,
                        Err(_) => break,
                    }
                }
                // The producer may overflow while this batch is being drained. Consume the flag
                // again so a quiet directory still reconciles immediately after dropped events.
                force_rescan |= overflowed.swap(false, Ordering::AcqRel);

                let normalized = normalize_events(&watch_dir, &source, events, force_rescan);
                for path in &normalized.cancel_stable {
                    stable.cancel(path);
                }
                let mut queue_overflowed = false;
                for path in normalized.schedule_stable {
                    if !stable.schedule(path) {
                        queue_overflowed = true;
                    }
                }
                let batch = if queue_overflowed {
                    rescan_batch(&watch_dir, &source)
                } else {
                    normalized.batch
                };
                if active.load(Ordering::Acquire) && !batch.changes.is_empty() {
                    on_change(batch);
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
                if let Some(item) = inventory_item(&path, &source) {
                    out.push(item);
                }
            }
        }
        sort_inventory(&mut out);
        out
    }
}

struct NormalizedEvents {
    batch: DirectoryChangeBatch,
    schedule_stable: Vec<PathBuf>,
    cancel_stable: Vec<PathBuf>,
}

fn normalize_events(
    watch_dir: &Path,
    source: &str,
    events: Vec<Event>,
    mut force_rescan: bool,
) -> NormalizedEvents {
    let mut changes = BTreeMap::<String, DirectoryChange>::new();
    let mut schedule = BTreeMap::<String, PathBuf>::new();
    let mut cancel = BTreeMap::<String, PathBuf>::new();

    for event in events {
        if event.need_rescan() {
            force_rescan = true;
        }
        match event.kind {
            EventKind::Access(_) => {}
            EventKind::Create(_) => {
                for path in event.paths {
                    if !is_top_level_path(watch_dir, &path) {
                        continue;
                    }
                    let path_key = path.to_string_lossy().into_owned();
                    if schedule.contains_key(&path_key) {
                        continue;
                    }
                    if let Some(node) = inventory_item(&path, source) {
                        let key = node.path.clone();
                        let needs_stable = node.kind != "dir";
                        changes.insert(key.clone(), DirectoryChange::Upsert { node });
                        if needs_stable {
                            schedule.insert(key, path);
                        }
                    }
                }
            }
            EventKind::Remove(_) => {
                for path in event.paths {
                    if !is_top_level_path(watch_dir, &path) {
                        continue;
                    }
                    let key = path.to_string_lossy().into_owned();
                    changes.insert(key.clone(), DirectoryChange::Remove { path: key.clone() });
                    cancel.insert(key, path);
                }
            }
            EventKind::Modify(ModifyKind::Name(RenameMode::Both)) => {
                if event.paths.len() != 2 {
                    force_rescan = true;
                    continue;
                }
                let old_path = &event.paths[0];
                let new_path = &event.paths[1];
                if !is_top_level_path(watch_dir, old_path)
                    || !is_top_level_path(watch_dir, new_path)
                {
                    continue;
                }
                let old_key = old_path.to_string_lossy().into_owned();
                cancel.insert(old_key.clone(), old_path.clone());
                if let Some(node) = inventory_item(new_path, source) {
                    let new_key = node.path.clone();
                    let needs_stable = node.kind != "dir";
                    changes.remove(&old_key);
                    changes.remove(&new_key);
                    changes.insert(
                        format!("rename:{old_key}"),
                        DirectoryChange::Rename {
                            old_path: old_key,
                            node,
                        },
                    );
                    if needs_stable {
                        schedule.insert(new_key, new_path.clone());
                    }
                } else {
                    force_rescan = true;
                }
            }
            EventKind::Modify(ModifyKind::Name(_)) => {
                // Some backends emit From/To as separate events and cannot reliably pair them.
                force_rescan = true;
                for path in event.paths {
                    if !is_top_level_path(watch_dir, &path) {
                        continue;
                    }
                    let key = path.to_string_lossy().into_owned();
                    if path.is_file() {
                        schedule.insert(key, path);
                    } else {
                        cancel.insert(key, path);
                    }
                }
            }
            EventKind::Modify(_) => {
                for path in event.paths {
                    if !is_top_level_path(watch_dir, &path) {
                        continue;
                    }
                    let key = path.to_string_lossy().into_owned();
                    if schedule.contains_key(&key) {
                        continue;
                    }
                    if let Some(node) = inventory_item(&path, source) {
                        let needs_stable = node.kind != "dir";
                        changes.insert(key.clone(), DirectoryChange::Upsert { node });
                        if needs_stable {
                            schedule.insert(key, path);
                        }
                    } else {
                        force_rescan = true;
                        cancel.insert(key, path);
                    }
                }
            }
            EventKind::Any | EventKind::Other => force_rescan = true,
        }
    }

    let batch = if force_rescan {
        rescan_batch(watch_dir, source)
    } else {
        DirectoryChangeBatch {
            watch_dir: watch_dir.to_string_lossy().into_owned(),
            changes: changes.into_values().collect(),
        }
    };
    NormalizedEvents {
        batch,
        schedule_stable: schedule.into_values().collect(),
        cancel_stable: cancel.into_values().collect(),
    }
}

fn rescan_batch(watch_dir: &Path, source: &str) -> DirectoryChangeBatch {
    let mut nodes = vec![];
    if let Ok(entries) = std::fs::read_dir(watch_dir) {
        for entry in entries.flatten() {
            if let Some(node) = inventory_item(&entry.path(), source) {
                nodes.push(node);
            }
        }
    }
    sort_inventory(&mut nodes);
    DirectoryChangeBatch {
        watch_dir: watch_dir.to_string_lossy().into_owned(),
        changes: vec![DirectoryChange::Rescan { nodes }],
    }
}

fn is_top_level_path(watch_dir: &Path, path: &Path) -> bool {
    path.parent() == Some(watch_dir)
}

fn sort_inventory(items: &mut [DetectedItem]) {
    items.sort_by_key(|item| (item.kind != "dir", item.name.to_lowercase()));
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

/// 目录树库存包含全部顶层普通文件；日志分类只影响展示样式与可打开性。
fn inventory_item(path: &Path, source: &str) -> Option<DetectedItem> {
    if path.is_dir() {
        let name = path.file_name()?.to_str()?.to_string();
        return Some(DetectedItem {
            path: path.to_str()?.to_string(),
            name,
            kind: "dir".into(),
            size: 0,
            source: source.to_string(),
            is_log: false,
        });
    }
    if !path.is_file() {
        return None;
    }
    let name = path.file_name()?.to_str()?.to_string();
    let size = std::fs::metadata(path).ok()?.len();
    let path_str = path.to_str()?.to_string();
    let archive = is_zip(path).unwrap_or(false);
    // 库存扫描不采样未知文件内容；稳定检测会异步补全其文本分类。
    let is_log = archive || is_log_name(&name);
    Some(DetectedItem {
        path: path_str,
        name,
        kind: if archive { "archive" } else { "file" }.into(),
        size,
        source: source.to_string(),
        is_log,
    })
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
            is_log: true,
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
            is_log: true,
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
    use notify::event::{CreateKind, DataChange, RemoveKind};
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
            is_log: true,
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
    fn inventory_includes_directories_and_binary_files_without_recursive_scanning() {
        let fixture = FixtureDir::new();
        fixture.write("server.log", b"log");
        fixture.write("image.bin", &[0, 1, 2, 3]);
        let nested = fixture.path.join("nested");
        std::fs::create_dir(&nested).unwrap();
        std::fs::write(nested.join("not-loaded.log"), b"nested").unwrap();
        let state = WatchState::new(fixture.path.join("config.json"));

        let items = state.scan_dir(fixture.path.to_str().unwrap());
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].kind, "dir");
        assert_eq!(items[0].name, "nested");
        assert!(items
            .iter()
            .any(|item| item.name == "server.log" && item.is_log));
        assert!(items
            .iter()
            .any(|item| item.name == "image.bin" && !item.is_log));
        assert!(!items.iter().any(|item| item.name == "not-loaded.log"));
    }

    #[test]
    fn directory_events_are_structural_and_skip_stable_detection() {
        let fixture = FixtureDir::new();
        let directory = fixture.path.join("new-folder");
        std::fs::create_dir(&directory).unwrap();
        let event = Event::new(EventKind::Create(CreateKind::Folder)).add_path(directory.clone());

        let normalized = normalize_events(&fixture.path, "test", vec![event], false);
        assert!(matches!(
            normalized.batch.changes.as_slice(),
            [DirectoryChange::Upsert { node }] if node.kind == "dir" && node.path == directory.to_string_lossy()
        ));
        assert!(normalized.schedule_stable.is_empty());
    }

    #[test]
    fn expanded_directory_watchers_are_constrained_and_released_by_subtree() {
        let fixture = FixtureDir::new();
        let nested = fixture.path.join("nested");
        let child = nested.join("child");
        let outside = std::env::temp_dir().join(format!(
            "logpeek-outside-{}-{}",
            std::process::id(),
            DIR_SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&child).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let state = WatchState::new(fixture.path.join("config.json"));
        state.add_dir(fixture.path.to_str().unwrap()).unwrap();

        assert!(state.is_allowed_directory(nested.to_str().unwrap()));
        assert!(!state.is_allowed_directory(outside.to_str().unwrap()));
        state
            .start_watch(fixture.path.to_str().unwrap(), |_| {}, |_| {})
            .unwrap();
        state
            .start_watch(nested.to_str().unwrap(), |_| {}, |_| {})
            .unwrap();
        state
            .start_watch(child.to_str().unwrap(), |_| {}, |_| {})
            .unwrap();
        assert_eq!(state.watchers.lock().unwrap().len(), 3);

        state.stop_aux_watch_tree(nested.to_str().unwrap());
        let watchers = state.watchers.lock().unwrap();
        assert_eq!(watchers.len(), 1);
        assert!(watchers.contains_key(fixture.path.to_str().unwrap()));
        drop(watchers);
        let _ = std::fs::remove_dir_all(outside);
    }

    #[test]
    fn normalizes_create_remove_modify_and_rename_events() {
        let fixture = FixtureDir::new();
        let created = fixture.write("created.log", b"created");
        let modified = fixture.write("modified.log", b"modified");
        let old = fixture.write("old.log", b"renamed");
        let renamed = fixture.path.join("renamed.log");
        std::fs::rename(&old, &renamed).unwrap();
        let removed = fixture.path.join("removed.log");
        let events = vec![
            Event::new(EventKind::Create(CreateKind::File)).add_path(created.clone()),
            Event::new(EventKind::Modify(ModifyKind::Data(DataChange::Content)))
                .add_path(modified.clone()),
            Event::new(EventKind::Modify(ModifyKind::Name(RenameMode::Both)))
                .add_path(old.clone())
                .add_path(renamed.clone()),
            Event::new(EventKind::Remove(RemoveKind::File)).add_path(removed.clone()),
        ];

        let normalized = normalize_events(&fixture.path, "test", events, false);
        assert!(normalized.batch.changes.iter().any(
            |change| matches!(change, DirectoryChange::Upsert { node } if node.path == created.to_string_lossy())
        ));
        assert!(normalized.batch.changes.iter().any(
            |change| matches!(change, DirectoryChange::Upsert { node } if node.path == modified.to_string_lossy())
        ));
        assert!(normalized.batch.changes.iter().any(
            |change| matches!(change, DirectoryChange::Rename { old_path, node } if old_path == &old.to_string_lossy() && node.path == renamed.to_string_lossy())
        ));
        assert!(normalized.batch.changes.iter().any(
            |change| matches!(change, DirectoryChange::Remove { path } if path == &removed.to_string_lossy())
        ));
    }

    #[test]
    fn ambiguous_rename_rescans_only_the_watched_directory() {
        let fixture = FixtureDir::new();
        let file = fixture.write("current.log", b"current");
        let event = Event::new(EventKind::Modify(ModifyKind::Name(RenameMode::From)))
            .add_path(fixture.path.join("old.log"));

        let normalized = normalize_events(&fixture.path, "test", vec![event], false);
        assert_eq!(normalized.batch.watch_dir, fixture.path.to_string_lossy());
        assert!(matches!(
            normalized.batch.changes.as_slice(),
            [DirectoryChange::Rescan { nodes }] if nodes.iter().any(|node| node.path == file.to_string_lossy())
        ));
    }

    #[test]
    fn repeated_modify_events_are_deduplicated_and_subdirectories_are_ignored() {
        let fixture = FixtureDir::new();
        let file = fixture.write("storm.log", b"storm");
        let nested = fixture.path.join("nested");
        std::fs::create_dir(&nested).unwrap();
        let nested_file = nested.join("ignored.log");
        std::fs::write(&nested_file, b"ignored").unwrap();
        let mut events = Vec::with_capacity(1_001);
        for _ in 0..1_000 {
            events.push(
                Event::new(EventKind::Modify(ModifyKind::Data(DataChange::Content)))
                    .add_path(file.clone()),
            );
        }
        events.push(Event::new(EventKind::Create(CreateKind::File)).add_path(nested_file));

        let normalized = normalize_events(&fixture.path, "test", events, false);
        assert_eq!(normalized.batch.changes.len(), 1);
        assert_eq!(normalized.schedule_stable, vec![file]);
    }

    #[test]
    fn cancelled_stable_detection_does_not_emit_a_stale_result() {
        let fixture = FixtureDir::new();
        let file = fixture.write("slow.log", b"complete");
        let (tx, rx) = sync_channel(1);
        let scheduler = StableScheduler::new(
            "test".into(),
            Arc::new(move |item| {
                let _ = tx.send(item);
            }),
        );

        assert!(scheduler.schedule(file.clone()));
        scheduler.cancel(&file);
        assert!(rx.recv_timeout(Duration::from_secs(2)).is_err());
    }

    #[test]
    fn native_watcher_reports_structure_before_stable_detection() {
        let fixture = FixtureDir::new();
        let state = WatchState::new(fixture.path.join("config.json"));
        let (detect_tx, detect_rx) = sync_channel(1);
        let (change_tx, change_rx) = sync_channel(4);
        state
            .start_watch(
                fixture.path.to_str().unwrap(),
                move |item| {
                    let _ = detect_tx.send(item);
                },
                move |batch| {
                    let _ = change_tx.send(batch);
                },
            )
            .unwrap();

        // FSEvents starts its stream asynchronously after `watch` returns.
        #[cfg(target_os = "macos")]
        std::thread::sleep(Duration::from_secs(1));

        let created = fixture.write("arriving.log", b"part one");
        // FSEvents may coalesce delivery for longer than Windows under loaded CI runners.
        let batch = change_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(batch.changes.iter().any(
            |change| matches!(change, DirectoryChange::Upsert { node } if node.path == created.to_string_lossy())
        ));
        assert!(detect_rx.try_recv().is_err());

        std::fs::remove_file(&created).unwrap();
        let batch = change_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(batch.changes.iter().any(
            |change| matches!(change, DirectoryChange::Remove { path } if path == &created.to_string_lossy())
        ));
        assert!(detect_rx.recv_timeout(Duration::from_secs(2)).is_err());
    }

    #[test]
    fn native_rename_sequence_produces_rename_or_consistent_rescan() {
        let fixture = FixtureDir::new();
        let old = fixture.write("before.log", b"complete");
        let new = fixture.path.join("after.log");
        let state = WatchState::new(fixture.path.join("config.json"));
        let (change_tx, change_rx) = sync_channel(4);
        state
            .start_watch(
                fixture.path.to_str().unwrap(),
                |_| {},
                move |batch| {
                    let _ = change_tx.send(batch);
                },
            )
            .unwrap();

        #[cfg(target_os = "macos")]
        std::thread::sleep(Duration::from_secs(1));

        std::fs::rename(&old, &new).unwrap();
        let batch = change_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(batch.changes.iter().any(|change| match change {
            DirectoryChange::Rename { old_path, node } => {
                old_path == &old.to_string_lossy() && node.path == new.to_string_lossy()
            }
            DirectoryChange::Rescan { nodes } => {
                nodes.iter().any(|node| node.path == new.to_string_lossy())
                    && nodes.iter().all(|node| node.path != old.to_string_lossy())
            }
            _ => false,
        }));
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

        assert!(state
            .start_watch(missing.to_str().unwrap(), |_| {}, |_| {})
            .is_ok());
        assert!(state.scan_dir(missing.to_str().unwrap()).is_empty());
    }
}
