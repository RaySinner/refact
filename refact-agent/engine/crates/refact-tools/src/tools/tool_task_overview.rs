use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use serde_json::{json, Value};
use serde_yaml::Value as YamlValue;
use tokio::fs;
use tokio::sync::Mutex as AMutex;

use refact_runtime_api::AtCommandsContext;
use refact_core::chat_types::{ChatContent, ChatMessage, ContextEnum};
use refact_chat_history::history_limit::{compute_context_budget, ContextBudgetReport, ContextPressure};
use refact_runtime_api::task_storage as storage;
use refact_tasks::types::{BoardCard, TaskBoard};
use crate::tools::tool_task_check_agents::{get_agent_statuses_for_ctx, AgentStatus};
use crate::tools::tools_description::{Tool, ToolDesc, ToolSource, ToolSourceType};
use refact_runtime_api::{ChatSessionFacade, SessionState};

pub const MAX_REPORT_CHARS: usize = 4096;
const MAX_DOCUMENTS: usize = 6;
const MAX_ACTIVITY: usize = 10;

pub struct ToolTaskOverview;

impl ToolTaskOverview {
    pub fn new() -> Self {
        Self
    }
}

pub struct OverviewContext {
    pub task_id: String,
    pub ctx: refact_runtime_api::ChatCtx,
    pub chat_facade: Arc<dyn ChatSessionFacade>,
    pub messages: Vec<ChatMessage>,
    pub n_ctx: usize,
}

#[derive(Default)]
pub struct ColumnCounts {
    pub planned: usize,
    pub doing: usize,
    pub done: usize,
    pub failed: usize,
}

#[derive(Clone)]
pub struct ActivityEntry {
    pub timestamp: DateTime<Utc>,
    pub text: String,
}

#[derive(Default)]
pub struct MemoryPulse {
    pub total: usize,
    pub pinned: usize,
    pub archived: usize,
    pub superseded: usize,
    pub active: usize,
    pub kinds: HashMap<String, usize>,
    pub tags: HashMap<String, usize>,
    pub activities: Vec<ActivityEntry>,
}

#[derive(Clone)]
pub struct DocumentInfo {
    pub slug: String,
    pub name: String,
    pub kind: String,
    pub version: u64,
    pub pinned: bool,
    pub updated_at: Option<DateTime<Utc>>,
}

async fn overview_context(
    ccx: &Arc<AMutex<AtCommandsContext>>,
    args: &HashMap<String, Value>,
) -> Result<OverviewContext, String> {
    let ccx_lock = ccx.lock().await;
    let is_planner = ccx_lock
        .task_meta
        .as_ref()
        .map(|meta| meta.role == "planner")
        .unwrap_or(false);
    if !is_planner {
        return Err("task_overview can only be called by the task planner. Switch to the planner chat for task situational awareness.".to_string());
    }
    let task_id = args
        .get("task_id")
        .and_then(|value| value.as_str())
        .filter(|value| !value.is_empty())
        .map(|value| value.to_string())
        .or_else(|| ccx_lock.task_meta.as_ref().map(|meta| meta.task_id.clone()))
        .ok_or_else(|| "Missing 'task_id' (and chat is not bound to a task)".to_string())?;
    Ok(OverviewContext {
        task_id,
        ctx: ccx_lock.app.chat_ctx(),
        chat_facade: ccx_lock.app.chat.facade.clone(),
        messages: ccx_lock.messages.clone(),
        n_ctx: ccx_lock.n_ctx,
    })
}

fn task_overview_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "task_id": {
                "type": "string",
                "description": "Task ID (optional if chat is bound to a task)"
            }
        },
        "required": []
    })
}

fn task_overview_description() -> ToolDesc {
    ToolDesc {
        name: "task_overview".to_string(),
        display_name: "Task Overview".to_string(),
        source: ToolSource {
            source_type: ToolSourceType::Builtin,
            config_path: String::new(),
        },
        experimental: false,
        allow_parallel: true,
        description: "Planner-only one-shot situational awareness report combining board state, agent health, memory pulse, documents, recent activity, and context pressure.".to_string(),
        input_schema: task_overview_schema(),
        output_schema: None,
        annotations: None,
    }
}

fn priority_key(priority: &str) -> String {
    let priority = priority.trim().to_ascii_uppercase();
    if priority.is_empty() {
        "P1".to_string()
    } else {
        priority
    }
}

fn board_counts(board: &TaskBoard) -> Vec<(String, ColumnCounts)> {
    let mut counts: HashMap<String, ColumnCounts> = HashMap::new();
    for priority in ["P0", "P1", "P2"] {
        counts.entry(priority.to_string()).or_default();
    }
    for card in &board.cards {
        let entry = counts.entry(priority_key(&card.priority)).or_default();
        match card.column.as_str() {
            "planned" => entry.planned += 1,
            "doing" => entry.doing += 1,
            "done" => entry.done += 1,
            "failed" | "regressed" => entry.failed += 1,
            _ => entry.planned += 1,
        }
    }
    let mut rows: Vec<_> = counts.into_iter().collect();
    rows.sort_by(|(left, _), (right, _)| priority_rank(left).cmp(&priority_rank(right)));
    rows
}

fn priority_rank(priority: &str) -> u8 {
    match priority {
        "P0" => 0,
        "P1" => 1,
        "P2" => 2,
        _ => 3,
    }
}

fn render_board(board: &TaskBoard) -> String {
    let mut output = String::from("## Board\n");
    for (priority, count) in board_counts(board) {
        output.push_str(&format!(
            "{}: {} planned, {} doing, {} done, {} failed\n",
            priority, count.planned, count.doing, count.done, count.failed
        ));
    }
    if board.cards.is_empty() {
        output.push_str("_No cards yet._\n");
    }
    output
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum OverviewAgentState {
    Running,
    Stuck,
    Failed,
    Done,
    Paused,
}

fn classify_agent(status: &AgentStatus, _now: DateTime<Utc>) -> OverviewAgentState {
    match status.column.as_str() {
        "done" => OverviewAgentState::Done,
        "failed" | "regressed" => OverviewAgentState::Failed,
        "doing" => {
            if matches!(status.session_state, Some(SessionState::Error)) {
                return OverviewAgentState::Stuck;
            }
            if matches!(status.session_state, Some(SessionState::Completed))
                && status.final_report.is_none()
            {
                return OverviewAgentState::Stuck;
            }
            if matches!(
                status.session_state,
                Some(
                    SessionState::Paused
                        | SessionState::WaitingUserInput
                        | SessionState::WaitingIde
                )
            ) {
                return OverviewAgentState::Paused;
            }
            if matches!(
                status.session_state,
                Some(SessionState::Generating | SessionState::ExecutingTools)
            ) {
                return OverviewAgentState::Running;
            }
            if generation_loop_is_off(status) {
                return OverviewAgentState::Stuck;
            }
            OverviewAgentState::Running
        }
        _ => OverviewAgentState::Running,
    }
}

fn generation_loop_is_off(status: &AgentStatus) -> bool {
    matches!(status.session_state, Some(SessionState::Idle) | None)
}

pub fn render_agent_health(statuses: &[AgentStatus], now: DateTime<Utc>) -> String {
    let mut running = 0usize;
    let mut stuck = 0usize;
    let mut failed = 0usize;
    let mut done_last_hour = 0usize;
    let mut running_labels = Vec::new();

    for status in statuses {
        match classify_agent(status, now) {
            OverviewAgentState::Running | OverviewAgentState::Paused => {
                running += 1;
                if running_labels.len() < 4 {
                    let age = status
                        .last_activity_at
                        .map(|timestamp| format_duration_short(now, timestamp))
                        .unwrap_or_else(|| "?".to_string());
                    running_labels.push(format!(
                        "{}: {} {}",
                        priority_key(&status.priority),
                        status.card_id,
                        age
                    ));
                }
            }
            OverviewAgentState::Stuck => stuck += 1,
            OverviewAgentState::Failed => failed += 1,
            OverviewAgentState::Done => {
                if status
                    .last_activity_at
                    .map(|timestamp| now.signed_duration_since(timestamp) <= Duration::hours(1))
                    .unwrap_or(false)
                {
                    done_last_hour += 1;
                }
            }
        }
    }

    let running_detail = if running_labels.is_empty() {
        String::new()
    } else {
        format!("   ({})", running_labels.join(", "))
    };
    format!(
        "## Agent Health\n🔄 {} running{}\n🔴 {} stuck\n❌ {} failed\n✅ {} done in last hour\n",
        running, running_detail, stuck, failed, done_last_hour
    )
}

fn split_frontmatter(raw: &str) -> Option<&str> {
    let rest = raw
        .strip_prefix("---\n")
        .or_else(|| raw.strip_prefix("---\r\n"))?;
    if let Some(index) = rest.find("\n---\n") {
        return Some(&rest[..index]);
    }
    if let Some(index) = rest.find("\r\n---\r\n") {
        return Some(&rest[..index]);
    }
    None
}

fn yaml_field<'a>(mapping: &'a serde_yaml::Mapping, key: &str) -> Option<&'a YamlValue> {
    mapping.get(&YamlValue::String(key.to_string()))
}

fn yaml_string(mapping: &serde_yaml::Mapping, key: &str) -> Option<String> {
    match yaml_field(mapping, key)? {
        YamlValue::String(value) => Some(value.clone()),
        YamlValue::Number(value) => Some(value.to_string()),
        YamlValue::Bool(value) => Some(value.to_string()),
        _ => None,
    }
}

fn yaml_bool(mapping: &serde_yaml::Mapping, key: &str) -> bool {
    match yaml_field(mapping, key) {
        Some(YamlValue::Bool(value)) => *value,
        Some(YamlValue::String(value)) => value.trim().eq_ignore_ascii_case("true"),
        _ => false,
    }
}

fn yaml_u64(mapping: &serde_yaml::Mapping, key: &str) -> u64 {
    match yaml_field(mapping, key) {
        Some(YamlValue::Number(value)) => value.as_u64().unwrap_or(0),
        Some(YamlValue::String(value)) => value.trim().parse().unwrap_or(0),
        _ => 0,
    }
}

fn yaml_string_list(mapping: &serde_yaml::Mapping, key: &str) -> Vec<String> {
    match yaml_field(mapping, key) {
        Some(YamlValue::Sequence(values)) => values
            .iter()
            .filter_map(|value| match value {
                YamlValue::String(value) => Some(value.trim().to_string()),
                YamlValue::Number(value) => Some(value.to_string()),
                _ => None,
            })
            .filter(|value| !value.is_empty())
            .collect(),
        Some(YamlValue::String(value)) => value
            .split(',')
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .collect(),
        _ => Vec::new(),
    }
}

fn parse_yaml_mapping(raw: &str) -> Option<serde_yaml::Mapping> {
    let frontmatter = split_frontmatter(raw)?;
    match serde_yaml::from_str::<YamlValue>(frontmatter).ok()? {
        YamlValue::Mapping(mapping) => Some(mapping),
        _ => None,
    }
}

fn parse_timestamp(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|timestamp| timestamp.with_timezone(&Utc))
}

async fn file_modified_at(path: &Path) -> Option<DateTime<Utc>> {
    fs::metadata(path)
        .await
        .ok()
        .and_then(|metadata| metadata.modified().ok())
        .map(system_time_to_utc)
}

fn system_time_to_utc(time: SystemTime) -> DateTime<Utc> {
    DateTime::<Utc>::from(time)
}

fn display_name_from_path(path: &Path) -> String {
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("untitled")
        .to_string()
}

async fn markdown_files_in(dir: &Path) -> Result<Vec<PathBuf>, String> {
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut files = Vec::new();
    let mut entries = fs::read_dir(dir)
        .await
        .map_err(|error| format!("failed to read {}: {}", dir.display(), error))?;
    while let Some(entry) = entries
        .next_entry()
        .await
        .map_err(|error| format!("failed to read {}: {}", dir.display(), error))?
    {
        let path = entry.path();
        if path.is_file()
            && matches!(
                path.extension().and_then(|ext| ext.to_str()),
                Some("md" | "mdx")
            )
        {
            files.push(path);
        }
    }
    files.sort();
    Ok(files)
}

async fn load_memory_pulse(task_dir: &Path) -> Result<MemoryPulse, String> {
    let memories_dir = task_dir.join("memories");
    let archived_dir = memories_dir.join("archived");
    let mut pulse = MemoryPulse::default();

    for (dir, force_archived) in [(&memories_dir, false), (&archived_dir, true)] {
        for path in markdown_files_in(dir).await? {
            let raw = fs::read_to_string(&path).await.unwrap_or_default();
            let mapping = parse_yaml_mapping(&raw).unwrap_or_default();
            let status = if force_archived {
                "archived".to_string()
            } else {
                yaml_string(&mapping, "status").unwrap_or_else(|| "active".to_string())
            };
            let kind = yaml_string(&mapping, "kind").unwrap_or_else(|| "freeform".to_string());
            let tags = yaml_string_list(&mapping, "tags");
            let title =
                yaml_string(&mapping, "title").unwrap_or_else(|| display_name_from_path(&path));
            let created_at = yaml_string(&mapping, "created_at")
                .and_then(|timestamp| parse_timestamp(&timestamp));
            let timestamp = created_at.or(file_modified_at(&path).await);

            pulse.total += 1;
            if yaml_bool(&mapping, "pinned") {
                pulse.pinned += 1;
            }
            match status.as_str() {
                "archived" => pulse.archived += 1,
                "superseded" => pulse.superseded += 1,
                _ => pulse.active += 1,
            }
            *pulse.kinds.entry(kind).or_default() += 1;
            for tag in tags {
                *pulse.tags.entry(tag).or_default() += 1;
            }
            if let Some(timestamp) = timestamp {
                pulse.activities.push(ActivityEntry {
                    timestamp,
                    text: format!("memory created: {}", truncate_chars(&title, 80)),
                });
            }
        }
    }

    Ok(pulse)
}

async fn load_documents(
    task_dir: &Path,
) -> Result<(Vec<DocumentInfo>, Vec<ActivityEntry>), String> {
    let documents_dir = task_dir.join("documents");
    let mut documents = Vec::new();
    let mut activities = Vec::new();

    for path in markdown_files_in(&documents_dir).await? {
        let raw = fs::read_to_string(&path).await.unwrap_or_default();
        let mapping = parse_yaml_mapping(&raw).unwrap_or_default();
        let slug = yaml_string(&mapping, "slug").unwrap_or_else(|| display_name_from_path(&path));
        let name = yaml_string(&mapping, "name").unwrap_or_else(|| slug.clone());
        let kind = yaml_string(&mapping, "kind").unwrap_or_else(|| "document".to_string());
        let updated_at = yaml_string(&mapping, "updated_at")
            .and_then(|timestamp| parse_timestamp(&timestamp))
            .or(file_modified_at(&path).await);
        let document = DocumentInfo {
            slug: slug.clone(),
            name,
            kind,
            version: yaml_u64(&mapping, "version"),
            pinned: yaml_bool(&mapping, "pinned"),
            updated_at,
        };
        if let Some(timestamp) = updated_at {
            activities.push(ActivityEntry {
                timestamp,
                text: format!("document updated: {}", truncate_chars(&slug, 80)),
            });
        }
        documents.push(document);
    }

    documents.sort_by(|left, right| {
        right
            .pinned
            .cmp(&left.pinned)
            .then_with(|| right.updated_at.cmp(&left.updated_at))
            .then_with(|| left.slug.cmp(&right.slug))
    });
    Ok((documents, activities))
}

fn top_counts(counts: &HashMap<String, usize>, limit: usize) -> String {
    if counts.is_empty() {
        return "none".to_string();
    }
    let mut rows: Vec<_> = counts.iter().collect();
    rows.sort_by(|(left_key, left_count), (right_key, right_count)| {
        right_count
            .cmp(left_count)
            .then_with(|| left_key.cmp(right_key))
    });
    rows.into_iter()
        .take(limit)
        .map(|(key, count)| format!("{}={}", key, count))
        .collect::<Vec<_>>()
        .join(", ")
}

fn render_memory_pulse(pulse: &MemoryPulse) -> String {
    let counts = if pulse.superseded > 0 {
        format!(
            "{} memories total ({} pinned, {} archived, {} superseded, {} active)",
            pulse.total, pulse.pinned, pulse.archived, pulse.superseded, pulse.active
        )
    } else {
        format!(
            "{} memories total ({} pinned, {} archived, {} active)",
            pulse.total, pulse.pinned, pulse.archived, pulse.active
        )
    };
    format!(
        "## Memory Pulse\n{}\nRecent kinds: {}\nTop tags: {}\n",
        counts,
        top_counts(&pulse.kinds, 5),
        top_counts(&pulse.tags, 5)
    )
}

fn render_documents(documents: &[DocumentInfo], now: DateTime<Utc>) -> String {
    let mut output = String::from("## Documents\n");
    if documents.is_empty() {
        output.push_str("- No task documents found.\n");
        return output;
    }
    for document in documents.iter().take(MAX_DOCUMENTS) {
        let updated = document
            .updated_at
            .map(|timestamp| format_age_ago(now, timestamp))
            .unwrap_or_else(|| "unknown".to_string());
        let pinned = if document.pinned { ", pinned" } else { "" };
        let version = if document.version > 0 {
            format!("v{}", document.version)
        } else {
            "v?".to_string()
        };
        output.push_str(&format!(
            "- {} ({}, {}, updated {}{})\n",
            truncate_chars(&document.name, 60),
            document.kind,
            version,
            updated,
            pinned
        ));
    }
    if documents.len() > MAX_DOCUMENTS {
        output.push_str(&format!(
            "- … {} more documents\n",
            documents.len() - MAX_DOCUMENTS
        ));
    }
    output
}

fn card_activity_entries(board: &TaskBoard) -> Vec<ActivityEntry> {
    let mut entries = Vec::new();
    for card in &board.cards {
        for update in &card.status_updates {
            if let Some(timestamp) = parse_timestamp(&update.timestamp) {
                entries.push(ActivityEntry {
                    timestamp,
                    text: format_card_update(card, &update.message),
                });
            }
        }
    }
    entries
}

fn format_card_update(card: &BoardCard, message: &str) -> String {
    let message = message.trim();
    let lower = message.to_ascii_lowercase();
    let text = if lower.contains("auto-committed") {
        "merged/committed work".to_string()
    } else if lower.contains("merged") || lower.contains("merge") {
        format!("merge: {}", message)
    } else {
        message.to_string()
    };
    format!("{} {}", card.id, truncate_chars(&text, 100))
}

fn render_recent_activity(activities: &[ActivityEntry], now: DateTime<Utc>) -> String {
    let mut recent: Vec<_> = activities
        .iter()
        .filter(|entry| now.signed_duration_since(entry.timestamp) <= Duration::hours(1))
        .collect();
    recent.sort_by(|left, right| right.timestamp.cmp(&left.timestamp));

    let mut output = String::from("## Recent Activity (last 1h)\n");
    if recent.is_empty() {
        output.push_str("- No activity in the last hour.\n");
        return output;
    }
    for entry in recent.into_iter().take(MAX_ACTIVITY) {
        output.push_str(&format!(
            "- {} ({})\n",
            entry.text,
            format_age_ago(now, entry.timestamp)
        ));
    }
    output
}

pub fn render_context_pressure(budget: &ContextBudgetReport) -> String {
    let pct_used = if budget.effective_n_ctx > 0 {
        budget.used_tokens_estimate.saturating_mul(100) / budget.effective_n_ctx
    } else {
        0
    };
    format!(
        "## Context Pressure\nPlanner chat: {}% of {} tokens used ({})\n",
        pct_used,
        budget.effective_n_ctx,
        pressure_label(&budget.pressure)
    )
}

fn pressure_label(pressure: &ContextPressure) -> &'static str {
    match pressure {
        ContextPressure::Low => "Low",
        ContextPressure::Medium => "Medium",
        ContextPressure::High => "High",
        ContextPressure::Critical => "Critical",
    }
}

pub fn render_task_overview(
    task_name: &str,
    board: &TaskBoard,
    statuses: &[AgentStatus],
    memory_pulse: &MemoryPulse,
    documents: &[DocumentInfo],
    activities: &[ActivityEntry],
    budget: &ContextBudgetReport,
    now: DateTime<Utc>,
) -> String {
    let mut output = format!("# Task Overview: {}\n\n", task_name.trim());
    output.push_str(&render_board(board));
    output.push('\n');
    output.push_str(&render_agent_health(statuses, now));
    output.push('\n');
    output.push_str(&render_memory_pulse(memory_pulse));
    output.push('\n');
    output.push_str(&render_documents(documents, now));
    output.push('\n');
    output.push_str(&render_recent_activity(activities, now));
    output.push('\n');
    output.push_str(&render_context_pressure(budget));
    truncate_report(output)
}

fn truncate_report(output: String) -> String {
    if output.chars().count() <= MAX_REPORT_CHARS {
        return output;
    }
    let note = "\n\n_Overview truncated to fit 4KB._";
    let keep = MAX_REPORT_CHARS.saturating_sub(note.chars().count());
    let mut truncated = output.chars().take(keep).collect::<String>();
    if let Some(index) = truncated.rfind('\n') {
        truncated.truncate(index);
    }
    truncated.push_str(note);
    truncated
}

fn truncate_chars(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_string();
    }
    let take = limit.saturating_sub(1);
    format!("{}…", text.chars().take(take).collect::<String>())
}

fn format_age_ago(now: DateTime<Utc>, timestamp: DateTime<Utc>) -> String {
    let duration = now.signed_duration_since(timestamp);
    let minutes = duration.num_minutes().max(0);
    if minutes == 0 {
        "now".to_string()
    } else if minutes < 60 {
        format!("{}m ago", minutes)
    } else if minutes < 60 * 24 {
        format!("{}h ago", minutes / 60)
    } else {
        format!("{}d ago", minutes / (60 * 24))
    }
}

fn format_duration_short(now: DateTime<Utc>, timestamp: DateTime<Utc>) -> String {
    let duration = now.signed_duration_since(timestamp);
    let minutes = duration.num_minutes().max(0);
    if minutes == 0 {
        "now".to_string()
    } else if minutes < 60 {
        format!("{}m", minutes)
    } else if minutes < 60 * 24 {
        format!("{}h", minutes / 60)
    } else {
        format!("{}d", minutes / (60 * 24))
    }
}

fn tool_output(tool_call_id: &String, result: String) -> (bool, Vec<ContextEnum>) {
    (
        false,
        vec![ContextEnum::ChatMessage(ChatMessage {
            role: "tool".to_string(),
            content: ChatContent::SimpleText(result),
            tool_calls: None,
            tool_call_id: tool_call_id.clone(),
            ..Default::default()
        })],
    )
}

#[async_trait]
impl Tool for ToolTaskOverview {
    fn tool_description(&self) -> ToolDesc {
        task_overview_description()
    }

    async fn tool_execute(
        &mut self,
        ccx: Arc<AMutex<AtCommandsContext>>,
        tool_call_id: &String,
        args: &HashMap<String, Value>,
    ) -> Result<(bool, Vec<ContextEnum>), String> {
        let context = overview_context(&ccx, args).await?;
        let meta = storage::load_task_meta_for_ctx(&context.ctx, &context.task_id).await?;
        let board = storage::load_board_for_ctx(&context.ctx, &context.task_id).await?;
        let statuses = get_agent_statuses_for_ctx(
            &context.ctx,
            context.chat_facade.clone(),
            &context.task_id,
        )
        .await?;
        let task_dir = storage::find_task_dir_for_ctx(&context.ctx, &context.task_id).await?;
        let memory_pulse = load_memory_pulse(&task_dir).await?;
        let (documents, document_activities) = load_documents(&task_dir).await?;
        let mut activities = card_activity_entries(&board);
        activities.extend(memory_pulse.activities.clone());
        activities.extend(document_activities);
        let budget = compute_context_budget(&context.messages, context.n_ctx);
        let report = render_task_overview(
            &meta.name,
            &board,
            &statuses,
            &memory_pulse,
            &documents,
            &activities,
            &budget,
            Utc::now(),
        );
        Ok(tool_output(tool_call_id, report))
    }

    fn tool_depends_on(&self) -> Vec<String> {
        vec![]
    }
}
