#[cfg(test)]
use std::path::Path;
use std::sync::Arc;

use crate::files_correction::get_project_dirs;
use crate::global_context::GlobalContext;
use crate::yaml_configs::customization_types::*;
use crate::yaml_configs::project_configs_bootstrap::{
    global_configs_try_create_all, project_configs_ensure_dirs,
};

pub use refact_yaml_configs::registry_cache::{RegistryCache, RegistryCacheManager};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigScope {
    Global,
    Local,
}

pub use refact_yaml_configs::customization_registry::*;

pub async fn get_project_registry(gcx: Arc<GlobalContext>) -> Option<ProjectRegistry> {
    let config_dir = gcx.config_dir.clone();
    let dirs = get_project_dirs(gcx.clone()).await;
    let project_root = dirs.first().cloned();

    let cache_key = project_root.clone().unwrap_or_else(|| config_dir.clone());

    {
        let cache_result = gcx.project_registry_cache.read();
        if let Ok(cache) = cache_result {
            if let Some(cached) = cache.get(&cache_key) {
                return Some(cached.registry.clone());
            }
        }
    }

    let _ = global_configs_try_create_all(&config_dir).await;
    if let Some(ref root) = project_root {
        let _ = project_configs_ensure_dirs(root).await;
    }

    let registry = load_merged_registry(&config_dir, project_root.as_deref()).await;

    {
        let cache_result = gcx.project_registry_cache.write();
        if let Ok(mut cache) = cache_result {
            cache.insert(cache_key, registry.clone());
        }
    }

    Some(registry)
}

#[allow(dead_code)]
pub async fn get_global_registry(gcx: Arc<GlobalContext>) -> ProjectRegistry {
    let config_dir = gcx.config_dir.clone();
    let _ = global_configs_try_create_all(&config_dir).await;
    load_registry_from_dir(&config_dir).await
}

pub async fn invalidate_all_registry_caches(gcx: Arc<GlobalContext>) {
    let cache_arc = gcx.project_registry_cache.clone();
    if let Ok(mut cache) = cache_arc.write() {
        cache.cache.clear();
    };
    gcx.tool_catalog_generations.advance_customization();
}

pub async fn get_mode_config(
    gcx: Arc<GlobalContext>,
    mode_id: &str,
    model_id: Option<&str>,
) -> Option<ModeConfig> {
    let registry = get_project_registry(gcx.clone()).await?;
    if let Some((overlay_id, dropped)) = mode_override_tool_drift(&registry, mode_id, model_id) {
        crate::yaml_configs::mode_validation::warn_override_tool_drift(
            gcx,
            mode_id,
            &overlay_id,
            &dropped,
        )
        .await;
    }
    resolve_mode_for_model(&registry, mode_id, model_id)
}

pub async fn get_subagent_config(
    gcx: Arc<GlobalContext>,
    subagent_id: &str,
    model_id: Option<&str>,
) -> Option<SubagentConfig> {
    let registry = get_project_registry(gcx).await?;
    resolve_subagent_for_model(&registry, subagent_id, model_id)
}

pub use refact_chat_api::setparams::map_legacy_mode_to_id;

pub fn match_tool_confirm_action(rules: &[ToolConfirmRule], tool_name: &str) -> Option<String> {
    for rule in rules {
        if glob_matches(&rule.match_pattern, tool_name) {
            return Some(rule.action.clone());
        }
    }
    None
}

fn glob_matches(pattern: &str, name: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    if pattern.ends_with("*") {
        let prefix = &pattern[..pattern.len() - 1];
        return name.starts_with(prefix);
    }
    if pattern.starts_with("*") {
        let suffix = &pattern[1..];
        return name.ends_with(suffix);
    }
    pattern == name
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn runtime_required_subagent_ids() -> Vec<&'static str> {
        vec![
            "subagent",
            "review_agents",
            "commit_message",
            "title_generation",
            "kg_enrich",
            "kg_deprecate",
            "code_edit",
            "follow_up",
            "mode_transition",
            "memo_extraction",
        ]
    }

    fn read_default_mode_file(filename: &str) -> String {
        std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("crates")
                .join("refact-yaml-configs")
                .join("src")
                .join("defaults")
                .join("modes")
                .join(filename),
        )
        .unwrap_or_default()
    }

    fn write_file(path: &Path, content: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    fn load_default_registry_for_tests() -> ProjectRegistry {
        use crate::yaml_configs::project_configs_bootstrap::global_configs_try_create_all;

        let temp_dir = tempfile::tempdir().unwrap();
        let config_dir = temp_dir.path().to_path_buf();
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            global_configs_try_create_all(&config_dir).await.unwrap();
            load_registry_from_dir(&config_dir).await
        })
    }

    fn base_agent_mode() -> ModeConfig {
        ModeConfig {
            schema_version: 1,
            id: "agent".to_string(),
            title: "Agent".to_string(),
            description: String::new(),
            specific: false,
            prompt: "base".to_string(),
            plan_template: String::new(),
            tools: vec!["tree".to_string(), "cat".to_string(), "shell".to_string()],
            allow_integrations: true,
            allow_mcp: true,
            allow_subagents: true,
            model_defaults: ModeModelDefaults::default(),
            tool_confirm: ToolConfirmConfig {
                rules: vec![
                    ToolConfirmRule {
                        match_pattern: "tree".to_string(),
                        action: "auto".to_string(),
                    },
                    ToolConfirmRule {
                        match_pattern: "shell".to_string(),
                        action: "ask".to_string(),
                    },
                ],
            },
            thread_defaults: ModeThreadDefaults {
                include_project_info: Some(true),
                checkpoints_enabled: Some(true),
                auto_approve_editing_tools: Some(true),
                auto_approve_dangerous_commands: Some(false),
            },
            ui: ModeUi::default(),
            base: None,
            match_models: None,
            override_config: None,
        }
    }

    fn mode_override(id: &str, patterns: &[&str], prompt: &str) -> ModeConfig {
        ModeConfig {
            schema_version: 1,
            id: id.to_string(),
            title: String::new(),
            description: String::new(),
            specific: true,
            prompt: String::new(),
            plan_template: String::new(),
            tools: Vec::new(),
            allow_integrations: false,
            allow_mcp: false,
            allow_subagents: false,
            model_defaults: ModeModelDefaults::default(),
            tool_confirm: ToolConfirmConfig::default(),
            thread_defaults: ModeThreadDefaults::default(),
            ui: ModeUi::default(),
            base: Some("agent".to_string()),
            match_models: Some(patterns.iter().map(|pattern| pattern.to_string()).collect()),
            override_config: Some(ModeOverride {
                prompt: Some(prompt.to_string()),
                ..Default::default()
            }),
        }
    }

    fn registry_with_mode_overrides(overrides: Vec<ModeConfig>) -> ProjectRegistry {
        let mut modes = HashMap::new();
        modes.insert("agent".to_string(), base_agent_mode());
        ProjectRegistry {
            modes,
            mode_overrides: overrides,
            ..Default::default()
        }
    }

    fn base_subagent() -> SubagentConfig {
        SubagentConfig {
            schema_version: 1,
            id: "coder".to_string(),
            title: "Base".to_string(),
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
            writes: true,
            decision_maker: false,
            room_role_hint: None,
            base: None,
            match_models: None,
            extra: HashMap::new(),
        }
    }

    fn subagent_override(id: &str, patterns: &[&str], title: &str) -> SubagentConfig {
        SubagentConfig {
            schema_version: 1,
            id: id.to_string(),
            title: title.to_string(),
            description: String::new(),
            specific: true,
            expose_as_tool: false,
            has_code: false,
            tool: None,
            subchat: SubchatConfig::default(),
            messages: SubagentMessages::default(),
            prompts: SubagentPrompts::default(),
            gather_files: GatherFilesConfig::default(),
            tools: Vec::new(),
            writes: true,
            decision_maker: false,
            room_role_hint: None,
            base: Some("coder".to_string()),
            match_models: Some(patterns.iter().map(|pattern| pattern.to_string()).collect()),
            extra: HashMap::new(),
        }
    }

    fn exposed_config_subagent(id: &str) -> SubagentConfig {
        SubagentConfig {
            id: id.to_string(),
            expose_as_tool: true,
            ..base_subagent()
        }
    }

    fn assert_mode_inherits_agent_surface(agent: &ModeConfig, resolved: &ModeConfig) {
        assert_eq!(resolved.tools, agent.tools);
        assert_eq!(resolved.allow_integrations, agent.allow_integrations);
        assert_eq!(resolved.allow_mcp, agent.allow_mcp);
        assert_eq!(resolved.allow_subagents, agent.allow_subagents);
        assert_eq!(
            resolved.tool_confirm.rules.len(),
            agent.tool_confirm.rules.len()
        );
        for (resolved_rule, agent_rule) in resolved
            .tool_confirm
            .rules
            .iter()
            .zip(agent.tool_confirm.rules.iter())
        {
            assert_eq!(resolved_rule.match_pattern, agent_rule.match_pattern);
            assert_eq!(resolved_rule.action, agent_rule.action);
        }
        assert_eq!(
            resolved.thread_defaults.include_project_info,
            agent.thread_defaults.include_project_info
        );
        assert_eq!(
            resolved.thread_defaults.checkpoints_enabled,
            agent.thread_defaults.checkpoints_enabled
        );
        assert_eq!(
            resolved.thread_defaults.auto_approve_editing_tools,
            agent.thread_defaults.auto_approve_editing_tools
        );
        assert_eq!(
            resolved.thread_defaults.auto_approve_dangerous_commands,
            agent.thread_defaults.auto_approve_dangerous_commands
        );
    }

    #[tokio::test]
    async fn imported_competitor_subagent_loads_through_registry() {
        use crate::ext::competitor_import::types::ImportStatus;

        let workspace = tempfile::tempdir().unwrap();
        write_file(
            &workspace.path().join(".claude/agents/registry-reviewer.md"),
            "---\nname: Registry Reviewer\ndescription: Reviews imported registry behavior\ntools:\n  - Read\n  - Grep\n  - Edit\ndenied-tools:\n  - Edit\nmaxTurns: 7\nmodel: sonnet\n---\nUse registry context to review {{task}}.",
        );

        let summary = crate::ext::competitor_import::run_project_import_with_paths(&[workspace
            .path()
            .to_path_buf()])
        .await;
        let registry = load_registry_from_dir(&workspace.path().join(".refact")).await;

        assert_eq!(summary.status_counts.get(&ImportStatus::Created), Some(&1));
        assert!(registry.errors.is_empty(), "{:?}", registry.errors);
        let subagent = registry
            .subagents
            .get("registry-reviewer")
            .expect("imported subagent should load through registry");
        assert_eq!(subagent.schema_version, 2);
        assert_eq!(subagent.title, "Registry Reviewer");
        assert_eq!(subagent.description, "Reviews imported registry behavior");
        assert_eq!(subagent.subchat.max_steps, Some(7));
        assert_eq!(subagent.subchat.model.as_deref(), Some("sonnet"));
        assert_eq!(
            subagent.messages.user_template.as_deref(),
            Some("{{task}}\n")
        );
        assert_eq!(subagent.tools, vec!["cat", "search_pattern"]);
        let tool = subagent.tool.as_ref().unwrap();
        assert!(tool.agentic);
        assert_eq!(tool.required, vec!["task"]);
    }

    #[test]
    fn test_model_matches_pattern_exact() {
        assert!(model_matches_pattern("gpt-4o", "gpt-4o"));
        assert!(!model_matches_pattern("gpt-4o", "gpt-4"));
    }

    #[test]
    fn test_model_matches_pattern_wildcard() {
        assert!(model_matches_pattern("gpt-4o", "*"));
        assert!(model_matches_pattern("claude-3", "*"));
    }

    #[test]
    fn test_model_matches_pattern_prefix() {
        assert!(model_matches_pattern("openai/gpt-4o", "gpt-4*"));
        assert!(model_matches_pattern("openrouter/openai/gpt-4o", "gpt-4*"));
        assert!(model_matches_pattern("claude-3.7-sonnet", "claude-3-7*"));
        assert!(model_matches_pattern(
            "anthropic/claude-3.7-sonnet",
            "claude-3-7*"
        ));

        assert!(model_matches_pattern("gpt-4o", "gpt-*"));
        assert!(model_matches_pattern("gpt-4-turbo", "gpt-*"));
        assert!(!model_matches_pattern("claude-3", "gpt-*"));
    }

    #[test]
    fn test_model_matches_pattern_contains_wildcard() {
        assert!(model_matches_pattern("llama-7b", "*7b*"));
        assert!(model_matches_pattern("qwen2.5-7b-instruct", "*7b*"));
        assert!(model_matches_pattern("phi-mini", "*mini*"));
        assert!(!model_matches_pattern("abc-def", "*def*abc*"));
    }

    #[test]
    fn test_mode_override_specificity_contains_wildcard_beats_broad_family() {
        let registry = registry_with_mode_overrides(vec![
            mode_override(
                "broad_family_agent",
                &["llama*", "qwen*", "phi*"],
                "generic",
            ),
            mode_override("weak_family_agent", &["*7b*", "*mini*"], "weak"),
        ]);

        let llama = resolve_mode_for_model(&registry, "agent", Some("llama-7b"))
            .expect("agent mode should resolve");
        let qwen = resolve_mode_for_model(&registry, "agent", Some("qwen2.5-7b-instruct"))
            .expect("agent mode should resolve");
        let phi = resolve_mode_for_model(&registry, "agent", Some("phi-mini"))
            .expect("agent mode should resolve");
        let generic = resolve_mode_for_model(&registry, "agent", Some("qwen2.5-coder"))
            .expect("agent mode should resolve");

        assert_eq!(llama.prompt, "weak");
        assert_eq!(qwen.prompt, "weak");
        assert_eq!(phi.prompt, "weak");
        assert_eq!(generic.prompt, "generic");
    }

    #[test]
    fn test_mode_override_specificity_exact_beats_broad_wildcard() {
        let registry = registry_with_mode_overrides(vec![
            mode_override("broad_wildcard_agent", &["gpt-5*"], "broad"),
            mode_override("exact_match_agent", &["gpt-5.5"], "exact"),
        ]);

        let resolved = resolve_mode_for_model(&registry, "agent", Some("openai/gpt-5.5"))
            .expect("agent mode should resolve");

        assert_eq!(resolved.prompt, "exact");
    }

    #[test]
    fn test_mode_override_specificity_qwen_provider_pattern_beats_broad() {
        let registry = registry_with_mode_overrides(vec![
            mode_override("broad_family_agent", &["qwen*"], "broad"),
            mode_override(
                "provider_pattern_agent",
                &["qwen3.6-flash", "Qwen/Qwen3.6-*"],
                "specific",
            ),
        ]);

        let exact_resolved = resolve_mode_for_model(&registry, "agent", Some("qwen3.6-flash"))
            .expect("agent mode should resolve");
        let provider_resolved =
            resolve_mode_for_model(&registry, "agent", Some("alibaba/Qwen/Qwen3.6-flash"))
                .expect("agent mode should resolve");

        assert!(model_matches_pattern("qwen3.6-flash", "qwen*"));
        assert!(model_matches_pattern("qwen3.6-flash", "qwen3.6-flash"));
        assert!(model_matches_pattern("alibaba/Qwen/Qwen3.6-flash", "qwen*"));
        assert!(model_matches_pattern(
            "alibaba/Qwen/Qwen3.6-flash",
            "Qwen/Qwen3.6-*"
        ));
        assert_eq!(exact_resolved.prompt, "specific");
        assert_eq!(provider_resolved.prompt, "specific");
    }

    fn mode_override_with_tools(
        id: &str,
        patterns: &[&str],
        tools_replace: Option<&[&str]>,
        tools_add: Option<&[&str]>,
    ) -> ModeConfig {
        let mut overlay = mode_override(id, patterns, "overlay");
        overlay.override_config = Some(ModeOverride {
            prompt: Some("overlay".to_string()),
            tools_replace: tools_replace.map(|tools| tools.iter().map(|t| t.to_string()).collect()),
            tools_add: tools_add.map(|tools| tools.iter().map(|t| t.to_string()).collect()),
            ..Default::default()
        });
        overlay
    }

    #[test]
    fn test_mode_override_tool_drift_reports_tools_dropped_by_tools_replace() {
        let registry = registry_with_mode_overrides(vec![mode_override_with_tools(
            "frozen_agent",
            &["gpt-5*"],
            Some(&["cat"]),
            None,
        )]);

        let (overlay_id, dropped) =
            mode_override_tool_drift(&registry, "agent", Some("gpt-5.6-sol"))
                .expect("tools_replace that drops base tools must be reported");

        assert_eq!(overlay_id, "frozen_agent");
        assert_eq!(dropped, vec!["shell".to_string(), "tree".to_string()]);
    }

    #[test]
    fn test_mode_override_tool_drift_ignores_tools_add_overlays() {
        let registry = registry_with_mode_overrides(vec![mode_override_with_tools(
            "additive_agent",
            &["gpt-5*"],
            None,
            Some(&["glob"]),
        )]);

        assert!(mode_override_tool_drift(&registry, "agent", Some("gpt-5.6-sol")).is_none());
    }

    #[test]
    fn test_mode_override_tool_drift_ignores_complete_tools_replace() {
        let registry = registry_with_mode_overrides(vec![mode_override_with_tools(
            "complete_agent",
            &["gpt-5*"],
            Some(&["tree", "cat", "shell", "glob"]),
            None,
        )]);

        assert!(mode_override_tool_drift(&registry, "agent", Some("gpt-5.6-sol")).is_none());
    }

    #[test]
    fn test_mode_override_tool_drift_is_none_without_model_id() {
        let registry = registry_with_mode_overrides(vec![mode_override_with_tools(
            "frozen_agent",
            &["gpt-5*"],
            Some(&["cat"]),
            None,
        )]);

        assert!(mode_override_tool_drift(&registry, "agent", None).is_none());
    }

    #[test]
    fn test_mode_override_specificity_preserves_stable_order_on_tie() {
        let registry = registry_with_mode_overrides(vec![
            mode_override("first", &["gpt-5*"], "first"),
            mode_override("second", &["gpt-5*"], "second"),
        ]);

        let resolved = resolve_mode_for_model(&registry, "agent", Some("gpt-5.5"))
            .expect("agent mode should resolve");

        assert_eq!(resolved.prompt, "first");
    }

    #[test]
    fn test_mode_override_specificity_preserves_local_precedence_on_tie() {
        let global =
            registry_with_mode_overrides(vec![mode_override("global", &["gpt-5*"], "global")]);
        let local = ProjectRegistry {
            mode_overrides: vec![mode_override("local", &["gpt-5*"], "local")],
            ..Default::default()
        };
        let merged = merge_registries(global, local);

        let resolved = resolve_mode_for_model(&merged, "agent", Some("gpt-5.5"))
            .expect("agent mode should resolve");

        assert_eq!(resolved.prompt, "local");
    }

    #[test]
    fn test_subagent_override_specificity_uses_best_match() {
        let mut subagents = HashMap::new();
        subagents.insert("coder".to_string(), base_subagent());
        let registry = ProjectRegistry {
            subagents,
            subagent_overrides: vec![
                subagent_override("oss_coder", &["qwen*"], "Broad"),
                subagent_override(
                    "qwen36_coder",
                    &["qwen3.6-flash", "Qwen/Qwen3.6-*"],
                    "Specific",
                ),
            ],
            ..Default::default()
        };

        let exact_resolved = resolve_subagent_for_model(&registry, "coder", Some("qwen3.6-flash"))
            .expect("subagent should resolve");
        let provider_resolved =
            resolve_subagent_for_model(&registry, "coder", Some("alibaba/Qwen/Qwen3.6-flash"))
                .expect("subagent should resolve");

        assert_eq!(exact_resolved.title, "Specific");
        assert_eq!(provider_resolved.title, "Specific");
    }

    #[test]
    fn test_prompt_and_model_defaults_only_mode_override_inherits_agent_surface() {
        let mut overlay = mode_override("provider_agent", &["provider-model*"], "provider");
        overlay
            .override_config
            .as_mut()
            .expect("override config should exist")
            .model_defaults = Some(ModeModelDefaults {
            default: Some(ModelTypeConfig {
                temperature: Some(0.2),
                ..Default::default()
            }),
            ..Default::default()
        });
        let registry = registry_with_mode_overrides(vec![overlay]);
        let agent = registry
            .modes
            .get("agent")
            .expect("base agent should exist")
            .clone();

        let resolved = resolve_mode_for_model(&registry, "agent", Some("provider-model-pro"))
            .expect("agent mode should resolve");

        assert_eq!(resolved.prompt, "provider");
        assert_eq!(
            resolved
                .model_defaults
                .default
                .as_ref()
                .and_then(|defaults| defaults.temperature),
            Some(0.2)
        );
        assert_mode_inherits_agent_surface(&agent, &resolved);
    }

    #[test]
    fn test_glob_matches_exact() {
        assert!(glob_matches("tree", "tree"));
        assert!(!glob_matches("tree", "cat"));
    }

    #[test]
    fn test_glob_matches_wildcard() {
        assert!(glob_matches("*", "anything"));
        assert!(glob_matches("*", "tree"));
    }

    #[test]
    fn test_glob_matches_prefix() {
        assert!(glob_matches("search_*", "search_pattern"));
        assert!(glob_matches("search_*", "search_semantic"));
        assert!(!glob_matches("search_*", "tree"));
    }

    #[test]
    fn test_glob_matches_suffix() {
        assert!(glob_matches("*_textdoc", "create_textdoc"));
        assert!(glob_matches("*_textdoc", "update_textdoc"));
        assert!(!glob_matches("*_textdoc", "tree"));
    }

    #[test]
    fn test_match_tool_confirm_action() {
        let rules = vec![
            ToolConfirmRule {
                match_pattern: "tree".to_string(),
                action: "auto".to_string(),
            },
            ToolConfirmRule {
                match_pattern: "search_*".to_string(),
                action: "auto".to_string(),
            },
            ToolConfirmRule {
                match_pattern: "*".to_string(),
                action: "ask".to_string(),
            },
        ];

        assert_eq!(
            match_tool_confirm_action(&rules, "tree"),
            Some("auto".to_string())
        );
        assert_eq!(
            match_tool_confirm_action(&rules, "search_pattern"),
            Some("auto".to_string())
        );
        assert_eq!(
            match_tool_confirm_action(&rules, "shell"),
            Some("ask".to_string())
        );
    }

    #[test]
    fn test_match_tool_confirm_action_empty_rules() {
        let rules: Vec<ToolConfirmRule> = vec![];
        assert_eq!(match_tool_confirm_action(&rules, "tree"), None);
    }

    #[test]
    fn test_map_legacy_mode_to_id() {
        assert_eq!(map_legacy_mode_to_id("AGENT"), "agent");
        assert_eq!(map_legacy_mode_to_id("EXPLORE"), "explore");
        assert_eq!(map_legacy_mode_to_id("NO_TOOLS"), "explore");
        assert_eq!(map_legacy_mode_to_id("CONFIGURE"), "configurator");
        assert_eq!(map_legacy_mode_to_id("TASK_PLANNER"), "task_planner");
        assert_eq!(map_legacy_mode_to_id("TASK_AGENT"), "task_agent");
        assert_eq!(map_legacy_mode_to_id("BRAINSTORM"), "brainstorming");
        assert_eq!(map_legacy_mode_to_id("BRAINSTORMING"), "brainstorming");
    }

    #[test]
    fn test_map_legacy_mode_to_id_lowercase_passthrough() {
        assert_eq!(map_legacy_mode_to_id("agent"), "agent");
        assert_eq!(map_legacy_mode_to_id("custom_mode"), "custom_mode");
        assert_eq!(map_legacy_mode_to_id("my-mode-123"), "my-mode-123");
    }

    #[test]
    fn test_map_legacy_mode_to_id_invalid_fallback() {
        assert_eq!(map_legacy_mode_to_id("Invalid Mode"), "agent");
        assert_eq!(map_legacy_mode_to_id("Mode!"), "agent");
    }

    #[test]
    fn test_registry_has_runtime_required_subagents() {
        use crate::yaml_configs::project_configs_bootstrap::global_configs_try_create_all;

        let temp_dir = tempfile::tempdir().unwrap();
        let config_dir = temp_dir.path();

        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let _ = global_configs_try_create_all(config_dir).await;
            let registry = load_registry_from_dir(config_dir).await;

            for id in runtime_required_subagent_ids() {
                assert!(
                    registry.subagents.contains_key(id),
                    "Missing required subagent: {}. Available: {:?}",
                    id,
                    registry.subagents.keys().collect::<Vec<_>>()
                );
            }
        });
    }

    #[test]
    fn test_default_subagent_is_unified_and_not_yaml_tool_exposed() {
        let registry = load_default_registry_for_tests();
        assert!(registry.errors.is_empty(), "{:?}", registry.errors);

        let subagent = registry
            .subagents
            .get("subagent")
            .expect("subagent should load");
        assert_eq!(subagent.schema_version, 7);
        assert!(!subagent.expose_as_tool);
        assert!(subagent.description.contains("background"));
        assert!(subagent
            .messages
            .system_prompt
            .as_deref()
            .unwrap_or_default()
            .contains("validate_goal"));
        assert!(subagent.tools.contains(&"apply_patch".to_string()));
        assert_eq!(subagent.subchat.autonomous_no_confirm, Some(true));
        assert_eq!(subagent.subchat.auto_approve_editing_tools, Some(true));
        assert_eq!(subagent.subchat.auto_approve_dangerous_commands, Some(true));
    }

    #[test]
    fn test_builtin_subagent_tool_ids_are_not_exposed_as_config_tools() {
        for id in [
            "subagent",
            "agent_list",
            "agent_status",
            "agent_wait",
            "agent_result",
            "agent_cancel",
        ] {
            let config = exposed_config_subagent(id);
            assert!(is_builtin_subagent_tool_id(id));
            assert!(!should_expose_subagent_as_config_tool(&config));
        }

        let config = exposed_config_subagent("project_researcher");
        assert!(should_expose_subagent_as_config_tool(&config));
    }

    #[test]
    fn test_registry_subagents_have_valid_optional_subchat_params() {
        use crate::yaml_configs::project_configs_bootstrap::global_configs_try_create_all;

        let temp_dir = tempfile::tempdir().unwrap();
        let config_dir = temp_dir.path();

        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let _ = global_configs_try_create_all(config_dir).await;
            let registry = load_registry_from_dir(config_dir).await;

            for (id, config) in &registry.subagents {
                if let Some(n_ctx) = config.subchat.n_ctx {
                    assert!(n_ctx > 0, "Subagent '{}' has invalid n_ctx", id);
                }
                if let Some(max_new_tokens) = config.subchat.max_new_tokens {
                    assert!(
                        max_new_tokens > 0,
                        "Subagent '{}' has invalid max_new_tokens",
                        id
                    );
                }
                if let Some(ref model_type) = config.subchat.model_type {
                    let valid = model_type.eq_ignore_ascii_case("light")
                        || model_type.eq_ignore_ascii_case("default")
                        || model_type.eq_ignore_ascii_case("thinking")
                        || model_type.eq_ignore_ascii_case("buddy")
                        || model_type.eq_ignore_ascii_case("model_2")
                        || model_type.eq_ignore_ascii_case("task_planner");
                    assert!(
                        valid,
                        "Subagent '{}' has invalid model_type: {}",
                        id, model_type
                    );
                }
                if let Some(ref reasoning_effort) = config.subchat.reasoning_effort {
                    let valid = reasoning_effort.eq_ignore_ascii_case("low")
                        || reasoning_effort.eq_ignore_ascii_case("medium")
                        || reasoning_effort.eq_ignore_ascii_case("high")
                        || reasoning_effort.eq_ignore_ascii_case("xhigh")
                        || reasoning_effort.eq_ignore_ascii_case("max");
                    assert!(
                        valid,
                        "Subagent '{}' has invalid reasoning_effort: {}",
                        id, reasoning_effort
                    );
                }
            }
        });
    }

    #[test]
    fn test_setup_mode_stays_read_only_by_default() {
        let content = read_default_mode_file("setup.yaml");

        assert!(content.contains("allow_integrations: false"));
        assert!(content.contains("allow_mcp: false"));
        assert!(content.contains("allow_subagents: false"));
        assert!(!content.contains("\n  - rm\n"));
        assert!(!content.contains("\n  - mv\n"));
    }

    #[test]
    fn test_setup_mcp_prompt_prefers_safe_examples() {
        let content = read_default_mode_file("setup_mcp.yaml");

        assert!(
            content.contains("HTTP over deprecated SSE")
                || content.contains("prefer HTTP over deprecated SSE")
        );
        assert!(!content.contains("@latest"));
        assert!(content.contains("@modelcontextprotocol/server-github@<version>"));
    }

    #[test]
    fn test_setup_skills_prompt_documents_supported_skill_fields() {
        let content = read_default_mode_file("setup_skills.yaml");

        for key in [
            "argument-hint",
            "allowed-tools",
            "user-invocable",
            "disable-model-invocation",
            "@include <relative-file>",
        ] {
            assert!(
                content.contains(key),
                "setup_skills.yaml should mention '{}': {}",
                key,
                content
            );
        }
    }

    #[test]
    fn test_registry_has_no_mode_config_errors() {
        use crate::yaml_configs::project_configs_bootstrap::global_configs_try_create_all;

        let temp_dir = tempfile::tempdir().unwrap();
        let config_dir = temp_dir.path();

        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let _ = global_configs_try_create_all(config_dir).await;
            let registry = load_registry_from_dir(config_dir).await;

            let mode_errors: Vec<_> = registry
                .errors
                .iter()
                .filter(|e| {
                    Path::new(&e.file_path)
                        .components()
                        .any(|c| c == std::path::Component::Normal("modes".as_ref()))
                })
                .map(|e| format!("{}: {}", e.file_path, e.error))
                .collect();

            assert!(
                mode_errors.is_empty(),
                "Default mode configs should parse without errors. Found: {:?}",
                mode_errors
            );
        });
    }
}
