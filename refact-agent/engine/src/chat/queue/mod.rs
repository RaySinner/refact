mod dispatch;
mod helpers;
mod setparams;

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests2;
#[cfg(test)]
mod tests_common;


use std::collections::{BTreeSet, VecDeque};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use chrono::Utc;
use tokio::sync::{Mutex as AMutex};
use tracing::warn;
use uuid::Uuid;

use crate::app_state::AppState;
use crate::buddy::chat_reactions::{maybe_enqueue_chat_reaction, AcceptedUserMessage};
use crate::call_validation::{ChatContent, ChatMessage, ContextFile};
use refact_buddy_core::user_action::UserAction;
use crate::files_correction::get_project_dirs;
use crate::ext::hooks::HookEvent;
use crate::ext::hooks_runner::{HookPayload, first_block_reason, get_project_dir_string, run_hooks};
use crate::chat::internal_roles::{self, mode_switch_event_with_diff, EventSubkind};
use crate::chat::plan_role;
use crate::yaml_configs::customization_registry::get_mode_config;
use crate::yaml_configs::customization_types::ModeConfig;

use crate::chat::types::*;
use crate::chat::browser_context;
use crate::chat::content::parse_content_with_attachments;
use crate::chat::generation::{
    start_generation, prepare_session_preamble_and_knowledge, maybe_save_trajectory_with_intent,
};
use crate::chat::goal_verifier::{
    should_verify_goal_on_done, verify_goal_before_completion, GoalCompletionGateOutcome,
};
use crate::chat::tools::{
    acquire_session_turn_tool_pool, execute_tools_with_session,
    resolve_tool_call_aliases_with_catalog,
};
use crate::chat::trajectories::{maybe_save_trajectory_background_with_intent};
use crate::ext::slash_expand::expand_slash_command;
use crate::ext::skills_context::{expand_skill_includes, SKILLS_CONTEXT_MARKER};
use crate::worktrees::service::WorktreeService;
use crate::worktrees::types::{WorktreeMeta, WorktreeReference};

use dispatch::*;
use helpers::*;
use setparams::*;

pub use dispatch::process_command_queue;
pub use helpers::{
    drain_non_priority_user_messages, drain_priority_user_messages, drain_queued_user_messages,
    find_allowed_command_while_paused, find_allowed_command_while_waiting_ide,
    inject_queued_user_messages_if_any,
    worktree_activation_message, worktree_disabled_message,
};
pub use setparams::{apply_setparams_patch, resolve_worktree_setparams_update, WorktreeSetParamsUpdate};
pub(crate) use helpers::user_message_with_client_message_id;
pub(crate) use setparams::add_mode_switch_event_and_plan_if_changed;
