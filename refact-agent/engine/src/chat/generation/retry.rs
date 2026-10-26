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
use super::preamble::*;
use super::recovery::*;
use super::run_loop::*;
use super::stream::*;

pub(in crate::chat::generation) fn is_responses_incomplete_stream_error(
    error: &LlmStreamError,
) -> bool {
    error.message.contains(RESPONSES_INCOMPLETE_STREAM_ERROR)
        || error
            .message
            .contains("OpenAI Codex WebSocket ended before completion")
        || error
            .message
            .contains("OpenAI Codex WebSocket closed before completion")
}

pub(in crate::chat::generation) fn responses_incomplete_stream_at_critical_pressure(
    error: &LlmStreamError,
    model_rec: &BaseModelRecord,
    messages: &[ChatMessage],
    effective_n_ctx: usize,
    usage_stale: bool,
) -> bool {
    model_rec.wire_format == crate::llm::WireFormat::OpenaiResponses
        && effective_n_ctx > 0
        && is_responses_incomplete_stream_error(error)
        && matches!(
            crate::chat::summarization::estimated_provider_context_pressure_with_usage_for_image_mode(
                messages,
                effective_n_ctx,
                usage_stale,
                model_rec.image_token_mode,
            ),
            ContextPressure::Critical
        )
}

pub(in crate::chat::generation) fn synthesize_responses_context_cutoff_error_if_needed(
    mut error: LlmStreamError,
    model_rec: &BaseModelRecord,
    messages: &[ChatMessage],
    effective_n_ctx: usize,
    usage_stale: bool,
) -> LlmStreamError {
    if responses_incomplete_stream_at_critical_pressure(
        &error,
        model_rec,
        messages,
        effective_n_ctx,
        usage_stale,
    ) {
        error.message = format!(
            "{RESPONSES_CONTEXT_CUTOFF_ERROR}. Original error: {}",
            error.message
        );
    }
    error
}

// Kept for callers that want the redacted form for logging without the surrounding
// message; currently nothing in the binary calls it.
#[allow(dead_code)]
pub(in crate::chat::generation) fn safe_context_limit_error_for_log(error: &str) -> String {
    safe_provider_error_diagnostic(error)
}

pub(in crate::chat::generation) fn context_limit_final_error_message(error: &str) -> String {
    format!(
        "Context too large and context reconstruction failed or the rebuilt context is still too large. Explicitly rebuild the context or start a new chat. Original error: {}",
        safe_provider_error_diagnostic(error)
    )
}

pub(in crate::chat::generation) fn partial_output_stream_error_message(original: &str) -> String {
    format!(
        "{} Original error: {}",
        PARTIAL_OUTPUT_STREAM_ERROR,
        safe_provider_error_diagnostic(original),
    )
}
