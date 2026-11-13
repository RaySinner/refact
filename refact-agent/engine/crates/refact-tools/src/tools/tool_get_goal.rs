use std::collections::HashMap;
use std::sync::Arc;
use async_trait::async_trait;
use refact_chat_api::GoalStatus;
use serde_json::{json, Value};
use tokio::sync::Mutex as AMutex;
use refact_runtime_api::AtCommandsContext;
use refact_core::chat_types::{ChatContent, ChatMessage, ContextEnum};
use refact_chat_api::session_goal_role as goal_role;
use refact_chat_api::ChatSession;
use crate::tools::tools_description::{Tool, ToolDesc, ToolSource, ToolSourceType};
pub struct ToolGetGoal {
    pub config_path: String,
}

impl ToolGetGoal {
    pub fn new(config_path: String) -> Self {
        Self { config_path }
    }
}

fn goal_value(session: &ChatSession, message: &ChatMessage) -> Result<Value, String> {
    let meta = message
        .extra
        .get("goal")
        .ok_or_else(|| "current goal is missing goal metadata".to_string())?;
    let version = meta
        .get("version")
        .and_then(Value::as_u64)
        .ok_or_else(|| "current goal is missing version".to_string())?;
    let current = session
        .goal
        .as_ref()
        .filter(|goal| u64::from(goal.version) == version);
    let progress = current
        .map(|goal| goal.progress.clone())
        .unwrap_or_default();
    let latest = current.and_then(|goal| goal.attempts.last());
    let content = goal_role::synthesize_current_goal(session)
        .ok_or_else(|| "current goal could not be synthesized".to_string())?;
    let delta_count = goal_role::goal_delta_events(session).len();

    Ok(json!({
        "content": content,
        "status": current.map(|goal| goal.status).unwrap_or(GoalStatus::Active),
        "version": version,
        "delta_count": delta_count,
        "turns_used": progress.turns_used,
        "tokens_used": progress.tokens_used,
        "latest_verdict": latest.map(|attempt| &attempt.verdict),
        "gaps": latest.map(|attempt| attempt.gaps.clone()).unwrap_or_default(),
    }))
}

fn output_message(tool_call_id: &str, value: Value) -> Result<ContextEnum, String> {
    let content = serde_json::to_string(&value)
        .map_err(|error| format!("failed to serialize get_goal output: {error}"))?;
    Ok(ContextEnum::ChatMessage(ChatMessage {
        role: "tool".to_string(),
        content: ChatContent::SimpleText(content),
        tool_calls: None,
        tool_call_id: tool_call_id.to_string(),
        ..Default::default()
    }))
}

#[async_trait]
impl Tool for ToolGetGoal {
    fn tool_description(&self) -> ToolDesc {
        ToolDesc {
            name: "get_goal".to_string(),
            display_name: "Get Goal".to_string(),
            source: ToolSource {
                source_type: ToolSourceType::Builtin,
                config_path: self.config_path.clone(),
            },
            experimental: false,
            allow_parallel: true,
            description: "Read the current goal installed on this chat. Returns merged goal content, status, version, delta count, budget counters, latest verifier verdict, and gaps.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {},
                "required": [],
            }),
            output_schema: None,
            annotations: None,
        }
    }

    async fn tool_execute(
        &mut self,
        ccx: Arc<AMutex<AtCommandsContext>>,
        tool_call_id: &String,
        _args: &HashMap<String, Value>,
    ) -> Result<(bool, Vec<ContextEnum>), String> {
        let (sessions, chat_id) = {
            let ccx = ccx.lock().await;
            (ccx.app.chat.sessions.clone(), ccx.chat_id.clone())
        };

        let session_arc = sessions
            .read()
            .await
            .get(&chat_id)
            .cloned()
            .ok_or_else(|| format!("chat session `{chat_id}` not found"))?;
        let session = session_arc.lock().await.try_accepted_control_projection()?;
        let goal = match goal_role::current_base_goal(&session) {
            Some(message) => goal_value(&session, &message)?,
            None => Value::Null,
        };
        Ok((
            false,
            vec![output_message(tool_call_id, json!({ "goal": goal }))?],
        ))
    }
}
