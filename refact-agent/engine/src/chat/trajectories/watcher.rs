use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex, OnceLock};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;
use notify::{Config, RecommendedWatcher, RecursiveMode, Watcher};
use tokio::fs;
use tracing::{debug, info, warn};
use crate::chat::types::TrajectorySourceIdentitySessionExt;

use crate::app_state::AppState;
use crate::chat::config::timeouts;
use crate::chat::trajectory_index;
use crate::chat::types::{ExternalReloadPending, TrajectorySourceIdentity};
use crate::global_context::GlobalContext;

use super::load::{load_generic_trajectory_for_chat, load_generic_trajectory_for_chat_matching_source};
use super::model::get_session_runtime_for_trajectory_source;
use super::paths::{
    find_trajectory_file, get_all_task_roots, get_all_task_roots_from_weak,
    get_all_trajectories_dirs_from_weak, index_dir_for_trajectory_file, list_trajectory_dirs,
    stale_entry_cleanup_allowed,
};
use super::queries::{
    deleted_trajectory_event, is_real_dir, is_real_dir_sync, loaded_trajectory_to_meta,
    updated_trajectory_event_from_meta,
};
use super::reload::{
    apply_external_delete_with_revalidation, apply_loaded_external_update_with_repair,
    can_apply_external_reload, persist_loaded_trajectory_repair, ExternalDeleteRevalidationOutcome,
};
use super::writer::{
    consume_trajectory_self_write, trajectory_watcher_self_write_rollout_enabled,
    TrajectorySelfWriteScope,
};

pub(super) async fn refresh_trajectory_index_entry_for_path(
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

pub(super) async fn remove_stale_trajectory_index_entries(gcx: Arc<GlobalContext>, chat_id: &str) {
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
pub(crate) async fn process_trajectory_change(
    gcx: Arc<GlobalContext>,
    chat_id: &str,
    is_remove: bool,
) {
    process_trajectory_change_for_source(gcx, chat_id, is_remove, None).await;
}

pub(super) async fn process_trajectory_change_for_source(
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

pub(super) fn task_trajectory_context_from_path(
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

pub(super) fn is_under_task_root(path: &Path, task_roots: &[PathBuf]) -> bool {
    let path = crate::files_correction::canonicalize_normalized_path(path.to_path_buf());
    task_roots.iter().any(|root| {
        let root = crate::files_correction::canonicalize_normalized_path(root.clone());
        is_real_dir_sync(&root) && path.starts_with(&root)
    })
}

pub(super) fn should_dispatch_trajectory_path(path: &Path, task_roots: &[PathBuf]) -> bool {
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

pub(super) fn is_trajectory_temporary_file(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.rsplit_once(".tmp."))
        .is_some_and(|(prefix, random)| prefix.ends_with(".json") && !random.is_empty())
}

pub(super) fn trajectory_source_identity_from_path(
    path: &Path,
    task_roots: &[PathBuf],
) -> TrajectorySourceIdentity {
    task_trajectory_context_from_path(path, task_roots)
        .map(|(task_id, role, agent_id)| {
            TrajectorySourceIdentity::task(task_id, role, agent_id, None, None)
        })
        .unwrap_or(TrajectorySourceIdentity::Normal)
}

pub(super) async fn collect_task_trajectory_sources_under_path(
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

pub(super) async fn collect_trajectory_sources_under_path(
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

pub(super) fn schedule_trajectory_watcher_overflow_recovery(
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

pub(super) type TrajectoryPendingKey = (String, TrajectorySourceIdentity);

pub(super) struct TrajectoryPendingChange {
    pub(super) last_event_at: Instant,
    pub(super) is_remove: bool,
    pub(super) path: PathBuf,
}

pub(super) type TrajectoryPendingMap =
    std::collections::HashMap<TrajectoryPendingKey, TrajectoryPendingChange>;

pub(super) fn insert_pending_trajectory_change(
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

pub(super) enum TrajectoryWatcherMessage {
    Trajectory {
        chat_id: String,
        is_remove: bool,
        source: TrajectorySourceIdentity,
        path: PathBuf,
    },
    ScanPath(PathBuf),
}

pub(super) fn trajectory_watcher_message_for_path(
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

pub(super) async fn process_trajectory_watcher_change(
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

pub(super) static TRAJECTORY_WATCHER_TASK: OnceLock<StdMutex<Option<tokio::task::JoinHandle<()>>>> =
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

#[cfg(test)]
pub(crate) fn reset_test_trajectory_watcher_task() {
    let mut guard = TRAJECTORY_WATCHER_TASK
        .get_or_init(|| StdMutex::new(None))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(handle) = guard.take() {
        handle.abort();
    }
}
