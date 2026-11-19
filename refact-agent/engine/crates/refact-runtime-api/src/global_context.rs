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
    pub files_runtime: Arc<dyn crate::files_in_workspace::FilesRuntime>,
    pub scheduler_runtime: Arc<dyn crate::scheduler::SchedulerRuntime>,
    pub memory_runtime: Arc<dyn crate::memories::MemoryRuntime>,
    pub task_storage_runtime: Arc<dyn crate::task_storage::TaskStorageRuntime>,
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
    /// Installed by the engine right after construction; lets this crate build
    /// `ChatCtx`/`ToolCtx`/`AppState` without depending on engine handle types.
    pub context_factory: Arc<crate::context_factory::ContextFactorySlot>,
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

/// Composite-context builders. They live here so every crate (including `refact-tools`)
/// can reach them; the bodies are delegated to the engine-installed factory.
pub trait GlobalContextCtx {
    fn chat_ctx(&self, gcx: SharedGlobalContext) -> crate::chat_ctx::ChatCtx;
    fn tool_ctx(&self, gcx: SharedGlobalContext) -> crate::tool_ctx::ToolCtx;
    fn app_state(&self, gcx: SharedGlobalContext) -> crate::app_state::AppState;
}

impl GlobalContextCtx for GlobalContext {
    fn chat_ctx(&self, gcx: SharedGlobalContext) -> crate::chat_ctx::ChatCtx {
        self.context_factory.get().chat_ctx(gcx)
    }

    fn tool_ctx(&self, gcx: SharedGlobalContext) -> crate::tool_ctx::ToolCtx {
        self.context_factory.get().tool_ctx(gcx)
    }

    fn app_state(&self, gcx: SharedGlobalContext) -> crate::app_state::AppState {
        self.context_factory.get().app_state(gcx)
    }
}

pub async fn app_state_from_gcx(gcx: SharedGlobalContext) -> crate::app_state::AppState {
    use GlobalContextCtx as _;
    gcx.app_state(gcx.clone())
}

pub async fn look_for_piggyback_fields(
    gcx: SharedGlobalContext,
    anything_from_server: &serde_json::Value,
) {
    let caps_state = gcx.caps_state.clone();
    let mut caps_state = caps_state.write().await;
    if let Some(dict) = anything_from_server.as_object() {
        let new_caps_version = dict
            .get("caps_version")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        if new_caps_version > 0 {
            if let Some(caps) = caps_state.caps.clone() {
                if caps.caps_version < new_caps_version {
                    tracing::info!(
                        "detected biggyback caps version {} is newer than the current version {}",
                        new_caps_version, caps.caps_version
                    );
                    caps_state.caps = None;
                    caps_state.last_attempted_ts = 0;
                    gcx.tool_catalog_generations.advance_capabilities();
                }
            }
        }
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

#[cfg(test)]
pub(crate) mod tests {
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize};
    use std::sync::{Arc, Mutex as StdMutex, RwLock as StdRwLock};

    use refact_agents_core::registry::BackgroundAgentRegistry;
    use refact_buddy_core::user_activity::UserActivityRing;
    use refact_caps_core::caps_state::CapsState;
    use refact_chat_history::trajectory_index::TrajectoryIndexCoordinator;
    use refact_codegraph::QueuedPath;
    use refact_core::at_commands_preview_cache::AtCommandsPreviewCache;
    use refact_core::background_tasks::BackgroundTasksHolder;
    use refact_core::knowledge_index::KnowledgeIndex;
    use refact_core::privacy_types::PrivacySettings;
    use refact_core::scheduler_config::SchedulerConfig;
    use refact_core::tokenizer_state::TokenizerState;
    use refact_exec::ExecRegistry;
    use refact_ext::hooks::HooksConfig;
    use refact_files::documents_state::DocumentsState;
    use refact_files::file_index::FileIndex;
    use refact_files::indexing_settings::IndexingEverywhere;
    use refact_knowledge_graph::enrichment_cache::EnrichmentCache;
    use refact_knowledge_graph::kg_structs::KnowledgeFrontmatter;
    use refact_knowledge_graph::memo::MemoRecord;
    use refact_knowledge_graph::memory_types::{EnrichmentParams, MemoryWriteOutcome};
    use refact_perf::perf_diagnostics;
    use refact_privacy::runtime_state::PrivacyObservationRuntimeState;
    use refact_privacy::PolicyLoad;
    use refact_providers::quota::ProviderQuotaCache;
    use refact_providers::ProviderRegistry;
    use refact_scratchpads::completion_cache::CompletionCache;
    use refact_yaml_configs::registry_cache::RegistryCacheManager;
    use tokio::sync::{Mutex as AMutex, RwLock as ARwLock};

    use crate::files_in_workspace::{FileReadContext, FilesRuntime};
    use crate::memories::MemoryRuntime;
    use crate::scheduler_runtime::EngineSchedulerRuntime;
    use crate::services::{PathServices, WorkspaceServices};
    use crate::task_storage_runtime::EngineTaskStorageRuntime;
    use crate::{
        AtCommandsContext, ChatCtx, CommandLine, TerminalSecurityConfig, ToolCatalogCache,
        ToolCatalogGenerations,
    };

    use super::{build_shared_http_client_builder, GlobalContext, SharedGlobalContext};

    struct UnusedRuntime;

    #[async_trait::async_trait]
    impl FilesRuntime for UnusedRuntime {
        async fn remove_memory_document_for_path(
            &self,
            _: Arc<GlobalContext>,
            _: &PathBuf,
        ) -> bool {
            unreachable!()
        }
        async fn remove_memory_documents_under_path(
            &self,
            _: Arc<GlobalContext>,
            _: &PathBuf,
        ) -> usize {
            unreachable!()
        }
        async fn get_file_text_from_memory_or_disk(
            &self,
            _: Arc<GlobalContext>,
            _: &PathBuf,
        ) -> Result<String, String> {
            unreachable!()
        }
        async fn get_file_text_from_memory_or_disk_for_model_context(
            &self,
            _: Arc<GlobalContext>,
            _: &PathBuf,
        ) -> Result<String, String> {
            unreachable!()
        }
        async fn prepare_file_read_context(&self, _: Arc<GlobalContext>) -> FileReadContext {
            unreachable!()
        }
        async fn get_file_text_from_memory_or_disk_for_model_context_with_context(
            &self,
            _: Arc<GlobalContext>,
            _: &PathBuf,
            _: &FileReadContext,
            _: Option<usize>,
        ) -> Result<String, String> {
            unreachable!()
        }
        async fn check_file_privacy_for_send(
            &self,
            _: Arc<GlobalContext>,
            _: &PathBuf,
        ) -> Result<(), String> {
            unreachable!()
        }
        async fn check_file_privacy_for_model_context(
            &self,
            _: Arc<GlobalContext>,
            _: &PathBuf,
        ) -> Result<(), String> {
            unreachable!()
        }
        async fn filter_privacy_allowed_files(
            &self,
            _: Arc<GlobalContext>,
            _: Vec<PathBuf>,
        ) -> Vec<PathBuf> {
            unreachable!()
        }
        async fn filter_privacy_allowed_files_for_model_context(
            &self,
            _: Arc<GlobalContext>,
            _: Vec<PathBuf>,
        ) -> Vec<PathBuf> {
            unreachable!()
        }
        async fn enqueue_some_docs_with_read_paths(
            &self,
            _: Arc<GlobalContext>,
            _: &[QueuedPath],
            _: bool,
        ) {
            unreachable!()
        }
        async fn enqueue_all_files_from_workspace_folders(
            &self,
            _: Arc<GlobalContext>,
            _: bool,
            _: bool,
        ) -> i32 {
            unreachable!()
        }
        async fn paths_from_anywhere(&self, _: Arc<GlobalContext>) -> Vec<PathBuf> {
            unreachable!()
        }
        async fn correct_to_nearest_filename(
            &self,
            _: Arc<GlobalContext>,
            _: &String,
            _: bool,
            _: usize,
        ) -> Vec<String> {
            unreachable!()
        }
        async fn correct_to_nearest_dir_path(
            &self,
            _: Arc<GlobalContext>,
            _: &String,
            _: bool,
            _: usize,
        ) -> Vec<String> {
            unreachable!()
        }
        async fn get_project_dirs(&self, _: Arc<GlobalContext>) -> Vec<PathBuf> {
            unreachable!()
        }
        async fn get_unscoped_project_dirs(&self, _: Arc<GlobalContext>) -> Vec<PathBuf> {
            unreachable!()
        }
        async fn get_active_project_path(&self, _: Arc<GlobalContext>) -> Option<PathBuf> {
            None
        }
        async fn get_active_project_path_for_services(
            &self,
            _: &PathServices,
            _: &WorkspaceServices,
        ) -> Option<PathBuf> {
            None
        }
        async fn shortify_paths(&self, _: Arc<GlobalContext>, _: &Vec<String>) -> Vec<String> {
            unreachable!()
        }
        async fn check_if_its_inside_a_workspace_or_config(
            &self,
            _: Arc<GlobalContext>,
            _: &Path,
        ) -> Result<(), String> {
            unreachable!()
        }
        fn check_file_privacy_with_context(
            &self,
            _: &FileReadContext,
            _: &Path,
        ) -> Result<(), String> {
            unreachable!()
        }
        fn check_file_privacy_for_read_preparation_with_context(
            &self,
            _: &FileReadContext,
            _: &Path,
        ) -> Result<(), String> {
            unreachable!()
        }
        async fn on_workspaces_init(&self, _: Arc<GlobalContext>) -> i32 {
            unreachable!()
        }
        async fn on_did_change(&self, _: Arc<GlobalContext>, _: &PathBuf, _: &String) {
            unreachable!()
        }
        async fn on_explicit_branch_change(&self, _: Arc<GlobalContext>, _: &PathBuf) {
            unreachable!()
        }
    }

    #[async_trait::async_trait]
    impl MemoryRuntime for UnusedRuntime {
        async fn memories_search(
            &self,
            _: Arc<GlobalContext>,
            _: &str,
            _: usize,
            _: usize,
            _: Option<&str>,
        ) -> Result<Vec<MemoRecord>, String> {
            unreachable!()
        }
        async fn memories_add_enriched(
            &self,
            _: Arc<AMutex<AtCommandsContext>>,
            _: &str,
            _: EnrichmentParams,
        ) -> Result<PathBuf, String> {
            unreachable!()
        }
        async fn touch_memory_observation_for_ctx(
            &self,
            _: &ChatCtx,
            _: &Path,
        ) -> Result<(), String> {
            unreachable!()
        }
        async fn memories_upsert_by_signal_key_for_ctx(
            &self,
            _: &ChatCtx,
            _: &str,
            _: &KnowledgeFrontmatter,
            _: &str,
        ) -> Result<MemoryWriteOutcome, String> {
            unreachable!()
        }
        async fn memories_add_dedup_for_ctx(
            &self,
            _: &ChatCtx,
            _: &KnowledgeFrontmatter,
            _: &str,
        ) -> Result<MemoryWriteOutcome, String> {
            unreachable!()
        }
        async fn rewrite_memory_document_for_ctx(
            &self,
            _: &ChatCtx,
            _: &Path,
            _: &KnowledgeFrontmatter,
            _: &str,
        ) -> Result<(), String> {
            unreachable!()
        }
        async fn enrichment_current_root_id(
            &self,
            _: Arc<GlobalContext>,
            _: Option<&str>,
        ) -> Option<String> {
            None
        }
        async fn memories_search_for_enrichment(
            &self,
            _: Arc<GlobalContext>,
            _: &str,
            _: usize,
            _: usize,
            _: Option<&str>,
            _: Option<&str>,
        ) -> Result<Vec<MemoRecord>, String> {
            unreachable!()
        }
    }

    /// Builds a `GlobalContext` with stub runtimes for in-crate tests.
    /// The two temp dirs are created under the system temp directory and are
    /// not registered for cleanup at exit, so they stay behind after the run.
    pub(crate) async fn make_test_gcx() -> SharedGlobalContext {
        let cache_dir = std::env::temp_dir().join(format!("refact-test-{}", uuid::Uuid::new_v4()));
        let config_dir = std::env::temp_dir().join(format!("refact-cfg-{}", uuid::Uuid::new_v4()));
        let _ = std::fs::create_dir_all(&cache_dir);
        let _ = std::fs::create_dir_all(&config_dir);

        let (ask_shutdown_sender, _) = std::sync::mpsc::channel::<String>();
        let user_activity = UserActivityRing::load(&cache_dir).await;
        let agents =
            BackgroundAgentRegistry::new(cache_dir.join(".refact").join("background_agents"))
                .await
                .unwrap_or_else(|error| {
                    panic!("failed to initialize test background agent registry: {error}")
                });

        let cmdline = CommandLine {
            ping_message: "pong".to_string(),
            logs_stderr: true,
            logs_to_file: String::new(),
            insecure: true,
            http_port: 0,
            http_host: None,
            lsp_port: 0,
            lsp_stdin_stdout: 0,
            verbose: false,
            ast: false,
            ast_max_files: 0,
            ast_permanent: String::new(),
            wait_ast: false,
            vecdb: false,
            vecdb_max_files: 0,
            vecdb_force_path: String::new(),
            wait_vecdb: false,
            files_jsonl_path: String::new(),
            workspace_folder: String::new(),
            allow_shared_workspace: true,
            only_create_yaml_configs: false,
            print_customization: false,
            experimental: false,
            inside_container: true,
            integrations_yaml: String::new(),
            variables_yaml: String::new(),
            secrets_yaml: String::new(),
            indexing_yaml: String::new(),
            privacy_yaml: String::new(),
            no_scheduler: false,
            daemon_endpoint: String::new(),
            daemon_auth_token: None,
            watch_parent_stdin: false,
            no_mdns: false,
            project_id: String::new(),
        };

        let http_client = build_shared_http_client_builder()
            .danger_accept_invalid_certs(true)
            .build()
            .unwrap();

        let cx = GlobalContext {
            files_runtime: Arc::new(UnusedRuntime),
            scheduler_runtime: Arc::new(EngineSchedulerRuntime),
            memory_runtime: Arc::new(UnusedRuntime),
            task_storage_runtime: Arc::new(EngineTaskStorageRuntime),
            shutdown_flag: Arc::new(AtomicBool::new(false)),
            lsp_tcp_client_count: Arc::new(AtomicUsize::new(0)),
            exec_registry: Arc::new(ExecRegistry::new()),
            card_verifier_tasks: Arc::new(AMutex::new(tokio::task::JoinSet::new())),
            trajectory_vectorization_queue: Arc::new(AMutex::new(Default::default())),
            cmdline,
            http_client,
            context_factory: Arc::new(crate::ContextFactorySlot::new()),
            cache_dir,
            config_dir,
            caps_state: Arc::new(ARwLock::new(CapsState {
                caps: None,
                reading_lock: Arc::new(AMutex::<bool>::new(false)),
                last_error: String::new(),
                last_attempted_ts: 0,
                models_dev_startup_refresh_attempted: true,
                loaded_project_root: None,
            })),
            tokenizer_state: Arc::new(StdRwLock::new(TokenizerState {
                map: HashMap::new(),
                download_lock: Arc::new(AMutex::<bool>::new(false)),
            })),
            completions_cache: Arc::new(StdRwLock::new(CompletionCache::new())),
            vec_db: Arc::new(AMutex::new(None)),
            vec_db_error: Arc::new(StdMutex::new(String::new())),
            codegraph: Arc::new(AMutex::new(None)),
            codegraph_error: Arc::new(StdMutex::new(String::new())),
            ask_shutdown_sender: Arc::new(StdMutex::new(ask_shutdown_sender)),
            documents_state: DocumentsState::new(vec![]).await,
            at_commands_preview_cache: Arc::new(AMutex::new(AtCommandsPreviewCache::new())),
            privacy_settings: Arc::new(StdRwLock::new(Arc::new(PrivacySettings::default()))),
            privacy_policy_load: Arc::new(StdRwLock::new(PolicyLoad::default())),
            file_index: Arc::new(FileIndex::default()),
            privacy_match_counts: Arc::new(StdRwLock::new(Default::default())),
            privacy_observation_runtime: Arc::new(StdRwLock::new(
                PrivacyObservationRuntimeState::default(),
            )),
            indexing_everywhere: Arc::new(StdRwLock::new(Arc::new(IndexingEverywhere::default()))),
            integration_sessions: Arc::new(AMutex::new(HashMap::new())),
            browser_runtimes: Arc::new(AMutex::new(HashMap::new())),
            init_shadow_repos_background_task_holder: BackgroundTasksHolder::new(vec![]),
            init_shadow_repos_lock: Arc::new(AMutex::new(false)),
            git_operations_abort_flag: Arc::new(AtomicBool::new(false)),
            app_searchable_id: Arc::new(StdMutex::new("test".to_string())),
            trajectory_events_tx: Some(tokio::sync::broadcast::channel(1024).0),
            workspace_changed_tx: Some(tokio::sync::broadcast::channel(16).0),
            task_events_tx: Some(tokio::sync::broadcast::channel(1024).0),
            task_events_seq: Some(Arc::new(AtomicU64::new(0))),
            notification_events_tx: Some(tokio::sync::broadcast::channel(256).0),
            chat_sessions: Arc::new(ARwLock::new(HashMap::new())),
            trajectory_index_coordinator: Arc::new(
                TrajectoryIndexCoordinator::new_with_local_listing_counters(),
            ),
            performance_telemetry: perf_diagnostics::process_telemetry(),
            project_registry_cache: Arc::new(StdRwLock::new(RegistryCacheManager::new())),
            providers: Arc::new(ARwLock::new(ProviderRegistry::default())),
            quota_cache: Arc::new(AMutex::new(ProviderQuotaCache::default())),
            knowledge_index: Arc::new(AMutex::new(KnowledgeIndex::empty())),
            enrichment_cache: Arc::new(EnrichmentCache::default()),
            enrichment_generation: Arc::new(AtomicU64::new(0)),
            llm_stats_sender: Arc::new(StdMutex::new(None)),
            ext_cache_generation: Arc::new(AtomicU64::new(0)),
            tool_catalog_generations: Arc::new(ToolCatalogGenerations::default()),
            tool_catalog_cache: Arc::new(ToolCatalogCache::default()),
            buddy: Arc::new(AMutex::new(None)),
            buddy_events_tx: Some(tokio::sync::broadcast::channel(256).0),
            user_activity: Arc::new(AMutex::new(user_activity)),
            agents,
            scheduler_config: SchedulerConfig::default(),
            hooks_config: HooksConfig::default(),
            terminal_security_config: TerminalSecurityConfig::default(),
            workspace_leases: Arc::new(StdMutex::new(Default::default())),
        };
        Arc::new(cx)
    }
}
