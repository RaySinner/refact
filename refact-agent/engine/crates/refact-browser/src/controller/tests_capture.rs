use super::*;
use super::tests_support::*;
use super::actions::{ClickIfExistsProbe, classify_click_if_exists_probe, click_if_exists_action_result, html_stabilizes_with, mask_console_entry, navigation_load_state_step, poll_deadline_error};
use super::capture::{omit_background_applies, prepare_screenshot_script, report_screenshot_requested, screenshot_policy, truncate_chars};
use super::context::{locator_outcome_for_resolve_error, locator_without_frames};
use super::eval_steps::{PAGE_CONTENT_JS, add_script_tag_js, add_style_tag_js, dispatch_event_js, eval_invocation_target, event_constructor, tag_source};
use super::execute::{is_navigation_step, is_non_fatal_step, replaces_document_in_place};
use super::expect_poll::{element_poll_js, is_truthy, page_poll_js, poll_backoff_schedule, poll_matcher_matches, redact_poll_value, strict_expectation_error};
use super::steps_runtime::{BrowserResetCounts, WindowBoundsRequest, apply_permission_state, burst_screenshot_options, cancel_download_step_result, cdp_send_result, download_step_result, filmstrip_step_data, http_request_result, is_context_management_step, is_instrumentation_step, is_screencast_step, network_conditions_summary, reset_sticky_registries, screencast_quality, validate_window_bounds_request, virtual_authenticator_added};

#[test]
fn eval_auto_invokes_function_results() {
    let object_id = "eval-function".to_string();
    assert_eq!(
        eval_invocation_target(&Runtime::RemoteObjectType::Function, Some(&object_id)),
        Some(object_id),
    );
}

#[test]
fn eval_leaves_non_function_results_untouched() {
    let object_id = "eval-object".to_string();
    assert_eq!(
        eval_invocation_target(&Runtime::RemoteObjectType::Number, Some(&object_id)),
        None,
    );
    assert_eq!(
        eval_invocation_target(&Runtime::RemoteObjectType::Object, Some(&object_id)),
        None,
    );
    assert_eq!(
        eval_invocation_target(&Runtime::RemoteObjectType::Function, None),
        None,
    );
}

#[test]
fn test_describe_locator_css() {
    let loc = BrowserLocator::css("#btn");
    assert_eq!(describe_locator(&loc), "css=#btn");
}

#[test]
fn test_describe_locator_ref() {
    let locator = BrowserLocator::reference("f2e7");
    assert_eq!(describe_locator(&locator), "ref=f2e7");
}

#[test]
fn aria_snapshot_truncation_preserves_complete_yaml_lines() {
    let value = "- button \"Save\"\n- textbox \"Search\"\n- link \"Guide\"".to_string();
    let truncated = truncate_chars(value, 40);
    assert!(truncated.ends_with("# ... (truncated)"));
    assert!(!truncated.contains("- textbox \"Sear"));
    assert!(truncated.chars().count() <= 40);
}

#[test]
fn single_element_expectations_are_strict_but_array_expectations_are_not() {
    let locator = BrowserLocator::css(".duplicate");
    let error = strict_expectation_error(
        &BrowserExpectation::ToBeVisible,
        &locator,
        2,
        &["<button class=\"duplicate\">One</button>".to_string()],
    )
    .unwrap();
    assert!(error.contains("css=.duplicate"));
    assert!(error.contains("<button class=\"duplicate\">"));
    assert!(strict_expectation_error(
        &BrowserExpectation::ToHaveCount { expected: 2 },
        &locator,
        2,
        &[],
    )
    .is_none());
    assert!(strict_expectation_error(
        &BrowserExpectation::ToHaveValues {
            expected: vec![BrowserExpectedText::Text("one".to_string())],
            ignore_case: false,
        },
        &locator,
        2,
        &[],
    )
    .is_none());
}

#[test]
fn test_describe_locator_id() {
    let loc = BrowserLocator::id("email");
    assert_eq!(describe_locator(&loc), "id=email");
}

#[test]
fn test_describe_locator_name() {
    let loc = BrowserLocator::name("q");
    assert_eq!(describe_locator(&loc), "name=q");
}

#[test]
fn test_describe_locator_label() {
    let loc = BrowserLocator::label("Email Address");
    assert_eq!(describe_locator(&loc), "label=Email Address");
}

#[test]
fn test_describe_locator_role_with_name() {
    let loc = BrowserLocator::role("textbox", Some("Search"));
    assert_eq!(describe_locator(&loc), "role=textbox[Search]");
}

#[test]
fn test_describe_locator_role_without_name() {
    let loc = BrowserLocator::role("button", None);
    assert_eq!(describe_locator(&loc), "role=button");
}

#[test]
fn test_describe_locator_text_exact() {
    let loc = BrowserLocator {
        strategy: LocatorStrategy::Text {
            value: "Submit".to_string(),
            exact: true,
            regex: None,
        },
        frames: Vec::new(),
        nth: None,
        within: None,
        locator: None,
        filter: None,
        and: None,
        or: None,
        first: None,
        last: None,
    };
    assert_eq!(describe_locator(&loc), "text=\"Submit\"");
}

#[test]
fn test_describe_locator_text_substring() {
    let loc = BrowserLocator {
        strategy: LocatorStrategy::Text {
            value: "Sub".to_string(),
            exact: false,
            regex: None,
        },
        frames: Vec::new(),
        nth: None,
        within: None,
        locator: None,
        filter: None,
        and: None,
        or: None,
        first: None,
        last: None,
    };
    assert_eq!(describe_locator(&loc), "text~\"Sub\"");
}

#[test]
fn test_describe_locator_placeholder() {
    let loc = BrowserLocator::placeholder("Search...");
    assert_eq!(describe_locator(&loc), "placeholder=Search...");
}

#[test]
fn test_describe_locator_testid() {
    let loc = BrowserLocator::test_id("submit-btn");
    assert_eq!(describe_locator(&loc), "testid=submit-btn");
}

#[test]
fn test_describe_locator_xpath() {
    let loc = BrowserLocator {
        strategy: LocatorStrategy::Xpath {
            value: "//button".to_string(),
        },
        frames: Vec::new(),
        nth: None,
        within: None,
        locator: None,
        filter: None,
        and: None,
        or: None,
        first: None,
        last: None,
    };
    assert_eq!(describe_locator(&loc), "xpath=//button");
}

#[test]
fn describe_locator_includes_outermost_first_frame_chain() {
    let locator = BrowserLocator::role("button", Some("Save")).in_frames(vec![
        BrowserLocator::css("#outer"),
        BrowserLocator::role("iframe", Some("Editor")),
    ]);

    assert_eq!(
        describe_locator(&locator),
        "frame(css=#outer).frame(role=iframe[Editor]).role=button[Save]"
    );
}

#[test]
fn injected_wait_payload_preserves_role_composition_without_frame_metadata() {
    let mut locator = BrowserLocator::role("button", Some("Save"))
        .in_frames(vec![BrowserLocator::css("#editor-frame")]);
    locator.locator = Some(Box::new(BrowserLocator::css("svg")));

    let payload = locator_without_frames(&locator).unwrap();

    assert_eq!(payload["by"], "role");
    assert_eq!(payload["name"], "Save");
    assert_eq!(
        payload["locator"],
        serde_json::json!({"by": "css", "value": "svg"})
    );
    assert!(payload.get("frames").is_none());
}

#[test]
fn http_request_result_inlines_small_bodies_and_reports_cookies_without_their_values() {
    let dir = tempfile::tempdir().unwrap();
    let response = http_response(200, br#"{"session":"live"}"#.to_vec(), "application/json");
    let options = BrowserHttpRequest {
        url: "https://api.example.test/v1/session".to_string(),
        ..Default::default()
    };

    let result = http_request_result(&response, &options, 3, dir.path());

    assert!(result.ok);
    assert_eq!(
        result.summary,
        "GET https://api.example.test/v1/session -> 200 OK (18 bytes, 1 cookies set)"
    );
    let data = &result.data.as_ref().unwrap()["http_request"];
    assert_eq!(data["status"], 200);
    assert_eq!(data["redirects"], 1);
    assert_eq!(data["body"], "{\n  \"session\": \"live\"\n}");
    assert_eq!(data["body_bytes"], 18);
    assert_eq!(data["set_cookies"]["count"], 1);
    assert_eq!(data["set_cookies"]["names"][0], "sid");
    assert!(data.get("artifact").is_none());

    let payload = serde_json::to_string(data).unwrap();
    assert!(!payload.contains("supersecret"), "{payload}");
    assert!(!payload.contains("server"), "{payload}");
    assert_eq!(data["headers"].as_object().unwrap().len(), 1);
}

#[test]
fn http_request_result_spills_oversized_bodies_and_keeps_full_headers_redacted() {
    let dir = tempfile::tempdir().unwrap();
    let body = vec![b'x'; http_client::HTTP_INLINE_BODY_LIMIT_BYTES + 1];
    let response = http_response(200, body.clone(), "text/plain");
    let options = BrowserHttpRequest {
        url: "https://api.example.test/v1/session".to_string(),
        full_headers: Some(true),
        ..Default::default()
    };

    let result = http_request_result(&response, &options, 0, dir.path());

    assert!(result.ok);
    let data = &result.data.as_ref().unwrap()["http_request"];
    assert!(data.get("body").is_none());
    assert_eq!(data["artifact"]["bytes"], body.len());
    let path = PathBuf::from(data["artifact"]["path"].as_str().unwrap());
    assert!(path.starts_with(dir.path()));
    assert_eq!(std::fs::read(&path).unwrap(), body);
    assert_eq!(data["headers"]["set-cookie"], "[REDACTED]");
    assert_eq!(data["headers"]["server"], "fixture");
}

#[test]
fn http_request_result_only_fails_on_non_2xx_when_fail_on_status_is_set() {
    let dir = tempfile::tempdir().unwrap();
    let response = http_response(404, b"missing".to_vec(), "text/plain");
    let lenient = BrowserHttpRequest {
        url: "https://api.example.test/v1/session".to_string(),
        ..Default::default()
    };
    let strict = BrowserHttpRequest {
        fail_on_status: Some(true),
        ..lenient.clone()
    };

    assert!(http_request_result(&response, &lenient, 0, dir.path()).ok);

    let failed = http_request_result(&response, &strict, 0, dir.path());
    assert!(!failed.ok);
    assert_eq!(failed.error.as_deref(), Some("HTTP 404 OK"));
    assert_eq!(
        failed.data.as_ref().unwrap()["http_request"]["body"],
        "missing"
    );

    let created = http_response(201, Vec::new(), "text/plain");
    let accepted = http_request_result(&created, &strict, 0, dir.path());
    assert!(accepted.ok);
    assert!(accepted.data.as_ref().unwrap()["http_request"]
        .get("body")
        .is_none());
}

#[test]
fn console_report_text_is_redacted() {
    let entry = mask_console_entry(refact_integrations::browser_types::ConsoleEntry {
        timestamp: 1.0,
        level: "error".to_string(),
        text: "password=hunter2".to_string(),
    });

    assert_eq!(entry.text, "password=[REDACTED]");
}

#[test]
fn html_stability_gate_returns_false_after_max_checks() {
    let mut value = 0;
    let stabilized = html_stabilizes_with(
        3,
        || {
            value += 1;
            Ok(serde_json::json!(value))
        },
        || {},
    );

    assert!(!stabilized);
}

#[test]
fn html_stability_gate_accepts_two_equal_samples() {
    let stabilized = html_stabilizes_with(2, || Ok(serde_json::json!("stable")), || {});

    assert!(stabilized);
}

#[test]
fn cdp_send_inlines_small_results_and_reports_warnings_in_the_summary() {
    let dir = tempfile::tempdir().unwrap();
    let step = cdp_send_result(
        "Emulation.setDeviceMetricsOverride",
        CdpTarget::Page,
        vec!["Emulation.setDeviceMetricsOverride is invisible to reset".to_string()],
        serde_json::json!({"ok": true}),
        3,
        dir.path(),
    );

    assert!(step.ok);
    assert!(
        step.summary
            .starts_with("Emulation.setDeviceMetricsOverride on page returned ")
            && step.summary.ends_with(" (1 warning)"),
        "unexpected summary: {}",
        step.summary
    );
    let data = step.data.as_ref().unwrap();
    assert_eq!(data["cdp_send"]["result"], serde_json::json!({"ok": true}));
    assert_eq!(data["cdp_send"]["target"], "page");
    assert_eq!(data["cdp_send"]["warnings"].as_array().unwrap().len(), 1);
    assert!(data["cdp_send"]["artifact"].is_null());
}

#[test]
fn cdp_send_spills_oversized_results_to_an_artifact_and_redacts_cookies() {
    let dir = tempfile::tempdir().unwrap();
    let cookies = (0..400)
        .map(|index| serde_json::json!({"name": format!("c{index}"), "value": "secret-value"}))
        .collect::<Vec<_>>();
    let step = cdp_send_result(
        "Network.getCookies",
        CdpTarget::Browser,
        Vec::new(),
        serde_json::json!({"cookies": cookies}),
        0,
        dir.path(),
    );

    assert!(step.ok, "{step:?}");
    assert!(!step.summary.contains("warning"));
    let artifact = &step.data.as_ref().unwrap()["cdp_send"]["artifact"];
    assert_eq!(artifact["mime"], "application/json");
    assert!(
        artifact["bytes"].as_u64().unwrap() > CDP_INLINE_RESULT_LIMIT_BYTES as u64,
        "artifact must only be used past the inline limit"
    );
    assert!(step.data.as_ref().unwrap()["cdp_send"]["result"].is_null());

    let saved = std::fs::read_to_string(artifact["path"].as_str().unwrap()).unwrap();
    assert!(
        !saved.contains("secret-value"),
        "cookie values must be redacted before they reach an artifact"
    );
    assert!(saved.contains("[REDACTED]"));
}

#[test]
fn reset_clears_every_sticky_registry_and_reports_truthful_counts() {
    let route_registry = populated_route_registry();
    let mut routes = route_registry.list();
    routes.push(har_replay_route());
    let websocket_registry = WebSocketRegistry::default();
    websocket_registry
        .add_route(
            UrlPattern::Text("wss://example.com/**".to_string()),
            WebSocketRouteMode::Mock,
            WebSocketMessageAction::Forward,
            WebSocketMessageAction::Forward,
        )
        .unwrap();
    let locator_handlers = populated_locator_handlers();

    let counts = reset_sticky_registries(
        &routes,
        &websocket_registry,
        &locator_handlers,
        1,
        2,
        true,
        true,
    )
    .unwrap();
    route_registry.remove(None);

    assert_eq!(
        counts,
        BrowserResetCounts {
            routes: 2,
            har_replays: 1,
            websocket_routes: 1,
            locator_handlers: 3,
            authenticators: 1,
            init_scripts: 2,
            clock: true,
            service_worker_block: true,
        }
    );
    assert_eq!(
        counts.summary(),
        "Reset: 2 routes, 1 har replay, 1 ws route, 3 locator handlers, 1 authenticator, 2 init scripts, offline off, throttling off, emulation and device cleared, clock cleared, service worker block cleared"
    );
    assert!(route_registry.is_empty());
    assert_eq!(websocket_registry.route_count(), 0);
    assert_eq!(
        locator_handlers
            .lock()
            .unwrap()
            .handlers()
            .iter()
            .map(|handler| handler.name.clone())
            .collect::<Vec<_>>(),
        vec![DEFAULT_DISMISS_OVERLAYS_HANDLER.to_string()]
    );
}

#[test]
fn network_conditions_summary_reports_offline_and_throttling_independently() {
    assert_eq!(
        network_conditions_summary(false, None),
        "Network conditions: online, no throttling"
    );
    assert_eq!(
        network_conditions_summary(true, None),
        "Network conditions: offline, no throttling"
    );

    let slow_3g = refact_browser::NetworkConditions::preset("slow-3g").unwrap();
    assert_eq!(
        network_conditions_summary(false, Some(&slow_3g)),
        "Network conditions: online, 2000ms latency, 400kbps down, 400kbps up"
    );
    assert_eq!(
        network_conditions_summary(true, Some(&slow_3g)),
        "Network conditions: offline, 2000ms latency, 400kbps down, 400kbps up"
    );
}

#[test]
fn reset_on_clean_state_succeeds_with_zero_counts() {
    let websocket_registry = WebSocketRegistry::default();
    let locator_handlers = populated_locator_handlers();

    reset_sticky_registries(
        &[],
        &websocket_registry,
        &locator_handlers,
        0,
        0,
        false,
        false,
    )
    .unwrap();
    let repeated = reset_sticky_registries(
        &[],
        &websocket_registry,
        &locator_handlers,
        0,
        0,
        false,
        false,
    )
    .unwrap();

    assert_eq!(repeated, BrowserResetCounts::default());
    assert_eq!(
        repeated.summary(),
        "Reset: 0 routes, 0 har replays, 0 ws routes, 0 locator handlers, 0 authenticators, 0 init scripts, offline off, throttling off, emulation and device cleared, clock off, service worker block off"
    );
    assert_eq!(websocket_registry.route_count(), 0);
    assert_eq!(locator_handlers.lock().unwrap().handlers().len(), 1);
}

#[test]
fn omit_background_is_skipped_for_jpeg_because_jpeg_has_no_alpha() {
    let transparent = BrowserScreenshotOptions {
        omit_background: true,
        ..Default::default()
    };
    assert!(omit_background_applies(&transparent));
    assert!(omit_background_applies(&BrowserScreenshotOptions {
        image_type: Some(BrowserScreenshotType::Webp),
        ..transparent.clone()
    }));
    assert!(!omit_background_applies(&BrowserScreenshotOptions {
        image_type: Some(BrowserScreenshotType::Jpeg),
        ..transparent.clone()
    }));
    assert!(!omit_background_applies(
        &BrowserScreenshotOptions::default()
    ));
}

#[test]
fn screenshot_policy_prefers_step_format_and_quality_over_the_image_policy() {
    let policy = ImagePolicy {
        format: ImageFormat::Webp,
        quality: Some(80),
        ..ImagePolicy::default()
    };
    let inherited = screenshot_policy(&BrowserScreenshotOptions::default(), &policy);
    assert_eq!(inherited.format, ImageFormat::Png);
    assert_eq!(inherited.quality, Some(80));

    let overridden = screenshot_policy(
        &BrowserScreenshotOptions {
            image_type: Some(BrowserScreenshotType::Jpeg),
            quality: Some(35),
            ..Default::default()
        },
        &policy,
    );
    assert_eq!(overridden.format, ImageFormat::Jpeg);
    assert_eq!(overridden.quality, Some(35));
    assert_eq!(overridden.max_side, policy.max_side);
}

#[test]
fn screenshot_preparation_pierces_shadow_roots_and_restores_every_style() {
    let script =
        prepare_screenshot_script(&[], "body{background:red}", true, true, "#FF00FF").unwrap();

    assert!(script.contains("createTreeWalker"));
    assert!(script.contains("node.shadowRoot"));
    assert!(script.contains("(scope === document ? root : scope).appendChild(style)"));
    assert!(script.contains("roots.flatMap(scope => scope.getAnimations())"));
    assert!(script.contains("for (const style of styles) style.remove()"));
}

#[test]
fn screenshot_preparation_skips_the_dom_walk_when_only_masks_are_requested() {
    let masked = prepare_screenshot_script(
        &[serde_json::json!({"x": 1.0, "y": 2.0, "width": 3.0, "height": 4.0})],
        "",
        false,
        false,
        "#FF00FF",
    )
    .unwrap();

    assert!(masked.contains("const roots = css || false ? collectRoots(document, []) : []"));
    assert!(masked.contains("\"x\":1.0"));
}

#[test]
fn dispatch_event_infers_the_playwright_event_class() {
    for (event_type, expected) in [
        ("click", "MouseEvent"),
        ("dblclick", "MouseEvent"),
        ("mouseenter", "MouseEvent"),
        ("keydown", "KeyboardEvent"),
        ("textInput", "KeyboardEvent"),
        ("touchstart", "TouchEvent"),
        ("pointerdown", "PointerEvent"),
        ("gotpointercapture", "PointerEvent"),
        ("focus", "FocusEvent"),
        ("blur", "FocusEvent"),
        ("dragstart", "DragEvent"),
        ("drop", "DragEvent"),
        ("wheel", "WheelEvent"),
        ("deviceorientation", "DeviceOrientationEvent"),
        ("devicemotion", "DeviceMotionEvent"),
        ("input", "Event"),
        ("change", "Event"),
        ("app:custom", "Event"),
    ] {
        assert_eq!(
            event_constructor(event_type, None),
            expected,
            "unexpected class for {event_type}"
        );
    }
}

#[test]
fn dispatch_event_upgrades_to_custom_event_only_for_unmapped_types_carrying_detail() {
    let detail = serde_json::json!({"detail": {"id": 7}});
    assert_eq!(
        event_constructor("app:custom", Some(&detail)),
        "CustomEvent"
    );
    assert_eq!(event_constructor("change", Some(&detail)), "CustomEvent");
    assert_eq!(event_constructor("click", Some(&detail)), "MouseEvent");

    let without_detail = serde_json::json!({"bubbles": false});
    assert_eq!(
        event_constructor("app:custom", Some(&without_detail)),
        "Event"
    );
}

#[test]
fn dispatch_event_js_defaults_bubbles_cancelable_and_composed_but_lets_the_caller_win() {
    let js = dispatch_event_js("click", None);
    assert!(
        js.contains("Object.assign({bubbles: true, cancelable: true, composed: true}, {})"),
        "{js}"
    );
    assert!(js.contains("new MouseEvent('click', init)"), "{js}");

    let overridden = dispatch_event_js("click", Some(&serde_json::json!({"bubbles": false})));
    assert!(
        overridden.contains(
            "Object.assign({bubbles: true, cancelable: true, composed: true}, {\"bubbles\":false})"
        ),
        "{overridden}"
    );
}

#[test]
fn tag_steps_require_exactly_one_source() {
    assert!(tag_source(Some("https://x.test/a.js"), None, "add_script_tag").is_ok());
    assert!(tag_source(None, Some("window.x = 1"), "add_script_tag").is_ok());

    let both =
        tag_source(Some("https://x.test/a.js"), Some("x"), "add_script_tag").unwrap_err();
    assert!(both.contains("not both"), "{both}");

    let neither = tag_source(None, None, "add_style_tag").unwrap_err();
    assert!(
        neither.contains("requires either url or content"),
        "{neither}"
    );
}

#[test]
fn tag_js_awaits_remote_sources_and_appends_inline_ones_immediately() {
    let remote = add_script_tag_js(Some("https://x.test/a.js"), None, Some("module"));
    assert!(remote.contains("element.type = 'module'"), "{remote}");
    assert!(
        remote.contains("element.src = 'https://x.test/a.js'"),
        "{remote}"
    );
    assert!(remote.contains("addEventListener('load'"), "{remote}");
    assert!(remote.contains("addEventListener('error'"), "{remote}");

    let inline = add_script_tag_js(None, Some("window.x = 1"), None);
    assert!(inline.contains("element.text = 'window.x = 1'"), "{inline}");
    assert!(!inline.contains("addEventListener"), "{inline}");

    let remote_style = add_style_tag_js(Some("https://x.test/a.css"), None);
    assert!(
        remote_style.contains("element.rel = 'stylesheet'"),
        "{remote_style}"
    );
    assert!(
        remote_style.contains("addEventListener('error'"),
        "{remote_style}"
    );

    let inline_style = add_style_tag_js(None, Some("body { color: red }"));
    assert!(
        inline_style.contains("createElement('style')"),
        "{inline_style}"
    );
    assert!(
        inline_style.contains("element.textContent = 'body { color: red }'"),
        "{inline_style}"
    );
}

#[test]
fn set_content_rebootstraps_refs_and_counts_as_a_page_change() {
    let step = BrowserStep::SetContent {
        html: "<p>hi</p>".to_string(),
        wait_until: None,
    };
    assert!(is_navigation_step(&step));
    assert!(replaces_document_in_place(&step));

    assert!(!replaces_document_in_place(&BrowserStep::PageContent));
    assert!(!replaces_document_in_place(&BrowserStep::Reload));
}

#[test]
fn set_content_forces_a_report_screenshot_even_though_the_url_never_changes() {
    assert!(report_screenshot_requested(
        None,
        PageContextMode::Screenshot,
        false
    ));
    assert!(!report_screenshot_requested(
        None,
        PageContextMode::Snapshot,
        false
    ));
    assert!(!report_screenshot_requested(
        Some(false),
        PageContextMode::Screenshot,
        false
    ));
}

#[test]
fn page_content_serializes_the_doctype_before_the_document_element() {
    assert!(PAGE_CONTENT_JS.contains("document.doctype"));
    assert!(PAGE_CONTENT_JS.contains("XMLSerializer"));
    assert!(PAGE_CONTENT_JS.contains("documentElement.outerHTML"));
    assert!(
        PAGE_CONTENT_JS.find("doctype").unwrap()
            < PAGE_CONTENT_JS.find("documentElement.outerHTML").unwrap()
    );
}

#[test]
fn reset_reports_the_cleared_subsystems_as_structured_data() {
    let counts = BrowserResetCounts {
        routes: 2,
        har_replays: 1,
        websocket_routes: 1,
        locator_handlers: 3,
        authenticators: 1,
        init_scripts: 2,
        clock: true,
        service_worker_block: true,
    };

    assert_eq!(
        counts.data(),
        serde_json::json!({
            "reset": {
                "routes": 2,
                "har_replays": 1,
                "websocket_routes": 1,
                "locator_handlers": 3,
                "authenticators": 1,
                "init_scripts": 2,
                "offline": false,
                "throttling_cleared": true,
                "emulation_cleared": true,
                "clock_cleared": true,
                "service_worker_block_cleared": true,
            }
        })
    );
}

#[test]
fn permission_state_tracks_only_the_permissions_that_stay_granted() {
    let granted = apply_permission_state(
        &[],
        &["geolocation".to_string(), "notifications".to_string()],
        BrowserPermissionState::Granted,
    );
    assert_eq!(granted, vec!["geolocation", "notifications"]);

    let denied = apply_permission_state(
        &granted,
        &["notifications".to_string()],
        BrowserPermissionState::Denied,
    );
    assert_eq!(denied, vec!["geolocation"]);

    let prompted = apply_permission_state(
        &denied,
        &["geolocation".to_string()],
        BrowserPermissionState::Prompt,
    );
    assert!(prompted.is_empty());

    let regranted = apply_permission_state(
        &["geolocation".to_string()],
        &["geolocation".to_string()],
        BrowserPermissionState::Granted,
    );
    assert_eq!(regranted, vec!["geolocation"]);
}

#[test]
fn wait_for_download_surfaces_the_failure_reason_and_keeps_the_download_data() {
    let failed =
        download_step_result(3, Ok(download(DownloadState::Canceled, Some("canceled"))));
    assert!(!failed.ok);
    assert_eq!(
        failed.error.as_deref(),
        Some("Download failed (canceled): report.csv")
    );
    assert_eq!(failed.data.as_ref().unwrap()["failure_reason"], "canceled");
    assert_eq!(failed.data.as_ref().unwrap()["state"], "canceled");

    let completed = download_step_result(3, Ok(download(DownloadState::Completed, None)));
    assert!(completed.ok);
    assert_eq!(completed.summary, "Downloaded report.csv");
    assert!(completed.data.as_ref().unwrap()["failure_reason"].is_null());

    let timed_out = download_step_result(3, Err("Timed out waiting for download".to_string()));
    assert!(!timed_out.ok);
    assert_eq!(
        timed_out.error.as_deref(),
        Some("Timed out waiting for download")
    );
    assert!(timed_out.data.is_none());
}

#[test]
fn cancel_download_reports_the_canceled_download_or_the_cancel_error() {
    let canceled =
        cancel_download_step_result(1, Ok(download(DownloadState::Canceled, Some("canceled"))));
    assert!(canceled.ok);
    assert_eq!(canceled.summary, "Canceled download report.csv");
    assert_eq!(
        canceled.data.as_ref().unwrap()["failure_reason"],
        "canceled"
    );

    let missing = cancel_download_step_result(1, Err("No download is in progress".to_string()));
    assert!(!missing.ok);
    assert_eq!(missing.summary, "Cancel download");
    assert_eq!(missing.error.as_deref(), Some("No download is in progress"));
}

#[test]
fn screencast_quality_defaults_and_rejects_out_of_range_values() {
    assert_eq!(
        screencast_quality(None).unwrap(),
        DEFAULT_SCREENCAST_QUALITY
    );
    assert_eq!(screencast_quality(Some(0)).unwrap(), 0);
    assert_eq!(screencast_quality(Some(100)).unwrap(), 100);
    assert_eq!(
        screencast_quality(Some(101)).unwrap_err(),
        "Screencast quality 101 must be between 0 and 100"
    );
}

#[test]
fn burst_screenshots_are_jpeg_and_carry_the_full_page_request() {
    let policy = ImagePolicy::browser_capture();

    let viewport = burst_screenshot_options(&policy, false);
    assert_eq!(viewport.image_type, Some(BrowserScreenshotType::Jpeg));
    assert_eq!(viewport.quality, policy.quality);
    assert!(!viewport.full_page);
    assert!(burst_screenshot_options(&policy, true).full_page);
}

#[test]
fn filmstrip_step_data_exposes_the_composite_as_the_step_image() {
    let data = filmstrip_step_data(&filmstrip_result());

    assert_eq!(data["mime"], serde_json::json!("image/jpeg"));
    assert_eq!(data["data"], serde_json::json!("ZmlsbXN0cmlw"));
    assert_eq!(data["artifact"]["kind"], serde_json::json!("filmstrip"));
    assert_eq!(data["frame_count"], serde_json::json!(2));
    assert_eq!(data["columns"], serde_json::json!(2));
    assert_eq!(data["duration_ms"], serde_json::json!(500));
    assert_eq!(data["frames"][0]["offset_ms"], serde_json::json!(0));
    assert!(data["frames"][0].get("changed_percent").is_none());
    assert_eq!(
        data["frames"][1]["changed_percent"],
        serde_json::json!(42.5)
    );
    assert_eq!(
        data["frames"][1]["artifact"]["kind"],
        serde_json::json!("frame")
    );
    assert_eq!(
        data["warnings"],
        serde_json::json!(["element-scoped frames use timed screenshots"])
    );
}

#[test]
fn screencast_steps_are_routed_to_the_runtime_dispatch() {
    for step in [
        BrowserStep::CaptureFrames {
            duration_ms: None,
            frame_count: None,
            interval_ms: None,
            locator: None,
            full_page: None,
        },
        BrowserStep::ScreencastStart {
            quality: None,
            max_width: None,
            max_height: None,
        },
        BrowserStep::ScreencastStop { compose: None },
    ] {
        assert!(is_screencast_step(&step));
        assert!(!is_instrumentation_step(&step));
        assert!(!is_context_management_step(&step));
        assert!(!is_tab_management_step(&step));
    }
    assert!(!is_screencast_step(&BrowserStep::StopCoverage));
}

#[test]
fn predicate_truthiness_follows_javascript_rules() {
    for truthy in [
        serde_json::json!(true),
        serde_json::json!(1),
        serde_json::json!(-0.5),
        serde_json::json!("ready"),
        serde_json::json!([]),
        serde_json::json!({}),
    ] {
        assert!(is_truthy(&truthy), "expected truthy: {truthy}");
    }
    for falsy in [
        serde_json::json!(null),
        serde_json::json!(false),
        serde_json::json!(0),
        serde_json::json!(0.0),
        serde_json::json!(""),
    ] {
        assert!(!is_truthy(&falsy), "expected falsy: {falsy}");
    }
}

#[test]
fn equals_compares_numbers_across_integer_and_float_encodings() {
    assert!(poll_matcher_matches(
        BrowserPollMatcher::Equals,
        &serde_json::json!(3),
        &serde_json::json!(3.0)
    )
    .unwrap());
    assert!(poll_matcher_matches(
        BrowserPollMatcher::Equals,
        &serde_json::json!({"a": [1, "x"]}),
        &serde_json::json!({"a": [1, "x"]})
    )
    .unwrap());
    assert!(!poll_matcher_matches(
        BrowserPollMatcher::Equals,
        &serde_json::json!(true),
        &serde_json::json!(1)
    )
    .unwrap());
}

#[test]
fn contains_matches_substrings_and_array_membership() {
    assert!(poll_matcher_matches(
        BrowserPollMatcher::Contains,
        &serde_json::json!("loading done"),
        &serde_json::json!("done")
    )
    .unwrap());
    assert!(poll_matcher_matches(
        BrowserPollMatcher::Contains,
        &serde_json::json!(["a", "b"]),
        &serde_json::json!("b")
    )
    .unwrap());
    assert!(!poll_matcher_matches(
        BrowserPollMatcher::Contains,
        &serde_json::json!(7),
        &serde_json::json!("7")
    )
    .unwrap());
}

#[test]
fn numeric_comparisons_ignore_non_numeric_values_instead_of_failing() {
    assert!(poll_matcher_matches(
        BrowserPollMatcher::Gt,
        &serde_json::json!(5),
        &serde_json::json!(4.5)
    )
    .unwrap());
    assert!(!poll_matcher_matches(
        BrowserPollMatcher::Gt,
        &serde_json::json!(4),
        &serde_json::json!(4)
    )
    .unwrap());
    assert!(poll_matcher_matches(
        BrowserPollMatcher::Lt,
        &serde_json::json!(1),
        &serde_json::json!(2)
    )
    .unwrap());
    assert!(!poll_matcher_matches(
        BrowserPollMatcher::Lt,
        &serde_json::json!("1"),
        &serde_json::json!(2)
    )
    .unwrap());
}

#[test]
fn matches_regex_accepts_a_bare_source_or_flagged_object_and_rejects_bad_patterns() {
    assert!(poll_matcher_matches(
        BrowserPollMatcher::MatchesRegex,
        &serde_json::json!("order-42-done"),
        &serde_json::json!("^order-\\d+-done$")
    )
    .unwrap());
    assert!(poll_matcher_matches(
        BrowserPollMatcher::MatchesRegex,
        &serde_json::json!("READY"),
        &serde_json::json!({"source": "ready", "flags": "i"})
    )
    .unwrap());
    assert!(!poll_matcher_matches(
        BrowserPollMatcher::MatchesRegex,
        &serde_json::json!(7),
        &serde_json::json!("7")
    )
    .unwrap());
    assert!(poll_matcher_matches(
        BrowserPollMatcher::MatchesRegex,
        &serde_json::json!("x"),
        &serde_json::json!("([")
    )
    .is_err());
    assert!(poll_matcher_matches(
        BrowserPollMatcher::MatchesRegex,
        &serde_json::json!("x"),
        &serde_json::json!(7)
    )
    .is_err());
}

#[test]
fn a_fixed_poll_interval_replaces_the_default_ladder() {
    assert_eq!(poll_backoff_schedule(None), FUNCTION_POLL_BACKOFF_MS);
    assert_eq!(poll_backoff_schedule(Some(40)), vec![0, 40]);
}

#[test]
fn poll_expressions_auto_invoke_functions_and_bind_the_element_argument() {
    let page = page_poll_js("() => window.ready");
    assert!(page.contains("() => window.ready"));
    assert!(page.contains("=== 'function'"));
    assert!(page.ends_with("})()"));
    assert!(!page.contains("(this)"));

    let element = element_poll_js("el => el.dataset.state === 'ready'");
    assert!(element.starts_with("function() {"));
    assert!(element.contains("(this)"));
}

#[test]
fn reported_poll_values_are_redacted_without_reshaping_them() {
    let redacted = redact_poll_value(serde_json::json!({
        "headers": ["authorization: Bearer abcdef123456"],
        "token": "sk-abcdef123456789",
        "count": 3,
        "ready": true,
    }));

    let serialized = serde_json::to_string(&redacted).unwrap();
    assert!(!serialized.contains("abcdef123456"), "{serialized}");
    assert!(!serialized.contains("sk-abcdef"), "{serialized}");
    assert!(serialized.contains("[REDACTED]"), "{serialized}");
    assert_eq!(redacted["count"], serde_json::json!(3));
    assert_eq!(redacted["ready"], serde_json::json!(true));
    assert_eq!(redacted["headers"].as_array().unwrap().len(), 1);
}

#[test]
fn only_soft_poll_assertions_join_the_non_fatal_step_set() {
    let poll = |soft| BrowserStep::ExpectPoll {
        expression: "window.__count".to_string(),
        expected: serde_json::json!(1),
        matcher: BrowserPollMatcher::Equals,
        timeout_ms: None,
        soft,
    };

    assert!(is_non_fatal_step(&poll(Some(true))));
    assert!(!is_non_fatal_step(&poll(Some(false))));
    assert!(!is_non_fatal_step(&poll(None)));
    assert!(!is_non_fatal_step(&BrowserStep::WaitForFunction {
        expression: "() => true".to_string(),
        locator: None,
        timeout_ms: None,
        polling_ms: None,
    }));
}

#[test]
fn added_virtual_authenticator_result_reports_the_minted_id() {
    let result = virtual_authenticator_added(4, "minted-uuid".to_string());

    assert!(result.ok);
    assert_eq!(result.step_index, 4);
    assert_eq!(result.summary, "Added virtual authenticator minted-uuid");
    assert_eq!(
        result.data.unwrap()["authenticator_id"],
        serde_json::json!("minted-uuid")
    );
}

#[test]
fn window_bounds_accept_any_single_dimension_and_keep_the_rest_unchanged() {
    assert_eq!(
        validate_window_bounds_request(None, None, Some(1024), None).unwrap(),
        WindowBoundsRequest {
            left: None,
            top: None,
            width: Some(1024),
            height: None,
        }
    );
    assert_eq!(
        validate_window_bounds_request(Some(12), Some(34), Some(800), Some(600)).unwrap(),
        WindowBoundsRequest {
            left: Some(12),
            top: Some(34),
            width: Some(800),
            height: Some(600),
        }
    );
}

#[test]
fn window_bounds_reject_an_empty_request() {
    let error = validate_window_bounds_request(None, None, None, None).unwrap_err();

    assert!(
        error.contains("at least one of x, y, width, height"),
        "unexpected error: {error}"
    );
}

#[test]
fn window_bounds_reject_zero_sizes_and_negative_positions() {
    for (x, y, width, height, needle) in [
        (None, None, Some(0), None, "width must be greater than zero"),
        (
            None,
            None,
            None,
            Some(0),
            "height must be greater than zero",
        ),
        (Some(-1), None, None, None, "x=-1 is negative"),
        (None, Some(-5), None, None, "y=-5 is negative"),
    ] {
        let error = validate_window_bounds_request(x, y, width, height).unwrap_err();
        assert!(error.contains(needle), "unexpected error: {error}");
    }
}

#[test]
fn set_window_bounds_is_a_runtime_step_batchable_with_viewport_emulation() {
    let request = parse_browser_action_request(serde_json::json!({
        "steps": [
            {"action": "set_window_bounds", "width": 1280, "height": 800},
            {"action": "set_viewport", "width": 390, "height": 844},
        ]
    }))
    .unwrap();

    assert!(matches!(
        request.steps[0],
        BrowserStep::SetWindowBounds {
            x: None,
            y: None,
            width: Some(1280),
            height: Some(800)
        }
    ));
    assert!(
        !is_context_management_step(&request.steps[0]),
        "set_window_bounds must not be routed as page-emulation context state"
    );
    assert!(is_context_management_step(&request.steps[1]));
}

#[test]
fn poll_deadline_reports_the_last_eval_error_instead_of_a_bare_timeout() {
    assert_eq!(
        poll_deadline_error(
            Some("JS evaluation failed: boom".to_string()),
            "Timed out after 5000ms".to_string()
        ),
        "Polling failed: JS evaluation failed: boom"
    );
    assert_eq!(
        poll_deadline_error(None, "Timed out after 5000ms".to_string()),
        "Timed out after 5000ms"
    );
}

#[test]
fn click_if_exists_skips_only_genuine_non_actionable_probes() {
    assert_eq!(
        classify_click_if_exists_probe(Ok(())),
        ClickIfExistsProbe::Proceed
    );
    assert_eq!(
        classify_click_if_exists_probe(Err("Element not found".to_string())),
        ClickIfExistsProbe::Skip("Element not found".to_string())
    );
    assert_eq!(
        classify_click_if_exists_probe(Err(
            "not actionable, element is not visible".to_string()
        )),
        ClickIfExistsProbe::Skip("not actionable, element is not visible".to_string())
    );
    assert_eq!(
        classify_click_if_exists_probe(Err(DEAD_TRANSPORT.to_string())),
        ClickIfExistsProbe::Fatal(DEAD_TRANSPORT.to_string())
    );
}

#[test]
fn click_if_exists_keeps_transport_dead_action_failures_fatal() {
    let succeeded = click_if_exists_action_result(StepResult::success(3, "click on <button>"));
    assert!(succeeded.ok);
    assert_eq!(succeeded.summary, "click on <button>");

    let non_fatal = click_if_exists_action_result(StepResult::failure(
        3,
        "click failed",
        "Element is not visible".to_string(),
    ));
    assert!(non_fatal.ok);
    assert_eq!(
        non_fatal.summary,
        "Click skipped (non-fatal): Element is not visible"
    );
    assert!(non_fatal.error.is_none());
    assert_eq!(non_fatal.data.as_ref().unwrap()["clicked"], false);
    assert_eq!(
        non_fatal.data.as_ref().unwrap()["skip_reason"],
        "Element is not visible"
    );

    let fatal = click_if_exists_action_result(StepResult::failure(
        3,
        "click failed",
        DEAD_TRANSPORT.to_string(),
    ));
    assert!(!fatal.ok);
    assert_eq!(fatal.error.as_deref(), Some(DEAD_TRANSPORT));
}

#[test]
fn transport_dead_resolve_errors_surface_for_every_locator_strategy() {
    assert_eq!(
        locator_outcome_for_resolve_error(&css_strategy(), "Element not found".to_string()),
        LocatorOutcome::NotFound
    );
    assert_eq!(
        locator_outcome_for_resolve_error(&css_strategy(), DEAD_TRANSPORT.to_string()),
        LocatorOutcome::Error {
            description: DEAD_TRANSPORT.to_string()
        }
    );
    assert_eq!(
        locator_outcome_for_resolve_error(
            &LocatorStrategy::Ref {
                value: "e1".to_string()
            },
            "Invalid browser ref e1".to_string()
        ),
        LocatorOutcome::Error {
            description: "Invalid browser ref e1".to_string()
        }
    );
}

#[test]
fn navigation_load_state_failures_are_reported_on_the_step() {
    let reached = navigation_load_state_step(1, "Navigation detected".to_string(), Ok(()));
    assert!(reached.ok);
    assert_eq!(reached.summary, "Navigation detected");
    assert!(reached.data.is_none());

    let not_reached = navigation_load_state_step(
        1,
        "Navigation detected".to_string(),
        Err("Timed out after 5000ms".to_string()),
    );
    assert!(not_reached.ok);
    assert!(
        not_reached.summary.contains("load state not reached"),
        "unexpected summary: {}",
        not_reached.summary
    );
    assert_eq!(
        not_reached.data,
        Some(serde_json::json!({"load_state_reached": false}))
    );

    let dead = navigation_load_state_step(
        1,
        "Navigation detected".to_string(),
        Err(DEAD_TRANSPORT.to_string()),
    );
    assert!(!dead.ok);
    assert_eq!(dead.error.as_deref(), Some(DEAD_TRANSPORT));
}
