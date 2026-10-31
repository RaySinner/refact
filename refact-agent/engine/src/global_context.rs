use std::collections::HashMap;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Mutex as StdMutex;
use std::sync::RwLock as StdRwLock;
use std::time::Duration;
use hyper::StatusCode;
use tokio::signal;
use tokio::sync::{Mutex as AMutex, RwLock as ARwLock};
use tracing::{error, info};

use crate::agents::registry::BackgroundAgentRegistry;
use crate::app_state::{
    AppActivitySink, AppBuddyEventSink, AppState, AppToolRegistry, BuddyServices, CapsState,
    ChatServices, EngineBuddyServicesHandle, EngineCapsProvider, EngineChatSessionFacade, EngineChatToolExecutor,
    EngineExecPolicyProvider, EngineFilesProvider, EngineHookRunner, EngineModeConfigProvider,
    EngineProjectTreeProvider, EnginePromptAssembler, EngineSubchatLauncher, EngineTrajectoryStore,
    IntegrationServices, ModelServices, PathServices, RuntimeServices, TokenizerState,
    ToolCatalogCache, WorkspaceServices,
};
use crate::caps::CodeAssistantCaps;
use crate::chat::ctx::{ChatCtx, ChatIntegrationScope, ChatWorkspace};
use crate::chat::EngineSessionsHandle;
use crate::chat::verifier::EngineCardVerifier;
use crate::caps::providers::get_latest_provider_mtime;
use crate::providers::load_providers_from_config;
use crate::providers::quota::ProviderQuotaCache;
use crate::completion_cache::CompletionCache;
use crate::custom_error::ScratchError;
use crate::exec::ExecRegistry;
use crate::files_in_workspace::DocumentsState;
use crate::privacy::{PrivacyObservationRuntimeState, PrivacySettings};
use crate::background_tasks::BackgroundTasksHolder;
use crate::ext::hooks_runner::HooksConfig;
use crate::scheduler::SchedulerConfig;
use crate::yaml_configs::customization_registry::RegistryCacheManager;
use crate::knowledge_index::KnowledgeIndex;
use crate::tools::ctx::{ToolChatScope, ToolCodeIntelScope, ToolCtx, ToolWorkspaceScope};
pub use refact_core::at_commands_preview_cache::AtCommandsPreviewCache;

pub use refact_runtime_api::{
    CommandLine, TerminalSecurityConfig, TerminalSecurityMode, ToolCatalogGenerations,
};
pub use refact_runtime_api::global_context::*;

/// Engine seam trait: `GlobalContext` lives in `refact-runtime-api`, but the
/// composite ctx constructors build engine handle objects, so per E0116 they
/// are provided as an engine-local extension trait instead of inherent methods.
pub trait GlobalContextCtx {
    fn chat_ctx(&self, gcx: SharedGlobalContext) -> ChatCtx;
    fn tool_ctx(&self, gcx: SharedGlobalContext) -> ToolCtx;
    fn app_state(&self, gcx: SharedGlobalContext) -> AppState;
}

impl GlobalContextCtx for GlobalContext {
    fn chat_ctx(&self, gcx: SharedGlobalContext) -> ChatCtx {
        ChatCtx {
            task_storage_runtime: self.task_storage_runtime.clone(),
            runtime: RuntimeServices {
                shutdown_flag: self.shutdown_flag.clone(),
                cmdline: Arc::new(self.cmdline.clone()),
                http_client: self.http_client.clone(),
                ask_shutdown_sender: self.ask_shutdown_sender.clone(),
                exec_registry: self.exec_registry.clone(),
            },
            paths: PathServices {
                cache_dir: self.cache_dir.clone(),
                config_dir: self.config_dir.clone(),
                app_searchable_id: self.app_searchable_id.lock().unwrap().clone(),
            },
            model: ModelServices {
                caps: self.caps_state.clone(),
                tokenizers: self.tokenizer_state.clone(),
                providers: self.providers.clone(),
                llm_stats_sender: self.llm_stats_sender.lock().unwrap().clone(),
            },
            workspace: ChatWorkspace {
                workspace_folders: self.documents_state.workspace_folders.clone(),
                vec_db: self.vec_db.clone(),
                vec_db_error: self.vec_db_error.clone(),
                knowledge_index: self.knowledge_index.clone(),
                at_commands_preview_cache: self.at_commands_preview_cache.clone(),
                privacy_settings: self.privacy_settings.read().unwrap().clone(),
                enrichment_generation: self.enrichment_generation.clone(),
                memory_document_map: self.documents_state.memory_document_map.clone(),
            },
            chat: ChatServices {
                sessions: self.chat_sessions.clone(),
                facade: Arc::new(EngineChatSessionFacade::new(gcx.clone())),
                trajectory_index_coordinator: self.trajectory_index_coordinator.clone(),
                trajectory_events_tx: self
                    .trajectory_events_tx
                    .clone()
                    .expect("trajectory event sender is not initialized"),
                workspace_changed_tx: self
                    .workspace_changed_tx
                    .clone()
                    .expect("workspace changed sender is not initialized"),
                task_events_tx: self
                    .task_events_tx
                    .clone()
                    .expect("task event sender is not initialized"),
                task_events_seq: self
                    .task_events_seq
                    .clone()
                    .expect("task event sequence is not initialized"),
                notification_events_tx: self
                    .notification_events_tx
                    .clone()
                    .expect("notification event sender is not initialized"),
            },
            buddy: BuddyServices {
                buddy: self.buddy.clone(),
                buddy_events_tx: self
                    .buddy_events_tx
                    .clone()
                    .expect("buddy event sender is not initialized"),
                user_activity: self.user_activity.clone(),
            },
            integrations: ChatIntegrationScope {
                browser_runtimes: self.browser_runtimes.clone(),
            },
            agents: self.agents.clone(),
            tool_registry: Arc::new(AppToolRegistry::new(gcx.clone())),
            activity_sink: Arc::new(AppActivitySink::new(self.user_activity.clone())),
            buddy_event_sink: Arc::new(AppBuddyEventSink::new(gcx.clone(), self.buddy.clone())),
            card_verifier: Arc::new(EngineCardVerifier),
            card_verifier_tasks: self.card_verifier_tasks.clone(),
            privacy_policy_load: self.privacy_policy_load.clone(),
            tool_catalog_generations: self.tool_catalog_generations.clone(),
            project_registry_cache: self.project_registry_cache.clone(),
            trajectory_vectorization_queue: self.trajectory_vectorization_queue.clone(),
            subchat: Arc::new(EngineSubchatLauncher::new(gcx.clone())),
            project_tree: Arc::new(EngineProjectTreeProvider::new(gcx.clone())),
            hooks: Arc::new(EngineHookRunner::new(gcx.clone())),
            mode_config: Arc::new(EngineModeConfigProvider::new(gcx.clone())),
            caps: Arc::new(EngineCapsProvider::new(gcx.clone())),
            exec_policy: Arc::new(EngineExecPolicyProvider::new(gcx.clone())),
            trajectories: Arc::new(EngineTrajectoryStore::new(gcx.clone())),
            prompts: Arc::new(EnginePromptAssembler::new(gcx.clone())),
            tool_executor: Arc::new(EngineChatToolExecutor::new(gcx)),
        }
    }

    fn tool_ctx(&self, gcx: SharedGlobalContext) -> ToolCtx {
        ToolCtx {
            runtime: RuntimeServices {
                shutdown_flag: self.shutdown_flag.clone(),
                cmdline: Arc::new(self.cmdline.clone()),
                http_client: self.http_client.clone(),
                ask_shutdown_sender: self.ask_shutdown_sender.clone(),
                exec_registry: self.exec_registry.clone(),
            },
            paths: PathServices {
                cache_dir: self.cache_dir.clone(),
                config_dir: self.config_dir.clone(),
                app_searchable_id: self.app_searchable_id.lock().unwrap().clone(),
            },
            model: ModelServices {
                caps: self.caps_state.clone(),
                tokenizers: self.tokenizer_state.clone(),
                providers: self.providers.clone(),
                llm_stats_sender: self.llm_stats_sender.lock().unwrap().clone(),
            },
            chat: ToolChatScope {
                sessions: Arc::new(EngineSessionsHandle(self.chat_sessions.clone())),
                facade: Arc::new(EngineChatSessionFacade::new(gcx.clone())),
                notification_events_tx: self
                    .notification_events_tx
                    .clone()
                    .expect("notification event sender is not initialized"),
            },
            buddy: Arc::new(EngineBuddyServicesHandle(BuddyServices {
                buddy: self.buddy.clone(),
                buddy_events_tx: self
                    .buddy_events_tx
                    .clone()
                    .expect("buddy event sender is not initialized"),
                user_activity: self.user_activity.clone(),
            })),
            integrations: IntegrationServices {
                integration_sessions: self.integration_sessions.clone(),
                browser_runtimes: self.browser_runtimes.clone(),
                ext_cache_generation: self.ext_cache_generation.clone(),
                project_registry_cache: self.project_registry_cache.clone(),
                init_shadow_repos_lock: self.init_shadow_repos_lock.clone(),
                git_operations_abort_flag: self.git_operations_abort_flag.clone(),
            },
            workspace: ToolWorkspaceScope {
                knowledge_index: self.knowledge_index.clone(),
                privacy_settings: self.privacy_settings.clone(),
                workspace_files: self.documents_state.workspace_files.clone(),
            },
            codeintel: ToolCodeIntelScope {
                codegraph: self.codegraph.clone(),
            },
            agents: self.agents.clone(),
            tool_registry: Arc::new(AppToolRegistry::new(gcx.clone())),
            privacy_policy_load: self.privacy_policy_load.clone(),
            files: Arc::new(EngineFilesProvider::new(gcx.clone())),
            at_command_executor: Arc::new(crate::at_commands::executor::EngineAtCommandExecutor),
            trajectories: Arc::new(EngineTrajectoryStore::new(gcx.clone())),
            subchat: Arc::new(EngineSubchatLauncher::new(gcx.clone())),
            mode_config: Arc::new(EngineModeConfigProvider::new(gcx.clone())),
            caps: Arc::new(EngineCapsProvider::new(gcx)),
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
                documents_state: self.documents_state.clone(),
                privacy_settings: ctx.workspace.privacy_settings.clone(),
                indexing_everywhere: self.indexing_everywhere.clone(),
                completions_cache: self.completions_cache.clone(),
                vec_db: ctx.workspace.vec_db.clone(),
                vec_db_error: ctx.workspace.vec_db_error.clone(),
                knowledge_index: ctx.workspace.knowledge_index.clone(),
                at_commands_preview_cache: ctx.workspace.at_commands_preview_cache.clone(),
            },
            chat: ctx.chat.clone(),
            buddy: ctx.buddy.clone(),
            integrations: IntegrationServices {
                integration_sessions: self.integration_sessions.clone(),
                browser_runtimes: ctx.integrations.browser_runtimes.clone(),
                ext_cache_generation: self.ext_cache_generation.clone(),
                project_registry_cache: ctx.project_registry_cache.clone(),
                init_shadow_repos_lock: self.init_shadow_repos_lock.clone(),
                git_operations_abort_flag: self.git_operations_abort_flag.clone(),
            },
            activity_sink: ctx.activity_sink.clone(),
            buddy_event_sink: ctx.buddy_event_sink.clone(),
            tool_registry: ctx.tool_registry.clone(),
            agents: ctx.agents.clone(),
            ctx,
            tool,
        }
    }
}

async fn load_engine_global_config(
    config_dir: &PathBuf,
    cmdline: &CommandLine,
) -> (SchedulerConfig, HooksConfig, TerminalSecurityConfig) {
    let path = if cmdline.privacy_yaml.is_empty() {
        config_dir.join("privacy.yaml")
    } else {
        crate::files_correction::canonical_path(cmdline.privacy_yaml.clone())
    };
    let config = match tokio::fs::read_to_string(&path).await {
        Ok(content) => {
            serde_yaml::from_str::<EngineGlobalConfig>(&content).unwrap_or_else(|error| {
                tracing::warn!(
                    "failed to parse engine global config from {}: {error}",
                    path.display()
                );
                EngineGlobalConfig::default()
            })
        }
        Err(error) => {
            tracing::warn!(
                "failed to read engine global config from {}: {error}",
                path.display()
            );
            EngineGlobalConfig::default()
        }
    };
    let scheduler = config
        .scheduler
        .with_startup_overrides(cmdline.no_scheduler);
    (scheduler, config.hooks, config.terminal_security)
}

const CAPS_RELOAD_BACKOFF: u64 = 60; // seconds
const CAPS_BACKGROUND_RELOAD: u64 = 3600; // seconds

pub async fn migrate_to_config_folder(config_dir: &PathBuf, cache_dir: &PathBuf) -> io::Result<()> {
    let mut entries = tokio::fs::read_dir(cache_dir).await?;
    while let Some(entry) = entries.next_entry().await? {
        let path = entry.path();
        let file_name = path.file_name().unwrap().to_string_lossy().into_owned();
        let file_type = entry.file_type().await?;
        let is_yaml_cfg =
            file_type.is_file() && path.extension().and_then(|e| e.to_str()) == Some("yaml");
        if is_yaml_cfg {
            let new_path = config_dir.join(&file_name);
            if new_path.exists() {
                tracing::info!(
                    "cannot migrate {:?} to {:?}: destination exists",
                    path,
                    new_path
                );
                continue;
            }
            tokio::fs::rename(&path, &new_path).await?;
            tracing::info!("migrated {:?} to {:?}", path, new_path);
        }
    }

    Ok(())
}

#[cfg(not(target_os = "windows"))]
pub fn get_app_searchable_id(workspace_folders: &[PathBuf]) -> String {
    let mac = pnet_datalink::interfaces()
        .into_iter()
        .find(|iface: &pnet_datalink::NetworkInterface| !iface.is_loopback() && iface.mac.is_some())
        .and_then(|iface| iface.mac)
        .map(|mac| mac.to_string().replace(":", ""))
        .unwrap_or_else(|| "no-mac".to_string());

    let folders = workspace_folders
        .iter()
        .map(|p| {
            p.file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string()
        })
        .collect::<Vec<_>>()
        .join(";");

    format!("{}-{}", mac, folders)
}

#[cfg(target_os = "windows")]
pub fn get_app_searchable_id(workspace_folders: &[PathBuf]) -> String {
    use winreg::enums::*;
    use winreg::RegKey;
    let machine_guid = RegKey::predef(HKEY_LOCAL_MACHINE)
        .open_subkey("SOFTWARE\\Microsoft\\Cryptography")
        .and_then(|key| key.get_value::<String, _>("MachineGuid"))
        .unwrap_or_else(|_| "no-machine-guid".to_string());
    let folders = workspace_folders
        .iter()
        .map(|p| {
            p.file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string()
        })
        .collect::<Vec<_>>()
        .join(";");
    format!("{}-{}", machine_guid, folders)
}

pub async fn try_load_caps_quickly_if_not_present(
    gcx: Arc<GlobalContext>,
    max_age_seconds: u64,
) -> Result<Arc<CodeAssistantCaps>, ScratchError> {
    let cmdline = gcx.cmdline.clone();
    let (caps_state, config_dir) = { (gcx.caps_state.clone(), gcx.config_dir.clone()) };
    let caps_reading_lock = caps_state.read().await.reading_lock.clone();

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let caps_last_attempted_ts;
    let latest_provider_mtime = get_latest_provider_mtime(&config_dir).await.unwrap_or(0);
    let (project_defaults_root, project_defaults_mtime) =
        crate::caps::project_model_defaults_state(gcx.clone()).await;
    let latest_provider_mtime = latest_provider_mtime.max(project_defaults_mtime);

    {
        // gcx is not locked, but a specialized async mutex is, up until caps are saved
        let _caps_reading_locked = caps_reading_lock.lock().await;

        let max_age = if max_age_seconds > 0 {
            max_age_seconds
        } else {
            CAPS_BACKGROUND_RELOAD
        };
        {
            let mut caps_state = caps_state.write().await;
            if caps_state.last_attempted_ts + max_age < now
                || latest_provider_mtime >= caps_state.last_attempted_ts
                || caps_state.project_root_changed(&project_defaults_root)
            {
                caps_state.caps = None;
                caps_state.last_attempted_ts = 0;
                gcx.tool_catalog_generations.advance_capabilities();
                caps_last_attempted_ts = 0;
            } else {
                if let Some(caps_arc) = caps_state.caps.clone() {
                    return Ok(caps_arc.clone());
                }
                caps_last_attempted_ts = caps_state.last_attempted_ts;
            }
        }
        if caps_last_attempted_ts + CAPS_RELOAD_BACKOFF > now {
            let caps_state = caps_state.read().await;
            return Err(ScratchError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                caps_state.last_error.clone(),
            ));
        }

        let caps_result = crate::caps::load_caps(cmdline, gcx.clone()).await;

        {
            let mut caps_state = caps_state.write().await;
            caps_state.last_attempted_ts = now;
            match caps_result {
                Ok(caps) => {
                    caps_state.caps = Some(caps.clone());
                    caps_state.loaded_project_root = Some(project_defaults_root);
                    caps_state.last_error = "".to_string();
                    gcx.tool_catalog_generations.advance_capabilities();
                    Ok(caps)
                }
                Err(e) => {
                    error!("caps fetch failed: {:?}", e);
                    caps_state.last_error = format!("caps fetch failed: {}", e);
                    return Err(ScratchError::new(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        caps_state.last_error.clone(),
                    ));
                }
            }
        }
    }
}

pub async fn look_for_piggyback_fields(
    gcx: Arc<GlobalContext>,
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
                    info!(
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

pub async fn block_until_signal(
    ask_shutdown_receiver: std::sync::mpsc::Receiver<String>,
    shutdown_flag: Arc<AtomicBool>,
) {
    let ctrl_c = async {
        signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let sigterm = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("failed to install signal handler")
            .recv()
            .await;
    };
    #[cfg(not(unix))]
    let sigterm = std::future::pending::<()>();

    #[cfg(unix)]
    let sigusr1 = async {
        signal::unix::signal(signal::unix::SignalKind::user_defined1())
            .expect("failed to install signal handler")
            .recv()
            .await;
    };
    #[cfg(not(unix))]
    let sigusr1 = std::future::pending::<()>();

    let shutdown_flag_clone = shutdown_flag.clone();
    tokio::select! {
        _ = ctrl_c => {
            info!("SIGINT signal received");
            shutdown_flag_clone.store(true, Ordering::SeqCst);
        },
        _ = sigterm => {
            info!("SIGTERM signal received");
            shutdown_flag_clone.store(true, Ordering::SeqCst);
        },
        _ = sigusr1 => {
            info!("SIGUSR1 signal received");
        },
        _ = tokio::task::spawn_blocking(move || {
            let _ = ask_shutdown_receiver.recv();
            shutdown_flag.store(true, Ordering::SeqCst);
        }) => {
            info!("graceful shutdown requested");
        }
    }
}

pub async fn create_global_context(
    cache_dir: PathBuf,
    config_dir: PathBuf,
    cmdline: CommandLine,
) -> (Arc<GlobalContext>, std::sync::mpsc::Receiver<String>) {
    let (ask_shutdown_sender, ask_shutdown_receiver) = std::sync::mpsc::channel::<String>();
    let cmdline = cmdline.with_daemon_auth_token_from_env();
    let mut http_client_builder = build_shared_http_client_builder();
    if cmdline.insecure {
        http_client_builder = http_client_builder.danger_accept_invalid_certs(true)
    }
    let http_client = http_client_builder.build().unwrap();
    let (scheduler_config, hooks_config, terminal_security_config) =
        load_engine_global_config(&config_dir, &cmdline).await;
    let trajectory_settings_path = crate::runtime_settings::settings_path(&config_dir);
    let trajectory_settings = crate::runtime_settings::load_from_path(&trajectory_settings_path)
        .await
        .unwrap_or_else(|error| {
            tracing::warn!("{error}");
            crate::runtime_settings::TrajectoryRuntimeSettings::default()
        });
    crate::runtime_settings::install_startup(trajectory_settings);

    let mut workspace_dirs: Vec<PathBuf> = vec![];
    if !cmdline.workspace_folder.is_empty() {
        let path = crate::files_correction::canonical_path(&cmdline.workspace_folder);
        workspace_dirs = vec![path];
    }
    let user_activity_project_root = workspace_dirs
        .first()
        .cloned()
        .unwrap_or_else(|| cache_dir.clone());
    let user_activity =
        crate::buddy::user_activity::UserActivityRing::load(user_activity_project_root.as_path())
            .await;
    let agents_root = user_activity_project_root
        .join(".refact")
        .join("background_agents");
    let agents = BackgroundAgentRegistry::new(agents_root)
        .await
        .unwrap_or_else(|error| panic!("failed to initialize background agent registry: {error}"));
    let cx = GlobalContext {
        task_storage_runtime: Arc::new(crate::tasks::storage::EngineTaskStorageRuntime),
        shutdown_flag: Arc::new(AtomicBool::new(false)),
        lsp_tcp_client_count: Arc::new(AtomicUsize::new(0)),
        exec_registry: Arc::new(ExecRegistry::new()),
        card_verifier_tasks: Arc::new(AMutex::new(tokio::task::JoinSet::new())),
        trajectory_vectorization_queue: Arc::new(AMutex::new(Default::default())),
        cmdline: cmdline.clone(),
        http_client: http_client.clone(),
        cache_dir,
        config_dir: config_dir.clone(),
        caps_state: Arc::new(ARwLock::new(CapsState {
            caps: None,
            reading_lock: Arc::new(AMutex::<bool>::new(false)),
            last_error: String::new(),
            last_attempted_ts: 0,
            models_dev_startup_refresh_attempted: false,
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
        documents_state: DocumentsState::new(workspace_dirs.clone()).await,
        at_commands_preview_cache: Arc::new(AMutex::new(AtCommandsPreviewCache::new())),
        privacy_settings: Arc::new(StdRwLock::new(Arc::new(PrivacySettings::default()))),
        privacy_policy_load: Arc::new(StdRwLock::new(refact_privacy::PolicyLoad::default())),
        file_index: Arc::new(crate::file_index::FileIndex::default()),
        privacy_match_counts: Arc::new(StdRwLock::new(Default::default())),
        privacy_observation_runtime: Arc::new(StdRwLock::new(
            PrivacyObservationRuntimeState::default(),
        )),
        indexing_everywhere: Arc::new(StdRwLock::new(Arc::new(
            crate::files_blocklist::IndexingEverywhere::default(),
        ))),
        integration_sessions: Arc::new(AMutex::new(HashMap::new())),
        browser_runtimes: Arc::new(AMutex::new(HashMap::new())),
        init_shadow_repos_background_task_holder: BackgroundTasksHolder::new(vec![]),
        init_shadow_repos_lock: Arc::new(AMutex::new(false)),
        git_operations_abort_flag: Arc::new(AtomicBool::new(false)),
        app_searchable_id: Arc::new(StdMutex::new(get_app_searchable_id(&workspace_dirs))),
        trajectory_events_tx: Some(tokio::sync::broadcast::channel(1024).0),
        workspace_changed_tx: Some(tokio::sync::broadcast::channel(16).0),
        task_events_tx: Some(tokio::sync::broadcast::channel(1024).0),
        task_events_seq: Some(Arc::new(std::sync::atomic::AtomicU64::new(0))),
        notification_events_tx: Some(tokio::sync::broadcast::channel(256).0),
        chat_sessions: crate::chat::create_sessions_map(),
        trajectory_index_coordinator: Arc::new(
            crate::chat::trajectory_index::TrajectoryIndexCoordinator::new(),
        ),
        performance_telemetry: crate::chat::perf_diagnostics::process_telemetry(),
        project_registry_cache: Arc::new(StdRwLock::new(RegistryCacheManager::new())),
        providers: Arc::new(ARwLock::new(
            load_providers_from_config(&config_dir, &http_client)
                .await
                .unwrap_or_default(),
        )),
        quota_cache: Arc::new(AMutex::new(ProviderQuotaCache::default())),
        knowledge_index: Arc::new(AMutex::new(KnowledgeIndex::empty())),
        enrichment_cache: Arc::new(crate::knowledge::enrichment::EnrichmentCache::default()),
        enrichment_generation: Arc::new(AtomicU64::new(0)),
        llm_stats_sender: Arc::new(StdMutex::new(None)),
        ext_cache_generation: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        tool_catalog_generations: Arc::new(ToolCatalogGenerations::default()),
        tool_catalog_cache: Arc::new(ToolCatalogCache::default()),
        buddy: Arc::new(AMutex::new(None)),
        buddy_events_tx: Some(tokio::sync::broadcast::channel(256).0),
        user_activity: Arc::new(AMutex::new(user_activity)),
        agents,
        scheduler_config,
        hooks_config,
        terminal_security_config,
        workspace_leases: Arc::new(StdMutex::new(Default::default())),
    };
    let gcx = Arc::new(cx);
    crate::files_in_workspace::watcher_init(gcx.clone()).await;
    let app_state = crate::app_state::app_state_from_gcx(gcx.clone()).await;
    crate::chat::start_session_cleanup_task(app_state.chat_ctx());
    crate::chat::start_trajectory_watcher(gcx.clone());
    (gcx, ask_shutdown_receiver)
}

#[cfg(any(test, feature = "bench"))]
pub mod tests {
    use super::*;
    #[cfg(test)]
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    pub async fn make_test_gcx() -> SharedGlobalContext {
        refact_test_support::make_test_gcx(Arc::new(crate::tasks::storage::EngineTaskStorageRuntime))
            .await
    }

    pub async fn make_test_gcx_with_dirs(
        cache_dir: PathBuf,
        config_dir: PathBuf,
    ) -> SharedGlobalContext {
        refact_test_support::make_test_gcx_with_dirs(
            cache_dir,
            config_dir,
            Arc::new(crate::tasks::storage::EngineTaskStorageRuntime),
        )
        .await
    }
    use refact_test_support::register_test_temp_dir;

    #[test]
    fn test_engine_global_config_parses_trusted_projects() {
        let yaml = "scheduler:\n  enabled: true\nhooks:\n  trusted_projects: [/home/me/repo]\nterminal_security:\n  mode: sandbox_preferred\n  env_passthrough: [HTTP_PROXY, CARGO_*]\n";
        let cfg: EngineGlobalConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(
            cfg.hooks.trusted_projects,
            vec!["/home/me/repo".to_string()]
        );
        assert!(cfg.scheduler.enabled);
        assert_eq!(
            cfg.terminal_security.env_passthrough,
            vec!["HTTP_PROXY".to_string(), "CARGO_*".to_string()]
        );
        assert_eq!(
            cfg.terminal_security.mode,
            TerminalSecurityMode::SandboxPreferred
        );
    }

    #[test]
    fn terminal_security_defaults_to_approval_only() {
        let cfg: EngineGlobalConfig = serde_yaml::from_str("{}").unwrap();

        assert_eq!(
            cfg.terminal_security.mode,
            TerminalSecurityMode::ApprovalOnly
        );
    }

    #[test]
    fn test_engine_global_config_hooks_default_empty() {
        let cfg: EngineGlobalConfig =
            serde_yaml::from_str("scheduler:\n  enabled: true\n").unwrap();
        assert!(cfg.hooks.trusted_projects.is_empty());
    }

    #[tokio::test]
    async fn shared_http_client_rejects_redirect_without_contacting_target() {
        let target_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let target_addr = target_listener.local_addr().unwrap();
        let source_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let source_addr = source_listener.local_addr().unwrap();
        let source = tokio::spawn(async move {
            let (mut socket, _) = source_listener.accept().await.unwrap();
            let mut request = [0u8; 1024];
            let _ = socket.read(&mut request).await.unwrap();
            socket
                .write_all(
                    format!(
                        "HTTP/1.1 307 Temporary Redirect\r\nLocation: http://{target_addr}/collect\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
        });

        let client = build_shared_http_client_builder().build().unwrap();
        let model = refact_core::llm_types::BaseModelRecord {
            id: "redirecting-provider".to_string(),
            name: "test-model".to_string(),
            endpoint: format!("http://{source_addr}/provider"),
            api_key: "provider-secret".to_string(),
            ..Default::default()
        };
        let error = refact_llm::forward_to_openai_style_endpoint(
            &model,
            "private prompt",
            &client,
            &refact_core::chat_types::SamplingParameters::default(),
        )
        .await
        .unwrap_err();
        assert!(error.contains("status=307"));
        source.await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(100), target_listener.accept())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn app_state_from_test_gcx_clones() {
        let gcx = make_test_gcx().await;
        let app_state = crate::app_state::app_state_from_gcx(gcx.clone()).await;
        let second_app_state = crate::app_state::app_state_from_gcx(gcx.clone()).await;
        let cloned = app_state.clone();

        assert_eq!(cloned.paths.app_searchable_id, "test");
        assert!(Arc::ptr_eq(
            &app_state.runtime.shutdown_flag,
            &cloned.runtime.shutdown_flag
        ));
        assert!(Arc::ptr_eq(
            &gcx.exec_registry,
            &app_state.runtime.exec_registry
        ));
        assert!(Arc::ptr_eq(
            &app_state.runtime.exec_registry,
            &second_app_state.runtime.exec_registry
        ));
        assert!(Arc::ptr_eq(
            &app_state.model.caps,
            &second_app_state.model.caps
        ));
        assert!(Arc::ptr_eq(
            &app_state.model.tokenizers,
            &second_app_state.model.tokenizers
        ));
        assert!(Arc::ptr_eq(&app_state.agents, &second_app_state.agents));
    }

    #[tokio::test]
    async fn make_test_gcx_initializes_exec_registry() {
        let gcx = make_test_gcx().await;
        let snapshot = gcx
            .exec_registry
            .register(
                crate::exec::ExecProcessMeta::new(
                    crate::exec::ExecMode::Foreground,
                    "true".to_string(),
                ),
                1024,
            )
            .await;

        assert_eq!(gcx.exec_registry.list(Default::default()).await.len(), 1);
        assert_eq!(
            gcx.exec_registry.get(&snapshot.meta.process_id).await,
            Some(snapshot)
        );
    }

    #[tokio::test]
    async fn test_gcx_temp_directories_are_registered_for_exit_cleanup() {
        let gcx = make_test_gcx().await;
        let cache_dir = gcx.cache_dir.clone();
        let config_dir = gcx.config_dir.clone();
        assert!(cache_dir.exists());
        assert!(config_dir.exists());
        drop(gcx);
        assert!(cache_dir.exists());
        assert!(config_dir.exists());
        let registered = refact_test_support::registered_test_temp_dirs();
        assert!(registered.contains(&cache_dir));
        assert!(registered.contains(&config_dir));
    }

    #[test]
    fn test_temp_dir_registration_rejects_unsafe_paths() {
        let unprefixed = std::env::temp_dir().join(format!("other-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&unprefixed).unwrap();
        assert!(!register_test_temp_dir(&unprefixed));
        std::fs::remove_dir_all(&unprefixed).unwrap();

        let outside = std::env::current_dir()
            .unwrap()
            .join(format!("refact-outside-{}", uuid::Uuid::new_v4()));
        if !outside.starts_with(std::env::temp_dir()) {
            std::fs::create_dir_all(&outside).unwrap();
            assert!(!register_test_temp_dir(&outside));
            std::fs::remove_dir_all(outside).unwrap();
        }
    }

    pub async fn test_chat_ctx() -> crate::chat::ctx::ChatCtx {
        let gcx = make_test_gcx().await;
        gcx.chat_ctx(gcx.clone())
    }

    pub async fn test_tool_ctx() -> crate::tools::ctx::ToolCtx {
        let gcx = make_test_gcx().await;
        gcx.tool_ctx(gcx.clone())
    }

    #[tokio::test]
    async fn test_chat_ctx_clones_and_shares_services() {
        let ctx = test_chat_ctx().await;
        let clone = ctx.clone();

        assert!(Arc::ptr_eq(
            &clone.runtime.shutdown_flag,
            &ctx.runtime.shutdown_flag
        ));
        assert!(Arc::ptr_eq(
            &clone.workspace.workspace_folders,
            &ctx.workspace.workspace_folders
        ));
        assert!(Arc::ptr_eq(&clone.agents, &ctx.agents));
    }
}
