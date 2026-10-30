use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::{Arc, Mutex as StdMutex, RwLock as StdRwLock};

use refact_browser::BrowserRuntime;
use refact_caps_core::caps_state::CapsState;
use refact_core::tokenizer_state::TokenizerState;
use refact_exec::ExecRegistry;
use refact_integrations::sessions::IntegrationSession;
use refact_providers::ProviderRegistry;
use refact_stats::event::LlmCallEvent;
use refact_yaml_configs::registry_cache::RegistryCacheManager;
use tokio::sync::{Mutex as AMutex, RwLock as ARwLock};

use crate::CommandLine;

#[derive(Clone)]
pub struct RuntimeServices {
    pub shutdown_flag: Arc<AtomicBool>,
    pub cmdline: Arc<CommandLine>,
    pub http_client: reqwest::Client,
    pub ask_shutdown_sender: Arc<StdMutex<std::sync::mpsc::Sender<String>>>,
    pub exec_registry: Arc<ExecRegistry>,
}

#[derive(Clone)]
pub struct PathServices {
    pub cache_dir: PathBuf,
    pub config_dir: PathBuf,
    pub app_searchable_id: String,
}

#[derive(Clone)]
pub struct ModelServices {
    pub caps: Arc<ARwLock<CapsState>>,
    pub tokenizers: Arc<StdRwLock<TokenizerState>>,
    pub providers: Arc<ARwLock<ProviderRegistry>>,
    pub llm_stats_sender: Option<tokio::sync::mpsc::Sender<LlmCallEvent>>,
}

#[derive(Clone)]
pub struct IntegrationServices {
    pub integration_sessions:
        Arc<AMutex<std::collections::HashMap<String, Arc<AMutex<Box<dyn IntegrationSession>>>>>>,
    pub browser_runtimes:
        Arc<AMutex<std::collections::HashMap<String, Arc<AMutex<BrowserRuntime>>>>>,
    pub ext_cache_generation: Arc<std::sync::atomic::AtomicU64>,
    pub project_registry_cache: Arc<StdRwLock<RegistryCacheManager>>,
    pub init_shadow_repos_lock: Arc<AMutex<bool>>,
    pub git_operations_abort_flag: Arc<AtomicBool>,
}
use refact_buddy_core::actor::BuddyService;
use refact_buddy_core::events::DiagnosticBuddyEvent;
use refact_buddy_core::user_activity::UserActivityRing;
use refact_chat_api::SessionsMap;
use refact_chat_api::notification_event::NotificationEvent;
use refact_chat_history::trajectory_event::TrajectoryEvent;
use refact_chat_history::trajectory_index::TrajectoryIndexCoordinator;
use refact_core::at_commands_preview_cache::AtCommandsPreviewCache;
use refact_core::knowledge_index::KnowledgeIndex;
use refact_core::privacy_types::PrivacySettings;
use refact_core::vecdb_types::VecdbSearch;
use refact_files::documents_state::DocumentsState;
use refact_files::indexing_settings::IndexingEverywhere;
use refact_scratchpads::completion_cache::CompletionCache;
use refact_tasks::events::TaskEventEnvelope;

use crate::ChatSessionFacade;

#[derive(Clone)]
pub struct WorkspaceServices {
    pub documents_state: DocumentsState,
    pub privacy_settings: Arc<PrivacySettings>,
    pub indexing_everywhere: Arc<StdRwLock<Arc<IndexingEverywhere>>>,
    pub completions_cache: Arc<StdRwLock<CompletionCache>>,
    pub vec_db: Arc<AMutex<Option<Arc<dyn VecdbSearch>>>>,
    pub vec_db_error: Arc<StdMutex<String>>,
    pub knowledge_index: Arc<AMutex<KnowledgeIndex>>,
    pub at_commands_preview_cache: Arc<AMutex<AtCommandsPreviewCache>>,
}

#[derive(Clone)]
pub struct ChatServices {
    pub sessions: SessionsMap,
    pub facade: Arc<dyn ChatSessionFacade>,
    pub trajectory_index_coordinator: Arc<TrajectoryIndexCoordinator>,
    pub trajectory_events_tx: tokio::sync::broadcast::Sender<TrajectoryEvent>,
    pub workspace_changed_tx: tokio::sync::broadcast::Sender<()>,
    pub task_events_tx: tokio::sync::broadcast::Sender<TaskEventEnvelope>,
    pub task_events_seq: Arc<AtomicU64>,
    pub notification_events_tx: tokio::sync::broadcast::Sender<NotificationEvent>,
}

#[derive(Clone)]
pub struct BuddyServices {
    pub buddy: Arc<AMutex<Option<BuddyService>>>,
    pub buddy_events_tx: tokio::sync::broadcast::Sender<DiagnosticBuddyEvent>,
    pub user_activity: Arc<AMutex<UserActivityRing>>,
}

