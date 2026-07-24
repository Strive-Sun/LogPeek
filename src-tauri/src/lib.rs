mod archive;
mod index;
mod macos_file_access;
#[cfg(windows)]
pub mod ntfs;
#[cfg(all(test, windows))]
mod performance;
#[cfg(desktop)]
mod search;
#[cfg(desktop)]
mod search_index;
#[cfg(desktop)]
mod startup;
mod watcher;

use archive::{open_archive, resolve_archive_chain, ArchiveEntry};
use index::{IndexProgress, LogLine, OpenResult, SessionManager, SnapshotExportResult};
use serde::Serialize;
use std::io::SeekFrom;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use tauri::{Emitter, Manager, State};
use tokio::sync::Notify;
use watcher::{DetectedItem, DirectoryChange, DirectoryChangeBatch, DroppedFileInfo, WatchState};

#[cfg(desktop)]
use search::{
    FileSearchManager, SearchConfig, SearchFeatureState, SearchPage, SearchPreferenceStore,
    SearchStatus,
};
#[cfg(desktop)]
use startup::StartupRenderer;
use tauri::menu::MenuItem;
#[cfg(desktop)]
use tauri::{
    menu::Menu,
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    webview::{PageLoadEvent, WebviewBuilder},
    window::WindowBuilder,
    PhysicalPosition, PhysicalSize,
};

const MAIN_WINDOW_LABEL: &str = "main";
const SHOW_MAIN_MENU_ID: &str = "show-main-window";
const EXIT_APP_MENU_ID: &str = "exit-logpeek";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LifecycleAction {
    HideMainWindow,
    ShowMainWindow,
    ExitApplication,
    Ignore,
}

fn close_action(window_label: &str) -> LifecycleAction {
    if window_label == MAIN_WINDOW_LABEL {
        LifecycleAction::HideMainWindow
    } else {
        LifecycleAction::Ignore
    }
}

fn menu_action(menu_id: &str) -> LifecycleAction {
    match menu_id {
        SHOW_MAIN_MENU_ID => LifecycleAction::ShowMainWindow,
        EXIT_APP_MENU_ID => LifecycleAction::ExitApplication,
        _ => LifecycleAction::Ignore,
    }
}

fn tray_click_action(is_left_button: bool, is_released: bool) -> LifecycleAction {
    if is_left_button && is_released {
        LifecycleAction::ShowMainWindow
    } else {
        LifecycleAction::Ignore
    }
}

#[cfg(any(target_os = "macos", test))]
fn reopen_action() -> LifecycleAction {
    LifecycleAction::ShowMainWindow
}

fn create_archive_cache(cache_root: &std::path::Path) -> PathBuf {
    let archive_root = cache_root.join("nested-archives");
    let current = archive_root.join(format!("run-{}", std::process::id()));
    if std::fs::create_dir_all(&current).is_ok() {
        return current;
    }
    let fallback = std::env::temp_dir().join(format!("logcrate-nested-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&fallback);
    fallback
}

fn cleanup_stale_archive_caches(root: &std::path::Path, current: &std::path::Path) {
    if current.parent() != Some(root) {
        return;
    }
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path == current {
            continue;
        }
        if path.is_dir() {
            // 其它实例可能仍在使用自己的 run 目录；只移除空目录，不递归删除。
            let _ = std::fs::remove_dir(path);
        } else {
            // 旧版本直接写在 nested-archives 根目录中的文件在启动后不再被使用。
            let _ = std::fs::remove_file(path);
        }
    }
}

struct ReadyAppState {
    watch: Arc<WatchState>,
    sessions: Arc<SessionManager>,
    archive_cache: PathBuf,
    #[cfg(desktop)]
    search: Arc<SearchRuntime>,
}

#[cfg(desktop)]
struct SearchRuntime {
    preferences: SearchPreferenceStore,
    current_enabled: bool,
    manager: OnceLock<Result<Arc<FileSearchManager>, String>>,
    manager_notify: Notify,
}

#[cfg(desktop)]
impl SearchRuntime {
    fn new(preferences: SearchPreferenceStore) -> Self {
        let current_enabled = preferences.config().enabled;
        Self {
            preferences,
            current_enabled,
            manager: OnceLock::new(),
            manager_notify: Notify::new(),
        }
    }

    fn publish_manager(&self, manager: Result<Arc<FileSearchManager>, String>) {
        if self.manager.set(manager).is_ok() {
            self.manager_notify.notify_waiters();
        }
    }

    async fn manager(&self) -> Result<Arc<FileSearchManager>, String> {
        loop {
            if let Some(manager) = self.manager.get() {
                return manager.clone();
            }
            let notified = self.manager_notify.notified();
            if let Some(manager) = self.manager.get() {
                return manager.clone();
            }
            notified.await;
        }
    }
}

#[cfg(desktop)]
const SEARCH_DISABLED_ERROR: &str = "SEARCH_DISABLED:请在设置中启用文件搜索并重新启动 LogCrate";

#[cfg(desktop)]
fn ensure_search_enabled(state: &ReadyAppState) -> Result<(), String> {
    if state.search.current_enabled {
        Ok(())
    } else {
        Err(SEARCH_DISABLED_ERROR.into())
    }
}

#[cfg(desktop)]
async fn ready_search(state: &ReadyAppState) -> Result<Arc<FileSearchManager>, String> {
    ensure_search_enabled(state)?;
    state.search.manager().await
}

#[derive(Clone, Default)]
struct AppState {
    ready: Arc<OnceLock<ReadyAppState>>,
    ready_notify: Arc<Notify>,
}

impl AppState {
    fn publish(&self, ready: ReadyAppState) {
        if self.ready.set(ready).is_ok() {
            self.ready_notify.notify_waiters();
        }
    }

    async fn ready(&self) -> &ReadyAppState {
        loop {
            if let Some(ready) = self.ready.get() {
                return ready;
            }
            let notified = self.ready_notify.notified();
            if let Some(ready) = self.ready.get() {
                return ready;
            }
            notified.await;
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct FileRevision {
    exists: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    revision: Option<String>,
}

#[tauri::command]
async fn file_revision(path: String) -> Result<FileRevision, String> {
    match std::fs::metadata(&path) {
        Ok(metadata) if metadata.is_file() => {
            let modified = metadata
                .modified()
                .ok()
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok());
            let revision = modified
                .map(|value| {
                    format!(
                        "{}:{}:{}",
                        metadata.len(),
                        value.as_secs(),
                        value.subsec_nanos()
                    )
                })
                .unwrap_or_else(|| format!("{}:unknown", metadata.len()));
            Ok(FileRevision {
                exists: true,
                revision: Some(revision),
            })
        }
        Ok(_) => Ok(FileRevision {
            exists: false,
            revision: None,
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(FileRevision {
            exists: false,
            revision: None,
        }),
        Err(error) => Err(error.to_string()),
    }
}

struct TrayMenuItems {
    show: MenuItem<tauri::Wry>,
    exit: MenuItem<tauri::Wry>,
}

fn tray_labels(locale: &str) -> (&'static str, &'static str) {
    if locale == "zh-CN" {
        ("显示主窗口", "退出 LogCrate")
    } else {
        ("Show main window", "Exit LogCrate")
    }
}

#[tauri::command]
fn set_app_locale(locale: String, items: State<TrayMenuItems>) -> Result<(), String> {
    let (show, exit) = tray_labels(&locale);
    items.show.set_text(show).map_err(|e| e.to_string())?;
    items.exit.set_text(exit).map_err(|e| e.to_string())?;
    Ok(())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TreeDir {
    id: String,
    name: String,
    kind: String,
    path: String,
    children: Vec<TreeChild>,
    access_status: macos_file_access::WatchAccessStatus,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TreeChild {
    id: String,
    name: String,
    kind: String,
    path: String,
    size: u64,
    is_log: bool,
    source: String,
    unread: bool,
}

fn tree_child(item: DetectedItem) -> TreeChild {
    TreeChild {
        id: item.path.clone(),
        name: item.name,
        kind: item.kind,
        path: item.path,
        size: item.size,
        is_log: item.is_log,
        source: item.source,
        unread: false,
    }
}

// ---- 命令 ----

#[tauri::command]
async fn list_watch_dirs(state: State<'_, AppState>) -> Result<Vec<TreeDir>, String> {
    let state = state.ready().await;
    let dirs = state.watch.list_dirs();
    Ok(dirs
        .into_iter()
        .map(|d| {
            let name = PathBuf::from(&d)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or(&d)
                .to_string();
            let children = state
                .watch
                .scan_dir(&d)
                .into_iter()
                .map(tree_child)
                .collect();
            TreeDir {
                id: d.clone(),
                name,
                kind: "dir".into(),
                access_status: state.watch.access_status(&d),
                path: d,
                children,
            }
        })
        .collect())
}

#[tauri::command]
async fn add_watch_dir(
    state: State<'_, AppState>,
    app: tauri::AppHandle,
    path: String,
    user_selected: Option<bool>,
) -> Result<(), String> {
    let state = state.ready().await;
    let added = if user_selected.unwrap_or(false) {
        state.watch.add_user_selected_dir(&path)
    } else {
        state.watch.add_dir(&path)
    }
    .map_err(|e| e.to_string())?;
    if added {
        spawn_watch(&state.watch, &app, &path)
    } else {
        Ok(())
    }
}

#[tauri::command]
async fn reauthorize_watch_dir(
    state: State<'_, AppState>,
    existing_path: String,
    selected_path: String,
) -> Result<(), String> {
    let state = state.ready().await;
    state
        .watch
        .reauthorize_dir(&existing_path, &selected_path)
        .map_err(|error| error.to_string())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct MacOsFileAccessCapabilities {
    supported: bool,
    onboarding_version: u32,
    sandboxed: bool,
}

#[tauri::command]
fn macos_file_access_capabilities() -> MacOsFileAccessCapabilities {
    MacOsFileAccessCapabilities {
        supported: cfg!(target_os = "macos"),
        onboarding_version: 1,
        // No App Sandbox entitlement is configured in the current distribution.
        sandboxed: false,
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct MacOsSystemSettingsResult {
    used_fallback: bool,
}

#[tauri::command]
fn open_macos_full_disk_access_settings(
    app: tauri::AppHandle,
) -> Result<MacOsSystemSettingsResult, String> {
    #[cfg(target_os = "macos")]
    {
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        app.run_on_main_thread(move || {
            let _ = sender.send(macos_file_access::open_full_disk_access_settings());
        })
        .map_err(|error| error.to_string())?;
        let used_fallback = receiver
            .recv_timeout(std::time::Duration::from_secs(5))
            .map_err(|_| "SYSTEM_SETTINGS_OPEN_FAILED:打开系统设置超时".to_string())??;
        Ok(MacOsSystemSettingsResult { used_fallback })
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = app;
        Err("MACOS_ONLY:完全磁盘访问权限设置仅适用于 macOS".into())
    }
}

#[tauri::command]
async fn inspect_dropped_file(
    state: State<'_, AppState>,
    path: String,
) -> Result<DroppedFileInfo, String> {
    let state = state.ready().await;
    state
        .watch
        .inspect_dropped_file(&path)
        .map_err(|error| error.to_string())
}

#[cfg(desktop)]
#[tauri::command]
async fn file_search_status(state: State<'_, AppState>) -> Result<SearchStatus, String> {
    let state = state.ready().await;
    Ok(ready_search(state).await?.status())
}

#[cfg(desktop)]
#[tauri::command]
async fn file_search_config(state: State<'_, AppState>) -> Result<SearchConfig, String> {
    let state = state.ready().await;
    Ok(ready_search(state).await?.config())
}

#[cfg(desktop)]
#[tauri::command]
async fn file_search_feature_state(
    state: State<'_, AppState>,
) -> Result<SearchFeatureState, String> {
    let state = state.ready().await;
    Ok(state
        .search
        .preferences
        .feature_state(state.search.current_enabled))
}

#[cfg(desktop)]
#[tauri::command]
async fn set_file_search_enabled(
    state: State<'_, AppState>,
    enabled: bool,
) -> Result<SearchFeatureState, String> {
    let state = state.ready().await;
    state
        .search
        .preferences
        .set_enabled(enabled)
        .map_err(|error| error.to_string())?;
    Ok(state
        .search
        .preferences
        .feature_state(state.search.current_enabled))
}

#[cfg(desktop)]
#[tauri::command]
async fn start_file_search_index(
    state: State<'_, AppState>,
    app: tauri::AppHandle,
    rebuild: bool,
) -> Result<(), String> {
    let state = state.ready().await;
    ready_search(state)
        .await?
        .start(app, rebuild)
        .map_err(|error| error.to_string())
}

#[cfg(desktop)]
#[tauri::command]
async fn pause_file_search_index(
    state: State<'_, AppState>,
    app: tauri::AppHandle,
) -> Result<(), String> {
    let state = state.ready().await;
    ready_search(state).await?.pause(&app);
    Ok(())
}

#[cfg(desktop)]
#[tauri::command]
async fn clear_file_search_index(
    state: State<'_, AppState>,
    app: tauri::AppHandle,
) -> Result<(), String> {
    let state = state.ready().await;
    ready_search(state)
        .await?
        .clear(&app)
        .map_err(|error| error.to_string())
}

#[cfg(desktop)]
#[tauri::command]
async fn set_file_search_exclusions(
    state: State<'_, AppState>,
    app: tauri::AppHandle,
    exclusions: Vec<String>,
) -> Result<(), String> {
    let state = state.ready().await;
    ready_search(state)
        .await?
        .set_exclusions(app, exclusions)
        .map_err(|error| error.to_string())
}

#[cfg(desktop)]
#[tauri::command]
async fn repair_file_search_service(state: State<'_, AppState>) -> Result<(), String> {
    let state = state.ready().await;
    let _search = ready_search(state).await?;
    #[cfg(windows)]
    {
        tauri::async_runtime::spawn_blocking(crate::ntfs::ipc::repair_service)
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())
    }
    #[cfg(not(windows))]
    {
        Err("NTFS 快速索引服务仅适用于 Windows".into())
    }
}

#[cfg(desktop)]
#[tauri::command]
async fn search_files(
    state: State<'_, AppState>,
    query: String,
    filter: String,
    offset: u32,
    limit: u32,
) -> Result<SearchPage, String> {
    let state = state.ready().await;
    let search = ready_search(state).await?;
    tauri::async_runtime::spawn_blocking(move || search.query(&query, &filter, offset, limit))
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| error.to_string())
}

#[cfg(desktop)]
#[tauri::command]
async fn inspect_search_result(
    state: State<'_, AppState>,
    path: String,
) -> Result<DroppedFileInfo, String> {
    let state = state.ready().await;
    let search = ready_search(state).await?;
    match state.watch.inspect_dropped_file(&path) {
        Ok(info) => Ok(info),
        Err(error) => {
            let _ = search.remove_stale_path(&path);
            Err(error.to_string())
        }
    }
}

#[cfg(desktop)]
#[tauri::command]
async fn add_search_result_parent(
    state: State<'_, AppState>,
    app: tauri::AppHandle,
    path: String,
) -> Result<String, String> {
    let state = state.ready().await;
    let search = ready_search(state).await?;
    let file = PathBuf::from(&path);
    if !file.is_file() {
        let _ = search.remove_stale_path(&path);
        return Err("文件已被删除或移动".into());
    }
    let parent = file
        .parent()
        .ok_or("无法确定文件所在目录")?
        .to_string_lossy()
        .into_owned();
    let added = state
        .watch
        .add_dir(&parent)
        .map_err(|error| error.to_string())?;
    if added {
        spawn_watch(&state.watch, &app, &parent)?;
    }
    Ok(parent)
}

/// 展开文件夹时按需读取直接子项并建立非递归 watcher。
#[tauri::command]
async fn expand_directory(
    state: State<'_, AppState>,
    app: tauri::AppHandle,
    path: String,
) -> Result<Vec<TreeChild>, String> {
    let state = state.ready().await;
    if !state.watch.is_allowed_directory(&path) {
        return Err("目录不在已配置的监控范围内".into());
    }
    spawn_watch(&state.watch, &app, &path)?;
    Ok(state
        .watch
        .scan_dir(&path)
        .into_iter()
        .map(tree_child)
        .collect())
}

/// 折叠文件夹时释放该目录及已展开后代的按需 watcher。
#[tauri::command]
async fn collapse_directory(state: State<'_, AppState>, path: String) -> Result<(), String> {
    let state = state.ready().await;
    state.watch.stop_aux_watch_tree(&path);
    Ok(())
}

#[tauri::command]
async fn remove_watch_dir(state: State<'_, AppState>, path: String) -> Result<(), String> {
    let state = state.ready().await;
    state.watch.remove_dir(&path);
    Ok(())
}

#[tauri::command]
async fn set_filter(
    state: State<'_, AppState>,
    suffixes: Vec<String>,
    show_all: bool,
) -> Result<(), String> {
    let state = state.ready().await;
    state.watch.set_filter(suffixes, show_all);
    Ok(())
}

/// 返回当前持久化的后缀筛选配置,供前端启动时同步,避免前后端筛选分叉。
#[tauri::command]
async fn get_filter(state: State<'_, AppState>) -> Result<(Vec<String>, bool), String> {
    let state = state.ready().await;
    Ok(state.watch.get_filter())
}

/// 重命名文件:仅改文件名,保持在同一目录内;拒绝路径穿越与覆盖已存在文件。
#[tauri::command]
async fn rename_file(path: String, new_name: String) -> Result<String, String> {
    let src = PathBuf::from(&path);
    if !src.is_file() {
        return Err("文件不存在".into());
    }
    // 只允许纯文件名,禁止包含路径分隔符,避免移动到其他目录
    let trimmed = new_name.trim();
    if trimmed.is_empty() {
        return Err("文件名不能为空".into());
    }
    if trimmed.contains('/') || trimmed.contains('\\') {
        return Err("文件名不能包含路径分隔符".into());
    }
    let parent = src.parent().ok_or("无法确定父目录")?;
    let dst = parent.join(trimmed);
    if dst.exists() {
        return Err(format!("已存在同名文件: {trimmed}"));
    }
    std::fs::rename(&src, &dst).map_err(|e| e.to_string())?;
    Ok(dst.to_string_lossy().into_owned())
}

/// 删除文件:移动到系统回收站(可恢复),而非永久删除。
#[tauri::command]
async fn delete_file(path: String) -> Result<(), String> {
    let p = PathBuf::from(&path);
    if !p.exists() {
        return Err("文件不存在".into());
    }
    trash::delete(&p).map_err(|e| e.to_string())
}

/// 在系统文件管理器中打开/定位路径(资源管理器 / Finder / 文件管理器)。
#[tauri::command]
async fn open_path(path: String) -> Result<(), String> {
    let p = PathBuf::from(&path);
    if !p.exists() {
        return Err("路径不存在".into());
    }
    #[cfg(target_os = "windows")]
    {
        // 目录直接打开,文件则在资源管理器中定位并选中
        let mut cmd = std::process::Command::new("explorer");
        if p.is_dir() {
            cmd.arg(&p);
        } else {
            cmd.arg("/select,").arg(&p);
        }
        cmd.spawn().map_err(|e| e.to_string())?;
    }
    #[cfg(target_os = "macos")]
    {
        let mut cmd = std::process::Command::new("open");
        if p.is_file() {
            cmd.arg("-R");
        }
        cmd.arg(&p).spawn().map_err(|e| e.to_string())?;
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let target = if p.is_file() {
            p.parent().unwrap_or(&p).to_path_buf()
        } else {
            p.clone()
        };
        std::process::Command::new("xdg-open")
            .arg(target)
            .spawn()
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// 重命名监控目录:磁盘改名 + 更新配置,并对新路径重建监听。
#[tauri::command]
async fn rename_watch_dir(
    state: State<'_, AppState>,
    app: tauri::AppHandle,
    path: String,
    new_name: String,
) -> Result<String, String> {
    let state = state.ready().await;
    let new_path = state
        .watch
        .rename_dir(&path, &new_name)
        .map_err(|e| e.to_string())?;
    spawn_watch(&state.watch, &app, &new_path)?;
    Ok(new_path)
}

/// 删除监控目录:整个文件夹移入回收站,并从配置中移除监控。
#[tauri::command]
async fn delete_watch_dir(state: State<'_, AppState>, path: String) -> Result<(), String> {
    let state = state.ready().await;
    let p = PathBuf::from(&path);
    if !p.exists() {
        return Err("目录不存在".into());
    }
    trash::delete(&p).map_err(|e| e.to_string())?;
    state.watch.remove_dir(&path);
    Ok(())
}

#[tauri::command]
async fn list_archive_entries(
    state: State<'_, AppState>,
    path: String,
) -> Result<Vec<ArchiveEntry>, String> {
    let state = state.ready().await;
    let resolved = resolve_archive_chain(&path, &state.archive_cache).map_err(|e| e.to_string())?;
    let mut reader = open_archive(resolved.path()).map_err(|e| e.to_string())?;
    let entries = reader.entries().map_err(|e| e.to_string())?;
    drop(reader);
    drop(resolved);
    Ok(entries)
}

#[tauri::command]
async fn open_log_session(
    state: State<'_, AppState>,
    app: tauri::AppHandle,
    archive_path: String,
    entry_path: String,
) -> Result<OpenResult, String> {
    let state = state.ready().await;
    let resolved =
        resolve_archive_chain(&archive_path, &state.archive_cache).map_err(|e| e.to_string())?;
    let mut reader = open_archive(resolved.path()).map_err(|e| e.to_string())?;
    let entries = reader.entries().map_err(|e| e.to_string())?;
    let meta = entries
        .iter()
        .find(|e| e.path == entry_path)
        .ok_or_else(|| format!("条目不存在: {entry_path}"))?;
    if meta.encrypted {
        return Err("条目已加密,暂不支持".into());
    }
    if !meta.is_log {
        return Err("该条目不是文本日志,无法查看".into());
    }
    let declared = meta.size;
    let remaining_limit = index::MAX_UNCOMPRESSED.saturating_sub(resolved.decoded_bytes());
    if declared > remaining_limit {
        return Err("嵌套归档链累计解码内容超过 2 GiB 安全上限".into());
    }
    let display = if entry_path == archive_path {
        entry_path.clone()
    } else {
        let root_archive = archive_path.split("::").next().unwrap_or(&archive_path);
        let arc_name = PathBuf::from(root_archive)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(root_archive)
            .to_string();
        let nested = archive_path
            .split("::")
            .skip(1)
            .chain(std::iter::once(entry_path.as_str()))
            .collect::<Vec<_>>()
            .join(" › ");
        format!("{arc_name} › {nested}")
    };
    let result = state
        .sessions
        .prepare(display, declared)
        .map_err(|e| e.to_string())?;
    let session_id = result.session_id.clone();
    let sessions = state.sessions.clone();
    std::thread::spawn(move || {
        // Keep lazily materialized ancestor archives alive until indexing has
        // finished, then remove them through ResolvedArchiveChain::drop.
        let _resolved = resolved;
        match reader.open_entry(&entry_path) {
            Ok(mut stream) => {
                let reset = if stream.is_seekable() {
                    stream.seek(SeekFrom::Start(0)).map(|_| ())
                } else {
                    Ok(())
                };
                if let Err(error) = reset {
                    let event = IndexProgress {
                        session_id,
                        percent: 100,
                        indexed_lines: 0,
                        done: true,
                        failed: true,
                        detected_encoding: "Unknown".into(),
                        effective_encoding: "UTF-8".into(),
                        error: Some(error.to_string()),
                    };
                    let _ = app.emit("index-progress", event);
                    return;
                }
                sessions.index_with_limit(
                    &session_id,
                    declared,
                    stream,
                    remaining_limit,
                    |event| {
                        let _ = app.emit("index-progress", event);
                    },
                );
            }
            Err(error) => {
                let event = IndexProgress {
                    session_id,
                    percent: 100,
                    indexed_lines: 0,
                    done: true,
                    failed: true,
                    detected_encoding: "Unknown".into(),
                    effective_encoding: "UTF-8".into(),
                    error: Some(error.to_string()),
                };
                let _ = app.emit("index-progress", event);
            }
        }
    });
    Ok(result)
}

#[tauri::command]
async fn read_lines(
    state: State<'_, AppState>,
    session_id: String,
    start: u64,
    count: u64,
) -> Result<Vec<LogLine>, String> {
    let state = state.ready().await;
    state
        .sessions
        .read_lines(&session_id, start, count)
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn line_count(state: State<'_, AppState>, session_id: String) -> Result<u64, String> {
    let state = state.ready().await;
    Ok(state.sessions.line_count(&session_id))
}

#[tauri::command]
async fn export_session_snapshot(
    state: State<'_, AppState>,
    session_id: String,
    destination: String,
) -> Result<SnapshotExportResult, String> {
    let state = state.ready().await;
    state
        .sessions
        .export_snapshot(&session_id, std::path::Path::new(&destination))
        .map_err(|error| error.to_string())
}

#[tauri::command]
async fn set_session_encoding(
    state: State<'_, AppState>,
    app: tauri::AppHandle,
    session_id: String,
    encoding: String,
) -> Result<u64, String> {
    let state = state.ready().await;
    let change = state
        .sessions
        .prepare_encoding_change(&session_id, &encoding)
        .map_err(|error| error.to_string())?;
    let generation = change.generation();
    let sessions = state.sessions.clone();
    std::thread::spawn(move || {
        sessions.apply_encoding_change(change, |event| {
            let _ = app.emit("encoding-progress", event);
        });
    });
    Ok(generation)
}

#[tauri::command]
async fn close_log_session(state: State<'_, AppState>, session_id: String) -> Result<(), String> {
    let state = state.ready().await;
    state.sessions.close(&session_id);
    Ok(())
}

fn spawn_watch(watch: &Arc<WatchState>, app: &tauri::AppHandle, dir: &str) -> Result<(), String> {
    let app2 = app.clone();
    let app3 = app.clone();
    let watch2 = watch.clone();
    watch
        .start_watch(
            dir,
            move |item: DetectedItem| {
                // 稳定检测可能补全未知后缀文本文件的分类，先更新目录库存。
                if let Some(parent) = PathBuf::from(&item.path).parent() {
                    if watch2.is_structure_watched(parent) {
                        let batch = DirectoryChangeBatch {
                            watch_dir: parent.to_string_lossy().into_owned(),
                            changes: vec![DirectoryChange::Upsert { node: item.clone() }],
                        };
                        let _ = app2.emit("directory-changed", batch);
                    }
                }
                // 应用用户配置的后缀筛选:不匹配的新文件不计入通知
                if watch2.is_watched_path(PathBuf::from(&item.path).as_path())
                    && watch2.should_notify(&item)
                {
                    let _ = app2.emit("new-archive-detected", &item);
                }
            },
            move |batch: DirectoryChangeBatch| {
                let _ = app3.emit("directory-changed", batch);
            },
        )
        .map_err(|error| error.to_string())
}

#[cfg(desktop)]
fn show_main_window(app: &tauri::AppHandle) {
    if let Some(window) = app.get_window(MAIN_WINDOW_LABEL) {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

#[cfg(desktop)]
fn setup_tray(app: &tauri::App) -> tauri::Result<()> {
    let show_item = MenuItem::with_id(
        app,
        SHOW_MAIN_MENU_ID,
        "Show main window",
        true,
        None::<&str>,
    )?;
    let exit_item = MenuItem::with_id(app, EXIT_APP_MENU_ID, "Exit LogCrate", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show_item, &exit_item])?;

    let mut tray = TrayIconBuilder::new()
        .tooltip("LogCrate")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match menu_action(event.id().as_ref()) {
            LifecycleAction::ShowMainWindow => show_main_window(app),
            LifecycleAction::ExitApplication => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button,
                button_state,
                ..
            } = event
            {
                let action = tray_click_action(
                    button == MouseButton::Left,
                    button_state == MouseButtonState::Up,
                );
                if action == LifecycleAction::ShowMainWindow {
                    show_main_window(tray.app_handle());
                }
            }
        });

    if let Some(icon) = app.default_window_icon().cloned() {
        tray = tray.icon(icon);
    }
    tray.build(app)?;
    app.manage(TrayMenuItems {
        show: show_item,
        exit: exit_item,
    });
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_process::init())
        .on_window_event(|window, event| {
            if close_action(window.label()) == LifecycleAction::HideMainWindow {
                if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .setup(|app| {
            #[cfg(desktop)]
            {
                let window_config = app
                    .config()
                    .app
                    .windows
                    .first()
                    .cloned()
                    .ok_or("缺少主窗口配置")?;
                let window = WindowBuilder::from_config(app, &window_config)?
                    .resizable(false)
                    .build()?;
                let startup_renderer = StartupRenderer::start(window.clone())?;

                app.handle()
                    .plugin(tauri_plugin_updater::Builder::new().build())?;
                setup_tray(app)?;

                let webview_window = window.clone();
                std::thread::spawn(move || {
                    let renderer = startup_renderer.clone();
                    let webview_builder = WebviewBuilder::from_config(&window_config).on_page_load(
                        move |webview, payload| {
                            if payload.event() != PageLoadEvent::Finished {
                                return;
                            }
                            renderer.stop();
                            let window = webview.window();
                            if let Ok(size) = window.inner_size() {
                                let _ = webview.set_position(PhysicalPosition::new(0, 0));
                                let _ = webview.set_size(size);
                                let _ = webview.set_auto_resize(true);
                            }
                            let _ = window.set_resizable(true);
                        },
                    );
                    let _ = webview_window.add_child(
                        webview_builder,
                        PhysicalPosition::new(-1, -1),
                        PhysicalSize::new(1, 1),
                    );
                });
            }

            let state = AppState::default();
            app.manage(state.clone());
            let config_dir = app
                .path()
                .app_config_dir()
                .unwrap_or_else(|_| std::env::temp_dir());
            let cache_dir = app
                .path()
                .app_cache_dir()
                .unwrap_or_else(|_| std::env::temp_dir())
                // Legacy cache directory keeps in-place LogPeek upgrades free of orphaned data.
                .join("logpeek-cache");
            let app_handle = app.handle().clone();
            std::thread::spawn(move || {
                let _ = std::fs::create_dir_all(&config_dir);
                let watch = WatchState::new(config_dir.join("watch-config.json"));
                let sessions = Arc::new(SessionManager::default());
                sessions.set_cache_dir(cache_dir.clone());
                let archive_root = cache_dir.join("nested-archives");
                let archive_cache = create_archive_cache(&cache_dir);
                let search_dir = config_dir.join("file-search");
                let search_preferences = SearchPreferenceStore::new(search_dir.clone());
                let search = Arc::new(SearchRuntime::new(search_preferences.clone()));
                state.publish(ReadyAppState {
                    watch: watch.clone(),
                    sessions,
                    archive_cache: archive_cache.clone(),
                    search: search.clone(),
                });

                for dir in watch.list_dirs() {
                    let _ = spawn_watch(&watch, &app_handle, &dir);
                }
                if search.current_enabled {
                    tauri::async_runtime::spawn(async move {
                        let manager = tauri::async_runtime::spawn_blocking(move || {
                            let manager = FileSearchManager::new_with_preferences(
                                search_dir,
                                &search_preferences,
                            );
                            let _ = manager.resume_or_watch(app_handle);
                            manager
                        })
                        .await
                        .map_err(|error| format!("搜索模块初始化失败：{error}"));
                        search.publish_manager(manager);
                    });
                }
                cleanup_stale_archive_caches(&archive_root, &archive_cache);
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            list_watch_dirs,
            expand_directory,
            collapse_directory,
            add_watch_dir,
            reauthorize_watch_dir,
            macos_file_access_capabilities,
            open_macos_full_disk_access_settings,
            inspect_dropped_file,
            file_search_status,
            file_search_config,
            file_search_feature_state,
            set_file_search_enabled,
            start_file_search_index,
            pause_file_search_index,
            clear_file_search_index,
            set_file_search_exclusions,
            repair_file_search_service,
            search_files,
            inspect_search_result,
            add_search_result_parent,
            remove_watch_dir,
            set_filter,
            get_filter,
            rename_file,
            delete_file,
            open_path,
            rename_watch_dir,
            delete_watch_dir,
            file_revision,
            list_archive_entries,
            open_log_session,
            read_lines,
            line_count,
            export_session_snapshot,
            set_session_encoding,
            close_log_session,
            set_app_locale
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application");

    app.run(|app_handle, event| {
        #[cfg(target_os = "macos")]
        if let tauri::RunEvent::Reopen { .. } = event {
            if reopen_action() == LifecycleAction::ShowMainWindow {
                show_main_window(app_handle);
            }
        }

        #[cfg(not(target_os = "macos"))]
        let _ = (app_handle, event);
    });
}

#[cfg(test)]
mod lifecycle_tests {
    use super::*;

    #[test]
    fn file_revision_reports_changes_and_missing_sources() {
        let path =
            std::env::temp_dir().join(format!("logcrate-revision-test-{}.log", std::process::id()));
        std::fs::write(&path, b"first").unwrap();
        let first =
            tauri::async_runtime::block_on(file_revision(path.to_string_lossy().into_owned()))
                .unwrap();
        assert!(first.exists);
        std::fs::write(&path, b"second version").unwrap();
        let second =
            tauri::async_runtime::block_on(file_revision(path.to_string_lossy().into_owned()))
                .unwrap();
        assert!(second.exists);
        assert_ne!(first.revision, second.revision);
        std::fs::remove_file(&path).unwrap();
        let missing =
            tauri::async_runtime::block_on(file_revision(path.to_string_lossy().into_owned()))
                .unwrap();
        assert!(!missing.exists);
        assert!(missing.revision.is_none());
    }

    #[test]
    fn main_window_close_hides_only_the_main_window() {
        assert_eq!(
            close_action(MAIN_WINDOW_LABEL),
            LifecycleAction::HideMainWindow
        );
        assert_eq!(close_action("settings"), LifecycleAction::Ignore);
    }

    #[test]
    fn tray_menu_maps_show_exit_and_unknown_actions() {
        assert_eq!(
            menu_action(SHOW_MAIN_MENU_ID),
            LifecycleAction::ShowMainWindow
        );
        assert_eq!(
            menu_action(EXIT_APP_MENU_ID),
            LifecycleAction::ExitApplication
        );
        assert_eq!(menu_action("unknown"), LifecycleAction::Ignore);
    }

    #[test]
    fn tray_labels_support_chinese_and_fall_back_to_english() {
        assert_eq!(tray_labels("zh-CN"), ("显示主窗口", "退出 LogCrate"));
        assert_eq!(tray_labels("en"), ("Show main window", "Exit LogCrate"));
        assert_eq!(tray_labels("fr"), ("Show main window", "Exit LogCrate"));
    }

    #[test]
    fn only_left_button_release_restores_the_window() {
        assert_eq!(
            tray_click_action(true, true),
            LifecycleAction::ShowMainWindow
        );
        assert_eq!(tray_click_action(true, false), LifecycleAction::Ignore);
        assert_eq!(tray_click_action(false, true), LifecycleAction::Ignore);
    }

    #[test]
    fn repeated_restore_requests_remain_idempotent_actions() {
        assert_eq!(tray_click_action(true, true), tray_click_action(true, true));
        assert_eq!(
            menu_action(SHOW_MAIN_MENU_ID),
            LifecycleAction::ShowMainWindow
        );
    }

    #[test]
    fn macos_reopen_restores_the_existing_main_window() {
        assert_eq!(reopen_action(), LifecycleAction::ShowMainWindow);
    }

    #[test]
    fn application_start_keeps_current_cache_and_removes_stale_runs() {
        let root = std::env::temp_dir().join(format!(
            "logcrate-startup-cache-test-{}",
            std::process::id()
        ));
        let current = root.join("current");
        let stale = root.join("stale");
        let legacy_file = root.join("legacy.archive");
        std::fs::create_dir_all(&current).unwrap();
        std::fs::create_dir_all(&stale).unwrap();
        std::fs::write(&legacy_file, b"stale").unwrap();
        cleanup_stale_archive_caches(&root, &current);
        assert!(current.exists());
        assert!(!stale.exists());
        assert!(!legacy_file.exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(desktop)]
    #[tokio::test]
    async fn core_state_is_ready_without_constructing_search_manager() {
        let root = std::env::temp_dir().join(format!(
            "logcrate-deferred-search-core-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let search_dir = root.join("search");
        let search = Arc::new(SearchRuntime::new(SearchPreferenceStore::new(
            search_dir.clone(),
        )));
        let state = AppState::default();
        state.publish(ReadyAppState {
            watch: WatchState::new(root.join("watch.json")),
            sessions: Arc::new(SessionManager::default()),
            archive_cache: root.join("archive-cache"),
            search,
        });

        let ready = state.ready().await;
        assert!(ready.search.manager.get().is_none());
        assert!(!search_dir.exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(desktop)]
    #[tokio::test]
    async fn search_commands_can_wait_for_deferred_manager_result() {
        let root = std::env::temp_dir().join(format!(
            "logcrate-deferred-search-wait-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let preferences = SearchPreferenceStore::new(root.clone());
        preferences.set_enabled(true).unwrap();
        let search = Arc::new(SearchRuntime::new(preferences));
        let waiting_search = search.clone();
        let waiter = tokio::spawn(async move { waiting_search.manager().await });
        tokio::task::yield_now().await;
        assert!(!waiter.is_finished());

        search.publish_manager(Err("deferred search failed".into()));
        assert_eq!(
            waiter.await.unwrap().err().as_deref(),
            Some("deferred search failed")
        );
        let _ = std::fs::remove_dir_all(root);
    }
}
