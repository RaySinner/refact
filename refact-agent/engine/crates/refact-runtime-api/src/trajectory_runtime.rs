use refact_chat_api::{ChatMessage, TaskMeta, ThreadParams, TrajectoryCommitIntent};
use refact_core::chat_types::ChatContent;
use uuid::Uuid;
use refact_chat_history::trajectory_model::LoadedTrajectory;
use refact_chat_history::trajectory_snapshot::TrajectorySnapshot;
use refact_perf::perf_diagnostics::{self, PerfComponent, PerfOutcome};

use crate::chat_ctx::ChatCtx;

pub async fn load_trajectory_for_chat(ctx: &ChatCtx, chat_id: &str) -> Option<LoadedTrajectory> {
    ctx.trajectories.load_trajectory_for_chat(chat_id).await
}

pub async fn find_trajectory_path(ctx: &ChatCtx, chat_id: &str) -> Option<std::path::PathBuf> {
    ctx.trajectories.find_trajectory_path(chat_id).await
}

pub async fn save_trajectory_snapshot(
    gcx: crate::global_context::SharedGlobalContext,
    snapshot: TrajectorySnapshot,
) -> Result<(), String> {
    crate::gcx_helpers::save_trajectory_snapshot_for_gcx(&gcx, snapshot).await
}

pub async fn set_trajectory_label(
    app: crate::app_state::AppState,
    chat_id: &str,
    label: &str,
) -> Result<(), String> {
    refact_chat_history::trajectory_paths::validate_trajectory_id(chat_id)
        .map_err(|error| error.message)?;
    let label = refact_chat_history::trajectory_metrics::validate_trajectory_label(label)?;
    let session_arc = app
        .chat
        .sessions
        .read()
        .await
        .get(chat_id)
        .cloned()
        .ok_or_else(|| format!("Live trajectory '{chat_id}' not found"))?;
    {
        let mut session = session_arc.lock().await;
        session.set_title_from_trajectory_label(label);
    }
    app.chat_ctx()
        .trajectories
        .try_save_trajectory_with_intent(session_arc, TrajectoryCommitIntent::Required)
        .await
        .map(|_| ())
}

fn task_meta_is_planner_for_task(task_meta: Option<&TaskMeta>, task_id: &str) -> bool {
    task_meta.is_some_and(|meta| meta.role == "planner" && meta.task_id == task_id)
}

pub async fn chat_id_is_planner_for_task(ctx: &ChatCtx, chat_id: &str, task_id: &str) -> bool {
    let session_arc = {
        let sessions = ctx.chat.sessions.read().await;
        sessions.get(chat_id).cloned()
    };
    if let Some(session_arc) = session_arc {
        let session = session_arc.lock().await;
        if task_meta_is_planner_for_task(session.thread.task_meta.as_ref(), task_id) {
            return true;
        }
    }

    load_trajectory_for_chat(ctx, chat_id)
        .await
        .is_some_and(|loaded| {
            task_meta_is_planner_for_task(loaded.thread.task_meta.as_ref(), task_id)
        })
}

pub async fn verified_planner_linked_root_chat_id(
    ctx: &ChatCtx,
    current_chat_id: &str,
    current_root_chat_id: Option<&str>,
    task_meta: &TaskMeta,
) -> Option<String> {
    if task_meta.role == "planner" {
        return None;
    }
    let root_chat_id =
        current_root_chat_id.filter(|id| !id.is_empty() && *id != current_chat_id)?;
    if task_meta.planner_chat_id.as_deref() == Some(root_chat_id) {
        return Some(root_chat_id.to_string());
    }
    if chat_id_is_planner_for_task(ctx, root_chat_id, &task_meta.task_id).await {
        return Some(root_chat_id.to_string());
    }
    None
}

pub async fn resolve_task_planner_controller_chat_id(
    ctx: &ChatCtx,
    current_chat_id: &str,
    current_root_chat_id: Option<&str>,
    task_meta: Option<&TaskMeta>,
) -> String {
    let Some(task_meta) = task_meta else {
        return current_chat_id.to_string();
    };
    if let Some(root_chat_id) =
        verified_planner_linked_root_chat_id(ctx, current_chat_id, current_root_chat_id, task_meta)
            .await
    {
        return root_chat_id;
    }
    if task_meta.role == "planner" {
        return task_meta
            .planner_chat_id
            .clone()
            .filter(|id| {
                !id.is_empty()
                    && current_root_chat_id.filter(|root_id| *root_id != current_chat_id)
                        != Some(id.as_str())
            })
            .unwrap_or_else(|| current_chat_id.to_string());
    }
    task_meta
        .planner_chat_id
        .clone()
        .filter(|id| !id.is_empty())
        .unwrap_or_else(|| current_chat_id.to_string())
}

pub async fn save_trajectory_as_with_intent(
    ctx: &ChatCtx,
    thread: &ThreadParams,
    messages: &[ChatMessage],
    intent: TrajectoryCommitIntent,
) {
    if let Err(e) = save_trajectory_as_with_intent_checked(ctx, thread, messages, intent).await {
        tracing::warn!("Failed to save trajectory: {}", e);
    }
}

pub async fn save_trajectory_as_with_intent_checked(
    ctx: &ChatCtx,
    thread: &ThreadParams,
    messages: &[ChatMessage],
    intent: TrajectoryCommitIntent,
) -> Result<(), String> {
    if !intent.persists() {
        return Ok(());
    }
    let snapshot = TrajectorySnapshot::from_thread_parts(
        thread.id.clone(),
        thread,
        messages.to_vec(),
        chrono::Utc::now().to_rfc3339(),
        1,
    );
    ctx.trajectories
        .persist_trajectory_snapshot_with_intent(snapshot, intent)
        .await
}

pub async fn save_trajectory_snapshot_for_ctx(
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
    let result = ctx
        .trajectories
        .persist_trajectory_snapshot_with_intent(snapshot, TrajectoryCommitIntent::Required)
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

pub async fn save_initial_task_chat_trajectory(
    ctx: &ChatCtx,
    task_id: &str,
    chat_id: &str,
    mode: &str,
) -> Result<(), String> {
    let greeting = "## 🎯 Task Planner

I'm your **Task Planner**. I handle the complete task lifecycle - from investigation to execution.

**Planning Phase:**
- Analyze the codebase using search and exploration tools
- Create task cards with clear acceptance criteria
- Set priorities and dependencies between cards

**Execution Phase:**
- Spawn agents to work on ready cards (each in isolated git worktree)
- Monitor agent progress and receive completion notifications
- Merge successful work back to main branch
- Handle failures and coordinate retries

**How to use me:**
1. Describe what you want to accomplish
2. I'll investigate and create a structured plan (task cards)
3. When ready, I'll spawn agents to implement each card
4. I'll notify you as work completes and handle merging
5. We iterate until the task is done

**Ready when you are!** Tell me what you'd like to build or fix.";

    let greeting_msg = ChatMessage {
        message_id: Uuid::new_v4().to_string(),
        role: "assistant".to_string(),
        content: ChatContent::SimpleText(greeting.to_string()),
        finish_reason: Some("stop".to_string()),
        ..Default::default()
    };

    let task_meta = TaskMeta {
        task_id: task_id.to_string(),
        role: "planner".to_string(),
        agent_id: None,
        card_id: None,
        planner_chat_id: Some(chat_id.to_string()),
    };

    let snapshot = TrajectorySnapshot {
        goal: None,
        goal_ledger: Vec::new(),
        pending_deliveries: Vec::new(),
        goal_verification_blocked_until_ms: None,
        chat_id: chat_id.to_string(),
        title: String::new(),
        model: String::new(),
        mode: mode.to_string(),
        tool_use: "agent".to_string(),
        messages: if mode == "task_planner" {
            vec![greeting_msg]
        } else {
            Vec::new()
        },
        created_at: chrono::Utc::now().to_rfc3339(),
        boost_reasoning: false,
        checkpoints_enabled: true,
        context_tokens_cap: None,
        auto_compression_cap: None,
        auto_compression_cap_pending: true,
        include_project_info: true,
        is_title_generated: false,
        auto_approve_editing_tools: true,
        auto_approve_dangerous_commands: true,
        autonomous_no_confirm: false,
        auto_enrichment_enabled: Some(false),
        task_meta: Some(task_meta),
        worktree: None,
        version: 1,
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
        buddy_meta: None,
        auto_compact_enabled: None,
        frozen_request_prefix: None,
        claude_code_identity: None,
        reactive_compact_attempts: None,
        wake_up_at: None,
        waiting_for_card_ids: Vec::new(),
    };

    save_trajectory_snapshot_for_ctx(ctx, snapshot).await
}
