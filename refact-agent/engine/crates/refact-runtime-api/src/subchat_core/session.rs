use std::sync::Arc;
use tokio::sync::Mutex as AMutex;
use tracing::warn;
use refact_chat_api::{ChatMessage, ChatSession, SessionState, SubchatConfig, ThreadParams};
use refact_chat_history::trajectory_paths::validate_trajectory_id;
use refact_worktrees::service::WorktreeService;
use refact_worktrees::types::{WorktreeMeta, WorktreeReference};

use crate::app_state::AppState;
use crate::chat_ctx::ChatCtx;

use super::state::stateful_thread_from_config;

pub async fn parent_thread_worktree(ctx: &ChatCtx, parent_id: &str) -> Option<WorktreeMeta> {
    let sessions = { ctx.chat.sessions.clone() };
    let session_arc = {
        let sessions_read = sessions.read().await;
        sessions_read.get(parent_id).cloned()
    };
    if let Some(session_arc) = session_arc {
        return session_arc.lock().await.thread.worktree.clone();
    }

    validate_trajectory_id(parent_id).ok()?;
    ctx.trajectories
        .load_trajectory_for_chat(parent_id)
        .await
        .and_then(|loaded| loaded.thread.worktree)
}

pub async fn resolve_subchat_worktree(
    ctx: &ChatCtx,
    parent_id: Option<&str>,
    parent_worktree: Option<WorktreeMeta>,
) -> Option<WorktreeMeta> {
    match parent_worktree {
        Some(parent_worktree) => Some(parent_worktree),
        None => match parent_id {
            Some(parent_id) => parent_thread_worktree(ctx, parent_id).await,
            None => None,
        },
    }
}

pub fn worktree_reference_for_thread(
    chat_id: &str,
    thread: &ThreadParams,
) -> Option<WorktreeReference> {
    let worktree = thread.worktree.as_ref()?;
    let task_meta = thread.task_meta.as_ref();
    Some(WorktreeReference {
        kind: worktree.kind.clone(),
        chat_id: Some(chat_id.to_string()),
        task_id: task_meta.map(|meta| meta.task_id.clone()),
        card_id: task_meta.and_then(|meta| meta.card_id.clone()),
        agent_id: task_meta.and_then(|meta| meta.agent_id.clone()),
    })
}

pub async fn register_stateful_subchat_worktree(
    ctx: &ChatCtx,
    chat_id: &str,
    thread: &mut ThreadParams,
) {
    let Some(worktree) = thread.worktree.clone() else {
        return;
    };
    let Some(reference) = worktree_reference_for_thread(chat_id, thread) else {
        return;
    };

    let cache_dir = { ctx.paths.cache_dir.clone() };
    let service =
        match WorktreeService::new_async(cache_dir, worktree.source_workspace_root.clone()).await {
            Ok(service) => service,
            Err(e) => {
                warn!(
                    "Failed to resolve worktree service while registering subchat '{}': {}",
                    chat_id, e
                );
                return;
            }
        };

    match service.add_reference(&worktree.id, reference).await {
        Ok(view) => thread.worktree = Some(view.meta),
        Err(e) => warn!(
            "Failed to add worktree reference '{}' for subchat '{}': {}",
            worktree.id, chat_id, e
        ),
    }
}

pub async fn mirror_subchat_messages_into_session(
    app: &AppState,
    chat_id: &str,
    messages: &[ChatMessage],
) {
    let session_arc = {
        let sessions = app.chat.sessions.read().await;
        sessions.get(chat_id).cloned()
    };
    let Some(session_arc) = session_arc else {
        return;
    };
    let mut session = session_arc.lock().await;
    if session.closed {
        return;
    }
    if !session.is_runner_owned_subagent_view() && session.trajectory_dirty {
        return;
    }
    session.mirror_runner_messages(messages.to_vec());
}

pub async fn install_stateful_subchat_session(
    app: &AppState,
    chat_id: &str,
    config: &SubchatConfig,
    messages: &[ChatMessage],
) {
    let thread = stateful_thread_from_config(chat_id, config);
    let session_arc = {
        let mut sessions = app.chat.sessions.write().await;
        if let Some(session) = sessions.get(chat_id) {
            session.clone()
        } else {
            let session = ChatSession::new_with_trajectory(
                chat_id.to_string(),
                messages.to_vec(),
                thread.clone(),
                chrono::Utc::now().to_rfc3339(),
                None,
                Vec::new(),
                None,
            );
            let session = Arc::new(AMutex::new(session));
            sessions.insert(chat_id.to_string(), session.clone());
            session
        }
    };
    if config.background_agent_id.is_some() {
        let mut session = session_arc.lock().await;
        session.set_runtime_state(SessionState::Starting, None);
    }
}

pub async fn finish_stateful_subchat_session(
    app: &AppState,
    chat_id: &str,
    config: &SubchatConfig,
) {
    if !config.stateful || config.background_agent_id.is_none() {
        return;
    }
    let session = app.chat.sessions.read().await.get(chat_id).cloned();
    if let Some(session) = session {
        session
            .lock()
            .await
            .set_runtime_state(SessionState::Idle, None);
    }
}
