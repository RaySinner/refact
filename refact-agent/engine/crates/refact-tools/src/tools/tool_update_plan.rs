use std::collections::HashMap;
use std::sync::Arc;
use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::sync::Mutex as AMutex;
use refact_runtime_api::AtCommandsContext;
use refact_core::chat_types::{ChatContent, ChatMessage, ContextEnum};
use refact_chat_api::internal_roles::{self, EventSubkind};
use refact_chat_api::plan_role;
use refact_chat_api::session_plan_role;
use refact_chat_api::types::{ChatSession, PendingDelivery, PushMode};
use crate::tools::tools_description::{ json_schema_from_params, Tool, ToolDesc, ToolSource, ToolSourceType, };
pub struct ToolUpdatePlan {
    pub config_path: String,
}

#[async_trait]
impl Tool for ToolUpdatePlan {
    fn tool_description(&self) -> ToolDesc {
        ToolDesc {
            name: "update_plan".to_string(),
            display_name: "Update Plan".to_string(),
            source: ToolSource {
                source_type: ToolSourceType::Builtin,
                config_path: self.config_path.clone(),
            },
            experimental: false,
            allow_parallel: false,
            description: "Append an incremental update to the current plan (cache-safe delta merged into the current plan). Use when the plan evolves; it does not rewrite the original plan.".to_string(),
            input_schema: { let mut schema = json_schema_from_params(
                &[
                    ("note", "string", "Plan update note. Required."),
                    (
                        "summary",
                        "string",
                        "Short description of what changed, ≤120 chars. Optional.",
                    ),
                ],
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
        let summary = optional_string_arg(args, "summary")?;
        if summary
            .as_ref()
            .is_some_and(|summary| summary.chars().count() > 120)
        {
            return Err("argument `summary` must be at most 120 chars".to_string());
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
            if !has_base_plan_including_queued(&session)? {
                return Err("no plan to update; call set_plan first".to_string());
            }
            let seq = plan_delta_count_including_queued(&session)? + 1;
            let (delta, result_truncation) = internal_roles::plan_delta_with_truncation(
                "tool.update_plan",
                json!({"seq": seq, "summary": summary}),
                note,
            );
            let mut messages = vec![delta];
            messages.push(internal_roles::event(
                EventSubkind::SystemNotice,
                "tool.update_plan",
                json!({"seq": seq, "summary": summary}),
                format!("Plan updated (delta {seq})"),
            ));
            let mut delivery = PendingDelivery::new(messages, push, "tool.update_plan", false);
            delivery.after_tool_call_id = Some(tool_call_id.clone());
            session.enqueue_delivery(delivery)?;
            (seq, result_truncation)
        };

        Ok((
            false,
            vec![ContextEnum::ChatMessage(ChatMessage {
                role: "tool".to_string(),
                content: ChatContent::SimpleText(
                    update_plan_tool_result(seq, result_truncation).to_string(),
                ),
                tool_call_id: tool_call_id.clone(),
                ..Default::default()
            })],
        ))
    }
}

fn update_plan_tool_result(
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

pub fn has_base_plan_including_queued(session: &ChatSession) -> Result<bool, String> {
    Ok(session_plan_role::try_current_base_plan(session)?.is_some())
}

fn plan_delta_count_including_queued(session: &ChatSession) -> Result<usize, String> {
    Ok(plan_role::try_plan_delta_events(&session.try_accepted_control_projection()?)?.len())
}

fn string_arg(args: &HashMap<String, Value>, name: &str) -> Result<String, String> {
    match args.get(name) {
        Some(Value::String(value)) => Ok(value.clone()),
        Some(value) => Err(format!("argument `{name}` is not a string: {value:?}")),
        None => Err(format!("argument `{name}` is missing")),
    }
}

fn optional_string_arg(
    args: &HashMap<String, Value>,
    name: &str,
) -> Result<Option<String>, String> {
    match args.get(name) {
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(Value::Null) | None => Ok(None),
        Some(value) => Err(format!("argument `{name}` is not a string: {value:?}")),
    }
}
