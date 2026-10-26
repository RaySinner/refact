use super::*;

use super::context::{clamp_timeout_ms, resolve_locator_handles, strict_locator_handle, strict_locator_previews};


pub(crate) fn expectation_label(matcher: &BrowserExpectation, negate: bool) -> String {
    if negate {
        format!("not {}", matcher.name())
    } else {
        matcher.name().to_string()
    }
}

pub(crate) fn step_expect(
    tab: &Tab,
    world: &WorldManager,
    idx: usize,
    locator: Option<&BrowserLocator>,
    matcher: &BrowserExpectation,
    timeout_ms: u64,
    soft: bool,
    negate: bool,
) -> StepResult {
    let label = expectation_label(matcher, negate);
    if matcher.requires_locator() && locator.is_none() {
        return StepResult::failure(
            idx,
            format!("Expect {label}"),
            "This matcher requires locator",
        );
    }
    if !matcher.requires_locator() && locator.is_some() {
        return StepResult::failure(
            idx,
            format!("Expect {label}"),
            "Page matchers do not accept locator",
        );
    }
    if let Err(error) = matcher.validate() {
        return StepResult::failure(idx, format!("Expect {label}"), error);
    }
    evaluate_expectation(
        idx,
        matcher,
        Duration::from_millis(clamp_timeout_ms(Some(timeout_ms))),
        soft,
        negate,
        || sample_expectation(tab, world, locator, matcher),
    )
}

pub(crate) fn evaluate_expectation(
    idx: usize,
    matcher: &BrowserExpectation,
    timeout: Duration,
    soft: bool,
    negate: bool,
    mut sample: impl FnMut() -> Result<(bool, Value), String>,
) -> StepResult {
    let label = expectation_label(matcher, negate);
    let expected = expectation_expected_value(matcher);
    let engine = ActionabilityEngine::new(SystemClock::default(), ActionabilityTimeouts::default());
    let result = engine.poll_expect(timeout, || {
        sample().map(|(matched, received)| (matched != negate, received))
    });
    let (passed, received, attempts, elapsed, terminal_error) = match result {
        ExpectPollResult::Matched {
            received,
            attempts,
            elapsed,
        } => (true, received, attempts, elapsed, None),
        ExpectPollResult::TimedOut {
            received,
            attempts,
            elapsed,
        } => (
            false,
            received.unwrap_or(Value::Null),
            attempts,
            elapsed,
            Some(format!("Timeout {}ms exceeded", timeout.as_millis())),
        ),
        ExpectPollResult::Failed {
            error,
            received,
            attempts,
            elapsed,
        } => (
            false,
            received.unwrap_or(Value::Null),
            attempts,
            elapsed,
            Some(error),
        ),
    };
    let diff = match matcher {
        BrowserExpectation::ToMatchAriaSnapshot { expected } if !passed && !negate => received
            .as_str()
            .map(|actual| refact_browser::assertions::aria_snapshot_diff(expected, actual)),
        _ => None,
    };
    let assertion = BrowserAssertionResult {
        matcher: label.clone(),
        passed,
        soft,
        expected: expected.clone(),
        received: received.clone(),
        diff,
        attempts,
        elapsed_ms: elapsed.as_millis().min(u128::from(u64::MAX)) as u64,
    };
    let mut step = if passed {
        StepResult::success(idx, format!("Assertion passed: {label}"))
    } else {
        let message = format!(
            "Expected {}{} but received {}{}",
            if negate { "not " } else { "" },
            json_for_message(&expected),
            json_for_message(&received),
            terminal_error
                .as_deref()
                .map(|error| format!(" ({error})"))
                .unwrap_or_default()
        );
        let summary = if soft {
            format!("Soft assertion failed: {label}")
        } else {
            format!("Assertion failed: {label}")
        };
        StepResult::failure(idx, summary, message)
    };
    step.retries = expect_retries(attempts);
    step.assertion = Some(assertion);
    step
}

pub(crate) fn expect_retries(attempts: u32) -> u32 {
    attempts.saturating_sub(1)
}

pub(crate) fn elapsed_ms(elapsed: Duration) -> u64 {
    elapsed.as_millis().min(u128::from(u64::MAX)) as u64
}

pub(crate) const POLL_VALUE_BINDING: &str = "__refact_poll_value";

pub(crate) fn page_poll_js(expression: &str) -> String {
    format!(
        "(() => {{ const {POLL_VALUE_BINDING} = ({expression}); return typeof {POLL_VALUE_BINDING} === 'function' ? {POLL_VALUE_BINDING}() : {POLL_VALUE_BINDING}; }})()"
    )
}

pub(crate) fn element_poll_js(expression: &str) -> String {
    format!(
        "function() {{ const {POLL_VALUE_BINDING} = ({expression}); return typeof {POLL_VALUE_BINDING} === 'function' ? {POLL_VALUE_BINDING}(this) : {POLL_VALUE_BINDING}; }}"
    )
}

pub(crate) fn js_exception_message(exception: &Runtime::ExceptionDetails) -> String {
    exception
        .exception
        .as_ref()
        .and_then(|value| value.description.as_deref())
        .unwrap_or(&exception.text)
        .to_string()
}

pub(crate) fn evaluate_page_poll(tab: &Tab, expression: &str) -> Result<Value, String> {
    let evaluated = tab
        .call_method(Runtime::Evaluate {
            expression: page_poll_js(expression),
            object_group: None,
            include_command_line_api: None,
            silent: None,
            context_id: None,
            return_by_value: Some(true),
            generate_preview: None,
            user_gesture: None,
            await_promise: Some(true),
            throw_on_side_effect: None,
            timeout: None,
            disable_breaks: None,
            repl_mode: None,
            allow_unsafe_eval_blocked_by_csp: None,
            unique_context_id: None,
            serialization_options: None,
        })
        .map_err(|error| format!("JS evaluation failed: {error}"))?;
    if let Some(exception) = evaluated.exception_details {
        return Err(js_exception_message(&exception));
    }
    Ok(evaluated.result.value.unwrap_or(Value::Null))
}

pub(crate) fn sample_poll_expression(
    tab: &Tab,
    world: &WorldManager,
    expression: &str,
    locator: Option<&BrowserLocator>,
) -> Result<Value, String> {
    let Some(locator) = locator else {
        return evaluate_page_poll(tab, expression);
    };
    let handles = match resolve_locator_handles(tab, world, locator) {
        Ok(handles) => handles,
        Err(error) if matches!(locator.strategy, LocatorStrategy::Ref { .. }) => return Err(error),
        Err(_) => Vec::new(),
    };
    let Some(handle) = strict_locator_handle(tab, world, locator, handles)? else {
        return Ok(Value::Null);
    };
    let handle = world.wrap_scoped(tab, handle);
    let value = world.call_function_on(tab, &handle, &element_poll_js(expression), Vec::new());
    match value {
        Ok(value) => Ok(value),
        Err(refact_browser::HandleError::Invalidated { .. }) => Ok(Value::Null),
        Err(error) => Err(error.to_string()),
    }
}

pub(crate) fn is_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::Number(number) => number.as_f64().is_some_and(|number| number != 0.0),
        Value::String(text) => !text.is_empty(),
        Value::Array(_) | Value::Object(_) => true,
    }
}

pub(crate) fn poll_backoff_schedule(polling_ms: Option<u64>) -> Vec<u64> {
    match polling_ms {
        Some(interval) => vec![0, interval],
        None => FUNCTION_POLL_BACKOFF_MS.to_vec(),
    }
}

pub(crate) fn json_equals(left: &Value, right: &Value) -> bool {
    match (left.as_f64(), right.as_f64()) {
        (Some(left), Some(right)) => left == right,
        _ => left == right,
    }
}

pub(crate) fn compare_poll_numbers(received: &Value, expected: &Value) -> Option<Ordering> {
    received.as_f64()?.partial_cmp(&expected.as_f64()?)
}

pub(crate) fn poll_expectation_regex(expected: &Value) -> Result<LocatorRegex, String> {
    match expected {
        Value::String(source) => Ok(LocatorRegex {
            source: source.clone(),
            flags: String::new(),
        }),
        Value::Object(_) => serde_json::from_value(expected.clone())
            .map_err(|error| format!("Invalid matches_regex expectation: {error}")),
        _ => Err("matches_regex expects a regex string or a {source, flags} object".to_string()),
    }
}

pub(crate) fn poll_matcher_matches(
    matcher: BrowserPollMatcher,
    received: &Value,
    expected: &Value,
) -> Result<bool, String> {
    match matcher {
        BrowserPollMatcher::Equals => Ok(json_equals(received, expected)),
        BrowserPollMatcher::Contains => Ok(match (received, expected) {
            (Value::String(received), Value::String(expected)) => received.contains(expected),
            (Value::Array(received), expected) => {
                received.iter().any(|item| json_equals(item, expected))
            }
            _ => false,
        }),
        BrowserPollMatcher::Gt => {
            Ok(compare_poll_numbers(received, expected).is_some_and(Ordering::is_gt))
        }
        BrowserPollMatcher::Lt => {
            Ok(compare_poll_numbers(received, expected).is_some_and(Ordering::is_lt))
        }
        BrowserPollMatcher::MatchesRegex => {
            let regex = poll_expectation_regex(expected)?;
            match received.as_str() {
                Some(received) => refact_browser::assertions::matches_text(
                    received,
                    &BrowserExpectedText::Regex(regex),
                    refact_browser::assertions::TextMatchKind::Exact,
                    false,
                ),
                None => Ok(false),
            }
        }
    }
}

pub(crate) fn redact_poll_value(value: Value) -> Value {
    match value {
        Value::String(text) => Value::String(refact_core::string_utils::redact_sensitive(&text)),
        Value::Array(values) => Value::Array(values.into_iter().map(redact_poll_value).collect()),
        Value::Object(entries) => Value::Object(
            entries
                .into_iter()
                .map(|(key, value)| (key, redact_poll_value(value)))
                .collect(),
        ),
        other => other,
    }
}

pub(crate) fn step_wait_for_function(
    tab: &Tab,
    world: &WorldManager,
    idx: usize,
    expression: &str,
    locator: Option<&BrowserLocator>,
    timeout_ms: u64,
    polling_ms: Option<u64>,
) -> StepResult {
    let engine = ActionabilityEngine::new(SystemClock::default(), ActionabilityTimeouts::default());
    let schedule = poll_backoff_schedule(polling_ms);
    let outcome =
        engine.poll_expect_with_backoff(Duration::from_millis(timeout_ms), &schedule, || {
            sample_poll_expression(tab, world, expression, locator)
                .map(|value| (is_truthy(&value), value))
        });
    let target = locator
        .map(|locator| format!(" ({})", describe_locator(locator)))
        .unwrap_or_default();
    let (received, attempts, elapsed, error) = match outcome {
        ExpectPollResult::Matched {
            received,
            attempts,
            elapsed,
        } => {
            let mut step = StepResult::success(idx, format!("Predicate satisfied{target}"))
                .with_data(serde_json::json!({
                    "value": redact_poll_value(received),
                    "attempts": attempts,
                    "elapsed_ms": elapsed_ms(elapsed),
                }));
            step.retries = expect_retries(attempts);
            return step;
        }
        ExpectPollResult::TimedOut {
            received,
            attempts,
            elapsed,
        } => (
            received.unwrap_or(Value::Null),
            attempts,
            elapsed,
            format!("Timeout {timeout_ms}ms exceeded"),
        ),
        ExpectPollResult::Failed {
            error,
            received,
            attempts,
            elapsed,
        } => (received.unwrap_or(Value::Null), attempts, elapsed, error),
    };
    let received = redact_poll_value(received);
    let mut step = StepResult::failure(
        idx,
        format!("Wait for function{target}"),
        format!("{error}; last value was {}", json_for_message(&received)),
    );
    step.retries = expect_retries(attempts);
    step.data = Some(serde_json::json!({
        "value": received,
        "attempts": attempts,
        "elapsed_ms": elapsed_ms(elapsed),
    }));
    step
}

pub(crate) fn step_expect_poll(
    tab: &Tab,
    idx: usize,
    expression: &str,
    expected: &Value,
    matcher: BrowserPollMatcher,
    timeout_ms: u64,
    soft: bool,
) -> StepResult {
    let engine = ActionabilityEngine::new(SystemClock::default(), ActionabilityTimeouts::default());
    let outcome = engine.poll_expect_with_backoff(
        Duration::from_millis(timeout_ms),
        FUNCTION_POLL_BACKOFF_MS,
        || {
            let received = evaluate_page_poll(tab, expression)?;
            let matched = poll_matcher_matches(matcher, &received, expected)?;
            Ok((matched, received))
        },
    );
    let (passed, received, attempts, elapsed, error) = match outcome {
        ExpectPollResult::Matched {
            received,
            attempts,
            elapsed,
        } => (true, received, attempts, elapsed, None),
        ExpectPollResult::TimedOut {
            received,
            attempts,
            elapsed,
        } => (
            false,
            received.unwrap_or(Value::Null),
            attempts,
            elapsed,
            Some(format!("Timeout {timeout_ms}ms exceeded")),
        ),
        ExpectPollResult::Failed {
            error,
            received,
            attempts,
            elapsed,
        } => (
            false,
            received.unwrap_or(Value::Null),
            attempts,
            elapsed,
            Some(error),
        ),
    };
    let received = redact_poll_value(received);
    let assertion = BrowserAssertionResult {
        matcher: matcher.name().to_string(),
        passed,
        soft,
        expected: expected.clone(),
        received: received.clone(),
        diff: None,
        attempts,
        elapsed_ms: elapsed_ms(elapsed),
    };
    let mut step = if passed {
        StepResult::success(idx, format!("Poll assertion passed: {}", matcher.name()))
    } else {
        let summary = if soft {
            format!("Soft poll assertion failed: {}", matcher.name())
        } else {
            format!("Poll assertion failed: {}", matcher.name())
        };
        StepResult::failure(
            idx,
            summary,
            format!(
                "Expected {} {} but received {}{}",
                matcher.name(),
                json_for_message(expected),
                json_for_message(&received),
                error
                    .as_deref()
                    .map(|error| format!(" ({error})"))
                    .unwrap_or_default()
            ),
        )
    };
    step.retries = expect_retries(attempts);
    step.assertion = Some(assertion);
    step
}

pub(crate) fn expectation_expected_value(matcher: &BrowserExpectation) -> Value {
    match matcher {
        BrowserExpectation::ToBeAttached
        | BrowserExpectation::ToBeVisible
        | BrowserExpectation::ToBeHidden
        | BrowserExpectation::ToBeEnabled
        | BrowserExpectation::ToBeDisabled
        | BrowserExpectation::ToBeEditable
        | BrowserExpectation::ToBeFocused
        | BrowserExpectation::ToBeEmpty => Value::Bool(true),
        BrowserExpectation::ToBeChecked {
            indeterminate: Some(true),
            ..
        } => Value::String("indeterminate".to_string()),
        BrowserExpectation::ToBeChecked { checked, .. } => Value::Bool(checked.unwrap_or(true)),
        BrowserExpectation::ToBeInViewport { ratio } => ratio
            .and_then(serde_json::Number::from_f64)
            .map(Value::Number)
            .unwrap_or(Value::Bool(true)),
        BrowserExpectation::ToHaveAttribute { expected: None, .. } => {
            Value::String("<present>".to_string())
        }
        BrowserExpectation::ToHaveText { expected, .. }
        | BrowserExpectation::ToContainText { expected, .. } => {
            serde_json::to_value(expected).unwrap_or(Value::Null)
        }
        BrowserExpectation::ToHaveValue { expected, .. }
        | BrowserExpectation::ToHaveClass { expected, .. }
        | BrowserExpectation::ToHaveId { expected, .. }
        | BrowserExpectation::ToHaveAccessibleName { expected, .. }
        | BrowserExpectation::ToHaveAccessibleDescription { expected, .. }
        | BrowserExpectation::ToHaveUrl { expected, .. }
        | BrowserExpectation::ToHaveTitle { expected, .. }
        | BrowserExpectation::ToHaveAttribute {
            expected: Some(expected),
            ..
        }
        | BrowserExpectation::ToHaveCss { expected, .. } => {
            serde_json::to_value(expected).unwrap_or(Value::Null)
        }
        BrowserExpectation::ToHaveValues { expected, .. } => {
            serde_json::to_value(expected).unwrap_or(Value::Null)
        }
        BrowserExpectation::ToContainClass { expected, .. }
        | BrowserExpectation::ToHaveRole { expected } => Value::String(expected.clone()),
        BrowserExpectation::ToHaveCount { expected } => Value::from(*expected),
        BrowserExpectation::ToHaveJsProperty { expected, .. } => expected.clone(),
        BrowserExpectation::ToMatchAriaSnapshot { expected } => Value::String(expected.clone()),
    }
}

pub(crate) fn sample_expectation(
    tab: &Tab,
    world: &WorldManager,
    locator: Option<&BrowserLocator>,
    matcher: &BrowserExpectation,
) -> Result<(bool, Value), String> {
    match matcher {
        BrowserExpectation::ToHaveUrl {
            expected,
            ignore_case,
        } => {
            let received = tab.get_url();
            let matches = refact_browser::assertions::matches_text(
                &received,
                expected,
                refact_browser::assertions::TextMatchKind::Exact,
                *ignore_case,
            )?;
            return Ok((matches, Value::String(received)));
        }
        BrowserExpectation::ToHaveTitle {
            expected,
            ignore_case,
        } => {
            let received = tab
                .evaluate("document.title", false)
                .map_err(|error| error.to_string())?
                .value
                .and_then(|value| value.as_str().map(str::to_string))
                .unwrap_or_default();
            let matches = refact_browser::assertions::matches_text(
                &received,
                expected,
                refact_browser::assertions::TextMatchKind::Exact,
                *ignore_case,
            )?;
            return Ok((matches, Value::String(received)));
        }
        _ => {}
    }

    let locator = locator.unwrap();
    let handles = match resolve_locator_handles(tab, world, locator) {
        Ok(handles) => handles,
        Err(error) if matches!(locator.strategy, LocatorStrategy::Ref { .. }) => return Err(error),
        Err(_) => Vec::new(),
    };
    let handles = world.wrap_scoped_all(tab, handles);
    if let BrowserExpectation::ToHaveCount { expected } = matcher {
        let count = handles.len();
        return Ok((count == *expected, Value::from(count)));
    }
    if let BrowserExpectation::ToBeHidden = matcher {
        if handles.is_empty() {
            return Ok((true, Value::String("detached".to_string())));
        }
    }
    if handles.len() > 1 && !matcher.is_multi_element() {
        let previews = strict_locator_previews(tab, world, handles.iter().map(|handle| &**handle));
        return Err(strict_expectation_error(matcher, locator, handles.len(), &previews).unwrap());
    }
    if let BrowserExpectation::ToHaveValues {
        expected,
        ignore_case,
    } = matcher
    {
        let mut received = Vec::new();
        for handle in &handles {
            let values = world
                .expectation_values(tab, handle)
                .map_err(|error| error.to_string())?;
            let value = values
                .get("value")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            received.push(value);
        }
        let matches = received.len() == expected.len()
            && received.iter().zip(expected).all(|(received, expected)| {
                refact_browser::assertions::matches_text(
                    received,
                    expected,
                    refact_browser::assertions::TextMatchKind::Exact,
                    *ignore_case,
                )
                .unwrap_or(false)
            });
        return Ok((matches, serde_json::to_value(received).unwrap_or_default()));
    }
    if let Some((expected, ignore_case, kind)) = expectation_text_list(matcher) {
        let mut received = Vec::new();
        for handle in &handles {
            let values = world
                .expectation_values(tab, handle)
                .map_err(|error| error.to_string())?;
            received.push(
                values
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            );
        }
        let matches =
            refact_browser::assertions::matches_text_list(&received, expected, kind, ignore_case)?;
        return Ok((matches, serde_json::to_value(received).unwrap_or_default()));
    }
    let Some(handle) = handles.first() else {
        return Ok((false, Value::String("detached".to_string())));
    };
    sample_single_element(tab, world, handle, matcher)
}

pub(crate) fn expectation_text_list(
    matcher: &BrowserExpectation,
) -> Option<(
    &[BrowserExpectedText],
    bool,
    refact_browser::assertions::TextMatchKind,
)> {
    match matcher {
        BrowserExpectation::ToHaveText {
            expected: BrowserExpectedTextOrList::Many(expected),
            ignore_case,
        } => Some((
            expected,
            *ignore_case,
            refact_browser::assertions::TextMatchKind::Exact,
        )),
        BrowserExpectation::ToContainText {
            expected: BrowserExpectedTextOrList::Many(expected),
            ignore_case,
        } => Some((
            expected,
            *ignore_case,
            refact_browser::assertions::TextMatchKind::Contains,
        )),
        _ => None,
    }
}

pub(crate) fn strict_expectation_error(
    matcher: &BrowserExpectation,
    locator: &BrowserLocator,
    count: usize,
    previews: &[String],
) -> Option<String> {
    (count > 1 && !matcher.is_multi_element())
        .then(|| refact_browser::strict_mode_violation(&describe_locator(locator), count, previews))
}

pub(crate) fn sample_single_element(
    tab: &Tab,
    world: &WorldManager,
    handle: &ElementHandle,
    matcher: &BrowserExpectation,
) -> Result<(bool, Value), String> {
    if let BrowserExpectation::ToMatchAriaSnapshot { expected } = matcher {
        let snapshot = world
            .aria_snapshot(tab, Some(handle.clone()), SnapshotOptions::default())
            .map_err(|error| error.to_string())?;
        let matches = refact_browser::assertions::match_aria_snapshot(expected, &snapshot.yaml)?;
        return Ok((matches, Value::String(snapshot.yaml)));
    }
    if let BrowserExpectation::ToHaveAttribute { name, .. }
    | BrowserExpectation::ToHaveCss { name, .. }
    | BrowserExpectation::ToHaveJsProperty { name, .. } = matcher
    {
        let (function, arguments) = match matcher {
            BrowserExpectation::ToHaveAttribute { .. } => (
                "function(name) { return this.getAttribute(name); }",
                vec![Value::String(name.clone())],
            ),
            BrowserExpectation::ToHaveCss { pseudo, .. } => (
                "function(name, pseudo) { return getComputedStyle(this, pseudo).getPropertyValue(name); }",
                vec![
                    Value::String(name.clone()),
                    pseudo
                        .map(|pseudo| Value::String(pseudo.selector().to_string()))
                        .unwrap_or(Value::Null),
                ],
            ),
            BrowserExpectation::ToHaveJsProperty { .. } => (
                "function(name) { return this[name]; }",
                vec![Value::String(name.clone())],
            ),
            _ => unreachable!(),
        };
        let received = world
            .call_function_on(tab, handle, function, arguments)
            .map_err(|error| error.to_string())?;
        let matches = match matcher {
            BrowserExpectation::ToHaveAttribute { expected: None, .. } => !received.is_null(),
            BrowserExpectation::ToHaveAttribute {
                expected: Some(expected),
                ignore_case,
                ..
            }
            | BrowserExpectation::ToHaveCss {
                expected,
                ignore_case,
                ..
            } => received.as_str().is_some_and(|received| {
                refact_browser::assertions::matches_text(
                    received,
                    expected,
                    refact_browser::assertions::TextMatchKind::Exact,
                    *ignore_case,
                )
                .unwrap_or(false)
            }),
            BrowserExpectation::ToHaveJsProperty { expected, .. } => {
                refact_browser::assertions::matches_json_property(&received, expected)
            }
            _ => false,
        };
        return Ok((matches, received));
    }
    let values = world
        .expectation_values(tab, handle)
        .map_err(|error| error.to_string())?;
    let field = |name: &str| values.get(name).cloned().unwrap_or(Value::Null);
    let boolean = |name: &str, expected: bool| {
        let received = field(name);
        (received == Value::Bool(expected), received)
    };
    let text = |name: &str,
                expected: &BrowserExpectedText,
                kind: refact_browser::assertions::TextMatchKind,
                ignore_case: bool|
     -> Result<(bool, Value), String> {
        let received = field(name);
        let value = received.as_str().unwrap_or_default();
        Ok((
            refact_browser::assertions::matches_text(value, expected, kind, ignore_case)?,
            received,
        ))
    };
    match matcher {
        BrowserExpectation::ToBeAttached => Ok(boolean("attached", true)),
        BrowserExpectation::ToBeVisible => Ok(boolean("visible", true)),
        BrowserExpectation::ToBeHidden => Ok(boolean("visible", false)),
        BrowserExpectation::ToBeEnabled => Ok(boolean("enabled", true)),
        BrowserExpectation::ToBeDisabled => Ok(boolean("enabled", false)),
        BrowserExpectation::ToBeEditable => Ok(boolean("editable", true)),
        BrowserExpectation::ToBeChecked {
            indeterminate: Some(true),
            ..
        } => Ok(boolean("indeterminate", true)),
        BrowserExpectation::ToBeChecked { checked, .. } => {
            Ok(boolean("checked", checked.unwrap_or(true)))
        }
        BrowserExpectation::ToBeFocused => Ok(boolean("focused", true)),
        BrowserExpectation::ToBeEmpty => Ok(boolean("empty", true)),
        BrowserExpectation::ToBeInViewport { ratio: None } => Ok(boolean("inViewport", true)),
        BrowserExpectation::ToBeInViewport { ratio: Some(ratio) } => {
            let received = field("viewportRatio");
            Ok((
                viewport_ratio_matches(received.as_f64().unwrap_or_default(), *ratio),
                received,
            ))
        }
        BrowserExpectation::ToHaveText {
            expected: BrowserExpectedTextOrList::One(expected),
            ignore_case,
        } => text(
            "text",
            expected,
            refact_browser::assertions::TextMatchKind::Exact,
            *ignore_case,
        ),
        BrowserExpectation::ToContainText {
            expected: BrowserExpectedTextOrList::One(expected),
            ignore_case,
        } => text(
            "text",
            expected,
            refact_browser::assertions::TextMatchKind::Contains,
            *ignore_case,
        ),
        BrowserExpectation::ToHaveValue {
            expected,
            ignore_case,
        } => text(
            "value",
            expected,
            refact_browser::assertions::TextMatchKind::Exact,
            *ignore_case,
        ),
        BrowserExpectation::ToHaveClass {
            expected,
            ignore_case,
        } => text(
            "class",
            expected,
            refact_browser::assertions::TextMatchKind::Exact,
            *ignore_case,
        ),
        BrowserExpectation::ToContainClass {
            expected,
            ignore_case,
        } => {
            let received = field("class");
            Ok((
                received.as_str().is_some_and(|received| {
                    refact_browser::assertions::matches_class_list(received, expected, *ignore_case)
                }),
                received,
            ))
        }
        BrowserExpectation::ToHaveId {
            expected,
            ignore_case,
        } => text(
            "id",
            expected,
            refact_browser::assertions::TextMatchKind::Exact,
            *ignore_case,
        ),
        BrowserExpectation::ToHaveRole { expected } => Ok((
            field("role").as_str() == Some(expected.as_str()),
            field("role"),
        )),
        BrowserExpectation::ToHaveAccessibleName {
            expected,
            ignore_case,
        } => text(
            "accessibleName",
            expected,
            refact_browser::assertions::TextMatchKind::Exact,
            *ignore_case,
        ),
        BrowserExpectation::ToHaveAccessibleDescription {
            expected,
            ignore_case,
        } => text(
            "accessibleDescription",
            expected,
            refact_browser::assertions::TextMatchKind::Exact,
            *ignore_case,
        ),
        _ => Err(format!("Unsupported element matcher: {}", matcher.name())),
    }
}

pub(crate) const VIEWPORT_RATIO_TOLERANCE: f64 = 1e-9;

pub(crate) fn viewport_ratio_matches(received: f64, ratio: f64) -> bool {
    if ratio > 0.0 {
        received + VIEWPORT_RATIO_TOLERANCE >= ratio
    } else {
        received > 0.0
    }
}

pub(crate) fn json_for_message(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "null".to_string())
}
