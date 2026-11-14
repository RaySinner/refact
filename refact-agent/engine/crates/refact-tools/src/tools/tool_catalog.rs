use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use refact_perf::perf_diagnostics::{self, PerfComponent, PerfOutcome};
use refact_core::custom_error::YamlError;
use refact_integrations::mcp::mcp_naming::server_name_from_config_path;
use refact_runtime_api::caps_loader::try_load_caps_quickly_if_not_present;
use refact_runtime_api::global_context::GlobalContext;
use refact_runtime_api::integr_abstract::IntegrationTrait;
use refact_runtime_api::integrations_runtime::load_integrations;

use crate::tools::tools_description::{
    Tool, ToolDesc, ToolGroup, ToolGroupCategory, ToolSourceType,
};

/// When MCP tool count exceeds this threshold, lazy loading activates.
/// The full MCP schemas are replaced by two fixed proxy tools:
/// - `mcp_tool_search` — discover MCP tools by regex, returns schema text
/// - `mcp_call`        — execute any MCP tool by name + args JSON
///
/// The tool list is FIXED for the entire session (cache-safe).
pub const MCP_LAZY_THRESHOLD: usize = 15;

pub type LoadedIntegrations = indexmap::IndexMap<String, Box<dyn IntegrationTrait + Send + Sync>>;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct IntegrationToolsCacheKey {
    config_dir: PathBuf,
    generation: u64,
}

#[derive(Default)]
struct IntegrationToolsCache {
    integrations: tokio::sync::RwLock<HashMap<IntegrationToolsCacheKey, Arc<LoadedIntegrations>>>,
    yaml_errors: tokio::sync::RwLock<HashMap<IntegrationToolsCacheKey, Arc<Vec<YamlError>>>>,
    build_locks:
        tokio::sync::Mutex<HashMap<IntegrationToolsCacheKey, Arc<tokio::sync::Mutex<()>>>>,
}

impl IntegrationToolsCache {
    async fn acquire_build_lock(
        &self,
        key: &IntegrationToolsCacheKey,
    ) -> Arc<tokio::sync::Mutex<()>> {
        let mut locks = self.build_locks.lock().await;
        locks
            .entry(key.clone())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    }

    async fn get(&self, key: &IntegrationToolsCacheKey) -> Option<Arc<LoadedIntegrations>> {
        self.integrations.read().await.get(key).cloned()
    }

    async fn insert(
        &self,
        key: IntegrationToolsCacheKey,
        integrations: Arc<LoadedIntegrations>,
        yaml_errors: Arc<Vec<YamlError>>,
    ) {
        let mut cached = self.integrations.write().await;
        cached.retain(|existing, _| existing.config_dir != key.config_dir);
        cached.insert(key.clone(), integrations);
        drop(cached);
        let mut errors = self.yaml_errors.write().await;
        errors.retain(|existing, _| existing.config_dir != key.config_dir);
        errors.insert(key, yaml_errors);
    }
}

fn integration_tools_cache() -> &'static IntegrationToolsCache {
    static CACHE: std::sync::OnceLock<IntegrationToolsCache> = std::sync::OnceLock::new();
    CACHE.get_or_init(IntegrationToolsCache::default)
}

#[cfg(any(test, feature = "test-util"))]
pub type TestIntegrationFactory =
    Arc<dyn Fn(Arc<GlobalContext>) -> (LoadedIntegrations, Vec<YamlError>) + Send + Sync>;

#[cfg(any(test, feature = "test-util"))]
static TEST_INTEGRATION_FACTORY: std::sync::OnceLock<
    tokio::sync::Mutex<HashMap<PathBuf, TestIntegrationFactory>>,
> = std::sync::OnceLock::new();

#[cfg(any(test, feature = "test-util"))]
pub fn test_integration_factory(
) -> &'static tokio::sync::Mutex<HashMap<PathBuf, TestIntegrationFactory>> {
    TEST_INTEGRATION_FACTORY.get_or_init(|| tokio::sync::Mutex::new(HashMap::new()))
}

async fn load_integrations_cached(gcx: Arc<GlobalContext>) -> Arc<LoadedIntegrations> {
    let key = IntegrationToolsCacheKey {
        config_dir: gcx.config_dir.clone(),
        generation: gcx
            .tool_catalog_generations
            .integrations
            .load(Ordering::Acquire),
    };
    let cache = integration_tools_cache();
    if let Some(integrations) = cache.get(&key).await {
        return integrations;
    }
    let build_lock = cache.acquire_build_lock(&key).await;
    let _build_guard = build_lock.lock().await;
    if let Some(integrations) = cache.get(&key).await {
        return integrations;
    }
    #[cfg(any(test, feature = "test-util"))]
    if let Some(factory) = test_integration_factory()
        .lock()
        .await
        .get(&gcx.config_dir)
        .cloned()
    {
        let (integrations, yaml_errors) = factory(gcx);
        let integrations = Arc::new(integrations);
        cache
            .insert(key, integrations.clone(), Arc::new(yaml_errors))
            .await;
        return integrations;
    }
    let (integrations, yaml_errors) = load_integrations(gcx, &["**/*".to_string()]).await;
    let integrations = Arc::new(integrations);
    cache
        .insert(key, integrations.clone(), Arc::new(yaml_errors))
        .await;
    integrations
}

/// Result of applying MCP lazy-loading logic on a tool list.
pub struct ToolsForMode {
    /// Tool list to send to the LLM as schemas. Fixed for the session lifetime.
    pub tools: Vec<Box<dyn Tool + Send>>,
    /// True when lazy mode replaced MCP schemas with the two proxy tools.
    pub mcp_lazy_mode: bool,
    /// Total count of all MCP tools (for the hint message).
    pub mcp_total_count: usize,
    /// (name, description) index for all MCP tools.
    ///
    /// Retained in the catalog for callers that need a local MCP index; the chat preamble
    /// deliberately keeps discovery on demand via `mcp_tool_search`.
    pub mcp_tool_index: Vec<(String, String)>,
}

fn is_integration_mcp_tool(t: &Box<dyn Tool + Send>) -> bool {
    let d = t.tool_description();
    d.name.starts_with("mcp") && matches!(d.source.source_type, ToolSourceType::Integration)
}

pub fn apply_mcp_lazy_filter(mut tools: Vec<Box<dyn Tool + Send>>) -> ToolsForMode {
    let mcp_tool_index: Vec<(String, String)> = tools
        .iter()
        .filter(|tool| is_integration_mcp_tool(tool))
        .map(|tool| {
            let description = tool.tool_description();
            (description.name, description.description)
        })
        .collect();
    let mcp_total_count = mcp_tool_index.len();
    let mcp_lazy_mode = mcp_total_count > MCP_LAZY_THRESHOLD;

    if mcp_lazy_mode {
        tools.retain(|t| !is_integration_mcp_tool(t));
        tools.push(Box::new(crate::tools::tool_mcp_search::ToolMcpSearch {}));
        tools.push(Box::new(crate::tools::tool_mcp_call::ToolMcpCall {}));
    }

    ToolsForMode {
        tools,
        mcp_lazy_mode,
        mcp_total_count,
        mcp_tool_index: if mcp_lazy_mode {
            mcp_tool_index
        } else {
            Vec::new()
        },
    }
}

pub fn provider_of_model(model_id: &str) -> &str {
    model_id
        .split_once('/')
        .map_or(model_id, |(provider, _)| provider)
}

pub async fn tool_access_policy(gcx: Arc<GlobalContext>) -> Arc<refact_privacy::PrivacyPolicy> {
    refact_runtime_api::privacy::load_privacy_if_needed(gcx.clone()).await;
    gcx.privacy_policy_load.read().unwrap().policy.clone()
}

pub fn mcp_tool_allowed(
    policy: &refact_privacy::PrivacyPolicy,
    provider: &str,
    desc: &ToolDesc,
) -> bool {
    if !(desc.name.starts_with("mcp")
        && matches!(desc.source.source_type, ToolSourceType::Integration))
    {
        return true;
    }
    if provider.is_empty() {
        return false;
    }
    let server = server_name_from_config_path(&desc.source.config_path);
    policy.tool_access.mcp_allowed(provider, &server)
}

pub async fn mcp_config_allowed_for_model(
    gcx: Arc<GlobalContext>,
    model_id: &str,
    config_path: &str,
) -> bool {
    let policy = tool_access_policy(gcx).await;
    if policy.tool_access.providers.is_empty() {
        return true;
    }
    if model_id.is_empty() {
        return false;
    }
    let server = server_name_from_config_path(config_path);
    policy
        .tool_access
        .mcp_allowed(provider_of_model(model_id), &server)
}

fn tool_available(
    tool: &Box<dyn Tool + Send>,
    codegraph_on: bool,
    vecdb_on: bool,
    is_there_a_thinking_model: bool,
    allow_knowledge: bool,
    allow_experimental: bool,
) -> bool {
    let dependencies = tool.tool_depends_on();
    if dependencies
        .iter()
        .any(|dependency| dependency == "ast" || dependency == "codegraph")
        && !codegraph_on
    {
        return false;
    }
    if dependencies.iter().any(|dependency| dependency == "vecdb") && !vecdb_on {
        return false;
    }
    if dependencies
        .iter()
        .any(|dependency| dependency == "thinking")
        && !is_there_a_thinking_model
    {
        return false;
    }
    if dependencies
        .iter()
        .any(|dependency| dependency == "knowledge")
        && !allow_knowledge
    {
        return false;
    }
    if tool.tool_description().experimental && !allow_experimental {
        return false;
    }
    true
}

pub fn catalog_policy_for_tools(
    tools: &[Box<dyn Tool + Send>],
) -> Vec<refact_runtime_api::ToolPolicyInfo> {
    tools
        .iter()
        .map(|tool| {
            let desc = tool.tool_description();
            let config_override = if !desc.source.config_path.is_empty() {
                tool.config().ok().and_then(|config| config.allow_parallel)
            } else {
                None
            };
            refact_runtime_api::ToolPolicyInfo {
                name: desc.name,
                effective_allow_parallel: desc.allow_parallel && config_override.unwrap_or(true),
            }
        })
        .collect()
}

async fn tool_available_from_gcx(
    gcx: Arc<GlobalContext>,
) -> impl Fn(&Box<dyn Tool + Send>) -> bool {
    let (codegraph_on, vecdb_on, allow_experimental) = {
        let vecdb_on = gcx.vec_db.lock().await.is_some();
        let codegraph_on = gcx.codegraph.lock().await.is_some();
        (codegraph_on, vecdb_on, gcx.cmdline.experimental)
    };

    let is_there_a_thinking_model = match try_load_caps_quickly_if_not_present(gcx.clone(), 0).await
    {
        Ok(caps) => caps
            .chat_models
            .get(&caps.defaults.chat_thinking_model)
            .is_some(),
        Err(_) => false,
    };
    let allow_knowledge = true;

    move |tool: &Box<dyn Tool + Send>| {
        tool_available(
            tool,
            codegraph_on,
            vecdb_on,
            is_there_a_thinking_model,
            allow_knowledge,
            allow_experimental,
        )
    }
}

pub async fn retain_available_tools(tool_group: &mut ToolGroup, gcx: Arc<GlobalContext>) {
    let tool_available = tool_available_from_gcx(gcx.clone()).await;
    tool_group.tools.retain(|tool| tool_available(tool));
}

pub async fn get_integration_tools(gcx: Arc<GlobalContext>) -> Vec<ToolGroup> {
    let mut integrations_group = ToolGroup {
        name: "Integrations".to_string(),
        description: "Integration tools".to_string(),
        category: ToolGroupCategory::Integration,
        tools: vec![],
    };

    let mut mcp_groups = HashMap::new();

    let span = perf_diagnostics::span(PerfComponent::ToolIntegrationToolsBuild, None, None);
    let integrations_map = load_integrations_cached(gcx.clone()).await;
    for (name, integr) in integrations_map.iter() {
        for tool in integr.integr_tools(name).await {
            let tool_desc = tool.tool_description();
            if tool_desc.name.starts_with("mcp") {
                let mcp_server_name = std::path::Path::new(&tool_desc.source.config_path)
                    .file_stem()
                    .and_then(|name| name.to_str())
                    .unwrap_or("unknown");

                if !mcp_groups.contains_key(mcp_server_name) {
                    mcp_groups.insert(
                        mcp_server_name.to_string(),
                        ToolGroup {
                            name: format!("MCP {}", mcp_server_name),
                            description: format!("MCP tools for {}", mcp_server_name),
                            category: ToolGroupCategory::MCP,
                            tools: vec![],
                        },
                    );
                }
                mcp_groups
                    .entry(mcp_server_name.to_string())
                    .and_modify(|group| group.tools.push(tool));
            } else {
                integrations_group.tools.push(tool);
            }
        }
    }

    let mut sorted_mcp: Vec<(String, ToolGroup)> = mcp_groups.into_iter().collect();
    sorted_mcp.sort_by(|(a, _), (b, _)| a.cmp(b));

    let mut tool_groups = vec![integrations_group];
    tool_groups.extend(sorted_mcp.into_iter().map(|(_, group)| group));

    for tool_group in tool_groups.iter_mut() {
        retain_available_tools(tool_group, gcx.clone()).await;
    }

    span.finish_tool(PerfOutcome::Success, 1, tool_groups.len() as u64, None);

    tool_groups
}
