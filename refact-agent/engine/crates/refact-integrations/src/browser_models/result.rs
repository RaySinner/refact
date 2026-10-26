use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::browser_types::{ConsoleEntry, NetworkEntry};
use super::har::HarNotFound;
use super::step::{RouteHandler, UrlPattern};
use super::websocket::WebSocketEvent;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum FieldKind {
    TextInput,
    PasswordInput,
    EmailInput,
    SearchInput,
    NumberInput,
    TelInput,
    UrlInput,
    Textarea,
    Select,
    Checkbox,
    Radio,
    ContentEditable,
    DateInput,
    FileInput,
    HiddenInput,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum FillStrategy {
    NativeTyping,
    DomValueSetter,
    NativePrototypeSetter,
    ContentEditablePath,
    ClickAndType,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActionabilityDiagnostics {
    pub call_log: Vec<String>,
    pub timed_out: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub elapsed_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attempts: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attached: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub visible: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stable: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub editable: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub receives_events: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub intercepting_element: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BrowserAssertionResult {
    pub matcher: String,
    pub passed: bool,
    pub soft: bool,
    pub expected: serde_json::Value,
    pub received: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff: Option<String>,
    pub attempts: u32,
    pub elapsed_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepResult {
    pub step_index: usize,
    pub ok: bool,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field_kind: Option<FieldKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fill_strategy: Option<FillStrategy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verified: Option<bool>,
    #[serde(default)]
    pub retries: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actionability: Option<ActionabilityDiagnostics>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assertion: Option<BrowserAssertionResult>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locator_echo: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub skipped: bool,
}

impl StepResult {
    pub fn success(step_index: usize, summary: impl Into<String>) -> Self {
        Self {
            step_index,
            ok: true,
            summary: summary.into(),
            error: None,
            data: None,
            field_kind: None,
            fill_strategy: None,
            verified: None,
            retries: 0,
            actionability: None,
            assertion: None,
            locator_echo: None,
            skipped: false,
        }
    }

    pub fn failure(
        step_index: usize,
        summary: impl Into<String>,
        error: impl Into<String>,
    ) -> Self {
        Self {
            step_index,
            ok: false,
            summary: summary.into(),
            error: Some(error.into()),
            data: None,
            field_kind: None,
            fill_strategy: None,
            verified: None,
            retries: 0,
            actionability: None,
            assertion: None,
            locator_echo: None,
            skipped: false,
        }
    }

    pub fn skipped(step_index: usize, failed_step_index: usize) -> Self {
        let mut result = Self::success(
            step_index,
            format!("Skipped: step {failed_step_index} failed earlier in this batch"),
        );
        result.ok = false;
        result.skipped = true;
        result
    }

    pub fn was_executed(&self) -> bool {
        !self.skipped
    }

    pub fn with_data(mut self, data: serde_json::Value) -> Self {
        self.data = Some(data);
        self
    }

    pub fn with_locator_echo(mut self, locator_echo: Option<String>) -> Self {
        self.locator_echo = locator_echo;
        self
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct BrowserConsoleCounts {
    #[serde(default)]
    pub errors: usize,
    #[serde(default)]
    pub warnings: usize,
}

impl BrowserConsoleCounts {
    pub fn is_empty(&self) -> bool {
        self.errors == 0 && self.warnings == 0
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BrowserSnapshotArtifact {
    pub kind: String,
    pub mime: String,
    pub path: std::path::PathBuf,
    pub bytes: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BrowserPageSnapshot {
    pub yaml: String,
    pub lines: usize,
    pub bytes: usize,
    #[serde(default)]
    pub truncated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact: Option<BrowserSnapshotArtifact>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub frames: Vec<BrowserFrameSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BrowserFrameSummary {
    pub frame_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    pub depth: usize,
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct BrowserPageContext {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    #[serde(default, skip_serializing_if = "BrowserConsoleCounts::is_empty")]
    pub console: BrowserConsoleCounts,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<BrowserPageSnapshot>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ExecutionReport {
    pub ok: bool,
    pub steps: Vec<StepResult>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page: Option<BrowserPageContext>,
    #[serde(default)]
    pub stabilized: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub console: Vec<ConsoleEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub page_errors: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub network: Vec<NetworkEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub network_summary: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub websockets: Vec<WebSocketEvent>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub locator_handlers: Vec<LocatorHandlerFiring>,
    pub dialogs: Vec<DialogInfo>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub uploads: Vec<UploadInfo>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub downloads: Vec<DownloadInfo>,
    pub new_tabs: Vec<TabInfo>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub active_routes: Vec<RouteInfo>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub intercepted_requests: Vec<RouteInterception>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<BrowserContextSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub screenshot: Option<BrowserScreenshot>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct BrowserContextSummary {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub viewport: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locale: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timezone: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color_scheme: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub permissions: Vec<String>,
    #[serde(default)]
    pub cookie_count: usize,
    #[serde(default)]
    pub local_storage_count: usize,
    #[serde(default)]
    pub session_storage_count: usize,
    #[serde(default)]
    pub offline: bool,
    #[serde(default)]
    pub http_credentials: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BrowserStorageKind {
    Local,
    Session,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BrowserStorageItem {
    pub name: String,
    pub value: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum BrowserCookieSameSite {
    Strict,
    Lax,
    None,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BrowserCookie {
    pub name: String,
    pub value: String,
    #[serde(default)]
    pub domain: String,
    #[serde(default = "default_cookie_path")]
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires: Option<f64>,
    #[serde(default)]
    pub http_only: bool,
    #[serde(default)]
    pub secure: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub same_site: Option<BrowserCookieSameSite>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

fn default_cookie_path() -> String {
    "/".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BrowserStorageOrigin {
    pub origin: String,
    #[serde(default)]
    pub local_storage: Vec<BrowserStorageItem>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub indexed_db: Vec<BrowserIndexedDbDatabase>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BrowserIndexedDbDatabase {
    pub name: String,
    pub version: f64,
    #[serde(default)]
    pub stores: Vec<BrowserIndexedDbStore>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BrowserIndexedDbStore {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_path: Option<serde_json::Value>,
    #[serde(default)]
    pub auto_increment: bool,
    #[serde(default)]
    pub records: Vec<BrowserIndexedDbRecord>,
    #[serde(default)]
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BrowserIndexedDbRecord {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<serde_json::Value>,
    pub value: serde_json::Value,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BrowserPermissionState {
    #[default]
    Granted,
    Denied,
    Prompt,
}

impl BrowserPermissionState {
    pub fn is_granted(&self) -> bool {
        matches!(self, BrowserPermissionState::Granted)
    }

    pub fn label(&self) -> &'static str {
        match self {
            BrowserPermissionState::Granted => "granted",
            BrowserPermissionState::Denied => "denied",
            BrowserPermissionState::Prompt => "prompt",
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct BrowserStorageState {
    #[serde(default)]
    pub cookies: Vec<BrowserCookie>,
    #[serde(default)]
    pub origins: Vec<BrowserStorageOrigin>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RouteInfo {
    pub pattern: UrlPattern,
    pub handler: RouteHandler,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub har: Option<HarRouteInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub times_remaining: Option<u32>,
    #[serde(default)]
    pub order: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HarRouteInfo {
    pub entry_count: usize,
    pub not_found: HarNotFound,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RouteInterception {
    pub url: String,
    pub method: String,
    pub pattern: UrlPattern,
    pub action: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub request_headers: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_body_preview: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_body_preview: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default)]
    pub redirect_hop: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DownloadState {
    InProgress,
    Completed,
    Canceled,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DownloadInfo {
    pub guid: String,
    pub url: String,
    pub frame_id: String,
    pub suggested_filename: String,
    pub local_path: String,
    pub received_bytes: u64,
    pub total_bytes: u64,
    pub state: DownloadState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UploadInfo {
    pub paths: Vec<String>,
    pub source: String,
    pub in_memory_payloads: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LocatorHandlerFiring {
    pub name: String,
    pub action: String,
    pub outcome: String,
    pub ok: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DialogType {
    Alert,
    Confirm,
    Prompt,
    Beforeunload,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DialogAction {
    Accepted,
    Dismissed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DialogInfo {
    #[serde(rename = "type")]
    pub dialog_type: DialogType,
    pub message: String,
    pub default_value: String,
    pub action: DialogAction,
    pub automatic: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrowserScreenshot {
    pub mime: String,
    pub data: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ElementInfo {
    pub tag: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub placeholder: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aria_label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    pub visible: bool,
    pub enabled: bool,
    pub readonly: bool,
    pub content_editable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inner_text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bbox: Option<ElementBBox>,
    pub field_kind: FieldKind,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ElementBBox {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TabOpener {
    pub tab_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TabInfo {
    pub id: String,
    pub target_id: String,
    pub url: String,
    pub title: String,
    pub active: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opener: Option<TabOpener>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opened_by_step: Option<usize>,
}
