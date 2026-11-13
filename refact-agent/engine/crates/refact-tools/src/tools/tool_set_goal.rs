use std::collections::HashMap;
use std::sync::Arc;
use async_trait::async_trait;
use refact_chat_api::GoalBudget;
use serde_json::{json, Value};
use tokio::sync::Mutex as AMutex;
use refact_runtime_api::AtCommandsContext;
use refact_core::chat_types::{ChatContent, ChatMessage, ContextEnum};
use refact_chat_api::goal_role::GoalInstallReport;
use refact_chat_api::session_goal_role as goal_role;
use refact_chat_api::internal_roles::{self, EventSubkind};
use refact_chat_api::types::{ChatSession, PendingDelivery, PushMode};
use crate::tools::tools_description::{Tool, ToolDesc, ToolSource, ToolSourceType};
use refact_chat_api::setparams::map_legacy_mode_to_id;
pub struct ToolSetGoal {
    pub config_path: String,
}

#[async_trait]
impl Tool for ToolSetGoal {
    fn tool_description(&self) -> ToolDesc {
        ToolDesc {
            name: "set_goal".to_string(),
            display_name: "Set Goal".to_string(),
            source: ToolSource {
                source_type: ToolSourceType::Builtin,
                config_path: self.config_path.clone(),
            },
            experimental: false,
            allow_parallel: false,
            description: "Install the chat's single active goal. Fails if a goal already exists — use `update_goal` to evolve it.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "push": PushMode::schema(),
                    "content": {"type": "string", "description": "Full goal body. Required."},
                    "criteria": {
                        "type": "array",
                        "description": "Optional structured success criteria the verifier checks one by one.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "id": {"type": "string", "description": "Short stable id, e.g. C1."},
                                "text": {"type": "string", "description": "The criterion itself."},
                                "verify_hint": {"type": "string", "description": "Optional hint on how to verify it."}
                            },
                            "required": ["id", "text"]
                        }
                    }
                },
                "required": ["content"],
            }),
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
        let content = string_arg(args, "content")?;
        if content.trim().is_empty() {
            return Err("argument `content` must be non-empty".to_string());
        }
        let criteria: Vec<refact_chat_api::GoalCriterion> = match args.get("criteria") {
            None | Some(Value::Null) => Vec::new(),
            Some(value) => serde_json::from_value(value.clone())
                .map_err(|error| format!("argument `criteria` is malformed: {error}"))?,
        };

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

        let report = {
            let mut session = session_arc.lock().await;
            if current_goal_including_queued(&session)?.is_some() {
                return Err("goal already exists; use update_goal".to_string());
            }
            let current_mode = map_legacy_mode_to_id(&session.thread.mode).to_string();
            let report = queue_goal_side_effect(
                &mut session,
                &current_mode,
                &content,
                &criteria,
                push,
                tool_call_id,
            )?;
            report
        };

        let result = json!({
            "version": report.version,
            "supersedes": report.supersedes,
        });

        Ok((
            false,
            vec![ContextEnum::ChatMessage(ChatMessage {
                role: "tool".to_string(),
                content: ChatContent::SimpleText(result.to_string()),
                tool_call_id: tool_call_id.clone(),
                ..Default::default()
            })],
        ))
    }
}

fn queue_goal_side_effect(
    session: &mut ChatSession,
    mode: &str,
    body: &str,
    criteria: &[refact_chat_api::GoalCriterion],
    push: PushMode,
    tool_call_id: &str,
) -> Result<GoalInstallReport, String> {
    let mut message = internal_roles::goal(mode, 1, body, None, true, GoalBudget::default());
    if !criteria.is_empty() {
        if let Some(meta) = message
            .extra
            .get_mut("goal")
            .and_then(|value| value.as_object_mut())
        {
            meta.insert("criteria".to_string(), json!(criteria));
        }
    }
    let mut delivery = PendingDelivery::new(
        vec![
            message,
            internal_roles::event(
                EventSubkind::SystemNotice,
                "tool.set_goal",
                json!({"version": 1}),
                "Goal updated to v1",
            ),
        ],
        push,
        "tool.set_goal",
        false,
    );
    delivery.after_tool_call_id = Some(tool_call_id.to_string());
    session.enqueue_delivery(delivery)?;
    Ok(GoalInstallReport {
        version: 1,
        supersedes: None,
    })
}

fn current_goal_including_queued(session: &ChatSession) -> Result<Option<ChatMessage>, String> {
    Ok(goal_role::current_base_goal(
        &session.try_accepted_control_projection()?,
    ))
}

fn string_arg(args: &HashMap<String, Value>, name: &str) -> Result<String, String> {
    match args.get(name) {
        Some(Value::String(value)) => Ok(value.clone()),
        Some(value) => Err(format!("argument `{name}` is not a string: {value:?}")),
        None => Err(format!("argument `{name}` is missing")),
    }
}
