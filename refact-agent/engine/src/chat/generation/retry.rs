#[allow(unused_imports)]
use std::sync::Arc;

use crate::call_validation::ChatMessage;
use crate::scratchpad_abstract::HasTokenizerAndEot;

use crate::chat::types::*;
use crate::chat::stream_core::{StreamCollector, LlmStreamError};
use crate::chat::diagnostics::safe_provider_error_diagnostic;
use crate::chat::history_limit::ContextPressure;
use refact_core::llm_types::BaseModelRecord;

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
