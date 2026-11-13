use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use tokio::sync::Mutex as AMutex;

use refact_runtime_api::AtCommandsContext;
use refact_core::chat_types::{ChatContent, ChatMessage, ChatToolCall, ChatUsage, ContextEnum};
use refact_runtime_api::task_storage as storage;
use refact_tasks::types::{AbVariantInfo, BoardCard};
use crate::tools::task_tool_helpers::{
    human_age_at, required_string, require_bound_planner_task, truncate_chars,
};
use crate::tools::tools_description::{Tool, ToolDesc, ToolSource, ToolSourceType};
use refact_core::string_utils::redact_sensitive;
use refact_runtime_api::{ChatSessionFacade, ChatSessionSnapshot, SessionState};

const PREVIEW_CHARS: usize = 200;
const VALUE_PREVIEW_CHARS: usize = 80;
const EDITING_TOOL_NAMES: &[&str] = &[
    "patch",
    "apply",
    "write",
    "replace_textdoc",
    "create_textdoc",
    "update_textdoc",
    "update_textdoc_regex",
    "update_textdoc_by_lines",
    "update_textdoc_anchored",
    "apply_patch",
];

pub struct ToolAgentPulse;

impl ToolAgentPulse {
    pub fn new() -> Self {
        Self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LastToolCall {
    preview: String,
    currently_editing: Option<String>,
}

#[derive(Debug, Clone)]
struct AgentPulse {
    card_id: String,
    variant_key: Option<String>,
    card_title: String,
    state: Option<SessionState>,
    card_column: String,
    last_activity_at: Option<DateTime<Utc>>,
    tokens_used: Option<usize>,
    token_cap: Option<usize>,
    latest_usage: Option<ChatUsage>,
    latest_finish_reason: Option<String>,
    currently_editing: Option<String>,
    last_assistant_preview: Option<String>,
    last_tool_call: Option<String>,
    session_note: Option<String>,
}

struct PulseTarget<'a> {
    card: &'a BoardCard,
    variant_key: Option<&'static str>,
    variant: Option<&'a AbVariantInfo>,
}

impl<'a> PulseTarget<'a> {
    fn chat_id(&self) -> Option<&str> {
        self.variant
            .map(|variant| variant.chat_id.as_str())
            .or_else(|| self.card.agent_chat_id.as_deref())
    }
}

fn parse_card_variant_id(input: &str) -> (&str, Option<&str>) {
    if let Some((card_id, variant)) = input.split_once('/') {
        return (card_id, Some(variant));
    }
    if let Some((card_id, variant)) = input.split_once(':') {
        return (card_id, Some(variant));
    }
    (input, None)
}

fn resolve_pulse_target<'a>(
    card: &'a BoardCard,
    requested_id: &str,
) -> Result<PulseTarget<'a>, String> {
    let (_, requested_variant) = parse_card_variant_id(requested_id);
    if let Some(variants) = card.ab_variants.as_ref() {
        let variant_key = match requested_variant {
            Some("a") | Some("A") => Some("a"),
            Some("b") | Some("B") => Some("b"),
            Some(other) => {
                return Err(format!(
                    "Invalid A/B variant '{}', expected 'a' or 'b'",
                    other
                ));
            }
            None if card.agent_chat_id.is_none() => Some("a"),
            None => None,
        };
        if let Some(variant_key) = variant_key {
            let variant = variants
                .variant(variant_key)
                .ok_or_else(|| format!("A/B variant {} not found", variant_key))?;
            return Ok(PulseTarget {
                card,
                variant_key: Some(variant_key),
                variant: Some(variant),
            });
        }
    }
    Ok(PulseTarget {
        card,
        variant_key: None,
        variant: None,
    })
}

async fn fetch_session_snapshot(
    chat_facade: Arc<dyn ChatSessionFacade>,
    chat_id: Option<&str>,
) -> (
    Option<ChatSessionSnapshot>,
    Option<SessionState>,
    Option<String>,
) {
    let Some(chat_id) = chat_id else {
        return (
            None,
            None,
            Some("Card has no agent chat session; showing board state only.".to_string()),
        );
    };

    let live_state = chat_facade.session_state(chat_id).await.unwrap_or(None);
    match chat_facade.session_snapshot(chat_id).await {
        Ok(snapshot) => {
            if live_state.is_none() && snapshot.messages.is_empty() {
                (
                    Some(snapshot),
                    None,
                    Some(
                        "Session snapshot unavailable; showing last known board state.".to_string(),
                    ),
                )
            } else {
                let state = live_state.or(Some(snapshot.session_state));
                (Some(snapshot), state, None)
            }
        }
        Err(_) => (
            None,
            live_state,
            Some("Session snapshot unavailable; showing last known board state.".to_string()),
        ),
    }
}

fn build_agent_pulse(
    target: &PulseTarget<'_>,
    snapshot: Option<&ChatSessionSnapshot>,
    session_state: Option<SessionState>,
    session_note: Option<String>,
    fallback_token_cap: usize,
    live_last_activity: Option<DateTime<Utc>>,
) -> AgentPulse {
    let card = target.card;
    let messages = snapshot
        .map(|snapshot| snapshot.messages.as_slice())
        .unwrap_or(&[]);
    let last_tool = last_tool_call_from_messages(messages);
    let token_cap = snapshot
        .and_then(|snapshot| snapshot.thread.context_tokens_cap)
        .or_else(|| (fallback_token_cap > 0).then_some(fallback_token_cap));
    let latest_assistant = messages
        .iter()
        .rev()
        .find(|message| message.role == "assistant" && !is_segment_summary_message(message));
    let latest_usage = latest_assistant
        .and_then(|message| message.usage.as_ref())
        .cloned();
    let latest_finish_reason = latest_assistant.and_then(|message| message.finish_reason.clone());

    AgentPulse {
        card_id: target
            .variant_key
            .map(|variant| format!("{}/{}", card.id, variant))
            .unwrap_or_else(|| card.id.clone()),
        variant_key: target.variant_key.map(str::to_string),
        card_title: target
            .variant_key
            .map(|variant| format!("{} [A/B {}]", card.title, variant.to_ascii_uppercase()))
            .unwrap_or_else(|| card.title.clone()),
        state: session_state,
        card_column: target
            .variant
            .and_then(|variant| variant.finish.as_ref())
            .map(|finish| if finish.success { "done" } else { "failed" }.to_string())
            .unwrap_or_else(|| card.column.clone()),
        last_activity_at: live_last_activity.or_else(|| last_activity_timestamp(card)),
        tokens_used: tokens_used(messages),
        token_cap,
        latest_usage,
        latest_finish_reason,
        currently_editing: last_tool
            .as_ref()
            .and_then(|tool_call| tool_call.currently_editing.clone()),
        last_assistant_preview: last_assistant_preview(messages),
        last_tool_call: last_tool.map(|tool_call| tool_call.preview),
        session_note,
    }
}

fn render_agent_pulse(pulse: &AgentPulse) -> String {
    render_agent_pulse_at(pulse, Utc::now())
}

fn render_agent_pulse_at(pulse: &AgentPulse, now: DateTime<Utc>) -> String {
    let last_activity = pulse
        .last_activity_at
        .map(|timestamp| human_age_at(timestamp, now))
        .unwrap_or_else(|| "unknown".to_string());
    let currently_editing = pulse.currently_editing.as_deref().unwrap_or("unknown");
    let assistant = pulse.last_assistant_preview.as_deref().unwrap_or("(none)");
    let mut result = format!(
        "# Agent Pulse: {}\n\n**Card:** {}\n**State:** {}\n**Last activity:** {}\n**Tokens used:** {}\n**Last finish reason:** {}\n**Latest response usage:** {}\n**Currently editing:** {}\n",
        pulse.card_id,
        pulse.card_title,
        format_session_state(pulse.state, &pulse.card_column),
        last_activity,
        format_tokens(pulse.tokens_used, pulse.token_cap),
        pulse.latest_finish_reason.as_deref().unwrap_or("unknown"),
        format_usage(pulse.latest_usage.as_ref()),
        currently_editing
    );

    if let Some(note) = &pulse.session_note {
        result.push_str("\n");
        result.push_str(note);
        result.push_str("\n");
    }
    if let Some(variant_key) = pulse.variant_key.as_deref() {
        result.push_str("\n**A/B variant:** ");
        result.push_str(&variant_key.to_ascii_uppercase());
        result.push_str("\n");
    }

    result.push_str("\n## Last assistant message\n> ");
    result.push_str(&markdown_quote_text(assistant));
    result.push_str("\n\n## Last tool call\n");
    match pulse.last_tool_call.as_deref() {
        Some(tool_call) => {
            result.push('`');
            result.push_str(&inline_code_text(tool_call));
            result.push_str("`\n");
        }
        None => result.push_str("(none)\n"),
    }
    result
}

fn format_session_state(state: Option<SessionState>, card_column: &str) -> String {
    match state {
        Some(SessionState::Starting) => "⏳ starting".to_string(),
        Some(SessionState::Idle) => "💤 idle".to_string(),
        Some(SessionState::Generating) => "🔄 generating response".to_string(),
        Some(SessionState::ExecutingTools) => "⚙️ executing tools".to_string(),
        Some(SessionState::Paused) => "⏸️ paused".to_string(),
        Some(SessionState::WaitingIde) => "⏳ waiting for IDE".to_string(),
        Some(SessionState::WaitingUserInput) => "⏳ waiting for user input".to_string(),
        Some(SessionState::Completed) => "✅ completed".to_string(),
        Some(SessionState::Error) => "❌ error".to_string(),
        None => format!("❓ last known: {}", card_column),
    }
}

fn format_tokens(tokens_used: Option<usize>, token_cap: Option<usize>) -> String {
    match (tokens_used, token_cap) {
        (Some(used), Some(cap)) => format!(
            "~{} / {}",
            format_token_count(used),
            format_token_count(cap)
        ),
        (Some(used), None) => format!("~{}", format_token_count(used)),
        (None, Some(cap)) => format!("unknown / {}", format_token_count(cap)),
        (None, None) => "unknown".to_string(),
    }
}

fn format_token_count(value: usize) -> String {
    if value < 1_000 {
        return value.to_string();
    }
    if value % 1_000 == 0 {
        return format!("{}k", value / 1_000);
    }
    let mut text = format!("{:.1}k", value as f64 / 1_000.0);
    if text.ends_with(".0k") {
        text = format!("{}k", value / 1_000);
    }
    text
}

fn format_usage(usage: Option<&ChatUsage>) -> String {
    usage
        .map(|usage| {
            let mut parts = vec![
                format!("prompt={}", format_token_count(usage.prompt_tokens)),
                format!("completion={}", format_token_count(usage.completion_tokens)),
                format!("total={}", format_token_count(usage.total_tokens)),
            ];
            if let Some(cache_read) = usage.cache_read_tokens {
                parts.push(format!("cache_read={}", format_token_count(cache_read)));
            }
            if let Some(cache_creation) = usage.cache_creation_tokens {
                parts.push(format!(
                    "cache_creation={}",
                    format_token_count(cache_creation)
                ));
            }
            parts.join(", ")
        })
        .unwrap_or_else(|| "unknown".to_string())
}

fn last_tool_call_from_messages(messages: &[ChatMessage]) -> Option<LastToolCall> {
    messages.iter().rev().find_map(|message| {
        message
            .tool_calls
            .as_ref()
            .and_then(|tool_calls| tool_calls.last())
            .map(|tool_call| LastToolCall {
                preview: format_tool_call(tool_call),
                currently_editing: currently_editing_from_tool_call(tool_call),
            })
    })
}

fn format_tool_call(tool_call: &ChatToolCall) -> String {
    let args_preview = format_tool_args(&tool_call.function.arguments);
    if args_preview.is_empty() {
        format!("{}()", tool_call.function.name)
    } else {
        format!("{}({})", tool_call.function.name, args_preview)
    }
}

fn format_tool_args(raw: &str) -> String {
    let raw = raw.trim();
    if raw.is_empty() {
        return String::new();
    }
    if let Ok(Value::Object(map)) = serde_json::from_str::<Value>(raw) {
        let mut keys = map.keys().cloned().collect::<Vec<_>>();
        keys.sort_by(|a, b| arg_key_rank(a).cmp(&arg_key_rank(b)).then_with(|| a.cmp(b)));
        let parts = keys
            .into_iter()
            .filter_map(|key| {
                map.get(&key).map(|value| {
                    format!(
                        "{}={}",
                        key,
                        sanitize_arg_value(&key, value, VALUE_PREVIEW_CHARS)
                    )
                })
            })
            .collect::<Vec<_>>();
        return truncate_chars(&parts.join(", "), PREVIEW_CHARS);
    }
    sanitize_preview(raw, PREVIEW_CHARS)
}

fn arg_key_rank(key: &str) -> u8 {
    match key {
        "path" => 0,
        "file_path" => 1,
        "filename" => 2,
        _ => 3,
    }
}

fn sanitize_arg_value(key: &str, value: &Value, limit: usize) -> String {
    let raw = match value {
        Value::String(text) => text.clone(),
        _ => serde_json::to_string(value).unwrap_or_default(),
    };
    let collapsed = collapse_whitespace(&raw);
    let sanitized = if is_path_key(key) {
        collapsed
    } else {
        redact_sensitive(&collapsed)
    };
    truncate_chars(&sanitized, limit)
}

fn is_path_key(key: &str) -> bool {
    matches!(key, "path" | "file_path" | "filename")
}

fn currently_editing_from_tool_call(tool_call: &ChatToolCall) -> Option<String> {
    if !EDITING_TOOL_NAMES.contains(&tool_call.function.name.as_str()) {
        return None;
    }
    let Value::Object(map) = serde_json::from_str::<Value>(&tool_call.function.arguments).ok()?
    else {
        return None;
    };
    ["path", "file_path", "filename"]
        .iter()
        .find_map(|key| map.get(*key).and_then(|value| value.as_str()))
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .map(str::to_string)
}

/// Segment summaries are assistant-role compression artifacts, not agent
/// output; pulses must report the agent's own latest message and usage.
fn is_segment_summary_message(message: &ChatMessage) -> bool {
    message
        .extra
        .get("compression")
        .and_then(|compression| compression.get("kind"))
        .and_then(|kind| kind.as_str())
        == Some("llm_segment_summary")
}

fn last_assistant_preview(messages: &[ChatMessage]) -> Option<String> {
    messages
        .iter()
        .rev()
        .filter(|message| message.role == "assistant" && !is_segment_summary_message(message))
        .find_map(|message| {
            let text = message.content.content_text_only();
            let preview = sanitize_preview(&text, PREVIEW_CHARS);
            (!preview.is_empty()).then_some(preview)
        })
}

fn tokens_used(messages: &[ChatMessage]) -> Option<usize> {
    let mut total = 0usize;
    let mut found = false;
    for message in messages
        .iter()
        .filter(|message| message.role == "assistant")
    {
        if let Some(usage) = &message.usage {
            found = true;
            let fallback_total = usage.prompt_tokens.saturating_add(usage.completion_tokens);
            total = total.saturating_add(usage.total_tokens.max(fallback_total));
        }
    }
    found.then_some(total)
}

fn sanitize_preview(text: &str, limit: usize) -> String {
    let redacted = redact_sensitive(text);
    let collapsed = collapse_whitespace(&redacted);
    truncate_chars(&collapsed, limit).trim().to_string()
}

fn collapse_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn markdown_quote_text(text: &str) -> String {
    text.replace('\n', "\n> ")
}

fn inline_code_text(text: &str) -> String {
    text.replace('`', "'")
}

fn parse_timestamp(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|timestamp| timestamp.with_timezone(&Utc))
}

fn last_activity_timestamp(card: &BoardCard) -> Option<DateTime<Utc>> {
    let last_status_update_at = card
        .status_updates
        .last()
        .and_then(|update| parse_timestamp(&update.timestamp));
    latest_timestamp([
        card.last_heartbeat_at.as_deref().and_then(parse_timestamp),
        last_status_update_at,
        card.completed_at.as_deref().and_then(parse_timestamp),
        card.started_at.as_deref().and_then(parse_timestamp),
        parse_timestamp(&card.created_at),
    ])
}

fn latest_timestamp(
    times: impl IntoIterator<Item = Option<DateTime<Utc>>>,
) -> Option<DateTime<Utc>> {
    times.into_iter().flatten().max()
}

#[async_trait]
impl Tool for ToolAgentPulse {
    fn tool_description(&self) -> ToolDesc {
        ToolDesc {
            name: "agent_pulse".to_string(),
            display_name: "Agent Pulse".to_string(),
            source: ToolSource {
                source_type: ToolSourceType::Builtin,
                config_path: String::new(),
            },
            experimental: false,
            allow_parallel: true,
            description: "Show a live planner-only pulse for one task agent: state, activity age, token usage, current edit target, last assistant preview, and last tool call. For A/B variants, pass card_id as T-1/a or T-1/b.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "card_id": {"type": "string", "description": "Card ID whose agent pulse to inspect"},
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
                "agent_pulse can only be called by the task planner. Switch to the planner chat to inspect agent pulse."
                    .to_string(),
            );
        }

        let requested_card_id = required_string(args, "card_id")?;
        let (card_id, _) = parse_card_variant_id(&requested_card_id);
        let task_id = require_bound_planner_task(&ccx, args).await?;
        let (ctx, chat_facade, sessions, fallback_token_cap) = {
            let ccx_lock = ccx.lock().await;
            (
                ccx_lock.app.ctx.clone(),
                ccx_lock.app.chat.facade.clone(),
                ccx_lock.app.chat.sessions.clone(),
                ccx_lock.n_ctx,
            )
        };
        let board = storage::load_board_for_ctx(&ctx, &task_id).await?;
        let card = board
            .get_card(card_id)
            .ok_or_else(|| format!("Card {} not found", card_id))?;
        let target = resolve_pulse_target(card, &requested_card_id)?;
        let chat_id = target.chat_id();
        let live_last_activity = if let Some(chat_id) = chat_id {
            let session_arc = sessions.read().await.get(chat_id).cloned();
            if let Some(session_arc) = session_arc {
                let session = session_arc.lock().await;
                let elapsed = session.last_activity.elapsed();
                drop(session);
                chrono::Duration::from_std(elapsed)
                    .ok()
                    .map(|d| Utc::now() - d)
            } else {
                None
            }
        } else {
            None
        };
        let (snapshot, session_state, session_note) =
            fetch_session_snapshot(chat_facade, chat_id).await;
        let pulse = build_agent_pulse(
            &target,
            snapshot.as_ref(),
            session_state,
            session_note,
            fallback_token_cap,
            live_last_activity,
        );
        let result = render_agent_pulse(&pulse);

        Ok((
            false,
            vec![ContextEnum::ChatMessage(ChatMessage {
                role: "tool".to_string(),
                content: ChatContent::SimpleText(result),
                tool_calls: None,
                tool_call_id: tool_call_id.clone(),
                ..Default::default()
            })],
        ))
    }

    fn tool_depends_on(&self) -> Vec<String> {
        vec![]
    }
}
