use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::sync::Mutex as AMutex;

use refact_runtime_api::AtCommandsContext;
use refact_runtime_api::GlobalContextCtx;
use refact_core::chat_types::{ChatContent, ChatMessage, ContextEnum};
use refact_chat_history::trajectory_metrics::TRAJECTORY_LABEL_MAX_CHARS;
use refact_runtime_api::trajectory_runtime::set_trajectory_label;
use crate::tools::tools_description::{Tool, ToolDesc, ToolSource, ToolSourceType};

pub struct ToolSetTrajectoryLabel {
    pub config_path: String,
}

#[async_trait]
impl Tool for ToolSetTrajectoryLabel {
    fn tool_description(&self) -> ToolDesc {
        ToolDesc {
            name: "set_trajectory_label".to_string(),
            display_name: "Set Trajectory Label".to_string(),
            source: ToolSource {
                source_type: ToolSourceType::Builtin,
                config_path: self.config_path.clone(),
            },
            experimental: false,
            allow_parallel: false,
            description: "Set a short user-authored label for the current trajectory.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "label": {
                        "type": "string",
                        "minLength": 1,
                        "maxLength": TRAJECTORY_LABEL_MAX_CHARS,
                        "description": "Non-empty trajectory label (at most 120 characters)."
                    }
                },
                "required": ["label"]
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
        let label = match args.get("label") {
            Some(Value::String(label)) => label.as_str(),
            _ => return Err("argument `label` must be a string".to_string()),
        };
        let (app, invoking_chat_id) = {
            let ccx = ccx.lock().await;
            (ccx.app.clone(), ccx.chat_id.clone())
        };
        let normalized_label =
            refact_chat_history::trajectory_metrics::validate_trajectory_label(label)?;
        set_trajectory_label(app, &invoking_chat_id, &normalized_label).await?;
        let result = json!({
            "trajectory_id": invoking_chat_id,
            "label": normalized_label,
            "is_title_generated": false
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

    fn tool_depends_on(&self) -> Vec<String> {
        vec![]
    }
}
