use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex as StdMutex, RwLock as StdRwLock};

use async_trait::async_trait;
use refact_agents_core::registry::BackgroundAgentRegistry;
use refact_browser::BrowserRuntime;
use refact_chat_api::subchat_types::{
    ChatMeta, ExecuteToolsOptions, ExplicitSubchatSpec, SubchatConfig, SubchatParameters,
    SubchatResult, TraceParent, WrapUpConfig,
};
use refact_chat_api::types::SessionsMap;
use refact_chat_api::{
    ChatMessage, ChatSession, TaskMeta, ThreadParams, TrajectoryCommitIntent,
    TrajectorySourceIdentity,
};
use refact_chat_history::trajectory_model::{LoadedTrajectory, TrajectoryData, TrajectoryRepairPatch};
use refact_core::ast_types::Document;
use refact_core::knowledge_index::KnowledgeIndex;
use refact_core::privacy_types::PrivacySettings;
use refact_core::vecdb_types::VecdbSearch;
use refact_core::worktree_meta::WorktreeMeta;
use refact_ext::hooks::HookEvent;
use refact_ext::hooks_payload::{HookPayload, HookResult};
use refact_ext::skills_context::SkillsTrackingInfo;
use refact_privacy::PolicyLoad;
use refact_scratchpads::scratchpad_utils::HasRagResults;
use refact_yaml_configs::registry_cache::RegistryCacheManager;
use tokio::sync::{Mutex as AMutex, RwLock as ARwLock};

use crate::services::{
    BuddyServices, ChatServices, ModelServices, PathServices, RuntimeServices,
};
use crate::verifier::CardVerifier;
use crate::{
    ActivitySink, BuddyEventSink, CapsProvider, ModeConfigProvider, TerminalSecurityConfig,
    ToolCatalogGenerations, ToolRegistry,
};

#[derive(Clone)]
pub struct ChatWorkspace {
    pub workspace_folders: Arc<StdMutex<Vec<PathBuf>>>,
    pub vec_db: Arc<AMutex<Option<Arc<dyn VecdbSearch>>>>,
    pub vec_db_error: Arc<StdMutex<String>>,
    pub knowledge_index: Arc<AMutex<KnowledgeIndex>>,
    pub at_commands_preview_cache: Arc<AMutex<refact_core::at_commands_preview_cache::AtCommandsPreviewCache>>,
    pub privacy_settings: Arc<PrivacySettings>,
    pub enrichment_generation: Arc<AtomicU64>,
    pub memory_document_map: Arc<AMutex<HashMap<PathBuf, Arc<ARwLock<Document>>>>>,
}

#[derive(Clone)]
pub struct ChatIntegrationScope {
    pub browser_runtimes: Arc<AMutex<HashMap<String, Arc<AMutex<BrowserRuntime>>>>>,
}

#[async_trait]
pub trait SubchatLauncher: Send + Sync {
    async fn run_subchat(
        &self,
        messages: Vec<ChatMessage>,
        config: SubchatConfig,
    ) -> Result<SubchatResult, String>;

    async fn run_subchat_once(
        &self,
        subagent_id: &str,
        messages: Vec<ChatMessage>,
        trace_parent: TraceParent,
    ) -> Result<SubchatResult, String>;

    async fn resolve_subchat_params(
        &self,
        tool_name: &str,
    ) -> Result<SubchatParameters, String>;

    async fn resolve_subchat_config_with_parent(
        &self,
        tool_name: &str,
        stateful: bool,
        chat_id: Option<String>,
        title: Option<String>,
        parent_id: Option<String>,
        link_type: Option<String>,
        root_chat_id: Option<String>,
        tools: Option<Vec<String>>,
        max_steps: usize,
        prepend_system_prompt: bool,
        wrap_up: Option<WrapUpConfig>,
        mode: String,
        task_meta: Option<TaskMeta>,
        worktree: Option<WorktreeMeta>,
        parent_tool_call_id: Option<String>,
        parent_subchat_tx: Option<Arc<AMutex<tokio::sync::mpsc::UnboundedSender<serde_json::Value>>>>,
        abort_flag: Option<Arc<std::sync::atomic::AtomicBool>>,
        subchat_depth: usize,
    ) -> Result<SubchatConfig, String>;

    async fn resolve_subchat_config_with_explicit_params(
        &self,
        attribution_id: &str,
        spec: &ExplicitSubchatSpec,
        stateful: bool,
        chat_id: Option<String>,
        title: Option<String>,
        parent_id: Option<String>,
        link_type: Option<String>,
        root_chat_id: Option<String>,
        tools: Option<Vec<String>>,
        max_steps: usize,
        prepend_system_prompt: bool,
        mode: String,
        task_meta: Option<TaskMeta>,
        worktree: Option<WorktreeMeta>,
        parent_tool_call_id: Option<String>,
        parent_subchat_tx: Option<Arc<AMutex<tokio::sync::mpsc::UnboundedSender<serde_json::Value>>>>,
        abort_flag: Option<Arc<std::sync::atomic::AtomicBool>>,
        subchat_depth: usize,
    ) -> Result<SubchatConfig, String>;

    async fn run_subchat_once_with_parent(
        &self,
        tool_name: &str,
        messages: Vec<ChatMessage>,
        parent_tool_call_id: String,
        parent_subchat_tx: Arc<AMutex<tokio::sync::mpsc::UnboundedSender<serde_json::Value>>>,
        parent_abort_flag: Arc<std::sync::atomic::AtomicBool>,
        parent_depth: usize,
        parent_task_meta: Option<TaskMeta>,
        parent_worktree: Option<WorktreeMeta>,
        trace_parent: TraceParent,
    ) -> Result<SubchatResult, String>;

    async fn run_subchat_once_with_explicit_params(
        &self,
        attribution_id: &str,
        spec: &ExplicitSubchatSpec,
        messages: Vec<ChatMessage>,
        parent_tool_call_id: String,
        parent_subchat_tx: Arc<AMutex<tokio::sync::mpsc::UnboundedSender<serde_json::Value>>>,
        parent_abort_flag: Arc<std::sync::atomic::AtomicBool>,
        parent_depth: usize,
        parent_task_meta: Option<TaskMeta>,
        parent_worktree: Option<WorktreeMeta>,
        trace_parent: TraceParent,
    ) -> Result<SubchatResult, String>;
}

#[async_trait]
pub trait ProjectTreeProvider: Send + Sync {
    async fn generate_compact_project_tree(&self, max_depth: usize) -> Result<String, String>;
}

#[async_trait]
pub trait HookRunner: Send + Sync {
    async fn project_dir(&self) -> String;
    async fn run_hooks(&self, event: HookEvent, payload: HookPayload) -> Vec<HookResult>;
}

pub trait ExecPolicyProvider: Send + Sync {
    fn terminal_security_config(&self) -> TerminalSecurityConfig;
}

#[async_trait]
pub trait TrajectoryStore: Send + Sync {
    async fn load_trajectory_for_chat(&self, chat_id: &str) -> Option<LoadedTrajectory>;
    async fn refresh_session_from_trajectory_if_stale(
        &self,
        chat_id: &str,
        allow_replacing_messages: bool,
    ) -> bool;
    async fn apply_mode_defaults_to_thread(
        &self,
        thread: &mut ThreadParams,
        auto_approve_editing_present: bool,
        auto_approve_dangerous_present: bool,
    );
    async fn persist_loaded_trajectory_repair_raw(
        &self,
        repair: &TrajectoryRepairPatch,
    ) -> Result<String, String>;
    async fn maybe_save_trajectory_with_intent(
        &self,
        session_arc: Arc<AMutex<ChatSession>>,
        intent: TrajectoryCommitIntent,
    );
    async fn try_save_trajectory_with_intent(
        &self,
        session_arc: Arc<AMutex<ChatSession>>,
        intent: TrajectoryCommitIntent,
    ) -> Result<bool, String>;
    async fn flush_trajectory_for_session(
        &self,
        session_arc: Arc<AMutex<ChatSession>>,
    ) -> Result<(), String>;

    async fn find_trajectory_path(&self, chat_id: &str) -> Option<PathBuf>;

    async fn find_trajectory_or_buddy_path(&self, chat_id: &str) -> Option<PathBuf>;

    async fn find_trajectory_path_for_active_chat(
        &self,
        sessions: &SessionsMap,
        chat_id: &str,
    ) -> Option<PathBuf>;

    async fn read_trajectory_or_buddy_content(&self, chat_id: &str) -> Option<String>;

    async fn load_trajectory_by_chat_id(&self, chat_id: &str) -> Option<LoadedTrajectory>;

    async fn load_generic_trajectory_for_chat(&self, chat_id: &str) -> Option<LoadedTrajectory>;

    async fn load_generic_trajectory_for_chat_matching_source(
        &self,
        chat_id: &str,
        source: &TrajectorySourceIdentity,
    ) -> Option<LoadedTrajectory>;

    async fn trajectory_source_path_is_allowed(&self, path: &std::path::Path) -> bool;

    async fn safe_new_task_trajectory_file(
        &self,
        task_meta: &TaskMeta,
        chat_id: &str,
    ) -> Result<PathBuf, String>;

    async fn safe_new_buddy_trajectory_file(&self, chat_id: &str) -> Result<PathBuf, String>;

    async fn safe_new_nested_trajectory_file(
        &self,
        chat_id: &str,
        root_chat_id: Option<&str>,
    ) -> Result<PathBuf, String>;

    async fn find_normal_trajectory_path(&self, chat_id: &str) -> Option<PathBuf>;

    async fn resolve_trajectory_data_save_path(
        &self,
        id: &str,
        data: &TrajectoryData,
    ) -> Result<PathBuf, String>;

    async fn validate_loaded_worktree_strict(
        &self,
        chat_id: &str,
        worktree: WorktreeMeta,
    ) -> Option<WorktreeMeta>;

    async fn index_dir_for_trajectory_file(&self, file_path: &std::path::Path) -> Option<PathBuf>;

    async fn list_trajectory_dirs(&self) -> Vec<PathBuf>;

    async fn get_all_task_roots(&self) -> Vec<PathBuf>;

    async fn enqueue_trajectory_vectorization(&self, file_path: PathBuf);

    async fn prune_buddy_conversations_throttled(&self);
}

#[async_trait]
pub trait PromptAssembler: Send + Sync {
    async fn prepend_system_prompt(
        &self,
        messages: Vec<ChatMessage>,
        chat_meta: &ChatMeta,
        task_meta: &Option<TaskMeta>,
        stream_back_to_user: &mut HasRagResults,
        tool_names: HashSet<String>,
        mode_id: &str,
        model_id: &str,
    ) -> (Vec<ChatMessage>, SkillsTrackingInfo);
}

#[async_trait]
pub trait ChatToolExecutor: Send + Sync {
    async fn execute_tools(
        &self,
        tool_calls: &[refact_core::chat_types::ChatToolCall],
        messages: &[ChatMessage],
        thread: &ThreadParams,
        mode_id: &str,
        model_id: Option<&str>,
        options: ExecuteToolsOptions,
    ) -> (Vec<ChatMessage>, bool);
}

#[derive(Clone)]
pub struct ChatCtx {
    pub knowledge_builder: Arc<dyn crate::knowledge_index::KnowledgeBuilder>,
    pub task_storage_runtime: Arc<dyn crate::task_storage::TaskStorageRuntime>,
    pub runtime: RuntimeServices,
    pub paths: PathServices,
    pub model: ModelServices,
    pub workspace: ChatWorkspace,
    pub chat: ChatServices,
    pub buddy: BuddyServices,
    pub integrations: ChatIntegrationScope,
    pub agents: Arc<BackgroundAgentRegistry>,
    pub tool_registry: Arc<dyn ToolRegistry>,
    pub activity_sink: Arc<dyn ActivitySink>,
    pub buddy_event_sink: Arc<dyn BuddyEventSink>,
    pub card_verifier: Arc<dyn CardVerifier>,
    pub card_verifier_tasks: Arc<AMutex<tokio::task::JoinSet<()>>>,
    pub privacy_policy_load: Arc<StdRwLock<PolicyLoad>>,
    pub tool_catalog_generations: Arc<ToolCatalogGenerations>,
    pub project_registry_cache: Arc<StdRwLock<RegistryCacheManager>>,
    pub trajectory_vectorization_queue: Arc<AMutex<refact_chat_api::TrajectoryVectorizationQueue>>,
    pub subchat: Arc<dyn SubchatLauncher>,
    pub project_tree: Arc<dyn ProjectTreeProvider>,
    pub hooks: Arc<dyn HookRunner>,
    pub mode_config: Arc<dyn ModeConfigProvider>,
    pub caps: Arc<dyn CapsProvider>,
    pub exec_policy: Arc<dyn ExecPolicyProvider>,
    pub trajectories: Arc<dyn TrajectoryStore>,
    pub prompts: Arc<dyn PromptAssembler>,
    pub tool_executor: Arc<dyn ChatToolExecutor>,
}
