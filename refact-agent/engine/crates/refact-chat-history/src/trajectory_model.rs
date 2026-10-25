use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};
use serde::{Deserialize, Serialize};

use refact_chat_api::{
    ChatMessage, FrozenRequestPrefix, GoalLedgerEntry, GoalSnapshot, GoalStatus, ThreadParams,
    TrajectorySourceIdentity, WorktreeMeta,
};
use crate::trajectory_event::TrajectoryEvent;
use crate::trajectory_metrics::trajectory_meta_title;

pub const INTERNAL_TRACE_LINK_PREFIX: &str = "internal:";

pub fn is_internal_trace_link_type(link_type: Option<&str>) -> bool {
    link_type.is_some_and(|value| value.starts_with(INTERNAL_TRACE_LINK_PREFIX))
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TrajectoryMeta {
    pub id: String,
    pub title: String,
    pub created_at: String,
    pub updated_at: String,
    pub model: String,
    pub mode: String,
    pub message_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub link_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub card_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub root_chat_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<WorktreeMeta>,
    #[serde(default)]
    pub total_lines_added: i64,
    #[serde(default)]
    pub total_lines_removed: i64,
    #[serde(default)]
    pub tasks_total: i32,
    #[serde(default)]
    pub tasks_done: i32,
    #[serde(default)]
    pub tasks_failed: i32,
    #[serde(default)]
    pub total_prompt_tokens: u64,
    #[serde(default)]
    pub total_completion_tokens: u64,
    #[serde(default)]
    pub total_tokens: u64,
    #[serde(default)]
    pub total_cache_read_tokens: u64,
    #[serde(default)]
    pub total_cache_creation_tokens: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_cost_usd: Option<f64>,
    #[serde(skip)]
    pub source: TrajectorySourceIdentity,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TrajectoryData {
    pub id: String,
    pub title: String,
    pub created_at: String,
    pub updated_at: String,
    pub model: String,
    pub mode: String,
    pub tool_use: String,
    pub messages: Vec<serde_json::Value>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct TrajectoryListData {
    pub id: String,
    pub updated_at: String,
    pub mode: Option<String>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

pub struct TrajectoryListCandidate {
    pub id: String,
    pub updated_at: String,
    pub path: PathBuf,
    pub indexed_meta: Option<TrajectoryMeta>,
    pub indexed_file_len: Option<u64>,
    pub indexed_file_modified_unix_ms: Option<i64>,
}

pub fn trajectory_list_main_link_type(link_type: Option<&str>) -> bool {
    matches!(link_type, Some("handoff" | "mode_transition" | "branch"))
}

pub fn trajectory_event_is_displayable_chat(event: &TrajectoryEvent) -> bool {
    if matches!(
        event.mode.as_deref(),
        Some("task_agent" | "task_planner" | "buddy")
    ) {
        return false;
    }
    if is_internal_trace_link_type(event.link_type.as_deref()) {
        return false;
    }
    if (event.link_type.is_some() || event.parent_id.is_some())
        && !trajectory_list_main_link_type(event.link_type.as_deref())
    {
        return false;
    }

    true
}

pub fn trajectory_list_data_is_displayable_chat(data: &TrajectoryListData) -> bool {
    if data.extra.get("buddy_meta").is_some_and(|v| !v.is_null()) {
        return false;
    }
    if matches!(data.mode.as_deref(), Some("task_agent" | "task_planner")) {
        return false;
    }

    let parent_id = data.extra.get("parent_id").and_then(|v| v.as_str());
    let link_type = data.extra.get("link_type").and_then(|v| v.as_str());
    if is_internal_trace_link_type(link_type) {
        return false;
    }
    if (link_type.is_some() || parent_id.is_some()) && !trajectory_list_main_link_type(link_type) {
        return false;
    }

    true
}

#[derive(Clone)]
pub struct LoadedTrajectory {
    pub source_path: PathBuf,
    pub messages: Vec<ChatMessage>,
    pub thread: ThreadParams,
    pub goal: Option<GoalSnapshot>,
    pub goal_ledger: Vec<GoalLedgerEntry>,
    pub goal_verification_blocked_until_ms: Option<u64>,
    pub created_at: String,
    pub updated_at: String,
    pub wake_up_at: Option<chrono::DateTime<chrono::Utc>>,
    pub waiting_for_card_ids: Vec<String>,
    pub pending_deliveries: Vec<refact_core::chat_types::PendingDelivery>,
    pub auto_approve_editing_tools_present: bool,
    pub auto_approve_dangerous_commands_present: bool,
    pub transition_identity_repaired: bool,
}

#[derive(Clone)]
pub struct TrajectoryRepairPatch {
    pub chat_id: String,
    pub source_path: PathBuf,
    pub created_at: String,
    pub frozen_request_prefix: Option<FrozenRequestPrefix>,
    pub auto_approve_editing_tools: bool,
    pub auto_approve_dangerous_commands: bool,
}

impl LoadedTrajectory {
    pub fn repair_patch(&self) -> TrajectoryRepairPatch {
        TrajectoryRepairPatch {
            chat_id: self.thread.id.clone(),
            source_path: self.source_path.clone(),
            created_at: self.created_at.clone(),
            frozen_request_prefix: self.thread.frozen_request_prefix.clone(),
            auto_approve_editing_tools: self.thread.auto_approve_editing_tools,
            auto_approve_dangerous_commands: self.thread.auto_approve_dangerous_commands,
        }
    }
}

pub fn clamp_goal_snapshot_for_load(mut goal: GoalSnapshot) -> GoalSnapshot {
    goal.budget = goal.budget.migrate_legacy_default_hard_limits();
    if let Some(max_turns) = goal.budget.max_turns.filter(|limit| *limit > 0) {
        goal.progress.turns_used = goal.progress.turns_used.min(max_turns);
    }
    if let Some(max_tokens) = goal.budget.max_tokens.filter(|limit| *limit > 0) {
        goal.progress.tokens_used = goal.progress.tokens_used.min(max_tokens);
    }
    if let Some(no_progress_turns) = goal.budget.no_progress_turns.filter(|limit| *limit > 0) {
        goal.progress.no_progress_turns = goal.progress.no_progress_turns.min(no_progress_turns);
    }
    if goal.status == GoalStatus::Verifying {
        goal.status = if goal.active {
            GoalStatus::Active
        } else {
            GoalStatus::Paused
        };
    }
    if matches!(
        goal.status,
        GoalStatus::BudgetExhausted | GoalStatus::NoProgress
    ) && refact_chat_api::goal_budget_exhaustion_status(
        &goal.budget,
        &goal.progress,
        epoch_ms_now(),
    )
    .is_none()
        && !goal_budget_has_hard_limits_for_load(&goal.budget)
    {
        goal.status = if goal.active {
            GoalStatus::Active
        } else {
            GoalStatus::Paused
        };
    }
    goal
}

pub fn goal_budget_has_hard_limits_for_load(budget: &refact_chat_api::GoalBudget) -> bool {
    budget.max_turns.is_some_and(|limit| limit > 0)
        || budget.max_minutes.is_some_and(|limit| limit > 0)
        || budget.max_tokens.is_some_and(|limit| limit > 0)
        || budget.max_cost_cents.is_some_and(|limit| limit > 0)
        || budget.no_progress_turns.is_some_and(|limit| limit > 0)
}

pub fn epoch_ms_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}
