use super::*;

#[test]
fn clock_install_time_is_optional_and_resume_takes_no_parameters() {
    let install: BrowserStep =
        serde_json::from_value(serde_json::json!({"action": "clock_install"})).unwrap();
    assert!(matches!(install, BrowserStep::ClockInstall { time: None }));
    assert_eq!(
        serde_json::to_value(&install).unwrap(),
        serde_json::json!({"action": "clock_install"})
    );
    let resume: BrowserStep =
        serde_json::from_value(serde_json::json!({"action": "clock_resume"})).unwrap();
    assert!(matches!(resume, BrowserStep::ClockResume));
}

#[test]
fn clock_steps_reject_missing_required_parameters() {
    for action in [
        "clock_fast_forward",
        "clock_run_for",
        "clock_pause_at",
        "clock_set_fixed_time",
        "clock_set_system_time",
    ] {
        assert!(
            serde_json::from_value::<BrowserStep>(serde_json::json!({"action": action})).is_err(),
            "{action} accepted a missing parameter"
        );
    }
}

#[test]
fn test_locator_css_serde() {
    let loc = BrowserLocator::css("#btn");
    let json = serde_json::to_value(&loc).unwrap();
    assert_eq!(json["by"], "css");
    assert_eq!(json["value"], "#btn");
    let parsed: BrowserLocator = serde_json::from_value(json).unwrap();
    assert_eq!(parsed, loc);
}

#[test]
fn ref_locator_round_trips() {
    for reference in ["e12", "f2e7"] {
        let locator = BrowserLocator::reference(reference);
        let value = serde_json::to_value(&locator).unwrap();
        assert_eq!(value, serde_json::json!({"by": "ref", "value": reference}));
        assert_eq!(
            serde_json::from_value::<BrowserLocator>(value).unwrap(),
            locator
        );
    }
}

#[test]
fn frame_chain_round_trips_outermost_first() {
    let locator = BrowserLocator::role("button", Some("Save")).in_frames(vec![
        BrowserLocator::css("#outer"),
        BrowserLocator::role("iframe", Some("Editor")),
    ]);
    let value = serde_json::to_value(&locator).unwrap();

    assert_eq!(
        value["frames"],
        serde_json::json!([
            {"by": "css", "value": "#outer"},
            {"by": "role", "role": "iframe", "name": "Editor"}
        ])
    );
    assert_eq!(
        serde_json::from_value::<BrowserLocator>(value).unwrap(),
        locator
    );
}

#[test]
fn test_locator_id_serde() {
    let loc = BrowserLocator::id("email");
    let json = serde_json::to_value(&loc).unwrap();
    assert_eq!(json["by"], "id");
    assert_eq!(json["value"], "email");
}

#[test]
fn test_locator_name_serde() {
    let loc = BrowserLocator::name("q");
    let json = serde_json::to_value(&loc).unwrap();
    assert_eq!(json["by"], "name");
    assert_eq!(json["value"], "q");
}

#[test]
fn test_locator_label_serde() {
    let loc = BrowserLocator::label("Email Address");
    let json = serde_json::to_value(&loc).unwrap();
    assert_eq!(json["by"], "label");
    assert_eq!(json["value"], "Email Address");
}

#[test]
fn test_locator_placeholder_serde() {
    let loc = BrowserLocator::placeholder("Search...");
    let json = serde_json::to_value(&loc).unwrap();
    assert_eq!(json["by"], "placeholder");
    assert_eq!(json["value"], "Search...");
}

#[test]
fn test_locator_role_serde() {
    let loc = BrowserLocator::role("textbox", Some("Search"));
    let json = serde_json::to_value(&loc).unwrap();
    assert_eq!(json["by"], "role");
    assert_eq!(json["role"], "textbox");
    assert_eq!(json["name"], "Search");
}

#[test]
fn test_locator_role_without_name_serde() {
    let loc = BrowserLocator::role("button", None);
    let json = serde_json::to_value(&loc).unwrap();
    assert_eq!(json["by"], "role");
    assert_eq!(json["role"], "button");
    assert!(json.get("name").is_none());
}

#[test]
fn test_locator_test_id_serde() {
    let loc = BrowserLocator::test_id("submit-btn");
    let json = serde_json::to_value(&loc).unwrap();
    assert_eq!(json["by"], "test_id");
    assert_eq!(json["value"], "submit-btn");
}

#[test]
fn test_locator_text_serde() {
    let json_str = r#"{"by": "text", "value": "Submit Form", "exact": true}"#;
    let loc: BrowserLocator = serde_json::from_str(json_str).unwrap();
    match &loc.strategy {
        LocatorStrategy::Text { value, exact, .. } => {
            assert_eq!(value, "Submit Form");
            assert!(*exact);
        }
        _ => panic!("Expected Text"),
    }
}

#[test]
fn old_locator_wire_shapes_still_round_trip() {
    for json in [
        r#"{"by":"test_id","value":"save"}"#,
        r#"{"by":"placeholder","value":"Search"}"#,
        r#"{"by":"text","value":"Submit"}"#,
        r#"{"by":"label","value":"Email"}"#,
        r#"{"by":"role","role":"button","name":"Save"}"#,
    ] {
        let locator: BrowserLocator = serde_json::from_str(json).unwrap();
        let round_trip = serde_json::to_value(&locator).unwrap();
        let expected: serde_json::Value = serde_json::from_str(json).unwrap();
        assert_eq!(round_trip, expected);
    }
}

#[test]
fn get_by_locator_options_round_trip() {
    let json = serde_json::json!({
        "by": "role",
        "role": "checkbox",
        "name_regex": {"source": "save\\s+item", "flags": "i"},
        "description": "primary action",
        "description_regex": {"source": "primary", "flags": "u"},
        "exact": true,
        "checked": "mixed",
        "pressed": false,
        "selected": true,
        "expanded": false,
        "disabled": true,
        "level": 3,
        "include_hidden": true,
        "nth": 1,
        "within": "#dialog"
    });
    let locator: BrowserLocator = serde_json::from_value(json.clone()).unwrap();
    assert_eq!(serde_json::to_value(locator).unwrap(), json);
}

#[test]
fn attribute_and_regex_locator_options_round_trip() {
    for json in [
        serde_json::json!({
            "by": "test_id",
            "value": "ignored for regex",
            "regex": {"source": "save-\\d+", "flags": "i"},
            "exact": true,
            "attribute": "data-qa"
        }),
        serde_json::json!({"by": "alt_text", "value": "logo", "exact": false}),
        serde_json::json!({"by": "title", "value": "Help", "exact": true}),
    ] {
        let locator: BrowserLocator = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(serde_json::to_value(locator).unwrap(), json);
    }
}

#[test]
fn test_locator_xpath_serde() {
    let json_str = r#"{"by": "xpath", "value": "//button[@type='submit']"}"#;
    let loc: BrowserLocator = serde_json::from_str(json_str).unwrap();
    match &loc.strategy {
        LocatorStrategy::Xpath { value } => {
            assert!(value.contains("submit"));
        }
        _ => panic!("Expected Xpath"),
    }
}

#[test]
fn test_locator_autocomplete_serde() {
    let json_str = r#"{"by": "autocomplete", "value": "email"}"#;
    let loc: BrowserLocator = serde_json::from_str(json_str).unwrap();
    match &loc.strategy {
        LocatorStrategy::Autocomplete { value } => {
            assert_eq!(value, "email");
        }
        _ => panic!("Expected Autocomplete"),
    }
}

#[test]
fn test_locator_with_nth_and_within() {
    let json_str = r##"{"by": "css", "value": "input", "nth": 2, "within": "#form"}"##;
    let loc: BrowserLocator = serde_json::from_str(json_str).unwrap();
    assert_eq!(loc.nth, Some(2));
    assert_eq!(loc.within.as_deref(), Some("#form"));
}

#[test]
fn test_locator_nth_and_within_omitted_when_none() {
    let loc = BrowserLocator::css("div");
    let json = serde_json::to_value(&loc).unwrap();
    assert!(json.get("nth").is_none());
    assert!(json.get("within").is_none());
}

#[test]
fn composed_locator_wire_round_trips() {
    let json = serde_json::json!({
        "by": "css",
        "value": ".card",
        "locator": {"by": "role", "role": "button", "name": "Open"},
        "filter": {
            "has": {"by": "css", "value": ".ready"},
            "has_not": {"by": "css", "value": ".disabled"},
            "has_text": "Alpha",
            "has_not_text": {"source": "archived", "flags": "i"},
            "visible": true
        },
        "and": {"by": "css", "value": ".featured"},
        "or": {"by": "test_id", "value": "fallback"},
        "last": true
    });
    let locator: BrowserLocator = serde_json::from_value(json.clone()).unwrap();
    assert_eq!(serde_json::to_value(locator).unwrap(), json);
}

#[test]
fn first_last_and_signed_nth_round_trip() {
    for json in [
        serde_json::json!({"by":"css","value":"li","first":true}),
        serde_json::json!({"by":"css","value":"li","last":true}),
        serde_json::json!({"by":"css","value":"li","nth":-1}),
    ] {
        let locator: BrowserLocator = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(serde_json::to_value(locator).unwrap(), json);
    }
}

#[test]
fn test_tab_target_active_serde() {
    let t = TabTarget::Active;
    let json = serde_json::to_value(&t).unwrap();
    assert_eq!(json["type"], "active");
}

#[test]
fn test_tab_target_id_serde() {
    let t = TabTarget::Id {
        id: "main".to_string(),
    };
    let json = serde_json::to_value(&t).unwrap();
    assert_eq!(json["type"], "id");
    assert_eq!(json["id"], "main");
}

#[test]
fn test_step_navigate_serde() {
    let step = BrowserStep::Navigate {
        url: "https://example.com".to_string(),
        timeout_ms: None,
    };
    let json = serde_json::to_value(&step).unwrap();
    assert_eq!(json["action"], "navigate");
    assert_eq!(json["url"], "https://example.com");
}

#[test]
fn test_step_click_serde() {
    let json_str = r##"{"action": "click", "locator": {"by": "css", "value": "#btn"}}"##;
    let step: BrowserStep = serde_json::from_str(json_str).unwrap();
    match step {
        BrowserStep::Click { locator, .. } => {
            assert_eq!(
                locator.strategy,
                LocatorStrategy::Css {
                    value: "#btn".to_string()
                }
            );
        }
        _ => panic!("Expected Click"),
    }
}

#[test]
fn test_step_fill_serde() {
    let json_str = r#"{
        "action": "fill",
        "locator": {"by": "name", "value": "q"},
        "text": "rust tutorial",
        "clear_first": true,
        "verify": true
    }"#;
    let step: BrowserStep = serde_json::from_str(json_str).unwrap();
    match step {
        BrowserStep::Fill {
            locator,
            text,
            clear_first,
            verify,
        } => {
            assert_eq!(text, "rust tutorial");
            assert!(clear_first);
            assert!(verify);
            match &locator.strategy {
                LocatorStrategy::Name { value } => assert_eq!(value, "q"),
                _ => panic!("Expected Name locator"),
            }
        }
        _ => panic!("Expected Fill"),
    }
}

#[test]
fn test_step_fill_defaults() {
    let json_str = r##"{
        "action": "fill",
        "locator": {"by": "css", "value": "#input"},
        "text": "hello"
    }"##;
    let step: BrowserStep = serde_json::from_str(json_str).unwrap();
    match step {
        BrowserStep::Fill {
            clear_first,
            verify,
            ..
        } => {
            assert!(clear_first, "clear_first should default to true");
            assert!(verify, "verify should default to true");
        }
        _ => panic!("Expected Fill"),
    }
}

#[test]
fn test_step_wait_for_url_serde() {
    let json_str = r#"{"action": "wait_for_url", "pattern": "/search", "timeout_ms": 5000}"#;
    let step: BrowserStep = serde_json::from_str(json_str).unwrap();
    match step {
        BrowserStep::WaitForUrl {
            pattern,
            timeout_ms,
        } => {
            assert_eq!(pattern, UrlPattern::Text("/search".to_string()));
            assert_eq!(timeout_ms, Some(5000));
        }
        _ => panic!("Expected WaitForUrl"),
    }

    let regex: BrowserStep = serde_json::from_str(
        r#"{"action": "wait_for_url", "pattern": {"source": "/search\\?q=", "flags": "i"}}"#,
    )
    .unwrap();
    assert!(matches!(
        regex,
        BrowserStep::WaitForUrl {
            pattern: UrlPattern::Regex { .. },
            ..
        }
    ));
}

#[test]
fn test_network_wait_steps_serde() {
    let load: BrowserStep = serde_json::from_str(
        r#"{"action":"wait_for_load_state","state":"networkidle","timeout_ms":7000}"#,
    )
    .unwrap();
    assert!(matches!(
        load,
        BrowserStep::WaitForLoadState {
            state: BrowserLoadState::Networkidle,
            timeout_ms: Some(7000)
        }
    ));

    let response: BrowserStep = serde_json::from_str(
        r#"{"action":"wait_for_response","pattern":{"source":"/api/.*","flags":"i"}}"#,
    )
    .unwrap();
    assert!(matches!(
        response,
        BrowserStep::WaitForResponse {
            pattern: UrlPattern::Regex { source, flags },
            method: None,
            status: None,
            timeout_ms: None
        } if source == "/api/.*" && flags == "i"
    ));

    let request: BrowserStep =
        serde_json::from_str(r#"{"action":"wait_for_request","pattern":{"source":"/api/.*"}}"#)
            .unwrap();
    assert!(matches!(
        request,
        BrowserStep::WaitForRequest {
            pattern: UrlPattern::Regex { source, flags },
            method: None,
            timeout_ms: None
        } if source == "/api/.*" && flags.is_empty()
    ));
}

#[test]
fn route_handlers_and_patternless_unroute_round_trip() {
    for value in [
        serde_json::json!({
            "action": "route",
            "pattern": "**/api/**",
            "handler": {
                "type": "fulfill",
                "status": 200,
                "content_type": "application/json",
                "body": "{\"ok\":true}",
                "headers": {"X-Mocked": "yes"}
            }
        }),
        serde_json::json!({
            "action": "route",
            "pattern": "**/*.png",
            "handler": {"type": "abort", "reason": "blockedbyclient"}
        }),
        serde_json::json!({
            "action": "route",
            "pattern": {"source": "/api/", "flags": "i"},
            "handler": {
                "type": "continue",
                "url": "https://example.com/api/other",
                "method": "POST",
                "headers": {"X-Test": "route"},
                "post_data": "password=secret"
            }
        }),
        serde_json::json!({"action": "unroute"}),
    ] {
        let step: BrowserStep = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(step).unwrap(), value);
    }
}

#[test]
fn test_file_transfer_steps_serde() {
    let upload: BrowserStep = serde_json::from_str(
        r##"{"action":"set_input_files","locator":{"by":"css","value":"#file"},"paths":["/workspace/a.txt"]}"##,
    )
    .unwrap();
    assert!(matches!(
        upload,
        BrowserStep::SetInputFiles { paths, .. } if paths == vec!["/workspace/a.txt"]
    ));

    let chooser: BrowserStep =
        serde_json::from_str(r#"{"action":"expect_file_chooser","paths":["/workspace/a.txt"]}"#)
            .unwrap();
    assert!(matches!(chooser, BrowserStep::ExpectFileChooser { paths } if paths.len() == 1));

    let download: BrowserStep = serde_json::from_str(
        r#"{"action":"wait_for_download","timeout_ms":9000,"save_as":"saved.txt"}"#,
    )
    .unwrap();
    assert!(matches!(
        download,
        BrowserStep::WaitForDownload {
            timeout_ms: Some(9000),
            save_as: Some(name)
        } if name == "saved.txt"
    ));
}

#[test]
fn readout_steps_address_a_single_locator() {
    for (json, expected) in [
        (
            r#"{"action":"bounding_box","locator":{"by":"ref","value":"e1"}}"#,
            "bounding_box",
        ),
        (
            r#"{"action":"count","locator":{"by":"css","value":".row"}}"#,
            "count",
        ),
        (
            r#"{"action":"input_value","locator":{"by":"ref","value":"e1"}}"#,
            "input_value",
        ),
        (
            r#"{"action":"element_state","locator":{"by":"ref","value":"e1"}}"#,
            "element_state",
        ),
    ] {
        let step: BrowserStep = serde_json::from_str(json).unwrap();
        assert_eq!(serde_json::to_value(&step).unwrap()["action"], expected);
    }
}

#[test]
fn all_texts_mode_defaults_to_inner_text_and_limit_is_optional() {
    let step: BrowserStep =
        serde_json::from_str(r#"{"action":"all_texts","locator":{"by":"css","value":"li"}}"#)
            .unwrap();

    assert!(matches!(
        step,
        BrowserStep::AllTexts {
            mode: BrowserTextMode::InnerText,
            limit: None,
            ..
        }
    ));

    let explicit: BrowserStep = serde_json::from_str(
        r#"{"action":"all_texts","locator":{"by":"css","value":"li"},"mode":"text_content","limit":5}"#,
    )
    .unwrap();

    assert!(matches!(
        explicit,
        BrowserStep::AllTexts {
            mode: BrowserTextMode::TextContent,
            limit: Some(5),
            ..
        }
    ));
}

#[test]
fn readout_steps_reject_unknown_fields() {
    let error = serde_json::from_str::<BrowserStep>(
        r#"{"action":"all_texts","locator":{"by":"css","value":"li"},"limits":5}"#,
    )
    .unwrap_err()
    .to_string();

    assert!(error.contains("limits"), "{error}");
}

#[test]
fn test_step_extract_links_serde() {
    let json_str = r#"{"action": "extract_links", "limit": 10}"#;
    let step: BrowserStep = serde_json::from_str(json_str).unwrap();
    match step {
        BrowserStep::ExtractLinks { locator, limit } => {
            assert!(locator.is_none());
            assert_eq!(limit, Some(10));
        }
        _ => panic!("Expected ExtractLinks"),
    }
}

#[test]
fn test_step_extract_table_serde() {
    let json_str =
        r#"{"action": "extract_table", "locator": {"by": "css", "value": "table"}, "limit": 4}"#;
    let step: BrowserStep = serde_json::from_str(json_str).unwrap();
    match step {
        BrowserStep::ExtractTable { limit, .. } => assert_eq!(limit, Some(4)),
        _ => panic!("Expected ExtractTable"),
    }
}

#[test]
fn test_step_extract_table_limit_defaults_to_none() {
    let json_str = r#"{"action": "extract_table", "locator": {"by": "css", "value": "table"}}"#;
    let step: BrowserStep = serde_json::from_str(json_str).unwrap();
    match step {
        BrowserStep::ExtractTable { limit, .. } => assert_eq!(limit, None),
        _ => panic!("Expected ExtractTable"),
    }
}

#[test]
fn test_step_screenshot_elements_serde() {
    let json_str = r##"{"action": "screenshot_elements", "locators": [{"by": "css", "value": ".card"}, {"by": "ref", "value": "e7"}], "compose": "separate", "labels": false, "type": "webp", "mask_color": "#00FF00"}"##;
    let step: BrowserStep = serde_json::from_str(json_str).unwrap();
    match step {
        BrowserStep::ScreenshotElements {
            locators,
            compose,
            labels,
            options,
        } => {
            assert_eq!(locators.len(), 2);
            assert_eq!(compose, BrowserComposeMode::Separate);
            assert_eq!(labels, Some(false));
            assert_eq!(options.image_type, Some(BrowserScreenshotType::Webp));
            assert_eq!(options.mask_color.as_deref(), Some("#00FF00"));
        }
        _ => panic!("Expected ScreenshotElements"),
    }
}

#[test]
fn test_step_screenshot_elements_defaults_to_a_labelled_grid() {
    let json_str =
        r#"{"action": "screenshot_elements", "locators": [{"by": "css", "value": ".card"}]}"#;
    let step: BrowserStep = serde_json::from_str(json_str).unwrap();
    match step {
        BrowserStep::ScreenshotElements {
            compose,
            labels,
            options,
            ..
        } => {
            assert_eq!(compose, BrowserComposeMode::Grid);
            assert_eq!(labels, None);
            assert_eq!(options, BrowserScreenshotOptions::default());
        }
        _ => panic!("Expected ScreenshotElements"),
    }
}

#[test]
fn test_step_capture_element_states_serde() {
    let json_str = r#"{"action": "capture_element_states", "locator": {"by": "css", "value": "button"}, "states": ["default", "hover", "focus", "active"], "full_page": false}"#;
    let step: BrowserStep = serde_json::from_str(json_str).unwrap();
    match step {
        BrowserStep::CaptureElementStates {
            states,
            labels,
            options,
            ..
        } => {
            assert_eq!(
                states,
                vec![
                    BrowserElementState::Default,
                    BrowserElementState::Hover,
                    BrowserElementState::Focus,
                    BrowserElementState::Active,
                ]
            );
            assert_eq!(labels, None);
            assert!(!options.full_page);
        }
        _ => panic!("Expected CaptureElementStates"),
    }
}

#[test]
fn test_element_states_round_trip_on_the_wire() {
    for (state, wire) in [
        (BrowserElementState::Default, "default"),
        (BrowserElementState::Hover, "hover"),
        (BrowserElementState::Focus, "focus"),
        (BrowserElementState::Active, "active"),
    ] {
        assert_eq!(serde_json::to_value(state).unwrap(), wire);
        assert_eq!(state.label(), wire);
        assert_eq!(
            serde_json::from_value::<BrowserElementState>(serde_json::json!(wire)).unwrap(),
            state
        );
    }
    assert!(serde_json::from_value::<BrowserElementState>(serde_json::json!("visited")).is_err());
}

#[test]
fn wait_for_function_defaults_to_a_page_predicate_without_a_fixed_interval() {
    let step: BrowserStep =
        serde_json::from_str(r#"{"action": "wait_for_function", "expression": "() => ready"}"#)
            .unwrap();

    let BrowserStep::WaitForFunction {
        expression,
        locator,
        timeout_ms,
        polling_ms,
    } = step
    else {
        panic!("expected WaitForFunction");
    };
    assert_eq!(expression, "() => ready");
    assert!(locator.is_none());
    assert!(timeout_ms.is_none());
    assert!(polling_ms.is_none());
}

#[test]
fn wait_for_function_accepts_a_locator_and_a_fixed_interval() {
    let step: BrowserStep = serde_json::from_str(
        r##"{"action": "wait_for_function", "expression": "el => el.dataset.state === 'ready'", "locator": {"by": "css", "value": "#row"}, "timeout_ms": 2000, "polling_ms": 40}"##,
    )
    .unwrap();

    let BrowserStep::WaitForFunction {
        locator,
        timeout_ms,
        polling_ms,
        ..
    } = step
    else {
        panic!("expected WaitForFunction");
    };
    assert!(locator.is_some());
    assert_eq!(timeout_ms, Some(2_000));
    assert_eq!(polling_ms, Some(40));
}

#[test]
fn expect_poll_round_trips_every_matcher_name() {
    for (name, expected) in [
        ("equals", BrowserPollMatcher::Equals),
        ("contains", BrowserPollMatcher::Contains),
        ("gt", BrowserPollMatcher::Gt),
        ("lt", BrowserPollMatcher::Lt),
        ("matches_regex", BrowserPollMatcher::MatchesRegex),
    ] {
        let step: BrowserStep = serde_json::from_value(serde_json::json!({
            "action": "expect_poll",
            "expression": "window.__count",
            "expected": 3,
            "matcher": name,
        }))
        .unwrap();

        let BrowserStep::ExpectPoll { matcher, soft, .. } = step else {
            panic!("expected ExpectPoll");
        };
        assert_eq!(matcher, expected);
        assert_eq!(matcher.name(), name);
        assert!(soft.is_none());
        assert_eq!(
            serde_json::to_value(matcher).unwrap(),
            serde_json::json!(name)
        );
    }
}

#[test]
fn expect_poll_requires_an_expression_expected_value_and_matcher() {
    for missing in [
        serde_json::json!({"action": "expect_poll", "expected": 1, "matcher": "equals"}),
        serde_json::json!({"action": "expect_poll", "expression": "x", "matcher": "equals"}),
        serde_json::json!({"action": "expect_poll", "expression": "x", "expected": 1}),
        serde_json::json!({"action": "expect_poll", "expression": "x", "expected": 1, "matcher": "starts_with"}),
    ] {
        assert!(
            serde_json::from_value::<BrowserStep>(missing.clone()).is_err(),
            "expected rejection: {missing}"
        );
    }
}

#[test]
fn test_step_eval_serde() {
    let json_str = r#"{"action": "eval", "expression": "document.title"}"#;
    let step: BrowserStep = serde_json::from_str(json_str).unwrap();
    match step {
        BrowserStep::Eval {
            expression,
            timeout_ms,
        } => {
            assert_eq!(expression, "document.title");
            assert_eq!(timeout_ms, None);
        }
        _ => panic!("Expected Eval"),
    }
}

#[test]
fn test_step_press_key_serde() {
    let json_str = r#"{"action": "press_key", "key": "Enter", "modifiers": ["Ctrl"]}"#;
    let step: BrowserStep = serde_json::from_str(json_str).unwrap();
    match step {
        BrowserStep::PressKey { key, modifiers } => {
            assert_eq!(key, "Enter");
            assert_eq!(modifiers, vec!["Ctrl"]);
        }
        _ => panic!("Expected PressKey"),
    }
}

#[test]
fn test_step_screenshot_serde() {
    let json_str = r#"{"action": "screenshot"}"#;
    let step: BrowserStep = serde_json::from_str(json_str).unwrap();
    assert!(matches!(step, BrowserStep::Screenshot { .. }));
}

#[test]
fn accessibility_snapshot_options_default_to_ai_refs_and_round_trip() {
    let step: BrowserStep = serde_json::from_str(r#"{"action":"accessibility_snapshot"}"#).unwrap();
    match step {
        BrowserStep::AccessibilitySnapshot { options } => {
            assert_eq!(options.mode, AccessibilitySnapshotMode::Ai);
            assert!(options.refs_enabled());
            assert!(!options.boxes);
            assert!(options.locator.is_none());
            assert!(options.depth.is_none());
            assert!(options.max_chars.is_none());
        }
        _ => panic!("Expected AccessibilitySnapshot"),
    }

    let value = serde_json::json!({
        "action": "accessibility_snapshot",
        "mode": "default",
        "refs": true,
        "boxes": true,
        "locator": {"by": "css", "value": "main"},
        "depth": 3,
        "max_chars": 4096
    });
    let step: BrowserStep = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(serde_json::to_value(step).unwrap(), value);
}

#[test]
fn accessibility_snapshot_accepts_the_legacy_root_alias() {
    let step: BrowserStep = serde_json::from_value(serde_json::json!({
        "action": "accessibility_snapshot",
        "root": {"by": "css", "value": "main"}
    }))
    .unwrap();
    let BrowserStep::AccessibilitySnapshot { options } = step else {
        panic!("Expected AccessibilitySnapshot");
    };
    assert_eq!(
        options.locator,
        Some(BrowserLocator::css("main")),
        "legacy root spelling must still deserialize"
    );
    assert_eq!(
        serde_json::to_value(BrowserStep::AccessibilitySnapshot { options }).unwrap()["locator"],
        serde_json::json!({"by": "css", "value": "main"}),
        "only the canonical locator name is emitted"
    );
}

#[test]
fn accessibility_snapshot_scoping_options_compose() {
    let step: BrowserStep = serde_json::from_value(serde_json::json!({
        "action": "accessibility_snapshot",
        "locator": {"by": "css", "value": "#menu"},
        "depth": 2,
        "boxes": true
    }))
    .unwrap();
    let BrowserStep::AccessibilitySnapshot { options } = step else {
        panic!("Expected AccessibilitySnapshot");
    };
    assert_eq!(options.locator, Some(BrowserLocator::css("#menu")));
    assert_eq!(options.depth, Some(2));
    assert!(options.boxes);
    assert!(options.refs_enabled());
}

#[test]
fn accessibility_snapshot_omitting_scoping_options_stays_byte_stable() {
    let options = AccessibilitySnapshotOptions::default();
    assert_eq!(
        serde_json::to_string(&BrowserStep::AccessibilitySnapshot { options }).unwrap(),
        r#"{"action":"accessibility_snapshot","mode":"ai","boxes":false}"#
    );
}

#[test]
fn test_step_handle_dialog_serde() {
    let json_str = r#"{"action": "handle_dialog", "accept": true, "prompt_text": "answer"}"#;
    let step: BrowserStep = serde_json::from_str(json_str).unwrap();
    match step {
        BrowserStep::HandleDialog {
            accept,
            prompt_text,
        } => {
            assert!(accept);
            assert_eq!(prompt_text.as_deref(), Some("answer"));
        }
        _ => panic!("Expected HandleDialog"),
    }
}

#[test]
fn test_step_dismiss_overlays_serde() {
    let json_str = r#"{"action": "dismiss_overlays"}"#;
    let step: BrowserStep = serde_json::from_str(json_str).unwrap();
    assert!(matches!(
        step,
        BrowserStep::DismissOverlays { aggressive: false }
    ));

    let opt_in: BrowserStep =
        serde_json::from_str(r#"{"action": "dismiss_overlays", "aggressive": true}"#).unwrap();
    assert!(matches!(
        opt_in,
        BrowserStep::DismissOverlays { aggressive: true }
    ));
}

#[test]
fn test_locator_handler_steps_serde() {
    let json_str = r##"{
        "action": "add_locator_handler",
        "name": "cookie",
        "locator": {"by": "css", "value": "#accept"},
        "handler": {"type": "click"},
        "times": 2,
        "no_wait_after": true
    }"##;
    let step: BrowserStep = serde_json::from_str(json_str).unwrap();
    match step {
        BrowserStep::AddLocatorHandler {
            name,
            handler,
            times,
            no_wait_after,
            ..
        } => {
            assert_eq!(name, "cookie");
            assert!(matches!(handler, LocatorHandlerAction::Click));
            assert_eq!(times, Some(2));
            assert!(no_wait_after);
        }
        _ => panic!("expected add_locator_handler"),
    }

    let remove: BrowserStep =
        serde_json::from_str(r#"{"action":"remove_locator_handler","name":"cookie"}"#).unwrap();
    assert!(matches!(
        remove,
        BrowserStep::RemoveLocatorHandler { name } if name == "cookie"
    ));
}

#[test]
fn test_step_wait_seconds_serde() {
    let json_str = r#"{"action": "wait_seconds", "seconds": 2.5}"#;
    let step: BrowserStep = serde_json::from_str(json_str).unwrap();
    match step {
        BrowserStep::WaitSeconds { seconds } => {
            assert!((seconds - 2.5).abs() < f64::EPSILON);
        }
        _ => panic!("Expected WaitSeconds"),
    }
}

#[test]
fn test_full_request_serde() {
    let json_str = r#"{
        "session": "shared_default",
        "target": {"type": "active"},
        "steps": [
            {"action": "navigate", "url": "https://www.google.com"},
            {"action": "fill", "locator": {"by": "name", "value": "q"}, "text": "rust tokio tutorial"},
            {"action": "press_key", "key": "Enter"},
            {"action": "wait_for_url", "contains": "/search"},
            {"action": "extract_links", "limit": 10}
        ]
    }"#;
    let req: BrowserActionRequest = serde_json::from_str(json_str).unwrap();
    assert_eq!(req.session, SessionPolicy::SharedDefault);
    assert_eq!(req.target, TabTarget::Active);
    assert_eq!(req.steps.len(), 5);
}

#[test]
fn test_request_defaults() {
    let json_str = r#"{"steps": [{"action": "screenshot"}]}"#;
    let req: BrowserActionRequest = serde_json::from_str(json_str).unwrap();
    assert_eq!(req.session, SessionPolicy::SharedDefault);
    assert_eq!(req.target, TabTarget::Active);
    assert!(req.attach_screenshot.is_none());
}

#[test]
fn test_request_attach_screenshot_is_tri_state() {
    let omitted: BrowserActionRequest = serde_json::from_str(r#"{"steps": []}"#).unwrap();
    assert_eq!(omitted.attach_screenshot, None);

    let enabled: BrowserActionRequest =
        serde_json::from_str(r#"{"attach_screenshot": true, "steps": []}"#).unwrap();
    assert_eq!(enabled.attach_screenshot, Some(true));

    let suppressed: BrowserActionRequest =
        serde_json::from_str(r#"{"attach_screenshot": false, "steps": []}"#).unwrap();
    assert_eq!(suppressed.attach_screenshot, Some(false));
}

#[test]
fn test_page_context_mode_defaults_to_snapshot_and_splits_the_two_attachments() {
    let omitted: BrowserActionRequest = serde_json::from_str(r#"{"steps": []}"#).unwrap();
    assert_eq!(omitted.page_context, None);
    assert_eq!(omitted.page_context_mode(), PageContextMode::Snapshot);
    assert_eq!(PageContextMode::default(), PageContextMode::Snapshot);

    for (mode, snapshot, screenshot) in [
        (PageContextMode::Snapshot, true, false),
        (PageContextMode::Screenshot, false, true),
        (PageContextMode::Both, true, true),
        (PageContextMode::None, false, false),
    ] {
        assert_eq!(mode.includes_snapshot(), snapshot, "{mode:?}");
        assert_eq!(mode.includes_screenshot(), screenshot, "{mode:?}");
    }
}

#[test]
fn test_page_context_is_omitted_from_the_wire_when_unset() {
    let request: BrowserActionRequest = serde_json::from_str(r#"{"steps": []}"#).unwrap();
    let value = serde_json::to_value(&request).unwrap();
    assert_eq!(value["page_context"], serde_json::Value::Null);
}

#[test]
fn test_execution_report_page_block_is_additive_for_existing_clients() {
    let legacy = serde_json::json!({
        "ok": true,
        "steps": [{"step_index": 0, "ok": true, "summary": "Navigated"}],
        "url": "https://example.com",
        "title": "Example",
        "dialogs": [],
        "new_tabs": [],
    });
    let report: ExecutionReport = serde_json::from_value(legacy).unwrap();
    assert!(report.page.is_none());
    assert!(report.steps[0].locator_echo.is_none());

    let serialized = serde_json::to_value(&report).unwrap();
    assert!(serialized.get("page").is_none());
    assert!(serialized["steps"][0].get("locator_echo").is_none());
    assert_eq!(serialized["url"], "https://example.com");
    assert_eq!(serialized["title"], "Example");
}

#[test]
fn test_page_context_round_trips_the_header_and_the_snapshot_pointer() {
    let page = BrowserPageContext {
        status: Some(404),
        console: BrowserConsoleCounts {
            errors: 1,
            warnings: 0,
        },
        snapshot: Some(BrowserPageSnapshot {
            yaml: "- heading \"Not Found\" [ref=e1]".to_string(),
            lines: 1,
            bytes: 12_000,
            truncated: true,
            artifact: Some(BrowserSnapshotArtifact {
                kind: "aria_snapshot".to_string(),
                mime: "text/yaml".to_string(),
                path: std::path::PathBuf::from("/tmp/artifacts/snapshot-1.yaml"),
                bytes: 12_000,
            }),
            frames: Vec::new(),
        }),
    };

    let value = serde_json::to_value(&page).unwrap();
    assert!(value["snapshot"].get("frames").is_none());
    assert_eq!(value["status"], 404);
    assert_eq!(value["console"]["errors"], 1);
    assert_eq!(value["snapshot"]["truncated"], true);
    assert_eq!(value["snapshot"]["artifact"]["kind"], "aria_snapshot");
    assert_eq!(value["snapshot"]["artifact"]["mime"], "text/yaml");
    assert_eq!(
        serde_json::from_value::<BrowserPageContext>(value).unwrap(),
        page
    );
}

#[test]
fn test_step_result_carries_the_canonical_locator_echo() {
    let step = StepResult::success(0, "click on <button>")
        .with_locator_echo(Some("getByRole('button', { name: 'Save' })".to_string()));

    let value = serde_json::to_value(&step).unwrap();
    assert_eq!(
        value["locator_echo"],
        "getByRole('button', { name: 'Save' })"
    );
    assert!(serde_json::to_value(StepResult::success(0, "noop"))
        .unwrap()
        .get("locator_echo")
        .is_none());
}

#[test]
fn test_request_network_mode_defaults_to_summary() {
    let omitted: BrowserActionRequest = serde_json::from_str(r#"{"steps": []}"#).unwrap();
    assert_eq!(omitted.network, NetworkReportMode::Summary);
    assert_eq!(NetworkReportMode::default(), NetworkReportMode::Summary);
}

#[test]
fn test_request_network_mode_accepts_every_lowercase_variant() {
    for (raw, expected) in [
        ("none", NetworkReportMode::None),
        ("summary", NetworkReportMode::Summary),
        ("full", NetworkReportMode::Full),
    ] {
        let parsed: BrowserActionRequest =
            serde_json::from_str(&format!(r#"{{"network": "{raw}", "steps": []}}"#)).unwrap();
        assert_eq!(parsed.network, expected);
        assert_eq!(serde_json::to_value(expected).unwrap(), raw);
    }
    assert!(
        serde_json::from_str::<BrowserActionRequest>(r#"{"network": "Full", "steps": []}"#)
            .is_err()
    );
}

#[test]
fn test_step_result_success() {
    let r = StepResult::success(0, "Navigated to https://example.com");
    assert!(r.ok);
    assert_eq!(r.step_index, 0);
    assert!(r.error.is_none());
}

#[test]
fn test_step_result_failure() {
    let r = StepResult::failure(1, "Click failed", "Element not found: #btn");
    assert!(!r.ok);
    assert_eq!(r.error.as_deref(), Some("Element not found: #btn"));
}

#[test]
fn test_step_result_with_data() {
    let r = StepResult::success(0, "Extracted").with_data(serde_json::json!(["link1", "link2"]));
    assert!(r.data.is_some());
}

#[test]
fn step_result_actionability_round_trips_all_none_states() {
    let mut result = StepResult::failure(2, "Click failed", "Timed out");
    result.actionability = Some(ActionabilityDiagnostics {
        call_log: vec![
            "waiting for css=#submit".to_string(),
            "element is not stable".to_string(),
        ],
        timed_out: true,
        elapsed_ms: Some(5_000),
        attempts: Some(3),
        attached: None,
        visible: None,
        stable: None,
        enabled: None,
        editable: None,
        receives_events: None,
        intercepting_element: None,
    });

    let json = serde_json::to_value(&result).unwrap();
    let actionability = json["actionability"].as_object().unwrap();
    assert_eq!(actionability["call_log"].as_array().unwrap().len(), 2);
    assert_eq!(actionability["timed_out"], true);
    assert_eq!(actionability["elapsed_ms"], 5_000);
    assert_eq!(actionability["attempts"], 3);
    for omitted in [
        "attached",
        "visible",
        "stable",
        "enabled",
        "editable",
        "receives_events",
        "intercepting_element",
    ] {
        assert!(!actionability.contains_key(omitted), "{omitted}");
    }

    let parsed: StepResult = serde_json::from_value(json).unwrap();
    assert_eq!(parsed.actionability, result.actionability);
}

#[test]
fn step_result_without_actionability_keeps_the_legacy_wire_shape() {
    let json = serde_json::to_value(StepResult::success(0, "Clicked")).unwrap();

    assert!(json.get("actionability").is_none());
}

#[test]
fn test_field_kind_serde() {
    let kinds = vec![
        FieldKind::TextInput,
        FieldKind::PasswordInput,
        FieldKind::SearchInput,
        FieldKind::Textarea,
        FieldKind::Select,
        FieldKind::ContentEditable,
    ];
    for kind in kinds {
        let json = serde_json::to_string(&kind).unwrap();
        let parsed: FieldKind = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, kind);
    }
}

#[test]
fn test_fill_strategy_serde() {
    let strategies = vec![
        FillStrategy::NativeTyping,
        FillStrategy::DomValueSetter,
        FillStrategy::NativePrototypeSetter,
        FillStrategy::ContentEditablePath,
        FillStrategy::ClickAndType,
    ];
    for s in strategies {
        let json = serde_json::to_string(&s).unwrap();
        let parsed: FillStrategy = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, s);
    }
}

#[test]
fn test_execution_report_serde() {
    let report = ExecutionReport {
        ok: true,
        warnings: vec!["browser session was dead; relaunched and retried".to_string()],
        steps: vec![
            StepResult::success(0, "nav ok"),
            StepResult::success(1, "click ok"),
        ],
        url: Some("https://example.com".to_string()),
        title: Some("Example".to_string()),
        page: Some(BrowserPageContext {
            status: Some(404),
            console: BrowserConsoleCounts {
                errors: 1,
                warnings: 2,
            },
            snapshot: None,
        }),
        stabilized: true,
        console: vec![],
        page_errors: vec![],
        network: vec![],
        network_summary: vec![],
        websockets: vec![],
        locator_handlers: vec![],
        dialogs: vec![DialogInfo {
            dialog_type: DialogType::Prompt,
            message: "Enter password=[REDACTED]".to_string(),
            default_value: "default".to_string(),
            action: DialogAction::Accepted,
            automatic: false,
        }],
        uploads: vec![UploadInfo {
            paths: vec!["/workspace/fixture.txt".to_string()],
            source: "direct".to_string(),
            in_memory_payloads: false,
        }],
        downloads: vec![DownloadInfo {
            guid: "download-guid".to_string(),
            url: "https://example.com/fixture.txt".to_string(),
            frame_id: "frame".to_string(),
            suggested_filename: "fixture.txt".to_string(),
            local_path: "/runtime/download-guid".to_string(),
            received_bytes: 7,
            total_bytes: 7,
            state: DownloadState::Completed,
            failure_reason: None,
        }],
        new_tabs: vec![],
        active_routes: vec![],
        intercepted_requests: vec![],
        context: None,
        screenshot: None,
    };
    let json = serde_json::to_value(&report).unwrap();
    assert!(json["ok"].as_bool().unwrap());
    assert_eq!(json["steps"].as_array().unwrap().len(), 2);
    assert_eq!(json["dialogs"][0]["type"], "prompt");
    assert_eq!(json["dialogs"][0]["action"], "accepted");
    assert_eq!(json["dialogs"][0]["automatic"], false);
    assert_eq!(json["uploads"][0]["source"], "direct");
    assert_eq!(json["downloads"][0]["state"], "completed");
    assert_eq!(json["page"]["status"], 404);
    assert_eq!(json["page"]["console"]["errors"], 1);
    assert_eq!(json["page"]["console"]["warnings"], 2);
    assert!(json["page"].get("snapshot").is_none());
    let parsed: ExecutionReport = serde_json::from_value(json).unwrap();
    assert!(parsed.ok);
    assert_eq!(parsed.page, report.page);
    assert_eq!(parsed.warnings, report.warnings);
}

#[test]
fn execution_report_warnings_stay_optional_on_the_wire() {
    let without_warnings = ExecutionReport {
        warnings: Vec::new(),
        ..serde_json::from_value::<ExecutionReport>(serde_json::json!({
            "ok": true,
            "steps": [],
            "dialogs": [],
            "new_tabs": [],
        }))
        .unwrap()
    };

    let json = serde_json::to_value(&without_warnings).unwrap();
    assert!(json.get("warnings").is_none());

    let legacy: ExecutionReport = serde_json::from_value(serde_json::json!({
        "ok": true,
        "steps": [],
        "dialogs": [],
        "new_tabs": [],
    }))
    .unwrap();
    assert!(legacy.warnings.is_empty());
}

#[test]
fn test_element_info_parse_from_js_json() {
    let json_str = r#"{
        "tag": "input",
        "input_type": "text",
        "id": "email",
        "name": "email",
        "placeholder": "Enter email",
        "aria_label": null,
        "role": null,
        "visible": true,
        "enabled": true,
        "readonly": false,
        "content_editable": false,
        "value": "",
        "inner_text": null,
        "bbox": {"x": 100.0, "y": 200.0, "width": 300.0, "height": 40.0},
        "field_kind": "text_input"
    }"#;
    let info: ElementInfo = serde_json::from_str(json_str).unwrap();
    assert_eq!(info.tag, "input");
    assert_eq!(info.input_type.as_deref(), Some("text"));
    assert!(info.visible);
    assert!(info.enabled);
    assert_eq!(info.field_kind, FieldKind::TextInput);
}

#[test]
fn test_tab_info_serde() {
    let ti = TabInfo {
        id: "1".to_string(),
        target_id: "ABC123".to_string(),
        url: "https://example.com".to_string(),
        title: "Example".to_string(),
        active: true,
        opener: Some(TabOpener {
            tab_id: "opener".to_string(),
            frame_id: Some("frame".to_string()),
        }),
        opened_by_step: Some(2),
    };
    let json = serde_json::to_value(&ti).unwrap();
    assert_eq!(json["id"], "1");
    assert!(json["active"].as_bool().unwrap());
    assert_eq!(json["opener"]["tab_id"], "opener");
}
