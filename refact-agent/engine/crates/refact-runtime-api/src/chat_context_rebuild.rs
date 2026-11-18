//! Model-based context reconstruction: model resolution, the next-request budget
//! check, the reconstruction subchat round trip, and the trajectory commit.
//!
//! The body moved here from the engine's `chat::context_rebuild::pipeline` once
//! everything it needs lived crate-side: `ChatCtx::{tool_registry, prompts,
//! mode_config, subchat, pending_handoff}`, `refact_chat_history::chat_compress`,
//! `mode_transition_runtime`, and the trajectory store. The engine module is a
//! pure re-export, so no call site changed.
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::Duration;

use tokio::sync::Mutex;

use refact_chat_api::compression_gate::compression_attempt_active;
use refact_chat_api::context_rebuild::{
    at_auto_cap, compaction_safe, idle, pending_tools, prepare_explicit_idle,
    resolve_effective_compression_cap, status, AttemptGuard, CompactionOutcome,
    ManualCompressionApplyResult, ManualCompressionPreview,
};
use refact_chat_api::pending_context_rebuild::PendingContextRebuild;
use refact_chat_api::{
    ChatMessage, ChatSession, CompressionPhase, CompressionReason, SessionState, ThreadParams,
    TrajectoryCommitIntent,
};
use refact_core::active_context::{
    active_context, legacy_rebuild_input, make_reconstruction_report, requires_explicit_rebuild,
    ReconstructionMetadata,
};
use refact_core::tokens_runtime::count_text_tokens;
use refact_chat_history::chat_compress::CompressChatApplyOutput;

use crate::chat_ctx::ChatCtx;
use crate::global_context::{GlobalContextCtx, SharedGlobalContext};

/// Bound on the reconstruction subchat. Sized above one honest attempt plus its
/// full retry ladder (5 attempts, 5+15+45+120s of backoff, 5 min idle watchdog)
/// and deliberately below `COMPRESSION_ATTEMPT_STALE_AFTER`, so a genuinely hung
/// summarizer still fails through the explicit terminal phase well before the
/// staleness backstop has to step in.
const RECONSTRUCTION_TIMEOUT: Duration = Duration::from_secs(10 * 60);

async fn model_tokenizer(
    gcx: SharedGlobalContext,
    model: &str,
) -> Option<Arc<tokenizers::Tokenizer>> {
    let caps = crate::caps_loader::try_load_caps_quickly_if_not_present(gcx.clone(), 0)
        .await
        .ok()?;
    let record = refact_caps_core::resolve_chat_model(caps, model).ok()?;
    crate::chat_tokens::cached_tokenizer(gcx, &record.base)
        .await
        .ok()
        .flatten()
}

pub fn estimated_tokens(
    messages: &[ChatMessage],
    tokenizer: Option<Arc<tokenizers::Tokenizer>>,
    fresh_usage: bool,
) -> usize {
    let Ok(visible) = refact_chat_history::linearize::apply_summarization_linearize(messages.to_vec())
    else {
        return usize::MAX;
    };
    let count = |messages: &[ChatMessage]| {
        let structural = refact_chat_history::history_limit::compute_context_budget_for_image_mode(
            messages,
            usize::MAX,
            refact_core::provider_types::ImageTokenMode::Provider,
        )
        .used_tokens_estimate;
        let tokens = messages
            .iter()
            .map(|message| {
                let content = message
                    .content
                    .count_tokens(tokenizer.clone(), &None)
                    .unwrap_or(0)
                    .max(0) as usize;
                let calls = message
                    .tool_calls
                    .as_ref()
                    .map(|calls| serde_json::to_string(calls).unwrap_or_default())
                    .unwrap_or_default();
                let calls = count_text_tokens(tokenizer.clone(), &calls)
                    .unwrap_or_else(|_| calls.len() / 3 + 1);
                content.saturating_add(calls).saturating_add(8)
            })
            .fold(0usize, usize::saturating_add);
        structural.max(tokens)
    };
    let estimate = count(&visible);
    if !fresh_usage {
        return estimate;
    }
    visible
        .iter()
        .enumerate()
        .rev()
        .find_map(|(index, message)| {
            if message.role != "assistant" {
                return None;
            }
            let usage = message.usage.as_ref()?;
            let input = usage
                .prompt_tokens
                .saturating_add(usage.cache_read_tokens.unwrap_or(0))
                .saturating_add(usage.cache_creation_tokens.unwrap_or(0));
            (input > 0).then(|| input.saturating_add(count(&visible[index..])))
        })
        .unwrap_or(estimate)
}

/// A prefix can budget the next request only when both mandatory parts are present
/// and the canonical tools are the array the wire adapters serialize.
pub fn budget_prefix_is_usable(prefix: &refact_chat_api::FrozenRequestPrefix) -> bool {
    prefix.schema_version == 1
        && crate::trajectory_prefix::frozen_prefix_is_complete(prefix)
        && prefix
            .tools_canonical
            .as_ref()
            .is_some_and(serde_json::Value::is_array)
}

/// A snapshot-only evaluator: never generates prompts, runs tools, or mutates a session.
/// Missing mandatory request components are an unavailable estimate, not zero tokens.
pub fn payload_request_cap(
    thread: &ThreadParams,
    n_ctx: usize,
    output_reserve: usize,
    tokenizer: Option<Arc<tokenizers::Tokenizer>>,
) -> Result<usize, String> {
    let prefix = thread
        .frozen_request_prefix
        .as_ref()
        .filter(|p| p.schema_version == 1)
        .ok_or("Next-request budget unavailable: no frozen request prefix")?;
    if !budget_prefix_is_usable(prefix) {
        return Err("Next-request budget unavailable: incomplete frozen request prefix".into());
    }
    let system = prefix
        .system_prompt
        .as_ref()
        .ok_or("Next-request budget unavailable: missing system prompt")?;
    let tools = prefix
        .tools_canonical
        .as_ref()
        .filter(|v| v.is_array())
        .ok_or("Next-request budget unavailable: missing canonical tools")?;
    let tools = serde_json::to_string(tools).map_err(|e| e.to_string())?;
    let count = |text: &str| -> Result<usize, String> {
        count_text_tokens(tokenizer.clone(), text)
    };
    let mandatory = count(system)?
        .saturating_add(count(&tools)?)
        .saturating_add(1024);
    thread
        .context_tokens_cap
        .filter(|v| *v > 0)
        .unwrap_or(n_ctx)
        .min(n_ctx)
        .checked_sub(output_reserve)
        .and_then(|v| v.checked_sub(mandatory))
        .filter(|v| *v > 0)
        .ok_or_else(|| "Mandatory system/tools and output reserve exceed request cap".into())
}

pub fn validate_output(
    source: &[ChatMessage],
    output: &[ChatMessage],
    cap: usize,
    tokenizer: Option<Arc<tokenizers::Tokenizer>>,
) -> Result<ReconstructionMetrics, String> {
    if !output
        .iter()
        .any(|m| !m.content.content_text_only().trim().is_empty())
        || pending_tools(output)
    {
        return Err("Reconstruction produced no useful completed context".into());
    }
    let before = estimated_tokens(source, tokenizer.clone(), false);
    let after = estimated_tokens(output, tokenizer, false);
    if before == usize::MAX || after == usize::MAX || after >= before || after > cap {
        return Err(format!("Reconstruction must be smaller and fit request cap (before={before}, after={after}, cap={cap})"));
    }
    Ok(ReconstructionMetrics {
        messages_before: source.len(),
        messages_after: output.len(),
        tokens_before: before,
        tokens_after: after,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReconstructionMetrics {
    messages_before: usize,
    messages_after: usize,
    tokens_before: usize,
    tokens_after: usize,
}

impl ReconstructionMetrics {
    fn to_json(self) -> serde_json::Value {
        let saved = self.tokens_before.saturating_sub(self.tokens_after);
        let reduction_percent = if self.tokens_before == 0 {
            0
        } else {
            saved.saturating_mul(100) / self.tokens_before
        };
        serde_json::json!({
            "messages_before": self.messages_before,
            "messages_after": self.messages_after,
            "tokens_before": self.tokens_before,
            "tokens_after": self.tokens_after,
            "estimated_tokens_saved": saved,
            "reduction_percent": reduction_percent,
        })
    }
}

/// Shared transaction seam: persistence must succeed before publishing any report.
pub async fn commit_report<F, Fut>(
    session: &mut ChatSession,
    report: ChatMessage,
    version: u64,
    attempt: u64,
    thread: &ThreadParams,
    deliveries: &[refact_core::chat_types::PendingDelivery],
    abort: &AtomicBool,
    persist: F,
) -> Result<(), String>
where
    F: FnOnce(refact_chat_history::trajectory_snapshot::TrajectorySnapshot) -> Fut,
    Fut: std::future::Future<Output = Result<(), String>>,
{
    if session.active_compression_attempt != Some(attempt)
        || !idle(session)
        || abort.load(Ordering::SeqCst)
        || session.trajectory_version != version
        || serde_json::to_value(&session.thread).ok() != serde_json::to_value(thread).ok()
        || session.pending_deliveries_for_snapshot() != deliveries
    {
        return Err("Context rebuild canceled: session changed".into());
    }
    let mut snapshot = refact_chat_history::trajectory_snapshot::trajectory_snapshot_from_session(session);
    snapshot.messages.push(report.clone());
    snapshot.version = version.saturating_add(1);
    snapshot.previous_response_id = None;
    snapshot.frozen_request_prefix = None;
    snapshot.claude_code_identity = None;
    persist(snapshot).await?;
    session.add_message(report);
    session.thread.previous_response_id = None;
    session.thread.frozen_request_prefix = None;
    session.thread.claude_code_identity = None;
    session.openai_codex_websocket = Default::default();
    session.reset_cache_guard_snapshot();
    session.provider_usage_stale = true;
    session.trajectory_committed_version = session.trajectory_version;
    session.trajectory_dirty = false;
    session.trajectory_save_error = None;
    session.last_rebuild_attempt_version = Some(session.trajectory_version);
    Ok(())
}

async fn resolve_model(
    gcx: SharedGlobalContext,
    thread: &ThreadParams,
    requested: Option<&str>,
) -> Result<(String, usize), String> {
    let caps = crate::caps_loader::try_load_caps_quickly_if_not_present(gcx, 0)
        .await
        .map_err(|e| e.message)?;
    let model = requested
        .filter(|m| !m.trim().is_empty())
        .unwrap_or(&thread.model);
    let model = if model.is_empty() {
        caps.defaults.chat_default_model.as_str()
    } else {
        model
    };
    let record = refact_caps_core::resolve_chat_model(caps.clone(), model).map_err(|e| e.to_string())?;
    Ok((model.to_string(), record.base.n_ctx))
}

/// The mode system prompt as the mode registry defines it. `None` mode keeps the
/// pre-move behaviour: an empty prompt plus the same warning.
async fn mode_system_prompt(ctx: &ChatCtx, mode_id: &str, model_id: Option<&str>) -> String {
    let mode_id = refact_chat_api::setparams::map_legacy_mode_to_id(mode_id);
    match ctx.mode_config.get_mode_config(mode_id, model_id).await {
        Some(mode_config) => mode_config.prompt,
        None => {
            tracing::warn!("Mode '{}' not found, using empty prompt", mode_id);
            String::new()
        }
    }
}

pub async fn preview_manual_context_rebuild(
    gcx: SharedGlobalContext,
    state: SessionState,
    compression_active: bool,
    messages: &[ChatMessage],
    thread: &ThreadParams,
    requested_model: Option<&str>,
) -> ManualCompressionPreview {
    let input = if requires_explicit_rebuild(messages) {
        legacy_rebuild_input(messages)
    } else {
        active_context(messages).map(|v| v.messages)
    };
    let mut preview = ManualCompressionPreview {
        eligible: false,
        trajectory_version: None,
        resolved_model: None,
        context_window: None,
        source_messages: 0,
        approximate_source_tokens: 0,
        reason: None,
    };
    let input = match input {
        Ok(v) => v,
        Err(e) => {
            preview.reason = Some(e.to_string());
            return preview;
        }
    };
    preview.source_messages = input.len();
    preview.approximate_source_tokens = refact_chat_history::trajectory_ops::approx_token_count(&input);
    if !matches!(
        state,
        SessionState::Idle | SessionState::Error | SessionState::Completed
    ) || compression_active
        || pending_tools(&input)
        || input.is_empty()
    {
        preview.reason = Some("Rebuild requires an idle session and completed tool results".into());
        return preview;
    }
    match resolve_model(gcx, thread, requested_model).await {
        Ok((model, n_ctx)) => {
            preview.resolved_model = Some(model);
            preview.context_window = Some(n_ctx);
            preview.eligible = true;
        }
        Err(e) => preview.reason = Some(e),
    }
    preview
}

/// Build off-lock, then CAS and durably append a report. The disk-only commit is
/// serialized with the ordinary writer. No provider/network await holds a lock.
pub async fn rebuild_session(
    gcx: SharedGlobalContext,
    session_arc: &Arc<Mutex<ChatSession>>,
    request: PendingContextRebuild,
    expected_version: Option<u64>,
    explicit: bool,
) -> Result<(), String> {
    let (messages, thread, version, attempt, abort, save_mutex, deliveries, catalog) = {
        let mut session = session_arc.lock().await;
        if expected_version.is_some_and(|v| v != session.trajectory_version) {
            return Err("Chat changed since preview; preview again".into());
        }
        if session.pending_mode_handoff.is_some() || session.pending_context_rebuild.is_some() {
            return Err("Another context operation is pending".into());
        }
        if explicit {
            prepare_explicit_idle(&mut session);
        }
        if !idle(&session) || compression_attempt_active(&session) {
            return Err("Rebuild requires an idle session with no active stream".into());
        }
        if !explicit && requires_explicit_rebuild(&session.messages) {
            return Err("Legacy context requires an explicit rebuild".into());
        }
        let messages = if explicit && requires_explicit_rebuild(&session.messages) {
            legacy_rebuild_input(&session.messages)
        } else {
            active_context(&session.messages).map(|v| v.messages)
        }
        .map_err(|e| e.to_string())?;
        if messages.is_empty() || pending_tools(&messages) {
            return Err("Rebuild requires nonempty context and completed tool results".into());
        }
        if !explicit && session.last_rebuild_attempt_version == Some(session.trajectory_version) {
            return Err("Rebuild was already attempted for this context".into());
        }
        session.last_rebuild_attempt_version = Some(session.trajectory_version);
        session.compression_attempt_generation =
            session.compression_attempt_generation.wrapping_add(1);
        let attempt = session.compression_attempt_generation;
        let abort = Arc::new(AtomicBool::new(false));
        session.active_compression_attempt = Some(attempt);
        session.compression_abort_flag = Some(abort.clone());
        session.compression_attempt_started_at_ms =
            Some(chrono::Utc::now().timestamp_millis() as u64);
        status(&mut session, CompressionPhase::Running, None);
        (
            messages,
            session.thread.clone(),
            session.trajectory_version,
            attempt,
            abort,
            session.trajectory_save_mutex.clone(),
            session.pending_deliveries_for_snapshot(),
            session.tool_catalog.clone(),
        )
    };
    let ctx = gcx.chat_ctx(gcx.clone());
    let _guard = AttemptGuard {
        session: session_arc.clone(),
        attempt,
        abort: abort.clone(),
    };
    let (target_model, n_ctx) = resolve_model(gcx.clone(), &thread, None).await?;
    let tokenizer = model_tokenizer(gcx.clone(), &target_model).await;
    let caps = crate::caps_loader::try_load_caps_quickly_if_not_present(gcx.clone(), 0)
        .await
        .map_err(|e| e.message)?;
    let record = refact_caps_core::resolve_chat_model(caps, &target_model).map_err(|e| e.to_string())?;
    let output_reserve = thread
        .max_tokens
        .or(record.default_max_tokens)
        .or(record.max_output_tokens)
        .unwrap_or(4096);
    // Rebuild clears the persisted prefix. Prepare a local-only replacement when
    // absent or incomplete (first-use, legacy, and repaired transition chats),
    // using the reserved catalog.
    let mut budget_thread = thread.clone();
    if !budget_thread
        .frozen_request_prefix
        .as_ref()
        .is_some_and(budget_prefix_is_usable)
    {
        let app = crate::global_context::app_state_from_gcx(gcx.clone()).await;
        let scope = thread
            .worktree
            .as_ref()
            .map(|w| w.root.to_string_lossy().into_owned());
        let tools = match catalog {
            Some(catalog) => catalog.index.tools.clone(),
            None => app
                .tool_registry
                .get_tools_index_for_mode_and_scope(
                    &thread.mode,
                    Some(&record.base.id),
                    scope.as_deref(),
                )
                .await
                .tools
                .clone(),
        };
        let canonical = crate::chat_prepare::build_canonical_openai_tools(
            gcx.chat_ctx(gcx.clone()),
            &tools,
            record.supports_strict_tools,
            record.supports_tools,
        )
        .await;
        let meta = refact_chat_api::ChatMeta {
            chat_id: thread.id.clone(),
            chat_mode: thread.mode.clone(),
            chat_remote: false,
            current_config_file: String::new(),
            context_tokens_cap: thread.context_tokens_cap,
            include_project_info: thread.include_project_info,
            request_attempt_id: String::new(),
            worktree: thread.worktree.clone(),
        };
        // Do not call the preamble mixer: task briefing can run another LLM.
        let preamble = if let Some(system) = messages.iter().find(|m| m.role == "system") {
            system.content.content_text_only()
        } else {
            let base = mode_system_prompt(&ctx, &thread.mode, Some(&target_model)).await;
            ctx.prompts
                .system_prompt_add_extra_instructions(
                base,
                tools.iter().map(|t| t.name.clone()).collect(),
                &meta,
                &thread.task_meta,
                &thread.mode,
            )
            .await
        };
        budget_thread.frozen_request_prefix = Some(refact_chat_api::FrozenRequestPrefix {
            schema_version: 1,
            created_at: String::new(),
            system_prompt: Some(preamble),
            tools_canonical: Some(serde_json::Value::Array(canonical.tools)),
        });
    }
    let request_cap =
        payload_request_cap(&budget_thread, n_ctx, output_reserve, tokenizer.clone())?;
    let outcome = tokio::time::timeout(
        RECONSTRUCTION_TIMEOUT,
        crate::mode_transition_runtime::reconstruct_context(
            gcx.clone(),
            refact_agentic::mode_transition::ReconstructionRequest {
                messages: &messages,
                target_mode: &thread.mode,
                target_mode_description: "Continue the current conversation in the same mode",
                parent_chat_id: Some(&thread.id),
                model_override: request.model,
                abort_flag: Some(abort.clone()),
                hints: None,
                target_budget_symbols: Some(request_cap),
                preserve_goal_messages: true,
            },
        ),
    )
    .await;
    let outcome = match outcome {
        Ok(Ok(outcome)) => Ok(outcome),
        // A hung summarizer must reach a terminal phase rather than hold the
        // reservation open forever. Route the elapsed case through the identical
        // terminal-phase path as a provider error.
        Ok(Err(e)) => Err(e),
        Err(_) => Err(format!(
            "Context reconstruction timed out after {}s",
            RECONSTRUCTION_TIMEOUT.as_secs()
        )),
    };
    let outcome = match outcome {
        Ok(v) => v,
        Err(e) => {
            let mut s = session_arc.lock().await;
            if s.active_compression_attempt == Some(attempt) {
                status(
                    &mut s,
                    CompressionPhase::Failed,
                    Some(CompressionReason::TransientFailure),
                );
            }
            return Err(e);
        }
    };
    let metrics = validate_output(&messages, &outcome.messages, request_cap, tokenizer)?;
    let report = make_reconstruction_report(
        outcome.messages,
        ReconstructionMetadata {
            source_version: Some(version),
            model: Some(outcome.model),
            trigger: Some(request.trigger),
            from_mode: Some(thread.mode.clone()),
            to_mode: Some(thread.mode.clone()),
            metrics: Some(metrics.to_json()),
        },
    )
    .map_err(|e| e.to_string());
    // Shield the disk-only commit from caller cancellation once it starts. The
    // guard still aborts/checks before this boundary; persistence and publication
    // then finish together under the existing writer/session locks.
    let session_arc = session_arc.clone();
    tokio::spawn(async move {
        let _writer = save_mutex.lock().await;
        let mut session = session_arc.lock().await;
        let result = match report {
            Ok(report) => {
                commit_report(
                    &mut session,
                    report,
                    version,
                    attempt,
                    &thread,
                    &deliveries,
                    &abort,
                    |snapshot| async move {
                        crate::trajectories::store::persist_trajectory_snapshot_with_intent(
                            gcx,
                            snapshot,
                            TrajectoryCommitIntent::Required,
                        )
                        .await
                    },
                )
                .await
            }
            Err(error) => Err(error),
        };
        if session.active_compression_attempt == Some(attempt) {
            status(
                &mut session,
                if result.is_ok() {
                    CompressionPhase::Applied
                } else {
                    CompressionPhase::Failed
                },
                result
                    .as_ref()
                    .err()
                    .map(|_| CompressionReason::SourceChanged),
            );
        }
        result
    })
    .await
    .map_err(|e| e.to_string())?
}

pub async fn apply_manual_context_rebuild(
    gcx: SharedGlobalContext,
    session_arc: &Arc<Mutex<ChatSession>>,
    requested_model: Option<&str>,
    expected_version: Option<u64>,
) -> ManualCompressionApplyResult {
    let (messages, thread, state, active, version) = {
        let s = session_arc.lock().await;
        (
            s.messages.clone(),
            s.thread.clone(),
            s.runtime.state,
            compression_attempt_active(&s),
            s.trajectory_version,
        )
    };
    let preview = preview_manual_context_rebuild(
        gcx.clone(),
        state,
        active,
        &messages,
        &thread,
        requested_model,
    )
    .await;
    let before = preview.approximate_source_tokens;
    let result = if expected_version != Some(version) {
        Err("Chat changed since preview; preview again".into())
    } else if !preview.eligible {
        Err(preview.reason.clone().unwrap_or_default())
    } else {
        rebuild_session(
            gcx,
            session_arc,
            PendingContextRebuild {
                model: requested_model.map(str::to_owned),
                trigger: "manual".into(),
            },
            expected_version,
            true,
        )
        .await
    };
    let after = {
        let s = session_arc.lock().await;
        active_context(&s.messages)
            .map(|v| v.messages)
            .unwrap_or_else(|_| messages.clone())
    };
    ManualCompressionApplyResult {
        applied: result.is_ok(),
        resolved_model: preview.resolved_model,
        context_window: preview.context_window,
        source_messages: preview.source_messages,
        approximate_source_tokens: before,
        before_message_count: preview.source_messages,
        after_message_count: after.len(),
        before_approx_tokens: before,
        after_approx_tokens: refact_chat_history::trajectory_ops::approx_token_count(&after),
        reason: result.err(),
    }
}

pub async fn apply_context_rebuild_with_reason(
    gcx: SharedGlobalContext,
    session: &Arc<Mutex<ChatSession>>,
    thread: &ThreadParams,
    force: bool,
    _reason: Option<CompressionReason>,
) -> CompactionOutcome {
    if !thread.auto_compact_enabled_effective() {
        return CompactionOutcome::NothingToCompact;
    }
    if !force {
        let Ok((model, n_ctx)) = resolve_model(gcx.clone(), thread, None).await else {
            return CompactionOutcome::NothingToCompact;
        };
        let tokenizer = model_tokenizer(gcx.clone(), &model).await;
        let mut s = session.lock().await;
        if !idle(&s)
            || s.last_rebuild_attempt_version == Some(s.trajectory_version)
            || serde_json::to_value(&s.thread).ok() != serde_json::to_value(thread).ok()
        {
            return CompactionOutcome::NothingToCompact;
        }
        let Ok(view) = active_context(&s.messages) else {
            return CompactionOutcome::NothingToCompact;
        };
        let (cap, cap_source) = resolve_effective_compression_cap(
            n_ctx,
            thread.context_tokens_cap,
            thread.auto_compression_cap,
        );
        let used = estimated_tokens(&view.messages, tokenizer, !s.provider_usage_stale);
        // usize::MAX means the local linearization failed. Report it as an
        // unavailable estimate so the diagnostics never display it as a real
        // (and absurdly large) context size, while the gate still compares it.
        s.set_compression_gate_diagnostics((used != usize::MAX).then_some(used), cap, cap_source);
        if !at_auto_cap(used, cap) {
            return CompactionOutcome::NothingToCompact;
        }
    }
    match rebuild_session(
        gcx,
        session,
        PendingContextRebuild {
            model: None,
            trigger: if force { "overflow" } else { "automatic" }.into(),
        },
        None,
        false,
    )
    .await
    {
        Ok(()) => CompactionOutcome::Applied,
        Err(e) => {
            let mut s = session.lock().await;
            if compression_attempt_active(&s) || s.abort_flag.load(Ordering::SeqCst) {
                return CompactionOutcome::NothingToCompact;
            }
            let safe_err = refact_chat_api::diagnostics::safe_provider_error_diagnostic(&e);
            s.append_error_message_deduped(&format!(
                "Context rebuild failed: {}",
                safe_err
            ));
            s.last_rebuild_attempt_version = Some(s.trajectory_version);
            s.set_runtime_state(SessionState::Error, Some(format!("Context rebuild failed: {}", safe_err)));
            CompactionOutcome::LlmUnavailable
        }
    }
}

/// Last-resort, model-free recovery: run one maximal deterministic compression
/// sweep over the whole active context so a conversation that outgrew the
/// summarizer's input budget can still be saved and generation can proceed.
pub async fn apply_deterministic_compaction_for_recovery(
    gcx: SharedGlobalContext,
    session: &Arc<Mutex<ChatSession>>,
) -> bool {
    let stored = session.lock().await.messages.clone();
    let projection = match refact_core::active_context::active_context(&stored) {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!("deterministic recovery skipped: active context unavailable: {e}");
            return false;
        }
    };
    let tool_call_names: std::collections::HashMap<String, String> = stored
        .iter()
        .filter_map(|m| m.tool_calls.as_ref())
        .flatten()
        .map(|tc| (tc.id.clone(), tc.function.name.clone()))
        .collect();
    let drop_context_files = std::collections::HashSet::new();
    let drop_memories = std::collections::HashSet::new();
    let truncate_tool_outputs = std::collections::HashSet::new();
    let drop_tool_outputs = std::collections::HashSet::new();
    let drop_context_messages = std::collections::HashSet::new();
    let request = refact_chat_history::chat_compress::build_maximal_full_sweep_request(
        &drop_context_files,
        &drop_memories,
        &truncate_tool_outputs,
        &drop_tool_outputs,
        &drop_context_messages,
        &tool_call_names,
    );
    let CompressChatApplyOutput {
        messages: head_messages,
        report,
        affected_boundary,
        modifiable_len,
        ..
    } = refact_chat_history::chat_compress::compress_chat_apply_head_output(
        projection.messages.clone(),
        &[],
        &request,
    );
    if head_messages.len() == projection.messages.len() && report.is_none() {
        return false;
    }
    let mut head_messages = head_messages;
    refact_chat_history::chat_compress::insert_current_compression_report(
        &mut head_messages,
        report,
        affected_boundary,
        modifiable_len,
    );
    let updated = match refact_core::active_context::writeback_active_context(
        &stored,
        &projection,
        &head_messages,
    ) {
        Ok(updated) => updated,
        Err(e) => {
            tracing::warn!("deterministic recovery skipped: writeback failed: {e}");
            return false;
        }
    };
    let mut s = session.lock().await;
    if compression_attempt_active(&s) || !compaction_safe(&s) {
        return false;
    }
    s.replace_messages(updated);
    s.thread.previous_response_id = None;
    s.thread.frozen_request_prefix = None;
    s.thread.claude_code_identity = None;
    s.openai_codex_websocket = Default::default();
    s.reset_cache_guard_snapshot();
    s.provider_usage_stale = true;
    status(&mut s, CompressionPhase::Applied, None);
    drop(s);
    crate::trajectories::store::maybe_save_trajectory_with_intent(
        crate::global_context::app_state_from_gcx(gcx).await,
        session.clone(),
        TrajectoryCommitIntent::Required,
    )
    .await;
    true
}

/// Drain only after every sibling tool result and post-tool side effect landed.
pub async fn drain_pending(
    gcx: SharedGlobalContext,
    session_arc: &Arc<Mutex<ChatSession>>,
) -> Result<bool, String> {
    let (rebuild, handoff) = {
        let mut s = session_arc.lock().await;
        if !idle(&s) {
            return Ok(false);
        }
        (
            s.pending_context_rebuild.take(),
            s.pending_mode_handoff.take(),
        )
    };
    if let Some(payload) = handoff {
        gcx.chat_ctx(gcx.clone())
            .pending_handoff
            .finalize_pending_handoff(session_arc, payload)
            .await?;
        return Ok(true);
    }
    if let Some(request) = rebuild {
        rebuild_session(gcx, session_arc, request, None, true).await?;
    }
    Ok(false)
}
