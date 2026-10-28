use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex as StdMutex, OnceLock, Weak};
use std::time::{Duration, Instant, UNIX_EPOCH};
use serde_json::json;
use tokio::fs;
use tokio::sync::{Mutex as AMutex, Notify};
use tracing::{info, warn};
use crate::chat::types::TrajectorySourceIdentitySessionExt;

use crate::app_state::AppState;
use crate::call_validation::ChatMessage;
use crate::chat::ctx::ChatCtx;
use crate::chat::perf_diagnostics::{self, PerfComponent, PerfOutcome};
use crate::chat::trajectory_index;
use crate::chat::types::{ChatSession, SessionState, TrajectoryCommitIntent, TrajectorySourceIdentity};
use crate::global_context::GlobalContext;
use refact_chat_history::trajectory_event::TrajectoryEvent;
use refact_chat_history::trajectory_snapshot::TrajectorySnapshot;

use super::load::{is_known_trajectory_top_level_key, preserve_existing_trajectory_metadata};
use super::model::trajectory_snapshot_from_session;
use super::paths::{
    prune_internal_traces_throttled, read_existing_trajectory_object, should_vectorize_trajectory,
    validate_trajectory_id,
};
use super::{
    calculate_line_changes_from_chat_messages, calculate_task_progress_from_chat_messages,
    calculate_token_totals_from_chat_messages, count_user_messages, is_placeholder_title,
    spawn_task_name_generation_task, spawn_title_generation_task, trajectory_meta_title,
};
use super::paths::is_internal_trace_link_type;
use super::reload::parsed_rfc3339_utc;
use super::writer::{
    atomic_write_json_with_tmp_path, trajectory_writer_rollout_enabled, unique_trajectory_tmp_path,
};

pub(super) const TRAJECTORY_FLUSH_TIMEOUT: Duration = Duration::from_secs(5);
pub(super) const TRAJECTORY_COMMIT_STALL_TIMEOUT: Duration = Duration::from_secs(60);

pub async fn save_trajectory_snapshot(
    gcx: Arc<GlobalContext>,
    snapshot: TrajectorySnapshot,
) -> Result<(), String> {
    save_trajectory_snapshot_for_ctx(&gcx.chat_ctx(gcx.clone()), snapshot).await
}

pub(crate) async fn save_trajectory_snapshot_for_ctx(
    ctx: &ChatCtx,
    snapshot: TrajectorySnapshot,
) -> Result<(), String> {
    let message_count = snapshot.messages.len() as u64;
    let trajectory_version = snapshot.version;
    let span = perf_diagnostics::span(
        PerfComponent::TrajectoryCommit,
        Some(&snapshot.chat_id),
        None,
    );
    let result = persist_trajectory_snapshot_with_intent_ctx(
        ctx,
        snapshot,
        TrajectoryCommitIntent::Required,
    )
    .await;
    span.finish(
        if result.is_ok() {
            PerfOutcome::Success
        } else {
            PerfOutcome::Failure
        },
        None,
        Some(message_count),
        Some(trajectory_version),
        None,
    );
    result
}

pub(super) struct DetachedTrajectoryWriterState {
    pub(super) pending: Option<(u64, TrajectorySnapshot)>,
    pub(super) requested_version: u64,
    pub(super) committed_version: u64,
    pub(super) in_flight: bool,
    pub(super) error: Option<String>,
    pub(super) error_version: u64,
    pub(super) notify: Arc<Notify>,
    pub(super) write_cache: Arc<AMutex<DetachedTrajectoryWriteCache>>,
}

#[derive(Default)]
pub(super) struct DetachedTrajectoryWriteCache {
    pub(super) backing_path: Option<PathBuf>,
    pub(super) existing_metadata: Option<serde_json::Map<String, serde_json::Value>>,
    pub(super) source: Option<TrajectorySourceIdentity>,
    pub(super) backing_stamp: Option<TrajectoryFileStamp>,
}

#[derive(Clone, PartialEq, Eq)]
pub(super) struct TrajectoryFileStamp {
    pub(super) len: u64,
    pub(super) modified_unix_ms: i64,
}

#[derive(Clone)]
pub(super) struct TrajectoryMetadataCacheEntry {
    pub(super) chat_id: String,
    pub(super) metadata: serde_json::Map<String, serde_json::Value>,
    pub(super) stamp: TrajectoryFileStamp,
    pub(super) last_access: u64,
}

pub(super) const MAX_CACHED_TRAJECTORY_METADATA: usize = 256;

pub(super) static TRAJECTORY_METADATA_CACHE_ACCESS_COUNTER: OnceLock<std::sync::atomic::AtomicU64> =
    OnceLock::new();

pub(super) fn next_trajectory_metadata_cache_access_stamp() -> u64 {
    TRAJECTORY_METADATA_CACHE_ACCESS_COUNTER
        .get_or_init(|| std::sync::atomic::AtomicU64::new(1))
        .fetch_add(1, Ordering::Relaxed)
}

pub(super) fn trajectory_metadata_cache(
) -> &'static StdMutex<std::collections::HashMap<PathBuf, TrajectoryMetadataCacheEntry>> {
    static CACHE: OnceLock<
        StdMutex<std::collections::HashMap<PathBuf, TrajectoryMetadataCacheEntry>>,
    > = OnceLock::new();
    CACHE.get_or_init(|| StdMutex::new(std::collections::HashMap::new()))
}

pub(super) fn trajectory_metadata_cache_key(path: &Path) -> PathBuf {
    crate::files_correction::canonicalize_normalized_path(path.to_path_buf())
}

pub(super) fn preservable_trajectory_metadata(
    source: &serde_json::Map<String, serde_json::Value>,
) -> serde_json::Map<String, serde_json::Value> {
    source
        .iter()
        .filter(|(key, _)| {
            // `created_at` is a known key but must survive re-saves, so it is cached too.
            matches!(key.as_str(), "browser_meta" | "created_at")
                || !is_known_trajectory_top_level_key(key)
        })
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

pub(super) fn store_trajectory_metadata_cache_entry(path: &Path, entry: TrajectoryMetadataCacheEntry) {
    let path = trajectory_metadata_cache_key(path);
    let mut cache = trajectory_metadata_cache()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if !cache.contains_key(&path) {
        while cache.len() >= MAX_CACHED_TRAJECTORY_METADATA {
            let Some(victim) = cache
                .iter()
                .min_by_key(|(_, entry)| entry.last_access)
                .map(|(path, _)| path.clone())
            else {
                break;
            };
            cache.remove(&victim);
        }
    }
    let mut entry = entry;
    entry.last_access = next_trajectory_metadata_cache_access_stamp();
    cache.insert(path, entry);
}

pub(super) async fn trajectory_file_stamp(path: &Path) -> Option<TrajectoryFileStamp> {
    let metadata = fs::symlink_metadata(path).await.ok()?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return None;
    }
    Some(TrajectoryFileStamp {
        len: metadata.len(),
        modified_unix_ms: metadata
            .modified()
            .ok()?
            .duration_since(UNIX_EPOCH)
            .ok()?
            .as_millis()
            .try_into()
            .ok()?,
    })
}

pub(super) async fn trajectory_stamp_matches(path: &Path, expected: &TrajectoryFileStamp) -> bool {
    trajectory_file_stamp(path).await.as_ref() == Some(expected)
}

pub(super) async fn cached_existing_trajectory_object(
    path: &Path,
    chat_id: &str,
) -> Result<Option<serde_json::Map<String, serde_json::Value>>, String> {
    let cache_key = trajectory_metadata_cache_key(path);
    let stamp = trajectory_file_stamp(path).await;
    if let Some(stamp) = stamp.as_ref() {
        let cached = {
            let mut cache = trajectory_metadata_cache()
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(entry) = cache.get_mut(&cache_key) {
                entry.last_access = next_trajectory_metadata_cache_access_stamp();
                (entry.chat_id == chat_id && &entry.stamp == stamp).then(|| entry.metadata.clone())
            } else {
                None
            }
        };
        if cached.is_some() {
            return Ok(cached);
        }
    }

    let metadata = read_existing_trajectory_object(path, chat_id).await?;
    match (metadata.as_ref(), stamp) {
        (Some(metadata), Some(stamp)) => {
            store_trajectory_metadata_cache_entry(
                path,
                TrajectoryMetadataCacheEntry {
                    chat_id: chat_id.to_string(),
                    metadata: preservable_trajectory_metadata(metadata),
                    stamp,
                    last_access: 0,
                },
            );
        }
        _ => {
            trajectory_metadata_cache()
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .remove(&cache_key);
        }
    }
    Ok(metadata)
}

pub(super) async fn cache_written_trajectory_metadata(
    path: &Path,
    chat_id: &str,
    trajectory: &serde_json::Value,
) -> Option<TrajectoryFileStamp> {
    let metadata = preservable_trajectory_metadata(trajectory.as_object()?);
    let stamp = trajectory_file_stamp(path).await?;
    store_trajectory_metadata_cache_entry(
        path,
        TrajectoryMetadataCacheEntry {
            chat_id: chat_id.to_string(),
            metadata,
            stamp: stamp.clone(),
            last_access: 0,
        },
    );
    Some(stamp)
}

impl Default for DetachedTrajectoryWriterState {
    fn default() -> Self {
        Self {
            pending: None,
            requested_version: 0,
            committed_version: 0,
            in_flight: false,
            error: None,
            error_version: 0,
            notify: Arc::new(Notify::new()),
            write_cache: Arc::new(AMutex::new(Default::default())),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) struct DetachedTrajectoryWriterKey {
    pub(super) workspace_folders: Vec<PathBuf>,
    pub(super) source: TrajectorySourceIdentity,
    pub(super) chat_id: String,
}

pub(super) type DetachedTrajectoryWriterRegistry = std::collections::HashMap<
    DetachedTrajectoryWriterKey,
    Weak<AMutex<DetachedTrajectoryWriterState>>,
>;

pub(super) fn detached_trajectory_writers() -> &'static StdMutex<DetachedTrajectoryWriterRegistry> {
    static WRITERS: OnceLock<StdMutex<DetachedTrajectoryWriterRegistry>> = OnceLock::new();
    WRITERS.get_or_init(|| StdMutex::new(std::collections::HashMap::new()))
}

pub(super) fn detached_trajectory_writer_key(
    ctx: &ChatCtx,
    snapshot: &TrajectorySnapshot,
) -> DetachedTrajectoryWriterKey {
    let workspace_folders = ctx
        .workspace
        .workspace_folders
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    let source = if let Some(task_meta) = snapshot.task_meta.as_ref() {
        TrajectorySourceIdentity::from_task_meta(task_meta)
    } else if snapshot.buddy_meta.is_some() {
        TrajectorySourceIdentity::Buddy
    } else {
        TrajectorySourceIdentity::Normal
    };
    DetachedTrajectoryWriterKey {
        workspace_folders,
        source,
        chat_id: snapshot.chat_id.clone(),
    }
}

pub(super) fn detached_trajectory_writer(
    ctx: &ChatCtx,
    snapshot: &TrajectorySnapshot,
) -> Arc<AMutex<DetachedTrajectoryWriterState>> {
    let mut writers = detached_trajectory_writers()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    writers.retain(|_, writer| writer.strong_count() > 0);
    let key = detached_trajectory_writer_key(ctx, snapshot);
    if let Some(writer) = writers.get(&key).and_then(Weak::upgrade) {
        return writer;
    }
    let writer = Arc::new(AMutex::new(Default::default()));
    writers.insert(key, Arc::downgrade(&writer));
    writer
}

pub(super) fn prune_detached_trajectory_writers() {
    detached_trajectory_writers()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .retain(|_, writer| writer.strong_count() > 0);
}

pub(super) async fn flush_detached_trajectory_writers() -> Result<(), Vec<String>> {
    let errors = {
        let writers = {
            let mut writers = detached_trajectory_writers()
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            writers.retain(|_, writer| writer.strong_count() > 0);
            writers
                .values()
                .filter_map(Weak::upgrade)
                .collect::<Vec<_>>()
        };
        futures::future::join_all(writers.into_iter().map(|writer| async move {
            let target_version = writer.lock().await.requested_version;
            if target_version == 0 {
                Ok(())
            } else {
                wait_for_detached_trajectory_commit(writer, target_version).await
            }
        }))
        .await
        .into_iter()
        .filter_map(Result::err)
        .collect::<Vec<_>>()
    };
    prune_detached_trajectory_writers();
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

pub(crate) async fn persist_trajectory_snapshot_with_intent(
    gcx: Arc<GlobalContext>,
    snapshot: TrajectorySnapshot,
    intent: TrajectoryCommitIntent,
) -> Result<(), String> {
    persist_trajectory_snapshot_with_intent_ctx(&gcx.chat_ctx(gcx.clone()), snapshot, intent).await
}

pub(super) async fn persist_trajectory_snapshot_with_intent_ctx(
    ctx: &ChatCtx,
    snapshot: TrajectorySnapshot,
    intent: TrajectoryCommitIntent,
) -> Result<(), String> {
    if trajectory_writer_rollout_enabled() {
        schedule_detached_trajectory_writer(ctx, snapshot, intent).await
    } else {
        save_trajectory_snapshot_inner(ctx, snapshot, None).await
    }
}

pub(super) async fn schedule_detached_trajectory_writer(
    ctx: &ChatCtx,
    snapshot: TrajectorySnapshot,
    intent: TrajectoryCommitIntent,
) -> Result<(), String> {
    let writer = detached_trajectory_writer(ctx, &snapshot);
    let target_version = {
        let mut state = writer.lock().await;
        state.requested_version = state.requested_version.saturating_add(1);
        let target_version = state.requested_version;
        state.pending = Some((target_version, snapshot));
        if !state.in_flight {
            state.in_flight = true;
            let writer = writer.clone();
            let ctx: ChatCtx = ctx.clone();
            tokio::spawn(async move {
                run_detached_trajectory_writer(&ctx, writer).await;
            });
        }
        target_version
    };
    if intent.requires_durability() {
        let result = wait_for_detached_trajectory_commit(writer.clone(), target_version).await;
        prune_detached_trajectory_writers();
        result?;
    }
    Ok(())
}

pub(super) async fn run_detached_trajectory_writer(
    ctx: &ChatCtx,
    writer: Arc<AMutex<DetachedTrajectoryWriterState>>,
) {
    loop {
        let (version, snapshot, write_cache) = {
            let mut state = writer.lock().await;
            let Some((version, snapshot)) = state.pending.take() else {
                state.in_flight = false;
                state.notify.notify_waiters();
                return;
            };
            (version, snapshot, state.write_cache.clone())
        };
        #[cfg(test)]
        let hook_chat_id = snapshot.chat_id.clone();
        let result = save_trajectory_snapshot_inner(ctx, snapshot, Some(write_cache)).await;
        #[cfg(test)]
        if result.is_err() {
            wait_for_test_detached_trajectory_writer_failure(&hook_chat_id).await;
        }
        let mut state = writer.lock().await;
        match result {
            Ok(()) => {
                state.committed_version = state.committed_version.max(version);
                state.error = None;
                state.error_version = 0;
                state.notify.notify_waiters();
            }
            Err(error) => {
                warn!(
                    "Detached trajectory writer failed at version {}: {}",
                    version, error
                );
                state.error = Some(error);
                state.error_version = version;
                state.notify.notify_waiters();
                if state
                    .pending
                    .as_ref()
                    .is_some_and(|(pending_version, _)| *pending_version > version)
                {
                    drop(state);
                    tokio::time::sleep(Duration::from_millis(10)).await;
                    continue;
                }
                state.in_flight = false;
                return;
            }
        }
    }
}

pub(super) async fn wait_for_detached_trajectory_commit(
    writer: Arc<AMutex<DetachedTrajectoryWriterState>>,
    target_version: u64,
) -> Result<(), String> {
    let mut last_progress = None;
    let mut deadline = Instant::now() + TRAJECTORY_COMMIT_STALL_TIMEOUT;
    loop {
        let notified = {
            let state = writer.lock().await;
            if state.committed_version >= target_version {
                return Ok(());
            }
            if state.error_version >= target_version {
                let error = state.error.clone().unwrap_or_else(|| {
                    format!(
                        "Detached trajectory writer failed at version {}",
                        state.error_version
                    )
                });
                return Err(error);
            }
            if !state.in_flight && state.pending.is_none() {
                return Err(state.error.clone().unwrap_or_else(|| {
                    format!(
                        "Detached trajectory writer stopped before committing version {} \
                         (committed {})",
                        target_version, state.committed_version
                    )
                }));
            }
            let progress = (
                state.committed_version,
                state.error_version,
                state.requested_version,
            );
            if last_progress != Some(progress) {
                last_progress = Some(progress);
                deadline = Instant::now() + TRAJECTORY_COMMIT_STALL_TIMEOUT;
            }
            let mut notified = Box::pin(state.notify.clone().notified_owned());
            let already_notified = notified.as_mut().enable();
            drop(state);
            if already_notified {
                continue;
            }
            notified
        };
        if tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), notified)
            .await
            .is_err()
        {
            let state = writer.lock().await;
            return Err(format!(
                "Detached trajectory writer made no progress for {} ms while committing version {} \
                 (committed {}, last error {:?})",
                TRAJECTORY_COMMIT_STALL_TIMEOUT.as_millis(),
                target_version,
                state.committed_version,
                state.error
            ));
        }
    }
}

#[cfg(test)]
pub(super) static TEST_MESSAGE_SERIALIZATION_FAILURE_ID: std::sync::OnceLock<
    std::sync::Mutex<Option<String>>,
> = std::sync::OnceLock::new();

#[cfg(test)]
pub(super) fn test_message_serialization_should_fail(message: &ChatMessage) -> bool {
    TEST_MESSAGE_SERIALIZATION_FAILURE_ID
        .get_or_init(|| std::sync::Mutex::new(None))
        .lock()
        .ok()
        .and_then(|message_id| message_id.clone())
        .as_deref()
        == Some(message.message_id.as_str())
}

#[cfg(test)]
pub(super) fn set_test_message_serialization_failure(message_id: Option<String>) {
    *TEST_MESSAGE_SERIALIZATION_FAILURE_ID
        .get_or_init(|| std::sync::Mutex::new(None))
        .lock()
        .expect("test serialization failure lock poisoned") = message_id;
}

#[cfg(test)]
pub(super) static TEST_DETACHED_TRAJECTORY_WRITER_FAILURE_HOOK: std::sync::OnceLock<
    std::sync::Mutex<Option<(String, Arc<Notify>, Arc<Notify>)>>,
> = std::sync::OnceLock::new();

#[cfg(test)]
pub(super) fn set_test_detached_trajectory_writer_failure_hook(
    hook: Option<(String, Arc<Notify>, Arc<Notify>)>,
) {
    *TEST_DETACHED_TRAJECTORY_WRITER_FAILURE_HOOK
        .get_or_init(|| std::sync::Mutex::new(None))
        .lock()
        .expect("detached trajectory writer failure hook lock poisoned") = hook;
}

#[cfg(test)]
pub(super) async fn wait_for_test_detached_trajectory_writer_failure(chat_id: &str) {
    let hook = TEST_DETACHED_TRAJECTORY_WRITER_FAILURE_HOOK
        .get_or_init(|| std::sync::Mutex::new(None))
        .lock()
        .expect("detached trajectory writer failure hook lock poisoned")
        .clone();
    if let Some((hook_chat_id, failed, release)) = hook {
        if hook_chat_id != chat_id {
            return;
        }
        failed.notify_one();
        release.notified().await;
    }
}

pub(super) async fn save_trajectory_snapshot_inner(
    ctx: &ChatCtx,
    mut snapshot: TrajectorySnapshot,
    write_cache: Option<Arc<AMutex<DetachedTrajectoryWriteCache>>>,
) -> Result<(), String> {
    validate_trajectory_id(&snapshot.chat_id).map_err(|e| e.message)?;
    let chat_id = snapshot.chat_id.clone();
    let existing_no_meta_path = if snapshot.task_meta.is_none() && snapshot.buddy_meta.is_none() {
        ctx.trajectories
            .find_normal_trajectory_path(&snapshot.chat_id)
            .await
    } else {
        None
    };
    if snapshot.messages.is_empty()
        && snapshot.pending_deliveries.is_empty()
        && snapshot.task_meta.is_none()
        && snapshot.buddy_meta.is_none()
        && snapshot.frozen_request_prefix.is_none()
        && snapshot.goal.is_none()
        && snapshot.auto_compression_cap.is_none()
        && !snapshot.auto_compression_cap_pending
        && snapshot.title.is_empty()
        && existing_no_meta_path.is_none()
    {
        return Ok(());
    }

    let message_count = snapshot.messages.len();
    let collect_metrics = snapshot.task_meta.is_none() && snapshot.buddy_meta.is_none();
    let should_generate_title =
        is_placeholder_title(&snapshot.title) && !snapshot.is_title_generated && message_count > 0;
    let should_generate_task_name = snapshot
        .task_meta
        .as_ref()
        .is_some_and(|task_meta| task_meta.role == "planner");
    let retain_background_messages = should_generate_title || should_generate_task_name;
    let messages = std::mem::take(&mut snapshot.messages);
    let mut serialize_span = perf_diagnostics::span(
        PerfComponent::TrajectorySerialize,
        Some(&snapshot.chat_id),
        None,
    );
    let message_serialization = tokio::task::spawn_blocking(move || {
        let messages_json: Result<Vec<serde_json::Value>, String> = messages
            .iter()
            .enumerate()
            .map(|(index, message)| {
                #[cfg(test)]
                if test_message_serialization_should_fail(message) {
                    return Err(format!(
                        "Failed to serialize trajectory message at index {index} (id {}, role {}): test failure",
                        message.message_id, message.role
                    ));
                }
                serde_json::to_value(message).map_err(|error| {
                    format!(
                        "Failed to serialize trajectory message at index {index} (id {}, role {}): {error}",
                        message.message_id, message.role
                    )
                })
            })
            .collect();
        messages_json.map(|messages_json| (messages, messages_json))
    })
    .await;
    let (messages, messages_json) = match message_serialization {
        Ok(Ok(result)) => {
            serialize_span.pause();
            result
        }
        Ok(Err(error)) => {
            serialize_span.finish(
                PerfOutcome::Failure,
                None,
                Some(message_count as u64),
                Some(snapshot.version),
                None,
            );
            return Err(error);
        }
        Err(error) => {
            serialize_span.finish(
                PerfOutcome::Failure,
                None,
                Some(message_count as u64),
                Some(snapshot.version),
                None,
            );
            return Err(format!(
                "Trajectory message serialization task failed: {}",
                error
            ));
        }
    };
    let metric_scan_span = perf_diagnostics::is_enabled()
        .then(|| perf_diagnostics::span(PerfComponent::TrajectoryMetricScan, Some(&chat_id), None));
    let metric_scan = tokio::task::spawn_blocking(move || {
        let background_messages_json = retain_background_messages.then(|| messages_json.clone());
        let line_changes =
            collect_metrics.then(|| calculate_line_changes_from_chat_messages(&messages));
        let task_progress =
            collect_metrics.then(|| calculate_task_progress_from_chat_messages(&messages));
        let token_totals =
            collect_metrics.then(|| calculate_token_totals_from_chat_messages(&messages));
        (
            messages_json,
            background_messages_json,
            line_changes,
            task_progress,
            token_totals,
        )
    })
    .await;
    if let Some(span) = metric_scan_span {
        span.finish(
            if metric_scan.is_ok() {
                PerfOutcome::Success
            } else {
                PerfOutcome::Failure
            },
            None,
            Some(message_count as u64),
            Some(snapshot.version),
            None,
        );
    }
    let (messages_json, background_messages_json, line_changes, task_progress, token_totals) =
        match metric_scan {
            Ok(result) => result,
            Err(error) => {
                serialize_span.finish(
                    PerfOutcome::Failure,
                    None,
                    Some(message_count as u64),
                    Some(snapshot.version),
                    None,
                );
                return Err(format!("Trajectory metric scan task failed: {}", error));
            }
        };

    let mut trajectory = json!({
        "id": snapshot.chat_id,
        "title": snapshot.title,
        "model": snapshot.model,
        "mode": snapshot.mode,
        "tool_use": snapshot.tool_use,
        "messages": messages_json,
        "created_at": snapshot.created_at,
        "boost_reasoning": snapshot.boost_reasoning,
        "checkpoints_enabled": snapshot.checkpoints_enabled,
        "context_tokens_cap": snapshot.context_tokens_cap,
        "auto_compression_cap": snapshot.auto_compression_cap,
        "auto_compression_cap_pending": snapshot.auto_compression_cap_pending,
        "include_project_info": snapshot.include_project_info,
        "isTitleGenerated": snapshot.is_title_generated,
        "auto_approve_editing_tools": snapshot.auto_approve_editing_tools,
        "auto_approve_dangerous_commands": snapshot.auto_approve_dangerous_commands,
        "autonomous_no_confirm": snapshot.autonomous_no_confirm,
    });

    if let Some(ref effort) = snapshot.reasoning_effort {
        trajectory["reasoning_effort"] = serde_json::Value::String(effort.clone());
    }
    if let Some(budget) = snapshot.thinking_budget {
        trajectory["thinking_budget"] = json!(budget);
    }
    if let Some(temp) = snapshot.temperature {
        trajectory["temperature"] = json!(temp);
    }
    if let Some(freq) = snapshot.frequency_penalty {
        trajectory["frequency_penalty"] = json!(freq);
    }
    if let Some(max_t) = snapshot.max_tokens {
        trajectory["max_tokens"] = json!(max_t);
    }
    if let Some(ref prev) = snapshot.previous_response_id {
        trajectory["previous_response_id"] = serde_json::Value::String(prev.clone());
    }
    if let Some(parallel) = snapshot.parallel_tool_calls {
        trajectory["parallel_tool_calls"] = json!(parallel);
    }
    if let Some(ref skill) = snapshot.active_skill {
        trajectory["active_skill"] = serde_json::Value::String(skill.clone());
    }
    if let Some(auto_enrich) = snapshot.auto_enrichment_enabled {
        trajectory["auto_enrichment_enabled"] = json!(auto_enrich);
    }
    if let Some(ref buddy_meta) = snapshot.buddy_meta {
        trajectory["buddy_meta"] = serde_json::to_value(buddy_meta).unwrap_or_default();
    }
    if let Some(auto_compact) = snapshot.auto_compact_enabled {
        trajectory["auto_compact_enabled"] = json!(auto_compact);
    }
    if let Some(ref frozen_request_prefix) = snapshot.frozen_request_prefix {
        trajectory["frozen_request_prefix"] =
            serde_json::to_value(frozen_request_prefix).unwrap_or_default();
    }
    if let Some(ref claude_code_identity) = snapshot.claude_code_identity {
        trajectory["claude_code_identity"] =
            serde_json::to_value(claude_code_identity).unwrap_or_default();
    }
    if let Some(wake_up_at) = snapshot.wake_up_at {
        trajectory["wake_up_at"] = json!(wake_up_at);
    }
    if !snapshot.waiting_for_card_ids.is_empty() {
        trajectory["waiting_for_card_ids"] = json!(snapshot.waiting_for_card_ids);
    }
    if !snapshot.pending_deliveries.is_empty() {
        trajectory["pending_deliveries"] = json!(snapshot.pending_deliveries);
    }
    if let Some(ref goal) = snapshot.goal {
        trajectory["goal"] = serde_json::to_value(goal).unwrap_or_default();
    }
    if !snapshot.goal_ledger.is_empty() {
        trajectory["goal_ledger"] = serde_json::to_value(&snapshot.goal_ledger).unwrap_or_default();
    }
    if let Some(blocked_until_ms) = snapshot.goal_verification_blocked_until_ms {
        trajectory["goal_verification_blocked_until_ms"] = json!(blocked_until_ms);
    }
    if let Some(ref worktree) = snapshot.worktree {
        trajectory["worktree"] = serde_json::to_value(worktree).unwrap_or_default();
    }

    if let Some(ref parent_id) = snapshot.parent_id {
        trajectory["parent_id"] = serde_json::Value::String(parent_id.clone());
    }
    if let Some(ref link_type) = snapshot.link_type {
        trajectory["link_type"] = serde_json::Value::String(link_type.clone());
    }

    let effective_root = snapshot
        .root_chat_id
        .clone()
        .unwrap_or_else(|| snapshot.chat_id.clone());
    trajectory["root_chat_id"] = serde_json::Value::String(effective_root);

    if let Some(ref task_meta) = snapshot.task_meta {
        trajectory["task_meta"] = serde_json::to_value(task_meta).unwrap_or_default();
    }

    let source = if let Some(task_meta) = snapshot.task_meta.as_ref() {
        TrajectorySourceIdentity::from_task_meta(task_meta)
    } else if snapshot.buddy_meta.is_some() {
        TrajectorySourceIdentity::Buddy
    } else {
        TrajectorySourceIdentity::Normal
    };
    let cached_entry = if let Some(write_cache) = write_cache.as_ref() {
        let write_cache = write_cache.lock().await;
        if write_cache.source.as_ref() == Some(&source) {
            match (
                write_cache.backing_path.clone(),
                write_cache.existing_metadata.clone(),
                write_cache.backing_stamp.clone(),
            ) {
                (Some(path), Some(metadata), Some(stamp)) => Some((path, metadata, stamp)),
                _ => None,
            }
        } else {
            None
        }
    } else {
        None
    };
    let cached_path_and_metadata = match cached_entry {
        Some((path, metadata, stamp))
            if metadata.get("id").and_then(|value| value.as_str())
                == Some(snapshot.chat_id.as_str())
                && trajectory_stamp_matches(&path, &stamp).await =>
        {
            Some((path, metadata))
        }
        _ => None,
    };
    let file_path_result = if let Some((path, _)) = cached_path_and_metadata.as_ref() {
        Ok(path.clone())
    } else if let Some(ref task_meta) = snapshot.task_meta {
        ctx.trajectories
            .safe_new_task_trajectory_file(task_meta, &snapshot.chat_id)
            .await
    } else if snapshot.buddy_meta.is_some() {
        ctx.trajectories
            .safe_new_buddy_trajectory_file(&snapshot.chat_id)
            .await
    } else if let Some(path) = existing_no_meta_path {
        Ok(path)
    } else {
        ctx.trajectories
            .safe_new_nested_trajectory_file(&snapshot.chat_id, snapshot.root_chat_id.as_deref())
            .await
    };
    let file_path = match file_path_result {
        Ok(path) => path,
        Err(error) => {
            serialize_span.finish(
                PerfOutcome::Failure,
                None,
                Some(message_count as u64),
                Some(snapshot.version),
                None,
            );
            return Err(error);
        }
    };
    let existing_trajectory = match cached_path_and_metadata.map(|(_, metadata)| metadata) {
        Some(existing_trajectory) => Some(existing_trajectory),
        None => match cached_existing_trajectory_object(&file_path, &snapshot.chat_id).await {
            Ok(existing_trajectory) => existing_trajectory,
            Err(error) => {
                serialize_span.finish(
                    PerfOutcome::Failure,
                    None,
                    Some(message_count as u64),
                    Some(snapshot.version),
                    None,
                );
                return Err(error);
            }
        },
    };

    let updated_at = chrono::Utc::now().to_rfc3339();
    trajectory["updated_at"] = serde_json::Value::String(updated_at.clone());
    if let Some(existing_created_at) = existing_trajectory
        .as_ref()
        .and_then(|existing| existing.get("created_at"))
        .and_then(|created_at| created_at.as_str())
        .filter(|created_at| parsed_rfc3339_utc(created_at).is_some())
    {
        trajectory["created_at"] = serde_json::Value::String(existing_created_at.to_string());
    }
    preserve_existing_trajectory_metadata(&mut trajectory, existing_trajectory);

    let tmp_path = unique_trajectory_tmp_path(&file_path);
    serialize_span.resume();
    let serialized = tokio::task::spawn_blocking(move || {
        let json_result = serde_json::to_string_pretty(&trajectory)
            .map_err(|e| format!("Failed to serialize trajectory: {}", e));
        (trajectory, json_result)
    })
    .await;
    let (trajectory, json_result) = match serialized {
        Ok((trajectory, json_result)) => {
            serialize_span.finish(
                if json_result.is_ok() {
                    PerfOutcome::Success
                } else {
                    PerfOutcome::Failure
                },
                json_result.as_ref().ok().map(|json| json.len() as u64),
                Some(message_count as u64),
                Some(snapshot.version),
                None,
            );
            (trajectory, json_result)
        }
        Err(error) => {
            serialize_span.finish(
                PerfOutcome::Failure,
                None,
                Some(message_count as u64),
                Some(snapshot.version),
                None,
            );
            return Err(format!("Trajectory serialization task failed: {}", error));
        }
    };
    let atomic_write_span = perf_diagnostics::span(
        PerfComponent::TrajectoryAtomicWrite,
        Some(&snapshot.chat_id),
        Some(&file_path),
    );
    let atomic_write_result = atomic_write_json_with_tmp_path(
        &file_path,
        &tmp_path,
        json_result,
        Some("Failed to write trajectory"),
        Some(source.clone()),
    )
    .await;
    atomic_write_span.finish(
        if atomic_write_result.is_ok() {
            PerfOutcome::Success
        } else {
            PerfOutcome::Failure
        },
        None,
        Some(message_count as u64),
        Some(snapshot.version),
        None,
    );
    atomic_write_result?;

    let backing_stamp =
        cache_written_trajectory_metadata(&file_path, &snapshot.chat_id, &trajectory).await;
    if let Some(write_cache) = write_cache.as_ref() {
        let metadata = trajectory.as_object().cloned();
        let mut write_cache = write_cache.lock().await;
        if let (Some(metadata), Some(stamp)) = (metadata, backing_stamp) {
            write_cache.backing_path = Some(file_path.clone());
            write_cache.existing_metadata = Some(metadata);
            write_cache.source = Some(source.clone());
            write_cache.backing_stamp = Some(stamp);
        } else {
            *write_cache = DetachedTrajectoryWriteCache::default();
        }
    }

    info!(
        "Saved trajectory for chat {} ({} messages) to {:?}",
        snapshot.chat_id, message_count, file_path
    );

    if !is_internal_trace_link_type(snapshot.link_type.as_deref()) {
        if let Some(dir) = ctx.trajectories.index_dir_for_trajectory_file(&file_path).await {
            let source_hint = if let Some(task_meta) = snapshot.task_meta.as_ref() {
                Some(TrajectorySourceIdentity::from_task_meta(task_meta))
            } else if snapshot.buddy_meta.is_some() {
                Some(TrajectorySourceIdentity::Buddy)
            } else {
                Some(TrajectorySourceIdentity::Normal)
            };
            trajectory_index::upsert_trajectory_index_entry_from_owned_value_with_rollout(
                &ctx.chat.trajectory_index_coordinator,
                &dir,
                &file_path,
                trajectory,
                source_hint,
            )
            .await?;
        }
    } else if let Some(parent) = file_path.parent() {
        match prune_internal_traces_throttled(parent).await {
            Ok(0) => {}
            Ok(removed) => info!("pruned {removed} old internal traces from {:?}", parent),
            Err(error) => warn!("internal trace pruning failed: {error}"),
        }
    }

    if snapshot.buddy_meta.is_some() {
        ctx.trajectories.prune_buddy_conversations_throttled().await;
    }

    if should_vectorize_trajectory(&source, snapshot.link_type.as_deref()) {
        ctx.trajectories
            .enqueue_trajectory_vectorization(file_path.clone())
            .await;
    }

    if snapshot.task_meta.is_none() && snapshot.buddy_meta.is_none() {
        let effective_root = snapshot
            .root_chat_id
            .clone()
            .unwrap_or_else(|| snapshot.chat_id.clone());
        let sessions = ctx.chat.sessions.clone();
        let source = TrajectorySourceIdentity::Normal;
        let live_session = sessions.read().await.get(&snapshot.chat_id).cloned();
        let runtime = live_session.as_ref().and_then(|session| {
            let session = session.try_lock().ok()?;
            Some(if source.matches_session(&session) {
                (
                    session.runtime.state.to_string(),
                    session.runtime.error.clone(),
                    session.thread.worktree.clone(),
                )
            } else {
                (SessionState::Idle.to_string(), None, None)
            })
        });
        let (session_state, session_error, session_worktree) = match runtime {
            Some((state, error, worktree)) => (Some(state), error, worktree),
            None if live_session.is_none() => (Some(SessionState::Idle.to_string()), None, None),
            None => (None, None, None),
        };
        let (total_lines_added, total_lines_removed) = line_changes.unwrap_or_default();
        let (tasks_total, tasks_done, tasks_failed) = task_progress.unwrap_or_default();
        let token_totals = token_totals.unwrap_or_default();
        let tx = &ctx.chat.trajectory_events_tx;
        {
            let event = TrajectoryEvent {
                event_type: "updated".to_string(),
                id: snapshot.chat_id.clone(),
                updated_at: Some(updated_at),
                title: Some(trajectory_meta_title(&snapshot.title)),
                is_title_generated: Some(snapshot.is_title_generated),
                session_state,
                error: session_error,
                message_count: Some(message_count),
                parent_id: snapshot.parent_id.clone(),
                link_type: snapshot.link_type.clone(),
                root_chat_id: Some(effective_root),
                task_id: None,
                task_role: None,
                agent_id: None,
                card_id: None,
                model: Some(snapshot.model.clone()),
                mode: Some(snapshot.mode.clone()),
                worktree: snapshot.worktree.clone().or(session_worktree),
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
            let _ = tx.send(event);
        }

        if should_generate_title {
            let _ = spawn_title_generation_task(
                ctx,
                snapshot.chat_id.clone(),
                background_messages_json.unwrap_or_default(),
                file_path.clone(),
                TrajectorySourceIdentity::Normal,
            );
        }
    } else if snapshot.task_meta.is_none() && snapshot.buddy_meta.is_some() {
        if should_generate_title {
            let _ = spawn_title_generation_task(
                ctx,
                snapshot.chat_id.clone(),
                background_messages_json.unwrap_or_default(),
                file_path.clone(),
                TrajectorySourceIdentity::Buddy,
            );
        }
    } else if let Some(ref task_meta) = snapshot.task_meta {
        let mut background_messages_json = background_messages_json.unwrap_or_default();

        if should_generate_title {
            let _ = spawn_title_generation_task(
                ctx,
                snapshot.chat_id.clone(),
                background_messages_json.clone(),
                file_path.clone(),
                TrajectorySourceIdentity::from_task_meta(task_meta),
            );
        }

        if task_meta.role == "planner" {
            let user_message_count = count_user_messages(&background_messages_json);
            if user_message_count >= 1 {
                spawn_task_name_generation_task(
                    ctx,
                    task_meta.task_id.clone(),
                    std::mem::take(&mut background_messages_json),
                );
            }
        }
    }

    Ok(())
}

pub async fn try_save_trajectory(
    app: AppState,
    session_arc: Arc<AMutex<ChatSession>>,
) -> Result<bool, String> {
    try_save_trajectory_with_intent(app, session_arc, TrajectoryCommitIntent::Required).await
}

pub async fn try_save_trajectory_with_intent(
    app: AppState,
    session_arc: Arc<AMutex<ChatSession>>,
    intent: TrajectoryCommitIntent,
) -> Result<bool, String> {
    if !intent.persists() {
        return Ok(true);
    }
    if !trajectory_writer_rollout_enabled() {
        return try_save_trajectory_legacy_with_intent(app, session_arc, intent).await;
    }
    {
        let session = session_arc.lock().await;
        if !session.trajectory_dirty {
            return Ok(true);
        }
    }
    let target_version = schedule_trajectory_writer(app, session_arc.clone(), intent).await?;
    if intent.requires_durability() {
        wait_for_trajectory_commit(session_arc.clone(), target_version).await?;
    }
    let session = session_arc.lock().await;
    Ok(!session.trajectory_dirty)
}

pub(super) async fn try_save_trajectory_legacy_with_intent(
    app: AppState,
    session_arc: Arc<AMutex<ChatSession>>,
    intent: TrajectoryCommitIntent,
) -> Result<bool, String> {
    let snapshot = {
        let session = session_arc.lock().await;
        if !session.trajectory_dirty {
            return Ok(true);
        }
        trajectory_snapshot_from_session(&session)
    };
    commit_trajectory_snapshot_for_session(&app.chat_ctx(), session_arc.clone(), snapshot, intent)
        .await?;
    Ok(!session_arc.lock().await.trajectory_dirty)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SessionTrajectoryCommitOutcome {
    Committed,
    SkippedStale,
    SkippedEphemeral,
}

pub(super) async fn commit_trajectory_snapshot_for_session(
    ctx: &ChatCtx,
    session_arc: Arc<AMutex<ChatSession>>,
    snapshot: TrajectorySnapshot,
    intent: TrajectoryCommitIntent,
) -> Result<SessionTrajectoryCommitOutcome, String> {
    if !intent.persists() {
        return Ok(SessionTrajectoryCommitOutcome::SkippedEphemeral);
    }
    let saved_version = snapshot.version;
    let chat_id = snapshot.chat_id.clone();
    let message_count = snapshot.messages.len() as u64;
    let span = perf_diagnostics::span(PerfComponent::TrajectoryCommit, Some(&chat_id), None);
    let save_mutex = {
        let session = session_arc.lock().await;
        session.trajectory_save_mutex.clone()
    };
    let save_mutex_wait_span = perf_diagnostics::is_enabled().then(|| {
        perf_diagnostics::span(PerfComponent::TrajectorySaveMutexWait, Some(&chat_id), None)
    });
    let _save_guard = save_mutex.lock().await;
    if let Some(span) = save_mutex_wait_span {
        span.finish(
            PerfOutcome::Success,
            None,
            Some(message_count),
            Some(saved_version),
            None,
        );
    }
    {
        let session = session_arc.lock().await;
        if !session.trajectory_commit_can_write(saved_version) {
            return Ok(SessionTrajectoryCommitOutcome::SkippedStale);
        }
    }

    let result = persist_trajectory_snapshot_with_intent_ctx(
        ctx,
        snapshot,
        TrajectoryCommitIntent::Required,
    )
    .await;
    span.finish(
        if result.is_ok() {
            PerfOutcome::Success
        } else {
            PerfOutcome::Failure
        },
        None,
        Some(message_count),
        Some(saved_version),
        None,
    );
    result.map_err(|error| format!("Failed to save trajectory for {}: {}", chat_id, error))?;

    let mut session = session_arc.lock().await;
    let committed = session.complete_trajectory_commit(saved_version);
    session.trajectory_commit_notify.notify_waiters();
    if committed {
        Ok(SessionTrajectoryCommitOutcome::Committed)
    } else {
        Ok(SessionTrajectoryCommitOutcome::SkippedStale)
    }
}

pub(super) async fn schedule_trajectory_writer(
    app: AppState,
    session_arc: Arc<AMutex<ChatSession>>,
    intent: TrajectoryCommitIntent,
) -> Result<u64, String> {
    if !trajectory_writer_rollout_enabled() {
        try_save_trajectory_legacy_with_intent(app, session_arc.clone(), intent).await?;
        return Ok(session_arc.lock().await.trajectory_committed_version);
    }
    let should_spawn = {
        let mut session = session_arc.lock().await;
        let target_version = session.trajectory_version;
        if !session.trajectory_dirty {
            return Ok(target_version);
        }
        if session.trajectory_save_in_flight {
            session.trajectory_save_queued = true;
            return Ok(target_version);
        }
        session.trajectory_save_in_flight = true;
        session.trajectory_save_queued = false;
        session.trajectory_save_error = None;
        target_version
    };
    let ctx = app.chat_ctx();
    let writer_session_arc = session_arc.clone();
    tokio::spawn(async move {
        loop {
            let snapshot = {
                let session = writer_session_arc.lock().await;
                if !session.trajectory_dirty {
                    None
                } else {
                    Some(trajectory_snapshot_from_session(&session))
                }
            };
            let Some(snapshot) = snapshot else {
                let mut session = writer_session_arc.lock().await;
                session.trajectory_save_in_flight = false;
                session.trajectory_save_queued = false;
                session.trajectory_commit_notify.notify_waiters();
                return;
            };

            let result = commit_trajectory_snapshot_for_session(
                &ctx,
                writer_session_arc.clone(),
                snapshot,
                TrajectoryCommitIntent::Checkpoint,
            )
            .await;
            let mut session = writer_session_arc.lock().await;
            match result {
                Ok(_) => {
                    session.trajectory_save_error = None;
                }
                Err(error) => {
                    warn!("{}", error);
                    session.trajectory_dirty = true;
                    session.trajectory_save_in_flight = false;
                    session.trajectory_save_queued = false;
                    session.trajectory_save_error = Some(error);
                    session.trajectory_commit_notify.notify_waiters();
                    return;
                }
            }
            if session.trajectory_dirty || session.trajectory_save_queued {
                session.trajectory_save_queued = false;
                continue;
            }
            session.trajectory_save_in_flight = false;
            session.trajectory_commit_notify.notify_waiters();
            return;
        }
    });
    if intent.requires_durability() {
        wait_for_trajectory_commit(session_arc, should_spawn).await?;
    }
    Ok(should_spawn)
}

pub(super) async fn wait_for_trajectory_commit(
    session_arc: Arc<AMutex<ChatSession>>,
    target_version: u64,
) -> Result<(), String> {
    let mut last_progress = None;
    let mut deadline = Instant::now() + TRAJECTORY_COMMIT_STALL_TIMEOUT;
    loop {
        let notified = {
            let session = session_arc.lock().await;
            if session.trajectory_committed_version >= target_version {
                return Ok(());
            }
            if let Some(error) = session.trajectory_save_error.clone() {
                return Err(error);
            }
            let progress = (
                session.trajectory_committed_version,
                session.trajectory_version,
            );
            if last_progress != Some(progress) {
                last_progress = Some(progress);
                deadline = Instant::now() + TRAJECTORY_COMMIT_STALL_TIMEOUT;
            }
            let mut notified = Box::pin(session.trajectory_commit_notify.clone().notified_owned());
            let already_notified = notified.as_mut().enable();
            drop(session);
            if already_notified {
                continue;
            }
            notified
        };
        if tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), notified)
            .await
            .is_err()
        {
            let session = session_arc.lock().await;
            return Err(format!(
                "Trajectory writer made no progress for {} ms while committing version {} \
                 (committed {})",
                TRAJECTORY_COMMIT_STALL_TIMEOUT.as_millis(),
                target_version,
                session.trajectory_committed_version
            ));
        }
    }
}

pub async fn flush_trajectory_for_session(
    app: AppState,
    session_arc: Arc<AMutex<ChatSession>>,
) -> Result<(), String> {
    try_save_trajectory_with_intent(app, session_arc, TrajectoryCommitIntent::Required)
        .await
        .map(|_| ())
}

pub async fn flush_all_trajectories(app: AppState) -> Result<(), Vec<String>> {
    flush_all_trajectories_with_timeout(app, TRAJECTORY_FLUSH_TIMEOUT).await
}

pub(super) async fn flush_all_trajectories_with_timeout(
    app: AppState,
    timeout: Duration,
) -> Result<(), Vec<String>> {
    let sessions: Vec<Arc<AMutex<ChatSession>>> = {
        let sessions = app.chat.sessions.read().await;
        sessions.values().cloned().collect()
    };
    let flushes = async move {
        let results = futures::future::join_all(
            sessions
                .into_iter()
                .map(|session_arc| flush_trajectory_for_session(app.clone(), session_arc)),
        )
        .await;
        let mut errors: Vec<String> = results.into_iter().filter_map(Result::err).collect();
        if let Err(mut detached_errors) = flush_detached_trajectory_writers().await {
            errors.append(&mut detached_errors);
        }
        errors
    };
    let errors = match tokio::time::timeout(timeout, flushes).await {
        Ok(errors) => errors,
        Err(_) => vec![format!(
            "trajectory flush timed out after {} ms",
            timeout.as_millis()
        )],
    };
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

pub fn maybe_save_trajectory_background(app: AppState, session_arc: Arc<AMutex<ChatSession>>) {
    maybe_save_trajectory_background_with_intent(
        app,
        session_arc,
        TrajectoryCommitIntent::Checkpoint,
    );
}

pub fn maybe_save_trajectory_background_with_intent(
    app: AppState,
    session_arc: Arc<AMutex<ChatSession>>,
    intent: TrajectoryCommitIntent,
) {
    if app.runtime.shutdown_flag.load(Ordering::Relaxed) || !intent.persists() {
        return;
    }
    tokio::spawn(async move {
        if let Err(error) = schedule_trajectory_writer(app, session_arc, intent).await {
            warn!("{}", error);
        }
    });
}

pub async fn maybe_save_trajectory(app: AppState, session_arc: Arc<AMutex<ChatSession>>) {
    maybe_save_trajectory_with_intent(app, session_arc, TrajectoryCommitIntent::Required).await;
}

pub async fn maybe_save_trajectory_with_intent(
    app: AppState,
    session_arc: Arc<AMutex<ChatSession>>,
    intent: TrajectoryCommitIntent,
) {
    if intent == TrajectoryCommitIntent::Checkpoint {
        maybe_save_trajectory_background_with_intent(app, session_arc, intent);
        return;
    }
    if let Err(e) = try_save_trajectory_with_intent(app, session_arc.clone(), intent).await {
        warn!("{}", e);
    }
}
