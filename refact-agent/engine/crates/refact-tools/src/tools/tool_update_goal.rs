use std::collections::HashMap;
use std::sync::Arc;
use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::sync::Mutex as AMutex;
use refact_runtime_api::AtCommandsContext;
use refact_core::chat_types::{ChatContent, ChatMessage, ContextEnum};
use refact_chat_api::session_goal_role as goal_role;
use refact_chat_api::internal_roles;
use refact_chat_api::types::{ChatSession, PendingDelivery, PushMode};
use crate::tools::tools_description::{ json_schema_from_params, Tool, ToolDesc, ToolSource, ToolSourceType, };
pub struct ToolUpdateGoal {
    pub config_path: String,
}

#[async_trait]
impl Tool for ToolUpdateGoal {
    fn tool_description(&self) -> ToolDesc {
        ToolDesc {
            name: "update_goal".to_string(),
            display_name: "Update Goal".to_string(),
            source: ToolSource {
                source_type: ToolSourceType::Builtin,
                config_path: self.config_path.clone(),
            },
            experimental: false,
            allow_parallel: false,
            description: "Append an incremental update to the current goal. Use when the goal evolves; it does not rewrite the original goal.".to_string(),
            input_schema: { let mut schema = json_schema_from_params(
                &[("note", "string", "Goal update note. Required.")],
                &["note"],
            ); schema["properties"]["push"] = PushMode::schema(); schema },
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
        let push = PushMode::from_args(args)?;
        let note = string_arg(args, "note")?;
        if note.trim().is_empty() {
            return Err("argument `note` must be non-empty".to_string());
        }

        let (sessions_map, chat_id) = {
            let guard = ccx.lock().await;
            (
                refact_runtime_api::session_handles::sessions_map_from_handle(&guard.tool.chat.sessions),
                guard.chat_id.clone(),
            )
        };
        let session_arc = {
            let sessions = sessions_map.read().await;
            sessions.get(&chat_id).cloned()
        }
        .ok_or_else(|| format!("chat session `{chat_id}` not found"))?;

        let (seq, result_truncation) = {
            let mut session = session_arc.lock().await;
            if !has_base_goal_including_queued(&session)? {
                return Err("no goal to update; call set_goal first".to_string());
            }
            let seq = goal_delta_count_including_queued(&session)? + 1;
            let (delta, result_truncation) = internal_roles::goal_delta_with_truncation(
                "tool.update_goal",
                json!({"seq": seq}),
                note,
            );
            let messages = vec![delta];
            let mut delivery = PendingDelivery::new(messages, push, "tool.update_goal", false);
            delivery.after_tool_call_id = Some(tool_call_id.clone());
            session.enqueue_delivery(delivery)?;
            (seq, result_truncation)
        };

        Ok((
            false,
            vec![ContextEnum::ChatMessage(ChatMessage {
                role: "tool".to_string(),
                content: ChatContent::SimpleText(
                    update_goal_tool_result(seq, result_truncation).to_string(),
                ),
                tool_call_id: tool_call_id.clone(),
                ..Default::default()
            })],
        ))
    }
}

fn update_goal_tool_result(
    seq: usize,
    truncation: Option<internal_roles::PlanDeltaTruncation>,
) -> Value {
    let Some(truncation) = truncation else {
        return json!({"seq": seq, "truncated": false});
    };
    json!({
        "seq": seq,
        "truncated": true,
        "original_chars": truncation.original_chars,
        "kept_chars": truncation.kept_chars,
    })
}

pub fn has_base_goal_including_queued(session: &ChatSession) -> Result<bool, String> {
    Ok(goal_role::current_base_goal(&session.try_accepted_control_projection()?).is_some())
}

fn goal_delta_count_including_queued(session: &ChatSession) -> Result<usize, String> {
    Ok(goal_role::goal_delta_events(&session.try_accepted_control_projection()?).len())
}

fn string_arg(args: &HashMap<String, Value>, name: &str) -> Result<String, String> {
    match args.get(name) {
        Some(Value::String(value)) => Ok(value.clone()),
        Some(value) => Err(format!("argument `{name}` is not a string: {value:?}")),
        None => Err(format!("argument `{name}` is missing")),
    }
}
