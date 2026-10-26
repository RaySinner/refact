use super::*;
use super::capture::{drive_element_state_actions};
use super::expect_poll::{evaluate_expectation};
use std::collections::BTreeMap;

pub(crate) const DEAD_TRANSPORT: &str =
    "Unable to make method calls because underlying connection is closed";


#[derive(Clone, Default)]
pub(crate) struct RecordingMouse {
    pub(crate) events: Arc<Mutex<Vec<(refact_browser::MouseEventType, Option<MouseButton>)>>>,
}


impl MouseDispatcher for RecordingMouse {
    fn dispatch(&mut self, event: refact_browser::MouseDispatch) -> Result<(), String> {
        if let refact_browser::MouseDispatch::Mouse(payload) = event {
            self.events
                .lock()
                .unwrap()
                .push((payload.event_type, payload.button));
        }
        Ok(())
    }
}


pub(crate) struct SilentKeyboard;

impl KeyboardDispatcher for SilentKeyboard {
    fn dispatch(&mut self, _event: refact_browser::KeyboardDispatch) -> Result<(), String> {
        Ok(())
    }
}


pub(crate) fn button_events(
    recorded: &RecordingMouse,
) -> Vec<(refact_browser::MouseEventType, Option<MouseButton>)> {
    recorded
        .events
        .lock()
        .unwrap()
        .iter()
        .filter(|(event_type, _)| {
            matches!(
                event_type,
                refact_browser::MouseEventType::Pressed
                    | refact_browser::MouseEventType::Released
            )
        })
        .cloned()
        .collect()
}


pub(crate) fn drive_states<F>(
    recorded: &RecordingMouse,
    states: &[BrowserElementState],
    observe: F,
) -> Result<Vec<(BrowserElementState, ())>, String>
where
    F: FnMut(BrowserElementState) -> Result<(), String>,
{
    let keyboard = Keyboard::new(SilentKeyboard);
    let mut mouse = Mouse::new(recorded.clone(), &keyboard);
    drive_element_state_actions(
        &mut mouse,
        element_state_sequence(states),
        || Ok(MainFrameCssPoint { x: 5.0, y: 5.0 }),
        |_| Ok(()),
        observe,
    )
}


pub(crate) fn report_with_step_error(error: Option<&str>) -> ExecutionReport {
    let step = match error {
        Some(error) => StepResult::failure(0, "OpenTab", error.to_string()),
        None => StepResult::success(0, "OpenTab"),
    };
    report_with_steps(vec![step])
}


pub(crate) fn report_with_steps(steps: Vec<StepResult>) -> ExecutionReport {
    ExecutionReport {
        ok: steps.iter().all(|step| step.ok),
        steps,
        warnings: Vec::new(),
        url: None,
        title: None,
        page: None,
        stabilized: false,
        console: vec![],
        page_errors: vec![],
        network: vec![],
        network_summary: vec![],
        websockets: vec![],
        locator_handlers: vec![],
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


pub(crate) fn section<T: serde::de::DeserializeOwned>(value: Value) -> Vec<T> {
    vec![serde_json::from_value(value).unwrap()]
}


pub(crate) fn nested_tab_management_report() -> ExecutionReport {
    let mut report = report_with_steps(vec![StepResult::success(0, "Opened new desktop tab")]);
    report.new_tabs = section(serde_json::json!({
        "id": "popup",
        "target_id": "popup",
        "url": "https://example.test/popup",
        "title": "Popup",
        "active": true,
    }));
    report.intercepted_requests = section(serde_json::json!({
        "url": "https://example.test/api",
        "method": "GET",
        "pattern": "**/api",
        "action": "fulfill",
    }));
    report.locator_handlers = section(serde_json::json!({
        "name": "dismiss_overlays",
        "action": "dismiss_overlays",
        "outcome": "dismissed 1 overlay",
        "ok": true,
    }));
    report.dialogs = section(serde_json::json!({
        "type": "confirm",
        "message": "Leave?",
        "default_value": "",
        "action": "accepted",
        "automatic": true,
    }));
    report.uploads = section(serde_json::json!({
        "paths": ["/tmp/report.pdf"],
        "source": "file_chooser",
        "in_memory_payloads": false,
    }));
    report.downloads = section(serde_json::json!({
        "guid": "download-1",
        "url": "https://example.test/report.pdf",
        "frame_id": "frame-1",
        "suggested_filename": "report.pdf",
        "local_path": "/tmp/report.pdf",
        "received_bytes": 10,
        "total_bytes": 10,
        "state": "completed",
    }));
    report.websockets = section(serde_json::json!({
        "sequence": 1,
        "socket_id": "socket-1",
        "url": "wss://example.test/live",
        "kind": "frame_received",
    }));
    report
}


pub(crate) fn actionable_state() -> refact_browser::ElementState {
    refact_browser::ElementState {
        visible: true,
        enabled: true,
        editable: Some(true),
        checked: None,
        stable: true,
    }
}


pub(crate) fn locator_handles(count: usize) -> Vec<ElementHandle> {
    (0..count)
        .map(|index| ElementHandle {
            object_id: format!("object-{index}"),
            backend_node_id: None,
            context_id: 1,
            frame_id: "main".to_string(),
        })
        .collect()
}


pub(crate) fn console_entry(level: &str, text: &str) -> refact_integrations::browser_types::ConsoleEntry {
    refact_integrations::browser_types::ConsoleEntry {
        timestamp: 1.0,
        level: level.to_string(),
        text: text.to_string(),
    }
}


pub(crate) fn scripted_expectation(
    matcher: &BrowserExpectation,
    negate: bool,
    timeout_ms: u64,
    samples: Vec<Result<(bool, Value), String>>,
) -> StepResult {
    let mut samples = samples.into_iter();
    let mut last = Ok((false, Value::Null));
    evaluate_expectation(
        0,
        matcher,
        Duration::from_millis(timeout_ms),
        false,
        negate,
        move || {
            if let Some(sample) = samples.next() {
                last = sample.clone();
            }
            last.clone()
        },
    )
}


pub(crate) fn element_info(visible: bool, bbox: Option<ElementBBox>) -> ElementInfo {
    ElementInfo {
        tag: "input".to_string(),
        input_type: Some("text".to_string()),
        id: None,
        name: None,
        placeholder: None,
        aria_label: None,
        role: None,
        visible,
        enabled: true,
        readonly: false,
        content_editable: false,
        value: None,
        inner_text: None,
        bbox,
        field_kind: FieldKind::TextInput,
    }
}


pub(crate) fn http_response(status: u16, body: Vec<u8>, content_type: &str) -> http_client::HttpResponse {
    http_client::HttpResponse {
        status,
        status_text: "OK".to_string(),
        final_url: "https://api.example.test/v1/session".to_string(),
        method: "GET".to_string(),
        redirects: 1,
        headers: BTreeMap::from([
            ("content-type".to_string(), content_type.to_string()),
            ("set-cookie".to_string(), "sid=supersecret".to_string()),
            ("server".to_string(), "fixture".to_string()),
        ]),
        set_cookies: vec![BrowserCookie {
            name: "sid".to_string(),
            value: "supersecret".to_string(),
            domain: "api.example.test".to_string(),
            path: "/".to_string(),
            expires: None,
            http_only: true,
            secure: true,
            same_site: None,
            url: None,
        }],
        rejected_cookies: Vec::new(),
        body,
    }
}


pub(crate) fn populated_route_registry() -> refact_browser::RouteRegistry {
    let registry = refact_browser::RouteRegistry::default();
    registry
        .add(
            UrlPattern::Text("**/api/**".to_string()),
            RouteHandler::Abort {
                reason: "blockedbyclient".to_string(),
            },
            None,
        )
        .unwrap();
    registry
        .add(
            UrlPattern::Text("**/assets/**".to_string()),
            RouteHandler::Abort {
                reason: "failed".to_string(),
            },
            None,
        )
        .unwrap();
    registry
}


pub(crate) fn har_replay_route() -> RouteInfo {
    RouteInfo {
        pattern: UrlPattern::Text("har-replay".to_string()),
        handler: RouteHandler::Abort {
            reason: "blockedbyclient".to_string(),
        },
        har: Some(HarRouteInfo {
            entry_count: 12,
            not_found: HarNotFound::Abort,
        }),
        times_remaining: None,
        order: 0,
    }
}


pub(crate) fn populated_locator_handlers() -> Mutex<LocatorHandlerRegistry> {
    let mut registry = LocatorHandlerRegistry::default();
    for name in ["cookie_banner", "survey_modal", "paywall"] {
        registry.register(
            LocatorHandler::registered(
                name.to_string(),
                BrowserLocator::css("#overlay"),
                LocatorHandlerAction::Click,
                None,
                false,
            )
            .unwrap()
            .unwrap(),
        );
    }
    Mutex::new(registry)
}


pub(crate) fn download(state: DownloadState, failure_reason: Option<&str>) -> DownloadInfo {
    DownloadInfo {
        guid: "guid-1".to_string(),
        url: "https://example.test/report.csv".to_string(),
        frame_id: "frame".to_string(),
        suggested_filename: "report.csv".to_string(),
        local_path: "/tmp/report.csv".to_string(),
        received_bytes: 4,
        total_bytes: 9,
        state,
        failure_reason: failure_reason.map(str::to_string),
    }
}


pub(crate) fn filmstrip_artifact(
    kind: &'static str,
    name: &str,
) -> refact_browser::screencast::FrameArtifact {
    refact_browser::screencast::FrameArtifact {
        kind,
        mime: "image/jpeg".to_string(),
        path: PathBuf::from(format!("/artifacts/{name}")),
        bytes: 2_048,
        width: 320,
        height: 200,
    }
}


pub(crate) fn filmstrip_result() -> FilmstripResult {
    FilmstripResult {
        frames: vec![
            refact_browser::screencast::FrameRecord {
                index: 0,
                offset_ms: 0,
                changed_percent: None,
                artifact: filmstrip_artifact("frame", "frame-00.jpg"),
            },
            refact_browser::screencast::FrameRecord {
                index: 1,
                offset_ms: 500,
                changed_percent: Some(42.5),
                artifact: filmstrip_artifact("frame", "frame-01.jpg"),
            },
        ],
        filmstrip: filmstrip_artifact("filmstrip", "filmstrip.jpg"),
        filmstrip_data: "ZmlsbXN0cmlw".to_string(),
        columns: 2,
        rows: 1,
        duration_ms: 500,
        warnings: vec!["element-scoped frames use timed screenshots".to_string()],
    }
}


pub(crate) fn css_strategy() -> LocatorStrategy {
    LocatorStrategy::Css {
        value: "#submit".to_string(),
    }
}

