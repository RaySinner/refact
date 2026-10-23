use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex as StdMutex, RwLock as StdRwLock};

use async_trait::async_trait;
use refact_chat_api::ChatMessage;
use refact_core::vecdb_types::VecdbSearch;
use refact_runtime_api::{ActivitySink, BuddyEventSink, ToolRegistry};
use tokio::sync::Mutex as AMutex;

use crate::agents::registry::BackgroundAgentRegistry;
use crate::app_state::{
    AtCommandsPreviewCache, BuddyServices, ChatServices, CommandLine, ModelServices, PathServices,
    RuntimeServices, ToolCatalogGenerations,
};
use crate::chat::trajectories::TrajectoryVectorizationQueue;
use crate::integrations::browser_runtime::BrowserRuntime;
use crate::knowledge_index::KnowledgeIndex;
use crate::privacy::PrivacySettings;
use crate::subchat::{SubchatConfig, SubchatResult};
use crate::yaml_configs::customization_registry::RegistryCacheManager;

#[derive(Clone)]
pub struct ChatWorkspace {
    pub workspace_folders: Arc<StdMutex<Vec<PathBuf>>>,
    pub vec_db: Arc<AMutex<Option<Arc<dyn VecdbSearch>>>>,
    pub vec_db_error: Arc<StdMutex<String>>,
    pub knowledge_index: Arc<AMutex<KnowledgeIndex>>,
    pub at_commands_preview_cache: Arc<AMutex<AtCommandsPreviewCache>>,
    pub privacy_settings: Arc<PrivacySettings>,
}

#[derive(Clone)]
pub struct ChatIntegrationScope {
    pub browser_runtimes: Arc<AMutex<HashMap<String, Arc<AMutex<BrowserRuntime>>>>>,
}

#[async_trait]
pub trait SubchatLauncher: Send + Sync {
    async fn run_subchat(
        &self,
        messages: Vec<ChatMessage>,
        config: SubchatConfig,
    ) -> Result<SubchatResult, String>;
}

#[async_trait]
pub trait ProjectTreeProvider: Send + Sync {
    async fn generate_compact_project_tree(&self, max_depth: usize) -> Result<String, String>;
}

#[derive(Clone)]
pub struct ChatCtx {
    pub runtime: RuntimeServices,
    pub paths: PathServices,
    pub model: ModelServices,
    pub workspace: ChatWorkspace,
    pub chat: ChatServices,
    pub buddy: BuddyServices,
    pub integrations: ChatIntegrationScope,
    pub agents: Arc<BackgroundAgentRegistry>,
    pub tool_registry: Arc<dyn ToolRegistry>,
    pub activity_sink: Arc<dyn ActivitySink>,
    pub buddy_event_sink: Arc<dyn BuddyEventSink>,
    pub card_verifier_tasks: Arc<AMutex<tokio::task::JoinSet<()>>>,
    pub privacy_policy_load: Arc<StdRwLock<refact_privacy::PolicyLoad>>,
    pub tool_catalog_generations: Arc<ToolCatalogGenerations>,
    pub project_registry_cache: Arc<StdRwLock<RegistryCacheManager>>,
    pub trajectory_vectorization_queue: Arc<AMutex<TrajectoryVectorizationQueue>>,
    pub subchat: Arc<dyn SubchatLauncher>,
    pub project_tree: Arc<dyn ProjectTreeProvider>,
}
