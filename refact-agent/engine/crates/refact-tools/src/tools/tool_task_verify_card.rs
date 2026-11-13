use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::sync::Mutex as AMutex;

use refact_runtime_api::AtCommandsContext;
use refact_core::chat_types::{ChatContent, ChatMessage, ContextEnum};
use refact_chat_api::verifier::VerifyCardRequest;
use refact_runtime_api::verifier::verify_card_for_ctx;
use crate::tools::task_tool_helpers::require_bound_planner_task;
use crate::tools::tools_description::{Tool, ToolDesc, ToolSource, ToolSourceType};

pub struct ToolTaskVerifyCard;

impl ToolTaskVerifyCard {
    pub fn new() -> Self {
        Self
    }
}

fn required_string(args: &HashMap<String, Value>, key: &str) -> Result<String, String> {
    args.get(key)
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| format!("Missing '{}'", key))
}

fn verify_card_task_scope_error(error: String) -> String {
    if error == "task_id override is not allowed from this planner chat" {
        "task_id does not match bound task_id".to_string()
    } else {
        error
    }
}

#[async_trait]
impl Tool for ToolTaskVerifyCard {
    fn tool_description(&self) -> ToolDesc {
        ToolDesc {
            name: "task_verify_card".to_string(),
            display_name: "Task Verify Card".to_string(),
            source: ToolSource {
                source_type: ToolSourceType::Builtin,
                config_path: String::new(),
            },
            experimental: false,
            allow_parallel: false,
            description: "Manually re-run verifier for a completed task card and store verifier_report on the card.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "card_id": {"type": "string", "description": "Card ID to verify"},
                    "task_id": {"type": "string", "description": "Task ID (optional if in task context)"},
                    "command": {"type": "string", "description": "Optional custom verification command to run in the card's worktree"}
                },
                "required": ["card_id"]
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
        let ccx_lock = ccx.lock().await;
        let is_planner = ccx_lock
            .task_meta
            .as_ref()
            .map(|meta| meta.role == "planner")
            .unwrap_or(false);
        if !is_planner {
            return Err("task_verify_card can only be called by the task planner.".to_string());
        }
        drop(ccx_lock);

        let card_id = required_string(args, "card_id")?;
        let command = args
            .get("command")
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string);
        let task_id = require_bound_planner_task(&ccx, args)
            .await
            .map_err(verify_card_task_scope_error)?;
        let ctx = ccx.lock().await.app.chat_ctx();
        let report = verify_card_for_ctx(
            &ctx,
            VerifyCardRequest {
                task_id,
                card_id: card_id.clone(),
                expected_state: None,
                custom_command: command,
            },
        )
        .await?;
        let concerns = if report.concerns.is_empty() {
            "none".to_string()
        } else {
            report.concerns.join("\n- ")
        };
        let content = format!(
            "# Verifier Report\n\n**Card:** {}\n**Passed:** {}\n**Recommendation:** {}\n\n## Concerns\n- {}",
            card_id, report.passed, report.recommendation, concerns
        );
        Ok((
            false,
            vec![ContextEnum::ChatMessage(ChatMessage {
                role: "tool".to_string(),
                content: ChatContent::SimpleText(content),
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
