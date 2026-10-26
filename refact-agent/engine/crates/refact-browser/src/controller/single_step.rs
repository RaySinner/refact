use super::*;

use super::actions::{browser_mouse_button, poll_deadline_error, step_actionable_action_in_mode, step_check_uncheck, step_clear, step_click_if_exists, step_drag_and_drop, step_drop_files, step_fill, step_insert_text, step_locator_action, step_locator_action_with_timeout, step_mouse, step_nav_js, step_navigate, step_press_key, step_press_sequentially, step_select_option, step_set_input_files, step_tap, step_wait_for_element_hidden, step_wait_for_element_stable, step_wait_for_navigation, step_wait_for_network_idle, step_wait_for_selector, step_wait_for_text, step_wait_for_url, step_wait_seconds, uses_actionability_engine};
use super::capture::{step_accessibility_snapshot, step_all_texts, step_bounding_box, step_capture_element_states, step_count, step_dom_snapshot, step_element_state, step_extract_links, step_extract_table, step_get_attribute, step_get_html, step_get_text, step_input_value, step_screenshot, step_screenshot_element, step_screenshot_elements};
use super::context::{clamp_timeout_ms, clamp_wait_seconds, eval_js_ok, eval_js_value};
use super::eval_steps::{dismiss_overlays, step_add_locator_handler, step_add_script_tag, step_add_style_tag, step_dismiss_overlays, step_dispatch_event, step_eval, step_hide_highlight, step_highlight, step_remove_locator_handler, step_set_content, step_styles, step_tab_log};
use super::expect_poll::{step_expect, step_expect_poll, step_wait_for_function};


pub(crate) fn execute_runtime_network_step(
    tab: &Tab,
    world: &WorldManager,
    network_monitor: &NetworkMonitorHandle,
    step: &BrowserStep,
    idx: usize,
    armed_cursor: Option<u64>,
    image_policy: &ImagePolicy,
    handlers: &Arc<Mutex<LocatorHandlerRegistry>>,
    locator_handler_firings: &mut Vec<LocatorHandlerFiring>,
    mouse_state: &mut MouseState,
) -> StepResult {
    match step {
        BrowserStep::SetContent { html, wait_until } => {
            step_set_content(tab, world, network_monitor, idx, html, *wait_until)
        }
        BrowserStep::WaitForNetworkIdle { timeout_ms } => {
            wait_for_network_idle_step(network_monitor, idx, clamp_timeout_ms(*timeout_ms))
        }
        BrowserStep::WaitForLoadState { state, timeout_ms } => {
            wait_for_load_state(network_monitor, idx, *state, clamp_timeout_ms(*timeout_ms))
        }
        BrowserStep::WaitForRequest {
            pattern,
            method,
            timeout_ms,
        } => wait_for_network_entry(
            network_monitor,
            idx,
            pattern,
            &NetworkWaitFilters {
                method: method.clone(),
                status: None,
            },
            armed_cursor.unwrap_or_else(|| network_monitor.request_cursor()),
            clamp_timeout_ms(*timeout_ms),
            false,
        ),
        BrowserStep::WaitForResponse {
            pattern,
            method,
            status,
            timeout_ms,
        } => wait_for_network_entry(
            network_monitor,
            idx,
            pattern,
            &NetworkWaitFilters {
                method: method.clone(),
                status: *status,
            },
            armed_cursor.unwrap_or_else(|| network_monitor.response_cursor()),
            clamp_timeout_ms(*timeout_ms),
            true,
        ),
        _ => execute_single_step(
            tab,
            world,
            step,
            idx,
            None,
            image_policy,
            Some(handlers),
            locator_handler_firings,
            mouse_state,
        ),
    }
}

pub(crate) fn wait_for_load_state(
    monitor: &NetworkMonitorHandle,
    idx: usize,
    state: BrowserLoadState,
    timeout_ms: u64,
) -> StepResult {
    let state_name = match state {
        BrowserLoadState::Domcontentloaded => "domcontentloaded",
        BrowserLoadState::Load => "load",
        BrowserLoadState::Networkidle => "networkidle",
    };
    wait_for_load_state_labelled(
        monitor,
        idx,
        state,
        timeout_ms,
        &format!("Wait for load state {state_name}"),
    )
}

pub(crate) fn wait_for_network_idle_step(
    monitor: &NetworkMonitorHandle,
    idx: usize,
    timeout_ms: u64,
) -> StepResult {
    wait_for_load_state_labelled(
        monitor,
        idx,
        BrowserLoadState::Networkidle,
        timeout_ms,
        "Wait for network idle",
    )
}

pub(crate) fn wait_for_load_state_labelled(
    monitor: &NetworkMonitorHandle,
    idx: usize,
    state: BrowserLoadState,
    timeout_ms: u64,
    summary: &str,
) -> StepResult {
    let state_name = match state {
        BrowserLoadState::Domcontentloaded => "domcontentloaded",
        BrowserLoadState::Load => "load",
        BrowserLoadState::Networkidle => "networkidle",
    };
    let monitor_state = match state {
        BrowserLoadState::Domcontentloaded => NetworkLoadState::Domcontentloaded,
        BrowserLoadState::Load => NetworkLoadState::Load,
        BrowserLoadState::Networkidle => NetworkLoadState::Networkidle,
    };
    match monitor.wait_for_load_state(monitor_state, Duration::from_millis(timeout_ms)) {
        Ok(()) => StepResult::success(idx, format!("Reached load state {state_name}")),
        Err(error) => StepResult::failure(idx, summary, error),
    }
}

pub(crate) fn wait_for_network_entry(
    monitor: &NetworkMonitorHandle,
    idx: usize,
    pattern: &UrlPattern,
    filters: &NetworkWaitFilters,
    cursor: u64,
    timeout_ms: u64,
    response: bool,
) -> StepResult {
    let matcher = match pattern {
        UrlPattern::Text(value) => UrlMatcher::text(value),
        UrlPattern::Regex { source, flags } => UrlMatcher::regex(source, flags),
    };
    let matcher = match matcher {
        Ok(matcher) => matcher,
        Err(error) => return StepResult::failure(idx, "Invalid URL pattern", error),
    };
    let result = if response {
        monitor.wait_for_response(&matcher, filters, cursor, Duration::from_millis(timeout_ms))
    } else {
        monitor.wait_for_request(&matcher, filters, cursor, Duration::from_millis(timeout_ms))
    };
    match result {
        Ok(entry) => {
            let kind = if response { "response" } else { "request" };
            StepResult::success(
                idx,
                format!("Matched {kind}: {} {}", entry.method, entry.url),
            )
            .with_data(serde_json::to_value(entry).unwrap_or_default())
        }
        Err(error) => {
            let kind = if response { "response" } else { "request" };
            StepResult::failure(idx, format!("Wait for {kind}"), error)
        }
    }
}

pub(crate) fn needs_locator_handler_checkpoint(step: &BrowserStep) -> bool {
    matches!(
        step,
        BrowserStep::Click { .. }
            | BrowserStep::ClickIfExists { .. }
            | BrowserStep::Tap {
                locator: Some(_),
                ..
            }
            | BrowserStep::Hover { .. }
            | BrowserStep::DragAndDrop { .. }
            | BrowserStep::DropFiles { .. }
            | BrowserStep::Focus { .. }
            | BrowserStep::InsertText {
                locator: Some(_),
                ..
            }
            | BrowserStep::PressSequentially { .. }
            | BrowserStep::Blur { .. }
            | BrowserStep::ScrollTo { .. }
            | BrowserStep::Fill { .. }
            | BrowserStep::Clear { .. }
            | BrowserStep::SelectOption { .. }
            | BrowserStep::Check { .. }
            | BrowserStep::Uncheck { .. }
            | BrowserStep::WaitForSelector { .. }
            | BrowserStep::WaitForText { .. }
            | BrowserStep::WaitForElementHidden { .. }
            | BrowserStep::WaitForElementStable { .. }
            | BrowserStep::Expect {
                locator: Some(_),
                ..
            }
            | BrowserStep::GetText { .. }
            | BrowserStep::GetHtml { .. }
            | BrowserStep::GetAttribute { .. }
            | BrowserStep::BoundingBox { .. }
            | BrowserStep::InputValue { .. }
            | BrowserStep::ElementState { .. }
            | BrowserStep::ScreenshotElement { .. }
            | BrowserStep::ScreenshotElements { .. }
            | BrowserStep::CaptureElementStates { .. }
            | BrowserStep::Styles { .. }
            | BrowserStep::HighlightElement { .. }
            | BrowserStep::Highlight { .. }
            | BrowserStep::Annotate { .. }
    )
}

pub(crate) fn perform_action_prechecks(
    tab: &Tab,
    world: &WorldManager,
    handlers: &Arc<Mutex<LocatorHandlerRegistry>>,
    firings: &mut Vec<LocatorHandlerFiring>,
    image_policy: &ImagePolicy,
    deadline: Instant,
) -> Result<(), String> {
    wait_for_pending_navigation(tab, deadline)?;
    perform_locator_handlers_checkpoint(tab, world, handlers, firings, image_policy, deadline)?;
    wait_for_pending_navigation(tab, deadline)
}

pub(crate) fn wait_for_pending_navigation(tab: &Tab, deadline: Instant) -> Result<(), String> {
    let settings = browser_settings::current();
    let mut last_error: Option<String>;
    loop {
        match eval_js_value(tab, "document.readyState === 'loading'") {
            Ok(value) => {
                if !value.as_bool().unwrap_or(false) {
                    return Ok(());
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
                "Timed out waiting for navigation before action".to_string(),
            ));
        }
        std::thread::sleep(
            Duration::from_millis(settings.timing.default_poll_interval_ms)
                .min(deadline.saturating_duration_since(Instant::now())),
        );
    }
}

pub(crate) fn perform_locator_handlers_checkpoint(
    tab: &Tab,
    world: &WorldManager,
    handlers: &Arc<Mutex<LocatorHandlerRegistry>>,
    firings: &mut Vec<LocatorHandlerFiring>,
    image_policy: &ImagePolicy,
    deadline: Instant,
) -> Result<(), String> {
    let handler_names = {
        let registry = handlers
            .lock()
            .map_err(|error| format!("Failed to lock locator handlers: {error}"))?;
        if registry.is_running() {
            return Ok(());
        }
        registry
            .handlers()
            .iter()
            .map(|handler| handler.name.clone())
            .collect::<Vec<_>>()
    };

    for name in handler_names {
        if Instant::now() >= deadline {
            return Err("Timed out while running locator handlers".to_string());
        }
        let handler = {
            handlers
                .lock()
                .map_err(|error| format!("Failed to lock locator handlers: {error}"))?
                .get(&name)
        };
        let Some(handler) = handler else {
            continue;
        };
        match probe_locator_handler(tab, world, &handler)? {
            LocatorHandlerProbe::Hidden | LocatorHandlerProbe::MultipleMatches { .. } => continue,
            LocatorHandlerProbe::Visible => {}
        }
        let lease = {
            handlers
                .lock()
                .map_err(|error| format!("Failed to lock locator handlers: {error}"))?
                .begin(&name)
        };
        let Some(lease) = lease else {
            continue;
        };
        let result = execute_locator_handler(
            tab,
            world,
            handlers,
            firings,
            image_policy,
            deadline,
            &lease,
            false,
        );
        let (ok, outcome) = match &result {
            Ok(outcome) => (true, outcome.clone()),
            Err(error) => (false, error.clone()),
        };
        let firing = handlers
            .lock()
            .map_err(|error| format!("Failed to lock locator handlers: {error}"))?
            .finish(lease, ok, outcome);
        firings.push(firing);
        result?;
    }
    Ok(())
}

pub(crate) fn probe_locator_handler(
    tab: &Tab,
    world: &WorldManager,
    handler: &LocatorHandler,
) -> Result<LocatorHandlerProbe, String> {
    if matches!(handler.operation, LocatorHandlerOperation::DismissOverlays) {
        return eval_js_ok(tab, &browser_locators::js_dismiss_overlays(true, false)).map(
            |result| {
                if result
                    .get("count")
                    .and_then(|value| value.as_u64())
                    .unwrap_or(0)
                    == 0
                {
                    LocatorHandlerProbe::Hidden
                } else {
                    LocatorHandlerProbe::Visible
                }
            },
        );
    }
    let locator = serde_json::to_value(&handler.locator)
        .map_err(|error| format!("Failed to serialize locator handler: {error}"))?;
    let handles = world
        .call_injected_handles(tab, "resolveAll", serde_json::json!([locator]))
        .map_err(|error| error.to_string())?;
    if handles.is_empty() {
        return Ok(LocatorHandlerProbe::Hidden);
    }
    if handles.len() > 1 {
        let count = handles.len();
        for handle in &handles {
            let _ = world.release_handle(tab, handle);
        }
        return Ok(LocatorHandlerProbe::MultipleMatches { count });
    }
    let handle = &handles[0];
    let inspect = format!(
        "function() {{ {INSPECT_ELEMENT_JS} return JSON.stringify(__refact_inspect_element(this, 1)); }}"
    );
    let value = world
        .call_function_on(tab, handle, &inspect, Vec::new())
        .map_err(|error| error.to_string());
    let _ = world.release_handle(tab, handle);
    let value = value?;
    let json = value
        .as_str()
        .map(str::to_string)
        .unwrap_or_else(|| value.to_string());
    let info = parse_element_info(&json)?;
    Ok(if info.visible {
        LocatorHandlerProbe::Visible
    } else {
        LocatorHandlerProbe::Hidden
    })
}

pub(crate) fn execute_locator_handler(
    tab: &Tab,
    world: &WorldManager,
    handlers: &Arc<Mutex<LocatorHandlerRegistry>>,
    firings: &mut Vec<LocatorHandlerFiring>,
    image_policy: &ImagePolicy,
    deadline: Instant,
    lease: &LocatorHandlerLease,
    aggressive: bool,
) -> Result<String, String> {
    let outcome = match &lease.handler.operation {
        LocatorHandlerOperation::DismissOverlays => dismiss_overlays(tab, aggressive),
        LocatorHandlerOperation::Action(LocatorHandlerAction::Click) => {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err("Timed out while running locator handler click".to_string());
            }
            let result = step_actionable_action_in_mode(
                tab,
                world,
                0,
                &lease.handler.locator,
                "click",
                ActionKind::Click,
                Some(handlers),
                firings,
                image_policy,
                ActionabilityExecutionMode::SkipLocatorHandlers,
                remaining,
            );
            if result.ok {
                Ok(result.summary)
            } else {
                Err(result.error.unwrap_or(result.summary))
            }
        }
        LocatorHandlerOperation::Action(LocatorHandlerAction::Steps { steps }) => {
            let mut summaries = Vec::new();
            for step in steps {
                if Instant::now() >= deadline {
                    return Err("Timed out while running locator handler steps".to_string());
                }
                let bounded_step = bound_handler_step(step, deadline);
                let result = execute_single_step(
                    tab,
                    world,
                    &bounded_step,
                    0,
                    None,
                    image_policy,
                    Some(handlers),
                    firings,
                    &mut MouseState::default(),
                );
                if !result.ok {
                    return Err(result.error.unwrap_or(result.summary));
                }
                summaries.push(result.summary);
            }
            Ok(summaries.join("; "))
        }
    }?;

    if !lease.handler.no_wait_after {
        wait_for_handler_hidden(tab, world, &lease.handler, deadline)?;
    }
    Ok(outcome)
}

pub(crate) fn bound_handler_step(step: &BrowserStep, deadline: Instant) -> BrowserStep {
    let remaining_ms = deadline
        .saturating_duration_since(Instant::now())
        .as_millis()
        .min(u64::MAX as u128) as u64;
    match step {
        BrowserStep::WaitForSelector { locator, state, .. } => BrowserStep::WaitForSelector {
            locator: locator.clone(),
            state: *state,
            timeout_ms: Some(remaining_ms),
        },
        BrowserStep::WaitForNavigation { .. } => BrowserStep::WaitForNavigation {
            timeout_ms: Some(remaining_ms),
        },
        BrowserStep::WaitForUrl { pattern, .. } => BrowserStep::WaitForUrl {
            pattern: pattern.clone(),
            timeout_ms: Some(remaining_ms),
        },
        BrowserStep::WaitForText { text, .. } => BrowserStep::WaitForText {
            text: text.clone(),
            timeout_ms: Some(remaining_ms),
        },
        BrowserStep::Expect {
            locator,
            matcher,
            soft,
            not,
            ..
        } => BrowserStep::Expect {
            locator: locator.clone(),
            matcher: matcher.clone(),
            timeout_ms: Some(remaining_ms),
            soft: *soft,
            not: *not,
        },
        BrowserStep::WaitForNetworkIdle { .. } => BrowserStep::WaitForNetworkIdle {
            timeout_ms: Some(remaining_ms),
        },
        BrowserStep::WaitForElementHidden { locator, .. } => BrowserStep::WaitForElementHidden {
            locator: locator.clone(),
            timeout_ms: Some(remaining_ms),
        },
        BrowserStep::WaitForElementStable { locator, .. } => BrowserStep::WaitForElementStable {
            locator: locator.clone(),
            timeout_ms: Some(remaining_ms),
        },
        BrowserStep::WaitSeconds { seconds } => BrowserStep::WaitSeconds {
            seconds: seconds.min(remaining_ms as f64 / 1_000.0),
        },
        _ => step.clone(),
    }
}

pub(crate) fn wait_for_handler_hidden(
    tab: &Tab,
    world: &WorldManager,
    handler: &LocatorHandler,
    deadline: Instant,
) -> Result<(), String> {
    let settings = browser_settings::current();
    loop {
        if matches!(
            probe_locator_handler(tab, world, handler)?,
            LocatorHandlerProbe::Hidden
        ) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "Timed out waiting for locator handler '{}' to become hidden",
                handler.name
            ));
        }
        std::thread::sleep(
            Duration::from_millis(settings.timing.default_poll_interval_ms)
                .min(deadline.saturating_duration_since(Instant::now())),
        );
    }
}

pub(crate) fn execute_single_step(
    tab: &Tab,
    world: &WorldManager,
    step: &BrowserStep,
    idx: usize,
    pre_step_url: Option<&str>,
    image_policy: &ImagePolicy,
    handlers: Option<&Arc<Mutex<LocatorHandlerRegistry>>>,
    locator_handler_firings: &mut Vec<LocatorHandlerFiring>,
    mouse_state: &mut MouseState,
) -> StepResult {
    let settings = browser_settings::current();
    if needs_locator_handler_checkpoint(step) && !uses_actionability_engine(step) {
        if let Some(handlers) = handlers {
            if let Err(error) = perform_action_prechecks(
                tab,
                world,
                handlers,
                locator_handler_firings,
                image_policy,
                Instant::now() + Duration::from_millis(settings.timing.default_wait_timeout_ms),
            ) {
                return StepResult::failure(idx, "Locator handler checkpoint failed", error);
            }
        }
    }
    match step {
        BrowserStep::Navigate { url, timeout_ms } => {
            step_navigate(tab, idx, url, clamp_timeout_ms(*timeout_ms))
        }
        BrowserStep::Reload => step_nav_js(tab, idx, "location.reload()", "Reloaded page"),
        BrowserStep::GoBack => step_nav_js(tab, idx, "history.back()", "Navigated back"),
        BrowserStep::GoForward => step_nav_js(tab, idx, "history.forward()", "Navigated forward"),

        BrowserStep::OpenTab { .. }
        | BrowserStep::CloseTab { .. }
        | BrowserStep::SwitchTab { .. }
        | BrowserStep::ListTabs
        | BrowserStep::WaitForPopup { .. }
        | BrowserStep::Route { .. }
        | BrowserStep::Unroute { .. }
        | BrowserStep::ListRoutes
        | BrowserStep::RouteWebSocket { .. }
        | BrowserStep::UnrouteWebSocket { .. }
        | BrowserStep::SendWebSocketMessage { .. }
        | BrowserStep::CloseWebSocket { .. }
        | BrowserStep::WaitForWebSocketFrame { .. }
        | BrowserStep::StartHarRecording { .. }
        | BrowserStep::StopHarRecording
        | BrowserStep::RouteFromHar { .. }
        | BrowserStep::Reset
        | BrowserStep::CdpSend { .. }
        | BrowserStep::CaptureFrames { .. }
        | BrowserStep::ScreencastStart { .. }
        | BrowserStep::ScreencastStop { .. }
        | BrowserStep::HttpRequest { .. }
        | BrowserStep::StartCoverage { .. }
        | BrowserStep::StopCoverage
        | BrowserStep::AddVirtualAuthenticator { .. }
        | BrowserStep::RemoveVirtualAuthenticator { .. }
        | BrowserStep::ListCredentials { .. }
        | BrowserStep::AddCredential { .. }
        | BrowserStep::ClearCredentials { .. }
        | BrowserStep::SetUserVerified { .. }
        | BrowserStep::SetViewport { .. }
        | BrowserStep::SetWindowBounds { .. }
        | BrowserStep::EmulateMedia { .. }
        | BrowserStep::SetLocale { .. }
        | BrowserStep::SetTimezone { .. }
        | BrowserStep::SetUserAgent { .. }
        | BrowserStep::SetGeolocation { .. }
        | BrowserStep::SetOffline { .. }
        | BrowserStep::SetNetworkConditions { .. }
        | BrowserStep::SetCpuThrottling { .. }
        | BrowserStep::EmulateDevice { .. }
        | BrowserStep::ListDevices { .. }
        | BrowserStep::SetExtraHttpHeaders { .. }
        | BrowserStep::GetCookies { .. }
        | BrowserStep::SetCookies { .. }
        | BrowserStep::ClearCookies { .. }
        | BrowserStep::GetStorage { .. }
        | BrowserStep::SetStorage { .. }
        | BrowserStep::ClearStorage { .. }
        | BrowserStep::StorageState { .. }
        | BrowserStep::SetStorageState { .. }
        | BrowserStep::GrantPermissions { .. }
        | BrowserStep::ClearPermissions
        | BrowserStep::SetHttpCredentials { .. }
        | BrowserStep::HandleDialog { .. } => StepResult::failure(
            idx,
            "Runtime management step",
            "Use execute_steps_with_runtime() for runtime management",
        ),

        BrowserStep::Expect {
            locator,
            matcher,
            timeout_ms,
            soft,
            not,
        } => step_expect(
            tab,
            world,
            idx,
            locator.as_ref(),
            matcher,
            timeout_ms.unwrap_or(ActionabilityTimeouts::default().expect.as_millis() as u64),
            *soft,
            not.unwrap_or(false),
        ),

        BrowserStep::ExpectPoll {
            expression,
            expected,
            matcher,
            timeout_ms,
            soft,
        } => step_expect_poll(
            tab,
            idx,
            expression,
            expected,
            *matcher,
            clamp_timeout_ms(*timeout_ms),
            soft.unwrap_or(false),
        ),

        BrowserStep::Click {
            locator,
            timeout_ms,
        } => step_locator_action_with_timeout(
            tab,
            world,
            idx,
            locator,
            "click",
            handlers,
            locator_handler_firings,
            image_policy,
            *timeout_ms,
        ),
        BrowserStep::ClickIfExists { locator } => step_click_if_exists(
            tab,
            world,
            idx,
            locator,
            handlers,
            locator_handler_firings,
            image_policy,
        ),
        BrowserStep::Tap { locator, x, y } => step_tap(
            tab,
            world,
            idx,
            locator.as_ref(),
            *x,
            *y,
            handlers,
            locator_handler_firings,
            image_policy,
            mouse_state,
        ),
        BrowserStep::Hover {
            locator,
            timeout_ms,
        } => step_locator_action_with_timeout(
            tab,
            world,
            idx,
            locator,
            "hover",
            handlers,
            locator_handler_firings,
            image_policy,
            *timeout_ms,
        ),
        BrowserStep::Focus { locator } => step_locator_action(
            tab,
            world,
            idx,
            locator,
            "focus",
            handlers,
            locator_handler_firings,
            image_policy,
        ),
        BrowserStep::Blur { locator } => step_locator_action(
            tab,
            world,
            idx,
            locator,
            "blur",
            handlers,
            locator_handler_firings,
            image_policy,
        ),
        BrowserStep::ScrollTo { locator } => step_locator_action(
            tab,
            world,
            idx,
            locator,
            "scroll_to",
            handlers,
            locator_handler_firings,
            image_policy,
        ),
        BrowserStep::PressKey { key, modifiers } => step_press_key(tab, idx, key, modifiers),
        BrowserStep::InsertText { locator, text } => step_insert_text(
            tab,
            world,
            idx,
            locator.as_ref(),
            text,
            handlers,
            locator_handler_firings,
            image_policy,
        ),
        BrowserStep::PressSequentially {
            locator,
            text,
            delay_ms,
        } => step_press_sequentially(
            tab,
            world,
            idx,
            locator,
            text,
            *delay_ms,
            handlers,
            locator_handler_firings,
            image_policy,
        ),
        BrowserStep::DragAndDrop {
            source,
            target,
            source_position,
            target_position,
        } => step_drag_and_drop(
            tab,
            world,
            idx,
            source,
            target,
            *source_position,
            *target_position,
            handlers,
            locator_handler_firings,
            image_policy,
            mouse_state,
        ),
        BrowserStep::DropFiles { target, paths } => step_drop_files(
            tab,
            world,
            idx,
            target,
            paths,
            handlers,
            locator_handler_firings,
            image_policy,
        ),
        BrowserStep::MouseMove { x, y, steps } => step_mouse(
            tab,
            idx,
            mouse_state,
            |mouse| mouse.move_to(*x, *y, steps.unwrap_or(1)),
            format!("Moved mouse to ({x}, {y})"),
        ),
        BrowserStep::MouseDown { button } => step_mouse(
            tab,
            idx,
            mouse_state,
            |mouse| mouse.down(browser_mouse_button(*button), 1),
            format!("Pressed {button:?} mouse button"),
        ),
        BrowserStep::MouseUp { button } => step_mouse(
            tab,
            idx,
            mouse_state,
            |mouse| mouse.up(browser_mouse_button(*button), 1),
            format!("Released {button:?} mouse button"),
        ),
        BrowserStep::MouseClickXy {
            x,
            y,
            button,
            click_count,
            delay,
        } => step_mouse(
            tab,
            idx,
            mouse_state,
            |mouse| {
                mouse.move_to(*x, *y, 1)?;
                for count in 1..=click_count.unwrap_or(1) {
                    mouse.down(browser_mouse_button(*button), count)?;
                    if let Some(delay) = delay {
                        std::thread::sleep(Duration::from_millis(*delay));
                    }
                    mouse.up(browser_mouse_button(*button), count)?;
                }
                Ok(())
            },
            format!("Clicked at ({x}, {y})"),
        ),
        BrowserStep::MouseDragXy {
            start_x,
            start_y,
            end_x,
            end_y,
        } => step_mouse(
            tab,
            idx,
            mouse_state,
            |mouse| {
                mouse.move_to(*start_x, *start_y, 1)?;
                mouse.down(MouseButton::Left, 1)?;
                mouse.move_to(*end_x, *end_y, 2)?;
                mouse.up(MouseButton::Left, 1)
            },
            format!("Dragged mouse from ({start_x}, {start_y}) to ({end_x}, {end_y})"),
        ),
        BrowserStep::MouseWheel { delta_x, delta_y } => step_mouse(
            tab,
            idx,
            mouse_state,
            |mouse| mouse.wheel(*delta_x, *delta_y),
            format!("Scrolled mouse wheel by ({delta_x}, {delta_y})"),
        ),

        BrowserStep::Fill {
            locator,
            text,
            clear_first,
            verify,
        } => step_fill(tab, world, idx, locator, text, *clear_first, *verify),
        BrowserStep::Clear { locator, verify } => step_clear(tab, world, idx, locator, *verify),
        BrowserStep::SelectOption { locator, value } => {
            step_select_option(tab, world, idx, locator, value)
        }
        BrowserStep::Check { locator } => step_check_uncheck(tab, world, idx, locator, true),
        BrowserStep::Uncheck { locator } => step_check_uncheck(tab, world, idx, locator, false),
        BrowserStep::SetInputFiles { locator, paths } => {
            step_set_input_files(tab, world, idx, locator, paths)
        }
        BrowserStep::ExpectFileChooser { .. } => StepResult::failure(
            idx,
            "Expect file chooser",
            "File chooser flow requires a browser runtime",
        ),

        BrowserStep::WaitForFunction {
            expression,
            locator,
            timeout_ms,
            polling_ms,
        } => step_wait_for_function(
            tab,
            world,
            idx,
            expression,
            locator.as_ref(),
            clamp_timeout_ms(*timeout_ms),
            *polling_ms,
        ),
        BrowserStep::WaitForSelector {
            locator,
            state,
            timeout_ms,
        } => step_wait_for_selector(
            tab,
            world,
            idx,
            locator,
            *state,
            clamp_timeout_ms(*timeout_ms),
        ),
        BrowserStep::WaitForNavigation { timeout_ms } => {
            step_wait_for_navigation(tab, idx, clamp_timeout_ms(*timeout_ms), pre_step_url)
        }
        BrowserStep::WaitForUrl {
            pattern,
            timeout_ms,
        } => step_wait_for_url(tab, idx, pattern, clamp_timeout_ms(*timeout_ms)),
        BrowserStep::WaitForText { text, timeout_ms } => {
            step_wait_for_text(tab, idx, text, clamp_timeout_ms(*timeout_ms))
        }
        BrowserStep::WaitForNetworkIdle { timeout_ms } => {
            step_wait_for_network_idle(tab, idx, clamp_timeout_ms(*timeout_ms))
        }
        BrowserStep::WaitForLoadState { .. }
        | BrowserStep::WaitForRequest { .. }
        | BrowserStep::WaitForResponse { .. }
        | BrowserStep::WaitForDownload { .. }
        | BrowserStep::CancelDownload { .. } => StepResult::failure(
            idx,
            "Network wait",
            "Network waits require a browser runtime",
        ),
        BrowserStep::WaitForConsoleMessage { .. } => StepResult::failure(
            idx,
            "Wait for console message",
            "Console waits require a browser runtime",
        ),
        BrowserStep::WaitForElementHidden {
            locator,
            timeout_ms,
        } => step_wait_for_element_hidden(tab, world, idx, locator, clamp_timeout_ms(*timeout_ms)),
        BrowserStep::WaitForElementStable {
            locator,
            timeout_ms,
        } => step_wait_for_element_stable(tab, world, idx, locator, clamp_timeout_ms(*timeout_ms)),
        BrowserStep::WaitSeconds { seconds } => {
            step_wait_seconds(idx, clamp_wait_seconds(*seconds))
        }

        BrowserStep::GetText { locator } => step_get_text(tab, world, idx, locator),
        BrowserStep::GetHtml { locator } => step_get_html(tab, world, idx, locator),
        BrowserStep::GetAttribute { locator, attribute } => {
            step_get_attribute(tab, world, idx, locator, attribute)
        }
        BrowserStep::BoundingBox { locator } => step_bounding_box(tab, world, idx, locator),
        BrowserStep::Count { locator } => step_count(tab, world, idx, locator),
        BrowserStep::InputValue { locator } => step_input_value(tab, world, idx, locator),
        BrowserStep::AllTexts {
            locator,
            mode,
            limit,
        } => step_all_texts(tab, world, idx, locator, *mode, *limit),
        BrowserStep::ElementState { locator } => step_element_state(tab, world, idx, locator),
        BrowserStep::ExtractLinks { locator, limit } => {
            step_extract_links(tab, world, idx, locator.as_ref(), *limit)
        }
        BrowserStep::ExtractTable { locator, limit } => {
            step_extract_table(tab, world, idx, locator, *limit)
        }
        BrowserStep::DomSnapshot {
            selector,
            max_chars,
        } => step_dom_snapshot(tab, idx, selector, *max_chars),
        BrowserStep::AccessibilitySnapshot { options } => {
            step_accessibility_snapshot(tab, world, idx, options)
        }
        BrowserStep::Screenshot { options } => {
            step_screenshot(tab, world, idx, options, image_policy)
        }
        BrowserStep::ScreenshotElement { locator, options } => {
            step_screenshot_element(tab, world, idx, locator, options, image_policy)
        }
        BrowserStep::ScreenshotElements {
            locators,
            compose,
            labels,
            options,
        } => step_screenshot_elements(
            tab,
            world,
            idx,
            locators,
            *compose,
            *labels,
            options,
            image_policy,
        ),
        BrowserStep::CaptureElementStates {
            locator,
            states,
            labels,
            options,
        } => step_capture_element_states(
            tab,
            world,
            idx,
            locator,
            states,
            *labels,
            options,
            image_policy,
        ),
        BrowserStep::Pdf { .. } => {
            StepResult::failure(idx, "PDF", "PDF generation requires a browser runtime")
        }

        BrowserStep::Eval {
            expression,
            timeout_ms,
        } => step_eval(tab, idx, expression, *timeout_ms),
        BrowserStep::Styles {
            locator,
            property_filter,
        } => step_styles(tab, world, idx, locator, property_filter.as_deref()),

        BrowserStep::SetContent { .. } => StepResult::failure(
            idx,
            "Set content",
            "Setting page content requires a browser runtime",
        ),
        BrowserStep::PageContent => StepResult::failure(
            idx,
            "Page content",
            "Reading page content requires a browser runtime",
        ),
        BrowserStep::AddScriptTag {
            url,
            content,
            script_type,
        } => step_add_script_tag(
            tab,
            idx,
            url.as_deref(),
            content.as_deref(),
            script_type.as_deref(),
        ),
        BrowserStep::AddStyleTag { url, content } => {
            step_add_style_tag(tab, idx, url.as_deref(), content.as_deref())
        }
        BrowserStep::AddInitScript { .. } | BrowserStep::RemoveInitScript { .. } => {
            StepResult::failure(idx, "Init script", "Init scripts require a browser runtime")
        }
        BrowserStep::DispatchEvent {
            locator,
            event_type,
            event_init,
        } => step_dispatch_event(tab, world, idx, locator, event_type, event_init.as_ref()),

        BrowserStep::TabLog => step_tab_log(idx),

        BrowserStep::AddLocatorHandler {
            name,
            locator,
            handler,
            times,
            no_wait_after,
        } => step_add_locator_handler(
            idx,
            handlers,
            name,
            locator,
            handler,
            *times,
            *no_wait_after,
        ),
        BrowserStep::RemoveLocatorHandler { name } => {
            step_remove_locator_handler(idx, handlers, name)
        }

        BrowserStep::DismissOverlays { aggressive } => step_dismiss_overlays(
            tab,
            world,
            idx,
            handlers,
            locator_handler_firings,
            image_policy,
            *aggressive,
        ),
        BrowserStep::HighlightElement { locator } => {
            step_highlight(tab, world, idx, locator, None, None, true)
        }
        BrowserStep::Highlight {
            locator,
            style,
            label,
        } => step_highlight(
            tab,
            world,
            idx,
            locator,
            style.as_deref(),
            label.as_deref(),
            false,
        ),
        BrowserStep::HideHighlight => step_hide_highlight(tab, world, idx),
        BrowserStep::Annotate { locator, text } => {
            step_highlight(tab, world, idx, locator, None, Some(text), false)
        }
        BrowserStep::Help { topic } => {
            StepResult::success(idx, "chrome help").with_data(serde_json::json!({
                "text": crate::help::chrome_help_text(topic.as_deref()),
            }))
        }
        BrowserStep::ClockInstall { .. }
        | BrowserStep::ClockFastForward { .. }
        | BrowserStep::ClockPauseAt { .. }
        | BrowserStep::ClockResume
        | BrowserStep::ClockRunFor { .. }
        | BrowserStep::ClockSetFixedTime { .. }
        | BrowserStep::ClockSetSystemTime { .. } => StepResult::failure(
            idx,
            "Browser clock",
            "Clock steps are session-scoped and run only from a browser request batch",
        ),
    }
}
