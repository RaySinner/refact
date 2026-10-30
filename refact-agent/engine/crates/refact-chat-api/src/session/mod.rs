use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Instant;

use serde_json::json;
use tokio::sync::{broadcast, Mutex as AMutex, Notify};
use tracing::{info, warn};
use uuid::Uuid;

use refact_core::chat_config::limits;
use refact_core::chat_types::{ChatContent, ChatMessage, ChatUsage};
use refact_perf::perf_diagnostics::{self, PerfComponent, PerfOutcome};

use crate::chat_local_types::{
    has_displayable_assistant_content, max_queue_size, session_idle_timeout,
};
use crate::compression_gate::compression_attempt_active;
use crate::diagnostics::{make_ui_only_error_message, make_ui_only_error_message_with_info};
use crate::internal_roles::{event, EventSubkind, GOAL_ROLE};
use crate::setparams::apply_setparams_patch;
use crate::trajectory_event::TrajectoryEvent;
use crate::trajectory_meta::{task_context_from_task_meta, trajectory_meta_title};
use crate::types::*;
pub use crate::types::epoch_ms_now;
use crate::wait_interrupt::WaitInterruptCause;

mod construct;
mod goal;
mod messages;
mod queue;
mod roles;
mod runtime;
mod snapshot;
mod stream;

pub use goal::{
    apply_goal_runtime_projection, goal_budget_has_hard_limits, goal_event_subkind,
    goal_events_from_messages, goal_runtime_projection, goal_snapshot_from_messages,
    is_goal_projection_event, message_affects_goal_projection, synthesized_goal_content,
    tool_result_counts_as_goal_evidence,
};
pub use snapshot::sse_broadcast_outcome;
pub use stream::StreamStartRefusal;

pub fn stamp_created_at_ms(message: &mut ChatMessage) {
    if message
        .extra
        .get("created_at_ms")
        .and_then(|v| v.as_u64())
        .is_none_or(|v| v == 0)
    {
        message
            .extra
            .insert("created_at_ms".into(), json!(epoch_ms_now()));
    }
}

pub fn is_background_agent_terminal(status: &str) -> bool {
    matches!(status, "completed" | "failed" | "cancelled" | "interrupted")
}

pub fn is_terminal_runtime_state(state: SessionState) -> bool {
    matches!(
        state,
        SessionState::Idle
            | SessionState::Completed
            | SessionState::Error
            | SessionState::WaitingUserInput
    )
}

pub fn is_active_compression_phase(phase: Option<CompressionPhase>) -> bool {
    matches!(
        phase,
        Some(CompressionPhase::Checking | CompressionPhase::Running)
    )
}

pub fn should_replace_background_agent(
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

pub struct ToolDecisionOutcome {
    pub accepted_ids: Vec<String>,
    pub denied_ids: Vec<String>,
}

pub fn tool_decision_message(
    decision: &str,
    tool_call_ids: Vec<String>,
    scope: &str,
) -> ChatMessage {
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

pub fn background_process_cleanup_notice(killed_count: usize) -> ChatMessage {
    event(
        EventSubkind::SystemNotice,
        "chat.session",
        json!({ "killed_count": killed_count }),
        format!("Cleared {killed_count} background processes from this chat"),
    )
}
