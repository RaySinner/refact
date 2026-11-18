#[allow(unused_imports)]
use std::sync::Arc;

use refact_core::call_validation::ChatMessage;

use refact_runtime_api::stream_core::LlmStreamError;
use refact_chat_api::diagnostics::safe_provider_error_diagnostic;
use refact_chat_history::history_limit::ContextPressure;
use refact_core::llm_types::BaseModelRecord;

use super::support::*;

pub fn is_responses_incomplete_stream_error(
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

pub fn responses_incomplete_stream_at_critical_pressure(
    error: &LlmStreamError,
    model_rec: &BaseModelRecord,
    messages: &[ChatMessage],
    effective_n_ctx: usize,
    usage_stale: bool,
) -> bool {
    model_rec.wire_format == refact_llm::WireFormat::OpenaiResponses
        && effective_n_ctx > 0
        && is_responses_incomplete_stream_error(error)
        && matches!(
            refact_chat_history::summarization::estimated_provider_context_pressure_with_usage_for_image_mode(
                messages,
                effective_n_ctx,
                usage_stale,
                model_rec.image_token_mode,
            ),
            ContextPressure::Critical
        )
}

pub fn synthesize_responses_context_cutoff_error_if_needed(
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
pub fn safe_context_limit_error_for_log(error: &str) -> String {
    safe_provider_error_diagnostic(error)
}

pub fn context_limit_final_error_message(error: &str) -> String {
    format!(
        "Context too large and context reconstruction failed or the rebuilt context is still too large. Explicitly rebuild the context or start a new chat. Original error: {}",
        safe_provider_error_diagnostic(error)
    )
}

pub fn partial_output_stream_error_message(original: &str) -> String {
    format!(
        "{} Original error: {}",
        PARTIAL_OUTPUT_STREAM_ERROR,
        safe_provider_error_diagnostic(original),
    )
}
