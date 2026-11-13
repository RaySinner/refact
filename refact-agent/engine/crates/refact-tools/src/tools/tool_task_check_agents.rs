use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use serde_json::{json, Value};
use tokio::sync::Mutex as AMutex;

use refact_runtime_api::AtCommandsContext;
use refact_runtime_api::GlobalContextCtx;
use refact_core::chat_types::{ChatContent, ChatMessage, ContextEnum};
use refact_runtime_api::task_storage as storage;
use refact_tasks::types::{AbVariantInfo, BoardCard, TaskBoard, TeamMember};
use crate::tools::task_tool_helpers::{human_age, require_bound_planner_task, truncate_chars};
use crate::tools::tools_description::{Tool, ToolDesc, ToolSource, ToolSourceType};
use refact_runtime_api::{ChatSessionFacade, SessionState};

pub const DEFAULT_LIMIT: usize = 20;
pub const GRACE_PERIOD_SECS: i64 = 180;

pub struct ToolTaskCheckAgents;

impl ToolTaskCheckAgents {
    pub fn new() -> Self {
        Self
    }
}

#[derive(Debug, Clone)]
pub struct AgentStatus {
    pub card_id: String,
    pub variant_key: Option<String>,
    pub card_title: String,
    pub agent_chat_id: String,
    pub column: String,
    pub priority: String,
    pub session_state: Option<SessionState>,
    pub last_status_update: Option<String>,
    pub last_activity_at: Option<DateTime<Utc>>,
    pub final_report: Option<String>,
    pub last_tool_name: Option<String>,
    pub change_seq: u64,
    pub room_members: Vec<TeamMember>,
}

impl AgentStatus {
    pub fn status_id(&self) -> String {
        self.variant_key
            .as_deref()
            .map(|variant| format!("{}/{}", self.card_id, variant))
            .unwrap_or_else(|| self.card_id.clone())
    }

    pub fn matches_card_filter(&self, filter: &str) -> bool {
        let filter = filter.trim();
        filter == self.card_id
            || filter == self.status_id()
            || self
                .variant_key
                .as_deref()
                .map(|variant| filter == format!("{}:{}", self.card_id, variant))
                .unwrap_or(false)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentReportFormat {
    Compact,
    Summary,
    Detail,
    Delta,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AgentStateKind {
    Running,
    Stuck,
    Failed,
    Done,
    Paused,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentSort {
    Priority,
    LastActivity,
    CardId,
    Status,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentGroupBy {
    Status,
    Priority,
    None,
}

#[derive(Debug, Clone)]
pub struct AgentStatusQuery {
    pub format: AgentReportFormat,
    pub status_filter: Option<HashSet<AgentStateKind>>,
    pub card_ids: Option<HashSet<String>>,
    pub priority_filter: Option<HashSet<String>>,
    pub min_age_minutes: Option<i64>,
    pub sort: AgentSort,
    pub limit: usize,
    pub offset: usize,
    pub since_seq: Option<u64>,
    pub group_by: AgentGroupBy,
}

impl AgentStatusQuery {
    pub fn default_for_format(format: AgentReportFormat) -> Self {
        let default_status_filter = if matches!(
            format,
            AgentReportFormat::Summary | AgentReportFormat::Detail
        ) {
            None
        } else {
            Some(HashSet::from([
                AgentStateKind::Running,
                AgentStateKind::Stuck,
                AgentStateKind::Failed,
            ]))
        };
        Self {
            format,
            status_filter: default_status_filter,
            card_ids: None,
            priority_filter: None,
            min_age_minutes: None,
            sort: AgentSort::Priority,
            limit: DEFAULT_LIMIT,
            offset: 0,
            since_seq: None,
            group_by: AgentGroupBy::Status,
        }
    }
}

pub fn agent_status_input_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "task_id": {
                "type": "string",
                "description": "Task ID (optional if chat is bound to a task)"
            },
            "format": {
                "type": "string",
                "enum": ["compact", "summary", "detail", "delta"],
                "description": "Output format. Default: compact"
            },
            "status_filter": {
                "type": "array",
                "items": {"type": "string", "enum": ["running", "stuck", "failed", "done", "paused", "all"]},
                "description": "Statuses to include. Default: running, stuck, failed"
            },
            "card_ids": {
                "type": "array",
                "items": {"type": "string"},
                "description": "Only include these card IDs"
            },
            "priority_filter": {
                "type": "array",
                "items": {"type": "string", "enum": ["P0", "P1", "P2"]},
                "description": "Only include these priorities"
            },
            "min_age_minutes": {
                "type": "number",
                "description": "Only include agents with last activity at least this many minutes ago"
            },
            "sort": {
                "type": "string",
                "enum": ["priority", "last_activity", "card_id", "status"],
                "description": "Sort key. Default: priority"
            },
            "limit": {
                "type": "number",
                "description": "Maximum rows after filtering. Default: 20"
            },
            "offset": {
                "type": "number",
                "description": "Rows to skip after filtering. Default: 0"
            },
            "since_seq": {
                "type": "number",
                "description": "Delta mode: only include cards changed after this sequence"
            },
            "group_by": {
                "type": "string",
                "enum": ["status", "priority", "none"],
                "description": "Grouping hint. Default: status"
            }
        },
        "required": []
    })
}

pub fn parse_agent_status_query(
    args: &HashMap<String, Value>,
) -> Result<AgentStatusQuery, String> {
    let format = parse_format(args.get("format"))?;
    let mut query = AgentStatusQuery::default_for_format(format);

    if let Some(statuses) = parse_string_list(args, "status_filter")? {
        query.status_filter = parse_status_filter(&statuses)?;
    }
    if let Some(card_ids) = parse_string_list(args, "card_ids")? {
        query.card_ids = Some(card_ids.into_iter().collect());
    }
    if let Some(priorities) = parse_string_list(args, "priority_filter")? {
        query.priority_filter = Some(
            priorities
                .into_iter()
                .map(|p| p.to_ascii_uppercase())
                .map(|p| match p.as_str() {
                    "P0" | "P1" | "P2" => Ok(p),
                    _ => Err(format!("Invalid priority_filter value: {}", p)),
                })
                .collect::<Result<HashSet<_>, _>>()?,
        );
    }
    if let Some(min_age) = parse_i64(args, "min_age_minutes")? {
        query.min_age_minutes = Some(min_age);
    }
    if let Some(sort) = args.get("sort") {
        query.sort = parse_sort(sort)?;
    }
    if let Some(limit) = parse_usize(args, "limit")? {
        query.limit = limit;
    }
    if let Some(offset) = parse_usize(args, "offset")? {
        query.offset = offset;
    }
    if let Some(seq) = parse_u64(args, "since_seq")? {
        query.since_seq = Some(seq);
    }
    if let Some(group_by) = args.get("group_by") {
        query.group_by = parse_group_by(group_by)?;
    }
    if matches!(query.format, AgentReportFormat::Delta) && query.since_seq.is_none() {
        return Err("delta format requires since_seq".to_string());
    }

    Ok(query)
}

pub fn parse_format(value: Option<&Value>) -> Result<AgentReportFormat, String> {
    match value.and_then(|v| v.as_str()).unwrap_or("compact") {
        "compact" => Ok(AgentReportFormat::Compact),
        "summary" => Ok(AgentReportFormat::Summary),
        "detail" => Ok(AgentReportFormat::Detail),
        "delta" => Ok(AgentReportFormat::Delta),
        other => Err(format!("Invalid format: {}", other)),
    }
}

pub fn parse_sort(value: &Value) -> Result<AgentSort, String> {
    match value.as_str().unwrap_or("priority") {
        "priority" => Ok(AgentSort::Priority),
        "last_activity" => Ok(AgentSort::LastActivity),
        "card_id" => Ok(AgentSort::CardId),
        "status" => Ok(AgentSort::Status),
        other => Err(format!("Invalid sort: {}", other)),
    }
}

pub fn parse_group_by(value: &Value) -> Result<AgentGroupBy, String> {
    match value.as_str().unwrap_or("status") {
        "status" => Ok(AgentGroupBy::Status),
        "priority" => Ok(AgentGroupBy::Priority),
        "none" => Ok(AgentGroupBy::None),
        other => Err(format!("Invalid group_by: {}", other)),
    }
}

pub fn parse_string_list(
    args: &HashMap<String, Value>,
    key: &str,
) -> Result<Option<Vec<String>>, String> {
    let Some(value) = args.get(key) else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    if let Some(single) = value.as_str() {
        return Ok(Some(vec![single.to_string()]));
    }
    let array = value
        .as_array()
        .ok_or_else(|| format!("{} must be an array of strings", key))?;
    let mut items = Vec::new();
    for item in array {
        let text = item
            .as_str()
            .ok_or_else(|| format!("{} must be an array of strings", key))?;
        items.push(text.to_string());
    }
    Ok(Some(items))
}

pub fn parse_status_filter(values: &[String]) -> Result<Option<HashSet<AgentStateKind>>, String> {
    let mut result = HashSet::new();
    for value in values {
        match value.as_str() {
            "all" => return Ok(None),
            "running" => {
                result.insert(AgentStateKind::Running);
            }
            "stuck" => {
                result.insert(AgentStateKind::Stuck);
            }
            "failed" => {
                result.insert(AgentStateKind::Failed);
            }
            "done" => {
                result.insert(AgentStateKind::Done);
            }
            "paused" => {
                result.insert(AgentStateKind::Paused);
            }
            other => return Err(format!("Invalid status_filter value: {}", other)),
        };
    }
    Ok(Some(result))
}

pub fn parse_usize(args: &HashMap<String, Value>, key: &str) -> Result<Option<usize>, String> {
    let Some(value) = args.get(key) else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let Some(n) = value.as_u64() else {
        return Err(format!("{} must be a non-negative number", key));
    };
    usize::try_from(n)
        .map(Some)
        .map_err(|_| format!("{} is too large", key))
}

pub fn parse_u64(args: &HashMap<String, Value>, key: &str) -> Result<Option<u64>, String> {
    let Some(value) = args.get(key) else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    value
        .as_u64()
        .map(Some)
        .ok_or_else(|| format!("{} must be a non-negative number", key))
}

pub fn parse_i64(args: &HashMap<String, Value>, key: &str) -> Result<Option<i64>, String> {
    let Some(value) = args.get(key) else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let Some(n) = value.as_i64() else {
        return Err(format!("{} must be a non-negative number", key));
    };
    if n < 0 {
        return Err(format!("{} must be a non-negative number", key));
    }
    Ok(Some(n))
}

pub async fn get_agent_statuses_for_ctx(
    ctx: &refact_runtime_api::ChatCtx,
    chat_facade: Arc<dyn ChatSessionFacade>,
    task_id: &str,
) -> Result<Vec<AgentStatus>, String> {
    let board = storage::load_board_for_ctx(ctx, task_id).await?;
    statuses_from_board(&board, chat_facade).await
}

pub async fn statuses_from_board(
    board: &TaskBoard,
    chat_facade: Arc<dyn ChatSessionFacade>,
) -> Result<Vec<AgentStatus>, String> {
    let mut statuses = Vec::new();

    for card in &board.cards {
        if !should_report_card(card) {
            continue;
        }

        if let Some(variants) = card.ab_variants.as_ref() {
            if card.column == "doing" && variants.winner.is_none() {
                for (variant_key, variant) in [("a", &variants.a), ("b", &variants.b)] {
                    let (session_state, last_tool_name) =
                        session_details(chat_facade.clone(), &variant.chat_id).await?;
                    statuses.push(agent_status_from_ab_variant(
                        card,
                        board.rev,
                        variant_key,
                        variant,
                        session_state,
                        last_tool_name,
                    ));
                }
                continue;
            }
        }

        if let Some(agent_chat_id) = &card.agent_chat_id {
            let (session_state, last_tool_name) =
                session_details(chat_facade.clone(), agent_chat_id).await?;
            statuses.push(agent_status_from_card(
                card,
                board.rev,
                session_state,
                last_tool_name,
            ));
        } else {
            statuses.push(agent_status_from_card(card, board.rev, None, None));
        }
    }

    Ok(statuses)
}

pub async fn session_details(
    chat_facade: Arc<dyn ChatSessionFacade>,
    agent_chat_id: &str,
) -> Result<(Option<SessionState>, Option<String>), String> {
    let live_state = chat_facade.session_state(agent_chat_id).await?;
    if live_state.is_none() {
        return Ok((None, None));
    }
    let snapshot = chat_facade.session_snapshot(agent_chat_id).await.ok();
    let last_tool_name = snapshot
        .as_ref()
        .and_then(|snapshot| last_tool_name_from_messages(&snapshot.messages));
    Ok((live_state, last_tool_name))
}

pub fn should_report_card(card: &BoardCard) -> bool {
    card.agent_chat_id.is_some() || matches!(card.column.as_str(), "doing" | "done" | "failed")
}

pub fn agent_status_from_card(
    card: &BoardCard,
    board_rev: u64,
    session_state: Option<SessionState>,
    last_tool_name: Option<String>,
) -> AgentStatus {
    let last_update = card.status_updates.last();
    let last_status_update_at = last_update.and_then(|update| parse_timestamp(&update.timestamp));
    let last_heartbeat_at = card.last_heartbeat_at.as_deref().and_then(parse_timestamp);
    let completed_at = card.completed_at.as_deref().and_then(parse_timestamp);
    let started_at = card.started_at.as_deref().and_then(parse_timestamp);
    let created_at = parse_timestamp(&card.created_at);
    let last_activity_at = latest_timestamp([
        last_heartbeat_at,
        last_status_update_at,
        completed_at,
        started_at,
        created_at,
    ]);
    let change_seq = change_seq_from_activity(board_rev, last_activity_at);
    let last_status_update = last_update.map(|u| format!("{}: {}", u.timestamp, u.message));

    AgentStatus {
        card_id: card.id.clone(),
        variant_key: None,
        card_title: card.title.clone(),
        agent_chat_id: card
            .agent_chat_id
            .clone()
            .unwrap_or_else(|| "none".to_string()),
        column: card.column.clone(),
        priority: card.priority.clone(),
        session_state,
        last_status_update,
        last_activity_at,
        final_report: card.final_report.clone(),
        last_tool_name,
        change_seq,
        room_members: card.team_members.clone(),
    }
}

pub fn agent_status_from_ab_variant(
    card: &BoardCard,
    board_rev: u64,
    variant_key: &str,
    variant: &AbVariantInfo,
    session_state: Option<SessionState>,
    last_tool_name: Option<String>,
) -> AgentStatus {
    let last_update = card.status_updates.last();
    let last_status_update_at = last_update.and_then(|update| parse_timestamp(&update.timestamp));
    let last_heartbeat_at = card.last_heartbeat_at.as_deref().and_then(parse_timestamp);
    let started_at = card.started_at.as_deref().and_then(parse_timestamp);
    let created_at = parse_timestamp(&card.created_at);
    let (column, completed_at, final_report, variant_update) = match variant.finish.as_ref() {
        Some(finish) => (
            if finish.success { "done" } else { "failed" }.to_string(),
            parse_timestamp(&finish.completed_at),
            Some(finish.final_report.clone()),
            Some(format!(
                "{}: A/B variant {} finished",
                finish.completed_at,
                variant_key.to_ascii_uppercase()
            )),
        ),
        None => (card.column.clone(), None, None, None),
    };
    let last_activity_at = latest_timestamp([
        completed_at,
        last_heartbeat_at,
        last_status_update_at,
        started_at,
        created_at,
    ]);
    let change_seq = change_seq_from_activity(board_rev, last_activity_at);
    let last_status_update = variant_update
        .or_else(|| last_update.map(|update| format!("{}: {}", update.timestamp, update.message)));

    AgentStatus {
        card_id: card.id.clone(),
        variant_key: Some(variant_key.to_string()),
        card_title: format!("{} [A/B {}]", card.title, variant_key.to_ascii_uppercase()),
        agent_chat_id: variant.chat_id.clone(),
        column,
        priority: card.priority.clone(),
        session_state,
        last_status_update,
        last_activity_at,
        final_report,
        last_tool_name,
        change_seq,
        room_members: vec![],
    }
}

pub fn change_seq_from_activity(board_rev: u64, last_activity_at: Option<DateTime<Utc>>) -> u64 {
    if board_rev > 0 {
        return board_rev;
    }
    last_activity_at
        .map(|timestamp| timestamp.timestamp_millis().max(0) as u64)
        .unwrap_or_default()
}

pub fn last_tool_name_from_messages(messages: &[ChatMessage]) -> Option<String> {
    messages.iter().rev().find_map(|message| {
        message
            .tool_calls
            .as_ref()
            .and_then(|tool_calls| tool_calls.last())
            .map(|tool_call| tool_call.function.name.clone())
    })
}

pub fn parse_timestamp(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
}

pub fn latest_timestamp(
    times: impl IntoIterator<Item = Option<DateTime<Utc>>>,
) -> Option<DateTime<Utc>> {
    times.into_iter().flatten().max()
}

pub fn classify_agent_status(status: &AgentStatus, now: DateTime<Utc>) -> AgentStateKind {
    match status.column.as_str() {
        "done" => AgentStateKind::Done,
        "failed" => AgentStateKind::Failed,
        "doing" => {
            if matches!(status.session_state, Some(SessionState::Starting)) {
                return AgentStateKind::Running;
            }
            if in_grace_period(status, now) {
                return AgentStateKind::Running;
            }
            if matches!(status.session_state, Some(SessionState::Error)) {
                return AgentStateKind::Stuck;
            }
            if matches!(status.session_state, Some(SessionState::Completed))
                && status.final_report.is_none()
            {
                return AgentStateKind::Stuck;
            }
            if matches!(
                status.session_state,
                Some(
                    SessionState::Paused
                        | SessionState::WaitingUserInput
                        | SessionState::WaitingIde
                )
            ) {
                return AgentStateKind::Paused;
            }
            if matches!(
                status.session_state,
                Some(SessionState::Generating | SessionState::ExecutingTools)
            ) {
                return AgentStateKind::Running;
            }
            if generation_loop_is_off(status) {
                return AgentStateKind::Stuck;
            }
            AgentStateKind::Running
        }
        _ => AgentStateKind::Running,
    }
}

pub fn in_grace_period(status: &AgentStatus, now: DateTime<Utc>) -> bool {
    status.column == "doing"
        && status.agent_chat_id != "none"
        && matches!(
            status.session_state,
            None | Some(SessionState::Starting | SessionState::Idle)
        )
        && status
            .last_activity_at
            .map(|last| now.signed_duration_since(last).num_seconds() < GRACE_PERIOD_SECS)
            .unwrap_or(false)
}

pub fn generation_loop_is_off(status: &AgentStatus) -> bool {
    matches!(status.session_state, Some(SessionState::Idle) | None)
}

pub fn has_active_agent_statuses(statuses: &[AgentStatus]) -> bool {
    let now = Utc::now();
    statuses
        .iter()
        .any(|status| waitable_agent_status_at(status, now))
}

pub fn waitable_agent_status_at(status: &AgentStatus, now: DateTime<Utc>) -> bool {
    if status.column != "doing" {
        return false;
    }
    if in_grace_period(status, now) {
        return true;
    }
    status.session_state.is_some()
        && matches!(
            classify_agent_status(status, now),
            AgentStateKind::Running | AgentStateKind::Paused
        )
}

pub fn waitable_agent_statuses<'a>(
    statuses: &'a [AgentStatus],
    query: &AgentStatusQuery,
    status_filter_explicit: bool,
) -> Vec<&'a AgentStatus> {
    let now = Utc::now();
    let filtered = if status_filter_explicit {
        filtered_statuses(statuses, query, now)
    } else {
        let mut query = query.clone();
        query.status_filter = None;
        filtered_statuses(statuses, &query, now)
    };
    filtered
        .into_iter()
        .filter(|status| waitable_agent_status_at(status, now))
        .collect()
}

pub fn format_agent_status_detail_at(status: &AgentStatus, now: DateTime<Utc>) -> String {
    let kind = classify_agent_status(status, now);
    let (state_emoji, state_text) = match kind {
        AgentStateKind::Done => ("✅", "Completed"),
        AgentStateKind::Failed => ("❌", "Failed"),
        AgentStateKind::Stuck => ("🔴", "Stuck / needs attention"),
        AgentStateKind::Paused => ("⏸️", "Paused / awaiting approval"),
        AgentStateKind::Running => match &status.session_state {
            Some(SessionState::Generating) => ("🔄", "Generating response"),
            Some(SessionState::ExecutingTools) => ("⚙️", "Executing tools"),
            Some(SessionState::Starting) => ("🚀", "Starting up"),
            Some(SessionState::WaitingIde) => ("⏳", "Waiting for IDE"),
            Some(SessionState::Idle) => ("💤", "Idle (waiting)"),
            None => ("❓", "Unknown/offline"),
            _ => ("🔄", "Running"),
        },
    };

    let mut result = format!(
        "### {} {} ({})\n**Status:** {} | **Column:** {} | **Priority:** {} | **Chat:** `{}`\n",
        state_emoji,
        status.card_title,
        status.status_id(),
        state_text,
        status.column,
        status.priority,
        status.agent_chat_id
    );

    if let Some(variant_key) = status.variant_key.as_deref() {
        result.push_str(&format!(
            "**A/B variant:** {}\n",
            variant_key.to_ascii_uppercase()
        ));
    }

    if let Some(last_activity) = status.last_activity_at {
        result.push_str(&format!(
            "**Last activity:** {} ({})\n",
            last_activity.to_rfc3339(),
            format_age_ago(now, last_activity)
        ));
    }
    if let Some(tool_name) = &status.last_tool_name {
        result.push_str(&format!("**Last tool:** `{}`\n", tool_name));
    }
    if let Some(report) = &status.final_report {
        let preview = truncate_chars(report, 300);
        result.push_str(&format!("\n**Final Report:**\n{}\n", preview));
    } else if let Some(update) = &status.last_status_update {
        result.push_str(&format!("\n**Last Update:** {}\n", update));
    }

    if status.room_members.len() > 1 {
        result.push_str("\n**Room members:**\n");
        for member in &status.room_members {
            let chat = member.agent_chat_id.as_deref().unwrap_or("\u{2014}");
            let member_status = member.typed_status().as_str();
            result.push_str(&format!("- {} \u{2014} `{}` ({})\n", member.role, chat, member_status));
        }
    }

    result
}

pub fn format_agent_statuses(
    statuses: &[AgentStatus],
    query: &AgentStatusQuery,
) -> Result<String, String> {
    format_agent_statuses_at(statuses, query, Utc::now())
}

pub fn format_agent_statuses_at(
    statuses: &[AgentStatus],
    query: &AgentStatusQuery,
    now: DateTime<Utc>,
) -> Result<String, String> {
    if matches!(query.format, AgentReportFormat::Delta) && query.since_seq.is_none() {
        return Err("delta format requires since_seq".to_string());
    }

    let alerts = AgentAlerts::from_statuses(statuses, now);
    let mut filtered = filtered_statuses(statuses, query, now);
    sort_statuses(&mut filtered, query.sort, query.group_by, now);

    let total_after_filter = filtered.len();
    let page = paginate_statuses(&filtered, query.offset, query.limit);
    let mut result = format_alerts(&alerts);

    if statuses.is_empty() {
        result.push_str("# Agent Status\n\nNo agents have been spawned yet for this task.\n\nUse `spawn_agent(card_id)` to spawn an agent for a card.");
        return Ok(result);
    }

    match query.format {
        AgentReportFormat::Summary => {
            result.push_str(&format_summary(&filtered, now));
        }
        AgentReportFormat::Detail => {
            result.push_str(&format_detail(&page, total_after_filter, now));
            result.push_str(&format_pagination(
                total_after_filter,
                page.len(),
                query.offset,
                query.limit,
            ));
        }
        AgentReportFormat::Compact => {
            result.push_str(&format_compact(&page, total_after_filter, now, None));
            result.push_str(&format_pagination(
                total_after_filter,
                page.len(),
                query.offset,
                query.limit,
            ));
        }
        AgentReportFormat::Delta => {
            let since_seq = query.since_seq.unwrap_or_default();
            result.push_str(&format_compact(
                &page,
                total_after_filter,
                now,
                Some(since_seq),
            ));
            result.push_str(&format_pagination(
                total_after_filter,
                page.len(),
                query.offset,
                query.limit,
            ));
        }
    }

    Ok(result)
}

pub fn filtered_statuses<'a>(
    statuses: &'a [AgentStatus],
    query: &AgentStatusQuery,
    now: DateTime<Utc>,
) -> Vec<&'a AgentStatus> {
    let mut seen = HashSet::new();
    statuses
        .iter()
        .filter(|status| {
            let status_id = status.status_id();
            if !seen.insert(status_id) {
                return false;
            }
            if matches!(query.format, AgentReportFormat::Delta)
                && status.change_seq <= query.since_seq.unwrap_or_default()
            {
                return false;
            }
            if let Some(card_ids) = &query.card_ids {
                if !card_ids
                    .iter()
                    .any(|card_id| status.matches_card_filter(card_id))
                {
                    return false;
                }
            }
            if let Some(priorities) = &query.priority_filter {
                if !priorities.contains(&status.priority.to_ascii_uppercase()) {
                    return false;
                }
            }
            if let Some(min_age) = query.min_age_minutes {
                let Some(age) = age_minutes(status, now) else {
                    return false;
                };
                if age < min_age {
                    return false;
                }
            }
            if let Some(status_filter) = &query.status_filter {
                if !status_filter.contains(&classify_agent_status(status, now)) {
                    return false;
                }
            }
            true
        })
        .collect()
}

pub fn sort_statuses(
    statuses: &mut Vec<&AgentStatus>,
    sort: AgentSort,
    group_by: AgentGroupBy,
    now: DateTime<Utc>,
) {
    statuses.sort_by(|a, b| {
        let group_cmp = match group_by {
            AgentGroupBy::Status => status_rank(classify_agent_status(a, now))
                .cmp(&status_rank(classify_agent_status(b, now))),
            AgentGroupBy::Priority => priority_rank(&a.priority).cmp(&priority_rank(&b.priority)),
            AgentGroupBy::None => std::cmp::Ordering::Equal,
        };
        if !group_cmp.is_eq() {
            return group_cmp;
        }
        let cmp = match sort {
            AgentSort::Priority => priority_rank(&a.priority).cmp(&priority_rank(&b.priority)),
            AgentSort::LastActivity => last_activity_sort_key(a).cmp(&last_activity_sort_key(b)),
            AgentSort::CardId => a.status_id().cmp(&b.status_id()),
            AgentSort::Status => status_rank(classify_agent_status(a, now))
                .cmp(&status_rank(classify_agent_status(b, now))),
        };
        cmp.then_with(|| a.status_id().cmp(&b.status_id()))
    });
}

pub fn paginate_statuses<'a>(
    statuses: &[&'a AgentStatus],
    offset: usize,
    limit: usize,
) -> Vec<&'a AgentStatus> {
    statuses.iter().skip(offset).take(limit).copied().collect()
}

pub fn last_activity_sort_key(status: &AgentStatus) -> i64 {
    status
        .last_activity_at
        .map(|ts| ts.timestamp())
        .unwrap_or_default()
}

pub fn status_rank(kind: AgentStateKind) -> u8 {
    match kind {
        AgentStateKind::Stuck => 0,
        AgentStateKind::Failed => 1,
        AgentStateKind::Paused => 2,
        AgentStateKind::Running => 3,
        AgentStateKind::Done => 4,
    }
}

pub fn priority_rank(priority: &str) -> u8 {
    match priority.to_ascii_uppercase().as_str() {
        "P0" => 0,
        "P1" => 1,
        "P2" => 2,
        _ => 3,
    }
}

#[derive(Default)]
pub struct AgentAlerts {
    pub stuck: usize,
    pub failed: usize,
    pub paused: usize,
}

impl AgentAlerts {
    pub fn from_statuses(statuses: &[AgentStatus], now: DateTime<Utc>) -> Self {
        let mut alerts = AgentAlerts::default();
        for status in statuses {
            match classify_agent_status(status, now) {
                AgentStateKind::Stuck => alerts.stuck += 1,
                AgentStateKind::Failed => alerts.failed += 1,
                AgentStateKind::Paused => alerts.paused += 1,
                _ => {}
            }
        }
        alerts
    }
}

pub fn format_alerts(alerts: &AgentAlerts) -> String {
    format!(
        "⚠️  Alerts: {} stuck, {} failed, {} needing approval\n\n",
        alerts.stuck, alerts.failed, alerts.paused
    )
}

pub fn format_summary(statuses: &[&AgentStatus], now: DateTime<Utc>) -> String {
    let mut running = 0;
    let mut stuck = 0;
    let mut failed = 0;
    let mut done = 0;
    let mut paused = 0;
    let mut priorities: HashMap<String, usize> = HashMap::new();

    for status in statuses {
        match classify_agent_status(status, now) {
            AgentStateKind::Running => running += 1,
            AgentStateKind::Stuck => stuck += 1,
            AgentStateKind::Failed => failed += 1,
            AgentStateKind::Done => done += 1,
            AgentStateKind::Paused => paused += 1,
        }
        *priorities
            .entry(status.priority.to_ascii_uppercase())
            .or_default() += 1;
    }

    let mut priority_counts = priorities.into_iter().collect::<Vec<_>>();
    priority_counts.sort_by(|(a, _), (b, _)| priority_rank(a).cmp(&priority_rank(b)));
    let priority_text = priority_counts
        .into_iter()
        .map(|(priority, count)| format!("{}={}", priority, count))
        .collect::<Vec<_>>()
        .join(", ");

    format!(
        "Total: {} | Running: {} | Stuck: {} | Failed: {} | Done: {} | Paused: {}\nBy priority: {}\n",
        statuses.len(), running, stuck, failed, done, paused, priority_text
    )
}

pub fn format_detail(
    statuses: &[&AgentStatus],
    total_after_filter: usize,
    now: DateTime<Utc>,
) -> String {
    if statuses.is_empty() {
        return "# Agent Status Detail\n\nNo agent statuses match the requested filters.\n"
            .to_string();
    }

    let mut result = format!(
        "# Agent Status Detail\n\nShowing {} of {} agents.\n\n",
        statuses.len(),
        total_after_filter
    );
    for status in statuses {
        result.push_str(&format_agent_status_detail_at(status, now));
        result.push_str("\n---\n\n");
    }
    result
}

pub fn format_compact(
    statuses: &[&AgentStatus],
    total_after_filter: usize,
    now: DateTime<Utc>,
    since_seq: Option<u64>,
) -> String {
    let mut result = String::new();
    if let Some(seq) = since_seq {
        result.push_str(&format!("Delta since seq {}\n\n", seq));
    }
    if statuses.is_empty() {
        if let Some(seq) = since_seq {
            result.push_str(&format!("No agent changes since seq {}.\n", seq));
        } else {
            result.push_str("No agent statuses match the requested filters.\n");
        }
        return result;
    }
    for status in statuses {
        result.push_str(&format_compact_line(status, now));
        result.push('\n');
    }
    if total_after_filter == 0 {
        result.push_str("No agent statuses match the requested filters.\n");
    }
    result
}

pub fn format_compact_line(status: &AgentStatus, now: DateTime<Utc>) -> String {
    let kind = classify_agent_status(status, now);
    let title = compact_title(&status.card_title, 22);
    let age = status
        .last_activity_at
        .map(|ts| format_age_ago(now, ts))
        .unwrap_or_else(|| "unknown".to_string());
    let short_age = status
        .last_activity_at
        .map(|ts| format_duration_short(now, ts))
        .unwrap_or_else(|| "?".to_string());
    let priority = status.priority.to_ascii_uppercase();
    let last_tool = status.last_tool_name.as_deref().unwrap_or("?");

    match kind {
        AgentStateKind::Stuck => format!(
            "{:<2} 🔴  {:<5} {:<22} | STUCK {:<5} | needs attention",
            priority,
            status.status_id(),
            title,
            short_age
        ),
        AgentStateKind::Failed => format!(
            "{:<2} ❌  {:<5} {:<22} | failed     | {}",
            priority,
            status.status_id(),
            title,
            age
        ),
        AgentStateKind::Done => format!(
            "{:<2} ✅  {:<5} {:<22} | done       | {}",
            priority,
            status.status_id(),
            title,
            age
        ),
        AgentStateKind::Paused => format!(
            "{:<2} ⏸️  {:<5} {:<22} | paused     | {} | approval",
            priority,
            status.status_id(),
            title,
            age
        ),
        AgentStateKind::Running => {
            let (emoji, state_text) = match status.session_state {
                Some(SessionState::ExecutingTools) => ("⚙️", "exec tools"),
                Some(SessionState::Generating) => ("🔄", "generating"),
                Some(SessionState::Starting) => ("🚀", "starting"),
                Some(SessionState::WaitingIde) => ("⏳", "waiting ide"),
                Some(SessionState::Idle) => ("💤", "idle"),
                None => ("❓", "offline"),
                _ => ("🔄", "running"),
            };
            format!(
                "{:<2} {}  {:<5} {:<22} | {:<10} | {:>7} | last: {}",
                priority,
                emoji,
                status.status_id(),
                title,
                state_text,
                age,
                last_tool
            )
        }
    }
}

pub fn compact_title(title: &str, limit: usize) -> String {
    let slug = title
        .split_whitespace()
        .collect::<Vec<_>>()
        .join("-")
        .to_ascii_lowercase();
    truncate_chars(&slug, limit)
}

pub fn age_minutes(status: &AgentStatus, now: DateTime<Utc>) -> Option<i64> {
    status
        .last_activity_at
        .map(|last| now.signed_duration_since(last).num_minutes().max(0))
}

pub fn format_age_ago(now: DateTime<Utc>, timestamp: DateTime<Utc>) -> String {
    let minutes = now.signed_duration_since(timestamp).num_minutes().max(0);
    human_age(Utc::now() - Duration::minutes(minutes))
}

pub fn format_duration_short(now: DateTime<Utc>, timestamp: DateTime<Utc>) -> String {
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

pub fn format_pagination(total: usize, shown: usize, offset: usize, limit: usize) -> String {
    if total == 0 {
        return String::new();
    }
    let next_offset = offset.saturating_add(shown);
    if limit == 0 || next_offset >= total {
        format!("\nshowing {} of {}; no more pages\n", shown, total)
    } else {
        format!(
            "\nshowing {} of {}; pass offset={} for next page\n",
            shown, total, next_offset
        )
    }
}

#[async_trait]
impl Tool for ToolTaskCheckAgents {
    fn tool_description(&self) -> ToolDesc {
        ToolDesc {
            name: "check_agents".to_string(),
            display_name: "Task Check Agents".to_string(),
            source: ToolSource {
                source_type: ToolSourceType::Builtin,
                config_path: String::new(),
            },
            experimental: false,
            allow_parallel: true,
            description: "Check spawned task agents with compact, summary, detail, or delta output. Supports filters, sorting, pagination, and sticky alerts for stuck, failed, or approval-blocked agents.".to_string(),
            input_schema: agent_status_input_schema(),
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
        let ccx_lock = ccx.lock().await;

        let is_planner = ccx_lock
            .task_meta
            .as_ref()
            .map(|m| m.role == "planner")
            .unwrap_or(false);

        if !is_planner {
            return Err("check_agents can only be called by the task planner. \
                 Switch to the planner chat to check agent status."
                .to_string());
        }

        drop(ccx_lock);

        let query = parse_agent_status_query(args)?;
        let task_id = require_bound_planner_task(&ccx, args).await?;
        let (ctx, chat_facade) = {
            let ccx_lock = ccx.lock().await;
            (ccx_lock.app.chat_ctx(), ccx_lock.app.chat.facade.clone())
        };

        let statuses = get_agent_statuses_for_ctx(&ctx, chat_facade, &task_id).await?;
        let result = format_agent_statuses(&statuses, &query)?;

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
