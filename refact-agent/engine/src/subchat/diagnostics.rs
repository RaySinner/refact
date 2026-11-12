use std::sync::{Arc, OnceLock};
use std::sync::atomic::AtomicBool;
use serde_json::json;
use tracing::warn;
use crate::call_validation::ChatMessage;
use crate::global_context::GlobalContext;
use crate::chat::diagnostics::{make_ui_only_error_message, safe_provider_error_diagnostic};
use crate::chat::retry_policy::classify_llm_error_for_retry;

use super::*;

pub(crate) fn partial_output_stream_error_message(original: &str) -> String {
    format!(
        "{} Original error: {}",
        PARTIAL_OUTPUT_STREAM_ERROR,
        safe_provider_error_diagnostic(original),
    )
}

pub(crate) fn should_compact_context_limit_error(
    error: &str,
    attempts: usize,
    abort_flag: &Option<Arc<AtomicBool>>,
) -> bool {
    classify_llm_error_for_retry(error).is_context_limit()
        && attempts < MAX_CONTEXT_LIMIT_COMPACT_ATTEMPTS
        && !is_aborted(abort_flag)
}

pub(crate) async fn emit_parent_compaction_diagnostics(
    config: &SubchatConfig,
    error: &str,
    attempt: usize,
    compacted: bool,
) {
    let (Some(parent_tx), Some(tool_call_id)) = (
        config.parent_subchat_tx.as_ref(),
        config.parent_tool_call_id.as_deref(),
    ) else {
        return;
    };
    let status = parent_compaction_diagnostic_status(error, attempt, compacted);
    let msg = json!({
        "tool_call_id": tool_call_id,
        "subchat_id": status,
        "attached_files": []
    });
    let _ = parent_tx.lock().await.send(msg);
}

pub(crate) fn parent_compaction_diagnostic_status(error: &str, attempt: usize, compacted: bool) -> String {
    let prefix = if compacted {
        format!(
            "Context limit error handled by rebuilding active context (attempt {}):\n",
            attempt,
        )
    } else {
        format!(
            "Context limit error could not rebuild active context (attempt {}):\n",
            attempt,
        )
    };
    let remaining = PARENT_COMPACTION_DIAGNOSTIC_MAX_CHARS.saturating_sub(prefix.len());
    format!(
        "{}{}",
        prefix,
        redact_and_cap_parent_diagnostic(error, remaining)
    )
}

pub(crate) fn redact_and_cap_parent_diagnostic(error: &str, max_chars: usize) -> String {
    redact_and_cap_context_limit_diagnostic(error, max_chars)
}

pub(crate) fn safe_context_limit_error_for_log(error: &str) -> String {
    redact_and_cap_context_limit_diagnostic(error, PARENT_COMPACTION_DIAGNOSTIC_MAX_CHARS)
}

pub(crate) fn redact_and_cap_context_limit_diagnostic(error: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    let (window, window_truncated) = bounded_context_limit_diagnostic_window(
        error,
        max_chars,
        PARENT_COMPACTION_DIAGNOSTIC_REDACTION_LOOKAHEAD_CHARS,
    );
    let mut redacted = redact_context_limit_diagnostic_sensitive(window)
        .trim()
        .to_string();
    if window_truncated || error.len() > max_chars {
        redacted = format!(
            "{}{}",
            redacted.trim_end(),
            PARENT_COMPACTION_DIAGNOSTIC_TRUNCATED,
        );
    }
    cap_context_limit_diagnostic(&redacted, max_chars)
}

pub(crate) fn bounded_context_limit_diagnostic_window(
    error: &str,
    max_chars: usize,
    extra_scan_chars: usize,
) -> (&str, bool) {
    let scan_cap = max_chars.saturating_add(extra_scan_chars);
    let window = crate::llm::safe_truncate(error, scan_cap);
    (window, window.len() < error.len())
}

pub(crate) fn redact_context_limit_diagnostic_sensitive(text: &str) -> String {
    static SK_PLACEHOLDER: OnceLock<regex::Regex> = OnceLock::new();
    let redacted = refact_core::string_utils::redact_sensitive(text);
    SK_PLACEHOLDER
        .get_or_init(|| regex::Regex::new(r"sk-[A-Za-z0-9_-]{8,}").unwrap())
        .replace_all(&redacted, "[REDACTED_SK_TOKEN]")
        .into_owned()
}

pub(crate) fn cap_context_limit_diagnostic(redacted: &str, max_chars: usize) -> String {
    if redacted.len() <= max_chars {
        return redacted.to_string();
    }
    if max_chars <= PARENT_COMPACTION_DIAGNOSTIC_TRUNCATED.len() {
        return crate::llm::safe_truncate(PARENT_COMPACTION_DIAGNOSTIC_TRUNCATED, max_chars)
            .to_string();
    }
    let keep = max_chars - PARENT_COMPACTION_DIAGNOSTIC_TRUNCATED.len();
    format!(
        "{}{}",
        crate::llm::safe_truncate(&redacted, keep).trim_end(),
        PARENT_COMPACTION_DIAGNOSTIC_TRUNCATED,
    )
}

pub(crate) fn append_reactive_compaction_diagnostic(
    messages: &mut Vec<ChatMessage>,
    error: &str,
    preserve_last_message: bool,
) {
    if preserve_last_message {
        if let Some(last) = messages.pop() {
            messages.push(make_ui_only_error_message(error));
            messages.push(last);
            return;
        }
    }
    messages.push(make_ui_only_error_message(error));
}

pub(crate) async fn apply_subchat_reactive_compaction(
    gcx: Arc<GlobalContext>,
    config: &SubchatConfig,
    messages: &mut Vec<ChatMessage>,
    error: &str,
    attempt: usize,
    preserve_last_message: bool,
) -> bool {
    if !subchat_retries_allowed(config) {
        return false;
    }
    let input = match refact_core::active_context::active_context(messages) {
        Ok(view) => view.messages,
        Err(_) => return false,
    };
    let owner = config.trace_parent.trace_folder_owner();
    let outcome = Box::pin(crate::agentic::mode_transition::reconstruct_context(
        gcx,
        crate::agentic::mode_transition::ReconstructionRequest {
            messages: &input,
            target_mode: &config.mode,
            target_mode_description: "Continue this subchat with rebuilt context",
            parent_chat_id: owner.as_deref(),
            model_override: Some(config.model.clone()),
            abort_flag: config.abort_flag.clone(),
            hints: None,
            target_budget_symbols: Some(config.n_ctx.saturating_mul(2)),
            preserve_goal_messages: true,
        },
    ))
    .await;
    let compacted = match outcome {
        Ok(outcome) => match refact_core::active_context::make_reconstruction_report(
            outcome.messages,
            refact_core::active_context::ReconstructionMetadata {
                model: Some(outcome.model),
                trigger: Some("subchat_context_limit".into()),
                from_mode: Some(config.mode.clone()),
                to_mode: Some(config.mode.clone()),
                ..Default::default()
            },
        ) {
            Ok(report) => {
                messages.push(report);
                true
            }
            Err(_) => false,
        },
        Err(failure) => {
            warn!(
                "Subchat context rebuild failed: {}",
                safe_provider_error_diagnostic(&failure)
            );
            false
        }
    };
    if !compacted {
        append_reactive_compaction_diagnostic(messages, error, preserve_last_message);
    }

    emit_parent_compaction_diagnostics(config, error, attempt, compacted).await;
    compacted
}
