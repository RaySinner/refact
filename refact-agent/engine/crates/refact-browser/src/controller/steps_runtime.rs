use super::*;

use super::capture::{capture_screenshot, screenshot_metrics};
use super::context::{resolve_element};
use super::execute::{is_navigation_step, is_non_fatal_step};
use super::single_step::{execute_single_step};


pub fn execute_steps(
    tab: &Tab,
    steps: &[BrowserStep],
    image_policy: &ImagePolicy,
) -> ExecutionReport {
    execute_steps_with_world(tab, steps, image_policy, &WorldManager::default())
}

pub(crate) fn execute_steps_with_world(
    tab: &Tab,
    steps: &[BrowserStep],
    image_policy: &ImagePolicy,
    world: &WorldManager,
) -> ExecutionReport {
    let _ = tab.evaluate(INSPECT_ELEMENT_JS, false);

    let mut results: Vec<StepResult> = Vec::new();
    let handlers = Arc::new(Mutex::new(LocatorHandlerRegistry::default()));
    let mut locator_handlers = Vec::new();
    let mut all_ok = true;
    let mut pre_step_url: Option<String> = Some(tab.get_url());
    let mut mouse_state = MouseState::default();

    for (idx, step) in steps.iter().enumerate() {
        let result = execute_single_step(
            tab,
            world,
            step,
            idx,
            pre_step_url.as_deref(),
            image_policy,
            Some(&handlers),
            &mut locator_handlers,
            &mut mouse_state,
        );
        let is_non_fatal = is_non_fatal_step(step);
        if !result.ok && !is_non_fatal {
            all_ok = false;
            results.push(result);
            break;
        }
        if result.ok && is_navigation_step(step) {
            let _ = tab.evaluate(INSPECT_ELEMENT_JS, false);
        }
        pre_step_url = Some(tab.get_url());
        results.push(result);
    }
    mark_unexecuted_steps_skipped(&mut results, steps.len());

    let _ = world.release_all(tab);

    ExecutionReport {
        ok: all_ok,
        steps: results,
        warnings: Vec::new(),
        url: Some(tab.get_url()),
        title: tab.get_title().ok(),
        page: None,
        stabilized: false,
        console: vec![],
        page_errors: vec![],
        network: vec![],
        network_summary: vec![],
        websockets: vec![],
        locator_handlers,
        dialogs: vec![],
        uploads: vec![],
        downloads: vec![],
        new_tabs: vec![],
        active_routes: vec![],
        intercepted_requests: vec![],
        context: None,
        screenshot: None,
    }
}

pub fn is_tab_management_step(step: &BrowserStep) -> bool {
    matches!(
        step,
        BrowserStep::OpenTab { .. }
            | BrowserStep::CloseTab { .. }
            | BrowserStep::SwitchTab { .. }
            | BrowserStep::ListTabs
            | BrowserStep::HandleDialog { .. }
            | BrowserStep::ExpectFileChooser { .. }
    )
}

pub(crate) fn is_context_management_step(step: &BrowserStep) -> bool {
    matches!(
        step,
        BrowserStep::SetViewport { .. }
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
    )
}

pub(crate) fn download_step_result(idx: usize, download: Result<DownloadInfo, String>) -> StepResult {
    match download {
        Ok(download) if download.state == DownloadState::Canceled => StepResult::failure(
            idx,
            "Wait for download",
            format!(
                "Download failed ({}): {}",
                download
                    .failure_reason
                    .clone()
                    .unwrap_or_else(|| "canceled".to_string()),
                download.suggested_filename
            ),
        )
        .with_data(serde_json::to_value(download).unwrap_or_default()),
        Ok(download) => {
            StepResult::success(idx, format!("Downloaded {}", download.suggested_filename))
                .with_data(serde_json::to_value(download).unwrap_or_default())
        }
        Err(error) => StepResult::failure(idx, "Wait for download", error),
    }
}

pub(crate) fn cancel_download_step_result(idx: usize, download: Result<DownloadInfo, String>) -> StepResult {
    match download {
        Ok(download) => StepResult::success(
            idx,
            format!("Canceled download {}", download.suggested_filename),
        )
        .with_data(serde_json::to_value(download).unwrap_or_default()),
        Err(error) => StepResult::failure(idx, "Cancel download", error),
    }
}

pub(crate) fn apply_permission_state(
    current: &[String],
    permissions: &[String],
    state: BrowserPermissionState,
) -> Vec<String> {
    let mut granted = current
        .iter()
        .filter(|permission| !permissions.contains(permission))
        .cloned()
        .collect::<Vec<_>>();
    if state.is_granted() {
        granted.extend(permissions.iter().cloned());
    }
    granted
}

pub(crate) fn open_tab_for_device(
    runtime: &BrowserRuntime,
    device: &Option<String>,
) -> Result<(&'static refact_browser::DeviceDescriptor, Arc<Tab>), String> {
    let descriptor = refact_browser::devices::lookup(device.as_deref().unwrap_or("desktop"))?;
    let tab = runtime
        .browser
        .new_tab()
        .map_err(|error| format!("Failed: {error}"))?;
    Ok((descriptor, tab))
}

pub(crate) fn network_conditions_summary(
    offline: bool,
    conditions: Option<&refact_browser::NetworkConditions>,
) -> String {
    let throttling = conditions.map_or_else(
        || "no throttling".to_string(),
        refact_browser::NetworkConditions::summary,
    );
    format!(
        "Network conditions: {}, {throttling}",
        if offline { "offline" } else { "online" }
    )
}

pub(crate) fn apply_context_to_tabs(runtime: &BrowserRuntime) -> Result<(), String> {
    for tab in runtime
        .browser
        .get_tabs()
        .lock()
        .map(|tabs| tabs.iter().cloned().collect::<Vec<_>>())
        .unwrap_or_default()
    {
        runtime.context_state.apply_to_tab(&tab)?;
    }
    Ok(())
}

pub(crate) fn context_summary(runtime: &BrowserRuntime) -> BrowserContextSummary {
    let Some(tab) = runtime.get_active_tab() else {
        return runtime.context_state.summary(0, 0, 0);
    };
    let cookies = refact_browser::context_state::get_cookies(&tab, None)
        .map(|cookies| cookies.len())
        .unwrap_or_default();
    let local = refact_browser::context_state::get_storage(&tab, BrowserStorageKind::Local, None)
        .map(|items| items.len())
        .unwrap_or_default();
    let session =
        refact_browser::context_state::get_storage(&tab, BrowserStorageKind::Session, None)
            .map(|items| items.len())
            .unwrap_or_default();
    runtime.context_state.summary(cookies, local, session)
}

pub(crate) fn execute_context_management_step(
    runtime: &mut BrowserRuntime,
    step: &BrowserStep,
    idx: usize,
) -> StepResult {
    let result: Result<StepResult, String> = (|| match step {
        BrowserStep::SetViewport {
            width,
            height,
            device_scale_factor,
            is_mobile,
            has_touch,
        } => {
            runtime.context_state.viewport = Some(refact_browser::ViewportState {
                width: *width,
                height: *height,
                device_scale_factor: device_scale_factor.unwrap_or(1.0),
                is_mobile: is_mobile.unwrap_or(false),
                has_touch: has_touch.unwrap_or(false),
            });
            apply_context_to_tabs(runtime)?;
            Ok(StepResult::success(
                idx,
                format!("Set viewport to {width}x{height}"),
            ))
        }
        BrowserStep::EmulateMedia {
            color_scheme,
            reduced_motion,
            forced_colors,
            contrast,
            media,
        } => {
            runtime.context_state.media = refact_browser::MediaState {
                color_scheme: color_scheme.clone(),
                reduced_motion: reduced_motion.clone(),
                forced_colors: forced_colors.clone(),
                contrast: contrast.clone(),
                media: media.clone(),
            };
            apply_context_to_tabs(runtime)?;
            Ok(StepResult::success(idx, "Applied media emulation"))
        }
        BrowserStep::SetLocale { locale } => {
            runtime.context_state.locale = Some(locale.clone());
            apply_context_to_tabs(runtime)?;
            Ok(StepResult::success(idx, format!("Set locale to {locale}")))
        }
        BrowserStep::SetTimezone { timezone } => {
            runtime.context_state.timezone = Some(timezone.clone());
            apply_context_to_tabs(runtime)?;
            Ok(StepResult::success(
                idx,
                format!("Set timezone to {timezone}"),
            ))
        }
        BrowserStep::SetUserAgent {
            user_agent,
            accept_language,
        } => {
            runtime.context_state.user_agent = Some((user_agent.clone(), accept_language.clone()));
            apply_context_to_tabs(runtime)?;
            Ok(StepResult::success(idx, "Set user agent"))
        }
        BrowserStep::SetGeolocation {
            latitude,
            longitude,
            accuracy,
        } => {
            runtime.context_state.geolocation =
                Some((*latitude, *longitude, accuracy.unwrap_or(0.0)));
            apply_context_to_tabs(runtime)?;
            Ok(StepResult::success(idx, "Set geolocation"))
        }
        BrowserStep::SetOffline { offline } => {
            runtime.context_state.offline = *offline;
            apply_context_to_tabs(runtime)?;
            Ok(StepResult::success(
                idx,
                if *offline {
                    "Went offline"
                } else {
                    "Went online"
                },
            ))
        }
        BrowserStep::SetNetworkConditions {
            offline,
            latency_ms,
            download_kbps,
            upload_kbps,
            preset,
        } => {
            let conditions = refact_browser::NetworkConditions::resolve(
                preset.as_deref(),
                *latency_ms,
                *download_kbps,
                *upload_kbps,
            )?;
            if let Some(offline) = offline {
                runtime.context_state.offline = *offline;
            }
            runtime.context_state.network_conditions = conditions;
            apply_context_to_tabs(runtime)?;
            Ok(StepResult::success(
                idx,
                network_conditions_summary(runtime.context_state.offline, conditions.as_ref()),
            )
            .with_data(serde_json::json!({
                "network_conditions": conditions.map(|conditions| serde_json::json!({
                    "latency_ms": conditions.latency_ms,
                    "download_kbps": conditions.download_kbps,
                    "upload_kbps": conditions.upload_kbps,
                })),
                "offline": runtime.context_state.offline,
            })))
        }
        BrowserStep::SetCpuThrottling { rate } => {
            if *rate < 1.0 {
                return Err(format!(
                    "rate must be at least 1 (1 disables CPU throttling), got {rate}"
                ));
            }
            runtime.context_state.cpu_throttling_rate = (*rate > 1.0).then_some(*rate);
            apply_context_to_tabs(runtime)?;
            Ok(StepResult::success(
                idx,
                if *rate > 1.0 {
                    format!("Throttling CPU {rate}x slower")
                } else {
                    "CPU throttling off".to_string()
                },
            )
            .with_data(serde_json::json!({"cpu_throttling_rate": rate})))
        }
        BrowserStep::EmulateDevice { name } => {
            let device = refact_browser::devices::lookup(name)?;
            runtime.context_state.viewport = Some(device.viewport.clone());
            runtime.context_state.user_agent = Some((device.user_agent.clone(), None));
            apply_context_to_tabs(runtime)?;
            Ok(
                StepResult::success(idx, format!("Emulating {}", device.summary())).with_data(
                    serde_json::json!({"device": {
                        "name": device.name,
                        "width": device.viewport.width,
                        "height": device.viewport.height,
                        "device_scale_factor": device.viewport.device_scale_factor,
                        "is_mobile": device.viewport.is_mobile,
                        "has_touch": device.viewport.has_touch,
                        "user_agent": device.user_agent,
                    }}),
                ),
            )
        }
        BrowserStep::ListDevices { filter } => {
            let names = refact_browser::devices::list(filter.as_deref());
            Ok(
                StepResult::success(idx, format!("{} matching device(s)", names.len())).with_data(
                    serde_json::json!({"devices": names, "aliases": ["mobile", "tablet", "desktop"]}),
                ),
            )
        }
        BrowserStep::SetExtraHttpHeaders { headers } => {
            runtime.context_state.extra_http_headers = headers.clone();
            apply_context_to_tabs(runtime)?;
            Ok(StepResult::success(
                idx,
                format!("Set {} extra HTTP header(s)", headers.len()),
            ))
        }
        BrowserStep::GetCookies { urls } => {
            let cookies = refact_browser::context_state::get_cookies(
                runtime
                    .get_active_tab()
                    .ok_or_else(|| "No active tab in browser runtime".to_string())?
                    .as_ref(),
                urls.clone(),
            )?;
            Ok(
                StepResult::success(idx, format!("Read {} cookie(s)", cookies.len())).with_data(
                    serde_json::json!({
                        "cookies": refact_browser::context_state::mask_cookies(&cookies)
                    }),
                ),
            )
        }
        BrowserStep::SetCookies { cookies } => {
            refact_browser::context_state::set_cookies(
                runtime
                    .get_active_tab()
                    .ok_or_else(|| "No active tab in browser runtime".to_string())?
                    .as_ref(),
                cookies,
            )?;
            Ok(StepResult::success(
                idx,
                format!("Set {} cookie(s)", cookies.len()),
            ))
        }
        BrowserStep::ClearCookies { name, domain, path } => {
            let cleared = refact_browser::context_state::clear_cookies(
                runtime
                    .get_active_tab()
                    .ok_or_else(|| "No active tab in browser runtime".to_string())?
                    .as_ref(),
                name.as_deref(),
                domain.as_deref(),
                path.as_deref(),
            )?;
            Ok(StepResult::success(
                idx,
                format!("Cleared {cleared} cookie(s)"),
            ))
        }
        BrowserStep::GetStorage { kind, origin } => {
            let items = refact_browser::context_state::get_storage(
                runtime
                    .get_active_tab()
                    .ok_or_else(|| "No active tab in browser runtime".to_string())?
                    .as_ref(),
                *kind,
                origin.as_deref(),
            )?;
            let masked = items
                .iter()
                .map(|item| BrowserStorageItem {
                    name: item.name.clone(),
                    value: "[REDACTED]".to_string(),
                })
                .collect::<Vec<_>>();
            Ok(
                StepResult::success(idx, format!("Read {} storage item(s)", items.len()))
                    .with_data(serde_json::json!({"items": masked})),
            )
        }
        BrowserStep::SetStorage { kind, items } => {
            refact_browser::context_state::set_storage(
                runtime
                    .get_active_tab()
                    .ok_or_else(|| "No active tab in browser runtime".to_string())?
                    .as_ref(),
                *kind,
                items,
            )?;
            Ok(StepResult::success(
                idx,
                format!("Set {} storage item(s)", items.len()),
            ))
        }
        BrowserStep::ClearStorage { kind } => {
            refact_browser::context_state::clear_storage(
                runtime
                    .get_active_tab()
                    .ok_or_else(|| "No active tab in browser runtime".to_string())?
                    .as_ref(),
                *kind,
            )?;
            Ok(StepResult::success(idx, "Cleared storage"))
        }
        BrowserStep::StorageState {
            save_as,
            indexed_db,
        } => {
            let state = refact_browser::context_state::storage_state(
                runtime
                    .get_active_tab()
                    .ok_or_else(|| "No active tab in browser runtime".to_string())?
                    .as_ref(),
                indexed_db.unwrap_or(false),
            )?;
            let masked = refact_browser::context_state::mask_storage_state(&state);
            let artifact = save_as
                .as_deref()
                .map(|save_as| {
                    refact_browser::context_state::save_storage_state(
                        &state,
                        &runtime.artifacts_dir,
                        save_as,
                    )
                })
                .transpose()?;
            match artifact {
                Some(artifact) => Ok(StepResult::success(
                    idx,
                    format!(
                        "Saved storage state to {} ({} bytes)",
                        artifact.path.display(),
                        artifact.bytes
                    ),
                )
                .with_data(serde_json::json!({
                    "state": masked,
                    "artifact": {
                        "kind": "storage_state",
                        "mime": "application/json",
                        "path": artifact.path,
                        "bytes": artifact.bytes,
                    }
                }))),
                None => Ok(StepResult::success(idx, "Captured storage state")
                    .with_data(serde_json::json!({"state": masked}))),
            }
        }
        BrowserStep::SetStorageState { state, indexed_db } => {
            refact_browser::context_state::set_storage_state(
                runtime
                    .get_active_tab()
                    .ok_or_else(|| "No active tab in browser runtime".to_string())?
                    .as_ref(),
                state,
                indexed_db.unwrap_or(false),
            )?;
            Ok(StepResult::success(idx, "Restored storage state"))
        }
        BrowserStep::GrantPermissions {
            permissions,
            origin,
            state,
        } => {
            refact_browser::context_state::grant_permissions(
                runtime
                    .get_active_tab()
                    .ok_or_else(|| "No active tab in browser runtime".to_string())?
                    .as_ref(),
                permissions,
                origin.clone(),
                *state,
            )?;
            runtime.context_state.permissions =
                apply_permission_state(&runtime.context_state.permissions, permissions, *state);
            Ok(StepResult::success(
                idx,
                format!(
                    "Set {} permission(s) to {}",
                    permissions.len(),
                    state.label()
                ),
            ))
        }
        BrowserStep::ClearPermissions => {
            refact_browser::context_state::clear_permissions(
                runtime
                    .get_active_tab()
                    .ok_or_else(|| "No active tab in browser runtime".to_string())?
                    .as_ref(),
            )?;
            runtime.context_state.permissions.clear();
            Ok(StepResult::success(idx, "Cleared permissions"))
        }
        BrowserStep::SetHttpCredentials { username, password } => {
            runtime.set_http_credentials(username.clone(), password.clone())?;
            Ok(StepResult::success(idx, "Set HTTP credentials"))
        }
        _ => unreachable!(),
    })();
    result.unwrap_or_else(|error| StepResult::failure(idx, "Browser context", error))
}

pub(crate) fn execute_route_management_step(
    runtime: &mut BrowserRuntime,
    step: &BrowserStep,
    idx: usize,
) -> Option<StepResult> {
    match step {
        BrowserStep::Route {
            pattern,
            handler,
            times,
        } => Some(
            match runtime.add_route(pattern.clone(), handler.clone(), *times) {
                Ok(()) => StepResult::success(idx, "Added network route")
                    .with_data(serde_json::json!({"routes": runtime.route_registry.list()})),
                Err(error) => StepResult::failure(idx, "Add network route", error),
            },
        ),
        BrowserStep::Unroute { pattern } => Some(match runtime.remove_routes(pattern.as_ref()) {
            Ok(removed) => StepResult::success(
                idx,
                if pattern.is_some() {
                    format!("Removed {removed} matching network route(s)")
                } else {
                    format!("Removed all {removed} network route(s)")
                },
            )
            .with_data(serde_json::json!({"routes": runtime.route_registry.list()})),
            Err(error) => StepResult::failure(idx, "Remove network routes", error),
        }),
        BrowserStep::ListRoutes => {
            let routes = runtime.route_registry.list();
            Some(
                StepResult::success(idx, format!("Listed {} network route(s)", routes.len()))
                    .with_data(serde_json::json!({"routes": routes})),
            )
        }
        BrowserStep::RouteWebSocket {
            pattern,
            mode,
            on_page_message,
            on_server_message,
        } => Some(
            match runtime.websocket_registry.add_route(
                pattern.clone(),
                *mode,
                *on_page_message,
                *on_server_message,
            ) {
                Ok(()) => StepResult::success(idx, "Added WebSocket route").with_data(
                    serde_json::json!({"route_count": runtime.websocket_registry.route_count()}),
                ),
                Err(error) => StepResult::failure(idx, "Add WebSocket route", error),
            },
        ),
        BrowserStep::UnrouteWebSocket { pattern } => {
            let removed = runtime.websocket_registry.remove_routes(pattern.as_ref());
            Some(StepResult::success(
                idx,
                if pattern.is_some() {
                    format!("Removed {removed} matching WebSocket route(s)")
                } else {
                    format!("Removed all {removed} WebSocket route(s)")
                },
            ))
        }
        BrowserStep::SendWebSocketMessage { pattern, text } => {
            let result = runtime.websocket_registry.send_to_page(pattern, text);
            Some(match flush_websocket_commands(runtime, result) {
                Ok(sent) => {
                    StepResult::success(idx, format!("Sent WebSocket message to {sent} socket(s)"))
                }
                Err(error) => StepResult::failure(idx, "Send WebSocket message", error),
            })
        }
        BrowserStep::CloseWebSocket {
            pattern,
            code,
            reason,
        } => {
            let result =
                runtime
                    .websocket_registry
                    .close_sockets(pattern, *code, reason.as_deref());
            Some(match flush_websocket_commands(runtime, result) {
                Ok(route_ids) => {
                    runtime.websocket_registry.forget_sockets(&route_ids);
                    let closed = route_ids.len();
                    StepResult::success(idx, format!("Closed {closed} WebSocket(s)")).with_data(
                        serde_json::json!({"closed": closed, "code": code, "reason": reason}),
                    )
                }
                Err(error) => StepResult::failure(idx, "Close WebSocket", error),
            })
        }
        BrowserStep::StartHarRecording {
            path,
            mode,
            content,
            url_filter,
            update,
        } => Some(
            match runtime.har_recorder.start(
                &runtime.artifacts_dir,
                path.as_deref(),
                *mode,
                *content,
                url_filter.as_ref(),
                update.as_deref(),
            ) {
                Ok(path) => StepResult::success(idx, "Started HAR recording")
                    .with_data(serde_json::json!({"path": path})),
                Err(error) => StepResult::failure(idx, "Start HAR recording", error),
            },
        ),
        BrowserStep::StopHarRecording => Some(match runtime.har_recorder.stop() {
            Ok(summary) => StepResult::success(
                idx,
                match &summary.updated_from {
                    Some(_) => format!(
                        "Updated HAR to {} entries ({} replaced, {} appended, {} bytes)",
                        summary.entry_count,
                        summary.replaced_entries,
                        summary.appended_entries,
                        summary.bytes
                    ),
                    None => format!(
                        "Saved HAR with {} entries ({} bytes)",
                        summary.entry_count, summary.bytes
                    ),
                },
            )
            .with_data(serde_json::json!({"artifact": {
                "kind": "har",
                "mime": "application/json",
                "path": summary.path,
                "bytes": summary.bytes,
                "entry_count": summary.entry_count,
                "updated_from": summary.updated_from,
                "replaced_entries": summary.replaced_entries,
                "appended_entries": summary.appended_entries
            }})),
            Err(error) => StepResult::failure(idx, "Stop HAR recording", error),
        }),
        BrowserStep::RouteFromHar {
            path,
            url_filter,
            not_found,
        } => Some(
            match runtime
                .contained_path(path, "HAR replay")
                .and_then(|resolved| {
                    refact_browser::har::HarReplay::load(&resolved, url_filter.as_ref(), *not_found)
                })
                .and_then(|replay| runtime.set_har_replay(replay))
            {
                Ok(()) => StepResult::success(idx, "Added HAR replay route")
                    .with_data(serde_json::json!({"routes": runtime.route_registry.list()})),
                Err(error) => StepResult::failure(idx, "Route from HAR", error),
            },
        ),
        _ => None,
    }
}

pub(crate) fn execute_init_script_step(
    runtime: &mut BrowserRuntime,
    step: &BrowserStep,
    idx: usize,
) -> StepResult {
    let tabs = runtime
        .browser
        .get_tabs()
        .lock()
        .map(|tabs| tabs.iter().cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    match step {
        BrowserStep::AddInitScript { content } => {
            for tab in &tabs {
                if let Err(error) = runtime.world_manager.ensure_utility_world(tab) {
                    return StepResult::failure(idx, "Add init script", error);
                }
            }
            match runtime.init_scripts.add(&tabs, content.clone()) {
                Ok(id) => StepResult::success(idx, format!("Added init script {id}"))
                    .with_data(serde_json::json!({"id": id})),
                Err(error) => StepResult::failure(idx, "Add init script", error),
            }
        }
        BrowserStep::RemoveInitScript { id } => match runtime.init_scripts.remove(&tabs, id) {
            Ok(()) => StepResult::success(idx, format!("Removed init script {id}"))
                .with_data(serde_json::json!({"id": id})),
            Err(error) => StepResult::failure(idx, "Remove init script", error),
        },
        _ => StepResult::failure(idx, "Init script", "Unsupported init script step"),
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct BrowserResetCounts {
    pub(crate) routes: usize,
    pub(crate) har_replays: usize,
    pub(crate) websocket_routes: usize,
    pub(crate) locator_handlers: usize,
    pub(crate) authenticators: usize,
    pub(crate) init_scripts: usize,
    pub(crate) clock: bool,
    pub(crate) service_worker_block: bool,
}

impl BrowserResetCounts {
    pub(crate) fn summary(&self) -> String {
        format!(
            "Reset: {}, {}, {}, {}, {}, {}, offline off, throttling off, emulation and device cleared, {}, {}",
            counted(self.routes, "route"),
            counted(self.har_replays, "har replay"),
            counted(self.websocket_routes, "ws route"),
            counted(self.locator_handlers, "locator handler"),
            counted(self.authenticators, "authenticator"),
            counted(self.init_scripts, "init script"),
            if self.clock {
                "clock cleared"
            } else {
                "clock off"
            },
            if self.service_worker_block {
                "service worker block cleared"
            } else {
                "service worker block off"
            },
        )
    }

    pub(crate) fn data(&self) -> Value {
        serde_json::json!({
            "reset": {
                "routes": self.routes,
                "har_replays": self.har_replays,
                "websocket_routes": self.websocket_routes,
                "locator_handlers": self.locator_handlers,
                "authenticators": self.authenticators,
                "init_scripts": self.init_scripts,
                "offline": false,
                "throttling_cleared": true,
                "emulation_cleared": true,
                "clock_cleared": self.clock,
                "service_worker_block_cleared": self.service_worker_block,
            }
        })
    }
}

pub(crate) fn counted(count: usize, singular: &str) -> String {
    if count == 1 {
        format!("{count} {singular}")
    } else {
        format!("{count} {singular}s")
    }
}

pub(crate) fn flush_websocket_commands<T>(
    runtime: &BrowserRuntime,
    result: Result<T, String>,
) -> Result<T, String> {
    let tabs = runtime
        .browser
        .get_tabs()
        .lock()
        .map(|tabs| tabs.iter().cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    let count = result?;
    runtime.websocket_registry.flush_commands(&tabs)?;
    Ok(count)
}

pub(crate) fn reset_sticky_registries(
    routes: &[RouteInfo],
    websocket_registry: &WebSocketRegistry,
    locator_handlers: &Mutex<LocatorHandlerRegistry>,
    authenticators: usize,
    init_scripts: usize,
    clock: bool,
    service_worker_block: bool,
) -> Result<BrowserResetCounts, String> {
    let har_replays = routes.iter().filter(|route| route.har.is_some()).count();
    Ok(BrowserResetCounts {
        routes: routes.len() - har_replays,
        har_replays,
        websocket_routes: websocket_registry.remove_routes(None),
        locator_handlers: locator_handlers
            .lock()
            .map_err(|error| format!("Failed to lock locator handlers: {error}"))?
            .reset(),
        authenticators,
        init_scripts,
        clock,
        service_worker_block,
    })
}

pub(crate) fn execute_reset_step(runtime: &mut BrowserRuntime, idx: usize) -> StepResult {
    let tabs = runtime
        .browser
        .get_tabs()
        .lock()
        .map(|tabs| tabs.iter().cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    let result: Result<BrowserResetCounts, String> = (|| {
        let routes = runtime.route_registry.list();
        runtime.remove_routes(None)?;
        let authenticators = runtime.webauthn_manager.cleanup(&tabs);
        let clock = runtime.clock.reset(&tabs)?;
        let init_scripts = runtime.init_scripts.reset(&tabs)?;
        let service_worker_block = runtime.context_state.block_service_workers;
        let counts = reset_sticky_registries(
            &routes,
            &runtime.websocket_registry,
            &runtime.locator_handlers,
            authenticators,
            init_scripts,
            clock,
            service_worker_block,
        )?;
        runtime.context_state.clear_overrides(&tabs)?;
        Ok(counts)
    })();
    match result {
        Ok(counts) => StepResult::success(idx, counts.summary()).with_data(counts.data()),
        Err(error) => StepResult::failure(idx, "Reset", error),
    }
}

pub(crate) fn save_cdp_result(
    rendered: &str,
    artifacts_dir: &Path,
    file_name: &str,
) -> Result<PathBuf, String> {
    std::fs::create_dir_all(artifacts_dir).map_err(|error| {
        format!(
            "Failed to create browser artifacts directory {}: {error}",
            artifacts_dir.display()
        )
    })?;
    let path = artifacts_dir.join(file_name);
    std::fs::write(&path, rendered).map_err(|error| {
        format!(
            "Failed to save the CDP result artifact {}: {error}",
            path.display()
        )
    })?;
    Ok(path)
}

pub(crate) fn cdp_send_result(
    method: &str,
    target: CdpTarget,
    warnings: Vec<String>,
    result: Value,
    idx: usize,
    artifacts_dir: &Path,
) -> StepResult {
    let redacted = redact_cdp_result(method, result);
    let rendered = serde_json::to_string_pretty(&redacted).unwrap_or_else(|_| redacted.to_string());
    let mut data = serde_json::json!({
        "cdp_send": {
            "method": method,
            "target": target.label(),
            "warnings": warnings,
            "bytes": rendered.len(),
        }
    });
    let entry = &mut data["cdp_send"];
    if rendered.len() > CDP_INLINE_RESULT_LIMIT_BYTES {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        match save_cdp_result(&rendered, artifacts_dir, &format!("cdp-{nonce}-{idx}.json")) {
            Ok(path) => {
                entry["artifact"] = serde_json::json!({
                    "kind": "cdp_result",
                    "mime": "application/json",
                    "path": path,
                    "bytes": rendered.len(),
                })
            }
            Err(error) => return StepResult::failure(idx, "cdp_send", error),
        }
    } else {
        entry["result"] = redacted;
    }
    let suffix = if warnings.is_empty() {
        String::new()
    } else {
        format!(" ({})", counted(warnings.len(), "warning"))
    };
    StepResult::success(
        idx,
        format!(
            "{method} on {} returned {} bytes{suffix}",
            target.label(),
            rendered.len()
        ),
    )
    .with_data(data)
}

pub(crate) fn execute_cdp_send_step(
    runtime: &mut BrowserRuntime,
    method: &str,
    params: Option<&Value>,
    target: CdpTarget,
    idx: usize,
) -> StepResult {
    let own_target_id = runtime
        .get_active_tab()
        .map(|tab| tab.get_target_id().to_string());
    let warnings = match classify_cdp_command(method, params, own_target_id.as_deref()) {
        CdpGuardrail::Denied { reason } => {
            return StepResult::failure(idx, format!("cdp_send {method} blocked"), reason);
        }
        CdpGuardrail::Allowed { warnings } => warnings,
    };
    let target_id = match target {
        CdpTarget::Page => match own_target_id {
            Some(target_id) => Some(target_id),
            None => {
                return StepResult::failure(
                    idx,
                    "cdp_send",
                    "No active tab in browser runtime".to_string(),
                );
            }
        },
        CdpTarget::Browser => None,
    };
    let artifacts_dir = runtime.artifacts_dir.clone();
    let session = match runtime.cdp_session() {
        Ok(session) => session,
        Err(error) => return StepResult::failure(idx, "cdp_send", error),
    };
    runtime.touch();
    match session.send(method, params, target_id.as_deref()) {
        Ok(result) => cdp_send_result(method, target, warnings, result, idx, &artifacts_dir),
        Err(error) => StepResult::failure(idx, format!("cdp_send {method} failed"), error),
    }
}

pub(crate) fn is_screencast_step(step: &BrowserStep) -> bool {
    matches!(
        step,
        BrowserStep::CaptureFrames { .. }
            | BrowserStep::ScreencastStart { .. }
            | BrowserStep::ScreencastStop { .. }
    )
}

pub(crate) fn screencast_quality(quality: Option<u32>) -> Result<u32, String> {
    match quality {
        Some(quality) if quality > 100 => Err(format!(
            "Screencast quality {quality} must be between 0 and 100"
        )),
        Some(quality) => Ok(quality),
        None => Ok(DEFAULT_SCREENCAST_QUALITY),
    }
}

pub(crate) fn burst_screenshot_options(policy: &ImagePolicy, full_page: bool) -> BrowserScreenshotOptions {
    BrowserScreenshotOptions {
        full_page,
        image_type: Some(BrowserScreenshotType::Jpeg),
        quality: policy.quality,
        ..Default::default()
    }
}

pub(crate) fn capture_scoped_frame(
    tab: &Tab,
    world: &WorldManager,
    locator: Option<&BrowserLocator>,
    options: &BrowserScreenshotOptions,
    policy: &ImagePolicy,
) -> Result<Vec<u8>, String> {
    let element = match locator {
        Some(locator) => {
            let resolved = resolve_element(tab, world, locator)?;
            let clip = resolved
                .bbox
                .as_ref()
                .filter(|bbox| bbox.width > 0.0 && bbox.height > 0.0)
                .ok_or_else(|| "Element has no visible bounds".to_string())
                .and_then(|bbox| {
                    screenshot_metrics(tab).map(|metrics| BrowserScreenshotClip {
                        x: metrics.page_x + bbox.x,
                        y: metrics.page_y + bbox.y,
                        width: bbox.width,
                        height: bbox.height,
                    })
                });
            let _ = world.release_handle(tab, &resolved.handle);
            Some(clip?)
        }
        None => None,
    };
    let capture = capture_screenshot(tab, world, options, element, policy)?;
    base64::prelude::BASE64_STANDARD
        .decode(capture.data)
        .map_err(|error| format!("Screencast frame decode failed: {error}"))
}

pub(crate) fn capture_frame_burst(
    tab: &Tab,
    world: &WorldManager,
    plan: &FrameBurstPlan,
    locator: Option<&BrowserLocator>,
    full_page: bool,
    policy: &ImagePolicy,
) -> Result<(Vec<CapturedFrame>, Vec<String>), String> {
    let scoped = locator.is_some() || full_page;
    if !scoped {
        let quality = policy
            .quality
            .map(u32::from)
            .unwrap_or(DEFAULT_SCREENCAST_QUALITY)
            .min(100);
        let frames = capture_screencast_burst(tab, plan, quality)?;
        if frames.len() >= 2 {
            return Ok((frames, Vec::new()));
        }
    }
    let options = burst_screenshot_options(policy, full_page && locator.is_none());
    let reason = if locator.is_some() {
        "element-scoped frames use timed screenshots"
    } else if full_page {
        "full-page frames use timed screenshots"
    } else {
        "the screencast produced too few frames, captured with timed screenshots instead"
    };
    let frames = capture_timed_frames(
        plan,
        || capture_scoped_frame(tab, world, locator, &options, policy),
        |remaining| std::thread::sleep(remaining),
    )?;
    Ok((frames, vec![reason.to_string()]))
}

pub(crate) fn filmstrip_step_data(result: &FilmstripResult) -> Value {
    serde_json::json!({
        "mime": result.filmstrip.mime,
        "data": result.filmstrip_data,
        "width": result.filmstrip.width,
        "height": result.filmstrip.height,
        "bytes": result.filmstrip.bytes,
        "artifact": result.filmstrip,
        "frames": result.frames,
        "frame_count": result.frames.len(),
        "columns": result.columns,
        "rows": result.rows,
        "duration_ms": result.duration_ms,
        "warnings": result.warnings,
    })
}

pub(crate) fn execute_screencast_step(
    runtime: &mut BrowserRuntime,
    step: &BrowserStep,
    idx: usize,
    image_policy: &ImagePolicy,
) -> StepResult {
    let result: Result<StepResult, String> = (|| {
        let tab = runtime
            .get_active_tab()
            .ok_or_else(|| "No active tab in browser runtime".to_string())?;
        match step {
            BrowserStep::CaptureFrames {
                duration_ms,
                frame_count,
                interval_ms,
                locator,
                full_page,
            } => {
                let plan = FrameBurstPlan::resolve(*duration_ms, *frame_count, *interval_ms)?;
                let (frames, warnings) = capture_frame_burst(
                    &tab,
                    &runtime.world_manager,
                    &plan,
                    locator.as_ref(),
                    full_page.unwrap_or(false),
                    image_policy,
                )?;
                let captured = frames.len();
                let filmstrip = build_filmstrip(
                    &frames,
                    &runtime.artifacts_dir,
                    "burst",
                    image_policy,
                    warnings,
                )?;
                Ok(StepResult::success(
                    idx,
                    format!("Captured {captured} frame(s) over {}ms", plan.duration_ms),
                )
                .with_data(filmstrip_step_data(&filmstrip)))
            }
            BrowserStep::ScreencastStart {
                quality,
                max_width,
                max_height,
            } => {
                runtime.screencast_manager.start(
                    &tab,
                    ScreencastSessionOptions {
                        quality: screencast_quality(*quality)?,
                        max_width: *max_width,
                        max_height: *max_height,
                    },
                )?;
                Ok(StepResult::success(
                    idx,
                    format!(
                        "Started screencast, auto-stops after {MAX_SESSION_DURATION_MS}ms or {MAX_SESSION_FRAMES} frames"
                    ),
                ))
            }
            BrowserStep::ScreencastStop { compose } => {
                let stopped = runtime.screencast_manager.stop(&tab)?;
                let mut warnings = Vec::new();
                if stopped.auto_stopped {
                    warnings.push(format!(
                        "The screencast auto-stopped at the {MAX_SESSION_DURATION_MS}ms / {MAX_SESSION_FRAMES} frame cap"
                    ));
                }
                let captured = stopped.frames.len();
                if !compose.unwrap_or(true) {
                    return Ok(StepResult::success(
                        idx,
                        format!("Stopped screencast after {captured} frame(s)"),
                    )
                    .with_data(serde_json::json!({
                        "frame_count": captured,
                        "duration_ms": stopped.duration_ms,
                        "warnings": warnings,
                    })));
                }
                let selected = select_evenly_spaced(stopped.frames, MAX_FRAME_COUNT);
                if selected.len() > 1 {
                    warnings.push(format!(
                        "Composed {} of {captured} captured frame(s)",
                        selected.len()
                    ));
                }
                let filmstrip = build_filmstrip(
                    &selected,
                    &runtime.artifacts_dir,
                    "session",
                    image_policy,
                    warnings,
                )?;
                Ok(StepResult::success(
                    idx,
                    format!("Stopped screencast after {captured} frame(s)"),
                )
                .with_data(filmstrip_step_data(&filmstrip)))
            }
            _ => unreachable!(),
        }
    })();
    result.unwrap_or_else(|error| StepResult::failure(idx, "Screencast", error))
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct WindowBoundsRequest {
    pub(crate) left: Option<u32>,
    pub(crate) top: Option<u32>,
    pub(crate) width: Option<u32>,
    pub(crate) height: Option<u32>,
}

pub(crate) fn validate_window_bounds_request(
    x: Option<i32>,
    y: Option<i32>,
    width: Option<u32>,
    height: Option<u32>,
) -> Result<WindowBoundsRequest, String> {
    if x.is_none() && y.is_none() && width.is_none() && height.is_none() {
        return Err("set_window_bounds needs at least one of x, y, width, height".to_string());
    }
    for (name, value) in [("x", x), ("y", y)] {
        if let Some(value) = value {
            if value < 0 {
                return Err(format!(
                    "set_window_bounds {name}={value} is negative; the CDP client only accepts non-negative window positions"
                ));
            }
        }
    }
    for (name, value) in [("width", width), ("height", height)] {
        if value == Some(0) {
            return Err(format!(
                "set_window_bounds {name} must be greater than zero"
            ));
        }
    }
    Ok(WindowBoundsRequest {
        left: x.map(|value| value as u32),
        top: y.map(|value| value as u32),
        width,
        height,
    })
}

pub(crate) fn execute_set_window_bounds_step(
    runtime: &mut BrowserRuntime,
    x: Option<i32>,
    y: Option<i32>,
    width: Option<u32>,
    height: Option<u32>,
    idx: usize,
) -> StepResult {
    let request = match validate_window_bounds_request(x, y, width, height) {
        Ok(request) => request,
        Err(error) => return StepResult::failure(idx, "Set window bounds", error),
    };
    if runtime.headless() {
        return StepResult::success(
            idx,
            "Set window bounds skipped: headless has no OS window, use set_viewport for emulation",
        )
        .with_data(serde_json::json!({"applied": false, "headless": true}));
    }
    let result: Result<StepResult, String> = (|| {
        let tab = runtime
            .get_active_tab()
            .ok_or_else(|| "No active tab in browser runtime".to_string())?;
        tab.set_bounds(headless_chrome::types::Bounds::Normal {
            left: request.left,
            top: request.top,
            width: request.width.map(f64::from),
            height: request.height.map(f64::from),
        })
        .map_err(|error| format!("Failed to set window bounds: {error}"))?;
        let bounds = tab
            .get_bounds()
            .map_err(|error| format!("Failed to read back window bounds: {error}"))?;
        let applied = refact_chat_api::WindowBounds {
            x: bounds.left as i32,
            y: bounds.top as i32,
            width: bounds.width as u32,
            height: bounds.height as u32,
        };
        let summary = format!(
            "Set window to {}x{} at ({}, {})",
            applied.width, applied.height, applied.x, applied.y
        );
        let data = serde_json::json!({
            "applied": true,
            "headless": false,
            "bounds": applied.clone(),
        });
        runtime.set_window_bounds(applied);
        Ok(StepResult::success(idx, summary).with_data(data))
    })();
    match result {
        Ok(step) => step,
        Err(error) => StepResult::failure(idx, "Set window bounds", error),
    }
}

pub(crate) fn is_clock_step(step: &BrowserStep) -> bool {
    matches!(
        step,
        BrowserStep::ClockInstall { .. }
            | BrowserStep::ClockFastForward { .. }
            | BrowserStep::ClockPauseAt { .. }
            | BrowserStep::ClockResume
            | BrowserStep::ClockRunFor { .. }
            | BrowserStep::ClockSetFixedTime { .. }
            | BrowserStep::ClockSetSystemTime { .. }
    )
}

pub(crate) fn clock_op_from_step(step: &BrowserStep) -> Result<ClockOp, String> {
    match step {
        BrowserStep::ClockInstall { time } => Ok(ClockOp::Install {
            time_ms: match time {
                Some(time) => parse_clock_time(time)?,
                None => current_wall_ms(),
            },
        }),
        BrowserStep::ClockFastForward { ticks } => Ok(ClockOp::FastForward {
            ticks_ms: parse_clock_ticks(ticks)?,
        }),
        BrowserStep::ClockPauseAt { time } => Ok(ClockOp::PauseAt {
            time_ms: parse_clock_time(time)?,
        }),
        BrowserStep::ClockResume => Ok(ClockOp::Resume),
        BrowserStep::ClockRunFor { ticks } => Ok(ClockOp::RunFor {
            ticks_ms: parse_clock_ticks(ticks)?,
        }),
        BrowserStep::ClockSetFixedTime { time } => Ok(ClockOp::SetFixedTime {
            time_ms: parse_clock_time(time)?,
        }),
        BrowserStep::ClockSetSystemTime { time } => Ok(ClockOp::SetSystemTime {
            time_ms: parse_clock_time(time)?,
        }),
        _ => unreachable!(),
    }
}

pub(crate) fn execute_clock_step(runtime: &mut BrowserRuntime, step: &BrowserStep, idx: usize) -> StepResult {
    let tabs = runtime
        .browser
        .get_tabs()
        .lock()
        .map(|tabs| tabs.iter().cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    let result: Result<ClockOp, String> = (|| {
        let op = clock_op_from_step(step)?;
        runtime.clock.run(&tabs, op, current_wall_ms())?;
        Ok(op)
    })();
    match result {
        Ok(op) => StepResult::success(idx, op.summary()).with_data(serde_json::json!({
            "clock": {
                "installed": runtime.clock.is_installed(),
                "paused": runtime.clock.is_paused(),
            }
        })),
        Err(error) => StepResult::failure(idx, "Browser clock", error),
    }
}

pub(crate) struct PreparedHttpRequest {
    pub(crate) spec: http_client::HttpRequestSpec,
    pub(crate) tab: Arc<Tab>,
    pub(crate) jar: Vec<refact_integrations::browser_models::BrowserCookie>,
    pub(crate) artifacts_dir: PathBuf,
}

pub(crate) fn prepare_http_request(
    runtime: &mut BrowserRuntime,
    options: &BrowserHttpRequest,
) -> Result<PreparedHttpRequest, String> {
    let settings = browser_settings::current();
    let artifacts_dir = runtime.artifacts_dir.clone();
    let prepared = (|| -> Result<(http_client::HttpRequestSpec, Arc<Tab>), String> {
        let spec = http_client::HttpRequestSpec {
            url: http_client::parse_http_url(&options.url)?,
            method: http_client::parse_http_method(options.method.as_deref())?,
            headers: options.headers.clone(),
            body: http_client::build_request_body(
                options.body.as_deref(),
                options.body_json.as_ref(),
                options.form.as_ref(),
            )?,
            timeout: Duration::from_millis(
                options
                    .timeout_ms
                    .unwrap_or(http_client::DEFAULT_HTTP_TIMEOUT_MS)
                    .min(settings.timing.max_wait_timeout_ms),
            ),
            max_redirects: options
                .max_redirects
                .unwrap_or(http_client::DEFAULT_HTTP_MAX_REDIRECTS)
                .min(http_client::DEFAULT_HTTP_MAX_REDIRECTS),
        };
        let tab = runtime
            .get_active_tab()
            .ok_or_else(|| "No active tab in browser runtime".to_string())?;
        Ok((spec, tab))
    })();
    let (spec, tab) = prepared?;
    let jar = refact_browser::context_state::get_cookies(&tab, None)?;
    Ok(PreparedHttpRequest {
        spec,
        tab,
        jar,
        artifacts_dir,
    })
}

pub(crate) fn apply_http_response(
    runtime: &mut BrowserRuntime,
    tab: &Tab,
    response: &http_client::HttpResponse,
) -> Result<(), String> {
    if !response.set_cookies.is_empty() {
        refact_browser::context_state::set_cookies(tab, &response.set_cookies)?;
    }
    runtime.touch();
    Ok(())
}

pub(crate) async fn execute_http_request_step(
    runtime: &mut BrowserRuntime,
    options: &BrowserHttpRequest,
    idx: usize,
) -> StepResult {
    let prepared = match prepare_http_request(runtime, options) {
        Ok(prepared) => prepared,
        Err(error) => return StepResult::failure(idx, "HTTP request", error),
    };
    let response = match http_client::send_http_request(&prepared.spec, &prepared.jar).await {
        Ok(response) => response,
        Err(error) => return StepResult::failure(idx, "HTTP request", error),
    };
    if let Err(error) = apply_http_response(runtime, &prepared.tab, &response) {
        return StepResult::failure(idx, "HTTP request", error);
    }
    http_request_result(&response, options, idx, &prepared.artifacts_dir)
}

pub(crate) fn http_request_result(
    response: &http_client::HttpResponse,
    options: &BrowserHttpRequest,
    idx: usize,
    artifacts_dir: &Path,
) -> StepResult {
    let content_type = response
        .headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("content-type"))
        .map(|(_, value)| value.clone());
    let mut data = serde_json::json!({
        "http_request": {
            "method": response.method,
            "url": refact_browser::mask_text(&response.final_url),
            "status": response.status,
            "status_text": response.status_text,
            "redirects": response.redirects,
            "headers": http_client::summarize_response_headers(
                &response.headers,
                options.full_headers.unwrap_or(false),
            ),
            "body_bytes": response.body.len(),
            "set_cookies": {
                "count": response.set_cookies.len(),
                "names": response.set_cookies.iter().map(|cookie| cookie.name.clone()).collect::<Vec<_>>(),
            },
            "rejected_cookies": response.rejected_cookies,
        }
    });
    let entry = &mut data["http_request"];
    match http_client::split_response_body(&response.body, content_type.as_deref()) {
        http_client::HttpResponseBody::Empty => {}
        http_client::HttpResponseBody::Inline(text) => entry["body"] = Value::String(text),
        http_client::HttpResponseBody::Artifact => {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis();
            let file_name = format!(
                "http-{nonce}-{idx}.{}",
                http_client::response_body_extension(content_type.as_deref())
            );
            match http_client::save_response_body(&response.body, artifacts_dir, &file_name) {
                Ok(path) => {
                    entry["artifact"] = serde_json::json!({
                        "path": path,
                        "bytes": response.body.len(),
                    })
                }
                Err(error) => return StepResult::failure(idx, "HTTP request", error),
            }
        }
    }
    let summary = format!(
        "{} {} -> {} {} ({} bytes, {} cookies set)",
        response.method,
        refact_browser::mask_text(&response.final_url),
        response.status,
        response.status_text,
        response.body.len(),
        response.set_cookies.len()
    );
    if options.fail_on_status.unwrap_or(false) && !(200..300).contains(&response.status) {
        return StepResult::failure(
            idx,
            summary,
            format!("HTTP {} {}", response.status, response.status_text),
        )
        .with_data(data);
    }
    StepResult::success(idx, summary).with_data(data)
}

pub(crate) fn is_instrumentation_step(step: &BrowserStep) -> bool {
    matches!(
        step,
        BrowserStep::StartCoverage { .. }
            | BrowserStep::StopCoverage
            | BrowserStep::AddVirtualAuthenticator { .. }
            | BrowserStep::RemoveVirtualAuthenticator { .. }
            | BrowserStep::ListCredentials { .. }
            | BrowserStep::AddCredential { .. }
            | BrowserStep::ClearCredentials { .. }
            | BrowserStep::SetUserVerified { .. }
    )
}

pub(crate) fn virtual_authenticator_added(idx: usize, id: String) -> StepResult {
    StepResult::success(idx, format!("Added virtual authenticator {id}"))
        .with_data(serde_json::json!({"authenticator_id": id}))
}

pub(crate) fn execute_instrumentation_step(
    runtime: &mut BrowserRuntime,
    step: &BrowserStep,
    idx: usize,
) -> StepResult {
    let result: Result<StepResult, String> = (|| {
        let tab = runtime
            .get_active_tab()
            .ok_or_else(|| "No active tab in browser runtime".to_string())?;
        match step {
            BrowserStep::StartCoverage {
                js,
                css,
                reset_on_navigation,
            } => {
                let options = refact_browser::coverage::CoverageOptions::resolve(
                    *js,
                    *css,
                    *reset_on_navigation,
                );
                runtime.coverage_manager.start(&tab, options)?;
                Ok(StepResult::success(
                    idx,
                    format!(
                        "Started coverage (JavaScript: {}, CSS: {})",
                        options.js, options.css
                    ),
                ))
            }
            BrowserStep::StopCoverage => {
                let stopped = runtime
                    .coverage_manager
                    .stop(&tab, &runtime.artifacts_dir)?;
                let resource_count = stopped.artifact.resource_count;
                let unmeasured = stopped.artifact.unmeasured_resource_count;
                let message = if unmeasured > 0 {
                    format!(
                        "Stopped coverage for {resource_count} resource(s), {unmeasured} \
                         unmeasured (source retrieval failed, no percentage reported)"
                    )
                } else {
                    format!("Stopped coverage for {resource_count} resource(s)")
                };
                Ok(
                    StepResult::success(idx, message).with_data(serde_json::json!({
                        "coverage": stopped.summaries,
                        "artifact": stopped.artifact,
                    })),
                )
            }
            BrowserStep::AddVirtualAuthenticator {
                protocol,
                transport,
                has_resident_key,
                has_user_verification,
                is_user_verified,
                ..
            } => {
                let id = runtime.webauthn_manager.add_virtual_authenticator(
                    &tab,
                    protocol.unwrap_or_default(),
                    transport.unwrap_or_default(),
                    has_resident_key.unwrap_or(false),
                    has_user_verification.unwrap_or(false),
                    is_user_verified.unwrap_or(false),
                )?;
                Ok(virtual_authenticator_added(idx, id))
            }
            BrowserStep::RemoveVirtualAuthenticator { id } => {
                runtime
                    .webauthn_manager
                    .remove_virtual_authenticator(&tab, id)?;
                Ok(StepResult::success(idx, "Removed virtual authenticator"))
            }
            BrowserStep::ListCredentials { id } => {
                let credentials = runtime.webauthn_manager.list_credentials(&tab, id)?;
                Ok(
                    StepResult::success(idx, format!("Listed {} credential(s)", credentials.len()))
                        .with_data(serde_json::json!({"credentials": credentials})),
                )
            }
            BrowserStep::AddCredential { id, credential } => {
                runtime
                    .webauthn_manager
                    .add_credential(&tab, id, credential)?;
                Ok(StepResult::success(
                    idx,
                    "Added virtual authenticator credential",
                ))
            }
            BrowserStep::ClearCredentials { id } => {
                runtime.webauthn_manager.clear_credentials(&tab, id)?;
                Ok(StepResult::success(
                    idx,
                    "Cleared virtual authenticator credentials",
                ))
            }
            BrowserStep::SetUserVerified { id, verified } => {
                runtime
                    .webauthn_manager
                    .set_user_verified(&tab, id, *verified)?;
                Ok(StepResult::success(
                    idx,
                    format!("Set virtual authenticator user verification to {verified}"),
                ))
            }
            _ => unreachable!(),
        }
    })();
    result.unwrap_or_else(|error| StepResult::failure(idx, "Browser instrumentation", error))
}
