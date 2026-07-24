use crate::archive::{is_archive_name, is_log_name};
#[cfg(windows)]
use crate::ntfs::{
    ipc::{enumerate_mft_via_service, query_usn_via_service, read_usn_via_service},
    resolve_mft_files_in_batches, resolve_mft_files_in_batches_retain, MftRecord, UsnJournalInfo,
};
use crate::search_index::{SearchIndex, SearchIndexEntry};
use notify::{Config as NotifyConfig, Event, RecommendedWatcher, RecursiveMode, Watcher};
use rusqlite::{params, params_from_iter, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
#[cfg(windows)]
use std::collections::HashMap;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{
    sync_channel, Receiver, RecvTimeoutError, SyncSender, TryRecvError, TrySendError,
};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tauri::Emitter;

const SCHEMA_VERSION: i64 = 8;
const SCAN_WRITE_BATCH: usize = 8_192;
#[cfg(windows)]
const NTFS_RESOLVE_BATCH: usize = 2_048;
const EVENT_BATCH: usize = 512;
const EVENT_QUEUE_CAPACITY: usize = 4096;
const QUERY_LIMIT_MAX: u32 = 500;
const METADATA_WORKERS_MAX: usize = 4;
#[cfg(windows)]
const MAX_USN_REPLAY_RECORDS: usize = 1_000_000;

fn query_index_staging_path(active: &Path) -> PathBuf {
    let mut value = active.as_os_str().to_os_string();
    value.push(".next");
    PathBuf::from(value)
}

fn query_index_previous_path(active: &Path) -> PathBuf {
    let mut value = active.as_os_str().to_os_string();
    value.push(".previous");
    PathBuf::from(value)
}

fn recover_query_index_directories(active: &Path) -> anyhow::Result<()> {
    let staging = query_index_staging_path(active);
    let previous = query_index_previous_path(active);
    if !active.exists() {
        if previous.exists() {
            fs::rename(&previous, active)?;
        } else if staging.exists() {
            fs::rename(&staging, active)?;
        }
    }
    if active.exists() && previous.exists() {
        fs::remove_dir_all(previous)?;
    }
    if active.exists() && staging.exists() {
        fs::remove_dir_all(staging)?;
    }
    Ok(())
}

const CREATE_FTS_TRIGGERS: &str =
    "CREATE TRIGGER IF NOT EXISTS files_ai AFTER INSERT ON files BEGIN
       INSERT INTO files_fts(rowid, name, path) VALUES (new.rowid, new.name, new.path);
     END;
     CREATE TRIGGER IF NOT EXISTS files_ad AFTER DELETE ON files BEGIN
       INSERT INTO files_fts(files_fts, rowid, name, path)
       VALUES('delete', old.rowid, old.name, old.path);
     END;
     CREATE TRIGGER IF NOT EXISTS files_au AFTER UPDATE ON files BEGIN
       INSERT INTO files_fts(files_fts, rowid, name, path)
       VALUES('delete', old.rowid, old.name, old.path);
       INSERT INTO files_fts(rowid, name, path) VALUES (new.rowid, new.name, new.path);
     END;";
const DROP_FTS_TRIGGERS: &str = "DROP TRIGGER IF EXISTS files_ai;
     DROP TRIGGER IF EXISTS files_ad;
     DROP TRIGGER IF EXISTS files_au;";
const CREATE_QUERY_CHANGE_TRIGGERS: &str =
    "CREATE TRIGGER IF NOT EXISTS files_q_ai AFTER INSERT ON files BEGIN
       INSERT INTO search_index_changes(path, operation) VALUES(new.path, 1)
       ON CONFLICT(path) DO UPDATE SET operation=1;
     END;
     CREATE TRIGGER IF NOT EXISTS files_q_ad AFTER DELETE ON files BEGIN
       INSERT INTO search_index_changes(path, operation) VALUES(old.path, 0)
       ON CONFLICT(path) DO UPDATE SET operation=0;
     END;
     CREATE TRIGGER IF NOT EXISTS files_q_au AFTER UPDATE ON files BEGIN
       INSERT INTO search_index_changes(path, operation) VALUES(old.path, 0)
       ON CONFLICT(path) DO UPDATE SET operation=0;
       INSERT INTO search_index_changes(path, operation) VALUES(new.path, 1)
       ON CONFLICT(path) DO UPDATE SET operation=1;
     END;";
const DROP_QUERY_CHANGE_TRIGGERS: &str = "DROP TRIGGER IF EXISTS files_q_ai;
     DROP TRIGGER IF EXISTS files_q_ad;
     DROP TRIGGER IF EXISTS files_q_au;";
const CREATE_FTS_TABLE: &str = "CREATE VIRTUAL TABLE files_fts USING fts5(
       name, path, content='files', content_rowid='rowid',
       tokenize='trigram', detail='none', columnsize=0
     );";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SearchConfig {
    #[serde(default = "search_config_version")]
    pub version: u32,
    pub enabled: bool,
    pub roots: Vec<String>,
    pub exclusions: Vec<String>,
}

const fn search_config_version() -> u32 {
    1
}

impl Default for SearchConfig {
    fn default() -> Self {
        Self {
            version: search_config_version(),
            enabled: false,
            roots: local_fixed_roots(),
            exclusions: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SearchFeatureState {
    pub current_enabled: bool,
    pub next_launch_enabled: bool,
}

#[derive(Clone)]
pub struct SearchPreferenceStore {
    config_path: PathBuf,
    config: Arc<Mutex<SearchConfig>>,
}

impl SearchPreferenceStore {
    pub fn new(data_dir: PathBuf) -> Self {
        let config_path = data_dir.join("file-search.json");
        let config = read_config(&config_path).unwrap_or_default();
        Self {
            config_path,
            config: Arc::new(Mutex::new(config)),
        }
    }

    pub fn config(&self) -> SearchConfig {
        self.config.lock().unwrap().clone()
    }

    pub fn feature_state(&self, current_enabled: bool) -> SearchFeatureState {
        SearchFeatureState {
            current_enabled,
            next_launch_enabled: self.config.lock().unwrap().enabled,
        }
    }

    pub fn set_enabled(&self, enabled: bool) -> anyhow::Result<()> {
        let mut config = self.config.lock().unwrap();
        let previous = config.enabled;
        config.enabled = enabled;
        config.version = search_config_version();
        if let Err(error) = write_config(&self.config_path, &config) {
            config.enabled = previous;
            return Err(error);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchStatus {
    pub phase: String,
    pub scanned_files: u64,
    pub skipped_directories: u64,
    pub indexed_files: u64,
    pub index_bytes: u64,
    pub roots: Vec<String>,
    pub exclusions: Vec<String>,
    pub providers: Vec<SearchProviderStatus>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SearchProviderStatus {
    pub root: String,
    pub provider: String,
    pub phase: String,
    pub fallback_reason: Option<String>,
}

impl SearchStatus {
    fn disabled(config: &SearchConfig) -> Self {
        Self {
            phase: "disabled".into(),
            scanned_files: 0,
            skipped_directories: 0,
            indexed_files: 0,
            index_bytes: 0,
            roots: config.roots.clone(),
            exclusions: config.exclusions.clone(),
            providers: planned_provider_statuses(&config.roots),
            error: None,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchResultItem {
    pub path: String,
    pub name: String,
    pub parent: String,
    pub kind: String,
    pub size: u64,
    pub modified_ms: Option<u64>,
    pub readable: bool,
    pub content_type: String,
    pub is_log: bool,
    pub is_archive: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchPage {
    pub items: Vec<SearchResultItem>,
    pub total: u64,
    pub partial: bool,
    pub elapsed_ms: u64,
}

#[derive(Debug, Clone)]
struct IndexedFile {
    path: String,
    name: String,
    root: String,
    size: u64,
    modified_ms: Option<u64>,
    is_log: bool,
    is_archive: bool,
    file_id: Option<[u8; 16]>,
    parent_id: Option<[u8; 16]>,
}

pub struct FileSearchManager {
    db_path: PathBuf,
    config_path: PathBuf,
    query_index_path: PathBuf,
    config: Arc<Mutex<SearchConfig>>,
    status: Mutex<SearchStatus>,
    generation: AtomicU64,
    cancel: AtomicBool,
    watcher: Mutex<Option<RecommendedWatcher>>,
    event_sender: Mutex<Option<SyncSender<Event>>>,
    event_dirty: Arc<AtomicBool>,
    query_index: Mutex<Option<SearchIndex>>,
    staged_query_index: Mutex<Option<SearchIndex>>,
    query_index_ready: AtomicBool,
    query_index_bulk: AtomicBool,
    query_index_staged: AtomicBool,
    persistence_recovery: AtomicBool,
}

impl FileSearchManager {
    #[cfg(test)]
    pub fn new(data_dir: PathBuf) -> Arc<Self> {
        let preferences = SearchPreferenceStore::new(data_dir.clone());
        Self::new_with_preferences(data_dir, &preferences)
    }

    pub fn new_with_preferences(
        data_dir: PathBuf,
        preferences: &SearchPreferenceStore,
    ) -> Arc<Self> {
        let _ = fs::create_dir_all(&data_dir);
        let config_path = preferences.config_path.clone();
        let query_index_path = data_dir.join("file-search-orange-gpl-v1");
        let _ = recover_query_index_directories(&query_index_path);
        let config_state = preferences.config.clone();
        let config = config_state.lock().unwrap().clone();
        let mut status = SearchStatus::disabled(&config);
        if config.enabled && data_dir.join("file-search.sqlite3").is_file() {
            status.phase = "ready".into();
        }
        let query_index = SearchIndex::open(&query_index_path);
        if let Err(error) = &query_index {
            status.error = Some(format!("Tantivy query index: {error}"));
        }
        let query_documents_at_start = query_index.as_ref().ok().map(SearchIndex::num_docs);
        let database_state = initialize_database_with_query(
            &data_dir.join("file-search.sqlite3"),
            query_documents_at_start,
        );
        let manager = Arc::new(Self {
            db_path: data_dir.join("file-search.sqlite3"),
            config_path,
            query_index_path,
            config: config_state,
            status: Mutex::new(status),
            generation: AtomicU64::new(0),
            cancel: AtomicBool::new(false),
            watcher: Mutex::new(None),
            event_sender: Mutex::new(None),
            event_dirty: Arc::new(AtomicBool::new(false)),
            query_index: Mutex::new(query_index.ok()),
            staged_query_index: Mutex::new(None),
            query_index_ready: AtomicBool::new(false),
            query_index_bulk: AtomicBool::new(false),
            query_index_staged: AtomicBool::new(false),
            persistence_recovery: AtomicBool::new(false),
        });
        match database_state {
            Err(error) => manager.status.lock().unwrap().error = Some(error.to_string()),
            Ok(database_state) => {
                manager.refresh_counts();
                let database_documents = manager.status.lock().unwrap().indexed_files;
                let query_documents = manager
                    .query_index
                    .lock()
                    .unwrap()
                    .as_ref()
                    .map(SearchIndex::num_docs);
                let query_ready = query_documents == Some(database_documents)
                    || (database_state.query_snapshot_complete
                        && query_documents.is_some_and(|documents| documents > 0));
                manager
                    .query_index_ready
                    .store(query_ready, Ordering::Release);
                manager
                    .persistence_recovery
                    .store(database_state.persistence_incomplete, Ordering::Release);
                if query_ready && manager.config.lock().unwrap().enabled {
                    let mut status = manager.status.lock().unwrap();
                    status.phase = "ready".into();
                    if database_state.query_snapshot_complete {
                        if let Some(query_documents) = query_documents {
                            status.indexed_files = query_documents;
                        }
                    }
                }
            }
        }
        manager
    }

    pub fn config(&self) -> SearchConfig {
        self.config.lock().unwrap().clone()
    }

    fn runtime_config(&self) -> SearchConfig {
        let mut config = self.config();
        if let Some(data_dir) = self.db_path.parent() {
            config
                .exclusions
                .push(data_dir.to_string_lossy().into_owned());
        }
        config.exclusions = normalize_unique_paths(config.exclusions);
        config
    }

    pub fn status(&self) -> SearchStatus {
        self.refresh_counts();
        self.status.lock().unwrap().clone()
    }

    pub fn start(self: &Arc<Self>, app: tauri::AppHandle, rebuild: bool) -> anyhow::Result<()> {
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        self.cancel.store(false, Ordering::SeqCst);
        self.stop_watcher();
        {
            let mut config = self.config.lock().unwrap();
            if config.roots.is_empty() {
                config.roots = local_fixed_roots();
            }
            write_config(&self.config_path, &config)?;
            let mut status = self.status.lock().unwrap();
            status.phase = "scanning".into();
            status.scanned_files = 0;
            if rebuild {
                status.indexed_files = 0;
            }
            status.skipped_directories = 0;
            status.roots = config.roots.clone();
            status.exclusions = config.exclusions.clone();
            status.providers = planned_provider_statuses(&config.roots);
            status.error = None;
        }
        self.emit_status(&app);
        let config = self.runtime_config();

        let manager = Arc::clone(self);
        std::thread::spawn(move || {
            let receiver = match manager.install_watcher(&config) {
                Ok(receiver) => receiver,
                Err(error) => {
                    manager.finish_with_error(&app, generation, error);
                    return;
                }
            };
            if let Err(error) = prepare_bulk_index(&manager.db_path, rebuild) {
                manager.stop_watcher();
                manager.finish_with_error(&app, generation, error);
                return;
            }
            if rebuild {
                if let Err(error) = manager.begin_query_index_bulk() {
                    manager.stop_watcher();
                    manager.finish_with_error(&app, generation, error);
                    return;
                }
                manager.query_index_ready.store(true, Ordering::Release);
            }
            match scan_with_providers(&manager, &app, generation, &config) {
                Ok(()) if !manager.is_cancelled(generation) => {
                    if let Err(error) = manager.finish_query_index_bulk() {
                        manager.stop_watcher();
                        manager.finish_with_error(&app, generation, error);
                        return;
                    }
                    if let Err(error) = finish_bulk_index(&manager.db_path) {
                        manager.stop_watcher();
                        manager.finish_with_error(&app, generation, error);
                        return;
                    }
                    if manager.is_cancelled(generation) {
                        manager.stop_watcher();
                        return;
                    }
                    if let Err(error) = drain_event_paths(&manager.db_path, &config, &receiver) {
                        manager.stop_watcher();
                        manager.finish_with_error(&app, generation, error);
                        return;
                    }
                    if let Err(error) = manager.drain_query_index_changes() {
                        manager.stop_watcher();
                        manager.finish_with_error(&app, generation, error);
                        return;
                    }
                    manager.spawn_event_worker(app.clone(), config, receiver);
                    {
                        let mut status = manager.status.lock().unwrap();
                        status.phase = "ready".into();
                        status.error = None;
                    }
                    manager.refresh_counts();
                    manager.emit_status(&app);
                }
                Ok(()) => manager.stop_watcher(),
                Err(error) => {
                    manager.stop_watcher();
                    manager.finish_with_error(&app, generation, error);
                }
            }
        });
        Ok(())
    }

    pub fn resume_or_watch(self: &Arc<Self>, app: tauri::AppHandle) -> anyhow::Result<()> {
        let config = self.runtime_config();
        let has_persisted_files = self.status.lock().unwrap().indexed_files > 0;
        let has_search_snapshot = self.query_index_ready.load(Ordering::Acquire);
        if self.db_path.is_file() && (has_persisted_files || has_search_snapshot) {
            let receiver = self.install_watcher(&config)?;
            #[cfg(windows)]
            {
                let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
                self.cancel.store(false, Ordering::SeqCst);
                let recover_persistence = self.persistence_recovery.load(Ordering::Acquire);
                self.status.lock().unwrap().phase = if recover_persistence {
                    "ready".into()
                } else {
                    "scanning".into()
                };
                let manager = Arc::clone(self);
                std::thread::spawn(move || {
                    if let Err(error) = manager.ensure_query_index_matches_database() {
                        manager.finish_with_error(&app, generation, error);
                        return;
                    }
                    for root in &config.roots {
                        let Some(volume) = ntfs_volume_letter(root) else {
                            continue;
                        };
                        set_provider_status(&manager, &app, root, "windowsNtfs", "scanning", None);
                        if recover_persistence {
                            let snapshot = enumerate_ntfs_volume_snapshot(
                                &manager, &app, generation, &config, root, volume,
                            );
                            match snapshot.and_then(|job| {
                                persist_and_finalize_ntfs_volume(&manager.db_path, job)
                            }) {
                                Ok(()) => {
                                    set_provider_status(
                                        &manager,
                                        &app,
                                        root,
                                        "windowsNtfs",
                                        "ready",
                                        None,
                                    );
                                }
                                Err(error) => {
                                    manager.finish_with_error(&app, generation, error);
                                    return;
                                }
                            }
                            continue;
                        }
                        let catch_up = catch_up_ntfs_volume(
                            &manager.db_path,
                            root,
                            volume,
                            &config.exclusions,
                        );
                        if catch_up.is_err() {
                            if let Err(error) =
                                scan_ntfs_volume(&manager, &app, generation, &config, root, volume)
                            {
                                set_provider_status(
                                    &manager,
                                    &app,
                                    root,
                                    "folderScan",
                                    "fallback",
                                    Some(error.to_string()),
                                );
                                continue;
                            }
                        }
                        if let Err(error) = manager.drain_query_index_changes() {
                            manager.finish_with_error(&app, generation, error);
                            return;
                        }
                        set_provider_status(&manager, &app, root, "windowsNtfs", "ready", None);
                    }
                    if recover_persistence {
                        if let Err(error) = finish_bulk_index(&manager.db_path) {
                            manager.finish_with_error(&app, generation, error);
                            return;
                        }
                        manager.persistence_recovery.store(false, Ordering::Release);
                    }
                    if let Err(error) = drain_event_paths(&manager.db_path, &config, &receiver) {
                        manager.finish_with_error(&app, generation, error);
                        return;
                    }
                    if let Err(error) = manager.drain_query_index_changes() {
                        manager.finish_with_error(&app, generation, error);
                        return;
                    }
                    manager.spawn_event_worker(app.clone(), config, receiver);
                    manager.status.lock().unwrap().phase = "ready".into();
                    manager.refresh_counts();
                    manager.emit_status(&app);
                });
                Ok(())
            }
            #[cfg(not(windows))]
            {
                let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
                self.cancel.store(false, Ordering::SeqCst);
                self.status.lock().unwrap().phase = "scanning".into();
                let manager = Arc::clone(self);
                std::thread::spawn(move || {
                    if let Err(error) = manager.ensure_query_index_matches_database() {
                        manager.finish_with_error(&app, generation, error);
                        return;
                    }
                    manager.spawn_event_worker(app.clone(), config, receiver);
                    manager.status.lock().unwrap().phase = "ready".into();
                    manager.emit_status(&app);
                });
                Ok(())
            }
        } else {
            self.start(app, true)
        }
    }

    pub fn pause(&self, app: &tauri::AppHandle) {
        self.cancel.store(true, Ordering::SeqCst);
        self.generation.fetch_add(1, Ordering::SeqCst);
        self.stop_watcher();
        self.status.lock().unwrap().phase = "paused".into();
        self.emit_status(app);
    }

    pub fn clear(&self, app: &tauri::AppHandle) -> anyhow::Result<()> {
        self.pause(app);
        clear_rows(&self.db_path)?;
        self.clear_query_index()?;
        {
            let config = self.config.lock().unwrap();
            write_config(&self.config_path, &config)?;
            *self.status.lock().unwrap() = SearchStatus::disabled(&config);
        }
        self.emit_status(app);
        Ok(())
    }

    pub fn set_exclusions(
        self: &Arc<Self>,
        app: tauri::AppHandle,
        exclusions: Vec<String>,
    ) -> anyhow::Result<()> {
        let normalized = normalize_unique_paths(exclusions);
        {
            let mut config = self.config.lock().unwrap();
            config.exclusions = normalized;
            write_config(&self.config_path, &config)?;
        }
        self.start(app, true)
    }

    pub fn query(
        &self,
        query: &str,
        filter: &str,
        offset: u32,
        limit: u32,
    ) -> anyhow::Result<SearchPage> {
        let started = std::time::Instant::now();
        let terms = query
            .split_whitespace()
            .map(|term| term.to_lowercase())
            .filter(|term| !term.is_empty())
            .collect::<Vec<_>>();
        if terms.is_empty() {
            return Ok(SearchPage {
                items: Vec::new(),
                total: 0,
                partial: self.status.lock().unwrap().phase != "ready",
                elapsed_ms: 0,
            });
        }
        let limit = limit.clamp(1, QUERY_LIMIT_MAX);
        let connection = open_database(&self.db_path)?;
        let phase = self.status.lock().unwrap().phase.clone();
        let filter_sql = match filter {
            "log" => " AND f.is_log = 1 AND f.is_archive = 0",
            "archive" => " AND f.is_archive = 1",
            _ => "",
        };
        let can_use_tantivy = !terms
            .iter()
            .any(|term| term.chars().any(|character| "\\/:".contains(character)));
        let (mut items, total) = if can_use_tantivy {
            match self.query_tantivy(&terms, filter, offset, limit)? {
                Some(result) => result,
                None => query_like(&connection, &terms, filter_sql, offset, limit)?,
            }
        } else {
            query_like(&connection, &terms, filter_sql, offset, limit)?
        };
        enrich_visible_metadata(&mut items);
        Ok(SearchPage {
            items,
            total,
            partial: phase != "ready",
            elapsed_ms: started.elapsed().as_millis() as u64,
        })
    }

    pub fn remove_stale_path(&self, path: &str) -> anyhow::Result<()> {
        let connection = open_database(&self.db_path)?;
        connection.execute("DELETE FROM files WHERE path = ?1", params![path])?;
        self.drain_query_index_changes()?;
        Ok(())
    }

    fn index_files(&self, files: &[IndexedFile]) -> anyhow::Result<()> {
        let entries = files.iter().map(search_index_entry).collect::<Vec<_>>();
        let bulk = self.query_index_bulk.load(Ordering::Acquire);
        if bulk && self.query_index_staged.load(Ordering::Acquire) {
            if let Some(index) = self.staged_query_index.lock().unwrap().as_mut() {
                index.add_batch(&entries)?;
            }
        } else if let Some(index) = self.query_index.lock().unwrap().as_mut() {
            if bulk {
                index.add_batch(&entries)?;
            } else {
                index.upsert_batch(&entries)?;
            }
        }
        if bulk {
            let mut status = self.status.lock().unwrap();
            status.indexed_files = status.indexed_files.saturating_add(files.len() as u64);
        }
        Ok(())
    }

    fn begin_query_index_bulk(&self) -> anyhow::Result<()> {
        self.staged_query_index.lock().unwrap().take();
        let staging_path = query_index_staging_path(&self.query_index_path);
        if staging_path.exists() {
            fs::remove_dir_all(&staging_path)?;
        }
        let has_complete_snapshot = self
            .query_index
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|index| index.num_docs() > 0);
        if has_complete_snapshot {
            let mut staging = SearchIndex::open(&staging_path)?;
            staging.begin_bulk()?;
            *self.staged_query_index.lock().unwrap() = Some(staging);
            self.query_index_staged.store(true, Ordering::Release);
        } else if let Some(index) = self.query_index.lock().unwrap().as_mut() {
            index.begin_bulk()?;
            self.query_index_staged.store(false, Ordering::Release);
        }
        self.query_index_bulk.store(true, Ordering::Release);
        Ok(())
    }

    fn finish_query_index_bulk(&self) -> anyhow::Result<()> {
        if self.query_index_staged.load(Ordering::Acquire) {
            if let Some(mut staging) = self.staged_query_index.lock().unwrap().take() {
                staging.finish_bulk()?;
                drop(staging);
            }
            self.activate_staged_query_index()?;
        } else if let Some(index) = self.query_index.lock().unwrap().as_mut() {
            index.finish_bulk()?;
        }
        self.query_index_bulk.store(false, Ordering::Release);
        self.query_index_staged.store(false, Ordering::Release);
        self.query_index_ready.store(true, Ordering::Release);
        Ok(())
    }

    fn clear_query_index(&self) -> anyhow::Result<()> {
        self.staged_query_index.lock().unwrap().take();
        self.query_index_staged.store(false, Ordering::Release);
        let staging_path = query_index_staging_path(&self.query_index_path);
        if staging_path.exists() {
            fs::remove_dir_all(staging_path)?;
        }
        if let Some(index) = self.query_index.lock().unwrap().as_mut() {
            index.clear()?;
        }
        Ok(())
    }

    fn commit_query_index(&self) -> anyhow::Result<()> {
        if self.query_index_staged.load(Ordering::Acquire) {
            if let Some(index) = self.staged_query_index.lock().unwrap().as_mut() {
                index.commit()?;
            }
            return Ok(());
        }
        if let Some(index) = self.query_index.lock().unwrap().as_mut() {
            index.commit()?;
        }
        Ok(())
    }

    fn activate_staged_query_index(&self) -> anyhow::Result<()> {
        let staging = query_index_staging_path(&self.query_index_path);
        let previous = query_index_previous_path(&self.query_index_path);
        self.query_index.lock().unwrap().take();
        if previous.exists() {
            fs::remove_dir_all(&previous)?;
        }
        if self.query_index_path.exists() {
            fs::rename(&self.query_index_path, &previous)?;
        }
        if let Err(error) = fs::rename(&staging, &self.query_index_path) {
            if previous.exists() && !self.query_index_path.exists() {
                let _ = fs::rename(&previous, &self.query_index_path);
            }
            *self.query_index.lock().unwrap() = SearchIndex::open(&self.query_index_path).ok();
            return Err(error.into());
        }
        match SearchIndex::open(&self.query_index_path) {
            Ok(index) => {
                *self.query_index.lock().unwrap() = Some(index);
                if previous.exists() {
                    fs::remove_dir_all(previous)?;
                }
                Ok(())
            }
            Err(error) => {
                let failed = query_index_staging_path(&self.query_index_path);
                let _ = fs::rename(&self.query_index_path, &failed);
                if previous.exists() {
                    let _ = fs::rename(&previous, &self.query_index_path);
                }
                *self.query_index.lock().unwrap() = SearchIndex::open(&self.query_index_path).ok();
                Err(error)
            }
        }
    }

    fn query_tantivy(
        &self,
        terms: &[String],
        filter: &str,
        offset: u32,
        limit: u32,
    ) -> anyhow::Result<Option<(Vec<SearchResultItem>, u64)>> {
        if !self.query_index_ready.load(Ordering::Acquire) {
            return Ok(None);
        }
        let guard = self.query_index.lock().unwrap();
        let Some(index) = guard.as_ref().filter(|index| index.num_docs() > 0) else {
            return Ok(None);
        };
        let (entries, total) = index.search(terms, filter, offset, limit)?;
        let items = entries
            .into_iter()
            .map(|entry| {
                let content_type = content_type_for_name(&entry.name).into();
                SearchResultItem {
                    parent: Path::new(&entry.path)
                        .parent()
                        .map(|parent| parent.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                    path: entry.path,
                    name: entry.name,
                    kind: if entry.is_archive {
                        "archive".into()
                    } else if entry.is_log {
                        "log".into()
                    } else {
                        "file".into()
                    },
                    size: 0,
                    modified_ms: None,
                    readable: false,
                    content_type,
                    is_log: entry.is_log,
                    is_archive: entry.is_archive,
                }
            })
            .collect();
        Ok(Some((items, total)))
    }

    fn ensure_query_index_matches_database(&self) -> anyhow::Result<()> {
        if self.query_index_ready.load(Ordering::Acquire) {
            return Ok(());
        }
        let mut guard = self.query_index.lock().unwrap();
        let Some(index) = guard.as_mut() else {
            return Ok(());
        };
        index.begin_bulk()?;
        let connection = open_database(&self.db_path)?;
        let mut statement = connection
            .prepare("SELECT path, name, is_log, is_archive FROM files ORDER BY rowid")?;
        let mut rows = statement.query([])?;
        let mut batch = Vec::with_capacity(SCAN_WRITE_BATCH);
        while let Some(row) = rows.next()? {
            batch.push(SearchIndexEntry {
                path: row.get(0)?,
                name: row.get(1)?,
                is_log: row.get(2)?,
                is_archive: row.get(3)?,
            });
            if batch.len() == SCAN_WRITE_BATCH {
                index.add_batch(&batch)?;
                batch.clear();
            }
        }
        if !batch.is_empty() {
            index.add_batch(&batch)?;
        }
        index.finish_bulk()?;
        self.query_index_ready.store(true, Ordering::Release);
        Ok(())
    }

    fn drain_query_index_changes(&self) -> anyhow::Result<()> {
        if self.query_index.lock().unwrap().is_none() {
            return Ok(());
        }
        let connection = open_database(&self.db_path)?;
        loop {
            let changes = {
                let mut statement = connection.prepare(
                    "SELECT path, operation FROM search_index_changes ORDER BY rowid LIMIT 4096",
                )?;
                let rows = statement
                    .query_map([], |row| {
                        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
                    })?
                    .collect::<Result<Vec<_>, _>>()?;
                rows
            };
            if changes.is_empty() {
                break;
            }
            let mut upserts = Vec::new();
            let upsert_paths = changes
                .iter()
                .filter(|(_, operation)| *operation == 1)
                .map(|(path, _)| path)
                .collect::<Vec<_>>();
            for path_chunk in upsert_paths.chunks(500) {
                let placeholders = std::iter::repeat("?")
                    .take(path_chunk.len())
                    .collect::<Vec<_>>()
                    .join(",");
                let sql = format!(
                    "SELECT path, name, is_log, is_archive FROM files WHERE path IN ({placeholders})"
                );
                let mut load = connection.prepare(&sql)?;
                let rows = load.query_map(params_from_iter(path_chunk.iter()), |row| {
                    Ok(SearchIndexEntry {
                        path: row.get(0)?,
                        name: row.get(1)?,
                        is_log: row.get(2)?,
                        is_archive: row.get(3)?,
                    })
                })?;
                upserts.extend(rows.collect::<Result<Vec<_>, _>>()?);
            }
            let paths = changes
                .iter()
                .map(|(path, _)| path.clone())
                .collect::<Vec<_>>();
            if let Some(index) = self.query_index.lock().unwrap().as_mut() {
                index.apply_changes(&paths, &upserts)?;
            }
            let tx = connection.unchecked_transaction()?;
            {
                let mut delete =
                    tx.prepare_cached("DELETE FROM search_index_changes WHERE path = ?1")?;
                for path in &paths {
                    delete.execute(params![path])?;
                }
            }
            tx.commit()?;
        }
        Ok(())
    }

    fn is_cancelled(&self, generation: u64) -> bool {
        self.cancel.load(Ordering::Relaxed) || self.generation.load(Ordering::Relaxed) != generation
    }

    fn finish_with_error(&self, app: &tauri::AppHandle, generation: u64, error: anyhow::Error) {
        if self.is_cancelled(generation) {
            return;
        }
        let mut status = self.status.lock().unwrap();
        status.phase = "error".into();
        status.error = Some(error.to_string());
        drop(status);
        self.emit_status(app);
    }

    fn emit_status(&self, app: &tauri::AppHandle) {
        let _ = app.emit("file-search-status", self.status.lock().unwrap().clone());
    }

    fn refresh_counts(&self) {
        let Ok(connection) = open_database(&self.db_path) else {
            return;
        };
        let indexed_files = if self.query_index_bulk.load(Ordering::Acquire)
            || self.persistence_recovery.load(Ordering::Acquire)
        {
            None
        } else {
            Some(
                connection
                    .query_row("SELECT COUNT(*) FROM files", [], |row| row.get::<_, u64>(0))
                    .unwrap_or(0),
            )
        };
        let index_bytes = database_size(&self.db_path);
        let mut status = self.status.lock().unwrap();
        if let Some(indexed_files) = indexed_files {
            status.indexed_files = indexed_files;
        }
        status.index_bytes = index_bytes;
    }

    fn stop_watcher(&self) {
        self.event_sender.lock().unwrap().take();
        self.watcher.lock().unwrap().take();
    }

    fn install_watcher(self: &Arc<Self>, config: &SearchConfig) -> anyhow::Result<Receiver<Event>> {
        self.stop_watcher();
        self.event_dirty.store(false, Ordering::SeqCst);
        let (sender, receiver) = sync_channel::<Event>(EVENT_QUEUE_CAPACITY);
        let callback_sender = sender.clone();
        let dirty = Arc::clone(&self.event_dirty);
        let event_roots = config.roots.clone();
        let event_exclusions = config.exclusions.clone();
        let mut watcher = RecommendedWatcher::new(
            move |result: notify::Result<Event>| {
                let Ok(mut event) = result else {
                    dirty.store(true, Ordering::Relaxed);
                    return;
                };
                event.paths.retain(|path| {
                    containing_root(path, &event_roots).is_some()
                        && !is_excluded(path, &event_exclusions)
                        && !is_platform_skipped_directory(path)
                });
                if event.paths.is_empty() {
                    return;
                }
                enqueue_event(&callback_sender, &dirty, event);
            },
            NotifyConfig::default(),
        )?;
        for root in &config.roots {
            let path = Path::new(root);
            if path.is_dir() {
                watcher.watch(path, RecursiveMode::Recursive)?;
            }
        }
        *self.watcher.lock().unwrap() = Some(watcher);
        *self.event_sender.lock().unwrap() = Some(sender);

        Ok(receiver)
    }

    fn spawn_event_worker(
        self: &Arc<Self>,
        app: tauri::AppHandle,
        config: SearchConfig,
        receiver: Receiver<Event>,
    ) {
        let manager = Arc::clone(self);
        std::thread::spawn(move || loop {
            let first = match receiver.recv_timeout(Duration::from_millis(500)) {
                Ok(event) => event,
                Err(RecvTimeoutError::Timeout) => {
                    if manager.event_sender.lock().unwrap().is_none() {
                        break;
                    }
                    if manager.event_dirty.swap(false, Ordering::SeqCst) {
                        let _ = manager.start(app.clone(), true);
                        break;
                    }
                    continue;
                }
                Err(RecvTimeoutError::Disconnected) => break,
            };
            let mut paths = first.paths;
            while let Ok(event) = receiver.try_recv() {
                paths.extend(event.paths);
                if paths.len() >= EVENT_BATCH {
                    break;
                }
            }
            paths.sort();
            paths.dedup();
            if let Err(error) = apply_event_paths(&manager.db_path, &config, &paths)
                .and_then(|_| manager.drain_query_index_changes())
            {
                manager.status.lock().unwrap().error = Some(error.to_string());
                manager.emit_status(&app);
            } else {
                manager.refresh_counts();
                manager.emit_status(&app);
            }
        });
    }
}

fn search_index_entry(file: &IndexedFile) -> SearchIndexEntry {
    SearchIndexEntry {
        path: file.path.clone(),
        name: file.name.clone(),
        is_log: file.is_log,
        is_archive: file.is_archive,
    }
}

fn enqueue_event(sender: &SyncSender<Event>, dirty: &AtomicBool, event: Event) {
    if let Err(TrySendError::Full(_)) = sender.try_send(event) {
        dirty.store(true, Ordering::Relaxed);
    }
}

fn scan_with_providers(
    manager: &Arc<FileSearchManager>,
    app: &tauri::AppHandle,
    generation: u64,
    config: &SearchConfig,
) -> anyhow::Result<()> {
    #[cfg(windows)]
    {
        let mut folder_roots = Vec::new();
        let mut ntfs_roots = Vec::new();
        let mut finalize_jobs = Vec::new();
        for root in &config.roots {
            if manager.is_cancelled(generation) {
                return Ok(());
            }
            let Some(volume) = ntfs_volume_letter(root) else {
                folder_roots.push(root.clone());
                set_provider_status(manager, app, root, "folderScan", "scanning", None);
                continue;
            };
            set_provider_status(manager, app, root, "windowsNtfs", "scanning", None);
            ntfs_roots.push((root.clone(), volume));
        }
        let system_volume = system_volume_letter();
        ntfs_roots.sort_by_key(|(_, volume)| ntfs_scan_priority(*volume, 0, system_volume));
        let mut snapshots = Vec::with_capacity(ntfs_roots.len());
        for (root, volume) in ntfs_roots {
            match enumerate_ntfs_volume_snapshot(manager, app, generation, config, &root, volume) {
                Ok(job) => snapshots.push(job),
                Err(error) if !manager.is_cancelled(generation) => {
                    let reason = error.to_string();
                    set_provider_status(
                        manager,
                        app,
                        &root,
                        "folderScan",
                        "fallback",
                        Some(reason),
                    );
                    folder_roots.push(root);
                }
                Err(_) => return Ok(()),
            }
        }
        snapshots
            .sort_by_key(|job| ntfs_scan_priority(job.volume, job.records.len(), system_volume));
        for snapshot in snapshots {
            let root = snapshot.root.clone();
            match index_ntfs_volume_paths(manager, app, generation, snapshot) {
                Ok(job) => {
                    finalize_jobs.push(job);
                    set_provider_status(manager, app, &root, "windowsNtfs", "ready", None);
                }
                Err(error) if !manager.is_cancelled(generation) => {
                    let reason = error.to_string();
                    set_provider_status(
                        manager,
                        app,
                        &root,
                        "folderScan",
                        "fallback",
                        Some(reason),
                    );
                    folder_roots.push(root);
                }
                Err(_) => return Ok(()),
            }
        }
        scan_folder_roots(manager, app, generation, config, &folder_roots)?;
        mark_query_snapshot_complete(&manager.db_path)?;
        manager.status.lock().unwrap().phase = "ready".into();
        manager.refresh_counts();
        manager.emit_status(app);
        for job in finalize_jobs {
            persist_and_finalize_ntfs_volume(&manager.db_path, job)?;
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        for root in &config.roots {
            set_provider_status(manager, app, root, "folderScan", "scanning", None);
        }
        scan_folder_roots(manager, app, generation, config, &config.roots)?;
        manager.status.lock().unwrap().phase = "ready".into();
        manager.refresh_counts();
        manager.emit_status(app);
        Ok(())
    }
}

fn set_provider_status(
    manager: &FileSearchManager,
    app: &tauri::AppHandle,
    root: &str,
    provider: &str,
    phase: &str,
    fallback_reason: Option<String>,
) {
    let mut status = manager.status.lock().unwrap();
    if let Some(item) = status.providers.iter_mut().find(|item| item.root == root) {
        item.provider = provider.into();
        item.phase = phase.into();
        item.fallback_reason = fallback_reason;
    }
    drop(status);
    manager.emit_status(app);
}

#[cfg(windows)]
struct NtfsFinalizeJob {
    root: String,
    volume: char,
    exclusions: Vec<String>,
    journal_before: UsnJournalInfo,
    records: Vec<MftRecord>,
}

#[cfg(windows)]
fn scan_ntfs_volume(
    manager: &Arc<FileSearchManager>,
    app: &tauri::AppHandle,
    generation: u64,
    config: &SearchConfig,
    root: &str,
    volume: char,
) -> anyhow::Result<()> {
    let snapshot = enumerate_ntfs_volume_snapshot(manager, app, generation, config, root, volume)?;
    let job = index_ntfs_volume_paths(manager, app, generation, snapshot)?;
    persist_and_finalize_ntfs_volume(&manager.db_path, job)
}

#[cfg(windows)]
fn enumerate_ntfs_volume_snapshot(
    manager: &Arc<FileSearchManager>,
    app: &tauri::AppHandle,
    generation: u64,
    config: &SearchConfig,
    root: &str,
    volume: char,
) -> anyhow::Result<NtfsFinalizeJob> {
    let journal_before = query_usn_via_service(volume)?;
    let mut records = Vec::<MftRecord>::new();
    enumerate_mft_via_service(volume, |batch| {
        if manager.is_cancelled(generation) {
            anyhow::bail!("MFT 枚举已取消");
        }
        manager.status.lock().unwrap().scanned_files += batch.len() as u64;
        records.extend(batch);
        manager.emit_status(app);
        Ok(())
    })?;
    if manager.is_cancelled(generation) {
        anyhow::bail!("MFT enumeration was cancelled");
    }
    Ok(NtfsFinalizeJob {
        root: root.into(),
        volume,
        exclusions: config.exclusions.clone(),
        journal_before,
        records,
    })
}

#[cfg(windows)]
fn index_ntfs_volume_paths(
    manager: &Arc<FileSearchManager>,
    app: &tauri::AppHandle,
    generation: u64,
    snapshot: NtfsFinalizeJob,
) -> anyhow::Result<NtfsFinalizeJob> {
    let root = snapshot.root.clone();
    let (_, records) = resolve_mft_files_in_batches_retain(
        &root,
        snapshot.records,
        NTFS_RESOLVE_BATCH,
        |entries| {
            if manager.is_cancelled(generation) {
                anyhow::bail!("MFT 路径重建已取消");
            }
            let files = entries
                .into_iter()
                .filter(|entry| !is_excluded(Path::new(&entry.path), &snapshot.exclusions))
                .map(|entry| indexed_mft_entry(&root, entry))
                .collect::<Vec<_>>();
            manager.index_files(&files)?;
            manager.emit_status(app);
            Ok(())
        },
    )?;
    manager.commit_query_index()?;
    Ok(NtfsFinalizeJob {
        root,
        volume: snapshot.volume,
        exclusions: snapshot.exclusions,
        journal_before: snapshot.journal_before,
        records,
    })
}

#[cfg(windows)]
fn system_volume_letter() -> Option<char> {
    std::env::var("SystemDrive")
        .ok()
        .and_then(|drive| drive.chars().find(char::is_ascii_alphabetic))
        .map(|letter| letter.to_ascii_uppercase())
}

#[cfg(windows)]
fn ntfs_scan_priority(
    volume: char,
    record_count: usize,
    system_volume: Option<char>,
) -> (bool, usize) {
    (
        system_volume.is_some_and(|letter| letter.eq_ignore_ascii_case(&volume)),
        record_count,
    )
}

#[cfg(windows)]
fn persist_and_finalize_ntfs_volume(db_path: &Path, job: NtfsFinalizeJob) -> anyhow::Result<()> {
    let mut connection = open_database(db_path)?;
    connection.pragma_update(None, "synchronous", "OFF")?;
    connection.pragma_update(None, "cache_size", -65_536)?;
    connection.execute("DELETE FROM files WHERE root = ?1", params![&job.root])?;
    let (_, records) = resolve_mft_files_in_batches_retain(
        &job.root,
        job.records,
        NTFS_RESOLVE_BATCH,
        |entries| {
            let files = entries
                .into_iter()
                .filter(|entry| !is_excluded(Path::new(&entry.path), &job.exclusions))
                .map(|entry| indexed_mft_entry(&job.root, entry))
                .collect::<Vec<_>>();
            insert_batch(&mut connection, &files)
        },
    )?;
    replace_ntfs_nodes(&mut connection, &job.root, &records)?;
    let journal_after = query_usn_via_service(job.volume)?;
    if journal_after.journal_id != job.journal_before.journal_id
        || job.journal_before.next_usn < journal_after.first_usn
    {
        anyhow::bail!("USN Journal 在 MFT 快照期间失效，需要重新枚举该卷");
    }
    let mut changes = Vec::new();
    read_usn_via_service(
        job.volume,
        job.journal_before.next_usn,
        job.journal_before.journal_id,
        journal_after.next_usn,
        |batch| {
            changes.extend(batch);
            Ok(())
        },
    )?;
    apply_usn_changes(&mut connection, &job.root, &job.exclusions, changes)?;
    save_ntfs_volume_state(&connection, &job.root, job.volume, &journal_after, true)?;
    Ok(())
}

#[cfg(windows)]
fn indexed_mft_entry(root: &str, entry: crate::ntfs::ResolvedMftEntry) -> IndexedFile {
    IndexedFile {
        is_log: is_log_name(&entry.name),
        is_archive: is_archive_name(&entry.name),
        path: entry.path,
        name: entry.name,
        root: root.into(),
        size: 0,
        modified_ms: None,
        file_id: Some(entry.id.as_bytes()),
        parent_id: Some(entry.parent_id.as_bytes()),
    }
}

#[cfg(windows)]
fn replace_ntfs_nodes(
    connection: &mut Connection,
    root: &str,
    records: &[MftRecord],
) -> anyhow::Result<()> {
    connection.execute_batch("DROP INDEX IF EXISTS ntfs_nodes_parent_idx")?;
    let tx = connection.transaction()?;
    tx.execute("DELETE FROM ntfs_nodes WHERE root = ?1", params![root])?;
    for chunk in records.chunks(500) {
        let placeholders = (0..chunk.len())
            .map(|_| "(?, ?, ?, ?, ?, ?)")
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "INSERT INTO ntfs_nodes(root, file_id, parent_id, name, attributes, usn)
             VALUES {placeholders}"
        );
        let mut values = Vec::<rusqlite::types::Value>::with_capacity(chunk.len() * 6);
        for record in chunk {
            values.push(root.to_owned().into());
            values.push(record.id.as_bytes().to_vec().into());
            values.push(record.parent_id.as_bytes().to_vec().into());
            values.push(record.name.clone().into());
            values.push(i64::from(record.attributes).into());
            values.push(record.usn.into());
        }
        tx.execute(&sql, rusqlite::params_from_iter(values))?;
    }
    tx.commit()?;
    connection.execute_batch(
        "CREATE INDEX IF NOT EXISTS ntfs_nodes_parent_idx
           ON ntfs_nodes(root, parent_id)",
    )?;
    Ok(())
}

#[cfg(windows)]
fn load_ntfs_nodes(connection: &Connection, root: &str) -> anyhow::Result<Vec<MftRecord>> {
    let mut statement = connection.prepare(
        "SELECT file_id, parent_id, name, attributes, usn
         FROM ntfs_nodes WHERE root = ?1",
    )?;
    let rows = statement.query_map(params![root], |row| {
        let id = row.get::<_, Vec<u8>>(0)?;
        let parent_id = row.get::<_, Vec<u8>>(1)?;
        let to_id = |bytes: Vec<u8>| -> rusqlite::Result<crate::ntfs::FileId> {
            let bytes: [u8; 16] = bytes.try_into().map_err(|_| {
                rusqlite::Error::InvalidColumnType(0, "file_id".into(), rusqlite::types::Type::Blob)
            })?;
            Ok(crate::ntfs::FileId::from_bytes(bytes))
        };
        Ok(MftRecord {
            id: to_id(id)?,
            parent_id: to_id(parent_id)?,
            name: row.get(2)?,
            attributes: row.get(3)?,
            reason: 0,
            usn: row.get(4)?,
        })
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

#[cfg(windows)]
fn apply_usn_changes(
    connection: &mut Connection,
    root: &str,
    exclusions: &[String],
    changes: Vec<MftRecord>,
) -> anyhow::Result<()> {
    const USN_REASON_FILE_DELETE: u32 = 0x0000_0200;
    const USN_REASON_RENAME_OLD_NAME: u32 = 0x0000_1000;
    if changes.is_empty() {
        return Ok(());
    }
    let old_records = load_ntfs_nodes(connection, root)?;
    let old_by_id = old_records
        .iter()
        .cloned()
        .map(|record| (record.id, record))
        .collect::<HashMap<_, _>>();
    let mut new_by_id = old_by_id.clone();
    let mut changed_ids = HashSet::new();
    let mut changed_directories = HashSet::new();
    for change in &changes {
        changed_ids.insert(change.id);
        if change.is_directory()
            || old_by_id
                .get(&change.id)
                .is_some_and(MftRecord::is_directory)
        {
            changed_directories.insert(change.id);
        }
        if change.reason & USN_REASON_FILE_DELETE != 0 {
            new_by_id.remove(&change.id);
        } else if change.reason & USN_REASON_RENAME_OLD_NAME == 0 {
            new_by_id.insert(change.id, change.clone());
        }
    }
    let old_affected = affected_file_ids(&old_by_id, &changed_ids, &changed_directories);
    let new_affected = affected_file_ids(&new_by_id, &changed_ids, &changed_directories);

    let tx = connection.transaction()?;
    {
        let mut delete_node =
            tx.prepare_cached("DELETE FROM ntfs_nodes WHERE root = ?1 AND file_id = ?2")?;
        let mut upsert_node = tx.prepare_cached(
            "INSERT INTO ntfs_nodes(root, file_id, parent_id, name, attributes, usn)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(root, file_id) DO UPDATE SET
               parent_id=excluded.parent_id, name=excluded.name,
               attributes=excluded.attributes, usn=excluded.usn",
        )?;
        for change in &changes {
            if change.reason & USN_REASON_FILE_DELETE != 0 {
                delete_node.execute(params![root, change.id.as_bytes().as_slice()])?;
            } else if change.reason & USN_REASON_RENAME_OLD_NAME == 0 {
                upsert_node.execute(params![
                    root,
                    change.id.as_bytes().as_slice(),
                    change.parent_id.as_bytes().as_slice(),
                    change.name,
                    change.attributes,
                    change.usn,
                ])?;
            }
        }
        let mut delete_file =
            tx.prepare_cached("DELETE FROM files WHERE root = ?1 AND file_id = ?2")?;
        for id in &old_affected {
            delete_file.execute(params![root, id.as_bytes().as_slice()])?;
        }
    }
    tx.commit()?;

    let records = new_by_id.into_values().collect::<Vec<_>>();
    resolve_mft_files_in_batches(root, records, NTFS_RESOLVE_BATCH, |entries| {
        let files = entries
            .into_iter()
            .filter(|entry| new_affected.contains(&entry.id))
            .filter(|entry| !is_excluded(Path::new(&entry.path), exclusions))
            .map(|entry| indexed_mft_entry(root, entry))
            .collect::<Vec<_>>();
        write_batch(connection, &files)
    })?;
    Ok(())
}

#[cfg(windows)]
fn affected_file_ids(
    records: &HashMap<crate::ntfs::FileId, MftRecord>,
    changed_ids: &HashSet<crate::ntfs::FileId>,
    changed_directories: &HashSet<crate::ntfs::FileId>,
) -> HashSet<crate::ntfs::FileId> {
    records
        .values()
        .filter(|record| !record.is_directory())
        .filter(|record| {
            if changed_ids.contains(&record.id) {
                return true;
            }
            let mut current = record.parent_id;
            let mut visited = HashSet::new();
            while visited.insert(current) {
                if changed_directories.contains(&current) {
                    return true;
                }
                let Some(parent) = records.get(&current) else {
                    break;
                };
                if parent.id == parent.parent_id {
                    break;
                }
                current = parent.parent_id;
            }
            false
        })
        .map(|record| record.id)
        .collect()
}

#[cfg(windows)]
fn save_ntfs_volume_state(
    connection: &Connection,
    root: &str,
    volume: char,
    journal: &UsnJournalInfo,
    snapshot_complete: bool,
) -> anyhow::Result<()> {
    let identity = format!(
        "{}:{}",
        volume.to_ascii_uppercase(),
        ntfs_volume_serial(root).unwrap_or(0)
    );
    connection.execute(
        "INSERT INTO search_volumes(
           root, provider, volume_identity, journal_id, next_usn,
           provider_version, schema_version, snapshot_complete
         ) VALUES(?1, 'windowsNtfs', ?2, ?3, ?4, 1, ?5, ?6)
         ON CONFLICT(root) DO UPDATE SET
           provider=excluded.provider, volume_identity=excluded.volume_identity,
           journal_id=excluded.journal_id, next_usn=excluded.next_usn,
           provider_version=excluded.provider_version,
           schema_version=excluded.schema_version,
           snapshot_complete=excluded.snapshot_complete",
        params![
            root,
            identity,
            journal.journal_id.to_le_bytes().as_slice(),
            journal.next_usn,
            SCHEMA_VERSION,
            snapshot_complete,
        ],
    )?;
    Ok(())
}

#[cfg(windows)]
#[derive(Debug)]
struct NtfsVolumeState {
    volume_identity: String,
    journal_id: u64,
    next_usn: i64,
    snapshot_complete: bool,
}

#[cfg(windows)]
fn load_ntfs_volume_state(
    connection: &Connection,
    root: &str,
) -> anyhow::Result<Option<NtfsVolumeState>> {
    connection
        .query_row(
            "SELECT volume_identity, journal_id, next_usn, snapshot_complete
             FROM search_volumes
             WHERE root = ?1 AND provider = 'windowsNtfs'
               AND provider_version = 1 AND schema_version = ?2",
            params![root, SCHEMA_VERSION],
            |row| {
                let journal_id = row.get::<_, Vec<u8>>(1)?;
                let journal_id: [u8; 8] = journal_id.try_into().map_err(|_| {
                    rusqlite::Error::InvalidColumnType(
                        1,
                        "journal_id".into(),
                        rusqlite::types::Type::Blob,
                    )
                })?;
                Ok(NtfsVolumeState {
                    volume_identity: row.get(0)?,
                    journal_id: u64::from_le_bytes(journal_id),
                    next_usn: row.get(2)?,
                    snapshot_complete: row.get(3)?,
                })
            },
        )
        .optional()
        .map_err(Into::into)
}

#[cfg(windows)]
fn catch_up_ntfs_volume(
    db_path: &Path,
    root: &str,
    volume: char,
    exclusions: &[String],
) -> anyhow::Result<()> {
    let mut connection = open_database(db_path)?;
    let state = load_ntfs_volume_state(&connection, root)?
        .filter(|state| state.snapshot_complete)
        .ok_or_else(|| anyhow::anyhow!("NTFS 快照未完成"))?;
    let identity = format!(
        "{}:{}",
        volume.to_ascii_uppercase(),
        ntfs_volume_serial(root).unwrap_or(0)
    );
    let journal = query_usn_via_service(volume)?;
    if state.volume_identity != identity
        || state.journal_id != journal.journal_id
        || state.next_usn < journal.first_usn
        || state.next_usn > journal.next_usn
    {
        anyhow::bail!("NTFS 卷身份或 USN 断点已失效");
    }
    let mut changes = Vec::new();
    read_usn_via_service(
        volume,
        state.next_usn,
        state.journal_id,
        journal.next_usn,
        |batch| {
            if changes.len().saturating_add(batch.len()) > MAX_USN_REPLAY_RECORDS {
                anyhow::bail!("USN 追赶记录超过有界上限，需要重建该卷");
            }
            changes.extend(batch);
            Ok(())
        },
    )?;
    apply_usn_changes(&mut connection, root, exclusions, changes)?;
    save_ntfs_volume_state(&connection, root, volume, &journal, true)
}

fn scan_folder_roots(
    manager: &Arc<FileSearchManager>,
    app: &tauri::AppHandle,
    generation: u64,
    config: &SearchConfig,
    roots: &[String],
) -> anyhow::Result<()> {
    let mut connection = open_database(&manager.db_path)?;
    connection.pragma_update(None, "synchronous", "OFF")?;
    connection.pragma_update(None, "cache_size", -65_536)?;
    let mut pending = Vec::with_capacity(SCAN_WRITE_BATCH);
    for root in roots {
        if manager.is_cancelled(generation) {
            return Ok(());
        }
        let root_path = PathBuf::from(root);
        if !root_path.is_dir() || is_excluded(&root_path, &config.exclusions) {
            continue;
        }
        let mut directories = vec![root_path.clone()];
        #[cfg(all(unix, not(target_os = "macos")))]
        let root_device = unix_device(&root_path);
        while let Some(directory) = directories.pop() {
            if manager.is_cancelled(generation) {
                return Ok(());
            }
            let entries = match fs::read_dir(&directory) {
                Ok(entries) => entries,
                Err(_) => {
                    manager.status.lock().unwrap().skipped_directories += 1;
                    continue;
                }
            };
            for entry in entries.flatten() {
                if manager.is_cancelled(generation) {
                    return Ok(());
                }
                let path = entry.path();
                if is_excluded(&path, &config.exclusions) {
                    continue;
                }
                let Ok(file_type) = entry.file_type() else {
                    continue;
                };
                if file_type.is_symlink() {
                    continue;
                }
                if file_type.is_dir() {
                    if is_reparse_point(&entry) || is_platform_skipped_directory(&path) {
                        continue;
                    }
                    #[cfg(all(unix, not(target_os = "macos")))]
                    if root_device.is_some() && unix_device(&path) != root_device {
                        continue;
                    }
                    directories.push(path);
                    continue;
                }
                if !file_type.is_file() {
                    continue;
                }
                if let Some(indexed) = indexed_file(&path, root) {
                    pending.push(indexed);
                    let mut status = manager.status.lock().unwrap();
                    status.scanned_files += 1;
                    let scanned = status.scanned_files;
                    drop(status);
                    if pending.len() >= SCAN_WRITE_BATCH {
                        write_batch(&mut connection, &pending)?;
                        manager.index_files(&pending)?;
                        pending.clear();
                    }
                    if scanned % 2048 == 0 {
                        manager.refresh_counts();
                        manager.emit_status(app);
                    }
                }
            }
        }
    }
    if !pending.is_empty() {
        write_batch(&mut connection, &pending)?;
        manager.index_files(&pending)?;
    }
    manager.commit_query_index()?;
    for root in roots {
        let fallback_reason = manager
            .status
            .lock()
            .unwrap()
            .providers
            .iter()
            .find(|item| item.root == *root)
            .and_then(|item| item.fallback_reason.clone());
        set_provider_status(manager, app, root, "folderScan", "ready", fallback_reason);
    }
    Ok(())
}

fn apply_event_paths(
    db_path: &Path,
    config: &SearchConfig,
    paths: &[PathBuf],
) -> anyhow::Result<()> {
    let mut connection = open_database(db_path)?;
    let tx = connection.transaction()?;
    for path in paths {
        let Some(root) = containing_root(path, &config.roots) else {
            continue;
        };
        if is_excluded(path, &config.exclusions) || is_platform_skipped_directory(path) {
            continue;
        }
        if path.is_file() {
            if let Some(file) = indexed_file(path, root) {
                upsert_file(&tx, &file)?;
            }
        } else if path.is_dir() {
            upsert_subtree(&tx, path, root, &config.exclusions)?;
        } else {
            let value = path.to_string_lossy();
            let prefix = format!("{}{}", value, std::path::MAIN_SEPARATOR);
            tx.execute(
                "DELETE FROM files WHERE path = ?1 OR substr(path, 1, length(?2)) = ?2",
                params![value.as_ref(), prefix],
            )?;
        }
    }
    tx.commit()?;
    Ok(())
}

fn drain_event_paths(
    db_path: &Path,
    config: &SearchConfig,
    receiver: &Receiver<Event>,
) -> anyhow::Result<()> {
    loop {
        let mut paths = Vec::new();
        loop {
            match receiver.try_recv() {
                Ok(event) => paths.extend(event.paths),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return Ok(()),
            }
            if paths.len() >= EVENT_BATCH {
                break;
            }
        }
        if paths.is_empty() {
            return Ok(());
        }
        paths.sort();
        paths.dedup();
        apply_event_paths(db_path, config, &paths)?;
    }
}

fn upsert_subtree(
    connection: &Connection,
    path: &Path,
    root: &str,
    exclusions: &[String],
) -> anyhow::Result<()> {
    let mut directories = vec![path.to_path_buf()];
    #[cfg(all(unix, not(target_os = "macos")))]
    let root_device = unix_device(Path::new(root));
    while let Some(directory) = directories.pop() {
        if is_excluded(&directory, exclusions) || is_platform_skipped_directory(&directory) {
            continue;
        }
        let Ok(entries) = fs::read_dir(directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if is_excluded(&path, exclusions) {
                continue;
            }
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_symlink() {
                continue;
            }
            if file_type.is_dir() {
                if is_reparse_point(&entry) || is_platform_skipped_directory(&path) {
                    continue;
                }
                #[cfg(all(unix, not(target_os = "macos")))]
                if root_device.is_some() && unix_device(&path) != root_device {
                    continue;
                }
                directories.push(path);
            } else if file_type.is_file() {
                if let Some(file) = indexed_file(&path, root) {
                    upsert_file(connection, &file)?;
                }
            }
        }
    }
    Ok(())
}

fn containing_root<'a>(path: &Path, roots: &'a [String]) -> Option<&'a str> {
    roots
        .iter()
        .filter(|root| path_is_within(path, Path::new(root)))
        .max_by_key(|root| Path::new(root).components().count())
        .map(String::as_str)
}

#[derive(Debug, Clone, Copy, Default)]
struct DatabaseInitialization {
    query_snapshot_complete: bool,
    persistence_incomplete: bool,
}

#[cfg(test)]
fn initialize_database(path: &Path) -> anyhow::Result<DatabaseInitialization> {
    initialize_database_with_query(path, None)
}

fn initialize_database_with_query(
    path: &Path,
    query_documents: Option<u64>,
) -> anyhow::Result<DatabaseInitialization> {
    let connection = open_database(path)?;
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS metadata(key TEXT PRIMARY KEY, value INTEGER NOT NULL);
         CREATE TABLE IF NOT EXISTS files(
           path TEXT PRIMARY KEY NOT NULL,
           name TEXT NOT NULL,
           parent TEXT NOT NULL,
           root TEXT NOT NULL,
           size INTEGER NOT NULL,
           modified_ms INTEGER,
           is_log INTEGER NOT NULL,
           is_archive INTEGER NOT NULL,
           file_id BLOB,
           parent_id BLOB
         );
         DROP INDEX IF EXISTS files_name_idx;
         DROP INDEX IF EXISTS files_modified_idx;
         CREATE TABLE IF NOT EXISTS ntfs_nodes(
           root TEXT NOT NULL,
           file_id BLOB NOT NULL,
           parent_id BLOB NOT NULL,
           name TEXT NOT NULL,
           attributes INTEGER NOT NULL,
           usn INTEGER NOT NULL,
           PRIMARY KEY(root, file_id)
         );
         CREATE INDEX IF NOT EXISTS ntfs_nodes_parent_idx
           ON ntfs_nodes(root, parent_id);
         CREATE TABLE IF NOT EXISTS search_volumes(
           root TEXT PRIMARY KEY NOT NULL,
           provider TEXT NOT NULL,
           volume_identity TEXT NOT NULL,
           journal_id BLOB,
           next_usn INTEGER,
           provider_version INTEGER NOT NULL,
           schema_version INTEGER NOT NULL,
           snapshot_complete INTEGER NOT NULL
         );
         CREATE TABLE IF NOT EXISTS search_index_changes(
           path TEXT PRIMARY KEY NOT NULL,
           operation INTEGER NOT NULL
         );
         CREATE VIRTUAL TABLE IF NOT EXISTS files_fts USING fts5(
           name, path, content='files', content_rowid='rowid',
           tokenize='trigram', detail='none', columnsize=0
         );",
    )?;
    ensure_file_identity_columns(&connection)?;
    let version = connection
        .query_row(
            "SELECT value FROM metadata WHERE key = 'schema_version'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .optional()?;
    let incomplete = connection
        .query_row(
            "SELECT value FROM metadata WHERE key = 'bulk_rebuild'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .optional()?
        == Some(1);
    let query_snapshot_marker = connection
        .query_row(
            "SELECT value FROM metadata WHERE key = 'query_snapshot_complete'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .optional()?;
    let persisted_files = connection
        .query_row("SELECT COUNT(*) FROM files", [], |row| row.get::<_, u64>(0))
        .unwrap_or(0);
    let legacy_completed_query = query_snapshot_marker.is_none()
        && incomplete
        && persisted_files > 0
        && query_documents.is_some_and(|documents| documents > 0);
    let query_snapshot_complete = query_snapshot_marker == Some(1) || legacy_completed_query;
    let mut state = DatabaseInitialization::default();
    if version != Some(SCHEMA_VERSION) {
        recreate_index(&connection)?;
        connection.execute(
            "INSERT OR REPLACE INTO metadata(key, value) VALUES('schema_version', ?1)",
            params![SCHEMA_VERSION],
        )?;
    } else if incomplete && query_snapshot_complete {
        reset_persistence(&connection)?;
        state.query_snapshot_complete = true;
        state.persistence_incomplete = true;
    } else if incomplete {
        reset_index(&connection)?;
    } else {
        connection.execute_batch(CREATE_FTS_TRIGGERS)?;
        connection.execute_batch(CREATE_QUERY_CHANGE_TRIGGERS)?;
    }
    connection.execute(
        "INSERT OR IGNORE INTO metadata(key, value) VALUES('fts_ready', 1)",
        [],
    )?;
    if !state.query_snapshot_complete {
        state.query_snapshot_complete = connection
            .query_row(
                "SELECT value FROM metadata WHERE key = 'query_snapshot_complete'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            == Some(1);
    }
    Ok(state)
}

fn recreate_index(connection: &Connection) -> anyhow::Result<()> {
    connection.execute_batch(DROP_FTS_TRIGGERS)?;
    connection.execute_batch(DROP_QUERY_CHANGE_TRIGGERS)?;
    connection.execute("DELETE FROM files", [])?;
    connection.execute("DELETE FROM ntfs_nodes", [])?;
    connection.execute("DELETE FROM search_volumes", [])?;
    connection.execute("DELETE FROM search_index_changes", [])?;
    connection.execute("DROP TABLE IF EXISTS files_fts", [])?;
    connection.execute_batch(CREATE_FTS_TABLE)?;
    connection.execute_batch(
        "INSERT OR REPLACE INTO metadata(key, value) VALUES('bulk_rebuild', 0);
         INSERT OR REPLACE INTO metadata(key, value) VALUES('fts_ready', 1);
         INSERT OR REPLACE INTO metadata(key, value) VALUES('query_snapshot_complete', 0);",
    )?;
    connection.execute_batch(CREATE_FTS_TRIGGERS)?;
    connection.execute_batch(CREATE_QUERY_CHANGE_TRIGGERS)?;
    Ok(())
}

fn ensure_file_identity_columns(connection: &Connection) -> anyhow::Result<()> {
    let mut statement = connection.prepare("PRAGMA table_info(files)")?;
    let columns = statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<HashSet<_>, _>>()?;
    if !columns.contains("file_id") {
        connection.execute("ALTER TABLE files ADD COLUMN file_id BLOB", [])?;
    }
    if !columns.contains("parent_id") {
        connection.execute("ALTER TABLE files ADD COLUMN parent_id BLOB", [])?;
    }
    Ok(())
}

fn reset_index(connection: &Connection) -> anyhow::Result<()> {
    connection.execute_batch(DROP_FTS_TRIGGERS)?;
    connection.execute_batch(DROP_QUERY_CHANGE_TRIGGERS)?;
    connection.execute("DELETE FROM files", [])?;
    connection.execute("DELETE FROM ntfs_nodes", [])?;
    connection.execute("DELETE FROM search_volumes", [])?;
    connection.execute("DELETE FROM search_index_changes", [])?;
    connection.execute_batch(
        "INSERT INTO files_fts(files_fts) VALUES('rebuild');
         INSERT OR REPLACE INTO metadata(key, value) VALUES('bulk_rebuild', 0);
         INSERT OR REPLACE INTO metadata(key, value) VALUES('fts_ready', 1);
         INSERT OR REPLACE INTO metadata(key, value) VALUES('query_snapshot_complete', 0);",
    )?;
    connection.execute_batch(CREATE_FTS_TRIGGERS)?;
    connection.execute_batch(CREATE_QUERY_CHANGE_TRIGGERS)?;
    Ok(())
}

fn reset_persistence(connection: &Connection) -> anyhow::Result<()> {
    connection.execute_batch(DROP_FTS_TRIGGERS)?;
    connection.execute_batch(DROP_QUERY_CHANGE_TRIGGERS)?;
    connection.execute("DELETE FROM search_index_changes", [])?;
    connection.execute_batch(
        "INSERT OR REPLACE INTO metadata(key, value) VALUES('bulk_rebuild', 1);
         INSERT OR REPLACE INTO metadata(key, value) VALUES('fts_ready', 0);
         INSERT OR REPLACE INTO metadata(key, value) VALUES('query_snapshot_complete', 1);",
    )?;
    Ok(())
}

fn prepare_bulk_index(path: &Path, rebuild: bool) -> anyhow::Result<()> {
    let connection = open_database(path)?;
    connection.execute_batch(
        "INSERT OR REPLACE INTO metadata(key, value) VALUES('bulk_rebuild', 1);
         INSERT OR REPLACE INTO metadata(key, value) VALUES('fts_ready', 0);",
    )?;
    connection.execute_batch(DROP_FTS_TRIGGERS)?;
    connection.execute_batch(DROP_QUERY_CHANGE_TRIGGERS)?;
    connection.execute("DELETE FROM search_index_changes", [])?;
    if rebuild {
        connection.execute(
            "INSERT OR REPLACE INTO metadata(key, value) VALUES('query_snapshot_complete', 0)",
            [],
        )?;
        connection.execute("DELETE FROM files", [])?;
        connection.execute("DELETE FROM ntfs_nodes", [])?;
        connection.execute("DELETE FROM search_volumes", [])?;
    }
    Ok(())
}

#[cfg(any(windows, test))]
fn mark_query_snapshot_complete(path: &Path) -> anyhow::Result<()> {
    let connection = open_database(path)?;
    connection.execute(
        "INSERT OR REPLACE INTO metadata(key, value) VALUES('query_snapshot_complete', 1)",
        [],
    )?;
    Ok(())
}

fn finish_bulk_index(path: &Path) -> anyhow::Result<()> {
    let connection = open_database(path)?;
    connection.execute_batch(
        "INSERT OR REPLACE INTO metadata(key, value) VALUES('bulk_rebuild', 0);
         INSERT OR REPLACE INTO metadata(key, value) VALUES('fts_ready', 0);
         INSERT OR REPLACE INTO metadata(key, value) VALUES('query_snapshot_complete', 1);",
    )?;
    connection.execute_batch(CREATE_QUERY_CHANGE_TRIGGERS)?;
    connection.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
    Ok(())
}

fn open_database(path: &Path) -> anyhow::Result<Connection> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let connection = Connection::open(path)?;
    connection.busy_timeout(Duration::from_secs(5))?;
    connection.pragma_update(None, "journal_mode", "WAL")?;
    connection.pragma_update(None, "synchronous", "NORMAL")?;
    Ok(connection)
}

fn clear_rows(path: &Path) -> anyhow::Result<()> {
    let connection = open_database(path)?;
    reset_index(&connection)?;
    connection.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
    Ok(())
}

fn write_batch(connection: &mut Connection, files: &[IndexedFile]) -> anyhow::Result<()> {
    write_files(connection, files, true)
}

#[cfg(windows)]
fn insert_batch(connection: &mut Connection, files: &[IndexedFile]) -> anyhow::Result<()> {
    write_files(connection, files, false)
}

fn write_files(
    connection: &mut Connection,
    files: &[IndexedFile],
    update_existing: bool,
) -> anyhow::Result<()> {
    let tx = connection.transaction()?;
    for chunk in files.chunks(500) {
        let placeholders = (0..chunk.len())
            .map(|_| "(?, ?, ?, ?, ?, ?, ?, ?, ?, ?)")
            .collect::<Vec<_>>()
            .join(",");
        let conflict = if update_existing {
            "ON CONFLICT(path) DO UPDATE SET
               name=excluded.name, parent=excluded.parent, root=excluded.root,
               size=excluded.size, modified_ms=excluded.modified_ms,
               is_log=excluded.is_log, is_archive=excluded.is_archive,
               file_id=excluded.file_id, parent_id=excluded.parent_id"
        } else {
            "ON CONFLICT(path) DO NOTHING"
        };
        let sql = format!(
            "INSERT INTO files(
               path, name, parent, root, size, modified_ms, is_log, is_archive, file_id, parent_id
             ) VALUES {placeholders}
             {conflict}"
        );
        let mut values = Vec::<rusqlite::types::Value>::with_capacity(chunk.len() * 10);
        for file in chunk {
            values.push(file.path.clone().into());
            values.push(file.name.clone().into());
            values.push(String::new().into());
            values.push(file.root.clone().into());
            values.push((file.size as i64).into());
            values.push(
                file.modified_ms
                    .map(|value| rusqlite::types::Value::Integer(value as i64))
                    .unwrap_or(rusqlite::types::Value::Null),
            );
            values.push((file.is_log as i64).into());
            values.push((file.is_archive as i64).into());
            values.push(
                file.file_id
                    .map(|value| rusqlite::types::Value::Blob(value.to_vec()))
                    .unwrap_or(rusqlite::types::Value::Null),
            );
            values.push(
                file.parent_id
                    .map(|value| rusqlite::types::Value::Blob(value.to_vec()))
                    .unwrap_or(rusqlite::types::Value::Null),
            );
        }
        tx.execute(&sql, rusqlite::params_from_iter(values))?;
    }
    tx.commit()?;
    Ok(())
}

fn upsert_file(connection: &Connection, file: &IndexedFile) -> anyhow::Result<()> {
    connection.execute(
        "INSERT INTO files(
           path, name, parent, root, size, modified_ms, is_log, is_archive, file_id, parent_id
         ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
         ON CONFLICT(path) DO UPDATE SET
           name=excluded.name, parent=excluded.parent, root=excluded.root,
           size=excluded.size, modified_ms=excluded.modified_ms,
           is_log=excluded.is_log, is_archive=excluded.is_archive,
           file_id=excluded.file_id, parent_id=excluded.parent_id",
        params![
            file.path,
            file.name,
            "",
            file.root,
            file.size,
            file.modified_ms,
            file.is_log,
            file.is_archive,
            file.file_id.as_ref().map(|id| id.as_slice()),
            file.parent_id.as_ref().map(|id| id.as_slice()),
        ],
    )?;
    Ok(())
}

#[cfg(test)]
fn query_fts(
    connection: &Connection,
    terms: &[String],
    filter_sql: &str,
    offset: u32,
    limit: u32,
) -> anyhow::Result<(Vec<SearchResultItem>, u64)> {
    let expression = terms
        .iter()
        .flat_map(|term| {
            let chars = term.chars().collect::<Vec<_>>();
            chars
                .windows(3)
                .map(|window| {
                    let token = window.iter().collect::<String>();
                    format!("\"{}\"", token.replace('"', "\"\""))
                })
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>()
        .join(" AND ");
    let validation = terms
        .iter()
        .enumerate()
        .map(|(index, _)| {
            format!(
                " AND instr(lower(f.name || ' ' || f.path), ?{}) > 0",
                index + 2
            )
        })
        .collect::<String>();
    let from = format!(
        " FROM files_fts s JOIN files f ON f.rowid = s.rowid
          WHERE files_fts MATCH ?1{validation}{filter_sql}"
    );
    let mut count_values = Vec::<rusqlite::types::Value>::with_capacity(terms.len() + 1);
    count_values.push(expression.clone().into());
    count_values.extend(terms.iter().cloned().map(Into::into));
    let total = connection.query_row(
        &format!("SELECT COUNT(*){from}"),
        rusqlite::params_from_iter(count_values),
        |row| row.get(0),
    )?;
    let exact_index = terms.len() + 2;
    let prefix_index = exact_index + 1;
    let limit_index = exact_index + 2;
    let offset_index = exact_index + 3;
    let sql = format!(
        "SELECT f.path, f.name, f.parent, f.size, f.modified_ms, f.is_log, f.is_archive{from}
         ORDER BY CASE WHEN lower(f.name) = ?{exact_index}
                       THEN 0 WHEN lower(f.name) LIKE ?{prefix_index} THEN 1 ELSE 2 END,
                  length(f.name), f.name COLLATE NOCASE, f.path COLLATE NOCASE
         LIMIT ?{limit_index} OFFSET ?{offset_index}"
    );
    let first = terms.first().cloned().unwrap_or_default();
    let mut values = Vec::<rusqlite::types::Value>::with_capacity(terms.len() + 5);
    values.push(expression.into());
    values.extend(terms.iter().cloned().map(Into::into));
    values.push(first.clone().into());
    values.push(format!("{first}%").into());
    values.push(i64::from(limit).into());
    values.push(i64::from(offset).into());
    let mut statement = connection.prepare(&sql)?;
    let rows = statement.query_map(rusqlite::params_from_iter(values), result_item)?;
    Ok((rows.collect::<Result<Vec<_>, _>>()?, total))
}

fn query_like(
    connection: &Connection,
    terms: &[String],
    filter_sql: &str,
    offset: u32,
    limit: u32,
) -> anyhow::Result<(Vec<SearchResultItem>, u64)> {
    let clauses = terms
        .iter()
        .enumerate()
        .map(|(index, _)| format!("instr(lower(f.name || ' ' || f.path), ?{}) > 0", index + 1))
        .collect::<Vec<_>>()
        .join(" AND ");
    let where_sql = format!(" WHERE {clauses}{filter_sql}");
    let values = terms
        .iter()
        .map(|term| term as &dyn rusqlite::ToSql)
        .collect::<Vec<_>>();
    let total = connection.query_row(
        &format!("SELECT COUNT(*) FROM files f{where_sql}"),
        values.as_slice(),
        |row| row.get(0),
    )?;
    let sql = format!(
        "SELECT f.path, f.name, f.parent, f.size, f.modified_ms, f.is_log, f.is_archive
         FROM files f{where_sql}
         ORDER BY length(f.name), f.name COLLATE NOCASE, f.path COLLATE NOCASE
         LIMIT {limit} OFFSET {offset}"
    );
    let mut statement = connection.prepare(&sql)?;
    let rows = statement.query_map(values.as_slice(), result_item)?;
    Ok((rows.collect::<Result<Vec<_>, _>>()?, total))
}

fn result_item(row: &rusqlite::Row<'_>) -> rusqlite::Result<SearchResultItem> {
    let path = row.get::<_, String>(0)?;
    let stored_parent = row.get::<_, String>(2)?;
    let is_log = row.get::<_, bool>(5)?;
    let is_archive = row.get::<_, bool>(6)?;
    Ok(SearchResultItem {
        parent: if stored_parent.is_empty() {
            Path::new(&path)
                .parent()
                .map(|parent| parent.to_string_lossy().into_owned())
                .unwrap_or_default()
        } else {
            stored_parent
        },
        path,
        name: row.get(1)?,
        size: row.get(3)?,
        modified_ms: row.get(4)?,
        readable: false,
        content_type: content_type_for_name(&row.get::<_, String>(1)?).into(),
        kind: if is_archive {
            "archive".into()
        } else if is_log {
            "log".into()
        } else {
            "file".into()
        },
        is_log,
        is_archive,
    })
}

fn indexed_file(path: &Path, root: &str) -> Option<IndexedFile> {
    let metadata = fs::metadata(path).ok()?;
    if !metadata.is_file() {
        return None;
    }
    let name = path.file_name()?.to_string_lossy().into_owned();
    let is_archive = is_archive_name(&name);
    Some(IndexedFile {
        path: path.to_string_lossy().into_owned(),
        is_log: is_log_name(&name),
        is_archive,
        name,
        root: root.into(),
        size: metadata.len(),
        modified_ms: metadata.modified().ok().and_then(system_time_ms),
        file_id: None,
        parent_id: None,
    })
}

fn enrich_visible_metadata(items: &mut [SearchResultItem]) {
    if items.is_empty() {
        return;
    }
    let workers = items.len().min(METADATA_WORKERS_MAX);
    let chunk_size = (items.len() + workers - 1) / workers;
    std::thread::scope(|scope| {
        for chunk in items.chunks_mut(chunk_size) {
            scope.spawn(move || {
                for item in chunk {
                    let Ok(metadata) = fs::metadata(&item.path) else {
                        continue;
                    };
                    if !metadata.is_file() {
                        continue;
                    }
                    item.size = metadata.len();
                    item.modified_ms = metadata.modified().ok().and_then(system_time_ms);
                    item.readable = fs::File::open(&item.path).is_ok();
                }
            });
        }
    });
}

fn content_type_for_name(name: &str) -> &'static str {
    let extension = Path::new(name)
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    match extension.as_str() {
        "zip" => "application/zip",
        "7z" => "application/x-7z-compressed",
        "rar" => "application/vnd.rar",
        "tar" => "application/x-tar",
        "gz" | "tgz" => "application/gzip",
        "bz2" | "tbz2" => "application/x-bzip2",
        "xz" | "txz" => "application/x-xz",
        "log" | "txt" | "out" | "err" | "trace" | "json" | "xml" | "yaml" | "yml" => "text/plain",
        _ => "application/octet-stream",
    }
}

fn system_time_ms(time: SystemTime) -> Option<u64> {
    time.duration_since(UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_millis() as u64)
}

fn is_excluded(path: &Path, exclusions: &[String]) -> bool {
    exclusions
        .iter()
        .any(|excluded| path_is_within(path, Path::new(excluded)))
}

#[cfg(windows)]
fn path_is_within(path: &Path, ancestor: &Path) -> bool {
    let path = path.to_string_lossy().replace('/', "\\").to_lowercase();
    let ancestor = ancestor
        .to_string_lossy()
        .replace('/', "\\")
        .trim_end_matches('\\')
        .to_lowercase();
    path == ancestor || path.starts_with(&format!("{ancestor}\\"))
}

#[cfg(not(windows))]
fn path_is_within(path: &Path, ancestor: &Path) -> bool {
    path.starts_with(ancestor)
}

#[cfg(target_os = "macos")]
fn is_platform_skipped_directory(path: &Path) -> bool {
    [
        "/Volumes",
        "/System/Volumes",
        "/Network",
        "/dev",
        "/net",
        "/home",
    ]
    .iter()
    .any(|prefix| path.starts_with(prefix))
}

#[cfg(not(target_os = "macos"))]
fn is_platform_skipped_directory(_path: &Path) -> bool {
    false
}

fn normalize_unique_paths(paths: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    paths
        .into_iter()
        .filter_map(|path| {
            let trimmed = path.trim();
            if trimmed.is_empty() {
                return None;
            }
            let normalized = PathBuf::from(trimmed).to_string_lossy().into_owned();
            path_key(&normalized).and_then(|key| seen.insert(key).then_some(normalized))
        })
        .collect()
}

fn path_key(path: &str) -> Option<String> {
    if path.is_empty() {
        None
    } else if cfg!(windows) {
        Some(path.replace('/', "\\").to_lowercase())
    } else {
        Some(path.into())
    }
}

fn read_config(path: &Path) -> Option<SearchConfig> {
    let bytes = fs::read(path).ok()?;
    let mut config = serde_json::from_slice::<SearchConfig>(&bytes).ok()?;
    config.version = search_config_version();
    config.roots = normalize_unique_paths(config.roots);
    config.exclusions = normalize_unique_paths(config.exclusions);
    Some(config)
}

fn write_config(path: &Path, config: &SearchConfig) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension("json.tmp");
    fs::write(&temporary, serde_json::to_vec_pretty(config)?)?;
    if let Err(error) = replace_file(&temporary, path) {
        let _ = fs::remove_file(&temporary);
        return Err(error.into());
    }
    Ok(())
}

#[cfg(not(windows))]
fn replace_file(source: &Path, destination: &Path) -> std::io::Result<()> {
    fs::rename(source, destination)
}

#[cfg(windows)]
fn replace_file(source: &Path, destination: &Path) -> std::io::Result<()> {
    use std::iter::once;
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };

    let source = source
        .as_os_str()
        .encode_wide()
        .chain(once(0))
        .collect::<Vec<_>>();
    let destination = destination
        .as_os_str()
        .encode_wide()
        .chain(once(0))
        .collect::<Vec<_>>();
    let result = unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if result == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn database_size(path: &Path) -> u64 {
    [path.to_path_buf(), path.with_extension("sqlite3-wal")]
        .iter()
        .filter_map(|path| fs::metadata(path).ok().map(|metadata| metadata.len()))
        .sum()
}

fn planned_provider_statuses(roots: &[String]) -> Vec<SearchProviderStatus> {
    roots
        .iter()
        .map(|root| SearchProviderStatus {
            root: root.clone(),
            provider: planned_provider(root).into(),
            phase: "pending".into(),
            fallback_reason: None,
        })
        .collect()
}

#[cfg(windows)]
fn planned_provider(root: &str) -> &'static str {
    if ntfs_volume_letter(root).is_some() {
        "windowsNtfs"
    } else {
        "folderScan"
    }
}

#[cfg(not(windows))]
fn planned_provider(_root: &str) -> &'static str {
    "folderScan"
}

#[cfg(windows)]
fn ntfs_volume_letter(root: &str) -> Option<char> {
    ntfs_volume_details(root).map(|(letter, _)| letter)
}

#[cfg(windows)]
fn ntfs_volume_serial(root: &str) -> Option<u32> {
    ntfs_volume_details(root).map(|(_, serial)| serial)
}

#[cfg(windows)]
fn ntfs_volume_details(root: &str) -> Option<(char, u32)> {
    const DRIVE_FIXED: u32 = 3;
    #[link(name = "kernel32")]
    extern "system" {
        fn GetDriveTypeW(root_path_name: *const u16) -> u32;
        fn GetVolumeInformationW(
            root_path_name: *const u16,
            volume_name_buffer: *mut u16,
            volume_name_size: u32,
            volume_serial_number: *mut u32,
            maximum_component_length: *mut u32,
            file_system_flags: *mut u32,
            file_system_name_buffer: *mut u16,
            file_system_name_size: u32,
        ) -> i32;
    }
    let normalized = root.replace('/', "\\");
    let trimmed = normalized.trim_end_matches('\\');
    let bytes = trimmed.as_bytes();
    if bytes.len() != 2 || bytes[1] != b':' || !bytes[0].is_ascii_alphabetic() {
        return None;
    }
    let letter = (bytes[0] as char).to_ascii_uppercase();
    let root = format!("{letter}:\\");
    let wide = root
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    if unsafe { GetDriveTypeW(wide.as_ptr()) } != DRIVE_FIXED {
        return None;
    }
    let mut file_system = [0_u16; 32];
    let mut serial = 0_u32;
    let success = unsafe {
        GetVolumeInformationW(
            wide.as_ptr(),
            std::ptr::null_mut(),
            0,
            &mut serial,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            file_system.as_mut_ptr(),
            file_system.len() as u32,
        )
    };
    if success == 0 {
        return None;
    }
    let length = file_system
        .iter()
        .position(|word| *word == 0)
        .unwrap_or(file_system.len());
    String::from_utf16_lossy(&file_system[..length])
        .eq_ignore_ascii_case("NTFS")
        .then_some((letter, serial))
}

#[cfg(windows)]
fn local_fixed_roots() -> Vec<String> {
    const DRIVE_FIXED: u32 = 3;
    #[link(name = "kernel32")]
    extern "system" {
        fn GetLogicalDrives() -> u32;
        fn GetDriveTypeW(root_path_name: *const u16) -> u32;
    }
    let mask = unsafe { GetLogicalDrives() };
    (0..26)
        .filter_map(|index| {
            if mask & (1 << index) == 0 {
                return None;
            }
            let letter = (b'A' + index as u8) as char;
            let root = format!("{letter}:\\");
            let wide = root
                .encode_utf16()
                .chain(std::iter::once(0))
                .collect::<Vec<_>>();
            (unsafe { GetDriveTypeW(wide.as_ptr()) } == DRIVE_FIXED).then_some(root)
        })
        .collect()
}

#[cfg(target_os = "macos")]
fn local_fixed_roots() -> Vec<String> {
    vec!["/".into()]
}

#[cfg(all(unix, not(target_os = "macos")))]
fn local_fixed_roots() -> Vec<String> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_dir())
        .map(|path| vec![path.to_string_lossy().into_owned()])
        .unwrap_or_else(|| vec!["/".into()])
}

#[cfg(windows)]
fn is_reparse_point(entry: &fs::DirEntry) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    entry
        .metadata()
        .map(|metadata| metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0)
        .unwrap_or(true)
}

#[cfg(not(windows))]
fn is_reparse_point(_entry: &fs::DirEntry) -> bool {
    false
}

#[cfg(all(unix, not(target_os = "macos")))]
fn unix_device(path: &Path) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    fs::metadata(path).ok().map(|metadata| metadata.dev())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_directory(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "logcrate-search-{name}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn search_feature_defaults_to_disabled() {
        let config = SearchConfig::default();
        assert!(!config.enabled);
        assert_eq!(config.version, search_config_version());
    }

    #[test]
    fn invalid_search_config_safely_falls_back_to_disabled() {
        let directory = test_directory("invalid-config");
        fs::write(directory.join("file-search.json"), b"not-json").unwrap();
        let preferences = SearchPreferenceStore::new(directory.clone());
        assert_eq!(
            preferences.feature_state(false),
            SearchFeatureState {
                current_enabled: false,
                next_launch_enabled: false,
            }
        );
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn legacy_search_config_is_migrated_without_enabling_search() {
        let directory = test_directory("legacy-config");
        let path = directory.join("file-search.json");
        fs::write(
            &path,
            br#"{"enabled":false,"roots":["D:\\"],"exclusions":[]}"#,
        )
        .unwrap();
        let config = read_config(&path).unwrap();
        assert_eq!(config.version, search_config_version());
        assert!(!config.enabled);
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn enabled_preference_is_separate_from_current_process_state() {
        let directory = test_directory("feature-state");
        let preferences = SearchPreferenceStore::new(directory.clone());
        preferences.set_enabled(true).unwrap();
        assert_eq!(
            preferences.feature_state(false),
            SearchFeatureState {
                current_enabled: false,
                next_launch_enabled: true,
            }
        );
        let persisted = read_config(&directory.join("file-search.json")).unwrap();
        assert!(persisted.enabled);
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn failed_preference_write_restores_previous_value() {
        let directory = test_directory("preference-write-failure");
        let preferences = SearchPreferenceStore::new(directory.clone());
        fs::create_dir(&preferences.config_path).unwrap();
        assert!(preferences.set_enabled(true).is_err());
        assert!(!preferences.feature_state(false).next_launch_enabled);
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn disabled_preference_read_does_not_create_search_storage() {
        let parent = test_directory("lightweight-preference");
        let search_dir = parent.join("search");
        let preferences = SearchPreferenceStore::new(search_dir.clone());
        assert!(!preferences.config().enabled);
        assert!(!search_dir.exists());
        let _ = fs::remove_dir_all(parent);
    }

    #[test]
    fn visible_metadata_is_enriched_without_reading_file_contents() {
        let directory = test_directory("visible-metadata");
        let path = directory.join("service.log");
        fs::write(&path, b"metadata-only").unwrap();
        let mut items = vec![SearchResultItem {
            path: path.to_string_lossy().into_owned(),
            name: "service.log".into(),
            parent: directory.to_string_lossy().into_owned(),
            kind: "log".into(),
            size: 0,
            modified_ms: None,
            readable: false,
            content_type: content_type_for_name("service.log").into(),
            is_log: true,
            is_archive: false,
        }];

        enrich_visible_metadata(&mut items);

        assert_eq!(items[0].size, 13);
        assert!(items[0].modified_ms.is_some());
        assert!(items[0].readable);
        assert_eq!(items[0].content_type, "text/plain");
        assert_eq!(content_type_for_name("bundle.zip"), "application/zip");
        assert_eq!(
            content_type_for_name("unknown.bin"),
            "application/octet-stream"
        );
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn query_snapshot_switch_is_atomic_and_recovers_interrupted_rebuild() {
        let directory = test_directory("atomic-query-snapshot");
        let manager = FileSearchManager::new(directory.clone());
        manager
            .query_index
            .lock()
            .unwrap()
            .as_mut()
            .unwrap()
            .add_batch(&[SearchIndexEntry {
                path: "D:\\logs\\stable-old.log".into(),
                name: "stable-old.log".into(),
                is_log: true,
                is_archive: false,
            }])
            .unwrap();
        manager.commit_query_index().unwrap();
        manager.query_index_ready.store(true, Ordering::Release);

        manager.begin_query_index_bulk().unwrap();
        manager
            .index_files(&[IndexedFile {
                path: "D:\\logs\\replacement-new.log".into(),
                name: "replacement-new.log".into(),
                root: "D:\\".into(),
                size: 1,
                modified_ms: None,
                is_log: true,
                is_archive: false,
                file_id: None,
                parent_id: None,
            }])
            .unwrap();
        let (_, old_total) = manager
            .query_tantivy(&["stable".into()], "log", 0, 10)
            .unwrap()
            .unwrap();
        assert_eq!(old_total, 1, "旧快照应在新快照完成前保持可查询");
        manager.finish_query_index_bulk().unwrap();
        let (_, new_total) = manager
            .query_tantivy(&["replacement".into()], "log", 0, 10)
            .unwrap()
            .unwrap();
        assert_eq!(new_total, 1);
        let (_, old_total) = manager
            .query_tantivy(&["stable".into()], "log", 0, 10)
            .unwrap()
            .unwrap();
        assert_eq!(old_total, 0);

        manager.begin_query_index_bulk().unwrap();
        drop(manager);
        let recovered = FileSearchManager::new(directory.clone());
        recovered.query_index_ready.store(true, Ordering::Release);
        let (_, new_total) = recovered
            .query_tantivy(&["replacement".into()], "log", 0, 10)
            .unwrap()
            .unwrap();
        assert_eq!(new_total, 1, "中断重建后应恢复上一份完整快照");
        assert!(!query_index_staging_path(&recovered.query_index_path).exists());
        drop(recovered);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn provider_switch_deduplicates_paths_and_removes_stale_results() {
        let directory = test_directory("provider-switch");
        let file_path = directory.join("service.log");
        fs::write(&file_path, b"first").unwrap();
        let manager = FileSearchManager::new(directory.clone());
        manager.query_index_ready.store(true, Ordering::Release);
        let root = directory.to_string_lossy().into_owned();
        let make_file = |size| IndexedFile {
            path: file_path.to_string_lossy().into_owned(),
            name: "service.log".into(),
            root: root.clone(),
            size,
            modified_ms: None,
            is_log: true,
            is_archive: false,
            file_id: None,
            parent_id: None,
        };

        let mut connection = open_database(&manager.db_path).unwrap();
        write_batch(&mut connection, &[make_file(5)]).unwrap();
        manager.index_files(&[make_file(5)]).unwrap();
        manager.commit_query_index().unwrap();
        write_batch(&mut connection, &[make_file(9)]).unwrap();
        manager.index_files(&[make_file(9)]).unwrap();
        manager.commit_query_index().unwrap();
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM files", [], |row| row.get::<_, u64>(0))
                .unwrap(),
            1
        );
        drop(connection);
        let (_, total) = manager
            .query_tantivy(&["service".into()], "log", 0, 10)
            .unwrap()
            .unwrap();
        assert_eq!(total, 1);

        fs::remove_file(&file_path).unwrap();
        let config = SearchConfig {
            version: search_config_version(),
            enabled: true,
            roots: vec![root],
            exclusions: Vec::new(),
        };
        apply_event_paths(&manager.db_path, &config, &[file_path]).unwrap();
        manager.drain_query_index_changes().unwrap();
        assert!(manager
            .query_tantivy(&["service".into()], "log", 0, 10)
            .unwrap()
            .is_none());
        drop(manager);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn sqlite_fallback_matches_name_path_filters_and_pagination() {
        let directory = test_directory("query");
        let db = directory.join("search.sqlite3");
        initialize_database(&db).unwrap();
        let mut connection = open_database(&db).unwrap();
        let files = vec![
            IndexedFile {
                path: "D:\\work\\logs\\server-error.log".into(),
                name: "server-error.log".into(),
                root: "D:\\".into(),
                size: 12,
                modified_ms: Some(2),
                is_log: true,
                is_archive: false,
                file_id: None,
                parent_id: None,
            },
            IndexedFile {
                path: "D:\\download\\server-backup.zip".into(),
                name: "server-backup.zip".into(),
                root: "D:\\".into(),
                size: 24,
                modified_ms: Some(1),
                is_log: false,
                is_archive: true,
                file_id: None,
                parent_id: None,
            },
        ];
        write_batch(&mut connection, &files).unwrap();
        let (items, total) = query_fts(&connection, &["server".into()], "", 0, 1).unwrap();
        assert_eq!(total, 2);
        assert_eq!(items.len(), 1);
        let (items, total) = query_fts(
            &connection,
            &["server".into()],
            " AND f.is_archive = 1",
            0,
            10,
        )
        .unwrap();
        assert_eq!(total, 1);
        assert_eq!(items[0].kind, "archive");
        drop(connection);
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn short_terms_use_case_insensitive_substring_matching() {
        let directory = test_directory("short");
        let db = directory.join("search.sqlite3");
        initialize_database(&db).unwrap();
        let mut connection = open_database(&db).unwrap();
        write_batch(
            &mut connection,
            &[IndexedFile {
                path: "/Users/test/logs/api.log".into(),
                name: "api.log".into(),
                root: "/".into(),
                size: 4,
                modified_ms: None,
                is_log: true,
                is_archive: false,
                file_id: None,
                parent_id: None,
            }],
        )
        .unwrap();
        let (items, total) = query_like(&connection, &["ap".into()], "", 0, 10).unwrap();
        assert_eq!(total, 1);
        assert_eq!(items[0].name, "api.log");
        drop(connection);
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn exclusions_are_normalized_and_match_descendants() {
        let root = test_directory("exclude");
        let excluded = root.join("private");
        fs::create_dir_all(excluded.join("nested")).unwrap();
        let normalized = normalize_unique_paths(vec![
            excluded.to_string_lossy().into_owned(),
            excluded.to_string_lossy().into_owned(),
        ]);
        assert_eq!(normalized.len(), 1);
        assert!(is_excluded(&excluded.join("nested/file.log"), &normalized));
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(windows)]
    #[test]
    fn non_system_ntfs_volumes_are_indexed_before_the_system_volume() {
        assert!(
            ntfs_scan_priority('D', 3_000_000, Some('C')) < ntfs_scan_priority('C', 1, Some('C'))
        );
        assert!(ntfs_scan_priority('E', 1, Some('C')) < ntfs_scan_priority('D', 2, Some('C')));
    }

    #[cfg(windows)]
    #[test]
    fn windows_scope_matching_ignores_case_and_mixed_separators() {
        assert!(path_is_within(
            Path::new("C:\\Users\\Alice\\Logs\\app.log"),
            Path::new("c:/users/alice/logs"),
        ));
        assert!(!path_is_within(
            Path::new("C:\\Users\\Alice\\Logs-old\\app.log"),
            Path::new("c:/users/alice/logs"),
        ));
    }

    #[cfg(windows)]
    #[test]
    fn usn_directory_rename_and_delete_update_only_affected_files() {
        use crate::ntfs::{FileId, FILE_ATTRIBUTE_DIRECTORY};

        let directory = test_directory("usn-update");
        let db = directory.join("search.sqlite3");
        initialize_database(&db).unwrap();
        let mut connection = open_database(&db).unwrap();
        let records = vec![
            MftRecord {
                id: FileId::from_u64(5),
                parent_id: FileId::from_u64(5),
                name: ".".into(),
                attributes: FILE_ATTRIBUTE_DIRECTORY,
                reason: 0,
                usn: 1,
            },
            MftRecord {
                id: FileId::from_u64(10),
                parent_id: FileId::from_u64(5),
                name: "Logs".into(),
                attributes: FILE_ATTRIBUTE_DIRECTORY,
                reason: 0,
                usn: 2,
            },
            MftRecord {
                id: FileId::from_u64(11),
                parent_id: FileId::from_u64(10),
                name: "app.log".into(),
                attributes: 0,
                reason: 0,
                usn: 3,
            },
        ];
        replace_ntfs_nodes(&mut connection, "C:\\", &records).unwrap();
        write_batch(
            &mut connection,
            &[IndexedFile {
                path: "C:\\Logs\\app.log".into(),
                name: "app.log".into(),
                root: "C:\\".into(),
                size: 0,
                modified_ms: None,
                is_log: true,
                is_archive: false,
                file_id: Some(FileId::from_u64(11).as_bytes()),
                parent_id: Some(FileId::from_u64(10).as_bytes()),
            }],
        )
        .unwrap();

        apply_usn_changes(
            &mut connection,
            "C:\\",
            &[],
            vec![MftRecord {
                id: FileId::from_u64(10),
                parent_id: FileId::from_u64(5),
                name: "Renamed".into(),
                attributes: FILE_ATTRIBUTE_DIRECTORY,
                reason: 0x2000,
                usn: 4,
            }],
        )
        .unwrap();
        let path = connection
            .query_row("SELECT path FROM files", [], |row| row.get::<_, String>(0))
            .unwrap();
        assert_eq!(path, "C:\\Renamed\\app.log");

        apply_usn_changes(
            &mut connection,
            "C:\\",
            &[],
            vec![MftRecord {
                id: FileId::from_u64(11),
                parent_id: FileId::from_u64(10),
                name: "app.log".into(),
                attributes: 0,
                reason: 0x200,
                usn: 5,
            }],
        )
        .unwrap();
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM files", [], |row| row.get::<_, u64>(0))
                .unwrap(),
            0
        );
        drop(connection);
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn runtime_config_excludes_the_search_database_directory() {
        let data_dir = test_directory("internal-exclusion");
        let manager = FileSearchManager::new(data_dir.clone());
        let config = manager.runtime_config();
        assert!(is_excluded(&manager.db_path, &config.exclusions));
        drop(manager);
        let _ = fs::remove_dir_all(data_dir);
    }

    #[test]
    fn full_event_queue_marks_the_index_scope_dirty() {
        let (sender, _receiver) = sync_channel(1);
        let dirty = AtomicBool::new(false);
        enqueue_event(&sender, &dirty, Event::new(notify::EventKind::Any));
        enqueue_event(&sender, &dirty, Event::new(notify::EventKind::Any));
        assert!(dirty.load(Ordering::Relaxed));
    }

    #[test]
    fn generation_and_pause_flags_cancel_stale_scans() {
        let data_dir = test_directory("cancel");
        let manager = FileSearchManager::new(data_dir.clone());
        let generation = manager.generation.load(Ordering::Relaxed);
        assert!(!manager.is_cancelled(generation));
        manager.generation.fetch_add(1, Ordering::SeqCst);
        assert!(manager.is_cancelled(generation));
        let current = manager.generation.load(Ordering::Relaxed);
        manager.cancel.store(true, Ordering::SeqCst);
        assert!(manager.is_cancelled(current));
        drop(manager);
        let _ = fs::remove_dir_all(data_dir);
    }

    #[test]
    fn directory_events_add_rename_and_delete_subtrees() {
        let root = test_directory("events");
        let db = root.join("search.sqlite3");
        initialize_database(&db).unwrap();
        let incoming = root.join("incoming");
        fs::create_dir_all(&incoming).unwrap();
        let original = incoming.join("server.log");
        fs::write(&original, b"one").unwrap();
        let config = SearchConfig {
            version: search_config_version(),
            enabled: true,
            roots: vec![root.to_string_lossy().into_owned()],
            exclusions: Vec::new(),
        };

        apply_event_paths(&db, &config, std::slice::from_ref(&incoming)).unwrap();
        let connection = open_database(&db).unwrap();
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM files", [], |row| row.get::<_, u64>(0))
                .unwrap(),
            1
        );
        drop(connection);

        let renamed = incoming.join("renamed.log");
        fs::rename(&original, &renamed).unwrap();
        apply_event_paths(&db, &config, &[original, renamed.clone()]).unwrap();
        let connection = open_database(&db).unwrap();
        let names = connection
            .prepare("SELECT name FROM files ORDER BY name")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(names, vec!["renamed.log"]);
        drop(connection);

        fs::remove_dir_all(&incoming).unwrap();
        apply_event_paths(&db, &config, &[incoming]).unwrap();
        let connection = open_database(&db).unwrap();
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM files", [], |row| row.get::<_, u64>(0))
                .unwrap(),
            0
        );
        drop(connection);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn schema_version_change_rebuilds_external_content_index() {
        let directory = test_directory("schema");
        let db = directory.join("search.sqlite3");
        initialize_database(&db).unwrap();
        let mut connection = open_database(&db).unwrap();
        write_batch(
            &mut connection,
            &[IndexedFile {
                path: "D:\\logs\\obsolete.log".into(),
                name: "obsolete.log".into(),
                root: "D:\\".into(),
                size: 1,
                modified_ms: None,
                is_log: true,
                is_archive: false,
                file_id: None,
                parent_id: None,
            }],
        )
        .unwrap();
        connection
            .execute(
                "UPDATE metadata SET value = 0 WHERE key = 'schema_version'",
                [],
            )
            .unwrap();
        drop(connection);

        initialize_database(&db).unwrap();
        let connection = open_database(&db).unwrap();
        let (items, total) = query_fts(&connection, &["obsolete".into()], "", 0, 10).unwrap();
        assert!(items.is_empty());
        assert_eq!(total, 0);
        drop(connection);
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn bulk_load_keeps_legacy_fts_disabled_after_tantivy_finalization() {
        let directory = test_directory("bulk");
        let db = directory.join("search.sqlite3");
        initialize_database(&db).unwrap();
        prepare_bulk_index(&db, true).unwrap();
        let mut connection = open_database(&db).unwrap();
        write_batch(
            &mut connection,
            &[IndexedFile {
                path: "D:\\logs\\during-build.log".into(),
                name: "during-build.log".into(),
                root: "D:\\".into(),
                size: 1,
                modified_ms: None,
                is_log: true,
                is_archive: false,
                file_id: None,
                parent_id: None,
            }],
        )
        .unwrap();
        let (_, like_total) = query_like(&connection, &["during".into()], "", 0, 10).unwrap();
        let (_, fts_total) = query_fts(&connection, &["during".into()], "", 0, 10).unwrap();
        assert_eq!(like_total, 1);
        assert_eq!(fts_total, 0);
        drop(connection);

        finish_bulk_index(&db).unwrap();
        let connection = open_database(&db).unwrap();
        let (_, fts_total) = query_fts(&connection, &["during".into()], "", 0, 10).unwrap();
        assert_eq!(fts_total, 0);
        drop(connection);
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn missing_tantivy_index_is_rebuilt_from_existing_database() {
        let directory = test_directory("tantivy-recovery");
        let db = directory.join("file-search.sqlite3");
        initialize_database(&db).unwrap();
        let mut connection = open_database(&db).unwrap();
        write_batch(
            &mut connection,
            &[IndexedFile {
                path: "D:\\logs\\recoverable-debug.log".into(),
                name: "recoverable-debug.log".into(),
                root: "D:\\".into(),
                size: 1,
                modified_ms: None,
                is_log: true,
                is_archive: false,
                file_id: None,
                parent_id: None,
            }],
        )
        .unwrap();
        drop(connection);

        let manager = FileSearchManager::new(directory.clone());
        assert!(!manager.query_index_ready.load(Ordering::Acquire));
        manager.ensure_query_index_matches_database().unwrap();
        assert!(manager.query_index_ready.load(Ordering::Acquire));
        let (items, total) = manager
            .query_tantivy(&["recoverable".into()], "log", 0, 20)
            .unwrap()
            .unwrap();
        assert_eq!(total, 1);
        assert_eq!(items[0].path, "D:\\logs\\recoverable-debug.log");
        drop(manager);
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn interrupted_bulk_load_is_discarded_on_next_initialization() {
        let directory = test_directory("bulk-recovery");
        let db = directory.join("search.sqlite3");
        initialize_database(&db).unwrap();
        prepare_bulk_index(&db, true).unwrap();
        let mut connection = open_database(&db).unwrap();
        write_batch(
            &mut connection,
            &[IndexedFile {
                path: "D:\\logs\\incomplete.log".into(),
                name: "incomplete.log".into(),
                root: "D:\\".into(),
                size: 1,
                modified_ms: None,
                is_log: true,
                is_archive: false,
                file_id: None,
                parent_id: None,
            }],
        )
        .unwrap();
        drop(connection);

        initialize_database(&db).unwrap();
        let connection = open_database(&db).unwrap();
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM files", [], |row| row.get::<_, u64>(0))
                .unwrap(),
            0
        );
        drop(connection);
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn completed_query_snapshot_survives_interrupted_persistence() {
        let directory = test_directory("query-snapshot-recovery");
        let db = directory.join("file-search.sqlite3");
        initialize_database(&db).unwrap();
        let mut query_index =
            SearchIndex::open(&directory.join("file-search-orange-gpl-v1")).unwrap();
        query_index.begin_bulk().unwrap();
        query_index
            .add_batch(&[
                SearchIndexEntry {
                    path: "C:\\logs\\debug.log".into(),
                    name: "debug.log".into(),
                    is_log: true,
                    is_archive: false,
                },
                SearchIndexEntry {
                    path: "D:\\logs\\debug.log".into(),
                    name: "debug.log".into(),
                    is_log: true,
                    is_archive: false,
                },
            ])
            .unwrap();
        query_index.finish_bulk().unwrap();
        drop(query_index);

        prepare_bulk_index(&db, true).unwrap();
        let mut connection = open_database(&db).unwrap();
        write_batch(
            &mut connection,
            &[IndexedFile {
                path: "D:\\logs\\debug.log".into(),
                name: "debug.log".into(),
                root: "D:\\".into(),
                size: 0,
                modified_ms: None,
                is_log: true,
                is_archive: false,
                file_id: None,
                parent_id: None,
            }],
        )
        .unwrap();
        drop(connection);
        mark_query_snapshot_complete(&db).unwrap();

        let state = initialize_database(&db).unwrap();
        assert!(state.query_snapshot_complete);
        assert!(state.persistence_incomplete);
        let manager = FileSearchManager::new(directory.clone());
        assert!(manager.query_index_ready.load(Ordering::Acquire));
        assert!(manager.persistence_recovery.load(Ordering::Acquire));
        let (items, _) = manager
            .query_tantivy(&["debug.log".into()], "", 0, 20)
            .unwrap()
            .unwrap();
        assert_eq!(items.len(), 2);
        drop(manager);
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn legacy_interrupted_persistence_preserves_nonempty_query_index() {
        let directory = test_directory("legacy-query-snapshot-recovery");
        let db = directory.join("file-search.sqlite3");
        initialize_database(&db).unwrap();
        prepare_bulk_index(&db, true).unwrap();
        let mut connection = open_database(&db).unwrap();
        write_batch(
            &mut connection,
            &[IndexedFile {
                path: "D:\\logs\\debug.log".into(),
                name: "debug.log".into(),
                root: "D:\\".into(),
                size: 0,
                modified_ms: None,
                is_log: true,
                is_archive: false,
                file_id: None,
                parent_id: None,
            }],
        )
        .unwrap();
        connection
            .execute(
                "DELETE FROM metadata WHERE key = 'query_snapshot_complete'",
                [],
            )
            .unwrap();
        drop(connection);

        let state = initialize_database_with_query(&db, Some(2)).unwrap();
        assert!(state.query_snapshot_complete);
        assert!(state.persistence_incomplete);
        let connection = open_database(&db).unwrap();
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM files", [], |row| row.get::<_, u64>(0))
                .unwrap(),
            1
        );
        drop(connection);
        let _ = fs::remove_dir_all(directory);
    }

    #[cfg(unix)]
    #[test]
    fn subtree_scan_does_not_follow_symbolic_link_directories() {
        use std::os::unix::fs::symlink;

        let root = test_directory("symlink-root");
        let outside = test_directory("symlink-outside");
        fs::write(root.join("inside.log"), b"inside").unwrap();
        fs::write(outside.join("outside.log"), b"outside").unwrap();
        symlink(&outside, root.join("linked")).unwrap();
        let db = root.join("search.sqlite3");
        initialize_database(&db).unwrap();
        let connection = open_database(&db).unwrap();
        upsert_subtree(&connection, &root, &root.to_string_lossy(), &[]).unwrap();
        let names = connection
            .prepare("SELECT name FROM files WHERE name LIKE '%.log' ORDER BY name")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(names, vec!["inside.log"]);
        drop(connection);
        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_dir_all(outside);
    }

    #[test]
    #[ignore = "million-path release performance baseline"]
    fn million_path_search_performance_baseline() {
        let directory = test_directory("million");
        let db = directory.join("search.sqlite3");
        let mut query_index = SearchIndex::open(&directory.join("tantivy")).unwrap();
        query_index.begin_bulk().unwrap();
        initialize_database(&db).unwrap();
        let started = std::time::Instant::now();
        prepare_bulk_index(&db, true).unwrap();
        let mut connection = open_database(&db).unwrap();
        connection
            .pragma_update(None, "synchronous", "OFF")
            .unwrap();
        connection
            .pragma_update(None, "cache_size", -65_536)
            .unwrap();
        for start in (0..1_000_000).step_by(SCAN_WRITE_BATCH) {
            let end = (start + SCAN_WRITE_BATCH).min(1_000_000);
            let files = (start..end)
                .map(|index| IndexedFile {
                    path: format!(
                        "D:\\logs\\service-{}\\server-error-{index}.log",
                        index % 500
                    ),
                    name: format!("server-error-{index}.log"),
                    root: "D:\\".into(),
                    size: index as u64,
                    modified_ms: Some(index as u64),
                    is_log: true,
                    is_archive: false,
                    file_id: None,
                    parent_id: None,
                })
                .collect::<Vec<_>>();
            write_batch(&mut connection, &files).unwrap();
            query_index
                .add_batch(&files.iter().map(search_index_entry).collect::<Vec<_>>())
                .unwrap();
        }
        drop(connection);
        query_index.finish_bulk().unwrap();
        finish_bulk_index(&db).unwrap();
        let build_elapsed = started.elapsed();
        let query_started = std::time::Instant::now();
        let (_, total) = query_index
            .search(&["server".into(), "error-999".into()], "", 0, 100)
            .unwrap();
        println!(
            "million path index: build={build_elapsed:?}, query={:?}, sqlite_bytes={}, documents={}, matches={total}",
            query_started.elapsed(),
            database_size(&db),
            query_index.num_docs(),
        );
        assert!(total > 0);
        drop(query_index);
        let _ = fs::remove_dir_all(directory);
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "requires installed LogCrate Index Service and a local NTFS C volume"]
    fn windows_ntfs_end_to_end_performance() {
        let directory = test_directory("ntfs-performance");
        let db = directory.join("search.sqlite3");
        let volume = std::env::var("LOGCRATE_NTFS_BENCH_VOLUME")
            .ok()
            .and_then(|value| value.chars().next())
            .map(|value| value.to_ascii_uppercase())
            .unwrap_or('C');
        let root = format!("{volume}:\\");
        let mut query_index = SearchIndex::open(&directory.join("tantivy")).unwrap();
        query_index.begin_bulk().unwrap();
        initialize_database(&db).unwrap();
        prepare_bulk_index(&db, true).unwrap();
        let started = std::time::Instant::now();
        let mut records = Vec::new();
        let enumeration = enumerate_mft_via_service(volume, |batch| {
            records.extend(batch);
            Ok(())
        })
        .unwrap();
        let enumerated_at = started.elapsed();
        let mut first_index = None;
        let (_, records) =
            resolve_mft_files_in_batches_retain(&root, records, NTFS_RESOLVE_BATCH, |entries| {
                let files = entries
                    .into_iter()
                    .map(|entry| indexed_mft_entry(&root, entry))
                    .collect::<Vec<_>>();
                query_index.add_batch(&files.iter().map(search_index_entry).collect::<Vec<_>>())?;
                first_index.get_or_insert_with(|| started.elapsed());
                Ok(())
            })
            .unwrap();
        query_index.finish_bulk().unwrap();
        let search_ready_at = started.elapsed();
        let query_started = std::time::Instant::now();
        let (_, total) = query_index.search(&["log".into()], "", 0, 100).unwrap();
        let query_elapsed = query_started.elapsed();
        if std::env::var_os("LOGCRATE_NTFS_BENCH_FAST_PHASE").is_some() {
            eprintln!(
                "NTFS_FAST_PHASE records={} enum_ms={} first_index_ms={} search_ready_ms={} documents={} query_ms={} matches={}",
                enumeration.records,
                enumerated_at.as_millis(),
                first_index.unwrap_or(search_ready_at).as_millis(),
                search_ready_at.as_millis(),
                query_index.num_docs(),
                query_elapsed.as_millis(),
                total,
            );
            drop(query_index);
            let _ = fs::remove_dir_all(directory);
            return;
        }
        let mut connection = open_database(&db).unwrap();
        connection
            .pragma_update(None, "synchronous", "OFF")
            .unwrap();
        connection
            .pragma_update(None, "cache_size", -65_536)
            .unwrap();
        let (_, records) =
            resolve_mft_files_in_batches_retain(&root, records, NTFS_RESOLVE_BATCH, |entries| {
                let files = entries
                    .into_iter()
                    .map(|entry| indexed_mft_entry(&root, entry))
                    .collect::<Vec<_>>();
                insert_batch(&mut connection, &files)
            })
            .unwrap();
        let persisted_at = started.elapsed();
        replace_ntfs_nodes(&mut connection, &root, &records).unwrap();
        let nodes_at = started.elapsed();
        if std::env::var_os("LOGCRATE_NTFS_BENCH_NODES_PHASE").is_some() {
            let node_count = connection
                .query_row("SELECT COUNT(*) FROM ntfs_nodes", [], |row| {
                    row.get::<_, u64>(0)
                })
                .unwrap();
            eprintln!(
                "NTFS_NODES_PHASE records={} enum_ms={} first_index_ms={} search_ready_ms={} persisted_ms={} nodes_ms={} nodes={} bytes={}",
                enumeration.records,
                enumerated_at.as_millis(),
                first_index.unwrap_or(search_ready_at).as_millis(),
                search_ready_at.as_millis(),
                persisted_at.as_millis(),
                nodes_at.as_millis(),
                node_count,
                database_size(&db),
            );
            drop(connection);
            let _ = fs::remove_dir_all(directory);
            return;
        }
        drop(connection);
        finish_bulk_index(&db).unwrap();
        let finished_at = started.elapsed();
        eprintln!(
            "NTFS_PERF volume={} records={} batches={} enum_ms={} first_index_ms={} search_ready_ms={} persisted_ms={} nodes_ms={} total_ms={} query_ms={} matches={} sqlite_bytes={} documents={}",
            volume,
            enumeration.records,
            enumeration.batches,
            enumerated_at.as_millis(),
            first_index.unwrap_or(finished_at).as_millis(),
            search_ready_at.as_millis(),
            persisted_at.as_millis(),
            nodes_at.as_millis(),
            finished_at.as_millis(),
            query_elapsed.as_millis(),
            total,
            database_size(&db),
            query_index.num_docs(),
        );
        assert!(enumeration.records > 0);
        assert!(query_elapsed < Duration::from_millis(100));
        drop(query_index);
        let _ = fs::remove_dir_all(directory);
    }
}
