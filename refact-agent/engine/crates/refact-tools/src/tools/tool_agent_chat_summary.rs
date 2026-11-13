use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::sync::Mutex as AMutex;

use refact_runtime_api::{AtCommandsContext, MAX_SUBCHAT_DEPTH};
use refact_core::chat_types::{ChatContent, ChatMessage, ChatToolCall, ContextEnum};
use refact_chat_api::{SubchatConfig, ToolsPolicy, TraceParent};
use refact_runtime_api::task_storage as storage;
use crate::tools::task_tool_helpers::require_bound_planner_task;
use crate::tools::tools_description::{Tool, ToolDesc, ToolSource, ToolSourceType};
use refact_runtime_api::{ChatSessionFacade};

const TOOL_ARG_LIMIT: usize = 300;
const TRANSCRIPT_CHAR_BUDGET: usize = 35_000;
const MESSAGE_CONTENT_LIMIT: usize = 4_000;
const SUMMARY_N_CTX: usize = 12_000;
const SUMMARY_MAX_NEW_TOKENS: usize = 2_000;
const SUMMARY_TIMEOUT_SECS: u64 = 30;

pub struct ToolAgentChatSummary;

impl ToolAgentChatSummary {
    pub fn new() -> Self {
        Self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AgentChatSummary {
    decisions: Vec<String>,
    files_touched: Vec<String>,
    blockers: Vec<String>,
    next_steps: Vec<String>,
    current_state: String,
    confidence_score: u8,
}

fn required_string(args: &HashMap<String, Value>, key: &str) -> Result<String, String> {
    args.get(key)
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| format!("Missing '{}'", key))
}

fn optional_string(args: &HashMap<String, Value>, key: &str) -> Option<String> {
    args.get(key)
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn truncate_chars(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_string();
    }
    if limit == 0 {
        return String::new();
    }
    let take = limit.saturating_sub(1);
    format!("{}…", text.chars().take(take).collect::<String>())
}

fn truncate_with_marker(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_string();
    }
    format!("{} [truncated]", truncate_chars(text, limit))
}

fn render_tool_call(tool_call: &ChatToolCall) -> String {
    let args = tool_call.function.arguments.trim();
    if args.is_empty() {
        return format!("{}()", tool_call.function.name);
    }
    format!(
        "{}({})",
        tool_call.function.name,
        truncate_chars(args, TOOL_ARG_LIMIT)
    )
}

fn render_message_block(index: usize, message: &ChatMessage) -> String {
    let mut lines = vec![format!("Message {} [{}]", index + 1, message.role)];

    if message.role == "tool" && !message.tool_call_id.is_empty() {
        lines.push(format!("Tool result for: {}", message.tool_call_id));
    }

    let content = message.content.to_text_with_image_placeholders();
    let content = content.trim();
    if !content.is_empty() {
        lines.push(truncate_with_marker(content, MESSAGE_CONTENT_LIMIT));
    }

    if let Some(tool_calls) = &message.tool_calls {
        for tool_call in tool_calls {
            lines.push(format!("Tool call: {}", render_tool_call(tool_call)));
        }
    }

    lines.join("\n")
}

fn linearize_transcript(messages: &[ChatMessage]) -> String {
    let blocks = messages
        .iter()
        .enumerate()
        .map(|(index, message)| render_message_block(index, message))
        .collect::<Vec<_>>();

    let mut selected = Vec::new();
    let mut used = 0usize;
    let mut omitted = 0usize;

    for block in blocks.iter().rev() {
        let block_len = block.len().saturating_add(2);
        if used.saturating_add(block_len) > TRANSCRIPT_CHAR_BUDGET && !selected.is_empty() {
            omitted += 1;
            continue;
        }
        used = used.saturating_add(block_len);
        selected.push(block.clone());
    }

    selected.reverse();
    let mut transcript = String::new();
    if omitted > 0 {
        transcript.push_str(&format!(
            "[{} older transcript messages omitted to fit summary budget]\n\n",
            omitted
        ));
    }
    transcript.push_str(&selected.join("\n\n"));
    transcript
}

fn build_summary_prompt(focus: Option<&str>, transcript: &str) -> String {
    let focus = focus.unwrap_or("general audit");
    format!(
        "Summarize the following agent transcript for the planner.\nFocus: {}\nReturn structured JSON with: decisions, files_touched, blockers, next_steps, current_state, confidence_score (0-100).\nReturn JSON only.\nTranscript:\n{}",
        focus, transcript
    )
}

fn extract_json_text(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    if serde_json::from_str::<Value>(trimmed).is_ok() {
        return Some(trimmed.to_string());
    }
    for fence in ["```json", "```"] {
        if let Some(start) = trimmed.find(fence) {
            let after = &trimmed[start + fence.len()..];
            let after = after.strip_prefix('\n').unwrap_or(after).trim_start();
            if let Some(end) = after.find("```") {
                return Some(after[..end].trim().to_string());
            }
        }
    }
    let start = trimmed.find('{')?;
    let end = trimmed.rfind('}')?;
    (end > start).then(|| trimmed[start..=end].to_string())
}

fn value_to_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => {
            let text = text.trim();
            (!text.is_empty()).then(|| text.to_string())
        }
        Value::Number(_) | Value::Bool(_) => Some(value.to_string()),
        Value::Object(map) => {
            let path = map
                .get("path")
                .or_else(|| map.get("file"))
                .or_else(|| map.get("filename"))
                .and_then(|value| value.as_str());
            let note = map
                .get("note")
                .or_else(|| map.get("summary"))
                .or_else(|| map.get("status"))
                .or_else(|| map.get("description"))
                .and_then(|value| value.as_str());
            match (path, note) {
                (Some(path), Some(note)) => Some(format!("{} ({})", path, note)),
                (Some(path), None) => Some(path.to_string()),
                _ => serde_json::to_string(value).ok(),
            }
        }
        _ => None,
    }
}

fn value_list(value: Option<&Value>) -> Vec<String> {
    match value {
        Some(Value::Array(items)) => items.iter().filter_map(value_to_text).collect(),
        Some(value) => value_to_text(value).into_iter().collect(),
        None => Vec::new(),
    }
}

fn value_string(value: Option<&Value>) -> String {
    value.and_then(value_to_text).unwrap_or_default()
}

fn value_confidence(value: Option<&Value>) -> u8 {
    let raw = match value {
        Some(Value::Number(number)) => number
            .as_u64()
            .or_else(|| number.as_f64().map(|v| v as u64)),
        Some(Value::String(text)) => text.trim().parse::<u64>().ok(),
        _ => None,
    }
    .unwrap_or(0);
    raw.min(100) as u8
}

fn parse_summary_response(raw: &str) -> Option<AgentChatSummary> {
    let json_text = extract_json_text(raw)?;
    let value = serde_json::from_str::<Value>(&json_text).ok()?;
    let map = value.as_object()?;
    Some(AgentChatSummary {
        decisions: value_list(map.get("decisions")),
        files_touched: value_list(map.get("files_touched")),
        blockers: value_list(
            map.get("blockers")
                .or_else(|| map.get("blockers_encountered")),
        ),
        next_steps: value_list(map.get("next_steps")),
        current_state: value_string(map.get("current_state")),
        confidence_score: value_confidence(map.get("confidence_score")),
    })
}

fn render_items(items: &[String]) -> String {
    if items.is_empty() {
        return "- None\n".to_string();
    }
    items
        .iter()
        .map(|item| format!("- {}\n", item.trim()))
        .collect()
}

fn render_structured_summary(card_id: &str, summary: &AgentChatSummary) -> String {
    let current_state = if summary.current_state.trim().is_empty() {
        "unknown"
    } else {
        summary.current_state.trim()
    };
    format!(
        "# Agent Chat Summary: {}\n\n**Confidence:** {}/100\n**Current state:** {}\n\n## Decisions\n{}\n## Files touched\n{}\n## Blockers encountered\n{}\n## Next steps\n{}",
        card_id,
        summary.confidence_score,
        current_state.replace('\n', " "),
        render_items(&summary.decisions),
        render_items(&summary.files_touched),
        render_items(&summary.blockers),
        render_items(&summary.next_steps),
    )
}

pub fn render_summary_response(card_id: &str, raw: &str) -> String {
    match parse_summary_response(raw) {
        Some(summary) => render_structured_summary(card_id, &summary),
        None => {
            let raw = raw.trim();
            if raw.is_empty() {
                render_unavailable(card_id, "empty subchat response")
            } else {
                format!("# Agent Chat Summary: {}\n\n{}", card_id, raw)
            }
        }
    }
}

fn render_unavailable(card_id: &str, error: &str) -> String {
    format!(
        "# Agent Chat Summary: {}\n\nSummary unavailable: {}",
        card_id, error
    )
}

fn render_no_messages(card_id: &str) -> String {
    format!(
        "# Agent Chat Summary: {}\n\nNo messages found for this agent session.",
        card_id
    )
}

fn tool_message(tool_call_id: &String, content: String) -> (bool, Vec<ContextEnum>) {
    (
        false,
        vec![ContextEnum::ChatMessage(ChatMessage {
            role: "tool".to_string(),
            content: ChatContent::SimpleText(content),
            tool_calls: None,
            tool_call_id: tool_call_id.clone(),
            ..Default::default()
        })],
    )
}

async fn load_agent_messages(
    chat_facade: Arc<dyn ChatSessionFacade>,
    chat_id: Option<&str>,
) -> Result<Vec<ChatMessage>, String> {
    let Some(chat_id) = chat_id else {
        return Ok(Vec::new());
    };
    let live_state = chat_facade.session_state(chat_id).await.unwrap_or(None);
    let snapshot = chat_facade.session_snapshot(chat_id).await?;
    if live_state.is_none() && snapshot.messages.is_empty() {
        return Ok(Vec::new());
    }
    Ok(snapshot.messages)
}

async fn run_summary_subchat(
    ccx: Arc<AMutex<AtCommandsContext>>,
    prompt: String,
    source_messages: &[ChatMessage],
    tool_call_id: String,
) -> Result<String, String> {
    let (
        subchat,
        current_model,
        parent_chat_id,
        root_chat_id,
        parent_subchat_tx,
        abort_flag,
        parent_depth,
        task_meta,
        worktree,
    ) = {
        let ccx_lock = ccx.lock().await;
        (
            ccx_lock.app.ctx.subchat.clone(),
            ccx_lock.current_model.clone(),
            ccx_lock.chat_id.clone(),
            ccx_lock.root_chat_id.clone(),
            ccx_lock.subchat_tx.clone(),
            ccx_lock.abort_flag.clone(),
            ccx_lock.subchat_depth,
            ccx_lock.task_meta.clone(),
            ccx_lock.execution_scope_worktree(),
        )
    };

    if parent_depth + 1 >= MAX_SUBCHAT_DEPTH {
        return Err(format!(
            "subchat depth limit ({}) exceeded",
            MAX_SUBCHAT_DEPTH
        ));
    }

    let trace_parent = TraceParent::rooted(&parent_chat_id, &root_chat_id);

    let config = SubchatConfig {
        tool_name: "agent_chat_summary".to_string(),
        stateful: false,
        autonomous_no_confirm: false,
        auto_approve_editing_tools: false,
        auto_approve_dangerous_commands: false,
        chat_id: None,
        title: Some("Agent Chat Summary".to_string()),
        parent_id: Some(parent_chat_id),
        link_type: Some("agent_chat_summary".to_string()),
        root_chat_id: Some(root_chat_id),
        tools: ToolsPolicy::None,
        max_steps: 1,
        prepend_system_prompt: false,
        wrap_up: None,
        task_meta,
        worktree,
        model: current_model,
        mode: "NO_TOOLS".to_string(),
        n_ctx: SUMMARY_N_CTX,
        max_new_tokens: SUMMARY_MAX_NEW_TOKENS,
        temperature: Some(0.0),
        reasoning_effort: None,
        cache_control: refact_core::chat_types::CacheControl::Ephemeral,
        parent_tool_call_id: Some(tool_call_id),
        parent_subchat_tx: Some(parent_subchat_tx),
        abort_flag: Some(abort_flag),
        soft_abort: false,
        activity_stamp: None,
        background_agent_id: None,
        subchat_depth: parent_depth + 1,
        final_step_force_answer: false,
        buddy_meta: None,
        step_progress: None,
        trace_parent,
    };

    let mut messages = vec![ChatMessage::new("user".to_string(), prompt)];
    refact_privacy::carry_records_into(&mut messages[0], source_messages)
        .map_err(|error| error.to_string())?;
    let result = tokio::time::timeout(
        Duration::from_secs(SUMMARY_TIMEOUT_SECS),
        subchat.run_subchat(messages, config),
    )
    .await
    .map_err(|_| format!("timed out after {}s", SUMMARY_TIMEOUT_SECS))??;

    result
        .messages
        .iter()
        .rev()
        .find(|message| message.role == "assistant")
        .map(|message| message.content.content_text_only())
        .filter(|text| !text.trim().is_empty())
        .ok_or_else(|| "subchat returned no assistant summary".to_string())
}

#[async_trait]
impl Tool for ToolAgentChatSummary {
    fn tool_description(&self) -> ToolDesc {
        ToolDesc {
            name: "agent_chat_summary".to_string(),
            display_name: "Agent Chat Summary".to_string(),
            source: ToolSource {
                source_type: ToolSourceType::Builtin,
                config_path: String::new(),
            },
            experimental: false,
            allow_parallel: true,
            description: "Planner-only LLM summary of one task agent transcript with decisions, files touched, blockers, next steps, current state, and confidence.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "card_id": {"type": "string", "description": "Card ID whose agent transcript to summarize"},
                    "focus": {"type": "string", "description": "Optional focus for the audit summary"},
                    "task_id": {"type": "string", "description": "Task ID (optional if chat is bound to a task)"}
                },
                "required": ["card_id"]
            }),
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
        let is_planner = {
            let ccx_lock = ccx.lock().await;
            ccx_lock
                .task_meta
                .as_ref()
                .map(|meta| meta.role == "planner")
                .unwrap_or(false)
        };
        if !is_planner {
            return Err(
                "agent_chat_summary can only be called by the task planner. Switch to the planner chat to audit agent transcripts."
                    .to_string(),
            );
        }

        let card_id = required_string(args, "card_id")?;
        let focus = optional_string(args, "focus");
        let task_id = require_bound_planner_task(&ccx, args).await?;
        let (ctx, chat_facade) = {
            let ccx_lock = ccx.lock().await;
            (ccx_lock.app.ctx.clone(), ccx_lock.app.chat.facade.clone())
        };
        let board = storage::load_board_for_ctx(&ctx, &task_id).await?;
        let card = board
            .get_card(&card_id)
            .ok_or_else(|| format!("Card {} not found", card_id))?;

        let messages = match load_agent_messages(chat_facade, card.agent_chat_id.as_deref()).await {
            Ok(messages) => messages,
            Err(error) => {
                return Ok(tool_message(
                    tool_call_id,
                    render_unavailable(&card_id, &error),
                ))
            }
        };
        if messages.is_empty() {
            return Ok(tool_message(tool_call_id, render_no_messages(&card_id)));
        }

        let transcript = linearize_transcript(&messages);
        let prompt = build_summary_prompt(focus.as_deref(), &transcript);
        let raw_summary =
            match run_summary_subchat(ccx, prompt, &messages, tool_call_id.clone()).await {
                Ok(summary) => summary,
                Err(error) => {
                    return Ok(tool_message(
                        tool_call_id,
                        render_unavailable(&card_id, &error),
                    ))
                }
            };

        Ok(tool_message(
            tool_call_id,
            render_summary_response(&card_id, &raw_summary),
        ))
    }

    fn tool_depends_on(&self) -> Vec<String> {
        vec![]
    }
}
