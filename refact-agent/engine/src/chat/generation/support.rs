#[allow(unused_imports)]
use std::sync::Arc;

use crate::scratchpad_abstract::HasTokenizerAndEot;

use crate::chat::types::*;
use crate::chat::stream_core::StreamCollector;

use super::retry::*;
use super::preamble::*;
use super::recovery::*;
use super::run_loop::*;
use super::stream::*;

pub(in crate::chat::generation) const TOKEN_BUDGET_CADENCE: usize = 6;
pub(in crate::chat::generation) const TOKEN_BUDGET_MARKER: &str = "token_budget_info";
pub(in crate::chat::generation) const MCP_LAZY_INDEX_MARKER: &str = "mcp_lazy_index";
pub(in crate::chat::generation) const LENGTH_STOP_NEAR_EMPTY_VISIBLE_CHARS: usize = 32;
pub(in crate::chat::generation) const PARTIAL_OUTPUT_STREAM_ERROR: &str =
    "Stream interrupted after partial output and all retry attempts failed.";
pub(in crate::chat::generation) const RESPONSES_INCOMPLETE_STREAM_ERROR: &str =
    "LLM stream ended unexpectedly without completion signal";
pub(in crate::chat::generation) const RESPONSES_CONTEXT_CUTOFF_ERROR: &str =
    "context_length_exceeded: Responses stream ended before a terminal event at critical context pressure";

pub(in crate::chat::generation) const DELTA_COALESCE_WINDOW: std::time::Duration =
    std::time::Duration::from_millis(10);
pub(in crate::chat::generation) const MAX_DELTA_BATCH_EVENTS: usize = 64;
pub(in crate::chat::generation) const MAX_DELTA_OPS_PER_EMIT: usize = 128;
pub(in crate::chat::generation) const MAX_DELTA_TEXT_BYTES: usize = 64 * 1024;

pub(in crate::chat::generation) fn delta_ops_contain_visible_content(ops: &[DeltaOp]) -> bool {
    ops.iter().any(|op| match op {
        DeltaOp::AppendContent { text }
        | DeltaOp::AppendReasoning { text }
        | DeltaOp::SetReasoning { text } => !text.is_empty(),
        DeltaOp::SetThinkingBlocks { blocks } => !blocks.is_empty(),
        _ => false,
    })
}

pub(in crate::chat::generation) fn delta_ops_contain_assistant_content(ops: &[DeltaOp]) -> bool {
    ops.iter()
        .any(|op| matches!(op, DeltaOp::AppendContent { text } if !text.is_empty()))
}

pub(in crate::chat::generation) fn delta_is_coalescible_text(op: &DeltaOp) -> bool {
    matches!(
        op,
        DeltaOp::AppendContent { .. } | DeltaOp::AppendReasoning { .. }
    )
}

pub(in crate::chat::generation) fn delta_text_len(op: &DeltaOp) -> usize {
    match op {
        DeltaOp::AppendContent { text } | DeltaOp::AppendReasoning { text } => text.len(),
        _ => 0,
    }
}

pub(in crate::chat::generation) fn append_coalesced_text_op(ops: &mut Vec<DeltaOp>, op: DeltaOp) {
    match op {
        DeltaOp::AppendContent { text } => {
            if let Some(DeltaOp::AppendContent { text: previous }) = ops.last_mut() {
                previous.push_str(&text);
            } else {
                ops.push(DeltaOp::AppendContent { text });
            }
        }
        DeltaOp::AppendReasoning { text } => {
            if let Some(DeltaOp::AppendReasoning { text: previous }) = ops.last_mut() {
                previous.push_str(&text);
            } else {
                ops.push(DeltaOp::AppendReasoning { text });
            }
        }
        _ => ops.push(op),
    }
}

pub(in crate::chat::generation) fn split_delta_text(text: &str) -> Vec<String> {
    if text.len() <= MAX_DELTA_TEXT_BYTES {
        return vec![text.to_string()];
    }
    let mut chunks = Vec::new();
    let mut start = 0usize;
    while start < text.len() {
        let mut end = (start + MAX_DELTA_TEXT_BYTES).min(text.len());
        while end > start && !text.is_char_boundary(end) {
            end -= 1;
        }
        if end == start {
            end = text[start..]
                .char_indices()
                .nth(1)
                .map(|(index, _)| start + index)
                .unwrap_or(text.len());
        }
        chunks.push(text[start..end].to_string());
        start = end;
    }
    chunks
}

pub(in crate::chat::generation) fn split_large_delta_text_ops(ops: Vec<DeltaOp>) -> Vec<DeltaOp> {
    let mut split = Vec::with_capacity(ops.len());
    for op in ops {
        match op {
            DeltaOp::AppendContent { text } => split.extend(
                split_delta_text(&text)
                    .into_iter()
                    .map(|text| DeltaOp::AppendContent { text }),
            ),
            DeltaOp::AppendReasoning { text } => split.extend(
                split_delta_text(&text)
                    .into_iter()
                    .map(|text| DeltaOp::AppendReasoning { text }),
            ),
            DeltaOp::SetReasoning { text } => {
                let chunks = split_delta_text(&text);
                if let Some((first, rest)) = chunks.split_first() {
                    split.push(DeltaOp::SetReasoning {
                        text: first.clone(),
                    });
                    split.extend(
                        rest.iter()
                            .cloned()
                            .map(|text| DeltaOp::AppendReasoning { text }),
                    );
                }
            }
            other => split.push(other),
        }
    }
    split
}

pub(crate) fn batch_stream_delta_ops(ops: Vec<DeltaOp>) -> Vec<Vec<DeltaOp>> {
    let mut batches = Vec::new();
    let mut text_ops = Vec::new();
    let mut text_bytes = 0usize;
    let flush_text = |batches: &mut Vec<Vec<DeltaOp>>, text_ops: &mut Vec<DeltaOp>| {
        if !text_ops.is_empty() {
            batches.push(std::mem::take(text_ops));
        }
    };
    for op in split_large_delta_text_ops(ops) {
        if !delta_is_coalescible_text(&op) {
            flush_text(&mut batches, &mut text_ops);
            text_bytes = 0;
            batches.push(vec![op]);
            continue;
        }
        let op_bytes = delta_text_len(&op);
        if text_ops.len() >= MAX_DELTA_OPS_PER_EMIT
            || text_bytes.saturating_add(op_bytes) > MAX_DELTA_TEXT_BYTES
        {
            flush_text(&mut batches, &mut text_ops);
        }
        append_coalesced_text_op(&mut text_ops, op);
        text_bytes = text_ops.iter().map(delta_text_len).sum();
    }
    flush_text(&mut batches, &mut text_ops);
    batches
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NormalizedStopReason {
    ProviderLengthStop,
    ContextLengthStop,
}

pub(crate) fn normalize_stop_reason(reason: &str) -> Option<NormalizedStopReason> {
    let lower = reason.trim().to_ascii_lowercase();
    if lower.is_empty() {
        return None;
    }
    if lower.contains("context_length_exceeded")
        || lower.contains("maximum context")
        || lower.contains("context window")
        || lower.contains("context length")
        || lower.contains("input too long")
        || lower.contains("input is too long")
        || lower.contains("model_length")
        || lower.contains("prompt is too long")
    {
        return Some(NormalizedStopReason::ContextLengthStop);
    }
    match lower.as_str() {
        "length"
        | "max_tokens"
        | "max_output_tokens"
        | "token_limit"
        | "max_tokens_stop"
        | "max_tokens_exceeded" => Some(NormalizedStopReason::ProviderLengthStop),
        _ => None,
    }
}

pub(in crate::chat::generation) fn compression_reason_for_stop_reason(
    reason: NormalizedStopReason,
) -> CompressionReason {
    match reason {
        NormalizedStopReason::ProviderLengthStop => CompressionReason::ProviderLengthStop,
        NormalizedStopReason::ContextLengthStop => CompressionReason::ContextLengthStop,
    }
}
