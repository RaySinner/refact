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
use super::recovery::*;
use super::run_loop::*;
use super::stream::*;

pub(in crate::chat::generation) fn model_type_defaults_for_thread<'a>(
    user_defaults: &'a ProviderDefaults,
    defaults: &crate::caps::DefaultModels,
    thread: &ThreadParams,
    model_id: &str,
) -> &'a ModelTypeDefaults {
    if thread
        .task_meta
        .as_ref()
        .is_some_and(|meta| meta.role == "agents")
    {
        user_defaults.defaults_for_task_planner_agent_model(
            model_id,
            &defaults.task_planner_agent_model,
            &defaults.chat_default_model,
            &defaults.chat_model_2,
            &defaults.chat_light_model,
            &defaults.chat_thinking_model,
            &defaults.chat_buddy_model,
        )
    } else {
        user_defaults.defaults_for_model(
            model_id,
            &defaults.chat_default_model,
            &defaults.chat_model_2,
            &defaults.chat_light_model,
            &defaults.chat_thinking_model,
            &defaults.chat_buddy_model,
        )
    }
}

pub(in crate::chat::generation) async fn user_stop_requested(
    session_arc: &Arc<AMutex<ChatSession>>,
) -> bool {
    let session = session_arc.lock().await;
    session.user_interrupt_flag.load(Ordering::SeqCst)
}

pub(in crate::chat::generation) fn check_aborted_before_stream(
    abort_flag: &AtomicBool,
) -> Result<(), LlmStreamError> {
    if abort_flag.load(Ordering::SeqCst) {
        Err(LlmStreamError::from(ABORT_ERROR_MESSAGE.to_string()))
    } else {
        Ok(())
    }
}

pub(in crate::chat::generation) fn make_runtime_event(
    signal_type: &str,
    title: &str,
    source: &str,
    dedupe_key: &str,
    status: &str,
    priority: Option<&str>,
) -> BuddyRuntimeEvent {
    BuddyRuntimeEvent {
        id: Uuid::new_v4().to_string(),
        signal_type: signal_type.to_string(),
        title: title.to_string(),
        description: None,
        source: source.to_string(),
        status: status.to_string(),
        failure_category: None,
        failure_summary: None,
        progress: None,
        dedupe_key: Some(dedupe_key.to_string()),
        priority: priority.unwrap_or("normal").to_string(),
        created_at: chrono::Utc::now().to_rfc3339(),
        ttl_ms: None,
        bubble_policy: None,
        speech_text: None,
        scene: None,
        duration_hint: None,
        persistent: false,
        controls: Vec::new(),
        chat_id: None,
        dismissed: false,
    }
}

pub(in crate::chat::generation) fn maybe_inject_token_budget_instruction(
    session: &mut ChatSession,
    effective_n_ctx: usize,
    cadence: usize,
    image_token_mode: refact_core::provider_types::ImageTokenMode,
) -> bool {
    let Ok(active) = refact_core::active_context::active_context(&session.messages) else {
        return false;
    };
    let messages = &active.messages;
    let used_tokens = refact_chat_history::trajectory_ops::approx_token_count_for_image_mode(
        messages,
        image_token_mode,
    );
    let remaining = effective_n_ctx.saturating_sub(used_tokens);
    let last_has_tool_calls = messages
        .last()
        .map(|msg| {
            msg.role == "assistant"
                && msg
                    .tool_calls
                    .as_ref()
                    .map(|tcs| !tcs.is_empty())
                    .unwrap_or(false)
        })
        .unwrap_or(false);
    if last_has_tool_calls {
        return false;
    }

    if messages
        .last()
        .is_some_and(|message| length_stop_kind(message).is_some())
    {
        return false;
    }

    let mut last_marker_idx = None;
    let mut user_or_assistant_since = 0usize;

    for (idx, msg) in messages.iter().enumerate().rev() {
        if msg.role == "cd_instruction" && msg.tool_call_id == TOKEN_BUDGET_MARKER {
            last_marker_idx = Some(idx);
            break;
        }
    }

    for (idx, msg) in messages.iter().enumerate().rev() {
        if let Some(marker_idx) = last_marker_idx {
            if idx <= marker_idx {
                break;
            }
        }
        if msg.role == "user" || msg.role == "assistant" {
            user_or_assistant_since += 1;
        }
    }

    if user_or_assistant_since < cadence {
        return false;
    }

    if messages
        .iter()
        .rev()
        .take(cadence)
        .any(|msg| msg.role == "cd_instruction" && msg.tool_call_id == TOKEN_BUDGET_MARKER)
    {
        return false;
    }

    let pct_used = if effective_n_ctx > 0 {
        used_tokens.saturating_mul(100) / effective_n_ctx
    } else {
        0
    };

    let message = ChatMessage {
        role: "cd_instruction".to_string(),
        tool_call_id: TOKEN_BUDGET_MARKER.to_string(),
        content: ChatContent::SimpleText(format!(
            "ЁЯТ┐ Token budget: ~{} used / ~{} available (~{}% used). ~{} tokens remaining. Consider using compress_chat_probe() if running low.",
            used_tokens,
            effective_n_ctx,
            pct_used,
            remaining
        )),
        ..Default::default()
    };
    session.add_message(message);
    true
}

pub(in crate::chat::generation) fn build_mcp_lazy_hint(total: usize) -> String {
    format!(
        "ЁЯТ┐ MCP Tools тАФ Lazy Mode Active ({total} tools available). \
         You MUST call `mcp_tool_search` before using any MCP tool. \
         Search by capability or provider, or use \
         mcp_tool_search({{\"query\": \".\", \"max_results\": {total}}}) to list all available MCP tools."
    )
}

pub(in crate::chat::generation) fn inject_mcp_lazy_hint(
    session: &mut ChatSession,
    total: usize,
) -> bool {
    let active = match refact_core::active_context::active_context(&session.messages) {
        Ok(active) => active.messages,
        Err(error) => {
            warn!("Skipping MCP lazy hint for {}: {}", session.chat_id, error);
            return false;
        }
    };
    if active.iter().any(|message| {
        message.role == "cd_instruction" && message.tool_call_id == MCP_LAZY_INDEX_MARKER
    }) {
        return false;
    }

    let insert_pos = active
        .iter()
        .position(|message| message.role == "system")
        .map(|index| index + 1)
        .unwrap_or(0);
    let hint = ChatMessage {
        role: "cd_instruction".to_string(),
        tool_call_id: MCP_LAZY_INDEX_MARKER.to_string(),
        content: ChatContent::SimpleText(build_mcp_lazy_hint(total)),
        ..Default::default()
    };
    match session.insert_active_message(insert_pos, hint) {
        Ok(_) => true,
        Err(error) => {
            warn!("Failed to inject MCP lazy hint: {}", error);
            false
        }
    }
}

pub(in crate::chat::generation) fn is_preamble_conversation_start(message: &ChatMessage) -> bool {
    message.role == "user"
        || message.role == "assistant"
        || message.role == crate::chat::internal_roles::EVENT_ROLE
}

pub(in crate::chat::generation) fn preamble_already_present(
    active_view: &[ChatMessage],
    msg: &ChatMessage,
) -> bool {
    if !msg.message_id.is_empty() && active_view.iter().any(|m| m.message_id == msg.message_id) {
        return true;
    }
    match msg.role.as_str() {
        "assistant" | "compression_report" => true,
        "system" => active_view.first().is_some_and(|m| m.role == "system"),
        "cd_instruction" => active_view.iter().any(|m| m.role == "cd_instruction"),
        "context_file" => active_view
            .iter()
            .any(|m| m.role == "context_file" && m.tool_call_id == msg.tool_call_id),
        _ => false,
    }
}

/// Persist the preamble messages the prompt mixer placed ahead of the conversation
/// into the active view, so they reach the model even after a context rebuild.
pub(in crate::chat::generation) fn persist_preamble_messages(
    session: &mut ChatSession,
    messages_with_preamble: &[ChatMessage],
) -> usize {
    let first_conv_idx = messages_with_preamble
        .iter()
        .position(is_preamble_conversation_start)
        .unwrap_or(messages_with_preamble.len());
    let Ok(active) = refact_core::active_context::active_context(&session.messages) else {
        return 0;
    };
    let mut active_view = active.messages;
    let mut system_insert_idx = 0;
    let mut context_insert_idx = active_view
        .iter()
        .position(|m| m.role == "system")
        .map(|i| i + 1)
        .unwrap_or(0);
    let mut inserted = 0;
    for msg in messages_with_preamble.iter().take(first_conv_idx) {
        if preamble_already_present(&active_view, msg) {
            continue;
        }
        let insert_idx = if msg.role == "system" {
            let idx = system_insert_idx;
            system_insert_idx += 1;
            context_insert_idx += 1;
            idx
        } else {
            let idx = context_insert_idx;
            context_insert_idx += 1;
            idx
        };
        match session.insert_active_message(insert_idx, msg.clone()) {
            Ok(_) => {
                active_view.insert(insert_idx.min(active_view.len()), msg.clone());
                inserted += 1;
            }
            Err(error) => {
                warn!(
                    "Failed to persist preamble message ({}): {}",
                    msg.role, error
                );
                break;
            }
        }
    }
    inserted
}

pub(in crate::chat::generation) fn enrichment_insertion_index(
    session: &ChatSession,
    turn: &EnrichmentTurnIdentity,
    identity: Option<&str>,
) -> Option<usize> {
    let user_index = session
        .messages
        .iter()
        .rposition(|message| message.role == "user")?;
    let user_message = session.messages.get(user_index)?;
    (session.trajectory_version == turn.revision
        && user_index == session.messages.len().saturating_sub(1)
        && user_message.message_id == turn.latest_user_id
        && enrichment_query_fingerprint(&user_message.content.content_text_only())
            == turn.query_fingerprint
        && identity.is_some_and(|identity| !session.has_enrichment_identity(identity)))
    .then_some(user_index)
}

// Construct checkpoint futures inside their own poll frame. In debug builds,
// each inline await otherwise reserves large construction temporaries in the
// caller, even while a different branch (such as generation) is being polled.
pub(in crate::chat) fn maybe_save_trajectory_with_intent(
    app: AppState,
    session_arc: Arc<AMutex<ChatSession>>,
    intent: TrajectoryCommitIntent,
) -> futures::future::BoxFuture<'static, ()> {
    Box::pin(async move {
        crate::chat::trajectories::maybe_save_trajectory_with_intent(app, session_arc, intent)
            .await;
    })
}

pub fn prepare_session_preamble_and_knowledge(
    app: AppState,
    session_arc: Arc<AMutex<ChatSession>>,
) -> futures::future::BoxFuture<'static, ()> {
    Box::pin(async move {
        ensure_session_preamble_inner(app.clone(), session_arc.clone()).await;
        prepare_session_knowledge_inner(app, session_arc).await;
    })
}

/// A rebuilt or handed-off context starts without its preamble, so the generation
/// loop re-checks the active head before every request; the full preamble pipeline
/// only runs when the system prompt is actually missing.
pub fn ensure_session_preamble(
    app: AppState,
    session_arc: Arc<AMutex<ChatSession>>,
) -> futures::future::BoxFuture<'static, ()> {
    Box::pin(async move {
        let system_missing = {
            let session = session_arc.lock().await;
            refact_core::active_context::active_context(&session.messages)
                .is_ok_and(|active| !active.messages.first().is_some_and(|m| m.role == "system"))
        };
        if system_missing {
            ensure_session_preamble_inner(app, session_arc).await;
        }
    })
}

pub(in crate::chat::generation) async fn ensure_session_preamble_inner(
    app: AppState,
    session_arc: Arc<AMutex<ChatSession>>,
) {
    let (thread, chat_id, has_system, has_project_context) = {
        let session = session_arc.lock().await;
        let active = match refact_core::active_context::active_context(&session.messages) {
            Ok(active) => active.messages,
            Err(error) => {
                warn!("Skipping preamble for {}: {}", session.chat_id, error);
                return;
            }
        };
        let has_sys = active.first().is_some_and(|m| m.role == "system");
        let has_proj = active.iter().any(|m| {
            m.role == "context_file"
                && m.tool_call_id == crate::chat::system_context::PROJECT_CONTEXT_MARKER
        });
        (
            session.thread.clone(),
            session.chat_id.clone(),
            has_sys,
            has_proj,
        )
    };

    let needs_preamble = !has_system || (!has_project_context && thread.include_project_info);

    // Populated inside `needs_preamble`; used after to inject the MCP lazy-mode hint message.
    let mut mcp_lazy_total: Option<usize> = None;

    if needs_preamble {
        let caps = match app.ctx.caps.try_load_caps_quickly(0).await {
            Ok(caps) => caps,
            Err(e) => {
                warn!("Failed to load caps for preamble: {}", e.message);
                return;
            }
        };
        let model_rec = match crate::caps::resolve_chat_model(caps.clone(), &thread.model) {
            Ok(rec) => rec,
            Err(e) => {
                warn!("Failed to resolve model for preamble: {}", e);
                return;
            }
        };

        let execution_scope = thread
            .worktree
            .as_ref()
            .map(|worktree| worktree.root.to_string_lossy().into_owned());
        let tools_for_mode = app
            .tool_registry
            .get_tools_index_for_mode_and_scope(
                &thread.mode,
                Some(&model_rec.base.id),
                execution_scope.as_deref(),
            )
            .await;
        if tools_for_mode.mcp_lazy_mode {
            mcp_lazy_total = Some(tools_for_mode.mcp_total_count);
        }
        let tool_names: std::collections::HashSet<String> = tools_for_mode
            .tools
            .iter()
            .map(|t| t.name.clone())
            .collect();

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

        let messages = {
            let session = session_arc.lock().await;
            match refact_core::active_context::active_context(&session.messages) {
                Ok(active) => active.messages,
                Err(error) => {
                    warn!("Skipping preamble for {}: {}", session.chat_id, error);
                    return;
                }
            }
        };
        let mut has_rag_results = crate::scratchpads::scratchpad_utils::HasRagResults::new();
        let (messages_with_preamble, skills_info) =
            prepend_the_right_system_prompt_and_maybe_more_initial_messages(
                app.clone(),
                messages,
                &meta,
                &thread.task_meta,
                &mut has_rag_results,
                tool_names,
                &thread.mode,
                &thread.model,
            )
            .await;

        let mut session = session_arc.lock().await;
        session.skills_available_count = skills_info.available_count;
        session.skills_included = skills_info.included_names.clone();
        let inserted = persist_preamble_messages(&mut session, &messages_with_preamble);
        if inserted > 0 {
            info!("Saved {} preamble messages to session", inserted);
        }
    }

    // Inject MCP lazy-mode discovery hint (once per session, idempotent via marker).
    // Detailed names and schemas are returned only when mcp_tool_search is called.
    if let Some(mcp_total) = mcp_lazy_total {
        let inserted = {
            let mut session = session_arc.lock().await;
            inject_mcp_lazy_hint(&mut session, mcp_total)
        };
        if inserted {
            info!("Injected MCP lazy discovery hint with {} tools", mcp_total);
        }
    }
}

pub(in crate::chat::generation) async fn prepare_session_knowledge_inner(
    app: AppState,
    session_arc: Arc<AMutex<ChatSession>>,
) {
    let gcx = app.gcx.clone();
    let (thread, chat_id) = {
        let session = session_arc.lock().await;
        (session.thread.clone(), session.chat_id.clone())
    };

    let (
        last_is_user,
        auto_enrichment_enabled,
        user_count,
        has_manual_enrichment_for_turn,
        suppress_flag,
        enrichment_turn,
    ) = {
        let snapshot_started = perf_diagnostics::is_enabled().then(std::time::Instant::now);
        let mut session = session_arc.lock().await;
        let last_user_idx = session
            .messages
            .iter()
            .rposition(|m| is_prompt_turn_role(&m.role));
        let last_user =
            last_user_idx.is_some() && last_user_idx == session.messages.len().checked_sub(1);
        let auto = session.thread.auto_enrichment_enabled.unwrap_or(false);
        let count = session
            .messages
            .iter()
            .filter(|m| is_prompt_turn_role(&m.role))
            .count();
        let manual = last_user_idx
            .and_then(|idx| idx.checked_sub(1))
            .and_then(|idx| session.messages.get(idx))
            .map(|m| m.role == "context_file" && m.tool_call_id == "manual_memory_enrichment")
            .unwrap_or(false);
        let suppress = session.suppress_auto_enrichment_for_next_turn;
        if suppress {
            session.suppress_auto_enrichment_for_next_turn = false;
        }
        let enrichment_turn = last_user_idx
            .filter(|idx| *idx == session.messages.len().saturating_sub(1))
            .and_then(|idx| session.messages.get(idx))
            .filter(|message| message.role == "user")
            .map(|message| EnrichmentTurnIdentity {
                revision: session.trajectory_version,
                latest_user_id: message.message_id.clone(),
                query_fingerprint: enrichment_query_fingerprint(
                    &message.content.content_text_only(),
                ),
            });
        let message_bytes = perf_diagnostics::is_enabled().then(|| {
            session
                .messages
                .iter()
                .map(|message| {
                    serde_json::to_vec(message)
                        .map(|encoded| encoded.len())
                        .unwrap_or(0)
                })
                .sum::<usize>()
        });
        perf_diagnostics::record_enrichment(
            PerfComponent::EnrichmentSessionSnapshot,
            &chat_id,
            PerfOutcome::Success,
            snapshot_started
                .map(|started| started.elapsed().as_micros().try_into().unwrap_or(u64::MAX))
                .unwrap_or(0),
            message_bytes.map(|bytes| bytes as u64),
            Some(session.messages.len() as u64),
            message_bytes.map(|bytes| (bytes as u64).saturating_add(3) / 4),
        );
        (last_user, auto, count, manual, suppress, enrichment_turn)
    };
    if is_agentic_mode_id(&thread.mode)
        && last_is_user
        && auto_enrichment_enabled
        && !has_manual_enrichment_for_turn
        && !suppress_flag
        && enrichment_turn.is_some()
    {
        let force_enrichment = user_count > 1;
        let mut messages = {
            let session = session_arc.lock().await;
            session.messages.clone()
        };
        let msg_count_before = messages.len();
        enrich_messages_with_knowledge(
            gcx.clone(),
            &mut messages,
            Some(&chat_id),
            force_enrichment,
        )
        .await;
        if messages.len() > msg_count_before {
            let local_last_user_idx = messages
                .iter()
                .rposition(|m| is_prompt_turn_role(&m.role))
                .unwrap_or(0);
            if local_last_user_idx > 0 {
                let enriched_msg = &messages[local_last_user_idx - 1];
                if enriched_msg.role == "context_file" {
                    let mut session = session_arc.lock().await;
                    let turn = enrichment_turn
                        .as_ref()
                        .expect("turn checked before enrichment");
                    let identity = enrichment_identity_from_context(enriched_msg);
                    let session_last_user_idx =
                        enrichment_insertion_index(&session, turn, identity.as_deref());
                    let insertion_stale = session_last_user_idx.is_none();
                    if let Some(session_last_user_idx) = session_last_user_idx {
                        let identity = identity.expect("identity checked before insertion");
                        if let Some(user_message) = session.messages.get_mut(session_last_user_idx)
                        {
                            record_enrichment_identity_on_user_message(
                                user_message,
                                &identity,
                                &turn.query_fingerprint,
                            );
                        }
                        session.record_enrichment_identity(identity);
                        session.insert_message(session_last_user_idx, enriched_msg.clone());
                    }
                    if insertion_stale {
                        perf_diagnostics::record_enrichment(
                            PerfComponent::EnrichmentInsertionStale,
                            &chat_id,
                            PerfOutcome::Skipped,
                            0,
                            None,
                            Some(1),
                            None,
                        );
                    } else {
                        perf_diagnostics::record_enrichment(
                            PerfComponent::EnrichmentPersistenceScheduling,
                            &chat_id,
                            PerfOutcome::Success,
                            0,
                            None,
                            Some(1),
                            None,
                        );
                    }
                    if !insertion_stale {
                        info!(
                            "Saved auto knowledge enrichment context_file to session at index {}",
                            session_last_user_idx.unwrap_or_default()
                        );
                    }
                }
            }
        }
    }
}

pub fn save_rag_results_to_session(session: &mut ChatSession, rag_results: &[serde_json::Value]) {
    let last_user_idx = session
        .messages
        .iter()
        .rposition(|m| is_prompt_turn_role(&m.role));
    if let Some(insert_idx) = last_user_idx {
        let existing_content: std::collections::HashSet<String> = session
            .messages
            .iter()
            .filter(|m| m.role == "context_file" || m.role == "plain_text")
            .map(|m| m.content.content_text_only())
            .collect();
        let mut offset = 0;
        for rag_msg_json in rag_results {
            if let Ok(msg) = serde_json::from_value::<ChatMessage>(rag_msg_json.clone()) {
                if (msg.role == "context_file" || msg.role == "plain_text")
                    && !existing_content.contains(&msg.content.content_text_only())
                {
                    session.insert_message(insert_idx + offset, msg);
                    offset += 1;
                }
            }
        }
    }
}

pub(in crate::chat::generation) fn is_prompt_turn_role(role: &str) -> bool {
    role == "user"
        || role == crate::chat::internal_roles::EVENT_ROLE
        || role == crate::chat::internal_roles::PLAN_ROLE
}

pub(in crate::chat::generation) fn tail_needs_assistant(messages: &[ChatMessage]) -> bool {
    let mut saw_toolish = false;

    for m in messages.iter().rev() {
        match m.role.as_str() {
            "assistant" => {
                if !saw_toolish {
                    return false;
                }
                let Some(tcs) = m.tool_calls.as_ref() else {
                    return false;
                };
                if tcs.is_empty() {
                    return false;
                }
                return tcs.iter().any(|tc| !tc.id.starts_with("srvtoolu_"));
            }
            role if is_prompt_turn_role(role) => return true,
            "tool" | "context_file" => saw_toolish = true,
            _ => {}
        }
    }

    false
}

pub(in crate::chat::generation) fn latest_assistant_tool_call_window_closed(
    messages: &[ChatMessage],
) -> bool {
    let Some((assistant_index, assistant)) = messages
        .iter()
        .enumerate()
        .rev()
        .find(|(_, message)| message.role == "assistant")
    else {
        return true;
    };

    let Some(tool_calls) = assistant.tool_calls.as_ref() else {
        return true;
    };
    if tool_calls.is_empty() {
        return true;
    }

    let mut answered_ids = std::collections::HashSet::new();
    for message in messages.iter().skip(assistant_index + 1) {
        match message.role.as_str() {
            "tool" | "diff" if !message.tool_call_id.is_empty() => {
                answered_ids.insert(message.tool_call_id.as_str());
            }
            role if is_prompt_turn_role(role) || role == "assistant" => {
                break;
            }
            _ => {}
        }
    }

    tool_calls
        .iter()
        .all(|tc| answered_ids.contains(tc.id.as_str()))
}

pub(in crate::chat::generation) async fn inject_queued_user_messages_before_llm_if_safe(
    app: AppState,
    session_arc: Arc<AMutex<ChatSession>>,
) -> bool {
    let can_inject = {
        let session = session_arc.lock().await;
        latest_assistant_tool_call_window_closed(&session.messages)
    };

    can_inject && crate::chat::queue::inject_queued_user_messages_if_any(app, session_arc).await
}
