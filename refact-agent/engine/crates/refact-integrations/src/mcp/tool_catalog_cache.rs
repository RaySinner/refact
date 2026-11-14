use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

/// Process-scoped cache of the last successfully listed tool catalog per
/// config path. Lets `mcp_integr_tools` expose tools while a slow server is
/// still starting or reconnecting; execution against a not-yet-connected
/// client fails gracefully in `ToolMCP::tool_execute`.
static MCP_TOOL_CATALOG_CACHE: std::sync::Mutex<
    Option<HashMap<String, (std::time::Instant, Arc<Vec<rmcp::model::Tool>>)>>,
> = std::sync::Mutex::new(None);
const TOOL_CATALOG_CACHE_TTL_SECS: u64 = 1800;
const TOOL_CATALOG_CACHE_MAX_ENTRIES: usize = 64;

pub fn tool_catalog_cache_store(config_path: &str, tools: &[rmcp::model::Tool]) {
    let mut guard = match MCP_TOOL_CATALOG_CACHE.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    let cache = guard.get_or_insert_with(HashMap::new);
    if cache.len() >= TOOL_CATALOG_CACHE_MAX_ENTRIES && !cache.contains_key(config_path) {
        if let Some(oldest_key) = cache
            .iter()
            .min_by_key(|(_, (at, _))| *at)
            .map(|(key, _)| key.clone())
        {
            cache.remove(&oldest_key);
        }
    }
    cache.insert(
        config_path.to_string(),
        (std::time::Instant::now(), Arc::new(tools.to_vec())),
    );
}

pub fn tool_catalog_cache_get(config_path: &str) -> Option<Arc<Vec<rmcp::model::Tool>>> {
    let mut guard = match MCP_TOOL_CATALOG_CACHE.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    let cache = guard.as_mut()?;
    match cache.get(config_path) {
        Some((at, tools)) if at.elapsed() < Duration::from_secs(TOOL_CATALOG_CACHE_TTL_SECS) => {
            Some(Arc::clone(tools))
        }
        Some(_) => {
            cache.remove(config_path);
            None
        }
        None => None,
    }
}

pub fn tool_catalog_cache_remove(config_path: &str) {
    let mut guard = match MCP_TOOL_CATALOG_CACHE.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    if let Some(cache) = guard.as_mut() {
        cache.remove(config_path);
    }
}
