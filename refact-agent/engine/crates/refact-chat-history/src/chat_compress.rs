//! Message-transformation core of the compress-chat tool: cutoff selection, the
//! deterministic sweep, report insertion, tool-call repair and the token
//! estimates they compare against.
//!
//! Pure over `ChatMessage`, so `refact-tools` keeps the tool shell (schema,
//! confirmation, `Tool` implementations) and re-exports this module.

use std::collections::{HashMap, HashSet};

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use refact_core::chat_types::{ChatContent, ChatMessage, ContextFile};
use refact_core::string_utils::redact_sensitive;

use crate::history_limit::compress_duplicate_context_files;
use crate::trajectory_ops::{
    build_compression_report_message_with_fingerprint, insert_compression_report_at_boundary_scoped,
    is_memory_path, should_preserve_tool,
};

pub const TOOL_OUTPUT_TRUNCATE_LIMIT: usize = 200;
/// Minimum plain-text length (chars) for a turn to be reduced by the
/// deterministic recovery sweep. Keeps tiny chats a no-op.
pub const RECOVERY_PLAIN_TEXT_FLOOR: usize = 1024;

pub fn find_preserve_cutoff(messages: &[refact_core::chat_types::ChatMessage], turns: usize) -> usize {
    if turns == 0 {
        return messages.len();
    }

    let mut turn_count = 0usize;
    let mut idx = messages.len();
    while idx > 0 {
        idx -= 1;
        if messages[idx].role == "user" {
            turn_count += 1;
            if turn_count >= turns {
                return idx;
            }
        }
    }
    0
}

fn preserve_cutoff_for(messages: &[ChatMessage], preserve_last_turns: Option<usize>) -> usize {
    preserve_last_turns
        .map(|turns| find_preserve_cutoff(messages, turns))
        .unwrap_or(messages.len())
}

pub fn compression_context_message_target_id(message: &ChatMessage) -> Option<String> {
    compression_message_target_id("context_message", message)
}

pub fn compression_tool_output_target_id(message: &ChatMessage) -> Option<String> {
    compression_message_target_id("tool_output", message)
}

fn compression_message_target_id(kind: &str, message: &ChatMessage) -> Option<String> {
    let tool_call_id = message.tool_call_id.trim();
    if !tool_call_id.is_empty() {
        return Some(format!("{kind}:tool_call:{tool_call_id}"));
    }
    let message_id = message.message_id.trim();
    (!message_id.is_empty()).then(|| format!("{kind}:message:{message_id}"))
}

pub fn compression_context_file_target_id(file: &ContextFile) -> String {
    let kind = if is_memory_path(&file.file_name) {
        "memory"
    } else {
        "context_file"
    };
    format!("{kind}:{}", file.file_name)
}

fn message_request_id<'a>(
    requested: &'a HashSet<String>,
    canonical_id: Option<String>,
    message: &ChatMessage,
) -> Option<&'a str> {
    requested
        .iter()
        .find(|requested_id| {
            canonical_id.as_deref() == Some(requested_id.as_str())
                || (!message.tool_call_id.is_empty()
                    && requested_id.as_str() == message.tool_call_id)
        })
        .map(String::as_str)
}

fn context_file_request_id<'a>(
    requested: &'a HashSet<String>,
    file: &ContextFile,
    memory_operation: bool,
) -> Option<&'a str> {
    let canonical_id = compression_context_file_target_id(file);
    let is_memory = is_memory_path(&file.file_name);
    requested
        .iter()
        .find(|requested_id| {
            requested_id.as_str() == canonical_id
                || (is_memory == memory_operation && requested_id.as_str() == file.file_name)
        })
        .map(String::as_str)
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct CompressChatTargetResult {
    pub operation: String,
    pub target_id: String,
    pub applied: bool,
    pub reason: Option<String>,
    pub tokens_saved: usize,
}

struct CompressChatTargetResults {
    results: Vec<CompressChatTargetResult>,
}

impl CompressChatTargetResults {
    fn new(request: &CompressChatApplyRequest) -> Self {
        let operations = [
            ("drop_context_files", request.drop_context_files),
            ("drop_memories", request.drop_memories),
            ("truncate_tool_outputs", request.truncate_tool_outputs),
            ("drop_tool_outputs", request.drop_tool_outputs),
            ("drop_context_messages", request.drop_context_messages),
        ];
        let mut results = Vec::new();
        for (operation, targets) in operations {
            let mut targets: Vec<&String> = targets.iter().collect();
            targets.sort();
            for target_id in targets {
                let reason = if target_id.trim().is_empty() {
                    "missing_stable_id"
                } else {
                    "missing_or_already_dropped"
                };
                results.push(CompressChatTargetResult {
                    operation: operation.to_string(),
                    target_id: target_id.clone(),
                    applied: false,
                    reason: Some(reason.to_string()),
                    tokens_saved: 0,
                });
            }
        }
        Self { results }
    }

    fn mark_applied(&mut self, operation: &str, target_id: &str, tokens_saved: usize) {
        if let Some(result) = self
            .results
            .iter_mut()
            .find(|result| result.operation == operation && result.target_id == target_id)
        {
            result.applied = true;
            result.reason = None;
            result.tokens_saved += tokens_saved;
        }
    }

    fn mark_skipped(&mut self, operation: &str, target_id: &str, reason: &str) {
        if let Some(result) = self
            .results
            .iter_mut()
            .find(|result| {
                result.operation == operation && result.target_id == target_id && !result.applied
            })
        {
            result.reason = Some(reason.to_string());
        }
    }
}

#[derive(Default)]
pub struct CompressChatApplyStats {
    pub context_files_dropped: usize,
    pub context_messages_dropped: usize,
    pub memory_dropped: usize,
    pub tool_truncated: usize,
    pub tool_dropped: usize,
    pub project_info_dropped: usize,
    pub dedup_count: usize,
    pub non_text_stripped: usize,
    pub plain_text_reduced: usize,
}

impl CompressChatApplyStats {
    pub fn context_files_removed(&self) -> usize {
        self.context_files_dropped
            + self.memory_dropped
            + self.project_info_dropped
            + self.dedup_count
    }

    pub fn has_meaningful_mutation(&self) -> bool {
        self.context_files_dropped > 0
            || self.context_messages_dropped > 0
            || self.memory_dropped > 0
            || self.tool_truncated > 0
            || self.tool_dropped > 0
            || self.project_info_dropped > 0
            || self.dedup_count > 0
            || self.non_text_stripped > 0
            || self.plain_text_reduced > 0
    }
}

pub struct CompressChatApplyRequest<'a> {
    pub drop_context_files: &'a HashSet<String>,
    pub drop_all_context_files: bool,
    pub drop_memories: &'a HashSet<String>,
    pub drop_all_memories: bool,
    pub truncate_tool_outputs: &'a HashSet<String>,
    pub truncate_all_tool_outputs: bool,
    pub drop_tool_outputs: &'a HashSet<String>,
    pub drop_context_messages: &'a HashSet<String>,
    pub dedup_context_files: bool,
    pub drop_project_information: bool,
    pub strip_images: bool,
    pub reduce_plain_text: bool,
    pub strength: &'a str,
    pub preserve_last_turns: Option<usize>,
    pub target_tokens: Option<usize>,
    pub tool_call_names: &'a HashMap<String, String>,
}

pub struct CompressChatApplyOutput {
    pub messages: Vec<ChatMessage>,
    pub stats: CompressChatApplyStats,
    pub before_tokens: usize,
    pub tokens_saved: usize,
    pub target_results: Vec<CompressChatTargetResult>,
    pub report: Option<ChatMessage>,
    pub affected_boundary: Option<usize>,
    /// Length of the mutated modifiable prefix inside `messages`; everything at
    /// or past this index (the `preserve_last_turns` tail) must stay
    /// byte-identical, including during report deduplication.
    pub modifiable_len: usize,
}

fn first_changed_boundary(before: &[ChatMessage], after: &[ChatMessage]) -> Option<usize> {
    let common_len = before.len().min(after.len());
    for idx in 0..common_len {
        if serde_json::to_value(&before[idx]).ok() != serde_json::to_value(&after[idx]).ok() {
            return Some(idx);
        }
    }
    (before.len() != after.len()).then_some(common_len)
}

pub fn insert_current_compression_report(
    messages: &mut Vec<ChatMessage>,
    report: Option<ChatMessage>,
    affected_boundary: Option<usize>,
    modifiable_end: usize,
) -> Option<Value> {
    let report = report?;
    let boundary = affected_boundary.unwrap_or(modifiable_end.min(messages.len()));
    let report_idx =
        insert_compression_report_at_boundary_scoped(messages, report, boundary, modifiable_end);
    messages[report_idx]
        .extra
        .get("compression_report")
        .cloned()
}

fn tokens_with_tail(
    modifiable_prefix: &[ChatMessage],
    preserved_tail: &[ChatMessage],
    immutable_tail: &[ChatMessage],
) -> usize {
    modifiable_prefix
        .iter()
        .chain(preserved_tail.iter())
        .chain(immutable_tail.iter())
        .map(approx_tokens_for_message)
        .sum()
}

pub fn remove_invalid_tool_calls_and_tool_calls_results_before(
    modifiable_prefix: &mut Vec<ChatMessage>,
    immutable_tail: &[ChatMessage],
) -> bool {
    let before = serde_json::to_value(&*modifiable_prefix).ok();

    struct PendingToolCall {
        assistant_idx: usize,
        result_idx: Option<usize>,
    }

    let prefix_len = modifiable_prefix.len();
    let mut keep_prefix = vec![true; prefix_len];
    let mut open_by_id: HashMap<String, usize> = HashMap::new();
    let mut tool_calls: Vec<PendingToolCall> = Vec::new();
    let mut assistant_tool_calls: HashMap<usize, Vec<usize>> = HashMap::new();
    let mut result_to_tool_call: HashMap<usize, usize> = HashMap::new();

    for (idx, message) in modifiable_prefix
        .iter()
        .chain(immutable_tail.iter())
        .enumerate()
    {
        if let Some(calls) = &message.tool_calls {
            for call in calls {
                let tool_call_idx = tool_calls.len();
                tool_calls.push(PendingToolCall {
                    assistant_idx: idx,
                    result_idx: None,
                });
                assistant_tool_calls
                    .entry(idx)
                    .or_default()
                    .push(tool_call_idx);
                open_by_id.insert(call.id.clone(), tool_call_idx);
            }
        }

        let is_tool_result = message.role == "tool" || message.role == "diff";
        if !is_tool_result || message.tool_call_id.is_empty() {
            continue;
        }

        if let Some(tool_call_idx) = open_by_id.get(&message.tool_call_id).copied() {
            if let Some(previous_result_idx) = tool_calls[tool_call_idx].result_idx.replace(idx) {
                result_to_tool_call.remove(&previous_result_idx);
                if previous_result_idx < prefix_len {
                    keep_prefix[previous_result_idx] = false;
                }
            }
            result_to_tool_call.insert(idx, tool_call_idx);
        } else if idx < prefix_len {
            keep_prefix[idx] = false;
        }
    }

    let invalid_assistants: HashSet<usize> = assistant_tool_calls
        .iter()
        .filter_map(|(assistant_idx, call_indices)| {
            call_indices
                .iter()
                .any(|idx| tool_calls[*idx].result_idx.is_none())
                .then_some(*assistant_idx)
        })
        .collect();

    for (idx, message) in modifiable_prefix.iter().enumerate() {
        if invalid_assistants.contains(&idx) {
            keep_prefix[idx] = false;
        }

        let is_tool_result = message.role == "tool" || message.role == "diff";
        if !is_tool_result || message.tool_call_id.is_empty() {
            continue;
        }

        match result_to_tool_call.get(&idx) {
            Some(tool_call_idx)
                if !invalid_assistants.contains(&tool_calls[*tool_call_idx].assistant_idx) => {}
            _ => keep_prefix[idx] = false,
        }
    }

    let mut current_idx = 0usize;
    modifiable_prefix.retain(|_| {
        let idx = current_idx;
        current_idx += 1;
        keep_prefix[idx]
    });
    before != serde_json::to_value(&*modifiable_prefix).ok()
}

fn mark_preserved_message_targets(
    target_results: &mut CompressChatTargetResults,
    message: &ChatMessage,
    request: &CompressChatApplyRequest,
) {
    if message.role == "context_file" {
        if let Some(target_id) = message_request_id(
            request.drop_context_messages,
            compression_context_message_target_id(message),
            message,
        ) {
            target_results.mark_skipped("drop_context_messages", target_id, "preserve_tail");
        }
        for file in extract_context_files(message) {
            if let Some(target_id) =
                context_file_request_id(request.drop_context_files, &file, false)
            {
                target_results.mark_skipped("drop_context_files", target_id, "preserve_tail");
            }
            if let Some(target_id) = context_file_request_id(request.drop_memories, &file, true) {
                target_results.mark_skipped("drop_memories", target_id, "preserve_tail");
            }
        }
    }
    if message.role == "tool" || message.role == "diff" {
        let canonical_id = compression_tool_output_target_id(message);
        if let Some(target_id) =
            message_request_id(request.drop_tool_outputs, canonical_id.clone(), message)
        {
            target_results.mark_skipped("drop_tool_outputs", target_id, "preserve_tail");
        }
        if let Some(target_id) =
            message_request_id(request.truncate_tool_outputs, canonical_id, message)
        {
            target_results.mark_skipped("truncate_tool_outputs", target_id, "preserve_tail");
        }
    }
}

fn target_matches_other_kind(
    messages: &[ChatMessage],
    operation: &str,
    target_id: &str,
) -> bool {
    let mut context_message_match = false;
    let mut tool_output_match = false;
    let mut context_file_match = false;
    let mut memory_match = false;
    for message in messages {
        if message.role == "context_file" {
            context_message_match |= compression_context_message_target_id(message).as_deref()
                == Some(target_id)
                || (!message.tool_call_id.is_empty() && message.tool_call_id == target_id);
            for file in extract_context_files(message) {
                let file_match = compression_context_file_target_id(&file) == target_id
                    || file.file_name == target_id;
                context_file_match |= file_match && !is_memory_path(&file.file_name);
                memory_match |= file_match && is_memory_path(&file.file_name);
            }
        }
        if message.role == "tool" || message.role == "diff" {
            tool_output_match |= compression_tool_output_target_id(message).as_deref()
                == Some(target_id)
                || (!message.tool_call_id.is_empty() && message.tool_call_id == target_id);
        }
    }
    match operation {
        "drop_context_messages" => tool_output_match || context_file_match || memory_match,
        "drop_tool_outputs" | "truncate_tool_outputs" => {
            context_message_match || context_file_match || memory_match
        }
        "drop_context_files" => context_message_match || tool_output_match || memory_match,
        "drop_memories" => context_message_match || tool_output_match || context_file_match,
        _ => false,
    }
}

fn tool_replacement_tokens_saved(message: &ChatMessage, replacement: &str) -> Option<usize> {
    let original = message.content.content_text_only();
    if replacement.len() >= original.len() {
        return None;
    }
    let before_tokens = approx_tokens_for_message(message);
    let after_tokens = approx_tokens_for_len(replacement.len());
    (after_tokens < before_tokens).then_some(before_tokens - after_tokens)
}

pub fn compress_chat_apply_head_output(
    mut head_messages: Vec<ChatMessage>,
    immutable_tail: &[ChatMessage],
    request: &CompressChatApplyRequest,
) -> CompressChatApplyOutput {
    let original_head_messages = head_messages.clone();
    let before_tokens = tokens_with_tail(&head_messages, &[], immutable_tail);
    let preserve_cutoff = preserve_cutoff_for(&head_messages, request.preserve_last_turns);
    let mut preserved_tail = head_messages.split_off(preserve_cutoff.min(head_messages.len()));
    let mut stats = CompressChatApplyStats::default();
    let mut target_results = CompressChatTargetResults::new(request);
    for message in original_head_messages
        .iter()
        .skip(preserve_cutoff.min(original_head_messages.len()))
        .chain(immutable_tail.iter())
    {
        mark_preserved_message_targets(&mut target_results, message, request);
    }
    for result in target_results.results.iter_mut() {
        if result.reason.as_deref() == Some("missing_or_already_dropped")
            && (target_matches_other_kind(
                &original_head_messages,
                &result.operation,
                &result.target_id,
            ) || target_matches_other_kind(
                immutable_tail,
                &result.operation,
                &result.target_id,
            ))
        {
            result.reason = Some("mismatched_id".to_string());
        }
    }

    if request.drop_project_information {
        let first_system_idx = head_messages.iter().position(|m| m.role == "system");
        let mut idx = 0usize;
        head_messages.retain(|msg| {
            let keep = if msg.role != "system" {
                true
            } else if Some(idx) == first_system_idx {
                true
            } else {
                let text = msg.content.content_text_only().to_lowercase();
                if text.contains("project") || text.contains("workspace") {
                    stats.project_info_dropped += 1;
                    false
                } else {
                    true
                }
            };
            idx += 1;
            keep
        });
    }

    let mut updated_head: Vec<ChatMessage> = Vec::with_capacity(head_messages.len());
    for msg in head_messages.into_iter() {
        if msg.role != "context_file" {
            updated_head.push(msg);
            continue;
        }
        if let Some(target_id) = message_request_id(
            request.drop_context_messages,
            compression_context_message_target_id(&msg),
            &msg,
        ) {
            stats.context_messages_dropped += 1;
            target_results.mark_applied(
                "drop_context_messages",
                target_id,
                approx_tokens_for_message(&msg),
            );
            continue;
        }

        let mut files = extract_context_files(&msg);
        if files.is_empty() {
            updated_head.push(msg);
            continue;
        }

        let mut remaining: Vec<ContextFile> = Vec::new();
        for cf in files.drain(..) {
            let is_memory = is_memory_path(&cf.file_name);
            if let Some(target_id) =
                context_file_request_id(request.drop_context_files, &cf, false)
            {
                stats.context_files_dropped += 1;
                target_results.mark_applied(
                    "drop_context_files",
                    target_id,
                    approx_tokens_for_len(cf.file_content.len()),
                );
                continue;
            }
            if request.drop_all_memories && is_memory {
                stats.memory_dropped += 1;
                continue;
            }
            if let Some(target_id) = context_file_request_id(request.drop_memories, &cf, true) {
                stats.memory_dropped += 1;
                target_results.mark_applied(
                    "drop_memories",
                    target_id,
                    approx_tokens_for_len(cf.file_content.len()),
                );
                continue;
            }
            if request.drop_all_context_files {
                stats.context_files_dropped += 1;
                continue;
            }
            remaining.push(cf);
        }

        if remaining.is_empty() {
            stats.context_messages_dropped += 1;
            continue;
        }

        let mut new_msg = msg.clone();
        new_msg.content = ChatContent::ContextFiles(remaining);
        updated_head.push(new_msg);
    }

    head_messages = updated_head;

    if request.strip_images {
        for msg in head_messages.iter_mut() {
            if let ChatContent::Multimodal(elements) = &mut msg.content {
                for element in elements.iter_mut() {
                    if element.m_type != "text" {
                        element.m_type = "text".to_string();
                        element.m_content = "[non-text content removed by compression]".to_string();
                        stats.non_text_stripped += 1;
                    }
                }
            }
        }
    }

    if request.dedup_context_files {
        if let Ok((count, _)) = compress_duplicate_context_files(&mut head_messages) {
            stats.dedup_count = count;
        }
    }

    for msg in head_messages.iter_mut() {
        if msg.role != "tool" && msg.role != "diff" {
            continue;
        }
        let canonical_id = compression_tool_output_target_id(msg);
        let drop_target =
            message_request_id(request.drop_tool_outputs, canonical_id.clone(), msg);
        let truncate_target =
            message_request_id(request.truncate_tool_outputs, canonical_id, msg);
        if msg.preserve == Some(true) {
            if let Some(target_id) = drop_target {
                target_results.mark_skipped("drop_tool_outputs", target_id, "preserve_flag");
            }
            if let Some(target_id) = truncate_target {
                target_results.mark_skipped(
                    "truncate_tool_outputs",
                    target_id,
                    "preserve_flag",
                );
            }
            continue;
        }
        if let Some(name) = request.tool_call_names.get(&msg.tool_call_id) {
            if should_preserve_tool(name) && !request.truncate_all_tool_outputs {
                if let Some(target_id) = drop_target {
                    target_results.mark_skipped("drop_tool_outputs", target_id, "protected_tool");
                }
                if let Some(target_id) = truncate_target {
                    target_results.mark_skipped(
                        "truncate_tool_outputs",
                        target_id,
                        "protected_tool",
                    );
                }
                continue;
            }
        }
        if let Some(target_id) = drop_target {
            let replacement = "Tool result removed by compress_chat_apply";
            if let Some(tokens_saved) = tool_replacement_tokens_saved(msg, replacement) {
                msg.content = ChatContent::SimpleText(replacement.to_string());
                stats.tool_dropped += 1;
                target_results.mark_applied("drop_tool_outputs", target_id, tokens_saved);
            } else {
                target_results.mark_skipped("drop_tool_outputs", target_id, "no_savings");
            }
            if let Some(truncate_target) = truncate_target {
                target_results.mark_skipped(
                    "truncate_tool_outputs",
                    truncate_target,
                    "superseded_by_drop",
                );
            }
            continue;
        }
        if request.truncate_all_tool_outputs || truncate_target.is_some() {
            let content = msg.content.content_text_only();
            if content.len() > TOOL_OUTPUT_TRUNCATE_LIMIT {
                let redacted = redact_sensitive(&content);
                let preview: String = redacted.chars().take(TOOL_OUTPUT_TRUNCATE_LIMIT).collect();
                let replacement = format!("Tool result compressed: {}...", preview);
                if let Some(tokens_saved) = tool_replacement_tokens_saved(msg, &replacement) {
                    msg.content = ChatContent::SimpleText(replacement);
                    stats.tool_truncated += 1;
                    if let Some(target_id) = truncate_target {
                        target_results.mark_applied(
                            "truncate_tool_outputs",
                            target_id,
                            tokens_saved,
                        );
                    }
                    continue;
                }
            }
            if let Some(target_id) = truncate_target {
                target_results.mark_skipped("truncate_tool_outputs", target_id, "no_savings");
            }
        }
    }

    if request.reduce_plain_text {
        // Last-resort, model-free reduction: a conversation that outgrew the
        // summarizer's input budget (no context files or tool outputs to drop)
        // is saved by lossily shrinking plain-text turns above a size floor,
        // so a compression report can be produced and generation can proceed.
        // Tiny turns (below the floor) are left untouched, which keeps a
        // genuinely empty/near-empty chat a no-op.
        let last_oversized_assistant = head_messages.iter().rposition(|msg| {
            msg.role == "assistant"
                && matches!(
                    &msg.content,
                    ChatContent::SimpleText(text) if text.len() > RECOVERY_PLAIN_TEXT_FLOOR
                )
        });
        for (idx, msg) in head_messages.iter_mut().enumerate() {
            if Some(idx) == last_oversized_assistant || msg.preserve == Some(true) {
                continue;
            }
            if msg.role != "user" && msg.role != "assistant" {
                continue;
            }
            let ChatContent::SimpleText(text) = &mut msg.content else {
                continue;
            };
            let original_len = text.len();
            if original_len <= RECOVERY_PLAIN_TEXT_FLOOR {
                continue;
            }
            *text = format!(
                "[compressed by deterministic recovery sweep: {original_len} characters of original conversation text removed]"
            );
            stats.plain_text_reduced += 1;
        }
    }

    let mut cleanup_tail = Vec::with_capacity(preserved_tail.len() + immutable_tail.len());
    cleanup_tail.extend_from_slice(&preserved_tail);
    cleanup_tail.extend_from_slice(immutable_tail);
    if remove_invalid_tool_calls_and_tool_calls_results_before(&mut head_messages, &cleanup_tail) {
        stats.tool_dropped += 1;
    }

    if (request.strength == "balanced" || request.strength == "aggressive")
        && !request.dedup_context_files
    {
        let cur_tokens = tokens_with_tail(&head_messages, &preserved_tail, immutable_tail);
        let needs_more = request.target_tokens.map_or(true, |t| cur_tokens > t);
        if needs_more {
            if let Ok((count, _)) = compress_duplicate_context_files(&mut head_messages) {
                stats.dedup_count += count;
            }
        }
    }

    let after_tokens_pre_report =
        tokens_with_tail(&head_messages, &preserved_tail, immutable_tail);
    let tokens_saved = before_tokens.saturating_sub(after_tokens_pre_report);
    let report = if stats.has_meaningful_mutation() && tokens_saved > 0 {
        let sorted = |set: &HashSet<String>| -> Vec<String> {
            let mut items: Vec<String> = set.iter().cloned().collect();
            items.sort();
            items
        };
        // Canonical operation identity over every mutation-driving request field so
        // distinct manual operations never share a fingerprint.
        let fingerprint_payload = json!({
            "op": "ctx_apply",
            "drop_context_files": sorted(request.drop_context_files),
            "drop_all_context_files": request.drop_all_context_files,
            "drop_context_messages": sorted(request.drop_context_messages),
            "drop_tool_outputs": sorted(request.drop_tool_outputs),
            "truncate_tool_outputs": sorted(request.truncate_tool_outputs),
            "truncate_all_tool_outputs": request.truncate_all_tool_outputs,
            "drop_memories": sorted(request.drop_memories),
            "drop_all_memories": request.drop_all_memories,
            "dedup_context_files": request.dedup_context_files,
            "drop_project_information": request.drop_project_information,
            "reduce_plain_text": request.reduce_plain_text,
            "strength": request.strength,
            "preserve_last_turns": request.preserve_last_turns,
            "target_tokens": request.target_tokens,
        });
        let mut fingerprint_hasher = Sha256::new();
        fingerprint_hasher.update(serde_json::to_vec(&fingerprint_payload).unwrap_or_default());
        let op_fingerprint = hex::encode(fingerprint_hasher.finalize());
        Some(build_compression_report_message_with_fingerprint(
            stats.context_files_removed(),
            stats.context_messages_dropped,
            stats.tool_truncated + stats.tool_dropped,
            before_tokens,
            after_tokens_pre_report,
            &op_fingerprint,
        ))
    } else {
        None
    };
    let modifiable_len = head_messages.len();
    head_messages.append(&mut preserved_tail);
    let affected_boundary = report
        .as_ref()
        .and_then(|_| first_changed_boundary(&original_head_messages, &head_messages));
    CompressChatApplyOutput {
        messages: head_messages,
        stats,
        before_tokens,
        tokens_saved,
        target_results: target_results.results,
        report,
        affected_boundary,
        modifiable_len,
    }
}

pub fn build_maximal_full_sweep_request<'a>(
    drop_context_files: &'a HashSet<String>,
    drop_memories: &'a HashSet<String>,
    truncate_tool_outputs: &'a HashSet<String>,
    drop_tool_outputs: &'a HashSet<String>,
    drop_context_messages: &'a HashSet<String>,
    tool_call_names: &'a HashMap<String, String>,
) -> CompressChatApplyRequest<'a> {
    CompressChatApplyRequest {
        drop_context_files,
        drop_all_context_files: true,
        drop_memories,
        drop_all_memories: true,
        truncate_tool_outputs,
        truncate_all_tool_outputs: true,
        drop_tool_outputs,
        drop_context_messages,
        dedup_context_files: true,
        drop_project_information: true,
        strip_images: true,
        reduce_plain_text: true,
        strength: "aggressive",
        preserve_last_turns: None,
        target_tokens: None,
        tool_call_names,
    }
}

pub fn approx_tokens_for_len(len: usize) -> usize {
    len / 4 + 10
}

pub fn approx_tokens_for_message(msg: &ChatMessage) -> usize {
    let content_len = match &msg.content {
        ChatContent::SimpleText(text) => text.len(),
        ChatContent::Multimodal(elements) => elements.len() * 100,
        ChatContent::ContextFiles(files) => files.iter().map(|cf| cf.file_content.len()).sum(),
    };
    approx_tokens_for_len(content_len)
}

pub fn extract_context_files(message: &ChatMessage) -> Vec<ContextFile> {
    match &message.content {
        ChatContent::ContextFiles(files) => files.clone(),
        ChatContent::SimpleText(text) => serde_json::from_str(text).unwrap_or_default(),
        _ => vec![],
    }
}
