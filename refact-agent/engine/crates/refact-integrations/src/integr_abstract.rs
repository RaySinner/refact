use serde::{Deserialize, Serialize};

pub use refact_tool_api::IntegrationConfirmation;

#[derive(Deserialize, Serialize, Clone, Default)]
pub struct IntegrationAvailable {
    #[serde(default = "default_true")]
    pub on_your_laptop: bool,
    #[serde(default = "default_true")]
    pub when_isolated: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Deserialize, Serialize, Clone, Default)]
pub struct IntegrationCommon {
    #[serde(default)]
    pub available: IntegrationAvailable,
    #[serde(default)]
    pub confirmation: IntegrationConfirmation,
}
