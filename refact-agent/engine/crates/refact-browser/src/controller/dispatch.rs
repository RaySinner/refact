use super::*;

use super::actions::{NETWORK_INFLIGHT_TRACKER_JS, mask_console_entry, wait_for_console_message, wait_for_report_stability};
use super::capture::{capture_page_snapshot, capture_report_screenshot, console_counts, is_image_capture_step, notable_main_document_status, persist_image_artifact, report_screenshot_requested, report_snapshot_requested};
use super::context::{REPORT_STABILIZATION_TIMEOUT_MS, clamp_timeout_ms};
use super::eval_steps::{step_page_content, step_pdf, tab_log_from_buffer};
use super::execute::{is_navigation_step, is_non_fatal_step, replaces_document_in_place, step_hit_dead_transport};
use super::single_step::{execute_runtime_network_step, execute_single_step};
use super::steps_runtime::{apply_http_response, cancel_download_step_result, context_summary, download_step_result, execute_cdp_send_step, execute_clock_step, execute_context_management_step, execute_init_script_step, execute_instrumentation_step, execute_reset_step, execute_route_management_step, execute_screencast_step, execute_set_window_bounds_step, http_request_result, is_clock_step, is_context_management_step, is_instrumentation_step, is_screencast_step, prepare_http_request};


pub(crate) fn execute_step(
    tab: &Tab,
    step: &BrowserStep,
    idx: usize,
    image_policy: &ImagePolicy,
) -> StepResult {
    execute_step_with_world(tab, &WorldManager::default(), step, idx, image_policy)
}

pub(crate) fn execute_step_with_world(
    tab: &Tab,
    world: &WorldManager,
    step: &BrowserStep,
    idx: usize,
    image_policy: &ImagePolicy,
) -> StepResult {
    let _ = tab.evaluate(INSPECT_ELEMENT_JS, false);
    let handlers = Arc::new(Mutex::new(LocatorHandlerRegistry::default()));
    let mut mouse_state = MouseState::default();
    let result = execute_single_step(
        tab,
        world,
        step,
        idx,
        None,
        image_policy,
        Some(&handlers),
        &mut Vec::new(),
        &mut mouse_state,
    );
    if result.ok && is_navigation_step(step) {
        let _ = tab.evaluate(INSPECT_ELEMENT_JS, false);
    }
    let _ = world.release_all(tab);
    result
}

pub(crate) struct PendingPopupWait {
    pub(crate) step_index: usize,
    pub(crate) result_index: usize,
    pub(crate) deadline: Instant,
    pub(crate) timeout_ms: u64,
    pub(crate) baseline: std::collections::BTreeSet<String>,
}

impl PendingPopupWait {
    pub(crate) fn arm(
        step_index: usize,
        result_index: usize,
        timeout_ms: Option<u64>,
        baseline: std::collections::BTreeSet<String>,
    ) -> Self {
        let timeout_ms = clamp_timeout_ms(timeout_ms);
        Self {
            step_index,
            result_index,
            deadline: Instant::now() + Duration::from_millis(timeout_ms),
            timeout_ms,
            baseline,
        }
    }
}

pub(crate) fn popup_wait_conflict(armed_step_index: Option<usize>) -> Option<String> {
    armed_step_index.map(|index| format!("a popup wait is already armed at step {index}"))
}

pub(crate) fn popup_wait_opened_result(step_index: usize, tab_id: &str) -> StepResult {
    StepResult::success(step_index, format!("Popup opened: {tab_id}"))
        .with_data(serde_json::json!({"tab_id": tab_id}))
}

pub(crate) fn popup_wait_timeout_result(step_index: usize, timeout_ms: u64) -> StepResult {
    StepResult::failure(
        step_index,
        "Wait for popup",
        format!("Timed out after {timeout_ms}ms"),
    )
}

#[derive(Default)]
pub(crate) struct TabManagementSections {
    pub(crate) new_tabs: Vec<TabInfo>,
    pub(crate) intercepted_requests: Vec<RouteInterception>,
    pub(crate) locator_handlers: Vec<LocatorHandlerFiring>,
    pub(crate) dialogs: Vec<DialogInfo>,
    pub(crate) uploads: Vec<UploadInfo>,
    pub(crate) downloads: Vec<DownloadInfo>,
    pub(crate) websockets: Vec<WebSocketEvent>,
}

impl TabManagementSections {
    pub(crate) fn absorb(&mut self, report: ExecutionReport) -> Option<StepResult> {
        self.new_tabs.extend(report.new_tabs);
        self.intercepted_requests
            .extend(report.intercepted_requests);
        self.locator_handlers.extend(report.locator_handlers);
        self.dialogs.extend(report.dialogs);
        self.uploads.extend(report.uploads);
        self.downloads.extend(report.downloads);
        self.websockets.extend(report.websockets);
        report.steps.into_iter().next()
    }
}

pub(crate) async fn execute_request_with_runtime(
    runtime_arc: Arc<AMutex<BrowserRuntime>>,
    request: BrowserActionRequest,
    image_policy: &ImagePolicy,
) -> Result<ExecutionReport, String> {
    let settings = browser_settings::current();
    if request.session != SessionPolicy::SharedDefault {
        return Err(format!(
            "Unsupported browser session policy: {:?}",
            request.session
        ));
    }
    {
        let mut rt = runtime_arc.lock().await;
        rt.touch();
        if let TabTarget::Id { id } = &request.target {
            let tabs = rt
                .browser
                .get_tabs()
                .lock()
                .map(|t| t.clone())
                .unwrap_or_default();
            let tab = tabs
                .iter()
                .find(|t| t.get_target_id() == id.as_str())
                .ok_or_else(|| format!("No tab found with id={}", id))?;
            rt.set_active_tab_target_id(tab.get_target_id().to_string());
        }
        if let Some(block) = request.block_service_workers {
            rt.set_block_service_workers(block)?;
        }
    }

    let initial_url = {
        let rt = runtime_arc.lock().await;
        rt.get_active_tab().map(|tab| tab.get_url())
    };
    let mut current_tab = {
        let rt = runtime_arc.lock().await;
        rt.get_active_tab()
    };
    let world = {
        let rt = runtime_arc.lock().await;
        rt.world_manager.clone()
    };
    let handlers = {
        let rt = runtime_arc.lock().await;
        rt.locator_handlers.clone()
    };
    let network_monitor = {
        let rt = runtime_arc.lock().await;
        rt.network_monitor.clone()
    };
    let websocket_registry = {
        let rt = runtime_arc.lock().await;
        rt.websocket_registry.clone()
    };
    let download_monitor = {
        let rt = runtime_arc.lock().await;
        rt.download_monitor.clone()
    };
    let file_chooser_manager = {
        let rt = runtime_arc.lock().await;
        rt.file_chooser_manager.clone()
    };
    let armed_network_waits = request
        .steps
        .iter()
        .enumerate()
        .filter_map(|(index, step)| match step {
            BrowserStep::WaitForRequest { .. } => Some((index, network_monitor.request_cursor())),
            BrowserStep::WaitForResponse { .. } => Some((index, network_monitor.response_cursor())),
            BrowserStep::WaitForDownload { .. } => Some((index, download_monitor.cursor())),
            BrowserStep::WaitForWebSocketFrame { .. } => Some((index, websocket_registry.cursor())),
            _ => None,
        })
        .collect::<std::collections::HashMap<_, _>>();
    let (initial_tab_ids, armed_console_cursor) = {
        let mut rt = runtime_arc.lock().await;
        refact_browser::adopt_new_tabs(&mut rt, None);
        rt.drain_raw_events();
        (rt.known_tab_ids(), rt.console_buffer.len())
    };
    let mut results: Vec<StepResult> = Vec::new();
    let mut locator_handlers = Vec::new();
    let mut all_ok = true;
    let mut pending_popup_wait: Option<PendingPopupWait> = None;
    let mut new_tabs = Vec::new();
    let mut routed_requests = Vec::new();
    let mut nested_sections = TabManagementSections::default();
    for (idx, step) in request.steps.iter().enumerate() {
        let step_tab_ids = runtime_arc.lock().await.known_tab_ids();
        if let Some(tab) = &current_tab {
            let _ = tab.evaluate(NETWORK_INFLIGHT_TRACKER_JS, false);
        }
        let file_chooser_was_armed = file_chooser_manager.is_armed();
        let mut result = if let BrowserStep::WaitForPopup { timeout_ms } = step {
            match popup_wait_conflict(pending_popup_wait.as_ref().map(|wait| wait.step_index)) {
                Some(error) => StepResult::failure(idx, "Wait for popup", error),
                None => {
                    let baseline = runtime_arc.lock().await.known_tab_ids();
                    pending_popup_wait = Some(PendingPopupWait::arm(
                        idx,
                        results.len(),
                        *timeout_ms,
                        baseline,
                    ));
                    StepResult::success(idx, "Armed popup wait")
                }
            }
        } else if let BrowserStep::WaitForDownload {
            timeout_ms,
            save_as,
        } = step
        {
            let timeout = Duration::from_millis(clamp_timeout_ms(*timeout_ms));
            download_step_result(
                idx,
                tokio::task::block_in_place(|| {
                    download_monitor.wait_for_download(
                        armed_network_waits
                            .get(&idx)
                            .copied()
                            .unwrap_or_else(|| download_monitor.cursor()),
                        timeout,
                        save_as.as_deref(),
                    )
                }),
            )
        } else if let BrowserStep::WaitForConsoleMessage {
            contains,
            level,
            timeout_ms,
        } = step
        {
            wait_for_console_message(
                &runtime_arc,
                idx,
                contains.as_deref(),
                *level,
                armed_console_cursor,
                clamp_timeout_ms(*timeout_ms),
            )
            .await
        } else if let BrowserStep::CancelDownload { id } = step {
            cancel_download_step_result(
                idx,
                tokio::task::block_in_place(|| download_monitor.cancel_download(id.as_deref())),
            )
        } else if matches!(step, BrowserStep::TabLog) {
            let mut rt = runtime_arc.lock().await;
            rt.drain_raw_events();
            tab_log_from_buffer(idx, &rt.console_buffer)
        } else if let BrowserStep::Pdf { options } = step {
            let mut rt = runtime_arc.lock().await;
            let result = match rt.get_active_tab() {
                Some(tab) => {
                    tokio::task::block_in_place(|| step_pdf(&tab, idx, options, &rt.artifacts_dir))
                }
                None => StepResult::failure(idx, "PDF", "No active tab"),
            };
            rt.touch();
            result
        } else if matches!(step, BrowserStep::PageContent) {
            let mut rt = runtime_arc.lock().await;
            let result = match rt.get_active_tab() {
                Some(tab) => {
                    tokio::task::block_in_place(|| step_page_content(&tab, idx, &rt.artifacts_dir))
                }
                None => StepResult::failure(idx, "Page content", "No active tab"),
            };
            rt.touch();
            result
        } else if matches!(
            step,
            BrowserStep::AddInitScript { .. } | BrowserStep::RemoveInitScript { .. }
        ) {
            let mut rt = runtime_arc.lock().await;
            let result =
                tokio::task::block_in_place(|| execute_init_script_step(&mut rt, step, idx));
            rt.touch();
            result
        } else if matches!(
            step,
            BrowserStep::Route { .. }
                | BrowserStep::Unroute { .. }
                | BrowserStep::ListRoutes
                | BrowserStep::RouteWebSocket { .. }
                | BrowserStep::UnrouteWebSocket { .. }
                | BrowserStep::SendWebSocketMessage { .. }
                | BrowserStep::CloseWebSocket { .. }
                | BrowserStep::StartHarRecording { .. }
                | BrowserStep::StopHarRecording
                | BrowserStep::RouteFromHar { .. }
        ) {
            let mut rt = runtime_arc.lock().await;
            tokio::task::block_in_place(|| execute_route_management_step(&mut rt, step, idx))
                .unwrap()
        } else if let BrowserStep::WaitForWebSocketFrame {
            pattern,
            timeout_ms,
        } = step
        {
            let matcher = pattern
                .as_ref()
                .map(|pattern| match pattern {
                    UrlPattern::Text(value) => UrlMatcher::text(value),
                    UrlPattern::Regex { source, flags } => UrlMatcher::regex(source, flags),
                })
                .transpose();
            let cursor = armed_network_waits.get(&idx).copied().unwrap_or_default();
            match tokio::task::block_in_place(|| {
                matcher.and_then(|matcher| {
                    websocket_registry.wait_for_frame(
                        matcher.as_ref(),
                        cursor,
                        Duration::from_millis(clamp_timeout_ms(*timeout_ms)),
                    )
                })
            }) {
                Ok(frame) => StepResult::success(idx, "Observed WebSocket frame")
                    .with_data(serde_json::json!({"frame": frame})),
                Err(error) => StepResult::failure(idx, "Wait for WebSocket frame", error),
            }
        } else if matches!(step, BrowserStep::Reset) {
            let mut rt = runtime_arc.lock().await;
            tokio::task::block_in_place(|| execute_reset_step(&mut rt, idx))
        } else if let BrowserStep::CdpSend {
            method,
            params,
            target,
        } = step
        {
            tokio::task::block_in_place(|| {
                let mut rt = runtime_arc.blocking_lock();
                execute_cdp_send_step(&mut rt, method, params.as_ref(), *target, idx)
            })
        } else if is_screencast_step(step) {
            tokio::task::block_in_place(|| {
                let mut rt = runtime_arc.blocking_lock();
                execute_screencast_step(&mut rt, step, idx, image_policy)
            })
        } else if let BrowserStep::SetWindowBounds {
            x,
            y,
            width,
            height,
        } = step
        {
            let mut rt = runtime_arc.lock().await;
            tokio::task::block_in_place(|| {
                execute_set_window_bounds_step(&mut rt, *x, *y, *width, *height, idx)
            })
        } else if is_clock_step(step) {
            let mut rt = runtime_arc.lock().await;
            tokio::task::block_in_place(|| execute_clock_step(&mut rt, step, idx))
        } else if let BrowserStep::HttpRequest { options } = step {
            let prepared = {
                let mut rt = runtime_arc.lock().await;
                tokio::task::block_in_place(|| prepare_http_request(&mut rt, options))
            };
            match prepared {
                Ok(prepared) => {
                    match http_client::send_http_request(&prepared.spec, &prepared.jar).await {
                        Ok(response) => {
                            let applied = {
                                let mut rt = runtime_arc.lock().await;
                                tokio::task::block_in_place(|| {
                                    apply_http_response(&mut rt, &prepared.tab, &response)
                                })
                            };
                            match applied {
                                Ok(()) => http_request_result(
                                    &response,
                                    options,
                                    idx,
                                    &prepared.artifacts_dir,
                                ),
                                Err(error) => StepResult::failure(idx, "HTTP request", error),
                            }
                        }
                        Err(error) => StepResult::failure(idx, "HTTP request", error),
                    }
                }
                Err(error) => StepResult::failure(idx, "HTTP request", error),
            }
        } else if is_instrumentation_step(step) {
            let mut rt = runtime_arc.lock().await;
            tokio::task::block_in_place(|| execute_instrumentation_step(&mut rt, step, idx))
        } else if is_context_management_step(step) {
            let mut rt = runtime_arc.lock().await;
            tokio::task::block_in_place(|| execute_context_management_step(&mut rt, step, idx))
        } else if is_tab_management_step(step) {
            let step_report = tokio::task::block_in_place(|| {
                let mut rt = runtime_arc.blocking_lock();
                execute_steps_with_runtime(&mut rt, std::slice::from_ref(step), image_policy)
            });
            {
                let mut rt = runtime_arc.lock().await;
                rt.touch();
                current_tab = rt.get_active_tab();
            }
            let nested_step = nested_sections.absorb(step_report);
            new_tabs.append(&mut nested_sections.new_tabs);
            routed_requests.append(&mut nested_sections.intercepted_requests);
            locator_handlers.append(&mut nested_sections.locator_handlers);
            nested_step.unwrap_or_else(|| {
                StepResult::failure(idx, "Browser action", "No step result produced")
            })
        } else {
            if current_tab.is_none() {
                let mut rt = runtime_arc.lock().await;
                rt.touch();
                current_tab = rt.get_active_tab();
            }
            match &current_tab {
                Some(tab) => tokio::task::block_in_place(|| {
                    let mut rt = runtime_arc.blocking_lock();
                    let mouse_state = rt
                        .mouse_states
                        .entry(tab.get_target_id().to_string())
                        .or_default();
                    execute_runtime_network_step(
                        tab,
                        &world,
                        &network_monitor,
                        step,
                        idx,
                        armed_network_waits.get(&idx).copied(),
                        image_policy,
                        &handlers,
                        &mut locator_handlers,
                        mouse_state,
                    )
                }),
                None => StepResult::failure(
                    idx,
                    "No active tab",
                    "No tab available. Use OpenTab first.",
                ),
            }
        };
        if result.ok && is_image_capture_step(step) {
            let artifacts_dir = runtime_arc.lock().await.artifacts_dir.clone();
            result = persist_image_artifact(result, &artifacts_dir);
        }
        if file_chooser_was_armed && matches!(step, BrowserStep::Click { .. }) {
            if result.ok {
                result = match &current_tab {
                    Some(tab) => tokio::task::block_in_place(|| {
                        let completed = file_chooser_manager.complete(
                            tab,
                            Duration::from_millis(settings.timing.default_wait_timeout_ms),
                        );
                        let _ = tab.set_file_chooser_dialog_interception(false, None);
                        match completed {
                            Ok(upload) => {
                                let mut completed = StepResult::success(
                                    idx,
                                    format!(
                                        "Selected {} file(s) from file chooser",
                                        upload.paths.len()
                                    ),
                                )
                                .with_data(serde_json::to_value(upload).unwrap_or_default());
                                completed.retries = result.retries;
                                completed.actionability = result.actionability;
                                completed
                            }
                            Err(error) => StepResult::failure(idx, "File chooser failed", error),
                        }
                    }),
                    None => StepResult::failure(idx, "File chooser failed", "No active tab"),
                };
            } else {
                file_chooser_manager.disarm();
                if let Some(tab) = &current_tab {
                    let _ = tab.set_file_chooser_dialog_interception(false, None);
                }
            }
        }
        if result.ok && matches!(step, BrowserStep::SetInputFiles { .. }) {
            if let Some(data) = result.data.clone() {
                if let Ok(upload) = serde_json::from_value(data) {
                    file_chooser_manager.record(upload);
                }
            }
        }
        result.step_index = idx;

        let resolves_popup_wait = pending_popup_wait.is_some()
            && !matches!(step, BrowserStep::WaitForPopup { .. })
            && !is_tab_management_step(step);
        if !matches!(step, BrowserStep::WaitForPopup { .. }) {
            if resolves_popup_wait {
                let wait = pending_popup_wait.take().unwrap();
                let popup = loop {
                    let popup = {
                        let mut rt = runtime_arc.lock().await;
                        refact_browser::adopt_new_tabs(&mut rt, Some(idx));
                        let popup_id = rt
                            .list_tab_infos()
                            .into_iter()
                            .find(|tab| !wait.baseline.contains(&tab.id))
                            .map(|tab| tab.id);
                        popup_id.and_then(|tab_id| {
                            rt.tab_opened_by_step.entry(tab_id.clone()).or_insert(idx);
                            rt.set_active_tab_target_id(tab_id.clone());
                            rt.list_tab_infos().into_iter().find(|tab| tab.id == tab_id)
                        })
                    };
                    if let Some(tab) = popup {
                        break Some(tab);
                    }
                    if Instant::now() >= wait.deadline {
                        break None;
                    }
                    tokio::time::sleep(Duration::from_millis(25)).await;
                };
                match popup {
                    Some(tab) => {
                        new_tabs.push(tab.clone());
                        if let Some(wait_result) = results.get_mut(wait.result_index) {
                            *wait_result = popup_wait_opened_result(wait.step_index, &tab.id);
                        }
                        let rt = runtime_arc.lock().await;
                        current_tab = rt.get_active_tab();
                    }
                    None => {
                        if let Some(wait_result) = results.get_mut(wait.result_index) {
                            *wait_result =
                                popup_wait_timeout_result(wait.step_index, wait.timeout_ms);
                        }
                        all_ok = false;
                    }
                }
            } else if pending_popup_wait.is_none() {
                let adopted = {
                    let mut rt = runtime_arc.lock().await;
                    if result.ok
                        && matches!(
                            step,
                            BrowserStep::Click { .. } | BrowserStep::ClickIfExists { .. }
                        )
                    {
                        tokio::task::block_in_place(|| {
                            refact_browser::wait_for_new_tabs(
                                &mut rt,
                                &step_tab_ids,
                                Some(idx),
                                Duration::from_millis(500),
                            )
                        })
                    } else {
                        refact_browser::adopt_new_tabs(&mut rt, Some(idx))
                    }
                };
                new_tabs.extend(adopted);
            }
        }

        {
            let mut rt = runtime_arc.lock().await;
            rt.touch();
            let action_type = if result.ok { "action" } else { "error" };
            rt.push_agent_action(action_type, &result.summary);
            if rt.route_interception_stale() {
                if let Err(error) =
                    tokio::task::block_in_place(|| rt.reconcile_route_interception())
                {
                    tracing::warn!("Failed to reconcile route interception: {error}");
                }
            }
        }

        let is_non_fatal = is_non_fatal_step(step);
        if !result.ok && !is_non_fatal {
            all_ok = false;
            let transport_dead = step_hit_dead_transport(&result);
            results.push(result);
            if !request.continue_on_error || transport_dead {
                break;
            }
            continue;
        }
        results.push(result);
        if !all_ok && !request.continue_on_error {
            break;
        }
    }

    if let Some(wait) = pending_popup_wait.take() {
        if let Some(wait_result) = results.get_mut(wait.result_index) {
            *wait_result = popup_wait_timeout_result(wait.step_index, wait.timeout_ms);
        }
        all_ok = false;
    }
    mark_unexecuted_steps_skipped(&mut results, request.steps.len());

    {
        let mut rt = runtime_arc.lock().await;
        refact_browser::adopt_new_tabs(&mut rt, None);
        new_tabs.extend(
            rt.list_tab_infos()
                .into_iter()
                .filter(|tab| !initial_tab_ids.contains(&tab.id)),
        );
    }
    new_tabs.retain(|tab| !initial_tab_ids.contains(&tab.id));
    let mut seen_new_tabs = std::collections::HashSet::new();
    new_tabs.retain(|tab| seen_new_tabs.insert(tab.id.clone()));

    let active_tab = match current_tab {
        Some(tab) => Some(tab),
        None => runtime_arc.lock().await.get_active_tab(),
    };
    let page_context_mode = request.page_context_mode();
    let artifacts_dir = runtime_arc.lock().await.artifacts_dir.clone();
    let mut warnings: Vec<String> = Vec::new();
    let (url, title, stabilized, screenshot, snapshot) = if let Some(tab) = active_tab {
        let stabilized = tokio::task::block_in_place(|| {
            wait_for_report_stability(
                &tab,
                &world,
                &network_monitor,
                REPORT_STABILIZATION_TIMEOUT_MS,
            )
        });
        let url = tab.get_url();
        let page_changed = initial_url.as_deref() != Some(url.as_str())
            || request.steps.iter().any(replaces_document_in_place);
        let capture_requested = report_screenshot_requested(
            request.attach_screenshot,
            page_context_mode,
            request
                .steps
                .iter()
                .any(|step| matches!(step, BrowserStep::Screenshot { .. })),
        );
        let screenshot = if capture_requested {
            match tokio::task::block_in_place(|| capture_report_screenshot(&tab, image_policy)) {
                Ok(screenshot) => Some(screenshot),
                Err(error) => {
                    warnings.push(format!("screenshot capture failed: {error}"));
                    None
                }
            }
        } else {
            None
        };
        let snapshot = if report_snapshot_requested(page_context_mode, page_changed) {
            match tokio::task::block_in_place(|| {
                capture_page_snapshot(&tab, &world, &artifacts_dir)
            }) {
                Ok(snapshot) => Some(snapshot),
                Err(error) => {
                    warnings.push(format!("page snapshot capture failed: {error}"));
                    None
                }
            }
        } else {
            None
        };
        (
            Some(url),
            tab.get_title().ok(),
            stabilized,
            screenshot,
            snapshot,
        )
    } else {
        (None, None, false, None, None)
    };
    let (
        console,
        page_errors,
        network,
        websockets,
        dialogs,
        uploads,
        downloads,
        active_routes,
        intercepted_requests,
    ) = {
        let mut rt = runtime_arc.lock().await;
        rt.drain_raw_events();
        let mut console = rt
            .flush_report_console()
            .into_iter()
            .map(mask_console_entry)
            .collect::<Vec<_>>();
        let page_errors: Vec<String> = console
            .iter()
            .filter(|entry| entry.level == "page_error")
            .map(|entry| entry.text.clone())
            .collect();
        console.retain(|entry| entry.level != "page_error");
        let network = rt.flush_report_network();
        let mut websockets = nested_sections.websockets;
        websockets.extend(rt.websocket_registry.drain_report());
        let mut dialogs = nested_sections.dialogs;
        dialogs.extend(rt.dialog_manager.take_reports());
        let mut uploads = nested_sections.uploads;
        uploads.extend(rt.file_chooser_manager.take_uploads());
        let mut downloads = nested_sections.downloads;
        downloads.extend(rt.download_monitor.take_report());
        let active_routes = rt.route_registry.list();
        routed_requests.extend(rt.route_registry.drain_interceptions());
        (
            console,
            page_errors,
            network,
            websockets,
            dialogs,
            uploads,
            downloads,
            active_routes,
            routed_requests,
        )
    };

    let page = BrowserPageContext {
        status: notable_main_document_status(&network, url.as_deref()),
        console: console_counts(&console, &page_errors),
        snapshot,
    };
    let mut report = ExecutionReport {
        ok: all_ok,
        steps: results,
        warnings,
        url,
        title,
        page: (page != BrowserPageContext::default()).then_some(page),
        stabilized,
        console,
        page_errors,
        network,
        network_summary: Vec::new(),
        websockets,
        locator_handlers,
        dialogs,
        uploads,
        downloads,
        new_tabs,
        active_routes,
        intercepted_requests,
        context: {
            let runtime = runtime_arc.lock().await;
            Some(context_summary(&runtime))
        },
        screenshot,
    };
    apply_network_report_mode(&mut report, request.network);
    Ok(report)
}

pub(crate) fn resolve_pending_popup(
    runtime: &mut BrowserRuntime,
    wait: &PendingPopupWait,
    step_index: usize,
) -> Option<TabInfo> {
    loop {
        refact_browser::adopt_new_tabs(runtime, Some(step_index));
        let popup_id = runtime
            .list_tab_infos()
            .into_iter()
            .find(|tab| !wait.baseline.contains(&tab.id))
            .map(|tab| tab.id);
        if let Some(tab_id) = popup_id {
            runtime
                .tab_opened_by_step
                .entry(tab_id.clone())
                .or_insert(step_index);
            runtime.set_active_tab_target_id(tab_id.clone());
            return runtime
                .list_tab_infos()
                .into_iter()
                .find(|tab| tab.id == tab_id);
        }
        if Instant::now() >= wait.deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}
