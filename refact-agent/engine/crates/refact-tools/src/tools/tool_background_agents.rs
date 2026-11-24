use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use tokio::sync::Mutex as AMutex;

use refact_agents_core::types::{AgentListFilter, BackgroundAgent, BgAgentKind, BgAgentStatus};
use refact_runtime_api::AtCommandsContext;
use refact_core::chat_types::{ChatContent, ChatMessage, ContextEnum};
use refact_postprocessing::pp_command_output::OutputFilter;
use crate::tools::tools_description::{Tool, ToolDesc, ToolSource, ToolSourceType};

const DEFAULT_LIMIT: usize = 20;
const DEFAULT_TERMINAL_HOURS: i64 = 24;
const DEFAULT_WAIT_MS: u64 = 60_000;
const MIN_WAIT_MS: u64 = 1_000;
const MAX_WAIT_MS: u64 = 30 * 60 * 1_000;

pub struct ToolAgentList {
    pub config_path: String,
}

pub struct ToolAgentStatus {
    pub config_path: String,
}

pub struct ToolAgentWait {
    pub config_path: String,
}

pub struct ToolAgentResult {
    pub config_path: String,
}

pub struct ToolAgentCancel {
    pub config_path: String,
}

fn background_agent_tool_desc(
    config_path: String,
    name: &str,
    display_name: &str,
    description: &str,
    input_schema: Value,
) -> ToolDesc {
    ToolDesc {
        name: name.to_string(),
        display_name: display_name.to_string(),
        source: ToolSource {
            source_type: ToolSourceType::Builtin,
            config_path,
        },
        experimental: false,
        allow_parallel: true,
        description: description.to_string(),
        input_schema,
        output_schema: None,
        annotations: None,
    }
}

fn agent_list_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "status": {
                "type": "string",
                "enum": ["queued", "running", "waiting_for_approval", "completed", "failed", "cancelled", "interrupted", "all"],
                "description": "Only include agents with this status. Default: all"
            },
            "kind": {
                "type": "string",
                "enum": ["subagent", "delegate", "all"],
                "description": "Only include this kind of background agent. Default: all"
            },
            "include_terminal_within_hours": {
                "type": "integer",
                "minimum": 0,
                "description": "Include terminal agents finished within this many hours. Default: 24"
            },
            "limit": {
                "type": "integer",
                "minimum": 0,
                "description": "Maximum rows to return. Default: 20"
            }
        },
        "required": []
    })
}

fn agent_id_schema(extra: serde_json::Map<String, Value>) -> Value {
    let mut properties = serde_json::Map::new();
    properties.insert(
        "agent_id".to_string(),
        json!({
            "type": "string",
            "description": "Background agent ID, for example bgagent-..."
        }),
    );
    properties.extend(extra);
    json!({
        "type": "object",
        "properties": properties,
        "required": ["agent_id"]
    })
}

fn agent_status_schema() -> Value {
    agent_id_schema(serde_json::Map::new())
}

fn agent_wait_schema() -> Value {
    agent_id_schema(serde_json::Map::from_iter([(
        "timeout_ms".to_string(),
        json!({
            "type": "integer",
            "minimum": MIN_WAIT_MS,
            "maximum": MAX_WAIT_MS,
            "description": "How long to wait for completion. Default: 60000"
        }),
    )]))
}

fn agent_result_schema() -> Value {
    agent_id_schema(serde_json::Map::from_iter([(
        "include_details".to_string(),
        json!({
            "type": "boolean",
            "description": "Include expanded result details and payload. Default: false"
        }),
    )]))
}

fn agent_cancel_schema() -> Value {
    agent_id_schema(serde_json::Map::from_iter([
        (
            "reason".to_string(),
            json!({
                "type": "string",
                "description": "Optional cancellation reason"
            }),
        ),
        (
            "subtree".to_string(),
            json!({
                "type": "boolean",
                "default": true,
                "description": "Cancel descendants too. Default: true"
            }),
        ),
    ]))
}

fn parse_required_string(args: &HashMap<String, Value>, key: &str) -> Result<String, String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| format!("Missing '{key}'"))
}

fn parse_optional_string(args: &HashMap<String, Value>, key: &str) -> Option<String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn parse_optional_bool(
    args: &HashMap<String, Value>,
    key: &str,
    default: bool,
) -> Result<bool, String> {
    match args.get(key) {
        Some(Value::Null) | None => Ok(default),
        Some(value) => refact_tool_api::coerce_bool(value)
            .ok_or_else(|| format!("{key} must be a boolean, got {value}")),
    }
}

fn parse_optional_u64(
    args: &HashMap<String, Value>,
    key: &str,
    default: u64,
) -> Result<u64, String> {
    match args.get(key) {
        Some(Value::Null) | None => Ok(default),
        Some(value) => value
            .as_u64()
            .ok_or_else(|| format!("{key} must be a non-negative integer")),
    }
}

fn parse_optional_i64(
    args: &HashMap<String, Value>,
    key: &str,
    default: i64,
) -> Result<i64, String> {
    match args.get(key) {
        Some(Value::Null) | None => Ok(default),
        Some(value) => {
            let number = value
                .as_i64()
                .ok_or_else(|| format!("{key} must be a non-negative integer"))?;
            if number < 0 {
                return Err(format!("{key} must be a non-negative integer"));
            }
            Ok(number)
        }
    }
}

fn parse_optional_usize(
    args: &HashMap<String, Value>,
    key: &str,
    default: usize,
) -> Result<usize, String> {
    let value = parse_optional_u64(args, key, default as u64)?;
    usize::try_from(value).map_err(|_| format!("{key} is too large"))
}

fn parse_wait_timeout_ms(args: &HashMap<String, Value>) -> Result<u64, String> {
    let timeout_ms = parse_optional_u64(args, "timeout_ms", DEFAULT_WAIT_MS)?;
    if timeout_ms < MIN_WAIT_MS {
        return Err(format!("timeout_ms must be at least {MIN_WAIT_MS}ms"));
    }
    if timeout_ms > MAX_WAIT_MS {
        return Err(format!("timeout_ms must be at most {MAX_WAIT_MS}ms"));
    }
    Ok(timeout_ms)
}

fn parse_optional_enum<'a>(
    args: &'a HashMap<String, Value>,
    key: &str,
    default: &'a str,
) -> Result<&'a str, String> {
    match args.get(key) {
        Some(Value::Null) | None => Ok(default),
        Some(value) => value
            .as_str()
            .ok_or_else(|| format!("{key} must be a string")),
    }
}

fn parse_kind(value: &str) -> Result<Option<BgAgentKind>, String> {
    match value {
        "all" => Ok(None),
        "subagent" => Ok(Some(BgAgentKind::Subagent)),
        "delegate" => Ok(Some(BgAgentKind::Delegate)),
        other => Err(format!("Invalid kind: {other}")),
    }
}

fn parse_status(value: &str) -> Result<Option<Vec<BgAgentStatus>>, String> {
    match value {
        "all" => Ok(None),
        "queued" => Ok(Some(vec![BgAgentStatus::Queued])),
        "running" => Ok(Some(vec![BgAgentStatus::Running])),
        "waiting_for_approval" => Ok(Some(vec![BgAgentStatus::WaitingForApproval])),
        "completed" => Ok(Some(vec![BgAgentStatus::Completed])),
        "failed" => Ok(Some(vec![BgAgentStatus::Failed])),
        "cancelled" => Ok(Some(vec![BgAgentStatus::Cancelled])),
        "interrupted" => Ok(Some(vec![BgAgentStatus::Interrupted])),
        other => Err(format!("Invalid status: {other}")),
    }
}

fn parse_agent_list_filter(args: &HashMap<String, Value>) -> Result<AgentListFilter, String> {
    Ok(AgentListFilter {
        status: parse_status(parse_optional_enum(args, "status", "all")?)?,
        kind: parse_kind(parse_optional_enum(args, "kind", "all")?)?,
        include_terminal_within_hours: Some(parse_optional_i64(
            args,
            "include_terminal_within_hours",
            DEFAULT_TERMINAL_HOURS,
        )?),
        limit: Some(parse_optional_usize(args, "limit", DEFAULT_LIMIT)?),
    })
}

fn get_parent_chat_id(ccx_lock: &AtCommandsContext) -> String {
    ccx_lock.chat_id.clone()
}

async fn background_agent_context(
    ccx: &Arc<AMutex<AtCommandsContext>>,
) -> (refact_runtime_api::AppState, String) {
    let ccx_lock = ccx.lock().await;
    (ccx_lock.app.clone(), get_parent_chat_id(&ccx_lock))
}

pub fn not_found_error(agent_id: &str) -> String {
    format!("Background agent '{agent_id}' not found in this chat.")
}

fn map_registry_error(agent_id: &str, error: String) -> String {
    if error == "agent not found" {
        not_found_error(agent_id)
    } else {
        error
    }
}

fn tool_message(tool_call_id: &str, content: String) -> (bool, Vec<ContextEnum>) {
    (
        false,
        vec![ContextEnum::ChatMessage(ChatMessage {
            role: "tool".to_string(),
            content: ChatContent::SimpleText(content),
            tool_calls: None,
            tool_call_id: tool_call_id.to_string(),
            preserve: Some(true),
            output_filter: Some(OutputFilter::no_limits()),
            ..Default::default()
        })],
    )
}

fn truncate_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    if max == 0 {
        return String::new();
    }
    format!(
        "{}…",
        text.chars().take(max.saturating_sub(1)).collect::<String>()
    )
}

fn markdown_cell(text: &str, max: usize) -> String {
    truncate_chars(&text.replace('\n', " ").replace('|', "\\|"), max)
}

pub fn kind_label(kind: BgAgentKind) -> &'static str {
    match kind {
        BgAgentKind::Subagent => "subagent",
        BgAgentKind::Delegate => "delegate",
        BgAgentKind::Tool => "tool",
    }
}

fn kind_title(kind: BgAgentKind) -> &'static str {
    match kind {
        BgAgentKind::Subagent => "Subagent",
        BgAgentKind::Delegate => "Delegate",
        BgAgentKind::Tool => "Tool",
    }
}

fn format_status_chip(status: BgAgentStatus) -> &'static str {
    match status {
        BgAgentStatus::Queued => "⏳ queued",
        BgAgentStatus::Running => "🟢 running",
        BgAgentStatus::WaitingForApproval => "✋ waiting_for_approval",
        BgAgentStatus::Completed => "✅ completed",
        BgAgentStatus::Failed => "❌ failed",
        BgAgentStatus::Cancelled => "⏹ cancelled",
        BgAgentStatus::Interrupted => "⚠ interrupted",
    }
}

pub fn human_age(then: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let seconds = now.signed_duration_since(then).num_seconds().max(0);
    if seconds == 0 {
        "now".to_string()
    } else if seconds < 60 {
        "0m".to_string()
    } else if seconds < 60 * 60 {
        format!("{}m", seconds / 60)
    } else if seconds < 60 * 60 * 24 {
        format!("{}h", seconds / (60 * 60))
    } else {
        format!("{}d", seconds / (60 * 60 * 24))
    }
}

fn human_age_ago(then: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let age = human_age(then, now);
    if age == "now" {
        age
    } else {
        format!("{age} ago")
    }
}

fn format_agent_last_activity(record: &BackgroundAgent) -> String {
    if record.status == BgAgentStatus::Failed {
        if let Some(error) = &record.error {
            return format!("error: {}", truncate_chars(error, 80));
        }
    }
    if record.status.is_terminal() {
        return "(finished)".to_string();
    }
    record
        .last_activity
        .clone()
        .unwrap_or_else(|| "-".to_string())
}

fn format_agent_table(rows: &[BackgroundAgent]) -> String {
    let now = Utc::now();
    let mut result = String::from("# Background Agents\n\n");
    result.push_str("| ID | Kind | Status | Title | Tool | Tokens | Age | Last activity |\n");
    result.push_str("|---|---|---|---|---|---|---|---|\n");
    for row in rows {
        result.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} | {} | {} |\n",
            markdown_cell(&row.agent_id, 80),
            kind_label(row.kind),
            format_status_chip(row.status),
            markdown_cell(&row.title, 80),
            markdown_cell(row.current_tool.as_deref().unwrap_or("-"), 60),
            row.tokens_used,
            human_age(row.created_at, now),
            markdown_cell(&format_agent_last_activity(row), 80),
        ));
    }
    result.push('\n');
    result.push_str(&format_agent_counts(rows));
    result.push_str(
        "\nUse `agent_status(agent_id=...)`, `agent_wait(...)`, or `agent_result(...)` to follow up.",
    );
    result
}

fn format_agent_counts(rows: &[BackgroundAgent]) -> String {
    if rows.is_empty() {
        return "Total: 0.".to_string();
    }
    let statuses = [
        BgAgentStatus::Queued,
        BgAgentStatus::Running,
        BgAgentStatus::WaitingForApproval,
        BgAgentStatus::Completed,
        BgAgentStatus::Failed,
        BgAgentStatus::Cancelled,
        BgAgentStatus::Interrupted,
    ];
    let counts = statuses
        .into_iter()
        .filter_map(|status| {
            let count = rows.iter().filter(|row| row.status == status).count();
            (count > 0).then(|| format!("{}: {count}", status.as_str()))
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!("Total: {} ({}).", rows.len(), counts)
}

fn format_optional_line(result: &mut String, label: &str, value: Option<&str>) {
    if let Some(value) = value {
        if !value.trim().is_empty() {
            result.push_str(&format!("- {label}: {}\n", value.trim()));
        }
    }
}

fn format_list_line(result: &mut String, label: &str, values: &[String]) {
    if !values.is_empty() {
        result.push_str(&format!("- {label}: {}\n", values.join(", ")));
    }
}

fn format_agent_status_block(record: &BackgroundAgent) -> String {
    let now = Utc::now();
    let mut result = format!("# {}: {}\n", kind_title(record.kind), record.title);
    result.push_str(&format!(
        "- Status: {}\n",
        format_status_chip(record.status)
    ));
    result.push_str(&format!("- Agent ID: {}\n", record.agent_id));
    result.push_str(&format!("- Kind: {}\n", kind_label(record.kind)));
    let started = record
        .started_at
        .map(|started_at| human_age_ago(started_at, now))
        .unwrap_or_else(|| "not started".to_string());
    result.push_str(&format!("- Started: {started}\n"));
    result.push_str(&format!("- Step count: {}\n", record.step_count));
    format_optional_line(&mut result, "Current tool", record.current_tool.as_deref());
    format_optional_line(&mut result, "Model type", record.model_type.as_deref());
    result.push_str(&format!("- Model: {}\n", record.model));
    format_optional_line(
        &mut result,
        "Worktree branch",
        record.worktree_branch.as_deref(),
    );
    format_optional_line(&mut result, "Merge status", record.merge_status.as_deref());
    format_optional_line(&mut result, "Goal", record.goal_summary.as_deref());
    result.push_str(&format!("- Tokens: {}\n", record.tokens_used));
    if let Some(cost) = record.cost_usd {
        result.push_str(&format!("- Cost: ${cost:.4}\n"));
    }
    if !record.questions.is_empty() {
        let questions = record
            .questions
            .iter()
            .map(|question| match &question.answer {
                Some(answer) => {
                    format!("{}: answered ({})", question.id, truncate_chars(answer, 80))
                }
                None => format!(
                    "{}: pending ({})",
                    question.id,
                    truncate_chars(&question.text, 80)
                ),
            })
            .collect::<Vec<_>>();
        format_list_line(&mut result, "Questions", &questions);
    }
    result.push_str(&format!(
        "- Last activity: {}\n",
        record.last_activity.as_deref().unwrap_or("-")
    ));
    format_optional_line(&mut result, "Progress", record.progress.as_deref());
    if let Some(child_chat_id) = &record.child_chat_id {
        result.push_str(&format!(
            "\nChild trajectory: [view](refact://chat/{child_chat_id})\n"
        ));
    }
    if record.status.is_terminal() {
        format_list_line(&mut result, "Edited files", &record.edited_files);
        format_optional_line(&mut result, "Diff summary", record.diff_summary.as_deref());
        format_optional_line(
            &mut result,
            "Conflict summary",
            record.conflict_summary.as_deref(),
        );
        format_optional_line(&mut result, "Error", record.error.as_deref());
    } else {
        result.push_str("\n(Pull final result with agent_result after completion.)\n");
    }
    result
}

pub async fn format_agent_result(
    record: &BackgroundAgent,
    include_details: bool,
) -> Result<String, String> {
    if !record.status.is_terminal() {
        return Ok(format!(
            "Agent has not finished yet. Current status: {}. Use agent_wait to block.",
            format_status_chip(record.status)
        ));
    }

    let wants_payload = include_details
        || record
            .result_summary
            .as_deref()
            .map_or(true, |summary| summary.trim().is_empty());
    let mut payload_error = None;
    let payload = if wants_payload {
        match load_result_payload(record).await {
            Ok(payload) => payload,
            Err(error) => {
                payload_error = Some(error);
                None
            }
        }
    } else {
        None
    };
    let summary = record
        .result_summary
        .as_deref()
        .filter(|summary| !summary.trim().is_empty())
        .map(str::to_string)
        .or_else(|| {
            payload
                .as_ref()
                .and_then(|(_, value)| value.as_ref().and_then(format_payload_fallback))
        })
        .or_else(|| {
            payload
                .as_ref()
                .map(|(payload_text, _)| payload_text.clone())
        })
        .or_else(|| {
            record
                .error
                .as_deref()
                .filter(|error| !error.trim().is_empty())
                .map(str::to_string)
        })
        .unwrap_or_else(|| format!("Agent finished with status {}.", record.status.as_str()));
    let mut result = format!("# Agent Result: {}\n", record.title);
    result.push_str(&format!(
        "- Status: {}\n",
        format_status_chip(record.status)
    ));
    result.push_str(&format!("- Agent ID: {}\n", record.agent_id));
    result.push_str(&format!("- Kind: {}\n", kind_label(record.kind)));
    format_optional_line(&mut result, "Current tool", record.current_tool.as_deref());
    result.push_str(&format!("- Model: {}\n", record.model));
    format_optional_line(&mut result, "Model type", record.model_type.as_deref());
    format_optional_line(
        &mut result,
        "Worktree branch",
        record.worktree_branch.as_deref(),
    );
    format_optional_line(&mut result, "Merge status", record.merge_status.as_deref());
    format_optional_line(&mut result, "Goal", record.goal_summary.as_deref());
    result.push_str(&format!("- Tokens: {}\n", record.tokens_used));
    if let Some(cost) = record.cost_usd {
        result.push_str(&format!("- Cost: ${cost:.4}\n"));
    }
    if !record.questions.is_empty() {
        let questions = record
            .questions
            .iter()
            .map(|question| match &question.answer {
                Some(answer) => {
                    format!("{}: answered ({})", question.id, truncate_chars(answer, 80))
                }
                None => format!(
                    "{}: pending ({})",
                    question.id,
                    truncate_chars(&question.text, 80)
                ),
            })
            .collect::<Vec<_>>();
        format_list_line(&mut result, "Questions", &questions);
    }
    if let Some(child_chat_id) = &record.child_chat_id {
        result.push_str(&format!(
            "- Child trajectory: [view](refact://chat/{child_chat_id})\n"
        ));
    }
    result.push_str("\n");
    result.push_str(&summary);
    result.push('\n');
    format_list_line(&mut result, "Edited files", &record.edited_files);
    format_optional_line(&mut result, "Diff summary", record.diff_summary.as_deref());
    format_optional_line(
        &mut result,
        "Conflict summary",
        record.conflict_summary.as_deref(),
    );
    format_optional_line(&mut result, "Error", record.error.as_deref());
    format_optional_line(&mut result, "Result payload", payload_error.as_deref());

    if include_details {
        if let Some((payload_text, _)) = payload {
            result.push_str("\n## Details\n");
            result.push_str(&payload_text);
            result.push('\n');
        } else if let Some(payload_error) = payload_error {
            result.push_str("\n## Details\n");
            result.push_str(&payload_error);
            result.push('\n');
        }
    }

    Ok(result)
}

async fn load_result_payload(
    record: &BackgroundAgent,
) -> Result<Option<(String, Option<Value>)>, String> {
    let Some(path) = &record.result_payload_path else {
        return Ok(None);
    };
    let payload_text = tokio::fs::read_to_string(path).await.map_err(|error| {
        format!(
            "Failed to read background agent result payload {}: {error}",
            path.display()
        )
    })?;
    let payload = serde_json::from_str(&payload_text).ok();
    Ok(Some((payload_text, payload)))
}

fn format_payload_fallback(payload: &Value) -> Option<String> {
    let mut lines = Vec::new();
    if let Some(summary) = payload_string(payload, "result_summary") {
        lines.push(summary);
    }
    if let Some(status) = payload_string(payload, "status") {
        lines.push(format!("Status: {status}"));
    }
    if let Some(error) = payload_string(payload, "error") {
        lines.push(format!("Error: {error}"));
    }
    if let Some(files) = payload_string_list(payload, "edited_files") {
        lines.push(format!("Edited files: {}", files.join(", ")));
    }
    if let Some(diff_summary) = payload_string(payload, "diff_summary") {
        lines.push(format!("Diff summary: {diff_summary}"));
    }
    if let Some(conflict_summary) = payload_string(payload, "conflict_summary") {
        lines.push(format!("Conflict summary: {conflict_summary}"));
    }
    if let Some(child_chat_id) = payload_string(payload, "child_chat_id") {
        lines.push(format!("Child trajectory: refact://chat/{child_chat_id}"));
    }
    if lines.is_empty() {
        serde_json::to_string_pretty(payload).ok()
    } else {
        Some(lines.join("\n"))
    }
}

fn payload_string(payload: &Value, key: &str) -> Option<String> {
    payload
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn payload_string_list(payload: &Value, key: &str) -> Option<Vec<String>> {
    let values = payload.get(key)?.as_array()?;
    let values = values
        .iter()
        .filter_map(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    (!values.is_empty()).then_some(values)
}

#[async_trait]
impl Tool for ToolAgentList {
    fn tool_description(&self) -> ToolDesc {
        background_agent_tool_desc(
            self.config_path.clone(),
            "agent_list",
            "Agent List",
            "List background subagents and delegates spawned by this chat, with status, kind, terminal-window, and limit filters.",
            agent_list_schema(),
        )
    }

    async fn tool_execute(
        &mut self,
        ccx: Arc<AMutex<AtCommandsContext>>,
        tool_call_id: &String,
        args: &HashMap<String, Value>,
    ) -> Result<(bool, Vec<ContextEnum>), String> {
        let filter = parse_agent_list_filter(args)?;
        let (app, parent_chat_id) = background_agent_context(&ccx).await;
        let records = app.agents.list_for_parent(&parent_chat_id, filter).await;
        Ok(tool_message(tool_call_id, format_agent_table(&records)))
    }

    fn tool_depends_on(&self) -> Vec<String> {
        vec![]
    }
}

#[async_trait]
impl Tool for ToolAgentStatus {
    fn tool_description(&self) -> ToolDesc {
        background_agent_tool_desc(
            self.config_path.clone(),
            "agent_status",
            "Agent Status",
            "Show current status for one background agent spawned by this chat.",
            agent_status_schema(),
        )
    }

    async fn tool_execute(
        &mut self,
        ccx: Arc<AMutex<AtCommandsContext>>,
        tool_call_id: &String,
        args: &HashMap<String, Value>,
    ) -> Result<(bool, Vec<ContextEnum>), String> {
        let agent_id = parse_required_string(args, "agent_id")?;
        let (app, parent_chat_id) = background_agent_context(&ccx).await;
        let record = app
            .agents
            .get(&parent_chat_id, &agent_id)
            .await
            .map_err(|error| map_registry_error(&agent_id, error))?;
        Ok(tool_message(
            tool_call_id,
            format_agent_status_block(&record),
        ))
    }

    fn tool_depends_on(&self) -> Vec<String> {
        vec![]
    }
}

#[async_trait]
impl Tool for ToolAgentWait {
    fn tool_description(&self) -> ToolDesc {
        background_agent_tool_desc(
            self.config_path.clone(),
            "agent_wait",
            "Agent Wait",
            "Wait for one background agent spawned by this chat to reach a terminal state, or return its current status after a timeout. Incoming messages interrupt only the wait, not the underlying work.",
            agent_wait_schema(),
        )
    }

    async fn tool_execute(
        &mut self,
        ccx: Arc<AMutex<AtCommandsContext>>,
        tool_call_id: &String,
        args: &HashMap<String, Value>,
    ) -> Result<(bool, Vec<ContextEnum>), String> {
        let agent_id = parse_required_string(args, "agent_id")?;
        let timeout_ms = parse_wait_timeout_ms(args)?;
        let (app, parent_chat_id) = background_agent_context(&ccx).await;
        let initial = app
            .agents
            .get(&parent_chat_id, &agent_id)
            .await
            .map_err(|error| map_registry_error(&agent_id, error))?;
        if initial.status.is_terminal() {
            return Ok(tool_message(
                tool_call_id,
                format_agent_result(&initial, false).await?,
            ));
        }
        let interrupt = refact_runtime_api::WaitInterrupt::from_context(&ccx).await;
        let waited = app.agents.wait(
            &parent_chat_id,
            &agent_id,
            Duration::from_millis(timeout_ms),
        );
        let record = tokio::select! {
            biased;
            result = waited => result,
            _ = interrupt.wait() => app.agents.get(&parent_chat_id, &agent_id).await,
        }
        .map_err(|error| map_registry_error(&agent_id, error))?;
        let content = if record.status.is_terminal() {
            format_agent_result(&record, false).await?
        } else if interrupt.wait_flag.load(std::sync::atomic::Ordering::SeqCst)
            && !interrupt.abort_flag.load(std::sync::atomic::Ordering::SeqCst)
        {
            format!(
                "⏰ Interrupted while waiting — agent still running.\n\n{}",
                format_agent_status_block(&record)
            )
        } else {
            format!(
                "⏰ Timed out after {timeout_ms}ms — agent still running.\n\n{}",
                format_agent_status_block(&record)
            )
        };
        Ok(tool_message(tool_call_id, content))
    }

    fn tool_depends_on(&self) -> Vec<String> {
        vec![]
    }
}

#[async_trait]
impl Tool for ToolAgentResult {
    fn tool_description(&self) -> ToolDesc {
        background_agent_tool_desc(
            self.config_path.clone(),
            "agent_result",
            "Agent Result",
            "Return the final result for a completed, failed, cancelled, or interrupted background agent spawned by this chat.",
            agent_result_schema(),
        )
    }

    async fn tool_execute(
        &mut self,
        ccx: Arc<AMutex<AtCommandsContext>>,
        tool_call_id: &String,
        args: &HashMap<String, Value>,
    ) -> Result<(bool, Vec<ContextEnum>), String> {
        let agent_id = parse_required_string(args, "agent_id")?;
        let include_details = parse_optional_bool(args, "include_details", false)?;
        let (app, parent_chat_id) = background_agent_context(&ccx).await;
        let record = app
            .agents
            .get(&parent_chat_id, &agent_id)
            .await
            .map_err(|error| map_registry_error(&agent_id, error))?;
        let content = format_agent_result(&record, include_details).await?;
        Ok(tool_message(tool_call_id, content))
    }

    fn tool_depends_on(&self) -> Vec<String> {
        vec![]
    }
}

#[async_trait]
impl Tool for ToolAgentCancel {
    fn tool_description(&self) -> ToolDesc {
        background_agent_tool_desc(
            self.config_path.clone(),
            "agent_cancel",
            "Agent Cancel",
            "Request cancellation of one running background agent spawned by this chat.",
            agent_cancel_schema(),
        )
    }

    async fn tool_execute(
        &mut self,
        ccx: Arc<AMutex<AtCommandsContext>>,
        tool_call_id: &String,
        args: &HashMap<String, Value>,
    ) -> Result<(bool, Vec<ContextEnum>), String> {
        let agent_id = parse_required_string(args, "agent_id")?;
        let reason = parse_optional_string(args, "reason");
        let subtree = parse_optional_bool(args, "subtree", true)?;
        let (app, parent_chat_id) = background_agent_context(&ccx).await;
        let current = app
            .agents
            .get(&parent_chat_id, &agent_id)
            .await
            .map_err(|error| map_registry_error(&agent_id, error))?;
        let subtree_has_running_descendant = subtree
            && app
                .agents
                .list_descendants(&agent_id)
                .await
                .iter()
                .any(|record| !record.status.is_terminal());
        if current.status.is_terminal() && !subtree_has_running_descendant {
            return Ok(tool_message(
                tool_call_id,
                format!(
                    "Agent already in terminal state: {}. No action.",
                    format_status_chip(current.status)
                ),
            ));
        }
        let updated = app
            .agents
            .cancel_subtree(&parent_chat_id, &agent_id, subtree, reason)
            .await
            .map_err(|error| map_registry_error(&agent_id, error))?;
        for record in &updated {
            refact_agents_core::spawn::emit_background_agent_update(app.clone(), record).await;
        }
        let cancelled = updated
            .iter()
            .map(|record| record.agent_id.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        Ok(tool_message(
            tool_call_id,
            format!("✓ Cancel requested for: {cancelled}."),
        ))
    }

    fn tool_depends_on(&self) -> Vec<String> {
        vec![]
    }
}
