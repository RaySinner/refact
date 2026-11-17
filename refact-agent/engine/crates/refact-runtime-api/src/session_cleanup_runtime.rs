//! Session cleanup, close and snapshot-with-agents logic.
//!
//! Every input is a `ChatCtx` handle (`ChatServices`, `TrajectoryStore`,
//! `HookRunner`, `BackgroundAgentRegistry`), so the whole file is crate-typed.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Instant;

use tokio::sync::Mutex as AMutex;
use tracing::{info, warn};

use refact_agents_core::types::{AgentListFilter, BackgroundAgentSummary};
use refact_chat_api::session::should_replace_background_agent;
use refact_chat_api::types::{
    session_cleanup_interval, ChatEvent, ChatSession, PendingDelivery, SessionState,
    TrajectoryCommitIntent,
};
use refact_chat_api::SessionsMap;
use refact_exec::{ExecMode, ExecProcessFilter, ExecProcessSnapshot, ExecStatusKind};
use refact_ext::hooks::HookEvent;

use crate::chat_ctx::ChatCtx;
use crate::ext_hooks_runner::HookPayload;
use crate::integrations::browser_runtime::browser_snapshot_for_chat;

fn background_process_cleanup_modes(include_services: bool) -> Vec<ExecMode> {
    if include_services {
        vec![ExecMode::Background, ExecMode::Service]
    } else {
        vec![ExecMode::Background]
    }
}

pub async fn clean_background_processes_for_chat(
    ctx: ChatCtx,
    chat_id: &str,
    include_services: bool,
) -> Result<Vec<ExecProcessSnapshot>, String> {
    let mut killed = Vec::new();
    for mode in background_process_cleanup_modes(include_services) {
        for status in [ExecStatusKind::Starting, ExecStatusKind::Running] {
            killed.extend(
                ctx.runtime
                    .exec_registry
                    .remove_by_owner(ExecProcessFilter {
                        chat_id: Some(chat_id.to_string()),
                        mode: Some(mode.clone()),
                        status: Some(status),
                        ..ExecProcessFilter::default()
                    })
                    .await?,
            );
        }
    }
    killed.sort_by(|a, b| a.meta.process_id.as_str().cmp(b.meta.process_id.as_str()));
    Ok(killed)
}

pub fn snapshot_with_agents(
    ctx: ChatCtx,
    session: &ChatSession,
) -> impl std::future::Future<Output = (ChatEvent, Vec<BackgroundAgentSummary>)> + Send + 'static {
    let chat_id = session.chat_id.clone();
    let base_background_agents: HashMap<String, BackgroundAgentSummary> =
        session.background_agents.clone();
    let mut snapshot = session.snapshot();
    async move {
        let browser = browser_snapshot_for_chat(&ctx, &chat_id).await;
        let mut background_agents = base_background_agents;
        let agents = ctx
            .agents
            .list_for_parent(&chat_id, AgentListFilter::default())
            .await;
        for agent in agents.iter().map(BackgroundAgentSummary::from) {
            if should_replace_background_agent(background_agents.get(&agent.agent_id), &agent) {
                background_agents.insert(agent.agent_id.clone(), agent);
            }
        }
        let mut background_agents: Vec<_> = background_agents.into_values().collect();
        background_agents.sort_by(|a, b| {
            b.change_seq
                .cmp(&a.change_seq)
                .then(a.agent_id.cmp(&b.agent_id))
        });
        if let ChatEvent::Snapshot {
            background_agents: snapshot_background_agents,
            browser: snapshot_browser,
            ..
        } = &mut snapshot
        {
            *snapshot_background_agents = background_agents.clone();
            *snapshot_browser = browser;
        }
        (snapshot, background_agents)
    }
}

pub async fn close_all_chat_sessions(ctx: ChatCtx) {
    let sessions = ctx.chat.sessions.clone();
    let session_arcs: Vec<Arc<AMutex<ChatSession>>> = {
        let sessions_read = sessions.read().await;
        sessions_read.values().cloned().collect()
    };
    let flushes = futures::future::join_all(
        session_arcs
            .iter()
            .cloned()
            .map(|session_arc| ctx.trajectories.flush_trajectory_for_session(session_arc)),
    );
    match tokio::time::timeout(std::time::Duration::from_secs(5), flushes).await {
        Ok(results) => {
            for error in results.into_iter().filter_map(Result::err) {
                warn!(
                    "close_all_chat_sessions: trajectory flush failed: {}",
                    error
                );
            }
        }
        Err(_) => warn!("close_all_chat_sessions: trajectory flush timed out"),
    }
    for session_arc in session_arcs {
        let lock_result =
            tokio::time::timeout(std::time::Duration::from_millis(500), session_arc.lock()).await;
        match lock_result {
            Ok(mut session) => {
                session.abort_stream();
                session.close_event_channel();
                session.queue_notify.notify_waiters();
            }
            Err(_) => {
                warn!(
                    "close_all_chat_sessions: session lock timeout, notifying waiters without lock"
                );
                session_arc
                    .try_lock()
                    .map(|s| s.queue_notify.notify_waiters())
                    .ok();
            }
        }
    }
}

pub async fn cleanup_idle_session(
    ctx: ChatCtx,
    sessions: &SessionsMap,
    chat_id: &str,
    session_arc: Arc<AMutex<ChatSession>>,
) -> bool {
    if !save_idle_session_for_cleanup(ctx, session_arc.clone()).await {
        return false;
    }
    remove_idle_session_if_safe(sessions, chat_id, session_arc).await
}

async fn save_idle_session_for_cleanup(
    ctx: ChatCtx,
    session_arc: Arc<AMutex<ChatSession>>,
) -> bool {
    match ctx
        .trajectories
        .try_save_trajectory_with_intent(session_arc, TrajectoryCommitIntent::Required)
        .await
    {
        Ok(saved) => saved,
        Err(error) => {
            warn!("{}", error);
            false
        }
    }
}

pub async fn remove_idle_session_if_safe(
    sessions: &SessionsMap,
    chat_id: &str,
    session_arc: Arc<AMutex<ChatSession>>,
) -> bool {
    const MAX_REMOVAL_STRONG_REFS: usize = 3;

    let mut sessions_write = sessions.write().await;
    let current_matches = sessions_write
        .get(chat_id)
        .map_or(false, |current| Arc::ptr_eq(current, &session_arc));
    if !current_matches {
        return false;
    }

    let removal_snapshot = {
        let Ok(mut session) = session_arc.try_lock() else {
            return false;
        };
        let max_refs = MAX_REMOVAL_STRONG_REFS
            + usize::from(session.queue_processor_running.load(Ordering::SeqCst));
        if Arc::strong_count(&session_arc) > max_refs {
            return false;
        }
        if !close_idle_session_for_cleanup(&mut session) {
            return false;
        }
        CleanupRemovalSnapshot::capture(&session)
    };

    let safe_to_remove = {
        let Ok(session) = session_arc.try_lock() else {
            return false;
        };
        removal_snapshot.still_matches(&session)
    };
    if safe_to_remove {
        sessions_write.remove(chat_id);
        return true;
    }
    false
}

pub struct CleanupRemovalSnapshot {
    runtime_state: SessionState,
    runtime_paused: bool,
    runtime_error: Option<String>,
    pause_reasons_len: usize,
    messages_len: usize,
    command_queue_len: usize,
    pending_deliveries: Vec<PendingDelivery>,
    event_seq: u64,
    trajectory_version: u64,
    last_activity: Instant,
    draft_message_present: bool,
    pending_browser_message_present: bool,
    post_tool_side_effects_len: usize,
}

impl CleanupRemovalSnapshot {
    pub fn capture(session: &ChatSession) -> Self {
        Self {
            runtime_state: session.runtime.state,
            runtime_paused: session.runtime.paused,
            runtime_error: session.runtime.error.clone(),
            pause_reasons_len: session.runtime.pause_reasons.len(),
            messages_len: session.messages.len(),
            command_queue_len: session.command_queue.len(),
            pending_deliveries: session.pending_deliveries_for_snapshot(),
            event_seq: session.event_seq,
            trajectory_version: session.trajectory_version,
            last_activity: session.last_activity,
            draft_message_present: session.draft_message.is_some(),
            pending_browser_message_present: session.pending_browser_message.is_some(),
            post_tool_side_effects_len: session.post_tool_side_effects.len(),
        }
    }

    pub fn still_matches(&self, session: &ChatSession) -> bool {
        session.closed
            && session.closed_flag.load(Ordering::Relaxed)
            && session.is_idle_for_cleanup()
            && session.runtime.state == self.runtime_state
            && session.runtime.paused == self.runtime_paused
            && session.runtime.error == self.runtime_error
            && session.runtime.pause_reasons.len() == self.pause_reasons_len
            && session.messages.len() == self.messages_len
            && session.command_queue.len() == self.command_queue_len
            && session.pending_deliveries_for_snapshot() == self.pending_deliveries
            && session.event_seq == self.event_seq
            && session.trajectory_version == self.trajectory_version
            && session.last_activity == self.last_activity
            && session.draft_message.is_some() == self.draft_message_present
            && session.pending_browser_message.is_some() == self.pending_browser_message_present
            && session.post_tool_side_effects.len() == self.post_tool_side_effects_len
    }
}

pub fn close_idle_session_for_cleanup(session: &mut ChatSession) -> bool {
    if !session.is_idle_for_cleanup() {
        return false;
    }
    if !session.closed {
        session.close_event_channel();
    }
    session.queue_notify.notify_waiters();
    true
}

pub fn start_session_cleanup_task(ctx: ChatCtx) {
    tokio::spawn(async move {
        let shutdown_flag = ctx.runtime.shutdown_flag.clone();
        let mut run_immediately = true;
        loop {
            if !run_immediately {
                tokio::select! {
                    _ = tokio::time::sleep(session_cleanup_interval()) => {}
                    _ = async {
                        while !shutdown_flag.load(std::sync::atomic::Ordering::SeqCst) {
                            tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;
                        }
                    } => {
                        tracing::info!("Session cleanup: shutdown detected, stopping");
                        return;
                    }
                }
            }
            run_immediately = false;

            let sessions = ctx.chat.sessions.clone();

            let candidates: Vec<(String, Arc<AMutex<ChatSession>>)> = {
                let sessions_read = sessions.read().await;
                sessions_read
                    .iter()
                    .map(|(chat_id, session_arc)| (chat_id.clone(), session_arc.clone()))
                    .collect()
            };

            let mut to_cleanup = Vec::new();
            for (chat_id, session_arc) in candidates {
                let session = session_arc.lock().await;
                if session.is_pending_wake_up() {
                    continue;
                }
                if session.is_idle_for_cleanup() {
                    drop(session);
                    to_cleanup.push((chat_id, session_arc));
                }
            }

            if to_cleanup.is_empty() {
                continue;
            }

            info!("Cleaning up {} idle sessions", to_cleanup.len());

            for (chat_id, session_arc) in &to_cleanup {
                if !cleanup_idle_session(ctx.clone(), &sessions, chat_id, session_arc.clone()).await
                {
                    continue;
                }

                let ctx_hook = ctx.clone();
                let chat_id_hook = chat_id.clone();
                tokio::spawn(async move {
                    let project_dir = ctx_hook.hooks.project_dir().await;
                    let payload = HookPayload {
                        hook_event_name: "SessionEnd".to_string(),
                        session_id: chat_id_hook,
                        project_dir,
                        tool_name: None,
                        tool_input: None,
                        tool_output: None,
                        user_prompt: None,
                        extra: HashMap::new(),
                    };
                    ctx_hook
                        .hooks
                        .run_hooks(HookEvent::SessionEnd, payload)
                        .await;
                });

                info!("Saved trajectory for closed session {}", chat_id);
            }
        }
    });
}
