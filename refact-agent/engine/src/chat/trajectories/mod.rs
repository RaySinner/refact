use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex, OnceLock, Weak};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use axum::extract::Path as AxumPath;
use axum::http::{Response, StatusCode};
use axum::extract::State;
use hyper::Body;
use serde::de::{Deserializer as _, IgnoredAny, MapAccess, Visitor};
use serde::{Deserialize, Serialize};
use serde_json::json;
use futures::FutureExt;
use tokio::sync::{Mutex as AMutex, Notify, broadcast, mpsc};
use tokio::fs;
use notify::{Config, RecommendedWatcher, RecursiveMode, Watcher};
use tracing::{debug, info, warn};
use uuid::Uuid;
use refact_chat_api::FrozenRequestPrefix;

use crate::call_validation::{ChatMessage, ChatContent};
use crate::app_state::AppState;
use crate::custom_error::ScratchError;
use crate::global_context::GlobalContext;
use crate::files_correction::get_project_dirs;
use crate::subchat::run_subchat_once;
use crate::yaml_configs::customization_registry::get_subagent_config;
use crate::worktrees::service::WorktreeService;
use crate::worktrees::types::WorktreeMeta;

use refact_chat_api::{GoalLedgerEntry, GoalSnapshot, GoalStatus};
use crate::chat::perf_diagnostics::{self, PerfComponent, PerfOutcome};

mod load;
mod model;
mod paths;
mod reload;
mod store;
mod writer;

// Re-exports from writer
pub use writer::{
    atomic_write_file, TRAJECTORY_WRITER_ENV, TRAJECTORY_WATCHER_SELF_WRITE_ENV,
    trajectory_writer_rollout_enabled, trajectory_watcher_self_write_rollout_enabled,
};
pub(crate) use writer::{
    trajectory_writer_rollout_enabled_for, TrajectoryWriterRolloutGuard,
    trajectory_watcher_self_write_rollout_enabled_for,
};
pub(super) use writer::{
    atomic_write_json, unique_trajectory_tmp_path, consume_trajectory_self_write,
    TrajectorySelfWriteScope,
};
#[cfg(test)]
pub(crate) use writer::clear_all_trajectory_self_writes;

// Re-exports from model
pub use model::{
    apply_mode_defaults_to_thread, get_session_state_for_chat, LoadedTrajectory, TrajectoryData,
    TrajectoryMeta, trajectory_event_is_displayable_chat,
};
pub(crate) use model::{
    trajectory_list_data_is_displayable_chat, trajectory_snapshot_from_session,
    TrajectoryListCandidate, TrajectoryListData, TrajectoryRepairPatch,
};
pub(super) use model::{
    clamp_goal_snapshot_for_load, epoch_ms_now, get_session_runtime_for_trajectory_source,
};

// Re-exports from paths
pub use paths::{
    chat_folder_name, find_trajectory_or_buddy_path, find_trajectory_path,
    find_trajectory_path_for_active_chat, get_all_trajectories_dirs, get_buddy_conversations_dir,
    get_global_trajectories_dir, get_trajectories_dir, internal_trace_link_type,
    is_internal_trace_link_type, validate_trajectory_id, TrajectoryVectorizationQueue,
    INTERNAL_TRACE_LINK_PREFIX, UNATTRIBUTED_TRACES_DIR,
};
pub(crate) use paths::{
    find_trajectory_parent_id, index_dir_for_trajectory_file, is_trajectory_id_mismatch_error,
    list_chat_folder_dirs, list_task_trajectory_dirs, list_trajectory_dirs,
    trajectory_id_mismatch_error,
};
pub(super) use paths::{
    enqueue_trajectory_vectorization, ensure_existing_trajectory_file_matches,
    find_normal_trajectory_file, find_normal_trajectory_path, find_trajectory_file,
    find_trajectory_or_buddy_file, fix_tool_call_indexes, get_all_task_roots,
    get_all_task_roots_from_weak, get_all_trajectories_dirs_from_weak,
    get_or_create_buddy_conversations_dir, normal_trajectory_candidate_paths,
    parse_stored_messages, prune_buddy_conversations_throttled, prune_internal_traces_throttled,
    read_existing_trajectory_object, read_valid_trajectory_candidate, should_vectorize_trajectory,
    trajectory_candidate_paths, ValidTrajectoryCandidate,
};
#[cfg(test)]
pub(crate) use paths::{
    prune_buddy_conversations_in_dir, record_trajectory_metadata_disk_read,
    trajectory_metadata_disk_reads_by_chat, trajectory_metadata_disk_reads_for_chat,
};
#[cfg(test)]
pub(super) use paths::reset_test_trajectory_watcher_task;

// Re-exports from load
pub use load::{
    load_generic_trajectory_for_chat, load_trajectory_for_chat, save_initial_task_chat_trajectory,
    save_trajectory_as, save_trajectory_as_with_intent, save_trajectory_as_with_intent_checked,
    set_trajectory_label, validate_trajectory_label,
};
pub(crate) use load::{
    chat_id_is_planner_for_task, resolve_task_planner_controller_chat_id,
    trajectory_path_stem_matches_id, verified_planner_linked_root_chat_id,
};
pub(super) use load::{
    effective_trajectory_source_for_path, is_known_trajectory_top_level_key,
    load_generic_trajectory_for_chat_matching_source, load_trajectory_candidate,
    preserve_existing_trajectory_metadata, safe_new_buddy_trajectory_file,
    safe_new_nested_trajectory_file, safe_new_task_trajectory_file, sanitize_worktree_extra,
    title_generation_backing_file_matches, trajectory_candidate_has_minimum_schema,
    trajectory_root_id_matches, trajectory_source_matches_hint, trajectory_source_path_is_allowed,
    trajectory_worktree_from_extra, validate_loaded_worktree_strict,
};

// Re-exports from store
pub use store::{
    flush_all_trajectories, flush_trajectory_for_session, maybe_save_trajectory,
    maybe_save_trajectory_background, maybe_save_trajectory_background_with_intent,
    maybe_save_trajectory_with_intent, save_trajectory_snapshot, try_save_trajectory,
    try_save_trajectory_with_intent,
};
pub(crate) use store::persist_trajectory_snapshot_with_intent;
#[cfg(test)]
pub(crate) use store::{
    set_test_detached_trajectory_writer_failure_hook, set_test_message_serialization_failure,
    test_message_serialization_should_fail, wait_for_test_detached_trajectory_writer_failure,
    TEST_DETACHED_TRAJECTORY_WRITER_FAILURE_HOOK, TEST_MESSAGE_SERIALIZATION_FAILURE_ID,
};

// Re-exports from reload
pub use reload::{
    check_external_reload_pending, ensure_frozen_prefix, first_system_prompt,
    frozen_prefix_is_complete, new_frozen_request_prefix, refresh_session_from_trajectory_if_stale,
};
pub(crate) use reload::{
    new_legacy_incomplete_frozen_request_prefix, persist_loaded_trajectory_repair_raw,
};
pub(super) use reload::{
    apply_external_delete_with_revalidation, apply_loaded_external_update_with_repair,
    can_apply_external_reload, loaded_trajectory_source, parsed_rfc3339_utc,
    persist_loaded_trajectory_repair, raw_frozen_prefix_has_tools, repair_transition_frozen_prefix,
    session_is_stale_refresh_candidate, ExternalDeleteRevalidationOutcome,
};
#[cfg(test)]
pub(crate) use reload::{
    persist_frozen_prefix, trajectory_commit_contract, trajectory_commit_contract_for_test,
};

#[cfg(test)]
const TITLE_GENERATION_LLM_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(250);
#[cfg(not(test))]
const TITLE_GENERATION_LLM_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
const TRAJECTORY_META_TITLE_MAX_CHARS: usize = 120;

pub const TRAJECTORY_LABEL_MAX_CHARS: usize = TRAJECTORY_META_TITLE_MAX_CHARS;

pub use refact_chat_history::trajectory_event::TrajectoryEvent;

async fn refresh_trajectory_index_entry_for_path(
    gcx: Arc<GlobalContext>,
    path: &Path,
    source_hint: Option<TrajectorySourceIdentity>,
) -> Result<(), String> {
    let app = AppState::from_gcx(gcx.clone()).await;
    let dir = index_dir_for_trajectory_file(gcx, path)
        .await
        .ok_or_else(|| format!("Trajectory path has no parent: {}", path.display()))?;
    let dir = dir.as_path();
    let content = fs::read_to_string(path)
        .await
        .map_err(|e| format!("Failed to read trajectory for index refresh: {}", e))?;
    let parse_path = path.to_path_buf();
    let value = tokio::task::spawn_blocking(move || {
        serde_json::from_str::<serde_json::Value>(&content).map_err(|e| {
            format!(
                "Failed to parse trajectory '{}' for index refresh: {}",
                parse_path.display(),
                e
            )
        })
    })
    .await
    .map_err(|e| format!("Trajectory index refresh parse task failed: {}", e))??;
    trajectory_index::upsert_trajectory_index_entry_from_owned_value_with_rollout(
        &app.chat.trajectory_index_coordinator,
        dir,
        path,
        value,
        source_hint,
    )
    .await
}

async fn remove_stale_trajectory_index_entries(gcx: Arc<GlobalContext>, chat_id: &str) {
    let app = AppState::from_gcx(gcx.clone()).await;
    let dirs = match find_trajectory_file(gcx.clone(), chat_id).await {
        Some(candidate) => index_dir_for_trajectory_file(gcx.clone(), &candidate.path)
            .await
            .map(|dir| vec![dir])
            .unwrap_or_else(|| Vec::new()),
        None => Vec::new(),
    };
    let dirs = if dirs.is_empty() {
        list_trajectory_dirs(&gcx).await
    } else {
        dirs
    };
    for dir in dirs {
        let entries = match trajectory_index::list_trajectory_entries_with_rollout_for(
            &app.chat.trajectory_index_coordinator,
            &dir,
            None,
            trajectory_index::TrajectoryIndexListingCaller::StaleEntryCleanup,
        )
        .await
        {
            Ok(entries) => entries,
            Err(e) => {
                warn!("Failed to read trajectory index {:?}: {}", dir, e);
                continue;
            }
        };
        let mut should_remove = false;
        for entry in &entries {
            if entry.id == chat_id
                && !trajectory_index::trajectory_index_entry_is_fresh(&dir, entry).await
            {
                should_remove = true;
                break;
            }
        }
        if should_remove {
            if let Err(e) = trajectory_index::remove_trajectory_index_entry_with_rollout(
                &app.chat.trajectory_index_coordinator,
                &dir,
                chat_id,
            )
            .await
            {
                warn!(
                    "Failed to remove stale trajectory {} from index {:?}: {}",
                    chat_id, dir, e
                );
            }
        }
    }
}

#[cfg(test)]
async fn process_trajectory_change(gcx: Arc<GlobalContext>, chat_id: &str, is_remove: bool) {
    process_trajectory_change_for_source(gcx, chat_id, is_remove, None).await;
}

async fn process_trajectory_change_for_source(
    gcx: Arc<GlobalContext>,
    chat_id: &str,
    is_remove: bool,
    changed_source: Option<TrajectorySourceIdentity>,
) {
    let app = AppState::from_gcx(gcx.clone()).await;
    let sessions = app.chat.sessions.clone();

    if is_remove {
        remove_stale_trajectory_index_entries(gcx.clone(), chat_id).await;
        let session_arc = {
            let sessions_read = sessions.read().await;
            sessions_read.get(chat_id).cloned()
        };
        let outcome = if let Some(session_arc) = session_arc {
            apply_external_delete_with_revalidation(
                gcx.clone(),
                session_arc,
                chat_id,
                None,
                changed_source.clone(),
            )
            .await
        } else if let Some(loaded) = load_generic_trajectory_for_chat(gcx.clone(), chat_id).await {
            if loaded.transition_identity_repaired {
                persist_loaded_trajectory_repair(gcx.clone(), loaded.clone()).await;
            }
            ExternalDeleteRevalidationOutcome::Updated {
                loaded,
                applied_to_session: false,
            }
        } else {
            ExternalDeleteRevalidationOutcome::Deleted {
                applied_to_session: false,
            }
        };
        let event = match outcome {
            ExternalDeleteRevalidationOutcome::Updated {
                loaded,
                applied_to_session,
            } => {
                debug!(
                    "External delete revalidation for {} updated list; applied_to_session={}",
                    chat_id, applied_to_session
                );
                let task_roots = get_all_task_roots(gcx.clone()).await;
                let meta = loaded_trajectory_to_meta(&loaded, &task_roots);
                let (session_state, session_error, _) =
                    get_session_runtime_for_trajectory_source(&sessions, chat_id, &meta.source)
                        .await;
                let is_title_generated = loaded.thread.is_title_generated;
                updated_trajectory_event_from_meta(
                    meta,
                    Some(is_title_generated),
                    session_state,
                    session_error,
                )
            }
            ExternalDeleteRevalidationOutcome::Deleted { applied_to_session } => {
                debug!(
                    "External delete revalidation for {} deleted list item; applied_to_session={}",
                    chat_id, applied_to_session
                );
                deleted_trajectory_event(chat_id.to_string())
            }
            ExternalDeleteRevalidationOutcome::NoopStalePending => return,
        };
        let _ = app.chat.trajectory_events_tx.send(event);
        return;
    }

    let mut loaded = match changed_source.as_ref() {
        Some(source) => {
            load_generic_trajectory_for_chat_matching_source(gcx.clone(), chat_id, source).await
        }
        None => load_generic_trajectory_for_chat(gcx.clone(), chat_id).await,
    };

    if let Some(loaded_for_index) = loaded.as_ref() {
        let source_hint = changed_source.clone().or_else(|| {
            Some(TrajectorySourceIdentity::from_session_parts(
                &loaded_for_index.thread,
            ))
        });
        if let Err(e) = refresh_trajectory_index_entry_for_path(
            gcx.clone(),
            &loaded_for_index.source_path,
            source_hint,
        )
        .await
        {
            warn!("Failed to refresh trajectory index for {}: {}", chat_id, e);
        }
    } else if stale_entry_cleanup_allowed(chat_id) {
        remove_stale_trajectory_index_entries(gcx.clone(), chat_id).await;
    }

    let session_arc = {
        let sessions_read = sessions.read().await;
        sessions_read.get(chat_id).cloned()
    };

    if let Some(session_arc) = session_arc {
        if let Some(t) = loaded.as_ref() {
            let task_roots = get_all_task_roots(gcx.clone()).await;
            let meta = loaded_trajectory_to_meta(t, &task_roots);
            let (session_state, session_error, _) =
                get_session_runtime_for_trajectory_source(&sessions, chat_id, &meta.source).await;
            let is_title_generated = t.thread.is_title_generated;
            let event = updated_trajectory_event_from_meta(
                meta,
                Some(is_title_generated),
                session_state,
                session_error,
            );
            let _ = app.chat.trajectory_events_tx.send(event);
        }

        let source_collision = {
            let session = session_arc.lock().await;
            loaded.as_ref().is_some_and(|loaded| {
                !TrajectorySourceIdentity::from_session_parts(&loaded.thread)
                    .matches_session(&session)
            })
        };
        if source_collision {
            if let Some(loaded) = loaded.take() {
                if loaded.transition_identity_repaired {
                    persist_loaded_trajectory_repair(gcx.clone(), loaded).await;
                }
            }
            return;
        }

        if let Some(loaded) = loaded.take() {
            let pending = ExternalReloadPending::update(
                TrajectorySourceIdentity::from_session_parts(&loaded.thread),
            );
            apply_loaded_external_update_with_repair(
                gcx.clone(),
                session_arc.clone(),
                chat_id,
                loaded,
                None,
                Some(pending),
                &format!("Reloading trajectory for {} from external change", chat_id),
            )
            .await;
        } else {
            let mut session = session_arc.lock().await;
            let pending_source = changed_source
                .clone()
                .unwrap_or_else(|| TrajectorySourceIdentity::Normal);
            if !pending_source.matches_session(&session) {
                return;
            }
            if !can_apply_external_reload(&session) {
                session.external_reload_pending =
                    Some(ExternalReloadPending::update(pending_source));
            }
        }
        return;
    }

    let Some(loaded) = loaded.take() else {
        return;
    };

    let task_roots = get_all_task_roots(gcx.clone()).await;
    let meta = loaded_trajectory_to_meta(&loaded, &task_roots);
    let (session_state, session_error, _) =
        get_session_runtime_for_trajectory_source(&sessions, chat_id, &meta.source).await;
    let is_title_generated = loaded.thread.is_title_generated;
    let event = updated_trajectory_event_from_meta(
        meta,
        Some(is_title_generated),
        session_state,
        session_error,
    );
    let _ = app.chat.trajectory_events_tx.send(event);

    if loaded.transition_identity_repaired {
        persist_loaded_trajectory_repair(gcx, loaded).await;
    }
}

fn task_trajectory_context_from_path(
    path: &Path,
    task_roots: &[PathBuf],
) -> Option<(String, String, Option<String>)> {
    let path = crate::files_correction::canonicalize_normalized_path(path.to_path_buf());
    for root in task_roots {
        let root = crate::files_correction::canonicalize_normalized_path(root.clone());
        if !is_real_dir_sync(&root) {
            continue;
        }
        let Ok(relative) = path.strip_prefix(&root) else {
            continue;
        };
        let parts: Vec<String> = relative
            .components()
            .filter_map(|component| component.as_os_str().to_str().map(|s| s.to_string()))
            .collect();
        if parts.len() < 4 || parts.get(1).map(|s| s.as_str()) != Some("trajectories") {
            continue;
        }
        let role = parts[2].as_str();
        if role != "planner" && role != "agents" && role != "subchats" {
            continue;
        }
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let agent_id = if role == "agents" && parts.len() >= 5 {
            Some(parts[3].clone())
        } else {
            None
        };
        return Some((parts[0].clone(), role.to_string(), agent_id));
    }
    None
}

fn is_under_task_root(path: &Path, task_roots: &[PathBuf]) -> bool {
    let path = crate::files_correction::canonicalize_normalized_path(path.to_path_buf());
    task_roots.iter().any(|root| {
        let root = crate::files_correction::canonicalize_normalized_path(root.clone());
        is_real_dir_sync(&root) && path.starts_with(&root)
    })
}

fn should_dispatch_trajectory_path(path: &Path, task_roots: &[PathBuf]) -> bool {
    if path.extension().and_then(|e| e.to_str()) != Some("json") {
        return false;
    }
    if path.file_name().is_some_and(|name| name == "index.json") {
        return false;
    }
    if !is_under_task_root(path, task_roots) {
        return true;
    }
    task_trajectory_context_from_path(path, task_roots).is_some()
}

fn is_trajectory_temporary_file(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.rsplit_once(".tmp."))
        .is_some_and(|(prefix, random)| prefix.ends_with(".json") && !random.is_empty())
}

fn trajectory_source_identity_from_path(
    path: &Path,
    task_roots: &[PathBuf],
) -> TrajectorySourceIdentity {
    task_trajectory_context_from_path(path, task_roots)
        .map(|(task_id, role, agent_id)| {
            TrajectorySourceIdentity::task(task_id, role, agent_id, None, None)
        })
        .unwrap_or(TrajectorySourceIdentity::Normal)
}

async fn collect_task_trajectory_sources_under_path(
    path: &Path,
    task_roots: &[PathBuf],
) -> Vec<(String, TrajectorySourceIdentity, PathBuf)> {
    if !is_under_task_root(path, task_roots) {
        return Vec::new();
    }
    let mut sources = Vec::new();
    let mut pending = vec![path.to_path_buf()];
    while let Some(path) = pending.pop() {
        if should_dispatch_trajectory_path(&path, task_roots) {
            if let Some(chat_id) = path.file_stem().and_then(|s| s.to_str()) {
                sources.push((
                    chat_id.to_string(),
                    trajectory_source_identity_from_path(&path, task_roots),
                    path,
                ));
            }
            continue;
        }

        if !is_real_dir(&path).await {
            continue;
        }

        let mut entries = match fs::read_dir(&path).await {
            Ok(entries) => entries,
            Err(_) => continue,
        };
        while let Ok(Some(entry)) = entries.next_entry().await {
            pending.push(entry.path());
        }
    }
    sources
}

async fn collect_trajectory_sources_under_path(
    path: &Path,
    task_roots: &[PathBuf],
) -> Vec<(String, TrajectorySourceIdentity, PathBuf)> {
    if is_under_task_root(path, task_roots) {
        return collect_task_trajectory_sources_under_path(path, task_roots).await;
    }

    let mut sources = Vec::new();
    let mut pending = vec![path.to_path_buf()];
    while let Some(path) = pending.pop() {
        if should_dispatch_trajectory_path(&path, task_roots) {
            if let Some(chat_id) = path.file_stem().and_then(|stem| stem.to_str()) {
                sources.push((chat_id.to_string(), TrajectorySourceIdentity::Normal, path));
            }
            continue;
        }
        if !is_real_dir(&path).await {
            continue;
        }
        let mut entries = match fs::read_dir(&path).await {
            Ok(entries) => entries,
            Err(_) => continue,
        };
        while let Ok(Some(entry)) = entries.next_entry().await {
            pending.push(entry.path());
        }
    }
    sources
}

fn schedule_trajectory_watcher_overflow_recovery(
    pending_scans: &mut std::collections::HashMap<PathBuf, Instant>,
    dropped_events: &AtomicUsize,
    watched_roots: &[PathBuf],
) {
    let dropped = dropped_events.swap(0, Ordering::Relaxed);
    if dropped == 0 {
        return;
    }
    warn!(
        "Trajectory watcher dropped {} events; scheduling a recovery scan",
        dropped
    );
    let now = Instant::now();
    for root in watched_roots {
        pending_scans.insert(root.clone(), now);
    }
}

type TrajectoryPendingKey = (String, TrajectorySourceIdentity);

struct TrajectoryPendingChange {
    last_event_at: Instant,
    is_remove: bool,
    path: PathBuf,
}

type TrajectoryPendingMap =
    std::collections::HashMap<TrajectoryPendingKey, TrajectoryPendingChange>;

fn insert_pending_trajectory_change(
    pending: &mut TrajectoryPendingMap,
    chat_id: String,
    is_remove: bool,
    source: TrajectorySourceIdentity,
    path: PathBuf,
    now: Instant,
) {
    pending.insert(
        (chat_id, source),
        TrajectoryPendingChange {
            last_event_at: now,
            is_remove,
            path,
        },
    );
}

enum TrajectoryWatcherMessage {
    Trajectory {
        chat_id: String,
        is_remove: bool,
        source: TrajectorySourceIdentity,
        path: PathBuf,
    },
    ScanPath(PathBuf),
}

fn trajectory_watcher_message_for_path(
    path: PathBuf,
    is_remove: bool,
    task_roots: &[PathBuf],
) -> Option<TrajectoryWatcherMessage> {
    if is_trajectory_temporary_file(&path) {
        return None;
    }
    if should_dispatch_trajectory_path(&path, task_roots) {
        let chat_id = path.file_stem()?.to_str()?.to_string();
        return Some(TrajectoryWatcherMessage::Trajectory {
            chat_id,
            is_remove,
            source: trajectory_source_identity_from_path(&path, task_roots),
            path,
        });
    }
    (!is_remove && is_under_task_root(&path, task_roots))
        .then_some(TrajectoryWatcherMessage::ScanPath(path))
}

async fn process_trajectory_watcher_change(
    gcx: Arc<GlobalContext>,
    chat_id: String,
    is_remove: bool,
    source: TrajectorySourceIdentity,
    path: PathBuf,
) {
    if trajectory_watcher_self_write_rollout_enabled()
        && (consume_trajectory_self_write(&path, &source).await
            || (!matches!(source, TrajectorySourceIdentity::Normal)
                && consume_trajectory_self_write(&path, &TrajectorySourceIdentity::Normal).await))
    {
        return;
    }
    process_trajectory_change_for_source(gcx, &chat_id, is_remove, Some(source)).await;
}

static TRAJECTORY_WATCHER_TASK: OnceLock<StdMutex<Option<tokio::task::JoinHandle<()>>>> =
    OnceLock::new();

pub fn start_trajectory_watcher(gcx: Arc<GlobalContext>) {
    let watcher_task = TRAJECTORY_WATCHER_TASK.get_or_init(|| StdMutex::new(None));
    {
        let guard = watcher_task
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if guard.as_ref().is_some_and(|handle| !handle.is_finished()) {
            return;
        }
    }
    let gcx_weak = Arc::downgrade(&gcx);
    let (tx, mut rx) = tokio::sync::mpsc::channel::<TrajectoryWatcherMessage>(2048);
    let dropped_events = Arc::new(AtomicUsize::new(0));

    let handle = tokio::spawn(async move {
        let trajectories_dirs = get_all_trajectories_dirs_from_weak(&gcx_weak).await;
        let task_roots = get_all_task_roots_from_weak(&gcx_weak).await;
        if trajectories_dirs.is_empty() && task_roots.is_empty() {
            warn!("No trajectories directories found, trajectory watcher not started");
            return;
        }

        for dir in &trajectories_dirs {
            if !is_real_dir(dir).await {
                warn!("Skipping non-real trajectories dir {:?} for watcher", dir);
            }
        }
        for dir in &task_roots {
            if !is_real_dir(dir).await {
                warn!("Skipping non-real tasks dir {:?} for watcher", dir);
            }
        }
        let watched_roots = futures::future::join_all(
            trajectories_dirs
                .iter()
                .chain(task_roots.iter())
                .map(fs::canonicalize),
        )
        .await
        .into_iter()
        .filter_map(Result::ok)
        .map(|root| dunce::simplified(&root).to_path_buf())
        .collect::<Vec<_>>();
        let recovery_roots = watched_roots.clone();
        let _self_write_cleanup = TrajectorySelfWriteScope { watched_roots };

        let tx_clone = tx.clone();
        let dropped_events_for_callback = dropped_events.clone();
        let task_roots_for_callback = task_roots.clone();
        let event_callback = move |res: Result<notify::Event, notify::Error>| {
            if let Ok(event) = res {
                let dominated = matches!(
                    event.kind,
                    notify::EventKind::Create(_)
                        | notify::EventKind::Modify(_)
                        | notify::EventKind::Remove(_)
                );
                if !dominated {
                    return;
                }
                let is_remove = matches!(event.kind, notify::EventKind::Remove(_));
                for path in event.paths {
                    if let Some(message) = trajectory_watcher_message_for_path(
                        path,
                        is_remove,
                        &task_roots_for_callback,
                    ) {
                        if let Err(tokio::sync::mpsc::error::TrySendError::Full(_)) =
                            tx_clone.try_send(message)
                        {
                            let dropped =
                                dropped_events_for_callback.fetch_add(1, Ordering::Relaxed) + 1;
                            if dropped % 100 == 0 {
                                warn!("Trajectory watcher dropped {} events because the channel is full", dropped);
                            }
                        }
                    }
                }
            }
        };

        // Buddy conversations deliberately stay outside this watcher because active Buddy sessions
        // do not apply external trajectory reloads.
        let watcher = match RecommendedWatcher::new(event_callback, Config::default()) {
            Ok(w) => w,
            Err(e) => {
                warn!("Failed to create trajectory watcher: {}", e);
                return;
            }
        };

        let _watcher = Arc::new(std::sync::Mutex::new(watcher));
        {
            let mut w = _watcher.lock().unwrap();
            for dir in &trajectories_dirs {
                if let Err(e) = w.watch(dir, RecursiveMode::Recursive) {
                    warn!("Failed to watch trajectories dir {:?}: {}", dir, e);
                }
            }
            for dir in &task_roots {
                if let Err(e) = w.watch(dir, RecursiveMode::Recursive) {
                    warn!("Failed to watch tasks dir {:?}: {}", dir, e);
                }
            }
        }
        info!(
            "Trajectory watcher started for {} trajectory directories and {} task roots",
            trajectories_dirs.len(),
            task_roots.len()
        );

        let mut pending = TrajectoryPendingMap::new();
        let mut pending_scans: std::collections::HashMap<PathBuf, Instant> =
            std::collections::HashMap::new();
        let debounce = timeouts().watcher_debounce;

        loop {
            let timeout = if pending.is_empty() && pending_scans.is_empty() {
                timeouts().watcher_idle
            } else {
                timeouts().watcher_poll
            };

            tokio::select! {
                msg = rx.recv() => {
                    match msg {
                        Some(TrajectoryWatcherMessage::Trajectory {
                            chat_id,
                            is_remove,
                            source,
                            path,
                        }) => {
                            insert_pending_trajectory_change(
                                &mut pending,
                                chat_id,
                                is_remove,
                                source,
                                path,
                                Instant::now(),
                            );
                        }
                        Some(TrajectoryWatcherMessage::ScanPath(path)) => {
                            pending_scans.insert(path, Instant::now());
                        }
                        None => break,
                    }
                }
                _ = tokio::time::sleep(timeout) => {
                    if gcx_weak.upgrade().is_none() {
                        break;
                    }
                }
            }

            schedule_trajectory_watcher_overflow_recovery(
                &mut pending_scans,
                &dropped_events,
                &recovery_roots,
            );

            let now = Instant::now();
            let ready_scans: Vec<PathBuf> = pending_scans
                .iter()
                .filter(|(_, t)| now.duration_since(**t) >= debounce)
                .map(|(path, _)| path.clone())
                .collect();

            for path in ready_scans {
                pending_scans.remove(&path);
                for (chat_id, source, trajectory_path) in
                    collect_trajectory_sources_under_path(&path, &task_roots).await
                {
                    insert_pending_trajectory_change(
                        &mut pending,
                        chat_id,
                        false,
                        source,
                        trajectory_path,
                        Instant::now(),
                    );
                }
            }

            let now = Instant::now();
            let ready: Vec<_> = pending
                .iter()
                .filter(|(_, change)| now.duration_since(change.last_event_at) >= debounce)
                .map(|(key, change)| (key.clone(), change.is_remove, change.path.clone()))
                .collect();

            for ((chat_id, source), is_remove, path) in ready {
                pending.remove(&(chat_id.clone(), source.clone()));
                if let Some(gcx) = gcx_weak.upgrade() {
                    process_trajectory_watcher_change(gcx, chat_id, is_remove, source, path).await;
                }
            }
        }
    });
    let mut guard = watcher_task
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *guard = Some(handle);
}

fn is_placeholder_title(title: &str) -> bool {
    let normalized = title.trim().to_lowercase();
    normalized.is_empty() || normalized == "new chat" || normalized == "untitled"
}

fn is_placeholder_task_name(name: &str) -> bool {
    let normalized = name.trim().to_lowercase();
    normalized.is_empty() || normalized == "new task" || normalized == "untitled"
}

fn count_user_messages(messages: &[serde_json::Value]) -> usize {
    messages
        .iter()
        .filter(|msg| {
            msg.get("role")
                .and_then(|r| r.as_str())
                .map(|r| r == "user")
                .unwrap_or(false)
        })
        .count()
}

fn json_message_is_ui_only(msg: &serde_json::Value) -> bool {
    msg.get("_ui_only").and_then(|v| v.as_bool()) == Some(true)
        || msg
            .get("extra")
            .and_then(|extra| extra.get("_ui_only"))
            .and_then(|v| v.as_bool())
            == Some(true)
}

fn extract_first_user_message(messages: &[serde_json::Value]) -> Option<String> {
    for msg in messages {
        if json_message_is_ui_only(msg) {
            continue;
        }
        if msg.get("role").and_then(|r| r.as_str()) != Some("user") {
            continue;
        }
        if let Some(content) = msg
            .get("content")
            .and_then(extract_text_with_image_placeholders_from_json)
        {
            let trimmed = content.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.chars().take(200).collect());
            }
        }
    }
    None
}

pub fn extract_text_with_image_placeholders_from_json(
    content_value: &serde_json::Value,
) -> Option<String> {
    if let Some(content) = content_value.as_str() {
        return Some(content.to_string());
    }
    if let Some(content_arr) = content_value.as_array() {
        let parts: Vec<String> = content_arr
            .iter()
            .filter_map(|item| {
                if item.get("type").and_then(|t| t.as_str()) == Some("image_url") {
                    return Some("[image]".to_string());
                }
                if let Some(m_type) = item.get("m_type").and_then(|t| t.as_str()) {
                    if m_type.starts_with("image/") {
                        return Some("[image]".to_string());
                    }
                }
                item.get("text")
                    .and_then(|t| t.as_str())
                    .or_else(|| item.get("m_content").and_then(|t| t.as_str()))
                    .map(|s| s.to_string())
            })
            .collect();
        if !parts.is_empty() {
            return Some(parts.join("\n\n"));
        }
    }
    None
}

fn build_title_generation_context(messages: &[serde_json::Value]) -> String {
    let mut context = String::new();
    let max_messages = 6;
    let max_chars_per_message = 500;
    let mut included_count = 0;

    for msg in messages.iter() {
        if included_count >= max_messages {
            break;
        }
        let role = msg
            .get("role")
            .and_then(|r| r.as_str())
            .unwrap_or("unknown");
        if json_message_is_ui_only(msg) {
            continue;
        }
        if role == "error" {
            continue;
        }
        if role == refact_chat_history::trajectory_ops::COMPRESSION_REPORT_ROLE
            || matches!(
                role,
                "system" | "tool" | "context_file" | "cd_instruction" | "plan" | "goal" | "event"
            )
        {
            continue;
        }
        let content_text = match msg
            .get("content")
            .and_then(extract_text_with_image_placeholders_from_json)
        {
            Some(text) => text,
            None => continue,
        };
        let truncated: String = content_text.chars().take(max_chars_per_message).collect();
        if !truncated.trim().is_empty() {
            context.push_str(&format!("{}: {}\n\n", role, truncated));
            included_count += 1;
        }
    }
    context
}

fn truncate_text_chars(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    if max_chars <= 3 {
        return text.chars().take(max_chars).collect();
    }
    text.chars().take(max_chars - 3).collect::<String>() + "..."
}

fn clean_generated_title(raw_title: &str) -> String {
    let cleaned = raw_title
        .trim()
        .trim_matches('"')
        .trim_matches('\'')
        .trim_matches('`')
        .trim_matches('*')
        .replace('\n', " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    truncate_text_chars(&cleaned, 60)
}

pub(crate) fn trajectory_meta_title(title: &str) -> String {
    let cleaned = title.split_whitespace().collect::<Vec<_>>().join(" ");
    truncate_text_chars(&cleaned, TRAJECTORY_META_TITLE_MAX_CHARS)
}

pub(crate) fn task_context_from_task_meta(
    task_meta: Option<&crate::chat::types::TaskMeta>,
) -> (
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
) {
    task_meta
        .map(|meta| {
            (
                Some(meta.task_id.clone()),
                Some(meta.role.clone()),
                meta.agent_id.clone(),
                meta.card_id.clone(),
            )
        })
        .unwrap_or((None, None, None, None))
}

async fn generate_title_llm(
    gcx: Arc<GlobalContext>,
    messages: &[serde_json::Value],
    parent_chat_id: Option<&str>,
) -> Option<String> {
    let context = build_title_generation_context(messages);
    if context.trim().is_empty() {
        return None;
    }

    let subagent_config =
        match get_subagent_config(gcx.clone(), TITLE_GENERATION_SUBAGENT_ID, None).await {
            Some(config) => config,
            None => {
                warn!(
                    "subagent config '{}' not found",
                    TITLE_GENERATION_SUBAGENT_ID
                );
                return None;
            }
        };

    let title_prompt = match subagent_config.messages.user_template.as_ref() {
        Some(prompt) => prompt,
        None => {
            warn!(
                "messages.user_template not defined for subagent '{}'",
                TITLE_GENERATION_SUBAGENT_ID
            );
            return None;
        }
    };

    let prompt = format!("Chat conversation:\n{}\n\n{}", context, title_prompt);
    let chat_messages = vec![ChatMessage::new("user".to_string(), prompt)];

    match run_subchat_once(
        gcx,
        TITLE_GENERATION_SUBAGENT_ID,
        chat_messages,
        crate::subchat::TraceParent::from_parts(parent_chat_id, None),
    )
    .await
    {
        Ok(result) => {
            if let Some(last_msg) = result.messages.last() {
                let raw_title = last_msg.content.content_text_only();
                let cleaned = clean_generated_title(&raw_title);
                if !cleaned.is_empty() && cleaned.to_lowercase() != "new chat" {
                    info!("Generated title: {}", cleaned);
                    return Some(cleaned);
                }
            }
            None
        }
        Err(e) => {
            warn!("Title generation failed: {}", e);
            None
        }
    }
}

fn spawn_title_generation_task(
    gcx: Arc<GlobalContext>,
    id: String,
    messages: Vec<serde_json::Value>,
    file_path: PathBuf,
    source: TrajectorySourceIdentity,
) {
    tokio::spawn(async move {
        let app = AppState::from_gcx(gcx.clone()).await;
        let generated_title = match tokio::time::timeout(
            TITLE_GENERATION_LLM_TIMEOUT,
            generate_title_llm(gcx.clone(), &messages, Some(id.as_str())),
        )
        .await
        {
            Ok(title) => title,
            Err(_) => {
                warn!("Title generation timed out for {}", id);
                None
            }
        };
        let title = match generated_title {
            Some(t) => t,
            None => match extract_first_user_message(&messages) {
                Some(first_msg) => {
                    let truncated: String = first_msg.chars().take(60).collect();
                    if truncated.len() < first_msg.len() {
                        format!("{}...", truncated.trim_end())
                    } else {
                        truncated
                    }
                }
                None => return,
            },
        };
        let sessions = app.chat.sessions.clone();
        if !title_generation_backing_file_matches(&file_path, &id, &source).await {
            return;
        }
        let maybe_session_arc = {
            let sessions_read = sessions.read().await;
            sessions_read.get(&id).cloned()
        };
        if let Some(session_arc) = maybe_session_arc {
            let mut session = session_arc.lock().await;
            if TrajectorySourceIdentity::from_session(&session) != source {
                drop(session);
            } else {
                if session.thread.is_title_generated {
                    info!("Title already generated for {}, skipping", id);
                    return;
                }
                let suppressed_trajectory_events_tx =
                    if matches!(source, TrajectorySourceIdentity::Buddy) {
                        session.trajectory_events_tx.take()
                    } else {
                        None
                    };
                session.set_title(title.clone(), true);
                if let Some(tx) = suppressed_trajectory_events_tx {
                    session.trajectory_events_tx = Some(tx);
                }
                drop(session);
                maybe_save_trajectory_with_intent(
                    app.clone(),
                    session_arc,
                    TrajectoryCommitIntent::Checkpoint,
                )
                .await;
                info!("Updated session {} with generated title: {}", id, title);
                return;
            }
        }
        let content = match fs::read_to_string(&file_path).await {
            Ok(c) => c,
            Err(e) => {
                warn!("Failed to read trajectory for title update: {}", e);
                return;
            }
        };
        let mut data: TrajectoryData = match serde_json::from_str(&content) {
            Ok(d) => d,
            Err(e) => {
                warn!("Failed to parse trajectory for title update: {}", e);
                return;
            }
        };
        if data.id != id {
            warn!(
                "Skipping title update for {}: JSON id mismatch, found {}",
                file_path.display(),
                data.id
            );
            return;
        }
        let actual_source = match TrajectorySourceIdentity::from_extra(&data.extra) {
            Ok(actual_source) => actual_source,
            Err(e) => {
                warn!(
                    "Skipping title update for {}: trajectory source is invalid before write: {}",
                    file_path.display(),
                    e
                );
                return;
            }
        };
        if actual_source != source {
            warn!(
                "Skipping title update for {}: trajectory source changed before write",
                file_path.display()
            );
            return;
        }
        let already_generated = data
            .extra
            .get("isTitleGenerated")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if already_generated {
            info!("Title already generated for {}, skipping", id);
            return;
        }
        let updated_at = chrono::Utc::now().to_rfc3339();
        data.title = title.clone();
        data.updated_at = updated_at.clone();
        data.extra
            .insert("isTitleGenerated".to_string(), serde_json::json!(true));
        if let Err(e) = atomic_write_json(&file_path, &data, source.clone()).await {
            warn!("Failed to write trajectory with generated title: {}", e);
            return;
        }
        if let Some(dir) = index_dir_for_trajectory_file(gcx.clone(), &file_path).await {
            let value = match serde_json::to_value(&data) {
                Ok(value) => value,
                Err(e) => {
                    warn!(
                        "Failed to serialize trajectory title update for index: {}",
                        e
                    );
                    return;
                }
            };
            if let Err(e) =
                trajectory_index::upsert_trajectory_index_entry_from_owned_value_with_rollout(
                    &app.chat.trajectory_index_coordinator,
                    &dir,
                    &file_path,
                    value,
                    Some(source.clone()),
                )
                .await
            {
                warn!(
                    "Failed to update trajectory index for generated title: {}",
                    e
                );
            }
        }
        info!("Updated trajectory {} with generated title: {}", id, title);
        if !source.emits_generic_event() {
            return;
        }
        let content = match fs::read_to_string(&file_path).await {
            Ok(content) => content,
            Err(e) => {
                warn!("Failed to read updated trajectory for title SSE: {}", e);
                return;
            }
        };
        let updated_data: TrajectoryData = match serde_json::from_str(&content) {
            Ok(data) => data,
            Err(e) => {
                warn!("Failed to parse updated trajectory for title SSE: {}", e);
                return;
            }
        };
        let task_roots = get_all_task_roots(gcx.clone()).await;
        let mut meta = trajectory_data_to_meta_validated(app.clone(), &updated_data).await;
        apply_task_trajectory_context(&file_path, &task_roots, &mut meta);
        if meta.root_chat_id.is_none() {
            meta.root_chat_id = Some(id.clone());
        }
        let (session_state, session_error, session_worktree) =
            get_session_runtime_for_trajectory_source(&sessions, &id, &meta.source).await;
        let event = updated_trajectory_event_from_meta_with_worktree(
            meta,
            Some(true),
            session_state,
            session_error,
            session_worktree,
        );
        let tx = &app.chat.trajectory_events_tx;
        {
            let _ = tx.send(event);
        }
    });
}

fn spawn_task_name_generation_task(
    gcx: Arc<GlobalContext>,
    task_id: String,
    messages: Vec<serde_json::Value>,
) {
    tokio::spawn(async move {
        let task_meta = match crate::tasks::storage::load_task_meta(gcx.clone(), &task_id).await {
            Ok(meta) => meta,
            Err(e) => {
                warn!("Failed to load task meta for name generation: {}", e);
                return;
            }
        };

        if task_meta.is_name_generated {
            return;
        }

        if !is_placeholder_task_name(&task_meta.name) {
            return;
        }

        let generated_name = generate_title_llm(gcx.clone(), &messages, None).await;
        let name = match generated_name {
            Some(n) => n,
            None => match extract_first_user_message(&messages) {
                Some(first_msg) => {
                    let truncated: String = first_msg.chars().take(60).collect();
                    if truncated.len() < first_msg.len() {
                        format!("{}...", truncated.trim_end())
                    } else {
                        truncated
                    }
                }
                None => return,
            },
        };

        match crate::tasks::storage::update_task_name(gcx.clone(), &task_id, &name).await {
            Ok(_) => {
                info!("Updated task {} with generated name: {}", task_id, name);
            }
            Err(e) => {
                warn!("Failed to update task name: {}", e);
            }
        }
    });
}

pub(crate) fn calculate_line_changes_from_messages(messages: &[serde_json::Value]) -> (i64, i64) {
    let mut total_added: i64 = 0;
    let mut total_removed: i64 = 0;

    for msg in messages {
        let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("");
        if role != "diff" {
            continue;
        }

        let content = match msg.get("content") {
            Some(serde_json::Value::String(s)) => s.as_str(),
            _ => continue,
        };

        if let Ok(chunks) = serde_json::from_str::<Vec<serde_json::Value>>(content) {
            for chunk in chunks {
                if let Some(lines_add) = chunk.get("lines_add").and_then(|v| v.as_str()) {
                    if !lines_add.is_empty() {
                        total_added += lines_add.lines().count() as i64;
                    }
                }
                if let Some(lines_remove) = chunk.get("lines_remove").and_then(|v| v.as_str()) {
                    if !lines_remove.is_empty() {
                        total_removed += lines_remove.lines().count() as i64;
                    }
                }
            }
        }
    }

    (total_added, total_removed)
}

pub(crate) fn calculate_task_progress_from_messages(
    messages: &[serde_json::Value],
) -> (i32, i32, i32) {
    // Build a set of successful tool call IDs (tool messages without tool_failed=true)
    let mut successful_tool_ids: std::collections::HashSet<String> =
        std::collections::HashSet::new();
    for msg in messages {
        let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("");
        if role != "tool" {
            continue;
        }
        let tool_failed = msg
            .get("tool_failed")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if !tool_failed {
            if let Some(tool_call_id) = msg.get("tool_call_id").and_then(|v| v.as_str()) {
                successful_tool_ids.insert(tool_call_id.to_string());
            }
        }
    }

    // Find the last successful tasks_set tool call (iterate in reverse)
    for msg in messages.iter().rev() {
        let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("");
        if role != "assistant" {
            continue;
        }

        let tool_calls = match msg.get("tool_calls").and_then(|v| v.as_array()) {
            Some(tc) => tc,
            None => continue,
        };

        // Iterate tool_calls in reverse to find the last tasks_set
        for tc in tool_calls.iter().rev() {
            let function = match tc.get("function") {
                Some(f) => f,
                None => continue,
            };

            let name = function.get("name").and_then(|n| n.as_str()).unwrap_or("");
            if name != "tasks_set" {
                continue;
            }

            let tc_id = tc.get("id").and_then(|v| v.as_str()).unwrap_or("");
            if tc_id.is_empty() || !successful_tool_ids.contains(tc_id) {
                continue;
            }

            // Parse the arguments
            let args_str = function
                .get("arguments")
                .and_then(|a| a.as_str())
                .unwrap_or("");
            if let Ok(args) = serde_json::from_str::<serde_json::Value>(args_str) {
                if let Some(tasks) = args.get("tasks").and_then(|t| t.as_array()) {
                    let mut total = 0i32;
                    let mut done = 0i32;
                    let mut failed = 0i32;

                    for task in tasks {
                        total += 1;
                        let status = task.get("status").and_then(|s| s.as_str()).unwrap_or("");
                        match status.to_lowercase().as_str() {
                            "completed" | "done" | "complete" => done += 1,
                            "failed" | "error" => failed += 1,
                            _ => {}
                        }
                    }

                    return (total, done, failed);
                }
            }
        }
    }

    (0, 0, 0)
}

fn calculate_line_changes_from_chat_messages(messages: &[ChatMessage]) -> (i64, i64) {
    let mut total_added: i64 = 0;
    let mut total_removed: i64 = 0;

    for msg in messages {
        if msg.role != "diff" {
            continue;
        }

        let content = match &msg.content {
            ChatContent::SimpleText(s) => s.as_str(),
            _ => continue,
        };

        if let Ok(chunks) = serde_json::from_str::<Vec<serde_json::Value>>(content) {
            for chunk in chunks {
                if let Some(lines_add) = chunk.get("lines_add").and_then(|v| v.as_str()) {
                    if !lines_add.is_empty() {
                        total_added += lines_add.lines().count() as i64;
                    }
                }
                if let Some(lines_remove) = chunk.get("lines_remove").and_then(|v| v.as_str()) {
                    if !lines_remove.is_empty() {
                        total_removed += lines_remove.lines().count() as i64;
                    }
                }
            }
        }
    }

    (total_added, total_removed)
}

fn calculate_task_progress_from_chat_messages(messages: &[ChatMessage]) -> (i32, i32, i32) {
    // Build a set of successful tool call IDs (tool messages without tool_failed=true)
    let mut successful_tool_ids: std::collections::HashSet<String> =
        std::collections::HashSet::new();
    for msg in messages {
        if msg.role != "tool" {
            continue;
        }
        let tool_failed = msg.tool_failed.unwrap_or(false);
        if !tool_failed && !msg.tool_call_id.is_empty() {
            successful_tool_ids.insert(msg.tool_call_id.clone());
        }
    }

    // Find the last successful tasks_set tool call (iterate in reverse)
    for msg in messages.iter().rev() {
        if msg.role != "assistant" {
            continue;
        }

        let tool_calls = match &msg.tool_calls {
            Some(tc) => tc,
            None => continue,
        };

        // Iterate tool_calls in reverse to find the last tasks_set
        for tc in tool_calls.iter().rev() {
            if tc.function.name != "tasks_set" {
                continue;
            }

            if tc.id.is_empty() || !successful_tool_ids.contains(&tc.id) {
                continue;
            }

            // Parse the arguments
            if let Ok(args) = serde_json::from_str::<serde_json::Value>(&tc.function.arguments) {
                if let Some(tasks) = args.get("tasks").and_then(|t| t.as_array()) {
                    let mut total = 0i32;
                    let mut done = 0i32;
                    let mut failed = 0i32;

                    for task in tasks {
                        total += 1;
                        let status = task.get("status").and_then(|s| s.as_str()).unwrap_or("");
                        match status.to_lowercase().as_str() {
                            "completed" | "done" | "complete" => done += 1,
                            "failed" | "error" => failed += 1,
                            _ => {}
                        }
                    }

                    return (total, done, failed);
                }
            }
        }
    }

    (0, 0, 0)
}

#[derive(Default)]
pub(crate) struct TokenTotals {
    pub(crate) prompt_tokens: u64,
    pub(crate) completion_tokens: u64,
    pub(crate) total_tokens: u64,
    pub(crate) cache_read_tokens: u64,
    pub(crate) cache_creation_tokens: u64,
    pub(crate) cost_usd: Option<f64>,
}

pub(crate) fn calculate_token_totals_from_messages(messages: &[serde_json::Value]) -> TokenTotals {
    let mut prompt_tokens: u64 = 0;
    let mut completion_tokens: u64 = 0;
    let mut total_tokens: u64 = 0;
    let mut cache_read_tokens: u64 = 0;
    let mut cache_creation_tokens: u64 = 0;
    let mut cost_usd: Option<f64> = None;

    for msg in messages {
        let usage = match msg.get("usage") {
            Some(u) if !u.is_null() => u,
            _ => continue,
        };
        if let Some(v) = usage.get("prompt_tokens").and_then(|v| v.as_u64()) {
            prompt_tokens += v;
        }
        if let Some(v) = usage.get("completion_tokens").and_then(|v| v.as_u64()) {
            completion_tokens += v;
        }
        if let Some(v) = usage.get("total_tokens").and_then(|v| v.as_u64()) {
            total_tokens += v;
        }
        for key in &["cache_read_input_tokens", "cache_read_tokens"] {
            if let Some(v) = usage.get(key).and_then(|v| v.as_u64()) {
                cache_read_tokens += v;
                break;
            }
        }
        for key in &["cache_creation_input_tokens", "cache_creation_tokens"] {
            if let Some(v) = usage.get(key).and_then(|v| v.as_u64()) {
                cache_creation_tokens += v;
                break;
            }
        }
        if let Some(total) = usage
            .get("metering_usd")
            .and_then(|m| m.get("total_usd"))
            .and_then(|v| v.as_f64())
        {
            *cost_usd.get_or_insert(0.0) += total;
        }
    }

    TokenTotals {
        prompt_tokens,
        completion_tokens,
        total_tokens,
        cache_read_tokens,
        cache_creation_tokens,
        cost_usd,
    }
}

fn calculate_token_totals_from_chat_messages(messages: &[ChatMessage]) -> TokenTotals {
    let mut prompt_tokens: u64 = 0;
    let mut completion_tokens: u64 = 0;
    let mut total_tokens: u64 = 0;
    let mut cache_read_tokens: u64 = 0;
    let mut cache_creation_tokens: u64 = 0;
    let mut cost_usd: Option<f64> = None;

    for msg in messages {
        let usage = match &msg.usage {
            Some(u) => u,
            None => continue,
        };
        prompt_tokens += usage.prompt_tokens as u64;
        completion_tokens += usage.completion_tokens as u64;
        total_tokens += usage.total_tokens as u64;
        if let Some(v) = usage.cache_read_tokens {
            cache_read_tokens += v as u64;
        }
        if let Some(v) = usage.cache_creation_tokens {
            cache_creation_tokens += v as u64;
        }
        if let Some(ref m) = usage.metering_usd {
            *cost_usd.get_or_insert(0.0) += m.total_usd;
        }
    }

    TokenTotals {
        prompt_tokens,
        completion_tokens,
        total_tokens,
        cache_read_tokens,
        cache_creation_tokens,
        cost_usd,
    }
}

fn trajectory_data_to_meta(data: &TrajectoryData) -> TrajectoryMeta {
    let task_meta_json = data.extra.get("task_meta");
    let task_id = task_meta_json
        .and_then(|v| v.get("task_id"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let task_role = task_meta_json
        .and_then(|v| v.get("role"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let agent_id = task_meta_json
        .and_then(|v| v.get("agent_id"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let card_id = task_meta_json
        .and_then(|v| v.get("card_id"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let parent_id = data
        .extra
        .get("parent_id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let link_type = data
        .extra
        .get("link_type")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let root_chat_id = data
        .extra
        .get("root_chat_id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let worktree = None;

    let (total_lines_added, total_lines_removed) =
        calculate_line_changes_from_messages(&data.messages);
    let (tasks_total, tasks_done, tasks_failed) =
        calculate_task_progress_from_messages(&data.messages);
    let token_totals = calculate_token_totals_from_messages(&data.messages);

    TrajectoryMeta {
        id: data.id.clone(),
        title: trajectory_meta_title(&data.title),
        created_at: data.created_at.clone(),
        updated_at: data.updated_at.clone(),
        model: data.model.clone(),
        mode: data.mode.clone(),
        message_count: data.messages.len(),
        parent_id,
        link_type,
        task_id,
        task_role,
        agent_id,
        card_id,
        session_state: None,
        root_chat_id,
        worktree,
        total_lines_added,
        total_lines_removed,
        tasks_total,
        tasks_done,
        tasks_failed,
        total_prompt_tokens: token_totals.prompt_tokens,
        total_completion_tokens: token_totals.completion_tokens,
        total_tokens: token_totals.total_tokens,
        total_cache_read_tokens: token_totals.cache_read_tokens,
        total_cache_creation_tokens: token_totals.cache_creation_tokens,
        total_cost_usd: token_totals.cost_usd,
        source: TrajectorySourceIdentity::from_extra(&data.extra).unwrap_or_default(),
    }
}

#[derive(Debug, Deserialize)]
pub struct TrajectoriesListQuery {
    pub limit: Option<usize>,
    pub cursor: Option<String>,
    pub displayable_only: Option<bool>,
}

#[derive(Debug, Deserialize, Default)]
pub struct TrajectoriesAllQuery {
    #[serde(default)]
    pub displayable_only: bool,
}

#[derive(Debug, Serialize)]
pub struct PaginatedTrajectories {
    pub items: Vec<TrajectoryMeta>,
    pub next_cursor: Option<String>,
    pub has_more: bool,
    pub total_count: usize,
}

fn encode_cursor(updated_at: &str, id: &str) -> String {
    use base64::Engine;
    let cursor_data = format!("{}|{}", updated_at, id);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(cursor_data.as_bytes())
}

fn decode_cursor(cursor: &str) -> Option<(String, String)> {
    use base64::Engine;
    let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(cursor)
        .ok()?;
    let cursor_str = String::from_utf8(decoded).ok()?;
    let parts: Vec<&str> = cursor_str.splitn(2, '|').collect();
    if parts.len() == 2 {
        Some((parts[0].to_string(), parts[1].to_string()))
    } else {
        None
    }
}

async fn trajectory_data_to_meta_validated(app: AppState, data: &TrajectoryData) -> TrajectoryMeta {
    let mut meta = trajectory_data_to_meta(data);
    if let Some(worktree) = trajectory_worktree_from_extra(&data.extra) {
        meta.worktree = validate_loaded_worktree_strict(app, &data.id, worktree).await;
    }
    meta
}

fn loaded_trajectory_to_meta(loaded: &LoadedTrajectory, task_roots: &[PathBuf]) -> TrajectoryMeta {
    let effective_root = loaded
        .thread
        .root_chat_id
        .clone()
        .unwrap_or_else(|| loaded.thread.id.clone());
    let source = loaded_trajectory_source(loaded, task_roots);
    let (task_id, task_role, agent_id, card_id) =
        task_context_from_task_meta(loaded.thread.task_meta.as_ref());
    let (total_lines_added, total_lines_removed) =
        calculate_line_changes_from_chat_messages(&loaded.messages);
    let (tasks_total, tasks_done, tasks_failed) =
        calculate_task_progress_from_chat_messages(&loaded.messages);
    let token_totals = calculate_token_totals_from_chat_messages(&loaded.messages);

    let mut meta = TrajectoryMeta {
        id: loaded.thread.id.clone(),
        title: trajectory_meta_title(&loaded.thread.title),
        created_at: loaded.created_at.clone(),
        updated_at: loaded.updated_at.clone(),
        model: loaded.thread.model.clone(),
        mode: loaded.thread.mode.clone(),
        message_count: loaded.messages.len(),
        parent_id: loaded.thread.parent_id.clone(),
        link_type: loaded.thread.link_type.clone(),
        task_id,
        task_role,
        agent_id,
        card_id,
        session_state: None,
        root_chat_id: Some(effective_root),
        worktree: loaded.thread.worktree.clone(),
        total_lines_added,
        total_lines_removed,
        tasks_total,
        tasks_done,
        tasks_failed,
        total_prompt_tokens: token_totals.prompt_tokens,
        total_completion_tokens: token_totals.completion_tokens,
        total_tokens: token_totals.total_tokens,
        total_cache_read_tokens: token_totals.cache_read_tokens,
        total_cache_creation_tokens: token_totals.cache_creation_tokens,
        total_cost_usd: token_totals.cost_usd,
        source,
    };
    apply_task_trajectory_context(&loaded.source_path, task_roots, &mut meta);
    meta
}

fn deleted_trajectory_event(id: String) -> TrajectoryEvent {
    TrajectoryEvent {
        event_type: "deleted".to_string(),
        id,
        updated_at: None,
        title: None,
        is_title_generated: None,
        session_state: None,
        error: None,
        message_count: None,
        parent_id: None,
        link_type: None,
        root_chat_id: None,
        task_id: None,
        task_role: None,
        agent_id: None,
        card_id: None,
        model: None,
        mode: None,
        worktree: None,
        total_lines_added: None,
        total_lines_removed: None,
        tasks_total: None,
        tasks_done: None,
        tasks_failed: None,
        total_prompt_tokens: None,
        total_completion_tokens: None,
        total_tokens: None,
        total_cache_read_tokens: None,
        total_cache_creation_tokens: None,
        total_cost_usd: None,
    }
}

fn updated_trajectory_event_from_meta(
    meta: TrajectoryMeta,
    is_title_generated: Option<bool>,
    session_state: String,
    session_error: Option<String>,
) -> TrajectoryEvent {
    updated_trajectory_event_from_meta_with_worktree(
        meta,
        is_title_generated,
        session_state,
        session_error,
        None,
    )
}

fn updated_trajectory_event_from_meta_with_worktree(
    meta: TrajectoryMeta,
    is_title_generated: Option<bool>,
    session_state: String,
    session_error: Option<String>,
    session_worktree: Option<WorktreeMeta>,
) -> TrajectoryEvent {
    TrajectoryEvent {
        event_type: "updated".to_string(),
        id: meta.id,
        updated_at: Some(meta.updated_at),
        title: Some(meta.title),
        is_title_generated,
        session_state: Some(session_state),
        error: session_error,
        message_count: Some(meta.message_count),
        parent_id: meta.parent_id,
        link_type: meta.link_type,
        root_chat_id: meta.root_chat_id,
        task_id: meta.task_id,
        task_role: meta.task_role,
        agent_id: meta.agent_id,
        card_id: meta.card_id,
        model: Some(meta.model),
        mode: Some(meta.mode),
        worktree: meta.worktree.or(session_worktree),
        total_lines_added: Some(meta.total_lines_added),
        total_lines_removed: Some(meta.total_lines_removed),
        tasks_total: Some(meta.tasks_total),
        tasks_done: Some(meta.tasks_done),
        tasks_failed: Some(meta.tasks_failed),
        total_prompt_tokens: Some(meta.total_prompt_tokens),
        total_completion_tokens: Some(meta.total_completion_tokens),
        total_tokens: Some(meta.total_tokens),
        total_cache_read_tokens: Some(meta.total_cache_read_tokens),
        total_cache_creation_tokens: Some(meta.total_cache_creation_tokens),
        total_cost_usd: meta.total_cost_usd,
    }
}

fn apply_task_trajectory_context(path: &Path, task_roots: &[PathBuf], meta: &mut TrajectoryMeta) {
    if let Some((task_id, role, agent_id)) = task_trajectory_context_from_path(path, task_roots) {
        meta.source = effective_trajectory_source_for_path(meta.source.clone(), path, task_roots);
        if meta.task_id.is_none() {
            meta.task_id = Some(task_id);
        }
        if meta.task_role.is_none() {
            meta.task_role = Some(role);
        }
        if meta.agent_id.is_none() {
            meta.agent_id = agent_id;
        }
    }
}

fn trajectory_list_candidate_matches_hydrated_data(
    candidate: &TrajectoryListCandidate,
    data: &TrajectoryData,
) -> bool {
    if data.id == candidate.id && trajectory_path_stem_matches_id(&candidate.path, &data.id) {
        return true;
    }
    warn!(
        "Ignoring trajectory {} during hydration: expected id {}, found {}",
        candidate.path.display(),
        candidate.id,
        data.id
    );
    false
}

async fn hydrate_trajectory_list_candidate(
    app: AppState,
    candidate: &TrajectoryListCandidate,
    task_roots: &[PathBuf],
) -> Option<TrajectoryMeta> {
    if let (Some(indexed_len), Some(indexed_modified_ms)) = (
        candidate.indexed_file_len,
        candidate.indexed_file_modified_unix_ms,
    ) {
        let fresh = fs::symlink_metadata(&candidate.path)
            .await
            .ok()
            .and_then(|metadata| {
                if metadata.file_type().is_symlink() || !metadata.is_file() {
                    return None;
                }
                let modified = metadata.modified().ok()?;
                let ms = modified.duration_since(UNIX_EPOCH).ok()?.as_millis();
                let modified_ms = i64::try_from(ms).ok()?;
                Some(metadata.len() == indexed_len && modified_ms == indexed_modified_ms)
            })
            .unwrap_or(false);
        if !fresh {
            return None;
        }
    }
    if let Some(mut meta) = candidate.indexed_meta.clone() {
        if let Some(worktree) = meta.worktree.clone() {
            meta.worktree = validate_loaded_worktree_strict(app.clone(), &meta.id, worktree).await;
        }
        apply_task_trajectory_context(&candidate.path, task_roots, &mut meta);
        return Some(meta);
    }
    let content = fs::read_to_string(&candidate.path).await.ok()?;
    let data = serde_json::from_str::<TrajectoryData>(&content).ok()?;
    if !trajectory_list_candidate_matches_hydrated_data(candidate, &data) {
        return None;
    }
    let mut meta = trajectory_data_to_meta_validated(app, &data).await;
    apply_task_trajectory_context(&candidate.path, task_roots, &mut meta);
    Some(meta)
}

async fn hydrate_trajectory_list_page(
    app: AppState,
    candidates: Vec<TrajectoryListCandidate>,
    limit: usize,
    task_roots: &[PathBuf],
) -> (Vec<TrajectoryMeta>, bool) {
    let mut items = Vec::with_capacity(limit);
    let mut has_more = false;
    for candidate in candidates {
        let Some(meta) =
            hydrate_trajectory_list_candidate(app.clone(), &candidate, task_roots).await
        else {
            continue;
        };
        if items.len() == limit {
            has_more = true;
            break;
        }
        items.push(meta);
    }
    (items, has_more)
}

async fn collect_trajectory_list_candidates(
    gcx: &Arc<GlobalContext>,
    cursor_filter: Option<&(String, String)>,
    displayable_only: bool,
    caller: trajectory_index::TrajectoryIndexListingCaller,
) -> Vec<TrajectoryListCandidate> {
    let mut candidates = Vec::new();
    let mut seen_ids = std::collections::HashSet::new();
    let app = AppState::from_gcx(gcx.clone()).await;

    for trajectories_dir in list_trajectory_dirs(gcx).await {
        if !is_real_dir(&trajectories_dir).await {
            continue;
        }
        let entries = match trajectory_index::list_trajectory_entries_with_rollout_for(
            &app.chat.trajectory_index_coordinator,
            &trajectories_dir,
            None,
            caller,
        )
        .await
        {
            Ok(entries) => entries,
            Err(e) => {
                warn!(
                    "Failed to list trajectory index {:?}: {}",
                    trajectories_dir, e
                );
                continue;
            }
        };
        for entry in entries {
            if matches!(entry.source, trajectory_index::TrajectoryIndexSource::Buddy) {
                continue;
            }
            if displayable_only && !entry.displayable_chat {
                continue;
            }
            if !seen_ids.insert(entry.id.clone()) {
                continue;
            }
            if let Some((cursor_updated_at, cursor_id)) = cursor_filter {
                if !cursor_precedes_item(
                    (entry.updated_at.as_str(), entry.id.as_str()),
                    (cursor_updated_at.as_str(), cursor_id.as_str()),
                ) {
                    continue;
                }
            }
            candidates.push(trajectory_index::list_candidate_from_entry(
                &trajectories_dir,
                &entry,
            ));
        }
    }

    candidates
}

async fn is_real_dir(path: &Path) -> bool {
    matches!(fs::symlink_metadata(path).await, Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink())
}

fn is_real_dir_sync(path: &Path) -> bool {
    matches!(std::fs::symlink_metadata(path), Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink())
}

async fn is_real_file(path: &Path) -> bool {
    matches!(fs::symlink_metadata(path).await, Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink())
}

#[cfg(target_os = "macos")]
async fn allowed_real_dir_symlink_target(path: &Path) -> Option<PathBuf> {
    let expected_target = match path.to_str()? {
        "/tmp" => Path::new("/private/tmp"),
        "/var" => Path::new("/private/var"),
        _ => return None,
    };
    let resolved = fs::canonicalize(path).await.ok()?;
    (resolved == expected_target).then_some(resolved)
}

#[cfg(not(target_os = "macos"))]
async fn allowed_real_dir_symlink_target(_path: &Path) -> Option<PathBuf> {
    None
}

async fn ensure_real_dir_tree(path: &Path) -> Result<(), String> {
    let mut current = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(_)
            | Component::RootDir
            | Component::CurDir
            | Component::Normal(_) => {
                current.push(component.as_os_str());
            }
            Component::ParentDir => {
                return Err(format!(
                    "Refusing to create directory with parent component: {}",
                    path.display()
                ));
            }
        }
        if current.as_os_str().is_empty() {
            continue;
        }
        match fs::symlink_metadata(&current).await {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() {
                    if let Some(resolved) = allowed_real_dir_symlink_target(&current).await {
                        current = resolved;
                        continue;
                    }
                    return Err(format!(
                        "Refusing to use non-real directory component: {}",
                        current.display()
                    ));
                }
                if !metadata.is_dir() {
                    return Err(format!(
                        "Refusing to use non-real directory component: {}",
                        current.display()
                    ));
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                match fs::create_dir(&current).await {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                    Err(e) => {
                        return Err(format!(
                            "Failed to create directory {}: {}",
                            current.display(),
                            e
                        ))
                    }
                }
                let metadata = fs::symlink_metadata(&current).await.map_err(|e| {
                    format!(
                        "Failed to inspect created directory {}: {}",
                        current.display(),
                        e
                    )
                })?;
                if metadata.file_type().is_symlink() || !metadata.is_dir() {
                    return Err(format!(
                        "Created directory is not a real directory: {}",
                        current.display()
                    ));
                }
            }
            Err(e) => {
                return Err(format!(
                    "Failed to inspect directory component {}: {}",
                    current.display(),
                    e
                ));
            }
        }
    }
    if is_real_dir(path).await {
        Ok(())
    } else {
        Err(format!(
            "Directory is not a real directory: {}",
            path.display()
        ))
    }
}

async fn canonical_child_path_under_root(root: &Path, child: &Path) -> Result<PathBuf, String> {
    let root_canonical = fs::canonicalize(root)
        .await
        .map_err(|e| format!("Failed to resolve root {}: {}", root.display(), e))?;
    if !is_real_dir(root).await {
        return Err(format!("Root is not a real directory: {}", root.display()));
    }

    if is_real_file(child).await {
        let child_canonical = fs::canonicalize(child)
            .await
            .map_err(|e| format!("Failed to resolve child {}: {}", child.display(), e))?;
        if child_canonical.starts_with(&root_canonical) {
            return Ok(child.to_path_buf());
        }
        return Err(format!(
            "Child {} is outside root {}",
            child.display(),
            root.display()
        ));
    }

    if let Ok(metadata) = fs::symlink_metadata(child).await {
        if metadata.file_type().is_symlink() {
            return Err(format!("Refusing symlink child file: {}", child.display()));
        }
        return Err(format!("Child is not a real file: {}", child.display()));
    }

    let parent = child
        .parent()
        .ok_or_else(|| format!("Child has no parent: {}", child.display()))?;
    let parent_canonical = fs::canonicalize(parent)
        .await
        .map_err(|e| format!("Failed to resolve child parent {}: {}", parent.display(), e))?;
    if parent_canonical.starts_with(&root_canonical) {
        Ok(child.to_path_buf())
    } else {
        Err(format!(
            "Child parent {} is outside root {}",
            parent.display(),
            root.display()
        ))
    }
}

async fn safe_trajectory_file_in_dir(dir: &Path, chat_id: &str) -> Option<PathBuf> {
    validate_trajectory_id(chat_id).ok()?;
    ensure_real_dir_tree(dir).await.ok()?;
    let child = dir.join(format!("{}.json", chat_id));
    if !is_real_file(&child).await {
        return None;
    }
    canonical_child_path_under_root(dir, &child).await.ok()
}

async fn safe_new_trajectory_file_in_dir(dir: &Path, chat_id: &str) -> Result<PathBuf, String> {
    validate_trajectory_id(chat_id).map_err(|e| e.message)?;
    ensure_real_dir_tree(dir).await?;
    canonical_child_path_under_root(dir, &dir.join(format!("{}.json", chat_id))).await
}

fn cursor_precedes_item(item: (&str, &str), cursor: (&str, &str)) -> bool {
    item < cursor
}

pub async fn list_trajectories_page(
    app: AppState,
    limit: usize,
    cursor: Option<String>,
    displayable_only: bool,
) -> Result<PaginatedTrajectories, String> {
    list_trajectories_page_for(
        app,
        limit,
        cursor,
        displayable_only,
        trajectory_index::TrajectoryIndexListingCaller::ChatApi,
    )
    .await
}

pub async fn list_trajectories_page_for(
    app: AppState,
    limit: usize,
    cursor: Option<String>,
    displayable_only: bool,
    caller: trajectory_index::TrajectoryIndexListingCaller,
) -> Result<PaginatedTrajectories, String> {
    let gcx = app.gcx.clone();
    let limit = limit.clamp(1, 200);
    let cursor_filter = match cursor.as_deref() {
        Some(cursor) => {
            Some(decode_cursor(cursor).ok_or_else(|| "Invalid cursor format".to_string())?)
        }
        None => None,
    };

    let task_roots = get_all_task_roots(gcx.clone()).await;
    let mut candidates =
        collect_trajectory_list_candidates(&gcx, cursor_filter.as_ref(), displayable_only, caller)
            .await;
    let total_count = if cursor_filter.is_none() {
        candidates.len()
    } else {
        collect_trajectory_list_candidates(&gcx, None, displayable_only, caller)
            .await
            .len()
    };
    candidates.sort_by(|a, b| match b.updated_at.cmp(&a.updated_at) {
        std::cmp::Ordering::Equal => b.id.cmp(&a.id),
        other => other,
    });

    let (mut items, has_more) =
        hydrate_trajectory_list_page(app.clone(), candidates, limit, &task_roots).await;
    enrich_with_session_state(app, &mut items).await;

    let next_cursor = if has_more {
        items
            .last()
            .map(|last| encode_cursor(&last.updated_at, &last.id))
    } else {
        None
    };

    Ok(PaginatedTrajectories {
        items,
        next_cursor,
        has_more,
        total_count,
    })
}

pub async fn handle_v1_trajectories_list(
    State(app): State<AppState>,
    axum::extract::Query(params): axum::extract::Query<TrajectoriesListQuery>,
) -> Result<Response<Body>, ScratchError> {
    let response = list_trajectories_page(
        app,
        params.limit.unwrap_or(50),
        params.cursor,
        params.displayable_only.unwrap_or(false),
    )
    .await
    .map_err(|e| ScratchError::new(StatusCode::BAD_REQUEST, e))?;

    let json = serde_json::to_string(&response).map_err(|e| {
        ScratchError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Serialization error: {}", e),
        )
    })?;
    Ok(Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "application/json")
        .body(Body::from(json))
        .unwrap())
}

pub async fn list_all_trajectories_meta(
    app: AppState,
    displayable_only: bool,
) -> Result<Vec<TrajectoryMeta>, String> {
    let gcx = app.gcx.clone();
    let mut result: Vec<TrajectoryMeta> = Vec::new();
    let mut seen_ids = std::collections::HashSet::new();
    let task_roots = get_all_task_roots(gcx.clone()).await;

    for trajectories_dir in list_trajectory_dirs(&gcx).await {
        if !is_real_dir(&trajectories_dir).await {
            continue;
        }
        let entries = match trajectory_index::list_trajectory_entries_with_rollout_for(
            &app.chat.trajectory_index_coordinator,
            &trajectories_dir,
            None,
            trajectory_index::TrajectoryIndexListingCaller::AllTrajectoriesMetadata,
        )
        .await
        {
            Ok(entries) => entries,
            Err(e) => {
                warn!(
                    "Failed to list trajectory index {:?}: {}",
                    trajectories_dir, e
                );
                continue;
            }
        };
        for entry in entries {
            if matches!(entry.source, trajectory_index::TrajectoryIndexSource::Buddy) {
                continue;
            }
            if displayable_only && !entry.displayable_chat {
                continue;
            }
            if seen_ids.insert(entry.id.clone()) {
                let mut meta = trajectory_index::meta_from_entry(&trajectories_dir, &entry);
                if let Some(worktree) = meta.worktree.clone() {
                    meta.worktree =
                        validate_loaded_worktree_strict(app.clone(), &meta.id, worktree).await;
                }
                apply_task_trajectory_context(
                    &trajectory_index::trajectory_file_path_for_entry(&trajectories_dir, &entry),
                    &task_roots,
                    &mut meta,
                );
                result.push(meta);
            }
        }
    }

    enrich_with_session_state(app, &mut result).await;
    result.sort_by(|a, b| match b.updated_at.cmp(&a.updated_at) {
        std::cmp::Ordering::Equal => b.id.cmp(&a.id),
        other => other,
    });

    Ok(result)
}

pub async fn handle_v1_trajectories_all(
    State(app): State<AppState>,
    axum::extract::Query(params): axum::extract::Query<TrajectoriesAllQuery>,
) -> Result<Response<Body>, ScratchError> {
    let result = list_all_trajectories_meta(app, params.displayable_only)
        .await
        .map_err(|e| ScratchError::new(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    Ok(Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "application/json")
        .body(Body::from(serde_json::to_string(&result).unwrap()))
        .unwrap())
}

async fn enrich_with_session_state(app: AppState, trajectories: &mut Vec<TrajectoryMeta>) {
    let session_arcs: Vec<(usize, Arc<AMutex<ChatSession>>)> = {
        let sessions = app.chat.sessions.read().await;
        trajectories
            .iter()
            .enumerate()
            .filter_map(|(idx, traj)| sessions.get(&traj.id).map(|arc| (idx, arc.clone())))
            .collect()
    };

    for (idx, session_arc) in session_arcs {
        let session = session_arc.lock().await;
        if !trajectories[idx].source.matches_session(&session) {
            continue;
        }
        trajectories[idx].session_state = Some(session.runtime.state.to_string());
        if trajectories[idx].worktree.is_none() {
            trajectories[idx].worktree = session.thread.worktree.clone();
        }
    }
}

pub async fn handle_v1_trajectories_get(
    State(app): State<AppState>,
    AxumPath(id): AxumPath<String>,
) -> Result<Response<Body>, ScratchError> {
    let gcx = app.gcx.clone();
    validate_trajectory_id(&id)?;
    let candidate = find_trajectory_or_buddy_file(gcx, &id)
        .await
        .ok_or_else(|| {
            ScratchError::new(StatusCode::NOT_FOUND, "Trajectory not found".to_string())
        })?;
    let content = candidate.content;
    Ok(Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "application/json")
        .body(Body::from(content))
        .unwrap())
}
pub async fn handle_v1_trajectory_path(
    State(app): State<AppState>,
    AxumPath(id): AxumPath<String>,
) -> Result<Response<Body>, ScratchError> {
    let gcx = app.gcx.clone();
    let sessions = app.chat.sessions.clone();
    validate_trajectory_id(&id)?;
    let path = find_trajectory_path_for_active_chat(gcx, &sessions, &id)
        .await
        .ok_or_else(|| {
            ScratchError::new(StatusCode::NOT_FOUND, "Trajectory not found".to_string())
        })?;
    let body = serde_json::json!({ "path": path.to_string_lossy() });
    Ok(Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap())
}

pub async fn handle_v1_trajectories_save(
    State(app): State<AppState>,
    AxumPath(id): AxumPath<String>,
    body_bytes: hyper::body::Bytes,
) -> Result<Response<Body>, ScratchError> {
    let gcx = app.gcx.clone();
    validate_trajectory_id(&id)?;
    let data: TrajectoryData = serde_json::from_slice(&body_bytes)
        .map_err(|e| ScratchError::new(StatusCode::BAD_REQUEST, format!("Invalid JSON: {}", e)))?;
    let now = chrono::Utc::now().to_rfc3339();
    let mut data = data;
    data.updated_at = now.clone();
    if data.created_at.is_empty() {
        data.created_at = now.clone();
    }
    if data.id != id {
        return Err(ScratchError::new(
            StatusCode::BAD_REQUEST,
            "ID mismatch".to_string(),
        ));
    }
    let emits_generic_event = data
        .extra
        .get("buddy_meta")
        .map_or(true, |value| value.is_null());
    let file_path = resolve_trajectory_data_save_path(gcx.clone(), &id, &data)
        .await
        .map_err(|e| {
            let status = if is_trajectory_id_mismatch_error(&e) {
                StatusCode::CONFLICT
            } else {
                StatusCode::INTERNAL_SERVER_ERROR
            };
            ScratchError::new(status, e)
        })?;
    ensure_existing_trajectory_file_matches(&file_path, &id)
        .await
        .map_err(|e| ScratchError::new(StatusCode::CONFLICT, e))?;
    let is_new = !is_real_file(&file_path).await;
    let is_title_generated = data
        .extra
        .get("isTitleGenerated")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let should_generate_title =
        is_placeholder_title(&data.title) && !is_title_generated && !data.messages.is_empty();
    let worktree = if let Some(candidate) = sanitize_worktree_extra(&mut data.extra) {
        match validate_loaded_worktree_strict(app.clone(), &id, candidate).await {
            Some(validated) => {
                data.extra.insert(
                    "worktree".to_string(),
                    serde_json::to_value(&validated).unwrap_or_default(),
                );
                Some(validated)
            }
            None => {
                data.extra.remove("worktree");
                None
            }
        }
    } else {
        None
    };
    atomic_write_json(
        &file_path,
        &data,
        TrajectorySourceIdentity::from_extra(&data.extra).map_err(|e| {
            ScratchError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Invalid trajectory source for watcher suppression: {}", e),
            )
        })?,
    )
    .await
    .map_err(|e| ScratchError::new(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    if !is_internal_trace_link_type(data.extra.get("link_type").and_then(|value| value.as_str())) {
        let source_hint = Some(
            TrajectorySourceIdentity::from_extra(&data.extra).map_err(|e| {
                ScratchError::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Invalid trajectory source for index update: {}", e),
                )
            })?,
        );
        if let Some(dir) = index_dir_for_trajectory_file(gcx.clone(), &file_path).await {
            trajectory_index::upsert_trajectory_index_entry_from_owned_value_with_rollout(
                &app.chat.trajectory_index_coordinator,
                &dir,
                &file_path,
                serde_json::to_value(&data).map_err(|e| {
                    ScratchError::new(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        format!("Failed to serialize trajectory for index update: {}", e),
                    )
                })?,
                source_hint,
            )
            .await
            .map_err(|e| ScratchError::new(StatusCode::INTERNAL_SERVER_ERROR, e))?;
        }
    }
    let parent_id = data
        .extra
        .get("parent_id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let link_type = data
        .extra
        .get("link_type")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let effective_root = data
        .extra
        .get("root_chat_id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| id.clone());
    let sessions = app.chat.sessions.clone();
    let source = TrajectorySourceIdentity::from_extra(&data.extra).map_err(|e| {
        ScratchError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Invalid trajectory source for session enrichment: {}", e),
        )
    })?;
    let (session_state, session_error, session_worktree) =
        get_session_runtime_for_trajectory_source(&sessions, &id, &source).await;
    let (total_lines_added, total_lines_removed) =
        calculate_line_changes_from_messages(&data.messages);
    let (tasks_total, tasks_done, tasks_failed) =
        calculate_task_progress_from_messages(&data.messages);
    let token_totals = calculate_token_totals_from_messages(&data.messages);
    let (task_id, task_role, agent_id, card_id) = data
        .extra
        .get("task_meta")
        .and_then(|value| {
            serde_json::from_value::<crate::chat::types::TaskMeta>(value.clone()).ok()
        })
        .map(|meta| task_context_from_task_meta(Some(&meta)))
        .unwrap_or((None, None, None, None));
    if emits_generic_event {
        let event = TrajectoryEvent {
            event_type: if is_new {
                "created".to_string()
            } else {
                "updated".to_string()
            },
            id: id.clone(),
            updated_at: Some(data.updated_at.clone()),
            title: Some(trajectory_meta_title(&data.title)),
            is_title_generated: Some(is_title_generated),
            session_state: Some(session_state),
            error: session_error,
            message_count: Some(data.messages.len()),
            parent_id,
            link_type,
            root_chat_id: Some(effective_root),
            task_id,
            task_role,
            agent_id,
            card_id,
            model: Some(data.model.clone()),
            mode: Some(data.mode.clone()),
            worktree: worktree.or(session_worktree),
            total_lines_added: Some(total_lines_added),
            total_lines_removed: Some(total_lines_removed),
            tasks_total: Some(tasks_total),
            tasks_done: Some(tasks_done),
            tasks_failed: Some(tasks_failed),
            total_prompt_tokens: Some(token_totals.prompt_tokens),
            total_completion_tokens: Some(token_totals.completion_tokens),
            total_tokens: Some(token_totals.total_tokens),
            total_cache_read_tokens: Some(token_totals.cache_read_tokens),
            total_cache_creation_tokens: Some(token_totals.cache_creation_tokens),
            total_cost_usd: token_totals.cost_usd,
        };
        let _ = app.chat.trajectory_events_tx.send(event);
    }
    if should_generate_title {
        let source = TrajectorySourceIdentity::from_extra(&data.extra).map_err(|e| {
            ScratchError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Invalid trajectory source for title generation: {}", e),
            )
        })?;
        spawn_title_generation_task(
            gcx.clone(),
            id.clone(),
            data.messages.clone(),
            file_path.clone(),
            source,
        );
    }
    Ok(Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "application/json")
        .body(Body::from(r#"{"status":"ok"}"#))
        .unwrap())
}

pub async fn handle_v1_trajectories_delete(
    State(app): State<AppState>,
    AxumPath(id): AxumPath<String>,
) -> Result<Response<Body>, ScratchError> {
    let gcx = app.gcx.clone();
    validate_trajectory_id(&id)?;
    let candidate = find_trajectory_file(gcx.clone(), &id)
        .await
        .ok_or_else(|| {
            ScratchError::new(StatusCode::NOT_FOUND, "Trajectory not found".to_string())
        })?;
    let file_path = candidate.path;
    fs::remove_file(&file_path)
        .await
        .map_err(|e| ScratchError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    if let Some(dir) = index_dir_for_trajectory_file(gcx.clone(), &file_path).await {
        if let Err(e) = trajectory_index::remove_trajectory_index_entry_with_rollout(
            &app.chat.trajectory_index_coordinator,
            &dir,
            &id,
        )
        .await
        {
            warn!(
                "Failed to remove trajectory {} from index {:?}: {}",
                id, dir, e
            );
        }
    }

    let sessions = app.chat.sessions.clone();
    let fallback = match find_trajectory_file(gcx.clone(), &id).await {
        Some(candidate) => load_trajectory_candidate(gcx.clone(), &id, candidate).await,
        None => None,
    };
    let event = if let Some(mut fallback) = fallback {
        if fallback.transition_identity_repaired {
            apply_mode_defaults_to_thread(
                gcx.clone(),
                &mut fallback.thread,
                fallback.auto_approve_editing_tools_present,
                fallback.auto_approve_dangerous_commands_present,
            )
            .await;
            match persist_loaded_trajectory_repair_raw(gcx.clone(), &fallback.repair_patch()).await
            {
                Ok(updated_at) => fallback.updated_at = updated_at,
                Err(e) => warn!(
                    "Failed to persist repaired trajectory for {}: {}",
                    fallback.thread.id, e
                ),
            }
        }
        let task_roots = get_all_task_roots(gcx).await;
        let meta = loaded_trajectory_to_meta(&fallback, &task_roots);
        let (session_state, session_error, session_worktree) =
            get_session_runtime_for_trajectory_source(&sessions, &id, &meta.source).await;
        updated_trajectory_event_from_meta_with_worktree(
            meta,
            Some(fallback.thread.is_title_generated),
            session_state,
            session_error,
            session_worktree,
        )
    } else {
        deleted_trajectory_event(id.clone())
    };
    let _ = app.chat.trajectory_events_tx.send(event);
    Ok(Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "application/json")
        .body(Body::from(r#"{"status":"ok"}"#))
        .unwrap())
}

pub async fn handle_v1_trajectories_subscribe(
    State(app): State<AppState>,
) -> Result<Response<Body>, ScratchError> {
    let rx = app.chat.trajectory_events_tx.subscribe();
    let stream = async_stream::stream! {
        let mut rx = rx;
        loop {
            match rx.recv().await {
                Ok(event) => {
                    match serde_json::to_string(&event) {
                        Ok(json) => yield Ok::<_, std::convert::Infallible>(format!("data: {}\n\n", json)),
                        Err(e) => {
                            tracing::error!("Failed to serialize trajectory SSE event: {}", e);
                            break;
                        }
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    };
    Ok(Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "text/event-stream")
        .header("Cache-Control", "no-cache")
        .header("Connection", "keep-alive")
        .body(Body::wrap_stream(stream))
        .unwrap())
}

#[cfg(test)]
mod tests {
    #[test]
    fn malformed_nested_report_remains_browsable_without_emptying_archive() {
        let raw = serde_json::json!([
            {"role":"user", "content":"ARCHIVED_LOAD_SENTINEL"},
            {"role":"compression_report", "content":"invalid boundary", "compression_report":{
                "kind":"reconstructed_history", "schema_version":1,
                "payload":{"messages":"not an array"}
            }}
        ]);
        let messages = super::parse_stored_messages(raw).unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(
            messages[0].content.content_text_only(),
            "ARCHIVED_LOAD_SENTINEL"
        );
        assert!(refact_core::active_context::active_context(&messages).is_err());
        assert!(super::parse_stored_messages(serde_json::json!([{"role":42}])).is_err());
    }

    use super::*;
    use crate::chat::diagnostics::{is_ui_only_message, make_ui_only_error_message};
    use crate::chat::perf_diagnostics::{self, MemoryPerfSink, PerfClock, PerfRecorder};
    use crate::chat::types::{
        ActiveCommandContext, ChatEvent, CompressionPhase, CompressionReason, EventEnvelope,
    };
    use crate::vecdb::vdb_structs::{EmbeddingModelConfig, SearchResult, VecDbStatus, VecdbRecord};
    use async_trait::async_trait;
    use refact_core::vecdb_types::VecdbSearch;
    use refact_chat_api::{
        BuddyThreadMeta, ClaudeCodeIdentity, FrozenRequestPrefix, GoalBudget, GoalProgress,
        GoalStatus,
    };
    use serial_test::serial;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::sync::Mutex as StdMutex;

    struct BlockingRecordingVecdb {
        enqueued: Arc<StdMutex<Vec<String>>>,
        enqueue_started: Arc<Notify>,
        release_enqueue: Arc<Notify>,
    }

    struct PanickingVecdb {
        enqueue_started: Arc<Notify>,
    }

    #[async_trait]
    impl VecdbSearch for BlockingRecordingVecdb {
        async fn vecdb_search(
            &self,
            query: String,
            _top_n: usize,
            _filter_mb: Option<String>,
        ) -> Result<SearchResult, String> {
            Ok(SearchResult {
                query_text: query,
                results: vec![],
            })
        }

        async fn get_status(&self) -> Result<VecDbStatus, String> {
            Ok(VecDbStatus {
                files_unprocessed: 0,
                files_total: 0,
                requests_made_since_start: 0,
                vectors_made_since_start: 0,
                db_size: 0,
                db_cache_size: 0,
                state: "done".to_string(),
                queue_additions: false,
                vecdb_max_files_hit: false,
                vecdb_errors: Default::default(),
                cache_writes_failed: 0,
                vectors_missing_from_cache: 0,
            })
        }

        async fn remove_file(&self, _file_path: &PathBuf) -> Result<(), String> {
            Ok(())
        }

        async fn vectorizer_enqueue_files(
            &self,
            documents: &[String],
            _process_immediately: bool,
            _roots: refact_core::memory_plane::MemoryPlaneRoots,
        ) {
            self.enqueued.lock().unwrap().extend(documents.to_vec());
            self.enqueue_started.notify_one();
            self.release_enqueue.notified().await;
        }

        fn current_constants(&self) -> (EmbeddingModelConfig, usize) {
            (
                EmbeddingModelConfig {
                    model_id: String::new(),
                    endpoint: String::new(),
                    endpoint_style: String::new(),
                    embedding_endpoint_style: String::new(),
                    api_key: String::new(),
                    model_name: String::new(),
                    embedding_size: 0,
                    dimensions: None,
                    query_prefix: String::new(),
                    document_prefix: String::new(),
                    rejection_threshold: 0.0,
                    embedding_batch: 1,
                    n_ctx: 0,
                },
                0,
            )
        }

        async fn embed_query(&self, _query: &str) -> Result<Vec<f32>, String> {
            Ok(vec![])
        }

        async fn vecdb_search_with_embedding(
            &self,
            _embedding: &Vec<f32>,
            _top_n: usize,
            _filter_mb: Option<String>,
        ) -> Result<Vec<VecdbRecord>, String> {
            Ok(vec![])
        }
    }

    #[async_trait]
    impl VecdbSearch for PanickingVecdb {
        async fn vecdb_search(
            &self,
            query: String,
            _top_n: usize,
            _filter_mb: Option<String>,
        ) -> Result<SearchResult, String> {
            Ok(SearchResult {
                query_text: query,
                results: vec![],
            })
        }

        async fn get_status(&self) -> Result<VecDbStatus, String> {
            Ok(VecDbStatus {
                files_unprocessed: 0,
                files_total: 0,
                requests_made_since_start: 0,
                vectors_made_since_start: 0,
                db_size: 0,
                db_cache_size: 0,
                state: "done".to_string(),
                queue_additions: false,
                vecdb_max_files_hit: false,
                vecdb_errors: Default::default(),
                cache_writes_failed: 0,
                vectors_missing_from_cache: 0,
            })
        }

        async fn remove_file(&self, _file_path: &PathBuf) -> Result<(), String> {
            Ok(())
        }

        async fn vectorizer_enqueue_files(
            &self,
            _documents: &[String],
            _process_immediately: bool,
            _roots: refact_core::memory_plane::MemoryPlaneRoots,
        ) {
            self.enqueue_started.notify_one();
            panic!("test VecDB enqueue failure");
        }

        fn current_constants(&self) -> (EmbeddingModelConfig, usize) {
            (
                EmbeddingModelConfig {
                    model_id: String::new(),
                    endpoint: String::new(),
                    endpoint_style: String::new(),
                    embedding_endpoint_style: String::new(),
                    api_key: String::new(),
                    model_name: String::new(),
                    embedding_size: 0,
                    dimensions: None,
                    query_prefix: String::new(),
                    document_prefix: String::new(),
                    rejection_threshold: 0.0,
                    embedding_batch: 1,
                    n_ctx: 0,
                },
                0,
            )
        }

        async fn embed_query(&self, _query: &str) -> Result<Vec<f32>, String> {
            Ok(vec![])
        }

        async fn vecdb_search_with_embedding(
            &self,
            _embedding: &Vec<f32>,
            _top_n: usize,
            _filter_mb: Option<String>,
        ) -> Result<Vec<VecdbRecord>, String> {
            Ok(vec![])
        }
    }

    fn normalized_test_path(path: &Path) -> PathBuf {
        crate::files_correction::canonicalize_normalized_path(path.to_path_buf())
    }

    struct TestPerfClock {
        now: AtomicU64,
    }

    impl TestPerfClock {
        fn new() -> Self {
            Self {
                now: AtomicU64::new(0),
            }
        }
    }

    impl PerfClock for TestPerfClock {
        fn now_us(&self) -> u64 {
            self.now.fetch_add(1, Ordering::SeqCst)
        }
    }

    fn install_perf_recorder() -> (
        perf_diagnostics::TestRecorderGuard,
        Arc<MemoryPerfSink>,
        Arc<PerfRecorder>,
    ) {
        let sink = Arc::new(MemoryPerfSink::new());
        let recorder = Arc::new(PerfRecorder::with_salt(
            Arc::new(TestPerfClock::new()),
            sink.clone(),
            [5; 32],
        ));
        (
            perf_diagnostics::install_test_recorder(recorder.clone()),
            sink,
            recorder,
        )
    }

    fn serial_test_guard() -> std::sync::MutexGuard<'static, ()> {
        perf_diagnostics::PERF_RECORDER_TEST_LOCK.lock().unwrap()
    }

    #[test]
    fn trajectory_snapshot_excludes_runtime_only_diagnostic_timestamps() {
        let mut session = ChatSession::new("runtime-only-diagnostics".to_string());
        let now = Instant::now();
        session
            .command_enqueued_at
            .insert("request-1".to_string(), now);
        session.stream_started_at = Some(now);
        session.confirmation_paused_at = Some((
            now,
            std::collections::HashSet::from(["tool-call".to_string()]),
        ));

        let serialized = serde_json::to_string(&trajectory_snapshot_from_session(&session))
            .expect("trajectory snapshot serializes");

        assert!(!serialized.contains("command_enqueued_at"));
        assert!(!serialized.contains("stream_started_at"));
        assert!(!serialized.contains("confirmation_paused_at"));
    }

    #[test]
    fn trajectory_commit_intents_are_table_driven() {
        let boundaries = trajectory_commit_contract();
        assert_eq!(
            boundaries,
            [
                (
                    "explicit acknowledged mutations",
                    TrajectoryCommitIntent::Required
                ),
                (
                    "branch handoff mode transition",
                    TrajectoryCommitIntent::Required
                ),
                ("final failed subchat", TrajectoryCommitIntent::Required),
                ("close", TrajectoryCommitIntent::Required),
                ("graceful shutdown", TrajectoryCommitIntent::Required),
                (
                    "assistant tool subchat progress",
                    TrajectoryCommitIntent::Checkpoint,
                ),
                ("stream delta", TrajectoryCommitIntent::Ephemeral),
            ]
        );
        assert!(TrajectoryCommitIntent::Required.requires_durability());
        assert!(TrajectoryCommitIntent::Checkpoint.persists());
        assert!(!TrajectoryCommitIntent::Ephemeral.persists());
    }

    #[test]
    fn trajectory_commit_stale_completion_cannot_clear_newer_dirty_state() {
        let mut session = ChatSession::new("trajectory-commit-stale".to_string());

        session.increment_version();
        assert!(session.complete_trajectory_commit(1));
        assert_eq!(session.trajectory_committed_version, 1);
        assert!(!session.trajectory_dirty);

        session.increment_version();
        assert!(session.complete_trajectory_commit(2));
        assert_eq!(session.trajectory_committed_version, 2);
        assert!(!session.trajectory_dirty);

        session.increment_version();
        assert!(!session.complete_trajectory_commit(1));
        assert_eq!(session.trajectory_committed_version, 2);
        assert!(session.trajectory_dirty);
    }

    #[test]
    fn trajectory_commit_versions_are_isolated_per_chat() {
        let mut chat_a = ChatSession::new("trajectory-commit-a".to_string());
        let mut chat_b = ChatSession::new("trajectory-commit-b".to_string());

        chat_a.increment_version();
        chat_a.complete_trajectory_commit(1);
        chat_a.increment_version();

        chat_b.increment_version();
        chat_b.complete_trajectory_commit(1);
        chat_b.increment_version();
        chat_b.complete_trajectory_commit(2);

        assert_eq!(chat_a.trajectory_committed_version, 1);
        assert!(chat_a.trajectory_dirty);
        assert_eq!(chat_b.trajectory_committed_version, 2);
        assert!(!chat_b.trajectory_dirty);
    }

    #[serial(trajectory_perf)]
    #[tokio::test]
    async fn trajectory_commit_rejects_a_stale_snapshot_before_file_write() {
        let _lock = serial_test_guard();
        let workspace = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(workspace.path()).await;
        let chat_id = "trajectory-commit-file-barrier";
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));

        let stale = {
            let mut session = session_arc.lock().await;
            session.add_message(ChatMessage::new("user".to_string(), "older".to_string()));
            trajectory_snapshot_from_session(&session)
        };
        {
            let mut session = session_arc.lock().await;
            session.add_message(ChatMessage::new("user".to_string(), "newer".to_string()));
        }
        let current = {
            let session = session_arc.lock().await;
            trajectory_snapshot_from_session(&session)
        };

        assert_eq!(
            commit_trajectory_snapshot_for_session(
                gcx.clone(),
                session_arc.clone(),
                current,
                TrajectoryCommitIntent::Required,
            )
            .await
            .unwrap(),
            SessionTrajectoryCommitOutcome::Committed
        );
        assert_eq!(
            commit_trajectory_snapshot_for_session(
                gcx.clone(),
                session_arc.clone(),
                stale,
                TrajectoryCommitIntent::Required,
            )
            .await
            .unwrap(),
            SessionTrajectoryCommitOutcome::SkippedStale
        );

        let loaded = load_trajectory_for_chat(gcx, chat_id).await.unwrap();
        let content = loaded
            .messages
            .iter()
            .map(|message| message.content.content_text_only())
            .collect::<Vec<_>>();
        assert_eq!(content, vec!["older", "newer"]);
    }

    #[serial(trajectory_perf)]
    #[tokio::test]
    async fn trajectory_writer_enabled_checkpoints_coalesce_to_the_latest_session_snapshot() {
        let _lock = serial_test_guard();
        let _writer = TrajectoryWriterRolloutGuard::set(true);
        let workspace = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(workspace.path()).await;
        let chat_id = "checkpoint-writer-coalesces";
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));

        {
            let mut session = session_arc.lock().await;
            session.add_message(ChatMessage::new("user".to_string(), "first".to_string()));
        }
        maybe_save_trajectory_background_with_intent(
            app.clone(),
            session_arc.clone(),
            TrajectoryCommitIntent::Checkpoint,
        );
        {
            let mut session = session_arc.lock().await;
            session.add_message(ChatMessage::new("user".to_string(), "second".to_string()));
        }
        maybe_save_trajectory_background_with_intent(
            app.clone(),
            session_arc.clone(),
            TrajectoryCommitIntent::Checkpoint,
        );

        flush_trajectory_for_session(app, session_arc)
            .await
            .unwrap();
        let loaded = load_trajectory_for_chat(gcx, chat_id).await.unwrap();
        assert_eq!(
            loaded
                .messages
                .iter()
                .map(|message| message.content.content_text_only())
                .collect::<Vec<_>>(),
            vec!["first", "second"]
        );
    }

    #[serial(trajectory_perf)]
    #[tokio::test]
    async fn trajectory_writer_disabled_checkpoint_saves_synchronously() {
        let _lock = serial_test_guard();
        let _writer = TrajectoryWriterRolloutGuard::set(false);
        let workspace = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(workspace.path()).await;
        let chat_id = "checkpoint-writer-legacy";
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));

        session_arc
            .lock()
            .await
            .add_message(ChatMessage::new("user".to_string(), "first".to_string()));
        try_save_trajectory_with_intent(
            app,
            session_arc.clone(),
            TrajectoryCommitIntent::Checkpoint,
        )
        .await
        .unwrap();

        let session = session_arc.lock().await;
        assert!(!session.trajectory_dirty);
        assert!(!session.trajectory_save_in_flight);
        assert_eq!(
            session.trajectory_committed_version,
            session.trajectory_version
        );
        drop(session);
        let loaded = load_trajectory_for_chat(gcx, chat_id).await.unwrap();
        assert_eq!(loaded.messages.len(), 1);
    }

    #[serial(trajectory_perf)]
    #[tokio::test]
    async fn trajectory_writer_background_required_intent_waits_for_its_commit() {
        let _lock = serial_test_guard();
        let _writer = TrajectoryWriterRolloutGuard::set(true);
        let workspace = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(workspace.path()).await;
        let chat_id = "background-required-writer";
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        session_arc
            .lock()
            .await
            .add_message(ChatMessage::new("user".to_string(), "required".to_string()));

        maybe_save_trajectory_background_with_intent(
            app,
            session_arc.clone(),
            TrajectoryCommitIntent::Required,
        );
        tokio::time::timeout(
            Duration::from_secs(5),
            wait_for_trajectory_commit(session_arc.clone(), 1),
        )
        .await
        .expect("background required save should commit")
        .unwrap();

        let session = session_arc.lock().await;
        assert!(!session.trajectory_dirty);
        assert_eq!(
            session.trajectory_committed_version,
            session.trajectory_version
        );
        drop(session);
        assert!(load_trajectory_for_chat(gcx, chat_id).await.is_some());
    }

    #[serial(trajectory_perf)]
    #[tokio::test]
    async fn trajectory_writer_paths_preserve_stale_required_and_shutdown_commits() {
        let _lock = serial_test_guard();
        for enabled in [false, true] {
            let _writer = TrajectoryWriterRolloutGuard::set(enabled);
            let workspace = tempfile::tempdir().unwrap();
            let (gcx, app) = make_app_with_workspace(workspace.path()).await;
            let chat_id = format!("writer-path-{enabled}");
            let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.clone())));

            let stale = {
                let mut session = session_arc.lock().await;
                session.add_message(ChatMessage::new("user".to_string(), "older".to_string()));
                trajectory_snapshot_from_session(&session)
            };
            let current = {
                let mut session = session_arc.lock().await;
                session.add_message(ChatMessage::new("user".to_string(), "newer".to_string()));
                trajectory_snapshot_from_session(&session)
            };
            assert_eq!(
                commit_trajectory_snapshot_for_session(
                    gcx.clone(),
                    session_arc.clone(),
                    current,
                    TrajectoryCommitIntent::Required,
                )
                .await
                .unwrap(),
                SessionTrajectoryCommitOutcome::Committed
            );
            assert_eq!(
                commit_trajectory_snapshot_for_session(
                    gcx.clone(),
                    session_arc.clone(),
                    stale,
                    TrajectoryCommitIntent::Required,
                )
                .await
                .unwrap(),
                SessionTrajectoryCommitOutcome::SkippedStale
            );

            session_arc
                .lock()
                .await
                .add_message(ChatMessage::new("user".to_string(), "shutdown".to_string()));
            app.chat
                .sessions
                .write()
                .await
                .insert(chat_id.clone(), session_arc);
            flush_all_trajectories(app).await.unwrap();

            let loaded = load_trajectory_for_chat(gcx, &chat_id).await.unwrap();
            assert_eq!(
                loaded
                    .messages
                    .iter()
                    .map(|message| message.content.content_text_only())
                    .collect::<Vec<_>>(),
                vec!["older", "newer", "shutdown"]
            );
        }
    }

    #[serial(trajectory_perf)]
    #[tokio::test]
    async fn required_writer_waits_for_a_checkpoint_and_commits_latest_version() {
        let _lock = serial_test_guard();
        let _writer = TrajectoryWriterRolloutGuard::set(true);
        let workspace = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(workspace.path()).await;
        let chat_id = "required-after-checkpoint";
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));

        {
            let mut session = session_arc.lock().await;
            session.add_message(ChatMessage::new(
                "user".to_string(),
                "checkpoint".to_string(),
            ));
        }
        maybe_save_trajectory_background_with_intent(
            app.clone(),
            session_arc.clone(),
            TrajectoryCommitIntent::Checkpoint,
        );
        {
            let mut session = session_arc.lock().await;
            session.add_message(ChatMessage::new("user".to_string(), "required".to_string()));
        }

        try_save_trajectory_with_intent(app, session_arc.clone(), TrajectoryCommitIntent::Required)
            .await
            .unwrap();
        let session = session_arc.lock().await;
        assert!(!session.trajectory_dirty);
        assert_eq!(
            session.trajectory_committed_version,
            session.trajectory_version
        );
        drop(session);
        let loaded = load_trajectory_for_chat(gcx, chat_id).await.unwrap();
        assert_eq!(loaded.messages.len(), 2);
    }

    #[serial(trajectory_perf)]
    #[tokio::test]
    async fn different_session_writers_commit_independently() {
        let workspace = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(workspace.path()).await;
        let left = Arc::new(AMutex::new(ChatSession::new("writer-left".to_string())));
        let right = Arc::new(AMutex::new(ChatSession::new("writer-right".to_string())));
        left.lock()
            .await
            .add_message(ChatMessage::new("user".to_string(), "left".to_string()));
        right
            .lock()
            .await
            .add_message(ChatMessage::new("user".to_string(), "right".to_string()));

        let (left_result, right_result) = tokio::join!(
            flush_trajectory_for_session(app.clone(), left),
            flush_trajectory_for_session(app, right),
        );
        left_result.unwrap();
        right_result.unwrap();
        assert_eq!(
            load_trajectory_for_chat(gcx.clone(), "writer-left")
                .await
                .unwrap()
                .messages[0]
                .content
                .content_text_only(),
            "left"
        );
        assert_eq!(
            load_trajectory_for_chat(gcx, "writer-right")
                .await
                .unwrap()
                .messages[0]
                .content
                .content_text_only(),
            "right"
        );
    }

    #[tokio::test]
    async fn trajectory_commit_finishes_before_blocked_vecdb_enqueue() {
        let workspace = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(workspace.path()).await;
        let enqueued = Arc::new(StdMutex::new(Vec::new()));
        let enqueue_started = Arc::new(Notify::new());
        let release_enqueue = Arc::new(Notify::new());
        *gcx.vec_db.lock().await = Some(Arc::new(BlockingRecordingVecdb {
            enqueued: enqueued.clone(),
            enqueue_started: enqueue_started.clone(),
            release_enqueue: release_enqueue.clone(),
        }));

        let mut save = tokio::spawn(save_trajectory_snapshot(
            gcx.clone(),
            test_snapshot(
                "commit-before-enqueue",
                "Committed",
                vec![ChatMessage::new("user".to_string(), "durable".to_string())],
            ),
        ));

        tokio::time::timeout(Duration::from_secs(60), enqueue_started.notified())
            .await
            .unwrap();
        assert!(load_trajectory_for_chat(gcx, "commit-before-enqueue")
            .await
            .is_some());
        tokio::time::timeout(Duration::from_millis(100), &mut save)
            .await
            .expect("required trajectory commit must not wait for VecDB enqueue")
            .unwrap()
            .unwrap();
        release_enqueue.notify_one();
    }

    #[tokio::test]
    async fn buddy_trajectory_save_is_not_enqueued_for_vectorization() {
        let workspace = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(workspace.path()).await;
        let enqueued = Arc::new(StdMutex::new(Vec::new()));
        let enqueue_started = Arc::new(Notify::new());
        let release_enqueue = Arc::new(Notify::new());
        *gcx.vec_db.lock().await = Some(Arc::new(BlockingRecordingVecdb {
            enqueued: enqueued.clone(),
            enqueue_started: enqueue_started.clone(),
            release_enqueue,
        }));
        let mut snapshot = test_snapshot(
            "buddy-not-vectorized",
            "Buddy",
            vec![ChatMessage::new("user".to_string(), "hello".to_string())],
        );
        snapshot.buddy_meta = Some(buddy_thread_meta());

        save_trajectory_snapshot(gcx, snapshot).await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(100), enqueue_started.notified())
                .await
                .is_err()
        );

        assert!(enqueued.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn internal_trace_save_is_not_enqueued_for_vectorization() {
        let workspace = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(workspace.path()).await;
        let enqueued = Arc::new(StdMutex::new(Vec::new()));
        let enqueue_started = Arc::new(Notify::new());
        let release_enqueue = Arc::new(Notify::new());
        *gcx.vec_db.lock().await = Some(Arc::new(BlockingRecordingVecdb {
            enqueued: enqueued.clone(),
            enqueue_started: enqueue_started.clone(),
            release_enqueue,
        }));
        let mut snapshot = test_snapshot(
            "internal-trace-not-vectorized",
            "Trace",
            vec![ChatMessage::new("user".to_string(), "hello".to_string())],
        );
        snapshot.link_type = Some(internal_trace_link_type("title_generation"));

        save_trajectory_snapshot(gcx, snapshot).await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(100), enqueue_started.notified())
                .await
                .is_err()
        );

        assert!(enqueued.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn vecdb_enqueue_failure_does_not_fail_trajectory_save() {
        let workspace = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(workspace.path()).await;
        let enqueue_started = Arc::new(Notify::new());
        *gcx.vec_db.lock().await = Some(Arc::new(PanickingVecdb {
            enqueue_started: enqueue_started.clone(),
        }));

        save_trajectory_snapshot(
            gcx.clone(),
            test_snapshot(
                "enqueue-failure",
                "Committed",
                vec![ChatMessage::new("user".to_string(), "durable".to_string())],
            ),
        )
        .await
        .unwrap();
        tokio::time::timeout(Duration::from_secs(60), enqueue_started.notified())
            .await
            .expect("best-effort VecDB enqueue should have run");
        assert!(load_trajectory_for_chat(gcx, "enqueue-failure")
            .await
            .is_some());
    }

    #[test]
    fn trajectory_rollout_switches_default_on_and_honor_explicit_false() {
        let defaults = crate::runtime_settings::TrajectoryRuntimeSettings::default();
        assert!(defaults.trajectory_writer_enabled);
        assert!(defaults.trajectory_index_coordinator_enabled);
        assert!(defaults.trajectory_watcher_self_write_enabled);
        assert!(defaults.tool_catalog_snapshots_enabled);
        assert!(defaults.vecdb_path_coalescing_enabled);
        assert!(trajectory_writer_rollout_enabled_for(
            defaults.trajectory_writer_enabled.then_some("1")
        ));
        assert!(trajectory_watcher_self_write_rollout_enabled_for(
            defaults
                .trajectory_watcher_self_write_enabled
                .then_some("1")
        ));
        assert!(trajectory_writer_rollout_enabled_for(Some("1")));
        assert!(trajectory_writer_rollout_enabled_for(Some("true")));
        assert!(!trajectory_writer_rollout_enabled_for(Some("0")));
        assert!(!trajectory_writer_rollout_enabled_for(Some(" false ")));
        assert!(!trajectory_writer_rollout_enabled_for(Some("OFF")));
        assert!(!trajectory_watcher_self_write_rollout_enabled_for(Some(
            "0"
        )));
        assert!(!trajectory_watcher_self_write_rollout_enabled_for(Some(
            "false"
        )));
        assert!(trajectory_watcher_self_write_rollout_enabled_for(Some(
            "on"
        )));
    }

    #[test]
    fn trajectory_writer_rollout_remains_restart_cached() {
        let writer_cache = OnceLock::new();
        assert!(rollout_switch_enabled(&writer_cache, || true));
        assert!(rollout_switch_enabled(&writer_cache, || false));
    }

    #[tokio::test]
    async fn detached_trajectory_writer_keys_preserve_workspace_boundaries() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let snapshot = test_snapshot("writer-key", "Writer key", Vec::new());
        {
            let mut folders = gcx.documents_state.workspace_folders.lock().unwrap();
            *folders = vec![PathBuf::from("alpha|beta"), PathBuf::from("gamma")];
        }
        let first = detached_trajectory_writer_key(&gcx, &snapshot);
        {
            let mut folders = gcx.documents_state.workspace_folders.lock().unwrap();
            *folders = vec![PathBuf::from("alpha"), PathBuf::from("beta|gamma")];
        }
        let second = detached_trajectory_writer_key(&gcx, &snapshot);
        assert_ne!(first, second);
    }

    #[test]
    fn trajectory_writer_ten_thousand_stale_interleavings_preserve_newer_dirty_state() {
        let mut session = ChatSession::new("trajectory-ten-thousand-stale".to_string());
        for _ in 0..10_000 {
            session.increment_version();
            let stale_version = session.trajectory_version;
            session.increment_version();
            let current_version = session.trajectory_version;
            assert!(session.complete_trajectory_commit(current_version));
            session.increment_version();
            assert!(!session.complete_trajectory_commit(stale_version));
            assert_eq!(session.trajectory_committed_version, current_version);
            assert!(session.trajectory_dirty);
            assert!(session.complete_trajectory_commit(session.trajectory_version));
            assert!(!session.trajectory_dirty);
        }
        assert_eq!(session.trajectory_committed_version, 30_000);
    }

    #[serial(trajectory_perf)]
    #[tokio::test]
    async fn trajectory_writer_failed_checkpoint_retries_before_a_required_commit() {
        let _lock = serial_test_guard();
        let _writer = TrajectoryWriterRolloutGuard::set(true);
        let workspace = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(workspace.path()).await;
        let chat_id = "checkpoint-retry-required";
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        let failing_message = ChatMessage {
            message_id: "checkpoint-failure".to_string(),
            role: "user".to_string(),
            content: ChatContent::SimpleText("first".to_string()),
            ..Default::default()
        };
        {
            let mut session = session_arc.lock().await;
            session.add_message(failing_message.clone());
        }
        set_test_message_serialization_failure(Some(failing_message.message_id));
        try_save_trajectory_with_intent(
            app.clone(),
            session_arc.clone(),
            TrajectoryCommitIntent::Checkpoint,
        )
        .await
        .unwrap();
        tokio::time::timeout(Duration::from_secs(60), async {
            loop {
                if session_arc.lock().await.trajectory_save_error.is_some() {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("checkpoint failure should reach the session");
        set_test_message_serialization_failure(None);

        {
            let mut session = session_arc.lock().await;
            session.add_message(ChatMessage::new("user".to_string(), "required".to_string()));
        }
        try_save_trajectory_with_intent(app, session_arc.clone(), TrajectoryCommitIntent::Required)
            .await
            .unwrap();

        let session = session_arc.lock().await;
        assert!(!session.trajectory_dirty);
        assert_eq!(
            session.trajectory_committed_version,
            session.trajectory_version
        );
        drop(session);
        let loaded = load_trajectory_for_chat(gcx, chat_id).await.unwrap();
        assert_eq!(loaded.messages.len(), 2);
    }

    #[tokio::test]
    async fn trajectory_writer_cache_preserves_external_metadata() {
        let workspace = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(workspace.path()).await;
        let chat_id = "detached-writer-cache";
        let first = test_snapshot(
            chat_id,
            "First",
            vec![ChatMessage::new("user".to_string(), "first".to_string())],
        );
        let write_cache = Arc::new(AMutex::new(DetachedTrajectoryWriteCache::default()));
        save_trajectory_snapshot_inner(gcx.clone(), first, Some(write_cache.clone()))
            .await
            .unwrap();
        let path = workspace
            .path()
            .join(".refact")
            .join("trajectories")
            .join(chat_id)
            .join(format!("{chat_id}.json"));
        let mut external: serde_json::Value =
            serde_json::from_str(&tokio::fs::read_to_string(&path).await.unwrap()).unwrap();
        external["external_metadata"] = json!({"preserved": true});
        tokio::fs::write(&path, serde_json::to_string_pretty(&external).unwrap())
            .await
            .unwrap();

        let second = test_snapshot(
            chat_id,
            "Second",
            vec![ChatMessage::new("user".to_string(), "second".to_string())],
        );
        save_trajectory_snapshot_inner(gcx, second, Some(write_cache))
            .await
            .unwrap();

        let saved: serde_json::Value =
            serde_json::from_str(&tokio::fs::read_to_string(path).await.unwrap()).unwrap();
        assert_eq!(saved["external_metadata"], json!({"preserved": true}));
    }

    #[serial(trajectory_perf)]
    #[tokio::test]
    async fn repeated_save_reuses_pretty_round_trip_metadata_without_a_second_disk_read() {
        let _lock = serial_test_guard();
        let workspace = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(workspace.path()).await;
        let chat_id = "metadata-cache-round-trip";
        let path = workspace
            .path()
            .join(".refact")
            .join("trajectories")
            .join(chat_id)
            .join(format!("{chat_id}.json"));
        tokio::fs::create_dir_all(path.parent().unwrap())
            .await
            .unwrap();
        tokio::fs::write(
            &path,
            serde_json::to_string_pretty(&json!({
                "id": chat_id,
                "title": "Before",
                "messages": [],
                "browser_meta": {
                    "browser_runtime_id": "browser-round-trip",
                    "tab_urls": ["https://example.com/round-trip"]
                },
                "custom_future_field": {"nested": {"value": 61}},
                "custom_scalar": "preserve-me"
            }))
            .unwrap(),
        )
        .await
        .unwrap();

        let reads_before = trajectory_metadata_disk_reads_for_chat(chat_id);
        for title in ["First", "Second"] {
            save_trajectory_snapshot(
                gcx.clone(),
                test_snapshot(
                    chat_id,
                    title,
                    vec![ChatMessage::new("user".to_string(), title.to_string())],
                ),
            )
            .await
            .unwrap();
        }
        let reads_after = trajectory_metadata_disk_reads_for_chat(chat_id);
        assert_eq!(
            reads_after - reads_before,
            1,
            "the second save must reuse the cached metadata instead of re-reading from disk"
        );

        let text = tokio::fs::read_to_string(&path).await.unwrap();
        let saved: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(text, serde_json::to_string_pretty(&saved).unwrap());
        assert!(text.contains("\n  \"browser_meta\": {"));
        assert_eq!(saved["title"], "Second");
        assert_eq!(
            saved["browser_meta"]["browser_runtime_id"],
            "browser-round-trip"
        );
        assert_eq!(saved["custom_future_field"]["nested"]["value"], 61);
        assert_eq!(saved["custom_scalar"], "preserve-me");

        let loaded = load_trajectory_for_chat(gcx, chat_id).await.unwrap();
        assert_eq!(loaded.messages.len(), 1);
        assert_eq!(loaded.messages[0].content.content_text_only(), "Second");
    }

    #[tokio::test]
    async fn trajectory_writer_cache_rejects_replaced_trajectory_for_another_chat() {
        let workspace = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(workspace.path()).await;
        let chat_id = "detached-writer-cache-replaced";
        let write_cache = Arc::new(AMutex::new(DetachedTrajectoryWriteCache::default()));
        save_trajectory_snapshot_inner(
            gcx.clone(),
            test_snapshot(
                chat_id,
                "First",
                vec![ChatMessage::new("user".to_string(), "first".to_string())],
            ),
            Some(write_cache.clone()),
        )
        .await
        .unwrap();
        let path = workspace
            .path()
            .join(".refact")
            .join("trajectories")
            .join(chat_id)
            .join(format!("{chat_id}.json"));
        let foreign = json!({
            "id": "other-chat",
            "title": "Foreign",
            "messages": [],
        });
        tokio::fs::write(&path, serde_json::to_string_pretty(&foreign).unwrap())
            .await
            .unwrap();

        let error = save_trajectory_snapshot_inner(
            gcx,
            test_snapshot(
                chat_id,
                "Second",
                vec![ChatMessage::new("user".to_string(), "second".to_string())],
            ),
            Some(write_cache),
        )
        .await
        .unwrap_err();
        assert!(is_trajectory_id_mismatch_error(&error));
        let saved: serde_json::Value =
            serde_json::from_str(&tokio::fs::read_to_string(path).await.unwrap()).unwrap();
        assert_eq!(saved["id"], "other-chat");
    }

    #[serial(trajectory_perf)]
    #[tokio::test]
    async fn trajectory_writer_failure_is_stored_and_reported_to_waiters() {
        let workspace = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(workspace.path()).await;
        let mut snapshot = test_snapshot(
            "detached-writer-failure",
            "Failure",
            vec![ChatMessage::new("user".to_string(), "failure".to_string())],
        );
        snapshot.messages[0].message_id = "detached-writer-failure-message".to_string();
        let writer = Arc::new(AMutex::new(DetachedTrajectoryWriterState {
            pending: Some((1, snapshot.clone())),
            requested_version: 1,
            in_flight: true,
            ..Default::default()
        }));
        set_test_message_serialization_failure(Some(snapshot.messages[0].message_id.clone()));
        run_detached_trajectory_writer(gcx, writer.clone()).await;
        set_test_message_serialization_failure(None);

        let error = wait_for_detached_trajectory_commit(writer.clone(), 1)
            .await
            .unwrap_err();
        assert!(error.contains("test failure"));
        assert_eq!(writer.lock().await.error.as_deref(), Some(error.as_str()));
    }

    #[serial(trajectory_perf)]
    #[serial(trajectory_perf)]
    #[tokio::test]
    async fn detached_trajectory_writer_drains_newer_snapshot_after_a_failed_write() {
        let _lock = serial_test_guard();
        let workspace = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(workspace.path()).await;
        let chat_id = "detached-writer-drains-newer";
        let mut failed = test_snapshot(
            chat_id,
            "Failed",
            vec![ChatMessage::new("user".to_string(), "failed".to_string())],
        );
        failed.messages[0].message_id = "detached-writer-drains-newer-failed".to_string();
        let newer = test_snapshot(
            chat_id,
            "Newer",
            vec![ChatMessage::new("user".to_string(), "newer".to_string())],
        );
        let writer = Arc::new(AMutex::new(DetachedTrajectoryWriterState {
            pending: Some((1, failed.clone())),
            requested_version: 1,
            in_flight: true,
            ..Default::default()
        }));
        let failure_seen = Arc::new(Notify::new());
        let release_failure = Arc::new(Notify::new());
        set_test_detached_trajectory_writer_failure_hook(Some((
            chat_id.to_string(),
            failure_seen.clone(),
            release_failure.clone(),
        )));
        set_test_message_serialization_failure(Some(failed.messages[0].message_id.clone()));
        let failure_seen_wait = failure_seen.notified();
        let task = tokio::spawn(run_detached_trajectory_writer(gcx.clone(), writer.clone()));

        tokio::time::timeout(Duration::from_secs(5), failure_seen_wait)
            .await
            .expect("failed write should pause before recording its outcome");
        set_test_message_serialization_failure(None);
        {
            let mut state = writer.lock().await;
            state.requested_version = 2;
            state.pending = Some((2, newer));
        }
        release_failure.notify_one();
        tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .expect("writer should drain the newer snapshot")
            .unwrap();
        set_test_detached_trajectory_writer_failure_hook(None);

        let state = writer.lock().await;
        assert_eq!(state.committed_version, 2);
        assert_eq!(state.error, None);
        assert!(!state.in_flight);
        drop(state);
        let loaded = load_trajectory_for_chat(gcx, chat_id).await.unwrap();
        assert_eq!(loaded.messages[0].content.content_text_only(), "newer");
    }

    #[serial(trajectory_perf)]
    #[tokio::test]
    async fn trajectory_writer_registry_cleans_completed_and_source_isolated_entries() {
        let _lock = serial_test_guard();
        let _writer = TrajectoryWriterRolloutGuard::set(true);
        let workspace = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(workspace.path()).await;
        let chat_id = "detached-writer-source-isolation";
        let normal = test_snapshot(
            chat_id,
            "Normal",
            vec![ChatMessage::new("user".to_string(), "normal".to_string())],
        );
        let mut buddy = test_snapshot(
            chat_id,
            "Buddy",
            vec![ChatMessage::new("user".to_string(), "buddy".to_string())],
        );
        buddy.mode = "buddy".to_string();
        buddy.buddy_meta = Some(buddy_thread_meta());
        let normal_key = detached_trajectory_writer_key(&gcx, &normal);
        let buddy_key = detached_trajectory_writer_key(&gcx, &buddy);

        let (normal_result, buddy_result) = tokio::join!(
            save_trajectory_snapshot(gcx.clone(), normal),
            save_trajectory_snapshot(gcx.clone(), buddy),
        );
        normal_result.unwrap();
        buddy_result.unwrap();
        assert_ne!(normal_key, buddy_key);
        prune_detached_trajectory_writers();
        let writers = detached_trajectory_writers()
            .lock()
            .expect("detached trajectory writer registry poisoned");
        assert!(!writers.contains_key(&normal_key));
        assert!(!writers.contains_key(&buddy_key));
        drop(writers);

        let normal = load_trajectory_for_chat(gcx.clone(), chat_id)
            .await
            .unwrap();
        assert_eq!(normal.thread.title, "Normal");
        let buddy_path = workspace
            .path()
            .join(".refact")
            .join("buddy")
            .join("chats")
            .join("conversations")
            .join(format!("{chat_id}.json"));
        let buddy: serde_json::Value =
            serde_json::from_str(&tokio::fs::read_to_string(buddy_path).await.unwrap()).unwrap();
        assert_eq!(buddy["title"], "Buddy");
    }

    #[tokio::test]
    async fn trajectory_writer_shutdown_flush_is_bounded_and_reports_blocked_session() {
        let workspace = tempfile::tempdir().unwrap();
        let (_gcx, app) = make_app_with_workspace(workspace.path()).await;
        let session_arc = Arc::new(AMutex::new(ChatSession::new("flush-timeout".to_string())));
        session_arc
            .lock()
            .await
            .add_message(ChatMessage::new("user".to_string(), "pending".to_string()));
        app.chat
            .sessions
            .write()
            .await
            .insert("flush-timeout".to_string(), session_arc.clone());

        let guard = session_arc.lock().await;
        let errors = flush_all_trajectories_with_timeout(app, Duration::from_millis(10))
            .await
            .unwrap_err();
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("timed out"));
        drop(guard);
    }

    fn assert_same_path(left: &Path, right: &Path) {
        assert_eq!(normalized_test_path(left), normalized_test_path(right));
    }

    fn assert_same_optional_path(left: Option<PathBuf>, right: &Path) {
        assert_same_path(&left.expect("path should exist"), right);
    }

    fn assert_same_path_str(left: &str, right: &Path) {
        assert_same_path(Path::new(left), right);
    }

    async fn write_conversation_file(dir: &Path, id: &str, age_secs: u64) -> PathBuf {
        let path = dir.join(format!("{id}.json"));
        tokio::fs::write(
            &path,
            serde_json::json!({
                "id": id,
                "chat_id": id,
                "title": id,
                "created_at": "2026-01-01T00:00:00Z",
                "messages": [{"role": "user", "content": "hi"}]
            })
            .to_string(),
        )
        .await
        .unwrap();
        let modified = SystemTime::now() - std::time::Duration::from_secs(age_secs);
        let file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        file.set_modified(modified).unwrap();
        path
    }

    #[tokio::test]
    async fn prune_buddy_conversations_keeps_newest_and_updates_index() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("conversations");
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let old_a = write_conversation_file(&dir, "old-a", 4 * 86_400).await;
        let old_b = write_conversation_file(&dir, "old-b", 3 * 86_400).await;
        let new_a = write_conversation_file(&dir, "new-a", 2000).await;
        let new_b = write_conversation_file(&dir, "new-b", 1000).await;
        let index_path = dir.join("index.json");
        let index_entries = ["old-a", "old-b", "new-a", "new-b"]
            .iter()
            .map(|id| {
                serde_json::json!({
                    "id": id,
                    "file_name": format!("{id}.json"),
                    "created_at": "2026-01-01T00:00:00Z",
                    "updated_at": "2026-01-01T00:00:00Z",
                    "title": id,
                    "model": "test-model",
                    "mode": "AGENT",
                    "message_count": 1
                })
            })
            .collect::<Vec<_>>();
        tokio::fs::write(
            &index_path,
            serde_json::json!({
                "schema_version": 1,
                "updated_at": "2026-01-01T00:00:00Z",
                "entries": index_entries
            })
            .to_string(),
        )
        .await
        .unwrap();

        let removed = prune_buddy_conversations_in_dir(&dir, 2).await.unwrap();

        assert_eq!(removed, 2);
        assert!(!old_a.exists());
        assert!(!old_b.exists());
        assert!(new_a.exists());
        assert!(new_b.exists());
        assert!(index_path.exists());
        let index = trajectory_index::read_trajectory_index(&dir)
            .await
            .unwrap()
            .unwrap();
        let ids: Vec<&str> = index
            .entries
            .iter()
            .map(|entry| entry.id.as_str())
            .collect();
        assert_eq!(ids.len(), 2);
        assert!(ids.contains(&"new-a"));
        assert!(ids.contains(&"new-b"));
    }

    #[tokio::test]
    async fn prune_buddy_conversations_spares_recently_modified_overflow() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("conversations");
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let newest = write_conversation_file(&dir, "newest", 100).await;
        let recent_overflow = write_conversation_file(&dir, "recent-overflow", 2 * 3600).await;
        let old_overflow = write_conversation_file(&dir, "old-overflow", 3 * 86_400).await;

        let removed = prune_buddy_conversations_in_dir(&dir, 1).await.unwrap();

        assert_eq!(removed, 1);
        assert!(newest.exists());
        assert!(recent_overflow.exists());
        assert!(!old_overflow.exists());
    }

    #[tokio::test]
    async fn prune_buddy_conversations_noop_under_keep_limit_and_for_missing_dir() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("conversations");
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let kept = write_conversation_file(&dir, "chat-a", 100).await;

        let removed = prune_buddy_conversations_in_dir(&dir, 2).await.unwrap();

        assert_eq!(removed, 0);
        assert!(kept.exists());

        let missing = prune_buddy_conversations_in_dir(&temp.path().join("nope"), 2)
            .await
            .unwrap();
        assert_eq!(missing, 0);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn prune_buddy_conversations_skips_symlinked_entries() {
        let temp = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let dir = temp.path().join("conversations");
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let real = write_conversation_file(&dir, "real", 3 * 86_400).await;
        let outside_file = write_conversation_file(outside.path(), "outside", 3 * 86_400).await;
        std::os::unix::fs::symlink(&outside_file, dir.join("linked.json")).unwrap();

        let removed = prune_buddy_conversations_in_dir(&dir, 0).await.unwrap();

        assert_eq!(removed, 1);
        assert!(!real.exists());
        assert!(outside_file.exists());
        assert!(dir.join("linked.json").exists());
    }

    fn run_git(cwd: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn init_repo(root: &Path) {
        run_git(root, &["init"]);
        run_git(root, &["checkout", "-b", "main"]);
        run_git(root, &["config", "core.autocrlf", "false"]);
        run_git(root, &["config", "user.email", "test@example.com"]);
        run_git(root, &["config", "user.name", "Test User"]);
        std::fs::write(root.join("file.txt"), "hello\n").unwrap();
        run_git(root, &["add", "."]);
        run_git(root, &["commit", "-m", "initial"]);
    }

    async fn make_app_with_workspace(root: &Path) -> (Arc<GlobalContext>, AppState) {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let app = AppState::from_gcx(gcx.clone()).await;
        *app.workspace
            .documents_state
            .workspace_folders
            .lock()
            .unwrap() = vec![normalized_test_path(root)];
        (gcx, app)
    }

    fn sample_trajectory(id: &str, title: &str, updated_at: &str) -> serde_json::Value {
        json!({
            "id": id,
            "title": title,
            "model": "model",
            "mode": "agent",
            "tool_use": "agent",
            "messages": [],
            "created_at": "2024-01-01T00:00:00Z",
            "updated_at": updated_at,
            "include_project_info": true,
            "checkpoints_enabled": true
        })
    }

    async fn write_trajectory_file(path: &Path, id: &str, title: &str, updated_at: &str) {
        tokio::fs::create_dir_all(path.parent().unwrap())
            .await
            .unwrap();
        tokio::fs::write(
            path,
            serde_json::to_string(&sample_trajectory(id, title, updated_at)).unwrap(),
        )
        .await
        .unwrap();
    }

    async fn write_trajectory_file_with_user_message(
        path: &Path,
        id: &str,
        title: &str,
        message: &str,
    ) {
        let mut trajectory = sample_trajectory(id, title, "2024-01-01T00:00:01Z");
        trajectory["messages"] = json!([{ "role": "user", "content": message }]);
        tokio::fs::create_dir_all(path.parent().unwrap())
            .await
            .unwrap();
        tokio::fs::write(path, serde_json::to_string(&trajectory).unwrap())
            .await
            .unwrap();
    }

    async fn write_trajectory_file_with_metadata(
        path: &Path,
        id: &str,
        title: &str,
        updated_at: &str,
        message: &str,
    ) {
        let mut trajectory = sample_trajectory(id, title, updated_at);
        trajectory["model"] = json!("fallback-model");
        trajectory["mode"] = json!("task_planner");
        trajectory["isTitleGenerated"] = json!(true);
        trajectory["parent_id"] = json!("parent-fallback");
        trajectory["link_type"] = json!("handoff");
        trajectory["root_chat_id"] = json!("root-fallback");
        trajectory["messages"] = json!([{ "role": "user", "content": message }]);
        tokio::fs::create_dir_all(path.parent().unwrap())
            .await
            .unwrap();
        tokio::fs::write(path, serde_json::to_string(&trajectory).unwrap())
            .await
            .unwrap();
    }

    async fn write_schema_incomplete_trajectory_file(path: &Path, id: &str) {
        tokio::fs::create_dir_all(path.parent().unwrap())
            .await
            .unwrap();
        tokio::fs::write(path, serde_json::to_string(&json!({"id": id})).unwrap())
            .await
            .unwrap();
    }

    async fn write_buddy_conversation_file(path: &Path, id: &str, title: &str) {
        tokio::fs::create_dir_all(path.parent().unwrap())
            .await
            .unwrap();
        tokio::fs::write(
            path,
            serde_json::to_string(&json!({
                "id": id,
                "chat_id": id,
                "title": title,
                "model": "model",
                "mode": "buddy",
                "tool_use": "agent",
                "created_at": "2024-01-01T00:00:00Z",
                "updated_at": "2024-01-01T00:00:00Z",
                "include_project_info": true,
                "checkpoints_enabled": false,
                "messages": [{"role":"user","content":"hello buddy"}],
                "buddy_meta": {"is_buddy_chat": true, "buddy_chat_kind": "investigation"}
            }))
            .unwrap(),
        )
        .await
        .unwrap();
    }

    fn task_meta(
        task_id: &str,
        role: &str,
        agent_id: Option<&str>,
        card_id: Option<&str>,
        planner_chat_id: Option<&str>,
    ) -> crate::chat::types::TaskMeta {
        crate::chat::types::TaskMeta {
            task_id: task_id.to_string(),
            role: role.to_string(),
            agent_id: agent_id.map(ToString::to_string),
            card_id: card_id.map(ToString::to_string),
            planner_chat_id: planner_chat_id.map(ToString::to_string),
        }
    }

    async fn write_task_trajectory_file_with_user_message(
        path: &Path,
        id: &str,
        title: &str,
        message: &str,
        task_meta: &crate::chat::types::TaskMeta,
    ) {
        let mut trajectory = sample_trajectory(id, title, "2024-01-01T00:00:01Z");
        trajectory["messages"] = json!([{ "role": "user", "content": message }]);
        trajectory["task_meta"] = serde_json::to_value(task_meta).unwrap();
        tokio::fs::create_dir_all(path.parent().unwrap())
            .await
            .unwrap();
        tokio::fs::write(path, serde_json::to_string(&trajectory).unwrap())
            .await
            .unwrap();
    }

    fn buddy_thread_meta() -> BuddyThreadMeta {
        BuddyThreadMeta {
            is_buddy_chat: true,
            buddy_chat_kind: "investigation".to_string(),
            workflow_id: None,
        }
    }

    async fn wait_for_watcher_start() {
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    }

    fn nested_trajectory_path(
        workspace: &std::path::Path,
        folder: &str,
        chat_id: &str,
    ) -> std::path::PathBuf {
        workspace
            .join(".refact")
            .join("trajectories")
            .join(folder)
            .join(format!("{chat_id}.json"))
    }

    #[tokio::test]
    async fn new_root_chat_saves_into_its_own_folder() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let chat_id = "layout-root-chat";

        save_trajectory_snapshot(
            gcx.clone(),
            test_snapshot(
                chat_id,
                "Root",
                vec![ChatMessage::new("user".to_string(), "hi".to_string())],
            ),
        )
        .await
        .unwrap();

        let nested = nested_trajectory_path(dir.path(), chat_id, chat_id);
        assert!(tokio::fs::try_exists(&nested).await.unwrap());
        let flat = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        assert!(!tokio::fs::try_exists(&flat).await.unwrap());
        assert!(load_trajectory_for_chat(gcx, chat_id).await.is_some());
    }

    #[serial(trajectory_perf)]
    #[tokio::test]
    async fn performance_diagnostics_cover_snapshot_save_boundaries_without_raw_id_or_path() {
        let _lock = serial_test_guard();
        let (_guard, sink, recorder) = install_perf_recorder();
        let dir = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;
        let chat_id = "performance-save-chat";
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        {
            let mut session = session_arc.lock().await;
            session.messages.push(ChatMessage::new(
                "user".to_string(),
                "diagnostic content remains private".to_string(),
            ));
            session.increment_version();
        }

        let snapshot = {
            let session = session_arc.lock().await;
            trajectory_snapshot_from_session(&session)
        };
        save_trajectory_snapshot(gcx, snapshot).await.unwrap();
        drop(app);

        let events = sink.events();
        let components: std::collections::HashSet<_> =
            events.iter().map(|event| event.component).collect();
        assert!(components.contains("trajectory.snapshot"));
        assert!(components.contains("trajectory.serialize"));
        assert!(components.contains("trajectory.atomic_write"));
        assert!(components.contains("trajectory.commit"));
        assert_eq!(
            events
                .iter()
                .filter(|event| event.component == "trajectory.serialize")
                .count(),
            1
        );
        assert_eq!(
            events
                .iter()
                .find(|event| event.component == "trajectory.serialize")
                .expect("serialize metric")
                .elapsed_us,
            2
        );
        assert!(events.iter().all(|event| event.outcome == "success"));
        assert!(events.iter().all(|event| {
            !format!("{event:?}").contains(chat_id)
                && !format!("{event:?}").contains(dir.path().to_str().unwrap())
        }));
        let expected_chat_hash = recorder.hash_identity_for_test(chat_id);
        assert!(events
            .iter()
            .any(|event| { event.chat_id_hash.as_deref() == Some(expected_chat_hash.as_str()) }));
    }

    #[serial(trajectory_perf)]
    #[tokio::test]
    async fn performance_diagnostics_record_failed_trajectory_commit() {
        let _lock = serial_test_guard();
        let (_guard, sink, recorder) = install_perf_recorder();
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let chat_id = "invalid/id";
        let result = save_trajectory_snapshot(gcx, test_snapshot(chat_id, "Bad", Vec::new())).await;

        assert!(result.is_err());
        let expected_chat_hash = recorder.hash_identity_for_test(chat_id);
        let events = sink
            .events()
            .into_iter()
            .filter(|event| {
                event.component == PerfComponent::TrajectoryCommit.as_str()
                    && event.outcome == PerfOutcome::Failure.as_str()
                    && event.chat_id_hash.as_deref() == Some(expected_chat_hash.as_str())
            })
            .collect::<Vec<_>>();
        assert_eq!(events.len(), 1);
    }

    #[serial(trajectory_perf)]
    #[tokio::test]
    async fn trajectory_metadata_path_failure_records_one_serialize_failure() {
        let _lock = serial_test_guard();
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let chat_id = "serialize-path-failure";
        let path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        write_trajectory_file(&path, "different-id", "Existing", "2024-01-01T00:00:00Z").await;
        let before = tokio::fs::read_to_string(&path).await.unwrap();
        let (_guard, sink, _) = install_perf_recorder();

        let result = save_trajectory_snapshot(
            gcx,
            test_snapshot(
                chat_id,
                "New",
                vec![ChatMessage::new("user".to_string(), "hi".to_string())],
            ),
        )
        .await;

        assert!(result.is_err());
        assert_eq!(tokio::fs::read_to_string(&path).await.unwrap(), before);
        let serialize_events: Vec<_> = sink
            .events()
            .into_iter()
            .filter(|event| event.component == "trajectory.serialize")
            .collect();
        assert_eq!(serialize_events.len(), 1);
        assert_eq!(serialize_events[0].outcome, "failure");
    }

    #[serial(trajectory_perf)]
    #[tokio::test]
    async fn trajectory_message_serialization_failure_preserves_existing_file() {
        let _lock = serial_test_guard();
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let chat_id = "serialize-message-failure";
        let path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        write_trajectory_file(&path, chat_id, "Durable", "2024-01-01T00:00:00Z").await;
        let before = tokio::fs::read_to_string(&path).await.unwrap();
        let (_guard, sink, _) = install_perf_recorder();
        let message = ChatMessage {
            message_id: "unserializable-message".to_string(),
            role: "assistant".to_string(),
            content: ChatContent::SimpleText("private message content".to_string()),
            ..Default::default()
        };
        set_test_message_serialization_failure(Some(message.message_id.clone()));

        let error = save_trajectory_snapshot(gcx, test_snapshot(chat_id, "New", vec![message]))
            .await
            .unwrap_err();
        set_test_message_serialization_failure(None);

        assert!(error.contains("index 0"));
        assert!(error.contains("unserializable-message"));
        assert!(error.contains("assistant"));
        assert!(!error.contains("private message content"));
        assert_eq!(tokio::fs::read_to_string(&path).await.unwrap(), before);
        let serialize_events: Vec<_> = sink
            .events()
            .into_iter()
            .filter(|event| event.component == "trajectory.serialize")
            .collect();
        assert_eq!(serialize_events.len(), 1);
        assert_eq!(serialize_events[0].outcome, "failure");
    }

    #[tokio::test]
    async fn trajectory_commit_round_trips_every_message() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let chat_id = "serialize-all-messages";
        let messages = vec![
            ChatMessage {
                message_id: "message-user".to_string(),
                role: "user".to_string(),
                content: ChatContent::SimpleText("question".to_string()),
                ..Default::default()
            },
            ChatMessage {
                message_id: "message-assistant".to_string(),
                role: "assistant".to_string(),
                content: ChatContent::SimpleText("answer".to_string()),
                reasoning_content: Some("reasoning".to_string()),
                tool_calls: Some(vec![crate::call_validation::ChatToolCall {
                    id: "call-1".to_string(),
                    index: Some(0),
                    function: crate::call_validation::ChatToolFunction {
                        name: "sleep".to_string(),
                        arguments: "{}".to_string(),
                    },
                    tool_type: "function".to_string(),
                    extra_content: None,
                    started_at_ms: Some(1_700_000_000_000),
                    completed_at_ms: Some(1_700_000_008_000),
                }]),
                ..Default::default()
            },
            ChatMessage {
                message_id: "message-tool".to_string(),
                role: "tool".to_string(),
                content: ChatContent::SimpleText("result".to_string()),
                tool_call_id: "call-1".to_string(),
                ..Default::default()
            },
        ];
        let expected = serde_json::to_value(&messages).unwrap();

        save_trajectory_snapshot(gcx.clone(), test_snapshot(chat_id, "All", messages))
            .await
            .unwrap();

        let path = nested_trajectory_path(dir.path(), chat_id, chat_id);
        let saved: serde_json::Value =
            serde_json::from_str(&tokio::fs::read_to_string(path).await.unwrap()).unwrap();
        assert_eq!(saved["messages"], expected);
        let loaded = load_trajectory_for_chat(gcx, chat_id).await.unwrap();
        let tool_call = loaded.messages[1]
            .tool_calls
            .as_ref()
            .unwrap()
            .first()
            .unwrap();
        assert_eq!(tool_call.started_at_ms, Some(1_700_000_000_000));
        assert_eq!(tool_call.completed_at_ms, Some(1_700_000_008_000));
    }

    #[tokio::test]
    async fn subchat_saves_into_root_chat_folder() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let root_id = "layout-parent-chat";
        let child_id = "subchat-layout-child";

        let mut snapshot = test_snapshot(
            child_id,
            "Child",
            vec![ChatMessage::new("user".to_string(), "hi".to_string())],
        );
        snapshot.root_chat_id = Some(root_id.to_string());
        snapshot.parent_id = Some(root_id.to_string());
        snapshot.link_type = Some("subagent".to_string());
        save_trajectory_snapshot(gcx.clone(), snapshot)
            .await
            .unwrap();

        let nested = nested_trajectory_path(dir.path(), root_id, child_id);
        assert!(
            tokio::fs::try_exists(&nested).await.unwrap(),
            "child must live in the root chat folder"
        );
        assert!(load_trajectory_for_chat(gcx, child_id).await.is_some());
    }

    #[tokio::test]
    async fn resaving_a_trajectory_keeps_the_original_created_at() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let chat_id = "created-at-stable-on-resave";

        let mut first = test_snapshot(
            chat_id,
            "First",
            vec![ChatMessage::new("user".to_string(), "hi".to_string())],
        );
        first.created_at = "2024-01-01T00:00:00Z".to_string();
        save_trajectory_snapshot(gcx.clone(), first).await.unwrap();
        let path = nested_trajectory_path(dir.path(), chat_id, chat_id);
        let first_raw: serde_json::Value =
            serde_json::from_str(&tokio::fs::read_to_string(&path).await.unwrap()).unwrap();
        let first_created_at = first_raw["created_at"].as_str().unwrap().to_string();
        let first_updated_at = first_raw["updated_at"].as_str().unwrap().to_string();

        let mut second = test_snapshot(
            chat_id,
            "Second",
            vec![
                ChatMessage::new("user".to_string(), "hi".to_string()),
                ChatMessage::new("assistant".to_string(), "there".to_string()),
            ],
        );
        second.created_at = "2025-06-01T00:00:00Z".to_string();
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        save_trajectory_snapshot(gcx, second).await.unwrap();
        let second_raw: serde_json::Value =
            serde_json::from_str(&tokio::fs::read_to_string(&path).await.unwrap()).unwrap();

        assert_eq!(first_created_at, "2024-01-01T00:00:00Z");
        assert_eq!(second_raw["created_at"].as_str().unwrap(), first_created_at);
        assert_ne!(second_raw["updated_at"].as_str().unwrap(), first_updated_at);
    }

    #[tokio::test]
    async fn resaving_child_without_root_id_updates_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let root_id = "layout-resave-root";
        let child_id = "subchat-layout-resave-child";

        let mut first = test_snapshot(
            child_id,
            "Child",
            vec![ChatMessage::new("user".to_string(), "hi".to_string())],
        );
        first.root_chat_id = Some(root_id.to_string());
        first.parent_id = Some(root_id.to_string());
        first.link_type = Some("subagent".to_string());
        save_trajectory_snapshot(gcx.clone(), first).await.unwrap();

        let mut second = test_snapshot(
            child_id,
            "Child",
            vec![
                ChatMessage::new("user".to_string(), "hi".to_string()),
                ChatMessage::new("assistant".to_string(), "there".to_string()),
            ],
        );
        second.root_chat_id = None;
        second.parent_id = None;
        save_trajectory_snapshot(gcx.clone(), second).await.unwrap();

        let nested = nested_trajectory_path(dir.path(), root_id, child_id);
        let duplicate = nested_trajectory_path(dir.path(), child_id, child_id);
        assert!(tokio::fs::try_exists(&nested).await.unwrap());
        assert!(
            !tokio::fs::try_exists(&duplicate).await.unwrap(),
            "a re-save without root_chat_id must update in place, not fork a duplicate file"
        );

        let loaded = load_trajectory_for_chat(gcx, child_id)
            .await
            .expect("child must still resolve");
        assert_eq!(loaded.messages.len(), 2);
        assert_same_path(&loaded.source_path, &nested);
    }

    #[tokio::test]
    async fn legacy_flat_trajectory_keeps_being_written_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let chat_id = "layout-legacy-chat";
        let flat = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        write_trajectory_file(&flat, chat_id, "Legacy", "2024-01-01T00:00:00Z").await;

        save_trajectory_snapshot(
            gcx.clone(),
            test_snapshot(
                chat_id,
                "Legacy Updated",
                vec![ChatMessage::new("user".to_string(), "hi".to_string())],
            ),
        )
        .await
        .unwrap();

        let saved: serde_json::Value =
            serde_json::from_str(&tokio::fs::read_to_string(&flat).await.unwrap()).unwrap();
        assert_eq!(saved["title"], "Legacy Updated");
        let nested = nested_trajectory_path(dir.path(), chat_id, chat_id);
        assert!(
            !tokio::fs::try_exists(&nested).await.unwrap(),
            "legacy chats must not be duplicated into a folder"
        );
    }

    #[tokio::test]
    async fn nested_trajectory_is_indexed_and_resolvable_through_the_index() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let chat_id = "layout-indexed-chat";

        save_trajectory_snapshot(
            gcx.clone(),
            test_snapshot(
                chat_id,
                "Indexed",
                vec![ChatMessage::new("user".to_string(), "hi".to_string())],
            ),
        )
        .await
        .unwrap();

        AppState::from_gcx(gcx.clone())
            .await
            .chat
            .trajectory_index_coordinator
            .flush_all()
            .await
            .unwrap();

        let root = dir.path().join(".refact").join("trajectories");
        assert!(
            !tokio::fs::try_exists(root.join(chat_id).join("index.json"))
                .await
                .unwrap(),
            "index.json must not be created inside a chat folder"
        );

        let index = trajectory_index::read_trajectory_index(&root)
            .await
            .unwrap()
            .expect("root index should exist");
        let entry = index
            .entries
            .iter()
            .find(|entry| entry.id == chat_id)
            .expect("nested chat must be indexed");
        assert_eq!(entry.file_name, format!("{chat_id}/{chat_id}.json"));
        assert!(
            trajectory_index::trajectory_index_entry_is_fresh(&root, entry).await,
            "a nested index entry must not be treated as stale"
        );
        assert_eq!(
            trajectory_index::trajectory_file_path_for_entry(&root, entry),
            nested_trajectory_path(dir.path(), chat_id, chat_id)
        );
    }

    #[tokio::test]
    async fn id_mismatched_legacy_flat_slot_refuses_the_save() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let chat_id = "layout-squatted-chat";

        let root = dir.path().join(".refact").join("trajectories");
        tokio::fs::create_dir_all(&root).await.unwrap();
        tokio::fs::write(
            root.join(format!("{chat_id}.json")),
            serde_json::to_string(&serde_json::json!({
                "id": "some-other-chat",
                "title": "Squatter",
                "created_at": "2026-01-01T00:00:00Z",
                "messages": [],
            }))
            .unwrap(),
        )
        .await
        .unwrap();

        let result = save_trajectory_snapshot(
            gcx.clone(),
            test_snapshot(
                chat_id,
                "New",
                vec![ChatMessage::new("user".to_string(), "hi".to_string())],
            ),
        )
        .await;

        assert!(
            result.is_err(),
            "an id-mismatched legacy flat slot must refuse the save, not write elsewhere"
        );
        assert!(
            !tokio::fs::try_exists(nested_trajectory_path(dir.path(), chat_id, chat_id))
                .await
                .unwrap(),
            "refusal must not leave a nested copy behind"
        );
    }

    #[tokio::test]
    async fn internal_traces_are_not_written_into_the_index() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let root_id = "layout-trace-owner";
        let trace_id = "subchat-layout-trace";

        save_trajectory_snapshot(
            gcx.clone(),
            test_snapshot(
                root_id,
                "Owner",
                vec![ChatMessage::new("user".to_string(), "hi".to_string())],
            ),
        )
        .await
        .unwrap();

        let mut trace = test_snapshot(
            trace_id,
            "Trace",
            vec![ChatMessage::new("user".to_string(), "hi".to_string())],
        );
        trace.root_chat_id = Some(root_id.to_string());
        trace.link_type = Some(internal_trace_link_type("title_generation"));
        save_trajectory_snapshot(gcx.clone(), trace).await.unwrap();

        AppState::from_gcx(gcx.clone())
            .await
            .chat
            .trajectory_index_coordinator
            .flush_all()
            .await
            .unwrap();

        assert!(
            tokio::fs::try_exists(nested_trajectory_path(dir.path(), root_id, trace_id))
                .await
                .unwrap(),
            "the trace must still be written into the owning conversation folder"
        );

        let root = dir.path().join(".refact").join("trajectories");
        let index = trajectory_index::read_trajectory_index(&root)
            .await
            .unwrap()
            .expect("root index should exist");
        assert!(
            index.entries.iter().any(|entry| entry.id == root_id),
            "the owning chat stays indexed"
        );
        assert!(
            !index.entries.iter().any(|entry| entry.id == trace_id),
            "internal traces must not grow the index"
        );
    }

    #[test]
    fn internal_traces_are_not_enqueued_for_vectorization() {
        assert!(!should_vectorize_trajectory(
            &TrajectorySourceIdentity::Normal,
            Some("internal:title_generation")
        ));
        assert!(!should_vectorize_trajectory(
            &TrajectorySourceIdentity::Normal,
            Some("internal:commit_message")
        ));
        assert!(should_vectorize_trajectory(
            &TrajectorySourceIdentity::Normal,
            Some("subagent")
        ));
        assert!(should_vectorize_trajectory(
            &TrajectorySourceIdentity::Normal,
            None
        ));
    }

    fn write_temp_json(dir: &tempfile::TempDir, name: &str, contents: &str) -> PathBuf {
        let path = dir.path().join(name);
        std::fs::write(&path, contents).unwrap();
        path
    }

    #[test]
    fn top_level_link_type_classification_preserves_semantics() {
        let temp = tempfile::tempdir().unwrap();

        // Happy path: a top-level internal link_type is classified as an internal trace.
        assert!(parsed_top_level_link_type_is_internal(write_temp_json(
            &temp,
            "internal.json",
            r#"{"id":"a","link_type":"internal:title_generation","messages":[]}"#,
        )));

        // A link_type that is not the internal one.
        assert!(!parsed_top_level_link_type_is_internal(write_temp_json(
            &temp,
            "subagent.json",
            r#"{"id":"a","link_type":"subagent","messages":[]}"#,
        )));

        // No link_type field at all.
        assert!(!parsed_top_level_link_type_is_internal(write_temp_json(
            &temp,
            "missing.json",
            r#"{"id":"a","messages":[]}"#,
        )));

        // A null link_type is treated as absent, not internal.
        assert!(!parsed_top_level_link_type_is_internal(write_temp_json(
            &temp,
            "null.json",
            r#"{"id":"a","link_type":null}"#,
        )));

        // Not valid JSON.
        assert!(!parsed_top_level_link_type_is_internal(write_temp_json(
            &temp,
            "corrupt.json",
            "{not json at all",
        )));

        // A truncated document whose link_type *is* internal must still be rejected, so that a
        // torn write is never mistaken for a complete internal trace and pruned.
        assert!(!parsed_top_level_link_type_is_internal(write_temp_json(
            &temp,
            "truncated.json",
            r#"{"link_type":"internal:broken""#,
        )));

        // Valid JSON whose top level is not a map.
        assert!(!parsed_top_level_link_type_is_internal(write_temp_json(
            &temp,
            "array.json",
            r#"["link_type","internal:title_generation"]"#,
        )));
        assert!(!parsed_top_level_link_type_is_internal(write_temp_json(
            &temp,
            "scalar.json",
            r#""internal:title_generation""#,
        )));

        // A nested link_type must not be read as a top-level one.
        assert!(!parsed_top_level_link_type_is_internal(write_temp_json(
            &temp,
            "nested.json",
            r#"{"messages":[{"role":"user","content":"\"link_type\": \"internal:spoof\""}]}"#,
        )));

        // A missing file, and a directory, are both "not an internal trace".
        assert!(!parsed_top_level_link_type_is_internal(
            temp.path().join("does-not-exist.json")
        ));
        assert!(!parsed_top_level_link_type_is_internal(
            temp.path().to_path_buf()
        ));
    }

    #[test]
    fn top_level_link_type_stops_reading_once_a_non_internal_value_is_known() {
        struct CountingReader<R> {
            inner: R,
            bytes_read: Arc<AtomicUsize>,
        }

        impl<R: std::io::Read> std::io::Read for CountingReader<R> {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                let read = self.inner.read(buf)?;
                self.bytes_read.fetch_add(read, Ordering::Relaxed);
                Ok(read)
            }
        }

        // `link_type` comes first, followed by a megabyte of payload that the classifier has no
        // reason to parse once the answer is already known.
        let trailing = "x".repeat(1024 * 1024);
        let document = format!(r#"{{"link_type":"subagent","messages":"{trailing}"}}"#);

        let bytes_read = Arc::new(AtomicUsize::new(0));
        let reader = CountingReader {
            inner: std::io::Cursor::new(document.clone().into_bytes()),
            bytes_read: bytes_read.clone(),
        };

        assert!(!top_level_link_type_is_internal_from_reader(reader));

        let bytes_read = bytes_read.load(Ordering::Relaxed);
        assert!(
            bytes_read < 64 * 1024,
            "expected an early exit after link_type, but read {bytes_read} of {} bytes",
            document.len()
        );
    }

    #[test]
    fn top_level_link_type_reads_are_buffered() {
        struct ChunkCountingReader<R> {
            inner: R,
            reads: Arc<AtomicUsize>,
        }

        impl<R: std::io::Read> std::io::Read for ChunkCountingReader<R> {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                self.reads.fetch_add(1, Ordering::Relaxed);
                self.inner.read(buf)
            }
        }

        // An internal trace deliberately does *not* early-exit, so the whole document is consumed.
        // Unbuffered, serde_json's `IoRead` would issue roughly one `read()` call per byte.
        let padding = "y".repeat(256 * 1024);
        let document = format!(r#"{{"messages":"{padding}","link_type":"internal:test"}}"#);

        let reads = Arc::new(AtomicUsize::new(0));
        let reader = ChunkCountingReader {
            inner: std::io::Cursor::new(document.clone().into_bytes()),
            reads: reads.clone(),
        };

        assert!(top_level_link_type_is_internal_from_reader(reader));

        let reads = reads.load(Ordering::Relaxed);
        assert!(
            reads < document.len() / 100,
            "expected buffered reads, but made {reads} read() calls for {} bytes",
            document.len()
        );
    }

    #[tokio::test]
    async fn internal_trace_pruning_keeps_newest_and_preserves_displayable_files() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path();
        let displayable = dir.join("displayable.json");
        tokio::fs::write(
            &displayable,
            serde_json::json!({"id":"displayable","link_type":"subagent"}).to_string(),
        )
        .await
        .unwrap();
        for index in 0..4 {
            let path = dir.join(format!("internal-{index}.json"));
            let payload = if index == 0 {
                format!(
                    "{{\"id\":\"internal-0\",\"messages\":[{{\"role\":\"user\",\"content\":\"{}\"}}],\"link_type\":\"internal:title_generation\"}}",
                    "x".repeat((64 * 1024) + 1024)
                )
            } else {
                serde_json::json!({
                    "id": format!("internal-{index}"),
                    "messages": [{"role":"user","content":"trace"}],
                    "link_type": "internal:title_generation"
                })
                .to_string()
            };
            tokio::fs::write(&path, payload).await.unwrap();
            let modified = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(index + 1);
            std::fs::File::options()
                .write(true)
                .open(&path)
                .unwrap()
                .set_modified(modified)
                .unwrap();
        }

        let removed = prune_internal_traces_in_folder(dir, 2).await.unwrap();

        assert_eq!(removed, 2);
        assert!(displayable.exists());
        assert!(!dir.join("internal-0.json").exists());
        assert!(!dir.join("internal-1.json").exists());
        assert!(dir.join("internal-2.json").exists());
        assert!(dir.join("internal-3.json").exists());
    }

    #[tokio::test]
    async fn internal_trace_pruning_ignores_message_content_and_invalid_json() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path();
        let user_trajectory = dir.join("user.json");
        let corrupt = dir.join("corrupt.json");
        let truncated = dir.join("truncated.json");
        let old_trace = dir.join("old-trace.json");
        let new_trace = dir.join("new-trace.json");

        tokio::fs::write(
            &user_trajectory,
            serde_json::json!({
                "id": "user",
                "messages": [{"role": "user", "content": "\"link_type\": \"internal:spoof\""}]
            })
            .to_string(),
        )
        .await
        .unwrap();
        tokio::fs::write(&corrupt, "{\"link_type\":\"internal:broken\"")
            .await
            .unwrap();
        tokio::fs::write(&truncated, "{\"messages\":[")
            .await
            .unwrap();
        for (path, id, modified_secs) in
            [(&old_trace, "old-trace", 1), (&new_trace, "new-trace", 2)]
        {
            tokio::fs::write(
                path,
                serde_json::json!({
                    "id": id,
                    "link_type": "internal:title_generation",
                    "messages": []
                })
                .to_string(),
            )
            .await
            .unwrap();
            std::fs::File::options()
                .write(true)
                .open(path)
                .unwrap()
                .set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(modified_secs))
                .unwrap();
        }

        let removed = prune_internal_traces_in_folder(dir, 1).await.unwrap();

        assert_eq!(removed, 1);
        assert!(user_trajectory.exists());
        assert!(corrupt.exists());
        assert!(truncated.exists());
        assert!(!old_trace.exists());
        assert!(new_trace.exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn internal_trace_pruning_skips_symlinked_entries() {
        let temp = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let dir = temp.path();
        let outside_trace = outside.path().join("outside.json");
        let real_trace = dir.join("real.json");
        tokio::fs::write(
            &outside_trace,
            serde_json::json!({"id": "outside", "link_type": "internal:test"}).to_string(),
        )
        .await
        .unwrap();
        tokio::fs::write(
            &real_trace,
            serde_json::json!({"id": "real", "link_type": "internal:test"}).to_string(),
        )
        .await
        .unwrap();
        std::os::unix::fs::symlink(&outside_trace, dir.join("linked.json")).unwrap();

        let removed = prune_internal_traces_in_folder(dir, 0).await.unwrap();

        assert_eq!(removed, 1);
        assert!(!real_trace.exists());
        assert!(outside_trace.exists());
        assert!(dir.join("linked.json").exists());
    }

    fn assert_prune_throttle_is_bounded_and_isolates_directories() {
        let mut throttle = PruneThrottle::default();
        let left = PathBuf::from("/tmp/left");
        let right = PathBuf::from("/tmp/right");

        assert!(throttle.claim(&left, 1, 3600));
        assert!(throttle.claim(&right, 1, 3600));
        assert!(!throttle.claim(&left, 1, 3600));

        for index in 0..PRUNE_THROTTLE_MAX_ENTRIES {
            assert!(throttle.claim(&PathBuf::from(format!("/tmp/prune-{index}")), 1, 3600));
        }

        assert!(throttle.entries.len() <= PRUNE_THROTTLE_MAX_ENTRIES);
        assert!(throttle.claim(&right, 3601, 3600));
    }

    #[test]
    fn internal_trace_prune_throttle_is_bounded_and_isolates_directories() {
        assert_prune_throttle_is_bounded_and_isolates_directories();
    }

    #[test]
    fn buddy_conversations_prune_throttle_is_bounded_and_isolates_directories() {
        assert_prune_throttle_is_bounded_and_isolates_directories();
    }

    #[test]
    fn index_entry_file_names_accept_one_folder_level_only() {
        assert!(trajectory_index::index_entry_file_name_is_valid("a.json"));
        assert!(trajectory_index::index_entry_file_name_is_valid(
            "root/a.json"
        ));
        assert!(!trajectory_index::index_entry_file_name_is_valid(
            "a/b/c.json"
        ));
        assert!(!trajectory_index::index_entry_file_name_is_valid(
            "../a.json"
        ));
        assert!(!trajectory_index::index_entry_file_name_is_valid(
            "root/index.json"
        ));
        assert!(!trajectory_index::index_entry_file_name_is_valid("a.txt"));
        assert!(!trajectory_index::index_entry_file_name_is_valid(
            "C:/evil.json"
        ));
        assert!(!trajectory_index::index_entry_file_name_is_valid(
            "/etc/evil.json"
        ));
    }

    #[test]
    fn internal_trace_events_are_not_displayable_chats() {
        let event_with_link_type = |link_type: Option<String>| -> TrajectoryEvent {
            let mut value = serde_json::json!({
                "type": "trajectory_updated",
                "id": "trace-chat",
            });
            if let Some(link_type) = link_type {
                value["link_type"] = serde_json::Value::String(link_type);
            }
            serde_json::from_value(value).unwrap()
        };

        assert!(!trajectory_event_is_displayable_chat(
            &event_with_link_type(Some(internal_trace_link_type("title_generation")))
        ));
        assert!(trajectory_event_is_displayable_chat(&event_with_link_type(
            None
        )));
    }

    #[tokio::test]
    async fn task_agent_index_stays_in_its_own_agent_folder() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let agent_dir = dir
            .path()
            .join(".refact")
            .join("tasks")
            .join("task-index-scope")
            .join("trajectories")
            .join("agents")
            .join("agent-7");
        tokio::fs::create_dir_all(&agent_dir).await.unwrap();

        let resolved = index_dir_for_trajectory_file(gcx, &agent_dir.join("agent-chat.json"))
            .await
            .expect("index dir must resolve");

        assert_eq!(
            resolved, agent_dir,
            "a task agent index must not be relocated to the shared agents/ folder"
        );
    }

    #[tokio::test]
    async fn find_trajectory_parent_id_reports_root_child_and_unknown() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;

        save_trajectory_snapshot(
            gcx.clone(),
            test_snapshot(
                "root-parent-id-chat",
                "Root",
                vec![ChatMessage::new("user".to_string(), "root".to_string())],
            ),
        )
        .await
        .unwrap();

        let mut child = test_snapshot(
            "child-parent-id-chat",
            "Child",
            vec![ChatMessage::new("user".to_string(), "child".to_string())],
        );
        child.parent_id = Some("root-parent-id-chat".to_string());
        save_trajectory_snapshot(gcx.clone(), child).await.unwrap();

        assert_eq!(
            find_trajectory_parent_id(gcx.clone(), "root-parent-id-chat").await,
            Some(None)
        );
        assert_eq!(
            find_trajectory_parent_id(gcx.clone(), "child-parent-id-chat").await,
            Some(Some("root-parent-id-chat".to_string()))
        );
        assert_eq!(
            find_trajectory_parent_id(gcx, "missing-parent-id-chat").await,
            None
        );
    }

    #[tokio::test]
    async fn remove_stale_entries_targets_only_the_owning_index_directory() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;

        let root_dir = dir.path().join(".refact").join("trajectories");
        let task_dir = dir
            .path()
            .join(".refact")
            .join("tasks")
            .join("task-1")
            .join("trajectories");
        tokio::fs::create_dir_all(&root_dir).await.unwrap();
        tokio::fs::create_dir_all(&task_dir).await.unwrap();

        let stale_id = "scoped-stale-entry-chat";
        let stale_path = root_dir.join(format!("{stale_id}.json"));
        let stale_value = serde_json::json!({
            "id": stale_id,
            "title": "Stale",
            "created_at": "2026-01-01T00:00:00Z",
            "updated_at": "2026-01-01T00:00:01Z",
            "model": "test-model",
            "mode": "agent",
            "tool_use": "agent",
            "messages": []
        });
        tokio::fs::write(&stale_path, stale_value.to_string())
            .await
            .unwrap();
        trajectory_index::upsert_trajectory_index_entry_from_owned_value_with_rollout(
            &app.chat.trajectory_index_coordinator,
            &root_dir,
            &stale_path,
            stale_value,
            None,
        )
        .await
        .unwrap();
        tokio::fs::remove_file(&stale_path).await.unwrap();

        let fresh_id = "other-dir-chat";
        let fresh_path = task_dir.join(format!("{fresh_id}.json"));
        let fresh_value = serde_json::json!({
            "id": fresh_id,
            "title": "Fresh",
            "created_at": "2026-01-01T00:00:00Z",
            "updated_at": "2026-01-01T00:00:01Z",
            "model": "test-model",
            "mode": "agent",
            "tool_use": "agent",
            "messages": []
        });
        tokio::fs::write(&fresh_path, fresh_value.to_string())
            .await
            .unwrap();
        trajectory_index::upsert_trajectory_index_entry_from_owned_value_with_rollout(
            &app.chat.trajectory_index_coordinator,
            &task_dir,
            &fresh_path,
            fresh_value,
            None,
        )
        .await
        .unwrap();

        remove_stale_trajectory_index_entries(gcx.clone(), stale_id).await;

        let root_entries = trajectory_index::list_trajectory_entries_with_rollout_for(
            &app.chat.trajectory_index_coordinator,
            &root_dir,
            None,
            trajectory_index::TrajectoryIndexListingCaller::StaleEntryCleanup,
        )
        .await
        .unwrap();
        let task_entries = trajectory_index::list_trajectory_entries_with_rollout_for(
            &app.chat.trajectory_index_coordinator,
            &task_dir,
            None,
            trajectory_index::TrajectoryIndexListingCaller::StaleEntryCleanup,
        )
        .await
        .unwrap();

        assert!(root_entries.iter().all(|entry| entry.id != stale_id));
        assert_eq!(task_entries.len(), 1);
        assert_eq!(task_entries[0].id, fresh_id);
    }

    // Clears the process-wide metadata cache, so it must not run beside the tests that read it.
    #[serial(trajectory_perf)]
    #[test]
    fn trajectory_metadata_cache_lru_evicts_oldest_path() {
        let mut cache = trajectory_metadata_cache()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        cache.clear();
        drop(cache);

        for index in 0..MAX_CACHED_TRAJECTORY_METADATA {
            let path = PathBuf::from(format!("/tmp/trajectory-meta-{index}.json"));
            store_trajectory_metadata_cache_entry(
                &path,
                TrajectoryMetadataCacheEntry {
                    chat_id: format!("chat-{index}"),
                    metadata: serde_json::Map::new(),
                    stamp: TrajectoryFileStamp {
                        len: index as u64,
                        modified_unix_ms: 0,
                    },
                    last_access: 0,
                },
            );
        }

        let oldest = trajectory_metadata_cache_key(&PathBuf::from("/tmp/trajectory-meta-0.json"));
        let newest = trajectory_metadata_cache_key(&PathBuf::from(format!(
            "/tmp/trajectory-meta-{}.json",
            MAX_CACHED_TRAJECTORY_METADATA - 1
        )));
        let extra = PathBuf::from("/tmp/trajectory-meta-extra.json");
        store_trajectory_metadata_cache_entry(
            &extra,
            TrajectoryMetadataCacheEntry {
                chat_id: "chat-extra".to_string(),
                metadata: serde_json::Map::new(),
                stamp: TrajectoryFileStamp {
                    len: 999,
                    modified_unix_ms: 0,
                },
                last_access: 0,
            },
        );

        let extra = trajectory_metadata_cache_key(&extra);
        let cache = trajectory_metadata_cache()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        assert!(!cache.contains_key(&oldest));
        assert!(cache.contains_key(&newest));
        assert!(cache.contains_key(&extra));
    }

    #[tokio::test]
    async fn start_trajectory_watcher_is_idempotent_while_running() {
        reset_test_trajectory_watcher_task();
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;

        start_trajectory_watcher(gcx.clone());
        start_trajectory_watcher(gcx);

        let guard = TRAJECTORY_WATCHER_TASK
            .get_or_init(|| StdMutex::new(None))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        assert!(guard.as_ref().is_some_and(|handle| !handle.is_finished()));

        drop(guard);
        reset_test_trajectory_watcher_task();
    }

    #[test]
    fn internal_traces_are_not_displayable_chats() {
        let internal: TrajectoryListData = serde_json::from_value(serde_json::json!({
            "id": "internal-1",
            "updated_at": "2024-01-01T00:00:00Z",
            "mode": "agent",
            "link_type": "internal:title_generation"
        }))
        .unwrap();
        assert!(!trajectory_list_data_is_displayable_chat(&internal));

        let plain: TrajectoryListData = serde_json::from_value(serde_json::json!({
            "id": "plain-1",
            "updated_at": "2024-01-01T00:00:00Z",
            "mode": "agent"
        }))
        .unwrap();
        assert!(trajectory_list_data_is_displayable_chat(&plain));
    }

    fn test_snapshot(chat_id: &str, title: &str, messages: Vec<ChatMessage>) -> TrajectorySnapshot {
        TrajectorySnapshot {
            goal: None,
            goal_ledger: Vec::new(),
            pending_deliveries: Vec::new(),
            goal_verification_blocked_until_ms: None,
            chat_id: chat_id.to_string(),
            title: title.to_string(),
            model: "model".to_string(),
            mode: "agent".to_string(),
            tool_use: "agent".to_string(),
            messages,
            created_at: "2024-01-01T00:00:00Z".to_string(),
            boost_reasoning: false,
            checkpoints_enabled: true,
            context_tokens_cap: None,
            auto_compression_cap: None,
            auto_compression_cap_pending: false,
            include_project_info: true,
            is_title_generated: true,
            auto_approve_editing_tools: false,
            auto_approve_dangerous_commands: false,
            autonomous_no_confirm: false,
            version: 1,
            task_meta: None,
            worktree: None,
            parent_id: None,
            link_type: None,
            root_chat_id: None,
            reasoning_effort: None,
            thinking_budget: None,
            temperature: None,
            frequency_penalty: None,
            max_tokens: None,
            parallel_tool_calls: None,
            previous_response_id: None,
            active_skill: None,
            auto_enrichment_enabled: None,
            buddy_meta: None,
            auto_compact_enabled: None,
            frozen_request_prefix: None,
            claude_code_identity: None,
            reactive_compact_attempts: None,
            wake_up_at: None,
            waiting_for_card_ids: Vec::new(),
        }
    }

    async fn wait_for_trajectory_event(
        rx: &mut broadcast::Receiver<TrajectoryEvent>,
        id: &str,
    ) -> TrajectoryEvent {
        // Generous timeout: file-watcher notify events can be delayed under
        // heavy parallel test load (notify backend + tokio scheduler contention),
        // especially on macOS CI where FSEvents delivery can lag under cargo test load.
        tokio::time::timeout(std::time::Duration::from_secs(60), async {
            loop {
                match rx.recv().await {
                    Ok(event) if event.id == id => return event,
                    Ok(_) => continue,
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(err) => panic!("trajectory event channel closed: {}", err),
                }
            }
        })
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for trajectory event {id}"))
    }

    async fn drain_trajectory_events(rx: &mut broadcast::Receiver<TrajectoryEvent>) {
        loop {
            match tokio::time::timeout(std::time::Duration::from_millis(50), rx.recv()).await {
                Ok(Ok(_)) | Ok(Err(broadcast::error::RecvError::Lagged(_))) => continue,
                Ok(Err(broadcast::error::RecvError::Closed)) | Err(_) => break,
            }
        }
    }

    async fn assert_no_trajectory_event_for(
        rx: &mut broadcast::Receiver<TrajectoryEvent>,
        duration: std::time::Duration,
    ) {
        match tokio::time::timeout(duration, rx.recv()).await {
            Err(_) | Ok(Err(broadcast::error::RecvError::Closed)) => {}
            Ok(Ok(event)) => panic!("unexpected trajectory event: {:?}", event),
            Ok(Err(broadcast::error::RecvError::Lagged(skipped))) => {
                panic!("unexpected trajectory event lag, skipped {skipped}")
            }
        }
    }

    async fn assert_no_chat_event_for(
        rx: &mut broadcast::Receiver<Arc<String>>,
        duration: std::time::Duration,
    ) {
        match tokio::time::timeout(duration, rx.recv()).await {
            Err(_) | Ok(Err(broadcast::error::RecvError::Closed)) => {}
            Ok(Ok(event)) => panic!("unexpected chat event: {event}"),
            Ok(Err(broadcast::error::RecvError::Lagged(skipped))) => {
                panic!("unexpected chat event lag, skipped {skipped}")
            }
        }
    }

    async fn wait_for_file_title(path: &Path, expected_title: &str) {
        let mut last_title = None;
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if let Ok(content) = tokio::fs::read_to_string(path).await {
                    if let Ok(json) = serde_json::from_str::<serde_json::Value>(&content) {
                        last_title = json
                            .get("title")
                            .and_then(|value| value.as_str())
                            .map(ToString::to_string);
                        let generated = json
                            .get("isTitleGenerated")
                            .and_then(|value| value.as_bool())
                            .unwrap_or(false);
                        if last_title.as_deref() == Some(expected_title) && generated {
                            return;
                        }
                    }
                }
                tokio::time::sleep(std::time::Duration::from_millis(25)).await;
            }
        })
        .await
        .unwrap_or_else(|_| {
            panic!(
                "timed out waiting for title {expected_title:?}, last title was {:?}",
                last_title
            )
        });
    }

    async fn wait_for_session_title(session_arc: &Arc<AMutex<ChatSession>>, expected_title: &str) {
        let mut last_title = None;
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                {
                    let session = session_arc.lock().await;
                    last_title = Some(session.thread.title.clone());
                    if session.thread.title == expected_title && session.thread.is_title_generated {
                        return;
                    }
                }
                tokio::time::sleep(std::time::Duration::from_millis(25)).await;
            }
        })
        .await
        .unwrap_or_else(|_| {
            panic!(
                "timed out waiting for session title {expected_title:?}, last title was {:?}",
                last_title
            )
        });
    }

    fn trajectory_worktree_sample() -> WorktreeMeta {
        trajectory_worktree_sample_with_id("wt-1")
    }

    fn trajectory_worktree_sample_with_id(id: &str) -> WorktreeMeta {
        WorktreeMeta {
            id: id.to_string(),
            kind: "task_agent".to_string(),
            root: std::path::PathBuf::from("/tmp/refact-wt"),
            source_workspace_root: std::path::PathBuf::from("/tmp/refact-src"),
            repo_root: std::path::PathBuf::from("/tmp/refact-src"),
            branch: Some("refact/task/card".to_string()),
            base_branch: Some("main".to_string()),
            base_commit: Some("abc123".to_string()),
            task_id: Some("task-1".to_string()),
            card_id: Some("card-1".to_string()),
            agent_id: Some("agent-1".to_string()),
            enforce: true,
        }
    }

    #[serial(trajectory_perf)]
    #[tokio::test]
    async fn watcher_picks_up_external_edit_to_task_planner_trajectory() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;
        let mut rx = app.chat.trajectory_events_tx.subscribe();

        start_trajectory_watcher(gcx.clone());
        wait_for_watcher_start().await;
        drain_trajectory_events(&mut rx).await;

        let chat_id = "planner-watch-chat";
        let path = dir
            .path()
            .join(".refact")
            .join("tasks")
            .join("task-watch")
            .join("trajectories")
            .join("planner")
            .join(format!("{}.json", chat_id));
        write_trajectory_file(&path, chat_id, "Planner Watch", "2024-01-01T00:00:01Z").await;
        if cfg!(target_os = "macos") {
            process_trajectory_change_for_source(
                gcx.clone(),
                chat_id,
                false,
                Some(trajectory_source_identity_from_path(
                    &path,
                    &get_all_task_roots(gcx.clone()).await,
                )),
            )
            .await;
        }

        let event = wait_for_trajectory_event(&mut rx, chat_id).await;
        assert_eq!(event.event_type, "updated");
        assert_eq!(event.title.as_deref(), Some("Planner Watch"));
    }

    #[serial(trajectory_perf)]
    #[tokio::test]
    async fn watcher_ignores_non_trajectory_files_in_tasks_dir() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;
        let mut rx = app.chat.trajectory_events_tx.subscribe();

        start_trajectory_watcher(gcx.clone());
        wait_for_watcher_start().await;
        drain_trajectory_events(&mut rx).await;

        let task_dir = dir.path().join(".refact").join("tasks").join("task-ignore");
        tokio::fs::create_dir_all(&task_dir).await.unwrap();
        tokio::fs::write(task_dir.join("meta.yaml"), "id: task-ignore\n")
            .await
            .unwrap();

        assert_no_trajectory_event_for(&mut rx, std::time::Duration::from_millis(700)).await;
    }

    #[test]
    fn trajectory_dispatch_ignores_generated_index_files() {
        let dir = tempfile::tempdir().unwrap();
        let task_root = dir.path().join(".refact").join("tasks");
        std::fs::create_dir_all(&task_root).unwrap();
        let task_index = task_root
            .join("task-index")
            .join("trajectories")
            .join("agents")
            .join("agent-1")
            .join("index.json");
        let task_chat = task_index.with_file_name("chat-1.json");
        let normal_index = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join("index.json");

        assert!(!should_dispatch_trajectory_path(
            &task_index,
            &[task_root.clone()]
        ));
        assert!(!should_dispatch_trajectory_path(
            &normal_index,
            &[task_root.clone()]
        ));
        assert!(should_dispatch_trajectory_path(&task_chat, &[task_root]));
    }

    #[serial(trajectory_perf)]
    #[tokio::test]
    async fn watcher_picks_up_new_task_dir_created_after_startup() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;
        let mut rx = app.chat.trajectory_events_tx.subscribe();

        start_trajectory_watcher(gcx.clone());
        wait_for_watcher_start().await;
        drain_trajectory_events(&mut rx).await;

        let chat_id = "new-task-agent-chat";
        let path = dir
            .path()
            .join(".refact")
            .join("tasks")
            .join("task-created-later")
            .join("trajectories")
            .join("agents")
            .join("agent-1")
            .join(format!("{}.json", chat_id));
        write_trajectory_file(&path, chat_id, "New Task Agent", "2024-01-01T00:00:02Z").await;
        if cfg!(target_os = "macos") {
            process_trajectory_change_for_source(
                gcx.clone(),
                chat_id,
                false,
                Some(trajectory_source_identity_from_path(
                    &path,
                    &get_all_task_roots(gcx.clone()).await,
                )),
            )
            .await;
        }

        let event = wait_for_trajectory_event(&mut rx, chat_id).await;
        assert_eq!(event.event_type, "updated");
        assert_eq!(event.title.as_deref(), Some("New Task Agent"));
    }

    #[tokio::test]
    async fn paginated_list_includes_task_planner_and_agent_trajectories() {
        let dir = tempfile::tempdir().unwrap();
        let (_gcx, app) = make_app_with_workspace(dir.path()).await;
        let root = dir.path().join(".refact");

        write_trajectory_file(
            &root.join("trajectories").join("project-chat.json"),
            "project-chat",
            "Project Chat",
            "2024-01-01T00:00:03Z",
        )
        .await;
        write_trajectory_file(
            &root
                .join("tasks")
                .join("task-list")
                .join("trajectories")
                .join("planner")
                .join("planner-chat.json"),
            "planner-chat",
            "Planner Chat",
            "2024-01-01T00:00:02Z",
        )
        .await;
        write_trajectory_file(
            &root
                .join("tasks")
                .join("task-list")
                .join("trajectories")
                .join("agents")
                .join("agent-1")
                .join("agent-chat.json"),
            "agent-chat",
            "Agent Chat",
            "2024-01-01T00:00:01Z",
        )
        .await;

        let response = handle_v1_trajectories_list(
            State(app),
            axum::extract::Query(TrajectoriesListQuery {
                limit: Some(10),
                cursor: None,
                displayable_only: None,
            }),
        )
        .await
        .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = hyper::body::to_bytes(response.into_body()).await.unwrap();
        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let items = payload["items"].as_array().unwrap();
        let ids: std::collections::HashSet<_> = items
            .iter()
            .filter_map(|item| item["id"].as_str())
            .collect();

        assert_eq!(payload["total_count"].as_u64(), Some(3));
        assert!(ids.contains("project-chat"));
        assert!(ids.contains("planner-chat"));
        assert!(ids.contains("agent-chat"));

        let planner = items
            .iter()
            .find(|item| item["id"].as_str() == Some("planner-chat"))
            .unwrap();
        assert_eq!(planner["task_id"].as_str(), Some("task-list"));
        assert_eq!(planner["task_role"].as_str(), Some("planner"));

        let agent = items
            .iter()
            .find(|item| item["id"].as_str() == Some("agent-chat"))
            .unwrap();
        assert_eq!(agent["task_id"].as_str(), Some("task-list"));
        assert_eq!(agent["task_role"].as_str(), Some("agents"));
        assert_eq!(agent["agent_id"].as_str(), Some("agent-1"));
    }

    #[tokio::test]
    async fn paginated_list_displayable_only_filters_task_and_subagent_trajectories() {
        let dir = tempfile::tempdir().unwrap();
        let (_gcx, app) = make_app_with_workspace(dir.path()).await;
        let root = dir.path().join(".refact");

        write_trajectory_file(
            &root.join("trajectories").join("project-chat.json"),
            "project-chat",
            "Project Chat",
            "2024-01-01T00:00:05Z",
        )
        .await;

        let subagent_path = root.join("trajectories").join("subagent-chat.json");
        let mut subagent =
            sample_trajectory("subagent-chat", "Subagent Chat", "2024-01-01T00:00:04Z");
        subagent["parent_id"] = json!("project-chat");
        subagent["link_type"] = json!("subagent");
        tokio::fs::create_dir_all(subagent_path.parent().unwrap())
            .await
            .unwrap();
        tokio::fs::write(&subagent_path, serde_json::to_string(&subagent).unwrap())
            .await
            .unwrap();

        let planner_path = root
            .join("tasks")
            .join("task-list")
            .join("trajectories")
            .join("planner")
            .join("planner-chat.json");
        let mut planner =
            sample_trajectory("planner-chat", "Planner Chat", "2024-01-01T00:00:035Z");
        planner["mode"] = json!("task_planner");
        planner["task_meta"] = json!({ "task_id": "task-list", "role": "planner" });
        tokio::fs::create_dir_all(planner_path.parent().unwrap())
            .await
            .unwrap();
        tokio::fs::write(&planner_path, serde_json::to_string(&planner).unwrap())
            .await
            .unwrap();

        let review_path = root
            .join("tasks")
            .join("task-list")
            .join("trajectories")
            .join("planner")
            .join("review-chat.json");
        let mut review = sample_trajectory("review-chat", "Review Chat", "2024-01-01T00:00:045Z");
        review["mode"] = json!("review");
        review["parent_id"] = json!("planner-chat");
        review["link_type"] = json!("mode_transition");
        review["task_meta"] = json!({ "task_id": "task-list", "role": "planner" });
        tokio::fs::write(&review_path, serde_json::to_string(&review).unwrap())
            .await
            .unwrap();

        let task_path = root
            .join("trajectories")
            .join("legacy-task-agent-chat.json");
        let mut legacy_task = sample_trajectory(
            "legacy-task-agent-chat",
            "Legacy Task Agent Chat",
            "2024-01-01T00:00:02Z",
        );
        legacy_task["mode"] = json!("task_agent");
        tokio::fs::write(&task_path, serde_json::to_string(&legacy_task).unwrap())
            .await
            .unwrap();

        let buddy_path = root.join("trajectories").join("buddy-chat.json");
        let mut buddy = sample_trajectory("buddy-chat", "Buddy Chat", "2024-01-01T00:00:015Z");
        buddy["buddy_meta"] = json!({ "is_buddy_chat": true });
        tokio::fs::write(&buddy_path, serde_json::to_string(&buddy).unwrap())
            .await
            .unwrap();

        let missing_link_path = root.join("trajectories").join("missing-link-child.json");
        let mut missing_link_child = sample_trajectory(
            "missing-link-child",
            "Missing Link Child",
            "2024-01-01T00:00:01Z",
        );
        missing_link_child["parent_id"] = json!("project-chat");
        tokio::fs::write(
            &missing_link_path,
            serde_json::to_string(&missing_link_child).unwrap(),
        )
        .await
        .unwrap();

        let page = list_trajectories_page(app, 10, None, true).await.unwrap();
        let ids: std::collections::HashSet<_> =
            page.items.iter().map(|item| item.id.as_str()).collect();

        assert_eq!(
            ids,
            std::collections::HashSet::from(["project-chat", "review-chat"])
        );
        assert_eq!(page.total_count, 2);
        assert!(!page.has_more);
    }

    #[tokio::test]
    async fn all_trajectories_displayable_only_filters_subchats_without_parent() {
        let dir = tempfile::tempdir().unwrap();
        let (_gcx, app) = make_app_with_workspace(dir.path()).await;
        let root = dir.path().join(".refact");

        write_trajectory_file(
            &root.join("trajectories").join("project-chat.json"),
            "project-chat",
            "Project Chat",
            "2024-01-01T00:00:05Z",
        )
        .await;

        let linked_path = root.join("trajectories").join("subchat-linked.json");
        let mut linked = sample_trajectory("subchat-linked", "Subchat", "2024-01-01T00:00:04Z");
        linked["parent_id"] = json!("project-chat");
        linked["link_type"] = json!("subagent");
        tokio::fs::create_dir_all(linked_path.parent().unwrap())
            .await
            .unwrap();
        tokio::fs::write(&linked_path, serde_json::to_string(&linked).unwrap())
            .await
            .unwrap();

        let orphan_path = root.join("trajectories").join("subchat-orphan.json");
        let mut orphan = sample_trajectory("subchat-orphan", "Subchat", "2024-01-01T00:00:03Z");
        orphan["link_type"] = json!("subagent");
        tokio::fs::write(&orphan_path, serde_json::to_string(&orphan).unwrap())
            .await
            .unwrap();

        let handoff_path = root.join("trajectories").join("handoff-chat.json");
        let mut handoff = sample_trajectory("handoff-chat", "Handoff Chat", "2024-01-01T00:00:02Z");
        handoff["parent_id"] = json!("project-chat");
        handoff["link_type"] = json!("handoff");
        tokio::fs::write(&handoff_path, serde_json::to_string(&handoff).unwrap())
            .await
            .unwrap();

        let listed = list_all_trajectories_meta(app.clone(), true).await.unwrap();
        let ids: std::collections::HashSet<_> =
            listed.iter().map(|item| item.id.as_str()).collect();
        assert_eq!(
            ids,
            std::collections::HashSet::from(["project-chat", "handoff-chat"])
        );

        let unfiltered = list_all_trajectories_meta(app, false).await.unwrap();
        let unfiltered_ids: std::collections::HashSet<_> =
            unfiltered.iter().map(|item| item.id.as_str()).collect();
        assert!(
            unfiltered_ids.contains("subchat-orphan") && unfiltered_ids.contains("subchat-linked"),
            "displayable_only=false must keep returning every trajectory"
        );
    }

    #[test]
    fn subagentic_link_types_are_hidden_without_a_parent_id() {
        let orphan_subchat = |link_type: &str| -> TrajectoryListData {
            TrajectoryListData {
                id: "subchat-orphan".to_string(),
                updated_at: "2024-01-01T00:00:00Z".to_string(),
                mode: Some("agent".to_string()),
                extra: serde_json::from_value(json!({ "link_type": link_type })).unwrap(),
            }
        };

        for link_type in ["subagent", "gather_files", "review_agent"] {
            assert!(
                !trajectory_list_data_is_displayable_chat(&orphan_subchat(link_type)),
                "{link_type} without parent_id must stay hidden"
            );
        }

        for link_type in ["handoff", "mode_transition", "branch"] {
            assert!(
                trajectory_list_data_is_displayable_chat(&orphan_subchat(link_type)),
                "{link_type} must remain visible"
            );
        }
    }

    #[test]
    fn displayable_chat_predicates_follow_mode_not_task_scope() {
        let review = TrajectoryListData {
            id: "review-chat".to_string(),
            updated_at: "2024-01-01T00:00:00Z".to_string(),
            mode: Some("review".to_string()),
            extra: serde_json::from_value(json!({
                "parent_id": "planner-chat",
                "link_type": "mode_transition",
                "task_meta": { "task_id": "task-list", "role": "planner" }
            }))
            .unwrap(),
        };
        assert!(trajectory_list_data_is_displayable_chat(&review));

        let planner = TrajectoryListData {
            id: "planner-chat".to_string(),
            updated_at: "2024-01-01T00:00:00Z".to_string(),
            mode: Some("task_planner".to_string()),
            extra: serde_json::from_value(json!({
                "task_meta": { "task_id": "task-list", "role": "planner" }
            }))
            .unwrap(),
        };
        assert!(!trajectory_list_data_is_displayable_chat(&planner));

        let buddy = TrajectoryListData {
            id: "buddy-chat".to_string(),
            updated_at: "2024-01-01T00:00:00Z".to_string(),
            mode: Some("review".to_string()),
            extra: serde_json::from_value(json!({ "buddy_meta": { "is_buddy_chat": true } }))
                .unwrap(),
        };
        assert!(!trajectory_list_data_is_displayable_chat(&buddy));

        let subagent = TrajectoryListData {
            id: "subagent-chat".to_string(),
            updated_at: "2024-01-01T00:00:00Z".to_string(),
            mode: Some("agent".to_string()),
            extra: serde_json::from_value(json!({
                "parent_id": "project-chat",
                "link_type": "subagent"
            }))
            .unwrap(),
        };
        assert!(!trajectory_list_data_is_displayable_chat(&subagent));

        let review_event: TrajectoryEvent = serde_json::from_value(json!({
            "type": "updated",
            "id": "review-chat",
            "mode": "review",
            "task_id": "task-list",
            "parent_id": "planner-chat",
            "link_type": "mode_transition"
        }))
        .unwrap();
        assert!(trajectory_event_is_displayable_chat(&review_event));

        let planner_event: TrajectoryEvent = serde_json::from_value(json!({
            "type": "updated",
            "id": "planner-chat",
            "mode": "task_planner",
            "task_id": "task-list"
        }))
        .unwrap();
        assert!(!trajectory_event_is_displayable_chat(&planner_event));
    }

    #[tokio::test]
    async fn paginated_list_backfills_when_hydration_skips_changed_candidate() {
        let dir = tempfile::tempdir().unwrap();
        let (_gcx, app) = make_app_with_workspace(dir.path()).await;
        let root = dir.path().join(".refact").join("trajectories");
        write_trajectory_file(
            &root.join("top-chat.json"),
            "top-chat",
            "Top Chat",
            "2024-01-01T00:00:03Z",
        )
        .await;
        write_trajectory_file(
            &root.join("skip-chat.json"),
            "skip-chat",
            "Skip Chat",
            "2024-01-01T00:00:02Z",
        )
        .await;
        write_trajectory_file(
            &root.join("backfill-chat.json"),
            "backfill-chat",
            "Backfill Chat",
            "2024-01-01T00:00:01Z",
        )
        .await;
        tokio::fs::write(
            root.join("skip-chat.json"),
            serde_json::to_string(&sample_trajectory(
                "other-chat",
                "Changed",
                "2024-01-01T00:00:02Z",
            ))
            .unwrap(),
        )
        .await
        .unwrap();

        let candidates = vec![
            TrajectoryListCandidate {
                id: "top-chat".to_string(),
                updated_at: "2024-01-01T00:00:03Z".to_string(),
                path: root.join("top-chat.json"),
                indexed_meta: None,
                indexed_file_len: None,
                indexed_file_modified_unix_ms: None,
            },
            TrajectoryListCandidate {
                id: "skip-chat".to_string(),
                updated_at: "2024-01-01T00:00:02Z".to_string(),
                path: root.join("skip-chat.json"),
                indexed_meta: None,
                indexed_file_len: None,
                indexed_file_modified_unix_ms: None,
            },
            TrajectoryListCandidate {
                id: "backfill-chat".to_string(),
                updated_at: "2024-01-01T00:00:01Z".to_string(),
                path: root.join("backfill-chat.json"),
                indexed_meta: None,
                indexed_file_len: None,
                indexed_file_modified_unix_ms: None,
            },
        ];
        let task_roots = Vec::new();

        let (items, has_more) = hydrate_trajectory_list_page(app, candidates, 2, &task_roots).await;
        let ids: Vec<_> = items.iter().map(|item| item.id.as_str()).collect();

        assert_eq!(ids, vec!["top-chat", "backfill-chat"]);
        assert!(!has_more);
    }

    #[test]
    fn test_validate_trajectory_id_rejects_path_traversal() {
        assert!(validate_trajectory_id("../etc/passwd").is_err());
        assert!(validate_trajectory_id("..").is_err());
        assert!(validate_trajectory_id("a/../b").is_err());
    }

    #[tokio::test]
    async fn trajectory_path_helpers_and_save_reject_malformed_chat_id() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        tokio::fs::create_dir_all(dir.path().join(".refact").join("trajectories"))
            .await
            .unwrap();
        tokio::fs::create_dir_all(
            dir.path()
                .join(".refact")
                .join("buddy")
                .join("chats")
                .join("conversations"),
        )
        .await
        .unwrap();
        tokio::fs::write(dir.path().join(".refact").join("bad.json"), "{}")
            .await
            .unwrap();
        tokio::fs::write(
            dir.path()
                .join(".refact")
                .join("buddy")
                .join("chats")
                .join("bad.json"),
            "{}",
        )
        .await
        .unwrap();

        assert!(find_trajectory_path(gcx.clone(), "../bad").await.is_none());
        assert!(find_trajectory_or_buddy_path(gcx.clone(), "../bad")
            .await
            .is_none());

        let err = save_trajectory_snapshot(
            gcx,
            test_snapshot(
                "../save-bad",
                "Bad",
                vec![ChatMessage::new("user".to_string(), "hello".to_string())],
            ),
        )
        .await
        .unwrap_err();
        assert_eq!(err, "Invalid trajectory id");
        assert!(
            !tokio::fs::try_exists(dir.path().join(".refact").join("save-bad.json"))
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn generic_trajectory_delete_does_not_delete_buddy_conversation_file() {
        let dir = tempfile::tempdir().unwrap();
        let (_gcx, app) = make_app_with_workspace(dir.path()).await;
        let chat_id = "buddy-delete-isolated";
        let buddy_path = dir
            .path()
            .join(".refact")
            .join("buddy")
            .join("chats")
            .join("conversations")
            .join(format!("{}.json", chat_id));
        write_buddy_conversation_file(&buddy_path, chat_id, "Keep Buddy").await;

        let err = handle_v1_trajectories_delete(State(app), AxumPath(chat_id.to_string()))
            .await
            .unwrap_err();

        assert_eq!(err.status_code, StatusCode::NOT_FOUND);
        assert!(tokio::fs::try_exists(&buddy_path).await.unwrap());
    }

    #[tokio::test]
    async fn generic_trajectory_delete_does_not_emit_buddy_fallback_update() {
        let dir = tempfile::tempdir().unwrap();
        let (_gcx, app) = make_app_with_workspace(dir.path()).await;
        let mut rx = app.chat.trajectory_events_tx.subscribe();
        let chat_id = "buddy-delete-no-fallback-update";
        let normal_path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        let buddy_path = dir
            .path()
            .join(".refact")
            .join("buddy")
            .join("chats")
            .join("conversations")
            .join(format!("{chat_id}.json"));
        write_trajectory_file(
            &normal_path,
            chat_id,
            "Delete Normal",
            "2024-01-01T00:00:01Z",
        )
        .await;
        write_buddy_conversation_file(&buddy_path, chat_id, "Keep Buddy").await;

        handle_v1_trajectories_delete(State(app), AxumPath(chat_id.to_string()))
            .await
            .unwrap();

        let event = wait_for_trajectory_event(&mut rx, chat_id).await;
        assert_eq!(event.event_type, "deleted");
        assert_eq!(event.title, None);
        assert!(!tokio::fs::try_exists(&normal_path).await.unwrap());
        assert!(tokio::fs::try_exists(&buddy_path).await.unwrap());
    }

    #[tokio::test]
    async fn explicit_buddy_inclusive_lookup_loads_buddy_conversation_file() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let chat_id = "buddy-read-inclusive";
        let buddy_path = dir
            .path()
            .join(".refact")
            .join("buddy")
            .join("chats")
            .join("conversations")
            .join(format!("{}.json", chat_id));
        write_buddy_conversation_file(&buddy_path, chat_id, "Readable Buddy").await;

        assert!(find_trajectory_path(gcx.clone(), chat_id).await.is_none());
        assert_same_optional_path(
            find_trajectory_or_buddy_path(gcx.clone(), chat_id).await,
            &buddy_path,
        );
        let loaded = load_trajectory_for_chat(gcx, chat_id).await.unwrap();

        assert_eq!(loaded.thread.title, "Readable Buddy");
        assert_eq!(
            loaded.thread.buddy_meta.unwrap().buddy_chat_kind,
            "investigation"
        );
        assert_eq!(loaded.messages.len(), 1);
    }

    #[tokio::test]
    async fn empty_new_chat_pending_cap_survives_real_save_load_and_model_late() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let chat_id = "empty-pending-cap";
        let mut snapshot = test_snapshot(chat_id, "", Vec::new());
        snapshot.auto_compression_cap_pending = true;
        save_trajectory_snapshot(gcx.clone(), snapshot)
            .await
            .unwrap();
        let mut loaded = load_trajectory_for_chat(gcx.clone(), chat_id)
            .await
            .unwrap();
        assert!(loaded.messages.is_empty());
        assert!(loaded.thread.auto_compression_cap_pending);
        assert!(loaded.thread.resolve_pending_compression_cap(Some(1000)));
        assert_eq!(loaded.thread.auto_compression_cap, Some(900));
        let path = find_trajectory_path(gcx.clone(), chat_id).await.unwrap();
        let mut value: serde_json::Value =
            serde_json::from_slice(&tokio::fs::read(&path).await.unwrap()).unwrap();
        value
            .as_object_mut()
            .unwrap()
            .remove("auto_compression_cap_pending");
        tokio::fs::write(path, serde_json::to_vec(&value).unwrap())
            .await
            .unwrap();
        let mut legacy = load_trajectory_for_chat(gcx, chat_id).await.unwrap();
        assert!(legacy.messages.is_empty());
        assert!(!legacy.thread.resolve_pending_compression_cap(Some(1000)));
        assert_eq!(legacy.thread.auto_compression_cap, None);
    }

    #[tokio::test]
    async fn auto_compression_cap_survives_real_trajectory_save_and_load() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let chat_id = "auto-compression-cap-roundtrip";
        let mut snapshot = test_snapshot(
            chat_id,
            "Auto compression cap",
            vec![ChatMessage::new("user".to_string(), "hello".to_string())],
        );
        snapshot.auto_compression_cap = Some(96_000);

        save_trajectory_snapshot(gcx.clone(), snapshot)
            .await
            .unwrap();
        let loaded = load_trajectory_for_chat(gcx.clone(), chat_id)
            .await
            .unwrap();
        assert_eq!(loaded.thread.auto_compression_cap, Some(96_000));

        let path = find_trajectory_path(gcx.clone(), chat_id).await.unwrap();
        let mut value: serde_json::Value =
            serde_json::from_slice(&tokio::fs::read(&path).await.unwrap()).unwrap();
        value
            .as_object_mut()
            .unwrap()
            .remove("auto_compression_cap");
        value
            .as_object_mut()
            .unwrap()
            .remove("auto_compression_cap_pending");
        tokio::fs::write(&path, serde_json::to_vec(&value).unwrap())
            .await
            .unwrap();
        let mut legacy = load_trajectory_for_chat(gcx, chat_id).await.unwrap();
        assert_eq!(legacy.thread.auto_compression_cap, None);
        assert!(!legacy.thread.auto_compression_cap_pending);
        assert!(!legacy.thread.resolve_pending_compression_cap(Some(128_000)));
        assert_eq!(legacy.thread.auto_compression_cap, None);
    }

    #[tokio::test]
    async fn pending_new_chat_cap_survives_empty_first_save_and_model_late_load() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let chat_id = "pending-new-chat-cap";
        let mut snapshot = test_snapshot(chat_id, "", Vec::new());
        snapshot.auto_compression_cap_pending = true;
        snapshot.context_tokens_cap = Some(100_000);
        save_trajectory_snapshot(gcx.clone(), snapshot)
            .await
            .unwrap();
        let mut loaded = load_trajectory_for_chat(gcx.clone(), chat_id)
            .await
            .unwrap();
        assert!(loaded.messages.is_empty());
        assert!(loaded.thread.auto_compression_cap_pending);
        assert!(!loaded.thread.resolve_pending_compression_cap(None));
        assert_eq!(loaded.thread.auto_compression_cap, None);
        assert!(loaded.thread.resolve_pending_compression_cap(Some(80_000)));
        assert_eq!(loaded.thread.auto_compression_cap, Some(72_000));
        save_trajectory_as(gcx.clone(), &loaded.thread, &loaded.messages).await;
        let restored = load_trajectory_for_chat(gcx, chat_id).await.unwrap();
        assert_eq!(restored.thread.auto_compression_cap, Some(72_000));
        assert!(!restored.thread.auto_compression_cap_pending);
    }

    #[tokio::test]
    async fn new_empty_chat_default_provenance_survives_restart_before_model_selection() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let chat_id = "new-chat-pending-default";
        let mut snapshot = test_snapshot(chat_id, "", Vec::new());
        snapshot.auto_compression_cap_pending = true;
        snapshot.context_tokens_cap = Some(80_000);
        save_trajectory_snapshot(gcx.clone(), snapshot)
            .await
            .unwrap();
        let mut loaded = load_trajectory_for_chat(gcx.clone(), chat_id)
            .await
            .unwrap();
        assert!(loaded.messages.is_empty());
        assert_eq!(loaded.thread.auto_compression_cap, None);
        assert!(loaded.thread.auto_compression_cap_pending);
        assert!(loaded.thread.resolve_pending_compression_cap(Some(100_000)));
        assert_eq!(loaded.thread.auto_compression_cap, Some(72_000));

        // An empty legacy file is not evidence that this is a new chat.
        let path = find_trajectory_path(gcx.clone(), chat_id).await.unwrap();
        let mut value: serde_json::Value =
            serde_json::from_slice(&tokio::fs::read(&path).await.unwrap()).unwrap();
        value
            .as_object_mut()
            .unwrap()
            .remove("auto_compression_cap_pending");
        tokio::fs::write(path, serde_json::to_vec(&value).unwrap())
            .await
            .unwrap();
        let mut legacy = load_trajectory_for_chat(gcx, chat_id).await.unwrap();
        assert!(legacy.messages.is_empty());
        assert!(!legacy.thread.resolve_pending_compression_cap(Some(100_000)));
        assert_eq!(legacy.thread.auto_compression_cap, None);
    }

    #[tokio::test]
    async fn explicit_auto_compression_cap_makes_empty_chat_save_worthy() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let chat_id = "empty-chat-auto-compression-cap";
        let mut snapshot = test_snapshot(chat_id, "Empty capped chat", Vec::new());
        snapshot.auto_compression_cap = Some(64_000);

        save_trajectory_snapshot(gcx.clone(), snapshot)
            .await
            .unwrap();
        let loaded = load_trajectory_for_chat(gcx, chat_id).await.unwrap();
        assert!(loaded.messages.is_empty());
        assert_eq!(loaded.thread.auto_compression_cap, Some(64_000));
    }

    #[serial(trajectory_perf)]
    #[serial(trajectory_perf)]
    #[tokio::test]
    async fn save_trajectory_as_is_durable_with_the_detached_writer() {
        let _lock = serial_test_guard();
        let _writer = TrajectoryWriterRolloutGuard::set(true);
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let thread = ThreadParams {
            id: "save-trajectory-as-durable".to_string(),
            title: "Durable direct save".to_string(),
            ..Default::default()
        };

        save_trajectory_as(
            gcx.clone(),
            &thread,
            &[ChatMessage::new(
                "user".to_string(),
                "persisted".to_string(),
            )],
        )
        .await;

        let loaded = load_trajectory_for_chat(gcx, &thread.id).await;
        assert!(loaded.is_some());
        assert_eq!(
            loaded.unwrap().messages[0].content.content_text_only(),
            "persisted"
        );
    }

    #[tokio::test]
    async fn empty_chat_can_clear_previously_saved_auto_compression_cap() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let chat_id = "empty-chat-clear-auto-compression-cap";
        let mut thread = ThreadParams {
            id: chat_id.to_string(),
            title: "Empty capped chat".to_string(),
            auto_compression_cap: Some(64_000),
            ..Default::default()
        };

        save_trajectory_as(gcx.clone(), &thread, &[]).await;
        tokio::time::timeout(Duration::from_secs(60), async {
            loop {
                if load_trajectory_for_chat(gcx.clone(), chat_id)
                    .await
                    .is_some()
                {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("checkpoint should persist the empty capped chat");
        assert_eq!(
            load_trajectory_for_chat(gcx.clone(), chat_id)
                .await
                .unwrap()
                .thread
                .auto_compression_cap,
            Some(64_000)
        );

        thread.auto_compression_cap = None;
        save_trajectory_as(gcx.clone(), &thread, &[]).await;
        tokio::time::timeout(Duration::from_secs(60), async {
            loop {
                if load_trajectory_for_chat(gcx.clone(), chat_id)
                    .await
                    .is_some_and(|loaded| loaded.thread.auto_compression_cap.is_none())
                {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("checkpoint should clear the persisted cap");
        assert_eq!(
            load_trajectory_for_chat(gcx, chat_id)
                .await
                .unwrap()
                .thread
                .auto_compression_cap,
            None
        );
    }

    #[tokio::test]
    async fn generic_loader_ignores_buddy_only_fallback() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let chat_id = "buddy-generic-loader-isolated";
        let buddy_path = dir
            .path()
            .join(".refact")
            .join("buddy")
            .join("chats")
            .join("conversations")
            .join(format!("{chat_id}.json"));
        write_buddy_conversation_file(&buddy_path, chat_id, "Only Buddy").await;

        assert!(load_generic_trajectory_for_chat(gcx.clone(), chat_id)
            .await
            .is_none());
        let loaded = load_trajectory_for_chat(gcx, chat_id).await.unwrap();

        assert_eq!(loaded.thread.title, "Only Buddy");
        assert!(loaded.thread.buddy_meta.is_some());
    }

    #[tokio::test]
    async fn generic_watcher_remove_with_buddy_fallback_emits_deleted() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;
        let mut rx = app.chat.trajectory_events_tx.subscribe();
        let chat_id = "buddy-watcher-remove-fallback";
        let buddy_path = dir
            .path()
            .join(".refact")
            .join("buddy")
            .join("chats")
            .join("conversations")
            .join(format!("{chat_id}.json"));
        write_buddy_conversation_file(&buddy_path, chat_id, "Fallback Buddy").await;

        process_trajectory_change(gcx, chat_id, true).await;

        let event = wait_for_trajectory_event(&mut rx, chat_id).await;
        assert_eq!(event.event_type, "deleted");
        assert_eq!(event.title, None);
        assert_eq!(event.mode, None);
        assert!(tokio::fs::try_exists(&buddy_path).await.unwrap());
    }

    #[tokio::test]
    async fn generic_watcher_remove_with_loadable_fallback_emits_updated() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;
        let mut rx = app.chat.trajectory_events_tx.subscribe();
        let chat_id = "watcher-remove-generic-fallback-updated";
        let fallback_path = dir
            .path()
            .join(".refact")
            .join("tasks")
            .join("task-watch-fallback")
            .join("trajectories")
            .join("planner")
            .join(format!("{chat_id}.json"));
        write_trajectory_file_with_metadata(
            &fallback_path,
            chat_id,
            "Watcher Fallback",
            "2024-01-01T00:00:01Z",
            "fallback after remove",
        )
        .await;

        process_trajectory_change(gcx, chat_id, true).await;

        let event = wait_for_trajectory_event(&mut rx, chat_id).await;
        assert_eq!(event.event_type, "updated");
        assert_eq!(event.title.as_deref(), Some("Watcher Fallback"));
        assert_eq!(event.task_id.as_deref(), Some("task-watch-fallback"));
        assert_eq!(event.task_role.as_deref(), Some("planner"));
        assert!(tokio::fs::try_exists(&fallback_path).await.unwrap());
    }

    #[tokio::test]
    async fn trajectory_id_mismatch_is_not_loaded() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join("safe.json");
        write_trajectory_file(&path, "other-chat", "Mismatched", "2024-01-01T00:00:00Z").await;

        assert!(load_trajectory_for_chat(gcx, "safe").await.is_none());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn symlinked_normal_trajectory_root_is_ignored_for_load() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let chat_id = "symlink-load-ignored";
        let refact_dir = dir.path().join(".refact");
        tokio::fs::create_dir_all(&refact_dir).await.unwrap();
        std::os::unix::fs::symlink(outside.path(), refact_dir.join("trajectories")).unwrap();
        let outside_path = outside.path().join(format!("{chat_id}.json"));
        write_trajectory_file(
            &outside_path,
            chat_id,
            "Outside Symlink Load",
            "2024-01-01T00:00:00Z",
        )
        .await;

        assert!(find_trajectory_path(gcx.clone(), chat_id).await.is_none());
        assert!(load_trajectory_for_chat(gcx, chat_id).await.is_none());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn symlinked_normal_trajectory_root_is_not_used_for_save() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let chat_id = "symlink-save-blocked";
        let refact_dir = dir.path().join(".refact");
        tokio::fs::create_dir_all(&refact_dir).await.unwrap();
        std::os::unix::fs::symlink(outside.path(), refact_dir.join("trajectories")).unwrap();
        let outside_path = outside.path().join(format!("{chat_id}.json"));
        write_trajectory_file(
            &outside_path,
            chat_id,
            "Keep Outside Save",
            "2024-01-01T00:00:00Z",
        )
        .await;
        let before = tokio::fs::read_to_string(&outside_path).await.unwrap();

        let err = save_trajectory_snapshot(
            gcx,
            test_snapshot(
                chat_id,
                "Should Not Escape",
                vec![ChatMessage::new("user".to_string(), "hello".to_string())],
            ),
        )
        .await
        .unwrap_err();

        assert!(err.contains("Refusing to use non-real directory component"));
        assert_eq!(
            tokio::fs::read_to_string(outside_path).await.unwrap(),
            before
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn symlinked_normal_trajectory_root_is_not_used_for_delete() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let (_gcx, app) = make_app_with_workspace(dir.path()).await;
        let chat_id = "symlink-delete-blocked";
        let refact_dir = dir.path().join(".refact");
        tokio::fs::create_dir_all(&refact_dir).await.unwrap();
        std::os::unix::fs::symlink(outside.path(), refact_dir.join("trajectories")).unwrap();
        let outside_path = outside.path().join(format!("{chat_id}.json"));
        write_trajectory_file(
            &outside_path,
            chat_id,
            "Keep Outside Delete",
            "2024-01-01T00:00:00Z",
        )
        .await;

        let err = handle_v1_trajectories_delete(State(app), AxumPath(chat_id.to_string()))
            .await
            .unwrap_err();

        assert_eq!(err.status_code, StatusCode::NOT_FOUND);
        assert!(tokio::fs::try_exists(outside_path).await.unwrap());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn symlinked_normal_trajectory_file_is_not_followed() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;
        let chat_id = "symlink-file-blocked";
        let trajectories_dir = dir.path().join(".refact").join("trajectories");
        tokio::fs::create_dir_all(&trajectories_dir).await.unwrap();
        let outside_path = outside.path().join(format!("{chat_id}.json"));
        write_trajectory_file(
            &outside_path,
            chat_id,
            "Keep Outside File",
            "2024-01-01T00:00:00Z",
        )
        .await;
        let before = tokio::fs::read_to_string(&outside_path).await.unwrap();
        std::os::unix::fs::symlink(
            &outside_path,
            trajectories_dir.join(format!("{chat_id}.json")),
        )
        .unwrap();

        assert!(find_trajectory_path(gcx.clone(), chat_id).await.is_none());
        let delete_err = handle_v1_trajectories_delete(State(app), AxumPath(chat_id.to_string()))
            .await
            .unwrap_err();
        assert_eq!(delete_err.status_code, StatusCode::NOT_FOUND);

        let save_err = save_trajectory_snapshot(
            gcx,
            test_snapshot(
                chat_id,
                "Should Not Follow File",
                vec![ChatMessage::new("user".to_string(), "hello".to_string())],
            ),
        )
        .await
        .unwrap_err();

        assert!(save_err.contains("Refusing symlink child file"));
        assert_eq!(
            tokio::fs::read_to_string(outside_path).await.unwrap(),
            before
        );
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn macos_system_temp_symlink_roots_are_allowed_for_save() {
        let dir = tempfile::tempdir_in(std::env::temp_dir()).unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let chat_id = "macos-var-save-ok";

        save_trajectory_snapshot(
            gcx,
            test_snapshot(
                chat_id,
                "macOS /var Save",
                vec![ChatMessage::new("user".to_string(), "hello".to_string())],
            ),
        )
        .await
        .unwrap();

        assert!(tokio::fs::try_exists(
            dir.path()
                .join(".refact")
                .join("trajectories")
                .join(format!("{chat_id}.json"))
        )
        .await
        .unwrap());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn symlinked_task_trajectory_root_is_ignored_for_load_and_delete() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;
        let chat_id = "symlink-task-root-blocked";
        let tasks_dir = dir.path().join(".refact").join("tasks");
        tokio::fs::create_dir_all(tasks_dir.parent().unwrap())
            .await
            .unwrap();
        std::os::unix::fs::symlink(outside.path(), &tasks_dir).unwrap();
        let outside_path = outside
            .path()
            .join("task-symlink-root")
            .join("trajectories")
            .join("planner")
            .join(format!("{chat_id}.json"));
        write_trajectory_file(
            &outside_path,
            chat_id,
            "Outside Task Root",
            "2024-01-01T00:00:00Z",
        )
        .await;

        assert!(find_trajectory_path(gcx.clone(), chat_id).await.is_none());
        assert!(load_trajectory_for_chat(gcx, chat_id).await.is_none());
        let err = handle_v1_trajectories_delete(State(app), AxumPath(chat_id.to_string()))
            .await
            .unwrap_err();
        assert_eq!(err.status_code, StatusCode::NOT_FOUND);
        assert!(tokio::fs::try_exists(outside_path).await.unwrap());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn symlinked_task_trajectory_agent_dir_is_ignored_for_load_and_delete() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;
        let chat_id = "symlink-task-agent-blocked";
        let agents_dir = dir
            .path()
            .join(".refact")
            .join("tasks")
            .join("task-symlink-agent")
            .join("trajectories")
            .join("agents");
        tokio::fs::create_dir_all(&agents_dir).await.unwrap();
        std::os::unix::fs::symlink(outside.path(), agents_dir.join("agent-1")).unwrap();
        let outside_path = outside.path().join(format!("{chat_id}.json"));
        write_trajectory_file(
            &outside_path,
            chat_id,
            "Outside Task Agent",
            "2024-01-01T00:00:00Z",
        )
        .await;

        assert!(find_trajectory_path(gcx.clone(), chat_id).await.is_none());
        assert!(load_trajectory_for_chat(gcx, chat_id).await.is_none());
        let err = handle_v1_trajectories_delete(State(app), AxumPath(chat_id.to_string()))
            .await
            .unwrap_err();
        assert_eq!(err.status_code, StatusCode::NOT_FOUND);
        assert!(tokio::fs::try_exists(outside_path).await.unwrap());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn symlinked_buddy_conversations_dir_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let chat_id = "symlink-buddy-blocked";
        let buddy_parent = dir.path().join(".refact").join("buddy").join("chats");
        tokio::fs::create_dir_all(&buddy_parent).await.unwrap();
        std::os::unix::fs::symlink(outside.path(), buddy_parent.join("conversations")).unwrap();
        let outside_path = outside.path().join(format!("{chat_id}.json"));
        write_buddy_conversation_file(&outside_path, chat_id, "Outside Buddy").await;

        assert!(find_trajectory_or_buddy_path(gcx.clone(), chat_id)
            .await
            .is_none());
        assert!(load_trajectory_for_chat(gcx, chat_id).await.is_none());
    }

    #[tokio::test]
    async fn trajectory_id_mismatch_normal_candidate_does_not_shadow_task_candidate() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let chat_id = "safe-task-shadow";
        let normal_path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        let task_path = dir
            .path()
            .join(".refact")
            .join("tasks")
            .join("task-shadow")
            .join("trajectories")
            .join("planner")
            .join(format!("{chat_id}.json"));
        write_trajectory_file(
            &normal_path,
            "other-chat",
            "Mismatched Normal",
            "2024-01-01T00:00:00Z",
        )
        .await;
        write_trajectory_file(&task_path, chat_id, "Valid Task", "2024-01-01T00:00:01Z").await;

        assert_same_optional_path(find_trajectory_path(gcx.clone(), chat_id).await, &task_path);
        let loaded = load_trajectory_for_chat(gcx, chat_id).await.unwrap();
        assert_eq!(loaded.thread.title, "Valid Task");
    }

    #[tokio::test]
    async fn schema_incomplete_normal_candidate_does_not_shadow_task_candidate() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let chat_id = "schema-task-shadow";
        let normal_path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        let task_path = dir
            .path()
            .join(".refact")
            .join("tasks")
            .join("task-schema-shadow")
            .join("trajectories")
            .join("planner")
            .join(format!("{chat_id}.json"));
        write_schema_incomplete_trajectory_file(&normal_path, chat_id).await;
        write_trajectory_file(&task_path, chat_id, "Valid Task", "2024-01-01T00:00:01Z").await;

        assert_same_optional_path(find_trajectory_path(gcx.clone(), chat_id).await, &task_path);
        let loaded = load_trajectory_for_chat(gcx, chat_id).await.unwrap();
        assert_eq!(loaded.thread.title, "Valid Task");
    }

    #[tokio::test]
    async fn schema_incomplete_candidate_only_is_not_loaded() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let chat_id = "schema-incomplete-only";
        let path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        write_schema_incomplete_trajectory_file(&path, chat_id).await;

        assert!(find_trajectory_path(gcx.clone(), chat_id).await.is_none());
        assert!(load_trajectory_for_chat(gcx, chat_id).await.is_none());
    }

    #[tokio::test]
    async fn trajectory_id_mismatch_workspace_candidate_does_not_shadow_global_candidate() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let chat_id = "safe-global-shadow";
        let workspace_path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        let global_dir = get_global_trajectories_dir(gcx.clone()).await;
        let global_path = global_dir.join(format!("{chat_id}.json"));
        write_trajectory_file(
            &workspace_path,
            "other-chat",
            "Mismatched Workspace",
            "2024-01-01T00:00:00Z",
        )
        .await;
        write_trajectory_file(
            &global_path,
            chat_id,
            "Valid Global",
            "2024-01-01T00:00:01Z",
        )
        .await;

        assert_same_optional_path(
            find_trajectory_path(gcx.clone(), chat_id).await,
            &global_path,
        );
        let loaded = load_trajectory_for_chat(gcx, chat_id).await.unwrap();
        assert_eq!(loaded.thread.title, "Valid Global");
    }

    #[tokio::test]
    async fn trajectory_id_mismatch_delete_skips_invalid_higher_candidate() {
        let dir = tempfile::tempdir().unwrap();
        let (_gcx, app) = make_app_with_workspace(dir.path()).await;
        let chat_id = "delete-shadow";
        let normal_path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        let task_path = dir
            .path()
            .join(".refact")
            .join("tasks")
            .join("task-delete-shadow")
            .join("trajectories")
            .join("planner")
            .join(format!("{chat_id}.json"));
        write_trajectory_file(
            &normal_path,
            "other-chat",
            "Keep Mismatched",
            "2024-01-01T00:00:00Z",
        )
        .await;
        write_trajectory_file(&task_path, chat_id, "Delete Valid", "2024-01-01T00:00:01Z").await;

        handle_v1_trajectories_delete(State(app), AxumPath(chat_id.to_string()))
            .await
            .unwrap();

        assert!(tokio::fs::try_exists(&normal_path).await.unwrap());
        assert!(!tokio::fs::try_exists(&task_path).await.unwrap());
    }

    #[tokio::test]
    async fn generic_list_does_not_use_active_buddy_state_error_or_worktree() {
        let dir = tempfile::tempdir().unwrap();
        let (_gcx, app) = make_app_with_workspace(dir.path()).await;
        let chat_id = "list-active-buddy-clean";
        let normal_path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        write_trajectory_file(
            &normal_path,
            chat_id,
            "Generic List Item",
            "2024-01-01T00:00:01Z",
        )
        .await;
        let listed_without_session = list_all_trajectories_meta(app.clone(), false)
            .await
            .unwrap();
        let item_without_session = listed_without_session
            .iter()
            .find(|item| item.id == chat_id)
            .cloned()
            .unwrap();
        assert_eq!(item_without_session.session_state, None);
        assert!(item_without_session.worktree.is_none());

        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        {
            let mut session = session_arc.lock().await;
            session.thread.mode = "buddy".to_string();
            session.thread.buddy_meta = Some(buddy_thread_meta());
            session.thread.worktree = Some(trajectory_worktree_sample_with_id("buddy-wt"));
            session.runtime.state = SessionState::Generating;
            session.runtime.error = Some("buddy list error".to_string());
        }
        app.chat
            .sessions
            .write()
            .await
            .insert(chat_id.to_string(), session_arc);

        let listed = list_all_trajectories_meta(app, false).await.unwrap();
        let item = listed
            .iter()
            .find(|item| item.id == chat_id)
            .cloned()
            .unwrap();

        assert_eq!(item.session_state, None);
        assert!(item.worktree.is_none());
        assert_eq!(
            serde_json::to_value(&item).unwrap(),
            serde_json::to_value(&item_without_session).unwrap()
        );
    }

    #[tokio::test]
    async fn generic_list_keeps_normal_active_session_state_and_worktree_enrichment() {
        let dir = tempfile::tempdir().unwrap();
        let (_gcx, app) = make_app_with_workspace(dir.path()).await;
        let chat_id = "list-normal-active-enriched";
        let normal_path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        write_trajectory_file(
            &normal_path,
            chat_id,
            "Normal List Item",
            "2024-01-01T00:00:01Z",
        )
        .await;
        let worktree = trajectory_worktree_sample_with_id("normal-wt");
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        {
            let mut session = session_arc.lock().await;
            session.thread.worktree = Some(worktree.clone());
            session.runtime.state = SessionState::Generating;
        }
        app.chat
            .sessions
            .write()
            .await
            .insert(chat_id.to_string(), session_arc);

        let listed = list_all_trajectories_meta(app, false).await.unwrap();
        let item = listed.iter().find(|item| item.id == chat_id).unwrap();

        assert_eq!(item.session_state.as_deref(), Some("generating"));
        assert_eq!(
            item.worktree.as_ref().map(|meta| meta.id.as_str()),
            Some("normal-wt")
        );
    }

    #[tokio::test]
    async fn generic_list_normal_trajectory_ignores_active_task_state_error_and_worktree() {
        let dir = tempfile::tempdir().unwrap();
        let (_gcx, app) = make_app_with_workspace(dir.path()).await;
        let chat_id = "list-normal-active-task-clean";
        let normal_path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        write_trajectory_file(
            &normal_path,
            chat_id,
            "Normal Collision List Item",
            "2024-01-01T00:00:01Z",
        )
        .await;
        let task_meta = task_meta(
            "task-list-normal-collision",
            "agents",
            Some("agent-list-normal"),
            Some("card-list-normal"),
            None,
        );
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        {
            let mut session = session_arc.lock().await;
            session.thread.task_meta = Some(task_meta);
            session.thread.worktree = Some(trajectory_worktree_sample_with_id("task-list-wt"));
            session.runtime.state = SessionState::Generating;
            session.runtime.error = Some("task list error".to_string());
        }
        app.chat
            .sessions
            .write()
            .await
            .insert(chat_id.to_string(), session_arc);

        let listed = list_all_trajectories_meta(app, false).await.unwrap();
        let item = listed.iter().find(|item| item.id == chat_id).unwrap();

        assert_eq!(item.session_state, None);
        assert!(item.worktree.is_none());
    }

    #[tokio::test]
    async fn generic_list_task_trajectory_ignores_active_normal_state_error_and_worktree() {
        let dir = tempfile::tempdir().unwrap();
        let (_gcx, app) = make_app_with_workspace(dir.path()).await;
        let chat_id = "list-task-active-normal-clean";
        let task_meta = task_meta(
            "task-list-active-normal",
            "agents",
            Some("agent-list-task"),
            Some("card-list-task"),
            None,
        );
        let task_path = dir
            .path()
            .join(".refact")
            .join("tasks")
            .join("task-list-active-normal")
            .join("trajectories")
            .join("agents")
            .join("agent-list-task")
            .join(format!("{chat_id}.json"));
        write_task_trajectory_file_with_user_message(
            &task_path,
            chat_id,
            "Task Collision List Item",
            "task list message",
            &task_meta,
        )
        .await;
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        {
            let mut session = session_arc.lock().await;
            session.thread.worktree = Some(trajectory_worktree_sample_with_id("normal-list-wt"));
            session.runtime.state = SessionState::Generating;
            session.runtime.error = Some("normal list error".to_string());
        }
        app.chat
            .sessions
            .write()
            .await
            .insert(chat_id.to_string(), session_arc);

        let listed = list_all_trajectories_meta(app, false).await.unwrap();
        let item = listed.iter().find(|item| item.id == chat_id).unwrap();

        assert_eq!(item.task_id.as_deref(), Some("task-list-active-normal"));
        assert_eq!(item.task_role.as_deref(), Some("agents"));
        assert_eq!(item.session_state, None);
        assert!(item.worktree.is_none());
    }

    #[tokio::test]
    async fn generic_list_keeps_task_active_session_state_and_worktree_enrichment() {
        let dir = tempfile::tempdir().unwrap();
        let (_gcx, app) = make_app_with_workspace(dir.path()).await;
        let chat_id = "list-task-active-enriched";
        let task_meta = task_meta(
            "task-list-active-enriched",
            "agents",
            Some("agent-list-enriched"),
            Some("card-list-enriched"),
            None,
        );
        let task_path = dir
            .path()
            .join(".refact")
            .join("tasks")
            .join("task-list-active-enriched")
            .join("trajectories")
            .join("agents")
            .join("agent-list-enriched")
            .join(format!("{chat_id}.json"));
        write_task_trajectory_file_with_user_message(
            &task_path,
            chat_id,
            "Task List Item",
            "task list message",
            &task_meta,
        )
        .await;
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        {
            let mut session = session_arc.lock().await;
            session.thread.task_meta = Some(task_meta);
            session.thread.worktree =
                Some(trajectory_worktree_sample_with_id("task-list-enriched-wt"));
            session.runtime.state = SessionState::Generating;
        }
        app.chat
            .sessions
            .write()
            .await
            .insert(chat_id.to_string(), session_arc);

        let listed = list_all_trajectories_meta(app, false).await.unwrap();
        let item = listed.iter().find(|item| item.id == chat_id).unwrap();

        assert_eq!(item.session_state.as_deref(), Some("generating"));
        assert_eq!(
            item.worktree.as_ref().map(|meta| meta.id.as_str()),
            Some("task-list-enriched-wt")
        );
    }

    #[tokio::test]
    async fn http_delete_higher_priority_trajectory_emits_updated_from_fallback() {
        let dir = tempfile::tempdir().unwrap();
        let (_gcx, app) = make_app_with_workspace(dir.path()).await;
        let mut rx = app.chat.trajectory_events_tx.subscribe();
        let chat_id = "http-delete-fallback-updated";
        let normal_path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        let task_path = dir
            .path()
            .join(".refact")
            .join("tasks")
            .join("task-fallback")
            .join("trajectories")
            .join("planner")
            .join(format!("{chat_id}.json"));
        write_trajectory_file(
            &normal_path,
            chat_id,
            "Deleted Higher Priority",
            "2024-01-01T00:00:02Z",
        )
        .await;
        write_trajectory_file_with_metadata(
            &task_path,
            chat_id,
            "Fallback Lower Priority",
            "2024-01-01T00:00:01Z",
            "fallback message",
        )
        .await;
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        {
            let mut session = session_arc.lock().await;
            session.thread.title = "Active Normal Collision".to_string();
            session.runtime.state = SessionState::Generating;
            session.runtime.error = Some("busy fallback state".to_string());
        }
        app.chat
            .sessions
            .write()
            .await
            .insert(chat_id.to_string(), session_arc);

        handle_v1_trajectories_delete(State(app), AxumPath(chat_id.to_string()))
            .await
            .unwrap();

        let event = wait_for_trajectory_event(&mut rx, chat_id).await;
        assert_eq!(event.event_type, "updated");
        assert_eq!(event.updated_at.as_deref(), Some("2024-01-01T00:00:01Z"));
        assert_eq!(event.title.as_deref(), Some("Fallback Lower Priority"));
        assert_eq!(event.is_title_generated, Some(true));
        assert_eq!(event.message_count, Some(1));
        assert_eq!(event.parent_id.as_deref(), Some("parent-fallback"));
        assert_eq!(event.link_type.as_deref(), Some("handoff"));
        assert_eq!(event.root_chat_id.as_deref(), Some("root-fallback"));
        assert_eq!(event.task_id.as_deref(), Some("task-fallback"));
        assert_eq!(event.task_role.as_deref(), Some("planner"));
        assert_eq!(event.card_id, None);
        assert_eq!(event.model.as_deref(), Some("fallback-model"));
        assert_eq!(event.mode.as_deref(), Some("task_planner"));
        assert_eq!(event.session_state.as_deref(), Some("idle"));
        assert_eq!(event.error, None);
        assert_ne!(event.title.as_deref(), Some("Active Normal Collision"));
        assert!(!tokio::fs::try_exists(&normal_path).await.unwrap());
        assert!(tokio::fs::try_exists(&task_path).await.unwrap());
    }

    #[tokio::test]
    async fn http_delete_higher_priority_trajectory_repairs_fallback_before_updated_event() {
        let dir = tempfile::tempdir().unwrap();
        let (_gcx, app) = make_app_with_workspace(dir.path()).await;
        let mut rx = app.chat.trajectory_events_tx.subscribe();
        let chat_id = "http-delete-repaired-fallback-updated-at";
        let normal_path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        let task_path = dir
            .path()
            .join(".refact")
            .join("tasks")
            .join("task-repaired-fallback")
            .join("trajectories")
            .join("planner")
            .join(format!("{chat_id}.json"));
        write_trajectory_file(
            &normal_path,
            chat_id,
            "Deleted Higher Priority Repair",
            "2024-01-01T00:00:02Z",
        )
        .await;
        tokio::fs::create_dir_all(task_path.parent().unwrap())
            .await
            .unwrap();
        tokio::fs::write(
            &task_path,
            serde_json::to_string(&json!({
                "id": chat_id,
                "title": "Repaired Lower Priority Fallback",
                "model": "fallback-model",
                "mode": "task_planner",
                "tool_use": "agent",
                "parent_id": "source-chat",
                "link_type": "mode_transition",
                "previous_response_id": "resp_stale",
                "messages": [
                    {"role":"system","content":"target fallback system"},
                    {"role":"user","content":"fallback message"}
                ],
                "frozen_request_prefix": {
                    "schema_version": 1,
                    "created_at": "2026-05-29T00:00:00Z",
                    "system_prompt": "stale source system",
                    "tools_canonical": [{"type":"function","function":{"name":"source_tool"}}]
                },
                "claude_code_identity": {
                    "device_id":"source-device",
                    "session_id":"source-session"
                },
                "created_at": "not-a-date",
                "updated_at": "2024-01-01T00:00:01Z",
                "include_project_info": true,
                "checkpoints_enabled": true,
                "custom_future_field": {"keep": true, "nested": {"value": 73}}
            }))
            .unwrap(),
        )
        .await
        .unwrap();

        handle_v1_trajectories_delete(State(app), AxumPath(chat_id.to_string()))
            .await
            .unwrap();

        let event = wait_for_trajectory_event(&mut rx, chat_id).await;
        assert_eq!(event.event_type, "updated");
        assert_eq!(
            event.title.as_deref(),
            Some("Repaired Lower Priority Fallback")
        );
        assert_eq!(event.task_id.as_deref(), Some("task-repaired-fallback"));
        assert_eq!(event.task_role.as_deref(), Some("planner"));
        assert_ne!(event.updated_at.as_deref(), Some("2024-01-01T00:00:01Z"));
        assert!(!tokio::fs::try_exists(&normal_path).await.unwrap());
        assert!(tokio::fs::try_exists(&task_path).await.unwrap());

        let repaired: serde_json::Value =
            serde_json::from_str(&tokio::fs::read_to_string(&task_path).await.unwrap()).unwrap();
        assert_eq!(event.updated_at.as_deref(), repaired["updated_at"].as_str());
        assert!(parsed_rfc3339_utc(repaired["created_at"].as_str().unwrap()).is_some());
        assert_eq!(
            repaired["frozen_request_prefix"]["system_prompt"],
            "target fallback system"
        );
        assert!(repaired["frozen_request_prefix"]["tools_canonical"].is_null());
        assert!(repaired.get("previous_response_id").is_none());
        assert!(repaired.get("claude_code_identity").is_none());
        assert_eq!(repaired["custom_future_field"]["nested"]["value"], 73);
    }

    #[tokio::test]
    async fn http_delete_normal_with_task_fallback_does_not_reload_task_into_normal_session() {
        let dir = tempfile::tempdir().unwrap();
        let (_gcx, app) = make_app_with_workspace(dir.path()).await;
        let mut rx = app.chat.trajectory_events_tx.subscribe();
        let chat_id = "http-delete-normal-task-fallback-no-normal-mutation";
        let normal_path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        let task_meta = task_meta(
            "task-normal-delete-fallback",
            "agents",
            Some("agent-normal-delete-fallback"),
            Some("card-normal-delete-fallback"),
            None,
        );
        let task_path = dir
            .path()
            .join(".refact")
            .join("tasks")
            .join("task-normal-delete-fallback")
            .join("trajectories")
            .join("agents")
            .join("agent-normal-delete-fallback")
            .join(format!("{chat_id}.json"));
        write_trajectory_file(
            &normal_path,
            chat_id,
            "Deleted Normal",
            "2024-01-01T00:00:02Z",
        )
        .await;
        write_task_trajectory_file_with_user_message(
            &task_path,
            chat_id,
            "Task Fallback After Normal Delete",
            "task fallback should stay out of normal session",
            &task_meta,
        )
        .await;
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        let mut chat_rx = {
            let mut session = session_arc.lock().await;
            session.thread.title = "Active Normal Before Delete".to_string();
            session.messages.push(ChatMessage::new(
                "user".to_string(),
                "keep active normal after normal delete".to_string(),
            ));
            session.runtime.state = SessionState::Idle;
            session.trajectory_dirty = false;
            session.subscribe()
        };
        app.chat
            .sessions
            .write()
            .await
            .insert(chat_id.to_string(), session_arc.clone());

        handle_v1_trajectories_delete(State(app), AxumPath(chat_id.to_string()))
            .await
            .unwrap();

        let event = wait_for_trajectory_event(&mut rx, chat_id).await;
        assert_eq!(event.event_type, "updated");
        assert_eq!(
            event.title.as_deref(),
            Some("Task Fallback After Normal Delete")
        );
        assert_eq!(
            event.task_id.as_deref(),
            Some("task-normal-delete-fallback")
        );
        assert_eq!(event.task_role.as_deref(), Some("agents"));
        assert_eq!(
            event.agent_id.as_deref(),
            Some("agent-normal-delete-fallback")
        );
        assert_eq!(event.session_state.as_deref(), Some("idle"));
        assert!(!tokio::fs::try_exists(&normal_path).await.unwrap());
        assert!(tokio::fs::try_exists(&task_path).await.unwrap());

        let session = session_arc.lock().await;
        assert_eq!(session.external_reload_pending, None);
        assert_eq!(session.thread.title, "Active Normal Before Delete");
        assert!(session.thread.task_meta.is_none());
        assert_eq!(session.messages.len(), 1);
        assert_eq!(
            session.messages[0].content.content_text_only(),
            "keep active normal after normal delete"
        );
        drop(session);
        assert_no_chat_event_for(&mut chat_rx, std::time::Duration::from_millis(100)).await;
    }

    #[tokio::test]
    async fn http_delete_fallback_with_active_buddy_uses_generic_idle_state() {
        let dir = tempfile::tempdir().unwrap();
        let (_gcx, app) = make_app_with_workspace(dir.path()).await;
        let mut rx = app.chat.trajectory_events_tx.subscribe();
        let chat_id = "http-delete-fallback-active-buddy-clean";
        let normal_path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        let task_path = dir
            .path()
            .join(".refact")
            .join("tasks")
            .join("task-buddy-delete-fallback")
            .join("trajectories")
            .join("planner")
            .join(format!("{chat_id}.json"));
        write_trajectory_file(
            &normal_path,
            chat_id,
            "Deleted Higher Priority",
            "2024-01-01T00:00:02Z",
        )
        .await;
        write_trajectory_file_with_metadata(
            &task_path,
            chat_id,
            "Fallback With Buddy Collision",
            "2024-01-01T00:00:01Z",
            "fallback message",
        )
        .await;
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        {
            let mut session = session_arc.lock().await;
            session.thread.mode = "buddy".to_string();
            session.thread.buddy_meta = Some(buddy_thread_meta());
            session.thread.worktree = Some(trajectory_worktree_sample_with_id("buddy-delete-wt"));
            session.runtime.state = SessionState::Generating;
            session.runtime.error = Some("buddy delete fallback error".to_string());
        }
        app.chat
            .sessions
            .write()
            .await
            .insert(chat_id.to_string(), session_arc);

        handle_v1_trajectories_delete(State(app), AxumPath(chat_id.to_string()))
            .await
            .unwrap();

        let event = wait_for_trajectory_event(&mut rx, chat_id).await;
        assert_eq!(event.event_type, "updated");
        assert_eq!(
            event.title.as_deref(),
            Some("Fallback With Buddy Collision")
        );
        assert_eq!(event.task_id.as_deref(), Some("task-buddy-delete-fallback"));
        assert_eq!(event.session_state.as_deref(), Some("idle"));
        assert_eq!(event.error, None);
        assert_ne!(
            event.worktree.as_ref().map(|worktree| worktree.id.as_str()),
            Some("buddy-delete-wt")
        );
        assert!(!tokio::fs::try_exists(&normal_path).await.unwrap());
        assert!(tokio::fs::try_exists(&task_path).await.unwrap());
    }

    #[tokio::test]
    async fn http_delete_only_remaining_trajectory_emits_deleted() {
        let dir = tempfile::tempdir().unwrap();
        let (_gcx, app) = make_app_with_workspace(dir.path()).await;
        let mut rx = app.chat.trajectory_events_tx.subscribe();
        let chat_id = "http-delete-only-deleted";
        let normal_path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        write_trajectory_file(
            &normal_path,
            chat_id,
            "Only Remaining",
            "2024-01-01T00:00:01Z",
        )
        .await;

        handle_v1_trajectories_delete(State(app), AxumPath(chat_id.to_string()))
            .await
            .unwrap();

        let event = wait_for_trajectory_event(&mut rx, chat_id).await;
        assert_eq!(event.event_type, "deleted");
        assert_eq!(event.updated_at, None);
        assert_eq!(event.title, None);
        assert_eq!(event.session_state, None);
        assert!(!tokio::fs::try_exists(&normal_path).await.unwrap());
    }

    #[tokio::test]
    async fn trajectory_id_mismatch_save_snapshot_does_not_overwrite_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let chat_id = "save-mismatch-guard";
        let path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        write_trajectory_file(
            &path,
            "other-chat",
            "Keep Mismatched",
            "2024-01-01T00:00:00Z",
        )
        .await;
        let before = tokio::fs::read_to_string(&path).await.unwrap();

        let err = save_trajectory_snapshot(
            gcx,
            test_snapshot(
                chat_id,
                "Should Not Save",
                vec![ChatMessage::new("user".to_string(), "hello".to_string())],
            ),
        )
        .await
        .unwrap_err();

        assert!(err.contains("Existing trajectory file id mismatch"));
        assert_eq!(tokio::fs::read_to_string(path).await.unwrap(), before);
    }

    #[tokio::test]
    async fn ordinary_save_preserves_existing_browser_meta_and_unknown_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let chat_id = "metadata-preserve-browser-custom";
        let path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        tokio::fs::create_dir_all(path.parent().unwrap())
            .await
            .unwrap();
        tokio::fs::write(
            &path,
            serde_json::to_string(&json!({
                "id": chat_id,
                "title": "Existing Metadata",
                "model": "model",
                "mode": "agent",
                "tool_use": "agent",
                "messages": [{"role":"user","content":"old"}],
                "created_at": "2024-01-01T00:00:00Z",
                "updated_at": "2024-01-01T00:00:00Z",
                "include_project_info": true,
                "checkpoints_enabled": true,
                "browser_meta": {
                    "browser_runtime_id": "browser-preserve",
                    "tab_urls": ["https://example.com/preserve"],
                    "active_tab_id": "tab-preserve",
                    "attach_screenshot_on_send": true
                },
                "custom_future_field": {
                    "keep": true,
                    "nested": {"value": 57}
                }
            }))
            .unwrap(),
        )
        .await
        .unwrap();

        save_trajectory_snapshot(
            gcx,
            test_snapshot(
                chat_id,
                "Updated Metadata",
                vec![ChatMessage::new("user".to_string(), "new".to_string())],
            ),
        )
        .await
        .unwrap();

        let saved: serde_json::Value =
            serde_json::from_str(&tokio::fs::read_to_string(path).await.unwrap()).unwrap();
        assert_eq!(saved["title"], "Updated Metadata");
        assert_eq!(
            saved["browser_meta"]["browser_runtime_id"],
            "browser-preserve"
        );
        assert_eq!(saved["custom_future_field"]["nested"]["value"], 57);
    }

    #[tokio::test]
    async fn ordinary_save_removes_stale_provider_identity_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let chat_id = "metadata-clear-stale-provider";
        let path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        tokio::fs::create_dir_all(path.parent().unwrap())
            .await
            .unwrap();
        tokio::fs::write(
            &path,
            serde_json::to_string(&json!({
                "id": chat_id,
                "title": "Provider Existing",
                "model": "model",
                "mode": "agent",
                "tool_use": "agent",
                "messages": [{"role":"user","content":"old"}],
                "created_at": "2024-01-01T00:00:00Z",
                "updated_at": "2024-01-01T00:00:00Z",
                "include_project_info": true,
                "checkpoints_enabled": true,
                "previous_response_id": "resp_stale",
                "frozen_request_prefix": {
                    "schema_version": 1,
                    "created_at": "2026-05-29T00:00:00Z",
                    "system_prompt": "stale system",
                    "tools_canonical": [{"type":"function"}]
                },
                "claude_code_identity": {
                    "device_id": "device-stale",
                    "session_id": "session-stale"
                },
                "custom_future_field": {"keep": true}
            }))
            .unwrap(),
        )
        .await
        .unwrap();

        save_trajectory_snapshot(
            gcx,
            test_snapshot(
                chat_id,
                "Provider Cleared",
                vec![ChatMessage::new("user".to_string(), "new".to_string())],
            ),
        )
        .await
        .unwrap();

        let saved: serde_json::Value =
            serde_json::from_str(&tokio::fs::read_to_string(path).await.unwrap()).unwrap();
        assert_eq!(saved["title"], "Provider Cleared");
        assert!(saved.get("previous_response_id").is_none());
        assert!(saved.get("frozen_request_prefix").is_none());
        assert!(saved.get("claude_code_identity").is_none());
        assert_eq!(saved["custom_future_field"]["keep"], true);
    }

    #[tokio::test]
    async fn title_only_ordinary_save_preserves_unknown_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let chat_id = "metadata-title-only-preserve";
        let path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        tokio::fs::create_dir_all(path.parent().unwrap())
            .await
            .unwrap();
        tokio::fs::write(
            &path,
            serde_json::to_string(&json!({
                "id": chat_id,
                "title": "Old Title",
                "model": "model",
                "mode": "agent",
                "tool_use": "agent",
                "messages": [],
                "created_at": "2024-01-01T00:00:00Z",
                "updated_at": "2024-01-01T00:00:00Z",
                "include_project_info": true,
                "checkpoints_enabled": true,
                "custom_future_field": {"nested": {"value": 58}}
            }))
            .unwrap(),
        )
        .await
        .unwrap();

        save_trajectory_snapshot(gcx, test_snapshot(chat_id, "Retitled", Vec::new()))
            .await
            .unwrap();

        let saved: serde_json::Value =
            serde_json::from_str(&tokio::fs::read_to_string(path).await.unwrap()).unwrap();
        assert_eq!(saved["title"], "Retitled");
        assert_eq!(saved["custom_future_field"]["nested"]["value"], 58);
    }

    #[tokio::test]
    async fn trajectory_id_mismatch_http_save_does_not_overwrite_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let (_gcx, app) = make_app_with_workspace(dir.path()).await;
        let chat_id = "http-save-mismatch-guard";
        let path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        write_trajectory_file(
            &path,
            "other-chat",
            "Keep Mismatched",
            "2024-01-01T00:00:00Z",
        )
        .await;
        let before = tokio::fs::read_to_string(&path).await.unwrap();
        let payload = sample_trajectory(chat_id, "Should Not Save", "2024-01-01T00:00:01Z");

        let err = handle_v1_trajectories_save(
            State(app),
            AxumPath(chat_id.to_string()),
            hyper::body::Bytes::from(serde_json::to_vec(&payload).unwrap()),
        )
        .await
        .unwrap_err();

        assert_eq!(err.status_code, StatusCode::CONFLICT);
        assert_eq!(tokio::fs::read_to_string(path).await.unwrap(), before);
    }

    #[tokio::test]
    async fn http_save_updates_existing_global_trajectory_without_project_shadow() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;
        let chat_id = "http-save-global-original";
        let global_dir = get_global_trajectories_dir(gcx).await;
        let global_path = global_dir.join(format!("{chat_id}.json"));
        write_trajectory_file(
            &global_path,
            chat_id,
            "Global Original",
            "2024-01-01T00:00:00Z",
        )
        .await;
        let mut payload = sample_trajectory(chat_id, "Global Updated", "2024-01-01T00:00:01Z");
        payload["messages"] = json!([{"role":"user","content":"saved globally"}]);

        handle_v1_trajectories_save(
            State(app),
            AxumPath(chat_id.to_string()),
            hyper::body::Bytes::from(serde_json::to_vec(&payload).unwrap()),
        )
        .await
        .unwrap();

        let saved: serde_json::Value =
            serde_json::from_str(&tokio::fs::read_to_string(&global_path).await.unwrap()).unwrap();
        assert_eq!(saved["title"], "Global Updated");
        assert_eq!(saved["messages"].as_array().unwrap().len(), 1);
        let project_path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        assert!(!tokio::fs::try_exists(project_path).await.unwrap());
    }

    #[tokio::test]
    async fn http_save_updates_existing_task_trajectory_without_project_shadow() {
        let dir = tempfile::tempdir().unwrap();
        let (_gcx, app) = make_app_with_workspace(dir.path()).await;
        let chat_id = "http-save-task-original";
        let task_id = "task-http-save";
        let task_path = dir
            .path()
            .join(".refact")
            .join("tasks")
            .join(task_id)
            .join("trajectories")
            .join("agents")
            .join("agent-1")
            .join(format!("{chat_id}.json"));
        write_trajectory_file(&task_path, chat_id, "Task Original", "2024-01-01T00:00:00Z").await;
        let mut payload = sample_trajectory(chat_id, "Task Updated", "2024-01-01T00:00:01Z");
        payload["task_meta"] = json!({
            "task_id": task_id,
            "role": "agents",
            "agent_id": "agent-1",
            "card_id": "card-1"
        });

        handle_v1_trajectories_save(
            State(app),
            AxumPath(chat_id.to_string()),
            hyper::body::Bytes::from(serde_json::to_vec(&payload).unwrap()),
        )
        .await
        .unwrap();

        let saved: serde_json::Value =
            serde_json::from_str(&tokio::fs::read_to_string(&task_path).await.unwrap()).unwrap();
        assert_eq!(saved["title"], "Task Updated");
        assert_eq!(saved["task_meta"]["task_id"], task_id);
        let project_path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        assert!(!tokio::fs::try_exists(project_path).await.unwrap());
    }

    #[tokio::test]
    async fn http_save_updates_existing_buddy_trajectory_without_project_shadow() {
        let dir = tempfile::tempdir().unwrap();
        let (_gcx, app) = make_app_with_workspace(dir.path()).await;
        let chat_id = "http-save-buddy-original";
        let buddy_path = dir
            .path()
            .join(".refact")
            .join("buddy")
            .join("chats")
            .join("conversations")
            .join(format!("{chat_id}.json"));
        write_buddy_conversation_file(&buddy_path, chat_id, "Buddy Original").await;
        let mut payload = sample_trajectory(chat_id, "Buddy Updated", "2024-01-01T00:00:01Z");
        payload["mode"] = json!("buddy");
        payload["buddy_meta"] = json!({
            "is_buddy_chat": true,
            "buddy_chat_kind": "investigation"
        });

        handle_v1_trajectories_save(
            State(app),
            AxumPath(chat_id.to_string()),
            hyper::body::Bytes::from(serde_json::to_vec(&payload).unwrap()),
        )
        .await
        .unwrap();

        let saved: serde_json::Value =
            serde_json::from_str(&tokio::fs::read_to_string(&buddy_path).await.unwrap()).unwrap();
        assert_eq!(saved["title"], "Buddy Updated");
        assert!(saved["buddy_meta"]["is_buddy_chat"].as_bool().unwrap());
        let project_path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        assert!(!tokio::fs::try_exists(project_path).await.unwrap());
    }

    #[tokio::test]
    async fn http_generic_save_with_active_buddy_uses_generic_idle_state() {
        let dir = tempfile::tempdir().unwrap();
        let (_gcx, app) = make_app_with_workspace(dir.path()).await;
        let mut rx = app.chat.trajectory_events_tx.subscribe();
        let chat_id = "http-generic-save-active-buddy-clean";
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        {
            let mut session = session_arc.lock().await;
            session.thread.mode = "buddy".to_string();
            session.thread.buddy_meta = Some(buddy_thread_meta());
            session.thread.worktree = Some(trajectory_worktree_sample_with_id("buddy-save-wt"));
            session.runtime.state = SessionState::Generating;
            session.runtime.error = Some("buddy save error".to_string());
        }
        app.chat
            .sessions
            .write()
            .await
            .insert(chat_id.to_string(), session_arc);
        let mut payload =
            sample_trajectory(chat_id, "Generic Save Collision", "2024-01-01T00:00:01Z");
        payload["messages"] = json!([{ "role": "user", "content": "generic save" }]);

        handle_v1_trajectories_save(
            State(app),
            AxumPath(chat_id.to_string()),
            hyper::body::Bytes::from(serde_json::to_vec(&payload).unwrap()),
        )
        .await
        .unwrap();

        let event = wait_for_trajectory_event(&mut rx, chat_id).await;
        assert_eq!(event.event_type, "created");
        assert_eq!(event.title.as_deref(), Some("Generic Save Collision"));
        assert_eq!(event.session_state.as_deref(), Some("idle"));
        assert_eq!(event.error, None);
        assert!(event.worktree.is_none());
    }

    #[tokio::test]
    async fn http_buddy_save_does_not_emit_generic_trajectory_sse() {
        let dir = tempfile::tempdir().unwrap();
        let (_gcx, app) = make_app_with_workspace(dir.path()).await;
        let mut rx = app.chat.trajectory_events_tx.subscribe();
        let chat_id = "http-buddy-save-no-generic-sse";
        let mut payload = sample_trajectory(chat_id, "Buddy Saved", "2024-01-01T00:00:01Z");
        payload["mode"] = json!("buddy");
        payload["buddy_meta"] = json!({
            "is_buddy_chat": true,
            "buddy_chat_kind": "investigation",
            "workflow_id": null
        });

        handle_v1_trajectories_save(
            State(app),
            AxumPath(chat_id.to_string()),
            hyper::body::Bytes::from(serde_json::to_vec(&payload).unwrap()),
        )
        .await
        .unwrap();

        assert_no_trajectory_event_for(&mut rx, std::time::Duration::from_millis(100)).await;
    }

    #[tokio::test]
    async fn trajectory_save_namespace_generic_save_does_not_overwrite_existing_buddy_file() {
        let dir = tempfile::tempdir().unwrap();
        let (_gcx, app) = make_app_with_workspace(dir.path()).await;
        let chat_id = "namespace-generic-buddy-collision";
        let buddy_path = dir
            .path()
            .join(".refact")
            .join("buddy")
            .join("chats")
            .join("conversations")
            .join(format!("{chat_id}.json"));
        write_buddy_conversation_file(&buddy_path, chat_id, "Keep Buddy").await;
        let before = tokio::fs::read_to_string(&buddy_path).await.unwrap();
        let mut payload = sample_trajectory(chat_id, "Generic Saved", "2024-01-01T00:00:01Z");
        payload["messages"] = json!([{ "role": "user", "content": "generic" }]);

        handle_v1_trajectories_save(
            State(app),
            AxumPath(chat_id.to_string()),
            hyper::body::Bytes::from(serde_json::to_vec(&payload).unwrap()),
        )
        .await
        .unwrap();

        assert_eq!(
            tokio::fs::read_to_string(&buddy_path).await.unwrap(),
            before
        );
        let normal_path = nested_trajectory_path(dir.path(), chat_id, chat_id);
        let saved: serde_json::Value =
            serde_json::from_str(&tokio::fs::read_to_string(&normal_path).await.unwrap()).unwrap();
        assert_eq!(saved["title"], "Generic Saved");
        assert!(saved.get("buddy_meta").is_none());
    }

    #[tokio::test]
    async fn trajectory_save_namespace_generic_save_does_not_overwrite_existing_task_file() {
        let dir = tempfile::tempdir().unwrap();
        let (_gcx, app) = make_app_with_workspace(dir.path()).await;
        let chat_id = "namespace-generic-task-collision";
        let task_path = dir
            .path()
            .join(".refact")
            .join("tasks")
            .join("task-generic-collision")
            .join("trajectories")
            .join("agents")
            .join("agent-1")
            .join(format!("{chat_id}.json"));
        write_trajectory_file(&task_path, chat_id, "Keep Task", "2024-01-01T00:00:00Z").await;
        let before = tokio::fs::read_to_string(&task_path).await.unwrap();
        let payload = sample_trajectory(chat_id, "Generic Saved", "2024-01-01T00:00:01Z");

        handle_v1_trajectories_save(
            State(app),
            AxumPath(chat_id.to_string()),
            hyper::body::Bytes::from(serde_json::to_vec(&payload).unwrap()),
        )
        .await
        .unwrap();

        assert_eq!(tokio::fs::read_to_string(&task_path).await.unwrap(), before);
        let normal_path = nested_trajectory_path(dir.path(), chat_id, chat_id);
        let saved: serde_json::Value =
            serde_json::from_str(&tokio::fs::read_to_string(&normal_path).await.unwrap()).unwrap();
        assert_eq!(saved["title"], "Generic Saved");
        assert!(saved.get("task_meta").is_none());
    }

    #[tokio::test]
    async fn trajectory_save_namespace_task_save_does_not_overwrite_existing_normal_file() {
        let dir = tempfile::tempdir().unwrap();
        let (_gcx, app) = make_app_with_workspace(dir.path()).await;
        let chat_id = "namespace-task-normal-collision";
        let task_id = "task-normal-collision";
        let normal_path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        write_trajectory_file(&normal_path, chat_id, "Keep Normal", "2024-01-01T00:00:00Z").await;
        let before = tokio::fs::read_to_string(&normal_path).await.unwrap();
        let task_dir = dir.path().join(".refact").join("tasks").join(task_id);
        tokio::fs::create_dir_all(&task_dir).await.unwrap();
        let mut payload = sample_trajectory(chat_id, "Task Saved", "2024-01-01T00:00:01Z");
        payload["task_meta"] = json!({
            "task_id": task_id,
            "role": "agents",
            "agent_id": "agent-1",
            "card_id": "card-1"
        });

        handle_v1_trajectories_save(
            State(app),
            AxumPath(chat_id.to_string()),
            hyper::body::Bytes::from(serde_json::to_vec(&payload).unwrap()),
        )
        .await
        .unwrap();

        assert_eq!(
            tokio::fs::read_to_string(&normal_path).await.unwrap(),
            before
        );
        let task_path = task_dir
            .join("trajectories")
            .join("agents")
            .join("agent-1")
            .join(format!("{chat_id}.json"));
        let saved: serde_json::Value =
            serde_json::from_str(&tokio::fs::read_to_string(&task_path).await.unwrap()).unwrap();
        assert_eq!(saved["title"], "Task Saved");
        assert_eq!(saved["task_meta"]["task_id"], task_id);
    }

    #[tokio::test]
    async fn trajectory_save_namespace_buddy_save_does_not_overwrite_existing_normal_or_task_file()
    {
        let dir = tempfile::tempdir().unwrap();
        let (_gcx, app) = make_app_with_workspace(dir.path()).await;
        let chat_id = "namespace-buddy-normal-task-collision";
        let normal_path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        let task_path = dir
            .path()
            .join(".refact")
            .join("tasks")
            .join("task-buddy-collision")
            .join("trajectories")
            .join("planner")
            .join(format!("{chat_id}.json"));
        write_trajectory_file(&normal_path, chat_id, "Keep Normal", "2024-01-01T00:00:00Z").await;
        write_trajectory_file(&task_path, chat_id, "Keep Task", "2024-01-01T00:00:00Z").await;
        let normal_before = tokio::fs::read_to_string(&normal_path).await.unwrap();
        let task_before = tokio::fs::read_to_string(&task_path).await.unwrap();
        let mut payload = sample_trajectory(chat_id, "Buddy Saved", "2024-01-01T00:00:01Z");
        payload["mode"] = json!("buddy");
        payload["buddy_meta"] = json!({
            "is_buddy_chat": true,
            "buddy_chat_kind": "investigation",
            "workflow_id": null
        });

        handle_v1_trajectories_save(
            State(app),
            AxumPath(chat_id.to_string()),
            hyper::body::Bytes::from(serde_json::to_vec(&payload).unwrap()),
        )
        .await
        .unwrap();

        assert_eq!(
            tokio::fs::read_to_string(&normal_path).await.unwrap(),
            normal_before
        );
        assert_eq!(
            tokio::fs::read_to_string(&task_path).await.unwrap(),
            task_before
        );
        let buddy_path = dir
            .path()
            .join(".refact")
            .join("buddy")
            .join("chats")
            .join("conversations")
            .join(format!("{chat_id}.json"));
        let saved: serde_json::Value =
            serde_json::from_str(&tokio::fs::read_to_string(&buddy_path).await.unwrap()).unwrap();
        assert_eq!(saved["title"], "Buddy Saved");
        assert_eq!(saved["buddy_meta"]["is_buddy_chat"], true);
    }

    #[tokio::test]
    async fn trajectory_save_namespace_task_and_buddy_meta_rejects_and_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let (_gcx, app) = make_app_with_workspace(dir.path()).await;
        let chat_id = "namespace-task-buddy-meta-conflict";
        let task_id = "task-meta-conflict";
        tokio::fs::create_dir_all(dir.path().join(".refact").join("tasks").join(task_id))
            .await
            .unwrap();
        let mut payload = sample_trajectory(chat_id, "Meta Conflict", "2024-01-01T00:00:01Z");
        payload["task_meta"] = json!({
            "task_id": task_id,
            "role": "planner",
            "planner_chat_id": chat_id
        });
        payload["buddy_meta"] = json!({
            "is_buddy_chat": true,
            "buddy_chat_kind": "investigation",
            "workflow_id": null
        });

        let err = handle_v1_trajectories_save(
            State(app),
            AxumPath(chat_id.to_string()),
            hyper::body::Bytes::from(serde_json::to_vec(&payload).unwrap()),
        )
        .await
        .unwrap_err();

        assert_eq!(err.status_code, StatusCode::INTERNAL_SERVER_ERROR);
        assert!(err.message.contains("both task_meta and buddy_meta"));
        assert!(!tokio::fs::try_exists(
            dir.path()
                .join(".refact")
                .join("trajectories")
                .join(format!("{chat_id}.json"))
        )
        .await
        .unwrap());
        assert!(!tokio::fs::try_exists(
            dir.path()
                .join(".refact")
                .join("buddy")
                .join("chats")
                .join("conversations")
                .join(format!("{chat_id}.json"))
        )
        .await
        .unwrap());
        assert!(!tokio::fs::try_exists(
            dir.path()
                .join(".refact")
                .join("tasks")
                .join(task_id)
                .join("trajectories")
                .join("planner")
                .join(format!("{chat_id}.json"))
        )
        .await
        .unwrap());
    }

    #[tokio::test]
    async fn trajectory_save_namespace_malformed_non_null_task_meta_rejects_and_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let (_gcx, app) = make_app_with_workspace(dir.path()).await;
        let chat_id = "namespace-malformed-task-meta";
        let mut payload = sample_trajectory(chat_id, "Malformed Task", "2024-01-01T00:00:01Z");
        payload["task_meta"] = json!({"task_id": "task-malformed"});

        let err = handle_v1_trajectories_save(
            State(app),
            AxumPath(chat_id.to_string()),
            hyper::body::Bytes::from(serde_json::to_vec(&payload).unwrap()),
        )
        .await
        .unwrap_err();

        assert_eq!(err.status_code, StatusCode::INTERNAL_SERVER_ERROR);
        assert!(err.message.contains("Invalid task_meta"));
        assert!(!tokio::fs::try_exists(
            dir.path()
                .join(".refact")
                .join("trajectories")
                .join(format!("{chat_id}.json"))
        )
        .await
        .unwrap());
        assert!(!tokio::fs::try_exists(
            dir.path()
                .join(".refact")
                .join("tasks")
                .join("task-malformed")
        )
        .await
        .unwrap());
    }

    #[tokio::test]
    async fn trajectory_id_mismatch_title_generation_does_not_update_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let chat_id = "title-mismatch-guard";
        let trajectories_dir = dir.path().join(".refact").join("trajectories");
        let path = trajectories_dir.join(format!("{chat_id}.json"));
        write_trajectory_file(&path, "other-chat", "New Chat", "2024-01-01T00:00:00Z").await;
        let before = tokio::fs::read_to_string(&path).await.unwrap();

        spawn_title_generation_task(
            gcx,
            chat_id.to_string(),
            vec![json!({"role":"user","content":"Generate guarded title"})],
            path.clone(),
            TrajectorySourceIdentity::Normal,
        );
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        assert_eq!(tokio::fs::read_to_string(path).await.unwrap(), before);
    }

    #[tokio::test]
    async fn title_generation_mismatched_backing_file_does_not_update_active_session() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;
        let chat_id = "title-active-mismatch-guard";
        let trajectories_dir = dir.path().join(".refact").join("trajectories");
        let path = trajectories_dir.join(format!("{chat_id}.json"));
        write_trajectory_file(&path, "other-chat", "New Chat", "2024-01-01T00:00:00Z").await;
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        {
            let mut session = session_arc.lock().await;
            session.thread.title = "New Chat".to_string();
            session.thread.is_title_generated = false;
        }
        app.chat
            .sessions
            .write()
            .await
            .insert(chat_id.to_string(), session_arc.clone());

        spawn_title_generation_task(
            gcx,
            chat_id.to_string(),
            vec![json!({"role":"user","content":"Generate guarded active title"})],
            path.clone(),
            TrajectorySourceIdentity::Normal,
        );
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        let session = session_arc.lock().await;
        assert_eq!(session.thread.title, "New Chat");
        assert!(!session.thread.is_title_generated);
    }

    #[tokio::test]
    async fn generic_title_generation_with_active_buddy_updates_file_and_sse_only() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;
        let mut rx = app.chat.trajectory_events_tx.subscribe();
        let chat_id = "title-generic-active-buddy";
        let trajectories_dir = dir.path().join(".refact").join("trajectories");
        let path = trajectories_dir.join(format!("{chat_id}.json"));
        let expected_title = "Generic collision request";
        write_trajectory_file_with_user_message(&path, chat_id, "New Chat", expected_title).await;
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        {
            let mut session = session_arc.lock().await;
            session.thread.title = "Buddy Session Title".to_string();
            session.thread.mode = "buddy".to_string();
            session.thread.buddy_meta = Some(buddy_thread_meta());
            session.thread.is_title_generated = false;
            session.trajectory_events_tx = Some(app.chat.trajectory_events_tx.clone());
        }
        app.chat
            .sessions
            .write()
            .await
            .insert(chat_id.to_string(), session_arc.clone());

        spawn_title_generation_task(
            gcx,
            chat_id.to_string(),
            vec![json!({"role":"user","content":expected_title})],
            path.clone(),
            TrajectorySourceIdentity::Normal,
        );

        wait_for_file_title(&path, expected_title).await;
        let event = wait_for_trajectory_event(&mut rx, chat_id).await;
        assert_eq!(event.event_type, "updated");
        assert_eq!(event.title.as_deref(), Some(expected_title));
        assert_eq!(event.is_title_generated, Some(true));
        assert_eq!(event.session_state.as_deref(), Some("idle"));
        let session = session_arc.lock().await;
        assert_eq!(session.thread.title, "Buddy Session Title");
        assert!(!session.thread.is_title_generated);
    }

    #[tokio::test]
    async fn buddy_title_generation_with_active_generic_updates_file_without_generic_sse() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;
        let mut rx = app.chat.trajectory_events_tx.subscribe();
        let chat_id = "title-buddy-active-generic";
        let buddy_dir = dir
            .path()
            .join(".refact")
            .join("buddy")
            .join("chats")
            .join("conversations");
        let path = buddy_dir.join(format!("{chat_id}.json"));
        let expected_title = "Buddy collision request";
        write_buddy_conversation_file(&path, chat_id, "New Chat").await;
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        {
            let mut session = session_arc.lock().await;
            session.thread.title = "Generic Session Title".to_string();
            session.thread.is_title_generated = false;
            session.trajectory_events_tx = Some(app.chat.trajectory_events_tx.clone());
        }
        app.chat
            .sessions
            .write()
            .await
            .insert(chat_id.to_string(), session_arc.clone());

        spawn_title_generation_task(
            gcx,
            chat_id.to_string(),
            vec![json!({"role":"user","content":expected_title})],
            path.clone(),
            TrajectorySourceIdentity::Buddy,
        );

        wait_for_file_title(&path, expected_title).await;
        let session = session_arc.lock().await;
        assert_eq!(session.thread.title, "Generic Session Title");
        assert!(!session.thread.is_title_generated);
        drop(session);
        assert_no_trajectory_event_for(&mut rx, std::time::Duration::from_millis(200)).await;
    }

    #[tokio::test]
    async fn task_title_generation_with_active_normal_updates_task_file_only() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;
        let mut rx = app.chat.trajectory_events_tx.subscribe();
        let chat_id = "title-task-active-normal";
        let task_meta = task_meta(
            "task-title-normal",
            "agents",
            Some("agent-1"),
            Some("card-1"),
            None,
        );
        let path = dir
            .path()
            .join(".refact")
            .join("tasks")
            .join("task-title-normal")
            .join("trajectories")
            .join("agents")
            .join("agent-1")
            .join(format!("{chat_id}.json"));
        let expected_title = "Task collision request";
        write_task_trajectory_file_with_user_message(
            &path,
            chat_id,
            "New Chat",
            expected_title,
            &task_meta,
        )
        .await;
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        {
            let mut session = session_arc.lock().await;
            session.thread.title = "Normal Session Title".to_string();
            session.thread.is_title_generated = false;
            session.trajectory_events_tx = Some(app.chat.trajectory_events_tx.clone());
        }
        app.chat
            .sessions
            .write()
            .await
            .insert(chat_id.to_string(), session_arc.clone());

        spawn_title_generation_task(
            gcx,
            chat_id.to_string(),
            vec![json!({"role":"user","content":expected_title})],
            path.clone(),
            TrajectorySourceIdentity::from_task_meta(&task_meta),
        );

        wait_for_file_title(&path, expected_title).await;
        let event = wait_for_trajectory_event(&mut rx, chat_id).await;
        assert_eq!(event.event_type, "updated");
        assert_eq!(event.title.as_deref(), Some(expected_title));
        assert_eq!(event.task_id.as_deref(), Some("task-title-normal"));
        assert_eq!(event.task_role.as_deref(), Some("agents"));
        assert_eq!(event.agent_id.as_deref(), Some("agent-1"));
        assert_eq!(event.card_id.as_deref(), Some("card-1"));
        let session = session_arc.lock().await;
        assert_eq!(session.thread.title, "Normal Session Title");
        assert!(!session.thread.is_title_generated);
    }

    #[tokio::test]
    async fn file_only_title_generation_update_preserves_token_totals() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;
        let mut rx = app.chat.trajectory_events_tx.subscribe();
        let chat_id = "title-file-only-token-totals";
        let path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        let expected_title = "Token rich title request";
        let mut trajectory = sample_trajectory(chat_id, "New Chat", "2024-01-01T00:00:01Z");
        trajectory["messages"] = json!([
            {"role": "user", "content": expected_title},
            {
                "role": "assistant",
                "content": "response",
                "usage": {
                    "prompt_tokens": 11,
                    "completion_tokens": 7,
                    "total_tokens": 18,
                    "cache_read_input_tokens": 3,
                    "cache_creation_input_tokens": 2,
                    "metering_usd": {"total_usd": 0.012}
                }
            },
            {
                "role": "assistant",
                "content": "second response",
                "usage": {
                    "prompt_tokens": 13,
                    "completion_tokens": 5,
                    "total_tokens": 18,
                    "cache_read_tokens": 4,
                    "cache_creation_tokens": 6,
                    "metering_usd": {"total_usd": 0.008}
                }
            }
        ]);
        tokio::fs::create_dir_all(path.parent().unwrap())
            .await
            .unwrap();
        tokio::fs::write(&path, serde_json::to_string(&trajectory).unwrap())
            .await
            .unwrap();

        spawn_title_generation_task(
            gcx,
            chat_id.to_string(),
            vec![json!({"role":"user","content":expected_title})],
            path.clone(),
            TrajectorySourceIdentity::Normal,
        );

        wait_for_file_title(&path, expected_title).await;
        let event = wait_for_trajectory_event(&mut rx, chat_id).await;
        assert_eq!(event.event_type, "updated");
        assert_eq!(event.title.as_deref(), Some(expected_title));
        assert_eq!(event.is_title_generated, Some(true));
        assert_eq!(event.total_prompt_tokens, Some(24));
        assert_eq!(event.total_completion_tokens, Some(12));
        assert_eq!(event.total_tokens, Some(36));
        assert_eq!(event.total_cache_read_tokens, Some(7));
        assert_eq!(event.total_cache_creation_tokens, Some(8));
        assert!((event.total_cost_usd.unwrap() - 0.02).abs() < 1e-9);
    }

    #[tokio::test]
    async fn file_only_title_generation_update_preserves_line_change_totals() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;
        let mut rx = app.chat.trajectory_events_tx.subscribe();
        let chat_id = "title-file-only-line-totals";
        let path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        let expected_title = "Line rich title request";
        let diff_content = serde_json::to_string(&json!([
            {"lines_add": "a\nb\n", "lines_remove": "old\n"},
            {"lines_add": "c\n", "lines_remove": "old2\nold3\n"}
        ]))
        .unwrap();
        let mut trajectory = sample_trajectory(chat_id, "New Chat", "2024-01-01T00:00:01Z");
        trajectory["messages"] = json!([
            {"role": "user", "content": expected_title},
            {"role": "diff", "content": diff_content}
        ]);
        tokio::fs::create_dir_all(path.parent().unwrap())
            .await
            .unwrap();
        tokio::fs::write(&path, serde_json::to_string(&trajectory).unwrap())
            .await
            .unwrap();

        spawn_title_generation_task(
            gcx,
            chat_id.to_string(),
            vec![json!({"role":"user","content":expected_title})],
            path.clone(),
            TrajectorySourceIdentity::Normal,
        );

        wait_for_file_title(&path, expected_title).await;
        let event = wait_for_trajectory_event(&mut rx, chat_id).await;
        assert_eq!(event.title.as_deref(), Some(expected_title));
        assert_eq!(event.total_lines_added, Some(3));
        assert_eq!(event.total_lines_removed, Some(3));
    }

    #[tokio::test]
    async fn task_file_only_title_generation_update_includes_context_and_progress() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;
        let mut rx = app.chat.trajectory_events_tx.subscribe();
        let chat_id = "title-task-file-only-progress";
        let task_meta = task_meta(
            "task-title-progress",
            "agents",
            Some("agent-progress"),
            Some("card-progress"),
            None,
        );
        let path = dir
            .path()
            .join(".refact")
            .join("tasks")
            .join("task-title-progress")
            .join("trajectories")
            .join("agents")
            .join("agent-progress")
            .join(format!("{chat_id}.json"));
        let expected_title = "Task progress title request";
        let tasks_args = serde_json::to_string(&json!({
            "tasks": [
                {"title": "done task", "status": "completed"},
                {"title": "failed task", "status": "failed"},
                {"title": "todo task", "status": "pending"}
            ]
        }))
        .unwrap();
        let mut trajectory = sample_trajectory(chat_id, "New Chat", "2024-01-01T00:00:01Z");
        trajectory["mode"] = json!("task_agent");
        trajectory["task_meta"] = serde_json::to_value(&task_meta).unwrap();
        trajectory["messages"] = json!([
            {"role": "user", "content": expected_title},
            {
                "role": "assistant",
                "content": "setting tasks",
                "tool_calls": [{
                    "id": "tasks-call-1",
                    "function": {"name": "tasks_set", "arguments": tasks_args}
                }]
            },
            {"role": "tool", "tool_call_id": "tasks-call-1", "content": "ok"}
        ]);
        tokio::fs::create_dir_all(path.parent().unwrap())
            .await
            .unwrap();
        tokio::fs::write(&path, serde_json::to_string(&trajectory).unwrap())
            .await
            .unwrap();

        spawn_title_generation_task(
            gcx,
            chat_id.to_string(),
            vec![json!({"role":"user","content":expected_title})],
            path.clone(),
            TrajectorySourceIdentity::from_task_meta(&task_meta),
        );

        wait_for_file_title(&path, expected_title).await;
        let event = wait_for_trajectory_event(&mut rx, chat_id).await;
        assert_eq!(event.event_type, "updated");
        assert_eq!(event.title.as_deref(), Some(expected_title));
        assert_eq!(event.task_id.as_deref(), Some("task-title-progress"));
        assert_eq!(event.task_role.as_deref(), Some("agents"));
        assert_eq!(event.agent_id.as_deref(), Some("agent-progress"));
        assert_eq!(event.card_id.as_deref(), Some("card-progress"));
        assert_eq!(event.tasks_total, Some(3));
        assert_eq!(event.tasks_done, Some(1));
        assert_eq!(event.tasks_failed, Some(1));
    }

    #[tokio::test]
    async fn buddy_file_only_title_generation_emits_no_generic_sse() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;
        let mut rx = app.chat.trajectory_events_tx.subscribe();
        let chat_id = "title-buddy-file-only-no-sse";
        let buddy_dir = dir
            .path()
            .join(".refact")
            .join("buddy")
            .join("chats")
            .join("conversations");
        let path = buddy_dir.join(format!("{chat_id}.json"));
        let expected_title = "Buddy file-only title request";
        write_buddy_conversation_file(&path, chat_id, "New Chat").await;

        spawn_title_generation_task(
            gcx,
            chat_id.to_string(),
            vec![json!({"role":"user","content":expected_title})],
            path.clone(),
            TrajectorySourceIdentity::Buddy,
        );

        wait_for_file_title(&path, expected_title).await;
        assert_no_trajectory_event_for(&mut rx, std::time::Duration::from_millis(200)).await;
    }

    #[tokio::test]
    async fn normal_title_generation_with_active_task_updates_normal_file_only() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;
        let mut rx = app.chat.trajectory_events_tx.subscribe();
        let chat_id = "title-normal-active-task";
        let trajectories_dir = dir.path().join(".refact").join("trajectories");
        let path = trajectories_dir.join(format!("{chat_id}.json"));
        let expected_title = "Normal collision request";
        write_trajectory_file_with_user_message(&path, chat_id, "New Chat", expected_title).await;
        let active_task_meta = task_meta("task-active-title", "planner", None, None, Some(chat_id));
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        {
            let mut session = session_arc.lock().await;
            session.thread.title = "Task Session Title".to_string();
            session.thread.task_meta = Some(active_task_meta);
            session.thread.is_title_generated = false;
            session.trajectory_events_tx = Some(app.chat.trajectory_events_tx.clone());
        }
        app.chat
            .sessions
            .write()
            .await
            .insert(chat_id.to_string(), session_arc.clone());

        spawn_title_generation_task(
            gcx,
            chat_id.to_string(),
            vec![json!({"role":"user","content":expected_title})],
            path.clone(),
            TrajectorySourceIdentity::Normal,
        );

        wait_for_file_title(&path, expected_title).await;
        let event = wait_for_trajectory_event(&mut rx, chat_id).await;
        assert_eq!(event.event_type, "updated");
        assert_eq!(event.title.as_deref(), Some(expected_title));
        assert_eq!(event.task_id, None);
        let session = session_arc.lock().await;
        assert_eq!(session.thread.title, "Task Session Title");
        assert!(!session.thread.is_title_generated);
    }

    #[tokio::test]
    async fn generic_title_generation_with_active_generic_updates_session() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;
        let mut rx = app.chat.trajectory_events_tx.subscribe();
        let chat_id = "title-generic-active-generic";
        let trajectories_dir = dir.path().join(".refact").join("trajectories");
        let path = trajectories_dir.join(format!("{chat_id}.json"));
        let expected_title = "Generic active request";
        write_trajectory_file_with_user_message(&path, chat_id, "New Chat", expected_title).await;
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        {
            let mut session = session_arc.lock().await;
            session.thread.title = "New Chat".to_string();
            session.thread.is_title_generated = false;
            session.messages.push(ChatMessage::new(
                "user".to_string(),
                expected_title.to_string(),
            ));
            session.trajectory_events_tx = Some(app.chat.trajectory_events_tx.clone());
        }
        app.chat
            .sessions
            .write()
            .await
            .insert(chat_id.to_string(), session_arc.clone());

        spawn_title_generation_task(
            gcx,
            chat_id.to_string(),
            vec![json!({"role":"user","content":expected_title})],
            path.clone(),
            TrajectorySourceIdentity::Normal,
        );

        wait_for_session_title(&session_arc, expected_title).await;
        wait_for_file_title(&path, expected_title).await;
        let event = wait_for_trajectory_event(&mut rx, chat_id).await;
        assert_eq!(event.title.as_deref(), Some(expected_title));
    }

    #[tokio::test]
    async fn task_title_generation_with_matching_active_task_updates_session() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;
        let chat_id = "title-task-active-task";
        let task_meta = task_meta(
            "task-title-active",
            "agents",
            Some("agent-2"),
            Some("card-2"),
            None,
        );
        let path = dir
            .path()
            .join(".refact")
            .join("tasks")
            .join("task-title-active")
            .join("trajectories")
            .join("agents")
            .join("agent-2")
            .join(format!("{chat_id}.json"));
        let expected_title = "Task active request";
        write_task_trajectory_file_with_user_message(
            &path,
            chat_id,
            "New Chat",
            expected_title,
            &task_meta,
        )
        .await;
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        {
            let mut session = session_arc.lock().await;
            session.thread.title = "New Chat".to_string();
            session.thread.task_meta = Some(task_meta.clone());
            session.thread.is_title_generated = false;
            session.messages.push(ChatMessage::new(
                "user".to_string(),
                expected_title.to_string(),
            ));
            session.trajectory_events_tx = Some(app.chat.trajectory_events_tx.clone());
        }
        app.chat
            .sessions
            .write()
            .await
            .insert(chat_id.to_string(), session_arc.clone());

        spawn_title_generation_task(
            gcx,
            chat_id.to_string(),
            vec![json!({"role":"user","content":expected_title})],
            path.clone(),
            TrajectorySourceIdentity::from_task_meta(&task_meta),
        );

        wait_for_session_title(&session_arc, expected_title).await;
        wait_for_file_title(&path, expected_title).await;
    }

    #[tokio::test]
    async fn buddy_title_generation_with_active_buddy_updates_session_without_generic_sse() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;
        let mut rx = app.chat.trajectory_events_tx.subscribe();
        let chat_id = "title-buddy-active-buddy";
        let buddy_dir = dir
            .path()
            .join(".refact")
            .join("buddy")
            .join("chats")
            .join("conversations");
        let path = buddy_dir.join(format!("{chat_id}.json"));
        let expected_title = "Buddy active request";
        write_buddy_conversation_file(&path, chat_id, "New Chat").await;
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        {
            let mut session = session_arc.lock().await;
            session.thread.title = "New Chat".to_string();
            session.thread.mode = "buddy".to_string();
            session.thread.buddy_meta = Some(buddy_thread_meta());
            session.thread.is_title_generated = false;
            session.messages.push(ChatMessage::new(
                "user".to_string(),
                expected_title.to_string(),
            ));
            session.trajectory_events_tx = Some(app.chat.trajectory_events_tx.clone());
        }
        app.chat
            .sessions
            .write()
            .await
            .insert(chat_id.to_string(), session_arc.clone());

        spawn_title_generation_task(
            gcx,
            chat_id.to_string(),
            vec![json!({"role":"user","content":expected_title})],
            path.clone(),
            TrajectorySourceIdentity::Buddy,
        );

        wait_for_session_title(&session_arc, expected_title).await;
        wait_for_file_title(&path, expected_title).await;
        assert_no_trajectory_event_for(&mut rx, std::time::Duration::from_millis(200)).await;
    }

    #[tokio::test]
    async fn active_buddy_title_generation_uses_set_title_side_effects_without_generic_sse() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;
        let mut trajectory_rx = app.chat.trajectory_events_tx.subscribe();
        let chat_id = "title-buddy-set-title-event";
        let buddy_dir = dir
            .path()
            .join(".refact")
            .join("buddy")
            .join("chats")
            .join("conversations");
        let path = buddy_dir.join(format!("{chat_id}.json"));
        let expected_title = "Buddy event request";
        write_buddy_conversation_file(&path, chat_id, "New Chat").await;
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        {
            let mut session = session_arc.lock().await;
            session.thread.title = "New Chat".to_string();
            session.thread.mode = "buddy".to_string();
            session.thread.buddy_meta = Some(buddy_thread_meta());
            session.thread.is_title_generated = false;
            session.messages.push(ChatMessage::new(
                "user".to_string(),
                expected_title.to_string(),
            ));
            session.trajectory_events_tx = Some(app.chat.trajectory_events_tx.clone());
        }
        app.chat
            .sessions
            .write()
            .await
            .insert(chat_id.to_string(), session_arc.clone());

        spawn_title_generation_task(
            gcx,
            chat_id.to_string(),
            vec![json!({"role":"user","content":expected_title})],
            path.clone(),
            TrajectorySourceIdentity::Buddy,
        );

        wait_for_session_title(&session_arc, expected_title).await;
        {
            let session = session_arc.lock().await;
            assert!(session.trajectory_version > 0);
        }
        assert_no_trajectory_event_for(&mut trajectory_rx, std::time::Duration::from_millis(200))
            .await;
    }

    #[tokio::test]
    async fn title_generation_backing_file_matcher_rejects_source_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        let chat_id = "title-source-match";
        let generic_path = dir.path().join("generic.json");
        let buddy_path = dir.path().join("buddy.json");
        let task_path = dir.path().join("task.json");
        let invalid_path = dir.path().join("invalid.json");
        let task_meta = task_meta(
            "task-title-source",
            "agents",
            Some("agent-title-source"),
            Some("card-title-source"),
            None,
        );
        let task_source = TrajectorySourceIdentity::from_task_meta(&task_meta);
        write_trajectory_file(
            &generic_path,
            chat_id,
            "Generic Title",
            "2024-01-01T00:00:00Z",
        )
        .await;
        write_buddy_conversation_file(&buddy_path, chat_id, "Buddy Title").await;
        write_task_trajectory_file_with_user_message(
            &task_path,
            chat_id,
            "Task Title",
            "task title source",
            &task_meta,
        )
        .await;
        let mut invalid = sample_trajectory(chat_id, "Invalid", "2024-01-01T00:00:00Z");
        invalid["task_meta"] = serde_json::to_value(&task_meta).unwrap();
        invalid["buddy_meta"] = json!({"is_buddy_chat": true, "buddy_chat_kind": "investigation"});
        tokio::fs::write(&invalid_path, serde_json::to_string(&invalid).unwrap())
            .await
            .unwrap();

        assert!(
            title_generation_backing_file_matches(
                &generic_path,
                chat_id,
                &TrajectorySourceIdentity::Normal,
            )
            .await
        );
        assert!(
            title_generation_backing_file_matches(
                &buddy_path,
                chat_id,
                &TrajectorySourceIdentity::Buddy,
            )
            .await
        );
        assert!(title_generation_backing_file_matches(&task_path, chat_id, &task_source).await);
        assert!(
            !title_generation_backing_file_matches(
                &generic_path,
                chat_id,
                &TrajectorySourceIdentity::Buddy,
            )
            .await
        );
        assert!(
            !title_generation_backing_file_matches(
                &buddy_path,
                chat_id,
                &TrajectorySourceIdentity::Normal,
            )
            .await
        );
        assert!(
            !title_generation_backing_file_matches(
                &task_path,
                chat_id,
                &TrajectorySourceIdentity::Normal,
            )
            .await
        );
        assert!(!title_generation_backing_file_matches(&invalid_path, chat_id, &task_source).await);
    }

    #[test]
    fn trajectory_id_mismatch_list_hydration_recheck_rejects_changed_data() {
        let candidate = TrajectoryListCandidate {
            id: "hydration-chat".to_string(),
            updated_at: "2024-01-01T00:00:00Z".to_string(),
            path: PathBuf::from("hydration-chat.json"),
            indexed_meta: None,
            indexed_file_len: None,
            indexed_file_modified_unix_ms: None,
        };
        let data = TrajectoryData {
            id: "other-chat".to_string(),
            title: "Changed".to_string(),
            created_at: "2024-01-01T00:00:00Z".to_string(),
            updated_at: "2024-01-01T00:00:01Z".to_string(),
            model: "model".to_string(),
            mode: "agent".to_string(),
            tool_use: "agent".to_string(),
            messages: Vec::new(),
            extra: serde_json::Map::new(),
        };

        assert!(!trajectory_list_candidate_matches_hydrated_data(
            &candidate, &data
        ));
    }

    #[tokio::test]
    async fn trajectory_list_rebuilds_corrupt_cached_index_and_skips_index_json() {
        let dir = tempfile::tempdir().unwrap();
        let (_gcx, app) = make_app_with_workspace(dir.path()).await;
        let root = dir.path().join(".refact").join("trajectories");
        tokio::fs::create_dir_all(&root).await.unwrap();
        write_trajectory_file(
            &root.join("indexed-chat.json"),
            "indexed-chat",
            "Indexed",
            "2026-01-01T00:00:00Z",
        )
        .await;

        assert!(list_trajectories_page(app.clone(), 10, None, false)
            .await
            .unwrap()
            .items
            .iter()
            .any(|item| item.id == "indexed-chat"));

        tokio::fs::write(root.join("index.json"), "not-json")
            .await
            .unwrap();

        let page = list_trajectories_page(app, 10, None, false).await.unwrap();

        assert!(page.items.iter().any(|item| item.id == "indexed-chat"));
        let index = crate::chat::trajectory_index::read_trajectory_index(&root)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(index.entries.len(), 1);
        assert_eq!(index.entries[0].file_name, "indexed-chat.json");
    }

    #[tokio::test]
    async fn trajectory_id_mismatch_open_creates_fresh_session_without_file_state() {
        let dir = tempfile::tempdir().unwrap();
        let (_gcx, app) = make_app_with_workspace(dir.path()).await;
        let path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join("safe.json");
        write_trajectory_file(&path, "other-chat", "Mismatched", "2024-01-01T00:00:00Z").await;

        let session_arc = crate::chat::get_or_create_session_with_trajectory(
            app.clone(),
            &app.chat.sessions,
            "safe",
        )
        .await;
        let session = session_arc.lock().await;

        assert_eq!(session.chat_id, "safe");
        assert_eq!(session.thread.id, "safe");
        assert_ne!(session.thread.title, "Mismatched");
        assert!(session.messages.is_empty());
    }
    #[tokio::test]
    async fn pristine_session_is_born_clean_and_heals_from_late_trajectory() {
        let dir = tempfile::tempdir().unwrap();
        let (_gcx, app) = make_app_with_workspace(dir.path()).await;
        let chat_id = "subchat-late-trajectory";

        let session_arc = crate::chat::get_or_create_session_with_trajectory(
            app.clone(),
            &app.chat.sessions,
            chat_id,
        )
        .await;
        let mut chat_rx = {
            let session = session_arc.lock().await;
            assert!(session.messages.is_empty());
            assert!(!session.trajectory_dirty);
            session.subscribe()
        };

        let path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        write_trajectory_file_with_user_message(&path, chat_id, "Child Chat", "child work").await;

        let reopened = crate::chat::get_or_create_session_with_trajectory(
            app.clone(),
            &app.chat.sessions,
            chat_id,
        )
        .await;
        assert!(Arc::ptr_eq(&session_arc, &reopened));
        {
            let session = reopened.lock().await;
            assert_eq!(session.messages.len(), 1);
            assert_eq!(session.thread.title, "Child Chat");
        }

        let json = chat_rx.recv().await.unwrap();
        let envelope: EventEnvelope = serde_json::from_str(&json).unwrap();
        match envelope.event {
            ChatEvent::Snapshot { messages, .. } => {
                assert_eq!(messages.len(), 1);
            }
            other => panic!("expected Snapshot, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn pristine_session_save_does_not_overwrite_late_trajectory() {
        let dir = tempfile::tempdir().unwrap();
        let (_gcx, app) = make_app_with_workspace(dir.path()).await;
        let chat_id = "subchat-cleanup-no-clobber";

        let session_arc = crate::chat::get_or_create_session_with_trajectory(
            app.clone(),
            &app.chat.sessions,
            chat_id,
        )
        .await;

        let path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        write_trajectory_file_with_user_message(&path, chat_id, "Child Chat", "child work").await;

        let saved = try_save_trajectory(app.clone(), session_arc.clone())
            .await
            .unwrap();
        assert!(saved);

        let raw: serde_json::Value =
            serde_json::from_str(&tokio::fs::read_to_string(&path).await.unwrap()).unwrap();
        assert_eq!(raw["messages"].as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn stale_refresh_skips_busy_dirty_or_drafting_sessions() {
        let dir = tempfile::tempdir().unwrap();
        let (_gcx, app) = make_app_with_workspace(dir.path()).await;
        let chat_id = "subchat-refresh-guards";

        let path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        write_trajectory_file_with_user_message(&path, chat_id, "Child Chat", "child work").await;

        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        app.chat
            .sessions
            .write()
            .await
            .insert(chat_id.to_string(), session_arc.clone());

        {
            let mut session = session_arc.lock().await;
            session.trajectory_dirty = true;
        }
        assert!(!refresh_session_from_trajectory_if_stale(app.clone(), chat_id, false).await);

        {
            let mut session = session_arc.lock().await;
            session.trajectory_dirty = false;
            session.runtime.state = SessionState::Generating;
        }
        assert!(!refresh_session_from_trajectory_if_stale(app.clone(), chat_id, false).await);

        {
            let mut session = session_arc.lock().await;
            session.runtime.state = SessionState::Idle;
            session.draft_message = Some(ChatMessage::new("user".to_string(), "draft".to_string()));
        }
        assert!(!refresh_session_from_trajectory_if_stale(app.clone(), chat_id, false).await);

        {
            let mut session = session_arc.lock().await;
            session.draft_message = None;
        }
        assert!(refresh_session_from_trajectory_if_stale(app.clone(), chat_id, false).await);
        let session = session_arc.lock().await;
        assert_eq!(session.messages.len(), 1);
    }

    #[tokio::test]
    async fn completed_subchat_refresh_replaces_seed_messages() {
        let dir = tempfile::tempdir().unwrap();
        let (_gcx, app) = make_app_with_workspace(dir.path()).await;
        let chat_id = "subchat-refresh-final";

        let path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        write_trajectory_file_with_user_message(&path, chat_id, "Child Chat", "seed prompt").await;

        let session_arc = crate::chat::get_or_create_session_with_trajectory(
            app.clone(),
            &app.chat.sessions,
            chat_id,
        )
        .await;
        {
            let session = session_arc.lock().await;
            assert_eq!(session.messages.len(), 1);
            assert!(!session.trajectory_dirty);
        }

        let mut trajectory = sample_trajectory(chat_id, "Child Chat", "2024-01-01T00:00:02Z");
        trajectory["messages"] = json!([
            { "role": "user", "content": "seed prompt" },
            { "role": "assistant", "content": "final answer" }
        ]);
        tokio::fs::write(&path, serde_json::to_string(&trajectory).unwrap())
            .await
            .unwrap();

        assert!(!refresh_session_from_trajectory_if_stale(app.clone(), chat_id, false).await);
        assert!(refresh_session_from_trajectory_if_stale(app.clone(), chat_id, true).await);

        let session = session_arc.lock().await;
        assert_eq!(session.messages.len(), 2);
    }
    #[tokio::test]
    async fn buddy_child_refresh_replaces_seed_after_completion() {
        let dir = tempfile::tempdir().unwrap();
        let (_gcx, app) = make_app_with_workspace(dir.path()).await;
        let chat_id = "buddy-refresh-final";

        let buddy_path = dir
            .path()
            .join(".refact")
            .join("buddy")
            .join("chats")
            .join("conversations")
            .join(format!("{chat_id}.json"));
        write_buddy_conversation_file(&buddy_path, chat_id, "Buddy Child").await;

        let session_arc = crate::chat::get_or_create_session_with_trajectory(
            app.clone(),
            &app.chat.sessions,
            chat_id,
        )
        .await;
        {
            let session = session_arc.lock().await;
            assert_eq!(session.messages.len(), 1);
            assert!(session.thread.buddy_meta.is_some());
        }

        tokio::fs::write(
            &buddy_path,
            serde_json::to_string(&json!({
                "id": chat_id,
                "chat_id": chat_id,
                "title": "Buddy Child",
                "model": "model",
                "mode": "buddy",
                "tool_use": "agent",
                "created_at": "2024-01-01T00:00:00Z",
                "updated_at": "2024-01-01T00:00:02Z",
                "include_project_info": true,
                "checkpoints_enabled": false,
                "messages": [
                    {"role":"user","content":"hello buddy"},
                    {"role":"assistant","content":"buddy final answer"}
                ],
                "buddy_meta": {"is_buddy_chat": true, "buddy_chat_kind": "investigation"}
            }))
            .unwrap(),
        )
        .await
        .unwrap();

        assert!(refresh_session_from_trajectory_if_stale(app.clone(), chat_id, true).await);

        let session = session_arc.lock().await;
        assert_eq!(session.messages.len(), 2);
        assert!(session.thread.buddy_meta.is_some());
    }

    #[tokio::test]
    async fn reopening_child_chat_replaces_stale_seed_messages() {
        let dir = tempfile::tempdir().unwrap();
        let (_gcx, app) = make_app_with_workspace(dir.path()).await;
        let chat_id = "subchat-reopen-refresh";

        let path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        let mut trajectory = sample_trajectory(chat_id, "Child Chat", "2024-01-01T00:00:01Z");
        trajectory["parent_id"] = json!("parent-chat");
        trajectory["link_type"] = json!("subagent");
        trajectory["messages"] = json!([{ "role": "user", "content": "seed prompt" }]);
        tokio::fs::create_dir_all(path.parent().unwrap())
            .await
            .unwrap();
        tokio::fs::write(&path, serde_json::to_string(&trajectory).unwrap())
            .await
            .unwrap();

        let session_arc = crate::chat::get_or_create_session_with_trajectory(
            app.clone(),
            &app.chat.sessions,
            chat_id,
        )
        .await;
        {
            let session = session_arc.lock().await;
            assert_eq!(session.messages.len(), 1);
            assert_eq!(session.thread.parent_id.as_deref(), Some("parent-chat"));
        }

        trajectory["messages"] = json!([
            { "role": "user", "content": "seed prompt" },
            { "role": "assistant", "content": "final answer" }
        ]);
        tokio::fs::write(&path, serde_json::to_string(&trajectory).unwrap())
            .await
            .unwrap();

        let reopened = crate::chat::get_or_create_session_with_trajectory(
            app.clone(),
            &app.chat.sessions,
            chat_id,
        )
        .await;
        assert!(Arc::ptr_eq(&session_arc, &reopened));
        let session = reopened.lock().await;
        assert_eq!(session.messages.len(), 2);
    }

    #[tokio::test]
    async fn trajectory_id_missing_is_not_loaded() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join("missing-id.json");
        let mut payload = sample_trajectory("missing-id", "Missing Id", "2024-01-01T00:00:00Z");
        payload.as_object_mut().unwrap().remove("id");
        tokio::fs::create_dir_all(path.parent().unwrap())
            .await
            .unwrap();
        tokio::fs::write(&path, serde_json::to_string(&payload).unwrap())
            .await
            .unwrap();

        assert!(load_trajectory_for_chat(gcx, "missing-id").await.is_none());
    }

    #[tokio::test]
    async fn trajectory_id_mismatch_is_not_listed() {
        let dir = tempfile::tempdir().unwrap();
        let (_gcx, app) = make_app_with_workspace(dir.path()).await;
        let root = dir.path().join(".refact").join("trajectories");
        write_trajectory_file(
            &root.join("safe.json"),
            "other-chat",
            "Mismatched",
            "2024-01-01T00:00:00Z",
        )
        .await;
        write_trajectory_file(
            &root.join("valid-chat.json"),
            "valid-chat",
            "Valid",
            "2024-01-01T00:00:01Z",
        )
        .await;

        let listed = list_all_trajectories_meta(app, false).await.unwrap();
        let ids: std::collections::HashSet<_> =
            listed.iter().map(|item| item.id.as_str()).collect();

        assert!(ids.contains("valid-chat"));
        assert!(!ids.contains("other-chat"));
    }

    #[test]
    fn test_validate_trajectory_id_rejects_forward_slash() {
        assert!(validate_trajectory_id("a/b").is_err());
        assert!(validate_trajectory_id("/absolute").is_err());
    }

    #[test]
    fn test_validate_trajectory_id_rejects_backslash() {
        assert!(validate_trajectory_id("a\\b").is_err());
        assert!(validate_trajectory_id("\\windows\\path").is_err());
    }

    #[test]
    fn test_validate_trajectory_id_rejects_null_byte() {
        assert!(validate_trajectory_id("test\0id").is_err());
    }

    #[test]
    fn test_validate_trajectory_id_accepts_valid() {
        assert!(validate_trajectory_id("abc-123").is_ok());
        assert!(validate_trajectory_id("chat_456").is_ok());
        assert!(validate_trajectory_id("550e8400-e29b-41d4-a716-446655440000").is_ok());
        assert!(validate_trajectory_id("planner-task-1").is_ok());
        assert!(validate_trajectory_id("A1b2C3").is_ok());
    }

    #[test]
    fn test_validate_trajectory_id_rejects_empty() {
        assert!(validate_trajectory_id("").is_err());
    }

    #[test]
    fn test_validate_trajectory_id_rejects_too_long() {
        let long_id = "a".repeat(129);
        assert!(validate_trajectory_id(&long_id).is_err());
        let max_id = "a".repeat(128);
        assert!(validate_trajectory_id(&max_id).is_ok());
    }

    #[test]
    fn test_validate_trajectory_id_rejects_invalid_chars() {
        assert!(validate_trajectory_id("has space").is_err());
        assert!(validate_trajectory_id("has.dot").is_err());
        assert!(validate_trajectory_id("has@symbol").is_err());
        assert!(validate_trajectory_id("has#hash").is_err());
    }

    #[test]
    fn test_is_placeholder_title_new_chat() {
        assert!(is_placeholder_title("New Chat"));
        assert!(is_placeholder_title("new chat"));
        assert!(is_placeholder_title("NEW CHAT"));
        assert!(is_placeholder_title("  New Chat  "));
    }

    #[test]
    fn test_is_placeholder_title_untitled() {
        assert!(is_placeholder_title("untitled"));
        assert!(is_placeholder_title("Untitled"));
        assert!(is_placeholder_title("UNTITLED"));
    }

    #[test]
    fn test_is_placeholder_title_empty() {
        assert!(is_placeholder_title(""));
        assert!(is_placeholder_title("   "));
    }

    #[test]
    fn test_is_placeholder_title_real_titles() {
        assert!(!is_placeholder_title("Fix authentication bug"));
        assert!(!is_placeholder_title("Refactor database module"));
        assert!(!is_placeholder_title("New feature implementation"));
    }

    #[test]
    fn test_clean_generated_title_strips_quotes() {
        assert_eq!(clean_generated_title("\"Hello World\""), "Hello World");
        assert_eq!(clean_generated_title("'Hello World'"), "Hello World");
        assert_eq!(clean_generated_title("`Hello World`"), "Hello World");
    }

    #[test]
    fn test_clean_generated_title_strips_asterisks() {
        assert_eq!(clean_generated_title("*Bold Title*"), "Bold Title");
        assert_eq!(clean_generated_title("**Strong Title**"), "Strong Title");
    }

    #[test]
    fn test_clean_generated_title_collapses_whitespace() {
        assert_eq!(clean_generated_title("Hello   World"), "Hello World");
        assert_eq!(
            clean_generated_title("  Multiple   Spaces  "),
            "Multiple Spaces"
        );
    }

    #[test]
    fn test_clean_generated_title_removes_newlines() {
        assert_eq!(clean_generated_title("Hello\nWorld"), "Hello World");
        assert_eq!(
            clean_generated_title("Line1\nLine2\nLine3"),
            "Line1 Line2 Line3"
        );
    }

    #[test]
    fn test_clean_generated_title_truncates_long() {
        let long_title = "A".repeat(100);
        let result = clean_generated_title(&long_title);
        assert!(result.len() <= 60);
        assert!(result.ends_with("..."));
    }

    #[test]
    fn test_clean_generated_title_preserves_short() {
        let short_title = "Short Title";
        let result = clean_generated_title(short_title);
        assert_eq!(result, "Short Title");
        assert!(!result.ends_with("..."));
    }

    #[test]
    fn test_trajectory_meta_title_truncates_oversized_stored_title() {
        let long_title = "A".repeat(1024 * 1024);
        let result = trajectory_meta_title(&long_title);
        assert_eq!(result.chars().count(), TRAJECTORY_META_TITLE_MAX_CHARS);
        assert!(result.ends_with("..."));
    }

    #[test]
    fn test_trajectory_meta_uses_bounded_title() {
        let data = TrajectoryData {
            id: "big-title-chat".to_string(),
            title: "B".repeat(1024 * 1024),
            created_at: "2024-01-01T00:00:00Z".to_string(),
            updated_at: "2024-01-01T00:00:00Z".to_string(),
            model: "model".to_string(),
            mode: "agent".to_string(),
            tool_use: "agent".to_string(),
            messages: Vec::new(),
            extra: serde_json::Map::new(),
        };
        let meta = trajectory_data_to_meta(&data);
        assert_eq!(meta.title.chars().count(), TRAJECTORY_META_TITLE_MAX_CHARS);
    }

    #[test]
    fn test_cursor_precedes_item_matches_descending_order() {
        assert!(cursor_precedes_item(
            ("2024-01-01T00:00:00Z", "a"),
            ("2024-01-02T00:00:00Z", "b"),
        ));
        assert!(cursor_precedes_item(
            ("2024-01-01T00:00:00Z", "a"),
            ("2024-01-01T00:00:00Z", "b"),
        ));
        assert!(!cursor_precedes_item(
            ("2024-01-03T00:00:00Z", "a"),
            ("2024-01-02T00:00:00Z", "b"),
        ));
    }

    #[test]
    fn test_extract_first_user_message_string_content() {
        let messages = vec![
            json!({"role": "system", "content": "You are helpful"}),
            json!({"role": "user", "content": "Hello there"}),
        ];
        let result = extract_first_user_message(&messages);
        assert_eq!(result, Some("Hello there".to_string()));
    }

    #[test]
    fn test_extract_first_user_message_array_content_text() {
        let messages =
            vec![json!({"role": "user", "content": [{"type": "text", "text": "Array text"}]})];
        let result = extract_first_user_message(&messages);
        assert_eq!(result, Some("Array text".to_string()));
    }

    #[test]
    fn test_extract_first_user_message_array_content_m_content() {
        let messages = vec![
            json!({"role": "user", "content": [{"m_type": "text", "m_content": "M content"}]}),
        ];
        let result = extract_first_user_message(&messages);
        assert_eq!(result, Some("M content".to_string()));
    }

    #[test]
    fn test_extract_first_user_message_skips_empty() {
        let messages = vec![
            json!({"role": "user", "content": "   "}),
            json!({"role": "user", "content": "Second message"}),
        ];
        let result = extract_first_user_message(&messages);
        assert_eq!(result, Some("Second message".to_string()));
    }

    #[test]
    fn test_extract_first_user_message_truncates() {
        let long_message = "A".repeat(300);
        let messages = vec![json!({"role": "user", "content": long_message})];
        let result = extract_first_user_message(&messages);
        assert!(result.is_some());
        assert!(result.unwrap().len() <= 200);
    }

    #[test]
    fn test_extract_first_user_message_no_user() {
        let messages = vec![
            json!({"role": "system", "content": "System prompt"}),
            json!({"role": "assistant", "content": "Hello"}),
        ];
        let result = extract_first_user_message(&messages);
        assert!(result.is_none());
    }

    #[test]
    fn test_build_title_generation_context_skips_tool_messages() {
        let messages = vec![
            json!({"role": "user", "content": "User message"}),
            json!({"role": "tool", "content": "Tool result"}),
            json!({"role": "assistant", "content": "Response"}),
        ];
        let context = build_title_generation_context(&messages);
        assert!(context.contains("User message"));
        assert!(context.contains("Response"));
        assert!(!context.contains("Tool result"));
    }

    #[test]
    fn test_build_title_generation_context_skips_context_file() {
        let messages = vec![
            json!({"role": "user", "content": "Question"}),
            json!({"role": "context_file", "content": "File contents"}),
        ];
        let context = build_title_generation_context(&messages);
        assert!(context.contains("Question"));
        assert!(!context.contains("File contents"));
    }

    #[test]
    fn build_title_generation_context_skips_hidden_plan_and_event_roles() {
        let messages = vec![
            json!({"role": "user", "content": "Visible user request"}),
            json!({
                "role": "plan",
                "content": "Hidden base plan must not title the chat",
                "extra": {"plan": {"mode": "agent", "version": 1}}
            }),
            json!({
                "role": "event",
                "content": "Hidden plan delta must not title the chat",
                "extra": {
                    "event": {
                        "subkind": "plan_delta",
                        "source": "tool.update_plan",
                        "payload": {"seq": 1}
                    }
                }
            }),
            json!({"role": "assistant", "content": "Visible assistant response"}),
        ];

        let context = build_title_generation_context(&messages);

        assert!(context.contains("Visible user request"));
        assert!(context.contains("Visible assistant response"));
        assert!(!context.contains("Hidden base plan"));
        assert!(!context.contains("Hidden plan delta"));
    }

    #[test]
    fn build_title_generation_context_skips_ui_only_error() {
        let messages = vec![
            json!({"role": "error", "content": "context_length_exceeded", "_ui_only": true}),
            json!({"role": "user", "content": "Implement title filtering"}),
        ];
        let context = build_title_generation_context(&messages);
        assert!(context.contains("Implement title filtering"));
        assert!(!context.contains("context_length_exceeded"));
    }

    #[test]
    fn build_title_generation_context_skips_ui_only_reactive_compaction_report() {
        let messages = vec![
            json!({
                "role": "summarization",
                "content": "Legacy diagnostic report",
                "summarization_tier": "legacy_reactive",
                "_ui_only": true
            }),
            json!({"role": "user", "content": "Fix sanitizers"}),
        ];
        let context = build_title_generation_context(&messages);
        assert!(context.contains("Fix sanitizers"));
        assert!(!context.contains("Legacy diagnostic report"));
    }

    #[test]
    fn build_title_generation_context_skips_extra_ui_only_reactive_report() {
        let messages = vec![
            json!({
                "role": "summarization",
                "content": "Legacy diagnostic report",
                "summarization_tier": "legacy_reactive",
                "extra": {"_ui_only": true}
            }),
            json!({"role": "user", "content": "Fix sanitizers"}),
        ];
        let context = build_title_generation_context(&messages);
        assert!(context.contains("Fix sanitizers"));
        assert!(!context.contains("Legacy diagnostic report"));
    }

    #[test]
    fn test_build_title_generation_context_limits_messages() {
        let messages: Vec<_> = (0..10)
            .map(|i| json!({"role": "user", "content": format!("Message {}", i)}))
            .collect();
        let context = build_title_generation_context(&messages);
        assert!(context.contains("Message 0"));
        assert!(context.contains("Message 5"));
        assert!(!context.contains("Message 9"));
    }

    #[test]
    fn test_build_title_generation_context_truncates_long_messages() {
        let long_content = "A".repeat(1000);
        let messages = vec![json!({"role": "user", "content": long_content})];
        let context = build_title_generation_context(&messages);
        assert!(context.len() < 600);
    }

    #[test]
    fn test_fix_tool_call_indexes_sets_missing() {
        use crate::call_validation::{ChatToolCall, ChatToolFunction};
        let mut messages = vec![ChatMessage {
            role: "assistant".to_string(),
            tool_calls: Some(vec![
                ChatToolCall {
                    id: "call_1".to_string(),
                    index: None,
                    function: ChatToolFunction {
                        name: "test".to_string(),
                        arguments: "{}".to_string(),
                    },
                    tool_type: "function".to_string(),
                    extra_content: None,
                    started_at_ms: None,
                    completed_at_ms: None,
                },
                ChatToolCall {
                    id: "call_2".to_string(),
                    index: None,
                    function: ChatToolFunction {
                        name: "test2".to_string(),
                        arguments: "{}".to_string(),
                    },
                    tool_type: "function".to_string(),
                    extra_content: None,
                    started_at_ms: None,
                    completed_at_ms: None,
                },
            ]),
            ..Default::default()
        }];
        fix_tool_call_indexes(&mut messages);
        let tool_calls = messages[0].tool_calls.as_ref().unwrap();
        assert_eq!(tool_calls[0].index, Some(0));
        assert_eq!(tool_calls[1].index, Some(1));
    }

    #[test]
    fn test_fix_tool_call_indexes_preserves_existing() {
        use crate::call_validation::{ChatToolCall, ChatToolFunction};
        let mut messages = vec![ChatMessage {
            role: "assistant".to_string(),
            tool_calls: Some(vec![ChatToolCall {
                id: "call_1".to_string(),
                index: Some(5),
                function: ChatToolFunction {
                    name: "test".to_string(),
                    arguments: "{}".to_string(),
                },
                tool_type: "function".to_string(),
                extra_content: None,
                started_at_ms: None,
                completed_at_ms: None,
            }]),
            ..Default::default()
        }];
        fix_tool_call_indexes(&mut messages);
        let tool_calls = messages[0].tool_calls.as_ref().unwrap();
        assert_eq!(tool_calls[0].index, Some(5));
    }

    #[test]
    fn test_calculate_token_totals_from_messages_with_usage() {
        let messages = vec![
            json!({
                "role": "assistant",
                "content": "Hello",
                "usage": {
                    "prompt_tokens": 10,
                    "completion_tokens": 5,
                    "total_tokens": 15,
                    "cache_read_input_tokens": 3,
                    "cache_creation_input_tokens": 2,
                    "metering_usd": {
                        "prompt_usd": 0.001,
                        "generated_usd": 0.002,
                        "total_usd": 0.003
                    }
                }
            }),
            json!({
                "role": "assistant",
                "content": "World",
                "usage": {
                    "prompt_tokens": 20,
                    "completion_tokens": 10,
                    "total_tokens": 30,
                    "metering_usd": {
                        "prompt_usd": 0.002,
                        "generated_usd": 0.004,
                        "total_usd": 0.006
                    }
                }
            }),
        ];
        let totals = calculate_token_totals_from_messages(&messages);
        assert_eq!(totals.prompt_tokens, 30);
        assert_eq!(totals.completion_tokens, 15);
        assert_eq!(totals.total_tokens, 45);
        assert_eq!(totals.cache_read_tokens, 3);
        assert_eq!(totals.cache_creation_tokens, 2);
        let cost = totals.cost_usd.unwrap();
        assert!((cost - 0.009).abs() < 1e-9);
    }

    #[test]
    fn test_calculate_token_totals_from_messages_no_usage() {
        let messages = vec![
            json!({"role": "user", "content": "Hello"}),
            json!({"role": "assistant", "content": "Hi"}),
        ];
        let totals = calculate_token_totals_from_messages(&messages);
        assert_eq!(totals.prompt_tokens, 0);
        assert_eq!(totals.completion_tokens, 0);
        assert_eq!(totals.total_tokens, 0);
        assert_eq!(totals.cache_read_tokens, 0);
        assert_eq!(totals.cache_creation_tokens, 0);
        assert!(totals.cost_usd.is_none());
    }

    #[test]
    fn test_calculate_token_totals_from_messages_null_usage() {
        let messages = vec![json!({"role": "assistant", "content": "Hi", "usage": null})];
        let totals = calculate_token_totals_from_messages(&messages);
        assert_eq!(totals.prompt_tokens, 0);
        assert!(totals.cost_usd.is_none());
    }

    #[test]
    fn test_calculate_token_totals_from_messages_alias_keys() {
        let messages = vec![json!({
            "role": "assistant",
            "content": "Hi",
            "usage": {
                "prompt_tokens": 5,
                "completion_tokens": 3,
                "total_tokens": 8,
                "cache_read_tokens": 7,
                "cache_creation_tokens": 4,
            }
        })];
        let totals = calculate_token_totals_from_messages(&messages);
        assert_eq!(totals.cache_read_tokens, 7);
        assert_eq!(totals.cache_creation_tokens, 4);
    }

    #[test]
    fn test_calculate_token_totals_from_chat_messages_with_usage() {
        use crate::call_validation::{ChatUsage, MeteringUsd};
        let messages = vec![
            ChatMessage {
                role: "assistant".to_string(),
                usage: Some(ChatUsage {
                    prompt_tokens: 100,
                    completion_tokens: 50,
                    total_tokens: 150,
                    cache_read_tokens: Some(10),
                    cache_creation_tokens: Some(5),
                    metering_usd: Some(MeteringUsd {
                        prompt_usd: 0.01,
                        generated_usd: 0.02,
                        cache_read_usd: None,
                        cache_creation_usd: None,
                        total_usd: 0.03,
                    }),
                }),
                ..Default::default()
            },
            ChatMessage {
                role: "assistant".to_string(),
                usage: Some(ChatUsage {
                    prompt_tokens: 200,
                    completion_tokens: 100,
                    total_tokens: 300,
                    cache_read_tokens: None,
                    cache_creation_tokens: None,
                    metering_usd: None,
                }),
                ..Default::default()
            },
        ];
        let totals = calculate_token_totals_from_chat_messages(&messages);
        assert_eq!(totals.prompt_tokens, 300);
        assert_eq!(totals.completion_tokens, 150);
        assert_eq!(totals.total_tokens, 450);
        assert_eq!(totals.cache_read_tokens, 10);
        assert_eq!(totals.cache_creation_tokens, 5);
        let cost = totals.cost_usd.unwrap();
        assert!((cost - 0.03).abs() < 1e-9);
    }

    #[test]
    fn test_calculate_token_totals_from_chat_messages_no_usage() {
        let messages = vec![ChatMessage {
            role: "user".to_string(),
            ..Default::default()
        }];
        let totals = calculate_token_totals_from_chat_messages(&messages);
        assert_eq!(totals.prompt_tokens, 0);
        assert!(totals.cost_usd.is_none());
    }

    fn mark_active_compression(session: &mut ChatSession, phase: CompressionPhase) {
        session.is_compressing = true;
        session.runtime.is_compressing = true;
        session.compression_phase = Some(phase);
        session.runtime.compression_phase = Some(phase);
        session.compression_reason = Some(CompressionReason::PressureLow);
        session.runtime.compression_reason = Some(CompressionReason::PressureLow);
        session.compression_attempt_generation = 9;
        session.active_compression_attempt = Some(9);
    }

    async fn assert_external_remove_clears_active_compression(phase: CompressionPhase) {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;
        let chat_id = format!("external-remove-{:?}", phase).to_lowercase();
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.clone())));
        let mut chat_rx = {
            let mut session = session_arc.lock().await;
            session.messages.push(ChatMessage::new(
                "user".to_string(),
                "before delete".to_string(),
            ));
            mark_active_compression(&mut session, phase);
            session.trajectory_dirty = false;
            session.subscribe()
        };
        app.chat
            .sessions
            .write()
            .await
            .insert(chat_id.clone(), session_arc.clone());

        process_trajectory_change(gcx, &chat_id, true).await;

        let session = session_arc.lock().await;
        assert!(session.messages.is_empty());
        assert!(!session.is_compressing);
        assert!(!session.runtime.is_compressing);
        assert_eq!(session.compression_phase, None);
        assert_eq!(session.runtime.compression_phase, None);
        assert_eq!(session.compression_reason, None);
        assert_eq!(session.runtime.compression_reason, None);
        assert_eq!(session.active_compression_attempt, None);
        drop(session);

        let json = chat_rx.recv().await.unwrap();
        let envelope: EventEnvelope = serde_json::from_str(&json).unwrap();
        match envelope.event {
            ChatEvent::Snapshot {
                runtime, messages, ..
            } => {
                assert!(messages.is_empty());
                assert!(!runtime.is_compressing);
                assert_eq!(runtime.compression_phase, None);
                assert_eq!(runtime.compression_reason, None);
            }
            other => panic!("expected Snapshot, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn external_remove_clears_checking_compression_state_and_snapshot() {
        assert_external_remove_clears_active_compression(CompressionPhase::Checking).await;
    }

    #[tokio::test]
    async fn external_remove_clears_running_compression_state_and_snapshot() {
        assert_external_remove_clears_active_compression(CompressionPhase::Running).await;
    }

    #[tokio::test]
    async fn immediate_external_delete_with_loadable_trajectory_reloads_instead_of_clearing() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;
        let mut rx = app.chat.trajectory_events_tx.subscribe();
        let chat_id = "immediate-delete-loadable-reloads";
        let path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        write_trajectory_file_with_user_message(
            &path,
            chat_id,
            "Reloaded After Stale Delete",
            "recreated message",
        )
        .await;
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        let mut chat_rx = {
            let mut session = session_arc.lock().await;
            session.messages.push(ChatMessage::new(
                "user".to_string(),
                "old in-memory message".to_string(),
            ));
            session.thread.title = "Old In Memory".to_string();
            session.external_reload_pending = Some(ExternalReloadPending::delete(
                TrajectorySourceIdentity::Normal,
            ));
            session.runtime.state = SessionState::Idle;
            session.trajectory_dirty = false;
            session.subscribe()
        };
        app.chat
            .sessions
            .write()
            .await
            .insert(chat_id.to_string(), session_arc.clone());

        process_trajectory_change(gcx, chat_id, true).await;

        let event = wait_for_trajectory_event(&mut rx, chat_id).await;
        assert_eq!(event.event_type, "updated");
        assert_eq!(event.title.as_deref(), Some("Reloaded After Stale Delete"));

        let session = session_arc.lock().await;
        assert_eq!(session.external_reload_pending, None);
        assert_eq!(session.thread.title, "Reloaded After Stale Delete");
        assert_eq!(session.messages.len(), 1);
        assert_eq!(
            session.messages[0].content.content_text_only(),
            "recreated message"
        );
        drop(session);

        let json = chat_rx.recv().await.unwrap();
        let envelope: EventEnvelope = serde_json::from_str(&json).unwrap();
        match envelope.event {
            ChatEvent::Snapshot {
                thread, messages, ..
            } => {
                assert_eq!(thread.title, "Reloaded After Stale Delete");
                assert_eq!(messages.len(), 1);
            }
            other => panic!("expected Snapshot, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn immediate_external_true_delete_still_clears_reloadable_session() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;
        let mut rx = app.chat.trajectory_events_tx.subscribe();
        let chat_id = "immediate-true-delete-clears";
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        let mut chat_rx = {
            let mut session = session_arc.lock().await;
            session.messages.push(ChatMessage::new(
                "user".to_string(),
                "before true delete".to_string(),
            ));
            session.thread.title = "Delete Me".to_string();
            session.runtime.state = SessionState::Idle;
            session.trajectory_dirty = false;
            session.subscribe()
        };
        app.chat
            .sessions
            .write()
            .await
            .insert(chat_id.to_string(), session_arc.clone());

        process_trajectory_change(gcx, chat_id, true).await;

        let event = wait_for_trajectory_event(&mut rx, chat_id).await;
        assert_eq!(event.event_type, "deleted");
        assert_eq!(event.title, None);

        let session = session_arc.lock().await;
        assert_eq!(session.external_reload_pending, None);
        assert!(session.messages.is_empty());
        assert_eq!(session.thread.id, chat_id);
        assert_eq!(session.thread.title, ThreadParams::default().title);
        drop(session);

        let json = chat_rx.recv().await.unwrap();
        let envelope: EventEnvelope = serde_json::from_str(&json).unwrap();
        match envelope.event {
            ChatEvent::Snapshot {
                thread, messages, ..
            } => {
                assert_eq!(thread.id, chat_id);
                assert!(messages.is_empty());
            }
            other => panic!("expected Snapshot, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn final_delete_guard_defers_when_session_is_not_reloadable() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let chat_id = "final-delete-guard-defers";
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        {
            let mut session = session_arc.lock().await;
            session.messages.push(ChatMessage::new(
                "user".to_string(),
                "keep while busy".to_string(),
            ));
            session.external_reload_pending = Some(ExternalReloadPending::delete(
                TrajectorySourceIdentity::Normal,
            ));
            session.runtime.state = SessionState::Generating;
            session.trajectory_dirty = false;
        }

        let outcome = apply_external_delete_with_revalidation(
            gcx,
            session_arc.clone(),
            chat_id,
            Some(ExternalReloadPending::delete(
                TrajectorySourceIdentity::Normal,
            )),
            None,
        )
        .await;

        let session = session_arc.lock().await;
        assert!(matches!(
            outcome,
            ExternalDeleteRevalidationOutcome::Deleted {
                applied_to_session: false
            }
        ));
        assert_eq!(
            session.external_reload_pending,
            Some(ExternalReloadPending::delete(
                TrajectorySourceIdentity::Normal
            ))
        );
        assert_eq!(session.messages.len(), 1);
        assert_eq!(session.thread.id, chat_id);
    }

    #[tokio::test]
    async fn stale_pending_delete_helper_does_not_overwrite_newer_update_pending() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let chat_id = "stale-delete-keeps-update-pending";
        let path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        write_trajectory_file_with_user_message(
            &path,
            chat_id,
            "Newer Update",
            "newer update message",
        )
        .await;
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        {
            let mut session = session_arc.lock().await;
            session.external_reload_pending = Some(ExternalReloadPending::update(
                TrajectorySourceIdentity::Normal,
            ));
            session.runtime.state = SessionState::Generating;
            session.trajectory_dirty = false;
        }

        let outcome = apply_external_delete_with_revalidation(
            gcx,
            session_arc.clone(),
            chat_id,
            Some(ExternalReloadPending::delete(
                TrajectorySourceIdentity::Normal,
            )),
            None,
        )
        .await;

        let session = session_arc.lock().await;
        assert!(matches!(
            outcome,
            ExternalDeleteRevalidationOutcome::NoopStalePending
        ));
        assert_eq!(
            session.external_reload_pending,
            Some(ExternalReloadPending::update(
                TrajectorySourceIdentity::Normal
            ))
        );
    }

    #[tokio::test]
    async fn pending_external_delete_applies_when_session_becomes_reloadable() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;
        let mut rx = app.chat.trajectory_events_tx.subscribe();
        let chat_id = "pending-external-delete";
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        let mut chat_rx = {
            let mut session = session_arc.lock().await;
            session.messages.push(ChatMessage::new(
                "user".to_string(),
                "before deferred delete".to_string(),
            ));
            session.thread.title = "Delete Pending".to_string();
            session.thread.reactive_compact_attempts = Some(2);
            session.created_at = "2024-01-01T00:00:00Z".to_string();
            session.wake_up_at = Some(chrono::Utc::now() + chrono::Duration::minutes(10));
            session.waiting_for_card_ids = vec!["card-stale".to_string()];
            session.runtime.state = SessionState::Generating;
            session.trajectory_dirty = true;
            mark_active_compression(&mut session, CompressionPhase::Running);
            session.subscribe()
        };
        app.chat
            .sessions
            .write()
            .await
            .insert(chat_id.to_string(), session_arc.clone());

        process_trajectory_change(gcx.clone(), chat_id, true).await;
        {
            let session = session_arc.lock().await;
            assert_eq!(
                session.external_reload_pending,
                Some(ExternalReloadPending::delete(
                    TrajectorySourceIdentity::Normal
                ))
            );
            assert_eq!(session.messages.len(), 1);
            assert!(session.active_compression_attempt.is_some());
        }

        let event = wait_for_trajectory_event(&mut rx, chat_id).await;
        assert_eq!(event.event_type, "deleted");

        {
            let mut session = session_arc.lock().await;
            session.runtime.state = SessionState::Idle;
            session.trajectory_dirty = false;
        }
        check_external_reload_pending(gcx, session_arc.clone()).await;

        let session = session_arc.lock().await;
        assert_eq!(session.external_reload_pending, None);
        assert!(session.messages.is_empty());
        assert_eq!(session.thread.id, chat_id);
        assert_eq!(session.thread.title, ThreadParams::default().title);
        assert_eq!(session.thread.reactive_compact_attempts, None);
        assert_ne!(session.created_at, "2024-01-01T00:00:00Z");
        assert!(session.wake_up_at.is_none());
        assert!(session.waiting_for_card_ids.is_empty());
        assert!(!session.is_compressing);
        assert!(!session.runtime.is_compressing);
        assert_eq!(session.compression_phase, None);
        assert_eq!(session.runtime.compression_phase, None);
        assert_eq!(session.compression_reason, None);
        assert_eq!(session.runtime.compression_reason, None);
        assert_eq!(session.active_compression_attempt, None);
        drop(session);

        let json = chat_rx.recv().await.unwrap();
        let envelope: EventEnvelope = serde_json::from_str(&json).unwrap();
        match envelope.event {
            ChatEvent::Snapshot {
                thread,
                runtime,
                messages,
                ..
            } => {
                assert_eq!(thread.id, chat_id);
                assert_eq!(thread.reactive_compact_attempts, None);
                assert!(messages.is_empty());
                assert!(!runtime.is_compressing);
                assert_eq!(runtime.compression_phase, None);
            }
            other => panic!("expected Snapshot, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn pending_external_delete_revalidates_recreated_trajectory_before_clearing() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;
        let mut rx = app.chat.trajectory_events_tx.subscribe();
        let chat_id = "pending-delete-recreated-reloads";
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        let mut chat_rx = {
            let mut session = session_arc.lock().await;
            session.messages.push(ChatMessage::new(
                "user".to_string(),
                "before stale delete".to_string(),
            ));
            session.thread.title = "Busy Before Delete".to_string();
            session.runtime.state = SessionState::Generating;
            session.trajectory_dirty = true;
            session.subscribe()
        };
        app.chat
            .sessions
            .write()
            .await
            .insert(chat_id.to_string(), session_arc.clone());

        process_trajectory_change(gcx.clone(), chat_id, true).await;
        {
            let session = session_arc.lock().await;
            assert_eq!(
                session.external_reload_pending,
                Some(ExternalReloadPending::delete(
                    TrajectorySourceIdentity::Normal
                ))
            );
            assert_eq!(session.thread.title, "Busy Before Delete");
        }

        let event = wait_for_trajectory_event(&mut rx, chat_id).await;
        assert_eq!(event.event_type, "deleted");

        let path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        write_trajectory_file_with_user_message(
            &path,
            chat_id,
            "Recreated Before Apply",
            "recreated before pending delete",
        )
        .await;
        {
            let mut session = session_arc.lock().await;
            session.runtime.state = SessionState::Idle;
            session.trajectory_dirty = false;
        }
        check_external_reload_pending(gcx, session_arc.clone()).await;

        let session = session_arc.lock().await;
        assert_eq!(session.external_reload_pending, None);
        assert_eq!(session.thread.title, "Recreated Before Apply");
        assert_eq!(session.messages.len(), 1);
        assert_eq!(
            session.messages[0].content.content_text_only(),
            "recreated before pending delete"
        );
        drop(session);

        let json = chat_rx.recv().await.unwrap();
        let envelope: EventEnvelope = serde_json::from_str(&json).unwrap();
        match envelope.event {
            ChatEvent::Snapshot {
                thread, messages, ..
            } => {
                assert_eq!(thread.title, "Recreated Before Apply");
                assert_eq!(messages.len(), 1);
            }
            other => panic!("expected Snapshot, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn generic_watcher_remove_does_not_clear_active_buddy_session() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;
        let chat_id = "active-buddy-remove-guard";
        let buddy_path = dir
            .path()
            .join(".refact")
            .join("buddy")
            .join("chats")
            .join("conversations")
            .join(format!("{chat_id}.json"));
        let normal_path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        write_trajectory_file(
            &normal_path,
            chat_id,
            "Removed Normal",
            "2024-01-01T00:00:01Z",
        )
        .await;
        write_buddy_conversation_file(&buddy_path, chat_id, "Active Buddy File").await;
        tokio::fs::remove_file(&normal_path).await.unwrap();
        let mut rx = app.chat.trajectory_events_tx.subscribe();
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        let mut chat_rx = {
            let mut session = session_arc.lock().await;
            session.thread.title = "Active Buddy Session".to_string();
            session.thread.mode = "buddy".to_string();
            session.thread.buddy_meta = Some(buddy_thread_meta());
            session.external_reload_pending = Some(ExternalReloadPending::update(
                TrajectorySourceIdentity::Buddy,
            ));
            session.messages.push(ChatMessage::new(
                "user".to_string(),
                "keep buddy message".to_string(),
            ));
            session.runtime.state = SessionState::Idle;
            session.trajectory_dirty = false;
            session.subscribe()
        };
        app.chat
            .sessions
            .write()
            .await
            .insert(chat_id.to_string(), session_arc.clone());

        process_trajectory_change(gcx.clone(), chat_id, true).await;
        let event = wait_for_trajectory_event(&mut rx, chat_id).await;
        assert_eq!(event.event_type, "deleted");
        assert_eq!(event.title, None);
        assert!(tokio::fs::try_exists(&buddy_path).await.unwrap());
        assert!(!tokio::fs::try_exists(&normal_path).await.unwrap());

        let session = session_arc.lock().await;
        assert_eq!(
            session.external_reload_pending,
            Some(ExternalReloadPending::update(
                TrajectorySourceIdentity::Buddy
            ))
        );
        assert_eq!(session.thread.title, "Active Buddy Session");
        assert!(session.thread.buddy_meta.is_some());
        assert_eq!(session.messages.len(), 1);
        assert_eq!(
            session.messages[0].content.content_text_only(),
            "keep buddy message"
        );
        drop(session);

        assert_no_chat_event_for(&mut chat_rx, std::time::Duration::from_millis(100)).await;
    }

    #[tokio::test]
    async fn generic_watcher_remove_with_active_buddy_and_generic_fallback_emits_updated() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;
        let mut rx = app.chat.trajectory_events_tx.subscribe();
        let chat_id = "active-buddy-remove-generic-fallback";
        let fallback_path = dir
            .path()
            .join(".refact")
            .join("tasks")
            .join("task-active-buddy-fallback")
            .join("trajectories")
            .join("planner")
            .join(format!("{chat_id}.json"));
        write_trajectory_file_with_metadata(
            &fallback_path,
            chat_id,
            "Active Buddy Fallback",
            "2024-01-01T00:00:01Z",
            "fallback survives active buddy",
        )
        .await;
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        let mut chat_rx = {
            let mut session = session_arc.lock().await;
            session.thread.title = "Active Buddy Session".to_string();
            session.thread.mode = "buddy".to_string();
            session.thread.buddy_meta = Some(buddy_thread_meta());
            session.thread.worktree = Some(trajectory_worktree_sample_with_id("buddy-remove-wt"));
            session.external_reload_pending = Some(ExternalReloadPending::delete(
                TrajectorySourceIdentity::Buddy,
            ));
            session.runtime.state = SessionState::Generating;
            session.runtime.error = Some("buddy remove fallback error".to_string());
            session.messages.push(ChatMessage::new(
                "user".to_string(),
                "keep active buddy with fallback".to_string(),
            ));
            session.trajectory_dirty = false;
            session.subscribe()
        };
        app.chat
            .sessions
            .write()
            .await
            .insert(chat_id.to_string(), session_arc.clone());

        process_trajectory_change(gcx.clone(), chat_id, true).await;

        let event = wait_for_trajectory_event(&mut rx, chat_id).await;
        assert_eq!(event.event_type, "updated");
        assert_eq!(event.title.as_deref(), Some("Active Buddy Fallback"));
        assert_eq!(event.task_id.as_deref(), Some("task-active-buddy-fallback"));
        assert_eq!(event.task_role.as_deref(), Some("planner"));
        assert_eq!(event.message_count, Some(1));
        assert_eq!(event.session_state.as_deref(), Some("idle"));
        assert_eq!(event.error, None);
        assert_ne!(
            event.worktree.as_ref().map(|worktree| worktree.id.as_str()),
            Some("buddy-remove-wt")
        );

        let session = session_arc.lock().await;
        assert_eq!(
            session.external_reload_pending,
            Some(ExternalReloadPending::delete(
                TrajectorySourceIdentity::Buddy
            ))
        );
        assert_eq!(session.thread.title, "Active Buddy Session");
        assert_eq!(session.thread.mode, "buddy");
        assert!(session.thread.buddy_meta.is_some());
        assert_eq!(session.messages.len(), 1);
        assert_eq!(
            session.messages[0].content.content_text_only(),
            "keep active buddy with fallback"
        );
        drop(session);

        assert_no_chat_event_for(&mut chat_rx, std::time::Duration::from_millis(100)).await;
    }

    #[tokio::test]
    async fn generic_watcher_remove_with_active_buddy_persists_repaired_fallback() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;
        let mut rx = app.chat.trajectory_events_tx.subscribe();
        let chat_id = "active-buddy-remove-repaired-fallback";
        let fallback_path = dir
            .path()
            .join(".refact")
            .join("tasks")
            .join("task-active-buddy-repair-fallback")
            .join("trajectories")
            .join("planner")
            .join(format!("{chat_id}.json"));
        tokio::fs::create_dir_all(fallback_path.parent().unwrap())
            .await
            .unwrap();
        tokio::fs::write(
            &fallback_path,
            serde_json::to_string(&json!({
                "id": chat_id,
                "title": "Active Buddy Repaired Fallback",
                "model": "model",
                "mode": "task_planner",
                "tool_use": "agent",
                "parent_id": "source-chat",
                "link_type": "mode_transition",
                "previous_response_id": "resp_source",
                "messages": [
                    {"role":"system","content":"target task planner system"},
                    {"role":"user","content":"fallback survives active buddy repair"}
                ],
                "frozen_request_prefix": {
                    "schema_version": 1,
                    "created_at": "2026-05-29T00:00:00Z",
                    "system_prompt": "source system",
                    "tools_canonical": [{"type":"function","function":{"name":"source_tool"}}]
                },
                "claude_code_identity": {
                    "device_id":"source-device",
                    "session_id":"source-session"
                },
                "created_at": "2024-01-01T00:00:00Z",
                "updated_at": "2024-01-01T00:00:01Z",
                "include_project_info": true,
                "checkpoints_enabled": true,
                "custom_future_field": {"keep": true}
            }))
            .unwrap(),
        )
        .await
        .unwrap();
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        let mut chat_rx = {
            let mut session = session_arc.lock().await;
            session.thread.title = "Active Buddy Session".to_string();
            session.thread.mode = "buddy".to_string();
            session.thread.buddy_meta = Some(buddy_thread_meta());
            session.external_reload_pending = Some(ExternalReloadPending::delete(
                TrajectorySourceIdentity::Buddy,
            ));
            session.messages.push(ChatMessage::new(
                "user".to_string(),
                "keep active buddy during repaired fallback".to_string(),
            ));
            session.runtime.state = SessionState::Generating;
            session.trajectory_dirty = false;
            session.subscribe()
        };
        app.chat
            .sessions
            .write()
            .await
            .insert(chat_id.to_string(), session_arc.clone());

        process_trajectory_change(gcx.clone(), chat_id, true).await;

        let event = wait_for_trajectory_event(&mut rx, chat_id).await;
        assert_eq!(event.event_type, "updated");
        assert_eq!(
            event.title.as_deref(),
            Some("Active Buddy Repaired Fallback")
        );
        assert_eq!(
            event.task_id.as_deref(),
            Some("task-active-buddy-repair-fallback")
        );
        assert_eq!(event.session_state.as_deref(), Some("idle"));

        let session = session_arc.lock().await;
        assert_eq!(
            session.external_reload_pending,
            Some(ExternalReloadPending::delete(
                TrajectorySourceIdentity::Buddy
            ))
        );
        assert_eq!(session.thread.title, "Active Buddy Session");
        assert_eq!(session.thread.mode, "buddy");
        assert!(session.thread.buddy_meta.is_some());
        assert_eq!(session.messages.len(), 1);
        assert_eq!(
            session.messages[0].content.content_text_only(),
            "keep active buddy during repaired fallback"
        );
        drop(session);

        let raw: serde_json::Value =
            serde_json::from_str(&tokio::fs::read_to_string(&fallback_path).await.unwrap())
                .unwrap();
        assert_eq!(
            raw["frozen_request_prefix"]["system_prompt"],
            "target task planner system"
        );
        assert!(raw["frozen_request_prefix"]["tools_canonical"].is_null());
        assert!(raw.get("claude_code_identity").is_none());
        assert!(raw.get("previous_response_id").is_none());
        assert_eq!(raw["custom_future_field"]["keep"], true);
        assert_no_chat_event_for(&mut chat_rx, std::time::Duration::from_millis(100)).await;
    }

    #[tokio::test]
    async fn generic_watcher_update_does_not_replace_active_buddy_session() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;
        let chat_id = "active-buddy-update-guard";
        let normal_path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        write_trajectory_file_with_user_message(
            &normal_path,
            chat_id,
            "Normal Replacement",
            "normal replacement message",
        )
        .await;
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        let mut chat_rx = {
            let mut session = session_arc.lock().await;
            session.thread.title = "Active Buddy Session".to_string();
            session.thread.mode = "buddy".to_string();
            session.thread.buddy_meta = Some(buddy_thread_meta());
            session.messages.push(ChatMessage::new(
                "user".to_string(),
                "keep buddy message".to_string(),
            ));
            session.runtime.state = SessionState::Generating;
            session.trajectory_dirty = true;
            session.subscribe()
        };
        app.chat
            .sessions
            .write()
            .await
            .insert(chat_id.to_string(), session_arc.clone());

        process_trajectory_change(gcx.clone(), chat_id, false).await;
        check_external_reload_pending(gcx, session_arc.clone()).await;

        let session = session_arc.lock().await;
        assert_eq!(session.external_reload_pending, None);
        assert_eq!(session.thread.title, "Active Buddy Session");
        assert_eq!(session.thread.mode, "buddy");
        assert!(session.thread.buddy_meta.is_some());
        assert_eq!(session.messages.len(), 1);
        assert_eq!(
            session.messages[0].content.content_text_only(),
            "keep buddy message"
        );
        drop(session);

        assert_no_chat_event_for(&mut chat_rx, std::time::Duration::from_millis(100)).await;
    }

    #[tokio::test]
    async fn generic_watcher_update_with_active_buddy_persists_repaired_generic() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;
        let mut rx = app.chat.trajectory_events_tx.subscribe();
        let chat_id = "active-buddy-update-repaired-generic";
        let path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        tokio::fs::create_dir_all(path.parent().unwrap())
            .await
            .unwrap();
        tokio::fs::write(
            &path,
            serde_json::to_string(&json!({
                "id": chat_id,
                "title": "Repaired Generic Update",
                "model": "model",
                "mode": "task_planner",
                "tool_use": "agent",
                "parent_id": "source-chat",
                "link_type": "mode_transition",
                "previous_response_id": "resp_source",
                "messages": [
                    {"role":"system","content":"target task planner system"},
                    {"role":"user","content":"generic update active buddy repair"}
                ],
                "frozen_request_prefix": {
                    "schema_version": 1,
                    "created_at": "2026-05-29T00:00:00Z",
                    "system_prompt": "source system",
                    "tools_canonical": [{"type":"function","function":{"name":"source_tool"}}]
                },
                "claude_code_identity": {
                    "device_id":"source-device",
                    "session_id":"source-session"
                },
                "created_at": "2024-01-01T00:00:00Z",
                "updated_at": "2024-01-01T00:00:01Z",
                "include_project_info": true,
                "checkpoints_enabled": true,
                "custom_future_field": {"keep": true}
            }))
            .unwrap(),
        )
        .await
        .unwrap();
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        let mut chat_rx = {
            let mut session = session_arc.lock().await;
            session.thread.title = "Active Buddy Session".to_string();
            session.thread.mode = "buddy".to_string();
            session.thread.buddy_meta = Some(buddy_thread_meta());
            session.external_reload_pending = Some(ExternalReloadPending::update(
                TrajectorySourceIdentity::Buddy,
            ));
            session.messages.push(ChatMessage::new(
                "user".to_string(),
                "keep active buddy during repaired update".to_string(),
            ));
            session.runtime.state = SessionState::Generating;
            session.trajectory_dirty = true;
            session.subscribe()
        };
        app.chat
            .sessions
            .write()
            .await
            .insert(chat_id.to_string(), session_arc.clone());

        process_trajectory_change(gcx.clone(), chat_id, false).await;
        check_external_reload_pending(gcx.clone(), session_arc.clone()).await;

        let event = wait_for_trajectory_event(&mut rx, chat_id).await;
        assert_eq!(event.event_type, "updated");
        assert_eq!(event.title.as_deref(), Some("Repaired Generic Update"));
        assert_eq!(event.session_state.as_deref(), Some("idle"));

        let session = session_arc.lock().await;
        assert_eq!(
            session.external_reload_pending,
            Some(ExternalReloadPending::update(
                TrajectorySourceIdentity::Buddy
            ))
        );
        assert_eq!(session.thread.title, "Active Buddy Session");
        assert_eq!(session.thread.mode, "buddy");
        assert!(session.thread.buddy_meta.is_some());
        assert_eq!(session.messages.len(), 1);
        assert_eq!(
            session.messages[0].content.content_text_only(),
            "keep active buddy during repaired update"
        );
        drop(session);

        let raw: serde_json::Value =
            serde_json::from_str(&tokio::fs::read_to_string(&path).await.unwrap()).unwrap();
        assert_eq!(
            raw["frozen_request_prefix"]["system_prompt"],
            "target task planner system"
        );
        assert!(raw["frozen_request_prefix"]["tools_canonical"].is_null());
        assert!(raw.get("claude_code_identity").is_none());
        assert!(raw.get("previous_response_id").is_none());
        assert_eq!(raw["custom_future_field"]["keep"], true);
        assert_no_chat_event_for(&mut chat_rx, std::time::Duration::from_millis(100)).await;
    }

    #[tokio::test]
    async fn normal_save_sse_ignores_active_task_state_error_and_worktree() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;
        let mut rx = app.chat.trajectory_events_tx.subscribe();
        let chat_id = "normal-save-active-task-clean";
        let task_meta = task_meta(
            "task-normal-save-collision",
            "agents",
            Some("agent-save-collision"),
            Some("card-save-collision"),
            None,
        );
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        {
            let mut session = session_arc.lock().await;
            session.thread.task_meta = Some(task_meta);
            session.thread.worktree = Some(trajectory_worktree_sample_with_id("task-save-wt"));
            session.runtime.state = SessionState::Generating;
            session.runtime.error = Some("task save error".to_string());
        }
        app.chat
            .sessions
            .write()
            .await
            .insert(chat_id.to_string(), session_arc);

        save_trajectory_snapshot(
            gcx,
            test_snapshot(
                chat_id,
                "Normal Save Collision",
                vec![ChatMessage::new(
                    "user".to_string(),
                    "normal save".to_string(),
                )],
            ),
        )
        .await
        .unwrap();

        let event = wait_for_trajectory_event(&mut rx, chat_id).await;
        assert_eq!(event.event_type, "updated");
        assert!(matches!(
            event.session_state.as_deref(),
            None | Some("idle")
        ));
        assert_eq!(event.error, None);
        assert!(event.worktree.is_none());
    }

    #[tokio::test]
    async fn normal_save_under_session_lock_does_not_reenter_or_leak_runtime() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;
        let mut rx = app.chat.trajectory_events_tx.subscribe();
        let id = "locked-snapshot-publication";
        let session = Arc::new(AMutex::new(ChatSession::new(id.into())));
        app.chat
            .sessions
            .write()
            .await
            .insert(id.into(), session.clone());
        let mut locked = session.lock().await;
        locked.runtime.state = SessionState::Generating;
        locked.runtime.error = Some("not snapshot runtime".into());
        tokio::time::timeout(
            std::time::Duration::from_secs(10),
            save_trajectory_snapshot(
                gcx,
                test_snapshot(
                    id,
                    "Snapshot",
                    vec![ChatMessage::new("user".into(), "keep".into())],
                ),
            ),
        )
        .await
        .expect("saving must not reacquire the held session lock")
        .unwrap();
        let event = wait_for_trajectory_event(&mut rx, id).await;
        assert_eq!(event.session_state, None);
        assert_eq!(event.error, None);
        drop(locked);
    }

    #[tokio::test]
    async fn normal_save_sse_keeps_matching_active_normal_state_error_and_worktree() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;
        let mut rx = app.chat.trajectory_events_tx.subscribe();
        let chat_id = "normal-save-active-normal-enriched";
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        {
            let mut session = session_arc.lock().await;
            session.thread.worktree = Some(trajectory_worktree_sample_with_id(
                "normal-save-enriched-wt",
            ));
            session.runtime.state = SessionState::Generating;
            session.runtime.error = Some("normal save enriched error".to_string());
        }
        app.chat
            .sessions
            .write()
            .await
            .insert(chat_id.to_string(), session_arc);

        save_trajectory_snapshot(
            gcx,
            test_snapshot(
                chat_id,
                "Normal Save Enriched",
                vec![ChatMessage::new(
                    "user".to_string(),
                    "normal save".to_string(),
                )],
            ),
        )
        .await
        .unwrap();

        let event = wait_for_trajectory_event(&mut rx, chat_id).await;
        assert_eq!(event.event_type, "updated");
        assert_eq!(event.session_state.as_deref(), Some("generating"));
        assert_eq!(event.error.as_deref(), Some("normal save enriched error"));
        assert_eq!(
            event.worktree.as_ref().map(|worktree| worktree.id.as_str()),
            Some("normal-save-enriched-wt")
        );
    }

    #[tokio::test]
    async fn task_http_save_sse_ignores_active_normal_state_error_and_worktree() {
        let dir = tempfile::tempdir().unwrap();
        let (_gcx, app) = make_app_with_workspace(dir.path()).await;
        let mut rx = app.chat.trajectory_events_tx.subscribe();
        let chat_id = "task-http-save-active-normal-clean";
        let task_id = "task-http-save-active-normal";
        tokio::fs::create_dir_all(dir.path().join(".refact").join("tasks").join(task_id))
            .await
            .unwrap();
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        {
            let mut session = session_arc.lock().await;
            session.thread.worktree = Some(trajectory_worktree_sample_with_id("normal-save-wt"));
            session.runtime.state = SessionState::Generating;
            session.runtime.error = Some("normal save error".to_string());
        }
        app.chat
            .sessions
            .write()
            .await
            .insert(chat_id.to_string(), session_arc);
        let mut payload = sample_trajectory(chat_id, "Task Http Save", "2024-01-01T00:00:01Z");
        payload["task_meta"] = json!({
            "task_id": task_id,
            "role": "agents",
            "agent_id": "agent-http-save",
            "card_id": "card-http-save"
        });

        handle_v1_trajectories_save(
            State(app),
            AxumPath(chat_id.to_string()),
            hyper::body::Bytes::from(serde_json::to_vec(&payload).unwrap()),
        )
        .await
        .unwrap();

        let event = wait_for_trajectory_event(&mut rx, chat_id).await;
        assert_eq!(event.event_type, "created");
        assert_eq!(event.task_id.as_deref(), Some(task_id));
        assert_eq!(event.session_state.as_deref(), Some("idle"));
        assert_eq!(event.error, None);
        assert!(event.worktree.is_none());
    }

    #[tokio::test]
    async fn task_http_save_sse_keeps_matching_active_task_state_error_and_worktree() {
        let dir = tempfile::tempdir().unwrap();
        let (_gcx, app) = make_app_with_workspace(dir.path()).await;
        let mut rx = app.chat.trajectory_events_tx.subscribe();
        let chat_id = "task-http-save-active-task-enriched";
        let task_id = "task-http-save-active-task";
        tokio::fs::create_dir_all(dir.path().join(".refact").join("tasks").join(task_id))
            .await
            .unwrap();
        let task_meta = task_meta(
            task_id,
            "agents",
            Some("agent-http-save-enriched"),
            Some("card-http-save-enriched"),
            None,
        );
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        {
            let mut session = session_arc.lock().await;
            session.thread.task_meta = Some(task_meta);
            session.thread.worktree =
                Some(trajectory_worktree_sample_with_id("task-save-enriched-wt"));
            session.runtime.state = SessionState::Generating;
            session.runtime.error = Some("task save enriched error".to_string());
        }
        app.chat
            .sessions
            .write()
            .await
            .insert(chat_id.to_string(), session_arc);
        let mut payload =
            sample_trajectory(chat_id, "Task Http Save Enriched", "2024-01-01T00:00:01Z");
        payload["task_meta"] = json!({
            "task_id": task_id,
            "role": "agents",
            "agent_id": "agent-http-save-enriched",
            "card_id": "card-http-save-enriched"
        });

        handle_v1_trajectories_save(
            State(app),
            AxumPath(chat_id.to_string()),
            hyper::body::Bytes::from(serde_json::to_vec(&payload).unwrap()),
        )
        .await
        .unwrap();

        let event = wait_for_trajectory_event(&mut rx, chat_id).await;
        assert_eq!(event.event_type, "created");
        assert_eq!(event.task_id.as_deref(), Some(task_id));
        assert_eq!(event.session_state.as_deref(), Some("generating"));
        assert_eq!(event.error.as_deref(), Some("task save enriched error"));
        assert_eq!(
            event.worktree.as_ref().map(|worktree| worktree.id.as_str()),
            Some("task-save-enriched-wt")
        );
    }

    #[tokio::test]
    async fn task_update_with_active_normal_same_id_emits_update_without_session_mutation() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;
        let mut rx = app.chat.trajectory_events_tx.subscribe();
        let chat_id = "task-update-active-normal-collision";
        let task_meta = task_meta(
            "task-update-normal-collision",
            "agents",
            Some("agent-1"),
            Some("card-1"),
            None,
        );
        let task_path = dir
            .path()
            .join(".refact")
            .join("tasks")
            .join("task-update-normal-collision")
            .join("trajectories")
            .join("agents")
            .join("agent-1")
            .join(format!("{chat_id}.json"));
        write_task_trajectory_file_with_user_message(
            &task_path,
            chat_id,
            "Task Updated",
            "task replacement message",
            &task_meta,
        )
        .await;
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        let mut chat_rx = {
            let mut session = session_arc.lock().await;
            session.thread.title = "Active Normal".to_string();
            session.messages.push(ChatMessage::new(
                "user".to_string(),
                "keep active normal".to_string(),
            ));
            session.runtime.state = SessionState::Generating;
            session.trajectory_dirty = true;
            session.subscribe()
        };
        app.chat
            .sessions
            .write()
            .await
            .insert(chat_id.to_string(), session_arc.clone());

        process_trajectory_change_for_source(
            gcx,
            chat_id,
            false,
            Some(TrajectorySourceIdentity::from_task_meta(&task_meta)),
        )
        .await;

        let event = wait_for_trajectory_event(&mut rx, chat_id).await;
        assert_eq!(event.event_type, "updated");
        assert_eq!(event.title.as_deref(), Some("Task Updated"));
        assert_eq!(
            event.task_id.as_deref(),
            Some("task-update-normal-collision")
        );
        assert_eq!(event.session_state.as_deref(), Some("idle"));

        let session = session_arc.lock().await;
        assert_eq!(session.external_reload_pending, None);
        assert_eq!(session.thread.title, "Active Normal");
        assert!(session.thread.task_meta.is_none());
        assert_eq!(session.messages.len(), 1);
        assert_eq!(
            session.messages[0].content.content_text_only(),
            "keep active normal"
        );
        drop(session);
        assert_no_chat_event_for(&mut chat_rx, std::time::Duration::from_millis(100)).await;
    }

    #[tokio::test]
    async fn normal_update_with_active_task_same_id_emits_update_without_session_mutation() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;
        let mut rx = app.chat.trajectory_events_tx.subscribe();
        let chat_id = "normal-update-active-task-collision";
        let normal_path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        write_trajectory_file_with_user_message(
            &normal_path,
            chat_id,
            "Normal Updated",
            "normal replacement message",
        )
        .await;
        let active_task_meta = task_meta(
            "task-active-normal-update-collision",
            "agents",
            Some("agent-2"),
            Some("card-2"),
            None,
        );
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        let mut chat_rx = {
            let mut session = session_arc.lock().await;
            session.thread.title = "Active Task".to_string();
            session.thread.task_meta = Some(active_task_meta.clone());
            session.messages.push(ChatMessage::new(
                "user".to_string(),
                "keep active task".to_string(),
            ));
            session.runtime.state = SessionState::Generating;
            session.trajectory_dirty = true;
            session.subscribe()
        };
        app.chat
            .sessions
            .write()
            .await
            .insert(chat_id.to_string(), session_arc.clone());

        process_trajectory_change_for_source(
            gcx,
            chat_id,
            false,
            Some(TrajectorySourceIdentity::Normal),
        )
        .await;

        let event = wait_for_trajectory_event(&mut rx, chat_id).await;
        assert_eq!(event.event_type, "updated");
        assert_eq!(event.title.as_deref(), Some("Normal Updated"));
        assert_eq!(event.task_id, None);
        assert_eq!(event.session_state.as_deref(), Some("idle"));

        let session = session_arc.lock().await;
        assert_eq!(session.external_reload_pending, None);
        assert_eq!(session.thread.title, "Active Task");
        assert_eq!(session.thread.task_meta, Some(active_task_meta));
        assert_eq!(session.messages.len(), 1);
        assert_eq!(
            session.messages[0].content.content_text_only(),
            "keep active task"
        );
        drop(session);
        assert_no_chat_event_for(&mut chat_rx, std::time::Duration::from_millis(100)).await;
    }

    #[tokio::test]
    async fn task_path_hint_update_with_normal_same_id_loads_task_without_session_mutation() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;
        let mut rx = app.chat.trajectory_events_tx.subscribe();
        let chat_id = "task-path-hint-update-normal-collision";
        let normal_path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        write_trajectory_file_with_user_message(
            &normal_path,
            chat_id,
            "Normal Same Id",
            "normal must not be loaded",
        )
        .await;
        let task_meta = task_meta(
            "task-path-hint-update",
            "agents",
            Some("agent-path-hint"),
            Some("card-path-hint"),
            None,
        );
        let task_path = dir
            .path()
            .join(".refact")
            .join("tasks")
            .join("task-path-hint-update")
            .join("trajectories")
            .join("agents")
            .join("agent-path-hint")
            .join(format!("{chat_id}.json"));
        write_task_trajectory_file_with_user_message(
            &task_path,
            chat_id,
            "Task Path Hint Updated",
            "task path hint replacement",
            &task_meta,
        )
        .await;
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        let mut chat_rx = {
            let mut session = session_arc.lock().await;
            session.thread.title = "Active Normal Path Hint".to_string();
            session.messages.push(ChatMessage::new(
                "user".to_string(),
                "keep active normal path hint".to_string(),
            ));
            session.runtime.state = SessionState::Generating;
            session.trajectory_dirty = true;
            session.subscribe()
        };
        app.chat
            .sessions
            .write()
            .await
            .insert(chat_id.to_string(), session_arc.clone());

        process_trajectory_change_for_source(
            gcx.clone(),
            chat_id,
            false,
            Some(trajectory_source_identity_from_path(
                &task_path,
                &get_all_task_roots(gcx).await,
            )),
        )
        .await;

        let event = wait_for_trajectory_event(&mut rx, chat_id).await;
        assert_eq!(event.event_type, "updated");
        assert_eq!(event.title.as_deref(), Some("Task Path Hint Updated"));
        assert_eq!(event.task_id.as_deref(), Some("task-path-hint-update"));
        assert_eq!(event.task_role.as_deref(), Some("agents"));
        assert_eq!(event.agent_id.as_deref(), Some("agent-path-hint"));
        assert_eq!(event.card_id.as_deref(), Some("card-path-hint"));
        assert_eq!(event.session_state.as_deref(), Some("idle"));

        let session = session_arc.lock().await;
        assert_eq!(session.external_reload_pending, None);
        assert_eq!(session.thread.title, "Active Normal Path Hint");
        assert!(session.thread.task_meta.is_none());
        assert_eq!(session.messages.len(), 1);
        assert_eq!(
            session.messages[0].content.content_text_only(),
            "keep active normal path hint"
        );
        drop(session);
        assert_no_chat_event_for(&mut chat_rx, std::time::Duration::from_millis(100)).await;
    }

    #[tokio::test]
    async fn planner_chat_id_path_hint_does_not_use_file_stem() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _app) = make_app_with_workspace(dir.path()).await;
        let task_path = dir
            .path()
            .join(".refact")
            .join("tasks")
            .join("task-path-hint-planner")
            .join("trajectories")
            .join("planner")
            .join("misleading-file-stem.json");
        tokio::fs::create_dir_all(task_path.parent().unwrap())
            .await
            .unwrap();
        tokio::fs::write(&task_path, "{}").await.unwrap();

        let source =
            trajectory_source_identity_from_path(&task_path, &get_all_task_roots(gcx).await);

        assert_eq!(
            source,
            TrajectorySourceIdentity::task(
                "task-path-hint-planner".to_string(),
                "planner".to_string(),
                None,
                None,
                None,
            )
        );
    }

    #[tokio::test]
    async fn planner_chat_id_verified_root_requires_planner_for_same_task() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _app) = make_app_with_workspace(dir.path()).await;
        let child_meta = task_meta(
            "task-root-check",
            "subchats",
            None,
            None,
            Some("stale-planner"),
        );
        let planner_path = dir
            .path()
            .join(".refact")
            .join("tasks")
            .join("task-root-check")
            .join("trajectories")
            .join("planner")
            .join("planner-root-check.json");
        write_task_trajectory_file_with_user_message(
            &planner_path,
            "planner-root-check",
            "Planner Root Check",
            "planner root",
            &task_meta(
                "task-root-check",
                "planner",
                None,
                None,
                Some("planner-root-check"),
            ),
        )
        .await;

        assert_eq!(
            verified_planner_linked_root_chat_id(
                gcx.clone(),
                "child-root-check",
                Some("normal-root"),
                &child_meta,
            )
            .await,
            None
        );
        assert_eq!(
            verified_planner_linked_root_chat_id(
                gcx,
                "child-root-check",
                Some("planner-root-check"),
                &child_meta,
            )
            .await
            .as_deref(),
            Some("planner-root-check")
        );
    }

    #[tokio::test]
    async fn normal_path_update_with_task_same_id_loads_normal_without_session_mutation() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;
        let mut rx = app.chat.trajectory_events_tx.subscribe();
        let chat_id = "normal-path-update-task-collision";
        let task_meta = task_meta(
            "task-normal-path-update",
            "agents",
            Some("agent-normal-path"),
            Some("card-normal-path"),
            None,
        );
        let task_path = dir
            .path()
            .join(".refact")
            .join("tasks")
            .join("task-normal-path-update")
            .join("trajectories")
            .join("agents")
            .join("agent-normal-path")
            .join(format!("{chat_id}.json"));
        write_task_trajectory_file_with_user_message(
            &task_path,
            chat_id,
            "Task Same Id",
            "task must not be loaded",
            &task_meta,
        )
        .await;
        let normal_path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        write_trajectory_file_with_user_message(
            &normal_path,
            chat_id,
            "Normal Path Updated",
            "normal path replacement",
        )
        .await;
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        let mut chat_rx = {
            let mut session = session_arc.lock().await;
            session.thread.title = "Active Task Path Hint".to_string();
            session.thread.task_meta = Some(task_meta.clone());
            session.messages.push(ChatMessage::new(
                "user".to_string(),
                "keep active task path hint".to_string(),
            ));
            session.runtime.state = SessionState::Generating;
            session.trajectory_dirty = true;
            session.subscribe()
        };
        app.chat
            .sessions
            .write()
            .await
            .insert(chat_id.to_string(), session_arc.clone());

        process_trajectory_change_for_source(
            gcx,
            chat_id,
            false,
            Some(TrajectorySourceIdentity::Normal),
        )
        .await;

        let event = wait_for_trajectory_event(&mut rx, chat_id).await;
        assert_eq!(event.event_type, "updated");
        assert_eq!(event.title.as_deref(), Some("Normal Path Updated"));
        assert_eq!(event.task_id, None);
        assert_eq!(event.session_state.as_deref(), Some("idle"));

        let session = session_arc.lock().await;
        assert_eq!(session.external_reload_pending, None);
        assert_eq!(session.thread.title, "Active Task Path Hint");
        assert_eq!(session.thread.task_meta, Some(task_meta));
        assert_eq!(session.messages.len(), 1);
        assert_eq!(
            session.messages[0].content.content_text_only(),
            "keep active task path hint"
        );
        drop(session);
        assert_no_chat_event_for(&mut chat_rx, std::time::Duration::from_millis(100)).await;
    }

    #[tokio::test]
    async fn task_scan_collects_path_derived_task_source() {
        let dir = tempfile::tempdir().unwrap();
        let (_gcx, _) = make_app_with_workspace(dir.path()).await;
        let task_roots = vec![dir.path().join(".refact").join("tasks")];
        let chat_id = "scan-task-source";
        let path = task_roots[0]
            .join("task-scan-source")
            .join("trajectories")
            .join("agents")
            .join("agent-scan-source")
            .join(format!("{chat_id}.json"));
        write_trajectory_file(&path, chat_id, "Scan Task", "2024-01-01T00:00:01Z").await;

        let mut sources =
            collect_task_trajectory_sources_under_path(&task_roots[0], &task_roots).await;
        sources.sort_by(|left, right| left.0.cmp(&right.0));

        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0].0, chat_id);
        assert_eq!(
            sources[0].1,
            TrajectorySourceIdentity::task(
                "task-scan-source".to_string(),
                "agents".to_string(),
                Some("agent-scan-source".to_string()),
                None,
                None,
            )
        );
        assert_eq!(sources[0].2, path);
    }

    #[test]
    fn watcher_overflow_schedules_recovery_scans_and_resets_drop_counter() {
        let normal_root = PathBuf::from("normal-trajectories");
        let task_root = PathBuf::from("task-trajectories");
        let dropped_events = AtomicUsize::new(3);
        let mut pending_scans = std::collections::HashMap::new();

        schedule_trajectory_watcher_overflow_recovery(
            &mut pending_scans,
            &dropped_events,
            &[normal_root.clone(), task_root.clone()],
        );

        assert_eq!(dropped_events.load(Ordering::Relaxed), 0);
        assert_eq!(pending_scans.len(), 2);
        assert!(pending_scans.contains_key(&normal_root));
        assert!(pending_scans.contains_key(&task_root));
    }

    #[test]
    fn watcher_pending_debounce_keys_by_chat_and_source() {
        let chat_id = "debounce-source-collision".to_string();
        let task_source = TrajectorySourceIdentity::task(
            "task-debounce".to_string(),
            "agents".to_string(),
            Some("agent-debounce".to_string()),
            None,
            None,
        );
        let mut pending = TrajectoryPendingMap::new();
        let now = Instant::now();

        insert_pending_trajectory_change(
            &mut pending,
            chat_id.clone(),
            false,
            TrajectorySourceIdentity::Normal,
            PathBuf::from("normal.json"),
            now,
        );
        insert_pending_trajectory_change(
            &mut pending,
            chat_id.clone(),
            true,
            task_source.clone(),
            PathBuf::from("task.json"),
            now,
        );

        assert_eq!(pending.len(), 2);
        let normal = pending
            .get(&(chat_id.clone(), TrajectorySourceIdentity::Normal))
            .unwrap();
        assert_eq!(normal.last_event_at, now);
        assert!(!normal.is_remove);
        assert_eq!(normal.path, PathBuf::from("normal.json"));
        let task = pending.get(&(chat_id, task_source)).unwrap();
        assert_eq!(task.last_event_at, now);
        assert!(task.is_remove);
        assert_eq!(task.path, PathBuf::from("task.json"));
    }

    #[tokio::test]
    async fn busy_active_task_matching_update_stores_task_pending_source_with_normal_collision() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;
        let chat_id = "busy-task-update-pending-source";
        let normal_path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        write_trajectory_file_with_user_message(
            &normal_path,
            chat_id,
            "Normal Same Id",
            "normal same-id collision",
        )
        .await;
        let task_meta = task_meta(
            "task-pending-source",
            "agents",
            Some("agent-pending-source"),
            Some("card-pending-source"),
            None,
        );
        let task_path = dir
            .path()
            .join(".refact")
            .join("tasks")
            .join("task-pending-source")
            .join("trajectories")
            .join("agents")
            .join("agent-pending-source")
            .join(format!("{chat_id}.json"));
        write_task_trajectory_file_with_user_message(
            &task_path,
            chat_id,
            "Task Same Id",
            "task same-id update",
            &task_meta,
        )
        .await;
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        {
            let mut session = session_arc.lock().await;
            session.thread.task_meta = Some(task_meta.clone());
            session.runtime.state = SessionState::Generating;
            session.trajectory_dirty = true;
        }
        app.chat
            .sessions
            .write()
            .await
            .insert(chat_id.to_string(), session_arc.clone());

        process_trajectory_change_for_source(
            gcx,
            chat_id,
            false,
            Some(TrajectorySourceIdentity::from_task_meta(&task_meta)),
        )
        .await;

        let session = session_arc.lock().await;
        assert_eq!(
            session.external_reload_pending,
            Some(ExternalReloadPending::update(
                TrajectorySourceIdentity::from_task_meta(&task_meta)
            ))
        );
        assert_eq!(session.thread.task_meta, Some(task_meta));
    }

    #[tokio::test]
    async fn busy_active_normal_matching_update_stores_normal_pending_source_with_task_collision() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;
        let chat_id = "busy-normal-update-pending-source";
        let task_meta = task_meta(
            "task-normal-pending-source",
            "agents",
            Some("agent-normal-pending"),
            Some("card-normal-pending"),
            None,
        );
        let task_path = dir
            .path()
            .join(".refact")
            .join("tasks")
            .join("task-normal-pending-source")
            .join("trajectories")
            .join("agents")
            .join("agent-normal-pending")
            .join(format!("{chat_id}.json"));
        write_task_trajectory_file_with_user_message(
            &task_path,
            chat_id,
            "Task Same Id",
            "task same-id collision",
            &task_meta,
        )
        .await;
        let normal_path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        write_trajectory_file_with_user_message(
            &normal_path,
            chat_id,
            "Normal Same Id",
            "normal same-id update",
        )
        .await;
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        {
            let mut session = session_arc.lock().await;
            session.runtime.state = SessionState::Generating;
            session.trajectory_dirty = true;
        }
        app.chat
            .sessions
            .write()
            .await
            .insert(chat_id.to_string(), session_arc.clone());

        process_trajectory_change_for_source(
            gcx,
            chat_id,
            false,
            Some(TrajectorySourceIdentity::Normal),
        )
        .await;

        let session = session_arc.lock().await;
        assert_eq!(
            session.external_reload_pending,
            Some(ExternalReloadPending::update(
                TrajectorySourceIdentity::Normal
            ))
        );
        assert!(session.thread.task_meta.is_none());
    }

    #[tokio::test]
    async fn pending_task_update_applies_task_despite_normal_priority() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let chat_id = "pending-task-update-normal-priority";
        let normal_path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        write_trajectory_file_with_user_message(
            &normal_path,
            chat_id,
            "Normal Higher Priority",
            "normal must not apply",
        )
        .await;
        let task_meta = task_meta(
            "task-pending-update-apply",
            "agents",
            Some("agent-pending-apply"),
            Some("card-pending-apply"),
            None,
        );
        let task_path = dir
            .path()
            .join(".refact")
            .join("tasks")
            .join("task-pending-update-apply")
            .join("trajectories")
            .join("agents")
            .join("agent-pending-apply")
            .join(format!("{chat_id}.json"));
        write_task_trajectory_file_with_user_message(
            &task_path,
            chat_id,
            "Task Pending Applied",
            "task pending applied",
            &task_meta,
        )
        .await;
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        {
            let mut session = session_arc.lock().await;
            session.thread.task_meta = Some(task_meta.clone());
            session.thread.title = "Busy Task Before Pending Apply".to_string();
            session.messages.push(ChatMessage::new(
                "user".to_string(),
                "old busy task".to_string(),
            ));
            session.external_reload_pending = Some(ExternalReloadPending::update(
                TrajectorySourceIdentity::from_task_meta(&task_meta),
            ));
            session.runtime.state = SessionState::Idle;
            session.trajectory_dirty = false;
        }

        check_external_reload_pending(gcx, session_arc.clone()).await;

        let session = session_arc.lock().await;
        assert_eq!(session.external_reload_pending, None);
        assert_eq!(session.thread.title, "Task Pending Applied");
        assert_eq!(session.thread.task_meta, Some(task_meta));
        assert_eq!(session.messages.len(), 1);
        assert_eq!(
            session.messages[0].content.content_text_only(),
            "task pending applied"
        );
    }

    #[tokio::test]
    async fn pending_normal_update_applies_normal_despite_task_collision() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let chat_id = "pending-normal-update-task-collision";
        let task_meta = task_meta(
            "task-pending-normal-collision",
            "agents",
            Some("agent-pending-normal"),
            Some("card-pending-normal"),
            None,
        );
        let task_path = dir
            .path()
            .join(".refact")
            .join("tasks")
            .join("task-pending-normal-collision")
            .join("trajectories")
            .join("agents")
            .join("agent-pending-normal")
            .join(format!("{chat_id}.json"));
        write_task_trajectory_file_with_user_message(
            &task_path,
            chat_id,
            "Task Same Id",
            "task must not apply",
            &task_meta,
        )
        .await;
        let normal_path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        write_trajectory_file_with_user_message(
            &normal_path,
            chat_id,
            "Normal Pending Applied",
            "normal pending applied",
        )
        .await;
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        {
            let mut session = session_arc.lock().await;
            session.thread.title = "Busy Normal Before Pending Apply".to_string();
            session.messages.push(ChatMessage::new(
                "user".to_string(),
                "old busy normal".to_string(),
            ));
            session.external_reload_pending = Some(ExternalReloadPending::update(
                TrajectorySourceIdentity::Normal,
            ));
            session.runtime.state = SessionState::Idle;
            session.trajectory_dirty = false;
        }

        check_external_reload_pending(gcx, session_arc.clone()).await;

        let session = session_arc.lock().await;
        assert_eq!(session.external_reload_pending, None);
        assert_eq!(session.thread.title, "Normal Pending Applied");
        assert!(session.thread.task_meta.is_none());
        assert_eq!(session.messages.len(), 1);
        assert_eq!(
            session.messages[0].content.content_text_only(),
            "normal pending applied"
        );
    }

    #[tokio::test]
    async fn pending_task_update_missing_source_clears_while_normal_same_id_exists() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let chat_id = "pending-task-update-missing-normal-exists";
        let normal_path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        write_trajectory_file_with_user_message(
            &normal_path,
            chat_id,
            "Normal Same Id Exists",
            "normal must not replace missing task",
        )
        .await;
        let task_meta = task_meta(
            "task-missing-pending-update",
            "agents",
            Some("agent-missing-pending"),
            Some("card-missing-pending"),
            None,
        );
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        {
            let mut session = session_arc.lock().await;
            session.thread.task_meta = Some(task_meta.clone());
            session.thread.title = "Active Task Missing Pending Source".to_string();
            session.messages.push(ChatMessage::new(
                "user".to_string(),
                "keep active task when matching source missing".to_string(),
            ));
            session.external_reload_pending = Some(ExternalReloadPending::update(
                TrajectorySourceIdentity::from_task_meta(&task_meta),
            ));
            session.runtime.state = SessionState::Idle;
            session.trajectory_dirty = false;
        }

        check_external_reload_pending(gcx, session_arc.clone()).await;

        let session = session_arc.lock().await;
        assert_eq!(session.external_reload_pending, None);
        assert_eq!(session.thread.title, "Active Task Missing Pending Source");
        assert_eq!(session.thread.task_meta, Some(task_meta));
        assert_eq!(session.messages.len(), 1);
        assert_eq!(
            session.messages[0].content.content_text_only(),
            "keep active task when matching source missing"
        );
    }

    #[tokio::test]
    async fn pending_update_source_mismatch_clears_without_loading_other_source() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let chat_id = "pending-update-source-mismatch-clears";
        let normal_path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        write_trajectory_file_with_user_message(
            &normal_path,
            chat_id,
            "Mismatched Normal Pending",
            "normal mismatch must not apply",
        )
        .await;
        let task_meta = task_meta(
            "task-pending-mismatch",
            "agents",
            Some("agent-pending-mismatch"),
            Some("card-pending-mismatch"),
            None,
        );
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        {
            let mut session = session_arc.lock().await;
            session.thread.task_meta = Some(task_meta.clone());
            session.thread.title = "Active Task Mismatched Pending".to_string();
            session.messages.push(ChatMessage::new(
                "user".to_string(),
                "keep task on mismatched pending".to_string(),
            ));
            session.external_reload_pending = Some(ExternalReloadPending::update(
                TrajectorySourceIdentity::Normal,
            ));
            session.runtime.state = SessionState::Idle;
            session.trajectory_dirty = false;
        }

        check_external_reload_pending(gcx, session_arc.clone()).await;

        let session = session_arc.lock().await;
        assert_eq!(session.external_reload_pending, None);
        assert_eq!(session.thread.title, "Active Task Mismatched Pending");
        assert_eq!(session.thread.task_meta, Some(task_meta));
        assert_eq!(session.messages.len(), 1);
        assert_eq!(
            session.messages[0].content.content_text_only(),
            "keep task on mismatched pending"
        );
    }

    #[tokio::test]
    async fn pending_delete_source_mismatch_clears_without_loading_other_source() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let chat_id = "pending-delete-source-mismatch-clears";
        let normal_path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        write_trajectory_file_with_user_message(
            &normal_path,
            chat_id,
            "Normal Same Id For Delete Mismatch",
            "normal delete mismatch must not apply",
        )
        .await;
        let task_meta = task_meta(
            "task-delete-mismatch",
            "agents",
            Some("agent-delete-mismatch"),
            Some("card-delete-mismatch"),
            None,
        );
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        {
            let mut session = session_arc.lock().await;
            session.thread.title = "Active Normal Delete Mismatch".to_string();
            session.messages.push(ChatMessage::new(
                "user".to_string(),
                "keep normal on mismatched delete".to_string(),
            ));
            session.external_reload_pending = Some(ExternalReloadPending::delete(
                TrajectorySourceIdentity::from_task_meta(&task_meta),
            ));
            session.runtime.state = SessionState::Idle;
            session.trajectory_dirty = false;
        }

        check_external_reload_pending(gcx, session_arc.clone()).await;

        let session = session_arc.lock().await;
        assert_eq!(session.external_reload_pending, None);
        assert_eq!(session.thread.title, "Active Normal Delete Mismatch");
        assert!(session.thread.task_meta.is_none());
        assert_eq!(session.messages.len(), 1);
        assert_eq!(
            session.messages[0].content.content_text_only(),
            "keep normal on mismatched delete"
        );
    }

    #[tokio::test]
    async fn task_delete_with_active_normal_same_id_does_not_clear_session() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;
        let mut rx = app.chat.trajectory_events_tx.subscribe();
        let chat_id = "task-delete-active-normal-collision";
        let task_meta = task_meta(
            "task-delete-normal-collision",
            "agents",
            Some("agent-3"),
            Some("card-3"),
            None,
        );
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        let mut chat_rx = {
            let mut session = session_arc.lock().await;
            session.thread.title = "Active Normal Delete".to_string();
            session.messages.push(ChatMessage::new(
                "user".to_string(),
                "keep normal during task delete".to_string(),
            ));
            session.runtime.state = SessionState::Idle;
            session.trajectory_dirty = false;
            session.subscribe()
        };
        app.chat
            .sessions
            .write()
            .await
            .insert(chat_id.to_string(), session_arc.clone());

        process_trajectory_change_for_source(
            gcx,
            chat_id,
            true,
            Some(TrajectorySourceIdentity::from_task_meta(&task_meta)),
        )
        .await;

        let event = wait_for_trajectory_event(&mut rx, chat_id).await;
        assert_eq!(event.event_type, "deleted");
        let session = session_arc.lock().await;
        assert_eq!(session.external_reload_pending, None);
        assert_eq!(session.thread.title, "Active Normal Delete");
        assert_eq!(session.messages.len(), 1);
        assert_eq!(
            session.messages[0].content.content_text_only(),
            "keep normal during task delete"
        );
        drop(session);
        assert_no_chat_event_for(&mut chat_rx, std::time::Duration::from_millis(100)).await;
    }

    #[tokio::test]
    async fn task_delete_with_normal_fallback_emits_updated_without_loading_normal_into_task() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;
        let mut rx = app.chat.trajectory_events_tx.subscribe();
        let chat_id = "task-delete-normal-fallback-no-task-mutation";
        let normal_path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        write_trajectory_file_with_user_message(
            &normal_path,
            chat_id,
            "Normal Fallback After Task Delete",
            "normal fallback should stay out of task session",
        )
        .await;
        let task_meta = task_meta(
            "task-delete-normal-fallback",
            "agents",
            Some("agent-delete-normal-fallback"),
            Some("card-delete-normal-fallback"),
            None,
        );
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        let mut chat_rx = {
            let mut session = session_arc.lock().await;
            session.thread.title = "Active Task Before Delete".to_string();
            session.thread.task_meta = Some(task_meta.clone());
            session.messages.push(ChatMessage::new(
                "user".to_string(),
                "keep active task after task delete".to_string(),
            ));
            session.runtime.state = SessionState::Idle;
            session.trajectory_dirty = false;
            session.subscribe()
        };
        app.chat
            .sessions
            .write()
            .await
            .insert(chat_id.to_string(), session_arc.clone());

        process_trajectory_change_for_source(
            gcx,
            chat_id,
            true,
            Some(TrajectorySourceIdentity::from_task_meta(&task_meta)),
        )
        .await;

        let event = wait_for_trajectory_event(&mut rx, chat_id).await;
        assert_eq!(event.event_type, "updated");
        assert_eq!(
            event.title.as_deref(),
            Some("Normal Fallback After Task Delete")
        );
        assert_eq!(event.task_id, None);
        assert_eq!(event.task_role, None);
        assert_eq!(event.session_state.as_deref(), Some("idle"));
        assert!(tokio::fs::try_exists(&normal_path).await.unwrap());

        let session = session_arc.lock().await;
        assert_eq!(session.external_reload_pending, None);
        assert!(session.messages.is_empty());
        assert_eq!(session.thread.id, chat_id);
        assert!(session.thread.task_meta.is_none());
        assert_eq!(session.thread.title, ThreadParams::default().title);
        drop(session);

        let json = chat_rx.recv().await.unwrap();
        let envelope: EventEnvelope = serde_json::from_str(&json).unwrap();
        match envelope.event {
            ChatEvent::Snapshot {
                thread, messages, ..
            } => {
                assert_eq!(thread.id, chat_id);
                assert!(thread.task_meta.is_none());
                assert!(messages.is_empty());
            }
            other => panic!("expected Snapshot, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn busy_active_task_pending_delete_resolves_without_loading_normal_fallback() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;
        let mut rx = app.chat.trajectory_events_tx.subscribe();
        let chat_id = "busy-task-delete-normal-fallback-pending";
        let normal_path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        write_trajectory_file_with_user_message(
            &normal_path,
            chat_id,
            "Normal Fallback While Task Busy",
            "normal fallback while task busy",
        )
        .await;
        let task_meta = task_meta(
            "task-busy-delete-normal-fallback",
            "agents",
            Some("agent-busy-delete-normal-fallback"),
            Some("card-busy-delete-normal-fallback"),
            None,
        );
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        let mut chat_rx = {
            let mut session = session_arc.lock().await;
            session.thread.title = "Busy Task Before Delete".to_string();
            session.thread.task_meta = Some(task_meta.clone());
            session.messages.push(ChatMessage::new(
                "user".to_string(),
                "keep busy task after delete".to_string(),
            ));
            session.runtime.state = SessionState::Generating;
            session.trajectory_dirty = true;
            session.subscribe()
        };
        app.chat
            .sessions
            .write()
            .await
            .insert(chat_id.to_string(), session_arc.clone());

        process_trajectory_change_for_source(
            gcx.clone(),
            chat_id,
            true,
            Some(TrajectorySourceIdentity::from_task_meta(&task_meta)),
        )
        .await;

        let event = wait_for_trajectory_event(&mut rx, chat_id).await;
        assert_eq!(event.event_type, "updated");
        assert_eq!(
            event.title.as_deref(),
            Some("Normal Fallback While Task Busy")
        );
        {
            let session = session_arc.lock().await;
            assert_eq!(
                session.external_reload_pending,
                Some(ExternalReloadPending::delete(
                    TrajectorySourceIdentity::from_task_meta(&task_meta)
                ))
            );
            assert_eq!(session.thread.title, "Busy Task Before Delete");
            assert_eq!(session.thread.task_meta, Some(task_meta.clone()));
            assert_eq!(session.messages.len(), 1);
        }

        {
            let mut session = session_arc.lock().await;
            session.runtime.state = SessionState::Idle;
            session.trajectory_dirty = false;
        }
        check_external_reload_pending(gcx, session_arc.clone()).await;

        let session = session_arc.lock().await;
        assert_eq!(session.external_reload_pending, None);
        assert!(session.messages.is_empty());
        assert_eq!(session.thread.id, chat_id);
        assert!(session.thread.task_meta.is_none());
        assert_eq!(session.thread.title, ThreadParams::default().title);
        drop(session);

        let json = chat_rx.recv().await.unwrap();
        let envelope: EventEnvelope = serde_json::from_str(&json).unwrap();
        match envelope.event {
            ChatEvent::Snapshot {
                thread, messages, ..
            } => {
                assert_eq!(thread.id, chat_id);
                assert!(thread.task_meta.is_none());
                assert!(messages.is_empty());
            }
            other => panic!("expected Snapshot, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn normal_delete_with_active_task_same_id_does_not_clear_session() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;
        let mut rx = app.chat.trajectory_events_tx.subscribe();
        let chat_id = "normal-delete-active-task-collision";
        let active_task_meta = task_meta(
            "task-active-normal-delete-collision",
            "agents",
            Some("agent-4"),
            Some("card-4"),
            None,
        );
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        let mut chat_rx = {
            let mut session = session_arc.lock().await;
            session.thread.title = "Active Task Delete".to_string();
            session.thread.task_meta = Some(active_task_meta.clone());
            session.messages.push(ChatMessage::new(
                "user".to_string(),
                "keep task during normal delete".to_string(),
            ));
            session.runtime.state = SessionState::Idle;
            session.trajectory_dirty = false;
            session.subscribe()
        };
        app.chat
            .sessions
            .write()
            .await
            .insert(chat_id.to_string(), session_arc.clone());

        process_trajectory_change_for_source(
            gcx,
            chat_id,
            true,
            Some(TrajectorySourceIdentity::Normal),
        )
        .await;

        let event = wait_for_trajectory_event(&mut rx, chat_id).await;
        assert_eq!(event.event_type, "deleted");
        let session = session_arc.lock().await;
        assert_eq!(session.external_reload_pending, None);
        assert_eq!(session.thread.title, "Active Task Delete");
        assert_eq!(session.thread.task_meta, Some(active_task_meta));
        assert_eq!(session.messages.len(), 1);
        assert_eq!(
            session.messages[0].content.content_text_only(),
            "keep task during normal delete"
        );
        drop(session);
        assert_no_chat_event_for(&mut chat_rx, std::time::Duration::from_millis(100)).await;
    }

    #[tokio::test]
    async fn same_task_identity_update_and_delete_still_mutate_or_pend_active_task() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;
        let chat_id = "same-task-update-delete";
        let task_meta = task_meta(
            "task-same-source",
            "agents",
            Some("agent-5"),
            Some("card-5"),
            None,
        );
        let task_path = dir
            .path()
            .join(".refact")
            .join("tasks")
            .join("task-same-source")
            .join("trajectories")
            .join("agents")
            .join("agent-5")
            .join(format!("{chat_id}.json"));
        write_task_trajectory_file_with_user_message(
            &task_path,
            chat_id,
            "Same Task Updated",
            "same task replacement",
            &task_meta,
        )
        .await;
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        {
            let mut session = session_arc.lock().await;
            session.thread.task_meta = Some(task_meta.clone());
            session.thread.title = "Old Same Task".to_string();
            session.runtime.state = SessionState::Idle;
            session.trajectory_dirty = false;
        }
        app.chat
            .sessions
            .write()
            .await
            .insert(chat_id.to_string(), session_arc.clone());

        process_trajectory_change_for_source(
            gcx.clone(),
            chat_id,
            false,
            Some(TrajectorySourceIdentity::from_task_meta(&task_meta)),
        )
        .await;
        {
            let session = session_arc.lock().await;
            assert_eq!(session.external_reload_pending, None);
            assert_eq!(session.thread.title, "Same Task Updated");
            assert_eq!(session.thread.task_meta, Some(task_meta.clone()));
            assert_eq!(
                session.messages[0].content.content_text_only(),
                "same task replacement"
            );
        }
        tokio::fs::remove_file(&task_path).await.unwrap();
        {
            let mut session = session_arc.lock().await;
            session.runtime.state = SessionState::Generating;
            session.trajectory_dirty = true;
        }
        process_trajectory_change_for_source(
            gcx,
            chat_id,
            true,
            Some(TrajectorySourceIdentity::from_task_meta(&task_meta)),
        )
        .await;
        let session = session_arc.lock().await;
        assert_eq!(
            session.external_reload_pending,
            Some(ExternalReloadPending::delete(
                TrajectorySourceIdentity::from_task_meta(&task_meta)
            ))
        );
        assert_eq!(session.thread.title, "Same Task Updated");
    }

    #[tokio::test]
    async fn same_normal_identity_update_and_delete_still_mutate_or_pend_active_normal() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;
        let chat_id = "same-normal-update-delete";
        let normal_path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        write_trajectory_file_with_user_message(
            &normal_path,
            chat_id,
            "Same Normal Updated",
            "same normal replacement",
        )
        .await;
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        {
            let mut session = session_arc.lock().await;
            session.thread.title = "Old Same Normal".to_string();
            session.runtime.state = SessionState::Idle;
            session.trajectory_dirty = false;
        }
        app.chat
            .sessions
            .write()
            .await
            .insert(chat_id.to_string(), session_arc.clone());

        process_trajectory_change_for_source(
            gcx.clone(),
            chat_id,
            false,
            Some(TrajectorySourceIdentity::Normal),
        )
        .await;
        {
            let session = session_arc.lock().await;
            assert_eq!(session.external_reload_pending, None);
            assert_eq!(session.thread.title, "Same Normal Updated");
            assert!(session.thread.task_meta.is_none());
            assert_eq!(
                session.messages[0].content.content_text_only(),
                "same normal replacement"
            );
        }
        tokio::fs::remove_file(&normal_path).await.unwrap();
        {
            let mut session = session_arc.lock().await;
            session.runtime.state = SessionState::Generating;
            session.trajectory_dirty = true;
        }
        process_trajectory_change_for_source(
            gcx,
            chat_id,
            true,
            Some(TrajectorySourceIdentity::Normal),
        )
        .await;
        let session = session_arc.lock().await;
        assert_eq!(
            session.external_reload_pending,
            Some(ExternalReloadPending::delete(
                TrajectorySourceIdentity::Normal
            ))
        );
        assert_eq!(session.thread.title, "Same Normal Updated");
    }

    #[tokio::test]
    async fn generic_watcher_update_missing_file_emits_no_event() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;
        let mut rx = app.chat.trajectory_events_tx.subscribe();

        process_trajectory_change(gcx, "missing-update-no-event", false).await;

        assert_no_trajectory_event_for(&mut rx, std::time::Duration::from_millis(100)).await;
    }

    #[tokio::test]
    async fn generic_watcher_update_schema_incomplete_file_emits_no_event() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;
        let mut rx = app.chat.trajectory_events_tx.subscribe();
        let chat_id = "schema-update-no-event";
        let path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        write_schema_incomplete_trajectory_file(&path, chat_id).await;

        process_trajectory_change(gcx, chat_id, false).await;

        assert_no_trajectory_event_for(&mut rx, std::time::Duration::from_millis(100)).await;
    }

    #[tokio::test]
    async fn generic_watcher_update_id_mismatched_file_emits_no_event() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;
        let mut rx = app.chat.trajectory_events_tx.subscribe();
        let chat_id = "mismatch-update-no-event";
        let path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        write_trajectory_file(&path, "other-chat", "Mismatched", "2024-01-01T00:00:01Z").await;

        process_trajectory_change(gcx, chat_id, false).await;

        assert_no_trajectory_event_for(&mut rx, std::time::Duration::from_millis(100)).await;
    }

    #[tokio::test]
    async fn generic_watcher_update_valid_file_emits_metadata_rich_updated() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;
        let mut rx = app.chat.trajectory_events_tx.subscribe();
        let chat_id = "valid-update-rich-event";
        let path = dir
            .path()
            .join(".refact")
            .join("tasks")
            .join("task-update-rich")
            .join("trajectories")
            .join("planner")
            .join(format!("{chat_id}.json"));
        write_trajectory_file_with_metadata(
            &path,
            chat_id,
            "Watcher Update Rich",
            "2024-01-01T00:00:01Z",
            "valid update message",
        )
        .await;

        process_trajectory_change(gcx, chat_id, false).await;

        let event = wait_for_trajectory_event(&mut rx, chat_id).await;
        assert_eq!(event.event_type, "updated");
        assert_eq!(event.updated_at.as_deref(), Some("2024-01-01T00:00:01Z"));
        assert_eq!(event.title.as_deref(), Some("Watcher Update Rich"));
        assert_eq!(event.is_title_generated, Some(true));
        assert_eq!(event.message_count, Some(1));
        assert_eq!(event.parent_id.as_deref(), Some("parent-fallback"));
        assert_eq!(event.link_type.as_deref(), Some("handoff"));
        assert_eq!(event.root_chat_id.as_deref(), Some("root-fallback"));
        assert_eq!(event.task_id.as_deref(), Some("task-update-rich"));
        assert_eq!(event.task_role.as_deref(), Some("planner"));
        assert_eq!(event.model.as_deref(), Some("fallback-model"));
        assert_eq!(event.mode.as_deref(), Some("task_planner"));
        assert_eq!(event.session_state.as_deref(), Some("idle"));
    }

    #[tokio::test]
    async fn generic_watcher_update_missing_file_busy_generic_sets_pending_without_event() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;
        let mut rx = app.chat.trajectory_events_tx.subscribe();
        let chat_id = "missing-update-busy-pending";
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        {
            let mut session = session_arc.lock().await;
            session.runtime.state = SessionState::Generating;
            session.trajectory_dirty = false;
        }
        app.chat
            .sessions
            .write()
            .await
            .insert(chat_id.to_string(), session_arc.clone());

        process_trajectory_change(gcx, chat_id, false).await;

        let session = session_arc.lock().await;
        assert_eq!(
            session.external_reload_pending,
            Some(ExternalReloadPending::update(
                TrajectorySourceIdentity::Normal
            ))
        );
        drop(session);
        assert_no_trajectory_event_for(&mut rx, std::time::Duration::from_millis(100)).await;
    }

    #[tokio::test]
    async fn generic_watcher_update_missing_file_active_buddy_sets_no_pending_or_event() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;
        let mut rx = app.chat.trajectory_events_tx.subscribe();
        let chat_id = "missing-update-active-buddy-noop";
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        {
            let mut session = session_arc.lock().await;
            session.thread.mode = "buddy".to_string();
            session.thread.buddy_meta = Some(buddy_thread_meta());
            session.runtime.state = SessionState::Generating;
            session.trajectory_dirty = false;
        }
        app.chat
            .sessions
            .write()
            .await
            .insert(chat_id.to_string(), session_arc.clone());

        process_trajectory_change(gcx, chat_id, false).await;

        let session = session_arc.lock().await;
        assert_eq!(session.external_reload_pending, None);
        drop(session);
        assert_no_trajectory_event_for(&mut rx, std::time::Duration::from_millis(100)).await;
    }

    #[tokio::test]
    async fn immediate_external_delete_clears_existing_pending_reload_state() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;

        for (chat_id, pending) in [
            (
                "immediate-delete-clears-pending-update",
                ExternalReloadPending::update(TrajectorySourceIdentity::Normal),
            ),
            (
                "immediate-delete-clears-pending-delete",
                ExternalReloadPending::delete(TrajectorySourceIdentity::Normal),
            ),
        ] {
            let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
            {
                let mut session = session_arc.lock().await;
                session.messages.push(ChatMessage::new(
                    "user".to_string(),
                    "before immediate delete".to_string(),
                ));
                session.external_reload_pending = Some(pending);
                session.runtime.state = SessionState::Idle;
                session.trajectory_dirty = false;
                session.wake_up_at = Some(chrono::Utc::now() + chrono::Duration::minutes(10));
                session.waiting_for_card_ids = vec!["card-waiting".to_string()];
                session.thread.reactive_compact_attempts = Some(2);
            }
            app.chat
                .sessions
                .write()
                .await
                .insert(chat_id.to_string(), session_arc.clone());

            process_trajectory_change(gcx.clone(), chat_id, true).await;

            let session = session_arc.lock().await;
            assert_eq!(session.external_reload_pending, None);
            assert!(session.messages.is_empty());
            assert_eq!(session.thread.id, chat_id);
            assert!(session.wake_up_at.is_none());
            assert!(session.waiting_for_card_ids.is_empty());
            assert_eq!(session.thread.reactive_compact_attempts, None);
        }
    }

    #[tokio::test]
    async fn external_delete_then_save_does_not_persist_stale_delete_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;
        let chat_id = "external-delete-save-fresh-metadata";
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        let stale_created_at = "2024-01-01T00:00:00Z";
        {
            let mut session = session_arc.lock().await;
            session.messages.push(ChatMessage::new(
                "user".to_string(),
                "before external delete".to_string(),
            ));
            session.created_at = stale_created_at.to_string();
            session.wake_up_at = Some(chrono::Utc::now() + chrono::Duration::minutes(10));
            session.waiting_for_card_ids = vec!["card-stale".to_string()];
            session.thread.reactive_compact_attempts = Some(2);
            session.runtime.state = SessionState::Idle;
            session.trajectory_dirty = false;
        }
        app.chat
            .sessions
            .write()
            .await
            .insert(chat_id.to_string(), session_arc.clone());

        process_trajectory_change(gcx.clone(), chat_id, true).await;
        {
            let mut session = session_arc.lock().await;
            session.add_message(ChatMessage::new(
                "user".to_string(),
                "after external delete".to_string(),
            ));
        }
        try_save_trajectory(app, session_arc.clone()).await.unwrap();

        let path = nested_trajectory_path(dir.path(), chat_id, chat_id);
        let saved: serde_json::Value =
            serde_json::from_str(&tokio::fs::read_to_string(&path).await.unwrap()).unwrap();

        assert_ne!(saved["created_at"].as_str(), Some(stale_created_at));
        assert!(saved.get("wake_up_at").is_none());
        assert!(saved.get("waiting_for_card_ids").is_none());
        assert!(saved.get("reactive_compact_attempts").is_none());
        assert_eq!(saved["messages"].as_array().unwrap().len(), 1);
        assert_eq!(saved["messages"][0]["content"], "after external delete");
    }

    #[tokio::test]
    async fn pending_external_update_missing_file_clears_pending_state() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let chat_id = "pending-update-missing-file";
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        {
            let mut session = session_arc.lock().await;
            session.messages.push(ChatMessage::new(
                "user".to_string(),
                "keep after missing update".to_string(),
            ));
            session.external_reload_pending = Some(ExternalReloadPending::update(
                TrajectorySourceIdentity::Normal,
            ));
            session.runtime.state = SessionState::Idle;
            session.trajectory_dirty = false;
        }

        check_external_reload_pending(gcx, session_arc.clone()).await;

        let session = session_arc.lock().await;
        assert_eq!(session.external_reload_pending, None);
        assert_eq!(session.messages.len(), 1);
        assert_eq!(
            session.messages[0].content.content_text_only(),
            "keep after missing update"
        );
    }

    #[test]
    fn trajectory_snapshot_from_session_preserves_ui_only_diagnostics() {
        let mut session = ChatSession::new("ui-only-snapshot".to_string());
        let diagnostic = make_ui_only_error_message("context_length_exceeded");
        session
            .messages
            .push(ChatMessage::new("user".to_string(), "Hello".to_string()));
        session.messages.push(diagnostic.clone());

        let snapshot = trajectory_snapshot_from_session(&session);

        assert_eq!(snapshot.messages.len(), 2);
        assert!(snapshot.messages.iter().any(|message| {
            message.message_id == diagnostic.message_id
                && message.role == "error"
                && is_ui_only_message(message)
        }));
    }

    #[test]
    fn test_trajectory_snapshot_from_session_captures_fields() {
        use std::sync::Arc;
        use std::sync::atomic::AtomicBool;
        use tokio::sync::{broadcast, Mutex as AMutex, Notify};
        use std::collections::VecDeque;

        let (tx, _rx) = broadcast::channel(16);
        let session = ChatSession {
            chat_id: "test-123".to_string(),
            interruptible_waits: 0,
            wait_delivery_boundary: false,
            wait_interrupt_flag: Arc::new(AtomicBool::new(false)),
            derived_privacy_zones: crate::privacy::records::new_derived_privacy_zones(),
            tool_loop_guard: crate::tools::tool_call_loop_guard::ToolCallLoopGuard::new(),
            turn_investigation_tracker:
                crate::tools::tool_call_loop_guard::TurnInvestigationTracker::new(),
            thread: ThreadParams {
                id: "test-123".to_string(),
                title: "Test Thread".to_string(),
                model: "gpt-4".to_string(),
                mode: "AGENT".to_string(),
                tool_use: "agent".to_string(),
                boost_reasoning: Some(true),
                reasoning_effort: None,
                thinking_budget: None,
                temperature: None,
                frequency_penalty: None,
                max_tokens: None,
                parallel_tool_calls: None,
                context_tokens_cap: Some(8000),
                auto_compression_cap: Some(7000),
                auto_compression_cap_pending: false,
                include_project_info: false,
                checkpoints_enabled: true,
                is_title_generated: true,
                auto_approve_editing_tools: false,
                auto_approve_dangerous_commands: false,
                autonomous_no_confirm: false,
                task_meta: None,
                worktree: None,
                parent_id: Some("parent-chat-id".to_string()),
                link_type: Some("subagent".to_string()),
                root_chat_id: Some("root-chat-id".to_string()),
                previous_response_id: None,
                browser_meta: None,
                active_skill: None,
                auto_enrichment_enabled: None,
                buddy_meta: None,
                auto_compact_enabled: None,
                agent_nudge_enabled: None,
                strip_reasoning_from_prompt: None,
                frozen_request_prefix: None,
                claude_code_identity: None,
                reactive_compact_attempts: None,
            },
            messages: vec![ChatMessage::new("user".to_string(), "Hello".to_string())],
            runtime: super::super::types::RuntimeState::default(),
            goal: None,
            goal_active: false,
            goal_status: None,
            goal_turns_used: 0,
            goal_tokens_used: 0,
            goal_no_progress_turns: 0,
            is_compressing: false,
            compression_phase: None,
            compression_reason: None,
            compression_estimated_tokens: None,
            compression_effective_cap: None,
            compression_cap_source: None,
            compression_attempt_generation: 0,
            active_compression_attempt: None,
            compression_attempt_started_at_ms: None,
            compression_abort_flag: None,
            draft_message: None,
            draft_usage: None,
            command_queue: VecDeque::new(),
            pending_deliveries: VecDeque::new(),
            runner_pending_deliveries: Vec::new(),
            delivered_delivery_ids: Default::default(),
            turn_depth: 0,
            delivery_wake_sources: Default::default(),
            event_seq: 0,
            event_tx: tx,
            recent_request_ids: VecDeque::new(),
            recent_request_ids_set: std::collections::HashSet::new(),
            abort_flag: Arc::new(AtomicBool::new(false)),
            abort_notify: Arc::new(Notify::new()),
            user_interrupt_flag: Arc::new(AtomicBool::new(false)),
            queue_processor_running: Arc::new(AtomicBool::new(false)),
            queue_notify: Arc::new(Notify::new()),
            queue_processor_counters: Arc::new(
                crate::chat::types::QueueProcessorCounters::default(),
            ),
            last_activity: Instant::now(),
            last_stream_delta_at: None,
            command_enqueued_at: std::collections::HashMap::new(),
            stream_started_at: None,
            confirmation_paused_at: None,
            last_tool_started_at: None,
            last_tool_progress_at: None,
            trajectory_dirty: false,
            trajectory_version: 5,
            trajectory_committed_version: 0,
            trajectory_save_in_flight: false,
            trajectory_save_queued: false,
            trajectory_save_mutex: Arc::new(AMutex::new(())),
            trajectory_commit_notify: Arc::new(Notify::new()),
            trajectory_save_error: None,
            created_at: "2024-01-01T00:00:00Z".to_string(),
            closed: false,
            closed_flag: Arc::new(AtomicBool::new(false)),
            external_reload_pending: None,
            last_prompt_messages: Vec::new(),
            tool_catalog: None,
            turn_tool_pool: None,
            pending_context_rebuild: None,
            pending_mode_handoff: None,
            last_rebuild_attempt_version: None,
            pending_max_new_tokens_boost: None,
            cache_guard_snapshot: None,
            cache_guard_request_generation: 0,
            cache_guard_snapshot_generation: 0,
            cache_guard_reset_generation: 0,
            cache_guard_force_next: false,
            provider_usage_stale: false,
            task_agent_error: None,
            trajectory_events_tx: None,
            pending_browser_message: None,
            post_tool_side_effects: VecDeque::new(),
            active_command: ActiveCommandContext::default(),
            skills_available_count: 0,
            skills_included: Vec::new(),
            pending_skill_deactivation: None,
            post_turn_task_handles: Vec::new(),
            openai_codex_websocket: Default::default(),
            suppress_auto_enrichment_for_next_turn: false,
            enrichment_identities: std::collections::HashSet::new(),
            wake_up_at: None,
            waiting_for_card_ids: Vec::new(),
            background_agents: std::collections::HashMap::new(),
            goal_stopped_by_abort: false,
            goal_ledger: Vec::new(),
            goal_turn_evidence: false,
            goal_verification_blocked_until_ms: None,
        };

        let snapshot = trajectory_snapshot_from_session(&session);
        assert_eq!(snapshot.chat_id, "test-123");
        assert_eq!(snapshot.title, "Test Thread");
        assert_eq!(snapshot.model, "gpt-4");
        assert_eq!(snapshot.mode, "AGENT");
        assert!(snapshot.boost_reasoning);
        assert_eq!(snapshot.context_tokens_cap, Some(8000));
        assert_eq!(snapshot.auto_compression_cap, Some(7000));
        assert!(!snapshot.include_project_info);
        assert!(snapshot.is_title_generated);
        assert_eq!(snapshot.version, 5);
        assert_eq!(snapshot.messages.len(), 1);
    }

    #[test]
    fn test_trajectory_roundtrip_active_skill() {
        use super::super::types::*;
        use super::super::types::ActiveCommandContext;
        use std::sync::Arc;
        use std::sync::atomic::AtomicBool;
        use tokio::sync::{broadcast, Mutex as AMutex, Notify};
        use std::collections::VecDeque;
        use std::time::Instant;

        let (tx, _rx) = broadcast::channel(16);
        let mut session = ChatSession {
            chat_id: "skill-test".to_string(),
            interruptible_waits: 0,
            wait_delivery_boundary: false,
            wait_interrupt_flag: Arc::new(AtomicBool::new(false)),
            derived_privacy_zones: crate::privacy::records::new_derived_privacy_zones(),
            tool_loop_guard: crate::tools::tool_call_loop_guard::ToolCallLoopGuard::new(),
            turn_investigation_tracker:
                crate::tools::tool_call_loop_guard::TurnInvestigationTracker::new(),
            thread: ThreadParams {
                id: "skill-test".to_string(),
                active_skill: Some("my-skill".to_string()),
                ..Default::default()
            },
            messages: vec![ChatMessage::new("user".to_string(), "Hello".to_string())],
            runtime: RuntimeState::default(),
            goal: None,
            goal_active: false,
            goal_status: None,
            goal_turns_used: 0,
            goal_tokens_used: 0,
            goal_no_progress_turns: 0,
            is_compressing: false,
            compression_phase: None,
            compression_reason: None,
            compression_estimated_tokens: None,
            compression_effective_cap: None,
            compression_cap_source: None,
            compression_attempt_generation: 0,
            active_compression_attempt: None,
            compression_attempt_started_at_ms: None,
            compression_abort_flag: None,
            draft_message: None,
            draft_usage: None,
            command_queue: VecDeque::new(),
            pending_deliveries: VecDeque::new(),
            runner_pending_deliveries: Vec::new(),
            delivered_delivery_ids: Default::default(),
            turn_depth: 0,
            delivery_wake_sources: Default::default(),
            event_seq: 0,
            event_tx: tx,
            recent_request_ids: VecDeque::new(),
            recent_request_ids_set: std::collections::HashSet::new(),
            abort_flag: Arc::new(AtomicBool::new(false)),
            abort_notify: Arc::new(Notify::new()),
            user_interrupt_flag: Arc::new(AtomicBool::new(false)),
            queue_processor_running: Arc::new(AtomicBool::new(false)),
            queue_notify: Arc::new(Notify::new()),
            queue_processor_counters: Arc::new(
                crate::chat::types::QueueProcessorCounters::default(),
            ),
            last_activity: Instant::now(),
            last_stream_delta_at: None,
            command_enqueued_at: std::collections::HashMap::new(),
            stream_started_at: None,
            confirmation_paused_at: None,
            last_tool_started_at: None,
            last_tool_progress_at: None,
            trajectory_dirty: false,
            trajectory_version: 1,
            trajectory_committed_version: 0,
            trajectory_save_in_flight: false,
            trajectory_save_queued: false,
            trajectory_save_mutex: Arc::new(AMutex::new(())),
            trajectory_commit_notify: Arc::new(Notify::new()),
            trajectory_save_error: None,
            created_at: "2024-01-01T00:00:00Z".to_string(),
            closed: false,
            closed_flag: Arc::new(AtomicBool::new(false)),
            external_reload_pending: None,
            last_prompt_messages: Vec::new(),
            tool_catalog: None,
            turn_tool_pool: None,
            pending_context_rebuild: None,
            pending_mode_handoff: None,
            last_rebuild_attempt_version: None,
            pending_max_new_tokens_boost: None,
            cache_guard_snapshot: None,
            cache_guard_request_generation: 0,
            cache_guard_snapshot_generation: 0,
            cache_guard_reset_generation: 0,
            cache_guard_force_next: false,
            provider_usage_stale: false,
            task_agent_error: None,
            trajectory_events_tx: None,
            pending_browser_message: None,
            post_tool_side_effects: VecDeque::new(),
            active_command: ActiveCommandContext::default(),
            skills_available_count: 0,
            skills_included: Vec::new(),
            pending_skill_deactivation: None,
            post_turn_task_handles: Vec::new(),
            openai_codex_websocket: Default::default(),
            suppress_auto_enrichment_for_next_turn: false,
            enrichment_identities: std::collections::HashSet::new(),
            wake_up_at: None,
            waiting_for_card_ids: Vec::new(),
            background_agents: std::collections::HashMap::new(),
            goal_stopped_by_abort: false,
            goal_ledger: Vec::new(),
            goal_turn_evidence: false,
            goal_verification_blocked_until_ms: None,
        };

        let snapshot = trajectory_snapshot_from_session(&session);
        assert_eq!(snapshot.active_skill, Some("my-skill".to_string()));

        session.thread.active_skill = None;
        let snapshot_none = trajectory_snapshot_from_session(&session);
        assert!(snapshot_none.active_skill.is_none());
    }

    #[test]
    fn trajectory_snapshot_from_session_captures_wake_up_at() {
        let wake_up_at = chrono::Utc::now() + chrono::Duration::minutes(5);
        let mut session = ChatSession::new("wake-snapshot".to_string());
        session.wake_up_at = Some(wake_up_at);

        let snapshot = trajectory_snapshot_from_session(&session);

        assert_eq!(snapshot.wake_up_at, Some(wake_up_at));
    }

    #[tokio::test]
    async fn reopen_preserves_pinned_plan() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let mut plan_extra = serde_json::Map::new();
        plan_extra.insert(
            "plan".to_string(),
            json!({
                "mode": "agent",
                "version": 1,
                "created_at_ms": 123,
                "supersedes": null,
            }),
        );
        let plan = ChatMessage {
            role: "plan".to_string(),
            content: ChatContent::SimpleText("base plan".to_string()),
            preserve: Some(true),
            extra: plan_extra,
            ..Default::default()
        };
        let delta = crate::chat::internal_roles::plan_delta(
            "tool.set_plan",
            json!({"seq": 1}),
            "append update",
        );
        let mut session = ChatSession::new("plan-reopen".to_string());
        session.created_at = "2024-01-01T00:00:00Z".to_string();
        session.add_message(plan);
        session.add_message(delta);

        let snapshot = trajectory_snapshot_from_session(&session);
        save_trajectory_snapshot(gcx.clone(), snapshot)
            .await
            .unwrap();

        let loaded = load_trajectory_for_chat(gcx, "plan-reopen").await.unwrap();
        let plans: Vec<_> = loaded
            .messages
            .iter()
            .filter(|message| message.role == "plan")
            .collect();
        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0].content.content_text_only(), "base plan");
        assert_eq!(plans[0].preserve, Some(true));
        assert_eq!(plans[0].extra["plan"]["version"], json!(1));
        let deltas: Vec<_> = loaded
            .messages
            .iter()
            .filter(|message| {
                message.role == "event" && message.extra["event"]["subkind"] == json!("plan_delta")
            })
            .collect();
        assert_eq!(deltas.len(), 1);
        assert_eq!(deltas[0].content.content_text_only(), "append update");
    }

    #[tokio::test]
    async fn legacy_fallback_ids_are_distinct_for_repeated_hidden_events() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let trajectories_dir = dir.path().join(".refact").join("trajectories");
        tokio::fs::create_dir_all(&trajectories_dir).await.unwrap();
        tokio::fs::write(
            trajectories_dir.join("legacy-hidden-events.json"),
            serde_json::to_string(&json!({
                "id": "legacy-hidden-events",
                "title": "Legacy Hidden Events",
                "model": "model",
                "mode": "agent",
                "tool_use": "agent",
                "messages": [
                    {
                        "role": "event",
                        "content": "append update",
                        "extra": {
                            "event": {
                                "subkind": "plan_delta",
                                "source": "tool.update_plan",
                                "payload": {"seq": 1}
                            }
                        }
                    },
                    {
                        "role": "event",
                        "content": "append update",
                        "extra": {
                            "event": {
                                "subkind": "plan_delta",
                                "source": "tool.update_plan",
                                "payload": {"seq": 2}
                            }
                        }
                    },
                    {
                        "message_id": "kept-existing-id",
                        "role": "user",
                        "content": "visible"
                    }
                ],
                "created_at": "2024-01-01T00:00:00Z",
                "updated_at": "2024-01-01T00:00:00Z",
                "include_project_info": true,
                "checkpoints_enabled": true
            }))
            .unwrap(),
        )
        .await
        .unwrap();

        let loaded = load_trajectory_for_chat(gcx, "legacy-hidden-events")
            .await
            .unwrap();

        assert_eq!(loaded.messages.len(), 3);
        assert!(loaded.messages[0].message_id.starts_with("legacy:event:"));
        assert!(loaded.messages[1].message_id.starts_with("legacy:event:"));
        assert_ne!(loaded.messages[0].message_id, loaded.messages[1].message_id);
        assert_eq!(loaded.messages[2].message_id, "kept-existing-id");
    }

    #[tokio::test]
    async fn wake_up_at_round_trips_through_trajectory_save_and_load() {
        let dir = tempfile::tempdir().unwrap();
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let app = AppState::from_gcx(gcx.clone()).await;
        {
            *app.workspace
                .documents_state
                .workspace_folders
                .lock()
                .unwrap() = vec![dir.path().to_path_buf()];
        }

        let wake_up_at = chrono::Utc::now() + chrono::Duration::minutes(5);
        let mut session = ChatSession::new("wake-roundtrip".to_string());
        session.thread.title = "Wake Roundtrip".to_string();
        session.created_at = "2024-01-01T00:00:00Z".to_string();
        session.wake_up_at = Some(wake_up_at);
        session.add_message(ChatMessage::new("user".to_string(), "wait".to_string()));

        let snapshot = trajectory_snapshot_from_session(&session);
        save_trajectory_snapshot(gcx.clone(), snapshot)
            .await
            .unwrap();

        drop(session);

        let loaded = load_trajectory_for_chat(gcx, "wake-roundtrip")
            .await
            .unwrap();
        assert_eq!(loaded.wake_up_at, Some(wake_up_at));
    }

    #[test]
    fn trajectory_snapshot_from_session_preserves_goal_projection() {
        let mut session = ChatSession::new("goal-snapshot".to_string());
        session.install_goal("agent", "ship the goal", true, GoalBudget::default());
        session.goal.as_mut().unwrap().progress = GoalProgress {
            turns_used: 2,
            tokens_used: 345,
            started_at_ms: 123,
            no_progress_turns: 1,
            last_nudge_at_ms: 456,
            cost_used_cents: 0,
        };
        session.refresh_goal_runtime_mirror();

        let snapshot = trajectory_snapshot_from_session(&session);
        let goal = snapshot.goal.expect("goal snapshot");

        assert_eq!(goal.content, "ship the goal");
        assert_eq!(goal.status, GoalStatus::Active);
        assert_eq!(goal.progress.turns_used, 2);
        assert_eq!(goal.progress.tokens_used, 345);
        assert_eq!(goal.progress.no_progress_turns, 1);
    }

    #[test]
    fn clamp_goal_snapshot_for_load_migrates_implicit_legacy_budget() {
        let goal = GoalSnapshot {
            content: "ship legacy".to_string(),
            version: 1,
            active: true,
            status: GoalStatus::BudgetExhausted,
            budget: GoalBudget::legacy_default_hard_limits(),
            progress: GoalProgress {
                turns_used: 10,
                ..Default::default()
            },
            attempts: Vec::new(),
            events: Vec::new(),
            criteria: Vec::new(),
            snoozed_until_ms: None,
            stop_reason: None,
            transferred_from: None,
            transferred_to: None,
        };

        let loaded = clamp_goal_snapshot_for_load(goal);

        assert_eq!(loaded.budget, GoalBudget::default());
        assert_eq!(loaded.status, GoalStatus::Active);
    }

    #[test]
    fn clamp_goal_snapshot_for_load_heals_stale_verifying() {
        let goal = GoalSnapshot {
            content: "ship it".to_string(),
            version: 1,
            active: true,
            status: GoalStatus::Verifying,
            budget: GoalBudget::default(),
            progress: GoalProgress::default(),
            attempts: Vec::new(),
            events: Vec::new(),
            criteria: Vec::new(),
            snoozed_until_ms: None,
            stop_reason: None,
            transferred_from: None,
            transferred_to: None,
        };
        assert_eq!(
            clamp_goal_snapshot_for_load(goal).status,
            GoalStatus::Active
        );

        let inactive = GoalSnapshot {
            content: "ship it".to_string(),
            version: 1,
            active: false,
            status: GoalStatus::Verifying,
            budget: GoalBudget::default(),
            progress: GoalProgress::default(),
            attempts: Vec::new(),
            events: Vec::new(),
            criteria: Vec::new(),
            snoozed_until_ms: None,
            stop_reason: None,
            transferred_from: None,
            transferred_to: None,
        };
        assert_eq!(
            clamp_goal_snapshot_for_load(inactive).status,
            GoalStatus::Paused
        );
    }

    #[test]
    fn clamp_goal_snapshot_for_load_keeps_stopped_goals_stopped() {
        let goal = GoalSnapshot {
            content: "ship it".to_string(),
            version: 1,
            active: true,
            status: GoalStatus::Stopped,
            budget: GoalBudget::default(),
            progress: GoalProgress::default(),
            attempts: Vec::new(),
            events: Vec::new(),
            criteria: Vec::new(),
            snoozed_until_ms: None,
            stop_reason: None,
            transferred_from: None,
            transferred_to: None,
        };
        assert_eq!(
            clamp_goal_snapshot_for_load(goal).status,
            GoalStatus::Stopped
        );
    }

    #[test]
    fn clamp_goal_snapshot_for_load_preserves_explicit_legacy_budget() {
        let budget = GoalBudget {
            explicit: true,
            ..GoalBudget::legacy_default_hard_limits()
        };
        let goal = GoalSnapshot {
            content: "ship explicit".to_string(),
            version: 1,
            active: true,
            status: GoalStatus::BudgetExhausted,
            budget,
            progress: GoalProgress {
                turns_used: 12,
                ..Default::default()
            },
            attempts: Vec::new(),
            events: Vec::new(),
            criteria: Vec::new(),
            snoozed_until_ms: None,
            stop_reason: None,
            transferred_from: None,
            transferred_to: None,
        };

        let loaded = clamp_goal_snapshot_for_load(goal);

        assert_eq!(loaded.budget.max_turns, Some(10));
        assert_eq!(loaded.budget.max_minutes, Some(15));
        assert_eq!(loaded.budget.max_tokens, Some(200_000));
        assert_eq!(loaded.budget.no_progress_turns, Some(2));
        assert!(loaded.budget.explicit);
        assert_eq!(loaded.progress.turns_used, 10);
        assert_eq!(loaded.status, GoalStatus::BudgetExhausted);
    }

    #[tokio::test]
    async fn goal_round_trips_through_trajectory_save_and_load() {
        let dir = tempfile::tempdir().unwrap();
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let app = AppState::from_gcx(gcx.clone()).await;
        {
            *app.workspace
                .documents_state
                .workspace_folders
                .lock()
                .unwrap() = vec![dir.path().to_path_buf()];
        }

        let mut session = ChatSession::new("goal-roundtrip".to_string());
        session.thread.title = "Goal Roundtrip".to_string();
        session.created_at = "2024-01-01T00:00:00Z".to_string();
        session.install_goal("agent", "finish the card", true, GoalBudget::default());
        session.add_message(crate::chat::internal_roles::goal_delta(
            "tool.update_goal",
            json!({"seq": 1, "at_ms": 999}),
            "add tests",
        ));
        session.goal.as_mut().unwrap().progress = GoalProgress {
            turns_used: 3,
            tokens_used: 456,
            started_at_ms: 111,
            no_progress_turns: 1,
            last_nudge_at_ms: 222,
            cost_used_cents: 0,
        };
        session.refresh_goal_runtime_mirror();

        let started_at_ms = session.goal.as_ref().unwrap().progress.started_at_ms;
        let snapshot = trajectory_snapshot_from_session(&session);
        save_trajectory_snapshot(gcx.clone(), snapshot)
            .await
            .unwrap();

        let loaded = load_trajectory_for_chat(gcx, "goal-roundtrip")
            .await
            .unwrap();
        let loaded_goal = loaded.goal.clone().expect("loaded goal");

        assert_eq!(
            loaded_goal.content,
            "finish the card\n\n---\n\n## Goal updates\n\nadd tests"
        );
        assert_eq!(loaded_goal.status, GoalStatus::Active);
        assert_eq!(loaded_goal.progress.turns_used, 3);
        assert_eq!(loaded_goal.progress.tokens_used, 456);
        assert_eq!(loaded_goal.progress.no_progress_turns, 1);
        assert_eq!(loaded_goal.progress.started_at_ms, started_at_ms);
        assert_eq!(loaded_goal.transferred_from, None);
        assert_eq!(loaded_goal.transferred_to, None);
        assert!(loaded.messages.iter().any(|message| message.role == "goal"));
        assert!(loaded.messages.iter().any(|message| {
            message.role == "event" && message.extra["event"]["subkind"] == json!("goal_delta")
        }));

        let rehydrated = ChatSession::new_with_trajectory(
            "goal-roundtrip".to_string(),
            loaded.messages,
            loaded.thread,
            loaded.created_at,
            loaded.wake_up_at,
            loaded.waiting_for_card_ids,
            loaded.goal,
        );
        assert_eq!(rehydrated.goal_turns_used, 3);
        assert_eq!(rehydrated.goal_tokens_used, 456);
        assert_eq!(rehydrated.goal_no_progress_turns, 1);
        assert_eq!(rehydrated.event_seq, 0);
    }

    #[tokio::test]
    async fn wake_up_at_is_none_in_trajectories_created_before_field_existed() {
        let dir = tempfile::tempdir().unwrap();
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let app = AppState::from_gcx(gcx.clone()).await;
        {
            *app.workspace
                .documents_state
                .workspace_folders
                .lock()
                .unwrap() = vec![dir.path().to_path_buf()];
        }

        let trajectories_dir = dir.path().join(".refact").join("trajectories");
        tokio::fs::create_dir_all(&trajectories_dir).await.unwrap();
        tokio::fs::write(
            trajectories_dir.join("legacy-wake.json"),
            r#"{
                "id":"legacy-wake",
                "title":"Legacy",
                "created_at":"2024-01-01T00:00:00Z",
                "updated_at":"2024-01-01T00:00:00Z",
                "model":"model",
                "mode":"agent",
                "tool_use":"agent",
                "messages":[{"role":"user","content":"hello"}],
                "include_project_info":true,
                "checkpoints_enabled":true
            }"#,
        )
        .await
        .unwrap();

        let loaded = load_trajectory_for_chat(gcx, "legacy-wake").await.unwrap();
        assert!(loaded.wake_up_at.is_none());
    }

    #[tokio::test]
    async fn goal_ledger_round_trips_and_replay_wins_over_missing_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let app = AppState::from_gcx(gcx.clone()).await;
        {
            *app.workspace
                .documents_state
                .workspace_folders
                .lock()
                .unwrap() = vec![dir.path().to_path_buf()];
        }

        let mut session = ChatSession::new("goal-ledger-roundtrip".to_string());
        session.thread.title = "Ledger Roundtrip".to_string();
        session.created_at = "2024-01-01T00:00:00Z".to_string();
        session.install_goal("agent", "ship the card", true, GoalBudget::default());
        session.goal_record_progress_with_cost(50, false, 2);
        session.goal_set_status_reason(GoalStatus::Stopped, "goal_control");
        session.goal_verification_blocked_until_ms = Some(4_242);

        let mut snapshot = trajectory_snapshot_from_session(&session);
        assert!(!snapshot.goal_ledger.is_empty());
        snapshot.goal = None;
        save_trajectory_snapshot(gcx.clone(), snapshot)
            .await
            .unwrap();

        let loaded = load_trajectory_for_chat(gcx, "goal-ledger-roundtrip")
            .await
            .unwrap();
        assert!(!loaded.goal_ledger.is_empty());
        let loaded_goal = loaded.goal.expect("goal restored from ledger replay");
        assert_eq!(loaded_goal.status, GoalStatus::Stopped);
        assert_eq!(loaded_goal.stop_reason.as_deref(), Some("goal_control"));
        assert_eq!(loaded_goal.progress.turns_used, 1);
        assert_eq!(loaded_goal.progress.tokens_used, 50);
        assert_eq!(loaded_goal.progress.cost_used_cents, 2);
        assert_eq!(loaded_goal.progress.no_progress_turns, 1);
        assert_eq!(loaded.goal_verification_blocked_until_ms, Some(4_242));
    }

    #[tokio::test]
    async fn trajectory_snapshot_roundtrip_preserves_waiting_for_card_ids() {
        let dir = tempfile::tempdir().unwrap();
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let app = AppState::from_gcx(gcx.clone()).await;
        {
            *app.workspace
                .documents_state
                .workspace_folders
                .lock()
                .unwrap() = vec![dir.path().to_path_buf()];
        }

        let card_ids = vec!["T-1".to_string(), "T-10".to_string(), "T-2".to_string()];
        let mut session = ChatSession::new("wait-card-roundtrip".to_string());
        session.thread.title = "Wait Card Roundtrip".to_string();
        session.created_at = "2024-01-01T00:00:00Z".to_string();
        session.waiting_for_card_ids = card_ids.clone();
        session.add_message(ChatMessage::new("user".to_string(), "waiting".to_string()));

        let snapshot = trajectory_snapshot_from_session(&session);
        assert_eq!(snapshot.waiting_for_card_ids, card_ids);
        save_trajectory_snapshot(gcx.clone(), snapshot)
            .await
            .unwrap();

        let loaded = load_trajectory_for_chat(gcx, "wait-card-roundtrip")
            .await
            .unwrap();
        assert_eq!(loaded.waiting_for_card_ids, card_ids);
    }

    #[test]
    fn test_trajectory_load_without_active_skill_field() {
        let json_str = r#"{"id":"chat-1","title":"T","model":"m","mode":"agent","tool_use":"agent","messages":[],"created_at":"2024-01-01T00:00:00Z","updated_at":"2024-01-01T00:00:00Z","include_project_info":true,"checkpoints_enabled":true}"#;
        let t: serde_json::Value = serde_json::from_str(json_str).unwrap();
        let active_skill = t
            .get("active_skill")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        assert!(
            active_skill.is_none(),
            "Old trajectories must load with active_skill = None"
        );
    }

    #[test]
    fn trajectory_worktree_snapshot_from_session_captures_thread_worktree() {
        let worktree = trajectory_worktree_sample();
        let mut session = ChatSession::new("wt-snapshot".to_string());
        session.thread.worktree = Some(worktree.clone());
        let snapshot = trajectory_snapshot_from_session(&session);
        assert_eq!(snapshot.worktree, Some(worktree));
    }

    #[test]
    fn trajectory_snapshot_from_session_filters_empty_assistant_messages() {
        let mut session = ChatSession::new("empty-assistant-snapshot".to_string());
        session.messages.push(ChatMessage {
            role: "user".to_string(),
            content: ChatContent::SimpleText("hello".to_string()),
            ..Default::default()
        });
        session.messages.push(ChatMessage {
            role: "assistant".to_string(),
            content: ChatContent::SimpleText("   \n".to_string()),
            ..Default::default()
        });
        session.messages.push(ChatMessage {
            role: "error".to_string(),
            content: ChatContent::SimpleText("LLM error".to_string()),
            ..Default::default()
        });

        let snapshot = trajectory_snapshot_from_session(&session);

        assert_eq!(snapshot.messages.len(), 2);
        assert_eq!(snapshot.messages[0].role, "user");
        assert_eq!(snapshot.messages[1].role, "error");
    }

    #[test]
    fn trajectory_snapshot_from_session_preserves_ui_only_messages() {
        let mut session = ChatSession::new("ui-only-snapshot".to_string());
        session
            .messages
            .push(ChatMessage::new("user".to_string(), "visible".to_string()));
        session
            .messages
            .push(make_ui_only_error_message("context_length_exceeded"));

        let snapshot = trajectory_snapshot_from_session(&session);

        assert_eq!(snapshot.messages.len(), 2);
        assert_eq!(snapshot.messages[0].role, "user");
        assert_eq!(snapshot.messages[1].role, "error");
        assert!(is_ui_only_message(&snapshot.messages[1]));
    }

    #[test]
    fn trajectory_snapshot_from_session_filters_metadata_only_assistant_messages() {
        use crate::call_validation::ChatUsage;

        let mut session = ChatSession::new("metadata-only-assistant-snapshot".to_string());
        session.messages.push(ChatMessage {
            role: "user".to_string(),
            content: ChatContent::SimpleText("hello".to_string()),
            ..Default::default()
        });
        session.messages.push(ChatMessage {
            role: "assistant".to_string(),
            usage: Some(ChatUsage {
                prompt_tokens: 10,
                completion_tokens: 0,
                total_tokens: 10,
                cache_creation_tokens: None,
                cache_read_tokens: None,
                metering_usd: None,
            }),
            extra: serde_json::Map::from_iter([(
                "openai_response_id".to_string(),
                json!("resp_123"),
            )]),
            ..Default::default()
        });
        session.messages.push(ChatMessage {
            role: "assistant".to_string(),
            content: ChatContent::SimpleText("visible".to_string()),
            ..Default::default()
        });

        let snapshot = trajectory_snapshot_from_session(&session);

        assert_eq!(snapshot.messages.len(), 2);
        assert_eq!(snapshot.messages[0].role, "user");
        assert_eq!(snapshot.messages[1].role, "assistant");
        assert_eq!(snapshot.messages[1].content.content_text_only(), "visible");
    }

    #[test]
    fn trajectory_worktree_meta_creation_omits_unvalidated_worktree() {
        let worktree = trajectory_worktree_sample();
        let mut extra = serde_json::Map::new();
        extra.insert(
            "worktree".to_string(),
            serde_json::to_value(&worktree).unwrap(),
        );
        let data = TrajectoryData {
            id: "meta-chat".to_string(),
            title: "Meta".to_string(),
            created_at: "2024-01-01T00:00:00Z".to_string(),
            updated_at: "2024-01-01T00:00:00Z".to_string(),
            model: "model".to_string(),
            mode: "agent".to_string(),
            tool_use: "agent".to_string(),
            messages: Vec::new(),
            extra,
        };
        let meta = trajectory_data_to_meta(&data);
        assert!(meta.worktree.is_none());
    }

    #[test]
    fn trajectory_worktree_invalid_extra_is_not_preserved() {
        let mut extra = serde_json::Map::new();
        extra.insert("worktree".to_string(), json!({"root":"/tmp/untrusted"}));
        let worktree = sanitize_worktree_extra(&mut extra);
        assert!(worktree.is_none());
        assert!(extra.get("worktree").is_none());
    }

    #[tokio::test]
    async fn trajectory_save_removes_malformed_worktree_extra_from_json() {
        let dir = tempfile::tempdir().unwrap();
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let app = AppState::from_gcx(gcx.clone()).await;
        {
            *app.workspace
                .documents_state
                .workspace_folders
                .lock()
                .unwrap() = vec![dir.path().to_path_buf()];
        }
        let chat_id = "malformed-worktree-save";
        let payload = json!({
            "id": chat_id,
            "title": "Malformed Worktree",
            "model": "m",
            "mode": "agent",
            "tool_use": "agent",
            "messages": [],
            "created_at": "2024-01-01T00:00:00Z",
            "updated_at": "2024-01-01T00:00:00Z",
            "include_project_info": true,
            "checkpoints_enabled": true,
            "worktree": {"root":"/tmp/untrusted"}
        });

        handle_v1_trajectories_save(
            State(app),
            AxumPath(chat_id.to_string()),
            hyper::body::Bytes::from(serde_json::to_vec(&payload).unwrap()),
        )
        .await
        .unwrap();

        let path = nested_trajectory_path(dir.path(), chat_id, chat_id);
        let saved: serde_json::Value =
            serde_json::from_str(&tokio::fs::read_to_string(path).await.unwrap()).unwrap();
        assert!(saved.get("worktree").is_none());
    }

    #[tokio::test]
    async fn trajectory_save_preserves_valid_worktree_extra_in_json() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("repo");
        let cache = dir.path().join("cache");
        std::fs::create_dir_all(&source).unwrap();
        init_repo(&source);
        let gcx = crate::global_context::tests::make_test_gcx_with_dirs(
            cache.clone(),
            std::env::temp_dir().join(format!("refact-cfg-{}", uuid::Uuid::new_v4())),
        )
        .await;
        {
            *gcx.documents_state.workspace_folders.lock().unwrap() = vec![source.clone()];
        }
        let app = AppState::from_gcx(gcx.clone()).await;
        let service = WorktreeService::new(cache, source.clone()).unwrap();
        let created = service
            .create_worktree(crate::worktrees::types::CreateWorktreeRequest {
                branch: Some("refact/chat/save-preserve".to_string()),
                kind: Some("chat".to_string()),
                ..Default::default()
            })
            .await
            .unwrap();
        let worktree = created.worktree.meta.clone();
        let chat_id = "valid-worktree-save";
        let payload = json!({
            "id": chat_id,
            "title": "Valid Worktree",
            "model": "m",
            "mode": "agent",
            "tool_use": "agent",
            "messages": [],
            "created_at": "2024-01-01T00:00:00Z",
            "updated_at": "2024-01-01T00:00:00Z",
            "include_project_info": true,
            "checkpoints_enabled": true,
            "worktree": serde_json::to_value(&worktree).unwrap()
        });

        handle_v1_trajectories_save(
            State(app),
            AxumPath(chat_id.to_string()),
            hyper::body::Bytes::from(serde_json::to_vec(&payload).unwrap()),
        )
        .await
        .unwrap();

        let path = nested_trajectory_path(&source, chat_id, chat_id);
        let saved: serde_json::Value =
            serde_json::from_str(&tokio::fs::read_to_string(path).await.unwrap()).unwrap();
        assert_eq!(saved["worktree"]["id"], worktree.id);
        assert_eq!(saved["worktree"]["branch"], "refact/chat/save-preserve");
    }

    #[tokio::test]
    async fn trajectory_worktree_save_load_roundtrips_top_level_field() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("repo");
        let cache = dir.path().join("cache");
        std::fs::create_dir_all(&source).unwrap();
        init_repo(&source);
        let gcx = crate::global_context::tests::make_test_gcx_with_dirs(
            cache.clone(),
            std::env::temp_dir().join(format!("refact-cfg-{}", uuid::Uuid::new_v4())),
        )
        .await;
        {
            *gcx.documents_state.workspace_folders.lock().unwrap() = vec![source.clone()];
        }
        let app = AppState::from_gcx(gcx.clone()).await;
        let service = WorktreeService::new(cache, source.clone()).unwrap();
        let created = service
            .create_worktree(crate::worktrees::types::CreateWorktreeRequest {
                branch: Some("refact/chat/roundtrip".to_string()),
                kind: Some("chat".to_string()),
                ..Default::default()
            })
            .await
            .unwrap();

        let worktree = created.worktree.meta.clone();
        let chat_id = "wt-roundtrip".to_string();
        let snapshot = TrajectorySnapshot {
            goal: None,
            goal_ledger: Vec::new(),
            pending_deliveries: Vec::new(),
            goal_verification_blocked_until_ms: None,
            chat_id: chat_id.clone(),
            title: "Worktree Chat".to_string(),
            model: "model".to_string(),
            mode: "agent".to_string(),
            tool_use: "agent".to_string(),
            messages: vec![ChatMessage::new("user".to_string(), "Hello".to_string())],
            created_at: "2024-01-01T00:00:00Z".to_string(),
            boost_reasoning: false,
            checkpoints_enabled: true,
            context_tokens_cap: None,
            auto_compression_cap: None,
            auto_compression_cap_pending: true,
            include_project_info: true,
            is_title_generated: true,
            auto_approve_editing_tools: false,
            auto_approve_dangerous_commands: false,
            autonomous_no_confirm: false,
            version: 1,
            task_meta: None,
            worktree: Some(worktree.clone()),
            parent_id: None,
            link_type: None,
            root_chat_id: None,
            reasoning_effort: None,
            thinking_budget: None,
            temperature: None,
            frequency_penalty: None,
            max_tokens: None,
            parallel_tool_calls: None,
            previous_response_id: None,
            active_skill: None,
            auto_enrichment_enabled: None,
            buddy_meta: None,
            auto_compact_enabled: None,
            frozen_request_prefix: None,
            claude_code_identity: None,
            reactive_compact_attempts: None,
            wake_up_at: None,
            waiting_for_card_ids: Vec::new(),
        };

        save_trajectory_snapshot(gcx.clone(), snapshot)
            .await
            .unwrap();
        let path = nested_trajectory_path(&source, &chat_id, &chat_id);
        let raw = tokio::fs::read_to_string(path).await.unwrap();
        let raw_json: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(raw_json["worktree"]["id"], worktree.id);
        assert_eq!(raw_json["worktree"]["branch"], "refact/chat/roundtrip");

        let loaded = load_trajectory_for_chat(gcx.clone(), &chat_id)
            .await
            .unwrap();
        assert_eq!(loaded.thread.worktree, Some(worktree.clone()));
        let listed = list_all_trajectories_meta(app, false).await.unwrap();
        let listed_worktree = listed
            .iter()
            .find(|item| item.id == chat_id)
            .and_then(|item| item.worktree.clone())
            .unwrap();
        assert_eq!(listed_worktree.id, worktree.id);
    }

    #[tokio::test]
    async fn frozen_request_prefix_and_claude_code_identity_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let tools_canonical = json!([
            {
                "type": "function",
                "function": {
                    "name": "cat",
                    "description": "Read a file",
                    "parameters": {"type": "object"}
                }
            }
        ]);
        let frozen_request_prefix = FrozenRequestPrefix {
            schema_version: 1,
            created_at: "2026-05-29T00:00:00Z".to_string(),
            system_prompt: Some("verbatim system".to_string()),
            tools_canonical: Some(tools_canonical.clone()),
        };
        let claude_code_identity = ClaudeCodeIdentity {
            device_id: "device-123".to_string(),
            session_id: "session-456".to_string(),
        };
        let mut snapshot = test_snapshot(
            "frozen-prefix-roundtrip",
            "Frozen Prefix",
            vec![ChatMessage::new("user".to_string(), "hello".to_string())],
        );
        snapshot.frozen_request_prefix = Some(frozen_request_prefix.clone());
        snapshot.claude_code_identity = Some(claude_code_identity.clone());

        save_trajectory_snapshot(gcx.clone(), snapshot)
            .await
            .unwrap();

        let path = nested_trajectory_path(
            dir.path(),
            "frozen-prefix-roundtrip",
            "frozen-prefix-roundtrip",
        );
        let raw: serde_json::Value =
            serde_json::from_str(&tokio::fs::read_to_string(path).await.unwrap()).unwrap();
        assert_eq!(
            raw["frozen_request_prefix"]["tools_canonical"],
            tools_canonical
        );
        assert_eq!(raw["claude_code_identity"]["device_id"], "device-123");

        let loaded = load_trajectory_for_chat(gcx, "frozen-prefix-roundtrip")
            .await
            .unwrap();
        assert_eq!(
            loaded.thread.frozen_request_prefix,
            Some(frozen_request_prefix)
        );
        assert_eq!(
            loaded.thread.claude_code_identity,
            Some(claude_code_identity)
        );
    }

    #[tokio::test]
    async fn frozen_request_prefix_migrates_system_prompt_from_legacy_trajectory() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let traj_dir = dir.path().join(".refact").join("trajectories");
        tokio::fs::create_dir_all(&traj_dir).await.unwrap();
        tokio::fs::write(
            traj_dir.join("legacy-frozen-prefix.json"),
            r#"{
                "id":"legacy-frozen-prefix",
                "title":"Legacy",
                "created_at":"2024-01-01T00:00:00Z",
                "updated_at":"2024-01-01T00:00:00Z",
                "model":"model",
                "mode":"agent",
                "tool_use":"agent",
                "messages":[
                    {"role":"system","content":"legacy system"},
                    {"role":"user","content":"hello"}
                ],
                "include_project_info":true,
                "checkpoints_enabled":true
            }"#,
        )
        .await
        .unwrap();

        let loaded = load_trajectory_for_chat(gcx, "legacy-frozen-prefix")
            .await
            .unwrap();
        let prefix = loaded.thread.frozen_request_prefix.unwrap();
        assert_eq!(prefix.system_prompt.as_deref(), Some("legacy system"));
        assert!(prefix.tools_canonical.is_none());
        assert!(loaded.thread.claude_code_identity.is_none());
    }

    #[tokio::test]
    async fn mode_transition_load_discards_stale_copied_frozen_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let traj_dir = dir.path().join(".refact").join("trajectories");
        tokio::fs::create_dir_all(&traj_dir).await.unwrap();
        tokio::fs::write(
            traj_dir.join("transition-stale-frozen.json"),
            serde_json::to_string(&json!({
                "id":"transition-stale-frozen",
                "title":"Transition",
                "created_at":"2024-01-01T00:00:00Z",
                "updated_at":"2024-01-01T00:00:00Z",
                "model":"model",
                "mode":"task_planner",
                "tool_use":"agent",
                "parent_id":"source-chat",
                "link_type":"mode_transition",
                "previous_response_id":"resp_source",
                "messages":[
                    {"role":"system","content":"target task planner system"},
                    {"role":"user","content":"hello"}
                ],
                "frozen_request_prefix": {
                    "schema_version": 1,
                    "created_at": "2026-05-29T00:00:00Z",
                    "system_prompt": "old planning system",
                    "tools_canonical": [{"type":"function","function":{"name":"old_tool"}}]
                },
                "claude_code_identity": {
                    "device_id":"source-device",
                    "session_id":"source-session"
                },
                "include_project_info":true,
                "checkpoints_enabled":true
            }))
            .unwrap(),
        )
        .await
        .unwrap();

        let loaded = load_trajectory_for_chat(gcx, "transition-stale-frozen")
            .await
            .unwrap();
        let prefix = loaded.thread.frozen_request_prefix.unwrap();
        assert_eq!(
            prefix.system_prompt.as_deref(),
            Some("target task planner system")
        );
        assert!(prefix.tools_canonical.is_none());
        assert!(loaded.thread.claude_code_identity.is_none());
        assert!(loaded.thread.previous_response_id.is_none());
    }

    #[tokio::test]
    async fn mode_transition_no_frozen_prefix_with_copied_provider_state_is_repaired() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let traj_dir = dir.path().join(".refact").join("trajectories");
        tokio::fs::create_dir_all(&traj_dir).await.unwrap();
        tokio::fs::write(
            traj_dir.join("transition-no-prefix-provider.json"),
            serde_json::to_string(&json!({
                "id":"transition-no-prefix-provider",
                "title":"Transition",
                "created_at":"2024-01-02T00:00:00Z",
                "updated_at":"2024-01-02T00:00:00Z",
                "model":"model",
                "mode":"task_planner",
                "tool_use":"agent",
                "parent_id":"source-chat",
                "link_type":"mode_transition",
                "previous_response_id":"resp_source",
                "messages":[
                    {"role":"system","content":"target task planner system"},
                    {"role":"user","content":"hello"}
                ],
                "claude_code_identity": {
                    "device_id":"source-device",
                    "session_id":"source-session"
                },
                "include_project_info":true,
                "checkpoints_enabled":true
            }))
            .unwrap(),
        )
        .await
        .unwrap();

        let loaded = load_trajectory_for_chat(gcx, "transition-no-prefix-provider")
            .await
            .unwrap();
        let prefix = loaded.thread.frozen_request_prefix.unwrap();
        assert_eq!(
            prefix.system_prompt.as_deref(),
            Some("target task planner system")
        );
        assert!(prefix.tools_canonical.is_none());
        assert!(loaded.thread.claude_code_identity.is_none());
        assert!(loaded.thread.previous_response_id.is_none());
    }

    #[tokio::test]
    async fn mode_transition_matching_system_with_stale_tools_is_repaired_by_timestamp() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let traj_dir = dir.path().join(".refact").join("trajectories");
        tokio::fs::create_dir_all(&traj_dir).await.unwrap();
        tokio::fs::write(
            traj_dir.join("transition-stale-tools-timestamp.json"),
            serde_json::to_string(&json!({
                "id":"transition-stale-tools-timestamp",
                "title":"Transition",
                "created_at":"2024-01-02T00:00:00Z",
                "updated_at":"2024-01-02T00:00:00Z",
                "model":"model",
                "mode":"task_planner",
                "tool_use":"agent",
                "parent_id":"source-chat",
                "link_type":"mode_transition",
                "messages":[
                    {"role":"system","content":"target task planner system"},
                    {"role":"user","content":"hello"}
                ],
                "frozen_request_prefix": {
                    "schema_version": 1,
                    "created_at": "2024-01-01T00:00:00Z",
                    "system_prompt": "target task planner system",
                    "tools_canonical": [{"type":"function","function":{"name":"old_source_tool"}}]
                },
                "include_project_info":true,
                "checkpoints_enabled":true
            }))
            .unwrap(),
        )
        .await
        .unwrap();

        let loaded = load_trajectory_for_chat(gcx, "transition-stale-tools-timestamp")
            .await
            .unwrap();
        let prefix = loaded.thread.frozen_request_prefix.unwrap();
        assert_eq!(
            prefix.system_prompt.as_deref(),
            Some("target task planner system")
        );
        assert!(prefix.tools_canonical.is_none());
        assert!(loaded.thread.claude_code_identity.is_none());
        assert!(loaded.thread.previous_response_id.is_none());
    }

    #[tokio::test]
    async fn mode_transition_malformed_frozen_prefix_with_provider_identity_is_repaired() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let traj_dir = dir.path().join(".refact").join("trajectories");
        tokio::fs::create_dir_all(&traj_dir).await.unwrap();
        tokio::fs::write(
            traj_dir.join("transition-malformed-prefix-provider.json"),
            serde_json::to_string(&json!({
                "id":"transition-malformed-prefix-provider",
                "title":"Transition",
                "created_at":"2024-01-02T00:00:00Z",
                "updated_at":"2024-01-02T00:00:00Z",
                "model":"model",
                "mode":"task_planner",
                "tool_use":"agent",
                "parent_id":"source-chat",
                "link_type":"mode_transition",
                "previous_response_id":"resp_source",
                "messages":[
                    {"role":"system","content":"target task planner system"},
                    {"role":"user","content":"hello"}
                ],
                "frozen_request_prefix": {
                    "schema_version": "bad",
                    "created_at": 7,
                    "system_prompt": ["source system"],
                    "tools_canonical": [{"type":"function","function":{"name":"old_source_tool"}}]
                },
                "claude_code_identity": {
                    "device_id":"source-device",
                    "session_id":"source-session"
                },
                "include_project_info":true,
                "checkpoints_enabled":true
            }))
            .unwrap(),
        )
        .await
        .unwrap();

        let loaded = load_trajectory_for_chat(gcx, "transition-malformed-prefix-provider")
            .await
            .unwrap();
        let prefix = loaded.thread.frozen_request_prefix.unwrap();
        assert_eq!(
            prefix.system_prompt.as_deref(),
            Some("target task planner system")
        );
        assert!(prefix.tools_canonical.is_none());
        assert!(loaded.thread.claude_code_identity.is_none());
        assert!(loaded.thread.previous_response_id.is_none());
    }

    #[tokio::test]
    async fn mode_transition_invalid_prefix_timestamp_with_provider_state_is_repaired() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let traj_dir = dir.path().join(".refact").join("trajectories");
        tokio::fs::create_dir_all(&traj_dir).await.unwrap();
        tokio::fs::write(
            traj_dir.join("transition-invalid-prefix-time-provider.json"),
            serde_json::to_string(&json!({
                "id":"transition-invalid-prefix-time-provider",
                "title":"Transition",
                "created_at":"2024-01-02T00:00:00Z",
                "updated_at":"2024-01-02T00:00:00Z",
                "model":"model",
                "mode":"task_planner",
                "tool_use":"agent",
                "parent_id":"source-chat",
                "link_type":"mode_transition",
                "previous_response_id":"resp_source",
                "messages":[
                    {"role":"system","content":"target task planner system"},
                    {"role":"user","content":"hello"}
                ],
                "frozen_request_prefix": {
                    "schema_version": 1,
                    "created_at": "not-a-date",
                    "system_prompt": "target task planner system",
                    "tools_canonical": [{"type":"function","function":{"name":"old_source_tool"}}]
                },
                "claude_code_identity": {
                    "device_id":"source-device",
                    "session_id":"source-session"
                },
                "include_project_info":true,
                "checkpoints_enabled":true
            }))
            .unwrap(),
        )
        .await
        .unwrap();

        let loaded = load_trajectory_for_chat(gcx, "transition-invalid-prefix-time-provider")
            .await
            .unwrap();
        let prefix = loaded.thread.frozen_request_prefix.unwrap();
        assert_eq!(
            prefix.system_prompt.as_deref(),
            Some("target task planner system")
        );
        assert!(prefix.tools_canonical.is_none());
        assert!(loaded.thread.claude_code_identity.is_none());
        assert!(loaded.thread.previous_response_id.is_none());
        assert!(loaded.transition_identity_repaired);
    }

    #[tokio::test]
    async fn mode_transition_invalid_trajectory_created_at_with_tools_and_provider_state_is_repaired(
    ) {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let traj_dir = dir.path().join(".refact").join("trajectories");
        tokio::fs::create_dir_all(&traj_dir).await.unwrap();
        tokio::fs::write(
            traj_dir.join("transition-invalid-trajectory-time-provider.json"),
            serde_json::to_string(&json!({
                "id":"transition-invalid-trajectory-time-provider",
                "title":"Transition",
                "created_at":"not-a-date",
                "updated_at":"2024-01-02T00:00:00Z",
                "model":"model",
                "mode":"task_planner",
                "tool_use":"agent",
                "parent_id":"source-chat",
                "link_type":"mode_transition",
                "previous_response_id":"resp_source",
                "messages":[
                    {"role":"system","content":"target task planner system"},
                    {"role":"user","content":"hello"}
                ],
                "frozen_request_prefix": {
                    "schema_version": 1,
                    "created_at": "2024-01-03T00:00:00Z",
                    "system_prompt": "target task planner system",
                    "tools_canonical": [{"type":"function","function":{"name":"old_source_tool"}}]
                },
                "claude_code_identity": {
                    "device_id":"source-device",
                    "session_id":"source-session"
                },
                "include_project_info":true,
                "checkpoints_enabled":true
            }))
            .unwrap(),
        )
        .await
        .unwrap();

        let loaded = load_trajectory_for_chat(gcx, "transition-invalid-trajectory-time-provider")
            .await
            .unwrap();
        let prefix = loaded.thread.frozen_request_prefix.unwrap();
        assert_eq!(
            prefix.system_prompt.as_deref(),
            Some("target task planner system")
        );
        assert!(prefix.tools_canonical.is_none());
        assert!(loaded.thread.claude_code_identity.is_none());
        assert!(loaded.thread.previous_response_id.is_none());
        assert!(loaded.transition_identity_repaired);
    }

    #[tokio::test]
    async fn mode_transition_missing_trajectory_created_at_with_tools_and_provider_state_is_repaired(
    ) {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let traj_dir = dir.path().join(".refact").join("trajectories");
        tokio::fs::create_dir_all(&traj_dir).await.unwrap();
        tokio::fs::write(
            traj_dir.join("transition-missing-trajectory-time-provider.json"),
            serde_json::to_string(&json!({
                "id":"transition-missing-trajectory-time-provider",
                "title":"Transition",
                "updated_at":"2024-01-02T00:00:00Z",
                "model":"model",
                "mode":"task_planner",
                "tool_use":"agent",
                "parent_id":"source-chat",
                "link_type":"mode_transition",
                "previous_response_id":"resp_source",
                "messages":[
                    {"role":"system","content":"target task planner system"},
                    {"role":"user","content":"hello"}
                ],
                "frozen_request_prefix": {
                    "schema_version": 1,
                    "created_at": "2024-01-03T00:00:00Z",
                    "system_prompt": "target task planner system",
                    "tools_canonical": [{"type":"function","function":{"name":"old_source_tool"}}]
                },
                "claude_code_identity": {
                    "device_id":"source-device",
                    "session_id":"source-session"
                },
                "include_project_info":true,
                "checkpoints_enabled":true
            }))
            .unwrap(),
        )
        .await
        .unwrap();

        let loaded = load_trajectory_for_chat(gcx, "transition-missing-trajectory-time-provider")
            .await
            .unwrap();
        let prefix = loaded.thread.frozen_request_prefix.unwrap();
        assert_eq!(
            prefix.system_prompt.as_deref(),
            Some("target task planner system")
        );
        assert!(prefix.tools_canonical.is_none());
        assert!(loaded.thread.claude_code_identity.is_none());
        assert!(loaded.thread.previous_response_id.is_none());
        assert!(loaded.transition_identity_repaired);
    }

    #[tokio::test]
    async fn mode_transition_missing_frozen_system_with_tools_is_repaired() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let traj_dir = dir.path().join(".refact").join("trajectories");
        tokio::fs::create_dir_all(&traj_dir).await.unwrap();
        tokio::fs::write(
            traj_dir.join("transition-missing-system-tools.json"),
            serde_json::to_string(&json!({
                "id":"transition-missing-system-tools",
                "title":"Transition",
                "created_at":"2024-01-01T00:00:00Z",
                "updated_at":"2024-01-01T00:00:00Z",
                "model":"model",
                "mode":"task_planner",
                "tool_use":"agent",
                "parent_id":"source-chat",
                "link_type":"mode_transition",
                "messages":[
                    {"role":"system","content":"target task planner system"},
                    {"role":"user","content":"hello"}
                ],
                "frozen_request_prefix": {
                    "schema_version": 1,
                    "created_at": "2024-01-02T00:00:00Z",
                    "system_prompt": null,
                    "tools_canonical": [{"type":"function","function":{"name":"old_source_tool"}}]
                },
                "include_project_info":true,
                "checkpoints_enabled":true
            }))
            .unwrap(),
        )
        .await
        .unwrap();

        let loaded = load_trajectory_for_chat(gcx, "transition-missing-system-tools")
            .await
            .unwrap();
        let prefix = loaded.thread.frozen_request_prefix.unwrap();
        assert_eq!(
            prefix.system_prompt.as_deref(),
            Some("target task planner system")
        );
        assert!(prefix.tools_canonical.is_none());
        assert!(loaded.transition_identity_repaired);
    }

    #[tokio::test]
    async fn handoff_load_discards_stale_copied_frozen_prefix_and_provider_state() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let traj_dir = dir.path().join(".refact").join("trajectories");
        tokio::fs::create_dir_all(&traj_dir).await.unwrap();
        tokio::fs::write(
            traj_dir.join("handoff-stale-frozen-provider.json"),
            serde_json::to_string(&json!({
                "id":"handoff-stale-frozen-provider",
                "title":"Handoff",
                "created_at":"2024-01-02T00:00:00Z",
                "updated_at":"2024-01-02T00:00:00Z",
                "model":"model",
                "mode":"agent",
                "tool_use":"agent",
                "parent_id":"source-chat",
                "link_type":"handoff",
                "previous_response_id":"resp_source",
                "messages":[
                    {"role":"system","content":"target handoff system"},
                    {"role":"user","content":"hello"}
                ],
                "frozen_request_prefix": {
                    "schema_version": 1,
                    "created_at": "2024-01-01T00:00:00Z",
                    "system_prompt": "source system",
                    "tools_canonical": [{"type":"function","function":{"name":"old_source_tool"}}]
                },
                "claude_code_identity": {
                    "device_id":"source-device",
                    "session_id":"source-session"
                },
                "include_project_info":true,
                "checkpoints_enabled":true
            }))
            .unwrap(),
        )
        .await
        .unwrap();

        let loaded = load_trajectory_for_chat(gcx, "handoff-stale-frozen-provider")
            .await
            .unwrap();
        let prefix = loaded.thread.frozen_request_prefix.unwrap();
        assert_eq!(
            prefix.system_prompt.as_deref(),
            Some("target handoff system")
        );
        assert!(prefix.tools_canonical.is_none());
        assert!(loaded.thread.claude_code_identity.is_none());
        assert!(loaded.thread.previous_response_id.is_none());
        assert!(loaded.transition_identity_repaired);
    }

    #[tokio::test]
    async fn mode_transition_preserves_legitimate_prefix_created_after_trajectory() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let traj_dir = dir.path().join(".refact").join("trajectories");
        tokio::fs::create_dir_all(&traj_dir).await.unwrap();
        tokio::fs::write(
            traj_dir.join("transition-legitimate-prefix.json"),
            serde_json::to_string(&json!({
                "id":"transition-legitimate-prefix",
                "title":"Transition",
                "created_at":"2024-01-01T00:00:00Z",
                "updated_at":"2024-01-01T00:00:00Z",
                "model":"model",
                "mode":"task_planner",
                "tool_use":"agent",
                "parent_id":"source-chat",
                "link_type":"mode_transition",
                "messages":[
                    {"role":"system","content":"target task planner system"},
                    {"role":"user","content":"hello"}
                ],
                "frozen_request_prefix": {
                    "schema_version": 1,
                    "created_at": "2024-01-02T00:00:00Z",
                    "system_prompt": "target task planner system",
                    "tools_canonical": [{"type":"function","function":{"name":"target_tool"}}]
                },
                "include_project_info":true,
                "checkpoints_enabled":true
            }))
            .unwrap(),
        )
        .await
        .unwrap();

        let loaded = load_trajectory_for_chat(gcx, "transition-legitimate-prefix")
            .await
            .unwrap();
        let prefix = loaded.thread.frozen_request_prefix.unwrap();
        assert_eq!(
            prefix.tools_canonical,
            Some(json!([{"type":"function","function":{"name":"target_tool"}}]))
        );
        assert!(!loaded.transition_identity_repaired);
    }

    #[tokio::test]
    async fn ordinary_branch_load_preserves_copied_frozen_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let traj_dir = dir.path().join(".refact").join("trajectories");
        tokio::fs::create_dir_all(&traj_dir).await.unwrap();
        tokio::fs::write(
            traj_dir.join("branch-preserve-frozen.json"),
            serde_json::to_string(&json!({
                "id":"branch-preserve-frozen",
                "title":"Branch",
                "created_at":"2024-01-01T00:00:00Z",
                "updated_at":"2024-01-01T00:00:00Z",
                "model":"model",
                "mode":"agent",
                "tool_use":"agent",
                "parent_id":"source-chat",
                "link_type":"branch",
                "previous_response_id":"resp_source",
                "messages":[
                    {"role":"system","content":"branch system"},
                    {"role":"user","content":"hello"}
                ],
                "frozen_request_prefix": {
                    "schema_version": 1,
                    "created_at": "2026-05-29T00:00:00Z",
                    "system_prompt": "source system",
                    "tools_canonical": [{"type":"function","function":{"name":"source_tool"}}]
                },
                "claude_code_identity": {
                    "device_id":"source-device",
                    "session_id":"source-session"
                },
                "include_project_info":true,
                "checkpoints_enabled":true
            }))
            .unwrap(),
        )
        .await
        .unwrap();

        let loaded = load_trajectory_for_chat(gcx, "branch-preserve-frozen")
            .await
            .unwrap();
        let prefix = loaded.thread.frozen_request_prefix.unwrap();
        assert_eq!(prefix.system_prompt.as_deref(), Some("source system"));
        assert_eq!(
            prefix.tools_canonical,
            Some(json!([{"type":"function","function":{"name":"source_tool"}}]))
        );
        assert!(loaded.thread.claude_code_identity.is_some());
        assert_eq!(
            loaded.thread.previous_response_id.as_deref(),
            Some("resp_source")
        );
    }

    #[tokio::test]
    async fn mode_transition_open_persists_repaired_frozen_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let (_gcx, app) = make_app_with_workspace(dir.path()).await;
        let chat_id = "transition-open-repair";
        let trajectories_dir = dir.path().join(".refact").join("trajectories");
        tokio::fs::create_dir_all(&trajectories_dir).await.unwrap();
        let path = trajectories_dir.join(format!("{chat_id}.json"));
        tokio::fs::write(
            &path,
            serde_json::to_string(&json!({
                "id": chat_id,
                "title": "Transition Open",
                "model": "model",
                "mode": "task_planner",
                "tool_use": "agent",
                "parent_id": "source-chat",
                "link_type": "mode_transition",
                "previous_response_id": "resp_source",
                "messages": [
                    {"role":"system","content":"target task planner system"},
                    {"role":"user","content":"hello"}
                ],
                "frozen_request_prefix": {
                    "schema_version": 1,
                    "created_at": "2026-05-29T00:00:00Z",
                    "system_prompt": "old planning system",
                    "tools_canonical": [{"type":"function","function":{"name":"old_tool"}}]
                },
                "claude_code_identity": {
                    "device_id":"source-device",
                    "session_id":"source-session"
                },
                "created_at": "2024-01-01T00:00:00Z",
                "updated_at": "2024-01-01T00:00:00Z",
                "include_project_info": true,
                "checkpoints_enabled": true,
                "browser_meta": {
                    "browser_runtime_id": "browser-open",
                    "tab_urls": ["https://example.com/open"],
                    "active_tab_id": "tab-open",
                    "attach_screenshot_on_send": true
                },
                "custom_future_field": {
                    "keep": true,
                    "nested": {"value": 43}
                }
            }))
            .unwrap(),
        )
        .await
        .unwrap();

        let session_arc = crate::chat::get_or_create_session_with_trajectory(
            app.clone(),
            &app.chat.sessions,
            chat_id,
        )
        .await;
        {
            let session = session_arc.lock().await;
            let prefix = session.thread.frozen_request_prefix.as_ref().unwrap();
            assert_eq!(
                prefix.system_prompt.as_deref(),
                Some("target task planner system")
            );
            assert!(prefix.tools_canonical.is_none());
            assert!(session.thread.claude_code_identity.is_none());
        }

        let raw: serde_json::Value =
            serde_json::from_str(&tokio::fs::read_to_string(path).await.unwrap()).unwrap();
        assert_eq!(
            raw["frozen_request_prefix"]["system_prompt"],
            "target task planner system"
        );
        assert!(raw["frozen_request_prefix"]["tools_canonical"].is_null());
        assert!(raw.get("claude_code_identity").is_none());
        assert!(raw.get("previous_response_id").is_none());
        assert_eq!(raw["browser_meta"]["browser_runtime_id"], "browser-open");
        assert_eq!(raw["custom_future_field"]["nested"]["value"], 43);
    }

    #[tokio::test]
    async fn mode_transition_no_system_provider_cleanup_persists_on_open() {
        let dir = tempfile::tempdir().unwrap();
        let (_gcx, app) = make_app_with_workspace(dir.path()).await;
        let chat_id = "transition-no-system-provider-open";
        let trajectories_dir = dir.path().join(".refact").join("trajectories");
        tokio::fs::create_dir_all(&trajectories_dir).await.unwrap();
        let path = trajectories_dir.join(format!("{chat_id}.json"));
        tokio::fs::write(
            &path,
            serde_json::to_string(&json!({
                "id": chat_id,
                "title": "Transition Open",
                "model": "model",
                "mode": "task_planner",
                "tool_use": "agent",
                "parent_id": "source-chat",
                "link_type": "mode_transition",
                "previous_response_id": "resp_source",
                "messages": [
                    {"role":"user","content":"hello without system"}
                ],
                "claude_code_identity": {
                    "device_id":"source-device",
                    "session_id":"source-session"
                },
                "created_at": "2024-01-01T00:00:00Z",
                "updated_at": "2024-01-01T00:00:00Z",
                "include_project_info": true,
                "checkpoints_enabled": true,
                "browser_meta": {
                    "browser_runtime_id": "browser-immediate",
                    "tab_urls": ["https://example.com/immediate"],
                    "active_tab_id": "tab-immediate",
                    "attach_screenshot_on_send": false
                },
                "custom_future_field": {
                    "keep": true,
                    "nested": {"value": 44}
                }
            }))
            .unwrap(),
        )
        .await
        .unwrap();

        let session_arc = crate::chat::get_or_create_session_with_trajectory(
            app.clone(),
            &app.chat.sessions,
            chat_id,
        )
        .await;
        {
            let session = session_arc.lock().await;
            assert!(session.thread.frozen_request_prefix.is_none());
            assert!(session.thread.claude_code_identity.is_none());
            assert!(session.thread.previous_response_id.is_none());
        }

        let raw: serde_json::Value =
            serde_json::from_str(&tokio::fs::read_to_string(path).await.unwrap()).unwrap();
        assert!(raw.get("frozen_request_prefix").is_none());
        assert!(raw.get("claude_code_identity").is_none());
        assert!(raw.get("previous_response_id").is_none());
        assert_eq!(
            raw["browser_meta"]["browser_runtime_id"],
            "browser-immediate"
        );
        assert_eq!(raw["custom_future_field"]["nested"]["value"], 44);
    }

    #[tokio::test]
    async fn mode_transition_empty_provider_cleanup_persists_on_open() {
        let dir = tempfile::tempdir().unwrap();
        let (_gcx, app) = make_app_with_workspace(dir.path()).await;
        let chat_id = "transition-empty-provider-open";
        let trajectories_dir = dir.path().join(".refact").join("trajectories");
        tokio::fs::create_dir_all(&trajectories_dir).await.unwrap();
        let path = trajectories_dir.join(format!("{chat_id}.json"));
        tokio::fs::write(
            &path,
            serde_json::to_string(&json!({
                "id": chat_id,
                "title": "Transition Open",
                "model": "model",
                "mode": "task_planner",
                "tool_use": "agent",
                "parent_id": "source-chat",
                "link_type": "mode_transition",
                "previous_response_id": "resp_source",
                "messages": [],
                "claude_code_identity": {
                    "device_id":"source-device",
                    "session_id":"source-session"
                },
                "created_at": "2024-01-01T00:00:00Z",
                "updated_at": "2024-01-01T00:00:00Z",
                "include_project_info": true,
                "checkpoints_enabled": true
            }))
            .unwrap(),
        )
        .await
        .unwrap();

        let session_arc = crate::chat::get_or_create_session_with_trajectory(
            app.clone(),
            &app.chat.sessions,
            chat_id,
        )
        .await;
        {
            let session = session_arc.lock().await;
            assert!(session.messages.is_empty());
            assert!(session.thread.frozen_request_prefix.is_none());
            assert!(session.thread.claude_code_identity.is_none());
            assert!(session.thread.previous_response_id.is_none());
        }

        let raw: serde_json::Value =
            serde_json::from_str(&tokio::fs::read_to_string(path).await.unwrap()).unwrap();
        assert_eq!(raw["messages"].as_array().unwrap().len(), 0);
        assert!(raw.get("frozen_request_prefix").is_none());
        assert!(raw.get("claude_code_identity").is_none());
        assert!(raw.get("previous_response_id").is_none());
    }

    #[tokio::test]
    async fn mode_transition_empty_provider_cleanup_persists_to_original_global_path_on_open() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;
        let chat_id = "transition-empty-provider-global-open";
        let global_trajectories_dir = get_global_trajectories_dir(gcx.clone()).await;
        tokio::fs::create_dir_all(&global_trajectories_dir)
            .await
            .unwrap();
        let path = global_trajectories_dir.join(format!("{chat_id}.json"));
        tokio::fs::write(
            &path,
            serde_json::to_string(&json!({
                "id": chat_id,
                "title": "Transition Global Open",
                "model": "model",
                "mode": "task_planner",
                "tool_use": "agent",
                "parent_id": "source-chat",
                "link_type": "mode_transition",
                "previous_response_id": "resp_source",
                "messages": [],
                "claude_code_identity": {
                    "device_id":"source-device",
                    "session_id":"source-session"
                },
                "created_at": "2024-01-01T00:00:00Z",
                "updated_at": "2024-01-01T00:00:00Z",
                "include_project_info": true,
                "checkpoints_enabled": true,
                "browser_meta": {
                    "browser_runtime_id": "browser-global",
                    "tab_urls": ["https://example.com/global"],
                    "active_tab_id": "tab-global",
                    "attach_screenshot_on_send": true
                },
                "custom_future_field": {
                    "keep": true,
                    "nested": {"value": 46}
                }
            }))
            .unwrap(),
        )
        .await
        .unwrap();
        let project_path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));

        let session_arc = crate::chat::get_or_create_session_with_trajectory(
            app.clone(),
            &app.chat.sessions,
            chat_id,
        )
        .await;
        {
            let session = session_arc.lock().await;
            assert!(session.messages.is_empty());
            assert!(session.thread.frozen_request_prefix.is_none());
            assert!(session.thread.claude_code_identity.is_none());
            assert!(session.thread.previous_response_id.is_none());
        }

        let raw: serde_json::Value =
            serde_json::from_str(&tokio::fs::read_to_string(&path).await.unwrap()).unwrap();
        assert_eq!(raw["messages"].as_array().unwrap().len(), 0);
        assert!(raw.get("frozen_request_prefix").is_none());
        assert!(raw.get("claude_code_identity").is_none());
        assert!(raw.get("previous_response_id").is_none());
        assert_eq!(raw["browser_meta"]["browser_runtime_id"], "browser-global");
        assert_eq!(raw["custom_future_field"]["nested"]["value"], 46);
        assert!(!tokio::fs::try_exists(project_path).await.unwrap());
    }

    #[tokio::test]
    async fn mode_transition_global_placeholder_title_save_uses_original_path_without_workspace() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let chat_id = "transition-global-placeholder-title-save";
        let global_trajectories_dir = get_global_trajectories_dir(gcx.clone()).await;
        tokio::fs::create_dir_all(&global_trajectories_dir)
            .await
            .unwrap();
        let path = global_trajectories_dir.join(format!("{chat_id}.json"));
        tokio::fs::write(
            &path,
            serde_json::to_string(&json!({
                "id": chat_id,
                "title": "New Chat",
                "model": "model",
                "mode": "task_planner",
                "tool_use": "agent",
                "parent_id": "source-chat",
                "link_type": "mode_transition",
                "previous_response_id": "resp_source",
                "messages": [{"role":"user","content":"hello from global"}],
                "claude_code_identity": {
                    "device_id":"source-device",
                    "session_id":"source-session"
                },
                "created_at": "2024-01-01T00:00:00Z",
                "updated_at": "2024-01-01T00:00:00Z",
                "include_project_info": true,
                "checkpoints_enabled": true,
                "isTitleGenerated": false
            }))
            .unwrap(),
        )
        .await
        .unwrap();
        let mut snapshot = test_snapshot(
            chat_id,
            "New Chat",
            vec![ChatMessage::new(
                "user".to_string(),
                "hello from global".to_string(),
            )],
        );
        snapshot.mode = "task_planner".to_string();
        snapshot.parent_id = Some("source-chat".to_string());
        snapshot.link_type = Some("mode_transition".to_string());
        snapshot.previous_response_id = Some("resp_source".to_string());
        snapshot.claude_code_identity = Some(ClaudeCodeIdentity {
            device_id: "source-device".to_string(),
            session_id: "source-session".to_string(),
        });
        snapshot.is_title_generated = false;
        let project_dir_error = get_trajectories_dir(gcx.clone()).await.unwrap_err();
        assert_eq!(project_dir_error, "No workspace folder found");

        save_trajectory_snapshot(gcx.clone(), snapshot)
            .await
            .unwrap();
        wait_for_file_title(&path, "hello from global").await;

        let raw: serde_json::Value =
            serde_json::from_str(&tokio::fs::read_to_string(&path).await.unwrap()).unwrap();
        assert_eq!(raw["messages"].as_array().unwrap().len(), 1);
        assert_same_optional_path(
            find_trajectory_or_buddy_path(gcx.clone(), chat_id).await,
            &path,
        );
        let all_dirs = get_all_trajectories_dirs(gcx).await;
        assert_eq!(all_dirs, vec![global_trajectories_dir]);
    }

    #[tokio::test]
    async fn no_meta_snapshot_does_not_overwrite_buddy_conversation_collision() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let chat_id = "buddy-normal-collision";
        let buddy_path = dir
            .path()
            .join(".refact")
            .join("buddy")
            .join("chats")
            .join("conversations")
            .join(format!("{chat_id}.json"));
        write_buddy_conversation_file(&buddy_path, chat_id, "Keep Buddy Collision").await;

        save_trajectory_snapshot(
            gcx.clone(),
            test_snapshot(
                chat_id,
                "Normal Chat",
                vec![ChatMessage::new(
                    "user".to_string(),
                    "hello normal".to_string(),
                )],
            ),
        )
        .await
        .unwrap();

        let normal_path = nested_trajectory_path(dir.path(), chat_id, chat_id);
        assert!(tokio::fs::try_exists(&normal_path).await.unwrap());
        let buddy_raw: serde_json::Value =
            serde_json::from_str(&tokio::fs::read_to_string(&buddy_path).await.unwrap()).unwrap();
        assert_eq!(buddy_raw["title"], "Keep Buddy Collision");
        assert!(buddy_raw.get("buddy_meta").is_some());
        assert_same_optional_path(
            find_trajectory_path(gcx.clone(), chat_id).await,
            &normal_path,
        );
        assert_ne!(
            find_trajectory_or_buddy_path(gcx, chat_id)
                .await
                .map(|path| normalized_test_path(&path)),
            Some(normalized_test_path(&buddy_path))
        );
    }

    #[tokio::test]
    async fn no_meta_snapshot_does_not_overwrite_task_trajectory_collision() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let chat_id = "task-normal-collision";
        let task_path = dir
            .path()
            .join(".refact")
            .join("tasks")
            .join("task-collision")
            .join("trajectories")
            .join("agents")
            .join("agent-1")
            .join(format!("{chat_id}.json"));
        write_trajectory_file(
            &task_path,
            chat_id,
            "Keep Task Collision",
            "2024-01-01T00:00:00Z",
        )
        .await;
        let task_before = tokio::fs::read_to_string(&task_path).await.unwrap();

        save_trajectory_snapshot(
            gcx.clone(),
            test_snapshot(
                chat_id,
                "Normal Chat",
                vec![ChatMessage::new(
                    "user".to_string(),
                    "hello normal".to_string(),
                )],
            ),
        )
        .await
        .unwrap();

        let normal_path = nested_trajectory_path(dir.path(), chat_id, chat_id);
        assert!(tokio::fs::try_exists(&normal_path).await.unwrap());
        assert_eq!(
            tokio::fs::read_to_string(&task_path).await.unwrap(),
            task_before
        );
        let normal_raw: serde_json::Value =
            serde_json::from_str(&tokio::fs::read_to_string(&normal_path).await.unwrap()).unwrap();
        assert_eq!(normal_raw["title"], "Normal Chat");
        let task_raw: serde_json::Value =
            serde_json::from_str(&tokio::fs::read_to_string(&task_path).await.unwrap()).unwrap();
        assert_eq!(task_raw["title"], "Keep Task Collision");
        assert_same_optional_path(
            find_trajectory_path(gcx.clone(), chat_id).await,
            &normal_path,
        );
        assert_ne!(
            find_trajectory_path(gcx, chat_id)
                .await
                .map(|path| normalized_test_path(&path)),
            Some(normalized_test_path(&task_path))
        );
    }

    #[tokio::test]
    async fn mode_transition_new_empty_snapshot_without_existing_file_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let chat_id = "transition-new-empty-skip";

        save_trajectory_snapshot(gcx, test_snapshot(chat_id, "New Empty", Vec::new()))
            .await
            .unwrap();

        let path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        assert!(!tokio::fs::try_exists(path).await.unwrap());
    }

    #[tokio::test]
    async fn mode_transition_external_reload_persists_provider_cleanup() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;
        let chat_id = "transition-external-reload-provider";
        let trajectories_dir = dir.path().join(".refact").join("trajectories");
        tokio::fs::create_dir_all(&trajectories_dir).await.unwrap();
        let path = trajectories_dir.join(format!("{chat_id}.json"));
        tokio::fs::write(
            &path,
            serde_json::to_string(&json!({
                "id": chat_id,
                "title": "Transition External Reload",
                "model": "model",
                "mode": "task_planner",
                "tool_use": "agent",
                "parent_id": "source-chat",
                "link_type": "mode_transition",
                "previous_response_id": "resp_source",
                "messages": [],
                "claude_code_identity": {
                    "device_id":"source-device",
                    "session_id":"source-session"
                },
                "created_at": "2024-01-01T00:00:00Z",
                "updated_at": "2024-01-01T00:00:00Z",
                "include_project_info": true,
                "checkpoints_enabled": true,
                "browser_meta": {
                    "browser_runtime_id": "browser-immediate",
                    "tab_urls": ["https://example.com/immediate"],
                    "active_tab_id": "tab-immediate",
                    "attach_screenshot_on_send": false
                },
                "custom_future_field": {
                    "keep": true,
                    "nested": {"value": 44}
                }
            }))
            .unwrap(),
        )
        .await
        .unwrap();
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        app.chat
            .sessions
            .write()
            .await
            .insert(chat_id.to_string(), session_arc.clone());

        process_trajectory_change(gcx, chat_id, false).await;

        {
            let session = session_arc.lock().await;
            assert!(session.messages.is_empty());
            assert!(session.thread.frozen_request_prefix.is_none());
            assert!(session.thread.claude_code_identity.is_none());
            assert!(session.thread.previous_response_id.is_none());
            assert!(!session.trajectory_dirty);
        }
        let raw: serde_json::Value =
            serde_json::from_str(&tokio::fs::read_to_string(path).await.unwrap()).unwrap();
        assert_eq!(raw["messages"].as_array().unwrap().len(), 0);
        assert!(raw.get("frozen_request_prefix").is_none());
        assert!(raw.get("claude_code_identity").is_none());
        assert!(raw.get("previous_response_id").is_none());
        assert_eq!(
            raw["browser_meta"]["browser_runtime_id"],
            "browser-immediate"
        );
        assert_eq!(raw["custom_future_field"]["nested"]["value"], 44);
    }

    #[tokio::test]
    async fn mode_transition_pending_external_reload_persists_provider_cleanup_when_idle() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;
        let chat_id = "transition-pending-external-reload-provider";
        let trajectories_dir = dir.path().join(".refact").join("trajectories");
        tokio::fs::create_dir_all(&trajectories_dir).await.unwrap();
        let path = trajectories_dir.join(format!("{chat_id}.json"));
        tokio::fs::write(
            &path,
            serde_json::to_string(&json!({
                "id": chat_id,
                "title": "Pending Transition External Reload",
                "model": "model",
                "mode": "task_planner",
                "tool_use": "agent",
                "parent_id": "source-chat",
                "link_type": "mode_transition",
                "previous_response_id": "resp_source",
                "messages": [
                    {"role":"system","content":"target task planner system"},
                    {"role":"user","content":"hello"}
                ],
                "frozen_request_prefix": {
                    "schema_version": 1,
                    "created_at": "2026-05-29T00:00:00Z",
                    "system_prompt": "source system",
                    "tools_canonical": [{"type":"function","function":{"name":"source_tool"}}]
                },
                "claude_code_identity": {
                    "device_id":"source-device",
                    "session_id":"source-session"
                },
                "created_at": "2024-01-01T00:00:00Z",
                "updated_at": "2024-01-01T00:00:00Z",
                "include_project_info": true,
                "checkpoints_enabled": true,
                "browser_meta": {
                    "browser_runtime_id": "browser-pending",
                    "tab_urls": ["https://example.com/pending"],
                    "active_tab_id": "tab-pending",
                    "attach_screenshot_on_send": true
                },
                "custom_future_field": {
                    "keep": true,
                    "nested": {"value": 45}
                }
            }))
            .unwrap(),
        )
        .await
        .unwrap();
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        {
            let mut session = session_arc.lock().await;
            session.runtime.state = SessionState::Generating;
            session.trajectory_dirty = true;
        }
        app.chat
            .sessions
            .write()
            .await
            .insert(chat_id.to_string(), session_arc.clone());

        process_trajectory_change(gcx.clone(), chat_id, false).await;
        {
            let session = session_arc.lock().await;
            assert_eq!(
                session.external_reload_pending,
                Some(ExternalReloadPending::update(
                    TrajectorySourceIdentity::Normal
                ))
            );
            assert!(session.thread.claude_code_identity.is_none());
            assert!(session.thread.frozen_request_prefix.is_none());
        }

        {
            let mut session = session_arc.lock().await;
            session.runtime.state = SessionState::Idle;
            session.trajectory_dirty = false;
        }
        check_external_reload_pending(gcx, session_arc.clone()).await;

        {
            let session = session_arc.lock().await;
            assert_eq!(session.external_reload_pending, None);
            let prefix = session.thread.frozen_request_prefix.as_ref().unwrap();
            assert_eq!(
                prefix.system_prompt.as_deref(),
                Some("target task planner system")
            );
            assert!(prefix.tools_canonical.is_none());
            assert!(session.thread.claude_code_identity.is_none());
            assert!(session.thread.previous_response_id.is_none());
            assert!(!session.trajectory_dirty);
        }
        let raw: serde_json::Value =
            serde_json::from_str(&tokio::fs::read_to_string(path).await.unwrap()).unwrap();
        assert_eq!(
            raw["frozen_request_prefix"]["system_prompt"],
            "target task planner system"
        );
        assert!(raw["frozen_request_prefix"]["tools_canonical"].is_null());
        assert!(raw.get("claude_code_identity").is_none());
        assert!(raw.get("previous_response_id").is_none());
        assert_eq!(raw["browser_meta"]["browser_runtime_id"], "browser-pending");
        assert_eq!(raw["custom_future_field"]["nested"]["value"], 45);
    }

    #[tokio::test]
    async fn mode_transition_no_active_session_external_change_persists_provider_cleanup() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let chat_id = "transition-no-active-session-repair";
        let trajectories_dir = dir.path().join(".refact").join("trajectories");
        tokio::fs::create_dir_all(&trajectories_dir).await.unwrap();
        let path = trajectories_dir.join(format!("{chat_id}.json"));
        tokio::fs::write(
            &path,
            serde_json::to_string(&json!({
                "id": chat_id,
                "title": "No Active Session Repair",
                "model": "model",
                "mode": "task_planner",
                "tool_use": "agent",
                "parent_id": "source-chat",
                "link_type": "mode_transition",
                "previous_response_id": "resp_source",
                "messages": [
                    {"role":"system","content":"target task planner system"},
                    {"role":"user","content":"hello"}
                ],
                "frozen_request_prefix": {
                    "schema_version": 1,
                    "created_at": "2026-05-29T00:00:00Z",
                    "system_prompt": "source system",
                    "tools_canonical": [{"type":"function","function":{"name":"source_tool"}}]
                },
                "claude_code_identity": {
                    "device_id":"source-device",
                    "session_id":"source-session"
                },
                "created_at": "2024-01-01T00:00:00Z",
                "updated_at": "2024-01-01T00:00:00Z",
                "include_project_info": true,
                "checkpoints_enabled": true,
                "browser_meta": {
                    "browser_runtime_id": "browser-1",
                    "tab_urls": ["https://example.com"],
                    "active_tab_id": "tab-1",
                    "attach_screenshot_on_send": true
                },
                "custom_future_field": {
                    "keep": true,
                    "nested": {"value": 42}
                }
            }))
            .unwrap(),
        )
        .await
        .unwrap();

        process_trajectory_change(gcx, chat_id, false).await;

        let raw: serde_json::Value =
            serde_json::from_str(&tokio::fs::read_to_string(path).await.unwrap()).unwrap();
        assert_eq!(
            raw["frozen_request_prefix"]["system_prompt"],
            "target task planner system"
        );
        assert!(raw["frozen_request_prefix"]["tools_canonical"].is_null());
        assert!(raw.get("claude_code_identity").is_none());
        assert!(raw.get("previous_response_id").is_none());
        assert_eq!(raw["browser_meta"]["browser_runtime_id"], "browser-1");
        assert_eq!(
            raw["browser_meta"]["tab_urls"],
            json!(["https://example.com"])
        );
        assert_eq!(raw["custom_future_field"]["nested"]["value"], 42);
    }

    #[tokio::test]
    async fn repair_source_id_mismatch_errors_without_writing() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let chat_id = "repair-source-original";
        let path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        tokio::fs::create_dir_all(path.parent().unwrap())
            .await
            .unwrap();
        tokio::fs::write(
            &path,
            serde_json::to_string(&json!({
                "id": chat_id,
                "title": "Repair Source Original",
                "model": "model",
                "mode": "task_planner",
                "tool_use": "agent",
                "parent_id": "source-chat",
                "link_type": "mode_transition",
                "previous_response_id": "resp_source",
                "messages": [
                    {"role":"system","content":"target task planner system"},
                    {"role":"user","content":"hello"}
                ],
                "frozen_request_prefix": {
                    "schema_version": 1,
                    "created_at": "2026-05-29T00:00:00Z",
                    "system_prompt": "source system",
                    "tools_canonical": [{"type":"function","function":{"name":"source_tool"}}]
                },
                "claude_code_identity": {
                    "device_id":"source-device",
                    "session_id":"source-session"
                },
                "created_at": "2024-01-01T00:00:00Z",
                "updated_at": "2024-01-01T00:00:00Z",
                "include_project_info": true,
                "checkpoints_enabled": true,
                "custom_future_field": {"keep": true}
            }))
            .unwrap(),
        )
        .await
        .unwrap();

        let mut loaded = load_trajectory_for_chat(gcx.clone(), chat_id)
            .await
            .unwrap();
        assert!(loaded.transition_identity_repaired);
        apply_mode_defaults_to_thread(
            gcx.clone(),
            &mut loaded.thread,
            loaded.auto_approve_editing_tools_present,
            loaded.auto_approve_dangerous_commands_present,
        )
        .await;
        let repair_patch = loaded.repair_patch();
        let replacement = serde_json::to_string(&json!({
            "id": "repair-source-replacement",
            "title": "Replacement Must Stay",
            "model": "model",
            "mode": "agent",
            "tool_use": "agent",
            "messages": [],
            "created_at": "2024-01-01T00:00:00Z",
            "updated_at": "2024-01-01T00:00:00Z",
            "include_project_info": true,
            "checkpoints_enabled": true,
            "previous_response_id": "replacement-response"
        }))
        .unwrap();
        tokio::fs::write(&path, &replacement).await.unwrap();

        let err = persist_loaded_trajectory_repair_raw(gcx.clone(), &repair_patch)
            .await
            .unwrap_err();

        assert!(err.contains("Trajectory source id mismatch for repair"));
        assert_eq!(tokio::fs::read_to_string(path).await.unwrap(), replacement);
    }

    #[tokio::test]
    async fn repair_source_outside_approved_roots_errors_without_writing() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let chat_id = "repair-source-outside-root";
        let outside_path = outside.path().join(format!("{chat_id}.json"));
        write_trajectory_file(
            &outside_path,
            chat_id,
            "Outside Repair Source",
            "2024-01-01T00:00:00Z",
        )
        .await;
        let before = tokio::fs::read_to_string(&outside_path).await.unwrap();
        let repair_patch = TrajectoryRepairPatch {
            chat_id: chat_id.to_string(),
            source_path: outside_path.clone(),
            created_at: "2024-01-01T00:00:00Z".to_string(),
            frozen_request_prefix: None,
            auto_approve_editing_tools: true,
            auto_approve_dangerous_commands: true,
        };

        let err = persist_loaded_trajectory_repair_raw(gcx, &repair_patch)
            .await
            .unwrap_err();

        assert!(err.contains("not in an approved trajectory root"));
        assert_eq!(
            tokio::fs::read_to_string(outside_path).await.unwrap(),
            before
        );
    }

    #[tokio::test]
    async fn mode_transition_repair_persists_to_loaded_task_path_with_normal_collision() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let chat_id = "transition-task-normal-collision-repair";
        let task_path = dir
            .path()
            .join(".refact")
            .join("tasks")
            .join("task-loaded-repair")
            .join("trajectories")
            .join("agents")
            .join("agent-1")
            .join(format!("{chat_id}.json"));
        tokio::fs::create_dir_all(task_path.parent().unwrap())
            .await
            .unwrap();
        tokio::fs::write(
            &task_path,
            serde_json::to_string(&json!({
                "id": chat_id,
                "title": "Task Collision Repair",
                "model": "model",
                "mode": "task_planner",
                "tool_use": "agent",
                "parent_id": "source-chat",
                "link_type": "mode_transition",
                "previous_response_id": "task-response",
                "messages": [
                    {"role":"system","content":"target task planner system"},
                    {"role":"user","content":"hello"}
                ],
                "frozen_request_prefix": {
                    "schema_version": 1,
                    "created_at": "2026-05-29T00:00:00Z",
                    "system_prompt": "source system",
                    "tools_canonical": [{"type":"function","function":{"name":"source_tool"}}]
                },
                "claude_code_identity": {
                    "device_id":"source-device",
                    "session_id":"source-session"
                },
                "created_at": "2024-01-01T00:00:00Z",
                "updated_at": "2024-01-01T00:00:00Z",
                "include_project_info": true,
                "checkpoints_enabled": true,
                "task_meta": {
                    "task_id": "task-loaded-repair",
                    "role": "agents",
                    "agent_id": "agent-1",
                    "card_id": "T-1"
                },
                "custom_future_field": {"task": true}
            }))
            .unwrap(),
        )
        .await
        .unwrap();

        let mut loaded = load_trajectory_for_chat(gcx.clone(), chat_id)
            .await
            .unwrap();
        assert_same_path(&loaded.source_path, &task_path);
        assert!(loaded.transition_identity_repaired);
        apply_mode_defaults_to_thread(
            gcx.clone(),
            &mut loaded.thread,
            loaded.auto_approve_editing_tools_present,
            loaded.auto_approve_dangerous_commands_present,
        )
        .await;
        let repair_patch = loaded.repair_patch();

        let normal_path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        tokio::fs::create_dir_all(normal_path.parent().unwrap())
            .await
            .unwrap();
        tokio::fs::write(
            &normal_path,
            serde_json::to_string(&json!({
                "id": chat_id,
                "title": "Normal Collision Must Stay",
                "model": "model",
                "mode": "agent",
                "tool_use": "agent",
                "messages": [],
                "created_at": "2024-01-01T00:00:00Z",
                "updated_at": "2024-01-01T00:00:00Z",
                "include_project_info": true,
                "checkpoints_enabled": true,
                "previous_response_id": "normal-response",
                "custom_future_field": {"normal": true}
            }))
            .unwrap(),
        )
        .await
        .unwrap();
        let normal_before = tokio::fs::read_to_string(&normal_path).await.unwrap();

        persist_loaded_trajectory_repair_raw(gcx.clone(), &repair_patch)
            .await
            .unwrap();

        assert_eq!(
            tokio::fs::read_to_string(&normal_path).await.unwrap(),
            normal_before
        );
        let task_raw: serde_json::Value =
            serde_json::from_str(&tokio::fs::read_to_string(&task_path).await.unwrap()).unwrap();
        assert_eq!(
            task_raw["frozen_request_prefix"]["system_prompt"],
            "target task planner system"
        );
        assert!(task_raw["frozen_request_prefix"]["tools_canonical"].is_null());
        assert!(task_raw.get("claude_code_identity").is_none());
        assert!(task_raw.get("previous_response_id").is_none());
        assert_eq!(task_raw["custom_future_field"]["task"], true);
    }

    #[tokio::test]
    async fn handoff_invalid_or_missing_created_at_repairs_and_normalizes_on_open() {
        let dir = tempfile::tempdir().unwrap();
        let (_gcx, app) = make_app_with_workspace(dir.path()).await;
        let trajectories_dir = dir.path().join(".refact").join("trajectories");
        tokio::fs::create_dir_all(&trajectories_dir).await.unwrap();

        for (chat_id, created_at) in [
            ("handoff-invalid-created-at-open", Some(json!("not-a-date"))),
            ("handoff-missing-created-at-open", None),
        ] {
            let path = trajectories_dir.join(format!("{chat_id}.json"));
            let mut payload = json!({
                "id": chat_id,
                "title": "Handoff Open",
                "model": "model",
                "mode": "agent",
                "tool_use": "agent",
                "parent_id": "source-chat",
                "link_type": "handoff",
                "previous_response_id": "resp_source",
                "messages": [
                    {"role":"system","content":"target handoff system"},
                    {"role":"user","content":"hello"}
                ],
                "frozen_request_prefix": {
                    "schema_version": 1,
                    "created_at": "2024-01-03T00:00:00Z",
                    "system_prompt": "target handoff system",
                    "tools_canonical": [{"type":"function","function":{"name":"source_tool"}}]
                },
                "claude_code_identity": {
                    "device_id":"source-device",
                    "session_id":"source-session"
                },
                "updated_at": "2024-01-01T00:00:00Z",
                "include_project_info": true,
                "checkpoints_enabled": true
            });
            if let Some(created_at) = created_at {
                payload["created_at"] = created_at;
            }
            tokio::fs::write(&path, serde_json::to_string(&payload).unwrap())
                .await
                .unwrap();

            let session_arc = crate::chat::get_or_create_session_with_trajectory(
                app.clone(),
                &app.chat.sessions,
                chat_id,
            )
            .await;
            {
                let session = session_arc.lock().await;
                let prefix = session.thread.frozen_request_prefix.as_ref().unwrap();
                assert_eq!(
                    prefix.system_prompt.as_deref(),
                    Some("target handoff system")
                );
                assert!(prefix.tools_canonical.is_none());
                assert!(session.thread.claude_code_identity.is_none());
                assert!(session.thread.previous_response_id.is_none());
                assert!(parsed_rfc3339_utc(&session.created_at).is_some());
            }

            let raw: serde_json::Value =
                serde_json::from_str(&tokio::fs::read_to_string(&path).await.unwrap()).unwrap();
            let saved_created_at = raw["created_at"].as_str().unwrap();
            assert!(parsed_rfc3339_utc(saved_created_at).is_some());
            assert_ne!(saved_created_at, "not-a-date");
            assert_eq!(
                raw["frozen_request_prefix"]["system_prompt"],
                "target handoff system"
            );
            assert!(raw["frozen_request_prefix"]["tools_canonical"].is_null());
            assert!(raw.get("claude_code_identity").is_none());
            assert!(raw.get("previous_response_id").is_none());
        }
    }

    #[test]
    fn frozen_lazy_ensure_noops_on_empty_inputs_and_preserves_existing_prefix() {
        let mut session = ChatSession::new("lazy-frozen".to_string());
        assert!(ensure_frozen_prefix(&mut session, None, None).is_none());
        assert!(session.thread.frozen_request_prefix.is_none());
        assert_eq!(session.trajectory_version, 0);

        let no_tools = ensure_frozen_prefix(
            &mut session,
            Some("system no tools".to_string()),
            Some(json!([])),
        )
        .expect("known no-tools prefix should be installed");
        assert_eq!(no_tools.tools_canonical, Some(json!([])));
        assert!(frozen_prefix_is_complete(&no_tools));
        let version_after_no_tools = session.trajectory_version;

        let with_tools = ensure_frozen_prefix(
            &mut session,
            Some("system with tools".to_string()),
            Some(json!([{"type":"function","function":{"name":"cat"}}])),
        );
        assert!(with_tools.is_none());
        assert_eq!(session.trajectory_version, version_after_no_tools);
        assert_eq!(
            session
                .thread
                .frozen_request_prefix
                .as_ref()
                .and_then(|prefix| prefix.tools_canonical.clone()),
            Some(json!([]))
        );

        let mut session = ChatSession::new("lazy-frozen".to_string());

        let first = ensure_frozen_prefix(
            &mut session,
            Some("system one".to_string()),
            Some(json!([{"type":"function","function":{"name":"cat"}}])),
        )
        .expect("missing prefix should be installed");

        assert_eq!(first.schema_version, 1);
        assert_eq!(first.system_prompt.as_deref(), Some("system one"));
        assert!(session.thread.frozen_request_prefix.is_some());
        let version_after_first = session.trajectory_version;

        let second = ensure_frozen_prefix(
            &mut session,
            Some("system two".to_string()),
            Some(json!([{"type":"function","function":{"name":"shell"}}])),
        );
        assert!(second.is_none());
        assert_eq!(session.trajectory_version, version_after_first);
        let prefix = session.thread.frozen_request_prefix.as_ref().unwrap();
        assert_eq!(prefix.system_prompt.as_deref(), Some("system one"));
        assert_eq!(
            prefix.tools_canonical,
            Some(json!([{"type":"function","function":{"name":"cat"}}]))
        );
    }

    #[test]
    fn frozen_lazy_ensure_fills_partial_prefix_once() {
        let mut session = ChatSession::new("partial-frozen".to_string());
        let partial = ensure_frozen_prefix(&mut session, Some("system only".to_string()), None)
            .expect("partial prefix should be installed");
        assert_eq!(partial.system_prompt.as_deref(), Some("system only"));
        assert!(partial.tools_canonical.is_none());
        let version_after_partial = session.trajectory_version;

        let filled = ensure_frozen_prefix(
            &mut session,
            Some("replacement system".to_string()),
            Some(json!([{"type":"function","function":{"name":"cat"}}])),
        )
        .expect("partial prefix should be filled");
        assert_eq!(filled.system_prompt.as_deref(), Some("system only"));
        assert_eq!(
            filled.tools_canonical,
            Some(json!([{"type":"function","function":{"name":"cat"}}]))
        );
        assert_eq!(session.trajectory_version, version_after_partial + 1);

        let version_after_fill = session.trajectory_version;
        assert!(ensure_frozen_prefix(
            &mut session,
            Some("new system".to_string()),
            Some(json!([{"type":"function","function":{"name":"shell"}}])),
        )
        .is_none());
        assert_eq!(session.trajectory_version, version_after_fill);
        let prefix = session.thread.frozen_request_prefix.as_ref().unwrap();
        assert_eq!(prefix.system_prompt.as_deref(), Some("system only"));
        assert_eq!(
            prefix.tools_canonical,
            Some(json!([{"type":"function","function":{"name":"cat"}}]))
        );
    }

    #[test]
    fn frozen_complete_existing_prefix_is_never_overwritten() {
        let mut session = ChatSession::new("complete-frozen".to_string());
        let original = FrozenRequestPrefix {
            schema_version: 1,
            created_at: "2026-05-29T00:00:00Z".to_string(),
            system_prompt: Some("original system".to_string()),
            tools_canonical: Some(json!([])),
        };
        session.thread.frozen_request_prefix = Some(original.clone());

        let result = ensure_frozen_prefix(
            &mut session,
            Some("replacement system".to_string()),
            Some(json!([{"type":"function","function":{"name":"cat"}}])),
        );

        assert!(result.is_none());
        assert_eq!(session.thread.frozen_request_prefix, Some(original));
        assert_eq!(session.trajectory_version, 0);
    }

    #[test]
    fn frozen_racy_persist_prefix_helper_is_not_available_in_production() {
        let _helper = persist_frozen_prefix;
    }

    #[tokio::test]
    async fn frozen_legacy_normal_session_migrates_through_session_save() {
        let dir = tempfile::tempdir().unwrap();
        let (_gcx, app) = make_app_with_workspace(dir.path()).await;
        let chat_id = "legacy-freeze-normal";
        let trajectories_dir = dir.path().join(".refact").join("trajectories");
        tokio::fs::create_dir_all(&trajectories_dir).await.unwrap();
        tokio::fs::write(
            trajectories_dir.join(format!("{chat_id}.json")),
            serde_json::to_string(&json!({
                "id": chat_id,
                "title": "Legacy",
                "model": "test/model",
                "mode": "agent",
                "tool_use": "agent",
                "messages": [
                    {"role":"system","content":"legacy system"},
                    {"role":"user","content":"hello"}
                ],
                "created_at": "2024-01-01T00:00:00Z",
                "updated_at": "2024-01-01T00:00:00Z",
                "include_project_info": true,
                "checkpoints_enabled": true
            }))
            .unwrap(),
        )
        .await
        .unwrap();
        let session_arc = crate::chat::get_or_create_session_with_trajectory(
            app.clone(),
            &app.chat.sessions,
            chat_id,
        )
        .await;
        {
            let session = session_arc.lock().await;
            let prefix = session.thread.frozen_request_prefix.as_ref().unwrap();
            assert_eq!(prefix.system_prompt.as_deref(), Some("legacy system"));
            assert!(prefix.tools_canonical.is_none());
        }

        let raw: serde_json::Value = serde_json::from_str(
            &tokio::fs::read_to_string(trajectories_dir.join(format!("{chat_id}.json")))
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            raw["frozen_request_prefix"]["system_prompt"],
            "legacy system"
        );
        assert!(raw["frozen_request_prefix"]["tools_canonical"].is_null());
        assert_eq!(raw["messages"][0]["content"], "legacy system");
    }

    #[tokio::test]
    async fn frozen_legacy_unknown_tools_can_be_completed_once() {
        let dir = tempfile::tempdir().unwrap();
        let (_gcx, app) = make_app_with_workspace(dir.path()).await;
        let chat_id = "legacy-freeze-complete-once";
        let trajectories_dir = dir.path().join(".refact").join("trajectories");
        tokio::fs::create_dir_all(&trajectories_dir).await.unwrap();
        tokio::fs::write(
            trajectories_dir.join(format!("{chat_id}.json")),
            serde_json::to_string(&json!({
                "id": chat_id,
                "title": "Legacy",
                "model": "test/model",
                "mode": "agent",
                "tool_use": "agent",
                "messages": [
                    {"role":"system","content":"legacy system"},
                    {"role":"user","content":"hello"}
                ],
                "created_at": "2024-01-01T00:00:00Z",
                "updated_at": "2024-01-01T00:00:00Z",
                "include_project_info": true,
                "checkpoints_enabled": true
            }))
            .unwrap(),
        )
        .await
        .unwrap();
        let session_arc = crate::chat::get_or_create_session_with_trajectory(
            app.clone(),
            &app.chat.sessions,
            chat_id,
        )
        .await;

        let mut session = session_arc.lock().await;
        let completed = ensure_frozen_prefix(
            &mut session,
            Some("replacement system".to_string()),
            Some(json!([{"type":"function","function":{"name":"cat"}}])),
        )
        .expect("legacy unknown tools prefix should complete once");
        assert_eq!(completed.system_prompt.as_deref(), Some("legacy system"));
        assert_eq!(
            completed.tools_canonical,
            Some(json!([{"type":"function","function":{"name":"cat"}}]))
        );
        let version_after_complete = session.trajectory_version;

        assert!(ensure_frozen_prefix(
            &mut session,
            Some("new system".to_string()),
            Some(json!([{"type":"function","function":{"name":"shell"}}])),
        )
        .is_none());
        assert_eq!(session.trajectory_version, version_after_complete);
        assert_eq!(
            session
                .thread
                .frozen_request_prefix
                .as_ref()
                .and_then(|prefix| prefix.tools_canonical.clone()),
            Some(json!([{"type":"function","function":{"name":"cat"}}]))
        );
    }

    #[tokio::test]
    async fn frozen_legacy_task_session_migration_does_not_create_generic_copy() {
        let dir = tempfile::tempdir().unwrap();
        let (_gcx, app) = make_app_with_workspace(dir.path()).await;
        let task_id = "task-legacy-freeze";
        let agent_id = "agent-1";
        let chat_id = "legacy-task-freeze";
        let task_path = dir
            .path()
            .join(".refact")
            .join("tasks")
            .join(task_id)
            .join("trajectories")
            .join("agents")
            .join(agent_id)
            .join(format!("{chat_id}.json"));
        tokio::fs::create_dir_all(task_path.parent().unwrap())
            .await
            .unwrap();
        tokio::fs::write(
            &task_path,
            serde_json::to_string(&json!({
                "id": chat_id,
                "title": "Legacy Task",
                "model": "model",
                "mode": "task_agent",
                "tool_use": "agent",
                "messages": [
                    {"role":"system","content":"task system"},
                    {"role":"user","content":"hello task"}
                ],
                "created_at": "2024-01-01T00:00:00Z",
                "updated_at": "2024-01-01T00:00:00Z",
                "include_project_info": true,
                "checkpoints_enabled": true,
                "task_meta": {
                    "task_id": task_id,
                    "role": "agents",
                    "agent_id": agent_id,
                    "card_id": "T-1"
                }
            }))
            .unwrap(),
        )
        .await
        .unwrap();
        tokio::fs::create_dir_all(dir.path().join(".refact").join("tasks").join(task_id))
            .await
            .unwrap();
        let session_arc = crate::chat::get_or_create_session_with_trajectory(
            app.clone(),
            &app.chat.sessions,
            chat_id,
        )
        .await;
        {
            let session = session_arc.lock().await;
            let prefix = session.thread.frozen_request_prefix.as_ref().unwrap();
            assert_eq!(prefix.system_prompt.as_deref(), Some("task system"));
            assert!(prefix.tools_canonical.is_none());
        }

        let generic_path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        let raw: serde_json::Value =
            serde_json::from_str(&tokio::fs::read_to_string(&task_path).await.unwrap()).unwrap();
        assert!(!tokio::fs::try_exists(generic_path).await.unwrap());
        assert_eq!(raw["frozen_request_prefix"]["system_prompt"], "task system");
        assert!(raw["frozen_request_prefix"]["tools_canonical"].is_null());
    }

    #[tokio::test]
    async fn frozen_empty_normal_snapshot_with_prefix_is_saved() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let mut snapshot = test_snapshot("empty-frozen-normal", "Frozen Empty", vec![]);
        snapshot.frozen_request_prefix = Some(new_frozen_request_prefix(
            Some("empty snapshot system".to_string()),
            json!([]),
        ));

        save_trajectory_snapshot(gcx, snapshot).await.unwrap();

        let path = nested_trajectory_path(dir.path(), "empty-frozen-normal", "empty-frozen-normal");
        let raw: serde_json::Value =
            serde_json::from_str(&tokio::fs::read_to_string(path).await.unwrap()).unwrap();
        assert_eq!(
            raw["frozen_request_prefix"]["system_prompt"],
            "empty snapshot system"
        );
        assert_eq!(raw["messages"].as_array().unwrap().len(), 0);
    }
    #[tokio::test]
    async fn trajectory_updated_at_changes_when_title_changes_without_messages() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;

        let chat_id = "updated-at-title-change";
        let messages = vec![ChatMessage::new("user".to_string(), "Hello".to_string())];
        save_trajectory_snapshot(
            gcx.clone(),
            test_snapshot(chat_id, "First", messages.clone()),
        )
        .await
        .unwrap();
        let path = nested_trajectory_path(dir.path(), chat_id, chat_id);
        let first_raw: serde_json::Value =
            serde_json::from_str(&tokio::fs::read_to_string(&path).await.unwrap()).unwrap();
        let first_updated_at = first_raw["updated_at"].as_str().unwrap().to_string();

        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        save_trajectory_snapshot(gcx, test_snapshot(chat_id, "Retitled", messages))
            .await
            .unwrap();
        let retitled_raw: serde_json::Value =
            serde_json::from_str(&tokio::fs::read_to_string(&path).await.unwrap()).unwrap();
        assert_ne!(
            retitled_raw["updated_at"].as_str().unwrap(),
            first_updated_at
        );
    }

    #[tokio::test]
    async fn trajectory_updated_at_changes_when_worktree_changes() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;

        let chat_id = "updated-at-worktree-change";
        let messages = vec![ChatMessage::new("user".to_string(), "Hello".to_string())];
        save_trajectory_snapshot(
            gcx.clone(),
            test_snapshot(chat_id, "Title", messages.clone()),
        )
        .await
        .unwrap();
        let path = nested_trajectory_path(dir.path(), chat_id, chat_id);
        let first_raw: serde_json::Value =
            serde_json::from_str(&tokio::fs::read_to_string(&path).await.unwrap()).unwrap();
        let first_updated_at = first_raw["updated_at"].as_str().unwrap().to_string();

        let mut snapshot = test_snapshot(chat_id, "Title", messages);
        snapshot.worktree = Some(trajectory_worktree_sample());
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        save_trajectory_snapshot(gcx, snapshot).await.unwrap();
        let changed_raw: serde_json::Value =
            serde_json::from_str(&tokio::fs::read_to_string(&path).await.unwrap()).unwrap();
        assert_ne!(
            changed_raw["updated_at"].as_str().unwrap(),
            first_updated_at
        );
    }

    #[tokio::test]
    async fn trajectory_persistence_preserves_ui_only_messages() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let mut session = ChatSession::new("ui-only-roundtrip".to_string());
        session.created_at = "2024-01-01T00:00:00Z".to_string();
        session.add_message(ChatMessage::new("user".to_string(), "hello".to_string()));
        session.add_message(make_ui_only_error_message("context_length_exceeded"));

        let snapshot = trajectory_snapshot_from_session(&session);
        save_trajectory_snapshot(gcx.clone(), snapshot)
            .await
            .unwrap();

        let loaded = load_trajectory_for_chat(gcx, "ui-only-roundtrip")
            .await
            .unwrap();
        assert_eq!(loaded.messages.len(), 2);
        assert_eq!(loaded.messages[0].role, "user");
        assert_eq!(loaded.messages[1].role, "error");
        assert!(is_ui_only_message(&loaded.messages[1]));
    }

    #[tokio::test]
    async fn cache_guard_snapshot_is_runtime_only_and_not_persisted() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let raw_request_marker = "raw-cache-guard-request-body-marker";
        let mut session = ChatSession::new("cache-guard-runtime-only".to_string());
        session.created_at = "2024-01-01T00:00:00Z".to_string();
        session.cache_guard_snapshot = Some(json!({
            "messages": [{"role": "user", "content": raw_request_marker}],
            "provider_specific_fields": {"semantic": "value"}
        }));
        session.add_message(ChatMessage::new("user".to_string(), "visible".to_string()));

        let snapshot = trajectory_snapshot_from_session(&session);
        let snapshot_messages_json = serde_json::to_string(&snapshot.messages).unwrap();
        assert!(!snapshot_messages_json.contains(raw_request_marker));

        save_trajectory_snapshot(gcx.clone(), snapshot)
            .await
            .unwrap();

        let path = nested_trajectory_path(
            dir.path(),
            "cache-guard-runtime-only",
            "cache-guard-runtime-only",
        );
        let saved_json = tokio::fs::read_to_string(path).await.unwrap();
        assert!(!saved_json.contains("cache_guard_snapshot"));
        assert!(!saved_json.contains(raw_request_marker));
    }

    #[tokio::test]
    async fn reactive_compact_attempts_is_runtime_only_not_persisted() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let mut snapshot = test_snapshot(
            "reactive-attempts-roundtrip",
            "Reactive Attempts",
            vec![ChatMessage::new("user".to_string(), "visible".to_string())],
        );
        snapshot.reactive_compact_attempts = Some(2);
        save_trajectory_snapshot(gcx.clone(), snapshot)
            .await
            .unwrap();

        let traj_path = nested_trajectory_path(
            dir.path(),
            "reactive-attempts-roundtrip",
            "reactive-attempts-roundtrip",
        );
        let saved = tokio::fs::read_to_string(&traj_path).await.unwrap();
        assert!(!saved.contains("reactive_compact_attempts"));

        let mut raw: serde_json::Value = serde_json::from_str(&saved).unwrap();
        raw["reactive_compact_attempts"] = json!(99);
        tokio::fs::write(&traj_path, serde_json::to_string(&raw).unwrap())
            .await
            .unwrap();

        let loaded = load_trajectory_for_chat(gcx, "reactive-attempts-roundtrip")
            .await
            .unwrap();
        assert_eq!(loaded.thread.reactive_compact_attempts, None);
    }

    #[tokio::test]
    async fn pending_deliveries_save_load_preserves_identity_and_clears_delivered() {
        use refact_core::chat_types::{PendingDelivery, PushMode};
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let mut session = ChatSession::new("pending-roundtrip".into());
        session.created_at = "2024-01-01T00:00:00Z".into();
        let mut delivery = PendingDelivery::with_id(
            "stable-id",
            vec![ChatMessage::new("user".into(), "queued".into())],
            PushMode::WhenIdle,
            "test",
            true,
        );
        delivery.enqueued_at_ms = 42;
        session.pending_deliveries.push_back(delivery.clone());
        assert!(!session_is_stale_refresh_candidate(&session, true));
        save_trajectory_snapshot(gcx.clone(), trajectory_snapshot_from_session(&session))
            .await
            .unwrap();
        let loaded = load_trajectory_for_chat(gcx.clone(), "pending-roundtrip")
            .await
            .unwrap();
        assert_eq!(loaded.pending_deliveries, vec![delivery.clone()]);
        let mut restored = ChatSession::new("pending-roundtrip".into());
        restored.restore_pending_deliveries(loaded.pending_deliveries);
        assert_eq!(
            restored.pending_deliveries_for_snapshot(),
            vec![delivery.clone()]
        );
        restored.pending_deliveries.clear();
        restored.messages = delivery.stamp_messages();
        restored.restore_pending_deliveries(vec![delivery]);
        assert!(restored.pending_deliveries.is_empty());
        save_trajectory_snapshot(gcx.clone(), trajectory_snapshot_from_session(&restored))
            .await
            .unwrap();
        assert!(load_trajectory_for_chat(gcx, "pending-roundtrip")
            .await
            .unwrap()
            .pending_deliveries
            .is_empty());
    }

    #[tokio::test]
    async fn trajectory_persistence_keeps_normal_error_messages() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let mut session = ChatSession::new("normal-error-roundtrip".to_string());
        session.created_at = "2024-01-01T00:00:00Z".to_string();
        session.add_message(ChatMessage::new("user".to_string(), "hello".to_string()));
        session.add_message(ChatMessage::new(
            "error".to_string(),
            "LLM failed".to_string(),
        ));

        let snapshot = trajectory_snapshot_from_session(&session);
        save_trajectory_snapshot(gcx.clone(), snapshot)
            .await
            .unwrap();

        let loaded = load_trajectory_for_chat(gcx, "normal-error-roundtrip")
            .await
            .unwrap();
        assert_eq!(loaded.messages.len(), 2);
        assert_eq!(loaded.messages[1].role, "error");
        assert_eq!(loaded.messages[1].content.content_text_only(), "LLM failed");
    }

    #[test]
    fn trajectory_save_uses_unique_tmp() {
        let file_path = PathBuf::from("chat.json");
        let first = unique_trajectory_tmp_path(&file_path);
        let second = unique_trajectory_tmp_path(&file_path);

        assert_ne!(first, second);
        assert_eq!(
            first
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap()
                .len(),
            22
        );
        assert!(first
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap()
            .starts_with("chat.json.tmp."));
        assert!(second
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap()
            .starts_with("chat.json.tmp."));
    }

    #[tokio::test]
    async fn trajectory_save_cleans_up_on_error() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("chat.json");
        let tmp_path = unique_trajectory_tmp_path(&file_path);
        tokio::fs::write(&tmp_path, "stale").await.unwrap();

        let err = atomic_write_json_with_tmp_path(
            &file_path,
            &tmp_path,
            Err("Failed to serialize trajectory: injected".to_string()),
            Some("Failed to write trajectory"),
            None,
        )
        .await
        .unwrap_err();

        assert_eq!(err, "Failed to serialize trajectory: injected");
        assert!(!tmp_path.exists());
        let leftovers = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|entry| entry.ok())
            .filter(|entry| {
                entry
                    .file_name()
                    .to_str()
                    .map(|name| name.contains(".tmp"))
                    .unwrap_or(false)
            })
            .count();
        assert_eq!(leftovers, 0);
    }

    #[serial(trajectory_perf)]
    #[tokio::test]
    async fn trajectory_watcher_self_write_consumes_exact_fingerprint_once() {
        clear_all_trajectory_self_writes();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("self-write.json");
        let content = "{\"id\":\"self-write\"}";
        let handle =
            register_trajectory_self_write(&path, TrajectorySourceIdentity::Normal, content)
                .await
                .unwrap();
        fs::write(&path, content).await.unwrap();
        complete_trajectory_self_write(Some(handle)).await;

        assert!(consume_trajectory_self_write(&path, &TrajectorySourceIdentity::Normal).await);
        assert!(!consume_trajectory_self_write(&path, &TrajectorySourceIdentity::Normal).await);
        clear_all_trajectory_self_writes();
    }

    #[serial(trajectory_perf)]
    #[tokio::test]
    async fn trajectory_watcher_self_write_does_not_consume_modified_or_deleted_files() {
        clear_all_trajectory_self_writes();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("external-write.json");
        let handle = register_trajectory_self_write(
            &path,
            TrajectorySourceIdentity::Normal,
            "{\"id\":\"external-write\"}",
        )
        .await
        .unwrap();
        fs::write(&path, "{\"id\":\"external-write\"}")
            .await
            .unwrap();
        complete_trajectory_self_write(Some(handle)).await;
        fs::write(&path, "{\"id\":\"external-write\",\"title\":\"external\"}")
            .await
            .unwrap();

        assert!(!consume_trajectory_self_write(&path, &TrajectorySourceIdentity::Normal).await);
        fs::remove_file(&path).await.unwrap();
        assert!(!consume_trajectory_self_write(&path, &TrajectorySourceIdentity::Normal).await);
        clear_all_trajectory_self_writes();
    }

    #[serial(trajectory_perf)]
    #[tokio::test]
    async fn trajectory_watcher_self_write_respects_source_and_expires_records() {
        clear_all_trajectory_self_writes();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("same-id.json");
        let normal = TrajectorySourceIdentity::Normal;
        let task = TrajectorySourceIdentity::task(
            "task-source".to_string(),
            "agents".to_string(),
            Some("agent-source".to_string()),
            None,
            None,
        );
        let content = "{\"id\":\"same-id\"}";
        let handle = register_trajectory_self_write(&path, normal.clone(), content)
            .await
            .unwrap();
        fs::write(&path, content).await.unwrap();
        complete_trajectory_self_write(Some(handle)).await;

        assert!(!consume_trajectory_self_write(&path, &task).await);
        {
            let mut registry = trajectory_self_writes()
                .lock()
                .expect("trajectory self-write registry poisoned");
            for records in registry.values_mut() {
                for record in records {
                    record.created_at = Instant::now() - TRAJECTORY_SELF_WRITE_TTL;
                }
            }
        }
        assert!(!consume_trajectory_self_write(&path, &normal).await);
        clear_all_trajectory_self_writes();
    }

    #[serial(trajectory_perf)]
    #[tokio::test]
    async fn trajectory_watcher_self_write_consumes_remove_after_atomic_replacement() {
        clear_all_trajectory_self_writes();
        let dir = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;
        let chat_id = "self-write-remove";
        let path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        write_trajectory_file_with_user_message(&path, chat_id, "Before Self Write", "before")
            .await;
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        {
            let mut session = session_arc.lock().await;
            session.thread.title = "Before Self Write".to_string();
            session
                .messages
                .push(ChatMessage::new("user".to_string(), "before".to_string()));
            session.trajectory_dirty = false;
        }
        app.chat
            .sessions
            .write()
            .await
            .insert(chat_id.to_string(), session_arc.clone());

        let content = tokio::fs::read_to_string(&path).await.unwrap();
        let tmp_path = unique_trajectory_tmp_path(&path);
        atomic_write_json_with_tmp_path(
            &path,
            &tmp_path,
            Ok(content),
            Some("Failed to write trajectory"),
            Some(TrajectorySourceIdentity::Normal),
        )
        .await
        .unwrap();
        process_trajectory_watcher_change(
            gcx,
            chat_id.to_string(),
            true,
            TrajectorySourceIdentity::Normal,
            path,
        )
        .await;

        let session = session_arc.lock().await;
        assert_eq!(session.thread.title, "Before Self Write");
        assert_eq!(session.messages.len(), 1);
        assert_eq!(session.external_reload_pending, None);
        drop(session);
        clear_all_trajectory_self_writes();
    }

    #[serial(trajectory_perf)]
    #[tokio::test]
    async fn trajectory_watcher_self_write_disable_restores_external_reload_path() {
        crate::runtime_settings::reset_for_test();
        clear_all_trajectory_self_writes();
        let dir = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;
        let chat_id = "self-write-disabled";
        let path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        write_trajectory_file_with_user_message(&path, chat_id, "Before Self Write", "before")
            .await;
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        {
            let mut session = session_arc.lock().await;
            session.thread.title = "Before Self Write".to_string();
            session
                .messages
                .push(ChatMessage::new("user".to_string(), "before".to_string()));
            session.trajectory_dirty = false;
        }
        app.chat
            .sessions
            .write()
            .await
            .insert(chat_id.to_string(), session_arc.clone());
        let mut trajectory_events = app.chat.trajectory_events_tx.subscribe();

        let mut disabled = crate::runtime_settings::TrajectoryRuntimeSettings::default();
        disabled.trajectory_watcher_self_write_enabled = false;
        crate::runtime_settings::install_live(&disabled);
        assert!(!trajectory_watcher_self_write_rollout_enabled());

        let content = tokio::fs::read_to_string(&path).await.unwrap();
        let tmp_path = unique_trajectory_tmp_path(&path);
        atomic_write_json_with_tmp_path(
            &path,
            &tmp_path,
            Ok(content),
            Some("Failed to write trajectory"),
            Some(TrajectorySourceIdentity::Normal),
        )
        .await
        .unwrap();
        process_trajectory_watcher_change(
            gcx,
            chat_id.to_string(),
            true,
            TrajectorySourceIdentity::Normal,
            path,
        )
        .await;

        assert_eq!(trajectory_events.try_recv().unwrap().id, chat_id);
        crate::runtime_settings::reset_for_test();
        clear_all_trajectory_self_writes();
    }

    #[serial(trajectory_perf)]
    #[tokio::test]
    async fn trajectory_watcher_self_write_cleanup_keeps_other_watcher_scope() {
        clear_all_trajectory_self_writes();
        let dir = tempfile::tempdir().unwrap();
        let first_root = dir.path().join("first");
        let second_root = dir.path().join("second");
        tokio::fs::create_dir_all(&first_root).await.unwrap();
        tokio::fs::create_dir_all(&second_root).await.unwrap();
        let first_path = first_root.join("first.json");
        let second_path = second_root.join("second.json");
        let first_handle = register_trajectory_self_write(
            &first_path,
            TrajectorySourceIdentity::Normal,
            "{\"id\":\"first\"}",
        )
        .await
        .unwrap();
        let second_handle = register_trajectory_self_write(
            &second_path,
            TrajectorySourceIdentity::Normal,
            "{\"id\":\"second\"}",
        )
        .await
        .unwrap();
        tokio::fs::write(&first_path, "{\"id\":\"first\"}")
            .await
            .unwrap();
        tokio::fs::write(&second_path, "{\"id\":\"second\"}")
            .await
            .unwrap();
        complete_trajectory_self_write(Some(first_handle)).await;
        complete_trajectory_self_write(Some(second_handle)).await;

        clear_trajectory_self_writes(&[dunce::simplified(
            &tokio::fs::canonicalize(&first_root).await.unwrap(),
        )
        .to_path_buf()]);

        assert!(
            !consume_trajectory_self_write(&first_path, &TrajectorySourceIdentity::Normal).await
        );
        assert!(
            consume_trajectory_self_write(&second_path, &TrajectorySourceIdentity::Normal).await
        );
        clear_all_trajectory_self_writes();
    }

    #[test]
    fn trajectory_watcher_ignores_atomic_temporary_files() {
        let dir = tempfile::tempdir().unwrap();
        let task_root = dir.path().join(".refact").join("tasks");
        std::fs::create_dir_all(&task_root).unwrap();
        let temporary = task_root
            .join("task-1")
            .join("trajectories")
            .join("agents")
            .join("agent-1")
            .join("chat.json.tmp.abc123");

        assert!(is_trajectory_temporary_file(&temporary));
        assert!(trajectory_watcher_message_for_path(temporary, false, &[task_root]).is_none());
    }

    #[tokio::test]
    async fn orphaned_temp_file_does_not_replace_last_atomic_commit() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("chat.json");
        let committed = r#"{"id":"chat","messages":[]}"#;
        tokio::fs::write(&file_path, committed).await.unwrap();
        let tmp_path = unique_trajectory_tmp_path(&file_path);
        tokio::fs::write(&tmp_path, r#"{"id":"chat","messages":["partial"]"#)
            .await
            .unwrap();

        let restored = tokio::fs::read_to_string(&file_path).await.unwrap();
        assert_eq!(restored, committed);
        assert!(tmp_path.exists());
        tokio::fs::remove_file(tmp_path).await.unwrap();
    }

    #[tokio::test]
    async fn trajectory_worktree_old_json_without_worktree_loads_none() {
        let dir = tempfile::tempdir().unwrap();
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let app = AppState::from_gcx(gcx.clone()).await;
        {
            *app.workspace
                .documents_state
                .workspace_folders
                .lock()
                .unwrap() = vec![dir.path().to_path_buf()];
        }
        let traj_dir = dir.path().join(".refact").join("trajectories");
        tokio::fs::create_dir_all(&traj_dir).await.unwrap();
        tokio::fs::write(
            traj_dir.join("old-chat.json"),
            r#"{"id":"old-chat","title":"Old","model":"m","mode":"agent","tool_use":"agent","messages":[],"created_at":"2024-01-01T00:00:00Z","updated_at":"2024-01-01T00:00:00Z","include_project_info":true,"checkpoints_enabled":true}"#,
        )
        .await
        .unwrap();

        let loaded = load_trajectory_for_chat(gcx, "old-chat").await.unwrap();
        assert!(loaded.thread.worktree.is_none());
    }

    #[tokio::test]
    async fn trajectory_worktree_unregistered_top_level_metadata_is_stripped() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("repo");
        let cache = dir.path().join("cache");
        std::fs::create_dir_all(&source).unwrap();
        init_repo(&source);
        let gcx = crate::global_context::tests::make_test_gcx_with_dirs(
            cache.clone(),
            std::env::temp_dir().join(format!("refact-cfg-{}", uuid::Uuid::new_v4())),
        )
        .await;
        {
            *gcx.documents_state.workspace_folders.lock().unwrap() = vec![source.clone()];
        }
        let app = AppState::from_gcx(gcx.clone()).await;
        let traj_dir = source.join(".refact").join("trajectories");
        tokio::fs::create_dir_all(&traj_dir).await.unwrap();
        let untrusted = json!({
            "id": "untrusted-wt-chat",
            "title": "Untrusted",
            "model": "m",
            "mode": "agent",
            "tool_use": "agent",
            "messages": [],
            "created_at": "2024-01-01T00:00:00Z",
            "updated_at": "2024-01-01T00:00:00Z",
            "include_project_info": true,
            "checkpoints_enabled": true,
            "worktree": {
                "id": "wt-evil",
                "kind": "chat",
                "root": dir.path().join("evil").to_string_lossy().to_string(),
                "source_workspace_root": source.to_string_lossy().to_string(),
                "repo_root": source.to_string_lossy().to_string(),
                "enforce": true
            }
        });
        tokio::fs::write(
            traj_dir.join("untrusted-wt-chat.json"),
            serde_json::to_string(&untrusted).unwrap(),
        )
        .await
        .unwrap();

        let loaded = load_trajectory_for_chat(gcx.clone(), "untrusted-wt-chat")
            .await
            .unwrap();
        assert!(loaded.thread.worktree.is_none());
        let listed = list_all_trajectories_meta(app, false).await.unwrap();
        let listed_worktree = listed
            .iter()
            .find(|item| item.id == "untrusted-wt-chat")
            .and_then(|item| item.worktree.clone());
        assert!(listed_worktree.is_none());
    }

    #[tokio::test]
    async fn trajectory_worktree_legacy_task_agent_hydrates_from_board_mirror() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("repo");
        let cache = dir.path().join("cache");
        std::fs::create_dir_all(&source).unwrap();
        init_repo(&source);
        let gcx = crate::global_context::tests::make_test_gcx_with_dirs(
            cache.clone(),
            std::env::temp_dir().join(format!("refact-cfg-{}", uuid::Uuid::new_v4())),
        )
        .await;
        {
            *gcx.documents_state.workspace_folders.lock().unwrap() = vec![source.clone()];
        }
        let _app = AppState::from_gcx(gcx.clone()).await;

        let task_id = "task-legacy";
        let agent_id = "agent-1";
        let card_id = "card-1";
        let chat_id = "legacy-agent-chat";
        let task_dir = source.join(".refact").join("tasks").join(task_id);
        tokio::fs::create_dir_all(task_dir.join("trajectories").join("agents").join(agent_id))
            .await
            .unwrap();
        let meta = crate::tasks::types::TaskMeta {
            schema_version: 1,
            id: task_id.to_string(),
            name: "Task".to_string(),
            status: crate::tasks::types::TaskStatus::Active,
            created_at: "2024-01-01T00:00:00Z".to_string(),
            updated_at: "2024-01-01T00:00:00Z".to_string(),
            cards_total: 1,
            cards_done: 0,
            cards_failed: 0,
            agents_active: 1,
            base_branch: Some("main".to_string()),
            base_commit: Some("base123".to_string()),
            default_agent_model: None,
            is_name_generated: true,
            last_agents_summary_at: None,
            planner_session_state: None,
        };
        tokio::fs::write(
            task_dir.join("meta.yaml"),
            serde_yaml::to_string(&meta).unwrap(),
        )
        .await
        .unwrap();
        let project_hash = crate::worktrees::service::project_hash_for_path(
            &dunce::simplified(&source.canonicalize().unwrap()).to_path_buf(),
        );
        let worktree_cache_dir = cache.join("worktrees").join(&project_hash);
        std::fs::create_dir_all(&worktree_cache_dir).unwrap();
        let agent_worktree = worktree_cache_dir.join("agent-worktree");
        let agent_worktree_arg = agent_worktree.to_string_lossy().to_string();
        run_git(
            &source,
            &[
                "worktree",
                "add",
                "-b",
                "refact/task/card",
                &agent_worktree_arg,
                "main",
            ],
        );
        let board = crate::tasks::types::TaskBoard {
            schema_version: 1,
            rev: 1,
            columns: Vec::new(),
            cards: vec![crate::tasks::types::BoardCard {
                id: card_id.to_string(),
                title: "Card".to_string(),
                column: "doing".to_string(),
                priority: "P1".to_string(),
                depends_on: Vec::new(),
                instructions: String::new(),
                assignee: Some(agent_id.to_string()),
                agent_chat_id: Some(chat_id.to_string()),
                retry_count: 0,
                status_updates: Vec::new(),
                comments: vec![],
                final_report: None,
                final_report_structured: None,
                verifier_report: None,
                created_at: "2024-01-01T00:00:00Z".to_string(),
                started_at: None,
                last_heartbeat_at: None,
                completed_at: None,
                agent_branch: Some("refact/task/card".to_string()),
                agent_worktree: Some(agent_worktree.to_string_lossy().to_string()),
                agent_worktree_name: Some("wt-legacy".to_string()),
                base_branch: None,
                base_commit: None,
                ab_variants: None,
                team_members: vec![],
                target_files: Vec::new(),
                scope_guard_mode: Default::default(),
            }],
        };
        tokio::fs::write(
            task_dir.join("board.yaml"),
            serde_yaml::to_string(&board).unwrap(),
        )
        .await
        .unwrap();
        let trajectory = json!({
            "id": chat_id,
            "title": "Legacy Agent",
            "model": "m",
            "mode": "task_agent",
            "tool_use": "agent",
            "messages": [],
            "created_at": "2024-01-01T00:00:00Z",
            "updated_at": "2024-01-01T00:00:00Z",
            "include_project_info": true,
            "checkpoints_enabled": true,
            "task_meta": {
                "task_id": task_id,
                "role": "agents",
                "agent_id": agent_id,
                "card_id": card_id
            }
        });
        tokio::fs::write(
            task_dir
                .join("trajectories")
                .join("agents")
                .join(agent_id)
                .join(format!("{}.json", chat_id)),
            serde_json::to_string(&trajectory).unwrap(),
        )
        .await
        .unwrap();

        let loaded = load_trajectory_for_chat(gcx, chat_id).await.unwrap();
        let worktree = loaded.thread.worktree.unwrap();
        assert_eq!(worktree.id, "wt-legacy");
        assert_eq!(worktree.kind, "task_agent");
        assert_eq!(worktree.root, agent_worktree);
        assert_eq!(worktree.branch.as_deref(), Some("refact/task/card"));
        assert_eq!(worktree.base_branch.as_deref(), Some("main"));
        assert_eq!(worktree.base_commit.as_deref(), Some("base123"));
        assert!(worktree.enforce);
    }

    #[tokio::test]
    async fn trajectory_worktree_legacy_task_agent_rejects_mismatched_chat_identity() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("repo");
        let cache = dir.path().join("cache");
        std::fs::create_dir_all(&source).unwrap();
        init_repo(&source);
        let gcx = crate::global_context::tests::make_test_gcx_with_dirs(
            cache.clone(),
            std::env::temp_dir().join(format!("refact-cfg-{}", uuid::Uuid::new_v4())),
        )
        .await;
        {
            *gcx.documents_state.workspace_folders.lock().unwrap() = vec![source.clone()];
        }
        let _app = AppState::from_gcx(gcx.clone()).await;

        let task_id = "task-legacy-mismatch";
        let agent_id = "agent-1";
        let card_id = "card-1";
        let chat_id = "wrong-agent-chat";
        let task_dir = source.join(".refact").join("tasks").join(task_id);
        tokio::fs::create_dir_all(task_dir.join("trajectories").join("agents").join(agent_id))
            .await
            .unwrap();
        let meta = crate::tasks::types::TaskMeta {
            schema_version: 1,
            id: task_id.to_string(),
            name: "Task".to_string(),
            status: crate::tasks::types::TaskStatus::Active,
            created_at: "2024-01-01T00:00:00Z".to_string(),
            updated_at: "2024-01-01T00:00:00Z".to_string(),
            cards_total: 1,
            cards_done: 0,
            cards_failed: 0,
            agents_active: 1,
            base_branch: Some("main".to_string()),
            base_commit: Some("base123".to_string()),
            default_agent_model: None,
            is_name_generated: true,
            last_agents_summary_at: None,
            planner_session_state: None,
        };
        tokio::fs::write(
            task_dir.join("meta.yaml"),
            serde_yaml::to_string(&meta).unwrap(),
        )
        .await
        .unwrap();
        let agent_worktree = dir.path().join("agent-worktree-mismatch");
        let agent_worktree_arg = agent_worktree.to_string_lossy().to_string();
        run_git(
            &source,
            &[
                "worktree",
                "add",
                "-b",
                "refact/task/card-mismatch",
                &agent_worktree_arg,
                "main",
            ],
        );
        let board = crate::tasks::types::TaskBoard {
            schema_version: 1,
            rev: 1,
            columns: Vec::new(),
            cards: vec![crate::tasks::types::BoardCard {
                id: card_id.to_string(),
                title: "Card".to_string(),
                column: "doing".to_string(),
                priority: "P1".to_string(),
                depends_on: Vec::new(),
                instructions: String::new(),
                assignee: Some(agent_id.to_string()),
                agent_chat_id: Some("actual-agent-chat".to_string()),
                retry_count: 0,
                status_updates: Vec::new(),
                comments: vec![],
                final_report: None,
                final_report_structured: None,
                verifier_report: None,
                created_at: "2024-01-01T00:00:00Z".to_string(),
                started_at: None,
                last_heartbeat_at: None,
                completed_at: None,
                agent_branch: Some("refact/task/card-mismatch".to_string()),
                agent_worktree: Some(agent_worktree.to_string_lossy().to_string()),
                agent_worktree_name: Some("wt-legacy-mismatch".to_string()),
                base_branch: None,
                base_commit: None,
                ab_variants: None,
                team_members: vec![],
                target_files: Vec::new(),
                scope_guard_mode: Default::default(),
            }],
        };
        tokio::fs::write(
            task_dir.join("board.yaml"),
            serde_yaml::to_string(&board).unwrap(),
        )
        .await
        .unwrap();
        let trajectory = json!({
            "id": chat_id,
            "title": "Legacy Agent",
            "model": "m",
            "mode": "task_agent",
            "tool_use": "agent",
            "messages": [],
            "created_at": "2024-01-01T00:00:00Z",
            "updated_at": "2024-01-01T00:00:00Z",
            "include_project_info": true,
            "checkpoints_enabled": true,
            "task_meta": {
                "task_id": task_id,
                "role": "agents",
                "agent_id": agent_id,
                "card_id": card_id
            }
        });
        tokio::fs::write(
            task_dir
                .join("trajectories")
                .join("agents")
                .join(agent_id)
                .join(format!("{}.json", chat_id)),
            serde_json::to_string(&trajectory).unwrap(),
        )
        .await
        .unwrap();

        let loaded = load_trajectory_for_chat(gcx, chat_id).await.unwrap();
        assert!(loaded.thread.worktree.is_none());
    }

    #[tokio::test]
    #[ignore]
    async fn stress_atomic_write_json_pressure_baseline() {
        const MESSAGE_COUNT: usize = 1_000;
        const MESSAGE_SIZE: usize = 1_024;
        const WRITE_RUNS: usize = 120;

        let temp_dir = tempfile::TempDir::new().unwrap();
        let file_path = temp_dir.path().join("stress-trajectory.json");

        let mut messages = Vec::with_capacity(MESSAGE_COUNT);
        for i in 0..MESSAGE_COUNT {
            messages.push(json!({
                "message_id": format!("m{}", i),
                "role": if i % 2 == 0 { "user" } else { "assistant" },
                "content": "x".repeat(MESSAGE_SIZE),
            }));
        }

        let payload = json!({
            "id": "stress-chat",
            "title": "Stress",
            "created_at": "2026-01-01T00:00:00Z",
            "updated_at": "2026-01-01T00:00:00Z",
            "model": "test/model",
            "mode": "agent",
            "tool_use": "agent",
            "messages": messages,
            "isTitleGenerated": false,
        });

        let start = Instant::now();
        for _ in 0..WRITE_RUNS {
            atomic_write_json(&file_path, &payload, TrajectorySourceIdentity::Normal)
                .await
                .unwrap();
        }
        let elapsed = start.elapsed();

        assert!(file_path.exists());
        let content = fs::read_to_string(&file_path).await.unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(
            parsed.get("id").and_then(|v| v.as_str()),
            Some("stress-chat")
        );
        assert_eq!(
            parsed
                .get("messages")
                .and_then(|v| v.as_array())
                .map(|arr| arr.len()),
            Some(MESSAGE_COUNT)
        );

        println!(
            "STRESS_BASELINE trajectory_atomic_write: writes={}, messages={}, msg_size={}, elapsed_ms={}",
            WRITE_RUNS,
            MESSAGE_COUNT,
            MESSAGE_SIZE,
            elapsed.as_millis(),
        );
    }

    #[tokio::test]
    #[ignore]
    async fn stress_trajectory_json_read_parse_baseline() {
        const MESSAGE_COUNT: usize = 1_200;
        const MESSAGE_SIZE: usize = 512;
        const PARSE_RUNS: usize = 400;

        let temp_dir = tempfile::TempDir::new().unwrap();
        let file_path = temp_dir.path().join("stress-parse.json");

        let mut messages = Vec::with_capacity(MESSAGE_COUNT);
        for i in 0..MESSAGE_COUNT {
            messages.push(json!({
                "message_id": format!("msg-{}", i),
                "role": if i % 3 == 0 { "assistant" } else { "user" },
                "content": "y".repeat(MESSAGE_SIZE),
            }));
        }

        let data = TrajectoryData {
            id: "parse-chat".to_string(),
            title: "Parse".to_string(),
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
            model: "test/model".to_string(),
            mode: "agent".to_string(),
            tool_use: "agent".to_string(),
            messages,
            extra: serde_json::Map::new(),
        };

        atomic_write_json(&file_path, &data, TrajectorySourceIdentity::Normal)
            .await
            .unwrap();
        let content = fs::read_to_string(&file_path).await.unwrap();

        let start = Instant::now();
        for _ in 0..PARSE_RUNS {
            let parsed: TrajectoryData = serde_json::from_str(&content).unwrap();
            assert_eq!(parsed.id, "parse-chat");
            assert_eq!(parsed.messages.len(), MESSAGE_COUNT);
        }
        let elapsed = start.elapsed();

        println!(
            "STRESS_BASELINE trajectory_read_parse: parses={}, messages={}, msg_size={}, elapsed_ms={}",
            PARSE_RUNS,
            MESSAGE_COUNT,
            MESSAGE_SIZE,
            elapsed.as_millis(),
        );
    }

    #[serial(trajectory_perf)]
    #[tokio::test]
    async fn trajectory_get_handler_returns_404_for_missing_chat() {
        let dir = tempfile::tempdir().unwrap();
        let (_gcx, app) = make_app_with_workspace(dir.path()).await;
        let err =
            handle_v1_trajectories_get(State(app), AxumPath("nonexistent-chat-id".to_string()))
                .await
                .unwrap_err();
        assert_eq!(err.status_code, StatusCode::NOT_FOUND);
        assert_eq!(err.message, "Trajectory not found");
    }

    #[serial(trajectory_perf)]
    #[tokio::test]
    async fn trajectory_path_handler_returns_404_for_missing_chat() {
        let dir = tempfile::tempdir().unwrap();
        let (_gcx, app) = make_app_with_workspace(dir.path()).await;
        let err =
            handle_v1_trajectory_path(State(app), AxumPath("nonexistent-chat-id".to_string()))
                .await
                .unwrap_err();
        assert_eq!(err.status_code, StatusCode::NOT_FOUND);
    }

    #[serial(trajectory_perf)]
    #[tokio::test]
    async fn trajectory_path_handler_returns_400_for_invalid_id() {
        let dir = tempfile::tempdir().unwrap();
        let (_gcx, app) = make_app_with_workspace(dir.path()).await;
        let err = handle_v1_trajectory_path(State(app), AxumPath("../bad".to_string()))
            .await
            .unwrap_err();
        assert_eq!(err.status_code, StatusCode::BAD_REQUEST);
    }

    #[serial(trajectory_perf)]
    #[tokio::test]
    async fn trajectory_path_handler_returns_normal_trajectory_path() {
        let dir = tempfile::tempdir().unwrap();
        let (_gcx, app) = make_app_with_workspace(dir.path()).await;
        let chat_id = "path-normal";
        let path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        write_trajectory_file(&path, chat_id, "Normal", "2024-01-01T00:00:00Z").await;

        let response = handle_v1_trajectory_path(State(app), AxumPath(chat_id.to_string()))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = hyper::body::to_bytes(response.into_body()).await.unwrap();
        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let returned = payload["path"].as_str().unwrap();
        assert_same_path_str(returned, &path);
    }

    #[serial(trajectory_perf)]
    #[tokio::test]
    async fn trajectory_path_handler_returns_nested_normal_trajectory_path_for_active_session() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;
        let root_chat_id = "path-normal-nested-root";
        let chat_id = "path-normal-nested-active";
        let path = nested_trajectory_path(dir.path(), root_chat_id, chat_id);
        write_trajectory_file(&path, chat_id, "Normal", "2024-01-01T00:00:00Z").await;

        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        session_arc.lock().await.thread.root_chat_id = Some(root_chat_id.to_string());
        gcx.chat_sessions
            .write()
            .await
            .insert(chat_id.to_string(), session_arc);

        let response = handle_v1_trajectory_path(State(app), AxumPath(chat_id.to_string()))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = hyper::body::to_bytes(response.into_body()).await.unwrap();
        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_same_path_str(payload["path"].as_str().unwrap(), &path);
    }

    #[serial(trajectory_perf)]
    #[tokio::test]
    async fn trajectory_path_handler_returns_buddy_path() {
        let dir = tempfile::tempdir().unwrap();
        let (_gcx, app) = make_app_with_workspace(dir.path()).await;
        let chat_id = "path-buddy";
        let buddy_path = dir
            .path()
            .join(".refact")
            .join("buddy")
            .join("chats")
            .join("conversations")
            .join(format!("{chat_id}.json"));
        write_buddy_conversation_file(&buddy_path, chat_id, "Buddy").await;

        let response = handle_v1_trajectory_path(State(app), AxumPath(chat_id.to_string()))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = hyper::body::to_bytes(response.into_body()).await.unwrap();
        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_same_path_str(payload["path"].as_str().unwrap(), &buddy_path);
    }

    #[serial(trajectory_perf)]
    #[tokio::test]
    async fn trajectory_path_handler_does_not_leak_other_source_for_active_buddy_session() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;
        let chat_id = "path-collision-buddy";

        let normal_path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        write_trajectory_file(&normal_path, chat_id, "Normal", "2024-01-01T00:00:00Z").await;

        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        {
            let mut session = session_arc.lock().await;
            session.thread.mode = "buddy".to_string();
            session.thread.buddy_meta = Some(BuddyThreadMeta {
                is_buddy_chat: true,
                buddy_chat_kind: "investigation".to_string(),
                workflow_id: None,
            });
        }
        gcx.chat_sessions
            .write()
            .await
            .insert(chat_id.to_string(), session_arc);

        let err = handle_v1_trajectory_path(State(app), AxumPath(chat_id.to_string()))
            .await
            .unwrap_err();
        assert_eq!(err.status_code, StatusCode::NOT_FOUND);
        assert!(tokio::fs::try_exists(&normal_path).await.unwrap());
    }

    #[serial(trajectory_perf)]
    #[tokio::test]
    async fn trajectory_path_handler_returns_buddy_path_when_active_buddy_session_has_buddy_file() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;
        let chat_id = "path-buddy-resolved";

        let normal_path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        write_trajectory_file(&normal_path, chat_id, "Normal", "2024-01-01T00:00:00Z").await;

        let buddy_path = dir
            .path()
            .join(".refact")
            .join("buddy")
            .join("chats")
            .join("conversations")
            .join(format!("{chat_id}.json"));
        write_buddy_conversation_file(&buddy_path, chat_id, "Buddy").await;

        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        {
            let mut session = session_arc.lock().await;
            session.thread.mode = "buddy".to_string();
            session.thread.buddy_meta = Some(BuddyThreadMeta {
                is_buddy_chat: true,
                buddy_chat_kind: "investigation".to_string(),
                workflow_id: None,
            });
        }
        gcx.chat_sessions
            .write()
            .await
            .insert(chat_id.to_string(), session_arc);

        let response = handle_v1_trajectory_path(State(app), AxumPath(chat_id.to_string()))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = hyper::body::to_bytes(response.into_body()).await.unwrap();
        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let returned = payload["path"].as_str().unwrap();
        assert_same_path_str(returned, &buddy_path);
        assert_ne!(
            normalized_test_path(Path::new(returned)),
            normalized_test_path(&normal_path)
        );
    }

    #[serial(trajectory_perf)]
    #[tokio::test]
    async fn trajectory_path_handler_rejects_malformed_json() {
        let dir = tempfile::tempdir().unwrap();
        let (_gcx, app) = make_app_with_workspace(dir.path()).await;
        let chat_id = "path-malformed";
        let path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        tokio::fs::create_dir_all(path.parent().unwrap())
            .await
            .unwrap();
        tokio::fs::write(&path, b"this is not json").await.unwrap();

        let err = handle_v1_trajectory_path(State(app), AxumPath(chat_id.to_string()))
            .await
            .unwrap_err();
        assert_eq!(err.status_code, StatusCode::NOT_FOUND);
    }

    #[serial(trajectory_perf)]
    #[tokio::test]
    async fn trajectory_path_handler_rejects_id_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        let (_gcx, app) = make_app_with_workspace(dir.path()).await;
        let chat_id = "path-id-mismatch";
        let path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        tokio::fs::create_dir_all(path.parent().unwrap())
            .await
            .unwrap();
        let content = json!({
            "id": "different-id",
            "title": "Wrong id",
            "model": "m",
            "mode": "agent",
            "tool_use": "agent",
            "created_at": "2024-01-01T00:00:00Z",
            "updated_at": "2024-01-01T00:00:00Z",
            "messages": [{"role": "user", "content": "hi"}],
        });
        tokio::fs::write(&path, serde_json::to_string(&content).unwrap())
            .await
            .unwrap();

        let err = handle_v1_trajectory_path(State(app), AxumPath(chat_id.to_string()))
            .await
            .unwrap_err();
        assert_eq!(err.status_code, StatusCode::NOT_FOUND);
    }
}
