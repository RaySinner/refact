use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ShellLlmAuthority {
    AskOnly,
    AskAndAllow,
}

impl Default for ShellLlmAuthority {
    fn default() -> Self {
        Self::AskOnly
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ShellLlmOnFailure {
    Pass,
    Ask,
}

impl Default for ShellLlmOnFailure {
    fn default() -> Self {
        Self::Pass
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct ShellLlmValidation {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub authority: ShellLlmAuthority,
    #[serde(default = "default_llm_timeout")]
    pub timeout_secs: u64,
    #[serde(default)]
    pub on_failure: ShellLlmOnFailure,
    #[serde(default = "default_true")]
    pub cache_per_chat: bool,
}

impl Default for ShellLlmValidation {
    fn default() -> Self {
        Self {
            enabled: false,
            model: String::new(),
            authority: ShellLlmAuthority::AskOnly,
            timeout_secs: default_llm_timeout(),
            on_failure: ShellLlmOnFailure::Pass,
            cache_per_chat: true,
        }
    }
}

fn default_true() -> bool {
    true
}

fn default_llm_timeout() -> u64 {
    8
}
