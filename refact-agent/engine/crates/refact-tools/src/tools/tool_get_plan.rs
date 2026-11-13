use std::collections::HashMap;
use std::sync::Arc;
use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::sync::Mutex as AMutex;
use refact_runtime_api::AtCommandsContext;
use refact_core::chat_types::{ChatContent, ChatMessage, ContextEnum};
use refact_chat_api::plan_role;
use refact_chat_api::session_plan_role;
use refact_chat_api::ChatSession;
use crate::tools::tools_description::{Tool, ToolDesc, ToolSource, ToolSourceType};
pub struct ToolGetPlan {
    pub config_path: String,
}

impl ToolGetPlan {
    pub fn new(config_path: String) -> Self {
        Self { config_path }
    }
}

fn plan_value(session: &ChatSession, message: &ChatMessage) -> Result<Value, String> {
    let meta = message
        .extra
        .get("plan")
        .ok_or_else(|| "current plan is missing plan metadata".to_string())?;
    let mode = meta
        .get("mode")
        .and_then(Value::as_str)
        .ok_or_else(|| "current plan is missing mode".to_string())?;
    let version = meta
        .get("version")
        .and_then(Value::as_u64)
        .ok_or_else(|| "current plan is missing version".to_string())?;
    let created_at_ms = meta
        .get("created_at_ms")
        .and_then(Value::as_u64)
        .ok_or_else(|| "current plan is missing created_at_ms".to_string())?;
    let content = plan_role::try_synthesize_current_plan(session)?
        .ok_or_else(|| "current plan could not be synthesized".to_string())?;
    let delta_count = plan_role::try_plan_delta_events(session)?.len();

    Ok(json!({
        "content": content,
        "mode": mode,
        "version": version,
        "created_at_ms": created_at_ms,
        "delta_count": delta_count,
    }))
}

fn output_message(tool_call_id: &str, value: Value) -> Result<ContextEnum, String> {
    let content = serde_json::to_string(&value)
        .map_err(|error| format!("failed to serialize get_plan output: {error}"))?;
    Ok(ContextEnum::ChatMessage(ChatMessage {
        role: "tool".to_string(),
        content: ChatContent::SimpleText(content),
        tool_calls: None,
        tool_call_id: tool_call_id.to_string(),
        ..Default::default()
    }))
}

#[async_trait]
impl Tool for ToolGetPlan {
    fn tool_description(&self) -> ToolDesc {
        ToolDesc {
            name: "get_plan".to_string(),
            display_name: "Get Plan".to_string(),
            source: ToolSource {
                source_type: ToolSourceType::Builtin,
                config_path: self.config_path.clone(),
            },
            experimental: false,
            allow_parallel: true,
            description: "Read the current plan installed on this chat. Returns the merged current content, mode, base version, creation timestamp, and delta count.".to_string(),
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
        let plan = match session_plan_role::try_current_base_plan(&session)? {
            Some(message) => plan_value(&session, &message)?,
            None => Value::Null,
        };
        Ok((
            false,
            vec![output_message(tool_call_id, json!({ "plan": plan }))?],
        ))
    }
}
