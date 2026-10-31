use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize};
use std::sync::{Arc, Mutex as StdMutex, RwLock as StdRwLock};

use refact_agents_core::registry::BackgroundAgentRegistry;
use refact_buddy_core::user_activity::UserActivityRing;
use refact_caps_core::caps_state::CapsState;
use refact_chat_history::trajectory_index::TrajectoryIndexCoordinator;
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
use refact_perf::perf_diagnostics;
use refact_privacy::runtime_state::PrivacyObservationRuntimeState;
use refact_privacy::PolicyLoad;
use refact_providers::quota::ProviderQuotaCache;
use refact_providers::ProviderRegistry;
use refact_runtime_api::global_context::{
    build_shared_http_client_builder, EngineGlobalConfig, GlobalContext, SharedGlobalContext,
};
use refact_runtime_api::{CommandLine, TerminalSecurityConfig, ToolCatalogCache, ToolCatalogGenerations};
use refact_scratchpads::completion_cache::CompletionCache;
use refact_yaml_configs::registry_cache::RegistryCacheManager;
use tokio::sync::{Mutex as AMutex, RwLock as ARwLock};

static TEST_TEMP_DIRS: std::sync::OnceLock<StdMutex<Vec<PathBuf>>> = std::sync::OnceLock::new();

fn test_temp_dir_is_safe(path: &Path) -> bool {
    let temp_dir = std::env::temp_dir();
    if !path.starts_with(&temp_dir) {
        return false;
    }
    let Ok(path) = path.canonicalize() else {
        return false;
    };
    let Ok(temp_dir) = temp_dir.canonicalize() else {
        return false;
    };
    path.starts_with(&temp_dir)
        && path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("refact-"))
}

extern "C" fn remove_test_temp_dirs_at_exit() {
    let Some(dirs) = TEST_TEMP_DIRS.get() else {
        return;
    };
    let dirs = dirs.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    for dir in dirs.iter() {
        if test_temp_dir_is_safe(dir) {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

pub fn register_test_temp_dir(path: &Path) -> bool {
    if !test_temp_dir_is_safe(path) {
        return false;
    }
    let dirs = TEST_TEMP_DIRS.get_or_init(|| {
        unsafe {
            libc::atexit(remove_test_temp_dirs_at_exit);
        }
        StdMutex::new(Vec::new())
    });
    dirs.lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .push(path.to_path_buf());
    true
}

pub fn registered_test_temp_dirs() -> Vec<PathBuf> {
    TEST_TEMP_DIRS
        .get()
        .map(|dirs| {
            dirs.lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone()
        })
        .unwrap_or_default()
}

pub async fn make_test_gcx(
    task_storage_runtime: Arc<dyn refact_runtime_api::task_storage::TaskStorageRuntime>,
) -> SharedGlobalContext {
    let cache_dir = std::env::temp_dir().join(format!("refact-test-{}", uuid::Uuid::new_v4()));
    let config_dir = std::env::temp_dir().join(format!("refact-cfg-{}", uuid::Uuid::new_v4()));
    make_test_gcx_with_dirs(cache_dir, config_dir, task_storage_runtime).await
}

pub async fn make_test_gcx_with_dirs(
    cache_dir: PathBuf,
    config_dir: PathBuf,
    task_storage_runtime: Arc<dyn refact_runtime_api::task_storage::TaskStorageRuntime>,
) -> SharedGlobalContext {
    let (ask_shutdown_sender, _) = std::sync::mpsc::channel::<String>();

    let _ = std::fs::create_dir_all(&cache_dir);
    let _ = std::fs::create_dir_all(&config_dir);
    register_test_temp_dir(&cache_dir);
    register_test_temp_dir(&config_dir);
    let user_activity = UserActivityRing::load(&cache_dir).await;
    let agents = BackgroundAgentRegistry::new(cache_dir.join(".refact").join("background_agents"))
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

    let _ = EngineGlobalConfig::default();
    let cx = GlobalContext {
        task_storage_runtime,
        shutdown_flag: Arc::new(AtomicBool::new(false)),
        lsp_tcp_client_count: Arc::new(AtomicUsize::new(0)),
        exec_registry: Arc::new(ExecRegistry::new()),
        card_verifier_tasks: Arc::new(AMutex::new(tokio::task::JoinSet::new())),
        trajectory_vectorization_queue: Arc::new(AMutex::new(Default::default())),
        cmdline,
        http_client,
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
