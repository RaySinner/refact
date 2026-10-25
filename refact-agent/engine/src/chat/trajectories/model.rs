use std::sync::Arc;

use crate::call_validation::ChatMessage;
use crate::chat::perf_diagnostics::{self, PerfComponent, PerfOutcome};
use crate::chat::session::has_displayable_assistant_content;
use crate::chat::types::{
    ChatSession, SessionState, ThreadParams, TrajectorySourceIdentity,
    TrajectorySourceIdentitySessionExt,
};
use crate::chat::SessionsMap;
use crate::global_context::GlobalContext;
use crate::worktrees::types::WorktreeMeta;
use refact_chat_history::trajectory_snapshot::TrajectorySnapshot;

pub use refact_chat_history::trajectory_model::*;

pub async fn get_session_state_for_chat(
    sessions: &SessionsMap,
    chat_id: &str,
) -> (String, Option<String>) {
    let session_arc = sessions.read().await.get(chat_id).cloned();
    match session_arc {
        Some(arc) => {
            let session = arc.lock().await;
            (
                session.runtime.state.to_string(),
                session.runtime.error.clone(),
            )
        }
        None => (SessionState::Idle.to_string(), None),
    }
}

pub(super) async fn get_session_runtime_for_trajectory_source(
    sessions: &SessionsMap,
    chat_id: &str,
    source: &TrajectorySourceIdentity,
) -> (String, Option<String>, Option<WorktreeMeta>) {
    let session_arc = sessions.read().await.get(chat_id).cloned();
    match session_arc {
        Some(arc) => {
            let session = arc.lock().await;
            if !source.emits_generic_event() || !source.matches_session(&session) {
                return (SessionState::Idle.to_string(), None, None);
            }
            (
                session.runtime.state.to_string(),
                session.runtime.error.clone(),
                session.thread.worktree.clone(),
            )
        }
        None => (SessionState::Idle.to_string(), None, None),
    }
}

pub(crate) fn trajectory_snapshot_from_session(session: &ChatSession) -> TrajectorySnapshot {
    let span = perf_diagnostics::span(
        PerfComponent::TrajectorySnapshot,
        Some(&session.chat_id),
        None,
    );
    let messages = session
        .messages
        .iter()
        .filter(|message| message.role != "assistant" || has_displayable_assistant_content(message))
        .cloned()
        .collect();

    let mut snapshot = TrajectorySnapshot::from_thread_parts(
        session.chat_id.clone(),
        &session.thread,
        messages,
        session.created_at.clone(),
        session.trajectory_version,
    );
    snapshot.wake_up_at = session.wake_up_at;
    snapshot.waiting_for_card_ids = session.waiting_for_card_ids.clone();
    snapshot.goal = session.goal.clone();
    snapshot.goal_ledger = session.goal_ledger.clone();
    snapshot.goal_verification_blocked_until_ms = session.goal_verification_blocked_until_ms;
    snapshot.pending_deliveries = session.pending_deliveries_for_snapshot();
    span.finish(
        PerfOutcome::Success,
        None,
        Some(snapshot.messages.len() as u64),
        Some(session.trajectory_version),
        Some(session.command_queue.len() as u64),
    );
    snapshot
}

pub async fn apply_mode_defaults_to_thread(
    gcx: Arc<GlobalContext>,
    thread: &mut ThreadParams,
    auto_approve_editing_present: bool,
    auto_approve_dangerous_present: bool,
) {
    if auto_approve_editing_present && auto_approve_dangerous_present {
        return;
    }
    if let Some(mode_config) = crate::yaml_configs::customization_registry::get_mode_config(
        gcx.clone(),
        &thread.mode,
        None,
    )
    .await
    {
        let defaults = &mode_config.thread_defaults;
        if !auto_approve_editing_present {
            if let Some(v) = defaults.auto_approve_editing_tools {
                thread.auto_approve_editing_tools = v;
            }
        }
        if !auto_approve_dangerous_present {
            if let Some(v) = defaults.auto_approve_dangerous_commands {
                thread.auto_approve_dangerous_commands = v;
            }
        }
    }
}
