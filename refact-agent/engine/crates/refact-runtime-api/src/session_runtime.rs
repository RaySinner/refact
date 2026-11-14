//! Session lookup / restore shared by the chat HTTP handlers and the tools that
//! need a live session without going through the engine.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::Mutex as AMutex;
use tracing::{info, warn};

use refact_agents_core::types::{AgentListFilter, BackgroundAgentSummary};
use refact_chat_api::{ChatSession, SessionsMap, TrajectoryCommitIntent};

use crate::chat_ctx::ChatCtx;
use crate::trajectory_prefix::{ensure_frozen_prefix, first_system_prompt};

async fn migrate_legacy_frozen_prefix_on_open(
    ctx: ChatCtx,
    session_arc: Arc<AMutex<ChatSession>>,
) {
    let installed = {
        let mut session = session_arc.lock().await;
        let prefix = session.thread.frozen_request_prefix.as_ref();
        let needs_prefix = prefix.is_none();
        let needs_legacy_save = prefix
            .map(|prefix| prefix.tools_canonical.is_none() && prefix.system_prompt.is_some())
            .unwrap_or(false);
        if needs_prefix {
            let system_prompt = first_system_prompt(&session.messages);
            ensure_frozen_prefix(&mut session, system_prompt, None).is_some()
        } else if needs_legacy_save {
            session.increment_version();
            true
        } else {
            false
        }
    };
    if installed {
        ctx.trajectories
            .maybe_save_trajectory_with_intent(session_arc, TrajectoryCommitIntent::Required)
            .await;
    }
}

pub async fn get_or_create_session_with_trajectory(
    ctx: ChatCtx,
    sessions: &SessionsMap,
    chat_id: &str,
) -> Arc<AMutex<ChatSession>> {
    let maybe_existing = {
        let sessions_read = sessions.read().await;
        sessions_read.get(chat_id).cloned()
    };

    if let Some(session_arc) = maybe_existing {
        let is_closed = {
            let session = session_arc.lock().await;
            session.closed
        };
        if !is_closed {
            ctx.trajectories
                .refresh_session_from_trajectory_if_stale(chat_id, false)
                .await;
            return session_arc;
        }
        let mut sessions_write = sessions.write().await;
        if let Some(current) = sessions_write.get(chat_id) {
            if Arc::ptr_eq(current, &session_arc) {
                sessions_write.remove(chat_id);
            }
        }
    }

    let trajectory_events_tx = ctx.chat.trajectory_events_tx.clone();

    let (mut session, is_new, transition_repair_patch) = if let Some(mut loaded) =
        ctx.trajectories.load_trajectory_for_chat(chat_id).await
    {
        info!(
            "Loaded trajectory for chat {} with {} messages",
            chat_id,
            loaded.messages.len()
        );
        ctx.trajectories
            .apply_mode_defaults_to_thread(
                &mut loaded.thread,
                loaded.auto_approve_editing_tools_present,
                loaded.auto_approve_dangerous_commands_present,
            )
            .await;
        let transition_identity_repaired = loaded.transition_identity_repaired;
        let transition_repair_patch = transition_identity_repaired.then(|| loaded.repair_patch());
        let mut session = ChatSession::new_with_trajectory(
            chat_id.to_string(),
            loaded.messages,
            loaded.thread,
            loaded.created_at,
            loaded.wake_up_at,
            loaded.waiting_for_card_ids,
            loaded.goal,
        );
        if let Some(agent_id) = ctx.agents.find_agent_id_by_child_chat_id(chat_id).await {
            if ctx.agents.has_runtime(&agent_id).await {
                session.restore_pending_deliveries(Vec::new());
                session.runner_pending_deliveries = ctx.agents.pending_deliveries(&agent_id).await;
            } else {
                session.restore_pending_deliveries(loaded.pending_deliveries);
            }
        } else {
            session.restore_pending_deliveries(loaded.pending_deliveries);
        }
        session.goal_ledger = loaded.goal_ledger;
        session.goal_verification_blocked_until_ms = loaded.goal_verification_blocked_until_ms;
        if transition_identity_repaired {
            session.increment_version();
        }
        (session, false, transition_repair_patch)
    } else {
        let mut s = ChatSession::new(chat_id.to_string());
        s.increment_version();
        s.trajectory_dirty = false;
        (s, true, None)
    };

    let background_agents = ctx
        .agents
        .list_for_parent(chat_id, AgentListFilter::default())
        .await;
    session.upsert_background_agents(background_agents.iter().map(BackgroundAgentSummary::from));

    if is_new {
        session.thread.auto_enrichment_enabled = Some(true);
        if let Some(mode_config) = ctx
            .mode_config
            .get_mode_config(&session.thread.mode, None)
            .await
        {
            let defaults = &mode_config.thread_defaults;
            if let Some(v) = defaults.include_project_info {
                session.thread.include_project_info = v;
            }
            if let Some(v) = defaults.checkpoints_enabled {
                session.thread.checkpoints_enabled = v;
            }
            if let Some(v) = defaults.auto_approve_editing_tools {
                session.thread.auto_approve_editing_tools = v;
            }
            if let Some(v) = defaults.auto_approve_dangerous_commands {
                session.thread.auto_approve_dangerous_commands = v;
            }
        }
    }

    session.trajectory_events_tx = Some(trajectory_events_tx.clone());

    let (session_arc, inserted) = {
        let mut sessions_write = sessions.write().await;
        match sessions_write.entry(chat_id.to_string()) {
            std::collections::hash_map::Entry::Vacant(e) => {
                let arc = Arc::new(AMutex::new(session));
                e.insert(arc.clone());
                (arc, true)
            }
            std::collections::hash_map::Entry::Occupied(e) => (e.get().clone(), false),
        }
    };

    if inserted && is_new {
        let ctx_hook = ctx.clone();
        let chat_id_clone = chat_id.to_string();
        tokio::spawn(async move {
            let project_dir = ctx_hook.hooks.project_dir().await;
            let payload = refact_ext::hooks_payload::HookPayload {
                hook_event_name: "SessionStart".to_string(),
                session_id: chat_id_clone,
                project_dir,
                tool_name: None,
                tool_input: None,
                tool_output: None,
                user_prompt: None,
                extra: HashMap::new(),
            };
            ctx_hook
                .hooks
                .run_hooks(refact_ext::hooks::HookEvent::SessionStart, payload)
                .await;
        });
    }

    if inserted && !is_new {
        if transition_repair_patch.is_none() {
            migrate_legacy_frozen_prefix_on_open(ctx.clone(), session_arc.clone()).await;
        }
        if let Some(repair_patch) = transition_repair_patch {
            let repaired_version = {
                let session = session_arc.lock().await;
                session.trajectory_version
            };
            if let Err(e) = ctx
                .trajectories
                .persist_loaded_trajectory_repair_raw(&repair_patch)
                .await
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
    }

    session_arc
}

pub async fn try_restore_session_if_trajectory_exists(
    ctx: ChatCtx,
    sessions: &SessionsMap,
    chat_id: &str,
) -> bool {
    let maybe_existing = {
        let sessions_read = sessions.read().await;
        sessions_read.get(chat_id).cloned()
    };

    if let Some(session_arc) = maybe_existing {
        let is_closed = session_arc.lock().await.closed;
        if !is_closed {
            return true;
        }
        let mut sessions_write = sessions.write().await;
        if let Some(current) = sessions_write.get(chat_id) {
            if Arc::ptr_eq(current, &session_arc) {
                sessions_write.remove(chat_id);
            }
        }
    }

    if ctx
        .trajectories
        .load_trajectory_for_chat(chat_id)
        .await
        .is_none()
    {
        return false;
    }

    get_or_create_session_with_trajectory(ctx, sessions, chat_id).await;
    true
}
