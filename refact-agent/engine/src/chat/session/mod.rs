use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;
use serde_json::json;
use tokio::sync::{broadcast, Mutex as AMutex, Notify, RwLock as ARwLock};
use tracing::{info, warn};
use uuid::Uuid;

use crate::call_validation::{ChatContent, ChatMessage, ChatUsage};
use crate::chat::diagnostics::{make_ui_only_error_message, make_ui_only_error_message_with_info};
use crate::chat::internal_roles::{event, EventSubkind, GOAL_ROLE};
use crate::chat::perf_diagnostics::{self, PerfComponent, PerfOutcome};

use super::types::*;
use super::types::session_idle_timeout;
use super::config::limits;
use super::trajectories::{task_context_from_task_meta, trajectory_meta_title, TrajectoryEvent};

pub use super::session_runtime::{
    clean_background_processes_for_chat, close_all_chat_sessions,
    get_or_create_session_with_trajectory, snapshot_with_agents, start_session_cleanup_task,
    try_restore_session_if_trajectory_exists,
};

mod construct;
mod goal;
mod messages;
mod queue;
mod runtime;
mod snapshot;
mod stream;
#[cfg(test)]
mod tests_goal;
#[cfg(test)]
mod tests_messages;
#[cfg(test)]
mod tests_misc;
#[cfg(test)]
mod tests_queue;
#[cfg(test)]
mod tests_runtime;
#[cfg(test)]
mod tests_snapshot;
#[cfg(test)]
mod tests_stream;
#[cfg(test)]
mod tests_support;
#[cfg(test)]
mod tests_tool_decisions;

use construct::*;
use goal::*;
use messages::*;
use queue::*;
use runtime::*;
use snapshot::*;
use stream::*;
pub(crate) use goal::goal_snapshot_from_messages;

fn stamp_created_at_ms(message: &mut ChatMessage) {
    if message
        .extra
        .get("created_at_ms")
        .and_then(|v| v.as_u64())
        .is_none_or(|v| v == 0)
    {
        message.extra.insert("created_at_ms".into(), json!(epoch_ms_now()));
    }
}

pub(crate) use refact_chat_api::chat_local_types::has_displayable_assistant_content;

fn is_background_agent_terminal(status: &str) -> bool {
    matches!(status, "completed" | "failed" | "cancelled" | "interrupted")
}

fn is_terminal_runtime_state(state: SessionState) -> bool {
    matches!(
        state,
        SessionState::Idle
            | SessionState::Completed
            | SessionState::Error
            | SessionState::WaitingUserInput
    )
}

fn is_active_compression_phase(phase: Option<CompressionPhase>) -> bool {
    matches!(
        phase,
        Some(CompressionPhase::Checking | CompressionPhase::Running)
    )
}

fn epoch_ms_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

pub(super) fn should_replace_background_agent(
    existing: Option<&BackgroundAgentSummary>,
    incoming: &BackgroundAgentSummary,
) -> bool {
    match existing {
        None => true,
        Some(existing) if incoming.change_seq > existing.change_seq => true,
        Some(existing) if incoming.change_seq < existing.change_seq => false,
        Some(existing) => {
            is_background_agent_terminal(&incoming.status)
                && !is_background_agent_terminal(&existing.status)
        }
    }
}

pub type SessionsMap = Arc<ARwLock<HashMap<String, Arc<AMutex<ChatSession>>>>>;

pub fn create_sessions_map() -> SessionsMap {
    Arc::new(ARwLock::new(HashMap::new()))
}

pub struct ToolDecisionOutcome {
    pub accepted_ids: Vec<String>,
    pub denied_ids: Vec<String>,
}

fn tool_decision_message(decision: &str, tool_call_ids: Vec<String>, scope: &str) -> ChatMessage {
    let count = tool_call_ids.len();
    let verb = if decision == "approve" {
        "approved"
    } else {
        "rejected"
    };
    let noun = if count == 1 {
        "tool call"
    } else {
        "tool calls"
    };
    event(
        EventSubkind::ToolDecision,
        "chat.session",
        json!({
            "tool_call_ids": tool_call_ids,
            "decision": decision,
            "scope": scope,
        }),
        format!("User {verb} {count} {noun} ({scope})"),
    )
}

fn background_process_cleanup_notice(killed_count: usize) -> ChatMessage {
    event(
        EventSubkind::SystemNotice,
        "chat.session",
        json!({ "killed_count": killed_count }),
        format!("Cleared {killed_count} background processes from this chat"),
    )
}
