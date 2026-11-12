use std::sync::{Arc, Mutex as StdMutex};
use tokio::sync::Mutex as AMutex;
use serde_json::json;
use tracing::warn;
use crate::app_state::AppState;
use crate::at_commands::at_commands::AtCommandsContext;
use crate::call_validation::ChatMessage;
use crate::chat::types::{TaskMeta, ThreadParams, TrajectoryCommitIntent};
use crate::chat::trajectories::save_trajectory_as_with_intent;

use super::*;

pub(crate) fn stateful_thread_from_config(chat_id: &str, config: &SubchatConfig) -> ThreadParams {
    let tool_use = match &config.tools {
        ToolsPolicy::All => "agent".to_string(),
        ToolsPolicy::None => "none".to_string(),
        ToolsPolicy::Only(v) => v.join(","),
    };
    let task_meta = task_meta_for_stateful_subchat(config);

    let mut thread = ThreadParams {
        id: chat_id.to_string(),
        title: config
            .title
            .clone()
            .unwrap_or_else(|| "Subchat".to_string()),
        model: config.model.clone(),
        mode: config.mode.clone(),
        tool_use,
        task_meta,
        worktree: config.worktree.clone(),
        parent_id: config.parent_id.clone(),
        link_type: config.link_type.clone(),
        root_chat_id: config.root_chat_id.clone(),
        autonomous_no_confirm: config.autonomous_no_confirm,
        auto_approve_editing_tools: config.auto_approve_editing_tools,
        auto_approve_dangerous_commands: config.auto_approve_dangerous_commands,
        buddy_meta: config.buddy_meta.clone(),
        context_tokens_cap: Some(config.n_ctx),
        ..Default::default()
    };
    // SubchatConfig carries the already model-normalized request window.
    thread.resolve_pending_compression_cap(Some(config.n_ctx));
    thread
}

pub(crate) fn is_subagentic_link_type(link_type: &str) -> bool {
    !matches!(link_type, "handoff" | "mode_transition" | "branch")
}

pub(crate) fn trace_thread_from_config(chat_id: &str, config: &SubchatConfig) -> ThreadParams {
    let mut thread = stateful_thread_from_config(chat_id, config);
    if !config.stateful {
        thread.link_type = Some(crate::chat::trajectories::internal_trace_link_type(
            &config.tool_name,
        ));
        thread.root_chat_id = config
            .trace_parent
            .trace_folder_owner()
            .or_else(|| thread.root_chat_id.clone())
            .or_else(|| config.parent_id.clone())
            .or_else(|| Some(crate::chat::trajectories::UNATTRIBUTED_TRACES_DIR.to_string()));
    }
    thread
}

pub(crate) fn should_persist_subchat_trajectory(config: &SubchatConfig) -> bool {
    config.stateful || config.tool_name != "mode_transition"
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SubchatTrajectoryCommitPhase {
    Seed,
    Progress,
    Failed,
    Final,
}

pub(crate) fn subchat_trajectory_commit_intent(phase: SubchatTrajectoryCommitPhase) -> TrajectoryCommitIntent {
    match phase {
        SubchatTrajectoryCommitPhase::Seed | SubchatTrajectoryCommitPhase::Progress => {
            TrajectoryCommitIntent::Checkpoint
        }
        SubchatTrajectoryCommitPhase::Failed | SubchatTrajectoryCommitPhase::Final => {
            TrajectoryCommitIntent::Required
        }
    }
}

pub(crate) type SubchatProgressMessages = Arc<StdMutex<Vec<ChatMessage>>>;

pub(crate) fn record_subchat_progress(progress: &SubchatProgressMessages, messages: &[ChatMessage]) {
    let mut slot = match progress.lock() {
        Ok(slot) => slot,
        Err(poisoned) => poisoned.into_inner(),
    };
    *slot = messages.to_vec();
}

pub(crate) async fn persist_subchat_progress(
    ccx: &Arc<AMutex<AtCommandsContext>>,
    config: &SubchatConfig,
    progress: &SubchatProgressMessages,
    messages: &[ChatMessage],
) {
    record_subchat_progress(progress, messages);
    if messages.is_empty() || !should_persist_subchat_trajectory(config) {
        return;
    }

    let (app, chat_id) = {
        let cgcx = ccx.lock().await;
        (cgcx.app.clone(), cgcx.chat_id.clone())
    };

    let thread = trace_thread_from_config(&chat_id, config);
    let acknowledgements = if let Some(agent_id) = config.background_agent_id.as_deref() {
        app.agents
            .pending_deliveries(agent_id)
            .await
            .into_iter()
            .filter(|delivery| {
                messages.iter().any(|message| {
                    refact_core::chat_types::delivery_id_of_message(message)
                        == Some(delivery.id.as_str())
                })
            })
            .map(|delivery| delivery.id)
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    // A checkpoint may merely schedule a write. A delivery may only be removed
    // from the durable registry after a required trajectory commit has completed.
    let intent = if acknowledgements.is_empty() {
        subchat_trajectory_commit_intent(SubchatTrajectoryCommitPhase::Progress)
    } else {
        TrajectoryCommitIntent::Required
    };
    match crate::chat::trajectories::save_trajectory_as_with_intent_checked(&app.chat_ctx(),
        &thread,
        messages,
        intent,
    )
    .await
    {
        Ok(()) => {
            if let Some(agent_id) = config.background_agent_id.as_deref() {
                for id in acknowledgements {
                    if let Err(error) = app.agents.acknowledge_delivery(agent_id, &id).await {
                        warn!(%agent_id, %id, %error, "Failed to acknowledge durable runner delivery");
                    }
                }
                crate::agents::delivery::publish_runner_queue(&app, agent_id).await;
            }
        }
        Err(error) => warn!(%error, "Failed to save runner progress; retaining delivery payloads"),
    }
    if config.stateful {
        mirror_subchat_messages_into_session(&app, &chat_id, messages).await;
    }
}

pub(crate) fn take_subchat_progress(progress: &SubchatProgressMessages) -> Vec<ChatMessage> {
    match progress.lock() {
        Ok(slot) => slot.clone(),
        Err(poisoned) => poisoned.into_inner().clone(),
    }
}

pub(crate) async fn save_failed_subchat_trajectory(
    app: &AppState,
    chat_id: &str,
    config: &SubchatConfig,
    progress: &SubchatProgressMessages,
    error: &str,
) {
    if !should_persist_subchat_trajectory(config) {
        return;
    }
    let mut messages = take_subchat_progress(progress);
    if messages.is_empty() {
        return;
    }

    let error = safe_context_limit_error_for_log(error);
    messages.push(crate::chat::internal_roles::event(
        crate::chat::internal_roles::EventSubkind::SystemNotice,
        "subchat.run",
        json!({"error": error, "tool_name": config.tool_name}),
        format!("Subchat run failed: {error}"),
    ));

    let ctx = app.chat_ctx();
    let mut thread = trace_thread_from_config(chat_id, config);
    if config.stateful {
        register_stateful_subchat_worktree(&ctx, chat_id, &mut thread).await;
    }
    save_trajectory_as_with_intent(&ctx,
        &thread,
        &messages,
        subchat_trajectory_commit_intent(SubchatTrajectoryCommitPhase::Failed),
    )
    .await;
    if config.stateful {
        mirror_subchat_messages_into_session(app, chat_id, &messages).await;
    }
}

pub(crate) fn task_meta_for_stateful_subchat(config: &SubchatConfig) -> Option<TaskMeta> {
    let mut task_meta = config.task_meta.clone()?;
    if task_meta.role == "planner"
        && config
            .link_type
            .as_deref()
            .map(is_subagentic_link_type)
            .unwrap_or(false)
    {
        task_meta.role = "subchats".to_string();
    }
    Some(task_meta)
}

pub(crate) fn stable_subchat_chat_id(
    config: &SubchatConfig,
    fresh_id: impl FnOnce() -> String,
) -> String {
    if let Some(chat_id) = config
        .chat_id
        .as_deref()
        .map(str::trim)
        .filter(|chat_id| !chat_id.is_empty())
    {
        return chat_id.to_string();
    }
    if let Some(agent_id) = config
        .background_agent_id
        .as_deref()
        .map(str::trim)
        .filter(|agent_id| !agent_id.is_empty())
    {
        return format!("subchat-{agent_id}");
    }
    fresh_id()
}
