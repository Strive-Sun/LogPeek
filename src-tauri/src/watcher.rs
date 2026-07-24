//! 目录监控:多目录 notify + 大小稳定检测 + 类型判定 + 配置持久化。

use crate::archive::{is_archive, is_archive_name, is_log_name};
use crate::macos_file_access::{MacOsFileAccess, WatchAccessStatus};
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
const ARRIVAL_OVERFLOW_CAPACITY: usize = 4096;
const STABLE_WORKER_COUNT: usize = 2;

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

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DroppedFileInfo {
    pub path: String,
    pub name: String,
    pub kind: String,
    pub watch_path: String,
    pub is_log: bool,
    pub already_monitored: bool,
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
    #[serde(default = "default_config_version")]
    pub version: u32,
    #[serde(default)]
    pub dirs: Vec<String>,
    #[serde(default)]
    pub macos_bookmarks: BTreeMap<String, String>,
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
            version: default_config_version(),
            dirs: Vec::new(),
            macos_bookmarks: BTreeMap::new(),
            suffixes: default_suffixes(),
            show_all: false,
        }
    }
}

fn default_config_version() -> u32 {
    2
}

fn default_suffixes() -> Vec<String> {
    vec![".log".into(), ".txt".into(), ".out".into()]
}

pub struct WatchState {
    config_path: PathBuf,
    pub config: Arc<Mutex<WatchConfig>>,
    watchers: Mutex<HashMap<String, WatchRegistration>>,
    arrival_watchers: Mutex<HashMap<String, WatchRegistration>>,
    stable_scheduler: Mutex<Option<Arc<StableScheduler>>>,
    file_access: MacOsFileAccess,
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

#[derive(Clone)]
struct PendingStable {
    generation: u64,
    source: String,
}

struct StableScheduler {
    tx: SyncSender<PathBuf>,
    generations: Arc<Mutex<HashMap<PathBuf, PendingStable>>>,
    queued: Arc<Mutex<HashSet<PathBuf>>>,
    next_generation: AtomicU64,
}

impl StableScheduler {
    fn new(on_detect: DetectCallback) -> Self {
        let (tx, rx) = sync_channel(STABLE_QUEUE_CAPACITY);
        let rx = Arc::new(Mutex::new(rx));
        let generations = Arc::new(Mutex::new(HashMap::new()));
        let queued = Arc::new(Mutex::new(HashSet::new()));
        for _ in 0..STABLE_WORKER_COUNT {
            let worker_rx = rx.clone();
            let worker_generations = generations.clone();
            let worker_queued = queued.clone();
            let worker_on_detect = on_detect.clone();
            std::thread::spawn(move || {
                stable_worker(
                    worker_rx,
                    worker_on_detect,
                    worker_generations,
                    worker_queued,
                );
            });
        }
        Self {
            tx,
            generations,
            queued,
            next_generation: AtomicU64::new(1),
        }
    }

    fn schedule(&self, path: PathBuf, source: String) -> bool {
        let generation = self.next_generation.fetch_add(1, Ordering::Relaxed);
        self.generations
            .lock()
            .unwrap()
            .insert(path.clone(), PendingStable { generation, source });
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
    }

    fn cancel_uncovered(&self, roots: &[PathBuf]) {
        self.generations
            .lock()
            .unwrap()
            .retain(|path, _| roots.iter().any(|root| path_is_within(path, root)));
    }
}

#[cfg(windows)]
fn path_is_within(path: &Path, root: &Path) -> bool {
    let path = path
        .components()
        .map(|component| component.as_os_str().to_string_lossy().to_lowercase())
        .collect::<Vec<_>>();
    let root = root
        .components()
        .map(|component| component.as_os_str().to_string_lossy().to_lowercase())
        .collect::<Vec<_>>();
    path.starts_with(&root)
}

#[cfg(not(windows))]
fn path_is_within(path: &Path, root: &Path) -> bool {
    path.starts_with(root)
}

fn canonical_or_original(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

#[cfg(windows)]
fn user_facing_path(path: &Path) -> PathBuf {
    let value = path.to_string_lossy();
    if let Some(rest) = value.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{rest}"));
    }
    if let Some(rest) = value.strip_prefix(r"\\?\") {
        return PathBuf::from(rest);
    }
    path.to_path_buf()
}

#[cfg(not(windows))]
fn user_facing_path(path: &Path) -> PathBuf {
    path.to_path_buf()
}

fn minimal_coverage_roots(dirs: &[String]) -> Vec<PathBuf> {
    let mut candidates = dirs
        .iter()
        .map(|dir| {
            let original = PathBuf::from(dir);
            let comparable = canonical_or_original(&original);
            (original, comparable)
        })
        .collect::<Vec<_>>();
    candidates.sort_by_key(|(_, comparable)| comparable.components().count());

    let mut selected = Vec::<(PathBuf, PathBuf)>::new();
    for (original, comparable) in candidates {
        if selected
            .iter()
            .any(|(_, root)| path_is_within(&comparable, root))
        {
            continue;
        }
        selected.push((original, comparable));
    }
    selected.into_iter().map(|(original, _)| original).collect()
}

fn source_label(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_else(|| path.to_str().unwrap_or("LogCrate"))
        .to_string()
}

fn source_for_path(config: &WatchConfig, path: &Path, fallback: &Path) -> String {
    config
        .dirs
        .iter()
        .map(PathBuf::from)
        .filter(|root| path_is_within(path, root))
        .max_by_key(|root| root.components().count())
        .as_deref()
        .map(source_label)
        .unwrap_or_else(|| source_label(fallback))
}

fn is_arrival_candidate(config: &WatchConfig, path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }
    if config.show_all {
        return true;
    }
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    let lower = name.to_lowercase();
    is_archive_name(&lower)
        || config
            .suffixes
            .iter()
            .any(|suffix| lower.ends_with(&suffix.to_lowercase()))
}

fn normalize_descendant_path(root: &Path, path: &Path) -> Option<PathBuf> {
    if path_is_within(path, root) {
        return Some(path.to_path_buf());
    }

    let canonical_root = std::fs::canonicalize(root).ok()?;
    let canonical_path = if path.exists() {
        std::fs::canonicalize(path).ok()?
    } else {
        let parent = std::fs::canonicalize(path.parent()?).ok()?;
        parent.join(path.file_name()?)
    };
    let relative = canonical_path.strip_prefix(canonical_root).ok()?;
    Some(root.join(relative))
}

struct ArrivalEvents {
    schedule: Vec<PathBuf>,
    cancel: Vec<PathBuf>,
    degraded: bool,
}

fn add_arrival_schedule(
    schedule: &mut BTreeMap<String, PathBuf>,
    cancel: &mut BTreeMap<String, PathBuf>,
    path: PathBuf,
) {
    let key = path.to_string_lossy().into_owned();
    cancel.remove(&key);
    schedule.insert(key, path);
}

fn add_arrival_cancel(
    schedule: &mut BTreeMap<String, PathBuf>,
    cancel: &mut BTreeMap<String, PathBuf>,
    path: PathBuf,
) {
    let key = path.to_string_lossy().into_owned();
    schedule.remove(&key);
    cancel.insert(key, path);
}

fn normalize_arrival_events(root: &Path, events: Vec<Event>) -> ArrivalEvents {
    let mut schedule = BTreeMap::<String, PathBuf>::new();
    let mut cancel = BTreeMap::<String, PathBuf>::new();
    let mut degraded = false;

    for event in events {
        degraded |= event.need_rescan();
        match event.kind {
            EventKind::Access(_) => {}
            EventKind::Create(_) | EventKind::Modify(ModifyKind::Data(_)) => {
                for path in event.paths {
                    if let Some(path) = normalize_descendant_path(root, &path) {
                        add_arrival_schedule(&mut schedule, &mut cancel, path);
                    }
                }
            }
            EventKind::Remove(_) => {
                for path in event.paths {
                    if let Some(path) = normalize_descendant_path(root, &path) {
                        add_arrival_cancel(&mut schedule, &mut cancel, path);
                    }
                }
            }
            EventKind::Modify(ModifyKind::Name(RenameMode::Both)) => {
                if event.paths.len() != 2 {
                    degraded = true;
                    continue;
                }
                if let Some(path) = normalize_descendant_path(root, &event.paths[0]) {
                    add_arrival_cancel(&mut schedule, &mut cancel, path);
                }
                if let Some(path) = normalize_descendant_path(root, &event.paths[1]) {
                    add_arrival_schedule(&mut schedule, &mut cancel, path);
                }
            }
            EventKind::Modify(ModifyKind::Name(RenameMode::From)) => {
                for path in event.paths {
                    if let Some(path) = normalize_descendant_path(root, &path) {
                        add_arrival_cancel(&mut schedule, &mut cancel, path);
                    }
                }
            }
            EventKind::Modify(ModifyKind::Name(_)) | EventKind::Modify(_) => {
                for path in event.paths {
                    if let Some(path) = normalize_descendant_path(root, &path) {
                        if path.exists() {
                            add_arrival_schedule(&mut schedule, &mut cancel, path);
                        } else {
                            add_arrival_cancel(&mut schedule, &mut cancel, path);
                        }
                    }
                }
            }
            EventKind::Any | EventKind::Other => {
                degraded = true;
                for path in event.paths {
                    if let Some(path) = normalize_descendant_path(root, &path) {
                        if path.exists() {
                            add_arrival_schedule(&mut schedule, &mut cancel, path);
                        } else {
                            add_arrival_cancel(&mut schedule, &mut cancel, path);
                        }
                    }
                }
            }
        }
    }

    ArrivalEvents {
        schedule: schedule.into_values().collect(),
        cancel: cancel.into_values().collect(),
        degraded,
    }
}

fn build_arrival_watcher(
    root: &Path,
    config: Arc<Mutex<WatchConfig>>,
    scheduler: Arc<StableScheduler>,
) -> anyhow::Result<WatchRegistration> {
    let (tx, rx) = sync_channel(RAW_EVENT_CAPACITY);
    let overflow_paths = Arc::new(Mutex::new(HashSet::<PathBuf>::new()));
    let callback_overflow_paths = overflow_paths.clone();
    let degraded = Arc::new(AtomicBool::new(false));
    let callback_degraded = degraded.clone();
    let active = Arc::new(AtomicBool::new(true));
    let callback_active = active.clone();

    let mut watcher: RecommendedWatcher =
        notify::recommended_watcher(move |result: notify::Result<Event>| {
            if !callback_active.load(Ordering::Acquire) {
                return;
            }
            match tx.try_send(result) {
                Ok(()) => {}
                Err(TrySendError::Full(result)) => match result {
                    Ok(event) => {
                        let mut paths = callback_overflow_paths.lock().unwrap();
                        for path in event.paths {
                            if paths.len() >= ARRIVAL_OVERFLOW_CAPACITY {
                                callback_degraded.store(true, Ordering::Release);
                                break;
                            }
                            paths.insert(path);
                        }
                    }
                    Err(_) => callback_degraded.store(true, Ordering::Release),
                },
                Err(TrySendError::Disconnected(_)) => {}
            }
        })?;
    watcher.watch(root, RecursiveMode::Recursive)?;

    let watch_root = root.to_path_buf();
    let worker_root = watch_root.clone();
    let worker_active = active.clone();
    std::thread::spawn(move || {
        while let Ok(first) = rx.recv() {
            if !worker_active.load(Ordering::Acquire) {
                break;
            }
            let deadline = Instant::now() + EVENT_BATCH_WINDOW;
            let mut events = Vec::new();
            let mut batch_degraded = degraded.swap(false, Ordering::AcqRel);
            match first {
                Ok(event) => events.push(event),
                Err(_) => batch_degraded = true,
            }
            loop {
                let now = Instant::now();
                if now >= deadline {
                    break;
                }
                match rx.recv_timeout(deadline - now) {
                    Ok(Ok(event)) => events.push(event),
                    Ok(Err(_)) => batch_degraded = true,
                    Err(_) => break,
                }
            }

            let mut normalized = normalize_arrival_events(&worker_root, events);
            batch_degraded |= normalized.degraded;
            let overflow = {
                let mut paths = overflow_paths.lock().unwrap();
                std::mem::take(&mut *paths)
            };
            for raw_path in overflow {
                let Some(path) = normalize_descendant_path(&worker_root, &raw_path) else {
                    continue;
                };
                if path.exists() {
                    normalized.schedule.push(path);
                } else {
                    normalized.cancel.push(path);
                }
            }

            for path in normalized.cancel {
                scheduler.cancel(&path);
            }
            let config_snapshot = config.lock().unwrap().clone();
            let mut scheduled = HashSet::new();
            for path in normalized.schedule {
                if !scheduled.insert(path.clone()) || !is_arrival_candidate(&config_snapshot, &path)
                {
                    continue;
                }
                let source = source_for_path(&config_snapshot, &path, &worker_root);
                if !scheduler.schedule(path, source) {
                    batch_degraded = true;
                }
            }

            if batch_degraded {
                eprintln!(
                    "recursive arrival watcher degraded for {}; bounded queues prevented a full-root rescan",
                    worker_root.display()
                );
            }
            if !worker_active.load(Ordering::Acquire) {
                break;
            }
        }
    });

    Ok(WatchRegistration {
        _watcher: watcher,
        active,
    })
}

fn stable_worker(
    rx: Arc<Mutex<Receiver<PathBuf>>>,
    on_detect: DetectCallback,
    generations: Arc<Mutex<HashMap<PathBuf, PendingStable>>>,
    queued: Arc<Mutex<HashSet<PathBuf>>>,
) {
    loop {
        let candidate = {
            let receiver = rx.lock().unwrap();
            receiver.recv()
        };
        let Ok(candidate) = candidate else {
            break;
        };
        let path = candidate;
        loop {
            let pending = generations.lock().unwrap().get(&path).cloned();
            let Some(pending) = pending else {
                queued.lock().unwrap().remove(&path);
                break;
            };
            let item = stable_detect(&path, &pending.source);
            let latest = generations.lock().unwrap().get(&path).cloned();
            if latest.as_ref().map(|state| state.generation) != Some(pending.generation) {
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
        let mut config = load_config(&config_path).unwrap_or_default();
        let original_dirs = config.dirs.clone();
        config.dirs = minimal_coverage_roots(&config.dirs)
            .into_iter()
            .map(|root| root.to_string_lossy().into_owned())
            .collect();
        config.version = default_config_version();
        config
            .macos_bookmarks
            .retain(|path, _| config.dirs.iter().any(|dir| dir == path));
        let state = Arc::new(Self {
            config_path,
            config: Arc::new(Mutex::new(config)),
            watchers: Mutex::new(HashMap::new()),
            arrival_watchers: Mutex::new(HashMap::new()),
            stable_scheduler: Mutex::new(None),
            file_access: MacOsFileAccess::new(),
        });
        state.restore_persisted_access();
        if state.list_dirs() != original_dirs {
            state.persist();
        }
        state
    }

    fn restore_persisted_access(&self) {
        let bookmarks = self.config.lock().unwrap().macos_bookmarks.clone();
        let mut refreshed = Vec::new();
        for (path, bookmark) in bookmarks {
            match self
                .file_access
                .restore_bookmark(Path::new(&path), &bookmark)
            {
                Ok(restored) => {
                    if let Some(value) = restored.refreshed_bookmark {
                        refreshed.push((path, value));
                    }
                }
                Err(_) => self.file_access.mark_needs_authorization(Path::new(&path)),
            }
        }
        if refreshed.is_empty() {
            return;
        }
        let mut config = self.config.lock().unwrap();
        for (path, bookmark) in refreshed {
            config.macos_bookmarks.insert(path, bookmark);
        }
        drop(config);
        self.persist();
    }

    pub fn list_dirs(&self) -> Vec<String> {
        self.config.lock().unwrap().dirs.clone()
    }

    pub fn access_status(&self, dir: &str) -> WatchAccessStatus {
        let has_bookmark = self
            .config
            .lock()
            .unwrap()
            .macos_bookmarks
            .contains_key(dir);
        self.file_access.status(Path::new(dir), has_bookmark)
    }

    pub fn is_watched_path(&self, path: &Path) -> bool {
        let path = canonical_or_original(path);
        self.config
            .lock()
            .unwrap()
            .dirs
            .iter()
            .map(|root| canonical_or_original(Path::new(root)))
            .any(|root| path_is_within(&path, &root))
    }

    pub fn inspect_dropped_file(&self, path: &str) -> anyhow::Result<DroppedFileInfo> {
        let canonical = std::fs::canonicalize(path)
            .map_err(|_| anyhow::anyhow!("文件不存在或无法访问: {path}"))?;
        let metadata = std::fs::metadata(&canonical)
            .map_err(|_| anyhow::anyhow!("无法读取文件信息: {path}"))?;
        let normalized = user_facing_path(&canonical);
        let name = normalized
            .file_name()
            .and_then(|name| name.to_str())
            .map(str::to_string)
            .unwrap_or_else(|| normalized.to_string_lossy().into_owned());

        if metadata.is_dir() {
            std::fs::read_dir(&canonical).map_err(|_| anyhow::anyhow!("目录不可读: {path}"))?;
            return Ok(DroppedFileInfo {
                path: normalized.to_string_lossy().into_owned(),
                name,
                kind: "directory".into(),
                watch_path: normalized.to_string_lossy().into_owned(),
                is_log: false,
                already_monitored: self.is_watched_path(&canonical),
            });
        }
        if !metadata.is_file() {
            anyhow::bail!("仅支持拖入单个普通文件或文件夹");
        }
        std::fs::File::open(&canonical).map_err(|_| anyhow::anyhow!("文件不可读: {path}"))?;

        let detected = classify(&canonical, "drop");
        let kind = detected
            .as_ref()
            .map(|item| item.kind.clone())
            .unwrap_or_else(|| "file".into());
        let is_log = detected
            .as_ref()
            .is_some_and(|item| item.kind == "file" && item.is_log);
        let watch_path = normalized
            .parent()
            .ok_or_else(|| anyhow::anyhow!("无法确定文件所在目录"))?;

        Ok(DroppedFileInfo {
            path: normalized.to_string_lossy().into_owned(),
            name,
            kind,
            watch_path: watch_path.to_string_lossy().into_owned(),
            is_log,
            already_monitored: self.is_watched_path(&canonical),
        })
    }

    pub fn is_structure_watched(&self, dir: &Path) -> bool {
        self.watchers.lock().unwrap().keys().any(|watched| {
            let watched = Path::new(watched);
            path_is_within(dir, watched) && path_is_within(watched, dir)
        })
    }

    fn rebuild_arrival_watchers(&self) -> anyhow::Result<()> {
        let Some(scheduler) = self.stable_scheduler.lock().unwrap().clone() else {
            return Ok(());
        };
        let dirs = self.list_dirs();
        let configured_roots = dirs.iter().map(PathBuf::from).collect::<Vec<_>>();
        let desired = minimal_coverage_roots(&dirs);
        let desired_keys = desired
            .iter()
            .map(|root| root.to_string_lossy().into_owned())
            .collect::<HashSet<_>>();

        let mut watchers = self.arrival_watchers.lock().unwrap();
        watchers.retain(|root, _| desired_keys.contains(root));
        for root in desired {
            let key = root.to_string_lossy().into_owned();
            if watchers.contains_key(&key) || !root.is_dir() {
                continue;
            }
            let registration =
                build_arrival_watcher(&root, self.config.clone(), scheduler.clone())?;
            watchers.insert(key, registration);
        }
        drop(watchers);
        scheduler.cancel_uncovered(&configured_roots);
        Ok(())
    }

    #[allow(dead_code)]
    pub fn config(&self) -> WatchConfig {
        self.config.lock().unwrap().clone()
    }

    fn persist(&self) {
        let cfg = self.config.lock().unwrap().clone();
        if let Ok(json) = serde_json::to_string_pretty(&cfg) {
            #[cfg(target_os = "macos")]
            {
                let temporary = self.config_path.with_extension(format!(
                    "json.tmp-{}-{}",
                    std::process::id(),
                    cfg.version
                ));
                if std::fs::write(&temporary, json.as_bytes()).is_ok() {
                    if std::fs::rename(&temporary, &self.config_path).is_ok() {
                        return;
                    }
                    let _ = std::fs::remove_file(temporary);
                }
            }
            let _ = std::fs::write(&self.config_path, json);
        }
    }

    /// 添加监控目录:校验存在性 → 持久化 →(调用方负责 emit 初次扫描)
    pub fn add_dir(&self, dir: &str) -> anyhow::Result<bool> {
        self.add_dir_with_bookmark(dir, None)
    }

    pub fn add_user_selected_dir(&self, dir: &str) -> anyhow::Result<bool> {
        let canonical = std::fs::canonicalize(dir)
            .map_err(|error| anyhow::anyhow!(crate::macos_file_access::command_error(&error)))?;
        let bookmark = if cfg!(target_os = "macos") {
            Some(
                self.file_access
                    .create_bookmark(&canonical)
                    .map_err(anyhow::Error::msg)?,
            )
        } else {
            None
        };
        self.add_dir_with_bookmark(canonical.to_string_lossy().as_ref(), bookmark)
    }

    fn add_dir_with_bookmark(&self, dir: &str, bookmark: Option<String>) -> anyhow::Result<bool> {
        let canonical = std::fs::canonicalize(dir)
            .map_err(|error| anyhow::anyhow!(crate::macos_file_access::command_error(&error)))?;
        if !canonical.is_dir() {
            anyhow::bail!("目录不存在或不可读: {dir}");
        }
        let normalized = user_facing_path(&canonical).to_string_lossy().into_owned();
        let mut removed = Vec::new();
        let mut exact_existing = false;
        {
            let mut cfg = self.config.lock().unwrap();
            if cfg
                .dirs
                .iter()
                .map(|root| canonical_or_original(Path::new(root)))
                .any(|root| path_is_within(&canonical, &root))
            {
                exact_existing = cfg
                    .dirs
                    .iter()
                    .any(|root| canonical_or_original(Path::new(root)) == canonical);
                if exact_existing {
                    if let Some(value) = bookmark.clone() {
                        cfg.macos_bookmarks.insert(normalized.clone(), value);
                    }
                } else {
                    return Ok(false);
                }
            } else {
                cfg.dirs.retain(|root| {
                    let redundant =
                        path_is_within(&canonical_or_original(Path::new(root)), &canonical);
                    if redundant {
                        removed.push(root.clone());
                    }
                    !redundant
                });
                for root in &removed {
                    cfg.macos_bookmarks.remove(root);
                }
                cfg.dirs.push(normalized.clone());
                if let Some(value) = bookmark.clone() {
                    cfg.macos_bookmarks.insert(normalized.clone(), value);
                }
            }
        }
        for root in removed {
            self.stop_watch_tree(&root);
            self.file_access.release(Path::new(&root));
        }
        if let Some(value) = bookmark {
            self.file_access.release(&canonical);
            match self.file_access.restore_bookmark(&canonical, &value) {
                Ok(restored) => {
                    if let Some(refreshed) = restored.refreshed_bookmark {
                        self.config
                            .lock()
                            .unwrap()
                            .macos_bookmarks
                            .insert(normalized.clone(), refreshed);
                    }
                }
                Err(_) => self.file_access.mark_needs_authorization(&canonical),
            }
        }
        self.persist();
        Ok(!exact_existing)
    }

    pub fn reauthorize_dir(&self, existing: &str, selected: &str) -> anyhow::Result<()> {
        let existing_path = canonical_or_original(Path::new(existing));
        let selected_path = std::fs::canonicalize(selected)
            .map_err(|error| anyhow::anyhow!(crate::macos_file_access::command_error(&error)))?;
        if existing_path != selected_path {
            anyhow::bail!("BOOKMARK_IDENTITY_MISMATCH:请选择原监控目录");
        }
        let bookmark = self
            .file_access
            .create_bookmark(&selected_path)
            .map_err(anyhow::Error::msg)?;
        self.add_dir_with_bookmark(selected, Some(bookmark))?;
        Ok(())
    }

    pub fn remove_dir(&self, dir: &str) {
        {
            let mut cfg = self.config.lock().unwrap();
            cfg.dirs.retain(|d| d != dir);
            cfg.macos_bookmarks.remove(dir);
        }
        self.stop_watch_tree(dir);
        self.file_access.release(Path::new(dir));
        let _ = self.rebuild_arrival_watchers();
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
        let bookmark = {
            let mut cfg = self.config.lock().unwrap();
            cfg.macos_bookmarks.remove(old)
        };
        {
            let mut cfg = self.config.lock().unwrap();
            for d in cfg.dirs.iter_mut() {
                if d == old {
                    *d = dst_str.clone();
                }
            }
            if let Some(value) = bookmark.clone() {
                cfg.macos_bookmarks.insert(dst_str.clone(), value);
            }
        }
        self.file_access.release(Path::new(old));
        if let Some(value) = bookmark {
            let _ = self.file_access.restore_bookmark(&dst, &value);
        }
        self.stop_watch_tree(old);
        let _ = self.rebuild_arrival_watchers();
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
        self.stop_watch_tree(dir);
    }

    fn stop_watch_tree(&self, dir: &str) {
        let target = canonical_or_original(Path::new(dir));
        let roots = self
            .list_dirs()
            .iter()
            .map(|root| canonical_or_original(Path::new(root)))
            .collect::<Vec<_>>();
        self.watchers.lock().unwrap().retain(|watched, _| {
            let path = canonical_or_original(Path::new(watched));
            if !path_is_within(&path, &target) {
                return true;
            }
            roots
                .iter()
                .any(|root| path_is_within(&path, root) && path_is_within(root, &path))
                && path.is_dir()
        });
    }

    /// 注册一个目录的非递归结构监听；监控根另行建立递归到达监听。
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
        let is_configured_root = self
            .list_dirs()
            .iter()
            .any(|root| canonical_or_original(Path::new(root)) == canonical_or_original(p));
        if is_configured_root {
            let mut scheduler = self.stable_scheduler.lock().unwrap();
            if scheduler.is_none() {
                *scheduler = Some(Arc::new(StableScheduler::new(Arc::new(on_detect))));
            }
            drop(scheduler);
            self.rebuild_arrival_watchers()?;
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
        let on_change: Arc<dyn Fn(DirectoryChangeBatch) + Send + Sync> = Arc::new(on_change);
        std::thread::spawn(move || {
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
                let batch = normalized.batch;
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
}

fn normalize_events(
    watch_dir: &Path,
    source: &str,
    events: Vec<Event>,
    mut force_rescan: bool,
) -> NormalizedEvents {
    let mut changes = BTreeMap::<String, DirectoryChange>::new();

    for event in events {
        if event.need_rescan() {
            force_rescan = true;
        }
        match event.kind {
            EventKind::Access(_) => {}
            EventKind::Create(_) => {
                for path in event.paths {
                    let Some(path) = normalize_top_level_path(watch_dir, &path) else {
                        continue;
                    };
                    if let Some(node) = inventory_item(&path, source) {
                        let key = node.path.clone();
                        changes.insert(key.clone(), DirectoryChange::Upsert { node });
                    }
                }
            }
            EventKind::Remove(_) => {
                for path in event.paths {
                    let Some(path) = normalize_top_level_path(watch_dir, &path) else {
                        continue;
                    };
                    let key = path.to_string_lossy().into_owned();
                    changes.insert(key.clone(), DirectoryChange::Remove { path: key.clone() });
                }
            }
            EventKind::Modify(ModifyKind::Name(RenameMode::Both)) => {
                if event.paths.len() != 2 {
                    force_rescan = true;
                    continue;
                }
                let Some(old_path) = normalize_top_level_path(watch_dir, &event.paths[0]) else {
                    continue;
                };
                let Some(new_path) = normalize_top_level_path(watch_dir, &event.paths[1]) else {
                    continue;
                };
                let old_key = old_path.to_string_lossy().into_owned();
                if let Some(node) = inventory_item(&new_path, source) {
                    let new_key = node.path.clone();
                    changes.remove(&old_key);
                    changes.remove(&new_key);
                    changes.insert(
                        format!("rename:{old_key}"),
                        DirectoryChange::Rename {
                            old_path: old_key,
                            node,
                        },
                    );
                } else {
                    force_rescan = true;
                }
            }
            EventKind::Modify(ModifyKind::Name(_)) => {
                // Some backends emit From/To as separate events and cannot reliably pair them.
                if event
                    .paths
                    .iter()
                    .any(|path| normalize_top_level_path(watch_dir, path).is_some())
                {
                    force_rescan = true;
                }
            }
            EventKind::Modify(_) => {
                for path in event.paths {
                    let Some(path) = normalize_top_level_path(watch_dir, &path) else {
                        continue;
                    };
                    let key = path.to_string_lossy().into_owned();
                    if let Some(node) = inventory_item(&path, source) {
                        changes.insert(key.clone(), DirectoryChange::Upsert { node });
                    } else {
                        force_rescan = true;
                    }
                }
            }
            EventKind::Any | EventKind::Other => {
                if event.paths.is_empty()
                    || event
                        .paths
                        .iter()
                        .any(|path| normalize_top_level_path(watch_dir, path).is_some())
                {
                    force_rescan = true;
                }
            }
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
    NormalizedEvents { batch }
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

fn normalize_top_level_path(watch_dir: &Path, path: &Path) -> Option<PathBuf> {
    let parent = path.parent()?;
    if parent == watch_dir {
        return Some(path.to_path_buf());
    }
    // FSEvents canonicalizes `/var/...` to `/private/var/...`. Compare canonical parents,
    // then rebuild the path with the registered directory so frontend node IDs stay stable.
    let canonical_parent = std::fs::canonicalize(parent).ok()?;
    let canonical_watch_dir = std::fs::canonicalize(watch_dir).ok()?;
    if canonical_parent != canonical_watch_dir {
        return None;
    }
    Some(watch_dir.join(path.file_name()?))
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
    let archive = is_archive(path).unwrap_or(false);
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

/// 类型判定:受支持归档 / 裸文本日志 / 其余忽略
fn classify(path: &Path, source: &str) -> Option<DetectedItem> {
    let name = path.file_name()?.to_str()?.to_string();
    let size = std::fs::metadata(path).ok()?.len();
    let path_str = path.to_str()?.to_string();

    if is_archive(path).unwrap_or(false) {
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
                "logcrate-watcher-test-{}-{}",
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
    fn dropped_path_inspection_accepts_archives_logs_arbitrary_files_and_directories() {
        let fixture = FixtureDir::new();
        let state = WatchState::new(fixture.path.join("config.json"));
        let zip = fixture.write("download.zip", b"PK\x03\x04");
        let log = fixture.write("server.log", b"one\n");
        let txt = fixture.write("notes.txt", b"two\n");
        let json = fixture.write("event.json", br#"{"ok":true}"#);
        let sampled = fixture.write("trace.data", b"plain sampled text\n");
        let binary = fixture.write("image.bin", &[0, 1, 2, 3]);
        let directory = fixture.path.join("nested");
        std::fs::create_dir(&directory).unwrap();

        let zip_info = state.inspect_dropped_file(zip.to_str().unwrap()).unwrap();
        assert_eq!(zip_info.kind, "archive");
        assert_eq!(zip_info.name, "download.zip");
        assert!(!zip_info.is_log);
        assert!(!zip_info.already_monitored);

        for path in [log, txt, json, sampled] {
            let info = state.inspect_dropped_file(path.to_str().unwrap()).unwrap();
            assert_eq!(info.kind, "file");
            assert!(info.is_log);
            assert_eq!(
                Path::new(&info.watch_path),
                user_facing_path(&std::fs::canonicalize(&fixture.path).unwrap())
            );
        }

        let binary_info = state
            .inspect_dropped_file(binary.to_str().unwrap())
            .unwrap();
        assert_eq!(binary_info.kind, "file");
        assert!(!binary_info.is_log);

        let directory_info = state
            .inspect_dropped_file(directory.to_str().unwrap())
            .unwrap();
        assert_eq!(directory_info.kind, "directory");
        assert_eq!(directory_info.path, directory_info.watch_path);
        assert!(!directory_info.is_log);
    }

    #[test]
    fn dropped_path_inspection_rejects_missing_paths() {
        let fixture = FixtureDir::new();
        let state = WatchState::new(fixture.path.join("config.json"));
        let missing = fixture.path.join("missing.log");

        assert!(state
            .inspect_dropped_file(missing.to_str().unwrap())
            .is_err());
    }

    #[test]
    fn dropped_file_inspection_detects_coverage_from_a_parent_watch_root() {
        let fixture = FixtureDir::new();
        let nested = fixture.path.join("nested").join("deep");
        std::fs::create_dir_all(&nested).unwrap();
        let log = nested.join("server.log");
        std::fs::write(&log, b"log\n").unwrap();
        let state = WatchState::new(fixture.path.join("config.json"));
        state.add_dir(fixture.path.to_str().unwrap()).unwrap();

        let info = state.inspect_dropped_file(log.to_str().unwrap()).unwrap();
        assert!(info.already_monitored);
        assert!(state.is_watched_path(&log));
    }

    #[cfg(windows)]
    #[test]
    fn user_facing_windows_paths_remove_verbatim_prefixes() {
        assert_eq!(
            user_facing_path(Path::new(r"\\?\D:\logs\server.log")),
            PathBuf::from(r"D:\logs\server.log")
        );
        assert_eq!(
            user_facing_path(Path::new(r"\\?\UNC\server\share\server.log")),
            PathBuf::from(r"\\server\share\server.log")
        );
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
    }

    #[cfg(unix)]
    #[test]
    fn canonical_event_paths_are_rebased_to_the_registered_directory() {
        use std::os::unix::fs::symlink;

        let fixture = FixtureDir::new();
        let alias = fixture.path.with_extension("alias");
        symlink(&fixture.path, &alias).unwrap();
        let real_file = fixture.write("created.log", b"created");

        let normalized = normalize_top_level_path(&alias, &real_file).unwrap();
        assert_eq!(normalized, alias.join("created.log"));
        let _ = std::fs::remove_file(alias);
    }

    #[cfg(unix)]
    #[test]
    fn recursive_canonical_event_paths_preserve_the_registered_root() {
        use std::os::unix::fs::symlink;

        let fixture = FixtureDir::new();
        let nested = fixture.path.join("nested");
        std::fs::create_dir(&nested).unwrap();
        let real_file = nested.join("created.log");
        std::fs::write(&real_file, b"created").unwrap();
        let alias = fixture.path.with_extension("recursive-alias");
        symlink(&fixture.path, &alias).unwrap();

        let normalized = normalize_descendant_path(&alias, &real_file).unwrap();
        assert_eq!(normalized, alias.join("nested").join("created.log"));
        let _ = std::fs::remove_file(alias);
    }

    #[test]
    fn expanded_directory_watchers_are_constrained_and_released_by_subtree() {
        let fixture = FixtureDir::new();
        let nested = fixture.path.join("nested");
        let child = nested.join("child");
        let outside = std::env::temp_dir().join(format!(
            "logcrate-outside-{}-{}",
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
        assert_eq!(state.arrival_watchers.lock().unwrap().len(), 1);

        state.stop_aux_watch_tree(nested.to_str().unwrap());
        let watchers = state.watchers.lock().unwrap();
        assert_eq!(watchers.len(), 1);
        assert!(watchers.contains_key(fixture.path.to_str().unwrap()));
        drop(watchers);
        let _ = std::fs::remove_dir_all(outside);
    }

    #[test]
    fn overlapping_roots_are_not_stored_or_watched_twice() {
        let fixture = FixtureDir::new();
        let nested = fixture.path.join("nested");
        std::fs::create_dir(&nested).unwrap();
        let state = WatchState::new(fixture.path.join("config.json"));
        assert!(state.add_dir(fixture.path.to_str().unwrap()).unwrap());
        assert!(!state.add_dir(nested.to_str().unwrap()).unwrap());
        assert_eq!(state.list_dirs().len(), 1);

        state
            .start_watch(fixture.path.to_str().unwrap(), |_| {}, |_| {})
            .unwrap();
        let roots = minimal_coverage_roots(&state.list_dirs());
        assert_eq!(roots.len(), 1);
        assert_eq!(state.arrival_watchers.lock().unwrap().len(), 1);
    }

    #[test]
    fn adding_a_parent_root_replaces_existing_descendant_roots() {
        let fixture = FixtureDir::new();
        let nested = fixture.path.join("nested");
        std::fs::create_dir(&nested).unwrap();
        let state = WatchState::new(fixture.path.join("config.json"));

        assert!(state.add_dir(nested.to_str().unwrap()).unwrap());
        state
            .start_watch(nested.to_str().unwrap(), |_| {}, |_| {})
            .unwrap();
        assert_eq!(state.watchers.lock().unwrap().len(), 1);
        assert!(state.add_dir(fixture.path.to_str().unwrap()).unwrap());
        let dirs = state.list_dirs();
        assert_eq!(dirs.len(), 1);
        assert!(state.watchers.lock().unwrap().is_empty());
        assert!(path_is_within(
            &std::fs::canonicalize(&nested).unwrap(),
            &std::fs::canonicalize(&dirs[0]).unwrap()
        ));
    }

    #[test]
    fn startup_normalizes_persisted_overlapping_roots() {
        let fixture = FixtureDir::new();
        let nested = fixture.path.join("nested");
        std::fs::create_dir(&nested).unwrap();
        let config_path = fixture.path.join("config.json");
        let config = WatchConfig {
            dirs: vec![
                nested.to_string_lossy().into_owned(),
                fixture.path.to_string_lossy().into_owned(),
            ],
            ..WatchConfig::default()
        };
        std::fs::write(&config_path, serde_json::to_vec(&config).unwrap()).unwrap();

        let state = WatchState::new(config_path.clone());
        assert_eq!(state.list_dirs().len(), 1);
        let persisted: WatchConfig =
            serde_json::from_slice(&std::fs::read(config_path).unwrap()).unwrap();
        assert_eq!(persisted.dirs, state.list_dirs());
    }

    #[test]
    fn legacy_path_only_config_migrates_without_losing_roots() {
        let fixture = FixtureDir::new();
        let config_path = fixture.path.join("legacy-config.json");
        let legacy = serde_json::json!({
            "dirs": [fixture.path.to_string_lossy()],
            "suffixes": [".log"],
            "showAll": false
        });
        std::fs::write(&config_path, serde_json::to_vec(&legacy).unwrap()).unwrap();

        let state = WatchState::new(config_path.clone());
        assert_eq!(state.list_dirs(), vec![fixture.path.to_string_lossy()]);
        let persisted: WatchConfig =
            serde_json::from_slice(&std::fs::read(config_path).unwrap()).unwrap();
        assert_eq!(persisted.version, 2);
        assert!(persisted.macos_bookmarks.is_empty());
    }

    #[test]
    fn removing_a_root_also_removes_its_persisted_bookmark() {
        let fixture = FixtureDir::new();
        let state = WatchState::new(fixture.path.join("config.json"));
        let root = fixture.path.to_string_lossy().into_owned();
        state.add_dir(&root).unwrap();
        state
            .config
            .lock()
            .unwrap()
            .macos_bookmarks
            .insert(root.clone(), "opaque".into());

        state.remove_dir(&root);
        assert!(!state.config().macos_bookmarks.contains_key(&root));
    }

    #[test]
    fn reopening_directory_reestablishes_its_structure_watcher() {
        let fixture = FixtureDir::new();
        let nested = fixture.path.join("nested");
        std::fs::create_dir(&nested).unwrap();
        let state = WatchState::new(fixture.path.join("config.json"));
        state.add_dir(fixture.path.to_str().unwrap()).unwrap();
        state
            .start_watch(fixture.path.to_str().unwrap(), |_| {}, |_| {})
            .unwrap();
        state
            .start_watch(nested.to_str().unwrap(), |_| {}, |_| {})
            .unwrap();
        state.stop_aux_watch_tree(nested.to_str().unwrap());

        let (change_tx, change_rx) = sync_channel(2);
        state
            .start_watch(
                nested.to_str().unwrap(),
                |_| {},
                move |batch| {
                    let _ = change_tx.send(batch);
                },
            )
            .unwrap();
        #[cfg(target_os = "macos")]
        std::thread::sleep(Duration::from_secs(1));
        let created = nested.join("reopened.log");
        std::fs::write(&created, b"reopened").unwrap();
        let batch = change_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(batch.changes.iter().any(
            |change| matches!(change, DirectoryChange::Upsert { node } if node.path == created.to_string_lossy())
        ));
    }

    #[test]
    fn arrival_prefilter_uses_archive_suffixes_and_current_log_filter() {
        let fixture = FixtureDir::new();
        let zip = fixture.write("bundle.ZIP", b"PK\x03\x04");
        let log = fixture.write("server.LOG", b"log");
        let binary = fixture.write("image.bin", &[0, 1, 2]);
        let mut config = WatchConfig {
            suffixes: vec![".log".into()],
            ..WatchConfig::default()
        };

        assert!(is_arrival_candidate(&config, &zip));
        assert!(is_arrival_candidate(&config, &log));
        assert!(!is_arrival_candidate(&config, &binary));
        config.show_all = true;
        assert!(is_arrival_candidate(&config, &binary));
    }

    #[test]
    fn recursive_event_storm_deduplicates_ten_thousand_updates() {
        let fixture = FixtureDir::new();
        let nested = fixture.path.join("a").join("b").join("c");
        std::fs::create_dir_all(&nested).unwrap();
        let file = nested.join("storm.log");
        std::fs::write(&file, b"storm").unwrap();
        let events = (0..10_000)
            .map(|_| {
                Event::new(EventKind::Modify(ModifyKind::Data(DataChange::Content)))
                    .add_path(file.clone())
            })
            .collect();

        let normalized = normalize_arrival_events(&fixture.path, events);
        assert_eq!(normalized.schedule, vec![file.clone()]);
        assert!(normalized.cancel.is_empty());

        let scheduler = StableScheduler::new(Arc::new(|_| {}));
        for _ in 0..10_000 {
            assert!(scheduler.schedule(file.clone(), "test".into()));
        }
        assert_eq!(scheduler.queued.lock().unwrap().len(), 1);
        assert_eq!(scheduler.generations.lock().unwrap().len(), 1);
    }

    #[test]
    fn stable_scheduler_rejects_excess_unique_candidates_at_its_capacity() {
        let fixture = FixtureDir::new();
        let scheduler = StableScheduler::new(Arc::new(|_| {}));
        for worker in 0..STABLE_WORKER_COUNT {
            let path = fixture.write(&format!("busy-{worker}.log"), b"busy");
            assert!(scheduler.schedule(path, "test".into()));
        }
        std::thread::sleep(Duration::from_millis(50));

        let rejected = (0..10_000)
            .filter(|index| {
                !scheduler.schedule(
                    fixture.path.join(format!("queued-{index}.log")),
                    "test".into(),
                )
            })
            .count();
        assert!(rejected > 0);
        assert!(
            scheduler.queued.lock().unwrap().len() <= STABLE_QUEUE_CAPACITY + STABLE_WORKER_COUNT
        );
        assert!(
            scheduler.generations.lock().unwrap().len()
                <= STABLE_QUEUE_CAPACITY + STABLE_WORKER_COUNT
        );
    }

    #[test]
    fn recursive_watcher_detects_logs_in_unexpanded_deep_directory_once() {
        let fixture = FixtureDir::new();
        let nested = fixture.path.join("downloads").join("daily");
        std::fs::create_dir_all(&nested).unwrap();
        let state = WatchState::new(fixture.path.join("config.json"));
        state.add_dir(fixture.path.to_str().unwrap()).unwrap();
        let (detect_tx, detect_rx) = sync_channel(4);
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

        #[cfg(target_os = "macos")]
        std::thread::sleep(Duration::from_secs(1));

        let temporary = nested.join("daily.download");
        let finished = nested.join("daily.zip");
        let log = nested.join("server.log");
        std::fs::write(&temporary, b"PK\x03\x04payload").unwrap();
        std::thread::sleep(Duration::from_millis(250));
        std::fs::rename(&temporary, &finished).unwrap();
        std::fs::write(&log, b"server started\n").unwrap();

        let first = detect_rx.recv_timeout(Duration::from_secs(8)).unwrap();
        let second = detect_rx.recv_timeout(Duration::from_secs(8)).unwrap();
        let detected = [first, second]
            .into_iter()
            .map(|item| (item.path, item.kind))
            .collect::<BTreeMap<_, _>>();
        let finished = user_facing_path(&std::fs::canonicalize(&finished).unwrap())
            .to_string_lossy()
            .into_owned();
        let log = user_facing_path(&std::fs::canonicalize(&log).unwrap())
            .to_string_lossy()
            .into_owned();
        assert_eq!(detected.get(&finished).map(String::as_str), Some("archive"));
        assert_eq!(detected.get(&log).map(String::as_str), Some("file"));
        assert!(detect_rx.recv_timeout(Duration::from_secs(2)).is_err());
        while let Ok(batch) = change_rx.try_recv() {
            assert!(batch.changes.iter().all(|change| match change {
                DirectoryChange::Upsert { node } | DirectoryChange::Rename { node, .. } => {
                    node.path != finished && node.path != log
                }
                DirectoryChange::Remove { path } => path != &finished && path != &log,
                DirectoryChange::Rescan { nodes } => nodes
                    .iter()
                    .all(|node| { node.path != finished && node.path != log }),
            }));
        }
        assert!(state
            .scan_dir(fixture.path.to_str().unwrap())
            .iter()
            .all(|item| item.path != finished && item.path != log));
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
        assert!(matches!(
            normalized.batch.changes.as_slice(),
            [DirectoryChange::Upsert { node }] if node.path == file.to_string_lossy()
        ));
    }

    #[test]
    fn cancelled_stable_detection_does_not_emit_a_stale_result() {
        let fixture = FixtureDir::new();
        let file = fixture.write("slow.log", b"complete");
        let (tx, rx) = sync_channel(1);
        let scheduler = StableScheduler::new(Arc::new(move |item| {
            let _ = tx.send(item);
        }));

        assert!(scheduler.schedule(file.clone(), "test".into()));
        scheduler.cancel(&file);
        assert!(rx.recv_timeout(Duration::from_secs(2)).is_err());
    }

    #[test]
    fn cancel_then_reschedule_same_path_emits_only_latest_result() {
        let fixture = FixtureDir::new();
        let file = fixture.write("replaced.log", b"latest");
        let (tx, rx) = sync_channel(2);
        let scheduler = StableScheduler::new(Arc::new(move |item| {
            let _ = tx.send(item);
        }));

        assert!(scheduler.schedule(file.clone(), "old".into()));
        scheduler.cancel(&file);
        assert!(scheduler.schedule(file.clone(), "latest".into()));
        let item = rx.recv_timeout(Duration::from_secs(3)).unwrap();
        assert_eq!(item.source, "latest");
        assert!(rx.recv_timeout(Duration::from_secs(2)).is_err());
    }

    #[test]
    fn native_watcher_reports_structure_before_stable_detection() {
        let fixture = FixtureDir::new();
        let state = WatchState::new(fixture.path.join("config.json"));
        state.add_dir(fixture.path.to_str().unwrap()).unwrap();
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
        assert!(batch.changes.iter().any(|change| match change {
            DirectoryChange::Remove { path } => path == &created.to_string_lossy(),
            DirectoryChange::Rescan { nodes } => {
                nodes
                    .iter()
                    .all(|node| node.path != created.to_string_lossy())
            }
            _ => false,
        }));
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
            vec![user_facing_path(&std::fs::canonicalize(watched).unwrap())
                .to_string_lossy()
                .into_owned()]
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
