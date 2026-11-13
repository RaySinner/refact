use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::sync::Mutex as AMutex;

use refact_runtime_api::AtCommandsContext;
use refact_core::chat_types::{ChatContent, ChatMessage, ContextEnum};
use refact_runtime_api::ModeConfigProvider;
use crate::tools::tools_description::{Tool, ToolDesc, ToolSource, ToolSourceType};

pub struct ToolAgentRegistry;

impl ToolAgentRegistry {
    pub fn new() -> Self {
        Self
    }
}

fn agent_registry_schema() -> Value {
    json!({
        "type": "object",
        "properties": {},
        "required": []
    })
}

fn agent_registry_description() -> ToolDesc {
    ToolDesc {
        name: "agent_registry".to_string(),
        display_name: "Agent Registry".to_string(),
        source: ToolSource {
            source_type: ToolSourceType::Builtin,
            config_path: String::new(),
        },
        experimental: false,
        allow_parallel: true,
        description: "Planner-only: lists all loaded agent definitions (id, title, description, writes, decision_maker, tools). Call before creating a room to see which agents are available.".to_string(),
        input_schema: agent_registry_schema(),
        output_schema: None,
        annotations: None,
    }
}

async fn planner_mode_config(
    ccx: &Arc<AMutex<AtCommandsContext>>,
) -> Result<Arc<dyn ModeConfigProvider>, String> {
    let ccx_lock = ccx.lock().await;
    let is_planner = ccx_lock
        .task_meta
        .as_ref()
        .map(|meta| meta.role == "planner")
        .unwrap_or(false);
    if !is_planner {
        return Err("agent_registry can only be called by the task planner.".to_string());
    }
    Ok(ccx_lock.tool.mode_config.clone())
}

pub fn render_registry(registry: &refact_yaml_configs::customization_types::ProjectRegistry) -> String {
    let mut subagents: Vec<_> = registry.subagents.iter().collect();
    subagents.sort_by(|(a, _), (b, _)| a.cmp(b));

    if subagents.is_empty() {
        return "# Agent Registry\n\n_No agent definitions loaded._\n".to_string();
    }

    let mut output = format!("# Agent Registry ({} agents)\n\n", subagents.len());
    for (id, config) in subagents {
        let writes = if config.writes { "writes" } else { "read-only" };
        let decision = if config.decision_maker { ", decision-maker" } else { "" };
        let hint = config
            .room_role_hint
            .as_deref()
            .map(|h| format!("\n   _hint: {}_", h))
            .unwrap_or_default();
        let tools = if config.tools.is_empty() {
            "(no tools specified)".to_string()
        } else {
            config.tools.join(", ")
        };
        let desc = if config.description.is_empty() {
            "(no description)".to_string()
        } else {
            config.description.clone()
        };
        output.push_str(&format!(
            "- **{}** — {}\n   `{}`{}{}\n",
            id, desc, writes, decision, hint
        ));
        output.push_str(&format!("   tools: {}\n", tools));
    }
    output
}

fn tool_output(tool_call_id: &String, result: String) -> (bool, Vec<ContextEnum>) {
    (
        false,
        vec![ContextEnum::ChatMessage(ChatMessage {
            role: "tool".to_string(),
            content: ChatContent::SimpleText(result),
            tool_calls: None,
            tool_call_id: tool_call_id.clone(),
            ..Default::default()
        })],
    )
}

#[async_trait]
impl Tool for ToolAgentRegistry {
    fn tool_description(&self) -> ToolDesc {
        agent_registry_description()
    }

    async fn tool_execute(
        &mut self,
        ccx: Arc<AMutex<AtCommandsContext>>,
        tool_call_id: &String,
        _args: &HashMap<String, Value>,
    ) -> Result<(bool, Vec<ContextEnum>), String> {
        let registry = planner_mode_config(&ccx)
            .await?
            .get_project_registry()
            .await
            .ok_or_else(|| "Failed to load project registry".to_string())?;
        let report = render_registry(&registry);
        Ok(tool_output(tool_call_id, report))
    }

    fn tool_depends_on(&self) -> Vec<String> {
        vec![]
    }
}
