pub use refact_tools::tools::tool_agent_registry::*;

#[cfg(test)]
use std::collections::HashMap;
#[cfg(test)]
use std::sync::Arc;
#[cfg(test)]
use serde_json::json;
#[cfg(test)]
use tokio::sync::Mutex as AMutex;
#[cfg(test)]
use refact_runtime_api::AtCommandsContext;
#[cfg(test)]
use refact_core::chat_types::ChatContent;
#[cfg(test)]
use refact_core::chat_types::ChatMessage;
#[cfg(test)]
use refact_core::chat_types::ContextEnum;
#[cfg(test)]
use crate::tools::tools_description::Tool;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_state::AppState;
    use refact_chat_api::TaskMeta as ThreadTaskMeta;
    use crate::global_context::GlobalContext;

    async fn planner_ccx(
        gcx: Arc<GlobalContext>,
        role: &str,
    ) -> Arc<AMutex<AtCommandsContext>> {
        let app = refact_runtime_api::app_state_from_gcx(gcx).await;
        Arc::new(AMutex::new(
            crate::at_commands::at_commands::at_commands_new_from_app(
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
        registry: refact_yaml_configs::customization_types::ProjectRegistry,
    ) {
        use refact_yaml_configs::registry_cache::RegistryCache;
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
        use refact_yaml_configs::customization_types::{
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
        let registry = refact_yaml_configs::customization_types::ProjectRegistry::default();
        let output = render_registry(&registry);
        assert!(output.contains("_No agent definitions loaded._"));
    }

    #[test]
    fn render_registry_includes_all_fields() {
        use refact_yaml_configs::customization_types::{
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
        use refact_yaml_configs::customization_types::{
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
