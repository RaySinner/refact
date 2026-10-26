use super::*;

use super::context::{BrowserActionDriver, CONSOLE_POLL_INTERVAL_MS, DragActionabilityDriver, NAVIGATION_LIFECYCLE_EVENT, REPORT_STABILITY_INTERVAL_MS, ResolveElementError, TAP_AMBIGUOUS_TARGET, TAP_REQUIRES_TOUCH, call_handle_json, clamp_timeout_ms, eval_js_value, generate_locator_echo, release_locator_handles, resolve_element_typed, resolve_interactable, resolve_locator_handles, resolve_scoped_element, strict_locator_handle};
use super::single_step::{wait_for_load_state};


pub(crate) fn uses_actionability_engine(step: &BrowserStep) -> bool {
    matches!(
        step,
        BrowserStep::Click { .. }
            | BrowserStep::ClickIfExists { .. }
            | BrowserStep::Tap {
                locator: Some(_),
                ..
            }
            | BrowserStep::Hover { .. }
            | BrowserStep::Focus { .. }
            | BrowserStep::InsertText {
                locator: Some(_),
                ..
            }
            | BrowserStep::PressSequentially { .. }
            | BrowserStep::ScrollTo { .. }
            | BrowserStep::DragAndDrop { .. }
    )
}

pub(crate) fn element_drag_point(
    tab: &Tab,
    handle: &ElementHandle,
    position: Option<BrowserPosition>,
) -> Result<MainFrameCssPoint, refact_browser::MouseError> {
    if let Some(position) = position {
        let quads = tab
            .call_method(DOM::GetContentQuads {
                node_id: None,
                backend_node_id: None,
                object_id: Some(handle.object_id.clone()),
            })
            .map_err(|error| {
                refact_browser::MouseError::Protocol(format!(
                    "Failed to read browser element content quads: {error}"
                ))
            })?;
        let Some(quad) = quads.quads.first() else {
            return Err(refact_browser::MouseError::NoQuads);
        };
        if quad.len() != 8 {
            return Err(refact_browser::MouseError::Protocol(
                "Browser content quad must contain 8 coordinates".to_string(),
            ));
        }
        return Ok(MainFrameCssPoint {
            x: quad[0] + position.x,
            y: quad[1] + position.y,
        });
    }
    CdpMouseDispatcher::new(tab).clickable_point(handle)
}

pub(crate) fn resolve_drag_endpoint(
    tab: &Tab,
    world: &WorldManager,
    locator: &BrowserLocator,
    position: Option<BrowserPosition>,
    action: ActionKind,
    mode: ActionabilityExecutionMode,
    handlers: Option<&Arc<Mutex<LocatorHandlerRegistry>>>,
    firings: &mut Vec<LocatorHandlerFiring>,
    image_policy: &ImagePolicy,
) -> Result<
    refact_browser::ActionabilitySuccess<(String, ElementHandle, MainFrameCssPoint)>,
    refact_browser::ActionabilityError,
> {
    let settings = browser_settings::current();
    let engine = ActionabilityEngine::new(SystemClock::default(), ActionabilityTimeouts::default());
    let timeout = ActionabilityTimeouts::default().action;
    let mut driver = DragActionabilityDriver {
        tab,
        world,
        locator,
        handlers,
        locator_handler_firings: firings,
        image_policy,
        precheck_deadline: Instant::now()
            + timeout.min(Duration::from_millis(
                settings.timing.default_wait_timeout_ms,
            )),
        resolved: None,
        position,
    };
    engine.execute_with_timeout_in_mode(
        &describe_locator(locator),
        action,
        timeout,
        mode,
        &mut driver,
    )
}

pub(crate) fn step_drag_and_drop(
    tab: &Tab,
    world: &WorldManager,
    idx: usize,
    source: &BrowserLocator,
    target: &BrowserLocator,
    source_position: Option<BrowserPosition>,
    target_position: Option<BrowserPosition>,
    handlers: Option<&Arc<Mutex<LocatorHandlerRegistry>>>,
    firings: &mut Vec<LocatorHandlerFiring>,
    image_policy: &ImagePolicy,
    mouse_state: &mut MouseState,
) -> StepResult {
    let source = match resolve_drag_endpoint(
        tab,
        world,
        source,
        source_position,
        ActionKind::DragSource,
        ActionabilityExecutionMode::Standard,
        handlers,
        firings,
        image_policy,
    ) {
        Ok(source) => source,
        Err(error) => {
            let mut result = StepResult::failure(
                idx,
                "Drag source failed",
                actionability_error_text(&error, source),
            );
            result.actionability = Some(error.diagnostics(ActionKind::DragSource));
            return result;
        }
    };
    let target = match resolve_drag_endpoint(
        tab,
        world,
        target,
        target_position,
        ActionKind::DragTarget,
        ActionabilityExecutionMode::SkipLocatorHandlers,
        handlers,
        firings,
        image_policy,
    ) {
        Ok(target) => target,
        Err(error) => {
            let mut result = StepResult::failure(
                idx,
                "Drag target failed",
                actionability_error_text(&error, target),
            );
            result.actionability = Some(error.diagnostics(ActionKind::DragTarget));
            return result;
        }
    };
    let keyboard = Keyboard::new(CdpKeyboardDispatcher::new(tab));
    let dispatcher = CdpMouseDispatcher::new(tab);
    let mut mouse = Mouse::from_state(dispatcher, &keyboard, mouse_state.clone());
    let mut drag = CdpDragObserver::new(tab, world);
    let drag_result = refact_browser::drag_and_drop(
        &mut mouse,
        &mut drag,
        &source.output.1.frame_id,
        source.output.2,
        target.output.2,
        2,
    );
    *mouse_state = mouse.state();
    let _ = world.release_handle(tab, &source.output.1);
    let _ = world.release_handle(tab, &target.output.1);
    match drag_result {
        Ok(()) => {
            let mut result = StepResult::success(
                idx,
                format!("Dragged <{}> to <{}>", source.output.0, target.output.0),
            );
            result.retries = source.attempts + target.attempts;
            result
        }
        Err(error) => StepResult::failure(idx, "Drag and drop failed", error.to_string()),
    }
}

pub(crate) fn step_drop_files(
    tab: &Tab,
    world: &WorldManager,
    idx: usize,
    target: &BrowserLocator,
    paths: &[String],
    handlers: Option<&Arc<Mutex<LocatorHandlerRegistry>>>,
    firings: &mut Vec<LocatorHandlerFiring>,
    image_policy: &ImagePolicy,
) -> StepResult {
    let resolved = match resolve_drag_endpoint(
        tab,
        world,
        target,
        None,
        ActionKind::DragTarget,
        ActionabilityExecutionMode::Standard,
        handlers,
        firings,
        image_policy,
    ) {
        Ok(resolved) => resolved,
        Err(error) => {
            let mut result = StepResult::failure(
                idx,
                "File drop target failed",
                actionability_error_text(&error, target),
            );
            result.actionability = Some(error.diagnostics(ActionKind::DragTarget));
            return result;
        }
    };
    let outcome = refact_browser::drop_files(tab, world, &resolved.output.1, paths);
    let _ = world.release_handle(tab, &resolved.output.1);
    let result = match outcome {
        Ok(result) => result,
        Err(error) => return StepResult::failure(idx, "File drop failed", error),
    };
    let data = serde_json::to_value(&result).unwrap_or_default();
    match refact_browser::verify_file_drop(&result, paths) {
        Ok(()) => {
            StepResult::success(idx, format!("Dropped {} file(s)", paths.len())).with_data(data)
        }
        Err(error) => {
            let mut failure = StepResult::failure(idx, "File drop rejected", error);
            failure.data = Some(data);
            failure
        }
    }
}

pub(crate) fn browser_mouse_button(button: BrowserMouseButton) -> MouseButton {
    match button {
        BrowserMouseButton::Left => MouseButton::Left,
        BrowserMouseButton::Middle => MouseButton::Middle,
        BrowserMouseButton::Right => MouseButton::Right,
    }
}

pub(crate) fn step_mouse(
    tab: &Tab,
    idx: usize,
    state: &mut MouseState,
    operation: impl FnOnce(
        &mut Mouse<'_, CdpMouseDispatcher<'_>, CdpKeyboardDispatcher<'_>>,
    ) -> Result<(), refact_browser::MouseError>,
    summary: String,
) -> StepResult {
    let keyboard = Keyboard::new(CdpKeyboardDispatcher::new(tab));
    let mut mouse = Mouse::from_state(CdpMouseDispatcher::new(tab), &keyboard, state.clone());
    match operation(&mut mouse) {
        Ok(()) => {
            *state = mouse.state();
            StepResult::success(idx, summary)
        }
        Err(error) => StepResult::failure(idx, "Coordinate mouse action failed", error.to_string()),
    }
}

pub(crate) fn step_set_input_files(
    tab: &Tab,
    world: &WorldManager,
    idx: usize,
    locator: &BrowserLocator,
    paths: &[String],
) -> StepResult {
    let resolved = match resolve_scoped_element(tab, world, locator) {
        Ok(resolved) => resolved,
        Err(error) => return StepResult::failure(idx, "Set input files: resolution failed", error),
    };
    match refact_browser::files::set_input_files(tab, world, &resolved.handle, paths, "direct") {
        Ok(upload) => StepResult::success(
            idx,
            format!("Set {} file(s) on <input>", upload.paths.len()),
        )
        .with_data(serde_json::to_value(upload).unwrap_or_default()),
        Err(error) => StepResult::failure(idx, "Set input files failed", error),
    }
}

pub(crate) fn step_navigate(tab: &Tab, idx: usize, url: &str, timeout_ms: u64) -> StepResult {
    match run_and_wait_for_navigation(tab, timeout_ms, || trigger_page_navigation(tab, url)) {
        Ok(warning) => navigation_step_success(idx, format!("Navigated to {}", url), warning),
        Err(e) => StepResult::failure(idx, format!("Navigate to {}", url), e.to_string()),
    }
}

pub(crate) fn step_nav_js(tab: &Tab, idx: usize, js: &str, success_msg: &str) -> StepResult {
    let settings = browser_settings::current();
    match run_and_wait_for_navigation(tab, settings.timing.default_wait_timeout_ms, || {
        let target = js_navigation_target(tab, js)?;
        tab.evaluate(js, false)
            .map_err(|error| format!("JS navigation trigger failed: {error}"))?;
        Ok(target)
    }) {
        Ok(warning) => navigation_step_success(idx, success_msg.to_string(), warning),
        Err(error) => StepResult::failure(idx, success_msg.to_string(), error),
    }
}

pub(crate) fn merge_step_data(mut result: StepResult, extra: serde_json::Value) -> StepResult {
    match (result.data.as_mut(), extra) {
        (Some(Value::Object(existing)), Value::Object(extra)) => existing.extend(extra),
        (_, extra) => result.data = Some(extra),
    }
    result
}

pub(crate) fn navigation_step_success(
    idx: usize,
    summary: String,
    warning: Option<NavigationTimeoutWarning>,
) -> StepResult {
    match warning {
        Some(warning) => StepResult::success(idx, format!("{summary} ({warning})")).with_data(
            serde_json::json!({
                "load_event": "not_observed",
                "ready_state": warning.ready_state,
                "waited_ms": warning.timeout_ms,
            }),
        ),
        None => StepResult::success(idx, summary),
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum NavigationWaitTarget {
    Loader {
        frame_id: Page::FrameId,
        loader_id: Option<String>,
    },
    Triggered {
        frame_id: Page::FrameId,
        previous_loader_id: String,
        expected_url: Option<String>,
        allow_same_document: bool,
    },
    NoNavigation,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum NavigationWaitOutcome {
    Completed,
    TimedOut {
        committed: bool,
        expected_url: Option<String>,
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum NavigationEvent {
    Load {
        frame_id: Page::FrameId,
        loader_id: String,
    },
    FrameNavigated {
        frame_id: Page::FrameId,
        loader_id: String,
        url: String,
        restored_from_back_forward_cache: bool,
    },
    SameDocument {
        frame_id: Page::FrameId,
        url: String,
    },
}

pub(crate) fn trigger_page_navigation(tab: &Tab, url: &str) -> Result<NavigationWaitTarget, String> {
    let response = tab
        .call_method(Page::Navigate {
            url: url.to_string(),
            referrer: None,
            transition_Type: None,
            frame_id: None,
            referrer_policy: None,
        })
        .map_err(|error| format!("CDP Page.navigate failed: {error}"))?;
    if let Some(error) = response.error_text {
        return Err(format!("CDP Page.navigate failed: {error}"));
    }
    Ok(NavigationWaitTarget::Loader {
        frame_id: response.frame_id,
        loader_id: response.loader_id,
    })
}

pub(crate) fn js_navigation_target(tab: &Tab, js: &str) -> Result<NavigationWaitTarget, String> {
    let frame = tab
        .call_method(Page::GetFrameTree(None))
        .map_err(|error| format!("Failed to read the main frame before navigation: {error}"))?
        .frame_tree
        .frame;
    let expected_url = match js {
        "history.back()" => history_target_url(tab, -1)?,
        "history.forward()" => history_target_url(tab, 1)?,
        _ => None,
    };
    if matches!(js, "history.back()" | "history.forward()") && expected_url.is_none() {
        return Ok(NavigationWaitTarget::NoNavigation);
    }
    Ok(NavigationWaitTarget::Triggered {
        frame_id: frame.id,
        previous_loader_id: frame.loader_id,
        expected_url,
        allow_same_document: js != "location.reload()",
    })
}

pub(crate) fn history_target_url(tab: &Tab, offset: i64) -> Result<Option<String>, String> {
    let history = tab
        .call_method(Page::GetNavigationHistory(None))
        .map_err(|error| format!("Failed to read browser navigation history: {error}"))?;
    let target_index = i64::from(history.current_index) + offset;
    if target_index < 0 {
        return Ok(None);
    }
    Ok(history
        .entries
        .get(target_index as usize)
        .map(|entry| entry.url.clone()))
}

pub(crate) fn page_frame_url(frame: &Page::Frame) -> String {
    format!(
        "{}{}",
        frame.url,
        frame.url_fragment.as_deref().unwrap_or_default()
    )
}

pub(crate) fn document_ready_state(tab: &Tab) -> String {
    eval_js_value(tab, "document.readyState")
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_else(|| "unknown".to_string())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NavigationTimeoutWarning {
    pub(crate) ready_state: String,
    pub(crate) timeout_ms: u64,
}

impl std::fmt::Display for NavigationTimeoutWarning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{NAVIGATION_LIFECYCLE_EVENT} event not observed within {}ms; document.readyState={} — continuing",
            self.timeout_ms, self.ready_state
        )
    }
}

pub(crate) fn classify_navigation_timeout(
    ready_state: &str,
    committed: bool,
    timeout_ms: u64,
) -> Option<NavigationTimeoutWarning> {
    let document_readable = matches!(ready_state, "loading" | "interactive" | "complete");
    if !document_readable {
        return None;
    }
    if !matches!(ready_state, "interactive" | "complete") && !committed {
        return None;
    }
    Some(NavigationTimeoutWarning {
        ready_state: ready_state.to_string(),
        timeout_ms,
    })
}

pub(crate) fn run_and_wait_for_navigation(
    tab: &Tab,
    timeout_ms: u64,
    trigger: impl FnOnce() -> Result<NavigationWaitTarget, String>,
) -> Result<Option<NavigationTimeoutWarning>, String> {
    let (sender, receiver) = mpsc::channel();
    let listener = tab
        .add_event_listener(Arc::new(move |event: &Event| match event {
            Event::PageLifecycleEvent(event) if event.params.name == NAVIGATION_LIFECYCLE_EVENT => {
                let _ = sender.send(NavigationEvent::Load {
                    frame_id: event.params.frame_id.clone(),
                    loader_id: event.params.loader_id.clone(),
                });
            }
            Event::PageFrameNavigated(event) => {
                let frame = &event.params.frame;
                let _ = sender.send(NavigationEvent::FrameNavigated {
                    frame_id: frame.id.clone(),
                    loader_id: frame.loader_id.clone(),
                    url: page_frame_url(frame),
                    restored_from_back_forward_cache: matches!(
                        &event.params.Type,
                        Page::NavigationType::BackForwardCacheRestore
                    ),
                });
            }
            Event::PageNavigatedWithinDocument(event) => {
                let _ = sender.send(NavigationEvent::SameDocument {
                    frame_id: event.params.frame_id.clone(),
                    url: event.params.url.clone(),
                });
            }
            _ => {}
        }))
        .map_err(|error| format!("Failed to listen for CDP navigation events: {error}"))?;
    let result = trigger().and_then(|target| {
        wait_for_navigation_event(&receiver, &target, Duration::from_millis(timeout_ms))
    });
    let removal = tab
        .remove_event_listener(&listener)
        .map_err(|error| format!("Failed to remove CDP navigation listener: {error}"));
    let outcome = result?;
    removal?;
    match outcome {
        NavigationWaitOutcome::Completed => Ok(None),
        NavigationWaitOutcome::TimedOut {
            committed,
            expected_url,
            message,
        } => {
            let committed = committed || expected_url.is_some_and(|url| url == tab.get_url());
            let ready_state = document_ready_state(tab);
            match classify_navigation_timeout(&ready_state, committed, timeout_ms) {
                Some(warning) => Ok(Some(warning)),
                None if !matches!(ready_state.as_str(), "loading" | "interactive" | "complete") => {
                    Err(format!(
                        "{message}; document.readyState could not be read ({ready_state}), so the page is not evaluable"
                    ))
                }
                None => Err(message),
            }
        }
    }
}

pub(crate) fn wait_for_navigation_event(
    receiver: &Receiver<NavigationEvent>,
    target: &NavigationWaitTarget,
    timeout: Duration,
) -> Result<NavigationWaitOutcome, String> {
    let (frame_id, mut pending_loader_id, previous_loader_id, expected_url, allow_same_document) =
        match target {
            NavigationWaitTarget::NoNavigation => return Ok(NavigationWaitOutcome::Completed),
            NavigationWaitTarget::Loader {
                frame_id: _,
                loader_id: None,
            } => return Ok(NavigationWaitOutcome::Completed),
            NavigationWaitTarget::Loader {
                frame_id,
                loader_id: Some(loader_id),
            } => (frame_id, Some(loader_id.clone()), None, None, false),
            NavigationWaitTarget::Triggered {
                frame_id,
                previous_loader_id,
                expected_url,
                allow_same_document,
            } => (
                frame_id,
                None,
                Some(previous_loader_id),
                expected_url.as_ref(),
                *allow_same_document,
            ),
        };
    let deadline = Instant::now() + timeout;
    let mut loaded = std::collections::HashSet::new();
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match receiver.recv_timeout(remaining) {
            Ok(NavigationEvent::Load {
                frame_id: completed_frame,
                loader_id,
            }) if completed_frame == *frame_id => {
                if pending_loader_id.as_ref() == Some(&loader_id) {
                    return Ok(NavigationWaitOutcome::Completed);
                }
                loaded.insert(loader_id);
            }
            Ok(NavigationEvent::FrameNavigated {
                frame_id: completed_frame,
                loader_id,
                url,
                restored_from_back_forward_cache,
            }) if completed_frame == *frame_id
                && previous_loader_id.is_some_and(|previous| previous != &loader_id)
                && expected_url.is_none_or(|expected| expected == &url) =>
            {
                if restored_from_back_forward_cache || loaded.contains(&loader_id) {
                    return Ok(NavigationWaitOutcome::Completed);
                }
                pending_loader_id = Some(loader_id);
            }
            Ok(NavigationEvent::SameDocument {
                frame_id: completed_frame,
                url,
            }) if completed_frame == *frame_id
                && previous_loader_id.is_some()
                && allow_same_document
                && expected_url.is_none_or(|expected| expected == &url) =>
            {
                return Ok(NavigationWaitOutcome::Completed);
            }
            Ok(_) => {}
            Err(RecvTimeoutError::Timeout) => {
                let missing = pending_loader_id.as_ref().map_or_else(
                    || {
                        format!(
                            "CDP Page.frameNavigated or Page.navigatedWithinDocument for frame {frame_id}"
                        )
                    },
                    |loader_id| {
                        format!(
                            "CDP Page.lifecycleEvent({NAVIGATION_LIFECYCLE_EVENT}) for frame {frame_id} and loader {loader_id}"
                        )
                    },
                );
                return Ok(NavigationWaitOutcome::TimedOut {
                    committed: pending_loader_id.is_some(),
                    expected_url: expected_url.cloned(),
                    message: format!(
                        "Timed out after {}ms waiting for {missing}",
                        timeout.as_millis()
                    ),
                });
            }
            Err(RecvTimeoutError::Disconnected) => {
                return Err(format!(
                    "CDP navigation listener disconnected for frame {frame_id}"
                ));
            }
        }
    }
}

pub(crate) fn step_locator_action(
    tab: &Tab,
    world: &WorldManager,
    idx: usize,
    locator: &BrowserLocator,
    action: &str,
    handlers: Option<&Arc<Mutex<LocatorHandlerRegistry>>>,
    locator_handler_firings: &mut Vec<LocatorHandlerFiring>,
    image_policy: &ImagePolicy,
) -> StepResult {
    step_locator_action_with_timeout(
        tab,
        world,
        idx,
        locator,
        action,
        handlers,
        locator_handler_firings,
        image_policy,
        None,
    )
}

pub(crate) fn step_locator_action_with_timeout(
    tab: &Tab,
    world: &WorldManager,
    idx: usize,
    locator: &BrowserLocator,
    action: &str,
    handlers: Option<&Arc<Mutex<LocatorHandlerRegistry>>>,
    locator_handler_firings: &mut Vec<LocatorHandlerFiring>,
    image_policy: &ImagePolicy,
    timeout_ms: Option<u64>,
) -> StepResult {
    if let Some(action_kind) = match action {
        "click" => Some(ActionKind::Click),
        "hover" => Some(ActionKind::Hover),
        "tap" => Some(ActionKind::Tap),
        "focus" => Some(ActionKind::Focus),
        "scroll_to" => Some(ActionKind::ScrollIntoViewIfNeeded),
        _ => None,
    } {
        let timeout = timeout_ms
            .map(|ms| Duration::from_millis(clamp_timeout_ms(Some(ms))))
            .unwrap_or_else(|| ActionabilityTimeouts::default().action);
        return step_actionable_action_in_mode(
            tab,
            world,
            idx,
            locator,
            action,
            action_kind,
            handlers,
            locator_handler_firings,
            image_policy,
            ActionabilityExecutionMode::Standard,
            timeout,
        );
    }
    match resolve_interactable(tab, world, locator) {
        Ok(info) => {
            let action_js = match action {
                "focus" => browser_locators::js_focus_element().to_string(),
                "blur" => browser_locators::js_blur_element().to_string(),
                "scroll_to" => browser_locators::js_scroll_to_element().to_string(),
                _ => {
                    return StepResult::failure(idx, action, format!("Unknown action: {}", action))
                }
            };
            match call_handle_json(tab, world, &info.handle, &action_js) {
                Ok(_) => StepResult::success(
                    idx,
                    format!(
                        "{} on <{}> ({})",
                        action,
                        info.tag,
                        describe_locator(locator)
                    ),
                ),
                Err(e) => StepResult::failure(idx, format!("{} failed", action), e),
            }
        }
        Err(e) => StepResult::failure(idx, format!("{} failed", action), e),
    }
}

pub(crate) fn actionability_error_text(
    error: &refact_browser::ActionabilityError,
    locator: &BrowserLocator,
) -> String {
    format!("{}: {}", describe_locator(locator), error.headline())
}

pub(crate) fn step_actionable_action(
    tab: &Tab,
    world: &WorldManager,
    idx: usize,
    locator: &BrowserLocator,
    action: &str,
    action_kind: ActionKind,
    handlers: Option<&Arc<Mutex<LocatorHandlerRegistry>>>,
    locator_handler_firings: &mut Vec<LocatorHandlerFiring>,
    image_policy: &ImagePolicy,
) -> StepResult {
    step_actionable_action_in_mode(
        tab,
        world,
        idx,
        locator,
        action,
        action_kind,
        handlers,
        locator_handler_firings,
        image_policy,
        ActionabilityExecutionMode::Standard,
        ActionabilityTimeouts::default().action,
    )
}

pub(crate) fn step_actionable_action_in_mode(
    tab: &Tab,
    world: &WorldManager,
    idx: usize,
    locator: &BrowserLocator,
    action: &str,
    action_kind: ActionKind,
    handlers: Option<&Arc<Mutex<LocatorHandlerRegistry>>>,
    locator_handler_firings: &mut Vec<LocatorHandlerFiring>,
    image_policy: &ImagePolicy,
    mode: ActionabilityExecutionMode,
    timeout: Duration,
) -> StepResult {
    let engine = ActionabilityEngine::new(SystemClock::default(), ActionabilityTimeouts::default());
    let mut driver = BrowserActionDriver::new(
        tab,
        world,
        locator,
        action_kind,
        handlers,
        locator_handler_firings,
        image_policy,
        timeout,
    );
    match engine.execute_with_timeout_in_mode(
        &describe_locator(locator),
        action_kind,
        timeout,
        mode,
        &mut driver,
    ) {
        Ok(success) => {
            let mut result = StepResult::success(
                idx,
                format!(
                    "{action} on <{}> ({})",
                    success.output,
                    describe_locator(locator)
                ),
            );
            result.retries = success.attempts;
            if success.attempts > 0 {
                result.actionability = Some(success.diagnostics(action_kind));
            }
            result.locator_echo = driver.locator_echo.take();
            result
        }
        Err(error) => {
            let diagnostics = error.diagnostics(action_kind);
            let mut result = StepResult::failure(
                idx,
                format!("{action} failed"),
                actionability_error_text(&error, locator),
            );
            result.retries = diagnostics.attempts.unwrap_or_default();
            result.actionability = Some(diagnostics);
            result
        }
    }
}

pub(crate) fn click_if_exists_skip_reason(state: &refact_browser::ElementState) -> Option<&'static str> {
    let required = required_states(ActionKind::Click);
    if required.visible && !state.visible {
        Some("element is not visible")
    } else if required.enabled && !state.enabled {
        Some("element is not enabled")
    } else if required.stable && !state.stable {
        Some("element is not stable")
    } else {
        None
    }
}

pub(crate) fn probe_click_if_exists(
    tab: &Tab,
    world: &WorldManager,
    locator: &BrowserLocator,
) -> Result<(), String> {
    let resolved = match resolve_element_typed(tab, world, locator) {
        Ok(resolved) => resolved,
        Err(ResolveElementError::MultipleMatches { count, .. }) => {
            return Err(format!("locator resolved to {count} elements"));
        }
        Err(ResolveElementError::Other(message)) => return Err(message),
    };
    let state = world.element_states(tab, &resolved.handle);
    let _ = world.release_handle(tab, &resolved.handle);
    match state {
        Ok(state) => match click_if_exists_skip_reason(&state) {
            Some(reason) => Err(format!("not actionable, {reason}")),
            None => Ok(()),
        },
        Err(error) => Err(format!("not actionable, {error}")),
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ClickIfExistsProbe {
    Proceed,
    Skip(String),
    Fatal(String),
}

pub(crate) fn classify_click_if_exists_probe(probe: Result<(), String>) -> ClickIfExistsProbe {
    match probe {
        Ok(()) => ClickIfExistsProbe::Proceed,
        Err(error) if refact_browser::is_transport_dead_error(&error) => {
            ClickIfExistsProbe::Fatal(error)
        }
        Err(error) => ClickIfExistsProbe::Skip(error),
    }
}

pub(crate) fn click_if_exists_action_result(mut result: StepResult) -> StepResult {
    let error = result
        .error
        .clone()
        .unwrap_or_else(|| result.summary.clone());
    if result.ok || refact_browser::is_transport_dead_error(&error) {
        return result;
    }
    let mut data = result.data.take().unwrap_or_else(|| serde_json::json!({}));
    if let Some(object) = data.as_object_mut() {
        object.insert("clicked".to_string(), serde_json::Value::Bool(false));
        object.insert(
            "skip_reason".to_string(),
            serde_json::Value::String(error.clone()),
        );
    }
    StepResult {
        ok: true,
        summary: format!("Click skipped (non-fatal): {error}"),
        error: None,
        data: Some(data),
        ..result
    }
}

pub(crate) fn step_click_if_exists(
    tab: &Tab,
    world: &WorldManager,
    idx: usize,
    locator: &BrowserLocator,
    handlers: Option<&Arc<Mutex<LocatorHandlerRegistry>>>,
    locator_handler_firings: &mut Vec<LocatorHandlerFiring>,
    image_policy: &ImagePolicy,
) -> StepResult {
    match classify_click_if_exists_probe(probe_click_if_exists(tab, world, locator)) {
        ClickIfExistsProbe::Proceed => {}
        ClickIfExistsProbe::Skip(reason) => {
            return StepResult::success(
                idx,
                format!(
                    "Skipped click_if_exists ({}): {reason}",
                    describe_locator(locator)
                ),
            );
        }
        ClickIfExistsProbe::Fatal(error) => {
            return StepResult::failure(
                idx,
                format!("click_if_exists failed ({})", describe_locator(locator)),
                error,
            );
        }
    }
    click_if_exists_action_result(step_actionable_action(
        tab,
        world,
        idx,
        locator,
        "click",
        ActionKind::Click,
        handlers,
        locator_handler_firings,
        image_policy,
    ))
}

pub(crate) fn step_press_key(tab: &Tab, idx: usize, key: &str, modifiers: &[String]) -> StepResult {
    let mut tokens = Vec::with_capacity(modifiers.len() + 1);
    for modifier in modifiers {
        match modifier.as_str() {
            "Alt" | "Meta" | "Shift" => tokens.push(modifier.as_str()),
            "Ctrl" | "Control" => tokens.push("Control"),
            _ => {
                return StepResult::failure(
                    idx,
                    format!("Press key {key}"),
                    format!("Unknown key: \"{modifier}\""),
                )
            }
        }
    }
    tokens.push(key);
    let chord = tokens.join("+");
    let dispatcher = CdpKeyboardDispatcher::new(tab);
    let mut keyboard = Keyboard::new(dispatcher);
    match keyboard.press(&chord, None) {
        Ok(_) => {
            let mod_str = if modifiers.is_empty() {
                String::new()
            } else {
                format!("{}+", modifiers.join("+"))
            };
            StepResult::success(idx, format!("Pressed {}{}", mod_str, key))
        }
        Err(e) => StepResult::failure(idx, format!("Press key {}", key), e.to_string()),
    }
}

pub(crate) enum TapTarget<'a> {
    Element(&'a BrowserLocator),
    Point(f64, f64),
}

pub(crate) fn tap_target(
    locator: Option<&BrowserLocator>,
    x: Option<f64>,
    y: Option<f64>,
) -> Result<TapTarget<'_>, &'static str> {
    match (locator, x, y) {
        (Some(locator), None, None) => Ok(TapTarget::Element(locator)),
        (None, Some(x), Some(y)) => Ok(TapTarget::Point(x, y)),
        _ => Err(TAP_AMBIGUOUS_TARGET),
    }
}

pub(crate) fn touch_emulation_enabled(tab: &Tab) -> bool {
    eval_js_value(
        tab,
        "navigator.maxTouchPoints > 0 || 'ontouchstart' in window",
    )
    .ok()
    .and_then(|value| value.as_bool())
    .unwrap_or(false)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn step_tap(
    tab: &Tab,
    world: &WorldManager,
    idx: usize,
    locator: Option<&BrowserLocator>,
    x: Option<f64>,
    y: Option<f64>,
    handlers: Option<&Arc<Mutex<LocatorHandlerRegistry>>>,
    locator_handler_firings: &mut Vec<LocatorHandlerFiring>,
    image_policy: &ImagePolicy,
    mouse_state: &mut MouseState,
) -> StepResult {
    let target = match tap_target(locator, x, y) {
        Ok(target) => target,
        Err(error) => return StepResult::failure(idx, "Tap failed", error),
    };
    if !touch_emulation_enabled(tab) {
        return StepResult::failure(idx, "Tap failed", TAP_REQUIRES_TOUCH);
    }
    match target {
        TapTarget::Element(locator) => step_actionable_action(
            tab,
            world,
            idx,
            locator,
            "tap",
            ActionKind::Tap,
            handlers,
            locator_handler_firings,
            image_policy,
        ),
        TapTarget::Point(x, y) => step_mouse(
            tab,
            idx,
            mouse_state,
            |mouse| mouse.tap(x, y),
            format!("Tapped at ({x}, {y})"),
        ),
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn focus_for_keyboard(
    tab: &Tab,
    world: &WorldManager,
    idx: usize,
    locator: &BrowserLocator,
    action: &str,
    handlers: Option<&Arc<Mutex<LocatorHandlerRegistry>>>,
    locator_handler_firings: &mut Vec<LocatorHandlerFiring>,
    image_policy: &ImagePolicy,
) -> Option<StepResult> {
    let result = step_actionable_action(
        tab,
        world,
        idx,
        locator,
        action,
        ActionKind::Focus,
        handlers,
        locator_handler_firings,
        image_policy,
    );
    (!result.ok).then_some(result)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn step_insert_text(
    tab: &Tab,
    world: &WorldManager,
    idx: usize,
    locator: Option<&BrowserLocator>,
    text: &str,
    handlers: Option<&Arc<Mutex<LocatorHandlerRegistry>>>,
    locator_handler_firings: &mut Vec<LocatorHandlerFiring>,
    image_policy: &ImagePolicy,
) -> StepResult {
    if let Some(locator) = locator {
        if let Some(failure) = focus_for_keyboard(
            tab,
            world,
            idx,
            locator,
            "insert_text",
            handlers,
            locator_handler_firings,
            image_policy,
        ) {
            return failure;
        }
    }
    let mut keyboard = Keyboard::new(CdpKeyboardDispatcher::new(tab));
    match keyboard.insert_text(text) {
        Ok(()) => StepResult::success(
            idx,
            format!("Inserted {} chars without key events", text.chars().count()),
        ),
        Err(error) => StepResult::failure(idx, "Insert text failed", error),
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn step_press_sequentially(
    tab: &Tab,
    world: &WorldManager,
    idx: usize,
    locator: &BrowserLocator,
    text: &str,
    delay_ms: Option<u64>,
    handlers: Option<&Arc<Mutex<LocatorHandlerRegistry>>>,
    locator_handler_firings: &mut Vec<LocatorHandlerFiring>,
    image_policy: &ImagePolicy,
) -> StepResult {
    if let Some(failure) = focus_for_keyboard(
        tab,
        world,
        idx,
        locator,
        "press_sequentially",
        handlers,
        locator_handler_firings,
        image_policy,
    ) {
        return failure;
    }
    let delay = delay_ms
        .filter(|delay_ms| *delay_ms > 0)
        .map(Duration::from_millis);
    let mut keyboard = Keyboard::new(CdpKeyboardDispatcher::new(tab));
    match keyboard.press_sequentially(text, delay) {
        Ok(()) => StepResult::success(
            idx,
            format!(
                "Pressed {} chars sequentially into ({})",
                text.chars().count(),
                describe_locator(locator)
            ),
        ),
        Err(error) => StepResult::failure(idx, "Press sequentially failed", error),
    }
}

pub(crate) fn step_fill(
    tab: &Tab,
    world: &WorldManager,
    idx: usize,
    locator: &BrowserLocator,
    text: &str,
    clear_first: bool,
    verify: bool,
) -> StepResult {
    let info = match resolve_scoped_element(tab, world, locator) {
        Ok(i) => i,
        Err(e) => return StepResult::failure(idx, "Fill: element resolution failed", e),
    };
    match refact_browser::forms::fill(
        tab,
        world,
        &info.handle,
        &info.info,
        text,
        clear_first,
        verify,
    ) {
        Ok(outcome) => {
            let mut result = StepResult::success(
                idx,
                format!("Filled <{}> with {} chars", info.tag, text.len()),
            );
            result.locator_echo = generate_locator_echo(tab, world, &info.handle);
            result.field_kind = Some(info.field_kind.clone());
            result.fill_strategy = Some(outcome.strategy);
            result.verified = outcome.verified;
            result.retries = outcome.retries;
            result.actionability = outcome.actionability;
            result
        }
        Err(error) => {
            let mut result = StepResult::failure(
                idx,
                format!("Fill failed after {} strategies", error.retries),
                error.message,
            );
            result.field_kind = Some(info.field_kind.clone());
            result.retries = error.retries;
            result.actionability = error.actionability;
            result
        }
    }
}

pub(crate) fn step_clear(
    tab: &Tab,
    world: &WorldManager,
    idx: usize,
    locator: &BrowserLocator,
    verify: bool,
) -> StepResult {
    let info = match resolve_scoped_element(tab, world, locator) {
        Ok(i) => i,
        Err(e) => return StepResult::failure(idx, "Clear: element resolution failed", e),
    };
    match refact_browser::forms::clear(tab, world, &info.handle, &info.info, verify) {
        Ok(outcome) => {
            let mut result = StepResult::success(idx, format!("Cleared <{}>", info.tag));
            result.locator_echo = generate_locator_echo(tab, world, &info.handle);
            result.field_kind = Some(info.field_kind.clone());
            result.fill_strategy = outcome.strategy;
            result.verified = outcome.verified;
            result.retries = outcome.retries;
            result.actionability = outcome.actionability;
            result
        }
        Err(error) => {
            let mut result = StepResult::failure(idx, "Clear failed", error.message);
            result.field_kind = Some(info.field_kind.clone());
            result.retries = error.retries;
            result.actionability = error.actionability;
            result
        }
    }
}

pub(crate) fn step_select_option(
    tab: &Tab,
    world: &WorldManager,
    idx: usize,
    locator: &BrowserLocator,
    value: &str,
) -> StepResult {
    match resolve_scoped_element(tab, world, locator) {
        Ok(info) => match refact_browser::forms::select_option(tab, world, &info.handle, value) {
            Ok(outcome) => {
                let mut result =
                    StepResult::success(idx, format!("Selected '{}' in <{}>", value, info.tag))
                        .with_data(serde_json::json!({"selected": outcome.selected}))
                        .with_locator_echo(generate_locator_echo(tab, world, &info.handle));
                result.actionability = outcome.actionability;
                result
            }
            Err(error) => {
                let mut result = StepResult::failure(idx, "Select option failed", error.message);
                result.retries = error.retries;
                result.actionability = error.actionability;
                result
            }
        },
        Err(e) => StepResult::failure(idx, "Select: element resolution failed", e),
    }
}

pub(crate) fn step_check_uncheck(
    tab: &Tab,
    world: &WorldManager,
    idx: usize,
    locator: &BrowserLocator,
    check: bool,
) -> StepResult {
    let action = if check { "check" } else { "uncheck" };
    let info = match resolve_scoped_element(tab, world, locator) {
        Ok(i) => i,
        Err(e) => return StepResult::failure(idx, "Check/uncheck: resolution failed", e),
    };
    match refact_browser::forms::set_checked(tab, world, &info.handle, check) {
        Ok(outcome) => {
            let mut result = StepResult::success(idx, format!("{}ed <{}>", action, info.tag))
                .with_data(serde_json::json!({
                    "checked": outcome.checked,
                    "changed": outcome.changed,
                    "verified": outcome.verified,
                }))
                .with_locator_echo(generate_locator_echo(tab, world, &info.handle));
            result.actionability = outcome.actionability;
            result
        }
        Err(error) => {
            let mut result = StepResult::failure(idx, format!("{} failed", action), error.message);
            result.retries = error.retries;
            result.actionability = error.actionability;
            result
        }
    }
}

pub(crate) fn poll_deadline_error(last_error: Option<String>, timeout_message: String) -> String {
    match last_error {
        Some(error) => format!("Polling failed: {error}"),
        None => timeout_message,
    }
}

pub(crate) fn poll_condition(
    tab: &Tab,
    js_condition: &str,
    timeout_ms: u64,
    interval_ms: u64,
) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    let mut last_error: Option<String>;
    loop {
        match eval_js_value(tab, js_condition) {
            Ok(val) if val.as_bool() == Some(true) => return Ok(()),
            Ok(_) => last_error = None,
            Err(error) if refact_browser::is_transport_dead_error(&error) => {
                return Err(format!("Polling failed: {error}"));
            }
            Err(error) => last_error = Some(error),
        }
        if Instant::now() >= deadline {
            return Err(poll_deadline_error(
                last_error,
                format!("Timed out after {}ms", timeout_ms),
            ));
        }
        std::thread::sleep(Duration::from_millis(interval_ms));
    }
}

pub(crate) fn wait_for_report_stability(
    tab: &Tab,
    world: &WorldManager,
    network_monitor: &NetworkMonitorHandle,
    timeout_ms: u64,
) -> bool {
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    if poll_until_deadline(tab, "document.readyState !== 'loading'", deadline).is_err() {
        return false;
    }
    let remaining = deadline.saturating_duration_since(Instant::now());
    if network_monitor
        .wait_for_load_state(NetworkLoadState::Networkidle, remaining)
        .is_err()
    {
        return false;
    }

    let remaining = deadline.saturating_duration_since(Instant::now());
    let max_checks = (remaining.as_millis() / REPORT_STABILITY_INTERVAL_MS as u128) as usize;
    html_stabilizes_with(
        max_checks.max(1),
        || {
            world.eval_in_utility(
                tab,
                "document.documentElement ? document.documentElement.outerHTML : ''",
            )
        },
        || std::thread::sleep(Duration::from_millis(REPORT_STABILITY_INTERVAL_MS)),
    )
}

pub(crate) fn html_stabilizes_with<F, S>(max_checks: usize, mut sample: F, mut sleep: S) -> bool
where
    F: FnMut() -> Result<serde_json::Value, String>,
    S: FnMut(),
{
    let mut previous: Option<serde_json::Value> = None;
    for _ in 0..max_checks {
        let current = match sample() {
            Ok(value) => value,
            Err(_) => return false,
        };
        if previous.as_ref() == Some(&current) {
            return true;
        }
        previous = Some(current);
        sleep();
    }
    false
}

pub(crate) fn poll_until_deadline(tab: &Tab, js_condition: &str, deadline: Instant) -> Result<(), String> {
    loop {
        if eval_js_value(tab, js_condition)
            .ok()
            .and_then(|value| value.as_bool())
            == Some(true)
        {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err("Timed out".to_string());
        }
        std::thread::sleep(Duration::from_millis(REPORT_STABILITY_INTERVAL_MS));
    }
}

pub(crate) fn mask_console_entry(
    mut entry: refact_integrations::browser_types::ConsoleEntry,
) -> refact_integrations::browser_types::ConsoleEntry {
    entry.text = refact_core::string_utils::redact_sensitive(&entry.text);
    entry
}

pub(crate) fn console_entry_matches(
    entry: &refact_integrations::browser_types::ConsoleEntry,
    contains: Option<&str>,
    level: Option<BrowserConsoleLevel>,
) -> bool {
    let text_matches = contains.is_none_or(|needle| entry.text.contains(needle));
    let level_matches = level.is_none_or(|level| level.matches_level(&entry.level));
    text_matches && level_matches
}

pub(crate) async fn wait_for_console_message(
    runtime_arc: &Arc<AMutex<BrowserRuntime>>,
    idx: usize,
    contains: Option<&str>,
    level: Option<BrowserConsoleLevel>,
    armed_cursor: usize,
    timeout_ms: u64,
) -> StepResult {
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    let mut cursor = armed_cursor;
    loop {
        let matched = {
            let mut rt = runtime_arc.lock().await;
            rt.drain_raw_events();
            cursor = cursor.min(rt.console_buffer.len());
            let found = rt.console_buffer[cursor..]
                .iter()
                .position(|entry| console_entry_matches(entry, contains, level));
            match found {
                Some(offset) => {
                    let entry = rt.console_buffer[cursor + offset].clone();
                    cursor += offset + 1;
                    Some(mask_console_entry(entry))
                }
                None => {
                    cursor = rt.console_buffer.len();
                    None
                }
            }
        };
        if let Some(entry) = matched {
            return StepResult::success(
                idx,
                format!("Matched console {}: {}", entry.level, entry.text),
            )
            .with_data(serde_json::to_value(entry).unwrap_or_default());
        }
        if Instant::now() >= deadline {
            return StepResult::failure(
                idx,
                "Wait for console message",
                format!("Timed out after {timeout_ms}ms"),
            );
        }
        tokio::time::sleep(Duration::from_millis(CONSOLE_POLL_INTERVAL_MS)).await;
    }
}

pub(crate) fn wait_for_selector_matches(
    state: BrowserWaitState,
    attached: usize,
    visible: usize,
) -> Option<usize> {
    match state {
        BrowserWaitState::Attached => (attached > 0).then_some(attached),
        BrowserWaitState::Detached => (attached == 0).then_some(0),
        BrowserWaitState::Visible => (visible > 0).then_some(visible),
        BrowserWaitState::Hidden => (visible == 0).then_some(attached),
    }
}

pub(crate) fn handle_is_visible(
    tab: &Tab,
    world: &WorldManager,
    handle: &ElementHandle,
) -> Result<bool, String> {
    world
        .element_state(tab, handle, ElementStateName::Visible)
        .map_err(|error| error.to_string())
        .map(|value| value.get("matches") == Some(&serde_json::Value::Bool(true)))
}

pub(crate) fn count_visible_handles(
    tab: &Tab,
    world: &WorldManager,
    handles: &[ElementHandle],
) -> Result<usize, String> {
    let mut visible = 0;
    for handle in handles {
        if handle_is_visible(tab, world, handle)? {
            visible += 1;
        }
    }
    Ok(visible)
}

pub(crate) fn step_wait_for_selector(
    tab: &Tab,
    world: &WorldManager,
    idx: usize,
    locator: &BrowserLocator,
    state: Option<BrowserWaitState>,
    timeout_ms: u64,
) -> StepResult {
    let state = state.unwrap_or_default();
    match poll_locator_until(tab, world, locator, timeout_ms, |handles| {
        let visible = if state.needs_visibility() {
            count_visible_handles(tab, world, &handles)
        } else {
            Ok(0)
        };
        release_locator_handles(tab, world, &handles);
        Ok(wait_for_selector_matches(state, handles.len(), visible?))
    }) {
        Ok(matched) => StepResult::success(
            idx,
            format!(
                "{} ({}), {matched} match(es)",
                state.reached_label(),
                describe_locator(locator)
            ),
        ),
        Err(e) => StepResult::failure(
            idx,
            format!("Wait for selector ({})", describe_locator(locator)),
            e,
        ),
    }
}

pub(crate) fn navigation_load_state_step(idx: usize, summary: String, wait: Result<(), String>) -> StepResult {
    match wait {
        Ok(()) => StepResult::success(idx, summary),
        Err(error) if refact_browser::is_transport_dead_error(&error) => {
            StepResult::failure(idx, summary, error)
        }
        Err(error) => {
            StepResult::success(idx, format!("{summary} (load state not reached: {error})"))
                .with_data(serde_json::json!({"load_state_reached": false}))
        }
    }
}

pub(crate) fn step_wait_for_navigation(
    tab: &Tab,
    idx: usize,
    timeout_ms: u64,
    pre_step_url: Option<&str>,
) -> StepResult {
    let settings = browser_settings::current();
    let current_url = tab.get_url();
    let reference_url = pre_step_url.unwrap_or(&current_url);
    let complete_js = r#"(function() { return document.readyState === 'complete'; })()"#;

    if current_url != reference_url {
        return navigation_load_state_step(
            idx,
            format!("Navigation detected: {} -> {}", reference_url, current_url),
            poll_condition(
                tab,
                complete_js,
                timeout_ms,
                settings.timing.default_poll_interval_ms,
            ),
        );
    }

    let url_changed_js = format!(
        r#"(function() {{ return window.location.href !== {}; }})()"#,
        js_string_literal(reference_url),
    );

    match poll_condition(
        tab,
        &url_changed_js,
        timeout_ms,
        settings.timing.default_poll_interval_ms,
    ) {
        Ok(()) => {
            let end_url = tab.get_url();
            navigation_load_state_step(
                idx,
                format!("Navigation detected: {} -> {}", reference_url, end_url),
                poll_condition(
                    tab,
                    complete_js,
                    timeout_ms,
                    settings.timing.default_poll_interval_ms,
                ),
            )
        }
        Err(error) => StepResult::failure(
            idx,
            "Wait for navigation",
            format!("{error}; URL unchanged ({current_url})"),
        ),
    }
}

pub(crate) fn wait_for_url_matcher(pattern: &UrlPattern) -> Result<UrlMatcher, String> {
    match pattern {
        UrlPattern::Text(value) => UrlMatcher::substring(value),
        UrlPattern::Regex { source, flags } => UrlMatcher::regex(source, flags),
    }
}

pub(crate) fn describe_url_pattern(pattern: &UrlPattern) -> String {
    match pattern {
        UrlPattern::Text(value) => format!("'{value}'"),
        UrlPattern::Regex { source, flags } => format!("/{source}/{flags}"),
    }
}

pub(crate) fn poll_url_match(tab: &Tab, matcher: &UrlMatcher, timeout_ms: u64) -> Result<String, String> {
    let settings = browser_settings::current();
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    let mut last_error: Option<String>;
    loop {
        match eval_js_value(tab, "window.location.href") {
            Ok(value) => {
                let url = value.as_str().unwrap_or_default().to_string();
                if matcher.is_match(&url) {
                    return Ok(url);
                }
                last_error = None;
            }
            Err(error) if refact_browser::is_transport_dead_error(&error) => {
                return Err(format!("Polling failed: {error}"));
            }
            Err(error) => last_error = Some(error),
        }
        if Instant::now() >= deadline {
            return Err(poll_deadline_error(
                last_error,
                format!("Timed out after {}ms", timeout_ms),
            ));
        }
        std::thread::sleep(Duration::from_millis(
            settings.timing.default_poll_interval_ms,
        ));
    }
}

pub(crate) fn step_wait_for_url(tab: &Tab, idx: usize, pattern: &UrlPattern, timeout_ms: u64) -> StepResult {
    let matcher = match wait_for_url_matcher(pattern) {
        Ok(matcher) => matcher,
        Err(error) => return StepResult::failure(idx, "Invalid URL pattern", error),
    };
    let description = describe_url_pattern(pattern);
    match poll_url_match(tab, &matcher, timeout_ms) {
        Ok(url) => StepResult::success(idx, format!("URL matches {description}: {url}")),
        Err(error) => {
            StepResult::failure(idx, format!("Wait for URL matching {description}"), error)
        }
    }
}

pub(crate) fn step_wait_for_text(tab: &Tab, idx: usize, text: &str, timeout_ms: u64) -> StepResult {
    let settings = browser_settings::current();
    let js = browser_locators::js_check_text_present(text);
    match poll_condition(
        tab,
        &js,
        timeout_ms,
        settings.timing.default_poll_interval_ms,
    ) {
        Ok(()) => StepResult::success(idx, format!("Text '{}' found on page", text)),
        Err(e) => StepResult::failure(idx, format!("Wait for text '{}'", text), e),
    }
}

pub(crate) fn step_wait_for_element_hidden(
    tab: &Tab,
    world: &WorldManager,
    idx: usize,
    locator: &BrowserLocator,
    timeout_ms: u64,
) -> StepResult {
    match poll_locator_until(tab, world, locator, timeout_ms, |handles| {
        let Some(handle) = strict_locator_handle(tab, world, locator, handles)? else {
            return Ok(Some(()));
        };
        let handle = world.wrap_scoped(tab, handle);
        handle_is_visible(tab, world, &handle).map(|visible| (!visible).then_some(()))
    }) {
        Ok(()) => StepResult::success(idx, "Element is hidden".to_string()),
        Err(e) => StepResult::failure(idx, "Wait for element hidden", e),
    }
}

pub(crate) fn injected_state_is_stable(state: &Value) -> bool {
    state
        .get("stable")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

pub(crate) fn step_wait_for_element_stable(
    tab: &Tab,
    world: &WorldManager,
    idx: usize,
    locator: &BrowserLocator,
    timeout_ms: u64,
) -> StepResult {
    match poll_locator_until(tab, world, locator, timeout_ms, |handles| {
        let Some(handle) = strict_locator_handle(tab, world, locator, handles)? else {
            return Ok(None);
        };
        let handle = world.wrap_scoped(tab, handle);
        let state = world
            .element_state(tab, &handle, ElementStateName::Stable)
            .map_err(|error| error.to_string())?;
        Ok(injected_state_is_stable(&state).then_some(()))
    }) {
        Ok(()) => StepResult::success(idx, "Element is stable".to_string()),
        Err(error) => StepResult::failure(idx, "Wait for element stable", error),
    }
}

pub(crate) fn poll_locator_until<T>(
    tab: &Tab,
    world: &WorldManager,
    locator: &BrowserLocator,
    timeout_ms: u64,
    mut sample: impl FnMut(Vec<ElementHandle>) -> Result<Option<T>, String>,
) -> Result<T, String> {
    let settings = browser_settings::current();
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    let mut last_error: Option<String>;
    loop {
        match resolve_locator_handles(tab, world, locator).and_then(&mut sample) {
            Ok(Some(value)) => return Ok(value),
            Ok(None) => last_error = None,
            Err(error) if refact_browser::is_transport_dead_error(&error) => return Err(error),
            Err(error) if is_transient_page_error(&error) => last_error = Some(error),
            Err(error) => return Err(error),
        }
        if Instant::now() >= deadline {
            return Err(poll_deadline_error(
                last_error,
                format!("Timed out after {}ms", timeout_ms),
            ));
        }
        std::thread::sleep(Duration::from_millis(
            settings.timing.default_poll_interval_ms,
        ));
    }
}

pub(crate) fn is_transient_page_error(error: &str) -> bool {
    let lower = error.to_ascii_lowercase();
    [
        "cannot find context",
        "execution context was destroyed",
        "inspected target navigated",
        "no node with given id",
        "could not find object with given id",
        "could not find node with given id",
        "cannot find object",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
}

pub(crate) fn step_wait_seconds(idx: usize, seconds: f64) -> StepResult {
    let ms = (seconds * 1000.0) as u64;
    std::thread::sleep(Duration::from_millis(ms));
    StepResult::success(idx, format!("Waited {:.1}s", seconds))
}

pub(crate) const NETWORK_IDLE_WINDOW_MS: u64 = refact_browser::NETWORK_IDLE_MS;

pub(crate) const NETWORK_INFLIGHT_TRACKER_JS: &str = r#"(function() {
  if (window.__refact_inflight_installed) return;
  window.__refact_inflight_installed = true;
  window.__refact_inflight = 0;
  var origFetch = window.fetch;
  if (typeof origFetch === 'function') {
    window.fetch = function() {
      window.__refact_inflight++;
      var p = origFetch.apply(this, arguments);
      var done = function() { window.__refact_inflight = Math.max(0, window.__refact_inflight - 1); };
      return p.then(function(r) { done(); return r; }, function(e) { done(); throw e; });
    };
  }
  var XHR = window.XMLHttpRequest;
  if (typeof XHR === 'function') {
    var origSend = XHR.prototype.send;
    XHR.prototype.send = function() {
      window.__refact_inflight++;
      var self = this;
      var done = false;
      var finish = function() {
        if (done) return;
        done = true;
        window.__refact_inflight = Math.max(0, window.__refact_inflight - 1);
      };
      self.addEventListener('loadend', finish);
      self.addEventListener('error', finish);
      self.addEventListener('abort', finish);
      self.addEventListener('timeout', finish);
      return origSend.apply(this, arguments);
    };
  }
})()"#;

pub(crate) fn step_wait_for_network_idle(tab: &Tab, idx: usize, timeout_ms: u64) -> StepResult {
    let settings = browser_settings::current();
    let _ = tab.evaluate(NETWORK_INFLIGHT_TRACKER_JS, false);

    let snapshot_js = r#"(function() {
  var inflight = window.__refact_inflight_installed ? (window.__refact_inflight | 0) : -1;
  return JSON.stringify({inflight: inflight, ready: document.readyState});
})()"#;

    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    let idle_window = Duration::from_millis(NETWORK_IDLE_WINDOW_MS);
    let poll = Duration::from_millis(settings.timing.default_poll_interval_ms);
    let mut idle_since: Option<Instant> = None;

    loop {
        let snapshot = eval_js_value(tab, snapshot_js).unwrap_or(serde_json::Value::Null);
        let (inflight, ready) = match snapshot.as_str() {
            Some(s) => {
                let parsed: serde_json::Value =
                    serde_json::from_str(s).unwrap_or(serde_json::Value::Null);
                let i = parsed
                    .get("inflight")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(-1);
                let r = parsed
                    .get("ready")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                (i, r)
            }
            None => (-1, String::new()),
        };

        let is_idle = inflight == 0 && ready == "complete";
        if is_idle {
            if let Some(since) = idle_since {
                if Instant::now().duration_since(since) >= idle_window {
                    return StepResult::success(
                        idx,
                        format!(
                            "Network idle (inflight=0, readyState=complete, window={}ms)",
                            NETWORK_IDLE_WINDOW_MS
                        ),
                    );
                }
            } else {
                idle_since = Some(Instant::now());
            }
        } else {
            idle_since = None;
        }

        if Instant::now() >= deadline {
            return StepResult::failure(
                idx,
                "Wait for network idle",
                format!(
                    "Timed out after {}ms (inflight={}, readyState={})",
                    timeout_ms, inflight, ready
                ),
            );
        }
        std::thread::sleep(poll);
    }
}
