use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
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
