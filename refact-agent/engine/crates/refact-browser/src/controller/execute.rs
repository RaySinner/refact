use super::*;

use super::actions::{merge_step_data, navigation_step_success, run_and_wait_for_navigation, trigger_page_navigation};
use super::capture::{is_image_capture_step, persist_image_artifact};
use super::context::{clamp_timeout_ms};
use super::dispatch::{PendingPopupWait, popup_wait_conflict, popup_wait_opened_result, popup_wait_timeout_result, resolve_pending_popup};
use super::eval_steps::{step_page_content, step_pdf, tab_log_from_buffer};
use super::single_step::{execute_single_step, wait_for_network_idle_step};
use super::steps_runtime::{cancel_download_step_result, context_summary, download_step_result, execute_cdp_send_step, execute_context_management_step, execute_http_request_step, execute_init_script_step, execute_instrumentation_step, execute_reset_step, execute_route_management_step, execute_screencast_step, execute_set_window_bounds_step, is_context_management_step, is_instrumentation_step, is_screencast_step, open_tab_for_device};


pub(crate) fn execute_steps_with_runtime(
    runtime: &mut BrowserRuntime,
    steps: &[BrowserStep],
    image_policy: &ImagePolicy,
) -> ExecutionReport {
    let settings = browser_settings::current();
    refact_browser::adopt_new_tabs(runtime, None);
    let initial_tab_ids = runtime.known_tab_ids();
    let mut current_tab: Option<Arc<Tab>> = runtime.get_active_tab();
    if let Some(ref tab) = current_tab {
        let _ = tab.evaluate(INSPECT_ELEMENT_JS, false);
    }

    let mut results: Vec<StepResult> = Vec::new();
    let mut new_tabs = Vec::new();
    let handlers = runtime.locator_handlers.clone();
    let mut locator_handlers = Vec::new();
    let mut all_ok = true;
    let mut pending_popup_wait: Option<PendingPopupWait> = None;
    let mut pre_step_url: Option<String> = current_tab.as_ref().map(|t| t.get_url());
    let armed_download_waits = steps
        .iter()
        .enumerate()
        .filter_map(|(index, step)| match step {
            BrowserStep::WaitForDownload { .. } => Some((index, runtime.download_monitor.cursor())),
            _ => None,
        })
        .collect::<std::collections::HashMap<_, _>>();

    for (idx, step) in steps.iter().enumerate() {
        let step_tab_ids = runtime.known_tab_ids();
        let result = match step {
            BrowserStep::OpenTab { device, url } => match open_tab_for_device(runtime, device) {
                Ok((descriptor, new_tab)) => {
                    let device_label = descriptor.name.clone();
                    let target_id = new_tab.get_target_id().to_string();
                    runtime.context_state.viewport = Some(descriptor.viewport.clone());
                    runtime.context_state.user_agent = Some((descriptor.user_agent.clone(), None));
                    if let Err(error) =
                        crate::setup_recording_for_tab(
                            runtime,
                            new_tab.clone(),
                        )
                    {
                        let _ = new_tab.close(false);
                        all_ok = false;
                        results.push(StepResult::failure(idx, "OpenTab", error));
                        break;
                    }
                    let navigation = url.as_ref().map(|url| {
                        run_and_wait_for_navigation(
                            &new_tab,
                            settings.timing.default_wait_timeout_ms,
                            || trigger_page_navigation(&new_tab, url),
                        )
                    });
                    let _ = new_tab.evaluate(INSPECT_ELEMENT_JS, false);
                    current_tab = Some(new_tab);
                    runtime.set_active_tab_target_id(target_id.clone());
                    match navigation.transpose() {
                        Err(error) => StepResult::failure(idx, "OpenTab", error),
                        Ok(warning) => merge_step_data(
                            navigation_step_success(
                                idx,
                                format!(
                                    "Opened new {} tab ({})",
                                    device_label,
                                    &target_id[..8.min(target_id.len())]
                                ),
                                warning.flatten(),
                            ),
                            serde_json::json!({"tab_id": target_id}),
                        ),
                    }
                }
                Err(error) => StepResult::failure(idx, "OpenTab", error),
            },
            BrowserStep::CloseTab { tab: target } => {
                let tab = match target {
                    Some(target) => match resolve_tab(runtime, target) {
                        Ok(tab) => tab,
                        Err(error) => {
                            all_ok = false;
                            results.push(StepResult::failure(idx, "CloseTab", error));
                            break;
                        }
                    },
                    None => match &current_tab {
                        Some(tab) => tab.clone(),
                        None => {
                            all_ok = false;
                            results.push(StepResult::failure(idx, "CloseTab", "No active tab"));
                            break;
                        }
                    },
                };
                let target_id = tab.get_target_id().to_string();
                match tab.close(false) {
                    Ok(_) => {
                        runtime.world_manager.tab_closed(&target_id);
                        runtime.mouse_states.remove(&target_id);
                        runtime.select_tab_after_close(&target_id);
                        current_tab = runtime.get_active_tab();
                        StepResult::success(
                            idx,
                            format!("Closed tab {}", &target_id[..8.min(target_id.len())]),
                        )
                    }
                    Err(e) => StepResult::failure(idx, "CloseTab", &format!("Failed: {}", e)),
                }
            }
            BrowserStep::SwitchTab { tab: tab_target } => {
                let tabs = runtime
                    .browser
                    .get_tabs()
                    .lock()
                    .map(|t| t.clone())
                    .unwrap_or_default();
                let target_str = match tab_target {
                    TabTarget::Active => String::from("active"),
                    TabTarget::Id { id } => id.clone(),
                };
                let found = match tab_target {
                    TabTarget::Active => runtime.get_active_tab().or_else(|| tabs.first().cloned()),
                    TabTarget::Id { id } => tabs
                        .iter()
                        .find(|t| t.get_target_id() == id.as_str())
                        .cloned(),
                };
                match found {
                    Some(found_tab) => {
                        runtime.set_active_tab_target_id(found_tab.get_target_id().to_string());
                        let _ = found_tab.evaluate(INSPECT_ELEMENT_JS, false);
                        current_tab = Some(found_tab.clone());
                        StepResult::success(
                            idx,
                            format!("Switched to tab {} ({})", target_str, found_tab.get_url()),
                        )
                    }
                    None => StepResult::failure(
                        idx,
                        "SwitchTab",
                        format!("No tab matching '{}'", target_str),
                    ),
                }
            }
            BrowserStep::ListTabs => {
                let tab_list = runtime
                    .list_tab_infos()
                    .into_iter()
                    .map(|tab| serde_json::to_value(tab).unwrap_or_default())
                    .collect::<Vec<_>>();
                StepResult::success(idx, format!("Listed {} tabs", tab_list.len()))
                    .with_data(serde_json::json!({"tabs": tab_list}))
            }
            step @ (BrowserStep::Route { .. }
            | BrowserStep::Unroute { .. }
            | BrowserStep::ListRoutes
            | BrowserStep::RouteWebSocket { .. }
            | BrowserStep::UnrouteWebSocket { .. }
            | BrowserStep::SendWebSocketMessage { .. }
            | BrowserStep::CloseWebSocket { .. }
            | BrowserStep::StartHarRecording { .. }
            | BrowserStep::StopHarRecording
            | BrowserStep::RouteFromHar { .. }) => {
                execute_route_management_step(runtime, step, idx).unwrap()
            }
            BrowserStep::Reset => execute_reset_step(runtime, idx),
            BrowserStep::CdpSend {
                method,
                params,
                target,
            } => execute_cdp_send_step(runtime, method, params.as_ref(), *target, idx),
            step if is_screencast_step(step) => {
                execute_screencast_step(runtime, step, idx, image_policy)
            }
            BrowserStep::SetWindowBounds {
                x,
                y,
                width,
                height,
            } => execute_set_window_bounds_step(runtime, *x, *y, *width, *height, idx),
            BrowserStep::HttpRequest { options } => tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current()
                    .block_on(execute_http_request_step(runtime, options, idx))
            }),
            step if is_instrumentation_step(step) => {
                execute_instrumentation_step(runtime, step, idx)
            }
            step if is_context_management_step(step) => {
                execute_context_management_step(runtime, step, idx)
            }
            BrowserStep::WaitForPopup { timeout_ms } => {
                match popup_wait_conflict(pending_popup_wait.as_ref().map(|wait| wait.step_index)) {
                    Some(error) => StepResult::failure(idx, "Wait for popup", error),
                    None => {
                        pending_popup_wait = Some(PendingPopupWait::arm(
                            idx,
                            results.len(),
                            *timeout_ms,
                            runtime.known_tab_ids(),
                        ));
                        StepResult::success(idx, "Armed popup wait")
                    }
                }
            }
            BrowserStep::WaitForNetworkIdle { timeout_ms } => wait_for_network_idle_step(
                &runtime.network_monitor,
                idx,
                clamp_timeout_ms(*timeout_ms),
            ),
            BrowserStep::HandleDialog {
                accept,
                prompt_text,
            } => match runtime.dialog_manager.arm(*accept, prompt_text.clone()) {
                Ok(()) => StepResult::success(
                    idx,
                    if *accept {
                        "Armed acceptance for the next dialog"
                    } else {
                        "Armed dismissal for the next dialog"
                    },
                ),
                Err(error) => StepResult::failure(idx, "HandleDialog", error),
            },
            BrowserStep::ExpectFileChooser { paths } => {
                let Some(tab) = &current_tab else {
                    all_ok = false;
                    results.push(StepResult::failure(
                        idx,
                        "Expect file chooser",
                        "No active tab",
                    ));
                    break;
                };
                match runtime.file_chooser_manager.arm(paths).and_then(|_| {
                    tab.set_file_chooser_dialog_interception(true, None)
                        .map_err(|error| error.to_string())
                }) {
                    Ok(()) => StepResult::success(idx, "Armed the next file chooser"),
                    Err(error) => {
                        runtime.file_chooser_manager.disarm();
                        StepResult::failure(idx, "Expect file chooser", error)
                    }
                }
            }
            BrowserStep::WaitForDownload {
                timeout_ms,
                save_as,
            } => download_step_result(
                idx,
                runtime.download_monitor.wait_for_download(
                    armed_download_waits
                        .get(&idx)
                        .copied()
                        .unwrap_or_else(|| runtime.download_monitor.cursor()),
                    Duration::from_millis(clamp_timeout_ms(*timeout_ms)),
                    save_as.as_deref(),
                ),
            ),
            BrowserStep::CancelDownload { id } => cancel_download_step_result(
                idx,
                runtime.download_monitor.cancel_download(id.as_deref()),
            ),
            BrowserStep::Pdf { options } => match &current_tab {
                Some(tab) => step_pdf(tab, idx, options, &runtime.artifacts_dir),
                None => StepResult::failure(idx, "PDF", "No active tab"),
            },
            BrowserStep::PageContent => match &current_tab {
                Some(tab) => step_page_content(tab, idx, &runtime.artifacts_dir),
                None => StepResult::failure(idx, "Page content", "No active tab"),
            },
            BrowserStep::TabLog => {
                runtime.drain_raw_events();
                tab_log_from_buffer(idx, &runtime.console_buffer)
            }
            BrowserStep::AddInitScript { .. } | BrowserStep::RemoveInitScript { .. } => {
                execute_init_script_step(runtime, step, idx)
            }
            BrowserStep::Help { topic } => {
                StepResult::success(idx, "chrome help").with_data(serde_json::json!({
                    "text": crate::help::chrome_help_text(topic.as_deref()),
                }))
            }
            other => match &current_tab {
                Some(tab) => {
                    let chooser_was_armed = runtime.file_chooser_manager.is_armed();
                    let world = runtime.world_manager.clone();
                    let mut result = execute_single_step(
                        tab,
                        &world,
                        other,
                        idx,
                        pre_step_url.as_deref(),
                        image_policy,
                        Some(&handlers),
                        &mut locator_handlers,
                        runtime
                            .mouse_states
                            .entry(tab.get_target_id().to_string())
                            .or_default(),
                    );
                    if chooser_was_armed && matches!(other, BrowserStep::Click { .. }) {
                        if result.ok {
                            let completed = runtime.file_chooser_manager.complete(
                                tab,
                                Duration::from_millis(settings.timing.default_wait_timeout_ms),
                            );
                            let _ = tab.set_file_chooser_dialog_interception(false, None);
                            result = match completed {
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
                                Err(error) => {
                                    StepResult::failure(idx, "File chooser failed", error)
                                }
                            };
                        } else {
                            runtime.file_chooser_manager.disarm();
                            let _ = tab.set_file_chooser_dialog_interception(false, None);
                        }
                    }
                    if result.ok && matches!(other, BrowserStep::SetInputFiles { .. }) {
                        if let Some(data) = result.data.clone() {
                            if let Ok(upload) = serde_json::from_value(data) {
                                runtime.file_chooser_manager.record(upload);
                            }
                        }
                    }
                    if result.ok && is_image_capture_step(other) {
                        result = persist_image_artifact(result, &runtime.artifacts_dir);
                    }
                    result
                }
                None => StepResult::failure(
                    idx,
                    "No active tab",
                    "No tab available. Use OpenTab first.",
                ),
            },
        };

        let resolves_popup_wait = pending_popup_wait.is_some()
            && !matches!(step, BrowserStep::WaitForPopup { .. })
            && !is_tab_management_step(step);
        if resolves_popup_wait {
            let wait = pending_popup_wait.take().unwrap();
            match resolve_pending_popup(runtime, &wait, idx) {
                Some(tab) => {
                    current_tab = runtime.get_active_tab();
                    new_tabs.push(tab.clone());
                    if let Some(wait_result) = results.get_mut(wait.result_index) {
                        *wait_result = popup_wait_opened_result(wait.step_index, &tab.id);
                    }
                }
                None => {
                    if let Some(wait_result) = results.get_mut(wait.result_index) {
                        *wait_result = popup_wait_timeout_result(wait.step_index, wait.timeout_ms);
                    }
                    all_ok = false;
                }
            }
        }

        let is_non_fatal = is_non_fatal_step(step);
        if !result.ok && !is_non_fatal {
            all_ok = false;
            results.push(result);
            break;
        }
        if result.ok && is_navigation_step(step) {
            if let Some(ref tab) = current_tab {
                let _ = tab.evaluate(INSPECT_ELEMENT_JS, false);
            }
        }
        pre_step_url = current_tab.as_ref().map(|t| t.get_url());
        if !resolves_popup_wait && pending_popup_wait.is_none() {
            if result.ok
                && matches!(
                    step,
                    BrowserStep::Click { .. } | BrowserStep::ClickIfExists { .. }
                )
            {
                new_tabs.extend(refact_browser::wait_for_new_tabs(
                    runtime,
                    &step_tab_ids,
                    Some(idx),
                    Duration::from_millis(500),
                ));
            } else {
                new_tabs.extend(refact_browser::adopt_new_tabs(runtime, Some(idx)));
            }
        }
        results.push(result);
        if !all_ok {
            break;
        }
    }

    if let Some(wait) = pending_popup_wait.take() {
        if let Some(wait_result) = results.get_mut(wait.result_index) {
            *wait_result = popup_wait_timeout_result(wait.step_index, wait.timeout_ms);
        }
        all_ok = false;
    }
    mark_unexecuted_steps_skipped(&mut results, steps.len());

    let (url, title) = match &current_tab {
        Some(tab) => (Some(tab.get_url()), tab.get_title().ok()),
        None => (None, None),
    };
    let dialogs = runtime.dialog_manager.take_reports();
    let uploads = runtime.file_chooser_manager.take_uploads();
    let downloads = runtime.download_monitor.take_report();
    refact_browser::adopt_new_tabs(runtime, None);
    new_tabs.extend(
        runtime
            .list_tab_infos()
            .into_iter()
            .filter(|tab| !initial_tab_ids.contains(&tab.id)),
    );
    new_tabs.retain(|tab| !initial_tab_ids.contains(&tab.id));
    let mut seen_new_tabs = std::collections::HashSet::new();
    new_tabs.retain(|tab| seen_new_tabs.insert(tab.id.clone()));
    ExecutionReport {
        ok: all_ok,

        steps: results,
        warnings: Vec::new(),
        url,
        title,
        page: None,
        stabilized: false,
        console: vec![],
        page_errors: vec![],
        network: vec![],
        network_summary: vec![],
        websockets: runtime.websocket_registry.drain_report(),
        locator_handlers,
        dialogs,
        uploads,
        downloads,
        new_tabs,
        active_routes: runtime.route_registry.list(),
        intercepted_requests: runtime.route_registry.drain_interceptions(),
        context: Some(context_summary(runtime)),
        screenshot: None,
    }
}

pub(crate) fn step_hit_dead_transport(step: &StepResult) -> bool {
    step.error
        .as_deref()
        .is_some_and(refact_browser::is_transport_dead_error)
}

pub(crate) fn is_non_fatal_step(step: &BrowserStep) -> bool {
    matches!(
        step,
        BrowserStep::ClickIfExists { .. }
            | BrowserStep::Expect { soft: true, .. }
            | BrowserStep::ExpectPoll {
                soft: Some(true),
                ..
            }
    )
}

pub(crate) fn mark_unexecuted_steps_skipped(results: &mut Vec<StepResult>, total_steps: usize) {
    let executed = results.len();
    if executed >= total_steps {
        return;
    }
    let failed_step_index = results
        .iter()
        .rev()
        .find(|result| !result.ok)
        .map(|result| result.step_index)
        .unwrap_or(executed.saturating_sub(1));
    results.extend((executed..total_steps).map(|idx| StepResult::skipped(idx, failed_step_index)));
}

pub(crate) fn replaces_document_in_place(step: &BrowserStep) -> bool {
    matches!(step, BrowserStep::SetContent { .. })
}

pub(crate) fn is_navigation_step(step: &BrowserStep) -> bool {
    matches!(
        step,
        BrowserStep::Navigate { .. }
            | BrowserStep::Reload
            | BrowserStep::GoBack
            | BrowserStep::GoForward
            | BrowserStep::SetContent { .. }
    )
}
