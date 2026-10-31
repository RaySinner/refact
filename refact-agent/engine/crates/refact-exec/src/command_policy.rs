use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

pub type ChatMode = String;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExecSource {
    ShellTool,
    ProcessTool,
    CmdlineIntegration,
    SchedulerJob,
    Verifier,
}

impl ExecSource {
    pub fn audit_source(self) -> &'static str {
        match self {
            Self::ShellTool => "shell_tool",
            Self::ProcessTool => "process_tool",
            Self::CmdlineIntegration => "cmdline_integration",
            Self::SchedulerJob => "scheduler_job",
            Self::Verifier => "verifier",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExecEscalationMode {
    WorkspaceWrite,
    FullAccess,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct ExecEscalation {
    pub mode: ExecEscalationMode,
    pub justification: String,
}

impl ExecEscalation {
    pub fn validate(self) -> Result<Self, String> {
        if self.justification.trim().is_empty() {
            return Err("escalate.justification must be a non-empty string".to_string());
        }
        Ok(Self {
            justification: self.justification.trim().to_string(),
            ..self
        })
    }

    pub fn mode_name(&self) -> &'static str {
        match self.mode {
            ExecEscalationMode::WorkspaceWrite => "workspace_write",
            ExecEscalationMode::FullAccess => "full_access",
        }
    }
}

pub enum CommandKind<'a> {
    Shell(&'a str),
    Argv(&'a [String]),
}

pub struct CommandPolicyInput<'a> {
    pub source: ExecSource,
    pub command: CommandKind<'a>,
    pub cwd: Option<PathBuf>,
    pub env: HashMap<String, String>,
    pub chat_mode: Option<ChatMode>,
    pub escalation: Option<ExecEscalation>,
}

pub fn escalation_from_args(
    args: &HashMap<String, serde_json::Value>,
) -> Result<Option<ExecEscalation>, String> {
    let Some(value) = args.get("escalate") else {
        return Ok(None);
    };
    serde_json::from_value::<ExecEscalation>(value.clone())
        .map_err(|error| format!("invalid escalate argument: {error}"))?
        .validate()
        .map(Some)
}
