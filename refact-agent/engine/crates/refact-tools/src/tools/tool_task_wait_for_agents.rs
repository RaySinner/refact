use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use serde_json::{json, Value};
use tokio::sync::Mutex as AMutex;

use refact_runtime_api::AtCommandsContext;
use refact_core::chat_types::{ChatContent, ChatMessage, ContextEnum};
use refact_chat_api::ChatSession;
use crate::tools::task_tool_helpers::require_bound_planner_task;
use crate::tools::tool_task_check_agents::{
    AgentStatus, agent_status_input_schema, format_agent_statuses, get_agent_statuses_for_ctx,
    has_active_agent_statuses, parse_agent_status_query, waitable_agent_statuses,
};
use crate::tools::tools_description::{Tool, ToolDesc, ToolSource, ToolSourceType};

pub const WAKE_UP_AFTER_SECS_MIN: u64 = 30;
pub const WAKE_UP_AFTER_SECS_MAX: u64 = 1800;
pub const MAX_WAITING_CARD_IDS: usize = 50;

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

pub fn status_filter_was_explicit(args: &HashMap<String, Value>) -> bool {
    args.get("status_filter")
        .is_some_and(|value| !value.is_null())
}

pub fn replace_session_wake_up_at(session: &mut ChatSession, wake_up_after_secs: Option<u64>) {
    session.wake_up_at =
        wake_up_after_secs.map(|secs| Utc::now() + chrono::Duration::seconds(secs as i64));
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
            description: "Check spawned task agents using the same compact status view as check_agents, then stop the planner turn so it waits for agent completion messages. Pass wake_up_after_secs to set an auto-wake timer (30-1800 s).".to_string(),
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

        let statuses = get_agent_statuses_for_ctx(&ctx, chat_facade, &task_id).await?;
        let mut result = format_agent_statuses(&statuses, &query)?;
        let waitable = waitable_agent_statuses(&statuses, &query, status_filter_was_explicit(args));
        let resolved_card_ids = resolve_waiting_card_ids(&waitable);

        if statuses.is_empty() {
            result.push_str("\nNo agents are currently running.\n");
        } else if !resolved_card_ids.is_empty() {
            result.push_str("\n⏳ **Agents are still working.** Do not check again, wait for the completion message to arrive.\n");
        } else if has_active_agent_statuses(&statuses) {
            result.push_str("\nNo matching live agents are running or awaiting approval.\n");
        } else {
            result.push_str("\nNo agents are currently running.\n");
        }

        if resolved_card_ids.is_empty() {
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
                replace_session_wake_up_at(&mut session, wake_up_secs);
                session.waiting_for_card_ids = resolved_card_ids;
                session.mark_persisted_runtime_changed();
            }
        }

        {
            let ccx_lock = ccx.lock().await;
            ccx_lock
                .abort_flag
                .store(true, std::sync::atomic::Ordering::SeqCst);
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
