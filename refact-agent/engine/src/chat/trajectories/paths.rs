use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex, OnceLock, Weak};
use std::sync::atomic::Ordering;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use axum::http::StatusCode;
use futures::FutureExt;
use serde::de::{Deserializer as _, IgnoredAny, MapAccess, Visitor};
use tokio::fs;
use tokio::sync::mpsc;
use tracing::{info, warn};
use crate::chat::types::TrajectorySourceIdentitySessionExt;

use crate::app_state::AppState;
use crate::call_validation::ChatMessage;
use crate::chat::trajectory_index;
use crate::chat::types::TrajectorySourceIdentity;
use crate::custom_error::ScratchError;
use crate::files_correction::get_project_dirs;
use crate::global_context::GlobalContext;

use super::load::{
    effective_trajectory_source_for_path, trajectory_candidate_has_minimum_schema,
    trajectory_root_id_matches, trajectory_source_matches_hint,
};
use super::{ensure_real_dir_tree, is_real_dir, safe_trajectory_file_in_dir};

pub async fn get_trajectories_dir(gcx: Arc<GlobalContext>) -> Result<PathBuf, String> {
    let project_dirs = get_project_dirs(gcx).await;
    let workspace_root = project_dirs.first().ok_or("No workspace folder found")?;
    Ok(workspace_root.join(".refact").join("trajectories"))
}

pub async fn get_global_trajectories_dir(gcx: Arc<GlobalContext>) -> PathBuf {
    let app = AppState::from_gcx(gcx).await;
    let config_dir = app.paths.config_dir.clone();
    config_dir.join("trajectories")
}

pub const INTERNAL_TRACE_LINK_PREFIX: &str = "internal:";
pub const UNATTRIBUTED_TRACES_DIR: &str = "internal";
pub(super) const PRUNE_THROTTLE_MAX_ENTRIES: usize = 256;

#[derive(Default)]
pub(super) struct PruneThrottle {
    pub(super) entries: std::collections::HashMap<PathBuf, PruneThrottleEntry>,
    pub(super) next_access: u64,
}

pub(super) struct PruneThrottleEntry {
    pub(super) last_pruned_unix_secs: u64,
    pub(super) last_access: u64,
}

impl PruneThrottle {
    pub(super) fn claim(&mut self, dir: &Path, now: u64, interval_secs: u64) -> bool {
        self.next_access = self.next_access.saturating_add(1);
        if let Some(entry) = self.entries.get_mut(dir) {
            if now.saturating_sub(entry.last_pruned_unix_secs) < interval_secs {
                entry.last_access = self.next_access;
                return false;
            }
            entry.last_pruned_unix_secs = now;
            entry.last_access = self.next_access;
            return true;
        }
        if self.entries.len() >= PRUNE_THROTTLE_MAX_ENTRIES {
            if let Some(oldest) = self
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.last_access)
                .map(|(path, _)| path.clone())
            {
                self.entries.remove(&oldest);
            }
        }
        self.entries.insert(
            dir.to_path_buf(),
            PruneThrottleEntry {
                last_pruned_unix_secs: now,
                last_access: self.next_access,
            },
        );
        true
    }

    fn release(&mut self, dir: &Path, claimed_at_unix_secs: u64) {
        if self
            .entries
            .get(dir)
            .is_some_and(|entry| entry.last_pruned_unix_secs == claimed_at_unix_secs)
        {
            self.entries.remove(dir);
        }
    }
}

pub(super) fn claim_prune_throttle(
    throttle: &StdMutex<PruneThrottle>,
    dir: &Path,
    now: u64,
    interval_secs: u64,
    label: &str,
) -> Result<bool, String> {
    let mut throttle = throttle
        .lock()
        .map_err(|_| format!("{label} prune throttle lock poisoned"))?;
    Ok(throttle.claim(dir, now, interval_secs))
}

pub(super) fn release_prune_throttle(
    throttle: &StdMutex<PruneThrottle>,
    dir: &Path,
    claimed_at_unix_secs: u64,
) {
    if let Ok(mut throttle) = throttle.lock() {
        throttle.release(dir, claimed_at_unix_secs);
    }
}

pub(super) fn internal_trace_prune_throttle() -> &'static StdMutex<PruneThrottle> {
    static LAST_PRUNE_BY_DIR: OnceLock<StdMutex<PruneThrottle>> = OnceLock::new();
    LAST_PRUNE_BY_DIR.get_or_init(|| StdMutex::new(PruneThrottle::default()))
}

pub(super) const STALE_ENTRY_CLEANUP_INTERVAL_SECS: u64 = 60;

pub(super) fn stale_entry_cleanup_throttle() -> &'static StdMutex<PruneThrottle> {
    static LAST_CLEANUP_BY_CHAT: OnceLock<StdMutex<PruneThrottle>> = OnceLock::new();
    LAST_CLEANUP_BY_CHAT.get_or_init(|| StdMutex::new(PruneThrottle::default()))
}

pub(super) fn stale_entry_cleanup_allowed(chat_id: &str) -> bool {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0);
    claim_prune_throttle(
        stale_entry_cleanup_throttle(),
        Path::new(chat_id),
        now,
        STALE_ENTRY_CLEANUP_INTERVAL_SECS,
        "stale trajectory index entry cleanup",
    )
    .unwrap_or(true)
}

pub(super) struct TopLevelInternalTraceVisitor;

impl<'de> Visitor<'de> for TopLevelInternalTraceVisitor {
    type Value = bool;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a trajectory JSON object")
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut link_type = None;
        let mut link_type_seen = false;
        while let Some(key) = map.next_key::<String>()? {
            if key == "link_type" {
                if link_type_seen {
                    return Err(serde::de::Error::custom("duplicate top-level link_type"));
                }
                link_type_seen = true;
                link_type = map.next_value::<Option<String>>()?;
                if !is_internal_trace_link_type(link_type.as_deref()) {
                    // Early-out: the answer is already pinned to `false` and nothing later in the
                    // document can change it. A well-formed remainder still yields `false`, and a
                    // malformed remainder yields a parse error that the caller also maps to
                    // `false`. So we can stop reading bytes here without changing the result.
                    //
                    // We deliberately do NOT early-out on an *internal* link_type: there the
                    // remainder still matters, because a truncated or otherwise malformed file
                    // must stay classified as "not an internal trace" so that pruning never
                    // deletes a torn write. See
                    // `internal_trace_pruning_ignores_message_content_and_invalid_json`.
                    return Ok(false);
                }
            } else {
                map.next_value::<IgnoredAny>()?;
            }
        }
        Ok(is_internal_trace_link_type(link_type.as_deref()))
    }
}

pub(super) fn top_level_link_type_is_internal_from_reader<R: std::io::Read>(reader: R) -> bool {
    // `serde_json`'s `IoRead` pulls the document through `Read::bytes()`, i.e. one `read()`
    // syscall per byte consumed. Buffering is what makes scanning a multi-megabyte trajectory
    // affordable; without it a single 144 MB folder costs ~144 million syscalls per scan.
    let mut deserializer = serde_json::Deserializer::from_reader(std::io::BufReader::new(reader));
    (&mut deserializer)
        .deserialize_map(TopLevelInternalTraceVisitor)
        .and_then(|is_internal| deserializer.end().map(|_| is_internal))
        .unwrap_or(false)
}

pub(super) fn parsed_top_level_link_type_is_internal(path: PathBuf) -> bool {
    match std::fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {}
        _ => return false,
    }
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(_) => return false,
    };
    top_level_link_type_is_internal_from_reader(file)
}

pub fn is_internal_trace_link_type(link_type: Option<&str>) -> bool {
    link_type.is_some_and(|value| value.starts_with(INTERNAL_TRACE_LINK_PREFIX))
}

pub(super) fn should_vectorize_trajectory(
    source: &TrajectorySourceIdentity,
    link_type: Option<&str>,
) -> bool {
    !matches!(source, TrajectorySourceIdentity::Buddy) && !is_internal_trace_link_type(link_type)
}

pub(super) const TRAJECTORY_VECTORIZATION_QUEUE_CAPACITY: usize = 64;

#[derive(Default)]
pub struct TrajectoryVectorizationQueue {
    sender: Option<mpsc::Sender<PathBuf>>,
    worker: Option<tokio::task::JoinHandle<()>>,
}

pub(super) async fn wait_for_trajectory_vectorization_shutdown(
    shutdown_flag: Arc<std::sync::atomic::AtomicBool>,
) {
    while !shutdown_flag.load(Ordering::Relaxed) {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

pub(crate) async fn enqueue_trajectory_vectorization(gcx: Arc<GlobalContext>, file_path: PathBuf) {
    if gcx.shutdown_flag.load(Ordering::Relaxed) {
        return;
    }

    let finished_worker = {
        let mut queue = gcx.trajectory_vectorization_queue.lock().await;
        if queue
            .worker
            .as_ref()
            .is_some_and(tokio::task::JoinHandle::is_finished)
        {
            queue.sender = None;
            queue.worker.take()
        } else {
            None
        }
    };
    if let Some(worker) = finished_worker {
        if let Err(error) = worker.await {
            warn!("trajectory VecDB enqueue worker failed: {error}");
        }
    }

    let sender = {
        let mut queue = gcx.trajectory_vectorization_queue.lock().await;
        if queue.sender.is_none() {
            let (sender, receiver) = mpsc::channel(TRAJECTORY_VECTORIZATION_QUEUE_CAPACITY);
            queue.sender = Some(sender);
            queue.worker = Some(tokio::spawn(run_trajectory_vectorization_queue(
                Arc::downgrade(&gcx),
                receiver,
            )));
        }
        queue.sender.as_ref().cloned()
    };

    let Some(sender) = sender else {
        return;
    };
    if sender.try_send(file_path).is_err() {
        warn!("skipping best-effort trajectory VecDB enqueue because its queue is unavailable");
    }
}

pub(super) async fn run_trajectory_vectorization_queue(
    gcx_weak: Weak<GlobalContext>,
    mut receiver: mpsc::Receiver<PathBuf>,
) {
    let Some(gcx) = gcx_weak.upgrade() else {
        return;
    };
    let shutdown_flag = gcx.shutdown_flag.clone();
    drop(gcx);

    loop {
        let file_path = tokio::select! {
            file_path = receiver.recv() => file_path,
            _ = wait_for_trajectory_vectorization_shutdown(shutdown_flag.clone()) => return,
        };
        let Some(file_path) = file_path else {
            return;
        };
        let Some(gcx) = gcx_weak.upgrade() else {
            return;
        };
        let roots = crate::indexing_routing::memory_plane_roots(gcx.clone()).await;
        if !roots.is_trajectory_file(&file_path) {
            continue;
        }
        let vecdb = gcx.vec_db.lock().await.clone();
        if let Some(vecdb) = vecdb {
            let documents = vec![file_path.to_string_lossy().to_string()];
            tokio::select! {
                result = std::panic::AssertUnwindSafe(vecdb.vectorizer_enqueue_files(
                    &documents,
                    false,
                    roots,
                )).catch_unwind() => {
                    if result.is_err() {
                        warn!("best-effort trajectory VecDB enqueue panicked");
                    }
                }
                _ = wait_for_trajectory_vectorization_shutdown(shutdown_flag.clone()) => return,
            }
        }
    }
}

pub fn internal_trace_link_type(feature: &str) -> String {
    format!("{INTERNAL_TRACE_LINK_PREFIX}{feature}")
}

pub(super) async fn prune_internal_traces_in_folder(dir: &Path, keep: usize) -> Result<usize, String> {
    let mut entries = match fs::read_dir(dir).await {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => {
            return Err(format!(
                "failed to read trace folder {}: {error}",
                dir.display()
            ))
        }
    };
    let mut traces = Vec::new();
    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
            continue;
        }
        let metadata = match fs::symlink_metadata(&path).await {
            Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => metadata,
            _ => continue,
        };
        let path_for_read = path.clone();
        let is_internal = tokio::task::spawn_blocking(move || {
            parsed_top_level_link_type_is_internal(path_for_read)
        })
        .await
        .unwrap_or(false);
        if !is_internal {
            continue;
        }
        let modified = metadata
            .modified()
            .ok()
            .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
            .map(|duration| duration.as_millis());
        let Some(modified) = modified else {
            continue;
        };
        traces.push((modified, path));
    }
    if traces.len() <= keep {
        return Ok(0);
    }
    traces.sort_by(|left, right| right.0.cmp(&left.0));
    let mut removed = 0;
    for (scanned_modified, path) in traces.into_iter().skip(keep) {
        let current_modified = match fs::symlink_metadata(&path).await {
            Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => metadata
                .modified()
                .ok()
                .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
                .map(|duration| duration.as_millis()),
            _ => None,
        };
        if current_modified != Some(scanned_modified) {
            continue;
        }
        let path_for_read = path.clone();
        if !tokio::task::spawn_blocking(move || {
            parsed_top_level_link_type_is_internal(path_for_read)
        })
        .await
        .unwrap_or(false)
        {
            continue;
        }
        if fs::remove_file(&path).await.is_ok() {
            removed += 1;
        }
    }
    Ok(removed)
}

pub(super) async fn prune_internal_traces_throttled(dir: &Path) -> Result<usize, String> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    if !claim_prune_throttle(
        internal_trace_prune_throttle(),
        dir,
        now,
        crate::runtime_settings::current().internal_trace_prune_interval_secs,
        "internal trace",
    )? {
        return Ok(0);
    }
    let result = prune_internal_traces_in_folder(
        dir,
        crate::runtime_settings::current().internal_traces_keep_per_folder,
    )
    .await;
    if result.is_err() {
        release_prune_throttle(internal_trace_prune_throttle(), dir, now);
    }
    result
}

pub(crate) async fn index_dir_for_trajectory_file(
    gcx: Arc<GlobalContext>,
    file_path: &Path,
) -> Option<PathBuf> {
    let parent = file_path.parent()?;
    let known_dirs = list_trajectory_dirs(&gcx).await;
    if known_dirs.iter().any(|dir| dir == parent) {
        return Some(parent.to_path_buf());
    }
    if let Some(grandparent) = parent.parent() {
        if known_dirs.iter().any(|dir| dir == grandparent) {
            return Some(grandparent.to_path_buf());
        }
    }
    Some(parent.to_path_buf())
}

pub(super) const TRAJECTORY_ID_MISMATCH_PREFIX: &str = "Existing trajectory file id mismatch";

pub(crate) fn trajectory_id_mismatch_error(path: &Path) -> String {
    format!("{TRAJECTORY_ID_MISMATCH_PREFIX} for {}", path.display())
}

pub(crate) fn is_trajectory_id_mismatch_error(error: &str) -> bool {
    error.starts_with(TRAJECTORY_ID_MISMATCH_PREFIX)
}

pub fn chat_folder_name<'a>(chat_id: &'a str, root_chat_id: Option<&'a str>) -> &'a str {
    match root_chat_id {
        Some(root) if !root.is_empty() && validate_trajectory_id(root).is_ok() => root,
        _ => chat_id,
    }
}

pub(crate) async fn list_chat_folder_dirs(gcx: &Arc<GlobalContext>) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    for base in get_all_trajectories_dirs(gcx.clone()).await {
        let mut entries = match fs::read_dir(&base).await {
            Ok(entries) => entries,
            Err(_) => continue,
        };
        while let Ok(Some(entry)) = entries.next_entry().await {
            let path = entry.path();
            if is_real_dir(&path).await {
                dirs.push(path);
            }
        }
    }
    dirs
}

pub async fn get_all_trajectories_dirs(gcx: Arc<GlobalContext>) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    for dir in get_project_dirs(gcx.clone())
        .await
        .into_iter()
        .map(|p| p.join(".refact").join("trajectories"))
    {
        if is_real_dir(&dir).await {
            dirs.push(dir);
        }
    }

    let global_dir = get_global_trajectories_dir(gcx).await;
    if is_real_dir(&global_dir).await {
        dirs.push(global_dir);
    }

    dirs
}

pub(super) fn task_root_candidates(
    gcx: Arc<GlobalContext>,
) -> impl std::future::Future<Output = Vec<PathBuf>> {
    async move {
        let mut dirs: Vec<PathBuf> = get_project_dirs(gcx.clone())
            .await
            .into_iter()
            .map(|p| p.join(".refact").join("tasks"))
            .collect();

        dirs.push(crate::tasks::storage::get_global_tasks_dir(gcx).await);
        dirs
    }
}

pub(crate) async fn get_all_task_roots(gcx: Arc<GlobalContext>) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    for dir in task_root_candidates(gcx).await {
        if is_real_dir(&dir).await {
            dirs.push(dir);
        }
    }
    dirs
}

pub(super) async fn get_or_create_all_task_roots(gcx: Arc<GlobalContext>) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    for dir in task_root_candidates(gcx).await {
        if ensure_real_dir_tree(&dir).await.is_ok() {
            dirs.push(dir);
        }
    }
    dirs
}

pub(super) async fn get_all_task_roots_from_weak(gcx_weak: &Weak<GlobalContext>) -> Vec<PathBuf> {
    match gcx_weak.upgrade() {
        Some(gcx) => get_or_create_all_task_roots(gcx).await,
        None => vec![],
    }
}

pub(crate) async fn list_task_trajectory_dirs(gcx: &Arc<GlobalContext>) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    for tasks_dir in get_all_task_roots(gcx.clone()).await {
        let mut task_entries = match fs::read_dir(&tasks_dir).await {
            Ok(entries) => entries,
            Err(_) => continue,
        };
        while let Ok(Some(task_entry)) = task_entries.next_entry().await {
            let task_dir = task_entry.path();
            if !is_real_dir(&task_dir).await {
                continue;
            }
            for role in ["planner", "agents", "subchats"] {
                collect_existing_dirs(task_dir.join("trajectories").join(role), &mut dirs).await;
            }
        }
    }
    dirs
}

pub(super) async fn collect_existing_dirs(root: PathBuf, dirs: &mut Vec<PathBuf>) {
    let mut pending = vec![root];
    while let Some(dir) = pending.pop() {
        if !is_real_dir(&dir).await {
            continue;
        }
        dirs.push(dir.clone());
        let mut entries = match fs::read_dir(&dir).await {
            Ok(entries) => entries,
            Err(_) => continue,
        };
        while let Ok(Some(entry)) = entries.next_entry().await {
            let path = entry.path();
            if is_real_dir(&path).await {
                pending.push(path);
            }
        }
    }
}

pub(crate) async fn list_trajectory_dirs(gcx: &Arc<GlobalContext>) -> Vec<PathBuf> {
    let mut dirs = get_all_trajectories_dirs(gcx.clone()).await;
    dirs.extend(list_task_trajectory_dirs(gcx).await);
    dirs
}

pub(super) async fn get_all_trajectories_dirs_from_weak(
    gcx_weak: &Weak<GlobalContext>,
) -> Vec<PathBuf> {
    match gcx_weak.upgrade() {
        Some(gcx) => get_all_trajectories_dirs(gcx).await,
        None => vec![],
    }
}

pub async fn get_buddy_conversations_dir(gcx: Arc<GlobalContext>) -> Result<PathBuf, String> {
    let project_dirs = get_project_dirs(gcx).await;
    let workspace_root = project_dirs.first().ok_or("No workspace folder found")?;
    Ok(workspace_root
        .join(".refact")
        .join("buddy")
        .join("chats")
        .join("conversations"))
}

pub(super) async fn get_or_create_buddy_conversations_dir(
    gcx: Arc<GlobalContext>,
) -> Result<PathBuf, String> {
    let dir = get_buddy_conversations_dir(gcx).await?;
    ensure_real_dir_tree(&dir).await?;
    Ok(dir)
}

pub(super) fn buddy_conversation_prune_throttle() -> &'static StdMutex<PruneThrottle> {
    static LAST_PRUNE_BY_DIR: OnceLock<StdMutex<PruneThrottle>> = OnceLock::new();
    LAST_PRUNE_BY_DIR.get_or_init(|| StdMutex::new(PruneThrottle::default()))
}

pub(crate) async fn prune_buddy_conversations_throttled(gcx: Arc<GlobalContext>) {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0);
    let dir = match get_buddy_conversations_dir(gcx.clone()).await {
        Ok(dir) => dir,
        Err(_) => return,
    };
    let settings = crate::runtime_settings::current();
    if !claim_prune_throttle(
        buddy_conversation_prune_throttle(),
        &dir,
        now,
        settings.buddy_conversations_prune_interval_secs,
        "buddy conversation",
    )
    .unwrap_or(false)
    {
        return;
    }
    let app = AppState::from_gcx(gcx.clone()).await;
    match prune_buddy_conversations_in_dir_with_coordinator(
        &dir,
        settings.buddy_conversations_keep,
        settings.buddy_conversations_prune_min_age_secs,
        Some(&app.chat.trajectory_index_coordinator),
    )
    .await
    {
        Ok(0) => {}
        Ok(removed) => info!("pruned {} old buddy conversations from {:?}", removed, dir),
        Err(e) => {
            warn!("buddy conversations prune failed: {}", e);
            release_prune_throttle(buddy_conversation_prune_throttle(), &dir, now);
        }
    }
}

#[cfg(test)]
pub(crate) async fn prune_buddy_conversations_in_dir(
    dir: &Path,
    keep: usize,
) -> Result<usize, String> {
    prune_buddy_conversations_in_dir_with_coordinator(
        dir,
        keep,
        crate::runtime_settings::current().buddy_conversations_prune_min_age_secs,
        None,
    )
    .await
}

pub(super) async fn prune_buddy_conversations_in_dir_with_coordinator(
    dir: &Path,
    keep: usize,
    min_age_secs: u64,
    coordinator: Option<&trajectory_index::TrajectoryIndexCoordinator>,
) -> Result<usize, String> {
    let mut entries = match fs::read_dir(dir).await {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => {
            return Err(format!(
                "Failed to read buddy conversations dir {:?}: {e}",
                dir
            ))
        }
    };
    let mut files: Vec<(i64, PathBuf, String)> = Vec::new();
    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        if !path.extension().map(|e| e == "json").unwrap_or(false) {
            continue;
        }
        if path
            .file_name()
            .is_some_and(|name| name == trajectory_index::TRAJECTORY_INDEX_FILE)
        {
            continue;
        }
        let Some(stem) = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .map(str::to_string)
        else {
            continue;
        };
        let metadata = match fs::symlink_metadata(&path).await {
            Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => metadata,
            Err(_) => continue,
            _ => continue,
        };
        let modified_ms = metadata
            .modified()
            .ok()
            .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
            .map(|elapsed| elapsed.as_millis() as i64)
            .unwrap_or(0);
        files.push((modified_ms, path, stem));
    }
    if files.len() <= keep {
        return Ok(0);
    }
    files.sort_by(|a, b| b.0.cmp(&a.0));
    let min_age_cutoff_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as i64)
        .unwrap_or(0)
        .saturating_sub((min_age_secs as i64).saturating_mul(1000));
    let mut removed_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
    for (scanned_modified_ms, path, stem) in files.drain(keep..) {
        if scanned_modified_ms >= min_age_cutoff_ms {
            continue;
        }
        let current_modified_ms = match fs::symlink_metadata(&path).await {
            Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => metadata
                .modified()
                .ok()
                .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
                .map(|elapsed| elapsed.as_millis() as i64)
                .unwrap_or(i64::MAX),
            Err(_) => continue,
            _ => continue,
        };
        if current_modified_ms != scanned_modified_ms {
            continue;
        }
        match fs::remove_file(&path).await {
            Ok(()) => {
                removed_ids.insert(stem);
            }
            Err(e) => warn!("failed to remove old buddy conversation {:?}: {}", path, e),
        }
    }
    let removed = removed_ids.len();
    if removed > 0 {
        if let Some(coordinator) = coordinator {
            trajectory_index::remove_trajectory_index_entries_with_rollout(
                coordinator,
                dir,
                &removed_ids,
            )
            .await?;
        } else {
            trajectory_index::remove_trajectory_index_entries(dir, &removed_ids).await?;
        }
    }
    Ok(removed)
}

pub(super) fn parse_stored_messages(
    value: serde_json::Value,
) -> Result<Vec<ChatMessage>, serde_json::Error> {
    // Reconstruction payloads deliberately remain opaque extras for archive browsing.
    serde_json::from_value(value)
}

pub(super) fn fix_tool_call_indexes(messages: &mut [ChatMessage]) {
    for msg in messages.iter_mut() {
        if let Some(ref mut tool_calls) = msg.tool_calls {
            for (i, tc) in tool_calls.iter_mut().enumerate() {
                if tc.index.is_none() {
                    tc.index = Some(i);
                }
            }
        }
    }
}

pub(super) fn extend_unique_paths(
    target: &mut Vec<PathBuf>,
    seen: &mut std::collections::HashSet<PathBuf>,
    paths: Vec<PathBuf>,
) {
    for path in paths {
        if seen.insert(path.clone()) {
            target.push(path);
        }
    }
}

pub(super) async fn indexed_candidate_paths_in_dirs(
    gcx: &Arc<GlobalContext>,
    dirs: &[PathBuf],
    chat_id: &str,
    caller: trajectory_index::TrajectoryIndexListingCaller,
) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    let app = AppState::from_gcx(gcx.clone()).await;
    for dir in dirs {
        let entries = match trajectory_index::list_trajectory_entries_with_rollout_for(
            &app.chat.trajectory_index_coordinator,
            dir,
            None,
            caller,
        )
        .await
        {
            Ok(entries) => entries,
            Err(e) => {
                warn!("Failed to read trajectory index {:?}: {}", dir, e);
                continue;
            }
        };
        for entry in entries {
            if entry.id == chat_id
                && trajectory_index::trajectory_index_entry_is_fresh(dir, &entry).await
            {
                paths.push(trajectory_index::trajectory_file_path_for_entry(
                    dir, &entry,
                ));
            }
        }
    }
    paths
}

pub(super) async fn normal_trajectory_candidate_paths(
    gcx: Arc<GlobalContext>,
    chat_id: &str,
) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let normal_dirs = get_all_trajectories_dirs(gcx.clone()).await;
    for dir in &normal_dirs {
        if let Some(path) = safe_trajectory_file_in_dir(dir, chat_id).await {
            if seen.insert(path.clone()) {
                paths.push(path);
            }
        }
        let own_folder = dir.join(chat_id);
        if is_real_dir(&own_folder).await {
            if let Some(path) = safe_trajectory_file_in_dir(&own_folder, chat_id).await {
                if seen.insert(path.clone()) {
                    paths.push(path);
                }
            }
        }
    }
    let indexed = indexed_candidate_paths_in_dirs(
        &gcx,
        &normal_dirs,
        chat_id,
        trajectory_index::TrajectoryIndexListingCaller::ChatPathLookup,
    )
    .await;
    extend_unique_paths(&mut paths, &mut seen, indexed);
    paths
}

pub(super) async fn trajectory_candidate_paths(
    gcx: Arc<GlobalContext>,
    chat_id: &str,
) -> Vec<PathBuf> {
    let mut candidates = normal_trajectory_candidate_paths(gcx.clone(), chat_id).await;
    for dir in list_task_trajectory_dirs(&gcx).await {
        if let Some(path) = safe_trajectory_file_in_dir(&dir, chat_id).await {
            candidates.push(path);
        }
    }
    candidates
}

pub(super) async fn nested_chat_folder_candidate_paths(
    gcx: Arc<GlobalContext>,
    chat_id: &str,
) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    for dir in list_chat_folder_dirs(&gcx).await {
        if let Some(path) = safe_trajectory_file_in_dir(&dir, chat_id).await {
            paths.push(path);
        }
    }
    paths
}

pub(super) struct ValidTrajectoryCandidate {
    pub(super) path: PathBuf,
    pub(super) content: String,
    pub(super) json: serde_json::Value,
}

pub(super) async fn read_valid_trajectory_candidate(
    path: PathBuf,
    chat_id: &str,
) -> Option<ValidTrajectoryCandidate> {
    let content = match fs::read_to_string(&path).await {
        Ok(content) => content,
        Err(e) => {
            warn!(
                "Skipping trajectory candidate {} for chat {}: failed to read: {}",
                path.display(),
                chat_id,
                e
            );
            return None;
        }
    };
    let parsed = tokio::task::spawn_blocking(move || {
        serde_json::from_str(&content).map(|json| (content, json))
    })
    .await;
    let (content, json): (String, serde_json::Value) = match parsed {
        Ok(Ok(parsed)) => parsed,
        Ok(Err(e)) => {
            warn!(
                "Skipping trajectory candidate {} for chat {}: failed to parse: {}",
                path.display(),
                chat_id,
                e
            );
            return None;
        }
        Err(e) => {
            warn!(
                "Skipping trajectory candidate {} for chat {}: parse task failed: {}",
                path.display(),
                chat_id,
                e
            );
            return None;
        }
    };
    if !trajectory_candidate_has_minimum_schema(&json, chat_id, &path) {
        return None;
    }
    Some(ValidTrajectoryCandidate {
        path,
        content,
        json,
    })
}

pub(super) async fn first_valid_trajectory_candidate(
    paths: Vec<PathBuf>,
    chat_id: &str,
) -> Option<ValidTrajectoryCandidate> {
    for path in paths {
        if let Some(candidate) = read_valid_trajectory_candidate(path, chat_id).await {
            return Some(candidate);
        }
    }
    None
}

pub(super) async fn find_normal_trajectory_file(
    gcx: Arc<GlobalContext>,
    chat_id: &str,
) -> Option<ValidTrajectoryCandidate> {
    validate_trajectory_id(chat_id).ok()?;
    let task_roots = get_all_task_roots(gcx.clone()).await;
    for path in normal_trajectory_candidate_paths(gcx, chat_id).await {
        let Some(candidate) = read_valid_trajectory_candidate(path, chat_id).await else {
            continue;
        };
        let Ok(source) = TrajectorySourceIdentity::from_json(&candidate.json) else {
            continue;
        };
        let source = effective_trajectory_source_for_path(source, &candidate.path, &task_roots);
        if trajectory_source_matches_hint(&source, &TrajectorySourceIdentity::Normal) {
            return Some(candidate);
        }
    }
    None
}

pub(crate) async fn find_trajectory_file(
    gcx: Arc<GlobalContext>,
    chat_id: &str,
) -> Option<ValidTrajectoryCandidate> {
    validate_trajectory_id(chat_id).ok()?;
    let normal = normal_trajectory_candidate_paths(gcx.clone(), chat_id).await;
    let mut seen: std::collections::HashSet<PathBuf> = normal.iter().cloned().collect();
    if let Some(found) = first_valid_trajectory_candidate(normal, chat_id).await {
        return Some(found);
    }
    let mut candidates = Vec::new();
    let indexed = indexed_candidate_paths_in_dirs(
        &gcx,
        &list_trajectory_dirs(&gcx).await,
        chat_id,
        trajectory_index::TrajectoryIndexListingCaller::ChatPathLookup,
    )
    .await;
    extend_unique_paths(&mut candidates, &mut seen, indexed);
    let task_scoped = trajectory_candidate_paths(gcx.clone(), chat_id).await;
    extend_unique_paths(&mut candidates, &mut seen, task_scoped);
    if let Some(found) = first_valid_trajectory_candidate(candidates, chat_id).await {
        return Some(found);
    }
    let nested = nested_chat_folder_candidate_paths(gcx, chat_id).await;
    first_valid_trajectory_candidate(nested, chat_id).await
}

pub(crate) async fn find_normal_trajectory_path(
    gcx: Arc<GlobalContext>,
    chat_id: &str,
) -> Option<PathBuf> {
    find_normal_trajectory_file(gcx, chat_id)
        .await
        .map(|candidate| candidate.path)
}

pub async fn find_trajectory_path(gcx: Arc<GlobalContext>, chat_id: &str) -> Option<PathBuf> {
    find_trajectory_file(gcx, chat_id)
        .await
        .map(|candidate| candidate.path)
}

pub(crate) async fn find_trajectory_parent_id(
    gcx: Arc<GlobalContext>,
    chat_id: &str,
) -> Option<Option<String>> {
    let candidate = find_trajectory_file(gcx, chat_id).await?;
    let trajectory = candidate.json.as_object()?;
    Some(
        trajectory
            .get("parent_id")
            .and_then(|value| value.as_str())
            .map(|value| value.to_string()),
    )
}

pub async fn find_trajectory_or_buddy_path(
    gcx: Arc<GlobalContext>,
    chat_id: &str,
) -> Option<PathBuf> {
    find_trajectory_or_buddy_file(gcx, chat_id)
        .await
        .map(|candidate| candidate.path)
}
pub(super) async fn find_validated_trajectory_path_in_dirs(
    chat_id: &str,
    dirs: Vec<PathBuf>,
) -> Option<PathBuf> {
    for dir in dirs {
        let Some(path) = safe_trajectory_file_in_dir(&dir, chat_id).await else {
            continue;
        };
        if read_valid_trajectory_candidate(path.clone(), chat_id)
            .await
            .is_some()
        {
            return Some(path);
        }
    }
    None
}

pub(super) async fn find_validated_trajectory_path_for_source(
    gcx: Arc<GlobalContext>,
    chat_id: &str,
    source: &TrajectorySourceIdentity,
) -> Option<PathBuf> {
    match source {
        TrajectorySourceIdentity::Normal => find_normal_trajectory_path(gcx, chat_id).await,
        TrajectorySourceIdentity::Buddy => {
            let dirs = get_buddy_conversations_dir(gcx)
                .await
                .ok()
                .into_iter()
                .collect();
            find_validated_trajectory_path_in_dirs(chat_id, dirs).await
        }
        TrajectorySourceIdentity::Task { .. } => {
            let dirs = list_task_trajectory_dirs(&gcx).await;
            find_validated_trajectory_path_in_dirs(chat_id, dirs).await
        }
    }
}

pub async fn find_trajectory_path_for_active_chat(
    gcx: Arc<GlobalContext>,
    sessions: &crate::chat::SessionsMap,
    chat_id: &str,
) -> Option<PathBuf> {
    validate_trajectory_id(chat_id).ok()?;
    let source = {
        let session_arc = sessions.read().await.get(chat_id).cloned();
        match session_arc {
            Some(arc) => {
                let session = arc.lock().await;
                TrajectorySourceIdentity::from_session(&session)
            }
            None => {
                return find_trajectory_or_buddy_path(gcx, chat_id).await;
            }
        }
    };
    find_validated_trajectory_path_for_source(gcx, chat_id, &source).await
}

pub(crate) async fn find_trajectory_or_buddy_file(
    gcx: Arc<GlobalContext>,
    chat_id: &str,
) -> Option<ValidTrajectoryCandidate> {
    validate_trajectory_id(chat_id).ok()?;
    let mut candidates = trajectory_candidate_paths(gcx.clone(), chat_id).await;
    if let Ok(buddy_dir) = get_buddy_conversations_dir(gcx.clone()).await {
        if let Some(buddy_path) = safe_trajectory_file_in_dir(&buddy_dir, chat_id).await {
            candidates.push(buddy_path);
        }
    }
    if let Some(found) = first_valid_trajectory_candidate(candidates, chat_id).await {
        return Some(found);
    }
    let nested = nested_chat_folder_candidate_paths(gcx, chat_id).await;
    first_valid_trajectory_candidate(nested, chat_id).await
}

pub(super) async fn ensure_existing_trajectory_file_matches(
    path: &Path,
    chat_id: &str,
) -> Result<(), String> {
    match fs::symlink_metadata(path).await {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err(format!(
                    "Existing trajectory file is not a real file for {}",
                    path.display()
                ));
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(format!("Failed to check trajectory existence: {}", e)),
    }
    let content = fs::read_to_string(path)
        .await
        .map_err(|e| format!("Failed to read existing trajectory: {}", e))?;
    let json: serde_json::Value = tokio::task::spawn_blocking(move || {
        serde_json::from_str(&content)
            .map_err(|e| format!("Failed to parse existing trajectory: {}", e))
    })
    .await
    .map_err(|e| format!("Existing trajectory parse task failed: {}", e))??;
    if trajectory_root_id_matches(&json, chat_id, path) {
        Ok(())
    } else {
        Err(trajectory_id_mismatch_error(path))
    }
}

pub(super) async fn read_existing_trajectory_object(
    path: &Path,
    chat_id: &str,
) -> Result<Option<serde_json::Map<String, serde_json::Value>>, String> {
    #[cfg(test)]
    record_trajectory_metadata_disk_read(chat_id);
    match fs::symlink_metadata(path).await {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err(format!(
                    "Existing trajectory file is not a real file for {}",
                    path.display()
                ));
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("Failed to check trajectory existence: {}", e)),
    }
    let content = fs::read_to_string(path)
        .await
        .map_err(|e| format!("Failed to read existing trajectory: {}", e))?;
    let json: serde_json::Value = tokio::task::spawn_blocking(move || {
        serde_json::from_str(&content)
            .map_err(|e| format!("Failed to parse existing trajectory: {}", e))
    })
    .await
    .map_err(|e| format!("Existing trajectory parse task failed: {}", e))??;
    if !trajectory_root_id_matches(&json, chat_id, path) {
        return Err(trajectory_id_mismatch_error(path));
    }
    json.as_object()
        .cloned()
        .ok_or_else(|| {
            format!(
                "Existing trajectory JSON root must be an object for {}",
                path.display()
            )
        })
        .map(Some)
}

/// Per-chat metadata disk-read counters, used only by tests.
///
/// This intentionally is *not* a single process-wide counter. The read it counts happens inside
/// the detached trajectory writer, i.e. on a `tokio::spawn`ed task on some other worker thread,
/// so a thread-local/task-local scope (the `TrajectoryIndexListingCounterScope::local()` pattern
/// in `trajectory_index.rs`) would never observe it. Keying by `chat_id` gives each test its own
/// counter instead, since every test already uses a unique chat id, so concurrently running tests
/// can no longer inflate one another's deltas.
#[cfg(test)]
pub(super) fn trajectory_metadata_disk_reads_by_chat(
) -> &'static StdMutex<std::collections::HashMap<String, usize>> {
    static READS: OnceLock<StdMutex<std::collections::HashMap<String, usize>>> = OnceLock::new();
    READS.get_or_init(|| StdMutex::new(std::collections::HashMap::new()))
}

#[cfg(test)]
pub(super) fn record_trajectory_metadata_disk_read(chat_id: &str) {
    *trajectory_metadata_disk_reads_by_chat()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .entry(chat_id.to_string())
        .or_insert(0) += 1;
}

#[cfg(test)]
pub(super) fn trajectory_metadata_disk_reads_for_chat(chat_id: &str) -> usize {
    trajectory_metadata_disk_reads_by_chat()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(chat_id)
        .copied()
        .unwrap_or(0)
}


pub fn validate_trajectory_id(id: &str) -> Result<(), ScratchError> {
    if id.is_empty() || id.len() > 128 {
        return Err(ScratchError::new(
            StatusCode::BAD_REQUEST,
            "Invalid trajectory id".to_string(),
        ));
    }
    if !id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(ScratchError::new(
            StatusCode::BAD_REQUEST,
            "Invalid trajectory id".to_string(),
        ));
    }
    Ok(())
}
