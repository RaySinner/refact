use std::path::PathBuf;

use async_trait::async_trait;
use refact_buddy_core::snapshot::BuddySnapshot;
use refact_buddy_core::types::{BuddyRuntimeEvent, BuddySuggestion};
use refact_buddy_core::user_action::UserAction;
use refact_chat_api::{
    ChatCommand, ChatMessage, ContextFile, DeliveryOutcome, GoalSnapshot, NotificationEvent,
    PauseReason, PendingDelivery, PushMode, ThreadParams,
};
use refact_chat_history::trajectory_event::TrajectoryEvent;
use refact_chat_history::trajectory_snapshot::TrajectorySnapshot;
use refact_tasks::events::TaskEventEnvelope;
use refact_tool_api::{build_registry_from_names, ToolDesc};

pub mod command_line;
pub mod trajectories;
pub mod command_policy;
pub mod files_blocklist;
pub mod parent_stdin;
pub mod runtime_config;
pub mod call_validation;
pub mod compress_trajectory;
pub mod privacy_records;
pub mod indexing_routing;
pub mod files_in_jsonl;
pub mod git_runtime;
pub mod files_disk;
pub mod pp_gcx_context;
pub mod pp_tool_results;
pub mod pp_utils;
pub mod vdb_highlev;
pub mod vdb_structs;
pub mod scratchpads_runtime;
pub mod spawn;
pub mod codegraph_highlev;
pub mod codegraph_status;
pub mod config_files_bootstrap;
pub mod caps_loader;
pub mod context_factory;
pub mod chat_ctx;
pub mod files_correction;
pub mod files_correction_impl;
pub mod files_in_workspace;
pub mod gcx_helpers;
pub mod global_context;
pub mod engine_bootstrap;
pub mod integr_abstract;
pub mod integrations;
pub mod integrations_mcp_resources;
pub mod pp_tool_context;
pub mod scope_utils;
pub mod knowledge_graph;
pub mod knowledge_index;
pub mod restream;
pub mod knowledge_runtime;
pub mod mode_validation;
pub mod project_information;
pub mod privacy;
pub mod runtime_settings;
pub mod stats;
pub mod terminal_security;
pub mod tool_catalog;
pub mod tool_agent;
pub mod ctx_traits;
pub mod services;
pub mod tool_ctx;
pub mod app_state;
pub mod at_commands_ctx;
pub mod at_command_executor;
pub mod at_commands_impl;
pub mod at_commands_runtime;
pub mod at_tree;
pub mod ext_skills_context;
pub mod ext_slash_expand;
pub mod tool_trait;
pub mod post_merge_check;
pub mod task_storage;
pub mod memory_lifecycle_runtime;
pub mod buddy_memory_lifecycle;
pub mod buddy_autonomous_spec;
pub mod buddy_memory_ops;
pub mod buddy_planner_backstop_runner;
pub mod buddy_scheduler;
pub mod buddy_jobs;
pub mod goal_verifier_runtime;
pub mod task_agent_monitor;
pub mod task_agent_monitor_merge;
pub mod session_runtime;
pub mod system_context;
pub mod trajectory_model_runtime;
pub mod trajectory_prefix;
pub mod commit_message_runtime;
pub mod generate_code_edit_runtime;
pub mod generate_follow_up_runtime;
pub mod kg_subchat_runtime;
pub mod task_comments;
pub mod providers_oauth_runtime;
pub mod providers_http_runtime;
pub mod providers_pricing_runtime;
pub mod integrations_sessions;
pub mod agents_delivery;
pub mod buddy_actor_runtime;
pub mod buddy_observers;
pub mod buddy_chat_interjection;
pub mod buddy_diagnostics;
pub mod buddy_privacy_degraded;
pub mod buddy_pulse_inject;
pub mod buddy_runtime;
pub mod subchat_runtime;
pub mod trajectory_runtime;
pub mod mode_transition_runtime;
pub mod chat_tokens;
pub mod chat_cache_guard;
pub mod cloud_token_count;
pub mod buddy_issue_runtime;
pub mod integration_config_dirs;
pub mod context_tools;
 pub mod verifier;
 pub mod wait_interrupt;

pub use context_tools::vec_context_file_to_context_tools;
pub use wait_interrupt::WaitInterrupt;

pub use chat_ctx::{
    ChatCtx, ChatIntegrationScope, ChatToolExecutor, ChatWorkspace, ExecPolicyProvider, HookRunner,
    ProjectTreeProvider, PromptAssembler, SubchatLauncher, TrajectoryStore,
};
pub use caps_loader::try_load_caps_quickly_if_not_present;
pub use context_factory::{CapsLoader, ContextFactory, ContextFactorySlot};
pub use gcx_helpers::{
    declared_file_records, privacy_record_scope, reload_indexing_everywhere_for_gcx,
    tool_enrichment_from_path_references,
};
pub use global_context::{app_state_from_gcx, GlobalContextCtx};
pub use command_line::CommandLine;
pub use terminal_security::{TerminalSecurityConfig, TerminalSecurityMode};
pub use tool_catalog::{
    tool_catalog_snapshot_rollout_enabled, tool_catalog_snapshot_rollout_enabled_for,
    ToolCatalogCache, ToolCatalogGenerations, ToolCatalogKey, TOOL_CATALOG_SNAPSHOTS_ENV,
    TOOL_CATALOG_SNAPSHOT_CACHE_LIMIT,
};
pub use ctx_traits::{
    BuddyServicesHandle, CapsProvider, FilesProvider, ModeConfigProvider, SubchatLauncherHandle,
    TrajectoryStoreHandle,
};
pub use services::{
    BuddyServices, ChatServices, IntegrationServices, ModelServices, PathServices, RuntimeServices,
    WorkspaceServices,
};

pub use app_state::AppState;
pub use spawn::{spawn_background_agent, BackgroundAgentSpawner};
#[cfg(any(test, feature = "test-util"))]
pub use spawn::{current_test_runner, install_test_runner, TestRunner, TestRunnerGuard};
pub use at_command_executor::AtCommandExecutor;
pub use at_commands_ctx::{
    AtCommand, AtCommandMember, AtCommandsContext, AtParam, MAX_SUBCHAT_DEPTH,
};
pub use at_commands_runtime::{
    at_commands_dict, at_commands_new_from_app, at_commands_new_with_abort,
    at_commands_new_with_abort_and_execution_scope, correct_at_arg, execute_at_commands_in_query,
    run_at_commands_locally, MIN_RAG_CONTEXT_LIMIT,
};
pub use ext_skills_context::{build_skills_context_messages_tracked, build_skills_prompt_text};
pub use ext_slash_expand::{expand_slash_command_non_mcp, ExpandedCommand, SkillActivationInfo};
pub use tool_trait::{Tool, ToolGroup};
pub use refact_chat_api::{DeliveryOutcome as RuntimeDeliveryOutcome, SessionState, TaskMeta};
pub use refact_chat_api::{PendingDelivery as RuntimePendingDelivery, PushMode as RuntimePushMode};
pub use refact_chat_api::{ToolCatalogSnapshot, ToolPolicyInfo, ToolRegistryIndex, TurnToolPool};
pub use refact_tool_api::ToolDesc as RuntimeToolDesc;
pub use refact_buddy_core::types::BuddyRuntimeEvent as RuntimeBuddyEvent;
pub use refact_buddy_core::user_action::UserAction as RuntimeUserAction;
pub use refact_chat_history::trajectory_snapshot::TrajectorySnapshot as RuntimeTrajectorySnapshot;

#[async_trait]
pub trait ActivitySink: Send + Sync {
    async fn record_user_action(&self, action: UserAction);
}

#[async_trait]
pub trait BuddyEventSink: Send + Sync {
    async fn enqueue_event(&self, event: BuddyRuntimeEvent);
    async fn complete_event(&self, dedupe_key: &str, status: &str);
    async fn snapshot(&self) -> Option<BuddySnapshot>;
    async fn apply_chat_completion(&self, event: BuddyRuntimeEvent, xp: u64, mood: String);
    async fn report_error(
        &self,
        error_type: &str,
        error_msg: &str,
        source: Option<&str>,
        chat_id: Option<&str>,
    );
    async fn report_error_with_model(
        &self,
        error_type: &str,
        error_msg: &str,
        source: Option<&str>,
        chat_id: Option<&str>,
        model_id: Option<&str>,
    ) {
        let _ = model_id;
        self.report_error(error_type, error_msg, source, chat_id)
            .await;
    }
    async fn mark_chat_error(&self, event: BuddyRuntimeEvent);
    async fn maybe_add_suggestion(&self, suggestion: BuddySuggestion);
    async fn build_pulse_message(&self) -> Option<ChatMessage>;
    async fn render_runtime_event_fast(
        &self,
        workflow_id: &str,
        workflow_summary: &str,
        status: &str,
    ) -> Option<(String, Option<String>)>;

    /// Seam over the engine-only `buddy::speech::render_buddy_activity_title`.
    async fn render_autonomous_report_title(
        &self,
        workflow_id: &str,
        workflow_summary: String,
        fallback_title: String,
    ) -> String;

    /// Seam over the engine-only `buddy::speech::render_buddy_runtime_event`.
    /// `None` means the buddy service is unavailable, so the caller keeps its own
    /// fallback title and description.
    async fn render_autonomous_result_summary(
        &self,
        workflow_id: &str,
        workflow_summary: String,
        fallback_title: String,
        fallback_description: String,
    ) -> Option<(String, Option<String>)>;

    /// Seam over the engine buddy workflow wrapper. The engine implementation
    /// forwards to `buddy::workflows::buddy_wrap_workflow` (which builds the
    /// activity summary from the prefix and the workflow output); the default
    /// runs the work without buddy bookkeeping.
    async fn run_workflow(
        &self,
        _workflow_id: &str,
        _icon: &str,
        _xp: u64,
        _summary_prefix: &str,
        _summary_max_chars: usize,
        work: Box<dyn FnOnce() -> WorkflowFuture + Send + 'static>,
    ) -> Result<String, String> {
        work().await
    }
}

pub type WorkflowFuture =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send>>;

#[derive(Clone)]
pub struct ToolConfirmationCheck {
    pub tool_name: String,
    pub result: refact_tool_api::MatchConfirmDeny,
    pub integr_config_path: Option<String>,
}

#[derive(Clone)]
pub struct ToolExecutionResult {
    pub had_corrections: bool,
    pub messages: Vec<ChatMessage>,
    pub context_files: Vec<ContextFile>,
}

#[async_trait]
pub trait ToolRegistry: Send + Sync {
    async fn get_tools_for_mode(&self, mode: &str, model_id: Option<&str>) -> Vec<ToolDesc>;

    async fn get_tool_names_for_mode(
        &self,
        mode: &str,
        model_id: Option<&str>,
    ) -> std::collections::HashSet<String> {
        self.get_tools_for_mode(mode, model_id)
            .await
            .into_iter()
            .map(|tool| tool.name)
            .collect()
    }
    async fn get_tools_index_for_mode(
        &self,
        mode: &str,
        model_id: Option<&str>,
    ) -> ToolRegistryIndex;
    async fn get_tools_index_for_mode_and_scope(
        &self,
        mode: &str,
        model_id: Option<&str>,
        execution_scope: Option<&str>,
    ) -> ToolRegistryIndex {
        let _ = execution_scope;
        self.get_tools_index_for_mode(mode, model_id).await
    }
    async fn acquire_tool_catalog(
        &self,
        mode: &str,
        model_id: Option<&str>,
        execution_scope: Option<&str>,
    ) -> std::sync::Arc<ToolCatalogSnapshot> {
        let index = self
            .get_tools_index_for_mode_and_scope(mode, model_id, execution_scope)
            .await;
        let names = index
            .tools
            .iter()
            .map(|tool| tool.name.clone())
            .collect::<Vec<_>>();
        let policy = self.get_tool_policy_info(mode, model_id).await;
        std::sync::Arc::new(ToolCatalogSnapshot {
            index,
            policy,
            aliases: build_registry_from_names(&names),
        })
    }
    async fn acquire_turn_tool_pool(
        &self,
        mode: &str,
        model_id: Option<&str>,
        execution_scope: Option<&str>,
        catalog: &ToolCatalogSnapshot,
    ) -> Option<TurnToolPool> {
        let _ = (mode, model_id, execution_scope, catalog);
        None
    }
    async fn prepare_turn_tool_pool(
        &self,
        pool: &TurnToolPool,
        catalog: &ToolCatalogSnapshot,
        mode: &str,
        model_id: Option<&str>,
        tool_slots: &[(ToolDesc, usize)],
    ) -> Result<(), String> {
        let _ = (pool, catalog, mode, model_id, tool_slots);
        Ok(())
    }
    async fn check_tool_confirmation(
        &self,
        ccx: &(dyn std::any::Any + Send + Sync),
        mode: &str,
        model_id: Option<&str>,
        tool_name: &str,
        args: serde_json::Map<String, serde_json::Value>,
    ) -> Option<Result<ToolConfirmationCheck, String>>;
    async fn check_tool_confirmation_with_catalog(
        &self,
        ccx: &(dyn std::any::Any + Send + Sync),
        catalog: &ToolCatalogSnapshot,
        mode: &str,
        model_id: Option<&str>,
        tool_name: &str,
        args: serde_json::Map<String, serde_json::Value>,
    ) -> Option<Result<ToolConfirmationCheck, String>> {
        let _ = catalog;
        self.check_tool_confirmation(ccx, mode, model_id, tool_name, args)
            .await
    }
    async fn check_tool_confirmation_with_catalog_and_pool(
        &self,
        ccx: &(dyn std::any::Any + Send + Sync),
        catalog: &ToolCatalogSnapshot,
        pool: Option<&TurnToolPool>,
        mode: &str,
        model_id: Option<&str>,
        tool_name: &str,
        args: serde_json::Map<String, serde_json::Value>,
    ) -> Option<Result<ToolConfirmationCheck, String>> {
        let _ = pool;
        self.check_tool_confirmation_with_catalog(ccx, catalog, mode, model_id, tool_name, args)
            .await
    }
    async fn get_tool_policy_info(&self, mode: &str, model_id: Option<&str>)
        -> Vec<ToolPolicyInfo>;
    async fn execute_tool(
        &self,
        ccx: &(dyn std::any::Any + Send + Sync),
        mode: &str,
        model_id: Option<&str>,
        tool_call_id: &str,
        tool_name: &str,
        args: serde_json::Map<String, serde_json::Value>,
    ) -> Result<Option<ToolExecutionResult>, String>;
    async fn execute_tool_with_catalog(
        &self,
        ccx: &(dyn std::any::Any + Send + Sync),
        catalog: &ToolCatalogSnapshot,
        mode: &str,
        model_id: Option<&str>,
        tool_call_id: &str,
        tool_name: &str,
        args: serde_json::Map<String, serde_json::Value>,
    ) -> Result<Option<ToolExecutionResult>, String> {
        let _ = catalog;
        self.execute_tool(ccx, mode, model_id, tool_call_id, tool_name, args)
            .await
    }
    async fn execute_tool_with_catalog_and_pool(
        &self,
        ccx: &(dyn std::any::Any + Send + Sync),
        catalog: &ToolCatalogSnapshot,
        pool: Option<&TurnToolPool>,
        mode: &str,
        model_id: Option<&str>,
        tool_call_id: &str,
        tool_name: &str,
        args: serde_json::Map<String, serde_json::Value>,
    ) -> Result<Option<ToolExecutionResult>, String> {
        let _ = pool;
        self.execute_tool_with_catalog(ccx, catalog, mode, model_id, tool_call_id, tool_name, args)
            .await
    }
    async fn load_task_memories(&self, task_id: &str) -> Result<Vec<(PathBuf, String)>, String>;

    /// Name of the GitHub MCP tool that files issues, when such an integration tool is
    /// registered. Used by the buddy issue pipeline, which lives in this crate.
    async fn github_issue_create_tool(&self) -> Option<String> {
        None
    }

    /// Runs the `mcp_call` proxy for an MCP tool and returns the tool's first text
    /// message. Used by the buddy issue pipeline.
    async fn run_mcp_call_bypass(
        &self,
        tool_name: String,
        args: serde_json::Value,
    ) -> Result<String, String> {
        let _ = (tool_name, args);
        Err("mcp_call is not available".to_string())
    }

    /// Runs a builtin buddy inspection tool (`buddy_get_logs` / `buddy_get_context`) and
    /// returns the tool's first text message. Used by the buddy issue pipeline.
    async fn run_buddy_tool(
        &self,
        tool_name: &str,
        args: serde_json::Value,
        error: Option<String>,
    ) -> Result<String, String> {
        let _ = (tool_name, args, error);
        Err("buddy tool execution is not available".to_string())
    }

}

#[derive(Clone)]
pub struct ChatSessionSnapshot {
    pub messages: Vec<ChatMessage>,
    pub thread: ThreadParams,
    pub session_state: SessionState,
    pub pause_reasons: Vec<PauseReason>,
    pub goal: Option<GoalSnapshot>,
}

#[derive(Clone)]
pub struct ChatSessionUpdate {
    pub messages: Vec<ChatMessage>,
    pub previous_response_id: Option<String>,
}

#[derive(Clone)]
pub struct CreateSessionRequest {
    pub chat_id: String,
    pub thread: ThreadParams,
    pub messages: Vec<ChatMessage>,
}

#[async_trait]
pub trait ChatSessionFacade: Send + Sync {
    async fn session_snapshot(&self, chat_id: &str) -> Result<ChatSessionSnapshot, String>;
    async fn update_session(&self, chat_id: &str, update: ChatSessionUpdate) -> Result<(), String>;
    async fn create_session(&self, request: CreateSessionRequest) -> Result<(), String>;
    async fn push_command(&self, chat_id: &str, command: ChatCommand) -> Result<(), String>;
    async fn push_priority_command(
        &self,
        chat_id: &str,
        command: ChatCommand,
    ) -> Result<(), String> {
        self.push_command(chat_id, command).await
    }
    /// Unified delivery entry point for producers. The default implementation
    /// only enqueues the command (so existing mocks keep compiling); the engine
    /// implementation reports the real outcome.
    async fn deliver_messages(
        &self,
        chat_id: &str,
        delivery: PendingDelivery,
    ) -> Result<DeliveryOutcome, String> {
        self.push_command(chat_id, ChatCommand::DeliverMessages { delivery })
            .await?;
        Ok(DeliveryOutcome::Queued)
    }
    /// Reprioritize or cancel a pending delivery. Default implementation routes
    /// through the command queue.
    async fn update_pending_delivery(
        &self,
        chat_id: &str,
        delivery_id: &str,
        push: Option<PushMode>,
        cancel: bool,
    ) -> Result<(), String> {
        self.push_command(
            chat_id,
            ChatCommand::UpdatePendingDelivery {
                delivery_id: delivery_id.to_string(),
                push,
                cancel,
            },
        )
        .await
    }
    async fn session_state(&self, chat_id: &str) -> Result<Option<SessionState>, String>;
    async fn maybe_save_session(&self, chat_id: &str) -> Result<(), String>;
    async fn save_trajectory_snapshot(&self, snapshot: TrajectorySnapshot) -> Result<(), String>;
}

/// Read seam over the engine chat sessions map. The concrete map type stays in
/// the engine; lower crates hold `Arc<dyn SessionsHandle>` and can reach the
/// engine map through `as_any` (same escape hatch as `TurnToolPool`).
#[async_trait]
pub trait SessionsHandle: Send + Sync {
    fn as_any(&self) -> &dyn std::any::Any;
    async fn session_ids(&self) -> Vec<String>;
    async fn session_state(&self, chat_id: &str) -> Option<SessionState>;
}

/// Sender seam for trajectory index events; the engine wraps its broadcast channel.
pub trait TrajectoryEventSender: Send + Sync {
    fn send(&self, event: TrajectoryEvent) -> usize;
}

/// Sender seam for task board events; the engine wraps its broadcast channel.
pub trait TaskEventEnvelopeSender: Send + Sync {
    fn send(&self, envelope: TaskEventEnvelope) -> usize;
}

/// Sender seam for sidebar notification events; the engine wraps its broadcast channel.
pub trait NotificationEventSender: Send + Sync {
    fn send(&self, event: NotificationEvent) -> usize;
}

pub mod scheduler;
pub mod scheduler_runner;
pub mod scheduler_delivery;
pub mod scheduler_exec_action;
pub mod scheduler_runtime;
pub mod task_events;
pub mod task_storage_runtime;
pub mod caps_load_runtime;
pub mod caps_models_dev;
pub mod caps_model_caps;
pub mod room_roster;
pub mod customization_registry;
pub mod customization_registry_gcx;
pub mod memories;
pub mod memories_runtime;
pub mod session_handles;
pub mod git_cleanup;
pub mod ext_config_dirs;
pub mod ext_hooks_runner;
pub mod ext_marketplace;
pub mod ext_plugins;
pub mod ext_runtime;
pub mod integration_variables;
pub mod integrations_runtime;
pub mod slash_cache;
pub mod browser_frame;
pub mod cli_dispatch;
pub mod daemon_link;
pub mod lsp_workspace_roots;
