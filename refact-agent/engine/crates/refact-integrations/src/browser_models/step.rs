use std::collections::BTreeMap;

use serde::{Deserialize, Deserializer, Serialize};
use super::expectation::{
    AccessibilitySnapshotOptions, BrowserExpectation, BrowserPollMatcher, TabTarget,
};
use super::har::{HarContentPolicy, HarMode, HarNotFound};
use super::helpers::is_false;
use super::locator::BrowserLocator;
use super::result::{
    BrowserCookie, BrowserPermissionState, BrowserStorageItem, BrowserStorageKind,
    BrowserStorageState,
};
use super::screenshot::{
    BrowserComposeMode, BrowserElementState, BrowserHttpRequest, BrowserMouseButton,
    BrowserPdfOptions, BrowserPosition, BrowserScreenshotOptions,
};
use super::webauthn::{
    BrowserAuthenticatorProtocol, BrowserAuthenticatorTransport, BrowserWebAuthnCredential,
};
use super::websocket::{WebSocketMessageAction, WebSocketRouteMode};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum BrowserStep {
    Navigate {
        url: String,
        #[serde(default)]
        timeout_ms: Option<u64>,
    },
    Reload,
    GoBack,
    GoForward,

    OpenTab {
        #[serde(default)]
        device: Option<String>,
        #[serde(default)]
        url: Option<String>,
    },
    CloseTab {
        #[serde(default)]
        tab: Option<TabTarget>,
    },
    SwitchTab {
        tab: TabTarget,
    },
    ListTabs,
    WaitForPopup {
        #[serde(default)]
        timeout_ms: Option<u64>,
    },

    Route {
        pattern: UrlPattern,
        handler: RouteHandler,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        times: Option<u32>,
    },
    Unroute {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pattern: Option<UrlPattern>,
    },
    ListRoutes,

    RouteWebSocket {
        pattern: UrlPattern,
        #[serde(default)]
        mode: WebSocketRouteMode,
        #[serde(default)]
        on_page_message: WebSocketMessageAction,
        #[serde(default)]
        on_server_message: WebSocketMessageAction,
    },
    UnrouteWebSocket {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pattern: Option<UrlPattern>,
    },
    SendWebSocketMessage {
        #[serde(alias = "url_pattern")]
        pattern: UrlPattern,
        #[serde(alias = "data")]
        text: String,
    },
    CloseWebSocket {
        #[serde(alias = "url_pattern")]
        pattern: UrlPattern,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        code: Option<u16>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    WaitForWebSocketFrame {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pattern: Option<UrlPattern>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout_ms: Option<u64>,
    },

    StartHarRecording {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<String>,
        #[serde(default)]
        mode: HarMode,
        #[serde(default)]
        content: HarContentPolicy,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        url_filter: Option<UrlPattern>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        update: Option<String>,
    },
    StopHarRecording,
    RouteFromHar {
        path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        url_filter: Option<UrlPattern>,
        not_found: HarNotFound,
    },
    Reset,
    CdpSend {
        method: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        params: Option<serde_json::Value>,
        #[serde(default)]
        target: CdpTarget,
    },
    HttpRequest {
        #[serde(flatten)]
        options: BrowserHttpRequest,
    },

    ClockInstall {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        time: Option<ClockTime>,
    },
    ClockFastForward {
        ticks: ClockTicks,
    },
    ClockPauseAt {
        time: ClockTime,
    },
    ClockResume,
    ClockRunFor {
        ticks: ClockTicks,
    },
    ClockSetFixedTime {
        time: ClockTime,
    },
    ClockSetSystemTime {
        time: ClockTime,
    },

    StartCoverage {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        js: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        css: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reset_on_navigation: Option<bool>,
    },
    StopCoverage,

    CaptureFrames {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        duration_ms: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        frame_count: Option<usize>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        interval_ms: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        locator: Option<BrowserLocator>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        full_page: Option<bool>,
    },
    ScreencastStart {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        quality: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_width: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_height: Option<u32>,
    },
    ScreencastStop {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        compose: Option<bool>,
    },

    AddVirtualAuthenticator {
        #[serde(
            default,
            skip_serializing,
            deserialize_with = "reject_server_minted_authenticator_id"
        )]
        id: (),
        #[serde(default, skip_serializing_if = "Option::is_none")]
        protocol: Option<BrowserAuthenticatorProtocol>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        transport: Option<BrowserAuthenticatorTransport>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        has_resident_key: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        has_user_verification: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        is_user_verified: Option<bool>,
    },
    RemoveVirtualAuthenticator {
        id: String,
    },
    ListCredentials {
        id: String,
    },
    AddCredential {
        id: String,
        credential: BrowserWebAuthnCredential,
    },
    ClearCredentials {
        id: String,
    },
    SetUserVerified {
        id: String,
        verified: bool,
    },

    SetViewport {
        width: u32,
        height: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        device_scale_factor: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        is_mobile: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        has_touch: Option<bool>,
    },
    SetWindowBounds {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        x: Option<i32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        y: Option<i32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        width: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        height: Option<u32>,
    },
    EmulateMedia {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        color_scheme: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reduced_motion: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        forced_colors: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        contrast: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        media: Option<String>,
    },
    SetLocale {
        locale: String,
    },
    SetTimezone {
        timezone: String,
    },
    SetUserAgent {
        user_agent: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        accept_language: Option<String>,
    },
    SetGeolocation {
        latitude: f64,
        longitude: f64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        accuracy: Option<f64>,
    },
    SetOffline {
        offline: bool,
    },
    SetNetworkConditions {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        offline: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        latency_ms: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        download_kbps: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        upload_kbps: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        preset: Option<String>,
    },
    SetCpuThrottling {
        rate: f64,
    },
    EmulateDevice {
        name: String,
    },
    ListDevices {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        filter: Option<String>,
    },
    SetExtraHttpHeaders {
        headers: BTreeMap<String, String>,
    },
    GetCookies {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        urls: Option<Vec<String>>,
    },
    SetCookies {
        cookies: Vec<BrowserCookie>,
    },
    ClearCookies {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        domain: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<String>,
    },
    GetStorage {
        kind: BrowserStorageKind,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        origin: Option<String>,
    },
    SetStorage {
        kind: BrowserStorageKind,
        items: Vec<BrowserStorageItem>,
    },
    ClearStorage {
        kind: BrowserStorageKind,
    },
    StorageState {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        save_as: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        indexed_db: Option<bool>,
    },
    SetStorageState {
        state: BrowserStorageState,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        indexed_db: Option<bool>,
    },
    GrantPermissions {
        permissions: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        origin: Option<String>,
        #[serde(default, skip_serializing_if = "BrowserPermissionState::is_granted")]
        state: BrowserPermissionState,
    },
    ClearPermissions,
    SetHttpCredentials {
        username: String,
        password: String,
    },

    Expect {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        locator: Option<BrowserLocator>,
        matcher: BrowserExpectation,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout_ms: Option<u64>,
        #[serde(default)]
        soft: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        not: Option<bool>,
    },
    ExpectPoll {
        expression: String,
        expected: serde_json::Value,
        matcher: BrowserPollMatcher,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout_ms: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        soft: Option<bool>,
    },

    Click {
        locator: BrowserLocator,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout_ms: Option<u64>,
    },
    ClickIfExists {
        locator: BrowserLocator,
    },
    Tap {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        locator: Option<BrowserLocator>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        x: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        y: Option<f64>,
    },
    Hover {
        locator: BrowserLocator,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout_ms: Option<u64>,
    },
    Focus {
        locator: BrowserLocator,
    },
    Blur {
        locator: BrowserLocator,
    },
    ScrollTo {
        locator: BrowserLocator,
    },
    PressKey {
        key: String,
        #[serde(default)]
        modifiers: Vec<String>,
    },
    InsertText {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        locator: Option<BrowserLocator>,
        text: String,
    },
    PressSequentially {
        locator: BrowserLocator,
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        delay_ms: Option<u64>,
    },
    DragAndDrop {
        source: BrowserLocator,
        target: BrowserLocator,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source_position: Option<BrowserPosition>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target_position: Option<BrowserPosition>,
    },
    DropFiles {
        target: BrowserLocator,
        paths: Vec<String>,
    },
    MouseMove {
        x: f64,
        y: f64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        steps: Option<usize>,
    },
    MouseDown {
        #[serde(default)]
        button: BrowserMouseButton,
    },
    MouseUp {
        #[serde(default)]
        button: BrowserMouseButton,
    },
    MouseClickXy {
        x: f64,
        y: f64,
        #[serde(default)]
        button: BrowserMouseButton,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        click_count: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        delay: Option<u64>,
    },
    MouseDragXy {
        start_x: f64,
        start_y: f64,
        end_x: f64,
        end_y: f64,
    },
    MouseWheel {
        delta_x: f64,
        delta_y: f64,
    },

    Fill {
        locator: BrowserLocator,
        text: String,
        #[serde(default = "default_true")]
        clear_first: bool,
        #[serde(default = "default_true")]
        verify: bool,
    },
    Clear {
        locator: BrowserLocator,
        #[serde(default = "default_true")]
        verify: bool,
    },
    SelectOption {
        locator: BrowserLocator,
        value: String,
    },
    Check {
        locator: BrowserLocator,
    },
    Uncheck {
        locator: BrowserLocator,
    },
    SetInputFiles {
        locator: BrowserLocator,
        paths: Vec<String>,
    },
    ExpectFileChooser {
        paths: Vec<String>,
    },

    WaitForFunction {
        expression: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        locator: Option<BrowserLocator>,
        #[serde(default)]
        timeout_ms: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        polling_ms: Option<u64>,
    },
    WaitForSelector {
        locator: BrowserLocator,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        state: Option<BrowserWaitState>,
        #[serde(default)]
        timeout_ms: Option<u64>,
    },
    WaitForNavigation {
        #[serde(default)]
        timeout_ms: Option<u64>,
    },
    WaitForUrl {
        #[serde(alias = "contains")]
        pattern: UrlPattern,
        #[serde(default)]
        timeout_ms: Option<u64>,
    },
    WaitForText {
        text: String,
        #[serde(default)]
        timeout_ms: Option<u64>,
    },
    WaitForNetworkIdle {
        #[serde(default)]
        timeout_ms: Option<u64>,
    },
    WaitForLoadState {
        state: BrowserLoadState,
        #[serde(default)]
        timeout_ms: Option<u64>,
    },
    WaitForRequest {
        #[serde(alias = "url_or_pattern")]
        pattern: UrlPattern,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        method: Option<String>,
        #[serde(default)]
        timeout_ms: Option<u64>,
    },
    WaitForResponse {
        #[serde(alias = "url_or_pattern")]
        pattern: UrlPattern,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        method: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        status: Option<u16>,
        #[serde(default)]
        timeout_ms: Option<u64>,
    },
    WaitForConsoleMessage {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        contains: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        level: Option<BrowserConsoleLevel>,
        #[serde(default)]
        timeout_ms: Option<u64>,
    },
    WaitForDownload {
        #[serde(default)]
        timeout_ms: Option<u64>,
        #[serde(default)]
        save_as: Option<String>,
    },
    CancelDownload {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
    WaitForElementHidden {
        locator: BrowserLocator,
        #[serde(default)]
        timeout_ms: Option<u64>,
    },
    WaitForElementStable {
        locator: BrowserLocator,
        #[serde(default)]
        timeout_ms: Option<u64>,
    },
    WaitSeconds {
        seconds: f64,
    },

    GetText {
        locator: BrowserLocator,
    },
    GetHtml {
        locator: BrowserLocator,
    },
    GetAttribute {
        locator: BrowserLocator,
        attribute: String,
    },
    BoundingBox {
        locator: BrowserLocator,
    },
    Count {
        locator: BrowserLocator,
    },
    InputValue {
        locator: BrowserLocator,
    },
    AllTexts {
        locator: BrowserLocator,
        #[serde(default)]
        mode: BrowserTextMode,
        #[serde(default)]
        limit: Option<usize>,
    },
    ElementState {
        locator: BrowserLocator,
    },
    ExtractLinks {
        #[serde(default)]
        locator: Option<BrowserLocator>,
        #[serde(default)]
        limit: Option<usize>,
    },
    ExtractTable {
        locator: BrowserLocator,
        #[serde(default)]
        limit: Option<usize>,
    },
    DomSnapshot {
        selector: String,
        #[serde(default)]
        max_chars: Option<usize>,
    },
    AccessibilitySnapshot {
        #[serde(flatten)]
        options: AccessibilitySnapshotOptions,
    },
    Screenshot {
        #[serde(flatten)]
        options: BrowserScreenshotOptions,
    },
    ScreenshotElement {
        locator: BrowserLocator,
        #[serde(flatten)]
        options: BrowserScreenshotOptions,
    },
    ScreenshotElements {
        locators: Vec<BrowserLocator>,
        #[serde(default)]
        compose: BrowserComposeMode,
        #[serde(default)]
        labels: Option<bool>,
        #[serde(flatten)]
        options: BrowserScreenshotOptions,
    },
    CaptureElementStates {
        locator: BrowserLocator,
        #[serde(default)]
        states: Vec<BrowserElementState>,
        #[serde(default)]
        labels: Option<bool>,
        #[serde(flatten)]
        options: BrowserScreenshotOptions,
    },
    Pdf {
        #[serde(flatten)]
        options: BrowserPdfOptions,
    },

    Eval {
        expression: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout_ms: Option<u64>,
    },
    Styles {
        locator: BrowserLocator,
        #[serde(default)]
        property_filter: Option<String>,
    },

    SetContent {
        html: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        wait_until: Option<BrowserLoadState>,
    },
    PageContent,
    AddScriptTag {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        url: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        content: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        script_type: Option<String>,
    },
    AddStyleTag {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        url: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        content: Option<String>,
    },
    AddInitScript {
        content: String,
    },
    RemoveInitScript {
        id: String,
    },
    DispatchEvent {
        locator: BrowserLocator,
        event_type: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        event_init: Option<serde_json::Value>,
    },

    TabLog,

    AddLocatorHandler {
        name: String,
        locator: BrowserLocator,
        handler: LocatorHandlerAction,
        #[serde(default)]
        times: Option<u32>,
        #[serde(default)]
        no_wait_after: bool,
    },
    RemoveLocatorHandler {
        name: String,
    },
    HandleDialog {
        accept: bool,
        #[serde(default)]
        prompt_text: Option<String>,
    },

    DismissOverlays {
        #[serde(default)]
        aggressive: bool,
    },
    HighlightElement {
        locator: BrowserLocator,
    },
    Highlight {
        locator: BrowserLocator,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        style: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
    },
    HideHighlight,
    Annotate {
        locator: BrowserLocator,
        text: String,
    },

    Help {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        topic: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LocatorHandlerAction {
    Click,
    Steps { steps: Vec<BrowserStep> },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RouteHandler {
    Fulfill {
        #[serde(default = "default_fulfill_status")]
        status: u16,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        headers: BTreeMap<String, String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        body: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        json: Option<serde_json::Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        content_type: Option<String>,
        #[serde(default, skip_serializing_if = "is_false")]
        body_base64: bool,
    },
    Abort {
        reason: String,
    },
    Continue {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        url: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        method: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        headers: Option<BTreeMap<String, String>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        post_data: Option<String>,
    },
    Fallback,
    FetchAndFulfill {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        url: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        method: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        headers: Option<BTreeMap<String, String>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        post_data: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        status: Option<u16>,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        response_headers: BTreeMap<String, String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        body: Option<String>,
        #[serde(default, skip_serializing_if = "is_false")]
        body_base64: bool,
    },
}

fn default_fulfill_status() -> u16 {
    200
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BrowserLoadState {
    Domcontentloaded,
    Load,
    Networkidle,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BrowserWaitState {
    #[default]
    Attached,
    Detached,
    Visible,
    Hidden,
}

impl BrowserWaitState {
    pub fn needs_visibility(self) -> bool {
        matches!(self, Self::Visible | Self::Hidden)
    }

    pub fn reached_label(self) -> &'static str {
        match self {
            Self::Attached => "Element found",
            Self::Detached => "Element detached",
            Self::Visible => "Element visible",
            Self::Hidden => "Element hidden",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BrowserConsoleLevel {
    Log,
    Warning,
    Error,
}

impl BrowserConsoleLevel {
    pub fn matches_level(self, level: &str) -> bool {
        match self {
            Self::Log => level.eq_ignore_ascii_case("log"),
            Self::Warning => level.eq_ignore_ascii_case("warning"),
            Self::Error => {
                level.eq_ignore_ascii_case("error") || level.eq_ignore_ascii_case("page_error")
            }
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BrowserTextMode {
    #[default]
    InnerText,
    TextContent,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum UrlPattern {
    Text(String),
    Regex {
        source: String,
        #[serde(default)]
        flags: String,
    },
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CdpTarget {
    #[default]
    Page,
    Browser,
}

impl CdpTarget {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Page => "page",
            Self::Browser => "browser",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum ClockTicks {
    Millis(i64),
    Human(String),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum ClockTime {
    UnixMillis(i64),
    Text(String),
}

fn default_true() -> bool {
    true
}

fn reject_server_minted_authenticator_id<'de, D>(_deserializer: D) -> Result<(), D::Error>
where
    D: Deserializer<'de>,
{
    Err(serde::de::Error::custom(
        "id is server-minted; omit it and use the returned id",
    ))
}

impl BrowserStep {
    pub const ACTION_NAMES: &'static [&'static str] = &[
        "navigate",
        "reload",
        "go_back",
        "go_forward",
        "open_tab",
        "close_tab",
        "switch_tab",
        "list_tabs",
        "wait_for_popup",
        "route",
        "unroute",
        "list_routes",
        "route_web_socket",
        "unroute_web_socket",
        "send_web_socket_message",
        "close_web_socket",
        "wait_for_web_socket_frame",
        "start_har_recording",
        "stop_har_recording",
        "route_from_har",
        "reset",
        "cdp_send",
        "clock_install",
        "clock_fast_forward",
        "clock_pause_at",
        "clock_resume",
        "clock_run_for",
        "clock_set_fixed_time",
        "clock_set_system_time",
        "http_request",
        "start_coverage",
        "stop_coverage",
        "capture_frames",
        "screencast_start",
        "screencast_stop",
        "add_virtual_authenticator",
        "remove_virtual_authenticator",
        "list_credentials",
        "add_credential",
        "clear_credentials",
        "set_user_verified",
        "set_viewport",
        "set_window_bounds",
        "emulate_media",
        "set_locale",
        "set_timezone",
        "set_user_agent",
        "set_geolocation",
        "set_offline",
        "set_network_conditions",
        "set_cpu_throttling",
        "emulate_device",
        "list_devices",
        "set_extra_http_headers",
        "get_cookies",
        "set_cookies",
        "clear_cookies",
        "get_storage",
        "set_storage",
        "clear_storage",
        "storage_state",
        "set_storage_state",
        "grant_permissions",
        "clear_permissions",
        "set_http_credentials",
        "expect",
        "expect_poll",
        "click",
        "click_if_exists",
        "tap",
        "hover",
        "focus",
        "blur",
        "scroll_to",
        "press_key",
        "insert_text",
        "press_sequentially",
        "drag_and_drop",
        "drop_files",
        "mouse_move",
        "mouse_down",
        "mouse_up",
        "mouse_click_xy",
        "mouse_drag_xy",
        "mouse_wheel",
        "fill",
        "clear",
        "select_option",
        "check",
        "uncheck",
        "set_input_files",
        "expect_file_chooser",
        "wait_for_function",
        "wait_for_selector",
        "wait_for_navigation",
        "wait_for_url",
        "wait_for_text",
        "wait_for_network_idle",
        "wait_for_load_state",
        "wait_for_request",
        "wait_for_response",
        "wait_for_console_message",
        "wait_for_download",
        "cancel_download",
        "wait_for_element_hidden",
        "wait_for_element_stable",
        "wait_seconds",
        "get_text",
        "get_html",
        "get_attribute",
        "bounding_box",
        "count",
        "input_value",
        "all_texts",
        "element_state",
        "extract_links",
        "extract_table",
        "dom_snapshot",
        "accessibility_snapshot",
        "screenshot",
        "screenshot_element",
        "screenshot_elements",
        "capture_element_states",
        "pdf",
        "eval",
        "styles",
        "set_content",
        "page_content",
        "add_script_tag",
        "add_style_tag",
        "add_init_script",
        "remove_init_script",
        "dispatch_event",
        "tab_log",
        "add_locator_handler",
        "remove_locator_handler",
        "handle_dialog",
        "dismiss_overlays",
        "highlight_element",
        "highlight",
        "hide_highlight",
        "annotate",
        "help",
    ];
}
