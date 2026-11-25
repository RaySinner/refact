use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use refact_buddy_core::snapshot::BuddySnapshot;
use refact_buddy_core::types::{BuddyRuntimeEvent, BuddySuggestion};
use refact_buddy_core::user_action::UserAction;
use refact_chat_api::ChatMessage;
use refact_runtime_api::{
    ActivitySink, BuddyEventSink, BuddyServicesHandle, SubchatLauncherHandle,
    ToolConfirmationCheck, ToolExecutionResult, ToolPolicyInfo, ToolCatalogSnapshot, ToolRegistry,
    ToolRegistryIndex, TrajectoryStoreHandle, TurnToolPool, WorkflowFuture,
};

pub const TOOL_CATALOG_SNAPSHOTS_ENV: &str = refact_runtime_api::TOOL_CATALOG_SNAPSHOTS_ENV;
use tokio::sync::{Mutex as AMutex, Notify};

pub use refact_runtime_api::{app_state_from_gcx, CapsLoader, ContextFactory, GlobalContextCtx};
use crate::buddy::actor::BuddyService;
use crate::buddy::events::BuddyEvent;
use crate::buddy::user_activity::UserActivityRing;
use crate::chat::ctx::{
    CapsProvider, ChatCtx, ChatIntegrationScope, ChatToolExecutor, ChatWorkspace,
    ExecPolicyProvider, HookRunner, ModeConfigProvider, ProjectTreeProvider, PromptAssembler,
    SubchatLauncher, TrajectoryStore,
};
use crate::chat::EngineSessionsHandle;
use crate::chat::verifier::EngineCardVerifier;
use crate::tools::ctx::{ToolChatScope, ToolCodeIntelScope, ToolCtx, ToolWorkspaceScope};
use crate::chat::perf_diagnostics::{self, PerfComponent, PerfOutcome};
use crate::chat::types::{ChatSession, EnqueueCommandOutcome, ThreadParams, TrajectoryCommitIntent};
use crate::chat::trajectories::{self, TrajectoryEvent};
use crate::chat::{self, process_command_queue};
use crate::files_blocklist::IndexingEverywhere;
pub use crate::global_context::{AtCommandsPreviewCache, CommandLine, ToolCatalogGenerations};
use crate::global_context::SharedGlobalContext;
use crate::global_context::TerminalSecurityConfig;
use crate::call_validation::SubchatParameters;
use crate::subchat::{ExplicitSubchatSpec, SubchatConfig, SubchatResult, TraceParent, WrapUpConfig};
use crate::http::routers::v1::sidebar::NotificationEvent;
use crate::tasks::events::TaskEventEnvelope;
use crate::tools::ctx::FilesProvider;
pub use refact_caps_core::caps_state::CapsState;
pub use refact_core::tokenizer_state::TokenizerState;
use refact_runtime_api::{
    ChatSessionFacade, ChatSessionSnapshot, ChatSessionUpdate, CreateSessionRequest,
    NotificationEventSender, TaskEventEnvelopeSender, TrajectoryEventSender,
};

pub use refact_runtime_api::{IntegrationServices, ModelServices, PathServices, RuntimeServices};

pub use refact_runtime_api::services::{BuddyServices, ChatServices, WorkspaceServices};

pub use refact_runtime_api::buddy_runtime::buddy_services_from_handle;

mod facade;
mod generation;
mod handoff;
mod launchers;
mod prompt_and_tools;
mod providers;
mod queue;
mod sinks;
mod tool_pool;
mod tool_registry;
mod tool_registry_trait;
mod trajectory_store;

#[allow(unused_imports)]
pub use self::facade::*;
#[allow(unused_imports)]
pub use self::generation::*;
#[allow(unused_imports)]
pub use self::handoff::*;
#[allow(unused_imports)]
pub use self::launchers::*;
#[allow(unused_imports)]
pub use self::prompt_and_tools::*;
#[allow(unused_imports)]
pub use self::providers::*;
#[allow(unused_imports)]
pub use self::queue::*;
#[allow(unused_imports)]
pub use self::sinks::*;
#[allow(unused_imports)]
pub use self::tool_pool::*;
#[allow(unused_imports)]
pub use self::tool_registry::*;
#[allow(unused_imports)]
pub use self::tool_registry_trait::*;
#[allow(unused_imports)]
pub use self::trajectory_store::*;

pub use refact_runtime_api::AppState;

/// Engine implementation of the `ContextFactory` seam: it is the only place that knows how
/// to build the composite contexts from concrete engine handles.
pub struct EngineContextFactory;

impl ContextFactory for EngineContextFactory {
    fn chat_ctx(&self, gcx: SharedGlobalContext) -> ChatCtx {
        ChatCtx {
            knowledge_builder: Arc::new(crate::knowledge_index::EngineKnowledgeBuilder),
            memory_runtime: gcx.memory_runtime.clone(),
            memory_lifecycle: Arc::new(
                crate::buddy::memory_lifecycle::EngineMemoryLifecycleRuntime,
            ),
            goal_verifier: Arc::new(crate::chat::goal_verifier::EngineGoalVerifierRunner),
            task_storage_runtime: gcx.task_storage_runtime.clone(),
            agent_host: Arc::new(crate::chat::task_agent_monitor::EngineChatAgentHost::new(
                gcx.clone(),
            )),
            runtime: RuntimeServices {
                shutdown_flag: gcx.shutdown_flag.clone(),
                cmdline: Arc::new(gcx.cmdline.clone()),
                http_client: gcx.http_client.clone(),
                ask_shutdown_sender: gcx.ask_shutdown_sender.clone(),
                exec_registry: gcx.exec_registry.clone(),
            },
            paths: PathServices {
                cache_dir: gcx.cache_dir.clone(),
                config_dir: gcx.config_dir.clone(),
                app_searchable_id: gcx.app_searchable_id.lock().unwrap().clone(),
            },
            model: ModelServices {
                caps: gcx.caps_state.clone(),
                tokenizers: gcx.tokenizer_state.clone(),
                providers: gcx.providers.clone(),
                llm_stats_sender: gcx.llm_stats_sender.lock().unwrap().clone(),
            },
            workspace: ChatWorkspace {
                workspace_folders: gcx.documents_state.workspace_folders.clone(),
                vec_db: gcx.vec_db.clone(),
                vec_db_error: gcx.vec_db_error.clone(),
                knowledge_index: gcx.knowledge_index.clone(),
                at_commands_preview_cache: gcx.at_commands_preview_cache.clone(),
                privacy_settings: gcx.privacy_settings.read().unwrap().clone(),
                enrichment_generation: gcx.enrichment_generation.clone(),
                memory_document_map: gcx.documents_state.memory_document_map.clone(),
            },
            chat: ChatServices {
                sessions: gcx.chat_sessions.clone(),
                facade: Arc::new(EngineChatSessionFacade::new(gcx.clone())),
                trajectory_index_coordinator: gcx.trajectory_index_coordinator.clone(),
                trajectory_events_tx: gcx
                    .trajectory_events_tx
                    .clone()
                    .expect("trajectory event sender is not initialized"),
                workspace_changed_tx: gcx
                    .workspace_changed_tx
                    .clone()
                    .expect("workspace changed sender is not initialized"),
                task_events_tx: gcx
                    .task_events_tx
                    .clone()
                    .expect("task event sender is not initialized"),
                task_events_seq: gcx
                    .task_events_seq
                    .clone()
                    .expect("task event sequence is not initialized"),
                notification_events_tx: gcx
                    .notification_events_tx
                    .clone()
                    .expect("notification event sender is not initialized"),
            },
            buddy: BuddyServices {
                buddy: gcx.buddy.clone(),
                buddy_events_tx: gcx
                    .buddy_events_tx
                    .clone()
                    .expect("buddy event sender is not initialized"),
                user_activity: gcx.user_activity.clone(),
                supervision: refact_runtime_api::buddy_supervision::BuddySupervisionCollector::new(
                    gcx.documents_state
                        .workspace_folders
                        .lock()
                        .unwrap()
                        .first()
                        .cloned()
                        .unwrap_or_else(|| gcx.cache_dir.clone()),
                ),
            },
            integrations: ChatIntegrationScope {
                browser_runtimes: gcx.browser_runtimes.clone(),
            },
            agents: gcx.agents.clone(),
            tool_registry: Arc::new(AppToolRegistry::new(gcx.clone())),
            activity_sink: Arc::new(AppActivitySink::new(gcx.user_activity.clone())),
            buddy_event_sink: Arc::new(AppBuddyEventSink::new(gcx.clone(), gcx.buddy.clone())),
            card_verifier: Arc::new(EngineCardVerifier),
            card_verifier_tasks: gcx.card_verifier_tasks.clone(),
            privacy_policy_load: gcx.privacy_policy_load.clone(),
            tool_catalog_generations: gcx.tool_catalog_generations.clone(),
            project_registry_cache: gcx.project_registry_cache.clone(),
            trajectory_vectorization_queue: gcx.trajectory_vectorization_queue.clone(),
            subchat: Arc::new(EngineSubchatLauncher::new(gcx.clone())),
            spawn: Arc::new(EngineBackgroundAgentSpawner),
            project_tree: Arc::new(EngineProjectTreeProvider::new(gcx.clone())),
            hooks: Arc::new(EngineHookRunner::new(gcx.clone())),
            mode_config: Arc::new(EngineModeConfigProvider::new(gcx.clone())),
            caps: Arc::new(EngineCapsProvider::new(gcx.clone())),
            exec_policy: Arc::new(EngineExecPolicyProvider::new(gcx.clone())),
            trajectories: Arc::new(EngineTrajectoryStore::new(gcx.clone())),
            prompts: Arc::new(EnginePromptAssembler::new(gcx.clone())),
            tool_executor: Arc::new(EngineChatToolExecutor::new(gcx.clone())),
            generation: Arc::new(EngineGenerationStarter),
            queue_processor: Arc::new(EngineCommandQueueStarter),
            pending_handoff: Arc::new(EnginePendingHandoffFinalizer::new(gcx.clone())),
        }
    }

    fn at_commands(&self) -> HashMap<String, Arc<dyn refact_runtime_api::AtCommand + Send>> {
        crate::at_commands::at_commands::engine_at_commands_dict()
    }

    fn tool_ctx(&self, gcx: SharedGlobalContext) -> ToolCtx {
        ToolCtx {
            runtime: RuntimeServices {
                shutdown_flag: gcx.shutdown_flag.clone(),
                cmdline: Arc::new(gcx.cmdline.clone()),
                http_client: gcx.http_client.clone(),
                ask_shutdown_sender: gcx.ask_shutdown_sender.clone(),
                exec_registry: gcx.exec_registry.clone(),
            },
            paths: PathServices {
                cache_dir: gcx.cache_dir.clone(),
                config_dir: gcx.config_dir.clone(),
                app_searchable_id: gcx.app_searchable_id.lock().unwrap().clone(),
            },
            model: ModelServices {
                caps: gcx.caps_state.clone(),
                tokenizers: gcx.tokenizer_state.clone(),
                providers: gcx.providers.clone(),
                llm_stats_sender: gcx.llm_stats_sender.lock().unwrap().clone(),
            },
            chat: ToolChatScope {
                sessions: Arc::new(EngineSessionsHandle(gcx.chat_sessions.clone())),
                facade: Arc::new(EngineChatSessionFacade::new(gcx.clone())),
                notification_events_tx: gcx
                    .notification_events_tx
                    .clone()
                    .expect("notification event sender is not initialized"),
            },
            buddy: Arc::new(EngineBuddyServicesHandle(BuddyServices {
                buddy: gcx.buddy.clone(),
                buddy_events_tx: gcx
                    .buddy_events_tx
                    .clone()
                    .expect("buddy event sender is not initialized"),
                user_activity: gcx.user_activity.clone(),
                supervision: refact_runtime_api::buddy_supervision::BuddySupervisionCollector::new(
                    gcx.documents_state
                        .workspace_folders
                        .lock()
                        .unwrap()
                        .first()
                        .cloned()
                        .unwrap_or_else(|| gcx.cache_dir.clone()),
                ),
            })),
            integrations: IntegrationServices {
                integration_sessions: gcx.integration_sessions.clone(),
                browser_runtimes: gcx.browser_runtimes.clone(),
                ext_cache_generation: gcx.ext_cache_generation.clone(),
                project_registry_cache: gcx.project_registry_cache.clone(),
                init_shadow_repos_lock: gcx.init_shadow_repos_lock.clone(),
                git_operations_abort_flag: gcx.git_operations_abort_flag.clone(),
            },
            workspace: ToolWorkspaceScope {
                knowledge_index: gcx.knowledge_index.clone(),
                privacy_settings: gcx.privacy_settings.clone(),
                workspace_files: gcx.documents_state.workspace_files.clone(),
            },
            codeintel: ToolCodeIntelScope {
                codegraph: gcx.codegraph.clone(),
            },
            agents: gcx.agents.clone(),
            tool_registry: Arc::new(AppToolRegistry::new(gcx.clone())),
            privacy_policy_load: gcx.privacy_policy_load.clone(),
            files: Arc::new(EngineFilesProvider::new(gcx.clone())),
            at_command_executor: Arc::new(crate::at_commands::executor::EngineAtCommandExecutor),
            trajectories: Arc::new(EngineTrajectoryStore::new(gcx.clone())),
            subchat: Arc::new(EngineSubchatLauncher::new(gcx.clone())),
            mode_config: Arc::new(EngineModeConfigProvider::new(gcx.clone())),
            caps: Arc::new(EngineCapsProvider::new(gcx.clone())),
        }
    }

    fn app_state(&self, gcx: SharedGlobalContext) -> AppState {
        let ctx = self.chat_ctx(gcx.clone());
        let tool = self.tool_ctx(gcx.clone());
        AppState {
            gcx: gcx.clone(),
            runtime: ctx.runtime.clone(),
            paths: ctx.paths.clone(),
            model: ctx.model.clone(),
            workspace: WorkspaceServices {
                files_runtime: gcx.files_runtime.clone(),
                documents_state: gcx.documents_state.clone(),
                privacy_settings: ctx.workspace.privacy_settings.clone(),
                indexing_everywhere: gcx.indexing_everywhere.clone(),
                completions_cache: gcx.completions_cache.clone(),
                vec_db: ctx.workspace.vec_db.clone(),
                vec_db_error: ctx.workspace.vec_db_error.clone(),
                knowledge_index: ctx.workspace.knowledge_index.clone(),
                at_commands_preview_cache: ctx.workspace.at_commands_preview_cache.clone(),
            },
            chat: ctx.chat.clone(),
            buddy: ctx.buddy.clone(),
            integrations: IntegrationServices {
                integration_sessions: gcx.integration_sessions.clone(),
                browser_runtimes: ctx.integrations.browser_runtimes.clone(),
                ext_cache_generation: gcx.ext_cache_generation.clone(),
                project_registry_cache: ctx.project_registry_cache.clone(),
                init_shadow_repos_lock: gcx.init_shadow_repos_lock.clone(),
                git_operations_abort_flag: gcx.git_operations_abort_flag.clone(),
            },
            activity_sink: ctx.activity_sink.clone(),
            buddy_event_sink: ctx.buddy_event_sink.clone(),
            tool_registry: ctx.tool_registry.clone(),
            agents: ctx.agents.clone(),
            ctx,
            tool,
        }
    }

    fn caps_loader(&self) -> Arc<dyn CapsLoader> {
        Arc::new(EngineCapsLoader)
    }

    fn integration_provider(
        &self,
    ) -> Arc<dyn refact_runtime_api::integrations_runtime::IntegrationProvider> {
        Arc::new(refact_runtime_api::integrations_runtime::BuiltinIntegrationProvider)
    }
}

/// Engine side of the `CapsLoader` seam: the provider/models.dev machinery behind
/// `load_caps` and the project-defaults mtime probe stay here.
struct EngineCapsLoader;

#[async_trait]
impl CapsLoader for EngineCapsLoader {
    async fn load_caps(
        &self,
        cmdline: CommandLine,
        gcx: SharedGlobalContext,
    ) -> Result<Arc<crate::caps::CodeAssistantCaps>, String> {
        crate::providers::oauth_refresh::refresh_expiring_claude_code_tokens(&gcx).await;
        crate::caps::load_caps(cmdline, gcx).await
    }

    async fn project_model_defaults_state(
        &self,
        gcx: SharedGlobalContext,
    ) -> (Option<PathBuf>, u64) {
        crate::caps::project_model_defaults_state(gcx).await
    }
}

pub struct EngineExecPolicyProvider {
    gcx: SharedGlobalContext,
}

impl EngineExecPolicyProvider {
    pub fn new(gcx: SharedGlobalContext) -> Self {
        Self { gcx }
    }
}

impl ExecPolicyProvider for EngineExecPolicyProvider {
    fn terminal_security_config(&self) -> TerminalSecurityConfig {
        self.gcx.terminal_security_config.clone()
    }
}

pub struct EngineFilesProvider {
    gcx: SharedGlobalContext,
}

impl EngineFilesProvider {
    pub fn new(gcx: SharedGlobalContext) -> Self {
        Self { gcx }
    }
}

#[async_trait]
impl FilesProvider for EngineFilesProvider {
    async fn get_file_text_from_memory_or_disk(&self, path: &PathBuf) -> Result<String, String> {
        crate::files_in_workspace::get_file_text_from_memory_or_disk(self.gcx.clone(), path).await
    }

    async fn get_file_text_from_memory_or_disk_for_model_context(
        &self,
        path: &PathBuf,
    ) -> Result<String, String> {
        crate::files_in_workspace::get_file_text_from_memory_or_disk_for_model_context(
            self.gcx.clone(),
            path,
        )
        .await
    }

    async fn fast_enqueue_for_edit(&self, docs: &[PathBuf]) -> Result<(), String> {
        crate::tools::file_edit::auxiliary::fast_enqueue_for_edit(self.gcx.clone(), docs).await
    }

    async fn remove_memory_document_for_path(&self, path: &PathBuf) -> bool {
        crate::files_in_workspace::remove_memory_document_for_path(self.gcx.clone(), path).await
    }

    async fn file_repair_candidates(&self, value: &str, top_n: usize, fuzzy: bool) -> Vec<String> {
        let tool = self.gcx.tool_ctx(self.gcx.clone());
        crate::at_commands::at_file::file_repair_candidates(&tool, &value.to_string(), top_n, fuzzy)
            .await
    }

    async fn project_dirs(&self) -> Vec<PathBuf> {
        crate::files_correction::get_unscoped_project_dirs(self.gcx.clone()).await
    }

    async fn paths_from_anywhere(&self) -> Vec<PathBuf> {
        crate::files_correction::paths_from_anywhere(self.gcx.clone()).await
    }

    async fn check_file_privacy_for_send(&self, path: &PathBuf) -> Result<(), String> {
        crate::files_in_workspace::check_file_privacy_for_send(self.gcx.clone(), path).await
    }

    async fn check_file_privacy_for_model_context(&self, path: &PathBuf) -> Result<(), String> {
        crate::files_in_workspace::check_file_privacy_for_model_context(self.gcx.clone(), path)
            .await
    }

    async fn filter_privacy_allowed_files(&self, files: Vec<PathBuf>) -> Vec<PathBuf> {
        crate::files_in_workspace::filter_privacy_allowed_files(self.gcx.clone(), files).await
    }

    async fn filter_privacy_allowed_files_for_model_context(
        &self,
        files: Vec<PathBuf>,
    ) -> Vec<PathBuf> {
        crate::files_in_workspace::filter_privacy_allowed_files_for_model_context(
            self.gcx.clone(),
            files,
        )
        .await
    }

    async fn indexing_everywhere(&self) -> Arc<IndexingEverywhere> {
        crate::files_blocklist::reload_indexing_everywhere_if_needed(self.gcx.clone()).await
    }

    async fn correct_to_nearest_dir_path(
        &self,
        correction_candidate: &str,
        fuzzy: bool,
        top_n: usize,
    ) -> Vec<String> {
        crate::files_correction::correct_to_nearest_dir_path(
            self.gcx.clone(),
            &correction_candidate.to_string(),
            fuzzy,
            top_n,
        )
        .await
    }

    async fn correct_to_nearest_filename(
        &self,
        correction_candidate: &str,
        fuzzy: bool,
        top_n: usize,
    ) -> Vec<String> {
        crate::files_correction::correct_to_nearest_filename(
            self.gcx.clone(),
            &correction_candidate.to_string(),
            fuzzy,
            top_n,
        )
        .await
    }

    async fn shortify_paths(&self, paths: &[String]) -> Vec<String> {
        crate::files_correction::shortify_paths(self.gcx.clone(), &paths.to_vec()).await
    }

    async fn return_one_candidate_or_a_good_error(
        &self,
        file_path: &str,
        candidates: &[String],
        project_paths: &[PathBuf],
        dirs: bool,
    ) -> Result<String, String> {
        let tool = self.gcx.tool_ctx(self.gcx.clone());
        crate::at_commands::at_file::return_one_candidate_or_a_good_error(
            &tool,
            &file_path.to_string(),
            &candidates.to_vec(),
            &project_paths.to_vec(),
            dirs,
        )
        .await
    }
}

pub use refact_runtime_api::{ToolCatalogCache, ToolCatalogKey, TOOL_CATALOG_SNAPSHOT_CACHE_LIMIT};

pub(crate) use refact_runtime_api::tool_catalog_snapshot_rollout_enabled;
#[cfg(test)]
pub(crate) use refact_runtime_api::tool_catalog_snapshot_rollout_enabled_for;

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::tools::tools_description::{Tool, ToolDesc, ToolSource, ToolSourceType};
    use serial_test::serial;

    struct ToolCatalogSnapshotsEnvGuard(Option<std::ffi::OsString>);

    impl ToolCatalogSnapshotsEnvGuard {
        fn enable() -> Self {
            let previous = std::env::var_os(TOOL_CATALOG_SNAPSHOTS_ENV);
            std::env::set_var(TOOL_CATALOG_SNAPSHOTS_ENV, "1");
            Self(previous)
        }
    }

    impl Drop for ToolCatalogSnapshotsEnvGuard {
        fn drop(&mut self) {
            if let Some(previous) = self.0.take() {
                std::env::set_var(TOOL_CATALOG_SNAPSHOTS_ENV, previous);
            } else {
                std::env::remove_var(TOOL_CATALOG_SNAPSHOTS_ENV);
            }
        }
    }

    struct FixtureTool {
        _build_number: usize,
        name: String,
    }

    #[async_trait]
    impl Tool for FixtureTool {
        async fn tool_execute(
            &mut self,
            _ccx: Arc<AMutex<crate::at_commands::at_commands::AtCommandsContext>>,
            _tool_call_id: &String,
            _args: &HashMap<String, serde_json::Value>,
        ) -> Result<(bool, Vec<crate::call_validation::ContextEnum>), String> {
            Ok((false, Vec::new()))
        }

        fn tool_description(&self) -> ToolDesc {
            ToolDesc {
                name: self.name.clone(),
                experimental: false,
                allow_parallel: true,
                description: format!("fixture {}", self._build_number),
                input_schema: serde_json::json!({"type":"object"}),
                output_schema: None,
                annotations: None,
                display_name: self.name.clone(),
                source: ToolSource {
                    source_type: ToolSourceType::Builtin,
                    config_path: String::new(),
                },
            }
        }
    }

    fn fixture_registry(gcx: SharedGlobalContext, builds: Arc<AtomicUsize>) -> AppToolRegistry {
        fixture_registry_with_names(gcx, builds, vec!["fixture".to_string()])
    }

    fn fixture_registry_with_names(
        gcx: SharedGlobalContext,
        builds: Arc<AtomicUsize>,
        names: Vec<String>,
    ) -> AppToolRegistry {
        AppToolRegistry::with_fixture_tool_factory(
            gcx,
            Arc::new(move || {
                let build_number = builds.fetch_add(1, Ordering::SeqCst);
                names
                    .iter()
                    .map(|name| {
                        Box::new(FixtureTool {
                            _build_number: build_number,
                            name: name.clone(),
                        })
                            as Box<dyn crate::tools::tools_description::Tool + Send>
                    })
                    .collect()
            }),
        )
    }

    #[serial(runtime_settings)]
    #[tokio::test]
    async fn tool_catalog_snapshot_single_flights_and_reuses_descriptors() {
        let _env = ToolCatalogSnapshotsEnvGuard::enable();
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let builds = Arc::new(AtomicUsize::new(0));
        let registry = Arc::new(fixture_registry(gcx.clone(), builds.clone()));
        let mut tasks = Vec::new();
        for _ in 0..16 {
            let registry = registry.clone();
            tasks.push(tokio::spawn(async move {
                registry
                    .snapshot_for_mode_for_test("agent", Some("provider/model"))
                    .await
            }));
        }
        let snapshots = futures::future::join_all(tasks)
            .await
            .into_iter()
            .map(Result::unwrap)
            .collect::<Vec<_>>();

        assert_eq!(builds.load(Ordering::SeqCst), 1);
        assert!(snapshots
            .iter()
            .all(|snapshot| Arc::ptr_eq(snapshot, &snapshots[0])));
        assert_eq!(gcx.tool_catalog_cache.snapshot_count().await, 1);
    }

    #[serial(runtime_settings)]
    #[tokio::test]
    async fn tool_catalog_snapshot_warm_acquisition_stays_below_two_milliseconds() {
        let _env = ToolCatalogSnapshotsEnvGuard::enable();
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let builds = Arc::new(AtomicUsize::new(0));
        let registry = fixture_registry(gcx, builds.clone());
        registry
            .snapshot_for_mode_for_test("agent", Some("provider/model"))
            .await;
        let mut samples = Vec::new();
        for _ in 0..16 {
            let started = std::time::Instant::now();
            registry
                .snapshot_for_mode_for_test("agent", Some("provider/model"))
                .await;
            samples.push(started.elapsed());
        }
        samples.sort_unstable();

        assert_eq!(builds.load(Ordering::SeqCst), 1);
        assert!(samples[15] < std::time::Duration::from_millis(2));
    }

    #[serial(runtime_settings)]
    #[tokio::test]
    async fn tool_catalog_generations_create_next_turn_snapshot_without_mutating_old_one() {
        let _env = ToolCatalogSnapshotsEnvGuard::enable();
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let builds = Arc::new(AtomicUsize::new(0));
        let registry = fixture_registry(gcx.clone(), builds.clone());
        let first = registry
            .snapshot_for_mode_for_test("agent", Some("provider/model"))
            .await;
        gcx.tool_catalog_generations.advance_privacy();
        let second = registry
            .snapshot_for_mode_for_test("agent", Some("provider/model"))
            .await;
        gcx.ext_cache_generation.fetch_add(1, Ordering::SeqCst);
        let third = registry
            .snapshot_for_mode_for_test("agent", Some("provider/model"))
            .await;

        assert_eq!(builds.load(Ordering::SeqCst), 3);
        assert!(!Arc::ptr_eq(&first, &second));
        assert!(!Arc::ptr_eq(&second, &third));
        assert_eq!(first.index.tools[0].name, "fixture");
        assert_eq!(second.index.tools[0].name, "fixture");
        assert_eq!(third.index.tools[0].name, "fixture");
    }

    #[serial(runtime_settings)]
    #[tokio::test]
    async fn tool_catalog_snapshot_cache_is_bounded_across_generations() {
        let _env = ToolCatalogSnapshotsEnvGuard::enable();
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let builds = Arc::new(AtomicUsize::new(0));
        let registry = fixture_registry(gcx.clone(), builds);

        for _ in 0..=TOOL_CATALOG_SNAPSHOT_CACHE_LIMIT {
            registry
                .snapshot_for_mode_for_test("agent", Some("provider/model"))
                .await;
            gcx.tool_catalog_generations.advance_integrations();
        }

        assert_eq!(
            gcx.tool_catalog_cache.snapshot_count().await,
            TOOL_CATALOG_SNAPSHOT_CACHE_LIMIT
        );
        assert_eq!(gcx.tool_catalog_cache.build_lock_count().await, 0);
    }

    #[serial(runtime_settings)]
    #[tokio::test]
    async fn tool_catalog_key_isolates_mode_and_model_and_keeps_mutable_instances_fresh() {
        let _env = ToolCatalogSnapshotsEnvGuard::enable();
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let builds = Arc::new(AtomicUsize::new(0));
        let registry = fixture_registry(gcx.clone(), builds.clone());
        let first = registry
            .snapshot_for_mode_for_test("agent", Some("provider/model-a"))
            .await;
        let second = registry
            .snapshot_for_mode_for_test("task_agent", Some("provider/model-a"))
            .await;
        let third = registry
            .snapshot_for_mode_for_test("agent", Some("provider/model-b"))
            .await;
        let mutable_a = registry
            .tools_for_mode(gcx.clone(), "agent", Some("provider/model-a"))
            .await;
        let mutable_b = registry
            .tools_for_mode(gcx.clone(), "agent", Some("provider/model-a"))
            .await;

        assert!(!Arc::ptr_eq(&first, &second));
        assert!(!Arc::ptr_eq(&first, &third));
        assert_eq!(builds.load(Ordering::SeqCst), 5);
        assert_eq!(gcx.tool_catalog_cache.snapshot_count().await, 3);
        assert_ne!(
            mutable_a[0].tool_description().description,
            mutable_b[0].tool_description().description
        );
    }

    #[serial(runtime_settings)]
    #[tokio::test]
    async fn tool_catalog_key_isolates_execution_scopes() {
        let _env = ToolCatalogSnapshotsEnvGuard::enable();
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let builds = Arc::new(AtomicUsize::new(0));
        let registry = fixture_registry(gcx, builds.clone());
        let first = registry
            .snapshot_for_mode_with_scope_for_test(
                "agent",
                Some("provider/model"),
                Some("/workspace/one".to_string()),
            )
            .await;
        let second = registry
            .snapshot_for_mode_with_scope_for_test(
                "agent",
                Some("provider/model"),
                Some("/workspace/two".to_string()),
            )
            .await;

        assert_eq!(builds.load(Ordering::SeqCst), 2);
        assert!(!Arc::ptr_eq(&first, &second));
    }

    #[serial(runtime_settings)]
    #[tokio::test]
    async fn tool_task_spawn_agent_room_read_only_catalog_filters_write_channels() {
        let _env = ToolCatalogSnapshotsEnvGuard::enable();
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let builds = Arc::new(AtomicUsize::new(0));
        let registry = fixture_registry_with_names(
            gcx,
            builds,
            vec![
                "cat".to_string(),
                "apply_patch".to_string(),
                "shell".to_string(),
                "search_pattern".to_string(),
                "process_start".to_string(),
            ],
        );

        let advisor = registry
            .snapshot_for_mode_with_scope_for_test(
                "task_agent",
                Some("provider/model"),
                Some("room_advisor:/workspace/writer".to_string()),
            )
            .await;
        let writer = registry
            .snapshot_for_mode_with_scope_for_test(
                "task_agent",
                Some("provider/model"),
                Some("/workspace/writer".to_string()),
            )
            .await;

        let advisor_names = advisor
            .index
            .tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(advisor_names, vec!["cat", "search_pattern"]);
        assert!(writer
            .index
            .tools
            .iter()
            .any(|tool| tool.name == "apply_patch"));
        assert!(writer.index.tools.iter().any(|tool| tool.name == "shell"));
        assert!(writer
            .index
            .tools
            .iter()
            .any(|tool| tool.name == "process_start"));
    }

    #[serial(runtime_settings)]
    #[tokio::test]
    async fn turn_tool_pool_unique_batch_uses_one_mutable_vector() {
        let _env = ToolCatalogSnapshotsEnvGuard::enable();
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let builds = Arc::new(AtomicUsize::new(0));
        let names = (0..200).map(|index| format!("tool-{index}")).collect();
        let registry = fixture_registry_with_names(gcx.clone(), builds.clone(), names);
        let catalog = registry
            .snapshot_for_mode_for_test("agent", Some("provider/model"))
            .await;
        let pool = registry
            .build_turn_tool_pool(gcx, "agent", Some("provider/model"), &catalog)
            .await;
        let builds_after_pool = builds.load(Ordering::SeqCst);
        let slots = catalog
            .index
            .tools
            .iter()
            .cloned()
            .map(|tool| (tool, 1))
            .collect::<Vec<_>>();

        registry
            .prepare_turn_tool_pool(&pool, &catalog, "agent", Some("provider/model"), &slots)
            .await
            .unwrap();

        assert_eq!(pool.initial_vector_builds(), 1);
        assert_eq!(pool.fallback_vector_builds(), 0);
        assert_eq!(builds.load(Ordering::SeqCst), builds_after_pool);
    }

    #[serial(runtime_settings)]
    #[tokio::test]
    async fn turn_tool_pool_reuses_one_instance_for_sequential_calls() {
        let _env = ToolCatalogSnapshotsEnvGuard::enable();
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let builds = Arc::new(AtomicUsize::new(0));
        let registry = fixture_registry(gcx.clone(), builds.clone());
        let catalog = registry
            .snapshot_for_mode_for_test("agent", Some("provider/model"))
            .await;
        let pool = registry
            .build_turn_tool_pool(gcx, "agent", Some("provider/model"), &catalog)
            .await;
        let builds_after_pool = builds.load(Ordering::SeqCst);

        for _ in 0..50 {
            let tool = registry
                .take_turn_tool(
                    &pool,
                    registry.gcx.clone(),
                    "agent",
                    Some("provider/model"),
                    &catalog,
                    "fixture",
                )
                .await
                .unwrap()
                .unwrap();
            AppToolRegistry::app_turn_tool_pool(&pool)
                .unwrap()
                .return_tool(tool)
                .await;
        }

        assert_eq!(pool.initial_vector_builds(), 1);
        assert_eq!(pool.fallback_vector_builds(), 0);
        assert_eq!(builds.load(Ordering::SeqCst), builds_after_pool);
    }

    #[serial(runtime_settings)]
    #[tokio::test]
    async fn turn_tool_pool_returns_the_confirmation_instance_to_execution() {
        let _env = ToolCatalogSnapshotsEnvGuard::enable();
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let builds = Arc::new(AtomicUsize::new(0));
        let registry = fixture_registry(gcx.clone(), builds);
        let catalog = registry
            .snapshot_for_mode_for_test("agent", Some("provider/model"))
            .await;
        let pool = registry
            .build_turn_tool_pool(gcx, "agent", Some("provider/model"), &catalog)
            .await;

        let confirmation_tool = registry
            .take_turn_tool(
                &pool,
                registry.gcx.clone(),
                "agent",
                Some("provider/model"),
                &catalog,
                "fixture",
            )
            .await
            .unwrap()
            .unwrap();
        let confirmation_instance = confirmation_tool.tool_description().description;
        AppToolRegistry::app_turn_tool_pool(&pool)
            .unwrap()
            .return_tool(confirmation_tool)
            .await;
        let execution_tool = registry
            .take_turn_tool(
                &pool,
                registry.gcx.clone(),
                "agent",
                Some("provider/model"),
                &catalog,
                "fixture",
            )
            .await
            .unwrap()
            .unwrap();

        assert_eq!(
            confirmation_instance,
            execution_tool.tool_description().description
        );
    }

    #[serial(runtime_settings)]
    #[tokio::test]
    async fn turn_tool_pool_same_name_parallelism_is_bounded_by_multiplicity() {
        let _env = ToolCatalogSnapshotsEnvGuard::enable();
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let builds = Arc::new(AtomicUsize::new(0));
        let registry = fixture_registry(gcx.clone(), builds);
        let catalog = registry
            .snapshot_for_mode_for_test("agent", Some("provider/model"))
            .await;
        let pool = registry
            .build_turn_tool_pool(gcx, "agent", Some("provider/model"), &catalog)
            .await;
        let parallelism = 8;

        registry
            .prepare_turn_tool_pool(
                &pool,
                &catalog,
                "agent",
                Some("provider/model"),
                &[(catalog.index.tools[0].clone(), parallelism)],
            )
            .await
            .unwrap();

        assert_eq!(pool.initial_vector_builds(), 1);
        assert_eq!(pool.fallback_vector_builds(), parallelism as u64 - 1);
        assert_eq!(
            pool.initial_vector_builds() + pool.fallback_vector_builds(),
            parallelism as u64
        );
    }

    #[serial(runtime_settings)]
    #[tokio::test]
    async fn parallel_slot_preparation_uses_maximum_deficit_not_sum_of_deficits() {
        let _env = ToolCatalogSnapshotsEnvGuard::enable();
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let builds = Arc::new(AtomicUsize::new(0));
        let names = vec!["first".to_string(), "second".to_string()];
        let registry = fixture_registry_with_names(gcx.clone(), builds.clone(), names);
        let catalog = registry
            .snapshot_for_mode_for_test("agent", Some("provider/model"))
            .await;
        let pool = registry
            .build_turn_tool_pool(gcx, "agent", Some("provider/model"), &catalog)
            .await;
        let builds_before_slots = builds.load(Ordering::SeqCst);

        registry
            .prepare_turn_tool_pool(
                &pool,
                &catalog,
                "agent",
                Some("provider/model"),
                &[
                    (catalog.index.tools[0].clone(), 4),
                    (catalog.index.tools[1].clone(), 4),
                ],
            )
            .await
            .unwrap();

        assert_eq!(pool.fallback_vector_builds(), 3);
        assert_eq!(builds.load(Ordering::SeqCst), builds_before_slots + 3);
    }

    #[serial(runtime_settings)]
    #[tokio::test]
    async fn concurrent_different_tool_misses_share_one_full_vector_build() {
        let _env = ToolCatalogSnapshotsEnvGuard::enable();
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let builds = Arc::new(AtomicUsize::new(0));
        let names = (0..8).map(|index| format!("tool-{index}")).collect();
        let registry = fixture_registry_with_names(gcx.clone(), builds.clone(), names);
        let catalog = registry
            .snapshot_for_mode_for_test("agent", Some("provider/model"))
            .await;
        let pool = registry
            .build_turn_tool_pool(gcx, "agent", Some("provider/model"), &catalog)
            .await;
        let pool_impl = AppToolRegistry::app_turn_tool_pool(&pool).unwrap();
        let mut leased = Vec::new();
        for desc in &catalog.index.tools {
            leased.push(pool_impl.take(desc).await.unwrap());
        }
        let builds_before_misses = builds.load(Ordering::SeqCst);

        let calls = catalog.index.tools.iter().map(|desc| {
            registry.take_turn_tool(
                &pool,
                registry.gcx.clone(),
                "agent",
                Some("provider/model"),
                &catalog,
                &desc.name,
            )
        });
        let replacements = futures::future::join_all(calls)
            .await
            .into_iter()
            .map(|result| result.unwrap().unwrap())
            .collect::<Vec<_>>();

        assert_eq!(replacements.len(), catalog.index.tools.len());
        assert_eq!(builds.load(Ordering::SeqCst), builds_before_misses + 1);
        assert_eq!(pool.fallback_vector_builds(), 1);
        assert_eq!(leased.len(), replacements.len());
    }

    #[serial(runtime_settings)]
    #[tokio::test]
    async fn concurrent_same_tool_calls_receive_distinct_mutable_instances() {
        let _env = ToolCatalogSnapshotsEnvGuard::enable();
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let builds = Arc::new(AtomicUsize::new(0));
        let registry = fixture_registry(gcx.clone(), builds);
        let catalog = registry
            .snapshot_for_mode_for_test("agent", Some("provider/model"))
            .await;
        let pool = registry
            .build_turn_tool_pool(gcx, "agent", Some("provider/model"), &catalog)
            .await;
        let first = registry
            .take_turn_tool(
                &pool,
                registry.gcx.clone(),
                "agent",
                Some("provider/model"),
                &catalog,
                "fixture",
            )
            .await
            .unwrap()
            .unwrap();
        let (second, third) = tokio::join!(
            registry.take_turn_tool(
                &pool,
                registry.gcx.clone(),
                "agent",
                Some("provider/model"),
                &catalog,
                "fixture",
            ),
            registry.take_turn_tool(
                &pool,
                registry.gcx.clone(),
                "agent",
                Some("provider/model"),
                &catalog,
                "fixture",
            )
        );
        let second = second.unwrap().unwrap();
        let third = third.unwrap().unwrap();

        assert_ne!(
            second.tool_description().description,
            third.tool_description().description
        );
        assert_ne!(
            first.tool_description().description,
            second.tool_description().description
        );
    }

    #[serial(runtime_settings)]
    #[tokio::test]
    async fn turn_tool_pools_are_isolated_between_turns() {
        let _env = ToolCatalogSnapshotsEnvGuard::enable();
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let builds = Arc::new(AtomicUsize::new(0));
        let registry = fixture_registry(gcx.clone(), builds.clone());
        let catalog = registry
            .snapshot_for_mode_for_test("agent", Some("provider/model"))
            .await;
        let first = registry
            .build_turn_tool_pool(gcx.clone(), "agent", Some("provider/model"), &catalog)
            .await;
        let second = registry
            .build_turn_tool_pool(gcx, "agent", Some("provider/model"), &catalog)
            .await;

        assert_eq!(first.initial_vector_builds(), 1);
        assert_eq!(second.initial_vector_builds(), 1);
        assert_eq!(builds.load(Ordering::SeqCst), 3);
    }

    #[serial(runtime_settings)]
    #[test]
    fn tool_catalog_snapshot_rollout_switch_defaults_on_and_keeps_the_cold_fallback_available() {
        assert!(
            crate::runtime_settings::TrajectoryRuntimeSettings::default()
                .tool_catalog_snapshots_enabled
        );
        for disabled in [Some("0"), Some("false"), Some("no"), Some("off")] {
            assert!(!AppToolRegistry::snapshot_cache_enabled_for(disabled));
        }
        for enabled in [Some("1"), Some("true"), Some("yes")] {
            assert!(AppToolRegistry::snapshot_cache_enabled_for(enabled));
        }
    }

    #[test]
    fn tool_catalog_generations_are_independent_for_every_invalidation_source() {
        let generations = crate::global_context::ToolCatalogGenerations::default();

        generations.advance_customization();
        generations.advance_integrations();
        generations.advance_mcp();
        generations.advance_privacy();
        generations.advance_capabilities();

        assert_eq!(generations.customization.load(Ordering::Acquire), 1);
        assert_eq!(generations.integrations.load(Ordering::Acquire), 1);
        assert_eq!(generations.mcp.load(Ordering::Acquire), 1);
        assert_eq!(generations.privacy.load(Ordering::Acquire), 1);
        assert_eq!(generations.capabilities.load(Ordering::Acquire), 1);
    }
}
