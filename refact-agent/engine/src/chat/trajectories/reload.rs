use std::path::PathBuf;
use std::sync::Arc;
use serde_json::json;
use tokio::sync::Mutex as AMutex;
use tracing::{info, warn};
use crate::chat::types::TrajectorySourceIdentitySessionExt;
#[cfg(test)]
use refact_runtime_api::GlobalContextCtx;

use crate::chat::ctx::ChatCtx;
use crate::chat::trajectory_index;
use crate::chat::types::{
    ChatSession, ExternalReloadPending, SessionState, ThreadParams, TrajectorySourceIdentity,
};
#[cfg(test)]
use crate::chat::types::TrajectoryCommitIntent;
#[cfg(test)]
use crate::global_context::GlobalContext;
#[cfg(test)]
use refact_chat_api::FrozenRequestPrefix;

use super::model::{LoadedTrajectory, TrajectoryRepairPatch};
#[cfg(test)]
use super::paths::find_trajectory_or_buddy_path;
use super::paths::validate_trajectory_id;
use super::watcher::trajectory_source_identity_from_path;
use super::writer::{atomic_write_json_with_tmp_path, unique_trajectory_tmp_path};

pub use refact_runtime_api::trajectory_prefix::{
    ensure_frozen_prefix, first_system_prompt, frozen_prefix_is_complete, new_frozen_request_prefix,
    parsed_rfc3339_utc, raw_frozen_prefix_has_tools, repair_transition_frozen_prefix,
};
pub use refact_runtime_api::trajectory_prefix::new_legacy_incomplete_frozen_request_prefix;

#[cfg(test)]
pub(super) async fn persist_frozen_prefix(
    gcx: Arc<GlobalContext>,
    chat_id: &str,
    frozen_request_prefix: FrozenRequestPrefix,
) -> Result<(), String> {
    validate_trajectory_id(chat_id).map_err(|e| e.message)?;
    let Some(file_path) = find_trajectory_or_buddy_path(&gcx.chat_ctx(gcx.clone()), chat_id).await else {
        return Ok(());
    };

    let content = tokio::fs::read_to_string(&file_path)
        .await
        .map_err(|e| format!("Failed to read trajectory: {}", e))?;
    let json_result = tokio::task::spawn_blocking(move || {
        let mut trajectory = serde_json::from_str::<serde_json::Value>(&content)
            .map_err(|e| format!("Failed to parse trajectory: {}", e))?;
        if trajectory
            .get("frozen_request_prefix")
            .is_some_and(|value| !value.is_null())
        {
            return Ok(None);
        }
        trajectory["frozen_request_prefix"] =
            serde_json::to_value(frozen_request_prefix).map_err(|e| e.to_string())?;
        trajectory["updated_at"] = serde_json::Value::String(chrono::Utc::now().to_rfc3339());
        serde_json::to_string_pretty(&trajectory)
            .map(Some)
            .map_err(|e| format!("Failed to serialize trajectory: {}", e))
    })
    .await
    .map_err(|e| format!("Trajectory prefix persistence task failed: {}", e))??;
    let Some(json) = json_result else {
        return Ok(());
    };

    let tmp_path = unique_trajectory_tmp_path(&file_path);
    atomic_write_json_with_tmp_path(
        &file_path,
        &tmp_path,
        Ok(json),
        Some("Failed to write trajectory"),
        Some(TrajectorySourceIdentity::Normal),
    )
    .await
}

#[cfg(test)]
pub(super) fn trajectory_commit_contract() -> [(&'static str, TrajectoryCommitIntent); 7] {
    [
        (
            "explicit acknowledged mutations",
            TrajectoryCommitIntent::Required,
        ),
        (
            "branch handoff mode transition",
            TrajectoryCommitIntent::Required,
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
}

#[cfg(test)]
pub(crate) fn trajectory_commit_contract_for_test(boundary: &str) -> TrajectoryCommitIntent {
    trajectory_commit_contract()
        .into_iter()
        .find_map(|(name, intent)| (name == boundary).then_some(intent))
        .expect("known trajectory commit boundary")
}

pub(crate) async fn persist_loaded_trajectory_repair_raw(
    ctx: &ChatCtx,
    repair: &TrajectoryRepairPatch,
) -> Result<String, String> {
    let chat_id = &repair.chat_id;
    validate_trajectory_id(chat_id).map_err(|e| e.message)?;
    let file_path = &repair.source_path;
    if !ctx
        .trajectories
        .trajectory_source_path_is_allowed(file_path)
        .await
    {
        return Err(format!(
            "Trajectory source path is not in an approved trajectory root: {}",
            file_path.display()
        ));
    }
    let content = tokio::fs::read_to_string(&file_path)
        .await
        .map_err(|e| format!("Failed to read trajectory: {}", e))?;
    let mut trajectory = serde_json::from_str::<serde_json::Value>(&content)
        .map_err(|e| format!("Failed to parse trajectory: {}", e))?;
    let trajectory_object = trajectory
        .as_object_mut()
        .ok_or_else(|| "Trajectory JSON root must be an object".to_string())?;
    let current_chat_id = trajectory_object.get("id").and_then(|value| value.as_str());
    if current_chat_id != Some(chat_id.as_str()) {
        return Err(format!(
            "Trajectory source id mismatch for repair: expected {}, found {}",
            chat_id,
            current_chat_id.unwrap_or("<missing>")
        ));
    }

    trajectory_object.remove("previous_response_id");
    trajectory_object.remove("claude_code_identity");
    match &repair.frozen_request_prefix {
        Some(frozen_request_prefix) => {
            trajectory_object.insert(
                "frozen_request_prefix".to_string(),
                serde_json::to_value(frozen_request_prefix).map_err(|e| e.to_string())?,
            );
        }
        None => {
            trajectory_object.remove("frozen_request_prefix");
        }
    }
    trajectory_object.insert(
        "created_at".to_string(),
        serde_json::Value::String(repair.created_at.clone()),
    );
    trajectory_object.insert(
        "auto_approve_editing_tools".to_string(),
        json!(repair.auto_approve_editing_tools),
    );
    trajectory_object.insert(
        "auto_approve_dangerous_commands".to_string(),
        json!(repair.auto_approve_dangerous_commands),
    );
    let updated_at = chrono::Utc::now().to_rfc3339();
    trajectory_object.insert(
        "updated_at".to_string(),
        serde_json::Value::String(updated_at.clone()),
    );

    let tmp_path = unique_trajectory_tmp_path(file_path);
    let (trajectory, json_result) = tokio::task::spawn_blocking(move || {
        let json_result = serde_json::to_string_pretty(&trajectory)
            .map_err(|e| format!("Failed to serialize trajectory: {}", e));
        (trajectory, json_result)
    })
    .await
    .map_err(|e| format!("Trajectory repair serialization task failed: {}", e))?;
    atomic_write_json_with_tmp_path(
        file_path,
        &tmp_path,
        json_result,
        Some("Failed to write trajectory"),
        Some(trajectory_index::source_from_hint_or_value(
            &trajectory,
            None,
        )),
    )
    .await?;
    if let Some(dir) = ctx
        .trajectories
        .index_dir_for_trajectory_file(file_path)
        .await
    {
        let source_hint = Some(trajectory_index::source_from_hint_or_value(
            &trajectory,
            None,
        ));
        trajectory_index::upsert_trajectory_index_entry_from_owned_value_with_rollout(
            &ctx.chat.trajectory_index_coordinator,
            &dir,
            file_path,
            trajectory,
            source_hint,
        )
        .await?;
    }
    Ok(updated_at)
}

pub(super) async fn persist_loaded_trajectory_repair(
    ctx: &ChatCtx,
    mut loaded: LoadedTrajectory,
) {
    let chat_id = loaded.thread.id.clone();
    ctx.trajectories
        .apply_mode_defaults_to_thread(
            &mut loaded.thread,
            loaded.auto_approve_editing_tools_present,
            loaded.auto_approve_dangerous_commands_present,
        )
        .await;
    if let Err(e) = persist_loaded_trajectory_repair_raw(ctx, &loaded.repair_patch()).await {
        warn!(
            "Failed to persist repaired trajectory for {}: {}",
            chat_id, e
        );
    }
}

pub(super) fn loaded_trajectory_source(
    loaded: &LoadedTrajectory,
    task_roots: &[PathBuf],
) -> TrajectorySourceIdentity {
    let loaded_source = TrajectorySourceIdentity::from_session_parts(&loaded.thread);
    let path_source = trajectory_source_identity_from_path(&loaded.source_path, task_roots);
    if matches!(&loaded_source, TrajectorySourceIdentity::Normal)
        && !matches!(&path_source, TrajectorySourceIdentity::Normal)
    {
        path_source
    } else {
        loaded_source
    }
}

pub(super) fn external_delete_matches_session(
    deleted_source: Option<&TrajectorySourceIdentity>,
    session: &ChatSession,
) -> bool {
    if is_active_buddy_session(session) {
        return false;
    }
    match deleted_source {
        Some(source) => source.matches_session_for_delete(session),
        None => TrajectorySourceIdentity::Normal.matches_session(session),
    }
}

pub(super) fn can_apply_external_reload(session: &ChatSession) -> bool {
    session.runtime.state == SessionState::Idle
        && !session.trajectory_dirty
        && session.pending_deliveries.is_empty()
        && session.runner_pending_deliveries.is_empty()
}

pub(super) fn pending_reload_source_matches_session(
    pending: &ExternalReloadPending,
    session: &ChatSession,
) -> bool {
    match pending {
        ExternalReloadPending::Update { source } | ExternalReloadPending::Delete { source } => {
            source.matches_session_for_delete(session)
        }
    }
}

pub(super) fn is_active_buddy_session(session: &ChatSession) -> bool {
    session.thread.buddy_meta.is_some()
}

pub(super) fn apply_external_delete_to_session(session: &mut ChatSession, chat_id: &str) {
    session.release_turn_only_state();
    session.messages.clear();
    session.thread = ThreadParams {
        id: chat_id.to_string(),
        ..Default::default()
    };
    session.created_at = chrono::Utc::now().to_rfc3339();
    session.wake_up_at = None;
    session.waiting_for_card_ids.clear();
    session.goal_ledger.clear();
    session.goal_verification_blocked_until_ms = None;
    session.set_goal_projection(None);
    session.reset_compaction_runtime_state();
    session.external_reload_pending = None;
    let snapshot = session.snapshot();
    session.emit(snapshot);
}

pub(super) fn apply_loaded_external_update_to_session(
    session: &mut ChatSession,
    loaded: LoadedTrajectory,
    transition_identity_repaired: bool,
) -> Option<u64> {
    session.release_turn_only_state();
    session.messages = loaded.messages;
    session.restore_pending_deliveries(loaded.pending_deliveries);
    session.thread = loaded.thread;
    session.reset_compaction_runtime_state();
    session.goal_ledger = loaded.goal_ledger;
    session.goal_verification_blocked_until_ms = loaded.goal_verification_blocked_until_ms;
    session.set_goal_projection(loaded.goal);
    session.created_at = loaded.created_at;
    session.wake_up_at = loaded.wake_up_at;
    session.waiting_for_card_ids = loaded.waiting_for_card_ids;
    session.external_reload_pending = None;
    if transition_identity_repaired {
        session.increment_version();
    }
    let snapshot = session.snapshot();
    session.emit(snapshot);
    transition_identity_repaired.then_some(session.trajectory_version)
}
pub(super) fn session_is_passive_child_view(session: &ChatSession) -> bool {
    session.thread.buddy_meta.is_some()
        || (session.thread.parent_id.is_some()
            && session.thread.link_type.as_deref().map_or(true, |lt| {
                !matches!(lt, "handoff" | "mode_transition" | "branch")
            }))
}

pub(super) fn session_is_stale_refresh_candidate(
    session: &ChatSession,
    allow_replacing_messages: bool,
) -> bool {
    if session.closed {
        return false;
    }
    if !can_apply_external_reload(session) {
        return false;
    }
    if !session.command_queue.is_empty() || session.draft_message.is_some() {
        return false;
    }
    if session
        .queue_processor_running
        .load(std::sync::atomic::Ordering::SeqCst)
    {
        return false;
    }
    if !allow_replacing_messages
        && !session_is_passive_child_view(session)
        && !session.messages.is_empty()
    {
        return false;
    }
    true
}

pub async fn refresh_session_from_trajectory_if_stale(
    ctx: &ChatCtx,
    chat_id: &str,
    allow_replacing_messages: bool,
) -> bool {
    let session_arc = {
        let sessions_read = ctx.chat.sessions.read().await;
        sessions_read.get(chat_id).cloned()
    };
    let Some(session_arc) = session_arc else {
        return false;
    };
    {
        let session = session_arc.lock().await;
        if !session_is_stale_refresh_candidate(&session, allow_replacing_messages) {
            return false;
        }
    }
    let Some(mut loaded) = ctx.trajectories.load_trajectory_for_chat(chat_id).await else {
        return false;
    };
    if loaded.messages.is_empty() {
        return false;
    }
    ctx.trajectories
        .apply_mode_defaults_to_thread(
            &mut loaded.thread,
            loaded.auto_approve_editing_tools_present,
            loaded.auto_approve_dangerous_commands_present,
        )
        .await;
    let transition_identity_repaired = loaded.transition_identity_repaired;
    let mut session = session_arc.lock().await;
    if !session_is_stale_refresh_candidate(&session, allow_replacing_messages) {
        return false;
    }
    let session_has_identity = !session.messages.is_empty()
        || session.thread.task_meta.is_some()
        || session.thread.buddy_meta.is_some();
    if session_has_identity {
        let loaded_source = TrajectorySourceIdentity::from_session_parts(&loaded.thread);
        if !loaded_source.matches_session(&session) {
            return false;
        }
    }
    info!(
        "Refreshing stale session {} from trajectory ({} messages)",
        chat_id,
        loaded.messages.len()
    );
    apply_loaded_external_update_to_session(&mut session, loaded, transition_identity_repaired);
    true
}

pub(super) async fn apply_loaded_external_update_with_repair(
    ctx: &ChatCtx,
    session_arc: Arc<AMutex<ChatSession>>,
    chat_id: &str,
    mut loaded: LoadedTrajectory,
    expected_pending: Option<ExternalReloadPending>,
    pending_if_not_reloadable: Option<ExternalReloadPending>,
    log_message: &str,
) -> bool {
    let loaded_source = TrajectorySourceIdentity::from_session_parts(&loaded.thread);
    let transition_identity_repaired = loaded.transition_identity_repaired;
    ctx.trajectories
        .apply_mode_defaults_to_thread(
            &mut loaded.thread,
            loaded.auto_approve_editing_tools_present,
            loaded.auto_approve_dangerous_commands_present,
        )
        .await;
    let repair_patch = loaded.repair_patch();
    let mut skipped_active_buddy = false;
    let mut skipped_source_mismatch = false;
    let repaired_version = {
        let mut session = session_arc.lock().await;
        if let Some(expected) = expected_pending {
            if session.external_reload_pending != Some(expected) {
                return false;
            }
        }
        if !loaded_source.matches_session(&session) {
            skipped_source_mismatch = true;
            None
        } else if is_active_buddy_session(&session) {
            skipped_active_buddy = true;
            None
        } else if !can_apply_external_reload(&session) {
            if let Some(pending) = pending_if_not_reloadable {
                session.external_reload_pending = Some(pending);
            }
            return false;
        } else {
            info!("{}", log_message);
            apply_loaded_external_update_to_session(
                &mut session,
                loaded,
                transition_identity_repaired,
            )
        }
    };
    if skipped_active_buddy || skipped_source_mismatch {
        if transition_identity_repaired {
            if let Err(e) = persist_loaded_trajectory_repair_raw(ctx, &repair_patch).await {
                warn!(
                    "Failed to persist repaired trajectory for {}: {}",
                    chat_id, e
                );
            }
        }
        return false;
    }
    if let Some(repaired_version) = repaired_version {
        if let Err(e) = persist_loaded_trajectory_repair_raw(ctx, &repair_patch).await {
            warn!(
                "Failed to persist repaired trajectory for {}: {}",
                chat_id, e
            );
        } else {
            let mut session = session_arc.lock().await;
            session.complete_trajectory_commit(repaired_version);
        }
    }
    true
}

pub(super) enum ExternalDeleteRevalidationOutcome {
    Updated {
        loaded: LoadedTrajectory,
        applied_to_session: bool,
    },
    Deleted {
        applied_to_session: bool,
    },
    NoopStalePending,
}

pub(super) async fn apply_external_delete_with_revalidation(
    ctx: &ChatCtx,
    session_arc: Arc<AMutex<ChatSession>>,
    chat_id: &str,
    expected_pending: Option<ExternalReloadPending>,
    deleted_source: Option<TrajectorySourceIdentity>,
) -> ExternalDeleteRevalidationOutcome {
    let loaded = ctx.trajectories.load_generic_trajectory_for_chat(chat_id).await;

    if let Some(mut loaded) = loaded {
        let task_roots = ctx.trajectories.get_all_task_roots().await;
        let loaded_source = loaded_trajectory_source(&loaded, &task_roots);
        let transition_identity_repaired = loaded.transition_identity_repaired;
        ctx.trajectories
            .apply_mode_defaults_to_thread(
                &mut loaded.thread,
                loaded.auto_approve_editing_tools_present,
                loaded.auto_approve_dangerous_commands_present,
            )
            .await;
        let repair_patch = loaded.repair_patch();
        let outcome_loaded = loaded.clone();
        let same_source_loaded = match deleted_source.as_ref() {
            Some(source) if loaded_source != *source => {
                let mut loaded =
                    ctx.trajectories
                        .load_generic_trajectory_for_chat_matching_source(chat_id, source)
                        .await;
                if let Some(loaded) = loaded.as_mut() {
                    ctx.trajectories
                        .apply_mode_defaults_to_thread(
                            &mut loaded.thread,
                            loaded.auto_approve_editing_tools_present,
                            loaded.auto_approve_dangerous_commands_present,
                        )
                        .await;
                }
                loaded
            }
            _ => None,
        };
        let same_source_repair_patch = same_source_loaded
            .as_ref()
            .map(LoadedTrajectory::repair_patch);
        let same_source_transition_identity_repaired = same_source_loaded
            .as_ref()
            .is_some_and(|loaded| loaded.transition_identity_repaired);
        let (repaired_version, applied_to_session) = {
            let mut session = session_arc.lock().await;
            if let Some(expected) = expected_pending {
                if session.external_reload_pending != Some(expected) {
                    return ExternalDeleteRevalidationOutcome::NoopStalePending;
                }
            }
            let deleted_matches_session =
                external_delete_matches_session(deleted_source.as_ref(), &session);
            let fallback_matches_session = loaded_source.matches_session(&session);
            if !(deleted_matches_session || fallback_matches_session) {
                drop(session);
                if transition_identity_repaired {
                    if let Err(e) =
                        persist_loaded_trajectory_repair_raw(ctx, &repair_patch).await
                    {
                        warn!(
                            "Failed to persist repaired trajectory for {}: {}",
                            chat_id, e
                        );
                    }
                }
                return ExternalDeleteRevalidationOutcome::Updated {
                    loaded: outcome_loaded,
                    applied_to_session: false,
                };
            }
            if deleted_matches_session && !fallback_matches_session {
                if let Some(same_source_loaded) = same_source_loaded {
                    if !can_apply_external_reload(&session) {
                        let pending_source = TrajectorySourceIdentity::from_session(&session);
                        session.external_reload_pending =
                            Some(ExternalReloadPending::delete(pending_source));
                        return ExternalDeleteRevalidationOutcome::Updated {
                            loaded: outcome_loaded,
                            applied_to_session: false,
                        };
                    }
                    info!(
                        "Reloading same-source trajectory for {} after delete revalidation",
                        chat_id
                    );
                    let repaired_version = apply_loaded_external_update_to_session(
                        &mut session,
                        same_source_loaded,
                        same_source_transition_identity_repaired,
                    );
                    (repaired_version, true)
                } else {
                    if !can_apply_external_reload(&session) {
                        let pending_source = TrajectorySourceIdentity::from_session(&session);
                        session.external_reload_pending =
                            Some(ExternalReloadPending::delete(pending_source));
                        return ExternalDeleteRevalidationOutcome::Updated {
                            loaded: outcome_loaded,
                            applied_to_session: false,
                        };
                    }
                    info!(
                        "Clearing trajectory for {} after same-source delete with fallback mismatch",
                        chat_id
                    );
                    apply_external_delete_to_session(&mut session, chat_id);
                    (None, true)
                }
            } else if !can_apply_external_reload(&session) {
                let pending_source = TrajectorySourceIdentity::from_session(&session);
                session.external_reload_pending =
                    Some(ExternalReloadPending::delete(pending_source));
                return ExternalDeleteRevalidationOutcome::Updated {
                    loaded: outcome_loaded,
                    applied_to_session: false,
                };
            } else {
                info!(
                    "Reloading trajectory for {} after delete revalidation",
                    chat_id
                );
                let repaired_version = apply_loaded_external_update_to_session(
                    &mut session,
                    loaded,
                    transition_identity_repaired,
                );
                (repaired_version, true)
            }
        };
        if let Some(repaired_version) = repaired_version {
            let applied_repair_patch = same_source_repair_patch.as_ref().unwrap_or(&repair_patch);
            if let Err(e) =
                persist_loaded_trajectory_repair_raw(ctx, applied_repair_patch).await
            {
                warn!(
                    "Failed to persist repaired trajectory for {}: {}",
                    chat_id, e
                );
            } else {
                let mut session = session_arc.lock().await;
                session.complete_trajectory_commit(repaired_version);
            }
        }
        if same_source_transition_identity_repaired {
            if let Some(repair_patch) = same_source_repair_patch.as_ref() {
                if let Err(e) =
                    persist_loaded_trajectory_repair_raw(ctx, repair_patch).await
                {
                    warn!(
                        "Failed to persist repaired trajectory for {}: {}",
                        chat_id, e
                    );
                }
            }
        }
        if transition_identity_repaired {
            if let Err(e) = persist_loaded_trajectory_repair_raw(ctx, &repair_patch).await {
                warn!(
                    "Failed to persist repaired trajectory for {}: {}",
                    chat_id, e
                );
            }
        }
        return ExternalDeleteRevalidationOutcome::Updated {
            loaded: outcome_loaded,
            applied_to_session,
        };
    }

    let mut session = session_arc.lock().await;
    if let Some(expected) = expected_pending {
        if session.external_reload_pending != Some(expected) {
            return ExternalDeleteRevalidationOutcome::NoopStalePending;
        }
    }
    if !external_delete_matches_session(deleted_source.as_ref(), &session) {
        return ExternalDeleteRevalidationOutcome::Deleted {
            applied_to_session: false,
        };
    }
    if !can_apply_external_reload(&session) {
        let pending_source = TrajectorySourceIdentity::from_session(&session);
        session.external_reload_pending = Some(ExternalReloadPending::delete(pending_source));
        return ExternalDeleteRevalidationOutcome::Deleted {
            applied_to_session: false,
        };
    }
    info!("Trajectory file removed externally for {}", chat_id);
    apply_external_delete_to_session(&mut session, chat_id);
    ExternalDeleteRevalidationOutcome::Deleted {
        applied_to_session: true,
    }
}

pub async fn check_external_reload_pending(
    ctx: &ChatCtx,
    session_arc: Arc<AMutex<ChatSession>>,
) {
    let (chat_id, pending) = {
        let mut session = session_arc.lock().await;
        if is_active_buddy_session(&session) {
            return;
        }
        let pending = if can_apply_external_reload(&session) {
            session.external_reload_pending.clone()
        } else {
            None
        };
        if let Some(pending) = pending.as_ref() {
            if !pending_reload_source_matches_session(pending, &session) {
                warn!(
                    "Clearing pending external reload for {} because pending source no longer matches active session",
                    session.chat_id
                );
                session.external_reload_pending = None;
                return;
            }
        }
        (session.chat_id.clone(), pending)
    };
    match pending {
        Some(ExternalReloadPending::Delete { source }) => {
            let expected_pending = ExternalReloadPending::delete(source.clone());
            apply_external_delete_with_revalidation(
                ctx,
                session_arc.clone(),
                &chat_id,
                Some(expected_pending),
                Some(source),
            )
            .await;
        }
        Some(ExternalReloadPending::Update { source }) => {
            let expected_pending = ExternalReloadPending::update(source.clone());
            if let Some(loaded) = ctx
                .trajectories
                .load_generic_trajectory_for_chat_matching_source(&chat_id, &source)
                .await
            {
                apply_loaded_external_update_with_repair(
                    ctx,
                    session_arc.clone(),
                    &chat_id,
                    loaded,
                    Some(expected_pending),
                    None,
                    &format!("Applying pending external reload for {}", chat_id),
                )
                .await;
            } else {
                let mut session = session_arc.lock().await;
                if can_apply_external_reload(&session)
                    && session.external_reload_pending == Some(expected_pending)
                {
                    warn!(
                        "Clearing pending external reload for {} because matching trajectory source is missing",
                        chat_id
                    );
                    session.external_reload_pending = None;
                }
            }
        }
        None => {}
    }
}
