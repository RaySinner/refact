use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize};
use std::sync::Mutex as StdMutex;
use std::sync::RwLock as StdRwLock;
use std::time::Duration;
use tokio::sync::{Mutex as AMutex, RwLock as ARwLock};

use crate::agents::registry::BackgroundAgentRegistry;
use crate::app_state::{CapsState, TokenizerState, ToolCatalogCache};
use crate::providers::load_providers_from_config;
use crate::providers::quota::ProviderQuotaCache;
use crate::completion_cache::CompletionCache;
use crate::exec::ExecRegistry;
use crate::files_in_workspace::DocumentsState;
use crate::privacy::{PrivacyObservationRuntimeState, PrivacySettings};
use crate::background_tasks::BackgroundTasksHolder;
use crate::yaml_configs::customization_registry::RegistryCacheManager;
use crate::knowledge_index::KnowledgeIndex;
pub use refact_core::at_commands_preview_cache::AtCommandsPreviewCache;

pub use refact_runtime_api::{
    CommandLine, TerminalSecurityConfig, TerminalSecurityMode, ToolCatalogGenerations,
};
pub use refact_runtime_api::caps_loader::try_load_caps_quickly_if_not_present;
pub use refact_runtime_api::global_context::*;
pub use refact_runtime_api::context_factory::{ContextFactory, ContextFactorySlot};
pub use refact_runtime_api::{app_state_from_gcx, GlobalContextCtx};
pub use refact_runtime_api::engine_bootstrap::{
    block_until_signal, get_app_searchable_id, migrate_to_config_folder,
};
use refact_runtime_api::engine_bootstrap::load_engine_global_config;

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
        files_runtime: Arc::new(crate::files_in_workspace::EngineFilesRuntime),
        scheduler_runtime: Arc::new(crate::scheduler::EngineSchedulerRuntime),
        memory_runtime: Arc::new(crate::memories::EngineMemoryRuntime),
        task_storage_runtime: Arc::new(crate::tasks::storage::EngineTaskStorageRuntime),
        shutdown_flag: Arc::new(AtomicBool::new(false)),
        lsp_tcp_client_count: Arc::new(AtomicUsize::new(0)),
        exec_registry: Arc::new(ExecRegistry::new()),
        card_verifier_tasks: Arc::new(AMutex::new(tokio::task::JoinSet::new())),
        trajectory_vectorization_queue: Arc::new(AMutex::new(Default::default())),
        cmdline: cmdline.clone(),
        http_client: http_client.clone(),
        context_factory: Arc::new(ContextFactorySlot::new()),
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
    gcx.context_factory
        .install(Arc::new(crate::app_state::EngineContextFactory));
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
        let gcx = refact_test_support::make_test_gcx(
            Arc::new(crate::scheduler::EngineSchedulerRuntime),
            Arc::new(crate::memories::EngineMemoryRuntime),
            Arc::new(crate::tasks::storage::EngineTaskStorageRuntime),
            Arc::new(crate::files_in_workspace::EngineFilesRuntime),
        )
        .await;
        gcx.context_factory
            .install(Arc::new(crate::app_state::EngineContextFactory));
        gcx
    }

    pub async fn make_test_gcx_with_dirs(
        cache_dir: PathBuf,
        config_dir: PathBuf,
    ) -> SharedGlobalContext {
        let gcx = refact_test_support::make_test_gcx_with_dirs(
            cache_dir,
            config_dir,
            Arc::new(crate::scheduler::EngineSchedulerRuntime),
            Arc::new(crate::memories::EngineMemoryRuntime),
            Arc::new(crate::tasks::storage::EngineTaskStorageRuntime),
            Arc::new(crate::files_in_workspace::EngineFilesRuntime),
        )
        .await;
        gcx.context_factory
            .install(Arc::new(crate::app_state::EngineContextFactory));
        gcx
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
