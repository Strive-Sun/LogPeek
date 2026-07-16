mod archive;
mod index;
mod watcher;

use archive::{open_archive, ArchiveEntry};
use index::{LogLine, OpenResult, SessionManager};
use serde::Serialize;
use std::path::PathBuf;
use std::sync::Arc;
use tauri::{Emitter, Manager, State};
use watcher::{DetectedItem, WatchState};

struct AppState {
    watch: Arc<WatchState>,
    sessions: Arc<SessionManager>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TreeDir {
    id: String,
    name: String,
    kind: String,
    path: String,
    children: Vec<TreeChild>,
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

// ---- 命令 ----

#[tauri::command]
fn list_watch_dirs(state: State<AppState>) -> Vec<TreeDir> {
    let dirs = state.watch.list_dirs();
    dirs.into_iter()
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
                .map(|it| TreeChild {
                    id: it.path.clone(),
                    name: it.name,
                    kind: it.kind,
                    path: it.path,
                    size: it.size,
                    is_log: true,
                    source: it.source,
                    unread: false,
                })
                .collect();
            TreeDir {
                id: d.clone(),
                name,
                kind: "dir".into(),
                path: d,
                children,
            }
        })
        .collect()
}

#[tauri::command]
fn add_watch_dir(
    state: State<AppState>,
    app: tauri::AppHandle,
    path: String,
) -> Result<(), String> {
    state.watch.add_dir(&path).map_err(|e| e.to_string())?;
    spawn_watch(&state.watch, &app, &path);
    Ok(())
}

#[tauri::command]
fn remove_watch_dir(state: State<AppState>, path: String) {
    state.watch.remove_dir(&path);
}

#[tauri::command]
fn set_filter(state: State<AppState>, suffixes: Vec<String>, show_all: bool) {
    state.watch.set_filter(suffixes, show_all);
}

/// 重命名文件:仅改文件名,保持在同一目录内;拒绝路径穿越与覆盖已存在文件。
#[tauri::command]
fn rename_file(path: String, new_name: String) -> Result<String, String> {
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
fn delete_file(path: String) -> Result<(), String> {
    let p = PathBuf::from(&path);
    if !p.exists() {
        return Err("文件不存在".into());
    }
    trash::delete(&p).map_err(|e| e.to_string())
}

#[tauri::command]
fn list_archive_entries(path: String) -> Result<Vec<ArchiveEntry>, String> {
    let mut reader = open_archive(&PathBuf::from(&path)).map_err(|e| e.to_string())?;
    reader.entries().map_err(|e| e.to_string())
}

#[tauri::command]
fn open_log_session(
    state: State<AppState>,
    archive_path: String,
    entry_path: String,
) -> Result<OpenResult, String> {
    let mut reader = open_archive(&PathBuf::from(&archive_path)).map_err(|e| e.to_string())?;
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
    let stream = reader.open_entry(&entry_path).map_err(|e| e.to_string())?;
    let display = if entry_path == archive_path || !archive_path.ends_with(".zip") {
        entry_path.clone()
    } else {
        let arc_name = PathBuf::from(&archive_path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(&archive_path)
            .to_string();
        format!("{arc_name} › {entry_path}")
    };
    state
        .sessions
        .open(stream, display, declared)
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn read_lines(
    state: State<AppState>,
    session_id: String,
    start: u64,
    count: u64,
) -> Result<Vec<LogLine>, String> {
    state
        .sessions
        .read_lines(&session_id, start, count)
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn line_count(state: State<AppState>, session_id: String) -> u64 {
    state.sessions.line_count(&session_id)
}

#[tauri::command]
fn close_log_session(state: State<AppState>, session_id: String) {
    state.sessions.close(&session_id);
}

fn spawn_watch(watch: &Arc<WatchState>, app: &tauri::AppHandle, dir: &str) {
    let app2 = app.clone();
    let _ = watch.start_watch(dir, move |item: DetectedItem| {
        let _ = app2.emit("new-archive-detected", &item);
    });
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let config_dir = app
                .path()
                .app_config_dir()
                .unwrap_or_else(|_| std::env::temp_dir());
            std::fs::create_dir_all(&config_dir).ok();
            let watch = WatchState::new(config_dir.join("watch-config.json"));

            let sessions = Arc::new(SessionManager::default());
            let cache_dir = app
                .path()
                .app_cache_dir()
                .unwrap_or_else(|_| std::env::temp_dir())
                .join("logpeek-cache");
            sessions.set_cache_dir(cache_dir);

            // 启动恢复:对已配置目录建立监听
            for dir in watch.list_dirs() {
                spawn_watch(&watch, app.handle(), &dir);
            }

            app.manage(AppState { watch, sessions });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            list_watch_dirs,
            add_watch_dir,
            remove_watch_dir,
            set_filter,
            rename_file,
            delete_file,
            list_archive_entries,
            open_log_session,
            read_lines,
            line_count,
            close_log_session
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
