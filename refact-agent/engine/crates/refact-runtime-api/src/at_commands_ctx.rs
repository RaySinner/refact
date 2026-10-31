use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::Arc;

use async_trait::async_trait;
use indexmap::IndexMap;
use refact_chat_api::{ChatMessage, SubchatParameters, TaskMeta};
use refact_core::chat_types::{ContextEnum, PostprocessSettings};
use refact_core::worktree_meta::WorktreeMeta;
use refact_privacy::DerivedPrivacyZones;
use refact_worktrees::scope::ExecutionScope;
use tokio::sync::{mpsc, Mutex as AMutex};

use crate::tool_ctx::ToolCtx;
use crate::AppState;

pub const MAX_SUBCHAT_DEPTH: usize = 5;

#[derive(Debug, Clone, Default)]
pub struct AtCommandMember {
    pub kind: String,
    pub text: String,
    pub pos1: usize,
    pub pos2: usize,
    pub ok: bool,
    pub reason: Option<String>,
}

impl AtCommandMember {
    pub fn new(kind: String, text: String, pos1: usize, pos2: usize) -> Self {
        Self {
            kind,
            text,
            pos1,
            pos2,
            ok: true,
            reason: None,
        }
    }
}

#[async_trait]
pub trait AtCommand: Send + Sync {
    fn params(&self) -> &Vec<Box<dyn AtParam>>;
    // returns (messages_for_postprocessing, text_on_clip)
    async fn at_execute(
        &self,
        ccx: Arc<AMutex<AtCommandsContext>>,
        cmd: &mut AtCommandMember,
        args: &mut Vec<AtCommandMember>,
    ) -> Result<(Vec<ContextEnum>, String), String>;
    fn depends_on(&self) -> Vec<String> {
        vec![]
    } // "ast", "vecdb"
}

#[async_trait]
pub trait AtParam: Send + Sync {
    async fn is_value_valid(&self, ccx: Arc<AMutex<AtCommandsContext>>, value: &String) -> bool;
    async fn param_completion(
        &self,
        ccx: Arc<AMutex<AtCommandsContext>>,
        value: &String,
    ) -> Vec<String>;
    fn param_completion_valid(&self) -> bool {
        false
    }
}

pub struct AtCommandsContext {
    pub app: AppState,
    pub tool: ToolCtx,
    pub n_ctx: usize,
    pub top_n: usize,
    pub tokens_for_rag: usize,
    pub messages: Vec<ChatMessage>,
    #[allow(dead_code)]
    pub is_preview: bool,
    pub pp_skeleton: bool,
    #[allow(dead_code)]
    pub correction_only_up_to_step: usize,
    pub chat_id: String,
    pub root_chat_id: String,
    pub current_model: String,
    pub task_meta: Option<TaskMeta>,
    pub execution_scope: Option<ExecutionScope>,
    pub derived_privacy_zones: DerivedPrivacyZones,
    pub subchat_depth: usize,
    pub tool_access_bypass: bool,
    pub background_agent_id: Option<String>,

    pub at_commands: HashMap<String, Arc<dyn AtCommand + Send>>,
    pub subchat_tool_parameters: IndexMap<String, SubchatParameters>,
    pub postprocess_parameters: PostprocessSettings,

    pub subchat_tx: Arc<AMutex<mpsc::UnboundedSender<serde_json::Value>>>,
    pub subchat_rx: Arc<AMutex<mpsc::UnboundedReceiver<serde_json::Value>>>,
    pub abort_flag: Arc<AtomicBool>,
    pub activity_stamp: Option<Arc<AtomicU64>>,
}

impl AtCommandsContext {
    pub fn execution_scope_root(&self) -> Option<std::path::PathBuf> {
        self.execution_scope
            .as_ref()
            .map(|scope| scope.effective_root().to_path_buf())
    }

    pub fn effective_project_dirs(&self) -> Vec<std::path::PathBuf> {
        self.execution_scope
            .as_ref()
            .map(|scope| scope.effective_project_dirs())
            .unwrap_or_default()
    }

    pub fn execution_scope_worktree(&self) -> Option<WorktreeMeta> {
        self.execution_scope
            .as_ref()
            .map(|scope| scope.worktree().clone())
    }
}
