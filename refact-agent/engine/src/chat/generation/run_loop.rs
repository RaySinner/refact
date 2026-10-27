#[allow(unused_imports)]
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;
use serde_json::json;
use tokio::sync::{Mutex as AMutex};
use tracing::{info, warn};
use uuid::Uuid;
use refact_buddy_core::types::BuddyRuntimeEvent;
use refact_chat_api::{NUDGE_MARKER, NUDGE_ROLE, SilentTailSignal};
use refact_runtime_api::ToolCatalogSnapshot;

use crate::app_state::AppState;
use crate::buddy::chat_reactions::{maybe_enqueue_chat_activity_reaction, ChatActivityCompletion};
use crate::subchat::{resolve_subchat_config_with_parent, run_subchat};

use crate::at_commands::at_commands::AtCommandsContext;
use crate::call_validation::{
    ChatContent, ChatMessage, ChatMeta, ChatUsage, MeteringUsd, SamplingParameters,
    is_agentic_mode_id,
};
use crate::stats::event::{LlmCallEvent, canonicalize_mode_for_stats, split_model_provider};
use crate::chat::tool_call_recovery;
use crate::chat::tool_call_recovery_oss;
use crate::llm::LlmRequest;
use crate::llm::params::CacheControl;
use crate::scratchpad_abstract::HasTokenizerAndEot;
use crate::constants::CHAT_TOP_N;
use crate::knowledge::enrichment::{
    enrich_messages_with_knowledge, enrichment_identity_from_context, enrichment_query_fingerprint,
    record_enrichment_identity_on_user_message,
};
use crate::chat::perf_diagnostics::{self, PerfComponent, PerfOutcome};

use crate::chat::goal_monitor::handle_goal_turn_end;
use crate::chat::types::*;
use crate::chat::trajectories::{
    check_external_reload_pending, ensure_frozen_prefix, first_system_prompt,
    frozen_prefix_is_complete, maybe_save_trajectory_background_with_intent,
};
use crate::chat::tools::{process_tool_calls_once, ToolStepOutcome};
use crate::chat::prepare::{
    build_canonical_openai_tools_from_aliases, prepare_chat_passthrough, ChatPrepareOptions,
};
use crate::chat::prompts::prepend_the_right_system_prompt_and_maybe_more_initial_messages;
use crate::chat::stream_core::{
    StreamRunParams, StreamCollector, normalize_tool_call, ChoiceFinal, LlmStreamError,
    LlmStreamOutcome, ABORT_ERROR_MESSAGE,
};
use crate::chat::config::tokens;
use crate::ext::hooks::HookEvent;
use crate::ext::hooks_runner::{HookPayload, get_project_dir_string, run_hooks};
use crate::chat::diagnostics::{
    make_ui_only_error_message, make_ui_only_retry_status_message, safe_provider_error_diagnostic,
};
use crate::chat::history_limit::ContextPressure;
use refact_core::llm_types::BaseModelRecord;
use refact_core::provider_types::{ModelTypeDefaults, ProviderDefaults};

use super::support::*;
use super::retry::*;
use super::preamble::*;
use super::recovery::*;
use super::stream::*;

pub fn start_generation(
    app: AppState,
    session_arc: Arc<AMutex<ChatSession>>,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> {
    Box::pin(async move {
        let gcx = app.gcx.clone();
        let mut network_retry_attempt = 0usize;
        // A failed reactive compaction in a previous turn must not permanently
        // disable the model-based rebuild: the breaker only resets after a
        // successful generation, so clear it once per fresh generation run.
        {
            let mut session = session_arc.lock().await;
            if session.thread.reactive_compact_attempts.take().is_some() {
                session.increment_version();
                session.touch();
            }
        }
        // Marks the whole turn (assistant plus the multi-step tool loop) as
        // in-flight, so `when_idle` deliveries do not land on a transient Idle.
        let mut turn_guard = TurnDepthGuard::enter(app.clone(), session_arc.clone()).await;
        loop {
            match crate::chat::context_rebuild::drain_pending(gcx.clone(), &session_arc).await {
                Ok(true) => break,
                Err(error) => {
                    let mut session = session_arc.lock().await;
                    session.append_error_message_deduped(&format!(
                        "Context operation failed: {}",
                        safe_provider_error_diagnostic(&error)
                    ));
                    session.set_runtime_state(SessionState::Error, Some(error));
                    break;
                }
                Ok(false) => {}
            }
            {
                let mut session = session_arc.lock().await;
                if let Err(error) = refact_core::active_context::active_context(&session.messages) {
                    let error = format!("Invalid active context: {error}. Explicit rebuild required before generation");
                    session.append_error_message_deduped(&error);
                    session.set_runtime_state(SessionState::Error, Some(error));
                    break;
                }
            }
            if inject_queued_user_messages_before_llm_if_safe(app.clone(), session_arc.clone())
                .await
            {
                continue;
            }
            // `append` deliveries land here: before the next LLM request, with
            // the previous assistant/tool window closed.
            crate::chat::delivery::drain_deliveries_at_boundary(app.clone(), session_arc.clone())
                .await;

            let (mut thread, chat_id) = {
                let session = session_arc.lock().await;
                (session.thread.clone(), session.chat_id.clone())
            };
            {
                let session = session_arc.lock().await;
                if let Some(ref m) = session.active_command.model_override {
                    if !m.is_empty() {
                        thread.model = m.clone();
                    }
                }
            }

            let chat_label = {
                let t = thread.title.trim().to_string();
                if t.is_empty() || t == "New Chat" {
                    "Untitled chat".to_string()
                } else {
                    t.chars().take(60).collect()
                }
            };

            let fork_agent_name = {
                let session = session_arc.lock().await;
                session.active_command.context_fork.clone()
            };

            if let Some(agent_name) = fork_agent_name {
                let user_content_opt = {
                    let session = session_arc.lock().await;
                    refact_core::active_context::active_context(&session.messages)
                        .ok()
                        .and_then(|view| {
                            view.messages
                                .into_iter()
                                .rev()
                                .find(|message| message.role == "user")
                                .map(|message| message.content.content_text_only())
                        })
                };
                {
                    let mut session = session_arc.lock().await;
                    session.active_command.context_fork = None;
                }
                let user_content = match user_content_opt {
                    Some(c) => c,
                    None => {
                        warn!(
                            "Fork skill '{}' skipped: no user message found in session {}",
                            agent_name, chat_id
                        );
                        continue;
                    }
                };

                let fork_result =
                    run_fork_subchat(app.clone(), &agent_name, &user_content, &thread, &chat_id)
                        .await;

                match fork_result {
                    Ok(assistant_content) => {
                        let mut session = session_arc.lock().await;
                        session.add_message(ChatMessage {
                            role: "assistant".to_string(),
                            content: ChatContent::SimpleText(assistant_content),
                            ..Default::default()
                        });
                        session.set_runtime_state(SessionState::Idle, None);
                        drop(session);
                        spawn_completion_activity_reaction(app.clone(), session_arc.clone()).await;
                        maybe_save_trajectory_with_intent(
                            app.clone(),
                            session_arc.clone(),
                            TrajectoryCommitIntent::Checkpoint,
                        )
                        .await;
                        break;
                    }
                    Err(e) => {
                        warn!(
                            "Fork skill subchat failed ({}), falling back to normal generation",
                            e
                        );
                        continue;
                    }
                }
            }

            // Resolve before the very first proactive check, with the command's
            // actual model override applied. Failed resolution remains pending.
            if thread.auto_compression_cap_pending {
                if let Ok(caps) =
                    app.ctx.caps.try_load_caps_quickly(0).await
                {
                    if let Ok(model) = crate::caps::resolve_chat_model(caps, &thread.model) {
                        let window = if model.base.n_ctx > 0 {
                            model.base.n_ctx
                        } else {
                            tokens().default_n_ctx
                        };
                        resolve_session_compression_cap(&app, &session_arc, &mut thread, window)
                            .await;
                    }
                }
            }

            if matches!(
                crate::chat::context_rebuild::apply_context_rebuild_with_reason(
                    gcx.clone(),
                    &session_arc,
                    &thread,
                    false,
                    None,
                )
                .await,
                crate::chat::context_rebuild::CompactionOutcome::LlmUnavailable
            ) {
                if crate::chat::context_rebuild::apply_deterministic_compaction_for_recovery(
                    gcx.clone(),
                    &session_arc,
                )
                .await
                {
                    warn!("Auto-compaction LLM unavailable; deterministic full sweep applied, continuing");
                } else {
                    break;
                }
            }

            thread = {
                let session = session_arc.lock().await;
                let mut refreshed = session.thread.clone();
                if let Some(model) = session
                    .active_command
                    .model_override
                    .as_ref()
                    .filter(|m| !m.is_empty())
                {
                    refreshed.model = model.clone();
                }
                refreshed
            };

            // Rebuilds and handoffs replace the conversation but not the preamble.
            ensure_session_preamble(app.clone(), session_arc.clone()).await;

            if user_stop_requested(&session_arc).await {
                break;
            }

            let (abort_flag, abort_notify) = {
                let mut session = session_arc.lock().await;
                // Re-check the blocker here so the notice can name it. `start_stream`
                // refuses on the same predicate, so this cannot race into a
                // different cause.
                let refusal = session.stream_start_refusal();
                match session.start_stream() {
                    Some((_message_id, abort_flag)) => {
                        session.delivery_wake_sources.clear();
                        let notify = session.abort_notify.clone();
                        (abort_flag, notify)
                    }
                    None => {
                        let refusal =
                            refusal.expect("start_stream refuses only via this predicate");
                        warn!(
                            "Cannot start generation for {}: {}",
                            chat_id,
                            refusal.as_str()
                        );
                        session.append_stream_start_refusal(&refusal);
                        break;
                    }
                }
            };
            let stream_prepare_started_at = perf_diagnostics::is_enabled().then(Instant::now);

            {
                let mut ev = make_runtime_event(
                    "chat_started",
                    &format!("Started: {}", chat_label),
                    "chat",
                    &format!("chat_{}", chat_id),
                    "started",
                    None,
                );
                ev.chat_id = Some(chat_id.to_string());
                app.buddy_event_sink.enqueue_event(ev).await;
                let mut ev = make_runtime_event(
                    "streaming",
                    &format!("Generating reply in '{}'", chat_label),
                    "chat",
                    &format!("chat_{}", chat_id),
                    "streaming",
                    None,
                );
                ev.speech_text = Some(format!("Working on your request in '{}'...", chat_label));
                ev.scene = Some("working".to_string());
                ev.persistent = true;
                ev.chat_id = Some(chat_id.to_string());
                app.buddy_event_sink.enqueue_event(ev).await;
            }

            let generation_result = run_llm_generation(
                app.clone(),
                session_arc.clone(),
                thread.clone(),
                chat_id.clone(),
                abort_flag.clone(),
                abort_notify.clone(),
                stream_prepare_started_at,
            )
            .await;

            if let Ok(GenerationResult::PausedForUserDecision) = generation_result {
                maybe_save_trajectory_with_intent(
                    app.clone(),
                    session_arc.clone(),
                    TrajectoryCommitIntent::Checkpoint,
                )
                .await;
                break;
            }

            if let Err(mut error) = generation_result {
                let retry_decision = error.retry_decision();
                let should_retry_network = error.should_retry(network_retry_attempt, &abort_flag);
                let retry_reason = retry_decision.reason();
                if should_retry_network {
                    let delay =
                        crate::chat::retry_policy::retry_delay_for_attempt(network_retry_attempt);
                    network_retry_attempt += 1;
                    {
                        let mut session = session_arc.lock().await;
                        if !session.abort_flag.load(Ordering::SeqCst) {
                            session.clear_stream_for_retry();
                            let retry_msg = make_ui_only_retry_status_message(
                                &error.message,
                                network_retry_attempt,
                                crate::chat::retry_policy::MAX_LLM_RETRY_ATTEMPTS,
                                delay.as_secs(),
                            );
                            session.add_message(retry_msg);
                        }
                    }
                    {
                        let mut ev = make_runtime_event(
                            "chat_retrying",
                            &format!(
                                "Retrying '{}' in {}s (attempt {}/{})",
                                chat_label,
                                delay.as_secs(),
                                network_retry_attempt,
                                crate::chat::retry_policy::MAX_LLM_RETRY_ATTEMPTS,
                            ),
                            "chat",
                            &format!("chat_{}", chat_id),
                            "retrying",
                            None,
                        );
                        ev.chat_id = Some(chat_id.to_string());
                        ev.persistent = true;
                        app.buddy_event_sink.enqueue_event(ev).await;
                    }
                    warn!(
                        "Retrying chat generation after retryable LLM error in {}s (attempt {}/{}, reason={})",
                        delay.as_secs(),
                        network_retry_attempt,
                        crate::chat::retry_policy::MAX_LLM_RETRY_ATTEMPTS,
                        retry_reason,
                    );
                    if crate::chat::retry_policy::sleep_or_abort(delay, abort_flag.clone()).await {
                        break;
                    }
                    continue;
                }
                let is_context_limit =
                    error.retry_decision().is_context_limit() && !abort_flag.load(Ordering::SeqCst);
                if is_context_limit {
                    let original_error = error.message.clone();
                    let round = {
                        let mut session = session_arc.lock().await;
                        session.finish_stream_for_rebuild();
                        let round = session
                            .thread
                            .reactive_compact_attempts
                            .unwrap_or(0)
                            .saturating_add(1);
                        session.thread.reactive_compact_attempts = Some(round);
                        session.increment_version();
                        session.touch();
                        round
                    };
                    if round == 1
                        && thread.auto_compact_enabled_effective()
                        && crate::chat::context_rebuild::apply_context_rebuild_with_reason(
                            gcx.clone(),
                            &session_arc,
                            &thread,
                            true,
                            Some(CompressionReason::ContextLengthStop),
                        )
                        .await
                        .applied()
                    {
                        continue;
                    }
                    if thread.auto_compact_enabled_effective()
                        && crate::chat::context_rebuild::apply_deterministic_compaction_for_recovery(
                            gcx.clone(),
                            &session_arc,
                        )
                        .await
                    {
                        warn!("Context limit reached and model reconstruction unavailable; deterministic full sweep applied, retrying");
                        continue;
                    }
                    error.message = context_limit_final_error_message(&original_error);
                    {
                        let mut session = session_arc.lock().await;
                        crate::chat::goal_monitor::mark_goal_blocked_on_context_limit(&mut session);
                    }
                }

                if error.partial_output_emitted && !abort_flag.load(Ordering::SeqCst) {
                    let original = error.message.clone();
                    let safe_error = partial_output_stream_error_message(&original);
                    warn!("{}", safe_error);
                    error.message = safe_error;
                }

                let error_message = error.message;

                let task_meta_opt = {
                    let mut session = session_arc.lock().await;
                    if !session.abort_flag.load(Ordering::SeqCst) {
                        let app2 = app.clone();
                        let err_clone = error_message.clone();
                        let chat_id2 = chat_id.clone();
                        let chat_label2 = chat_label.clone();
                        let model2 = session.thread.model.clone();
                        tokio::spawn(async move {
                            let model_ref = Some(model2.as_str()).filter(|m| !m.is_empty());
                            app2.buddy_event_sink
                                .report_error_with_model(
                                    "llm_error",
                                    &err_clone,
                                    Some("chat/generation.rs"),
                                    Some(&chat_id2),
                                    model_ref,
                                )
                                .await;
                            let short_err: String = err_clone.chars().take(60).collect();
                            let priority =
                                refact_buddy_core::diagnostics::diagnostic_priority_label(
                                    &err_clone,
                                );
                            let mut ev = make_runtime_event(
                                "chat_error",
                                &format!("Error in '{}': {}", chat_label2, short_err),
                                "chat",
                                &format!("chat_{}", chat_id2),
                                "failed",
                                Some(priority),
                            );
                            ev.chat_id = Some(chat_id2.to_string());
                            app2.buddy_event_sink.mark_chat_error(ev).await;
                        });
                        session.finish_stream_with_error(error_message);
                    }
                    session.thread.task_meta.clone()
                };

                maybe_save_trajectory_with_intent(
                    app.clone(),
                    session_arc.clone(),
                    TrajectoryCommitIntent::Checkpoint,
                )
                .await;

                turn_guard.release().await;
                if let Some(task_meta) = task_meta_opt {
                    let error_msg = {
                        let session = session_arc.lock().await;
                        session.task_agent_error.clone()
                    };
                    if let Some(error) = error_msg {
                        crate::chat::task_agent_monitor::handle_agent_streaming_error(
                            app.clone(),
                            &task_meta,
                            &error,
                        )
                        .await;
                    }
                }
                {
                    let mut session = session_arc.lock().await;
                    if session.user_interrupt_flag.load(Ordering::SeqCst) {
                        session.clear_stream_for_retry();
                    }
                }
                break;
            }

            network_retry_attempt = 0;

            {
                let mut session = session_arc.lock().await;
                session.provider_usage_stale = false;
                if session.thread.reactive_compact_attempts.take().is_some() {
                    session.increment_version();
                    session.touch();
                }
            }

            if abort_flag.load(Ordering::SeqCst) {
                break;
            }

            let (mode_id, model_id, context_tokens_cap) = {
                let session = session_arc.lock().await;
                (
                    session.thread.mode.clone(),
                    session.thread.model.clone(),
                    session.thread.context_tokens_cap,
                )
            };

            let model_id_opt = if model_id.is_empty() {
                None
            } else {
                Some(model_id.as_str())
            };

            let (effective_n_ctx, image_token_mode) = {
                let caps = app.ctx.caps.try_load_caps_quickly(0).await;
                let model_rec = match caps {
                    Ok(caps) => crate::caps::resolve_chat_model(caps, &model_id).ok(),
                    Err(_) => None,
                };
                let image_token_mode = model_rec
                    .as_ref()
                    .map(|rec| rec.base.image_token_mode)
                    .unwrap_or_default();
                let effective_n_ctx = model_rec.map(|rec| {
                    let model_n_ctx = if rec.base.n_ctx > 0 {
                        rec.base.n_ctx
                    } else {
                        tokens().default_n_ctx
                    };
                    match context_tokens_cap {
                        Some(cap) if cap > 0 => cap.min(model_n_ctx),
                        _ => model_n_ctx,
                    }
                });
                (effective_n_ctx, image_token_mode)
            };

            if let Some(effective_n_ctx) = effective_n_ctx {
                let mut session = session_arc.lock().await;
                maybe_inject_token_budget_instruction(
                    &mut session,
                    effective_n_ctx,
                    TOKEN_BUDGET_CADENCE,
                    image_token_mode,
                );
            }

            maybe_save_trajectory_background_with_intent(
                app.clone(),
                session_arc.clone(),
                TrajectoryCommitIntent::Checkpoint,
            );

            match process_tool_calls_once(app.clone(), session_arc.clone(), &mode_id, model_id_opt)
                .await
            {
                ToolStepOutcome::NoToolCalls => {
                    if handle_task_agent_reasoning_token_stop(
                        app.clone(),
                        session_arc.clone(),
                        effective_n_ctx,
                        image_token_mode,
                    )
                    .await
                    {
                        break;
                    }
                    if maybe_recover_after_length_stop(
                        gcx.clone(),
                        &session_arc,
                        &thread,
                        effective_n_ctx,
                        image_token_mode,
                    )
                    .await
                    {
                        maybe_save_trajectory_with_intent(
                            app.clone(),
                            session_arc.clone(),
                            TrajectoryCommitIntent::Checkpoint,
                        )
                        .await;
                        continue;
                    }
                    if inject_queued_user_messages_before_llm_if_safe(
                        app.clone(),
                        session_arc.clone(),
                    )
                    .await
                    {
                        continue;
                    }
                    match maybe_nudge_after_silent_tail(app.clone(), &session_arc).await {
                        SilentTailNudge::Nudged => continue,
                        SilentTailNudge::Exhausted | SilentTailNudge::NotApplicable => {}
                    }
                    let should_continue = {
                        let session = session_arc.lock().await;
                        tail_needs_assistant(&session.messages)
                    };
                    if should_continue {
                        continue;
                    }
                    if maybe_record_goal_pursuit_progress(session_arc.clone()).await {
                        maybe_save_trajectory_with_intent(
                            app.clone(),
                            session_arc.clone(),
                            TrajectoryCommitIntent::Checkpoint,
                        )
                        .await;
                    }
                    if handle_goal_turn_end(app.clone(), session_arc.clone()).await {
                        break;
                    }
                    spawn_chat_completion_side_effects(
                        app.clone(),
                        session_arc.clone(),
                        chat_id.clone(),
                        chat_label.clone(),
                        true,
                    )
                    .await;
                    break;
                }
                ToolStepOutcome::Paused => {
                    let mut ev = make_runtime_event(
                        "tool_confirmation",
                        &format!("Waiting for approval: {}", chat_label),
                        "chat",
                        &format!("chat_{}", chat_id),
                        "paused",
                        None,
                    );
                    ev.chat_id = Some(chat_id.to_string());
                    ev.persistent = true;
                    app.buddy_event_sink.enqueue_event(ev).await;
                    break;
                }
                ToolStepOutcome::Stop => {
                    let completed = {
                        let session = session_arc.lock().await;
                        session.runtime.state == SessionState::Completed
                    };
                    if completed {
                        spawn_chat_completion_side_effects(
                            app.clone(),
                            session_arc.clone(),
                            chat_id.clone(),
                            chat_label.clone(),
                            false,
                        )
                        .await;
                    }
                    break;
                }
                ToolStepOutcome::Continue => {
                    // The step's assistant response and all its tool results are
                    // in, so the `append` boundary is open exactly here. Land
                    // pending deliveries before the loop-top pre-steps can
                    // inject a queued user message ahead of them or end the
                    // turn for a mode handoff.
                    crate::chat::delivery::drain_deliveries_at_boundary(
                        app.clone(),
                        session_arc.clone(),
                    )
                    .await;
                    if inject_queued_user_messages_before_llm_if_safe(
                        app.clone(),
                        session_arc.clone(),
                    )
                    .await
                    {
                        continue;
                    }
                }
            }
        }

        check_external_reload_pending(gcx.clone(), session_arc.clone()).await;

        {
            let session = session_arc.lock().await;
            session.abort_flag.store(false, Ordering::SeqCst);
            session.user_interrupt_flag.store(false, Ordering::SeqCst);
            session.queue_notify.notify_one();
        }

        // Whole turn is over: release the depth guard, then land `when_idle`
        // deliveries (and any `append` ones still waiting).
        turn_guard.release().await;
        crate::chat::delivery::drain_deliveries_at_boundary(app.clone(), session_arc.clone()).await;
    })
}

/// Tracks how many generation turns are in flight for a session, so
/// `when_idle` deliveries wait for the real end of the turn instead of an
/// intermediate `Idle` between tool steps.
pub(in crate::chat::generation) struct TurnDepthGuard {
    app: AppState,
    session_arc: Arc<AMutex<ChatSession>>,
    released: bool,
}

impl TurnDepthGuard {
    async fn enter(app: AppState, session_arc: Arc<AMutex<ChatSession>>) -> Self {
        {
            let mut session = session_arc.lock().await;
            session.turn_depth = session.turn_depth.saturating_add(1);
        }
        Self {
            app,
            session_arc,
            released: false,
        }
    }

    /// Release synchronously so a following drain observes the finished turn.
    async fn release(&mut self) {
        if self.released {
            return;
        }
        let mut session = self.session_arc.lock().await;
        session.turn_depth = session.turn_depth.saturating_sub(1);
        session.queue_notify.notify_one();
        self.released = true;
    }
}

impl Drop for TurnDepthGuard {
    fn drop(&mut self) {
        if self.released {
            return;
        }
        // Fallback for early returns/cancellation: never leave the turn pinned.
        let session_arc = self.session_arc.clone();
        let app = self.app.clone();
        tokio::spawn(async move {
            let mut session = session_arc.lock().await;
            session.turn_depth = session.turn_depth.saturating_sub(1);
            if matches!(
                session.runtime.state,
                SessionState::Generating | SessionState::ExecutingTools
            ) {
                session.abort_stream();
                session.clear_pending_tool_calls_for_interruption();
            }
            session.drain_post_tool_side_effects();
            session.queue_notify.notify_one();
            drop(session);
            crate::chat::delivery::drain_deliveries_at_boundary(app, session_arc).await;
        });
    }
}

/// Persist the new-chat default at the model-resolution boundary, not on load or
/// based on an empty history. Shared by the proactive gate and direct generation.
pub(in crate::chat::generation) async fn resolve_session_compression_cap(
    app: &AppState,
    session_arc: &Arc<AMutex<ChatSession>>,
    thread: &mut ThreadParams,
    model_window: usize,
) {
    let changed = {
        let mut session = session_arc.lock().await;
        let changed = session
            .thread
            .resolve_pending_compression_cap(Some(model_window));
        thread.auto_compression_cap = session.thread.auto_compression_cap;
        thread.auto_compression_cap_pending = session.thread.auto_compression_cap_pending;
        if changed {
            let params = json!({
                "auto_compression_cap": session.thread.auto_compression_cap,
                "auto_compression_cap_pending": session.thread.auto_compression_cap_pending,
            });
            session.emit(ChatEvent::ThreadUpdated { params });
            session.increment_version();
            session.touch();
        }
        changed
    };
    if changed {
        maybe_save_trajectory_with_intent(
            app.clone(),
            session_arc.clone(),
            TrajectoryCommitIntent::Checkpoint,
        )
        .await;
    }
}
