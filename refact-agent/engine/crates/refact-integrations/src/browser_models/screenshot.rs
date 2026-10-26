use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use super::helpers::is_false;
use super::locator::BrowserLocator;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum BrowserScreenshotType {
    #[default]
    Png,
    Jpeg,
    Webp,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum BrowserScreenshotScale {
    Css,
    #[default]
    Device,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum BrowserScreenshotAnimations {
    #[default]
    Allow,
    Disabled,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum BrowserScreenshotCaret {
    #[default]
    Hide,
    Initial,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct BrowserScreenshotClip {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct BrowserHttpRequest {
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body_json: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub form: Option<BTreeMap<String, String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_redirects: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fail_on_status: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub full_headers: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct BrowserScreenshotOptions {
    #[serde(default, skip_serializing_if = "is_false")]
    pub full_page: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clip: Option<BrowserScreenshotClip>,
    #[serde(default, rename = "type", skip_serializing_if = "Option::is_none")]
    pub image_type: Option<BrowserScreenshotType>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quality: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scale: Option<BrowserScreenshotScale>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub omit_background: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub animations: Option<BrowserScreenshotAnimations>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caret: Option<BrowserScreenshotCaret>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mask: Vec<BrowserLocator>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mask_color: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub style: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum BrowserComposeMode {
    #[default]
    Grid,
    Separate,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum BrowserElementState {
    #[default]
    Default,
    Hover,
    Focus,
    Active,
}

impl BrowserElementState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Hover => "hover",
            Self::Focus => "focus",
            Self::Active => "active",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct BrowserPdfMargin {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub right: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bottom: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub left: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct BrowserPdfOptions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub landscape: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub print_background: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scale: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub margins: Option<BrowserPdfMargin>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page_ranges: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefer_css_page_size: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tagged: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outline: Option<bool>,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq)]
pub struct BrowserPosition {
    pub x: f64,
    pub y: f64,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BrowserMouseButton {
    #[default]
    Left,
    Middle,
    Right,
}
