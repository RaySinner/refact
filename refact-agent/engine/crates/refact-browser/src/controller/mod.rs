pub(crate) use std::cmp::Ordering;
pub(crate) use std::path::{Path, PathBuf};
pub(crate) use std::sync::{Arc, Mutex};
pub(crate) use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
pub(crate) use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub(crate) use base64::Engine;
pub(crate) use headless_chrome::Tab;
pub(crate) use headless_chrome::protocol::cdp::{DOM, Emulation, IO, Page, Runtime, types::Event};
pub(crate) use serde_json::Value;
pub(crate) use tokio::sync::Mutex as AMutex;

pub(crate) use refact_integrations::browser_locators::{
    self, js_string_literal, parse_element_info, INSPECT_ELEMENT_JS,
};
pub(crate) use refact_integrations::browser_models::*;
pub(crate) use crate::BrowserRuntime;
pub(crate) use refact_integrations::browser_types::{ConsoleEntry, NetworkEntry};
pub(crate) use refact_browser::{
    ActionKind, ActionabilityDiagnostic, ActionabilityDriver, ActionabilityEngine,
    ActionabilityExecutionMode, ActionabilityTimeouts, CDP_INLINE_RESULT_LIMIT_BYTES,
    CdpDragObserver, CdpGuardrail, CdpKeyboardDispatcher, CdpMouseDispatcher, ClockOp,
    DEFAULT_DISMISS_OVERLAYS_HANDLER, ElementHandle, ElementStateName, ExpectPollResult,
    FUNCTION_POLL_BACKOFF_MS, HitTargetController, HitTargetPoint, HitTargetResult, Keyboard,
    KeyboardDispatcher, LocatorGenerationOptions, LocatorHandler, LocatorHandlerLease,
    LocatorHandlerOperation, LocatorHandlerProbe, LocatorHandlerRegistry, LocatorOutcome,
    MainFrameCssPoint, Mouse, MouseButton, MouseDispatcher, MouseState, NetworkLoadState,
    NetworkMonitorHandle, NetworkWaitFilters, Ref, ScrollStrategy, SnapshotMode, SnapshotOptions,
    SystemClock, TabScopedHandle, UrlMatcher, WebSocketRegistry, WorldManager,
    apply_network_report_mode, classify_cdp_command, current_wall_ms, parse_clock_ticks,
    parse_clock_time, redact_cdp_result, required_states,
};
pub(crate) use refact_browser::artifacts::{
    ComposeLayout, ElementStateAction, ScreenshotMetrics, compose_sheet, element_state_sequence,
    pdf_payload, screenshot_capture,
};
pub(crate) use refact_browser::screencast::{
    build_filmstrip, capture_screencast_burst, capture_timed_frames, select_evenly_spaced,
    CapturedFrame, FilmstripResult, FrameBurstPlan, ScreencastSessionOptions,
    DEFAULT_SCREENCAST_QUALITY, MAX_FRAME_COUNT, MAX_SESSION_DURATION_MS, MAX_SESSION_FRAMES,
};
pub(crate) use refact_browser::http_client;
pub(crate) use refact_core::image_policy::{resize_to_policy, ImageFormat, ImagePolicy};

pub(crate) use crate::settings as browser_settings;

mod context;
mod steps_runtime;
mod dispatch;
mod execute;
mod single_step;
mod expect_poll;
mod actions;
mod capture;
mod eval_steps;

pub use capture::{capture_viewport_screenshot, capture_viewport_screenshot_png, run_element_state_sequence};
pub use context::{resolve_locator_element, resolve_tab, session_tab};
pub use dispatch::{execute_request_with_runtime, execute_step};
pub use eval_steps::{describe_locator};
pub use execute::{execute_steps_with_runtime, mark_unexecuted_steps_skipped};
pub use steps_runtime::{execute_steps, is_tab_management_step};

#[cfg(test)]
mod tests_support;
#[cfg(test)]
mod tests_steps;
#[cfg(test)]
mod tests_capture;
