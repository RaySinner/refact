use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Instant;

use notify::RecommendedWatcher;
use tokio::sync::{Mutex as AMutex, Notify, RwLock as ARwLock};

pub use refact_core::ast_types::Document;

use crate::correction_cache::CacheCorrection;

#[derive(Debug, Clone)]
pub struct PendingBranchHeadChange {
    pub old_head: Option<String>,
    pub new_head: Option<String>,
    pub last_event: Instant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DebouncedFileEventKind {
    Upsert,
    Remove,
}

#[derive(Debug, Clone)]
pub struct DebouncedFileEvent {
    pub read_path: PathBuf,
    pub at: Instant,
    pub kind: DebouncedFileEventKind,
}

#[derive(Clone)]
pub struct DocumentsState {
    pub workspace_folders: Arc<StdMutex<Vec<PathBuf>>>,
    pub workspace_files: Arc<StdMutex<Vec<PathBuf>>>,
    pub workspace_vcs_roots: Arc<StdMutex<Vec<PathBuf>>>,

    pub active_file_path: Arc<AMutex<Option<PathBuf>>>,
    pub jsonl_files: Arc<StdMutex<Vec<PathBuf>>>,
    // document_map on windows: c%3A/Users/user\Documents/file.ext
    // query on windows: C:/Users/user/Documents/file.ext
    pub memory_document_map: Arc<AMutex<HashMap<PathBuf, Arc<ARwLock<Document>>>>>, // if a file is open in IDE, and it's outside workspace dirs, it will be in this map and not in workspace_files
    pub cache_dirty: Arc<AMutex<f64>>,
    pub cache_correction: Arc<StdMutex<Arc<CacheCorrection>>>,
    pub fs_watcher: Arc<StdMutex<Option<Arc<ARwLock<RecommendedWatcher>>>>>,
    pub git_branch_heads: Arc<StdMutex<HashMap<PathBuf, String>>>,
    pub branch_reindex_last_ts: Arc<AtomicU64>,
    pub file_event_debounce: Arc<StdMutex<HashMap<PathBuf, DebouncedFileEvent>>>,
    pub file_event_debounce_task: Arc<StdMutex<Option<tokio::task::JoinHandle<()>>>>,
    pub file_event_debounce_notify: Arc<Notify>,
    pub branch_head_debounce: Arc<StdMutex<HashMap<PathBuf, PendingBranchHeadChange>>>,
    pub branch_head_debounce_tasks: Arc<StdMutex<HashMap<PathBuf, tokio::task::JoinHandle<()>>>>,
    pub watch_registration_tasks: Arc<StdMutex<Vec<tokio::task::JoinHandle<()>>>>,
}

impl DocumentsState {
    pub async fn new(workspace_dirs: Vec<PathBuf>) -> Self {
        Self {
            workspace_folders: Arc::new(StdMutex::new(workspace_dirs)),
            workspace_files: Arc::new(StdMutex::new(Vec::new())),
            workspace_vcs_roots: Arc::new(StdMutex::new(Vec::new())),

            active_file_path: Arc::new(AMutex::new(None)),
            jsonl_files: Arc::new(StdMutex::new(Vec::new())),
            memory_document_map: Arc::new(AMutex::new(HashMap::new())),
            cache_dirty: Arc::new(AMutex::<f64>::new(0.0)),
            cache_correction: Arc::new(StdMutex::new(Arc::new(CacheCorrection::new()))),
            fs_watcher: Arc::new(StdMutex::new(None)),
            git_branch_heads: Arc::new(StdMutex::new(HashMap::new())),
            branch_reindex_last_ts: Arc::new(AtomicU64::new(0)),
            file_event_debounce: Arc::new(StdMutex::new(HashMap::new())),
            file_event_debounce_task: Arc::new(StdMutex::new(None)),
            file_event_debounce_notify: Arc::new(Notify::new()),
            branch_head_debounce: Arc::new(StdMutex::new(HashMap::new())),
            branch_head_debounce_tasks: Arc::new(StdMutex::new(HashMap::new())),
            watch_registration_tasks: Arc::new(StdMutex::new(Vec::new())),
        }
    }
}
