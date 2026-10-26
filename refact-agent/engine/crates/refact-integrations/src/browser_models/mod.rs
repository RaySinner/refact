mod action_fields;
mod expectation;
mod har;
mod helpers;
mod locator;
mod parse;
mod request;
mod result;
mod screenshot;
mod step;
mod webauthn;
mod websocket;

#[cfg(test)]
mod browser_action_parse_tests;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_serde;

pub use action_fields::BrowserActionFields;

pub use expectation::{
    AccessibilitySnapshotMode, AccessibilitySnapshotOptions, BrowserExpectation,
    BrowserExpectedText, BrowserExpectedTextOrList, BrowserPollMatcher, BrowserPseudoElement,
    TabTarget,
};

pub use har::{HarContentPolicy, HarMode, HarNotFound};

pub use locator::{
    locator_strategy_from_wire, AriaCheckedState, AriaMixedState, BrowserLocator, LocatorFilter,
    LocatorRegex, LocatorStrategy, LocatorTextMatcher,
};

pub use parse::parse_browser_action_request;

pub use request::{BrowserActionRequest, NetworkReportMode, PageContextMode, SessionPolicy};

pub use result::{
    ActionabilityDiagnostics, BrowserAssertionResult, BrowserConsoleCounts, BrowserContextSummary,
    BrowserCookie, BrowserCookieSameSite, BrowserFrameSummary, BrowserIndexedDbDatabase,
    BrowserIndexedDbRecord, BrowserIndexedDbStore, BrowserPageContext, BrowserPageSnapshot,
    BrowserPermissionState, BrowserScreenshot, BrowserSnapshotArtifact, BrowserStorageItem,
    BrowserStorageKind, BrowserStorageOrigin, BrowserStorageState, DialogAction, DialogInfo,
    DialogType, DownloadInfo, DownloadState, ElementBBox, ElementInfo, ExecutionReport, FieldKind,
    FillStrategy, HarRouteInfo, LocatorHandlerFiring, RouteInfo, RouteInterception, StepResult,
    TabInfo, TabOpener, UploadInfo,
};

pub use screenshot::{
    BrowserComposeMode, BrowserElementState, BrowserHttpRequest, BrowserMouseButton,
    BrowserPdfMargin, BrowserPdfOptions, BrowserPosition, BrowserScreenshotAnimations,
    BrowserScreenshotCaret, BrowserScreenshotClip, BrowserScreenshotOptions,
    BrowserScreenshotScale, BrowserScreenshotType,
};

pub use step::{
    BrowserConsoleLevel, BrowserLoadState, BrowserStep, BrowserTextMode, BrowserWaitState,
    CdpTarget, ClockTicks, ClockTime, LocatorHandlerAction, RouteHandler, UrlPattern,
};

pub use webauthn::{
    BrowserAuthenticatorProtocol, BrowserAuthenticatorTransport, BrowserWebAuthnCredential,
};

pub use websocket::{
    WebSocketEvent, WebSocketEventKind, WebSocketFrameDisposition, WebSocketMessageAction,
    WebSocketRouteMode,
};
