//! Canonical OpenAI tool array for a mode: alias assignment plus the
//! `handoff_to_mode` enrichment that advertises the available modes.
//!
//! Lives here rather than in `refact-tools` because the context-rebuild budget
//! check has to build the exact array the wire adapters serialize, and that check
//! must not reach into the tool shell. `refact-tools::chat_prepare` re-exports it.

use serde_json::Value;

use refact_tool_api::{build_registry_from_names, ToolAliasRegistry, ToolDesc};

use crate::chat_ctx::ChatCtx;

pub struct CanonicalOpenAiTools {
    pub tools: Vec<Value>,
    pub alias_registry: ToolAliasRegistry,
}

pub fn build_canonical_openai_tools_from_aliases(
    tools: &[ToolDesc],
    aliases: ToolAliasRegistry,
    mode_supports_strict: bool,
    supports_tools: bool,
) -> CanonicalOpenAiTools {
    let filtered_tools: Vec<ToolDesc> = if supports_tools {
        tools.to_vec()
    } else {
        vec![]
    };
    let openai_tools = filtered_tools
        .iter()
        .map(|tool| {
            let alias = aliases
                .get_alias(&tool.name)
                .unwrap_or(&tool.name)
                .to_string();
            let mut value = tool.clone().into_openai_style(mode_supports_strict);
            if alias != tool.name {
                if let Some(function) = value.get_mut("function") {
                    function["name"] = serde_json::Value::String(alias);
                }
            }
            value
        })
        .collect();

    CanonicalOpenAiTools {
        tools: openai_tools,
        alias_registry: aliases,
    }
}

pub async fn build_canonical_openai_tools(
    ctx: ChatCtx,
    tools: &[ToolDesc],
    mode_supports_strict: bool,
    supports_tools: bool,
) -> CanonicalOpenAiTools {
    let filtered_tools: Vec<ToolDesc> = if supports_tools {
        tools.to_vec()
    } else {
        vec![]
    };
    let tool_names: Vec<String> = filtered_tools.iter().map(|t| t.name.clone()).collect();
    let alias_registry = build_registry_from_names(&tool_names);
    let mut openai_tools: Vec<Value> = filtered_tools
        .iter()
        .map(|tool| {
            let alias = alias_registry
                .get_alias(&tool.name)
                .unwrap_or(&tool.name)
                .to_string();
            let mut v = tool.clone().into_openai_style(mode_supports_strict);
            if alias != tool.name {
                if let Some(func) = v.get_mut("function") {
                    func["name"] = serde_json::Value::String(alias);
                }
            }
            v
        })
        .collect();

    if supports_tools {
        let handoff_alias = alias_registry
            .get_alias("handoff_to_mode")
            .unwrap_or("handoff_to_mode");
        if let Some(idx) = openai_tools.iter().position(|t| {
            t.get("function")
                .and_then(|f| f.get("name"))
                .and_then(|n| n.as_str())
                .map(|n| n == handoff_alias)
                .unwrap_or(false)
        }) {
            if let Some(registry) = ctx.mode_config.get_project_registry().await {
                let mut mode_lines = Vec::new();
                let mut mode_ids = Vec::new();
                let mut modes: Vec<_> = registry.modes.values().collect();
                modes.sort_by(|a, b| a.id.cmp(&b.id));
                for mode in modes {
                    if mode.specific {
                        continue;
                    }
                    let title = if mode.title.is_empty() {
                        mode.id.clone()
                    } else {
                        mode.title.clone()
                    };
                    let mut desc = mode.description.clone();
                    if desc.len() > 120 {
                        desc = format!("{}...", desc.chars().take(120).collect::<String>());
                    }
                    mode_lines.push(format!(
                        "- {}: {}",
                        mode.id,
                        if desc.is_empty() { title } else { desc }
                    ));
                    mode_ids.push(mode.id.clone());
                }
                let mode_list = mode_lines.join("\n");
                if let Some(func) = openai_tools[idx].get_mut("function") {
                    if let Some(desc_val) = func.get_mut("description") {
                        let desc = desc_val.as_str().unwrap_or("");
                        let enriched = format!("{}\n\nAvailable modes:\n{}", desc, mode_list);
                        *desc_val = serde_json::Value::String(enriched);
                    }
                    if let Some(params) = func.get_mut("parameters") {
                        if let Some(props) = params.get_mut("properties") {
                            if let Some(target_mode) = props.get_mut("target_mode") {
                                let desc =
                                    format!("Target mode ID. Available modes:\n{}", mode_list);
                                target_mode["description"] = serde_json::Value::String(desc);
                                target_mode["enum"] = serde_json::Value::Array(
                                    mode_ids
                                        .into_iter()
                                        .map(serde_json::Value::String)
                                        .collect(),
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    CanonicalOpenAiTools {
        tools: openai_tools,
        alias_registry,
    }
}
