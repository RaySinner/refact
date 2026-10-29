use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TerminalSecurityMode {
    Off,
    Audit,
    #[default]
    ApprovalOnly,
    SandboxPreferred,
    SandboxRequired,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct TerminalSecurityConfig {
    #[serde(default)]
    pub mode: TerminalSecurityMode,
    #[serde(default)]
    pub env_passthrough: Vec<String>,
}
