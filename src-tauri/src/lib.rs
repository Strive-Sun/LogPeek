mod archive;
mod index;
mod watcher;

use archive::{open_archive, ArchiveEntry};
use index::{IndexProgress, LogLine, OpenResult, SessionManager};
use serde::Serialize;
use std::io::SeekFrom;
use std::path::PathBuf;
use std::sync::Arc;
use tauri::{Emitter, Manager, State};
use watcher::{DetectedItem, DirectoryChange, DirectoryChangeBatch, WatchState};

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
                .map(tree_child)
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
    spawn_watch(&state.watch, &app, &path)
}

/// 展开文件夹时按需读取直接子项并建立非递归 watcher。
#[tauri::command]
fn expand_directory(
    state: State<AppState>,
    app: tauri::AppHandle,
    path: String,
) -> Result<Vec<TreeChild>, String> {
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
fn collapse_directory(state: State<AppState>, path: String) {
    state.watch.stop_aux_watch_tree(&path);
}

#[tauri::command]
fn remove_watch_dir(state: State<AppState>, path: String) {
    state.watch.remove_dir(&path);
}

#[tauri::command]
fn set_filter(state: State<AppState>, suffixes: Vec<String>, show_all: bool) {
    state.watch.set_filter(suffixes, show_all);
}

/// 返回当前持久化的后缀筛选配置,供前端启动时同步,避免前后端筛选分叉。
#[tauri::command]
fn get_filter(state: State<AppState>) -> (Vec<String>, bool) {
    state.watch.get_filter()
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

/// 在系统文件管理器中打开/定位路径(资源管理器 / Finder / 文件管理器)。
#[tauri::command]
fn open_path(path: String) -> Result<(), String> {
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
fn rename_watch_dir(
    state: State<AppState>,
    app: tauri::AppHandle,
    path: String,
    new_name: String,
) -> Result<String, String> {
    let new_path = state
        .watch
        .rename_dir(&path, &new_name)
        .map_err(|e| e.to_string())?;
    spawn_watch(&state.watch, &app, &new_path)?;
    Ok(new_path)
}

/// 删除监控目录:整个文件夹移入回收站,并从配置中移除监控。
#[tauri::command]
fn delete_watch_dir(state: State<AppState>, path: String) -> Result<(), String> {
    let p = PathBuf::from(&path);
    if !p.exists() {
        return Err("目录不存在".into());
    }
    trash::delete(&p).map_err(|e| e.to_string())?;
    state.watch.remove_dir(&path);
    Ok(())
}

#[tauri::command]
fn list_archive_entries(path: String) -> Result<Vec<ArchiveEntry>, String> {
    let mut reader = open_archive(&PathBuf::from(&path)).map_err(|e| e.to_string())?;
    reader.entries().map_err(|e| e.to_string())
}

#[tauri::command]
fn open_log_session(
    state: State<AppState>,
    app: tauri::AppHandle,
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
    let result = state
        .sessions
        .prepare(display, declared)
        .map_err(|e| e.to_string())?;
    let session_id = result.session_id.clone();
    let sessions = state.sessions.clone();
    std::thread::spawn(move || match reader.open_entry(&entry_path) {
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
            sessions.index(&session_id, declared, stream, |event| {
                let _ = app.emit("index-progress", event);
            });
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
    });
    Ok(result)
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
fn set_session_encoding(
    state: State<AppState>,
    app: tauri::AppHandle,
    session_id: String,
    encoding: String,
) -> Result<u64, String> {
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
fn close_log_session(state: State<AppState>, session_id: String) {
    state.sessions.close(&session_id);
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
                    let batch = DirectoryChangeBatch {
                        watch_dir: parent.to_string_lossy().into_owned(),
                        changes: vec![DirectoryChange::Upsert { node: item.clone() }],
                    };
                    let _ = app2.emit("directory-changed", batch);
                }
                // 应用用户配置的后缀筛选:不匹配的新文件不计入通知
                if watch2.should_notify(&item) {
                    let _ = app2.emit("new-archive-detected", &item);
                }
            },
            move |batch: DirectoryChangeBatch| {
                let _ = app3.emit("directory-changed", batch);
            },
        )
        .map_err(|error| error.to_string())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_process::init())
        .setup(|app| {
            #[cfg(desktop)]
            app.handle()
                .plugin(tauri_plugin_updater::Builder::new().build())?;

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
                let _ = spawn_watch(&watch, app.handle(), &dir);
            }

            app.manage(AppState { watch, sessions });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            list_watch_dirs,
            expand_directory,
            collapse_directory,
            add_watch_dir,
            remove_watch_dir,
            set_filter,
            get_filter,
            rename_file,
            delete_file,
            open_path,
            rename_watch_dir,
            delete_watch_dir,
            list_archive_entries,
            open_log_session,
            read_lines,
            line_count,
            set_session_encoding,
            close_log_session
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
