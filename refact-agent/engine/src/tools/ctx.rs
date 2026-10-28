use std::path::PathBuf;
use std::sync::{Arc, RwLock as StdRwLock};

use async_trait::async_trait;
use refact_runtime_api::{ChatSessionFacade, ToolRegistry};
use tokio::sync::{broadcast, Mutex as AMutex};

use crate::agents::registry::BackgroundAgentRegistry;
use crate::app_state::{
    BuddyServices, IntegrationServices, ModelServices, PathServices, RuntimeServices,
};
use crate::chat::ctx::{CapsProvider, ModeConfigProvider, SubchatLauncher, TrajectoryStore};
use crate::chat::SessionsMap;
use crate::codegraph::CodeGraphService;
use crate::http::routers::v1::sidebar::NotificationEvent;
use crate::knowledge_index::KnowledgeIndex;
use crate::privacy::PrivacySettings;

#[derive(Clone)]
pub struct ToolChatScope {
    pub sessions: SessionsMap,
    pub facade: Arc<dyn ChatSessionFacade>,
    pub notification_events_tx: broadcast::Sender<NotificationEvent>,
}

#[derive(Clone)]
pub struct ToolWorkspaceScope {
    pub knowledge_index: Arc<AMutex<KnowledgeIndex>>,
    pub privacy_settings: Arc<StdRwLock<Arc<PrivacySettings>>>,
}

#[derive(Clone)]
pub struct ToolCodeIntelScope {
    pub codegraph: Arc<AMutex<Option<Arc<CodeGraphService>>>>,
}

#[async_trait]
pub trait FilesProvider: Send + Sync {
    async fn get_file_text_from_memory_or_disk(&self, path: &PathBuf) -> Result<String, String>;
    async fn fast_enqueue_for_edit(&self, docs: &[PathBuf]) -> Result<(), String>;
    async fn remove_memory_document_for_path(&self, path: &PathBuf) -> bool;
    async fn file_repair_candidates(&self, value: &str, top_n: usize, fuzzy: bool) -> Vec<String>;
}

#[derive(Clone)]
pub struct ToolCtx {
    pub runtime: RuntimeServices,
    pub paths: PathServices,
    pub model: ModelServices,
    pub chat: ToolChatScope,
    pub buddy: BuddyServices,
    pub integrations: IntegrationServices,
    pub workspace: ToolWorkspaceScope,
    pub codeintel: ToolCodeIntelScope,
    pub agents: Arc<BackgroundAgentRegistry>,
    pub tool_registry: Arc<dyn ToolRegistry>,
    pub privacy_policy_load: Arc<StdRwLock<refact_privacy::PolicyLoad>>,
    pub files: Arc<dyn FilesProvider>,
    pub trajectories: Arc<dyn TrajectoryStore>,
    pub subchat: Arc<dyn SubchatLauncher>,
    pub mode_config: Arc<dyn ModeConfigProvider>,
    pub caps: Arc<dyn CapsProvider>,
}
