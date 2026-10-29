use std::any::Any;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use refact_tool_api::{ToolAliasRegistry, ToolDesc};

#[derive(Clone)]
pub struct ToolRegistryIndex {
    pub tools: Vec<ToolDesc>,
    pub mcp_lazy_mode: bool,
    pub mcp_total_count: usize,
    pub mcp_tool_index: Vec<(String, String)>,
}

#[derive(Clone)]
pub struct ToolCatalogSnapshot {
    pub index: ToolRegistryIndex,
    pub policy: Vec<ToolPolicyInfo>,
    pub aliases: ToolAliasRegistry,
}

#[derive(Clone)]
pub struct ToolPolicyInfo {
    pub name: String,
    pub effective_allow_parallel: bool,
}

#[derive(Clone)]
pub struct TurnToolPool {
    inner: Arc<dyn Any + Send + Sync>,
    initial_vector_builds: Arc<AtomicU64>,
    fallback_vector_builds: Arc<AtomicU64>,
}

impl TurnToolPool {
    pub fn new<T>(inner: T) -> Self
    where
        T: Any + Send + Sync,
    {
        Self {
            inner: Arc::new(inner),
            initial_vector_builds: Arc::new(AtomicU64::new(0)),
            fallback_vector_builds: Arc::new(AtomicU64::new(0)),
        }
    }

    pub fn downcast_ref<T>(&self) -> Option<&T>
    where
        T: Any,
    {
        self.inner.downcast_ref()
    }

    pub fn record_initial_vector_build(&self) {
        self.initial_vector_builds.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_fallback_vector_build(&self) {
        self.fallback_vector_builds.fetch_add(1, Ordering::Relaxed);
    }

    pub fn initial_vector_builds(&self) -> u64 {
        self.initial_vector_builds.load(Ordering::Relaxed)
    }

    pub fn fallback_vector_builds(&self) -> u64 {
        self.fallback_vector_builds.load(Ordering::Relaxed)
    }
}
