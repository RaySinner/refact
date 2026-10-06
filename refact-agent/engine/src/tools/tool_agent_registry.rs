use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::sync::Mutex as AMutex;

use crate::at_commands::at_commands::AtCommandsContext;
use crate::call_validation::{ChatContent, ChatMessage, ContextEnum};
use crate::global_context::GlobalContext;
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

async fn planner_gcx(
    ccx: &Arc<AMutex<AtCommandsContext>>,
) -> Result<Arc<GlobalContext>, String> {
    let ccx_lock = ccx.lock().await;
    let is_planner = ccx_lock
        .task_meta
        .as_ref()
        .map(|meta| meta.role == "planner")
        .unwrap_or(false);
    if !is_planner {
        return Err("agent_registry can only be called by the task planner.".to_string());
    }
    Ok(ccx_lock.app.gcx.clone())
}

fn render_registry(registry: &crate::yaml_configs::customization_types::ProjectRegistry) -> String {
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
        let gcx = planner_gcx(&ccx).await?;
        let registry = crate::yaml_configs::customization_registry::get_project_registry(gcx)
            .await
            .ok_or_else(|| "Failed to load project registry".to_string())?;
        let report = render_registry(&registry);
        Ok(tool_output(tool_call_id, report))
    }

    fn tool_depends_on(&self) -> Vec<String> {
        vec![]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_state::AppState;
    use crate::chat::types::TaskMeta as ThreadTaskMeta;

    async fn planner_ccx(
        gcx: Arc<GlobalContext>,
        role: &str,
    ) -> Arc<AMutex<AtCommandsContext>> {
        let app = AppState::from_gcx(gcx).await;
        Arc::new(AMutex::new(
            AtCommandsContext::new_from_app(
                app,
                200_000,
                20,
                false,
                vec![],
                "planner-chat".to_string(),
                None,
                "model".to_string(),
                Some(ThreadTaskMeta {
                    task_id: "task-1".to_string(),
                    role: role.to_string(),
                    agent_id: None,
                    card_id: None,
                    planner_chat_id: None,
                }),
                None,
            )
            .await,
        ))
    }

    fn output_text(result: (bool, Vec<ContextEnum>)) -> String {
        match result.1.into_iter().next().unwrap() {
            ContextEnum::ChatMessage(message) => match message.content {
                ChatContent::SimpleText(text) => text,
                _ => panic!("expected text output"),
            },
            _ => panic!("expected chat message"),
        }
    }

    fn insert_registry(
        gcx: &Arc<GlobalContext>,
        registry: crate::yaml_configs::customization_types::ProjectRegistry,
    ) {
        use crate::yaml_configs::customization_registry::RegistryCache;
        gcx.project_registry_cache.write().unwrap().cache.insert(
            gcx.config_dir.clone(),
            RegistryCache {
                project_root: gcx.config_dir.clone(),
                registry,
                last_scan: std::time::SystemTime::now(),
            },
        );
    }

    #[test]
    fn tool_agent_registry_description_correct() {
        let desc = ToolAgentRegistry::new().tool_description();
        assert_eq!(desc.name, "agent_registry");
        assert_eq!(desc.display_name, "Agent Registry");
        assert_eq!(desc.input_schema["required"], json!([]));
        assert!(desc.description.contains("Planner-only"));
    }

    #[tokio::test]
    async fn tool_agent_registry_rejects_non_planner() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let ccx = planner_ccx(gcx, "agents").await;
        let err = ToolAgentRegistry::new()
            .tool_execute(ccx, &"call".to_string(), &HashMap::new())
            .await
            .unwrap_err();
        assert!(err.contains("can only be called by the task planner"));
    }

    #[tokio::test]
    async fn tool_agent_registry_renders_registered_agents() {
        use crate::yaml_configs::customization_types::{
            GatherFilesConfig, ProjectRegistry, SubagentConfig, SubagentMessages,
            SubagentPrompts, SubchatConfig,
        };
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let mut registry = ProjectRegistry::default();
        registry.subagents.insert(
            "coder".to_string(),
            SubagentConfig {
                schema_version: 1,
                id: "coder".to_string(),
                title: "Coder".to_string(),
                description: "Writes code changes".to_string(),
                specific: false,
                expose_as_tool: false,
                has_code: false,
                tool: None,
                subchat: SubchatConfig::default(),
                messages: SubagentMessages::default(),
                prompts: SubagentPrompts::default(),
                gather_files: GatherFilesConfig::default(),
                tools: vec!["cat".to_string()],
                writes: true,
                decision_maker: false,
                room_role_hint: None,
                base: None,
                match_models: None,
                extra: HashMap::new(),
            },
        );
        insert_registry(&gcx, registry);

        let ccx = planner_ccx(gcx, "planner").await;
        let output = output_text(
            ToolAgentRegistry::new()
                .tool_execute(ccx, &"call".to_string(), &HashMap::new())
                .await
                .unwrap(),
        );
        assert!(output.contains("# Agent Registry (1 agents)"));
        assert!(output.contains("**coder**"));
        assert!(output.contains("Writes code changes"));
        assert!(output.contains("cat"));
    }

    #[test]
    fn render_registry_empty() {
        let registry = crate::yaml_configs::customization_types::ProjectRegistry::default();
        let output = render_registry(&registry);
        assert!(output.contains("_No agent definitions loaded._"));
    }

    #[test]
    fn render_registry_includes_all_fields() {
        use crate::yaml_configs::customization_types::{
            GatherFilesConfig, ProjectRegistry, SubagentConfig, SubagentMessages,
            SubagentPrompts, SubchatConfig,
        };
        let config = SubagentConfig {
            schema_version: 1,
            id: "coder".to_string(),
            title: "Coder".to_string(),
            description: "Writes code changes".to_string(),
            specific: false,
            expose_as_tool: false,
            has_code: false,
            tool: None,
            subchat: SubchatConfig::default(),
            messages: SubagentMessages::default(),
            prompts: SubagentPrompts::default(),
            gather_files: GatherFilesConfig::default(),
            tools: vec!["cat".to_string(), "update_textdoc".to_string()],
            writes: true,
            decision_maker: false,
            room_role_hint: Some("primary implementer".to_string()),
            base: None,
            match_models: None,
            extra: HashMap::new(),
        };
        let mut registry = ProjectRegistry::default();
        registry.subagents.insert("coder".to_string(), config);

        let output = render_registry(&registry);
        assert!(output.contains("# Agent Registry (1 agents)"));
        assert!(output.contains("**coder**"));
        assert!(output.contains("Writes code changes"));
        assert!(output.contains("`writes`"));
        assert!(!output.contains("decision-maker"));
        assert!(output.contains("primary implementer"));
        assert!(output.contains("cat, update_textdoc"));
    }

    #[test]
    fn render_registry_decision_maker_and_read_only() {
        use crate::yaml_configs::customization_types::{
            GatherFilesConfig, ProjectRegistry, SubagentConfig, SubagentMessages,
            SubagentPrompts, SubchatConfig,
        };
        let config = SubagentConfig {
            schema_version: 1,
            id: "reviewer".to_string(),
            title: "Reviewer".to_string(),
            description: String::new(),
            specific: false,
            expose_as_tool: false,
            has_code: false,
            tool: None,
            subchat: SubchatConfig::default(),
            messages: SubagentMessages::default(),
            prompts: SubagentPrompts::default(),
            gather_files: GatherFilesConfig::default(),
            tools: Vec::new(),
            writes: false,
            decision_maker: true,
            room_role_hint: None,
            base: None,
            match_models: None,
            extra: HashMap::new(),
        };
        let mut registry = ProjectRegistry::default();
        registry.subagents.insert("reviewer".to_string(), config);

        let output = render_registry(&registry);
        assert!(output.contains("`read-only`"));
        assert!(output.contains("decision-maker"));
        assert!(output.contains("(no description)"));
        assert!(output.contains("(no tools specified)"));
    }
}
