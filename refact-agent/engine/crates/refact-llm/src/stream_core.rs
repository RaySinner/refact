use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use serde_json::{json, Value};

use refact_chat_api::DeltaOp;
use refact_chat_history::openai_merge::ToolCallAccumulator;
use refact_chat_history::retry_policy::{
    classify_llm_error_for_retry, should_retry_llm_error, RetryDecision,
};
use refact_core::chat_types::ChatUsage;
use refact_core::llm_types::BaseModelRecord;

use crate::adapter::{StreamParseError, WireFormat};
use crate::canonical::{LlmRequest, LlmStreamDelta};

pub fn merge_usage(existing: Option<ChatUsage>, incoming: ChatUsage) -> ChatUsage {
    match existing {
        None => incoming,
        Some(prev) => {
            let prev_cache_read = prev.cache_read_tokens.unwrap_or(0);
            let incoming_cache_read = incoming.cache_read_tokens.unwrap_or(0);
            let cache_read_increased = incoming_cache_read > prev_cache_read;

            let merged_cache_creation =
                match (prev.cache_creation_tokens, incoming.cache_creation_tokens) {
                    (Some(a), Some(b)) => Some(std::cmp::max(a, b)),
                    (Some(a), None) => Some(a),
                    (None, Some(b)) => Some(b),
                    (None, None) => None,
                };
            let merged_cache_read = match (prev.cache_read_tokens, incoming.cache_read_tokens) {
                (Some(a), Some(b)) => Some(std::cmp::max(a, b)),
                (Some(a), None) => Some(a),
                (None, Some(b)) => Some(b),
                (None, None) => None,
            };

            let merged_prompt_tokens = if cache_read_increased {
                incoming.prompt_tokens
            } else if prev.prompt_tokens == 0 && incoming.prompt_tokens > 0 {
                incoming.prompt_tokens
            } else {
                std::cmp::max(prev.prompt_tokens, incoming.prompt_tokens)
            };

            let merged_completion =
                std::cmp::max(prev.completion_tokens, incoming.completion_tokens);

            let merged_metering = match (prev.metering_usd, incoming.metering_usd) {
                (_, Some(b)) => Some(b),
                (Some(a), None) => Some(a),
                (None, None) => None,
            };

            let merged_total = merged_prompt_tokens
                + merged_completion
                + merged_cache_creation.unwrap_or(0)
                + merged_cache_read.unwrap_or(0);

            ChatUsage {
                prompt_tokens: merged_prompt_tokens,
                completion_tokens: merged_completion,
                total_tokens: merged_total,
                cache_creation_tokens: merged_cache_creation,
                cache_read_tokens: merged_cache_read,
                metering_usd: merged_metering,
            }
        }
    }
}

pub struct StreamRunParams {
    pub llm_request: LlmRequest,
    pub model_rec: BaseModelRecord,
    pub chat_id: Option<String>,
    pub allow_websocket: bool,
    pub abort_flag: Option<Arc<AtomicBool>>,
    pub abort_notify: Option<Arc<tokio::sync::Notify>>,
    pub supports_tools: bool,
    pub supports_reasoning: bool,
    pub reasoning_type: Option<String>,
    pub supports_temperature: bool,
}

#[derive(Clone, Debug)]
pub struct LlmStreamError {
    pub message: String,
    pub partial_output_emitted: bool,
}

impl LlmStreamError {
    pub fn new(message: impl Into<String>, partial_output_emitted: bool) -> Self {
        Self {
            message: message.into(),
            partial_output_emitted,
        }
    }

    pub fn retry_decision(&self) -> RetryDecision {
        classify_llm_error_for_retry(&self.message)
    }

    pub fn should_retry(&self, attempt: usize, abort: &AtomicBool) -> bool {
        if self.retry_decision().is_user_cancelled() {
            return false;
        }
        should_retry_llm_error(&self.message, attempt, abort)
    }
}

impl std::fmt::Display for LlmStreamError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.message.fmt(f)
    }
}

impl std::error::Error for LlmStreamError {}

impl std::ops::Deref for LlmStreamError {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        &self.message
    }
}

impl From<LlmStreamError> for String {
    fn from(error: LlmStreamError) -> Self {
        error.message
    }
}

impl From<String> for LlmStreamError {
    fn from(message: String) -> Self {
        Self::new(message, false)
    }
}
#[derive(Default, Clone)]
pub struct ChoiceFinal {
    pub content: String,
    pub raw_content: String,
    pub reasoning: String,
    pub thinking_blocks: Vec<serde_json::Value>,
    pub tool_calls_raw: Vec<serde_json::Value>,
    pub citations: Vec<serde_json::Value>,
    pub server_content_blocks: Vec<serde_json::Value>,
    pub extra: serde_json::Map<String, serde_json::Value>,
    pub finish_reason: Option<String>,
    pub usage: Option<ChatUsage>,
}

pub trait StreamCollector: Send {
    fn on_delta_ops(&mut self, choice_idx: usize, ops: Vec<DeltaOp>);
    fn on_usage(&mut self, usage: &ChatUsage);
    fn on_finish(&mut self, choice_idx: usize, finish_reason: Option<String>);
}

pub enum CollectorReplayEvent {
    DeltaOps(usize, Vec<DeltaOp>),
    Usage(ChatUsage),
    Finish(usize, Option<String>),
}

#[derive(Default)]
pub struct ReplayCollector {
    pub events: Vec<CollectorReplayEvent>,
}

impl ReplayCollector {
    pub fn replay<C: StreamCollector>(self, collector: &mut C) {
        for event in self.events {
            match event {
                CollectorReplayEvent::DeltaOps(choice_idx, ops) => {
                    collector.on_delta_ops(choice_idx, ops);
                }
                CollectorReplayEvent::Usage(usage) => {
                    collector.on_usage(&usage);
                }
                CollectorReplayEvent::Finish(choice_idx, finish_reason) => {
                    collector.on_finish(choice_idx, finish_reason);
                }
            }
        }
    }
}

impl StreamCollector for ReplayCollector {
    fn on_delta_ops(&mut self, choice_idx: usize, ops: Vec<DeltaOp>) {
        self.events
            .push(CollectorReplayEvent::DeltaOps(choice_idx, ops));
    }

    fn on_usage(&mut self, usage: &ChatUsage) {
        self.events.push(CollectorReplayEvent::Usage(usage.clone()));
    }

    fn on_finish(&mut self, choice_idx: usize, finish_reason: Option<String>) {
        self.events
            .push(CollectorReplayEvent::Finish(choice_idx, finish_reason));
    }
}

pub struct PartialOutputCollector<'a, C: StreamCollector> {
    pub collector: &'a mut C,
    pub partial_output_emitted: &'a mut bool,
    pub progress_events: Arc<AtomicU64>,
}

impl<C: StreamCollector> StreamCollector for PartialOutputCollector<'_, C> {
    fn on_delta_ops(&mut self, choice_idx: usize, ops: Vec<DeltaOp>) {
        if !ops.is_empty() {
            *self.partial_output_emitted = true;
            self.progress_events.fetch_add(1, Ordering::SeqCst);
        }
        self.collector.on_delta_ops(choice_idx, ops);
    }

    fn on_usage(&mut self, usage: &ChatUsage) {
        self.collector.on_usage(usage);
    }

    fn on_finish(&mut self, choice_idx: usize, finish_reason: Option<String>) {
        self.collector.on_finish(choice_idx, finish_reason);
    }
}
pub const FUNCTION_CALLS_OPEN_TAG: &str = "<function_calls>";
pub const FUNCTION_CALLS_CLOSE_TAG: &str = "</function_calls>";

pub fn find_function_calls_close_tag(text: &str) -> Option<usize> {
    text.find(FUNCTION_CALLS_CLOSE_TAG)
}

pub fn json_value_size_estimate(value: &serde_json::Value) -> usize {
    match value {
        serde_json::Value::Null => 4,
        serde_json::Value::Bool(_) => 5,
        serde_json::Value::Number(_) => 8,
        serde_json::Value::String(text) => text.len().saturating_add(2),
        serde_json::Value::Array(items) => items
            .iter()
            .map(json_value_size_estimate)
            .fold(2usize, |acc, size| {
                acc.saturating_add(size).saturating_add(1)
            }),
        serde_json::Value::Object(map) => map.iter().fold(2usize, |acc, (key, value)| {
            acc.saturating_add(key.len())
                .saturating_add(4)
                .saturating_add(json_value_size_estimate(value))
        }),
    }
}

pub fn split_with_partial_literal_suffix<'a>(text: &'a str, tag: &str) -> (&'a str, &'a str) {
    let max_len = tag.len().saturating_sub(1).min(text.len());
    for len in (1..=max_len).rev() {
        let suffix_start = text.len() - len;
        if !text.is_char_boundary(suffix_start) {
            continue;
        }
        let suffix = &text[suffix_start..];
        if tag.starts_with(suffix) {
            return (&text[..suffix_start], suffix);
        }
    }
    (text, "")
}

pub fn route_append_content_without_function_calls(
    acc: &mut ChoiceAccumulator,
    ops: &mut Vec<DeltaOp>,
    incoming_text: String,
    block_index: Option<u64>,
) {
    if !acc.inside_function_calls_tag
        && acc.pending_function_calls_parse.is_empty()
        && !incoming_text.contains('<')
    {
        handle_append_content_delta(acc, ops, incoming_text, block_index);
        return;
    }

    acc.pending_function_calls_parse.push_str(&incoming_text);

    loop {
        if acc.inside_function_calls_tag {
            if let Some(close_idx) =
                find_function_calls_close_tag(&acc.pending_function_calls_parse)
            {
                let drain_until = close_idx + FUNCTION_CALLS_CLOSE_TAG.len();
                acc.function_calls_xml_raw
                    .push_str(&acc.pending_function_calls_parse[..drain_until]);
                acc.pending_function_calls_parse.drain(..drain_until);
                acc.inside_function_calls_tag = false;
                continue;
            }
            let (_, keep) = split_with_partial_literal_suffix(
                &acc.pending_function_calls_parse,
                FUNCTION_CALLS_CLOSE_TAG,
            );
            let consumed_len = acc.pending_function_calls_parse.len() - keep.len();
            acc.function_calls_xml_raw
                .push_str(&acc.pending_function_calls_parse[..consumed_len]);
            acc.pending_function_calls_parse = keep.to_string();
            break;
        }

        if let Some(open_idx) = acc
            .pending_function_calls_parse
            .find(FUNCTION_CALLS_OPEN_TAG)
        {
            let content_text = acc.pending_function_calls_parse[..open_idx].to_string();
            handle_append_content_delta(acc, ops, content_text, block_index);
            let drain_until = open_idx + FUNCTION_CALLS_OPEN_TAG.len();
            acc.function_calls_xml_raw
                .push_str(&acc.pending_function_calls_parse[open_idx..drain_until]);
            acc.pending_function_calls_parse.drain(..drain_until);
            acc.inside_function_calls_tag = true;
            continue;
        }

        let (emit, keep) = split_with_partial_literal_suffix(
            &acc.pending_function_calls_parse,
            FUNCTION_CALLS_OPEN_TAG,
        );
        let content_text = emit.to_string();
        acc.pending_function_calls_parse = keep.to_string();
        handle_append_content_delta(acc, ops, content_text, block_index);
        break;
    }
}

pub fn flush_pending_function_calls_parse(acc: &mut ChoiceAccumulator, ops: &mut Vec<DeltaOp>) {
    if acc.pending_function_calls_parse.is_empty() {
        return;
    }

    let pending = std::mem::take(&mut acc.pending_function_calls_parse);
    if acc.inside_function_calls_tag {
        acc.function_calls_xml_raw.push_str(&pending);
        return;
    }
    handle_append_content_delta(acc, ops, pending, None);
}

pub const THINK_OPEN_TAG: &str = "<think>";
pub const THINK_CLOSE_TAG: &str = "</think>";

pub fn find_ascii_case_insensitive(haystack: &str, needle: &str) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    for (idx, _) in haystack.match_indices('<') {
        if idx + needle.len() > haystack.len() {
            continue;
        }
        if let Some(candidate) = haystack.get(idx..idx + needle.len()) {
            if candidate.eq_ignore_ascii_case(needle) {
                return Some(idx);
            }
        }
    }
    None
}

pub fn split_with_partial_tag_suffix<'a>(text: &'a str, tag: &str) -> (&'a str, &'a str) {
    if let Some(last_lt) = text.rfind('<') {
        let suffix = &text[last_lt..];
        if suffix.len() < tag.len() {
            if let Some(tag_prefix) = tag.get(..suffix.len()) {
                if suffix.eq_ignore_ascii_case(tag_prefix) {
                    return (&text[..last_lt], suffix);
                }
            }
        }
    }
    (text, "")
}

pub fn push_content_delta(
    acc: &mut ChoiceAccumulator,
    ops: &mut Vec<DeltaOp>,
    text: String,
    block_index: Option<u64>,
) {
    if text.is_empty() {
        return;
    }
    acc.content.push_str(&text);
    if let Some(idx) = block_index {
        acc.content_per_block
            .entry(idx)
            .or_default()
            .push_str(&text);
    }
    ops.push(DeltaOp::AppendContent { text });
}

pub fn push_reasoning_delta(
    acc: &mut ChoiceAccumulator,
    ops: &mut Vec<DeltaOp>,
    text: String,
    block_index: Option<u64>,
) {
    if text.is_empty() {
        return;
    }
    acc.reasoning.push_str(&text);
    if let Some(idx) = block_index {
        acc.reasoning_per_block
            .entry(idx)
            .or_default()
            .push_str(&text);
    }
    ops.push(DeltaOp::AppendReasoning { text });
}

pub fn joined_keyed_reasoning(acc: &ChoiceAccumulator) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for key in &acc.reasoning_key_order {
        if let Some(text) = acc.reasoning_per_key.get(key) {
            if text.is_empty() {
                continue;
            }

            let mut insert_at = parts.len();
            let mut part_index = 0;
            let mut covered = false;
            while part_index < parts.len() {
                let existing = parts[part_index];
                if existing == text || existing.starts_with(text.as_str()) {
                    covered = true;
                    break;
                }
                if text.starts_with(existing) {
                    insert_at = insert_at.min(part_index);
                    parts.remove(part_index);
                    continue;
                }
                part_index += 1;
            }
            if covered {
                continue;
            }
            parts.insert(insert_at.min(parts.len()), text.as_str());
        }
    }
    parts.join("\n\n")
}

pub fn push_keyed_reasoning_delta(
    acc: &mut ChoiceAccumulator,
    ops: &mut Vec<DeltaOp>,
    text: String,
    reasoning_key: String,
) {
    if text.is_empty() {
        return;
    }
    if acc.reasoning_key_finalized.get(&reasoning_key).copied() == Some(true) {
        return;
    }
    if !acc.reasoning_per_key.contains_key(&reasoning_key) {
        acc.reasoning_key_order.push(reasoning_key.clone());
    }
    acc.reasoning_per_key
        .entry(reasoning_key)
        .or_default()
        .push_str(&text);
    let joined = joined_keyed_reasoning(acc);
    acc.reasoning = joined.clone();
    ops.push(DeltaOp::SetReasoning { text: joined });
}

pub fn finalize_keyed_reasoning(
    acc: &mut ChoiceAccumulator,
    ops: &mut Vec<DeltaOp>,
    text: String,
    reasoning_key: String,
) {
    if text.is_empty() {
        return;
    }
    if acc.reasoning_key_finalized.get(&reasoning_key).copied() == Some(true)
        && acc
            .reasoning_per_key
            .get(&reasoning_key)
            .map(|s| s.as_str())
            == Some(text.as_str())
    {
        return;
    }
    if !acc.reasoning_per_key.contains_key(&reasoning_key) {
        acc.reasoning_key_order.push(reasoning_key.clone());
    }
    acc.reasoning_per_key.insert(reasoning_key.clone(), text);
    acc.reasoning_key_finalized.insert(reasoning_key, true);
    let joined = joined_keyed_reasoning(acc);
    acc.reasoning = joined.clone();
    ops.push(DeltaOp::SetReasoning { text: joined });
}

pub fn route_append_content_with_think_tags(
    acc: &mut ChoiceAccumulator,
    ops: &mut Vec<DeltaOp>,
    incoming_text: String,
    block_index: Option<u64>,
) {
    if !acc.inside_think_tag && acc.pending_think_parse.is_empty() && !incoming_text.contains('<') {
        push_content_delta(acc, ops, incoming_text, block_index);
        return;
    }

    acc.pending_think_parse.push_str(&incoming_text);

    loop {
        if acc.inside_think_tag {
            if let Some(close_idx) =
                find_ascii_case_insensitive(&acc.pending_think_parse, THINK_CLOSE_TAG)
            {
                let reasoning_text = acc.pending_think_parse[..close_idx].to_string();
                push_reasoning_delta(acc, ops, reasoning_text, block_index);
                let drain_until = close_idx + THINK_CLOSE_TAG.len();
                acc.pending_think_parse.drain(..drain_until);
                acc.inside_think_tag = false;
                continue;
            }

            let (emit, keep) =
                split_with_partial_tag_suffix(&acc.pending_think_parse, THINK_CLOSE_TAG);
            let reasoning_text = emit.to_string();
            let keep_text = keep.to_string();
            push_reasoning_delta(acc, ops, reasoning_text, block_index);
            acc.pending_think_parse = keep_text;
            break;
        }

        if let Some(open_idx) =
            find_ascii_case_insensitive(&acc.pending_think_parse, THINK_OPEN_TAG)
        {
            let content_text = acc.pending_think_parse[..open_idx].to_string();
            push_content_delta(acc, ops, content_text, block_index);
            let drain_until = open_idx + THINK_OPEN_TAG.len();
            acc.pending_think_parse.drain(..drain_until);
            acc.inside_think_tag = true;
            continue;
        }

        let (emit, keep) = split_with_partial_tag_suffix(&acc.pending_think_parse, THINK_OPEN_TAG);
        let content_text = emit.to_string();
        let keep_text = keep.to_string();
        push_content_delta(acc, ops, content_text, block_index);
        acc.pending_think_parse = keep_text;
        break;
    }
}

pub fn flush_pending_think_parse(acc: &mut ChoiceAccumulator, ops: &mut Vec<DeltaOp>) {
    if acc.pending_think_parse.is_empty() {
        return;
    }

    let pending = std::mem::take(&mut acc.pending_think_parse);
    if acc.inside_think_tag {
        push_reasoning_delta(acc, ops, pending, None);
    } else {
        push_content_delta(acc, ops, pending, None);
    }
}

pub fn handle_append_content_delta(
    acc: &mut ChoiceAccumulator,
    ops: &mut Vec<DeltaOp>,
    text: String,
    block_index: Option<u64>,
) {
    if block_index.is_some() {
        flush_pending_think_parse(acc, ops);
        push_content_delta(acc, ops, text, block_index);
    } else {
        route_append_content_with_think_tags(acc, ops, text, block_index);
    }
}
pub fn process_stream_event_data<C: StreamCollector>(
    adapter: &dyn crate::adapter::LlmWireAdapter,
    auth_token: &str,
    data: &str,
    accumulators: &mut [ChoiceAccumulator],
    collector: &mut C,
    malformed_is_fatal: bool,
) -> Result<bool, String> {
    let deltas = match adapter.parse_stream_chunk(data) {
        Ok(d) => d,
        Err(StreamParseError::Skip) => return Ok(false),
        Err(StreamParseError::MalformedChunk(e)) if malformed_is_fatal => {
            return Err(format!("Malformed stream chunk: {}", e));
        }
        Err(StreamParseError::MalformedChunk(e)) => {
            tracing::warn!("Malformed stream chunk: {}", e);
            return Ok(false);
        }
        Err(StreamParseError::FatalError(e)) => {
            return Err(format!("LLM error: {}", e));
        }
    };

    let acc = &mut accumulators[0];
    let mut ops = Vec::new();
    let mut stream_done = false;

    for delta in deltas {
        match delta {
            LlmStreamDelta::AppendContent { text, block_index } => {
                route_append_content_without_function_calls(acc, &mut ops, text, block_index);
            }
            LlmStreamDelta::AppendReasoning { text, block_index } => {
                flush_pending_think_parse(acc, &mut ops);
                push_reasoning_delta(acc, &mut ops, text, block_index);
            }
            LlmStreamDelta::FinalizeReasoning { text, block_index } => {
                if let Some(block_index) = block_index {
                    acc.finalized_reasoning_per_block
                        .insert(block_index, text.clone());
                } else {
                    acc.finalized_reasoning = Some(text.clone());
                }
                let replacement = if !acc.finalized_reasoning_per_block.is_empty() {
                    let mut blocks: Vec<_> = acc.finalized_reasoning_per_block.iter().collect();
                    blocks.sort_by_key(|(block_index, _)| **block_index);
                    blocks
                        .into_iter()
                        .map(|(_, text)| text.as_str())
                        .collect::<Vec<_>>()
                        .join("\n\n")
                } else {
                    text
                };
                ops.push(DeltaOp::SetReasoning { text: replacement });
            }
            LlmStreamDelta::AppendKeyedReasoning {
                text,
                reasoning_key,
            } => {
                flush_pending_think_parse(acc, &mut ops);
                push_keyed_reasoning_delta(acc, &mut ops, text, reasoning_key);
            }
            LlmStreamDelta::FinalizeKeyedReasoning {
                text,
                reasoning_key,
            } => {
                flush_pending_think_parse(acc, &mut ops);
                finalize_keyed_reasoning(acc, &mut ops, text, reasoning_key);
            }
            LlmStreamDelta::SetToolCalls { tool_calls } => {
                let tool_calls = if !auth_token.is_empty() {
                    tool_calls
                        .into_iter()
                        .map(|mut tc| {
                            strip_mcp_prefix_from_tool_call(&mut tc);
                            tc
                        })
                        .collect()
                } else {
                    tool_calls
                };
                for tc in &tool_calls {
                    acc.tool_calls.merge(tc);
                }
                ops.push(DeltaOp::SetToolCalls {
                    tool_calls: acc.tool_calls.finalize(),
                });
            }
            LlmStreamDelta::FinalizeToolCalls { tool_calls } => {
                let tool_calls = if !auth_token.is_empty() {
                    tool_calls
                        .into_iter()
                        .map(|mut tc| {
                            strip_mcp_prefix_from_tool_call(&mut tc);
                            tc
                        })
                        .collect()
                } else {
                    tool_calls
                };
                for tc in &tool_calls {
                    acc.tool_calls.set_final(tc);
                }
                ops.push(DeltaOp::SetToolCalls {
                    tool_calls: acc.tool_calls.finalize(),
                });
            }
            LlmStreamDelta::SetThinkingBlocks { blocks } => {
                merge_thinking_blocks(&mut acc.thinking_blocks, blocks);
                ops.push(DeltaOp::SetThinkingBlocks {
                    blocks: acc.thinking_blocks.clone(),
                });
            }
            LlmStreamDelta::AddCitation { citation } => {
                let dedupe =
                    citation.get("provider").and_then(Value::as_str) == Some("google_cloud_code");
                if !dedupe || !acc.citations.contains(&citation) {
                    acc.citations.push(citation.clone());
                    ops.push(DeltaOp::AddCitation { citation });
                }
            }
            LlmStreamDelta::AddServerContentBlock { block } => {
                let dedupe = block.get("type").and_then(Value::as_str)
                    == Some("google_cloud_code_inline_data");
                if !dedupe || !acc.server_content_blocks.contains(&block) {
                    acc.server_content_blocks.push(block.clone());
                    ops.push(DeltaOp::AddServerContentBlock { block });
                }
            }
            LlmStreamDelta::SetUsage { usage } => {
                acc.usage = Some(merge_usage(acc.usage.take(), usage.clone()));
                if let Some(ref merged) = acc.usage {
                    collector.on_usage(merged);
                    ops.push(DeltaOp::SetUsage {
                        usage: json!(merged),
                    });
                }
            }
            LlmStreamDelta::SetFinishReason { reason } => {
                acc.finish_reason = Some(reason);
            }
            LlmStreamDelta::MergeExtra { extra } => {
                for (k, v) in &extra {
                    match (acc.extra.get_mut(k), v) {
                        (Some(Value::Array(existing)), Value::Array(incoming)) => {
                            existing.extend(incoming.clone());
                        }
                        (Some(Value::Object(existing)), Value::Object(incoming)) => {
                            for (ik, iv) in incoming {
                                existing.insert(ik.clone(), iv.clone());
                            }
                        }
                        _ => {
                            acc.extra.insert(k.clone(), v.clone());
                        }
                    }
                }
                ops.push(DeltaOp::MergeExtra { extra });
            }
            LlmStreamDelta::Done => {
                stream_done = true;
                break;
            }
        }
    }

    if stream_done {
        flush_pending_function_calls_parse(acc, &mut ops);
    }

    if !ops.is_empty() {
        collector.on_delta_ops(0, ops);
    }

    Ok(stream_done)
}
pub const MAX_NDJSON_LINE_BYTES: usize = 8 * 1024 * 1024;

pub fn ndjson_line_size_error(size: usize) -> String {
    format!("Ollama NDJSON line exceeds {MAX_NDJSON_LINE_BYTES} bytes ({size} bytes)")
}

pub fn process_ndjson_bytes<C: StreamCollector>(
    adapter: &dyn crate::adapter::LlmWireAdapter,
    auth_token: &str,
    pending: &mut Vec<u8>,
    bytes: &[u8],
    accumulators: &mut [ChoiceAccumulator],
    collector: &mut C,
) -> Result<bool, String> {
    for segment in bytes.split_inclusive(|b| *b == b'\n') {
        let segment_line_len = segment
            .iter()
            .position(|b| *b == b'\n')
            .unwrap_or(segment.len());
        let next_line_len = pending.len().saturating_add(segment_line_len);
        if next_line_len > MAX_NDJSON_LINE_BYTES {
            return Err(ndjson_line_size_error(next_line_len));
        }

        pending.extend_from_slice(segment);
        if process_complete_ndjson_lines(adapter, auth_token, pending, accumulators, collector)? {
            return Ok(true);
        }
        if pending.len() > MAX_NDJSON_LINE_BYTES {
            return Err(ndjson_line_size_error(pending.len()));
        }
    }
    Ok(false)
}

pub fn process_complete_ndjson_lines<C: StreamCollector>(
    adapter: &dyn crate::adapter::LlmWireAdapter,
    auth_token: &str,
    pending: &mut Vec<u8>,
    accumulators: &mut [ChoiceAccumulator],
    collector: &mut C,
) -> Result<bool, String> {
    loop {
        let Some(pos) = pending.iter().position(|b| *b == b'\n') else {
            if pending.len() > MAX_NDJSON_LINE_BYTES {
                return Err(ndjson_line_size_error(pending.len()));
            }
            return Ok(false);
        };
        let mut line: Vec<u8> = pending.drain(..=pos).collect();
        if line.last() == Some(&b'\n') {
            line.pop();
        }
        if line.last() == Some(&b'\r') {
            line.pop();
        }
        if line.len() > MAX_NDJSON_LINE_BYTES {
            return Err(ndjson_line_size_error(line.len()));
        }
        if line.iter().all(|b| b.is_ascii_whitespace()) {
            continue;
        }
        let data = std::str::from_utf8(&line)
            .map_err(|e| format!("Malformed stream chunk: utf8: {}", e))?;
        if process_stream_event_data(adapter, auth_token, data, accumulators, collector, true)? {
            return Ok(true);
        }
    }
}

pub fn process_ndjson_eof<C: StreamCollector>(
    adapter: &dyn crate::adapter::LlmWireAdapter,
    auth_token: &str,
    pending: &mut Vec<u8>,
    accumulators: &mut [ChoiceAccumulator],
    collector: &mut C,
) -> Result<bool, String> {
    if pending.len() > MAX_NDJSON_LINE_BYTES {
        return Err(ndjson_line_size_error(pending.len()));
    }
    if pending.iter().all(|b| b.is_ascii_whitespace()) {
        pending.clear();
        return Ok(false);
    }
    let mut line = std::mem::take(pending);
    while line.last().is_some_and(|b| b.is_ascii_whitespace()) {
        line.pop();
    }
    if line.len() > MAX_NDJSON_LINE_BYTES {
        return Err(ndjson_line_size_error(line.len()));
    }
    let data =
        std::str::from_utf8(&line).map_err(|e| format!("Malformed stream chunk: utf8: {}", e))?;
    process_stream_event_data(adapter, auth_token, data, accumulators, collector, true)
}
pub fn finalize_accumulators<C: StreamCollector>(
    mut accumulators: Vec<ChoiceAccumulator>,
    collector: &mut C,
) -> Vec<ChoiceFinal> {
    for (idx, acc) in accumulators.iter_mut().enumerate() {
        let mut tail_ops = Vec::new();
        flush_pending_function_calls_parse(acc, &mut tail_ops);
        flush_pending_think_parse(acc, &mut tail_ops);
        if !tail_ops.is_empty() {
            collector.on_delta_ops(idx, tail_ops);
        }
    }

    accumulators
        .into_iter()
        .enumerate()
        .map(|(idx, acc)| {
            collector.on_finish(idx, acc.finish_reason.clone());
            let keyed_reasoning = joined_keyed_reasoning(&acc);
            let reasoning = if !acc.reasoning_per_key.is_empty() {
                keyed_reasoning
            } else if !acc.finalized_reasoning_per_block.is_empty() {
                let mut blocks: Vec<_> = acc.finalized_reasoning_per_block.into_iter().collect();
                blocks.sort_by_key(|(block_index, _)| *block_index);
                blocks
                    .into_iter()
                    .map(|(_, text)| text)
                    .collect::<Vec<_>>()
                    .join("\n\n")
            } else {
                acc.finalized_reasoning.unwrap_or(acc.reasoning)
            };
            let thinking_blocks = if !acc.thinking_blocks.is_empty() && !reasoning.is_empty() {
                acc.thinking_blocks
                    .into_iter()
                    .map(|mut block| {
                        if let Some(obj) = block.as_object_mut() {
                            let is_anthropic_thinking =
                                obj.get("type").and_then(|t| t.as_str()) == Some("thinking");
                            let thinking_is_empty = obj
                                .get("thinking")
                                .and_then(|v| v.as_str())
                                .map_or(true, |s| s.trim().is_empty());
                            if is_anthropic_thinking && thinking_is_empty {
                                let block_idx = obj.get("index").and_then(|v| v.as_u64());
                                let reasoning_text = block_idx
                                    .and_then(|idx| acc.reasoning_per_block.get(&idx))
                                    .unwrap_or(&reasoning);
                                if !reasoning_text.is_empty() {
                                    obj.insert(
                                        "thinking".to_string(),
                                        json!(reasoning_text.clone()),
                                    );
                                }
                            }
                        }
                        block
                    })
                    .collect()
            } else if acc.thinking_blocks.is_empty() && !reasoning.is_empty() {
                vec![json!({
                    "type": "reasoning",
                    "summary": [{"type": "summary_text", "text": reasoning.clone()}]
                })]
            } else {
                acc.thinking_blocks
            };

            let raw_content = format!(
                "{}{}{}",
                acc.content, acc.function_calls_xml_raw, acc.pending_function_calls_parse
            );

            ChoiceFinal {
                content: acc.content,
                raw_content: raw_content,
                reasoning,
                thinking_blocks,
                tool_calls_raw: acc.tool_calls.finalize(),
                citations: acc.citations,
                server_content_blocks: acc.server_content_blocks,
                extra: {
                    let mut extra = acc.extra;
                    if !acc.content_per_block.is_empty() {
                        let mut text_blocks: Vec<_> = acc.content_per_block.into_iter().collect();
                        text_blocks.sort_by_key(|(idx, _)| *idx);
                        extra.insert(
                            "_anthropic_text_blocks".to_string(),
                            json!(text_blocks
                                .into_iter()
                                .map(|(idx, text)| { json!({"index": idx, "text": text}) })
                                .collect::<Vec<_>>()),
                        );
                    }
                    extra
                },
                finish_reason: acc.finish_reason,
                usage: acc.usage,
            }
        })
        .collect()
}
/// Merges incoming thinking blocks into the accumulator, deduplicating by:
/// 1. `id` field (if present)
/// 2. `(type, index)` pair (Anthropic signature deltas)
/// 3. `(type, signature)` pair (LiteLLM blocks without index)
///
/// When a duplicate is found, reasoning and thinking blocks are enriched with
/// all non-null incoming fields. Signatures use the latest incoming value.
pub fn merge_thinking_blocks(
    dst: &mut Vec<serde_json::Value>,
    incoming: Vec<serde_json::Value>,
) {
    for block in incoming {
        let block_type = block.get("type").and_then(|v| v.as_str()).unwrap_or("");
        let block_id = block.get("id").and_then(|v| v.as_str());
        let block_index = block.get("index").and_then(|v| v.as_u64());
        let block_sig = block.get("signature").and_then(|v| v.as_str());

        let existing_idx = if let Some(id) = block_id {
            dst.iter()
                .position(|b| b.get("id").and_then(|v| v.as_str()) == Some(id))
        } else if let Some(idx) = block_index {
            dst.iter().position(|b| {
                b.get("type").and_then(|v| v.as_str()).unwrap_or("") == block_type
                    && b.get("index").and_then(|v| v.as_u64()) == Some(idx)
            })
        } else if let Some(sig) = block_sig {
            dst.iter().position(|b| {
                b.get("type").and_then(|v| v.as_str()).unwrap_or("") == block_type
                    && b.get("signature").and_then(|v| v.as_str()) == Some(sig)
            })
        } else {
            None
        };

        if let Some(pos) = existing_idx {
            if let Some(new_sig) = block.get("signature").and_then(|v| v.as_str()) {
                if let Some(obj) = dst[pos].as_object_mut() {
                    obj.insert("signature".to_string(), json!(new_sig));
                }
            }
            if block_type == "reasoning" || block_type == "thinking" {
                if let (Some(existing), Some(incoming)) =
                    (dst[pos].as_object_mut(), block.as_object())
                {
                    for (key, value) in incoming {
                        if key != "signature" && !value.is_null() {
                            existing.insert(key.clone(), value.clone());
                        }
                    }
                }
            }
        } else {
            dst.push(block);
        }
    }
}

#[derive(Default)]
pub struct ChoiceAccumulator {
    pub content: String,
    /// Per-block content text for Anthropic interleaved output.
    /// Key is the content block index from the stream.
    pub content_per_block: HashMap<u64, String>,
    pub reasoning: String,
    /// Per-block reasoning text for Anthropic interleaved thinking.
    /// Key is the content block index from the stream.
    pub reasoning_per_block: HashMap<u64, String>,
    pub finalized_reasoning: Option<String>,
    pub finalized_reasoning_per_block: HashMap<u64, String>,
    pub reasoning_per_key: HashMap<String, String>,
    pub reasoning_key_finalized: HashMap<String, bool>,
    pub reasoning_key_order: Vec<String>,
    pub thinking_blocks: Vec<serde_json::Value>,
    pub tool_calls: ToolCallAccumulator,
    pub citations: Vec<serde_json::Value>,
    pub server_content_blocks: Vec<serde_json::Value>,
    pub extra: serde_json::Map<String, serde_json::Value>,
    pub finish_reason: Option<String>,
    pub usage: Option<ChatUsage>,
    pub pending_think_parse: String,
    pub inside_think_tag: bool,
    pub pending_function_calls_parse: String,
    pub inside_function_calls_tag: bool,
    pub function_calls_xml_raw: String,
}

pub fn strip_mcp_prefix_from_tool_call(tc: &mut serde_json::Value) {
    if let Some(func) = tc.get_mut("function") {
        if let Some(name) = func
            .get("name")
            .and_then(|n| n.as_str())
            .map(|s| s.to_string())
        {
            if let Some(stripped) = name.strip_prefix("mcp_") {
                func["name"] = serde_json::json!(stripped);
            }
        }
    }
}

pub fn normalize_tool_call(tc: &serde_json::Value) -> Option<refact_core::chat_types::ChatToolCall> {
    let function = tc.get("function")?;
    let name = function
        .get("name")
        .and_then(|n| n.as_str())
        .filter(|s| !s.is_empty())?;

    let id = tc
        .get("id")
        .and_then(|i| i.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| {
            format!(
                "call_{}",
                uuid::Uuid::new_v4().to_string().replace("-", "")[..24].to_string()
            )
        });

    let arguments = match function.get("arguments") {
        Some(serde_json::Value::String(s)) if s.trim().starts_with('{') => s.clone(),
        Some(serde_json::Value::Object(_)) => {
            serde_json::to_string(&function["arguments"]).unwrap_or_else(|_| "{}".to_string())
        }
        _ => "{}".to_string(),
    };

    let tool_type = tc
        .get("type")
        .and_then(|t| t.as_str())
        .unwrap_or("function")
        .to_string();

    let index = tc.get("index").and_then(|i| i.as_u64()).map(|i| i as usize);

    let extra_content = tc.get("extra_content").filter(|v| !v.is_null()).cloned();

    Some(refact_core::chat_types::ChatToolCall {
        id,
        index,
        function: refact_core::chat_types::ChatToolFunction {
            name: name.to_string(),
            arguments,
        },
        tool_type,
        extra_content,
        started_at_ms: None,
        completed_at_ms: None,
    })
}

pub fn should_finish_on_anthropic_stop_reason_eof(
    wire_format: WireFormat,
    accumulators: &[ChoiceAccumulator],
) -> bool {
    wire_format == WireFormat::AnthropicMessages
        && accumulators.iter().any(|acc| {
            matches!(
                acc.finish_reason.as_deref(),
                Some("end_turn" | "stop_sequence")
            )
        })
}
