// Task agent failure detection and automatic cleanup
//
// This module monitors task agents and automatically marks them as failed when:
// - Streaming errors occur (network, model, timeout)
// - Agent becomes stuck (no activity beyond threshold)
// - Session ends in Error state without calling agent_finish

use std::path::{Path, PathBuf};

use std::sync::Arc;

use std::time::Duration;

use tokio::sync::Mutex as AMutex;

use chrono::Utc;

use serde_json::json;

use tokio::time::sleep;

use crate::app_state::AppState;

use crate::tasks::storage;

use crate::tasks::types::{BoardCard, StatusUpdate};

use crate::call_validation::{ChatMessage, ChatUsage};

use crate::chat::ctx::ChatCtx;

use crate::chat::internal_roles::{event, EventSubkind};

use crate::chat::retry_policy::{
    RetryDecision, UserErrorCategory, classify_llm_error_for_retry, classify_user_error,
    user_error_info,
};

use crate::chat::types::{ChatSession, SessionState, TaskMeta};

use crate::chat::get_or_create_session_with_trajectory;

use async_trait::async_trait;

use refact_chat_api::agent_notifications::ChatAgentHost;

use refact_core::chat_types::{DeliveryOutcome, PendingDelivery, PushMode};

use crate::worktrees::service::WorktreeService;

use crate::worktrees::git;

use refact_buddy_core::types::BuddyRuntimeEvent;

use uuid::Uuid;

/// Timeout for agent inactivity before considering it stuck (20 minutes)
const AGENT_STUCK_TIMEOUT: Duration = Duration::from_secs(20 * 60);

/// How often to check for stuck agents (2 minutes)
const MONITOR_INTERVAL: Duration = Duration::from_secs(2 * 60);

/// Timeout for in-flight stream stall (Generating with no token activity)
const STREAM_STALL_TIMEOUT: Duration = Duration::from_secs(4 * 60);

const TOOL_STALL_TIMEOUT: Duration = Duration::from_secs(8 * 60);

const MAX_STALL_PLANNER_NOTIFICATIONS_PER_CARD: usize = 2;

const STALL_PLANNER_NOTIFY_GRACE: Duration = Duration::from_secs(60);

const STALL_PLANNER_NOTIFY_COOLDOWN_SECONDS: i64 = 5 * 60;

const STALL_PLANNER_NOTIFY_STATUS_PREFIX: &str = "Planner notified about stall:";

const STALL_AGENT_NOTIFY_STATUS_PREFIX: &str = "Agent notified about stall:";

const STALL_ESCALATION_STATUS_PREFIX: &str = "Stalled agent escalation recorded:";

/// A nudge to the agent is a second line of defence behind the in-turn nudge from
/// `generation.rs`; it gets its own, separate budget so the two never starve
/// each other and so exhausting one never marks the card failed.
const MAX_STALL_AGENT_NOTIFICATIONS_PER_CARD: usize = 2;

const TASK_AGENT_MONITOR_SOURCE: &str = "chat.task_agent_monitor";

pub use refact_chat_api::agent_notifications::{
    notify_planner_agents_finished, notify_planner_blocking_question,
};


mod broadcast;
#[allow(unused_imports)]
pub use broadcast::*;
mod finish;
#[allow(unused_imports)]
pub use finish::*;
mod merge;
#[allow(unused_imports)]
pub use merge::*;
pub use refact_runtime_api::task_agent_monitor::append_card_target_files;
pub use refact_worktrees::git::git_diff_name_only;
mod monitor;
#[allow(unused_imports)]
pub use monitor::*;

#[cfg(test)]
mod tests;
