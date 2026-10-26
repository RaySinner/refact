use super::*;

use super::actions::{element_drag_point};
use super::single_step::{perform_locator_handlers_checkpoint, wait_for_pending_navigation};

use std::cmp::Ordering;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::Engine;
use headless_chrome::Tab;
use headless_chrome::protocol::cdp::{DOM, Emulation, IO, Page, Runtime, types::Event};
use serde_json::Value;
use tokio::sync::Mutex as AMutex;

use refact_integrations::browser_locators::{
    self, js_string_literal, parse_element_info, INSPECT_ELEMENT_JS,
};
use refact_integrations::browser_models::*;
use crate::BrowserRuntime;
use refact_integrations::browser_types::{ConsoleEntry, NetworkEntry};
use refact_browser::{
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
use refact_browser::artifacts::{
    ComposeLayout, ElementStateAction, ScreenshotMetrics, compose_sheet, element_state_sequence,
    pdf_payload, screenshot_capture,
};
use refact_browser::screencast::{
    build_filmstrip, capture_screencast_burst, capture_timed_frames, select_evenly_spaced,
    CapturedFrame, FilmstripResult, FrameBurstPlan, ScreencastSessionOptions,
    DEFAULT_SCREENCAST_QUALITY, MAX_FRAME_COUNT, MAX_SESSION_DURATION_MS, MAX_SESSION_FRAMES,
};
use refact_browser::http_client;
use refact_core::image_policy::{resize_to_policy, ImageFormat, ImagePolicy};

use crate::settings as browser_settings;

#[allow(dead_code)]
pub(crate) const DEFAULT_WAIT_TIMEOUT_MS: u64 = 5_000;
#[allow(dead_code)]
pub(crate) const MAX_WAIT_TIMEOUT_MS: u64 = 60_000;
pub(crate) const MAX_WAIT_SECONDS: f64 = 60.0;
pub(crate) const MIN_WAIT_SECONDS: f64 = 0.0;
pub(crate) const NAVIGATION_LIFECYCLE_EVENT: &str = "load";

#[allow(dead_code)]
pub(crate) const MAX_DOM_SNAPSHOT_CHARS: usize = 100_000;
#[allow(dead_code)]
pub(crate) const MAX_EXTRACT_LINKS: usize = 500;
#[allow(dead_code)]
pub(crate) const MAX_EXTRACT_TABLE_ROWS: usize = 100;
#[allow(dead_code)]
pub(crate) const DEFAULT_ALL_TEXTS: usize = 50;
pub(crate) const MAX_ALL_TEXTS: usize = 500;
#[allow(dead_code)]
pub(crate) const DEFAULT_ARIA_SNAPSHOT_CHARS: usize = 20_000;
#[allow(dead_code)]
pub(crate) const MAX_ARIA_SNAPSHOT_CHARS: usize = 100_000;
#[allow(dead_code)]
pub(crate) const MAX_INLINE_SNAPSHOT_BYTES: usize = 6 * 1024;
pub(crate) const SNAPSHOT_SUMMARY_LINES: usize = 40;

pub(crate) const TAP_AMBIGUOUS_TARGET: &str = "tap requires either a locator or both x and y";
pub(crate) const TAP_REQUIRES_TOUCH: &str = "The page does not support tap: enable touch emulation first with a set_viewport step that sets has_touch to true";

pub(crate) fn clamp_timeout_ms(requested: Option<u64>) -> u64 {
    let settings = browser_settings::current();
    requested
        .unwrap_or(settings.timing.default_wait_timeout_ms)
        .min(settings.timing.max_wait_timeout_ms)
}

pub(crate) fn clamp_wait_seconds(requested: f64) -> f64 {
    if requested.is_nan() || requested.is_infinite() || requested < MIN_WAIT_SECONDS {
        MIN_WAIT_SECONDS
    } else if requested > MAX_WAIT_SECONDS {
        MAX_WAIT_SECONDS
    } else {
        requested
    }
}

#[allow(dead_code)]
pub(crate) const DEFAULT_POLL_INTERVAL_MS: u64 = 200;
pub(crate) const REPORT_STABILIZATION_TIMEOUT_MS: u64 = 3_000;
pub(crate) const REPORT_STABILITY_INTERVAL_MS: u64 = 200;
pub(crate) const CONSOLE_POLL_INTERVAL_MS: u64 = 50;

pub fn resolve_tab(runtime: &BrowserRuntime, target: &TabTarget) -> Result<Arc<Tab>, String> {
    match target {
        TabTarget::Active => runtime
            .get_active_tab()
            .ok_or_else(|| "No active tab in browser runtime".to_string()),
        TabTarget::Id { id } => {
            let tabs = runtime
                .browser
                .get_tabs()
                .lock()
                .map_err(|e| format!("Failed to lock browser tabs: {}", e))?;
            tabs.iter()
                .find(|t| t.get_target_id() == id)
                .cloned()
                .ok_or_else(|| format!("Tab not found with id: {}", id))
        }
    }
}

pub fn session_tab(runtime: &mut BrowserRuntime) -> Result<Arc<Tab>, String> {
    refact_browser::adopt_new_tabs(runtime, None);
    resolve_tab(runtime, &TabTarget::Active)
}

pub(crate) fn eval_js_value(tab: &Tab, js: &str) -> Result<serde_json::Value, String> {
    let remote = tab
        .evaluate(js, false)
        .map_err(|e| format!("JS evaluation failed: {}", e))?;
    remote
        .value
        .ok_or_else(|| "JS evaluation returned no value".to_string())
}

pub(crate) fn eval_js_json(tab: &Tab, js: &str) -> Result<serde_json::Value, String> {
    let val = eval_js_value(tab, js)?;
    match val.as_str() {
        Some(s) => {
            serde_json::from_str(s).map_err(|e| format!("Failed to parse JS JSON result: {}", e))
        }
        None if val.is_object() || val.is_array() => Ok(val),
        None => Err(format!("Unexpected JS result type: {:?}", val)),
    }
}

pub(crate) fn eval_js_ok(tab: &Tab, js: &str) -> Result<serde_json::Value, String> {
    let result = eval_js_json(tab, js)?;
    if let Some(err) = result.get("error").and_then(|v| v.as_str()) {
        return Err(err.to_string());
    }
    Ok(result)
}

pub(crate) struct ResolvedElement {
    pub(crate) handle: ElementHandle,
    pub(crate) info: ElementInfo,
}

pub(crate) enum ResolveElementError {
    MultipleMatches { count: usize, previews: Vec<String> },
    Other(String),
}

impl ResolveElementError {
    pub(crate) fn into_message(self, locator: &BrowserLocator) -> String {
        match self {
            Self::MultipleMatches { count, previews } => {
                refact_browser::strict_mode_violation(&describe_locator(locator), count, &previews)
            }
            Self::Other(message) => message,
        }
    }
}

pub(crate) fn locator_outcome_for_resolve_error(strategy: &LocatorStrategy, error: String) -> LocatorOutcome {
    if matches!(strategy, LocatorStrategy::Ref { .. })
        || refact_browser::is_transport_dead_error(&error)
    {
        LocatorOutcome::Error { description: error }
    } else {
        LocatorOutcome::NotFound
    }
}

impl std::ops::Deref for ResolvedElement {
    type Target = ElementInfo;

    fn deref(&self) -> &Self::Target {
        &self.info
    }
}

pub(crate) struct ScopedElement<'a> {
    pub(crate) handle: TabScopedHandle<'a>,
    pub(crate) info: ElementInfo,
}

impl std::ops::Deref for ScopedElement<'_> {
    type Target = ElementInfo;

    fn deref(&self) -> &Self::Target {
        &self.info
    }
}

pub(crate) fn resolve_scoped_element<'a>(
    tab: &'a Tab,
    world: &'a WorldManager,
    locator: &BrowserLocator,
) -> Result<ScopedElement<'a>, String> {
    let resolved = resolve_element(tab, world, locator)?;
    Ok(ScopedElement {
        handle: world.wrap_scoped(tab, resolved.handle),
        info: resolved.info,
    })
}

pub(crate) struct BrowserActionDriver<'a> {
    pub(crate) tab: &'a Tab,
    pub(crate) world: &'a WorldManager,
    pub(crate) locator: &'a BrowserLocator,
    pub(crate) action: ActionKind,
    pub(crate) handlers: Option<&'a Arc<Mutex<LocatorHandlerRegistry>>>,
    pub(crate) locator_handler_firings: &'a mut Vec<LocatorHandlerFiring>,
    pub(crate) image_policy: &'a ImagePolicy,
    pub(crate) precheck_deadline: Instant,
    pub(crate) resolved: Option<ResolvedElement>,
    pub(crate) locator_echo: Option<String>,
    pub(crate) pointer_attempt: usize,
}

pub(crate) struct DragActionabilityDriver<'a> {
    pub(crate) tab: &'a Tab,
    pub(crate) world: &'a WorldManager,
    pub(crate) locator: &'a BrowserLocator,
    pub(crate) handlers: Option<&'a Arc<Mutex<LocatorHandlerRegistry>>>,
    pub(crate) locator_handler_firings: &'a mut Vec<LocatorHandlerFiring>,
    pub(crate) image_policy: &'a ImagePolicy,
    pub(crate) precheck_deadline: Instant,
    pub(crate) resolved: Option<ResolvedElement>,
    pub(crate) position: Option<BrowserPosition>,
}

impl Drop for DragActionabilityDriver<'_> {
    fn drop(&mut self) {
        if let Some(resolved) = self.resolved.take() {
            let _ = self.world.release_handle(self.tab, &resolved.handle);
        }
    }
}

impl ActionabilityDriver for DragActionabilityDriver<'_> {
    type Output = (String, ElementHandle, MainFrameCssPoint);

    fn resolve(&mut self) -> LocatorOutcome {
        if let Some(resolved) = self.resolved.take() {
            let _ = self.world.release_handle(self.tab, &resolved.handle);
        }
        match resolve_element_typed(self.tab, self.world, self.locator) {
            Ok(resolved) => {
                let preview = element_preview(self.tab, self.world, &resolved.handle);
                self.resolved = Some(resolved);
                LocatorOutcome::Found { preview }
            }
            Err(ResolveElementError::MultipleMatches { count, previews }) => {
                LocatorOutcome::MultipleMatches { count, previews }
            }
            Err(ResolveElementError::Other(error)) => {
                locator_outcome_for_resolve_error(&self.locator.strategy, error)
            }
        }
    }

    fn element_state(&mut self) -> Result<refact_browser::ElementState, ActionabilityDiagnostic> {
        let resolved = self
            .resolved
            .as_ref()
            .ok_or(ActionabilityDiagnostic::Detached)?;
        self.world
            .element_states(self.tab, &resolved.handle)
            .map_err(|error| ActionabilityDiagnostic::PrecheckFailed {
                description: error.to_string(),
            })
    }

    fn perform(&mut self) -> Result<Self::Output, ActionabilityDiagnostic> {
        let resolved = self
            .resolved
            .as_ref()
            .ok_or(ActionabilityDiagnostic::Detached)?;
        let point =
            element_drag_point(self.tab, &resolved.handle, self.position).map_err(|error| {
                ActionabilityDiagnostic::PrecheckFailed {
                    description: error.to_string(),
                }
            })?;
        match HitTargetController::default().expect_hit_target(
            self.tab,
            self.world,
            &resolved.handle,
            HitTargetPoint {
                x: point.x,
                y: point.y,
            },
        ) {
            Ok(HitTargetResult::Done) => {
                let resolved = self.resolved.take().unwrap();
                Ok((resolved.info.tag, resolved.handle, point))
            }
            Ok(HitTargetResult::Intercepted { description }) => {
                Err(intercepts_pointer_events(description))
            }
            Ok(HitTargetResult::NotConnected) => Err(ActionabilityDiagnostic::Detached),
            Ok(HitTargetResult::Skipped) => Err(ActionabilityDiagnostic::PrecheckFailed {
                description: "Browser drag hit-target check was skipped".to_string(),
            }),
            Err(error) => Err(ActionabilityDiagnostic::PrecheckFailed {
                description: error.to_string(),
            }),
        }
    }

    fn wait_for_navigation(&mut self) -> Result<(), ActionabilityDiagnostic> {
        wait_for_pending_navigation(self.tab, self.precheck_deadline)
            .map_err(|description| ActionabilityDiagnostic::PrecheckFailed { description })
    }

    fn locator_handlers_checkpoint(&mut self) -> Result<(), ActionabilityDiagnostic> {
        let Some(handlers) = self.handlers else {
            return Ok(());
        };
        perform_locator_handlers_checkpoint(
            self.tab,
            self.world,
            handlers,
            self.locator_handler_firings,
            self.image_policy,
            self.precheck_deadline,
        )
        .map_err(|description| ActionabilityDiagnostic::PrecheckFailed { description })
    }
}

impl<'a> BrowserActionDriver<'a> {
    pub(super) fn new(
        tab: &'a Tab,
        world: &'a WorldManager,
        locator: &'a BrowserLocator,
        action: ActionKind,
        handlers: Option<&'a Arc<Mutex<LocatorHandlerRegistry>>>,
        locator_handler_firings: &'a mut Vec<LocatorHandlerFiring>,
        image_policy: &'a ImagePolicy,
        timeout: Duration,
    ) -> Self {
        let settings = browser_settings::current();
        Self {
            tab,
            world,
            locator,
            action,
            handlers,
            locator_handler_firings,
            image_policy,
            precheck_deadline: Instant::now()
                + timeout.min(Duration::from_millis(
                    settings.timing.default_wait_timeout_ms,
                )),
            resolved: None,
            locator_echo: None,
            pointer_attempt: 0,
        }
    }

    fn release_resolved(&mut self) {
        if let Some(resolved) = self.resolved.take() {
            let _ = self.world.release_handle(self.tab, &resolved.handle);
        }
    }

    fn resolved(&self) -> Result<&ResolvedElement, ActionabilityDiagnostic> {
        self.resolved
            .as_ref()
            .ok_or(ActionabilityDiagnostic::Detached)
    }
}

impl Drop for BrowserActionDriver<'_> {
    fn drop(&mut self) {
        self.release_resolved();
    }
}

impl ActionabilityDriver for BrowserActionDriver<'_> {
    type Output = String;

    fn resolve(&mut self) -> LocatorOutcome {
        self.release_resolved();
        match resolve_element_typed(self.tab, self.world, self.locator) {
            Ok(resolved) => {
                let preview = element_preview(self.tab, self.world, &resolved.handle);
                self.resolved = Some(resolved);
                LocatorOutcome::Found { preview }
            }
            Err(ResolveElementError::MultipleMatches { count, previews }) => {
                LocatorOutcome::MultipleMatches { count, previews }
            }
            Err(ResolveElementError::Other(error)) => {
                locator_outcome_for_resolve_error(&self.locator.strategy, error)
            }
        }
    }

    fn element_state(&mut self) -> Result<refact_browser::ElementState, ActionabilityDiagnostic> {
        let resolved = self.resolved()?;
        self.world
            .element_states(self.tab, &resolved.handle)
            .map_err(|error| match error {
                refact_browser::HandleError::Invalidated { .. } => {
                    ActionabilityDiagnostic::Detached
                }
                _ => ActionabilityDiagnostic::PrecheckFailed {
                    description: error.to_string(),
                },
            })
    }

    fn perform(&mut self) -> Result<Self::Output, ActionabilityDiagnostic> {
        self.locator_echo = self
            .resolved
            .as_ref()
            .and_then(|resolved| generate_locator_echo(self.tab, self.world, &resolved.handle));
        let scroll_strategy = ScrollStrategy::for_attempt(self.pointer_attempt);
        self.pointer_attempt += 1;
        let resolved = self.resolved()?;
        let dispatcher = CdpMouseDispatcher::new(self.tab);
        if self.action == ActionKind::Focus {
            return call_handle_json(
                self.tab,
                self.world,
                &resolved.handle,
                &browser_locators::js_focus_element(),
            )
            .map(|_| resolved.info.tag.clone())
            .map_err(|description| ActionabilityDiagnostic::PrecheckFailed { description });
        }
        if self.action == ActionKind::ScrollIntoViewIfNeeded {
            return dispatcher
                .scroll_into_view(&resolved.handle, ScrollStrategy::Protocol)
                .map(|_| resolved.info.tag.clone())
                .map_err(|error| ActionabilityDiagnostic::PrecheckFailed {
                    description: error.to_string(),
                });
        }
        if let Err(error) = dispatcher.scroll_into_view(&resolved.handle, scroll_strategy) {
            if refact_browser::is_transport_dead_error(&error.to_string()) {
                return Err(ActionabilityDiagnostic::PrecheckFailed {
                    description: error.to_string(),
                });
            }
        }
        let point = dispatcher
            .clickable_point(&resolved.handle)
            .map_err(|error| match error {
                refact_browser::MouseError::OutsideViewport => {
                    ActionabilityDiagnostic::OutsideViewport
                }
                _ => ActionabilityDiagnostic::PrecheckFailed {
                    description: error.to_string(),
                },
            })?;
        let hit_target = HitTargetController::default();
        let hit_target_point = HitTargetPoint {
            x: point.x,
            y: point.y,
        };
        match hit_target.expect_hit_target(self.tab, self.world, &resolved.handle, hit_target_point)
        {
            Ok(HitTargetResult::Done) => {}
            Ok(HitTargetResult::Intercepted { description }) => {
                return Err(intercepts_pointer_events(description));
            }
            Ok(HitTargetResult::NotConnected) => {
                return Err(ActionabilityDiagnostic::Detached);
            }
            Ok(HitTargetResult::Skipped) => {}
            Err(error) => {
                return Err(ActionabilityDiagnostic::PrecheckFailed {
                    description: error.to_string(),
                });
            }
        }
        let token = hit_target
            .install_interceptor(
                self.tab,
                self.world,
                &resolved.handle,
                self.action,
                Some(hit_target_point),
            )
            .map_err(|error| ActionabilityDiagnostic::PrecheckFailed {
                description: error.to_string(),
            })?;
        let keyboard = Keyboard::new(CdpKeyboardDispatcher::new(self.tab));
        let mut mouse = Mouse::new(dispatcher, &keyboard);
        let action_result = match self.action {
            ActionKind::Click => mouse.click(point.x, point.y, MouseButton::Left),
            ActionKind::Hover => mouse.hover(point.x, point.y),
            ActionKind::Tap => mouse.tap(point.x, point.y),
            _ => unreachable!(),
        };
        let hit_result = hit_target.take_result(self.tab, self.world, token);
        action_result.map_err(|error| ActionabilityDiagnostic::PrecheckFailed {
            description: error.to_string(),
        })?;
        match hit_result {
            Ok(HitTargetResult::Done | HitTargetResult::Skipped) => Ok(resolved.info.tag.clone()),
            Ok(HitTargetResult::Intercepted { description }) => {
                Err(intercepts_pointer_events(description))
            }
            Ok(HitTargetResult::NotConnected) => Err(ActionabilityDiagnostic::Detached),
            Err(error) => Err(ActionabilityDiagnostic::PrecheckFailed {
                description: error.to_string(),
            }),
        }
    }

    fn wait_for_navigation(&mut self) -> Result<(), ActionabilityDiagnostic> {
        wait_for_pending_navigation(self.tab, self.precheck_deadline)
            .map_err(|description| ActionabilityDiagnostic::PrecheckFailed { description })
    }

    fn locator_handlers_checkpoint(&mut self) -> Result<(), ActionabilityDiagnostic> {
        let Some(handlers) = self.handlers else {
            return Ok(());
        };
        perform_locator_handlers_checkpoint(
            self.tab,
            self.world,
            handlers,
            self.locator_handler_firings,
            self.image_policy,
            self.precheck_deadline,
        )
        .map_err(|description| ActionabilityDiagnostic::PrecheckFailed { description })
    }
}

pub(crate) fn intercepts_pointer_events(description: String) -> ActionabilityDiagnostic {
    ActionabilityDiagnostic::InterceptsPointerEvents {
        description: description
            .strip_suffix(" intercepts pointer events")
            .unwrap_or(&description)
            .to_string(),
    }
}

pub(crate) fn generate_locator_echo(
    tab: &Tab,
    world: &WorldManager,
    handle: &ElementHandle,
) -> Option<String> {
    world
        .generate_locator(tab, handle, LocatorGenerationOptions::default())
        .ok()
        .filter(|locator| !locator.is_empty())
}

pub(crate) fn element_preview(tab: &Tab, world: &WorldManager, handle: &ElementHandle) -> String {
    world
        .call_function_on(
            tab,
            handle,
            "function() { return this.outerHTML.substring(0, 500); }",
            Vec::new(),
        )
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_else(|| "<element>".to_string())
}

pub(crate) fn resolve_element(
    tab: &Tab,
    world: &WorldManager,
    locator: &BrowserLocator,
) -> Result<ResolvedElement, String> {
    resolve_element_typed(tab, world, locator).map_err(|error| error.into_message(locator))
}

pub(crate) fn resolve_element_typed(
    tab: &Tab,
    world: &WorldManager,
    locator: &BrowserLocator,
) -> Result<ResolvedElement, ResolveElementError> {
    let handles =
        resolve_locator_handles(tab, world, locator).map_err(ResolveElementError::Other)?;
    let handle = match strict_locator_handle_result(tab, world, handles) {
        StrictLocatorHandle::None => {
            return Err(ResolveElementError::Other("Element not found".to_string()));
        }
        StrictLocatorHandle::One(handle) => handle,
        StrictLocatorHandle::Multiple { count, previews } => {
            return Err(ResolveElementError::MultipleMatches { count, previews });
        }
    };
    inspect_resolved_element(tab, world, handle).map_err(ResolveElementError::Other)
}

pub fn resolve_locator_element(
    tab: &Tab,
    world: &WorldManager,
    locator: &BrowserLocator,
) -> Result<ElementHandle, String> {
    resolve_element(tab, world, locator).map(|resolved| resolved.handle)
}

pub(crate) fn resolve_locator_handles(
    tab: &Tab,
    world: &WorldManager,
    locator: &BrowserLocator,
) -> Result<Vec<ElementHandle>, String> {
    match &locator.strategy {
        LocatorStrategy::Ref { value } => {
            if !locator.frames.is_empty()
                || locator.nth.is_some()
                || locator.within.is_some()
                || locator.locator.is_some()
                || locator.filter.is_some()
                || locator.and.is_some()
                || locator.or.is_some()
                || locator.first.is_some()
                || locator.last.is_some()
            {
                return Err("Ref locators cannot be composed or filtered".to_string());
            }
            let reference = value
                .parse::<Ref>()
                .map_err(|error| format!("Invalid browser ref {value}: {error}"))?;
            Ok(vec![world
                .resolve_ref(tab, &reference)
                .map_err(|error| error.to_string())?])
        }
        _ => {
            let locator_value = locator_without_frames(locator)?;
            if locator.frames.is_empty() {
                world
                    .call_injected_handles(tab, "resolveAll", serde_json::json!([locator_value]))
                    .map_err(|error| error.to_string())
            } else {
                let owners = locator
                    .frames
                    .iter()
                    .map(locator_without_frames)
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|error| format!("Failed to serialize frame locator: {error}"))?;
                world
                    .resolve_frame_locator(tab, &owners, locator_value)
                    .map_err(|error| error.to_string())
            }
        }
    }
}

pub(crate) fn locator_without_frames(locator: &BrowserLocator) -> Result<serde_json::Value, String> {
    let mut locator = serde_json::to_value(locator)
        .map_err(|error| format!("Failed to serialize browser locator: {error}"))?;
    locator
        .as_object_mut()
        .ok_or_else(|| "Browser locator did not serialize as an object".to_string())?
        .remove("frames");
    Ok(locator)
}

pub(crate) fn strict_locator_handle(
    tab: &Tab,
    world: &WorldManager,
    locator: &BrowserLocator,
    handles: Vec<ElementHandle>,
) -> Result<Option<ElementHandle>, String> {
    match strict_locator_handle_result(tab, world, handles) {
        StrictLocatorHandle::None => Ok(None),
        StrictLocatorHandle::One(handle) => Ok(Some(handle)),
        StrictLocatorHandle::Multiple { count, previews } => Err(
            refact_browser::strict_mode_violation(&describe_locator(locator), count, &previews),
        ),
    }
}

pub(crate) enum StrictLocatorHandle {
    None,
    One(ElementHandle),
    Multiple { count: usize, previews: Vec<String> },
}

pub(crate) fn strict_locator_handle_result(
    tab: &Tab,
    world: &WorldManager,
    mut handles: Vec<ElementHandle>,
) -> StrictLocatorHandle {
    if handles.len() > 1 {
        let count = handles.len();
        let previews = strict_locator_previews(tab, world, &handles);
        release_locator_handles(tab, world, &handles);
        return StrictLocatorHandle::Multiple { count, previews };
    }
    handles
        .pop()
        .map(StrictLocatorHandle::One)
        .unwrap_or(StrictLocatorHandle::None)
}

pub(crate) fn strict_locator_previews<'h>(
    tab: &Tab,
    world: &WorldManager,
    handles: impl IntoIterator<Item = &'h ElementHandle>,
) -> Vec<String> {
    handles
        .into_iter()
        .take(5)
        .filter_map(|handle| {
            world
                .call_function_on(
                    tab,
                    handle,
                    "function() { return this.outerHTML.substring(0, 200); }",
                    Vec::new(),
                )
                .ok()
                .and_then(|value| value.as_str().map(str::to_string))
        })
        .collect()
}

pub(crate) fn release_locator_handles(tab: &Tab, world: &WorldManager, handles: &[ElementHandle]) {
    for handle in handles {
        let _ = world.release_handle(tab, handle);
    }
}

pub(crate) fn inspect_resolved_element(
    tab: &Tab,
    world: &WorldManager,
    handle: ElementHandle,
) -> Result<ResolvedElement, String> {
    let inspect = format!(
        "function() {{ {INSPECT_ELEMENT_JS} return JSON.stringify(__refact_inspect_element(this, 1)); }}"
    );
    let val = match world.call_function_on(tab, &handle, &inspect, Vec::new()) {
        Ok(value) => value,
        Err(error) => {
            let _ = world.release_handle(tab, &handle);
            return Err(error.to_string());
        }
    };
    let json_str = match val.as_str() {
        Some(s) => s.to_string(),
        None => match serde_json::to_string(&val) {
            Ok(json) => json,
            Err(error) => {
                let _ = world.release_handle(tab, &handle);
                return Err(format!("Failed to serialize resolve result: {error}"));
            }
        },
    };
    let info = match parse_element_info(&json_str) {
        Ok(info) => info,
        Err(error) => {
            let _ = world.release_handle(tab, &handle);
            return Err(error);
        }
    };
    Ok(ResolvedElement { handle, info })
}

pub(crate) fn resolve_interactable(
    tab: &Tab,
    world: &WorldManager,
    locator: &BrowserLocator,
) -> Result<ResolvedElement, String> {
    let resolved = resolve_element(tab, world, locator)?;
    if !resolved.info.visible {
        let _ = world.release_handle(tab, &resolved.handle);
        return Err("Element is not visible".to_string());
    }
    if !resolved.info.enabled {
        let _ = world.release_handle(tab, &resolved.handle);
        return Err("Element is disabled".to_string());
    }
    Ok(resolved)
}

/// Executes a JavaScript function declaration with the element handle bound as `this`.
///
/// Callers must pass the declaration itself, never an already-invoked function expression.
pub(crate) fn call_handle_json(
    tab: &Tab,
    world: &WorldManager,
    handle: &ElementHandle,
    function_declaration: &str,
) -> Result<serde_json::Value, String> {
    let value = world
        .call_function_on(tab, handle, function_declaration, Vec::new())
        .map_err(|error| error.to_string())?;
    let result = match value.as_str() {
        Some(json) => serde_json::from_str(json)
            .map_err(|error| format!("Failed to parse JS JSON result: {error}"))?,
        None if value.is_object() || value.is_array() => value,
        None => return Err(format!("Unexpected JS result type: {value:?}")),
    };
    if let Some(error) = result.get("error").and_then(|value| value.as_str()) {
        return Err(error.to_string());
    }
    Ok(result)
}
