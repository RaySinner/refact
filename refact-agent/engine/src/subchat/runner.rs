use std::sync::{Arc, Mutex as StdMutex, OnceLock};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use tokio::sync::{Mutex as AMutex, mpsc};
use serde_json::{json, Value};
use tracing::{info, warn};
use uuid::Uuid;
use crate::app_state::AppState;
use crate::caps::resolve_chat_model;
use crate::tools::tools_description::ToolDesc;
use crate::tools::tools_list::get_available_tools;
use crate::at_commands::at_commands::AtCommandsContext;
use crate::call_validation::{ChatContent, ChatMeta, ChatToolCall, SamplingParameters, ChatMessage, ChatUsage, ReasoningEffort, ContextFile};
use crate::global_context::GlobalContext;
use crate::scratchpad_abstract::HasTokenizerAndEot;
use crate::chat::ctx::ChatCtx;
use crate::chat::prepare::{prepare_chat_passthrough, ChatPrepareOptions};
use crate::chat::internal_roles::{event, EventSubkind};
use crate::llm::params::CacheControl;
use crate::chat::stream_core::{run_llm_stream, StreamRunParams, ChoiceFinal, StreamCollector, normalize_tool_call, LlmStreamError, clear_unbound_openai_codex_websocket_session};
use crate::chat::retry_policy::{retry_delay_for_attempt, sleep_or_abort, MAX_LLM_RETRY_ATTEMPTS};
use crate::chat::tools::{execute_tools, resolve_tool_call_aliases, ExecuteToolsOptions};
use crate::chat::types::{TaskMeta, ThreadParams};
use crate::worktrees::types::WorktreeMeta;
use crate::chat::trajectories::save_trajectory_as_with_intent;
use crate::chat::trajectory_ops::sanitize_messages_for_new_thread;
use crate::stats::event::{canonicalize_mode_for_stats, split_model_provider, LlmCallEvent};

use super::*;

pub(crate) fn append_runner_deliveries(
    messages: &mut Vec<ChatMessage>,
    deliveries: Vec<refact_chat_api::PendingDelivery>,
) -> bool {
    let mut wake = false;
    for delivery in deliveries {
        if messages.iter().any(|message| {
            refact_core::chat_types::delivery_id_of_message(message) == Some(delivery.id.as_str())
        }) {
            continue;
        }
        wake |= delivery.wake;
        if delivery.push == refact_chat_api::PushMode::Preempt {
            messages.push(event(
                EventSubkind::CancellationNote,
                "runner",
                json!({"source": delivery.source, "delivery_id": delivery.id}),
                format!("The current response was interrupted by {}. Follow the delivered instructions below.", delivery.source),
            ));
        }
        messages.extend(delivery.stamp_messages());
    }
    wake
}

pub(crate) fn runner_tool_window_closed(messages: &[ChatMessage]) -> bool {
    let Some(index) = messages
        .iter()
        .rposition(|message| message.role == "assistant")
    else {
        return true;
    };
    messages[index].tool_calls.as_ref().map_or(true, |calls| {
        calls.iter().all(|call| {
            messages[index + 1..].iter().any(|message| {
                (message.role == "tool" || message.role == "diff")
                    && message.tool_call_id == call.id
            })
        })
    })
}

pub(crate) async fn drain_runner_deliveries(
    app: &AppState,
    agent_id: &str,
    session: Option<&refact_chat_api::ChatSession>,
    idle: bool,
    seal: bool,
) -> Result<Vec<refact_chat_api::PendingDelivery>, String> {
    let deliveries = app.agents.pending_deliveries(agent_id).await;
    let stale_missing_goal_ids = session
        .map(|session| session.stale_missing_goal_runner_delivery_ids(&deliveries))
        .unwrap_or_default();
    app.agents
        .drain_runner_deliveries(
            agent_id,
            idle,
            seal,
            &stale_missing_goal_ids,
            session,
        )
        .await
}

pub(crate) async fn import_runner_local_deliveries(
    app: &AppState,
    agent_id: &str,
    chat_id: &str,
    messages: &[ChatMessage],
) {
    let projected = match refact_core::active_context::active_context(messages) {
        Ok(view) => view.messages,
        Err(error) => {
            warn!(%chat_id, %error, "Cannot import deliveries into unavailable active context");
            return;
        }
    };
    if !runner_tool_window_closed(&projected) {
        return;
    }
    let session = app.chat.sessions.read().await.get(chat_id).cloned();
    let Some(session) = session else {
        return;
    };
    let deliveries = session.lock().await.pending_deliveries.clone();
    for mut delivery in deliveries {
        if let Some(tool_id) = delivery.after_tool_call_id.as_deref() {
            let ready = projected.iter().enumerate().any(|(index, message)| {
                message.role == "assistant"
                    && message.tool_calls.as_ref().is_some_and(|calls| {
                        calls.iter().any(|call| {
                            (tool_id.is_empty() || call.id == tool_id)
                                && projected[index + 1..].iter().any(|result| {
                                    result.role == "tool" && result.tool_call_id == call.id
                                })
                        })
                    })
            });
            if !ready {
                continue;
            }
        }
        delivery.after_tool_call_id = None;
        let id = delivery.id.clone();
        match app.agents.enqueue_delivery(agent_id, delivery).await {
            Ok(_) => {
                let _ = session
                    .lock()
                    .await
                    .update_pending_delivery(&id, None, true);
            }
            Err(error) => warn!(%agent_id, %id, %error, "Failed to import runner local delivery"),
        }
    }
}

pub(crate) async fn drain_background_agent_inbox(
    ccx: &Arc<AMutex<AtCommandsContext>>,
    messages: &mut Vec<ChatMessage>,
) {
    if !runner_tool_window_closed(messages) {
        return;
    }
    let (app, agent_id, chat_id) = {
        let ccx = ccx.lock().await;
        (
            ccx.app.clone(),
            ccx.background_agent_id.clone(),
            ccx.chat_id.clone(),
        )
    };
    let Some(agent_id) = agent_id else {
        return;
    };
    import_runner_local_deliveries(&app, &agent_id, &chat_id, messages).await;
    {
        let session = app.chat.sessions.read().await.get(&chat_id).cloned();
        let mut session = match session.as_ref() {
            Some(session) => Some(session.lock().await),
            None => None,
        };
        // A yielded wait opens C only after every tool result has landed. Keep
        // the boundary through local tick import, and retain it on drain errors.
        let include_when_idle = session
            .as_ref()
            .is_some_and(|session| session.wait_delivery_boundary);
        match drain_runner_deliveries(
            &app,
            &agent_id,
            session.as_deref(),
            include_when_idle,
            false,
        )
        .await
        {
            Ok(deliveries) => {
                append_runner_deliveries(messages, deliveries);
                if let Some(session) = session.as_mut() {
                    session.wait_delivery_boundary = false;
                }
            }
            Err(error) => warn!(%agent_id, %error, "Failed to drain runner deliveries"),
        }
    }    crate::agents::delivery::publish_runner_queue(&app, &agent_id).await;
    let inboxes = app.agents.drain_inbox(&agent_id).await;
    let has_inboxes = !inboxes.is_empty();
    for message in inboxes {        let sender = match message.from.as_str() {
            "user" => "[message from user]".to_string(),
            "parent" => "[message from parent]".to_string(),
            from => format!("[notice from sibling {from}]"),
        };        messages.push(event(
            EventSubkind::SystemNotice,
            "agents.inbox",
            json!({ "from": message.from, "queued_at": message.queued_at }),
            format!("{sender}\n{}", message.text),
        ));
    }
    if has_inboxes {
        mirror_subchat_messages_into_session(&app, &chat_id, messages).await;
    }}

pub(crate) fn get_context_files_from_messages(messages: &[ChatMessage]) -> Vec<String> {
    use std::collections::HashSet;
    let mut seen = HashSet::new();
    let mut paths = Vec::new();
    for msg in messages {
        if msg.role == "context_file" {
            match &msg.content {
                ChatContent::ContextFiles(files) => {
                    for file in files {
                        if seen.insert(file.file_name.clone()) {
                            paths.push(file.file_name.clone());
                        }
                    }
                }
                ChatContent::SimpleText(text) => {
                    if let Ok(files) = serde_json::from_str::<Vec<ContextFile>>(text) {
                        for file in files {
                            if seen.insert(file.file_name.clone()) {
                                paths.push(file.file_name.clone());
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }
    paths
}

pub(crate) fn should_stream_thinking_progress(tool_name: &str) -> bool {
    tool_name == "review_agents" || tool_name.starts_with("review_")
}

pub(crate) struct SubchatProgressCollector {
    sender: Option<mpsc::UnboundedSender<Value>>,
    tool_call_id: Option<String>,
    activity_stamp: Option<Arc<AtomicU64>>,
    thinking_tail: String,
    reasoning_tail: String,
    content_tail: String,
    last_sent: String,
    last_sent_at: std::time::Instant,
}

impl SubchatProgressCollector {
    pub(super) fn new(
        sender: Option<mpsc::UnboundedSender<Value>>,
        tool_call_id: Option<String>,
        activity_stamp: Option<Arc<AtomicU64>>,
    ) -> Self {
        Self {
            sender,
            tool_call_id,
            activity_stamp,
            thinking_tail: String::new(),
            reasoning_tail: String::new(),
            content_tail: String::new(),
            last_sent: String::new(),
            last_sent_at: std::time::Instant::now()
                .checked_sub(std::time::Duration::from_secs(60))
                .unwrap_or_else(std::time::Instant::now),
        }
    }

    pub(super) fn append_tail(buf: &mut String, text: &str, max_chars: usize) {
        if text.is_empty() {
            return;
        }
        buf.push_str(text);
        if buf.len() > max_chars {
            let mut start = buf.len().saturating_sub(max_chars);
            while start < buf.len() && !buf.is_char_boundary(start) {
                start += 1;
            }
            buf.drain(..start);
        }
    }

    pub(super) fn extract_thinking_preview(blocks: &[serde_json::Value]) -> Option<String> {
        for block in blocks.iter().rev() {
            let Some(obj) = block.as_object() else {
                continue;
            };
            for key in ["thinking", "text", "content"] {
                if let Some(s) = obj.get(key).and_then(|v| v.as_str()) {
                    if !s.trim().is_empty() {
                        return Some(s.to_string());
                    }
                }
            }
        }
        None
    }

    fn normalize_preview(text: &str) -> String {
        // Preserve newlines for markdown rendering, just normalize CRLF/CR.
        text.replace("\r\n", "\n").replace('\r', "\n")
    }

    fn maybe_send_update(&mut self) {
        let Some(sender) = self.sender.as_ref() else {
            return;
        };
        let Some(tool_call_id) = self.tool_call_id.as_ref() else {
            return;
        };

        // Content takes priority: if the model has started producing the actual
        // answer, preview it even if thinking/reasoning content is present.
        // Otherwise the UI would be stuck showing only thoughts.
        let raw = if !self.content_tail.trim().is_empty() {
            &self.content_tail
        } else if !self.thinking_tail.trim().is_empty() {
            &self.thinking_tail
        } else {
            &self.reasoning_tail
        };

        let mut progress = Self::normalize_preview(raw);
        if progress.is_empty() {
            return;
        }

        // UI renders markdown up to ~50k chars; keep progress within that.
        const MAX_CHARS: usize = 50_000;
        let truncated = crate::llm::safe_truncate(&progress, MAX_CHARS);
        if truncated.len() != progress.len() {
            progress = format!("{}…", truncated);
        }

        if self.last_sent == progress {
            return;
        }

        let now = std::time::Instant::now();
        if now.duration_since(self.last_sent_at) < std::time::Duration::from_millis(750)
            && !self.last_sent.is_empty()
        {
            return;
        }

        let msg = json!({
            "tool_call_id": tool_call_id,
            "subchat_id": progress,
        });
        let _ = sender.send(msg);

        self.last_sent = progress;
        self.last_sent_at = now;
    }
}

impl StreamCollector for SubchatProgressCollector {
    fn on_delta_ops(&mut self, _choice_idx: usize, ops: Vec<crate::chat::types::DeltaOp>) {
        stamp_activity(&self.activity_stamp);
        for op in ops {
            match op {
                crate::chat::types::DeltaOp::AppendReasoning { text } => {
                    if self.thinking_tail.trim().is_empty() {
                        Self::append_tail(&mut self.reasoning_tail, &text, 50_000);
                    }
                }
                crate::chat::types::DeltaOp::SetReasoning { text } => {
                    if self.thinking_tail.trim().is_empty() {
                        self.reasoning_tail.clear();
                        Self::append_tail(&mut self.reasoning_tail, &text, 50_000);
                    }
                }
                crate::chat::types::DeltaOp::AppendContent { text } => {
                    // Content always takes over from thinking/reasoning: once the
                    // model emits answer content, the preview must follow it too.
                    Self::append_tail(&mut self.content_tail, &text, 50_000);
                }
                crate::chat::types::DeltaOp::SetThinkingBlocks { blocks } => {
                    if let Some(preview) = Self::extract_thinking_preview(&blocks) {
                        self.thinking_tail = preview;
                    }
                }
                _ => {}
            }
        }

        self.maybe_send_update();
    }

    fn on_usage(&mut self, _usage: &ChatUsage) {}

    fn on_finish(&mut self, _choice_idx: usize, _finish_reason: Option<String>) {}
}

pub(crate) fn has_final_answer(messages: &[ChatMessage]) -> bool {
    messages
        .last()
        .filter(|m| m.role == "assistant")
        .map(|m| m.tool_calls.as_ref().map_or(true, |tc| tc.is_empty()))
        .unwrap_or(false)
}

pub async fn run_subchat(
    gcx: Arc<GlobalContext>,
    messages: Vec<ChatMessage>,
    mut config: SubchatConfig,
) -> Result<SubchatResult, String> {
    if !subchat_retries_allowed(&config) {
        config.wrap_up = None;
        config.final_step_force_answer = false;
    }
    info!(
        "run_subchat tool={} model={} stateful={}",
        config.tool_name, config.model, config.stateful
    );

    let app = crate::app_state::app_state_from_gcx(gcx.clone()).await;
    let ctx = app.chat_ctx();

    let chat_id = stable_subchat_chat_id(&config, || format!("subchat-{}", Uuid::new_v4()));

    let messages = refact_core::active_context::active_context(&messages)
        .map_err(|e| e.to_string())?
        .messages;
    let messages = sanitize_messages_for_new_thread(&messages);
    let messages = prepare_subchat_messages(&ctx, messages, &config.model)?;
    if should_persist_subchat_trajectory(&config) {
        let thread = trace_thread_from_config(&chat_id, &config);
        save_trajectory_as_with_intent(&ctx,
            &thread,
            &messages,
            subchat_trajectory_commit_intent(SubchatTrajectoryCommitPhase::Seed),
        )
        .await;
    }
    if config.stateful {
        install_stateful_subchat_session(&app, &chat_id, &config, &messages).await;
    }
    let ccx = Arc::new(AMutex::new(
        crate::at_commands::at_commands::at_commands_new_with_abort(
            app.clone(),
            config.n_ctx,
            1,
            false,
            messages.clone(),
            chat_id.clone(),
            config.root_chat_id.clone(),
            config.model.clone(),
            config.task_meta.clone(),
            config.worktree.clone(),
            config.abort_flag.clone(),
        )
        .await,
    ));

    ccx.lock().await.subchat_depth = config.subchat_depth;
    ccx.lock().await.background_agent_id = config.background_agent_id.clone();
    ccx.lock().await.activity_stamp = config.activity_stamp.clone();

    if let Some(ref parent_tx) = config.parent_subchat_tx {
        ccx.lock().await.subchat_tx = parent_tx.clone();
    }

    let mut _usage = ChatUsage::default();

    let progress_messages: SubchatProgressMessages = Arc::new(StdMutex::new(messages.clone()));

    let current_messages_result: Result<(Vec<ChatMessage>, bool), String> = async {
        let mut messages = messages;
        loop {
            let (mut completed, aborted) = if let Some(ref wrap_up) = config.wrap_up {
                Box::pin(run_subchat_with_wrap_up(
                    ccx.clone(),
                    &config,
                    messages,
                    &config.tools,
                    wrap_up,
                    &mut _usage,
                    &progress_messages,
                ))
                .await?
            } else {
                Box::pin(run_subchat_loop(
                    ccx.clone(),
                    &config,
                    messages,
                    &config.tools,
                    &mut _usage,
                    &progress_messages,
                ))
                .await?
            };
            if aborted {
                break Ok((completed, true));
            }
            // Outside interruptible-wait boundaries, C becomes eligible after
            // forced finals and wrap-up. The registry closes acceptance atomically
            // with this drain so a successful enqueue cannot be stranded on exit.
            let wake = if let Some(agent_id) = config.background_agent_id.as_deref() {
                import_runner_local_deliveries(&app, agent_id, &chat_id, &completed).await;
                let session = app.chat.sessions.read().await.get(&chat_id).cloned();
                let session = match session.as_ref() {
                    Some(session) => Some(session.lock().await),
                    None => None,
                };
                let deliveries = drain_runner_deliveries(
                    &app,
                    agent_id,
                    session.as_deref(),
                    true,
                    true,
                )
                .await?;
                drop(session);
                let wake = append_runner_deliveries(&mut completed, deliveries);
                crate::agents::delivery::publish_runner_queue(&app, agent_id).await;
                wake
            } else {
                false
            };
            persist_subchat_progress(&ccx, &config, &progress_messages, &completed).await;
            if !wake {
                break Ok((completed, false));
            }
            messages = completed;
        }
    }
    .await;
    let (mut current_messages, aborted) = match current_messages_result {
        Ok((messages, aborted)) => (messages, aborted),
        Err(e) => {
            save_failed_subchat_trajectory(&app, &chat_id, &config, &progress_messages, &e).await;
            finish_stateful_subchat_session(&app, &chat_id, &config).await;
            clear_unbound_openai_codex_websocket_session(&chat_id).await;
            return Err(e);
        }
    };

    let subagent_policy = ctx
        .privacy_policy_load
        .read()
        .unwrap()
        .policy
        .subagents
        .clone();
    if let Err(error) = apply_subchat_report_policy(&subagent_policy, &mut current_messages) {
        let error = error.to_string();
        save_failed_subchat_trajectory(&app, &chat_id, &config, &progress_messages, &error).await;
        finish_stateful_subchat_session(&app, &chat_id, &config).await;
        clear_unbound_openai_codex_websocket_session(&chat_id).await;
        return Err(error);
    }

    if config.stateful {
        let mut thread = stateful_thread_from_config(&chat_id, &config);
        register_stateful_subchat_worktree(&ctx, &chat_id, &mut thread).await;
        save_trajectory_as_with_intent(&ctx,
            &thread,
            &current_messages,
            subchat_trajectory_commit_intent(SubchatTrajectoryCommitPhase::Final),
        )
        .await;
        mirror_subchat_messages_into_session(&app, &chat_id, &current_messages).await;
    } else if should_persist_subchat_trajectory(&config) {
        let thread = trace_thread_from_config(&chat_id, &config);
        save_trajectory_as_with_intent(&ctx,
            &thread,
            &current_messages,
            subchat_trajectory_commit_intent(SubchatTrajectoryCommitPhase::Final),
        )
        .await;
    }

    let metering = aggregate_metering_from_messages(&current_messages);
    finish_stateful_subchat_session(&app, &chat_id, &config).await;
    clear_unbound_openai_codex_websocket_session(&chat_id).await;

    Ok(SubchatResult {
        messages: current_messages,
        metering,
        chat_id: if config.stateful { Some(chat_id) } else { None },
        aborted,
    })
}

pub async fn run_subchat_once(
    gcx: Arc<GlobalContext>,
    tool_name: &str,
    messages: Vec<ChatMessage>,
    trace_parent: TraceParent,
) -> Result<SubchatResult, String> {
    run_subchat_once_with_abort(gcx, tool_name, messages, None, trace_parent).await
}

pub async fn run_subchat_once_with_abort(
    gcx: Arc<GlobalContext>,
    tool_name: &str,
    messages: Vec<ChatMessage>,
    abort_flag: Option<Arc<AtomicBool>>,
    trace_parent: TraceParent,
) -> Result<SubchatResult, String> {
    let mut config = resolve_subchat_config_with_parent(
        gcx.clone(),
        tool_name,
        false,
        None,
        None,
        None,
        None,
        None,
        Some(vec![]),
        1,
        false,
        None,
        "agent".to_string(),
        None,
        None,
        None,
        None,
        abort_flag,
        0,
    )
    .await?;
    config.trace_parent = trace_parent;

    run_subchat(gcx, messages, config).await
}

pub async fn run_subchat_once_with_parent(
    gcx: Arc<GlobalContext>,
    tool_name: &str,
    messages: Vec<ChatMessage>,
    parent_tool_call_id: String,
    parent_subchat_tx: Arc<AMutex<mpsc::UnboundedSender<Value>>>,
    parent_abort_flag: Arc<AtomicBool>,
    parent_depth: usize,
    parent_task_meta: Option<TaskMeta>,
    parent_worktree: Option<WorktreeMeta>,
    trace_parent: TraceParent,
) -> Result<SubchatResult, String> {
    let mut config = resolve_subchat_config_with_parent(
        gcx.clone(),
        tool_name,
        false,
        None,
        None,
        None,
        None,
        None,
        Some(vec![]),
        1,
        false,
        None,
        "agent".to_string(),
        parent_task_meta,
        parent_worktree,
        Some(parent_tool_call_id),
        Some(parent_subchat_tx),
        Some(parent_abort_flag),
        parent_depth + 1,
    )
    .await?;
    config.trace_parent = trace_parent;

    run_subchat(gcx, messages, config).await
}

pub(crate) fn is_abort_error(err: &str) -> bool {
    err.eq_ignore_ascii_case("aborted")
}

pub(crate) fn epoch_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

pub(crate) fn stamp_activity(stamp: &Option<Arc<AtomicU64>>) {
    if let Some(stamp) = stamp {
        stamp.store(epoch_ms(), Ordering::Relaxed);
    }
}

pub(crate) fn final_step_wrap_up_message(max_steps: usize) -> ChatMessage {
    ChatMessage::new(
        "user".to_string(),
        format!(
            "⚠️ Step budget reached ({max_steps}/{max_steps}): no more tool calls are available. Stop investigating now and write your final response using everything you have gathered so far, following the exact final report / Status format defined in your system prompt. Do not call any tools. If something is still unverified, give your best partial findings and flag the gaps — never return an empty result."
        ),
    )
}

pub(crate) fn needs_forced_final_answer(
    final_step_force_answer: bool,
    aborted: bool,
    has_answer: bool,
) -> bool {
    final_step_force_answer && !aborted && !has_answer
}

/// Race only the uncommitted model draft, never tool execution. Dropping this
/// future discards partial output without setting the runner's permanent abort flag.
pub(crate) async fn wait_for_runner_preempt(app: &AppState, agent_id: &str) {
    let Some(notify) = app.agents.interrupt_notify(agent_id).await else {
        std::future::pending::<()>().await;
        return;
    };
    loop {
        let notified = notify.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        if app.agents.has_preempt(agent_id).await {
            return;
        }
        notified.await;
    }
}

pub(crate) async fn runner_model_turn(
    ccx: Arc<AMutex<AtCommandsContext>>,
    config: &SubchatConfig,
    messages: &mut Vec<ChatMessage>,
    tools_subset: Option<Vec<String>>,
    prepend_system_prompt: bool,
) -> Result<Vec<Vec<ChatMessage>>, String> {
    let projected =
        refact_core::active_context::active_context(messages).map_err(|error| error.to_string())?;
    if !runner_tool_window_closed(&projected.messages) {
        return Err(
            "Cannot generate a runner draft before all tool calls have results".to_string(),
        );
    }
    let (app, agent_id, chat_id) = {
        let context = ccx.lock().await;
        (
            context.app.clone(),
            context.background_agent_id.clone(),
            context.chat_id.clone(),
        )
    };
    loop {
        let result = {
            let generation = subchat_single_internal(
                ccx.clone(),
                &config.model,
                &config.mode,
                refact_core::active_context::active_context(messages)
                    .map_err(|e| e.to_string())?
                    .messages,
                tools_subset.clone(),
                false,
                config.temperature,
                config.max_new_tokens,
                config.reasoning_effort.clone(),
                config.cache_control,
                prepend_system_prompt,
                if should_stream_thinking_progress(&config.tool_name) {
                    config.parent_tool_call_id.as_deref()
                } else {
                    None
                },
                subchat_retries_allowed(config),
            );
            tokio::pin!(generation);
            if let Some(agent_id) = agent_id.as_deref() {
                tokio::select! {
                    biased;
                    _ = wait_for_runner_preempt(&app, agent_id) => None,
                    result = &mut generation => Some(result),
                }
            } else {
                Some(generation.await)
            }
        };        if let Some(result) = result {
            let projected_len = refact_core::active_context::active_context(messages)
                .map_err(|e| e.to_string())?
                .messages
                .len();
            
            // Mirror any messages generated so far into the session (especially if we received an empty finish)
            let result_ref = result.as_ref();
            if result_ref.is_ok() {
                mirror_subchat_messages_into_session(&app, &chat_id, messages).await;
            }
                        return result.map(|choices| {
                choices
                    .into_iter()
                    .map(|choice| {
                        let mut raw = messages.clone();
                        raw.extend(choice.into_iter().skip(projected_len));
                        raw
                    })
                    .collect()
            });
        }
        // The cancelled generation has been dropped before touching its context.
        clear_unbound_openai_codex_websocket_session(&chat_id).await;
        drain_background_agent_inbox(&ccx, messages).await;
        if let Some(agent_id) = agent_id.as_deref() {
            if app.agents.has_preempt(agent_id).await {
                // A concurrent new preemption can retry, but never busy-spin on
                // a failed durable drain: let the registry error reach the caller.
                let session = app.chat.sessions.read().await.get(&chat_id).cloned();
                let session = match session.as_ref() {
                    Some(session) => Some(session.lock().await),
                    None => None,
                };
                let deliveries = drain_runner_deliveries(
                    &app,
                    agent_id,
                    session.as_deref(),
                    false,
                    false,
                )
                .await?;
                drop(session);
                append_runner_deliveries(messages, deliveries);
                crate::agents::delivery::publish_runner_queue(&app, agent_id).await;
            }
        }
        if is_aborted(&config.abort_flag) {
            return Err("Aborted".to_string());
        }
    }
}

pub(crate) async fn run_subchat_loop(
    ccx: Arc<AMutex<AtCommandsContext>>,
    config: &SubchatConfig,
    mut messages: Vec<ChatMessage>,
    tools_policy: &ToolsPolicy,
    usage: &mut ChatUsage,
    progress: &SubchatProgressMessages,
) -> Result<(Vec<ChatMessage>, bool), String> {
    let mut context_limit_compact_count = 0usize;
    let mut empty_choice_retry_count = 0usize;
    for step in 0..config.max_steps {
        if is_aborted(&config.abort_flag) {
            if config.soft_abort {
                return Ok((messages, true));
            }
            return Err("Aborted".to_string());
        }
        stamp_activity(&config.activity_stamp);
        emit_subchat_progress(config, SubchatProgress::Step(step + 1));
        drain_background_agent_inbox(&ccx, &mut messages).await;
        persist_subchat_progress(&ccx, config, progress, &messages).await;

        if config.background_agent_id.is_some() {
            let app = ccx.lock().await.app.clone();
            let chat_id = ccx.lock().await.chat_id.clone();
            let sessions = app.chat.sessions.read().await;
            if let Some(session) = sessions.get(&chat_id) {
                session.lock().await.set_runtime_state(crate::chat::types::SessionState::Generating, None);
            }
        }

        let results = loop {
            match runner_model_turn(
                ccx.clone(),
                config,
                &mut messages,
                tools_policy.to_subset_for_llm(),
                config.prepend_system_prompt && step == 0,
            )
            .await
            {
                Ok(r) => break r,
                Err(ref err)
                    if subchat_retries_allowed(config)
                        && should_compact_context_limit_error(
                            err,
                            context_limit_compact_count,
                            &config.abort_flag,
                        ) =>
                {
                    let original_error = err.clone();
                    let log_error = safe_context_limit_error_for_log(&original_error);
                    context_limit_compact_count += 1;
                    warn!(
                        "Subchat context limit, rebuilding context attempt {}/{}: {}",
                        context_limit_compact_count, MAX_CONTEXT_LIMIT_COMPACT_ATTEMPTS, log_error,
                    );
                    apply_subchat_reactive_compaction(
                        ccx.lock().await.app.gcx.clone(),
                        config,
                        &mut messages,
                        &original_error,
                        context_limit_compact_count,
                        false,
                    )
                    .await;
                }
                Err(ref err)
                    if err.starts_with(EMPTY_CHOICE_ERROR_PREFIX)
                        && subchat_retries_allowed(config)
                        && empty_choice_retry_count < MAX_EMPTY_CHOICE_RETRIES
                        && !is_aborted(&config.abort_flag) =>
                {
                    empty_choice_retry_count += 1;
                    warn!(
                        "Subchat returned no visible content, retrying step ({}/{}): {}",
                        empty_choice_retry_count, MAX_EMPTY_CHOICE_RETRIES, err,
                    );
                }
                Err(err) => {
                    if config.soft_abort && is_abort_error(&err) {
                        return Ok((messages, true));
                    }
                    return Err(err);
                }
            }
        };

        update_usage_from_messages(usage, &results);
        let ctx = ccx.lock().await.app.chat_ctx();
        emit_usage_progress(&ctx, config, &results).await;
        messages = results.into_iter().next().unwrap_or(messages);
        persist_subchat_progress(&ccx, config, progress, &messages).await;

        if has_final_answer(&messages) {
            break;
        }

        if config.background_agent_id.is_some() {
            let app = ccx.lock().await.app.clone();
            let chat_id = ccx.lock().await.chat_id.clone();
            let sessions = app.chat.sessions.read().await;
            if let Some(session) = sessions.get(&chat_id) {
                session.lock().await.set_runtime_state(crate::chat::types::SessionState::ExecutingTools, None);
            }
        }

        messages = execute_pending_tool_calls(
            ccx.clone(),
            &config.model,
            &config.mode,
            messages,
            tools_policy,
            step,
            config.max_steps,
            config.parent_tool_call_id.clone(),
            config.autonomous_no_confirm,
            config.auto_approve_editing_tools,
            config.auto_approve_dangerous_commands,
            config.step_progress.clone(),
        )
        .await?;
        persist_subchat_progress(&ccx, config, progress, &messages).await;

        if is_aborted(&config.abort_flag) {
            if config.soft_abort {
                return Ok((messages, true));
            }
            return Err("Aborted".to_string());
        }
    }

    if needs_forced_final_answer(
        config.final_step_force_answer && subchat_retries_allowed(config),
        is_aborted(&config.abort_flag),
        has_final_answer(&messages),
    ) {
        drain_background_agent_inbox(&ccx, &mut messages).await;
        messages = run_forced_final_answer_turn(
            ccx.clone(),
            config,
            messages,
            &mut context_limit_compact_count,
            usage,
        )
        .await?;
        persist_subchat_progress(&ccx, config, progress, &messages).await;
    }

    Ok((messages, is_aborted(&config.abort_flag)))
}

pub(crate) async fn run_forced_final_answer_turn(
    ccx: Arc<AMutex<AtCommandsContext>>,
    config: &SubchatConfig,
    mut messages: Vec<ChatMessage>,
    context_limit_compact_count: &mut usize,
    usage: &mut ChatUsage,
) -> Result<Vec<ChatMessage>, String> {
    messages.push(final_step_wrap_up_message(config.max_steps));
    let mut empty_choice_retry_count = 0usize;

    let results = loop {
        match runner_model_turn(ccx.clone(), config, &mut messages, Some(vec![]), false).await {
            Ok(r) => break r,
            Err(ref err)
                if subchat_retries_allowed(config)
                    && should_compact_context_limit_error(
                        err,
                        *context_limit_compact_count,
                        &config.abort_flag,
                    ) =>
            {
                let original_error = err.clone();
                let log_error = safe_context_limit_error_for_log(&original_error);
                *context_limit_compact_count += 1;
                warn!(
                    "Subchat forced final answer context limit, rebuilding context attempt {}/{}: {}",
                    *context_limit_compact_count, MAX_CONTEXT_LIMIT_COMPACT_ATTEMPTS, log_error,
                );
                apply_subchat_reactive_compaction(
                    ccx.lock().await.app.gcx.clone(),
                    config,
                    &mut messages,
                    &original_error,
                    *context_limit_compact_count,
                    true,
                )
                .await;
            }
            Err(ref err)
                if err.starts_with(EMPTY_CHOICE_ERROR_PREFIX)
                    && subchat_retries_allowed(config)
                    && empty_choice_retry_count < MAX_EMPTY_CHOICE_RETRIES
                    && !is_aborted(&config.abort_flag) =>
            {
                empty_choice_retry_count += 1;
                warn!(
                    "Subchat forced final answer returned no visible content, retrying ({}/{}): {}",
                    empty_choice_retry_count, MAX_EMPTY_CHOICE_RETRIES, err,
                );
            }
            Err(err) => {
                if config.soft_abort && is_abort_error(&err) {
                    return Ok(messages);
                }
                return Err(err);
            }
        }
    };

    update_usage_from_messages(usage, &results);
    let ctx = ccx.lock().await.app.chat_ctx();
    emit_usage_progress(&ctx, config, &results).await;
    Ok(results.into_iter().next().unwrap_or(messages))
}

pub(crate) async fn run_subchat_with_wrap_up(
    ccx: Arc<AMutex<AtCommandsContext>>,
    config: &SubchatConfig,
    mut messages: Vec<ChatMessage>,
    tools_policy: &ToolsPolicy,
    wrap_up: &WrapUpConfig,
    usage: &mut ChatUsage,
    progress: &SubchatProgressMessages,
) -> Result<(Vec<ChatMessage>, bool), String> {
    let mut step_n = 0;
    let mut context_limit_compact_count = 0usize;
    let mut empty_choice_retry_count = 0usize;

    loop {
        if is_aborted(&config.abort_flag) {
            if config.soft_abort {
                return Ok((messages, true));
            }
            return Err("Aborted".to_string());
        }

        stamp_activity(&config.activity_stamp);
        emit_subchat_progress(config, SubchatProgress::Step(step_n + 1));
        drain_background_agent_inbox(&ccx, &mut messages).await;
        persist_subchat_progress(&ccx, config, progress, &messages).await;

        if has_final_answer(&messages) {
            break;
        }

        let last_message = match messages.last() {
            Some(m) => m,
            None => break,
        };

        if last_message.role == "assistant"
            && last_message
                .tool_calls
                .as_ref()
                .map_or(false, |tc| !tc.is_empty())
        {
            if step_n >= wrap_up.depth {
                break;
            }
            if let Some(msg_usage) = &last_message.usage {
                if msg_usage.prompt_tokens + msg_usage.completion_tokens > wrap_up.tokens_cnt {
                    break;
                }
            }
        }

        let results = loop {
            match runner_model_turn(
                ccx.clone(),
                config,
                &mut messages,
                tools_policy.to_subset_for_llm(),
                config.prepend_system_prompt && step_n == 0,
            )
            .await
            {
                Ok(r) => break r,
                Err(ref err)
                    if subchat_retries_allowed(config)
                        && should_compact_context_limit_error(
                            err,
                            context_limit_compact_count,
                            &config.abort_flag,
                        ) =>
                {
                    let original_error = err.clone();
                    let log_error = safe_context_limit_error_for_log(&original_error);
                    context_limit_compact_count += 1;
                    warn!(
                        "Subchat wrap-up context limit, rebuilding context attempt {}/{}: {}",
                        context_limit_compact_count, MAX_CONTEXT_LIMIT_COMPACT_ATTEMPTS, log_error,
                    );
                    apply_subchat_reactive_compaction(
                        ccx.lock().await.app.gcx.clone(),
                        config,
                        &mut messages,
                        &original_error,
                        context_limit_compact_count,
                        false,
                    )
                    .await;
                }
                Err(ref err)
                    if err.starts_with(EMPTY_CHOICE_ERROR_PREFIX)
                        && subchat_retries_allowed(config)
                        && empty_choice_retry_count < MAX_EMPTY_CHOICE_RETRIES
                        && !is_aborted(&config.abort_flag) =>
                {
                    empty_choice_retry_count += 1;
                    warn!(
                        "Subchat returned no visible content, retrying step ({}/{}): {}",
                        empty_choice_retry_count, MAX_EMPTY_CHOICE_RETRIES, err,
                    );
                }
                Err(err) => {
                    if config.soft_abort && is_abort_error(&err) {
                        return Ok((messages, true));
                    }
                    return Err(err);
                }
            }
        };

        update_usage_from_messages(usage, &results);
        let ctx = ccx.lock().await.app.chat_ctx();
        emit_usage_progress(&ctx, config, &results).await;
        messages = results.into_iter().next().unwrap_or(messages);
        persist_subchat_progress(&ccx, config, progress, &messages).await;

        messages = execute_pending_tool_calls(
            ccx.clone(),
            &config.model,
            &config.mode,
            messages,
            tools_policy,
            step_n,
            config.max_steps,
            config.parent_tool_call_id.clone(),
            config.autonomous_no_confirm,
            config.auto_approve_editing_tools,
            config.auto_approve_dangerous_commands,
            config.step_progress.clone(),
        )
        .await?;
        persist_subchat_progress(&ccx, config, progress, &messages).await;

        step_n += 1;

        if is_aborted(&config.abort_flag) {
            if config.soft_abort {
                return Ok((messages, true));
            }
            return Err("Aborted".to_string());
        }
    }

    if is_aborted(&config.abort_flag) {
        if config.soft_abort {
            return Ok((messages, true));
        }
        return Err("Aborted".to_string());
    }

    messages = execute_pending_tool_calls(
        ccx.clone(),
        &config.model,
        &config.mode,
        messages,
        tools_policy,
        step_n,
        config.max_steps,
        config.parent_tool_call_id.clone(),
        config.autonomous_no_confirm,
        config.auto_approve_editing_tools,
        config.auto_approve_dangerous_commands,
        config.step_progress.clone(),
    )
    .await?;
    persist_subchat_progress(&ccx, config, progress, &messages).await;

    messages.push(ChatMessage::new("user".to_string(), wrap_up.prompt.clone()));
    drain_background_agent_inbox(&ccx, &mut messages).await;
    record_subchat_progress(progress, &messages);

    let final_results = loop {
        match runner_model_turn(ccx.clone(), config, &mut messages, Some(vec![]), false).await {
            Ok(r) => break r,
            Err(ref err)
                if subchat_retries_allowed(config)
                    && should_compact_context_limit_error(
                        err,
                        context_limit_compact_count,
                        &config.abort_flag,
                    ) =>
            {
                let original_error = err.clone();
                let log_error = safe_context_limit_error_for_log(&original_error);
                context_limit_compact_count += 1;
                warn!(
                    "Subchat wrap-up final context limit, rebuilding context attempt {}/{}: {}",
                    context_limit_compact_count, MAX_CONTEXT_LIMIT_COMPACT_ATTEMPTS, log_error,
                );
                apply_subchat_reactive_compaction(
                    ccx.lock().await.app.gcx.clone(),
                    config,
                    &mut messages,
                    &original_error,
                    context_limit_compact_count,
                    true,
                )
                .await;
            }
            Err(err) => {
                if config.soft_abort && is_abort_error(&err) {
                    return Ok((messages, true));
                }
                return Err(err);
            }
        }
    };
    update_usage_from_messages(usage, &final_results);
    let ctx = ccx.lock().await.app.chat_ctx();
    emit_usage_progress(&ctx, config, &final_results).await;

    Ok((
        final_results.into_iter().next().unwrap_or_default(),
        is_aborted(&config.abort_flag),
    ))
}

pub(crate) fn truncate_args(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let boundary = s
        .char_indices()
        .take_while(|(i, _)| *i < max)
        .last()
        .map(|(i, c)| i + c.len_utf8())
        .unwrap_or(0);
    format!("{}…", &s[..boundary])
}

pub(crate) fn emit_subchat_progress(config: &SubchatConfig, progress: SubchatProgress) {
    if let Some(callback) = &config.step_progress {
        callback(progress);
    }
}

pub(crate) async fn emit_usage_progress(
    ctx: &ChatCtx,
    config: &SubchatConfig,
    results: &[Vec<ChatMessage>],
) {
    let usage = results
        .first()
        .and_then(|messages| messages.last())
        .and_then(|message| message.usage.as_ref());
    let Some(usage) = usage else {
        return;
    };
    let tokens_delta = usage.total_tokens as u64;
    if tokens_delta > 0 {
        let cost_delta = match usage.metering_usd.as_ref() {
            Some(metering) => Some(metering.total_usd),
            None => crate::providers::pricing::lookup_model_pricing_for_ctx(ctx, &config.model)
                .await
                .and_then(|pricing| {
                    crate::providers::pricing::compute_cost(usage, &pricing)
                        .map(|metering| metering.total_usd)
                }),
        };
        emit_subchat_progress(
            config,
            SubchatProgress::Usage {
                tokens_delta,
                cost_delta,
            },
        );
    }
}

pub(crate) fn tool_arg_preview(arguments: &str) -> Option<String> {
    static SECRET_ARG_NAME: OnceLock<regex::Regex> = OnceLock::new();
    let value: Value = serde_json::from_str(arguments).ok()?;
    let object = value.as_object()?;
    for (key, value) in object {
        if SECRET_ARG_NAME
            .get_or_init(|| regex::Regex::new("(?i)key|token|secret|password").unwrap())
            .is_match(key)
        {
            continue;
        }
        let scalar = match value {
            Value::String(value) => value.clone(),
            Value::Number(value) => value.to_string(),
            Value::Bool(value) => value.to_string(),
            _ => continue,
        };
        let preview = scalar.split_whitespace().collect::<Vec<_>>().join(" ");
        if !preview.is_empty() {
            return Some(truncate_args(&preview, 40));
        }
    }
    None
}

pub(crate) async fn execute_pending_tool_calls(
    ccx: Arc<AMutex<AtCommandsContext>>,
    model_id: &str,
    mode_id: &str,
    mut messages: Vec<ChatMessage>,
    tools_policy: &ToolsPolicy,
    step_idx: usize,
    max_steps: usize,
    tx_toolid_mb: Option<String>,
    autonomous_no_confirm: bool,
    auto_approve_editing_tools: bool,
    auto_approve_dangerous_commands: bool,
    step_progress: Option<Arc<dyn Fn(SubchatProgress) + Send + Sync>>,
) -> Result<Vec<ChatMessage>, String> {
    let (app, n_ctx, task_meta, worktree, chat_id, root_chat_id) = {
        let cgcx = ccx.lock().await;
        (
            cgcx.app.clone(),
            cgcx.n_ctx,
            cgcx.task_meta.clone(),
            cgcx.execution_scope_worktree(),
            cgcx.chat_id.clone(),
            cgcx.root_chat_id.clone(),
        )
    };
    let last = match messages.last() {
        Some(m) => m,
        None => return Ok(messages),
    };
    let tool_calls = match &last.tool_calls {
        Some(tc) if !tc.is_empty() => tc.clone(),
        _ => return Ok(messages),
    };
    let tool_calls =
        resolve_tool_call_aliases(app.clone(), tool_calls, mode_id, Some(model_id)).await;

    for tool_call in &tool_calls {
        if let Some(callback) = &step_progress {
            callback(SubchatProgress::ToolStarted {
                name: tool_call.function.name.clone(),
                arg_preview: tool_arg_preview(&tool_call.function.arguments),
            });
        }
    }

    let mut allowed: Vec<ChatToolCall> = vec![];
    let mut denied_msgs: Vec<ChatMessage> = vec![];

    for tc in tool_calls.iter() {
        if !tools_policy.allows_tool(&tc.function.name) {
            denied_msgs.push(ChatMessage {
                message_id: Uuid::new_v4().to_string(),
                role: "tool".to_string(),
                tool_call_id: tc.id.clone(),
                tool_failed: Some(true),
                content: ChatContent::SimpleText(format!(
                    "Tool '{}' not allowed in this subchat",
                    tc.function.name
                )),
                ..Default::default()
            });
        } else {
            allowed.push(tc.clone());
        }
    }

    let thread = ThreadParams {
        id: chat_id,
        model: model_id.to_string(),
        mode: mode_id.to_string(),
        context_tokens_cap: Some(n_ctx),
        task_meta,
        worktree,
        root_chat_id: Some(root_chat_id),
        autonomous_no_confirm,
        auto_approve_editing_tools,
        auto_approve_dangerous_commands,
        ..Default::default()
    };

    if let Some(tx_toolid) = &tx_toolid_mb {
        let subchat_tx = ccx.lock().await.subchat_tx.clone();
        let context_files = get_context_files_from_messages(&messages);
        for tc in &allowed {
            let args_truncated = truncate_args(&tc.function.arguments, 200);
            let progress_msg = format!(
                "{}/{}: {}({})",
                step_idx + 1,
                max_steps,
                tc.function.name,
                args_truncated
            );
            let tool_msg = json!({
                "tool_call_id": tx_toolid,
                "subchat_id": progress_msg,
                "attached_files": context_files
            });
            let _ = subchat_tx.lock().await.send(tool_msg);
        }
    }

    let (mut tool_results, _) = execute_tools(
        app,
        &allowed,
        &messages,
        &thread,
        &thread.mode,
        Some(&thread.model),
        ExecuteToolsOptions::default(),
    )
    .await;

    if let Some(callback) = &step_progress {
        callback(SubchatProgress::ToolsFinished);
    }

    for tc in &tool_calls {
        let answered = denied_msgs
            .iter()
            .chain(tool_results.iter())
            .any(|m| m.tool_call_id == tc.id);
        if !answered {
            tool_results.push(ChatMessage {
                message_id: Uuid::new_v4().to_string(),
                role: "tool".to_string(),
                tool_call_id: tc.id.clone(),
                tool_failed: Some(false),
                content: ChatContent::SimpleText("Tool executed with no output.".to_string()),
                ..Default::default()
            });
        }
    }

    messages.extend(denied_msgs);
    messages.extend(tool_results);
    drain_background_agent_inbox(&ccx, &mut messages).await;

    if let Some(tx_toolid) = &tx_toolid_mb {
        let subchat_tx = ccx.lock().await.subchat_tx.clone();
        let context_files = get_context_files_from_messages(&messages);
        if !context_files.is_empty() {
            let tool_msg = json!({
                "tool_call_id": tx_toolid,
                "subchat_id": "/tool:files",
                "attached_files": context_files
            });
            let _ = subchat_tx.lock().await.send(tool_msg);
        }
    }

    Ok(messages)
}

pub(crate) fn stable_stream_chat_id(
    context_chat_id: &str,
    fresh_id: impl FnOnce() -> String,
) -> String {
    let context_chat_id = context_chat_id.trim();
    if context_chat_id.is_empty() {
        fresh_id()
    } else {
        context_chat_id.to_string()
    }
}

pub(crate) async fn subchat_stream(
    ccx: Arc<AMutex<AtCommandsContext>>,
    model_id: &str,
    mode_id: &str,
    messages: Vec<ChatMessage>,
    tools: Vec<ToolDesc>,
    prepend_system_prompt: bool,
    temperature: Option<f32>,
    max_new_tokens: usize,
    reasoning_effort: Option<ReasoningEffort>,
    cache_control: CacheControl,
    only_deterministic_messages: bool,
    progress_tool_call_id: Option<&str>,
    allow_provider_retries: bool,
) -> Result<Vec<Vec<ChatMessage>>, String> {
    let (
        ctx,
        app,
        effective_n_ctx,
        abort_flag,
        activity_stamp,
        task_meta,
        worktree,
        context_chat_id,
        context_root_id,
    ) = {
        let cgcx = ccx.lock().await;
        (
            cgcx.app.chat_ctx(),
            cgcx.app.clone(),
            cgcx.n_ctx,
            cgcx.abort_flag.clone(),
            cgcx.activity_stamp.clone(),
            cgcx.task_meta.clone(),
            cgcx.execution_scope_worktree(),
            cgcx.chat_id.clone(),
            cgcx.root_chat_id.clone(),
        )
    };

    let caps = ctx
        .caps
        .try_load_caps_quickly(0)
        .await
        .map_err(|e| format!("no caps: {:?}", e))?;
    let model_rec = resolve_chat_model(caps, model_id)?;

    let tokenizer_arc = crate::tokens::cached_tokenizer_for_ctx(&ctx, &model_rec.base).await?;
    let t = HasTokenizerAndEot::new(tokenizer_arc);

    let capped_n_ctx = if model_rec.base.n_ctx > 0 {
        effective_n_ctx.min(model_rec.base.n_ctx)
    } else {
        effective_n_ctx
    };

    let stream_chat_id = stable_stream_chat_id(&context_chat_id, || Uuid::new_v4().to_string());
    let stream_root_chat_id = Some(context_root_id).filter(|id| !id.trim().is_empty());

    let meta = ChatMeta {
        chat_id: stream_chat_id.clone(),
        chat_mode: mode_id.to_string(),
        chat_remote: false,
        current_config_file: String::new(),
        context_tokens_cap: Some(capped_n_ctx),
        include_project_info: true,
        request_attempt_id: Uuid::new_v4().to_string(),
        worktree: worktree.clone(),
    };

    let thread = ThreadParams {
        id: stream_chat_id,
        model: model_id.to_string(),
        mode: mode_id.to_string(),
        context_tokens_cap: Some(capped_n_ctx),
        task_meta,
        worktree,
        root_chat_id: stream_root_chat_id,
        ..Default::default()
    };

    let mut parameters = SamplingParameters {
        max_new_tokens,
        temperature,
        n: Some(1),
        reasoning_effort,
        ..Default::default()
    };

    let options = ChatPrepareOptions {
        prepend_system_prompt,
        allow_at_commands: false,
        allow_tool_prerun: false,
        supports_tools: model_rec.supports_tools,
        cache_control,
        ..Default::default()
    };

    if only_deterministic_messages {
        return Ok(vec![messages]);
    }

    let messages_count = messages.len();
    let tools_count = tools.len();
    let mode_for_stats = canonicalize_mode_for_stats(mode_id);

    let (
        stats_chat_id,
        stats_root_chat_id,
        stats_task_id,
        stats_task_role,
        stats_agent_id,
        stats_card_id,
    ) = {
        let cgcx = ccx.lock().await;
        let tm = cgcx.task_meta.as_ref();
        (
            cgcx.chat_id.clone(),
            cgcx.root_chat_id.clone(),
            tm.map(|t| t.task_id.clone()),
            tm.map(|t| t.role.clone()),
            tm.and_then(|t| t.agent_id.clone()),
            tm.and_then(|t| t.card_id.clone()),
        )
    };

    let prepared = prepare_chat_passthrough(
        ctx.clone(),
        ccx.clone(),
        &t,
        messages.clone(),
        &thread,
        model_id,
        mode_id,
        tools,
        &meta,
        &mut parameters,
        &options,
    )
    .await?;

    let t1 = std::time::Instant::now();
    let llm_request = prepared.llm_request;

    let progress_sender: Option<mpsc::UnboundedSender<Value>> = if progress_tool_call_id.is_some() {
        let subchat_tx_arc = ccx.lock().await.subchat_tx.clone();
        let x = Some(subchat_tx_arc.lock().await.clone());
        x
    } else {
        None
    };
    let progress_tool_call_id = progress_tool_call_id.map(|s| s.to_string());

    let mut attempt = 0usize;
    let mut last_retry_reason: Option<String> = None;
    let results = loop {
        attempt += 1;
        let params = StreamRunParams {
            llm_request: llm_request.clone(),
            model_rec: model_rec.base.clone(),
            chat_id: Some(stats_chat_id.clone()),
            allow_websocket: true,
            abort_flag: Some(abort_flag.clone()),
            abort_notify: None,
            supports_tools: model_rec.supports_tools,
            supports_reasoning: model_rec.has_reasoning_support(),
            reasoning_type: model_rec.reasoning_type_string(),
            supports_temperature: model_rec.supports_temperature,
        };

        let mut collector = SubchatProgressCollector::new(
            progress_sender.clone(),
            progress_tool_call_id.clone(),
            activity_stamp.clone(),
        );

        let call_ts_start = chrono::Utc::now().to_rfc3339();
        let call_start = std::time::Instant::now();
        let mut attempt_result: Result<Vec<_>, _> =
            run_llm_stream(app.clone(), params, &mut collector)
                .await
                .and_then(|o| match o {
                    crate::chat::stream_core::LlmStreamOutcome::Choices(c) => Ok(c),
                    crate::chat::stream_core::LlmStreamOutcome::PausedForCacheGuard => {
                        Err(crate::chat::stream_core::LlmStreamError {
                            message: "subchat generation paused by cache guard; retry".to_string(),
                            partial_output_emitted: false,
                        })
                    }
                });
        let duration_ms = call_start.elapsed().as_millis() as u64;
        let call_ts_end = chrono::Utc::now().to_rfc3339();

        let (provider, model_short) = split_model_provider(model_id);
        let mut monitor_error: Option<String> = None;

        match &mut attempt_result {
            Err(error) => {
                let retry_decision = error.retry_decision();
                let retry_reason = retry_decision
                    .is_retryable_transient()
                    .then(|| retry_decision.reason().to_string());
                let retry_attempt = attempt.saturating_sub(1);
                let should_retry =
                    allow_provider_retries && error.should_retry(retry_attempt, &abort_flag);
                if error.partial_output_emitted
                    && !should_retry
                    && !retry_decision.is_context_limit()
                    && !abort_flag.load(Ordering::SeqCst)
                {
                    let original = error.message.clone();
                    let safe_error = partial_output_stream_error_message(&original);
                    warn!("{}", safe_error);
                    error.message = safe_error;
                    monitor_error = Some(error.message.clone());
                }
                let event = LlmCallEvent {
                    id: uuid::Uuid::new_v4().to_string(),
                    ts_start: call_ts_start,
                    ts_end: call_ts_end,
                    duration_ms,
                    chat_id: stats_chat_id.clone(),
                    root_chat_id: Some(stats_root_chat_id.clone()),
                    mode: mode_for_stats.clone(),
                    task_id: stats_task_id.clone(),
                    task_role: stats_task_role.clone(),
                    agent_id: stats_agent_id.clone(),
                    card_id: stats_card_id.clone(),
                    model_id: model_id.to_string(),
                    provider,
                    model: model_short,
                    messages_count,
                    tools_count,
                    max_tokens: max_new_tokens,
                    temperature,
                    success: false,
                    error_message: Some(error.message.chars().take(200).collect()),
                    finish_reason: None,
                    attempt_n: attempt,
                    retry_reason: retry_reason.clone(),
                    prompt_tokens: 0,
                    completion_tokens: 0,
                    cache_read_tokens: None,
                    cache_creation_tokens: None,
                    total_tokens: 0,
                    cost_usd: None,
                };
                if let Some(sender) = &ctx.model.llm_stats_sender {
                    if sender.try_send(event).is_err() {
                        tracing::warn!("stats: channel full, dropping LLM call event");
                    }
                }

                if should_retry {
                    let delay = retry_delay_for_attempt(retry_attempt);
                    let retry_reason_for_log = retry_reason.as_deref().unwrap_or("retryable_error");
                    last_retry_reason = Some(retry_reason_for_log.to_string());
                    warn!(
                        "Retrying subchat generation after retryable LLM error in {}s (attempt {}/{}, reason={})",
                        delay.as_secs(),
                        attempt,
                        MAX_LLM_RETRY_ATTEMPTS,
                        retry_reason_for_log,
                    );
                    if sleep_or_abort(delay, abort_flag.clone()).await {
                        break Err(LlmStreamError::from("aborted".to_string()));
                    }
                    continue;
                }
            }
            Ok(ref results_ok) => {
                let usage = results_ok.first().and_then(|r| r.usage.as_ref());
                let event = LlmCallEvent {
                    id: uuid::Uuid::new_v4().to_string(),
                    ts_start: call_ts_start,
                    ts_end: call_ts_end,
                    duration_ms,
                    chat_id: stats_chat_id.clone(),
                    root_chat_id: Some(stats_root_chat_id.clone()),
                    mode: mode_for_stats.clone(),
                    task_id: stats_task_id.clone(),
                    task_role: stats_task_role.clone(),
                    agent_id: stats_agent_id.clone(),
                    card_id: stats_card_id.clone(),
                    model_id: model_id.to_string(),
                    provider,
                    model: model_short,
                    messages_count,
                    tools_count,
                    max_tokens: max_new_tokens,
                    temperature,
                    success: true,
                    error_message: None,
                    finish_reason: results_ok.first().and_then(|r| r.finish_reason.clone()),
                    attempt_n: attempt,
                    retry_reason: last_retry_reason.clone(),
                    prompt_tokens: usage.map(|u| u.prompt_tokens).unwrap_or(0),
                    completion_tokens: usage.map(|u| u.completion_tokens).unwrap_or(0),
                    cache_read_tokens: usage.and_then(|u| u.cache_read_tokens),
                    cache_creation_tokens: usage.and_then(|u| u.cache_creation_tokens),
                    total_tokens: usage.map(|u| u.total_tokens).unwrap_or(0),
                    cost_usd: usage
                        .and_then(|u| u.metering_usd.as_ref())
                        .map(|m| m.total_usd),
                };
                if let Some(sender) = &ctx.model.llm_stats_sender {
                    if sender.try_send(event).is_err() {
                        tracing::warn!("stats: channel full, dropping LLM call event");
                    }
                }
            }
        }

        if let Some(error_message) = monitor_error {
            if let Some(task_meta) = thread.task_meta.clone() {
                crate::chat::task_agent_monitor::handle_agent_streaming_error(
                    app.clone(),
                    &task_meta,
                    &error_message,
                )
                .await;
            }
        }

        break attempt_result;
    }?;

    info!(
        "stream generation took {:?}ms",
        t1.elapsed().as_millis() as i32
    );

    convert_results_to_messages(results, messages)
}

pub(crate) fn convert_results_to_messages(
    results: Vec<ChoiceFinal>,
    original_messages: Vec<ChatMessage>,
) -> Result<Vec<Vec<ChatMessage>>, String> {
    if results.is_empty() {
        return Err("subchat model returned no output (empty choices)".to_string());
    }

    let mut all_choices = vec![];
    let mut skipped_empty_finish_reason: Option<String> = None;
    for result in results {
        let tool_calls: Option<Vec<_>> = if result.tool_calls_raw.is_empty() {
            None
        } else {
            let parsed: Vec<_> = result
                .tool_calls_raw
                .iter()
                .filter_map(|tc| normalize_tool_call(tc))
                .collect();
            if parsed.is_empty() {
                None
            } else {
                Some(parsed)
            }
        };

        // A thinking model may finish with only reasoning content and no answer
        // text; that is still visible content and must not be discarded.
        let has_reasoning =
            !result.reasoning.trim().is_empty() || !result.thinking_blocks.is_empty();
        if tool_calls.is_none() && result.content.trim().is_empty() && !has_reasoning {
            skipped_empty_finish_reason = Some(
                result
                    .finish_reason
                    .clone()
                    .unwrap_or_else(|| "unknown".to_string()),
            );
            continue;
        }

        let msg = ChatMessage {
            message_id: uuid::Uuid::new_v4().to_string(),
            role: "assistant".to_string(),
            content: ChatContent::SimpleText(result.content),
            tool_calls,
            reasoning_content: if result.reasoning.is_empty() {
                None
            } else {
                Some(result.reasoning)
            },
            thinking_blocks: if result.thinking_blocks.is_empty() {
                None
            } else {
                Some(result.thinking_blocks)
            },
            citations: result.citations,
            finish_reason: result.finish_reason,
            usage: result.usage,
            extra: result.extra,
            ..Default::default()
        };

        let mut extended = original_messages.clone();
        extended.push(msg);
        all_choices.push(extended);
    }

    if all_choices.is_empty() {
        return Err(format!(
            "{EMPTY_CHOICE_ERROR_PREFIX} (finish_reason: {})",
            skipped_empty_finish_reason.unwrap_or_else(|| "unknown".to_string())
        ));
    }

    Ok(all_choices)
}

pub(crate) fn update_usage_from_messages(usage: &mut ChatUsage, messages: &[Vec<ChatMessage>]) {
    if let Some(message_0) = messages.first() {
        if let Some(last_message) = message_0.last() {
            if let Some(u) = last_message.usage.as_ref() {
                usage.total_tokens += u.total_tokens;
                usage.completion_tokens += u.completion_tokens;
                usage.prompt_tokens += u.prompt_tokens;
                if let Some(cache_creation) = u.cache_creation_tokens {
                    *usage.cache_creation_tokens.get_or_insert(0) += cache_creation;
                }
                if let Some(cache_read) = u.cache_read_tokens {
                    *usage.cache_read_tokens.get_or_insert(0) += cache_read;
                }
            }
        }
    }
}

pub(crate) fn aggregate_metering_from_messages(
    _messages: &[ChatMessage],
) -> serde_json::Map<String, serde_json::Value> {
    serde_json::Map::new()
}

pub(crate) async fn subchat_single_internal(
    ccx: Arc<AMutex<AtCommandsContext>>,
    model_id: &str,
    mode_id: &str,
    messages: Vec<ChatMessage>,
    tools_subset: Option<Vec<String>>,
    only_deterministic_messages: bool,
    temperature: Option<f32>,
    max_new_tokens: usize,
    reasoning_effort: Option<ReasoningEffort>,
    cache_control: CacheControl,
    prepend_system_prompt: bool,
    progress_tool_call_id: Option<&str>,
    allow_provider_retries: bool,
) -> Result<Vec<Vec<ChatMessage>>, String> {
    let gcx = {
        let cgcx = ccx.lock().await;
        cgcx.app.gcx.clone()
    };

    let tools: Vec<ToolDesc> = if tools_subset
        .as_ref()
        .is_some_and(|subset| subset.is_empty())
    {
        vec![]
    } else {
        let mut tools_turned_on_by_cmdline = get_available_tools(gcx.clone())
            .await
            .into_iter()
            .map(|tool| tool.tool_description())
            .collect::<Vec<_>>();
        let policy = crate::tools::tools_list::tool_access_policy(gcx.clone()).await;
        if !policy.tool_access.providers.is_empty() {
            let provider = crate::tools::tools_list::provider_of_model(model_id);
            tools_turned_on_by_cmdline
                .retain(|desc| crate::tools::tools_list::mcp_tool_allowed(&policy, provider, desc));
        }

        match tools_subset.as_ref() {
            Some(subset) => tools_turned_on_by_cmdline
                .into_iter()
                .filter(|tool| subset.contains(&tool.name))
                .collect(),
            None => tools_turned_on_by_cmdline,
        }
    };

    subchat_stream(
        ccx.clone(),
        model_id,
        mode_id,
        messages,
        tools,
        prepend_system_prompt,
        temperature,
        max_new_tokens,
        reasoning_effort,
        cache_control,
        only_deterministic_messages,
        progress_tool_call_id,
        allow_provider_retries,
    )
    .await
}
