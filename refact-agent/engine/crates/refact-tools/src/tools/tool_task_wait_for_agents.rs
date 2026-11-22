use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use serde_json::{json, Value};
use tokio::sync::Mutex as AMutex;

use refact_agents_core::types::{AgentListFilter, BgAgentStatus};
use refact_runtime_api::AtCommandsContext;
use refact_core::chat_types::{ChatContent, ChatMessage, ContextEnum};
use refact_chat_api::session::is_background_agent_terminal;
use refact_chat_api::ChatSession;
use crate::tools::task_tool_helpers::require_bound_planner_task;
use crate::tools::tool_task_check_agents::{
    AgentStateKind, AgentStatus, agent_status_input_schema, format_agent_statuses,
    get_agent_statuses_for_ctx, has_active_agent_statuses, parse_agent_status_query,
    waitable_agent_statuses,
};
use crate::tools::tools_description::{Tool, ToolDesc, ToolSource, ToolSourceType};

pub const WAKE_UP_AFTER_SECS_MIN: u64 = 30;
pub const WAKE_UP_AFTER_SECS_MAX: u64 = 1800;
pub const MAX_WAITING_CARD_IDS: usize = 50;
pub const MAX_WAITING_AGENT_IDS: usize = 50;

pub fn wait_for_agents_input_schema() -> Value {
    let mut schema = agent_status_input_schema();
    if let Some(props) = schema.get_mut("properties").and_then(|p| p.as_object_mut()) {
        props.insert(
            "wake_up_after_secs".to_string(),
            json!({
                "type": "integer",
                "minimum": 30,
                "maximum": 1800,
                "description": "If set, the backend will wake the planner after this many seconds (30-1800) if no agent reports back. Recommended for long-running cards."
            }),
        );
    }
    schema
}

pub fn parse_wake_up_after_secs(
    args: &HashMap<String, Value>,
) -> Result<Option<u64>, String> {
    let Some(value) = args.get("wake_up_after_secs") else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let secs = value
        .as_u64()
        .ok_or_else(|| "wake_up_after_secs must be a non-negative integer".to_string())?;
    if secs < WAKE_UP_AFTER_SECS_MIN {
        return Err(format!(
            "wake_up_after_secs must be at least {} seconds",
            WAKE_UP_AFTER_SECS_MIN
        ));
    }
    if secs > WAKE_UP_AFTER_SECS_MAX {
        return Err(format!(
            "wake_up_after_secs must be at most {} seconds",
            WAKE_UP_AFTER_SECS_MAX
        ));
    }
    Ok(Some(secs))
}

pub fn natural_card_id_key(id: &str) -> (String, u64) {
    match id.rfind('-') {
        Some(pos) => {
            let prefix = &id[..pos];
            let suffix = &id[pos + 1..];
            match suffix.parse::<u64>() {
                Ok(n) => (prefix.to_string(), n),
                Err(_) => (id.to_string(), 0),
            }
        }
        None => (id.to_string(), 0),
    }
}

pub fn resolve_waiting_card_ids(statuses: &[&AgentStatus]) -> Vec<String> {
    let mut ids: Vec<String> = statuses.iter().map(|s| s.status_id()).collect();
    ids.sort_by(|a, b| natural_card_id_key(a).cmp(&natural_card_id_key(b)));
    ids.truncate(MAX_WAITING_CARD_IDS);
    ids
}

pub fn resolve_waiting_agent_ids(statuses: &[(String, BgAgentStatus)]) -> Vec<String> {
    let mut ids: Vec<String> = statuses
        .iter()
        .filter(|(_, status)| !status.is_terminal())
        .map(|(agent_id, _)| agent_id.clone())
        .collect();
    ids.sort_by(|a, b| natural_card_id_key(a).cmp(&natural_card_id_key(b)));
    ids.truncate(MAX_WAITING_AGENT_IDS);
    ids
}

fn background_agent_state_kind(status: BgAgentStatus) -> AgentStateKind {
    match status {
        BgAgentStatus::Queued | BgAgentStatus::Running => AgentStateKind::Running,
        BgAgentStatus::WaitingForApproval => AgentStateKind::Paused,
        BgAgentStatus::Completed => AgentStateKind::Done,
        BgAgentStatus::Failed | BgAgentStatus::Cancelled | BgAgentStatus::Interrupted => {
            AgentStateKind::Failed
        }
    }
}

fn background_agent_matches_status_filter(
    status: BgAgentStatus,
    status_filter: Option<&HashSet<AgentStateKind>>,
) -> bool {
    status_filter.map_or(true, |filters| {
        filters.contains(&background_agent_state_kind(status))
    })
}

pub fn status_filter_was_explicit(args: &HashMap<String, Value>) -> bool {
    args.get("status_filter")
        .is_some_and(|value| !value.is_null())
}

pub fn replace_session_wake_up_at(session: &mut ChatSession, wake_up_after_secs: Option<u64>) {
    session.wake_up_at =
        wake_up_after_secs.map(|secs| Utc::now() + chrono::Duration::seconds(secs as i64));
}

pub fn register_wait_targets(
    session: &mut ChatSession,
    wake_up_after_secs: Option<u64>,
    card_ids: Vec<String>,
    agent_ids: Vec<String>,
) {
    replace_session_wake_up_at(session, wake_up_after_secs);
    session.waiting_for_card_ids = card_ids;
    let terminal_agent_ids: HashSet<String> = session
        .background_agents
        .values()
        .filter(|agent| is_background_agent_terminal(&agent.status))
        .map(|agent| agent.agent_id.clone())
        .collect();
    session.waiting_for_agent_ids = agent_ids
        .into_iter()
        .filter(|agent_id| !terminal_agent_ids.contains(agent_id.as_str()))
        .collect();
    session.mark_persisted_runtime_changed();
}

fn request_planner_turn_stop(abort_flag: &AtomicBool) {
    abort_flag.store(true, Ordering::SeqCst);
}

pub struct ToolTaskWaitForAgents;

impl ToolTaskWaitForAgents {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for ToolTaskWaitForAgents {
    fn tool_description(&self) -> ToolDesc {
        ToolDesc {
            name: "wait_agents".to_string(),
            display_name: "Task Wait For Agents".to_string(),
            source: ToolSource {
                source_type: ToolSourceType::Builtin,
                config_path: String::new(),
            },
            experimental: false,
            allow_parallel: false,
            description: "Check spawned task agents and standalone background children, then stop the planner turn so it waits for their completion messages. Pass wake_up_after_secs to set an auto-wake timer (30-1800 s).".to_string(),
            input_schema: wait_for_agents_input_schema(),
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
            return Err("wait_agents can only be called by the task planner. \
                 Switch to the planner chat to check agent status."
                .to_string());
        }

        drop(ccx_lock);

        let wake_up_secs = parse_wake_up_after_secs(args)?;

        let query = parse_agent_status_query(args)?;
        let task_id = require_bound_planner_task(&ccx, args).await?;
        let (ctx, chat_facade, chat_id, app) = {
            let ccx_lock = ccx.lock().await;
            (
                ccx_lock.app.chat_ctx(),
                ccx_lock.app.chat.facade.clone(),
                ccx_lock.chat_id.clone(),
                ccx_lock.app.clone(),
            )
        };

        let status_filter_explicit = status_filter_was_explicit(args);
        let background_status_filter = if status_filter_explicit {
            query.status_filter.as_ref()
        } else {
            None
        };
        let statuses = get_agent_statuses_for_ctx(&ctx, chat_facade, &task_id).await?;
        let mut result = format_agent_statuses(&statuses, &query)?;
        let waitable = waitable_agent_statuses(&statuses, &query, status_filter_explicit);
        let resolved_card_ids = resolve_waiting_card_ids(&waitable);
        let background_agents = app
            .agents
            .list_for_parent(&chat_id, AgentListFilter::default())
            .await;
        let background_agent_statuses: Vec<(String, BgAgentStatus)> = background_agents
            .iter()
            .filter(|agent| {
                background_agent_matches_status_filter(agent.status, background_status_filter)
            })
            .map(|agent| (agent.agent_id.clone(), agent.status))
            .collect();
        let resolved_agent_ids = resolve_waiting_agent_ids(&background_agent_statuses);

        if !resolved_card_ids.is_empty() || !resolved_agent_ids.is_empty() {
            result.push_str("\n⏳ **Agents are still working.** Do not check again, wait for the completion message to arrive.\n");
            if !resolved_agent_ids.is_empty() {
                result.push_str(&format!(
                    "Standalone background agents still working: {}.\n",
                    resolved_agent_ids.join(", ")
                ));
            }
        } else if statuses.is_empty() && background_agents.is_empty() {
            result.push_str("\nNo agents are currently running.\n");
        } else if has_active_agent_statuses(&statuses) {
            result.push_str("\nNo matching live agents are running or awaiting approval.\n");
        } else {
            result.push_str("\nNo agents are currently running.\n");
        }

        if resolved_card_ids.is_empty() && resolved_agent_ids.is_empty() {
            result.push_str("\nNo matching live agents are running or awaiting approval; planner wait mode was not entered.\n");
            return Ok((
                false,
                vec![ContextEnum::ChatMessage(ChatMessage {
                    role: "tool".to_string(),
                    content: ChatContent::SimpleText(result),
                    tool_calls: None,
                    tool_call_id: tool_call_id.clone(),
                    tool_failed: Some(true),
                    ..Default::default()
                })],
            ));
        }

        {
            let sessions = app.chat.sessions.read().await;
            if let Some(session_arc) = sessions.get(&chat_id) {
                let mut session = session_arc.lock().await;
                register_wait_targets(
                    &mut session,
                    wake_up_secs,
                    resolved_card_ids,
                    resolved_agent_ids,
                );
            }
        }

        {
            let ccx_lock = ccx.lock().await;
            request_planner_turn_stop(&ccx_lock.abort_flag);
        }

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standalone_only_wait_registers_agent_and_turn_stop() {
        let statuses = vec![("bgagent-a".to_string(), BgAgentStatus::Running)];
        let agent_ids = resolve_waiting_agent_ids(&statuses);
        assert_eq!(agent_ids, vec!["bgagent-a".to_string()]);

        let mut session = ChatSession::new("planner".to_string());
        register_wait_targets(&mut session, Some(60), Vec::new(), agent_ids);
        assert!(session.waiting_for_card_ids.is_empty());
        assert_eq!(session.waiting_for_agent_ids, vec!["bgagent-a".to_string()]);
        assert!(session.wake_up_at.is_some());

        let abort_flag = AtomicBool::new(false);
        request_planner_turn_stop(&abort_flag);
        assert!(abort_flag.load(Ordering::SeqCst));
    }

    #[test]
    fn card_and_standalone_wait_registrations_are_independent() {
        let mut session = ChatSession::new("planner".to_string());
        register_wait_targets(
            &mut session,
            Some(30),
            vec!["T-1".to_string()],
            vec!["bgagent-a".to_string()],
        );

        assert_eq!(session.waiting_for_card_ids, vec!["T-1".to_string()]);
        assert_eq!(session.waiting_for_agent_ids, vec!["bgagent-a".to_string()]);
    }

    #[test]
    fn terminal_standalone_agent_is_not_waitable() {
        let statuses = vec![
            ("bgagent-a".to_string(), BgAgentStatus::Completed),
            ("bgagent-b".to_string(), BgAgentStatus::Failed),
        ];
        assert!(resolve_waiting_agent_ids(&statuses).is_empty());
    }

    #[test]
    fn running_standalone_agent_keeps_its_timeout_registration() {
        let statuses = vec![("bgagent-a".to_string(), BgAgentStatus::Running)];
        let mut session = ChatSession::new("planner".to_string());
        register_wait_targets(
            &mut session,
            Some(120),
            Vec::new(),
            resolve_waiting_agent_ids(&statuses),
        );

        assert_eq!(session.waiting_for_agent_ids, vec!["bgagent-a".to_string()]);
        assert!(session.wake_up_at.is_some());
    }
}
