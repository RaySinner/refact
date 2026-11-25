#[allow(unused_imports)]
use std::sync::Arc;
use tokio::sync::{Mutex as AMutex};
use tracing::warn;
use refact_chat_api::{NUDGE_MARKER, NUDGE_ROLE, SilentTailSignal};
use refact_chat_api::pursuit::{
    arbitrate_pursuit, stamp_pursuit_message, PursuitArbitrationContext, PursuitDecision,
    PursuitRequest, PursuitSource,
};
use refact_runtime_api::ToolCatalogSnapshot;

use refact_runtime_api::app_state::AppState;
use refact_runtime_api::buddy_chat_reactions::{
    maybe_enqueue_chat_activity_reaction, ChatActivityCompletion,
};
use refact_runtime_api::chat_ctx::SubchatLauncher;

use refact_core::call_validation::{ChatContent, ChatMessage};

use refact_chat_api::types::*;
use refact_ext::hooks::HookEvent;
use refact_runtime_api::ext_hooks_runner::{HookPayload, get_project_dir_string, run_hooks};
use refact_chat_api::diagnostics::make_ui_only_error_message;
use refact_chat_history::history_limit::ContextPressure;
use refact_core::llm_types::BaseModelRecord;

use super::support::*;
use super::preamble::*;

pub fn is_claude_code_model(model: &BaseModelRecord) -> bool {
    model.wire_format == refact_llm::WireFormat::AnthropicMessages && !model.auth_token.is_empty()
}

pub async fn completion_activity(session_arc: &Arc<AMutex<ChatSession>>) -> ChatActivityCompletion {
    let session = session_arc.lock().await;
    ChatActivityCompletion {
        chat_id: session.chat_id.clone(),
        thread: session.thread.clone(),
    }
}

pub async fn spawn_completion_activity_reaction(
    app: AppState,
    session_arc: Arc<AMutex<ChatSession>>,
) {
    let activity = completion_activity(&session_arc).await;
    let mut session = session_arc.lock().await;
    if session.closed {
        return;
    }
    let handle = tokio::spawn(async move {
        maybe_enqueue_chat_activity_reaction(app, activity).await;
    });
    session.track_post_turn_task(handle);
}

pub async fn spawn_chat_completion_side_effects(
    app: AppState,
    session_arc: Arc<AMutex<ChatSession>>,
    chat_id: String,
    chat_label: String,
    run_stop_hook: bool,
) {
    let activity = completion_activity(&session_arc).await;
    let mut session = session_arc.lock().await;
    if session.closed {
        return;
    }
    let handle = tokio::spawn(async move {
        if run_stop_hook {
            let project_dir = get_project_dir_string(app.clone()).await;
            let payload = HookPayload {
                hook_event_name: "Stop".to_string(),
                session_id: chat_id.clone(),
                project_dir,
                tool_name: None,
                tool_input: None,
                tool_output: None,
                user_prompt: None,
                extra: std::collections::HashMap::new(),
            };
            run_hooks(app.clone(), HookEvent::Stop, payload).await;
        }

        let mut event = make_runtime_event(
            "chat_completed",
            &format!("Completed: {}", chat_label),
            "chat",
            &format!("chat_{}", chat_id),
            "completed",
            None,
        );
        event.chat_id = Some(chat_id);
        app.buddy_event_sink
            .apply_chat_completion(event, 4, "happy".to_string())
            .await;
        maybe_enqueue_chat_activity_reaction(app, activity).await;
    });
    session.track_post_turn_task(handle);
}

pub async fn ensure_claude_code_identity(
    session_arc: &Arc<AMutex<ChatSession>>,
    model: &BaseModelRecord,
) -> Option<refact_llm::ClaudeCodeIdentity> {
    if !is_claude_code_model(model) {
        return None;
    }

    let mut session = session_arc.lock().await;
    if let Some(identity) = session.thread.claude_code_identity.clone() {
        return Some(identity);
    }

    let identity = refact_llm::adapters::claude_code_compat::generate_claude_code_identity();
    session.thread.claude_code_identity = Some(identity.clone());
    session.increment_version();
    session.touch();
    Some(identity)
}

#[cfg(any(test, feature = "test-util"))]
pub fn ensure_claude_code_identity_for_test(
    session: &mut ChatSession,
    model: &BaseModelRecord,
) -> Option<refact_llm::ClaudeCodeIdentity> {
    if !is_claude_code_model(model) {
        return None;
    }

    if let Some(identity) = session.thread.claude_code_identity.clone() {
        return Some(identity);
    }

    let identity = refact_llm::adapters::claude_code_compat::generate_claude_code_identity();
    session.thread.claude_code_identity = Some(identity.clone());
    session.increment_version();
    session.touch();
    Some(identity)
}

pub fn is_reasoning_token_limit_stop(message: &ChatMessage) -> bool {
    if message.role != "assistant"
        || message
            .tool_calls
            .as_ref()
            .is_some_and(|calls| !calls.is_empty())
    {
        return false;
    }

    let finish_reason = message.finish_reason.as_deref().unwrap_or_default();
    if normalize_stop_reason(finish_reason) != Some(NormalizedStopReason::ProviderLengthStop) {
        return false;
    }

    message.content.content_text_only().trim().is_empty()
        && (message
            .reasoning_content
            .as_deref()
            .is_some_and(|reasoning| !reasoning.trim().is_empty())
            || message
                .thinking_blocks
                .as_ref()
                .is_some_and(|blocks| !blocks.is_empty()))
}

pub fn length_like_finish_reason(finish_reason: Option<&str>) -> Option<NormalizedStopReason> {
    finish_reason.and_then(normalize_stop_reason)
}

pub fn has_empty_or_near_empty_visible_output(message: &ChatMessage) -> bool {
    message.content.content_text_only().trim().chars().count()
        <= LENGTH_STOP_NEAR_EMPTY_VISIBLE_CHARS
}

pub fn is_high_pressure_length_stop(
    message: &ChatMessage,
    messages: &[ChatMessage],
    effective_n_ctx: usize,
    usage_stale: bool,
    image_token_mode: refact_core::provider_types::ImageTokenMode,
) -> bool {
    if message.role != "assistant"
        || message
            .tool_calls
            .as_ref()
            .is_some_and(|calls| !calls.is_empty())
        || length_like_finish_reason(message.finish_reason.as_deref()).is_none()
        || !has_empty_or_near_empty_visible_output(message)
    {
        return false;
    }

    matches!(
        refact_chat_history::summarization::estimated_provider_context_pressure_with_usage_for_image_mode(
            messages,
            effective_n_ctx,
            usage_stale,
            image_token_mode,
        ),
        ContextPressure::High | ContextPressure::Critical
    )
}

pub fn is_length_stop_compression_candidate(message: &ChatMessage) -> bool {
    message.role == "assistant"
        && !message
            .tool_calls
            .as_ref()
            .is_some_and(|calls| !calls.is_empty())
        && length_like_finish_reason(message.finish_reason.as_deref()).is_some()
        && has_empty_or_near_empty_visible_output(message)
}

pub async fn maybe_compact_after_high_pressure_length_stop(
    gcx: Arc<refact_runtime_api::global_context::GlobalContext>,
    session_arc: &Arc<AMutex<ChatSession>>,
    thread: &ThreadParams,
    effective_n_ctx: Option<usize>,
    image_token_mode: refact_core::provider_types::ImageTokenMode,
) -> bool {
    let Some(effective_n_ctx) = effective_n_ctx else {
        let mut session = session_arc.lock().await;
        if matches!(
            session.runtime.state,
            SessionState::Idle | SessionState::Completed
        ) && session
            .messages
            .last()
            .is_some_and(is_length_stop_compression_candidate)
        {
            refact_chat_api::context_rebuild::emit_compression_skipped_status(
                &mut session,
                CompressionReason::EffectiveContextUnknown,
            );
        }
        return false;
    };
    let reason = {
        let session = session_arc.lock().await;
        if !matches!(
            session.runtime.state,
            SessionState::Idle | SessionState::Completed
        ) {
            return false;
        }
        let Some(message) = session.messages.last() else {
            return false;
        };
        if !is_high_pressure_length_stop(
            message,
            &session.messages,
            effective_n_ctx,
            session.provider_usage_stale,
            image_token_mode,
        ) {
            return false;
        }
        length_like_finish_reason(message.finish_reason.as_deref())
            .map(compression_reason_for_stop_reason)
    };

    warn!("High-pressure length stop, rebuilding active context");
    let outcome = refact_runtime_api::chat_context_rebuild::apply_context_rebuild_with_reason(
        gcx,
        session_arc,
        thread,
        true,
        reason,
    )
    .await;
    if outcome.applied() {
        let mut session = session_arc.lock().await;
        session.thread.previous_response_id = None;
        session.reset_cache_guard_snapshot();
        return true;
    }

    false
}

pub const LENGTH_STOP_CONTINUE_MARKER: &str = "length_stop_continue";
pub const MAX_LENGTH_STOP_RECOVERY_ATTEMPTS: usize = 2;
pub const LENGTH_STOP_BOOSTED_MAX_NEW_TOKENS: usize = 16_000;

/// Tool an agent is told to call when it ends a turn with plain text. It is
/// only ever named after `chat_exposes_finish_tool` confirmed the running mode
/// actually offers it, so the nudge never asks for a tool the model cannot see.
pub const SILENT_TAIL_FINISH_TOOL: &str = "agent_finish";
/// How many times a single turn may be nudged back after a silent tail. After
/// the last attempt the branch falls through to its normal `break` and the stall
/// monitor escalates.
pub const MAX_SILENT_TAIL_NUDGE_ATTEMPTS: u32 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LengthStopKind {
    EmptyOutput,
    PartialOutput,
}

pub fn length_stop_kind(message: &ChatMessage) -> Option<LengthStopKind> {
    if message.role != "assistant"
        || message
            .tool_calls
            .as_ref()
            .is_some_and(|calls| !calls.is_empty())
        || length_like_finish_reason(message.finish_reason.as_deref())
            != Some(NormalizedStopReason::ProviderLengthStop)
    {
        return None;
    }
    if has_empty_or_near_empty_visible_output(message) {
        Some(LengthStopKind::EmptyOutput)
    } else {
        Some(LengthStopKind::PartialOutput)
    }
}

pub fn length_stop_recovery_attempts(messages: &[ChatMessage]) -> usize {
    let start = messages
        .iter()
        .rposition(|message| message.role == "user")
        .map_or(0, |idx| idx + 1);
    messages[start..]
        .iter()
        .filter(|message| {
            message.role == "cd_instruction" && message.tool_call_id == LENGTH_STOP_CONTINUE_MARKER
        })
        .count()
}

pub fn trailing_token_budget_marker_index(messages: &[ChatMessage]) -> Option<usize> {
    messages
        .last()
        .is_some_and(|message| {
            message.role == "cd_instruction" && message.tool_call_id == TOKEN_BUDGET_MARKER
        })
        .then_some(messages.len().saturating_sub(1))
}

pub fn length_stop_continue_instruction(kind: LengthStopKind) -> ChatMessage {
    let text = match kind {
        LengthStopKind::EmptyOutput => {
            " The previous response was cut off by the output token limit before any visible output was produced. Respond again, keep internal reasoning brief, and produce the answer directly."
        }
        LengthStopKind::PartialOutput => {
            " The previous message was cut off by the output token limit. Continue exactly where it stopped; do not repeat content that was already produced."
        }
    };
    ChatMessage {
        role: "cd_instruction".to_string(),
        tool_call_id: LENGTH_STOP_CONTINUE_MARKER.to_string(),
        content: ChatContent::SimpleText(text.to_string()),
        ..Default::default()
    }
}

pub fn silent_tail_nudge_attempts(messages: &[ChatMessage]) -> u32 {
    let start = messages
        .iter()
        .rposition(|message| message.role == "user")
        .map_or(0, |idx| idx + 1);
    messages[start..]
        .iter()
        .filter(|message| message.role == NUDGE_ROLE && message.tool_call_id == NUDGE_MARKER)
        .count() as u32
}

pub fn is_task_agent_chat(thread: &ThreadParams) -> bool {
    thread
        .task_meta
        .as_ref()
        .is_some_and(|meta| meta.role == "agents")
}

pub fn chat_exposes_finish_tool(catalog: Option<&ToolCatalogSnapshot>) -> bool {
    catalog.is_some_and(|catalog| {
        catalog
            .index
            .tools
            .iter()
            .any(|tool| tool.name == SILENT_TAIL_FINISH_TOOL)
    })
}

/// Pure decision for the silent-tail nudge: the instruction to insert, or `None`
/// when this turn must keep going through its normal `break`.
///
/// Every gate lives here so the behaviour is testable without an LLM: the flag
/// must be on, the chat must be a task agent, the running mode must actually
/// expose the finish tool, the model must have said something, and this turn
/// must still be under `max_attempts` nudges.
pub fn decide_silent_tail_nudge(
    messages: &[ChatMessage],
    nudge_enabled: bool,
    is_agent_chat: bool,
    has_finish_tool: bool,
    max_attempts: u32,
    last_response_text: &str,
    model: Option<&str>,
) -> Option<String> {
    if !nudge_enabled || !is_agent_chat || !has_finish_tool {
        return None;
    }
    if last_response_text.trim().is_empty() {
        return None;
    }
    let signal = SilentTailSignal {
        attempts: silent_tail_nudge_attempts(messages),
        max_attempts,
        last_response_text: last_response_text.to_string(),
        model: model.map(str::to_string),
    };
    signal
        .should_nudge()
        .then(|| signal.nudge_message(SILENT_TAIL_FINISH_TOOL))
}

/// What the silent-tail check decided for the current `NoToolCalls` step.
pub enum SilentTailNudge {
    /// A nudge was inserted; the turn continues.
    Nudged,
    /// Attempts are spent; the branch falls through to its normal `break` and
    /// the stall monitor escalates.
    Exhausted,
    /// The nudge does not apply here at all.
    NotApplicable,
}

pub async fn maybe_nudge_after_silent_tail(
    app: AppState,
    session_arc: &Arc<AMutex<ChatSession>>,
) -> SilentTailNudge {
    let (messages, thread, catalog, last_response_text) = {
        let session = session_arc.lock().await;
        // Reasoning models can finish a turn with empty visible content.
        // Passing an empty string here would make `decide_silent_tail_nudge`
        // report `NotApplicable` and the turn breaks out of `start_generation`
        // silently. Fall back to the internal reasoning so the nudge path can
        // ask the model to keep going (e.g. call `agent_finish`) instead of
        // abandoning a turn that was thinking but said nothing visible.
        let last_response_text = session
            .messages
            .last()
            .map(|message| {
                let content = message.content.content_text_only();
                if !content.trim().is_empty() {
                    content
                } else if let Some(reasoning) = message
                    .reasoning_content
                    .as_deref()
                    .filter(|reasoning| !reasoning.trim().is_empty())
                {
                    reasoning.to_string()
                } else if message
                    .thinking_blocks
                    .as_ref()
                    .is_some_and(|blocks| !blocks.is_empty())
                {
                    // Synthetic marker: `decide_silent_tail_nudge` only needs to
                    // know the model actually spoke/reasoned this turn.
                    "model performed internal reasoning".to_string()
                } else {
                    content
                }
            })
            .unwrap_or_default();
        (
            session.messages.clone(),
            session.thread.clone(),
            session.tool_catalog.clone(),
            last_response_text,
        )
    };
    let model = (!thread.model.is_empty()).then_some(thread.model.as_str());
    let nudge_enabled = thread.agent_nudge_enabled_effective();
    let is_agent_chat = is_task_agent_chat(&thread);
    let has_finish_tool = chat_exposes_finish_tool(catalog.as_deref());
    let attempts = silent_tail_nudge_attempts(&messages);

    let Some(text) = decide_silent_tail_nudge(
        &messages,
        nudge_enabled,
        is_agent_chat,
        has_finish_tool,
        MAX_SILENT_TAIL_NUDGE_ATTEMPTS,
        &last_response_text,
        model,
    ) else {
        let exhausted = nudge_enabled
            && is_agent_chat
            && has_finish_tool
            && !last_response_text.trim().is_empty()
            && attempts >= MAX_SILENT_TAIL_NUDGE_ATTEMPTS;
        if exhausted {
            let (task_id, card_id, chat_id) = {
                let session = session_arc.lock().await;
                let meta = session.thread.task_meta.as_ref();
                (
                    meta.map(|meta| meta.task_id.clone()).unwrap_or_default(),
                    meta.and_then(|meta| meta.card_id.clone())
                        .unwrap_or_default(),
                    session.chat_id.clone(),
                )
            };
            warn!(
                "silent_tail: nudge exhausted after {} attempt(s); card={} task={} chat={} model={:?}",
                attempts,
                card_id,
                task_id,
                chat_id,
                model,
            );
            return SilentTailNudge::Exhausted;
        }
        return SilentTailNudge::NotApplicable;
    };

    {
        let mut session = session_arc.lock().await;
        let request = PursuitRequest::new(
            PursuitSource::SilentTail,
            "silent_tail",
            session.event_seq,
            true,
        );
        if arbitrate_pursuit(
            &session,
            &request,
            PursuitArbitrationContext::new(epoch_ms_now()),
        ) != PursuitDecision::StartTurn
        {
            return SilentTailNudge::NotApplicable;
        }
        let goal_revision = session.goal.as_ref().map(|goal| goal.revision).unwrap_or(0);
        let mut message = ChatMessage {
            role: NUDGE_ROLE.to_string(),
            tool_call_id: NUDGE_MARKER.to_string(),
            content: ChatContent::SimpleText(text),
            ..Default::default()
        };
        stamp_pursuit_message(&mut message, &request, goal_revision);
        session.add_message(message);
        session.goal_mark_nudge_pending();
        session.goal_record_nudge(epoch_ms_now());
    }
    maybe_save_trajectory_with_intent(app, session_arc.clone(), TrajectoryCommitIntent::Checkpoint)
        .await;
    SilentTailNudge::Nudged
}

pub async fn maybe_recover_after_length_stop(
    gcx: Arc<refact_runtime_api::global_context::GlobalContext>,
    session_arc: &Arc<AMutex<ChatSession>>,
    thread: &ThreadParams,
    effective_n_ctx: Option<usize>,
    image_token_mode: refact_core::provider_types::ImageTokenMode,
) -> bool {
    let (kind, dead_end_message_id, attempts, trailing_budget_marker_id) = {
        let session = session_arc.lock().await;
        if !matches!(
            session.runtime.state,
            SessionState::Idle | SessionState::Completed
        ) {
            return false;
        }
        let last_idx = trailing_token_budget_marker_index(&session.messages)
            .unwrap_or(session.messages.len())
            .saturating_sub(1);
        let Some(last) = session.messages.get(last_idx) else {
            return false;
        };
        let Some(kind) = length_stop_kind(last) else {
            return false;
        };
        (
            kind,
            last.message_id.clone(),
            length_stop_recovery_attempts(&session.messages),
            trailing_token_budget_marker_index(&session.messages)
                .and_then(|idx| session.messages.get(idx))
                .map(|message| message.message_id.clone()),
        )
    };

    if attempts >= MAX_LENGTH_STOP_RECOVERY_ATTEMPTS {
        let mut session = session_arc.lock().await;
        session.add_message(make_ui_only_error_message(
            "Generation stopped by the output token limit repeatedly; automatic retries exhausted. Send a message to continue.",
        ));
        return false;
    }

    let compacted = maybe_compact_after_high_pressure_length_stop(
        gcx.clone(),
        session_arc,
        thread,
        effective_n_ctx,
        image_token_mode,
    )
    .await;

    if thread.max_tokens.is_some() && kind == LengthStopKind::PartialOutput {
        if !compacted {
            return false;
        }
        let mut session = session_arc.lock().await;
        session.add_message(length_stop_continue_instruction(kind));
        warn!(
            "Recovering from {:?} length stop after compaction (attempt {}/{})",
            kind,
            attempts + 1,
            MAX_LENGTH_STOP_RECOVERY_ATTEMPTS,
        );
        return true;
    }

    let mut session = session_arc.lock().await;
    if let Some(marker_id) = trailing_budget_marker_id.as_deref() {
        session.remove_message(marker_id);
    }
    if kind == LengthStopKind::EmptyOutput {
        if !dead_end_message_id.is_empty() {
            session.remove_message(&dead_end_message_id);
        } else if session
            .messages
            .last()
            .is_some_and(|message| length_stop_kind(message) == Some(LengthStopKind::EmptyOutput))
        {
            session.messages.pop();
            session.increment_version();
            session.touch();
        }
    }
    if thread.max_tokens.is_none() {
        session.pending_max_new_tokens_boost = Some(LENGTH_STOP_BOOSTED_MAX_NEW_TOKENS);
    }
    session.add_message(length_stop_continue_instruction(kind));
    warn!(
        "Recovering from {:?} length stop (attempt {}/{}, compacted={})",
        kind,
        attempts + 1,
        MAX_LENGTH_STOP_RECOVERY_ATTEMPTS,
        compacted,
    );
    true
}

pub fn should_notify_task_agent_reasoning_token_stop(
    message: &ChatMessage,
    messages: &[ChatMessage],
    effective_n_ctx: Option<usize>,
    usage_stale: bool,
    image_token_mode: refact_core::provider_types::ImageTokenMode,
) -> bool {
    if !is_reasoning_token_limit_stop(message) {
        return false;
    }

    !effective_n_ctx.is_some_and(|n_ctx| {
        is_high_pressure_length_stop(message, messages, n_ctx, usage_stale, image_token_mode)
    })
}

pub async fn handle_task_agent_reasoning_token_stop(
    app: AppState,
    session_arc: Arc<AMutex<ChatSession>>,
    effective_n_ctx: Option<usize>,
    image_token_mode: refact_core::provider_types::ImageTokenMode,
) -> bool {
    let (task_meta, finish_reason, usage, message_id, agent_chat_id) = {
        let mut session = session_arc.lock().await;
        let Some(meta) = session.thread.task_meta.clone() else {
            return false;
        };
        if meta.role != "agents" {
            return false;
        }
        let Some(message) = session.messages.last() else {
            return false;
        };
        if !should_notify_task_agent_reasoning_token_stop(
            message,
            &session.messages,
            effective_n_ctx,
            session.provider_usage_stale,
            image_token_mode,
        ) {
            return false;
        }

        let finish_reason = message
            .finish_reason
            .clone()
            .unwrap_or_else(|| "length".to_string());
        let usage = message.usage.clone();
        let message_id = message.message_id.clone();
        let agent_chat_id = session.chat_id.clone();
        session.set_runtime_state(SessionState::Completed, None);
        (meta, finish_reason, usage, message_id, agent_chat_id)
    };

    maybe_save_trajectory_with_intent(
        app.clone(),
        session_arc.clone(),
        TrajectoryCommitIntent::Checkpoint,
    )
    .await;

    if let Err(error) = crate::chat_task_agent_monitor::handle_agent_reasoning_token_limit_stop(
        app.chat_ctx(),
        task_meta,
        finish_reason,
        usage,
        message_id,
        agent_chat_id,
    )
    .await
    {
        tracing::warn!("failed to notify planner about task agent reasoning token stop: {error}");
    }
    true
}

pub async fn run_fork_subchat(
    app: AppState,
    agent_name: &str,
    user_content: &str,
    thread: &ThreadParams,
    parent_chat_id: &str,
) -> Result<String, String> {
    let config = app
        .ctx
        .subchat
        .resolve_subchat_config_with_parent(
            agent_name,
            false,
            None,
            Some(format!("Fork: {}", agent_name)),
            Some(parent_chat_id.to_string()),
            Some("fork".to_string()),
            thread.root_chat_id.clone(),
            None,
            10,
            true,
            None,
            thread.mode.clone(),
            thread.task_meta.clone(),
            thread.worktree.clone(),
            None,
            None,
            None,
            0,
        )
        .await?;

    let messages = vec![ChatMessage {
        role: "user".to_string(),
        content: ChatContent::SimpleText(user_content.to_string()),
        ..Default::default()
    }];

    let result = app.ctx.subchat.run_subchat(messages, config).await?;

    let last_assistant = result.messages.iter().rev().find(|m| m.role == "assistant");
    Ok(last_assistant
        .map(|m| m.content.content_text_only())
        .unwrap_or_else(|| "Fork skill completed but produced no response.".to_string()))
}
