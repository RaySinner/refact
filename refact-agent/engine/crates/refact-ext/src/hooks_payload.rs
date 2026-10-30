use std::collections::HashMap;
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct HookPayload {
    pub hook_event_name: String,
    pub session_id: String,
    pub project_dir: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_input: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_output: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_prompt: Option<String>,
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

#[derive(Debug)]
#[allow(dead_code)]
pub enum HookResult {
    Success(String),
    Block(String),
    Warning(String),
    Timeout,
}
