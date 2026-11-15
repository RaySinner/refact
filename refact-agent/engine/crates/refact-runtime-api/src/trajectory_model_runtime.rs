use std::sync::Arc;

use refact_chat_api::types::{
    SessionState, ThreadParams, TrajectorySourceIdentity, TrajectorySourceIdentitySessionExt,
};
use refact_chat_api::SessionsMap;
use refact_worktrees::types::WorktreeMeta;

use crate::global_context::GlobalContext;

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

pub async fn get_session_runtime_for_trajectory_source(
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

pub async fn apply_mode_defaults_to_thread(
    gcx: Arc<GlobalContext>,
    thread: &mut ThreadParams,
    auto_approve_editing_present: bool,
    auto_approve_dangerous_present: bool,
) {
    if auto_approve_editing_present && auto_approve_dangerous_present {
        return;
    }
    if let Some(mode_config) =
        crate::customization_registry_gcx::get_mode_config(gcx.clone(), &thread.mode, None).await
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
