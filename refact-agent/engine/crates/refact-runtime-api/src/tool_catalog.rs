use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use tokio::sync::{Mutex as AMutex, RwLock as ARwLock};

use crate::ToolCatalogSnapshot;

#[derive(Default)]
pub struct ToolCatalogGenerations {
    pub customization: AtomicU64,
    pub integrations: AtomicU64,
    pub mcp: AtomicU64,
    pub privacy: AtomicU64,
    pub capabilities: AtomicU64,
}

impl ToolCatalogGenerations {
    pub fn advance_customization(&self) {
        self.customization.fetch_add(1, Ordering::SeqCst);
    }

    pub fn advance_integrations(&self) {
        self.integrations.fetch_add(1, Ordering::SeqCst);
    }

    pub fn advance_mcp(&self) {
        self.mcp.fetch_add(1, Ordering::SeqCst);
    }

    pub fn advance_privacy(&self) {
        self.privacy.fetch_add(1, Ordering::SeqCst);
    }

    pub fn advance_capabilities(&self) {
        self.capabilities.fetch_add(1, Ordering::SeqCst);
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct ToolCatalogKey {
    pub workspace_scope: String,
    pub execution_scope: Option<String>,
    pub mode: String,
    pub model: Option<String>,
    pub customization_generation: u64,
    pub integration_generation: u64,
    pub mcp_generation: u64,
    pub privacy_generation: u64,
    pub capability_generation: u64,
    pub extension_generation: u64,
}

pub const TOOL_CATALOG_SNAPSHOT_CACHE_LIMIT: usize = 128;

#[derive(Default)]
pub struct ToolCatalogCache {
    snapshots: ARwLock<HashMap<ToolCatalogKey, Arc<ToolCatalogSnapshot>>>,
    build_locks: AMutex<HashMap<ToolCatalogKey, Arc<AMutex<()>>>>,
}

impl ToolCatalogCache {
    pub async fn acquire_build_lock(&self, key: &ToolCatalogKey) -> Arc<AMutex<()>> {
        let mut locks = self.build_locks.lock().await;
        locks
            .entry(key.clone())
            .or_insert_with(|| Arc::new(AMutex::new(())))
            .clone()
    }

    pub async fn get(&self, key: &ToolCatalogKey) -> Option<Arc<ToolCatalogSnapshot>> {
        self.snapshots.read().await.get(key).cloned()
    }

    pub async fn insert(&self, key: ToolCatalogKey, snapshot: Arc<ToolCatalogSnapshot>) {
        let mut snapshots = self.snapshots.write().await;
        if snapshots.len() >= TOOL_CATALOG_SNAPSHOT_CACHE_LIMIT && !snapshots.contains_key(&key) {
            if let Some(evicted) = snapshots.keys().next().cloned() {
                snapshots.remove(&evicted);
            }
        }
        snapshots.insert(key, snapshot);
    }

    pub async fn release_build_lock(&self, key: &ToolCatalogKey) {
        self.build_locks.lock().await.remove(key);
    }

    pub async fn snapshot_count(&self) -> usize {
        self.snapshots.read().await.len()
    }

    pub async fn build_lock_count(&self) -> usize {
        self.build_locks.lock().await.len()
    }
}
