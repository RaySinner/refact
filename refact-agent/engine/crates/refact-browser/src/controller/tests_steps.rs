use super::*;
use super::tests_support::*;
use super::actions::{NETWORK_IDLE_WINDOW_MS, NavigationEvent, NavigationWaitOutcome, NavigationWaitTarget, TapTarget, classify_navigation_timeout, click_if_exists_skip_reason, console_entry_matches, injected_state_is_stable, mask_console_entry, merge_step_data, navigation_step_success, tap_target, uses_actionability_engine, wait_for_navigation_event, wait_for_selector_matches};
use super::capture::{all_texts_limit, build_page_snapshot, console_counts, element_bounding_box, notable_main_document_status, report_screenshot_requested, report_snapshot_requested, truncate_table_rows};
use super::context::{MAX_ALL_TEXTS, SNAPSHOT_SUMMARY_LINES, TAP_AMBIGUOUS_TARGET, TAP_REQUIRES_TOUCH, intercepts_pointer_events};
use super::dispatch::{PendingPopupWait, TabManagementSections, popup_wait_conflict, popup_wait_opened_result, popup_wait_timeout_result};
use super::expect_poll::{expect_retries, expectation_expected_value, expectation_text_list, viewport_ratio_matches};
use super::single_step::{needs_locator_handler_checkpoint};

#[test]
fn a_failed_capture_while_pressed_still_releases_the_mouse_button() {
    let recorded = RecordingMouse::default();
    let outcome = drive_states(
        &recorded,
        &[BrowserElementState::Active, BrowserElementState::Default],
        |state| match state {
            BrowserElementState::Active => Err("screenshot failed".to_string()),
            _ => Ok(()),
        },
    );

    assert_eq!(outcome.unwrap_err(), "screenshot failed");
    let events = button_events(&recorded);
    assert_eq!(
        events,
        vec![
            (
                refact_browser::MouseEventType::Pressed,
                Some(MouseButton::Left)
            ),
            (
                refact_browser::MouseEventType::Released,
                Some(MouseButton::Left)
            ),
        ],
        "a held button must never outlive the failing sequence"
    );
}

#[test]
fn a_completed_sequence_does_not_release_the_button_twice() {
    let recorded = RecordingMouse::default();
    let observed = drive_states(
        &recorded,
        &[BrowserElementState::Active, BrowserElementState::Default],
        |_| Ok(()),
    )
    .unwrap();

    assert_eq!(observed.len(), 2);
    assert_eq!(
        button_events(&recorded)
            .iter()
            .filter(|(event_type, _)| *event_type == refact_browser::MouseEventType::Released)
            .count(),
        1
    );
}

#[test]
fn a_failure_before_the_press_leaves_the_button_untouched() {
    let recorded = RecordingMouse::default();
    let outcome = drive_states(&recorded, &[BrowserElementState::Hover], |_| {
        Err("capture failed".to_string())
    });

    assert_eq!(outcome.unwrap_err(), "capture failed");
    assert!(button_events(&recorded).is_empty());
}

#[test]
fn arming_a_popup_wait_while_one_is_pending_fails_that_step() {
    assert_eq!(popup_wait_conflict(None), None);
    assert_eq!(
        popup_wait_conflict(Some(3)).as_deref(),
        Some("a popup wait is already armed at step 3")
    );
}

#[test]
fn popup_wait_arming_clamps_the_timeout_and_remembers_the_armed_step() {
    let settings = browser_settings::current();
    let armed = PendingPopupWait::arm(
        4,
        2,
        Some(settings.timing.max_wait_timeout_ms + 1_000),
        std::collections::BTreeSet::new(),
    );
    assert_eq!(armed.step_index, 4);
    assert_eq!(armed.result_index, 2);
    assert_eq!(armed.timeout_ms, settings.timing.max_wait_timeout_ms);

    let defaulted = PendingPopupWait::arm(0, 0, None, std::collections::BTreeSet::new());
    assert_eq!(
        defaulted.timeout_ms,
        settings.timing.default_wait_timeout_ms
    );
}

#[test]
fn popup_wait_outcomes_report_the_armed_step_index() {
    let opened = popup_wait_opened_result(1, "tab-9");
    assert!(opened.ok);
    assert_eq!(opened.step_index, 1);
    assert_eq!(opened.summary, "Popup opened: tab-9");
    assert_eq!(opened.data.unwrap()["tab_id"], "tab-9");

    let timed_out = popup_wait_timeout_result(1, 5_000);
    assert!(!timed_out.ok);
    assert_eq!(timed_out.step_index, 1);
    assert_eq!(timed_out.error.as_deref(), Some("Timed out after 5000ms"));
}

#[test]
fn nested_tab_management_reports_merge_every_section() {
    let nested = nested_tab_management_report();
    let mut sections = TabManagementSections::default();
    let step = sections.absorb(nested.clone()).unwrap();

    assert_eq!(step.summary, "Opened new desktop tab");
    assert_eq!(sections.new_tabs, nested.new_tabs);
    assert_eq!(sections.intercepted_requests, nested.intercepted_requests);
    assert_eq!(sections.locator_handlers, nested.locator_handlers);
    assert_eq!(sections.dialogs, nested.dialogs);
    assert_eq!(sections.uploads, nested.uploads);
    assert_eq!(sections.downloads, nested.downloads);
    assert_eq!(sections.websockets, nested.websockets);
}

#[test]
fn absorbing_two_nested_reports_keeps_every_section_in_order() {
    let mut sections = TabManagementSections::default();
    sections.absorb(nested_tab_management_report()).unwrap();
    sections.absorb(nested_tab_management_report()).unwrap();

    assert_eq!(sections.new_tabs.len(), 2);
    assert_eq!(sections.intercepted_requests.len(), 2);
    assert_eq!(sections.locator_handlers.len(), 2);
    assert_eq!(sections.dialogs.len(), 2);
    assert_eq!(sections.uploads.len(), 2);
    assert_eq!(sections.downloads.len(), 2);
    assert_eq!(sections.websockets.len(), 2);
}

#[test]
fn element_stability_is_read_from_the_injected_predicate() {
    assert!(injected_state_is_stable(&serde_json::json!({
        "stable": true,
        "matches": true
    })));
    assert!(!injected_state_is_stable(&serde_json::json!({
        "stable": false,
        "unstable": "deadline",
        "matches": false
    })));
    assert!(!injected_state_is_stable(&serde_json::json!({})));
}

#[test]
fn the_injected_idle_window_matches_the_network_monitor_definition() {
    assert_eq!(NETWORK_IDLE_WINDOW_MS, refact_browser::NETWORK_IDLE_MS);
}

#[test]
fn wait_for_selector_states_stay_non_strict_across_the_match_matrix() {
    let cases = [
        (BrowserWaitState::Attached, 0, 0, None),
        (BrowserWaitState::Attached, 3, 0, Some(3)),
        (BrowserWaitState::Detached, 0, 0, Some(0)),
        (BrowserWaitState::Detached, 3, 1, None),
        (BrowserWaitState::Visible, 3, 0, None),
        (BrowserWaitState::Visible, 3, 2, Some(2)),
        (BrowserWaitState::Visible, 0, 0, None),
        (BrowserWaitState::Hidden, 3, 0, Some(3)),
        (BrowserWaitState::Hidden, 3, 1, None),
        (BrowserWaitState::Hidden, 0, 0, Some(0)),
    ];
    for (state, attached, visible, expected) in cases {
        assert_eq!(
            wait_for_selector_matches(state, attached, visible),
            expected,
            "state={state:?} attached={attached} visible={visible}"
        );
    }
}

#[test]
fn absent_wait_for_selector_state_keeps_the_attached_behavior_and_summary() {
    let state = Option::<BrowserWaitState>::None.unwrap_or_default();

    assert_eq!(state, BrowserWaitState::Attached);
    assert!(!state.needs_visibility());
    assert!(BrowserWaitState::Visible.needs_visibility());
    assert!(BrowserWaitState::Hidden.needs_visibility());
    assert!(!BrowserWaitState::Detached.needs_visibility());
    assert_eq!(state.reached_label(), "Element found");
}

#[test]
fn console_filters_combine_substring_and_level() {
    let error = console_entry("Error", "boom while loading");
    let log = console_entry("Log", "boom later");
    let page_error = console_entry("page_error", "Uncaught boom");

    assert!(console_entry_matches(&error, None, None));
    assert!(console_entry_matches(
        &error,
        Some("boom"),
        Some(BrowserConsoleLevel::Error)
    ));
    assert!(!console_entry_matches(
        &error,
        Some("missing"),
        Some(BrowserConsoleLevel::Error)
    ));
    assert!(!console_entry_matches(
        &log,
        Some("boom"),
        Some(BrowserConsoleLevel::Error)
    ));
    assert!(console_entry_matches(
        &log,
        Some("boom"),
        Some(BrowserConsoleLevel::Log)
    ));
    assert!(console_entry_matches(
        &page_error,
        Some("boom"),
        Some(BrowserConsoleLevel::Error)
    ));
    assert!(!console_entry_matches(
        &page_error,
        None,
        Some(BrowserConsoleLevel::Warning)
    ));
    assert!(console_entry_matches(
        &console_entry("Warning", "slow"),
        None,
        Some(BrowserConsoleLevel::Warning)
    ));
}

#[test]
fn matched_console_entries_are_redacted_before_they_are_reported() {
    let entry = mask_console_entry(console_entry("Error", "token=super-secret-value failed"));

    assert!(!entry.text.contains("super-secret-value"), "{}", entry.text);
}

#[test]
fn click_if_exists_skips_attached_but_non_actionable_elements() {
    let mut invisible = actionable_state();
    invisible.visible = false;
    let mut disabled = actionable_state();
    disabled.enabled = false;
    let mut moving = actionable_state();
    moving.stable = false;

    assert_eq!(
        click_if_exists_skip_reason(&invisible),
        Some("element is not visible")
    );
    assert_eq!(
        click_if_exists_skip_reason(&disabled),
        Some("element is not enabled")
    );
    assert_eq!(
        click_if_exists_skip_reason(&moving),
        Some("element is not stable")
    );
}

#[test]
fn click_if_exists_clicks_actionable_elements() {
    assert_eq!(click_if_exists_skip_reason(&actionable_state()), None);
}

#[test]
fn tap_accepts_a_locator_or_a_coordinate_pair_but_never_both_or_neither() {
    let locator = BrowserLocator::css("#target");

    assert!(matches!(
        tap_target(Some(&locator), None, None),
        Ok(TapTarget::Element(_))
    ));
    let Ok(TapTarget::Point(x, y)) = tap_target(None, Some(4.0), Some(9.0)) else {
        panic!("coordinates must resolve to a coordinate tap");
    };
    assert_eq!((x, y), (4.0, 9.0));

    for (locator, x, y) in [
        (Some(&locator), Some(4.0), Some(9.0)),
        (Some(&locator), Some(4.0), None),
        (None, Some(4.0), None),
        (None, None, Some(9.0)),
        (None, None, None),
    ] {
        assert_eq!(tap_target(locator, x, y).err(), Some(TAP_AMBIGUOUS_TARGET));
    }
}

#[test]
fn tap_touch_requirement_names_the_step_and_field_that_enable_it() {
    assert!(TAP_REQUIRES_TOUCH.contains("set_viewport"));
    assert!(TAP_REQUIRES_TOUCH.contains("has_touch"));
}

#[test]
fn locator_tap_and_keyboard_steps_are_guarded_by_the_actionability_engine() {
    let locator = || BrowserLocator::css("#target");
    for step in [
        BrowserStep::Tap {
            locator: Some(locator()),
            x: None,
            y: None,
        },
        BrowserStep::InsertText {
            locator: Some(locator()),
            text: "hi".to_string(),
        },
        BrowserStep::PressSequentially {
            locator: locator(),
            text: "hi".to_string(),
            delay_ms: None,
        },
    ] {
        assert!(needs_locator_handler_checkpoint(&step), "{step:?}");
        assert!(uses_actionability_engine(&step), "{step:?}");
    }
}

#[test]
fn coordinate_tap_and_bare_insert_text_skip_locator_handler_checkpoints() {
    for step in [
        BrowserStep::Tap {
            locator: None,
            x: Some(1.0),
            y: Some(2.0),
        },
        BrowserStep::InsertText {
            locator: None,
            text: "hi".to_string(),
        },
    ] {
        assert!(!needs_locator_handler_checkpoint(&step), "{step:?}");
        assert!(!uses_actionability_engine(&step), "{step:?}");
    }
}

#[test]
fn wait_for_selector_is_satisfied_by_multiple_matches() {
    let attached = BrowserWaitState::Attached;
    assert_eq!(
        wait_for_selector_matches(attached, locator_handles(13).len(), 0),
        Some(13)
    );
    assert_eq!(
        wait_for_selector_matches(attached, locator_handles(1).len(), 0),
        Some(1)
    );
    assert_eq!(
        wait_for_selector_matches(attached, locator_handles(0).len(), 0),
        None
    );
    assert_eq!(
        wait_for_selector_matches(BrowserWaitState::Visible, 13, 13),
        Some(13)
    );
}

#[test]
fn the_default_page_context_attaches_a_snapshot_and_no_screenshot() {
    assert_eq!(PageContextMode::default(), PageContextMode::Snapshot);
    assert!(report_snapshot_requested(PageContextMode::default(), true));
    assert!(!report_screenshot_requested(
        None,
        PageContextMode::default(),
        false
    ));
}

#[test]
fn the_page_context_matrix_pairs_each_mode_with_the_page_changed_flag() {
    for (mode, page_changed, snapshot, screenshot) in [
        (PageContextMode::Snapshot, true, true, false),
        (PageContextMode::Snapshot, false, false, false),
        (PageContextMode::Screenshot, true, false, true),
        (PageContextMode::Screenshot, false, false, true),
        (PageContextMode::Both, true, true, true),
        (PageContextMode::Both, false, false, true),
        (PageContextMode::None, true, false, false),
        (PageContextMode::None, false, false, false),
    ] {
        assert_eq!(
            report_snapshot_requested(mode, page_changed),
            snapshot,
            "snapshot for {mode:?} page_changed={page_changed}"
        );
        assert_eq!(
            report_screenshot_requested(None, mode, false),
            screenshot,
            "screenshot for {mode:?} page_changed={page_changed}"
        );
    }
}

#[test]
fn a_navigation_alone_no_longer_triggers_a_screenshot() {
    for mode in [PageContextMode::Snapshot, PageContextMode::None] {
        assert!(!report_screenshot_requested(None, mode, false));
    }
}

#[test]
fn an_explicit_screenshot_step_still_attaches_the_report_screenshot() {
    assert!(report_screenshot_requested(
        None,
        PageContextMode::Snapshot,
        true
    ));
    assert!(!report_screenshot_requested(
        None,
        PageContextMode::None,
        true
    ));
}

#[test]
fn attach_screenshot_stays_the_authoritative_override_over_page_context() {
    for mode in [
        PageContextMode::Snapshot,
        PageContextMode::Screenshot,
        PageContextMode::Both,
        PageContextMode::None,
    ] {
        assert!(report_screenshot_requested(Some(true), mode, false));
        assert!(!report_screenshot_requested(Some(false), mode, true));
    }
}

#[test]
fn attach_screenshot_false_suppresses_only_the_report_screenshot() {
    assert!(!report_screenshot_requested(
        Some(false),
        PageContextMode::Screenshot,
        true
    ));
}

#[test]
fn console_counts_aggregate_levels_and_page_errors_without_the_text() {
    let entry = |level: &str| ConsoleEntry {
        timestamp: 0.0,
        level: level.to_string(),
        text: "secret detail".to_string(),
    };
    let counts = console_counts(
        &[
            entry("error"),
            entry("assert"),
            entry("warning"),
            entry("warn"),
            entry("info"),
            entry("log"),
        ],
        &["boom".to_string()],
    );

    assert_eq!(counts.errors, 3);
    assert_eq!(counts.warnings, 2);
    assert!(!serde_json::to_string(&counts).unwrap().contains("secret"));

    let cdp_debug_repr = console_counts(&[entry("Error"), entry("Warning"), entry("Log")], &[]);
    assert_eq!(cdp_debug_repr.errors, 1);
    assert_eq!(cdp_debug_repr.warnings, 1);
}

#[test]
fn console_levels_are_normalized_from_cdp_debug_names() {
    assert_eq!(refact_browser::console_level_label("Error"), "error");
    assert_eq!(refact_browser::console_level_label("Warning"), "warning");
    assert_eq!(refact_browser::console_level_label("Log"), "log");
    assert_eq!(
        refact_browser::console_level_label("StartGroup"),
        "start_group"
    );
    assert_eq!(
        refact_browser::console_level_label("page_error"),
        "page_error"
    );
}

#[test]
fn an_all_clear_console_leaves_the_page_block_out_of_the_envelope() {
    assert!(console_counts(&[], &[]).is_empty());
    assert_eq!(
        serde_json::to_value(BrowserPageContext::default()).unwrap(),
        serde_json::json!({})
    );
}

#[test]
fn only_a_non_2xx_main_document_status_is_surfaced() {
    let document = |url: &str, status: u16| NetworkEntry {
        url: url.to_string(),
        resource_type: "Document".to_string(),
        status: Some(status),
        ..NetworkEntry::default()
    };
    let asset = |status: u16| NetworkEntry {
        url: "https://example.com/app.js".to_string(),
        resource_type: "Script".to_string(),
        status: Some(status),
        ..NetworkEntry::default()
    };
    let url = Some("https://example.com/missing");

    assert_eq!(
        notable_main_document_status(&[document("https://example.com/missing", 404)], url),
        Some(404)
    );
    assert_eq!(
        notable_main_document_status(&[document("https://example.com/missing", 200)], url),
        None
    );
    assert_eq!(
        notable_main_document_status(&[document("https://example.com/missing", 204)], url),
        None
    );
    assert_eq!(
        notable_main_document_status(&[asset(500)], Some("https://example.com/app.js")),
        None,
        "a failing subresource is not the main document"
    );
    assert_eq!(notable_main_document_status(&[asset(500)], url), None);
    assert_eq!(notable_main_document_status(&[], url), None);
}

#[test]
fn a_followed_redirect_reports_the_final_document_status() {
    let entries = vec![
        NetworkEntry {
            url: "https://example.com/old".to_string(),
            resource_type: "Document".to_string(),
            status: Some(301),
            ..NetworkEntry::default()
        },
        NetworkEntry {
            url: "https://example.com/new".to_string(),
            resource_type: "Document".to_string(),
            status: Some(200),
            ..NetworkEntry::default()
        },
    ];

    assert_eq!(
        notable_main_document_status(&entries, Some("https://example.com/new")),
        None
    );
    assert_eq!(notable_main_document_status(&entries, None), None);
}

#[test]
fn a_small_snapshot_is_inlined_whole_without_an_artifact() {
    let directory = tempfile::tempdir().unwrap();
    let yaml = "- button \"Save\" [ref=e1]\n- link \"Home\" [ref=e2]".to_string();

    let snapshot = build_page_snapshot(yaml.clone(), directory.path()).unwrap();

    assert_eq!(snapshot.yaml, yaml);
    assert_eq!(snapshot.bytes, yaml.len());
    assert_eq!(snapshot.lines, 2);
    assert!(!snapshot.truncated);
    assert!(snapshot.artifact.is_none());
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
}

#[test]
fn an_oversize_snapshot_spills_to_an_artifact_and_inlines_only_a_head() {
    let settings = browser_settings::current();
    let directory = tempfile::tempdir().unwrap();
    let yaml = (1..=400)
        .map(|index| format!("- button \"Item {index}\" [ref=e{index}]"))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(yaml.len() > settings.capture.max_inline_snapshot_bytes);

    let snapshot = build_page_snapshot(yaml.clone(), directory.path()).unwrap();
    let artifact = snapshot.artifact.as_ref().unwrap();

    assert!(snapshot.truncated);
    assert_eq!(snapshot.bytes, yaml.len());
    assert_eq!(snapshot.lines, 400);
    assert_eq!(snapshot.yaml.lines().count(), SNAPSHOT_SUMMARY_LINES);
    assert!(snapshot.yaml.len() < yaml.len());
    assert!(yaml.starts_with(&snapshot.yaml));
    assert_eq!(artifact.kind, "aria_snapshot");
    assert_eq!(artifact.mime, "text/yaml");
    assert_eq!(artifact.bytes, yaml.len());
    assert_eq!(std::fs::read_to_string(&artifact.path).unwrap(), yaml);
    assert!(artifact.path.starts_with(directory.path()));
}

#[test]
fn the_inline_snapshot_budget_switches_exactly_at_the_cap() {
    let settings = browser_settings::current();
    let directory = tempfile::tempdir().unwrap();

    let at_cap = build_page_snapshot(
        "a".repeat(settings.capture.max_inline_snapshot_bytes),
        directory.path(),
    )
    .unwrap();
    let over_cap = build_page_snapshot(
        "a".repeat(settings.capture.max_inline_snapshot_bytes + 1),
        directory.path(),
    )
    .unwrap();

    assert!(at_cap.artifact.is_none());
    assert!(!at_cap.truncated);
    assert!(over_cap.artifact.is_some());
    assert!(over_cap.truncated);
}

#[test]
fn the_page_header_stays_far_below_the_envelope_budget() {
    let page = BrowserPageContext {
        status: Some(404),
        console: BrowserConsoleCounts {
            errors: 1,
            warnings: 0,
        },
        snapshot: None,
    };
    let report: ExecutionReport = serde_json::from_value(serde_json::json!({
        "ok": true,
        "steps": [],
        "url": "https://example.com/missing",
        "title": "Not Found",
        "page": page,
        "dialogs": [],
        "new_tabs": [],
    }))
    .unwrap();

    let envelope = serde_json::to_string(&report).unwrap();
    assert!(report.screenshot.is_none());
    assert!(
        envelope.len() <= 600,
        "envelope was {} chars",
        envelope.len()
    );
}

#[test]
fn matched_expect_attempts_exclude_the_first_attempt_from_retries() {
    assert_eq!(expect_retries(4), 3);
}

#[test]
fn timed_out_expect_attempts_exclude_the_first_attempt_from_retries() {
    assert_eq!(expect_retries(2), 1);
    assert_eq!(expect_retries(0), 0);
}

#[test]
fn negated_expect_retries_until_the_matcher_stops_matching() {
    let step = scripted_expectation(
        &BrowserExpectation::ToBeVisible,
        true,
        2_000,
        vec![
            Ok((true, Value::Bool(true))),
            Ok((true, Value::Bool(true))),
            Ok((false, Value::Bool(false))),
        ],
    );

    assert!(step.ok);
    let assertion = step.assertion.unwrap();
    assert_eq!(assertion.matcher, "not to_be_visible");
    assert!(assertion.passed);
    assert_eq!(assertion.attempts, 3);
    assert_eq!(step.retries, 2);
    assert_eq!(step.summary, "Assertion passed: not to_be_visible");
}

#[test]
fn negated_expect_timeout_reports_the_still_matching_state() {
    let step = scripted_expectation(
        &BrowserExpectation::ToHaveText {
            expected: BrowserExpectedTextOrList::One(BrowserExpectedText::Text(
                "Loading".to_string(),
            )),
            ignore_case: false,
        },
        true,
        60,
        vec![Ok((true, Value::String("Loading".to_string())))],
    );

    assert!(!step.ok);
    assert_eq!(step.summary, "Assertion failed: not to_have_text");
    let error = step.error.unwrap();
    assert!(
        error.starts_with("Expected not \"Loading\" but received \"Loading\""),
        "unexpected error: {error}"
    );
    assert!(error.contains("Timeout"), "unexpected error: {error}");
    let assertion = step.assertion.unwrap();
    assert_eq!(assertion.received, Value::String("Loading".to_string()));
    assert!(!assertion.passed);
}

#[test]
fn negation_leaves_the_positive_path_untouched() {
    let step = scripted_expectation(
        &BrowserExpectation::ToBeVisible,
        false,
        2_000,
        vec![Ok((true, Value::Bool(true)))],
    );

    assert!(step.ok);
    let assertion = step.assertion.unwrap();
    assert_eq!(assertion.matcher, "to_be_visible");
    assert_eq!(assertion.expected, Value::Bool(true));
    assert_eq!(assertion.attempts, 1);
}

#[test]
fn negated_expect_still_fails_hard_on_a_sampling_error() {
    let step = scripted_expectation(
        &BrowserExpectation::ToBeVisible,
        true,
        2_000,
        vec![Err("strict mode violation".to_string())],
    );

    assert!(!step.ok);
    assert!(
        step.error.unwrap().contains("strict mode violation"),
        "sampling errors must stay terminal under negation"
    );
}

#[test]
fn expectation_expected_values_describe_the_new_matcher_options() {
    assert_eq!(
        expectation_expected_value(&BrowserExpectation::ToBeChecked {
            checked: None,
            indeterminate: None
        }),
        Value::Bool(true)
    );
    assert_eq!(
        expectation_expected_value(&BrowserExpectation::ToBeChecked {
            checked: Some(false),
            indeterminate: None
        }),
        Value::Bool(false)
    );
    assert_eq!(
        expectation_expected_value(&BrowserExpectation::ToBeChecked {
            checked: None,
            indeterminate: Some(true)
        }),
        Value::String("indeterminate".to_string())
    );
    assert_eq!(
        expectation_expected_value(&BrowserExpectation::ToBeInViewport { ratio: None }),
        Value::Bool(true)
    );
    assert_eq!(
        expectation_expected_value(&BrowserExpectation::ToBeInViewport { ratio: Some(0.5) }),
        Value::from(0.5)
    );
    assert_eq!(
        expectation_expected_value(&BrowserExpectation::ToHaveAttribute {
            name: "data-ready".to_string(),
            expected: None,
            ignore_case: false
        }),
        Value::String("<present>".to_string())
    );
}

#[test]
fn viewport_ratio_needs_a_real_intersection_and_tolerates_float_drift() {
    assert!(viewport_ratio_matches(0.5, 0.5));
    assert!(viewport_ratio_matches(1.0, 0.5));
    assert!(!viewport_ratio_matches(0.49, 0.5));
    assert!(viewport_ratio_matches(0.1, 0.0));
    assert!(!viewport_ratio_matches(0.0, 0.0));
}

#[test]
fn text_list_expectations_pick_their_playwright_match_kind() {
    let have = BrowserExpectation::ToHaveText {
        expected: BrowserExpectedTextOrList::Many(vec![BrowserExpectedText::Text(
            "One".to_string(),
        )]),
        ignore_case: true,
    };
    let contain = BrowserExpectation::ToContainText {
        expected: BrowserExpectedTextOrList::Many(vec![BrowserExpectedText::Text(
            "One".to_string(),
        )]),
        ignore_case: false,
    };
    let single = BrowserExpectation::ToHaveText {
        expected: BrowserExpectedTextOrList::One(BrowserExpectedText::Text("One".to_string())),
        ignore_case: false,
    };

    let (expected, ignore_case, kind) = expectation_text_list(&have).unwrap();
    assert_eq!(expected.len(), 1);
    assert!(ignore_case);
    assert_eq!(kind, refact_browser::assertions::TextMatchKind::Exact);
    assert_eq!(
        expectation_text_list(&contain).unwrap().2,
        refact_browser::assertions::TextMatchKind::Contains
    );
    assert!(expectation_text_list(&single).is_none());
    assert!(have.is_multi_element());
    assert!(contain.is_multi_element());
    assert!(!single.is_multi_element());
}

#[test]
fn bounding_box_reports_viewport_css_pixels_for_a_visible_element() {
    let info = element_info(
        true,
        Some(ElementBBox {
            x: 12.5,
            y: 34.0,
            width: 200.0,
            height: 40.0,
        }),
    );

    assert_eq!(
        element_bounding_box(&info),
        serde_json::json!({"x": 12.5, "y": 34.0, "width": 200.0, "height": 40.0})
    );
}

#[test]
fn bounding_box_is_null_when_the_element_is_hidden_or_has_no_box() {
    let hidden = element_info(
        false,
        Some(ElementBBox {
            x: 0.0,
            y: 0.0,
            width: 0.0,
            height: 0.0,
        }),
    );

    assert_eq!(element_bounding_box(&hidden), Value::Null);
    assert_eq!(element_bounding_box(&element_info(true, None)), Value::Null);
}

#[test]
fn all_texts_limit_defaults_and_clamps_to_the_extraction_cap() {
    let settings = browser_settings::current();
    assert_eq!(all_texts_limit(None), settings.capture.default_all_texts);
    assert_eq!(all_texts_limit(Some(3)), 3);
    assert_eq!(all_texts_limit(Some(0)), 0);
    assert_eq!(all_texts_limit(Some(10_000)), MAX_ALL_TEXTS);
}

#[test]
fn input_value_reads_the_live_property_and_rejects_other_elements() {
    let script = browser_locators::js_input_value();

    assert!(script.contains("String(el.value)"));
    assert!(!script.contains("getAttribute"));
    assert!(script.contains("tag !== 'input' && tag !== 'textarea' && tag !== 'select'"));
}

#[test]
fn all_texts_mode_selects_inner_text_or_text_content() {
    assert!(
        browser_locators::js_element_text(BrowserTextMode::InnerText).contains("innerText")
    );
    assert!(
        browser_locators::js_element_text(BrowserTextMode::TextContent).contains("textContent")
    );
    assert!(
        !browser_locators::js_element_text(BrowserTextMode::TextContent).contains("innerText")
    );
}

#[test]
fn element_state_is_surfaced_read_only_with_every_tracked_flag() {
    let mut state = actionable_state();
    state.checked = Some(refact_browser::CheckedState::Mixed);

    assert_eq!(
        serde_json::json!({"state": state}),
        serde_json::json!({"state": {
            "visible": true,
            "enabled": true,
            "editable": true,
            "checked": "mixed",
            "stable": true,
        }})
    );
}

#[test]
fn extract_table_limit_truncates_rows_and_preserves_total() {
    let extracted = serde_json::json!({
        "ok": true,
        "rows": [["a"], ["b"], ["c"], ["d"], ["e"]],
        "total_rows": 87,
    });

    let truncated = truncate_table_rows(extracted, Some(3));

    assert_eq!(truncated["rows"], serde_json::json!([["a"], ["b"], ["c"]]));
    assert_eq!(truncated["total_rows"], serde_json::json!(87));
}

#[test]
fn extract_table_without_limit_keeps_every_extracted_row() {
    let extracted = serde_json::json!({
        "ok": true,
        "rows": [["a"], ["b"]],
        "total_rows": 2,
    });

    assert_eq!(truncate_table_rows(extracted.clone(), None), extracted);
}

#[test]
fn extract_table_limit_above_the_extraction_cap_is_clamped() {
    let settings = browser_settings::current();
    let rows = (0..settings.capture.max_extract_table_rows + 20)
        .map(|index| serde_json::json!([index.to_string()]))
        .collect::<Vec<_>>();
    let extracted = serde_json::json!({"ok": true, "rows": rows, "total_rows": 1_000});

    let truncated = truncate_table_rows(extracted, Some(1_000));

    assert_eq!(
        truncated["rows"].as_array().unwrap().len(),
        settings.capture.max_extract_table_rows
    );
    assert_eq!(truncated["total_rows"], serde_json::json!(1_000));
}

#[test]
fn navigation_wait_accepts_event_buffered_before_wait() {
    let (sender, receiver) = mpsc::channel();
    sender
        .send(NavigationEvent::Load {
            frame_id: "main".to_string(),
            loader_id: "loader".to_string(),
        })
        .unwrap();

    assert_eq!(
        wait_for_navigation_event(
            &receiver,
            &NavigationWaitTarget::Loader {
                frame_id: "main".to_string(),
                loader_id: Some("loader".to_string()),
            },
            Duration::ZERO,
        )
        .unwrap(),
        NavigationWaitOutcome::Completed
    );
}

#[test]
fn navigation_wait_timeout_reports_committed_navigation_and_missing_cdp_event() {
    let (_sender, receiver) = mpsc::channel();

    let outcome = wait_for_navigation_event(
        &receiver,
        &NavigationWaitTarget::Loader {
            frame_id: "main".to_string(),
            loader_id: Some("loader".to_string()),
        },
        Duration::ZERO,
    )
    .unwrap();

    let NavigationWaitOutcome::TimedOut {
        committed, message, ..
    } = outcome
    else {
        panic!("expected a navigation timeout outcome");
    };
    assert!(committed);
    assert!(message.contains("Timed out after 0ms"));
    assert!(message.contains("CDP Page.lifecycleEvent(load)"));
    assert!(message.contains("frame main"));
    assert!(message.contains("loader loader"));
}

#[test]
fn lifecycle_timeout_on_loaded_document_degrades_to_a_warning() {
    let complete = classify_navigation_timeout("complete", false, 5_000).unwrap();
    assert_eq!(
        complete.to_string(),
        "load event not observed within 5000ms; document.readyState=complete — continuing"
    );
    assert_eq!(
        classify_navigation_timeout("interactive", false, 5_000)
            .unwrap()
            .to_string(),
        "load event not observed within 5000ms; document.readyState=interactive — continuing"
    );
}

#[test]
fn lifecycle_timeout_on_committed_navigation_degrades_to_a_warning() {
    assert_eq!(
        classify_navigation_timeout("loading", true, 5_000)
            .unwrap()
            .to_string(),
        "load event not observed within 5000ms; document.readyState=loading — continuing"
    );
}

#[test]
fn lifecycle_timeout_without_commit_stays_a_failure() {
    assert!(classify_navigation_timeout("loading", false, 5_000).is_none());
    assert!(classify_navigation_timeout("unknown", false, 5_000).is_none());
}

#[test]
fn lifecycle_timeout_with_unreadable_document_is_never_a_soft_success() {
    assert!(classify_navigation_timeout("unknown", true, 5_000).is_none());
    assert!(classify_navigation_timeout("", true, 5_000).is_none());
}

#[test]
fn degraded_navigation_step_succeeds_and_carries_the_warning() {
    let warned = navigation_step_success(
        3,
        "Navigated to https://example.test".to_string(),
        classify_navigation_timeout("interactive", true, 5_000),
    );

    assert!(warned.ok);
    assert!(warned.error.is_none());
    assert_eq!(
        warned.summary,
        "Navigated to https://example.test (load event not observed within 5000ms; document.readyState=interactive — continuing)"
    );
    let data = warned.data.as_ref().unwrap();
    assert_eq!(data["load_event"], "not_observed");
    assert_eq!(data["ready_state"], "interactive");
    assert_eq!(data["waited_ms"], 5_000);
    let merged = merge_step_data(warned, serde_json::json!({"tab_id": "abc"}));
    assert_eq!(merged.data.as_ref().unwrap()["tab_id"], "abc");
    assert_eq!(merged.data.as_ref().unwrap()["ready_state"], "interactive");
    assert_eq!(
        navigation_step_success(3, "Navigated to https://example.test".to_string(), None)
            .summary,
        "Navigated to https://example.test"
    );
}

#[test]
fn same_document_navigation_needs_no_load_event() {
    let (_sender, receiver) = mpsc::channel();

    assert_eq!(
        wait_for_navigation_event(
            &receiver,
            &NavigationWaitTarget::Loader {
                frame_id: "main".to_string(),
                loader_id: None,
            },
            Duration::ZERO,
        )
        .unwrap(),
        NavigationWaitOutcome::Completed
    );
}

#[test]
fn triggered_navigation_accepts_load_buffered_before_commit() {
    let (sender, receiver) = mpsc::channel();
    sender
        .send(NavigationEvent::Load {
            frame_id: "main".to_string(),
            loader_id: "new-loader".to_string(),
        })
        .unwrap();
    sender
        .send(NavigationEvent::FrameNavigated {
            frame_id: "main".to_string(),
            loader_id: "new-loader".to_string(),
            url: "https://example.test/next".to_string(),
            restored_from_back_forward_cache: false,
        })
        .unwrap();

    assert_eq!(
        wait_for_navigation_event(
            &receiver,
            &NavigationWaitTarget::Triggered {
                frame_id: "main".to_string(),
                previous_loader_id: "old-loader".to_string(),
                expected_url: Some("https://example.test/next".to_string()),
                allow_same_document: true,
            },
            Duration::ZERO,
        )
        .unwrap(),
        NavigationWaitOutcome::Completed
    );
}

#[test]
fn triggered_navigation_waits_for_load_after_commit() {
    let (sender, receiver) = mpsc::channel();
    sender
        .send(NavigationEvent::FrameNavigated {
            frame_id: "main".to_string(),
            loader_id: "new-loader".to_string(),
            url: "https://example.test/next".to_string(),
            restored_from_back_forward_cache: false,
        })
        .unwrap();
    sender
        .send(NavigationEvent::Load {
            frame_id: "main".to_string(),
            loader_id: "new-loader".to_string(),
        })
        .unwrap();

    assert_eq!(
        wait_for_navigation_event(
            &receiver,
            &NavigationWaitTarget::Triggered {
                frame_id: "main".to_string(),
                previous_loader_id: "old-loader".to_string(),
                expected_url: Some("https://example.test/next".to_string()),
                allow_same_document: true,
            },
            Duration::ZERO,
        )
        .unwrap(),
        NavigationWaitOutcome::Completed
    );
}

#[test]
fn triggered_navigation_ignores_unrelated_frame_and_url() {
    let (sender, receiver) = mpsc::channel();
    sender
        .send(NavigationEvent::SameDocument {
            frame_id: "child".to_string(),
            url: "https://example.test/next".to_string(),
        })
        .unwrap();
    sender
        .send(NavigationEvent::SameDocument {
            frame_id: "main".to_string(),
            url: "https://example.test/unrelated".to_string(),
        })
        .unwrap();

    let outcome = wait_for_navigation_event(
        &receiver,
        &NavigationWaitTarget::Triggered {
            frame_id: "main".to_string(),
            previous_loader_id: "loader".to_string(),
            expected_url: Some("https://example.test/next".to_string()),
            allow_same_document: true,
        },
        Duration::ZERO,
    )
    .unwrap();

    let NavigationWaitOutcome::TimedOut {
        committed,
        expected_url,
        message,
    } = outcome
    else {
        panic!("expected a navigation timeout outcome");
    };
    assert!(!committed);
    assert_eq!(expected_url.as_deref(), Some("https://example.test/next"));
    assert!(message.contains("Page.frameNavigated or Page.navigatedWithinDocument"));
}

#[test]
fn triggered_same_document_navigation_needs_no_load_event() {
    let (sender, receiver) = mpsc::channel();
    sender
        .send(NavigationEvent::SameDocument {
            frame_id: "main".to_string(),
            url: "https://example.test/page#next".to_string(),
        })
        .unwrap();

    assert_eq!(
        wait_for_navigation_event(
            &receiver,
            &NavigationWaitTarget::Triggered {
                frame_id: "main".to_string(),
                previous_loader_id: "loader".to_string(),
                expected_url: Some("https://example.test/page#next".to_string()),
                allow_same_document: true,
            },
            Duration::ZERO,
        )
        .unwrap(),
        NavigationWaitOutcome::Completed
    );
}

#[test]
fn reload_ignores_same_document_events() {
    let (sender, receiver) = mpsc::channel();
    sender
        .send(NavigationEvent::SameDocument {
            frame_id: "main".to_string(),
            url: "https://example.test/page#noise".to_string(),
        })
        .unwrap();

    let outcome = wait_for_navigation_event(
        &receiver,
        &NavigationWaitTarget::Triggered {
            frame_id: "main".to_string(),
            previous_loader_id: "loader".to_string(),
            expected_url: None,
            allow_same_document: false,
        },
        Duration::ZERO,
    )
    .unwrap();

    let NavigationWaitOutcome::TimedOut {
        committed, message, ..
    } = outcome
    else {
        panic!("expected a navigation timeout outcome");
    };
    assert!(!committed);
    assert!(message.contains("Page.frameNavigated or Page.navigatedWithinDocument"));
}

#[test]
fn back_forward_cache_restore_needs_no_load_event() {
    let (sender, receiver) = mpsc::channel();
    sender
        .send(NavigationEvent::FrameNavigated {
            frame_id: "main".to_string(),
            loader_id: "restored-loader".to_string(),
            url: "https://example.test/back".to_string(),
            restored_from_back_forward_cache: true,
        })
        .unwrap();

    assert_eq!(
        wait_for_navigation_event(
            &receiver,
            &NavigationWaitTarget::Triggered {
                frame_id: "main".to_string(),
                previous_loader_id: "current-loader".to_string(),
                expected_url: Some("https://example.test/back".to_string()),
                allow_same_document: true,
            },
            Duration::ZERO,
        )
        .unwrap(),
        NavigationWaitOutcome::Completed
    );
}

#[test]
fn triggered_navigation_timeout_names_missing_commit_event() {
    let (_sender, receiver) = mpsc::channel();

    let outcome = wait_for_navigation_event(
        &receiver,
        &NavigationWaitTarget::Triggered {
            frame_id: "main".to_string(),
            previous_loader_id: "loader".to_string(),
            expected_url: None,
            allow_same_document: true,
        },
        Duration::ZERO,
    )
    .unwrap();

    let NavigationWaitOutcome::TimedOut {
        committed, message, ..
    } = outcome
    else {
        panic!("expected a navigation timeout outcome");
    };
    assert!(!committed);
    assert!(message.contains("Timed out after 0ms"));
    assert!(message.contains("Page.frameNavigated or Page.navigatedWithinDocument"));
    assert!(message.contains("frame main"));
}

#[test]
fn unavailable_history_direction_needs_no_navigation_event() {
    let (_sender, receiver) = mpsc::channel();

    assert_eq!(
        wait_for_navigation_event(
            &receiver,
            &NavigationWaitTarget::NoNavigation,
            Duration::ZERO,
        )
        .unwrap(),
        NavigationWaitOutcome::Completed
    );
}

#[test]
fn intercepting_preview_excludes_failure_suffix() {
    assert_eq!(
        intercepts_pointer_events("<div class=overlay> intercepts pointer events".to_string()),
        ActionabilityDiagnostic::InterceptsPointerEvents {
            description: "<div class=overlay>".to_string(),
        }
    );
}

