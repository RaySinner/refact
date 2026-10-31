use std::path::PathBuf;
use std::sync::{Arc, Mutex as StdMutex, RwLock as StdRwLock};

use refact_agents_core::registry::BackgroundAgentRegistry;
use refact_chat_api::notification_event::NotificationEvent;
use refact_codegraph::CodeGraphService;
use refact_core::knowledge_index::KnowledgeIndex;
use refact_core::privacy_types::PrivacySettings;
use refact_privacy::PolicyLoad;
use tokio::sync::{broadcast, Mutex as AMutex};

use crate::services::{IntegrationServices, ModelServices, PathServices, RuntimeServices};
use crate::{
    BuddyServicesHandle, CapsProvider, ChatSessionFacade, FilesProvider, ModeConfigProvider,
    SessionsHandle, SubchatLauncherHandle, ToolRegistry, TrajectoryStoreHandle,
};

#[derive(Clone)]
pub struct ToolChatScope {
    pub sessions: Arc<dyn SessionsHandle>,
    pub facade: Arc<dyn ChatSessionFacade>,
    pub notification_events_tx: broadcast::Sender<NotificationEvent>,
}

#[derive(Clone)]
pub struct ToolWorkspaceScope {
    pub knowledge_index: Arc<AMutex<KnowledgeIndex>>,
    pub privacy_settings: Arc<StdRwLock<Arc<PrivacySettings>>>,
    pub workspace_files: Arc<StdMutex<Vec<PathBuf>>>,
}

#[derive(Clone)]
pub struct ToolCodeIntelScope {
    pub codegraph: Arc<AMutex<Option<Arc<CodeGraphService>>>>,
}

#[derive(Clone)]
pub struct ToolCtx {
    pub runtime: RuntimeServices,
    pub paths: PathServices,
    pub model: ModelServices,
    pub chat: ToolChatScope,
    pub buddy: Arc<dyn BuddyServicesHandle>,
    pub integrations: IntegrationServices,
    pub workspace: ToolWorkspaceScope,
    pub codeintel: ToolCodeIntelScope,
    pub agents: Arc<BackgroundAgentRegistry>,
    pub tool_registry: Arc<dyn ToolRegistry>,
    pub privacy_policy_load: Arc<StdRwLock<PolicyLoad>>,
    pub files: Arc<dyn FilesProvider>,
    pub at_command_executor: Arc<dyn crate::AtCommandExecutor>,
    pub trajectories: Arc<dyn TrajectoryStoreHandle>,
    pub subchat: Arc<dyn SubchatLauncherHandle>,
    pub mode_config: Arc<dyn ModeConfigProvider>,
    pub caps: Arc<dyn CapsProvider>,
}
