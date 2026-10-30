use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize};
use std::sync::{Arc, Mutex as StdMutex, RwLock as StdRwLock};

use refact_agents_core::registry::BackgroundAgentRegistry;
use refact_buddy_core::actor::BuddyService;
use refact_buddy_core::events::DiagnosticBuddyEvent;
use refact_buddy_core::user_activity::UserActivityRing;
use refact_caps_core::caps_state::CapsState;
use refact_chat_api::notification_event::NotificationEvent;
use refact_chat_api::vectorization_queue::TrajectoryVectorizationQueue;
use refact_chat_api::SessionsMap;
use refact_chat_history::trajectory_event::TrajectoryEvent;
use refact_chat_history::trajectory_index::TrajectoryIndexCoordinator;
use refact_codegraph::CodeGraphService;
use refact_context_api::{HttpClientAccess, PathsAccess, ShutdownAccess};
use refact_core::at_commands_preview_cache::AtCommandsPreviewCache;
use refact_core::background_tasks::BackgroundTasksHolder;
use refact_core::knowledge_index::KnowledgeIndex;
use refact_core::privacy_types::PrivacySettings;
use refact_core::scheduler_config::SchedulerConfig;
use refact_core::tokenizer_state::TokenizerState;
use refact_core::vecdb_types::VecdbSearch;
use refact_daemon::daemon::lock::WorkspaceLeaseSet;
use refact_exec::ExecRegistry;
use refact_ext::hooks::HooksConfig;
use refact_files::documents_state::DocumentsState;
use refact_files::file_index::FileIndex;
use refact_files::indexing_settings::IndexingEverywhere;
use refact_integrations::sessions::IntegrationSession;
use refact_browser::BrowserRuntime;
use refact_knowledge_graph::enrichment_cache::EnrichmentCache;
use refact_perf::perf_telemetry::PerformanceTelemetry;
use refact_privacy::runtime_state::{PrivacyMatchCounts, PrivacyObservationRuntimeState};
use refact_privacy::PolicyLoad;
use refact_providers::quota::ProviderQuotaCache;
use refact_providers::ProviderRegistry;
use refact_scratchpads::completion_cache::CompletionCache;
use refact_stats::event::LlmCallEvent;
use refact_tasks::events::TaskEventEnvelope;
use refact_yaml_configs::registry_cache::RegistryCacheManager;
use serde::Deserialize;
use tokio::sync::{Mutex as AMutex, RwLock as ARwLock};

pub use crate::terminal_security::{TerminalSecurityConfig, TerminalSecurityMode};

#[derive(Debug, Deserialize)]
pub struct EngineGlobalConfig {
    #[serde(default)]
    pub scheduler: SchedulerConfig,
    #[serde(default)]
    pub hooks: HooksConfig,
    #[serde(default)]
    pub terminal_security: TerminalSecurityConfig,
}

impl Default for EngineGlobalConfig {
    fn default() -> Self {
        Self {
            scheduler: SchedulerConfig::default(),
            hooks: HooksConfig::default(),
            terminal_security: TerminalSecurityConfig::default(),
        }
    }
}

pub struct GlobalContext {
    pub shutdown_flag: Arc<AtomicBool>,
    pub lsp_tcp_client_count: Arc<AtomicUsize>,
    pub exec_registry: Arc<ExecRegistry>,
    pub card_verifier_tasks: Arc<AMutex<tokio::task::JoinSet<()>>>,
    pub trajectory_vectorization_queue: Arc<AMutex<TrajectoryVectorizationQueue>>,
    pub cmdline: crate::CommandLine,
    pub http_client: reqwest::Client,
    pub cache_dir: PathBuf,
    pub config_dir: PathBuf,
    pub caps_state: Arc<ARwLock<CapsState>>,
    pub tokenizer_state: Arc<StdRwLock<TokenizerState>>,
    pub completions_cache: Arc<StdRwLock<CompletionCache>>,
    pub vec_db: Arc<AMutex<Option<Arc<dyn VecdbSearch>>>>,
    pub vec_db_error: Arc<StdMutex<String>>,
    pub codegraph: Arc<AMutex<Option<Arc<CodeGraphService>>>>,
    pub codegraph_error: Arc<StdMutex<String>>,
    pub ask_shutdown_sender: Arc<StdMutex<std::sync::mpsc::Sender<String>>>,
    pub documents_state: DocumentsState,
    pub at_commands_preview_cache: Arc<AMutex<AtCommandsPreviewCache>>,
    pub privacy_settings: Arc<StdRwLock<Arc<PrivacySettings>>>,
    pub privacy_policy_load: Arc<StdRwLock<PolicyLoad>>,
    pub file_index: Arc<FileIndex>,
    pub privacy_match_counts: Arc<StdRwLock<PrivacyMatchCounts>>,
    pub privacy_observation_runtime: Arc<StdRwLock<PrivacyObservationRuntimeState>>,
    pub indexing_everywhere: Arc<StdRwLock<Arc<IndexingEverywhere>>>,
    pub integration_sessions:
        Arc<AMutex<HashMap<String, Arc<AMutex<Box<dyn IntegrationSession>>>>>>,
    pub browser_runtimes: Arc<AMutex<HashMap<String, Arc<AMutex<BrowserRuntime>>>>>,
    pub init_shadow_repos_background_task_holder: BackgroundTasksHolder,
    pub init_shadow_repos_lock: Arc<AMutex<bool>>,
    pub git_operations_abort_flag: Arc<AtomicBool>,
    pub app_searchable_id: Arc<StdMutex<String>>,
    pub trajectory_events_tx: Option<tokio::sync::broadcast::Sender<TrajectoryEvent>>,
    pub workspace_changed_tx: Option<tokio::sync::broadcast::Sender<()>>,
    pub task_events_tx: Option<tokio::sync::broadcast::Sender<TaskEventEnvelope>>,
    pub task_events_seq: Option<Arc<std::sync::atomic::AtomicU64>>,
    pub notification_events_tx:
        Option<tokio::sync::broadcast::Sender<NotificationEvent>>,
    pub chat_sessions: SessionsMap,
    pub trajectory_index_coordinator: Arc<TrajectoryIndexCoordinator>,
    pub performance_telemetry: Arc<PerformanceTelemetry>,
    pub project_registry_cache: Arc<StdRwLock<RegistryCacheManager>>,
    pub providers: Arc<ARwLock<ProviderRegistry>>,
    pub quota_cache: Arc<AMutex<ProviderQuotaCache>>,
    pub knowledge_index: Arc<AMutex<KnowledgeIndex>>,
    pub enrichment_cache: Arc<EnrichmentCache>,
    pub enrichment_generation: Arc<AtomicU64>,
    pub llm_stats_sender:
        Arc<StdMutex<Option<tokio::sync::mpsc::Sender<LlmCallEvent>>>>,
    pub ext_cache_generation: Arc<std::sync::atomic::AtomicU64>,
    pub tool_catalog_generations: Arc<crate::tool_catalog::ToolCatalogGenerations>,
    pub tool_catalog_cache: Arc<crate::tool_catalog::ToolCatalogCache>,
    pub buddy: Arc<AMutex<Option<BuddyService>>>,
    pub buddy_events_tx: Option<tokio::sync::broadcast::Sender<DiagnosticBuddyEvent>>,
    pub user_activity: Arc<AMutex<UserActivityRing>>,
    pub agents: Arc<BackgroundAgentRegistry>,
    pub scheduler_config: SchedulerConfig,
    pub hooks_config: HooksConfig,
    pub terminal_security_config: TerminalSecurityConfig,
    /// Exclusive per-workspace leases held for the whole engine lifetime; dropping the context
    /// closes the lock descriptors and lets another engine take over the workspace.
    pub workspace_leases: Arc<StdMutex<WorkspaceLeaseSet>>,
}

pub type SharedGlobalContext = Arc<GlobalContext>;

impl ShutdownAccess for GlobalContext {
    fn shutdown_flag(&self) -> Arc<AtomicBool> {
        self.shutdown_flag.clone()
    }
}

impl PathsAccess for GlobalContext {
    fn cache_dir(&self) -> PathBuf {
        self.cache_dir.clone()
    }

    fn config_dir(&self) -> PathBuf {
        self.config_dir.clone()
    }

    fn workspace_folders(&self) -> Vec<PathBuf> {
        self.documents_state
            .workspace_folders
            .lock()
            .unwrap()
            .clone()
    }
}

impl HttpClientAccess for GlobalContext {
    fn http_client(&self) -> reqwest::Client {
        self.http_client.clone()
    }
}

pub fn build_shared_http_client_builder() -> reqwest::ClientBuilder {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(std::time::Duration::from_secs(15))
        .tcp_keepalive(Some(std::time::Duration::from_secs(30)))
        .pool_idle_timeout(Some(std::time::Duration::from_secs(60)))
        .http2_keep_alive_interval(Some(std::time::Duration::from_secs(20)))
        .http2_keep_alive_timeout(std::time::Duration::from_secs(10))
        .http2_keep_alive_while_idle(true)
}
