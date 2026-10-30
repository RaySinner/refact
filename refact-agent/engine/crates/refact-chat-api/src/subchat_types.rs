use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::Arc;

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex as AMutex, mpsc};

use refact_core::chat_types::{
    CacheControl, ChatMessage, ChatModelType, PostprocessSettings, ReasoningEffort,
};
use refact_core::worktree_meta::WorktreeMeta;

use crate::tool_catalog::{ToolCatalogSnapshot, TurnToolPool};
use crate::types::TaskMeta;

#[derive(Clone, Debug)]
pub enum ToolsPolicy {
    All,
    None,
    Only(Vec<String>),
}

impl ToolsPolicy {
    pub fn from_option(opt: Option<Vec<String>>) -> Self {
        match opt {
            None => ToolsPolicy::All,
            Some(v) if v.is_empty() => ToolsPolicy::None,
            Some(v) => ToolsPolicy::Only(v),
        }
    }

    pub fn to_subset_for_llm(&self) -> Option<Vec<String>> {
        match self {
            ToolsPolicy::All => None,
            ToolsPolicy::None => Some(vec![]),
            ToolsPolicy::Only(v) => Some(v.clone()),
        }
    }

    pub fn allows_tool(&self, tool_name: &str) -> bool {
        match self {
            ToolsPolicy::All => true,
            ToolsPolicy::None => false,
            ToolsPolicy::Only(v) => v.iter().any(|t| t == tool_name),
        }
    }
}

#[derive(Clone)]
pub struct WrapUpConfig {
    pub depth: usize,
    pub tokens_cnt: usize,
    pub prompt: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct TraceParent {
    pub chat_id: Option<String>,
    pub root_chat_id: Option<String>,
}

impl TraceParent {
    pub fn unattributed() -> Self {
        Self::default()
    }

    pub fn from_parts(chat_id: Option<&str>, root_chat_id: Option<&str>) -> Self {
        let normalize = |value: Option<&str>| {
            value
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
        };
        Self {
            chat_id: normalize(chat_id),
            root_chat_id: normalize(root_chat_id),
        }
    }

    pub fn chat(chat_id: &str) -> Self {
        Self::from_parts(Some(chat_id), None)
    }

    pub fn rooted(chat_id: &str, root_chat_id: &str) -> Self {
        Self::from_parts(Some(chat_id), Some(root_chat_id))
    }

    pub fn trace_folder_owner(&self) -> Option<String> {
        self.root_chat_id.clone().or_else(|| self.chat_id.clone())
    }
}

fn default_subchat_cache_control() -> CacheControl {
    CacheControl::Ephemeral
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct SubchatParameters {
    #[serde(default)]
    pub subchat_model_type: ChatModelType,
    #[serde(default)]
    pub subchat_model: String,
    pub subchat_n_ctx: usize,
    pub subchat_max_new_tokens: usize,
    pub subchat_temperature: Option<f32>,
    pub subchat_tokens_for_rag: usize,
    pub subchat_reasoning_effort: Option<ReasoningEffort>,
    #[serde(default = "default_subchat_cache_control")]
    pub subchat_cache_control: CacheControl,
}

fn default_mode_id() -> String {
    "agent".to_string()
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ChatMeta {
    #[serde(default)]
    pub chat_id: String,
    #[serde(default)]
    pub request_attempt_id: String,
    #[serde(default)]
    pub chat_remote: bool,
    #[serde(default = "default_mode_id")]
    pub chat_mode: String,
    #[serde(default)]
    pub current_config_file: String,
    #[serde(default = "default_true")]
    pub include_project_info: bool,
    #[serde(default)]
    pub context_tokens_cap: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<WorktreeMeta>,
}

impl Default for ChatMeta {
    fn default() -> Self {
        ChatMeta {
            chat_id: String::new(),
            request_attempt_id: String::new(),
            chat_remote: false,
            chat_mode: default_mode_id(),
            current_config_file: String::new(),
            include_project_info: true,
            context_tokens_cap: None,
            worktree: None,
        }
    }
}

#[derive(Clone)]
pub struct SubchatConfig {
    pub tool_name: String,
    pub stateful: bool,
    pub autonomous_no_confirm: bool,
    pub auto_approve_editing_tools: bool,
    pub auto_approve_dangerous_commands: bool,
    pub chat_id: Option<String>,
    pub title: Option<String>,
    pub parent_id: Option<String>,
    pub link_type: Option<String>,
    pub root_chat_id: Option<String>,
    pub tools: ToolsPolicy,
    pub max_steps: usize,
    pub prepend_system_prompt: bool,
    pub wrap_up: Option<WrapUpConfig>,
    pub task_meta: Option<TaskMeta>,
    pub worktree: Option<WorktreeMeta>,
    pub model: String,
    pub mode: String,
    pub n_ctx: usize,
    pub max_new_tokens: usize,
    pub temperature: Option<f32>,
    pub reasoning_effort: Option<ReasoningEffort>,
    pub cache_control: CacheControl,
    pub parent_tool_call_id: Option<String>,
    pub parent_subchat_tx: Option<Arc<AMutex<mpsc::UnboundedSender<serde_json::Value>>>>,
    pub abort_flag: Option<Arc<AtomicBool>>,
    pub soft_abort: bool,
    pub activity_stamp: Option<Arc<AtomicU64>>,
    pub background_agent_id: Option<String>,
    pub subchat_depth: usize,
    pub final_step_force_answer: bool,
    pub buddy_meta: Option<refact_core::buddy_meta::BuddyThreadMeta>,
    pub step_progress: Option<Arc<dyn Fn(SubchatProgress) + Send + Sync>>,
    pub trace_parent: TraceParent,
}

#[derive(Clone, Debug, PartialEq)]
pub enum SubchatProgress {
    Step(usize),
    ToolStarted {
        name: String,
        arg_preview: Option<String>,
    },
    ToolsFinished,
    Usage {
        tokens_delta: u64,
        cost_delta: Option<f64>,
    },
}

pub struct SubchatResult {
    pub messages: Vec<ChatMessage>,
    pub metering: serde_json::Map<String, serde_json::Value>,
    #[allow(dead_code)]
    pub chat_id: Option<String>,
    pub aborted: bool,
}

#[derive(Debug, Clone)]
pub struct ExplicitSubchatSpec {
    pub params: SubchatParameters,
    pub model: String,
    pub autonomous_no_confirm: bool,
}

#[derive(Default)]
pub struct ExecuteToolsOptions {
    pub subchat_tool_parameters: Option<IndexMap<String, SubchatParameters>>,
    pub postprocess_settings: Option<PostprocessSettings>,
    pub allowed_tools: Vec<String>,
    pub catalog: Option<Arc<ToolCatalogSnapshot>>,
    pub turn_tool_pool: Option<TurnToolPool>,
}
