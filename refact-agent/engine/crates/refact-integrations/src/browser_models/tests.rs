use std::collections::BTreeMap;

use super::*;

#[test]
fn context_cookie_and_storage_state_serde_round_trip() {
    let state = BrowserStorageState {
        cookies: vec![BrowserCookie {
            name: "session".to_string(),
            value: "secret".to_string(),
            domain: "example.test".to_string(),
            path: "/".to_string(),
            expires: Some(1_900_000_000.0),
            http_only: true,
            secure: true,
            same_site: Some(BrowserCookieSameSite::Lax),
            url: None,
        }],
        origins: vec![BrowserStorageOrigin {
            origin: "https://example.test".to_string(),
            local_storage: vec![BrowserStorageItem {
                name: "logged_in".to_string(),
                value: "true".to_string(),
            }],
            indexed_db: Vec::new(),
        }],
    };
    let json = serde_json::to_value(&state).unwrap();
    assert_eq!(json["origins"][0]["local_storage"][0]["name"], "logged_in");
    assert_eq!(
        serde_json::from_value::<BrowserStorageState>(json).unwrap(),
        state
    );
}

#[test]
fn context_step_serde_round_trip_masks_nothing_in_typed_input() {
    let step = BrowserStep::SetHttpCredentials {
        username: "user".to_string(),
        password: "secret".to_string(),
    };
    let json = serde_json::to_value(&step).unwrap();
    assert_eq!(json["action"], "set_http_credentials");
    assert!(matches!(
        serde_json::from_value::<BrowserStep>(json).unwrap(),
        BrowserStep::SetHttpCredentials { .. }
    ));
}

fn all_browser_steps() -> Vec<BrowserStep> {
    let locator = || BrowserLocator::reference("e1");
    vec![
        BrowserStep::Navigate {
            url: "https://example.com".to_string(),
            timeout_ms: Some(1_000),
        },
        BrowserStep::Reload,
        BrowserStep::GoBack,
        BrowserStep::GoForward,
        BrowserStep::OpenTab {
            device: Some("desktop".to_string()),
            url: Some("https://example.com/new".to_string()),
        },
        BrowserStep::CloseTab { tab: None },
        BrowserStep::SwitchTab {
            tab: TabTarget::Active,
        },
        BrowserStep::ListTabs,
        BrowserStep::WaitForPopup {
            timeout_ms: Some(1_000),
        },
        BrowserStep::Route {
            pattern: UrlPattern::Text("**/api/**".to_string()),
            handler: RouteHandler::Abort {
                reason: "blockedbyclient".to_string(),
            },
            times: Some(2),
        },
        BrowserStep::Unroute { pattern: None },
        BrowserStep::ListRoutes,
        BrowserStep::RouteWebSocket {
            pattern: UrlPattern::Text("wss://example.com/**".to_string()),
            mode: WebSocketRouteMode::Mock,
            on_page_message: WebSocketMessageAction::Forward,
            on_server_message: WebSocketMessageAction::Forward,
        },
        BrowserStep::UnrouteWebSocket { pattern: None },
        BrowserStep::CloseWebSocket {
            pattern: UrlPattern::Text("wss://example.com/**".to_string()),
            code: Some(1001),
            reason: Some("going away".to_string()),
        },
        BrowserStep::SendWebSocketMessage {
            pattern: UrlPattern::Text("wss://example.com/**".to_string()),
            text: "hello".to_string(),
        },
        BrowserStep::WaitForWebSocketFrame {
            pattern: None,
            timeout_ms: Some(1_000),
        },
        BrowserStep::StartHarRecording {
            path: Some("page.har".to_string()),
            mode: HarMode::Full,
            content: HarContentPolicy::Embed,
            url_filter: None,
            update: None,
        },
        BrowserStep::StopHarRecording,
        BrowserStep::RouteFromHar {
            path: "page.har".to_string(),
            url_filter: None,
            not_found: HarNotFound::Abort,
        },
        BrowserStep::Reset,
        BrowserStep::CdpSend {
            method: "Runtime.evaluate".to_string(),
            params: Some(serde_json::json!({"expression": "1 + 1"})),
            target: CdpTarget::Page,
        },
        BrowserStep::ClockInstall {
            time: Some(ClockTime::Text("2020-02-02T00:00:00Z".to_string())),
        },
        BrowserStep::ClockFastForward {
            ticks: ClockTicks::Human("01:00".to_string()),
        },
        BrowserStep::ClockPauseAt {
            time: ClockTime::UnixMillis(1_580_601_600_000),
        },
        BrowserStep::ClockResume,
        BrowserStep::ClockRunFor {
            ticks: ClockTicks::Millis(1000),
        },
        BrowserStep::ClockSetFixedTime {
            time: ClockTime::Text("2020-02-02".to_string()),
        },
        BrowserStep::ClockSetSystemTime {
            time: ClockTime::UnixMillis(1_580_601_600_000),
        },
        BrowserStep::HttpRequest {
            options: BrowserHttpRequest {
                url: "https://example.com/api/session".to_string(),
                ..Default::default()
            },
        },
        BrowserStep::StartCoverage {
            js: Some(true),
            css: Some(true),
            reset_on_navigation: Some(true),
        },
        BrowserStep::StopCoverage,
        BrowserStep::CaptureFrames {
            duration_ms: Some(1_000),
            frame_count: Some(8),
            interval_ms: None,
            locator: Some(locator()),
            full_page: Some(false),
        },
        BrowserStep::ScreencastStart {
            quality: Some(80),
            max_width: Some(1_280),
            max_height: Some(800),
        },
        BrowserStep::ScreencastStop {
            compose: Some(true),
        },
        BrowserStep::AddVirtualAuthenticator {
            id: (),
            protocol: Some(BrowserAuthenticatorProtocol::Ctap2),
            transport: Some(BrowserAuthenticatorTransport::Usb),
            has_resident_key: Some(true),
            has_user_verification: Some(true),
            is_user_verified: Some(true),
        },
        BrowserStep::RemoveVirtualAuthenticator {
            id: "authenticator".to_string(),
        },
        BrowserStep::ListCredentials {
            id: "authenticator".to_string(),
        },
        BrowserStep::AddCredential {
            id: "authenticator".to_string(),
            credential: BrowserWebAuthnCredential {
                credential_id: "credential".to_string(),
                is_resident_credential: true,
                rp_id: Some("example.com".to_string()),
                private_key: "private".to_string(),
                user_handle: Some("user".to_string()),
                sign_count: 0,
                large_blob: None,
                backup_eligibility: None,
                backup_state: None,
                user_name: None,
                user_display_name: None,
            },
        },
        BrowserStep::ClearCredentials {
            id: "authenticator".to_string(),
        },
        BrowserStep::SetUserVerified {
            id: "authenticator".to_string(),
            verified: true,
        },
        BrowserStep::SetViewport {
            width: 390,
            height: 844,
            device_scale_factor: Some(3.0),
            is_mobile: Some(true),
            has_touch: Some(true),
        },
        BrowserStep::SetWindowBounds {
            x: Some(40),
            y: Some(60),
            width: Some(1280),
            height: Some(800),
        },
        BrowserStep::EmulateMedia {
            color_scheme: Some("dark".to_string()),
            reduced_motion: None,
            forced_colors: None,
            contrast: None,
            media: None,
        },
        BrowserStep::SetLocale {
            locale: "ja-JP".to_string(),
        },
        BrowserStep::SetTimezone {
            timezone: "Asia/Tokyo".to_string(),
        },
        BrowserStep::SetUserAgent {
            user_agent: "agent".to_string(),
            accept_language: None,
        },
        BrowserStep::SetGeolocation {
            latitude: 1.0,
            longitude: 2.0,
            accuracy: None,
        },
        BrowserStep::SetOffline { offline: false },
        BrowserStep::SetNetworkConditions {
            offline: Some(false),
            latency_ms: Some(150.0),
            download_kbps: Some(1600.0),
            upload_kbps: Some(750.0),
            preset: Some("slow-3g".to_string()),
        },
        BrowserStep::SetCpuThrottling { rate: 4.0 },
        BrowserStep::EmulateDevice {
            name: "iPhone 13".to_string(),
        },
        BrowserStep::ListDevices {
            filter: Some("pixel".to_string()),
        },
        BrowserStep::SetExtraHttpHeaders {
            headers: BTreeMap::new(),
        },
        BrowserStep::GetCookies { urls: None },
        BrowserStep::SetCookies {
            cookies: Vec::new(),
        },
        BrowserStep::ClearCookies {
            name: None,
            domain: None,
            path: None,
        },
        BrowserStep::GetStorage {
            kind: BrowserStorageKind::Local,
            origin: None,
        },
        BrowserStep::SetStorage {
            kind: BrowserStorageKind::Local,
            items: Vec::new(),
        },
        BrowserStep::ClearStorage {
            kind: BrowserStorageKind::Session,
        },
        BrowserStep::StorageState {
            save_as: Some("auth.json".to_string()),
            indexed_db: Some(true),
        },
        BrowserStep::SetStorageState {
            state: BrowserStorageState::default(),
            indexed_db: None,
        },
        BrowserStep::GrantPermissions {
            permissions: vec!["geolocation".to_string()],
            origin: None,
            state: BrowserPermissionState::Denied,
        },
        BrowserStep::ClearPermissions,
        BrowserStep::SetHttpCredentials {
            username: "user".to_string(),
            password: "secret".to_string(),
        },
        BrowserStep::Expect {
            locator: Some(locator()),
            matcher: BrowserExpectation::ToBeVisible,
            timeout_ms: Some(1_000),
            soft: false,
            not: None,
        },
        BrowserStep::ExpectPoll {
            expression: "window.__count".to_string(),
            expected: serde_json::json!(3),
            matcher: BrowserPollMatcher::Gt,
            timeout_ms: Some(1_000),
            soft: Some(true),
        },
        BrowserStep::Click {
            locator: locator(),
            timeout_ms: None,
        },
        BrowserStep::ClickIfExists { locator: locator() },
        BrowserStep::Tap {
            locator: Some(locator()),
            x: None,
            y: None,
        },
        BrowserStep::Hover {
            locator: locator(),
            timeout_ms: None,
        },
        BrowserStep::Focus { locator: locator() },
        BrowserStep::Blur { locator: locator() },
        BrowserStep::ScrollTo { locator: locator() },
        BrowserStep::PressKey {
            key: "Enter".to_string(),
            modifiers: vec!["Ctrl".to_string()],
        },
        BrowserStep::InsertText {
            locator: Some(locator()),
            text: "hello".to_string(),
        },
        BrowserStep::PressSequentially {
            locator: locator(),
            text: "hello".to_string(),
            delay_ms: Some(10),
        },
        BrowserStep::DragAndDrop {
            source: locator(),
            target: locator(),
            source_position: Some(BrowserPosition { x: 1.0, y: 2.0 }),
            target_position: Some(BrowserPosition { x: 3.0, y: 4.0 }),
        },
        BrowserStep::DropFiles {
            target: locator(),
            paths: vec!["/tmp/file".to_string()],
        },
        BrowserStep::MouseMove {
            x: 10.0,
            y: 20.0,
            steps: Some(2),
        },
        BrowserStep::MouseDown {
            button: BrowserMouseButton::Left,
        },
        BrowserStep::MouseUp {
            button: BrowserMouseButton::Left,
        },
        BrowserStep::MouseClickXy {
            x: 10.0,
            y: 20.0,
            button: BrowserMouseButton::Left,
            click_count: Some(2),
            delay: Some(10),
        },
        BrowserStep::MouseDragXy {
            start_x: 1.0,
            start_y: 2.0,
            end_x: 3.0,
            end_y: 4.0,
        },
        BrowserStep::MouseWheel {
            delta_x: 0.0,
            delta_y: 100.0,
        },
        BrowserStep::Fill {
            locator: locator(),
            text: "hi".to_string(),
            clear_first: true,
            verify: true,
        },
        BrowserStep::Clear {
            locator: locator(),
            verify: true,
        },
        BrowserStep::SelectOption {
            locator: locator(),
            value: "one".to_string(),
        },
        BrowserStep::Check { locator: locator() },
        BrowserStep::Uncheck { locator: locator() },
        BrowserStep::SetInputFiles {
            locator: locator(),
            paths: vec!["/tmp/file".to_string()],
        },
        BrowserStep::ExpectFileChooser {
            paths: vec!["/tmp/file".to_string()],
        },
        BrowserStep::WaitForFunction {
            expression: "() => window.__ready".to_string(),
            locator: Some(locator()),
            timeout_ms: Some(1_000),
            polling_ms: Some(50),
        },
        BrowserStep::WaitForSelector {
            locator: locator(),
            state: Some(BrowserWaitState::Visible),
            timeout_ms: Some(1_000),
        },
        BrowserStep::WaitForNavigation {
            timeout_ms: Some(1_000),
        },
        BrowserStep::WaitForUrl {
            pattern: UrlPattern::Text("/done".to_string()),
            timeout_ms: Some(1_000),
        },
        BrowserStep::WaitForText {
            text: "done".to_string(),
            timeout_ms: Some(1_000),
        },
        BrowserStep::WaitForNetworkIdle {
            timeout_ms: Some(1_000),
        },
        BrowserStep::WaitForLoadState {
            state: BrowserLoadState::Load,
            timeout_ms: Some(1_000),
        },
        BrowserStep::WaitForRequest {
            pattern: UrlPattern::Text("/api".to_string()),
            method: Some("GET".to_string()),
            timeout_ms: Some(1_000),
        },
        BrowserStep::WaitForResponse {
            pattern: UrlPattern::Regex {
                source: "/api".to_string(),
                flags: "i".to_string(),
            },
            method: Some("POST".to_string()),
            status: Some(200),
            timeout_ms: Some(1_000),
        },
        BrowserStep::WaitForConsoleMessage {
            contains: Some("ready".to_string()),
            level: Some(BrowserConsoleLevel::Error),
            timeout_ms: Some(1_000),
        },
        BrowserStep::WaitForDownload {
            timeout_ms: Some(1_000),
            save_as: Some("file.txt".to_string()),
        },
        BrowserStep::CancelDownload { id: None },
        BrowserStep::WaitForElementHidden {
            locator: locator(),
            timeout_ms: Some(1_000),
        },
        BrowserStep::WaitForElementStable {
            locator: locator(),
            timeout_ms: Some(1_000),
        },
        BrowserStep::WaitSeconds { seconds: 0.1 },
        BrowserStep::GetText { locator: locator() },
        BrowserStep::GetHtml { locator: locator() },
        BrowserStep::GetAttribute {
            locator: locator(),
            attribute: "href".to_string(),
        },
        BrowserStep::BoundingBox { locator: locator() },
        BrowserStep::Count { locator: locator() },
        BrowserStep::InputValue { locator: locator() },
        BrowserStep::AllTexts {
            locator: locator(),
            mode: BrowserTextMode::TextContent,
            limit: Some(10),
        },
        BrowserStep::ElementState { locator: locator() },
        BrowserStep::ExtractLinks {
            locator: Some(locator()),
            limit: Some(10),
        },
        BrowserStep::ExtractTable {
            locator: locator(),
            limit: Some(10),
        },
        BrowserStep::DomSnapshot {
            selector: "main".to_string(),
            max_chars: Some(1_000),
        },
        BrowserStep::AccessibilitySnapshot {
            options: AccessibilitySnapshotOptions::default(),
        },
        BrowserStep::Screenshot {
            options: BrowserScreenshotOptions::default(),
        },
        BrowserStep::ScreenshotElement {
            locator: locator(),
            options: BrowserScreenshotOptions::default(),
        },
        BrowserStep::ScreenshotElements {
            locators: vec![locator()],
            compose: BrowserComposeMode::Grid,
            labels: Some(true),
            options: BrowserScreenshotOptions::default(),
        },
        BrowserStep::CaptureElementStates {
            locator: locator(),
            states: vec![BrowserElementState::Default, BrowserElementState::Hover],
            labels: Some(true),
            options: BrowserScreenshotOptions::default(),
        },
        BrowserStep::Pdf {
            options: BrowserPdfOptions::default(),
        },
        BrowserStep::Eval {
            expression: "document.title".to_string(),
            timeout_ms: None,
        },
        BrowserStep::Styles {
            locator: locator(),
            property_filter: Some("color".to_string()),
        },
        BrowserStep::SetContent {
            html: "<p>hi</p>".to_string(),
            wait_until: Some(BrowserLoadState::Load),
        },
        BrowserStep::PageContent,
        BrowserStep::AddScriptTag {
            url: None,
            content: Some("window.ok = 1".to_string()),
            script_type: Some("module".to_string()),
        },
        BrowserStep::AddStyleTag {
            url: Some("https://example.com/app.css".to_string()),
            content: None,
        },
        BrowserStep::AddInitScript {
            content: "window.__flag = 1".to_string(),
        },
        BrowserStep::RemoveInitScript {
            id: "init1".to_string(),
        },
        BrowserStep::DispatchEvent {
            locator: locator(),
            event_type: "click".to_string(),
            event_init: Some(serde_json::json!({"detail": 2})),
        },
        BrowserStep::TabLog,
        BrowserStep::AddLocatorHandler {
            name: "overlay".to_string(),
            locator: locator(),
            handler: LocatorHandlerAction::Click,
            times: Some(1),
            no_wait_after: false,
        },
        BrowserStep::RemoveLocatorHandler {
            name: "overlay".to_string(),
        },
        BrowserStep::HandleDialog {
            accept: true,
            prompt_text: Some("answer".to_string()),
        },
        BrowserStep::DismissOverlays { aggressive: true },
        BrowserStep::HighlightElement { locator: locator() },
        BrowserStep::Highlight {
            locator: locator(),
            style: Some("outline: 2px solid red".to_string()),
            label: Some("Target".to_string()),
        },
        BrowserStep::HideHighlight,
        BrowserStep::Annotate {
            locator: locator(),
            text: "Review".to_string(),
        },
        BrowserStep::Help {
            topic: Some("routes".to_string()),
        },
    ]
}

#[test]
fn help_topic_is_optional_and_omitted_from_the_wire_when_unset() {
    let bare: BrowserStep = serde_json::from_value(serde_json::json!({"action": "help"})).unwrap();
    assert!(matches!(bare, BrowserStep::Help { topic: None }));
    assert_eq!(
        serde_json::to_value(&bare).unwrap(),
        serde_json::json!({"action": "help"})
    );

    let scoped: BrowserStep =
        serde_json::from_value(serde_json::json!({"action": "help", "topic": "locators"})).unwrap();
    assert!(matches!(
        scoped,
        BrowserStep::Help { topic: Some(ref topic) } if topic == "locators"
    ));
}

#[test]
fn add_virtual_authenticator_parses_without_an_id_and_never_serializes_one() {
    let step: BrowserStep = serde_json::from_str(
        r#"{"action": "add_virtual_authenticator", "protocol": "ctap2", "transport": "internal"}"#,
    )
    .unwrap();

    assert!(matches!(
        step,
        BrowserStep::AddVirtualAuthenticator {
            protocol: Some(BrowserAuthenticatorProtocol::Ctap2),
            transport: Some(BrowserAuthenticatorTransport::Internal),
            ..
        }
    ));
    assert!(
        serde_json::to_value(&step).unwrap().get("id").is_none(),
        "add_virtual_authenticator must not carry an id on the wire"
    );
}

#[test]
fn add_virtual_authenticator_rejects_a_client_supplied_id() {
    let error = serde_json::from_str::<BrowserStep>(
        r#"{"action": "add_virtual_authenticator", "id": "mine"}"#,
    )
    .unwrap_err()
    .to_string();

    assert!(error.contains("id is server-minted"), "{error}");
    assert!(error.contains("use the returned id"), "{error}");
}

#[test]
fn other_webauthn_steps_still_address_the_minted_id() {
    let step: BrowserStep =
        serde_json::from_str(r#"{"action": "remove_virtual_authenticator", "id": "minted-uuid"}"#)
            .unwrap();

    assert!(matches!(
        step,
        BrowserStep::RemoveVirtualAuthenticator { ref id } if id == "minted-uuid"
    ));
}

#[test]
fn route_web_socket_defaults_to_mock_with_both_directions_forwarding() {
    let step: BrowserStep = serde_json::from_value(
        serde_json::json!({"action": "route_web_socket", "pattern": "wss://example.com/**"}),
    )
    .unwrap();

    assert!(matches!(
        step,
        BrowserStep::RouteWebSocket {
            mode: WebSocketRouteMode::Mock,
            on_page_message: WebSocketMessageAction::Forward,
            on_server_message: WebSocketMessageAction::Forward,
            ..
        }
    ));
}

#[test]
fn route_web_socket_accepts_intercept_mode_with_per_direction_overrides() {
    let step: BrowserStep = serde_json::from_value(serde_json::json!({
        "action": "route_web_socket",
        "pattern": "wss://example.com/**",
        "mode": "intercept",
        "on_page_message": "drop",
        "on_server_message": "capture",
    }))
    .unwrap();

    assert!(matches!(
        step,
        BrowserStep::RouteWebSocket {
            mode: WebSocketRouteMode::Intercept,
            on_page_message: WebSocketMessageAction::Drop,
            on_server_message: WebSocketMessageAction::Capture,
            ..
        }
    ));
}

#[test]
fn close_web_socket_takes_an_optional_code_and_reason() {
    let bare: BrowserStep = serde_json::from_value(
        serde_json::json!({"action": "close_web_socket", "pattern": "wss://example.com/**"}),
    )
    .unwrap();
    assert!(matches!(
        bare,
        BrowserStep::CloseWebSocket {
            code: None,
            reason: None,
            ..
        }
    ));

    let detailed: BrowserStep = serde_json::from_value(serde_json::json!({
        "action": "close_web_socket",
        "pattern": "wss://example.com/**",
        "code": 1001,
        "reason": "going away",
    }))
    .unwrap();
    assert!(matches!(
        detailed,
        BrowserStep::CloseWebSocket {
            code: Some(1001),
            ref reason,
            ..
        } if reason.as_deref() == Some("going away")
    ));
}

#[test]
fn browser_step_action_names_cover_every_variant() {
    let serialized = all_browser_steps()
        .into_iter()
        .map(|step| {
            serde_json::to_value(step).unwrap()["action"]
                .as_str()
                .unwrap()
                .to_string()
        })
        .collect::<std::collections::BTreeSet<_>>();
    let declared = BrowserStep::ACTION_NAMES
        .iter()
        .map(|action| action.to_string())
        .collect::<std::collections::BTreeSet<_>>();

    assert_eq!(declared, serialized);
}

#[test]
fn clock_ticks_accept_milliseconds_and_human_strings() {
    let millis: BrowserStep =
        serde_json::from_value(serde_json::json!({"action": "clock_run_for", "ticks": 1000}))
            .unwrap();
    assert!(matches!(
        millis,
        BrowserStep::ClockRunFor {
            ticks: ClockTicks::Millis(1000)
        }
    ));
    let human: BrowserStep = serde_json::from_value(
        serde_json::json!({"action": "clock_fast_forward", "ticks": "01:00"}),
    )
    .unwrap();
    let BrowserStep::ClockFastForward { ticks } = &human else {
        panic!("expected clock_fast_forward, got {human:?}");
    };
    assert_eq!(*ticks, ClockTicks::Human("01:00".to_string()));
    assert_eq!(
        serde_json::to_value(&human).unwrap(),
        serde_json::json!({"action": "clock_fast_forward", "ticks": "01:00"})
    );
}

#[test]
fn clock_time_accepts_unix_millis_and_iso_strings() {
    let numeric: BrowserStep = serde_json::from_value(
        serde_json::json!({"action": "clock_pause_at", "time": 1_580_601_600_000i64}),
    )
    .unwrap();
    let BrowserStep::ClockPauseAt { time } = &numeric else {
        panic!("expected clock_pause_at, got {numeric:?}");
    };
    assert_eq!(*time, ClockTime::UnixMillis(1_580_601_600_000));
    let iso: BrowserStep = serde_json::from_value(
        serde_json::json!({"action": "clock_set_fixed_time", "time": "2020-02-02"}),
    )
    .unwrap();
    let BrowserStep::ClockSetFixedTime { time } = &iso else {
        panic!("expected clock_set_fixed_time, got {iso:?}");
    };
    assert_eq!(*time, ClockTime::Text("2020-02-02".to_string()));
}
