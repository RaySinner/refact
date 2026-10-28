#[allow(unused_imports)]
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Instant;
use serde_json::json;
use tokio::sync::{Mutex as AMutex};
use tracing::{info, warn};
use uuid::Uuid;

use crate::app_state::AppState;

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
use crate::chat::perf_diagnostics::{self, PerfComponent, PerfOutcome};

use crate::chat::types::*;
use crate::chat::trajectories::{
    ensure_frozen_prefix, first_system_prompt, frozen_prefix_is_complete,
    maybe_save_trajectory_background_with_intent,
};
use crate::chat::prepare::{
    build_canonical_openai_tools_from_aliases, prepare_chat_passthrough, ChatPrepareOptions,
};
use crate::chat::stream_core::{
    StreamRunParams, StreamCollector, normalize_tool_call, ChoiceFinal, LlmStreamError,
    LlmStreamOutcome,
};
use crate::chat::config::tokens;

use super::support::*;
use super::retry::*;
use super::preamble::*;
use super::recovery::*;
use super::run_loop::*;

pub enum GenerationResult {
    Completed,
    PausedForUserDecision,
}

pub(in crate::chat::generation) async fn run_streaming_generation(
    app: AppState,
    session_arc: Arc<AMutex<ChatSession>>,
    mut llm_request: LlmRequest,
    model_rec: &crate::caps::ChatModelRecord,
    abort_flag: Arc<AtomicBool>,
    abort_notify: Arc<tokio::sync::Notify>,
    stream_prepare_started_at: Option<Instant>,
) -> Result<GenerationResult, LlmStreamError> {
    info!(
        "session generation: model={}, messages={}",
        llm_request.model_id,
        llm_request.messages.len()
    );
    let (chat_id, root_chat_id, mode, task_id, task_role, agent_id, card_id) = {
        let session = session_arc.lock().await;
        let tm = session.thread.task_meta.as_ref();
        (
            session.chat_id.clone(),
            session.thread.root_chat_id.clone(),
            session.thread.mode.clone(),
            tm.map(|t| t.task_id.clone()),
            tm.map(|t| t.role.clone()),
            tm.and_then(|t| t.agent_id.clone()),
            tm.and_then(|t| t.card_id.clone()),
        )
    };
    let mode_for_stats = canonicalize_mode_for_stats(&mode);

    const TEMPERATURE_BUMP: f32 = 0.1;
    const MAX_RETRY_TEMPERATURE: f32 = 0.5;
    let user_specified_temp = llm_request.params.temperature;
    let model_supports_temperature = model_rec.supports_temperature;
    let can_retry_with_temp_bump = user_specified_temp.is_none() && model_supports_temperature;
    let max_attempts = if can_retry_with_temp_bump {
        (MAX_RETRY_TEMPERATURE / TEMPERATURE_BUMP).floor() as usize + 2
    } else {
        1
    };
    let mut attempt = 0;

    let (result, pending_success_event) = loop {
        attempt += 1;
        if can_retry_with_temp_bump && attempt > 1 {
            let retry_temp = TEMPERATURE_BUMP * (attempt - 2) as f32;
            llm_request.params.temperature = Some(retry_temp.min(MAX_RETRY_TEMPERATURE));
        }

        let params = StreamRunParams {
            llm_request: llm_request.clone(),
            model_rec: model_rec.base.clone(),
            chat_id: Some(chat_id.clone()),
            allow_websocket: true,
            abort_flag: Some(abort_flag.clone()),
            abort_notify: Some(abort_notify.clone()),
            supports_tools: model_rec.supports_tools,
            supports_reasoning: model_rec.has_reasoning_support(),
            reasoning_type: model_rec.reasoning_type_string(),
            supports_temperature: model_rec.supports_temperature,
        };

        let mut cloud_input_usage_task =
            crate::chat::cloud_token_count::spawn_input_token_count_task(
                app.chat_ctx(),
                app.runtime.http_client.clone(),
                &llm_request,
                &model_rec.base,
                Some(chat_id.clone()),
            );

        enum CollectorEventPayload {
            DeltaOps(Vec<DeltaOp>),
            Usage(ChatUsage),
        }

        const EMITTER_QUEUE_CAPACITY: usize = 256;
        let (tx, mut rx) =
            tokio::sync::mpsc::channel::<CollectorEventPayload>(EMITTER_QUEUE_CAPACITY);
        let overflow_usage = Arc::new(std::sync::Mutex::new(None::<ChatUsage>));
        let overflow_ops = Arc::new(std::sync::Mutex::new(Vec::<DeltaOp>::new()));

        struct SessionCollector {
            tx: tokio::sync::mpsc::Sender<CollectorEventPayload>,
            overflow_usage: Arc<std::sync::Mutex<Option<ChatUsage>>>,
            overflow_ops: Arc<std::sync::Mutex<Vec<DeltaOp>>>,
        }

        impl StreamCollector for SessionCollector {
            fn on_delta_ops(&mut self, _choice_idx: usize, ops: Vec<DeltaOp>) {
                match self.tx.try_send(CollectorEventPayload::DeltaOps(ops)) {
                    Ok(()) => {}
                    Err(tokio::sync::mpsc::error::TrySendError::Full(event)) => {
                        if let CollectorEventPayload::DeltaOps(ops) = event {
                            if let Ok(mut guard) = self.overflow_ops.lock() {
                                guard.extend(ops);
                            }
                        }
                    }
                    Err(tokio::sync::mpsc::error::TrySendError::Closed(_event)) => {}
                }
            }

            fn on_usage(&mut self, usage: &ChatUsage) {
                let usage_clone = usage.clone();
                match self
                    .tx
                    .try_send(CollectorEventPayload::Usage(usage_clone.clone()))
                {
                    Ok(()) => {}
                    Err(tokio::sync::mpsc::error::TrySendError::Full(_event)) => {
                        if let Ok(mut guard) = self.overflow_usage.lock() {
                            *guard = Some(usage_clone);
                        }
                    }
                    Err(tokio::sync::mpsc::error::TrySendError::Closed(_event)) => {}
                }
            }

            fn on_finish(&mut self, _choice_idx: usize, _finish_reason: Option<String>) {}
        }

        let mut collector = SessionCollector {
            tx,
            overflow_usage: overflow_usage.clone(),
            overflow_ops: overflow_ops.clone(),
        };

        let session_arc_emitter = session_arc.clone();
        let first_content_started_at = stream_prepare_started_at;
        let emitter_chat_id = chat_id.clone();
        let emitter_task = tokio::spawn(async move {
            fn merge_events(
                events: &mut Vec<CollectorEventPayload>,
                batched_ops: &mut Vec<DeltaOp>,
                latest_usage: &mut Option<ChatUsage>,
            ) {
                for event in events.drain(..) {
                    match event {
                        CollectorEventPayload::DeltaOps(ops) => {
                            batched_ops.extend(ops);
                        }
                        CollectorEventPayload::Usage(usage) => {
                            *latest_usage = Some(usage);
                        }
                    }
                }
            }
            let mut pending = Vec::<CollectorEventPayload>::new();
            let mut first_visible_delta_emitted = false;
            let mut first_content_delta_recorded = false;

            while let Some(first_event) = rx.recv().await {
                let flush_immediately = match &first_event {
                    CollectorEventPayload::Usage(_) => true,
                    CollectorEventPayload::DeltaOps(ops) => {
                        !first_visible_delta_emitted && delta_ops_contain_visible_content(ops)
                            || ops.iter().any(|op| !delta_is_coalescible_text(op))
                    }
                };
                pending.push(first_event);

                if !flush_immediately {
                    let deadline = tokio::time::Instant::now() + DELTA_COALESCE_WINDOW;
                    while pending.len() < MAX_DELTA_BATCH_EVENTS {
                        match tokio::time::timeout_at(deadline, rx.recv()).await {
                            Ok(Some(event)) => {
                                let must_flush = match &event {
                                    CollectorEventPayload::Usage(_) => true,
                                    CollectorEventPayload::DeltaOps(ops) => {
                                        (!first_visible_delta_emitted
                                            && delta_ops_contain_visible_content(ops))
                                            || ops.iter().any(|op| !delta_is_coalescible_text(op))
                                    }
                                };
                                pending.push(event);
                                if must_flush {
                                    break;
                                }
                            }
                            Ok(None) | Err(_) => break,
                        }
                    }
                }

                let mut batched_ops = Vec::new();
                let mut latest_usage: Option<ChatUsage> = None;
                merge_events(&mut pending, &mut batched_ops, &mut latest_usage);

                if let Ok(mut guard) = overflow_ops.lock() {
                    if !guard.is_empty() {
                        let mut drained = std::mem::take(&mut *guard);
                        batched_ops.append(&mut drained);
                    }
                }
                if let Ok(mut guard) = overflow_usage.lock() {
                    if let Some(usage) = guard.take() {
                        latest_usage = Some(usage);
                    }
                }

                let batched_ops = batch_stream_delta_ops(batched_ops);

                let mut session = session_arc_emitter.lock().await;
                if !batched_ops.is_empty() {
                    for ops in batched_ops {
                        let carries_content = delta_ops_contain_assistant_content(&ops);
                        first_visible_delta_emitted |= delta_ops_contain_visible_content(&ops);
                        session.emit_stream_delta(ops);
                        if carries_content && !first_content_delta_recorded {
                            if let Some(started_at) = first_content_started_at {
                                perf_diagnostics::record(
                                    PerfComponent::StreamFirstContentDelta,
                                    Some(&emitter_chat_id),
                                    PerfOutcome::Success,
                                    started_at
                                        .elapsed()
                                        .as_micros()
                                        .try_into()
                                        .unwrap_or(u64::MAX),
                                    None,
                                    None,
                                    None,
                                );
                            }
                            first_content_delta_recorded = true;
                        }
                    }
                }
                if let Some(usage) = latest_usage {
                    session.draft_usage = Some(usage);
                }
            }

            let mut final_ops = Vec::new();
            let mut final_usage: Option<ChatUsage> = None;
            if let Ok(mut guard) = overflow_ops.lock() {
                if !guard.is_empty() {
                    final_ops = std::mem::take(&mut *guard);
                }
            }
            if let Ok(mut guard) = overflow_usage.lock() {
                if let Some(usage) = guard.take() {
                    final_usage = Some(usage);
                }
            }

            if !final_ops.is_empty() || final_usage.is_some() {
                let final_ops = batch_stream_delta_ops(final_ops);

                let mut session = session_arc_emitter.lock().await;
                if !final_ops.is_empty() {
                    for ops in final_ops {
                        let carries_content = delta_ops_contain_assistant_content(&ops);
                        session.emit_stream_delta(ops);
                        if carries_content && !first_content_delta_recorded {
                            if let Some(started_at) = first_content_started_at {
                                perf_diagnostics::record(
                                    PerfComponent::StreamFirstContentDelta,
                                    Some(&emitter_chat_id),
                                    PerfOutcome::Success,
                                    started_at
                                        .elapsed()
                                        .as_micros()
                                        .try_into()
                                        .unwrap_or(u64::MAX),
                                    None,
                                    None,
                                    None,
                                );
                            }
                            first_content_delta_recorded = true;
                        }
                    }
                }
                if let Some(usage) = final_usage {
                    session.draft_usage = Some(usage);
                }
            }
        });

        let call_ts_start = chrono::Utc::now().to_rfc3339();
        let call_start = std::time::Instant::now();

        let stream_outcome = crate::chat::stream_core::run_llm_stream_with_prepare_started(
            app.clone(),
            params,
            &mut collector,
            stream_prepare_started_at,
        )
        .await;
        let mut cloud_input_usage = None;
        if stream_outcome.is_err() {
            if let Some(mut handle) = cloud_input_usage_task.take() {
                match tokio::time::timeout(std::time::Duration::from_secs(2), &mut handle).await {
                    Ok(Ok(usage)) => cloud_input_usage = usage,
                    Ok(Err(_)) => {}
                    Err(_) => handle.abort(),
                }
            }
        } else if let Some(handle) = cloud_input_usage_task.take() {
            match handle.await {
                Ok(usage) => {
                    cloud_input_usage = usage;
                }
                Err(err) => {
                    if !err.is_cancelled() {
                        tracing::warn!(error = %err, "cloud token count task failed");
                    }
                }
            }
        }
        drop(collector);
        let _ = emitter_task.await;

        if let Ok(LlmStreamOutcome::PausedForCacheGuard) = stream_outcome {
            tracing::info!("Generation paused by cache guard");
            return Ok(GenerationResult::PausedForUserDecision);
        }

        let results = stream_outcome
            .map(|o| match o {
                LlmStreamOutcome::Choices(c) => c,
                LlmStreamOutcome::PausedForCacheGuard => unreachable!(),
            })
            .map_err(|error| {
                synthesize_responses_context_cutoff_error_if_needed(
                    error,
                    &model_rec.base,
                    &llm_request.messages,
                    llm_request.params.n_ctx.unwrap_or(model_rec.base.n_ctx),
                    true,
                )
            });

        let duration_ms = call_start.elapsed().as_millis() as u64;
        let call_ts_end = chrono::Utc::now().to_rfc3339();

        let (
            model_id_for_stats,
            messages_count,
            tools_count,
            temperature_for_stats,
            max_tokens_for_stats,
        ) = (
            llm_request.model_id.clone(),
            llm_request.messages.len(),
            llm_request.tools.as_ref().map(|t| t.len()).unwrap_or(0),
            llm_request.params.temperature,
            llm_request.params.max_tokens,
        );

        match &results {
            Err(e) => {
                let usage_for_error = {
                    let session = session_arc.lock().await;
                    session.draft_usage.clone()
                }
                .or_else(|| cloud_input_usage.as_ref().map(|count| count.usage.clone()));
                let (provider, model) = split_model_provider(&model_id_for_stats);
                let event = LlmCallEvent {
                    id: uuid::Uuid::new_v4().to_string(),
                    ts_start: call_ts_start.clone(),
                    ts_end: call_ts_end.clone(),
                    duration_ms,
                    chat_id: chat_id.clone(),
                    root_chat_id: root_chat_id.clone(),
                    mode: mode_for_stats.clone(),
                    task_id: task_id.clone(),
                    task_role: task_role.clone(),
                    agent_id: agent_id.clone(),
                    card_id: card_id.clone(),
                    model_id: model_id_for_stats.clone(),
                    provider,
                    model,
                    messages_count,
                    tools_count,
                    max_tokens: max_tokens_for_stats,
                    temperature: temperature_for_stats,
                    success: false,
                    error_message: Some(e.message.chars().take(200).collect()),
                    finish_reason: None,
                    attempt_n: attempt,
                    retry_reason: None,
                    prompt_tokens: usage_for_error
                        .as_ref()
                        .map(|u| u.prompt_tokens)
                        .unwrap_or(0),
                    completion_tokens: usage_for_error
                        .as_ref()
                        .map(|u| u.completion_tokens)
                        .unwrap_or(0),
                    cache_read_tokens: usage_for_error.as_ref().and_then(|u| u.cache_read_tokens),
                    cache_creation_tokens: usage_for_error
                        .as_ref()
                        .and_then(|u| u.cache_creation_tokens),
                    total_tokens: usage_for_error
                        .as_ref()
                        .map(|u| u.total_tokens)
                        .unwrap_or(0),
                    cost_usd: None,
                };
                if let Some(sender) = &app.model.llm_stats_sender {
                    if sender.try_send(event).is_err() {
                        tracing::warn!("stats: channel full, dropping LLM call event");
                    }
                }
            }
            Ok(_) => {}
        }

        let results = results?;

        let mut result = results.into_iter().next().unwrap_or_default();

        if result.usage.is_none() {
            if let Some(count) = cloud_input_usage.clone() {
                result.usage = Some(count.usage);
            }
        }
        if let Some(usage) = result.usage.clone() {
            let mut session = session_arc.lock().await;
            session.draft_usage = Some(usage);
        }

        if is_result_empty(&result) {
            let draft_usage = {
                let session = session_arc.lock().await;
                session.draft_usage.clone()
            };
            let (provider, model) = split_model_provider(&model_id_for_stats);
            let event = LlmCallEvent {
                id: uuid::Uuid::new_v4().to_string(),
                ts_start: call_ts_start,
                ts_end: call_ts_end,
                duration_ms,
                chat_id: chat_id.clone(),
                root_chat_id: root_chat_id.clone(),
                mode: mode_for_stats.clone(),
                task_id: task_id.clone(),
                task_role: task_role.clone(),
                agent_id: agent_id.clone(),
                card_id: card_id.clone(),
                model_id: model_id_for_stats,
                provider,
                model,
                messages_count,
                tools_count,
                max_tokens: max_tokens_for_stats,
                temperature: temperature_for_stats,
                success: false,
                error_message: Some("empty_response".to_string()),
                finish_reason: result.finish_reason.clone(),
                attempt_n: attempt,
                retry_reason: Some("empty_response".to_string()),
                prompt_tokens: draft_usage.as_ref().map(|u| u.prompt_tokens).unwrap_or(0),
                completion_tokens: draft_usage
                    .as_ref()
                    .map(|u| u.completion_tokens)
                    .unwrap_or(0),
                cache_read_tokens: draft_usage.as_ref().and_then(|u| u.cache_read_tokens),
                cache_creation_tokens: draft_usage.as_ref().and_then(|u| u.cache_creation_tokens),
                total_tokens: draft_usage.as_ref().map(|u| u.total_tokens).unwrap_or(0),
                cost_usd: draft_usage
                    .as_ref()
                    .and_then(|u| u.metering_usd.as_ref())
                    .map(|m| m.total_usd),
            };
            if let Some(sender) = &app.model.llm_stats_sender {
                if sender.try_send(event).is_err() {
                    tracing::warn!("stats: channel full, dropping LLM call event");
                }
            }

            if attempt < max_attempts && can_retry_with_temp_bump {
                let current_temp_display = if attempt == 1 {
                    "default".to_string()
                } else {
                    format!("{:.1}", TEMPERATURE_BUMP * (attempt - 2) as f32)
                };
                let next_temp =
                    (TEMPERATURE_BUMP * (attempt - 1) as f32).min(MAX_RETRY_TEMPERATURE);
                warn!(
                    "Empty assistant response at T={}, retrying with T={:.1} (attempt {}/{})",
                    current_temp_display, next_temp, attempt, max_attempts
                );
                {
                    let mut session = session_arc.lock().await;
                    if let Some(ref mut draft) = session.draft_message {
                        draft.content = ChatContent::SimpleText(String::new());
                        draft.tool_calls = None;
                        draft.reasoning_content = None;
                        draft.thinking_blocks = None;
                        draft.citations = Vec::new();
                        draft.server_content_blocks = Vec::new();
                        draft.extra = serde_json::Map::new();
                    }
                    session.draft_usage = None;
                }
                continue;
            } else {
                let effective_temp = llm_request.params.temperature.unwrap_or(0.0);
                return Err(format!(
                    "Empty assistant response after {} attempts (T={:.1})",
                    max_attempts, effective_temp
                )
                .into());
            }
        }

        // --- Tool call recovery ---
        // GPT-5 Codex models occasionally leak tool calls into text content instead of
        // emitting structured function_call events. Detect and recover them.
        let allowed_tools = tool_call_recovery::allowed_tool_names(&llm_request.tools);

        // 1. Unwrap multi_tool_use.parallel wrappers in structured tool_calls
        if !result.tool_calls_raw.is_empty() {
            result.tool_calls_raw = tool_call_recovery::unwrap_multi_tool_use_parallel(
                &result.tool_calls_raw,
                &allowed_tools,
            );
        }

        // 2. Recover tool calls from garbled ChatML content (when no structured calls exist)
        if result.tool_calls_raw.is_empty() && !allowed_tools.is_empty() {
            if let Some((cleaned_content, recovered_calls)) =
                tool_call_recovery::recover_tool_calls_from_chatml_content(
                    &result.raw_content,
                    &allowed_tools,
                )
            {
                warn!(
                    "tool_call_recovery: recovered {} tool call(s) from garbled content",
                    recovered_calls.len()
                );
                result.content = cleaned_content;
                result.tool_calls_raw = recovered_calls;
            }
        }

        if result.tool_calls_raw.is_empty() && !allowed_tools.is_empty() {
            if let Some((cleaned_content, recovered_calls, source)) =
                tool_call_recovery_oss::recover_tool_calls_from_oss_text(
                    &result.raw_content,
                    &allowed_tools,
                )
            {
                warn!(
                    "tool_call_recovery_oss: recovered {} tool call(s) via {}",
                    recovered_calls.len(),
                    source
                );
                result
                    .extra
                    .insert("_tool_call_recovery_source".to_string(), json!(source));
                result.content = cleaned_content;
                result.tool_calls_raw = recovered_calls;
            }
        }

        if !result.tool_calls_raw.is_empty() {
            let parsed: Vec<_> = result
                .tool_calls_raw
                .iter()
                .filter_map(|tc| normalize_tool_call(tc))
                .collect();
            if parsed.is_empty() {
                let has_content = !result.content.trim().is_empty()
                    || !result.reasoning.trim().is_empty()
                    || !result.server_content_blocks.is_empty()
                    || !result.citations.is_empty();
                let names: Vec<_> = result
                    .tool_calls_raw
                    .iter()
                    .filter_map(|tc| {
                        tc.get("function")
                            .and_then(|f| f.get("name"))
                            .and_then(|n| n.as_str())
                    })
                    .collect();
                tracing::warn!(
                    "All {} tool calls unparsable, names={:?}",
                    result.tool_calls_raw.len(),
                    names,
                );
                if !has_content {
                    return Err("Model returned tool_calls but none were parsable"
                        .to_string()
                        .into());
                }
                // Has useful content тАФ discard unparsable tool calls and continue
                result.tool_calls_raw.clear();
            } else if parsed.len() < result.tool_calls_raw.len() {
                let dropped_names: Vec<_> = result
                    .tool_calls_raw
                    .iter()
                    .filter(|tc| normalize_tool_call(tc).is_none())
                    .filter_map(|tc| {
                        tc.get("function")
                            .and_then(|f| f.get("name"))
                            .and_then(|n| n.as_str())
                    })
                    .collect();
                tracing::warn!(
                    "Dropped {}/{} tool calls during normalization, names={:?}",
                    dropped_names.len(),
                    result.tool_calls_raw.len(),
                    dropped_names,
                );
            }
        }

        maybe_downgrade_bogus_tool_calls_finish_reason(&mut result, "post_tool_normalization");

        let draft_usage_for_success = {
            let session = session_arc.lock().await;
            session.draft_usage.clone()
        };
        let usage_for_event = result.usage.as_ref().or(draft_usage_for_success.as_ref());
        let (provider, model) = split_model_provider(&model_id_for_stats);
        let pending_success_event = LlmCallEvent {
            id: uuid::Uuid::new_v4().to_string(),
            ts_start: call_ts_start,
            ts_end: call_ts_end,
            duration_ms,
            chat_id: chat_id.clone(),
            root_chat_id: root_chat_id.clone(),
            mode: mode_for_stats.clone(),
            task_id: task_id.clone(),
            task_role: task_role.clone(),
            agent_id: agent_id.clone(),
            card_id: card_id.clone(),
            model_id: model_id_for_stats,
            provider,
            model,
            messages_count,
            tools_count,
            max_tokens: max_tokens_for_stats,
            temperature: temperature_for_stats,
            success: true,
            error_message: None,
            finish_reason: result.finish_reason.clone(),
            attempt_n: attempt,
            retry_reason: None,
            prompt_tokens: usage_for_event.map(|u| u.prompt_tokens).unwrap_or(0),
            completion_tokens: usage_for_event.map(|u| u.completion_tokens).unwrap_or(0),
            cache_read_tokens: usage_for_event.and_then(|u| u.cache_read_tokens),
            cache_creation_tokens: usage_for_event.and_then(|u| u.cache_creation_tokens),
            total_tokens: usage_for_event.map(|u| u.total_tokens).unwrap_or(0),
            cost_usd: None,
        };

        break (result, pending_success_event);
    };

    let (model_id, usage_for_pricing) = {
        let session = session_arc.lock().await;
        (model_rec.base.id.clone(), session.draft_usage.clone())
    };
    let metering_usd = if let Some(ref usage) = usage_for_pricing {
        generation_metering_usd(&app, &model_id, usage).await
    } else {
        None
    };

    {
        let mut success_event = pending_success_event;
        success_event.cost_usd = metering_usd.as_ref().map(|m| m.total_usd);
        if let Some(sender) = &app.model.llm_stats_sender {
            if sender.try_send(success_event).is_err() {
                tracing::warn!("stats: channel full, dropping LLM call event");
            }
        }
    }

    {
        let mut session = session_arc.lock().await;
        // Whether this chat diverts pre-tool monologue aggressively (task
        // agents and the classic agent modes act first, think less in the
        // visible output).
        let is_agent_for_monologue =
            is_task_agent_chat(&session.thread) || is_agentic_mode_id(&mode);
        if let Some(ref mut draft) = session.draft_message {
            // Divert pre-tool monologue ("thinking out loud" in `content`
            // right before a tool call) into `reasoning_content`: the UI then
            // shows it under "Thought for..." and strip_reasoning_from_prompt
            // drops it from future prompts so the model is not confused by
            // its own past thoughts.
            let pre_tool_monologue = (!result.tool_calls_raw.is_empty()
                && !result.content.trim().is_empty()
                && is_pre_tool_monologue(&result.content, is_agent_for_monologue))
            .then(|| result.content.clone());
            draft.content = match &pre_tool_monologue {
                Some(_) => ChatContent::SimpleText(String::new()),
                None => ChatContent::SimpleText(result.content),
            };

            if !result.tool_calls_raw.is_empty() {
                info!(
                    "Parsing {} accumulated tool calls",
                    result.tool_calls_raw.len()
                );
                let parsed: Vec<_> = result
                    .tool_calls_raw
                    .iter()
                    .filter_map(|tc| normalize_tool_call(tc))
                    .collect();
                info!("Successfully parsed {} tool calls", parsed.len());
                if !parsed.is_empty() {
                    draft.tool_calls = Some(parsed);
                }
            }

            if !result.reasoning.is_empty() {
                draft.reasoning_content = Some(result.reasoning);
            }
            if let Some(monologue) = pre_tool_monologue {
                info!(
                    "Diverted {} chars of pre-tool monologue into reasoning_content",
                    monologue.len()
                );
                match draft.reasoning_content.as_mut() {
                    Some(existing) if !existing.trim().is_empty() => {
                        existing.push_str(&format!("\n{monologue}"));
                    }
                    _ => draft.reasoning_content = Some(monologue),
                }
            }
            if !result.thinking_blocks.is_empty() {
                draft.thinking_blocks = Some(result.thinking_blocks);
            }
            if !result.citations.is_empty() {
                draft.citations = result.citations;
            }
            if !result.server_content_blocks.is_empty() {
                draft.server_content_blocks = result.server_content_blocks;
            }
            if !result.extra.is_empty() {
                draft.extra = result.extra;
            }
        }

        // Store previous_response_id for stateful multi-turn on Platform API only.
        // ChatGPT backend doesn't support previous_response_id, so don't store it тАФ
        // otherwise prepare_chat_passthrough activates tail-only mode and the server
        // receives function_call_output without matching function_call items.
        let is_chatgpt_backend = model_rec.base.endpoint.contains("chatgpt.com/backend-api");
        if model_rec.base.wire_format == crate::llm::WireFormat::OpenaiResponses
            && !is_chatgpt_backend
        {
            if let Some(resp_id) = session
                .draft_message
                .as_ref()
                .and_then(|m| m.extra.get("openai_response_id"))
                .and_then(|v| v.as_str())
            {
                if session.thread.previous_response_id.as_deref() != Some(resp_id) {
                    session.thread.previous_response_id = Some(resp_id.to_string());
                    session.increment_version();
                }
            }
        }

        if let Some(ref mut usage) = session.draft_usage {
            usage.metering_usd = metering_usd;
        }

        let next_state = if session
            .draft_message
            .as_ref()
            .and_then(|m| m.tool_calls.as_ref())
            .is_some_and(|tool_calls| !tool_calls.is_empty())
        {
            SessionState::ExecutingTools
        } else {
            SessionState::Idle
        };
        session.finish_stream_with_next_state(result.finish_reason, next_state);
    }

    Ok(GenerationResult::Completed)
}

pub(in crate::chat::generation) fn is_result_empty(result: &ChoiceFinal) -> bool {
    result.content.trim().is_empty()
        && result.tool_calls_raw.is_empty()
        && result.reasoning.trim().is_empty()
        && result.thinking_blocks.is_empty()
        && result.citations.is_empty()
        && result.server_content_blocks.is_empty()
}

pub(in crate::chat::generation) fn event_payload_bool(
    message: &ChatMessage,
    subkind: &str,
    key: &str,
) -> bool {
    message.role == crate::chat::internal_roles::EVENT_ROLE
        && message
            .extra
            .get("event")
            .and_then(|event| event.get("subkind"))
            .and_then(|value| value.as_str())
            == Some(subkind)
        && message
            .extra
            .get("event")
            .and_then(|event| event.get("payload"))
            .and_then(|payload| payload.get(key))
            .and_then(|value| value.as_bool())
            .unwrap_or(false)
}

pub(in crate::chat::generation) fn assistant_goal_pursuit_accounting_enabled(
    message: &ChatMessage,
) -> bool {
    message
        .extra
        .get("goal_pursuit")
        .and_then(|value| value.get("account_progress"))
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
}

pub(in crate::chat::generation) fn latest_goal_pursuit_usage(
    session: &ChatSession,
) -> Option<ChatUsage> {
    let goal = session.goal.as_ref()?;
    if !goal.active || goal.status != GoalStatus::Active {
        return None;
    }
    let assistant_index = session
        .messages
        .iter()
        .rposition(|message| message.role == "assistant")?;
    let assistant = &session.messages[assistant_index];
    let marked_on_assistant = assistant_goal_pursuit_accounting_enabled(assistant);
    let marked_before_assistant = session.messages[..assistant_index]
        .iter()
        .rev()
        .take_while(|message| message.role != "assistant")
        .any(|message| event_payload_bool(message, "goal_pursuit", "account_progress"));
    if marked_on_assistant || marked_before_assistant {
        assistant.usage.clone()
    } else {
        None
    }
}

pub(in crate::chat::generation) async fn maybe_record_goal_pursuit_progress(
    session_arc: Arc<AMutex<ChatSession>>,
) -> bool {
    let mut session = session_arc.lock().await;
    let Some(usage) = latest_goal_pursuit_usage(&session) else {
        return false;
    };
    session.goal_record_progress_from_usage(&usage)
}

pub(in crate::chat::generation) fn maybe_downgrade_bogus_tool_calls_finish_reason(
    result: &mut ChoiceFinal,
    stage: &str,
) {
    let original = match result.finish_reason.as_deref() {
        Some(reason @ ("tool_calls" | "tool_use")) => reason.to_string(),
        _ => return,
    };
    if !result.tool_calls_raw.is_empty() {
        return;
    }

    warn!(
        "tool_call_guard: finish_reason='{}' without tool calls at stage '{}', downgrading to 'stop'",
        original, stage
    );
    result.extra.insert(
        "_tool_call_guard".to_string(),
        json!({
            "kind": "tool_calls_finish_without_calls",
            "stage": stage,
            "original_finish_reason": original,
            "adjusted_finish_reason": "stop",
        }),
    );
    result.finish_reason = Some("stop".to_string());
}

pub async fn run_llm_generation(
    app: AppState,
    session_arc: Arc<AMutex<ChatSession>>,
    mut thread: ThreadParams,
    chat_id: String,
    abort_flag: Arc<AtomicBool>,
    abort_notify: Arc<tokio::sync::Notify>,
    stream_prepare_started_at: Option<Instant>,
) -> Result<GenerationResult, LlmStreamError> {
    check_aborted_before_stream(&abort_flag)?;
    let caps = app
        .ctx
        .caps
        .try_load_caps_quickly(0)
        .await
        .map_err(|e| e.message)?;
    let model_rec = crate::caps::resolve_chat_model(caps.clone(), &thread.model)?;
    let model_n_ctx = if model_rec.base.n_ctx > 0 {
        model_rec.base.n_ctx
    } else {
        tokens().default_n_ctx
    };
    resolve_session_compression_cap(&app, &session_arc, &mut thread, model_n_ctx).await;
    check_aborted_before_stream(&abort_flag)?;

    let execution_scope = thread
        .worktree
        .as_ref()
        .map(|worktree| worktree.root.to_string_lossy().into_owned());
    let existing_catalog = {
        let session = session_arc.lock().await;
        session.tool_catalog.clone()
    };
    let catalog = match existing_catalog {
        Some(catalog) => catalog,
        None => {
            app.tool_registry
                .acquire_tool_catalog(
                    &thread.mode,
                    Some(&model_rec.base.id),
                    execution_scope.as_deref(),
                )
                .await
        }
    };
    let mcp_lazy_active = catalog.index.mcp_lazy_mode;
    let tools = catalog.index.tools.clone();

    info!(
        "session generation: model={}, tools count = {} (mcp_lazy={})",
        model_rec.base.id,
        tools.len(),
        mcp_lazy_active
    );

    let (messages, existing_frozen_prefix) = {
        let session = session_arc.lock().await;
        (
            refact_core::active_context::active_context(&session.messages)
                .map_err(|error| error.to_string())?
                .messages,
            session.thread.frozen_request_prefix.clone(),
        )
    };
    let canonical_tools = build_canonical_openai_tools_from_aliases(
        &tools,
        catalog.aliases.clone(),
        model_rec.supports_strict_tools,
        model_rec.supports_tools,
    );
    let mut installed_frozen_prefix = false;
    let frozen_request_prefix = if existing_frozen_prefix
        .as_ref()
        .is_some_and(frozen_prefix_is_complete)
    {
        existing_frozen_prefix
    } else {
        let system_prompt = first_system_prompt(&messages);
        let mut session = session_arc.lock().await;
        installed_frozen_prefix = ensure_frozen_prefix(
            &mut session,
            system_prompt,
            Some(serde_json::Value::Array(canonical_tools.tools.clone())),
        )
        .is_some();
        session.thread.frozen_request_prefix.clone()
    };
    if installed_frozen_prefix {
        maybe_save_trajectory_background_with_intent(
            app.clone(),
            session_arc.clone(),
            TrajectoryCommitIntent::Checkpoint,
        );
    }

    let effective_n_ctx = match thread.context_tokens_cap {
        Some(cap) if cap > 0 => cap.min(model_n_ctx),
        _ => model_n_ctx,
    };
    check_aborted_before_stream(&abort_flag)?;
    let tokenizer_arc = crate::tokens::cached_tokenizer_for_ctx(&app.ctx, &model_rec.base).await?;
    let t = HasTokenizerAndEot::new(tokenizer_arc);

    let meta = ChatMeta {
        chat_id: chat_id.clone(),
        chat_mode: thread.mode.clone(),
        chat_remote: false,
        current_config_file: String::new(),
        context_tokens_cap: thread.context_tokens_cap,
        include_project_info: thread.include_project_info,
        request_attempt_id: Uuid::new_v4().to_string(),
        worktree: thread.worktree.clone(),
    };

    let model_type_defaults = model_type_defaults_for_thread(
        &caps.user_defaults,
        &caps.defaults,
        &thread,
        &model_rec.base.id,
    );
    let mut parameters = SamplingParameters {
        temperature: thread.temperature.or(model_type_defaults.temperature),
        frequency_penalty: thread.frequency_penalty,
        max_new_tokens: thread
            .max_tokens
            .or(model_type_defaults.max_new_tokens)
            .unwrap_or(0),
        boost_reasoning: thread
            .boost_reasoning
            .unwrap_or_else(|| model_type_defaults.boost_reasoning.unwrap_or(false)),
        reasoning_effort: thread
            .reasoning_effort
            .as_ref()
            .and_then(|s| match s.as_str() {
                "low" => Some(crate::call_validation::ReasoningEffort::Low),
                "medium" => Some(crate::call_validation::ReasoningEffort::Medium),
                "high" => Some(crate::call_validation::ReasoningEffort::High),
                "xhigh" => Some(crate::call_validation::ReasoningEffort::XHigh),
                "max" => Some(crate::call_validation::ReasoningEffort::Max),
                _ => None,
            })
            .or_else(|| {
                model_type_defaults
                    .reasoning_effort
                    .as_ref()
                    .and_then(|s| match s.as_str() {
                        "low" => Some(crate::call_validation::ReasoningEffort::Low),
                        "medium" => Some(crate::call_validation::ReasoningEffort::Medium),
                        "high" => Some(crate::call_validation::ReasoningEffort::High),
                        "xhigh" => Some(crate::call_validation::ReasoningEffort::XHigh),
                        "max" => Some(crate::call_validation::ReasoningEffort::Max),
                        _ => None,
                    })
            }),
        thinking_budget: thread
            .thinking_budget
            .or(model_type_defaults.thinking_budget),
        ..Default::default()
    };

    {
        let mut session = session_arc.lock().await;
        if let Some(boost) = session.pending_max_new_tokens_boost.take() {
            if parameters.max_new_tokens > 0 && parameters.max_new_tokens < boost {
                parameters.max_new_tokens = boost;
            }
        }
    }

    let ccx = AtCommandsContext::new_from_app(
        app.clone(),
        effective_n_ctx,
        CHAT_TOP_N,
        false,
        messages.clone(),
        chat_id.clone(),
        thread.root_chat_id.clone(),
        model_rec.base.id.clone(),
        thread.task_meta.clone(),
        thread.worktree.clone(),
    )
    .await;
    let ccx_arc = Arc::new(AMutex::new(ccx));

    let options = ChatPrepareOptions {
        prepend_system_prompt: false,
        allow_at_commands: true,
        allow_tool_prerun: true,
        supports_tools: model_rec.supports_tools,
        parallel_tool_calls: thread.parallel_tool_calls,
        cache_control: CacheControl::Ephemeral,
        frozen_request_prefix: frozen_request_prefix.clone(),
        ..Default::default()
    };

    check_aborted_before_stream(&abort_flag)?;
    let mut prepared = prepare_chat_passthrough(
        app.ctx.clone(),
        ccx_arc.clone(),
        &t,
        messages,
        &thread,
        &model_rec.base.id,
        &thread.mode,
        tools,
        &meta,
        &mut parameters,
        &options,
    )
    .await?;

    let claude_code_identity = ensure_claude_code_identity(&session_arc, &model_rec.base).await;
    prepared.llm_request = prepared
        .llm_request
        .with_claude_code_identity(claude_code_identity);

    {
        let mut session = session_arc.lock().await;
        session.last_prompt_messages = prepared.limited_messages.clone();
        session.tool_catalog = Some(catalog);
        save_rag_results_to_session(&mut session, &prepared.rag_results);
    }

    check_aborted_before_stream(&abort_flag)?;
    run_streaming_generation(
        app,
        session_arc,
        prepared.llm_request,
        &model_rec,
        abort_flag,
        abort_notify,
        stream_prepare_started_at,
    )
    .await
}

pub(in crate::chat::generation) async fn generation_metering_usd(
    app: &AppState,
    model_id: &str,
    usage: &ChatUsage,
) -> Option<MeteringUsd> {
    let pricing =
        crate::providers::pricing::lookup_model_pricing_for_ctx(&app.ctx, model_id).await?;
    crate::providers::pricing::compute_cost(usage, &pricing)
}
