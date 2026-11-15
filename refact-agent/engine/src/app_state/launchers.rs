#[allow(unused_imports)]
use super::*;

pub struct EngineSubchatLauncher {
    gcx: SharedGlobalContext,
}

impl EngineSubchatLauncher {
    pub fn new(gcx: SharedGlobalContext) -> Self {
        Self { gcx }
    }
}

#[async_trait]
impl SubchatLauncher for EngineSubchatLauncher {
    async fn run_subchat(
        &self,
        messages: Vec<ChatMessage>,
        config: SubchatConfig,
    ) -> Result<SubchatResult, String> {
        crate::subchat::run_subchat(self.gcx.clone(), messages, config).await
    }

    async fn run_subchat_once(
        &self,
        subagent_id: &str,
        messages: Vec<ChatMessage>,
        trace_parent: crate::subchat::TraceParent,
    ) -> Result<SubchatResult, String> {
        crate::subchat::run_subchat_once(self.gcx.clone(), subagent_id, messages, trace_parent).await
    }

    async fn run_subchat_once_with_abort(
        &self,
        subagent_id: &str,
        messages: Vec<ChatMessage>,
        abort_flag: Option<Arc<AtomicBool>>,
        trace_parent: crate::subchat::TraceParent,
    ) -> Result<SubchatResult, String> {
        crate::subchat::run_subchat_once_with_abort(
            self.gcx.clone(),
            subagent_id,
            messages,
            abort_flag,
            trace_parent,
        )
        .await
    }

    async fn resolve_subchat_params(
        &self,
        tool_name: &str,
    ) -> Result<SubchatParameters, String> {
        let ctx = self.gcx.chat_ctx(self.gcx.clone());
        crate::subchat::resolve_subchat_params(&ctx, tool_name).await
    }

    async fn resolve_subchat_config(
        &self,
        tool_name: &str,
        stateful: bool,
        chat_id: Option<String>,
        title: Option<String>,
        tools: Option<Vec<String>>,
        max_steps: usize,
        prepend_system_prompt: bool,
        mode: String,
    ) -> Result<SubchatConfig, String> {
        crate::subchat::resolve_subchat_config(
            self.gcx.clone(),
            tool_name,
            stateful,
            chat_id,
            title,
            None,
            None,
            None,
            tools,
            max_steps,
            prepend_system_prompt,
            None,
            mode,
        )
        .await
    }

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
        task_meta: Option<crate::chat::types::TaskMeta>,
        worktree: Option<crate::worktrees::types::WorktreeMeta>,
        parent_tool_call_id: Option<String>,
        parent_subchat_tx: Option<Arc<AMutex<tokio::sync::mpsc::UnboundedSender<serde_json::Value>>>>,
        abort_flag: Option<Arc<AtomicBool>>,
        subchat_depth: usize,
    ) -> Result<SubchatConfig, String> {
        crate::subchat::resolve_subchat_config_with_parent(
            self.gcx.clone(),
            tool_name,
            stateful,
            chat_id,
            title,
            parent_id,
            link_type,
            root_chat_id,
            tools,
            max_steps,
            prepend_system_prompt,
            wrap_up,
            mode,
            task_meta,
            worktree,
            parent_tool_call_id,
            parent_subchat_tx,
            abort_flag,
            subchat_depth,
        )
        .await
    }

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
        task_meta: Option<crate::chat::types::TaskMeta>,
        worktree: Option<crate::worktrees::types::WorktreeMeta>,
        parent_tool_call_id: Option<String>,
        parent_subchat_tx: Option<Arc<AMutex<tokio::sync::mpsc::UnboundedSender<serde_json::Value>>>>,
        abort_flag: Option<Arc<AtomicBool>>,
        subchat_depth: usize,
    ) -> Result<SubchatConfig, String> {
        crate::subchat::resolve_subchat_config_with_explicit_params(
            self.gcx.clone(),
            attribution_id,
            spec,
            stateful,
            chat_id,
            title,
            parent_id,
            link_type,
            root_chat_id,
            tools,
            max_steps,
            prepend_system_prompt,
            mode,
            task_meta,
            worktree,
            parent_tool_call_id,
            parent_subchat_tx,
            abort_flag,
            subchat_depth,
        )
        .await
    }

    async fn run_subchat_once_with_parent(
        &self,
        tool_name: &str,
        messages: Vec<ChatMessage>,
        parent_tool_call_id: String,
        parent_subchat_tx: Arc<AMutex<tokio::sync::mpsc::UnboundedSender<serde_json::Value>>>,
        parent_abort_flag: Arc<AtomicBool>,
        parent_depth: usize,
        parent_task_meta: Option<crate::chat::types::TaskMeta>,
        parent_worktree: Option<crate::worktrees::types::WorktreeMeta>,
        trace_parent: TraceParent,
    ) -> Result<SubchatResult, String> {
        crate::subchat::run_subchat_once_with_parent(
            self.gcx.clone(),
            tool_name,
            messages,
            parent_tool_call_id,
            parent_subchat_tx,
            parent_abort_flag,
            parent_depth,
            parent_task_meta,
            parent_worktree,
            trace_parent,
        )
        .await
    }

    async fn run_subchat_once_with_explicit_params(
        &self,
        attribution_id: &str,
        spec: &ExplicitSubchatSpec,
        messages: Vec<ChatMessage>,
        parent_tool_call_id: String,
        parent_subchat_tx: Arc<AMutex<tokio::sync::mpsc::UnboundedSender<serde_json::Value>>>,
        parent_abort_flag: Arc<AtomicBool>,
        parent_depth: usize,
        parent_task_meta: Option<crate::chat::types::TaskMeta>,
        parent_worktree: Option<crate::worktrees::types::WorktreeMeta>,
        trace_parent: TraceParent,
    ) -> Result<SubchatResult, String> {
        crate::subchat::run_subchat_once_with_explicit_params(
            self.gcx.clone(),
            attribution_id,
            spec,
            messages,
            parent_tool_call_id,
            parent_subchat_tx,
            parent_abort_flag,
            parent_depth,
            parent_task_meta,
            parent_worktree,
            trace_parent,
        )
        .await
    }
}

impl SubchatLauncherHandle for EngineSubchatLauncher {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// Engine side of the `BackgroundAgentSpawner` seam: the orchestration keeps using the
/// engine `subchat` machinery, and lower crates reach it through `ChatCtx::spawn`.
pub struct EngineBackgroundAgentSpawner;

#[async_trait]
impl refact_runtime_api::spawn::BackgroundAgentSpawner for EngineBackgroundAgentSpawner {
    async fn spawn_background_agent(
        &self,
        app: AppState,
        request: refact_agents_core::spawn::SpawnRequest,
    ) -> Result<refact_agents_core::spawn::SpawnHandle, String> {
        crate::agents::spawn::spawn_background_agent(app, request).await
    }
}

