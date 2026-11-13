use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::sync::Mutex as AMutex;
use refact_runtime_api::AtCommandsContext;
use refact_core::chat_types::{ChatContent, ChatMessage, ContextEnum, ContextFile};
use refact_tool_api::IntegrationConfirmation;
use refact_postprocessing::pp_command_output::OutputFilter;
use crate::tools::tools_description::{ json_schema_from_params, Tool, ToolDesc, ToolSource, ToolSourceType, };
use refact_chat_history::history_limit::{compress_duplicate_context_files, compute_context_budget};
use refact_chat_history::trajectory_ops::{ build_compression_report_message_with_fingerprint, insert_compression_report_at_boundary_scoped, is_memory_path, should_preserve_tool, };
use refact_core::string_utils::redact_sensitive;
use refact_runtime_api::SessionState;
use sha2::{Digest, Sha256};
const TOOL_OUTPUT_TRUNCATE_LIMIT: usize = 200;
/// Minimum plain-text length (chars) for a turn to be reduced by the
/// deterministic recovery sweep. Keeps tiny chats a no-op.
const RECOVERY_PLAIN_TEXT_FLOOR: usize = 1024;
const MAX_PER_MESSAGE_ENTRIES: usize = 200;
const MAX_CONTEXT_ENTRIES: usize = 200;
const MAX_TOOL_OUTPUT_ENTRIES: usize = 200;

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
        if !msg.tool_call_id.is_empty() && request.drop_context_messages.contains(&msg.tool_call_id)
        {
            stats.context_messages_dropped += 1;
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
            if request.drop_context_files.contains(&cf.file_name) {
                stats.context_files_dropped += 1;
                continue;
            }
            if request.drop_all_memories && is_memory {
                stats.memory_dropped += 1;
                continue;
            }
            if request.drop_memories.contains(&cf.file_name) {
                stats.memory_dropped += 1;
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
        if msg.tool_call_id.is_empty() {
            continue;
        }
        if msg.preserve == Some(true) {
            continue;
        }
        if let Some(name) = request.tool_call_names.get(&msg.tool_call_id) {
            if should_preserve_tool(name) && !request.truncate_all_tool_outputs {
                continue;
            }
        }
        if request.drop_tool_outputs.contains(&msg.tool_call_id) {
            msg.content =
                ChatContent::SimpleText("Tool result removed by compress_chat_apply".to_string());
            stats.tool_dropped += 1;
            continue;
        }
        if request.truncate_all_tool_outputs
            || request.truncate_tool_outputs.contains(&msg.tool_call_id)
        {
            let content = msg.content.content_text_only();
            if content.len() > TOOL_OUTPUT_TRUNCATE_LIMIT {
                let redacted = redact_sensitive(&content);
                let preview: String = redacted.chars().take(TOOL_OUTPUT_TRUNCATE_LIMIT).collect();
                msg.content =
                    ChatContent::SimpleText(format!("Tool result compressed: {}...", preview));
                stats.tool_truncated += 1;
            }
        }
    }

    if request.reduce_plain_text {
        // Last-resort, model-free reduction: a conversation that outgrew the
        // summarizer's input budget (no context files or tool outputs to drop)
        // is saved by lossily shrinking plain-text turns above a size floor,
        // so a compression report can be produced and generation can proceed.
        // Tiny turns (below the floor) are left untouched, which keeps a
        // genuinely empty/near-empty chat a no-op. The modifiable head is the
        // already-expired part of the conversation: after this sweep the user
        // re-prompts, so every oversized plain-text turn is safe to shrink,
        // including the last one.
        for msg in head_messages.iter_mut() {
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

    let report = if stats.has_meaningful_mutation() {
        let after_tokens_pre_report =
            tokens_with_tail(&head_messages, &preserved_tail, immutable_tail);
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

fn approx_tokens_for_len(len: usize) -> usize {
    len / 4 + 10
}

fn approx_tokens_for_message(msg: &ChatMessage) -> usize {
    let content_len = match &msg.content {
        ChatContent::SimpleText(text) => text.len(),
        ChatContent::Multimodal(elements) => elements.len() * 100,
        ChatContent::ContextFiles(files) => files.iter().map(|cf| cf.file_content.len()).sum(),
    };
    approx_tokens_for_len(content_len)
}

fn extract_context_files(message: &ChatMessage) -> Vec<ContextFile> {
    match &message.content {
        ChatContent::ContextFiles(files) => files.clone(),
        ChatContent::SimpleText(text) => serde_json::from_str(text).unwrap_or_default(),
        _ => vec![],
    }
}

fn parse_bool(args: &HashMap<String, Value>, key: &str) -> bool {
    args.get(key)
        .and_then(refact_tool_api::coerce_bool)
        .unwrap_or(false)
}

fn parse_string_list(args: &HashMap<String, Value>, key: &str) -> Vec<String> {
    match args.get(key) {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .collect(),
        Some(Value::String(text)) => {
            let trimmed = text.trim();
            if trimmed.starts_with('[') {
                serde_json::from_str::<Vec<String>>(trimmed).unwrap_or_default()
            } else {
                trimmed
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect()
            }
        }
        _ => vec![],
    }
}

pub struct ToolCompressChatProbe {
    pub config_path: String,
}

pub struct ToolCompressChatApply {
    pub config_path: String,
}

#[async_trait]
impl Tool for ToolCompressChatProbe {
    fn tool_description(&self) -> ToolDesc {
        ToolDesc {
            name: "compress_chat_probe".to_string(),
            display_name: "Compress Chat (Probe)".to_string(),
            source: ToolSource {
                source_type: ToolSourceType::Builtin,
                config_path: self.config_path.clone(),
            },
            experimental: false,
            allow_parallel: false,
            description: "Analyze the current chat and report token distribution plus potential compression gains.".to_string(),
            input_schema: json_schema_from_params(&[], &[]),
            output_schema: None,
            annotations: None,
        }
    }

    async fn tool_execute(
        &mut self,
        ccx: Arc<AMutex<AtCommandsContext>>,
        tool_call_id: &String,
        _args: &HashMap<String, Value>,
    ) -> Result<(bool, Vec<ContextEnum>), String> {
        let (chat_facade, chat_id, n_ctx) = {
            let ccx_lock = ccx.lock().await;
            (
                ccx_lock.app.chat.facade.clone(),
                ccx_lock.chat_id.clone(),
                ccx_lock.n_ctx,
            )
        };

        let messages = chat_facade.session_snapshot(&chat_id).await?.messages;
        // Probe the linearized view: sources suppressed by segment summaries
        // never reach the model, so counting them would overstate usage and
        // tempt the agent into compressing messages that cost nothing.
        let messages = refact_core::active_context::active_context(&messages)
            .map_err(|e| e.to_string())?
            .messages;

        if messages.is_empty() {
            return Err("Cannot probe an empty chat".to_string());
        }

        let mut role_tokens: HashMap<String, usize> = HashMap::new();
        let mut per_message: Vec<Value> = Vec::new();
        let mut total_tokens = 0usize;

        let mut context_occurrences: HashMap<String, usize> = HashMap::new();
        let mut context_token_map: HashMap<String, Vec<usize>> = HashMap::new();

        for (idx, msg) in messages.iter().enumerate() {
            let content_len = match &msg.content {
                ChatContent::SimpleText(text) => text.len(),
                ChatContent::Multimodal(elements) => elements.len() * 100,
                ChatContent::ContextFiles(files) => {
                    files.iter().map(|cf| cf.file_content.len()).sum()
                }
            };
            let tokens = approx_tokens_for_len(content_len);
            total_tokens += tokens;
            *role_tokens.entry(msg.role.clone()).or_insert(0) += tokens;
            per_message.push(json!({
                "index": idx,
                "role": msg.role,
                "tokens": tokens,
                "chars": content_len,
            }));

            if msg.role == "context_file" {
                for cf in extract_context_files(msg) {
                    *context_occurrences.entry(cf.file_name.clone()).or_insert(0) += 1;
                    context_token_map
                        .entry(cf.file_name.clone())
                        .or_default()
                        .push(approx_tokens_for_len(cf.file_content.len()));
                }
            }
        }

        let mut context_files: Vec<Value> = Vec::new();
        let mut memory_tokens = 0usize;
        for (idx, msg) in messages.iter().enumerate() {
            if msg.role != "context_file" {
                continue;
            }
            for cf in extract_context_files(msg) {
                let tokens = approx_tokens_for_len(cf.file_content.len());
                let is_memory = is_memory_path(&cf.file_name);
                if is_memory {
                    memory_tokens += tokens;
                }
                let occurrences = context_occurrences.get(&cf.file_name).copied().unwrap_or(1);
                let file_name = cf.file_name.clone();
                context_files.push(json!({
                    "index": idx,
                    "file_name": file_name,
                    "tokens": tokens,
                    "chars": cf.file_content.len(),
                    "is_memory": is_memory,
                    "occurrences": occurrences,
                }));
            }
        }

        let mut tool_call_names: HashMap<String, String> = HashMap::new();
        for msg in &messages {
            if let Some(ref tool_calls) = msg.tool_calls {
                for tc in tool_calls {
                    tool_call_names.insert(tc.id.clone(), tc.function.name.clone());
                }
            }
        }

        let mut tool_outputs: Vec<Value> = Vec::new();
        let mut tool_output_tokens = 0usize;
        for (idx, msg) in messages.iter().enumerate() {
            if msg.role != "tool" && msg.role != "diff" {
                continue;
            }
            let tokens = approx_tokens_for_message(msg);
            let tool_name = tool_call_names
                .get(&msg.tool_call_id)
                .cloned()
                .unwrap_or_else(|| "unknown".to_string());
            if !should_preserve_tool(&tool_name) {
                tool_output_tokens += tokens;
            }
            tool_outputs.push(json!({
                "index": idx,
                "tool_call_id": msg.tool_call_id,
                "tool_name": tool_name,
                "role": msg.role,
                "tokens": tokens,
                "chars": msg.content.content_text_only().len(),
            }));
        }

        let mut context_messages: Vec<Value> = Vec::new();
        for (idx, msg) in messages.iter().enumerate() {
            if msg.role != "context_file" {
                continue;
            }
            context_messages.push(json!({
                "index": idx,
                "tool_call_id": msg.tool_call_id,
                "tokens": approx_tokens_for_message(msg),
                "chars": msg.content.content_text_only().len(),
            }));
        }

        let mut per_message_truncated = false;
        if per_message.len() > MAX_PER_MESSAGE_ENTRIES {
            let head = MAX_PER_MESSAGE_ENTRIES / 2;
            let tail = MAX_PER_MESSAGE_ENTRIES - head;
            let mut trimmed = Vec::with_capacity(MAX_PER_MESSAGE_ENTRIES);
            trimmed.extend_from_slice(&per_message[..head]);
            trimmed.extend_from_slice(&per_message[per_message.len().saturating_sub(tail)..]);
            per_message = trimmed;
            per_message_truncated = true;
        }

        let mut context_messages_truncated = false;
        if context_messages.len() > MAX_CONTEXT_ENTRIES {
            let head = MAX_CONTEXT_ENTRIES / 2;
            let tail = MAX_CONTEXT_ENTRIES - head;
            let mut trimmed = Vec::with_capacity(MAX_CONTEXT_ENTRIES);
            trimmed.extend_from_slice(&context_messages[..head]);
            trimmed.extend_from_slice(
                &context_messages[context_messages.len().saturating_sub(tail)..],
            );
            context_messages = trimmed;
            context_messages_truncated = true;
        }

        let mut context_files_truncated = false;
        if context_files.len() > MAX_CONTEXT_ENTRIES {
            context_files.sort_by_key(|v| v.get("tokens").and_then(|x| x.as_u64()).unwrap_or(0));
            context_files.reverse();
            context_files.truncate(MAX_CONTEXT_ENTRIES);
            context_files_truncated = true;
        }

        let mut tool_outputs_truncated = false;
        if tool_outputs.len() > MAX_TOOL_OUTPUT_ENTRIES {
            tool_outputs.sort_by_key(|v| v.get("tokens").and_then(|x| x.as_u64()).unwrap_or(0));
            tool_outputs.reverse();
            tool_outputs.truncate(MAX_TOOL_OUTPUT_ENTRIES);
            tool_outputs_truncated = true;
        }

        let mut duplicate_context_tokens = 0usize;
        for tokens in context_token_map.values() {
            if tokens.len() > 1 {
                let max_val = tokens.iter().copied().max().unwrap_or(0);
                let total: usize = tokens.iter().sum();
                duplicate_context_tokens += total.saturating_sub(max_val);
            }
        }

        let mut project_info_tokens = 0usize;
        let first_system_idx = messages.iter().position(|m| m.role == "system");
        for (idx, msg) in messages.iter().enumerate() {
            if msg.role == "system" && Some(idx) != first_system_idx {
                let text = msg.content.content_text_only().to_lowercase();
                if text.contains("project") || text.contains("workspace") {
                    project_info_tokens += approx_tokens_for_message(msg);
                }
            }
        }

        let role_tokens_json = serde_json::to_value(&role_tokens).unwrap_or_else(|_| json!({}));

        let budget = compute_context_budget(&messages, n_ctx);
        let pressure_label = match budget.pressure {
            refact_chat_history::history_limit::ContextPressure::Low => "low",
            refact_chat_history::history_limit::ContextPressure::Medium => "medium",
            refact_chat_history::history_limit::ContextPressure::High => "high",
            refact_chat_history::history_limit::ContextPressure::Critical => "critical",
        };
        let pct_used = if n_ctx > 0 {
            total_tokens.saturating_mul(100) / n_ctx
        } else {
            0
        };

        let result = json!({
            "type": "ctx_probe",
            "messages_count": messages.len(),
            "total_tokens": total_tokens,
            "n_ctx": n_ctx,
            "pct_used": pct_used,
            "context_pressure": pressure_label,
            "role_tokens": role_tokens_json,
            "per_message": per_message,
            "context_files": context_files,
            "context_messages": context_messages,
            "tool_outputs": tool_outputs,
            "per_message_truncated": per_message_truncated,
            "context_files_truncated": context_files_truncated,
            "context_messages_truncated": context_messages_truncated,
            "tool_outputs_truncated": tool_outputs_truncated,
            "potential_gains": {
                "duplicate_context_tokens": duplicate_context_tokens,
                "tool_output_tokens": tool_output_tokens,
                "memory_tokens": memory_tokens,
                "project_info_tokens": project_info_tokens,
            }
        });

        Ok((
            false,
            vec![ContextEnum::ChatMessage(ChatMessage {
                role: "tool".to_string(),
                content: ChatContent::SimpleText(
                    serde_json::to_string_pretty(&result).unwrap_or_else(|_| result.to_string()),
                ),
                tool_call_id: tool_call_id.clone(),
                output_filter: Some(OutputFilter::no_limits()),
                ..Default::default()
            })],
        ))
    }

    fn confirm_deny_rules(&self) -> Option<IntegrationConfirmation> {
        None
    }
}

#[async_trait]
impl Tool for ToolCompressChatApply {
    fn tool_description(&self) -> ToolDesc {
        let input_schema = json!({
            "type": "object",
            "properties": {
                "rebuild_context": {"type": "boolean", "description": "Queue a full model-based context rebuild after tool results complete. Cannot be mixed with static operations; dry_run never invokes a model."},
                "drop_context_files": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "List of context file names to drop entirely"
                },
                "drop_memories": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "Memory/knowledge file paths to drop"
                },
                "drop_context_messages": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "Context-file message tool_call_id values to drop entirely"
                },
                "drop_all_memories": {
                    "type": "boolean",
                    "description": "Drop all memory/knowledge context files"
                },
                "truncate_tool_outputs": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "Tool call IDs to truncate"
                },
                "drop_tool_outputs": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "Tool call IDs to drop (replaced with a short placeholder)"
                },
                "dedup_context_files": {
                    "type": "boolean",
                    "description": "Deduplicate repeated context files"
                },
                "drop_project_information": {
                    "type": "boolean",
                    "description": "Drop system/project info messages"
                },
                "target_tokens": {
                    "type": "integer",
                    "description": "Target total token count after compression"
                },
                "strength": {
                    "type": "string",
                    "enum": ["conservative", "balanced", "aggressive"],
                    "description": "conservative=explicit ops only, balanced=+auto dedup, aggressive=+auto dedup (static only; use rebuild_context for model reconstruction)"
                },
                "preserve_last_turns": {
                    "type": "integer",
                    "description": "Number of recent user/assistant turns to keep unmodified"
                },
                "dry_run": {
                    "type": "boolean",
                    "description": "Preview compression stats without applying changes"
                }
            },
            "required": []
        });

        ToolDesc {
            name: "compress_chat_apply".to_string(),
            display_name: "Compress Chat (Apply)".to_string(),
            source: ToolSource {
                source_type: ToolSourceType::Builtin,
                config_path: self.config_path.clone(),
            },
            experimental: false,
            allow_parallel: false,
            description: "Apply selective compression to the current chat using explicit drop/truncate lists.".to_string(),
            input_schema,
            output_schema: None,
            annotations: None,
        }
    }

    async fn tool_execute(
        &mut self,
        ccx: Arc<AMutex<AtCommandsContext>>,
        tool_call_id: &String,
        args: &HashMap<String, Value>,
    ) -> Result<(bool, Vec<ContextEnum>), String> {
        if parse_bool(args, "rebuild_context") {
            if args
                .keys()
                .any(|key| !matches!(key.as_str(), "rebuild_context" | "dry_run"))
            {
                return Err(
                    "rebuild_context cannot be mixed with static compression options".into(),
                );
            }
            let dry_run = parse_bool(args, "dry_run");
            if !dry_run {
                let (gcx, chat_id) = {
                    let c = ccx.lock().await;
                    (c.app.gcx.clone(), c.chat_id.clone())
                };
                let session = gcx
                    .chat_sessions
                    .read()
                    .await
                    .get(&chat_id)
                    .cloned()
                    .ok_or("Chat session not found")?;
                refact_chat_api::pending_context_rebuild::request_rebuild(
                    &mut *session.lock().await,
                    refact_chat_api::pending_context_rebuild::PendingContextRebuild {
                        model: None,
                        trigger: "compress_chat_apply".into(),
                    },
                )?;
            }
            return Ok((false, vec![ContextEnum::ChatMessage(ChatMessage {
                role: "tool".into(), tool_call_id: tool_call_id.clone(),
                content: ChatContent::SimpleText(json!({"type":"ctx_apply", "rebuild_context":true, "dry_run":dry_run, "queued":!dry_run}).to_string()),
                ..Default::default()
            })]));
        }
        let drop_context_files = parse_string_list(args, "drop_context_files");
        let drop_memories = parse_string_list(args, "drop_memories");
        let drop_all_memories = parse_bool(args, "drop_all_memories");
        let truncate_tool_outputs = parse_string_list(args, "truncate_tool_outputs");
        let drop_tool_outputs = parse_string_list(args, "drop_tool_outputs");
        let drop_context_messages = parse_string_list(args, "drop_context_messages");
        let dedup_context_files = parse_bool(args, "dedup_context_files");
        let drop_project_information = parse_bool(args, "drop_project_information");
        let dry_run = parse_bool(args, "dry_run");
        let strength = args
            .get("strength")
            .and_then(|v| v.as_str())
            .unwrap_or("conservative")
            .to_string();
        let preserve_last_turns = args
            .get("preserve_last_turns")
            .and_then(|v| v.as_u64())
            .map(|n| n as usize);
        let target_tokens = args
            .get("target_tokens")
            .and_then(|v| v.as_u64())
            .map(|n| n as usize);

        let (chat_facade, chat_id) = {
            let ccx_lock = ccx.lock().await;
            (ccx_lock.app.chat.facade.clone(), ccx_lock.chat_id.clone())
        };

        let mut session_snapshot = chat_facade.session_snapshot(&chat_id).await?;
        let stored_messages = session_snapshot.messages.clone();
        let projection = refact_core::active_context::active_context(&stored_messages)
            .map_err(|e| e.to_string())?;
        session_snapshot.messages = projection.messages.clone();
        if matches!(session_snapshot.session_state, SessionState::Generating) {
            return Err("Cannot compress while generating".to_string());
        }

        let before_count = session_snapshot.messages.len();
        let active_start = session_snapshot
            .messages
            .iter()
            .rposition(|m| {
                m.role == "assistant"
                    && m.tool_calls
                        .as_ref()
                        .map(|tcs| tcs.iter().any(|tc| tc.id == *tool_call_id))
                        .unwrap_or(false)
            })
            .unwrap_or(session_snapshot.messages.len());

        if active_start >= session_snapshot.messages.len() {
            return Err("Active tool call not found in session".to_string());
        }

        let tool_call_names: HashMap<String, String> = session_snapshot
            .messages
            .iter()
            .filter_map(|m| m.tool_calls.as_ref())
            .flatten()
            .map(|tc| (tc.id.clone(), tc.function.name.clone()))
            .collect();
        let tail_messages = session_snapshot.messages[active_start..].to_vec();

        let drop_context_files: HashSet<String> = drop_context_files.into_iter().collect();
        let drop_memories: HashSet<String> = drop_memories.into_iter().collect();
        let drop_context_messages: HashSet<String> = drop_context_messages.into_iter().collect();
        let truncate_tool_outputs: HashSet<String> = truncate_tool_outputs.into_iter().collect();
        let drop_tool_outputs: HashSet<String> = drop_tool_outputs.into_iter().collect();

        let request = CompressChatApplyRequest {
            drop_context_files: &drop_context_files,
            drop_all_context_files: false,
            drop_memories: &drop_memories,
            drop_all_memories,
            truncate_tool_outputs: &truncate_tool_outputs,
            truncate_all_tool_outputs: false,
            drop_tool_outputs: &drop_tool_outputs,
            drop_context_messages: &drop_context_messages,
            dedup_context_files,
            drop_project_information,
            strip_images: false,
            reduce_plain_text: false,
            strength: &strength,
            preserve_last_turns,
            target_tokens,
            tool_call_names: &tool_call_names,
        };
        let CompressChatApplyOutput {
            messages: mut head_messages,
            stats,
            before_tokens,
            report,
            affected_boundary,
            modifiable_len,
        } = compress_chat_apply_head_output(
            session_snapshot.messages[..active_start].to_vec(),
            &tail_messages,
            &request,
        );
        head_messages.extend(tail_messages);
        let compression_report_metadata = insert_current_compression_report(
            &mut head_messages,
            report,
            affected_boundary,
            modifiable_len,
        );

        let after_tokens = head_messages
            .iter()
            .map(approx_tokens_for_message)
            .sum::<usize>();
        let after_count = head_messages.len();
        let report_after_tokens = compression_report_metadata
            .as_ref()
            .and_then(|metadata| metadata.get("tokens_after"))
            .and_then(|value| value.as_u64())
            .map(|value| value as usize)
            .unwrap_or(after_tokens);
        let target_met = target_tokens.map_or(true, |t| report_after_tokens <= t);

        let first_role = head_messages.first().map(|m| m.role.as_str()).unwrap_or("");
        let first_role_valid = matches!(first_role, "system" | "user" | "event" | "plan");
        if !dry_run && !first_role_valid {
            return Err(format!(
                "ctx_apply would produce an invalid chat history: first message has role '{}', expected 'system', 'user', 'event', or 'plan'. Compression aborted.",
                if first_role.is_empty() { "(empty)" } else { first_role }
            ));
        }

        if !dry_run {
            let gcx = ccx.lock().await.app.gcx.clone();
            let session = gcx
                .chat_sessions
                .read()
                .await
                .get(&chat_id)
                .cloned()
                .ok_or("Chat session not found")?;
            let mut session = session.lock().await;
            if session.closed
                || refact_chat_api::compression_gate::compression_attempt_active(&session)
                || session.pending_mode_handoff.is_some()
                || session.pending_context_rebuild.is_some()
                || serde_json::to_value(&session.messages).ok()
                    != serde_json::to_value(&stored_messages).ok()
            {
                return Err("Context changed while applying static compression".into());
            }
            let updated = refact_core::active_context::writeback_active_context(
                &stored_messages,
                &projection,
                &head_messages,
            )
            .map_err(|e| e.to_string())?;
            session.replace_messages(updated);
            session.thread.previous_response_id = None;
            session.thread.frozen_request_prefix = None;
            session.thread.claude_code_identity = None;
            session.openai_codex_websocket = Default::default();
            session.reset_cache_guard_snapshot();
            session.provider_usage_stale = true;
            drop(session);
            chat_facade.maybe_save_session(&chat_id).await?;
        }

        let result = json!({
            "type": "ctx_apply",
            "dry_run": dry_run,
            "before_message_count": before_count,
            "after_message_count": after_count,
            "before_tokens": before_tokens,
            "after_tokens": report_after_tokens,
            "target_tokens": target_tokens,
            "target_met": target_met,
            "strength": strength,
            "context_files_dropped": stats.context_files_dropped,
            "context_messages_dropped": stats.context_messages_dropped,
            "memories_dropped": stats.memory_dropped,
            "tool_outputs_truncated": stats.tool_truncated,
            "tool_outputs_dropped": stats.tool_dropped,
            "project_info_dropped": stats.project_info_dropped,
            "dedup_context_files": stats.dedup_count,
            "compression_report": compression_report_metadata.map(|metadata| json!({
                "role": "compression_report",
                "context_files_removed": metadata.get("context_files_removed").cloned().unwrap_or(Value::Null),
                "context_messages_dropped": metadata.get("context_messages_dropped").cloned().unwrap_or(Value::Null),
                "tool_results_truncated": metadata.get("tool_results_truncated").cloned().unwrap_or(Value::Null),
                "tokens_before": metadata.get("tokens_before").cloned().unwrap_or(Value::Null),
                "tokens_after": metadata.get("tokens_after").cloned().unwrap_or(Value::Null),
            })),
            "active_tail_start": active_start,
            "would_produce_invalid_history": !first_role_valid,
        });

        Ok((
            false,
            vec![ContextEnum::ChatMessage(ChatMessage {
                role: "tool".to_string(),
                content: ChatContent::SimpleText(result.to_string()),
                tool_call_id: tool_call_id.clone(),
                output_filter: Some(OutputFilter::no_limits()),
                ..Default::default()
            })],
        ))
    }

    fn confirm_deny_rules(&self) -> Option<IntegrationConfirmation> {
        None
    }
}
