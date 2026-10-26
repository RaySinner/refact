use serde::{Deserialize, Serialize};
use super::expectation::TabTarget;
use super::step::BrowserStep;

#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum SessionPolicy {
    SharedDefault,
}

impl Default for SessionPolicy {
    fn default() -> Self {
        SessionPolicy::SharedDefault
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum NetworkReportMode {
    None,
    #[default]
    Summary,
    Full,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PageContextMode {
    #[default]
    Snapshot,
    Screenshot,
    Both,
    None,
}

impl PageContextMode {
    pub fn includes_snapshot(self) -> bool {
        matches!(self, Self::Snapshot | Self::Both)
    }

    pub fn includes_screenshot(self) -> bool {
        matches!(self, Self::Screenshot | Self::Both)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrowserActionRequest {
    #[serde(default)]
    pub session: SessionPolicy,
    #[serde(default)]
    pub target: TabTarget,
    #[serde(default)]
    pub attach_screenshot: Option<bool>,
    #[serde(default)]
    pub page_context: Option<PageContextMode>,
    #[serde(default)]
    pub network: NetworkReportMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub block_service_workers: Option<bool>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub continue_on_error: bool,
    pub steps: Vec<BrowserStep>,
}

impl BrowserActionRequest {
    pub fn page_context_mode(&self) -> PageContextMode {
        self.page_context.unwrap_or_default()
    }
}
