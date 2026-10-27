use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex as StdMutex, RwLock as StdRwLock};

use async_trait::async_trait;
use refact_chat_api::ChatMessage;
use refact_core::vecdb_types::VecdbSearch;
use refact_runtime_api::{ActivitySink, BuddyEventSink, ToolRegistry};
use tokio::sync::Mutex as AMutex;

use crate::agents::registry::BackgroundAgentRegistry;
use crate::app_state::{
    AtCommandsPreviewCache, BuddyServices, ChatServices, CommandLine, ModelServices, PathServices,
    RuntimeServices, ToolCatalogGenerations,
};
use crate::caps::CodeAssistantCaps;
use crate::call_validation::{ChatMeta, ChatToolCall};
use crate::chat::tools::ExecuteToolsOptions;
use crate::chat::trajectories::{
    LoadedTrajectory, TrajectoryData, TrajectoryRepairPatch, TrajectoryVectorizationQueue,
};
use crate::chat::types::{ChatSession, ThreadParams, TrajectoryCommitIntent, TrajectorySourceIdentity};
use crate::custom_error::ScratchError;
use crate::ext::hooks::HookEvent;
use crate::ext::hooks_runner::{HookPayload, HookResult};
use crate::ext::skills_context::SkillsTrackingInfo;
use crate::global_context::TerminalSecurityConfig;
use crate::integrations::browser_runtime::BrowserRuntime;
use crate::knowledge_index::KnowledgeIndex;
use crate::privacy::PrivacySettings;
use crate::scratchpads::scratchpad_utils::HasRagResults;
use crate::subchat::{SubchatConfig, SubchatResult, TraceParent};
use crate::yaml_configs::customization_registry::RegistryCacheManager;
use crate::yaml_configs::customization_types::{ModeConfig, ProjectRegistry, SubagentConfig};

#[derive(Clone)]
pub struct ChatWorkspace {
    pub workspace_folders: Arc<StdMutex<Vec<PathBuf>>>,
    pub vec_db: Arc<AMutex<Option<Arc<dyn VecdbSearch>>>>,
    pub vec_db_error: Arc<StdMutex<String>>,
    pub knowledge_index: Arc<AMutex<KnowledgeIndex>>,
    pub at_commands_preview_cache: Arc<AMutex<AtCommandsPreviewCache>>,
    pub privacy_settings: Arc<PrivacySettings>,
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

#[async_trait]
pub trait ModeConfigProvider: Send + Sync {
    async fn get_project_registry(&self) -> Option<ProjectRegistry>;
    async fn get_mode_config(&self, mode_id: &str, model_id: Option<&str>) -> Option<ModeConfig>;
    async fn get_subagent_config(
        &self,
        subagent_id: &str,
        model_id: Option<&str>,
    ) -> Option<SubagentConfig>;
}

#[async_trait]
pub trait CapsProvider: Send + Sync {
    async fn try_load_caps_quickly(
        &self,
        max_age_seconds: u64,
    ) -> Result<Arc<CodeAssistantCaps>, ScratchError>;
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
        sessions: &crate::chat::SessionsMap,
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
        task_meta: &crate::chat::types::TaskMeta,
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
        worktree: crate::worktrees::types::WorktreeMeta,
    ) -> Option<crate::worktrees::types::WorktreeMeta>;

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
        task_meta: &Option<crate::chat::types::TaskMeta>,
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
        tool_calls: &[ChatToolCall],
        messages: &[ChatMessage],
        thread: &ThreadParams,
        mode_id: &str,
        model_id: Option<&str>,
        options: ExecuteToolsOptions,
    ) -> (Vec<ChatMessage>, bool);
}

#[derive(Clone)]
pub struct ChatCtx {
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
    pub card_verifier_tasks: Arc<AMutex<tokio::task::JoinSet<()>>>,
    pub privacy_policy_load: Arc<StdRwLock<refact_privacy::PolicyLoad>>,
    pub tool_catalog_generations: Arc<ToolCatalogGenerations>,
    pub project_registry_cache: Arc<StdRwLock<RegistryCacheManager>>,
    pub trajectory_vectorization_queue: Arc<AMutex<TrajectoryVectorizationQueue>>,
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
