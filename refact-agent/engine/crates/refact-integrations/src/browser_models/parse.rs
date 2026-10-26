use serde::Deserialize;
use serde_json::Value;
use super::expectation::TabTarget;
use super::request::{BrowserActionRequest, NetworkReportMode, PageContextMode, SessionPolicy};
use super::step::BrowserStep;

const MAX_STEP_PARSE_ERROR_CHARS: usize = 400;
const MAX_ACTION_SUGGESTIONS: usize = 3;
const MIN_ACTION_SUGGESTION_SCORE: usize = 3;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BrowserActionRequestEnvelope {
    #[serde(default)]
    session: SessionPolicy,
    #[serde(default)]
    target: TabTarget,
    #[serde(default)]
    attach_screenshot: Option<bool>,
    #[serde(default)]
    page_context: Option<PageContextMode>,
    #[serde(default)]
    network: NetworkReportMode,
    #[serde(default)]
    block_service_workers: Option<bool>,
    #[serde(default)]
    continue_on_error: bool,
    steps: Vec<Value>,
}

pub fn parse_browser_action_request(value: Value) -> Result<BrowserActionRequest, String> {
    let envelope: BrowserActionRequestEnvelope =
        serde_json::from_value(value).map_err(|error| bounded_error_text(&error.to_string()))?;
    let mut steps = Vec::with_capacity(envelope.steps.len());
    for (index, raw_step) in envelope.steps.into_iter().enumerate() {
        steps.push(parse_browser_step(index, raw_step)?);
    }
    Ok(BrowserActionRequest {
        session: envelope.session,
        target: envelope.target,
        attach_screenshot: envelope.attach_screenshot,
        page_context: envelope.page_context,
        network: envelope.network,
        block_service_workers: envelope.block_service_workers,
        continue_on_error: envelope.continue_on_error,
        steps,
    })
}

fn parse_browser_step(index: usize, raw_step: Value) -> Result<BrowserStep, String> {
    let action = raw_step
        .get("action")
        .and_then(Value::as_str)
        .map(str::to_string);
    if let (Some(action), Some(object)) = (action.as_deref(), raw_step.as_object()) {
        if let Some(stray) = stray_field_on_fieldless_action(action, object) {
            return Err(format!(
                "step[{index}] ({action}): unknown field `{}`; `{action}` takes no fields",
                bounded_error_text(stray)
            ));
        }
    }
    serde_json::from_value::<BrowserStep>(raw_step).map_err(|error| match action {
        Some(action) if BrowserStep::ACTION_NAMES.contains(&action.as_str()) => format!(
            "step[{index}] ({action}): {}",
            bounded_error_text(&error.to_string())
        ),
        Some(action) => format!(
            "step[{index}]: unknown action '{}'{}",
            bounded_error_text(&action),
            format_action_suggestions(&action)
        ),
        None => format!("step[{index}]: {}", bounded_error_text(&error.to_string())),
    })
}

fn stray_field_on_fieldless_action<'a>(
    action: &str,
    object: &'a serde_json::Map<String, Value>,
) -> Option<&'a str> {
    let (_, required, optional, aliases) = BrowserStep::ACTION_FIELDS
        .iter()
        .find(|(name, ..)| *name == action)?;
    if !(required.is_empty() && optional.is_empty() && aliases.is_empty()) {
        return None;
    }
    object
        .keys()
        .find(|key| key.as_str() != "action")
        .map(String::as_str)
}

fn format_action_suggestions(action: &str) -> String {
    let lowered = action.to_ascii_lowercase().replace('-', "_");
    let max_distance = (lowered.len() / 3).max(2);
    let mut scored = BrowserStep::ACTION_NAMES
        .iter()
        .filter_map(|candidate| {
            let distance = edit_distance(&lowered, candidate);
            let prefix = shared_prefix_len(&lowered, candidate);
            let tier = if candidate.starts_with(&lowered) || lowered.starts_with(candidate) {
                0
            } else if distance <= max_distance {
                1
            } else if prefix >= MIN_ACTION_SUGGESTION_SCORE {
                2
            } else {
                return None;
            };
            Some((tier, distance, prefix, *candidate))
        })
        .collect::<Vec<_>>();
    scored.sort_by(|left, right| {
        left.0
            .cmp(&right.0)
            .then(left.1.cmp(&right.1))
            .then(right.2.cmp(&left.2))
            .then(left.3.cmp(right.3))
    });
    let names = scored
        .into_iter()
        .take(MAX_ACTION_SUGGESTIONS)
        .map(|(_, _, _, candidate)| candidate)
        .collect::<Vec<_>>();
    if names.is_empty() {
        String::new()
    } else {
        format!("; did you mean {}", names.join(", "))
    }
}

fn shared_prefix_len(left: &str, right: &str) -> usize {
    left.bytes()
        .zip(right.bytes())
        .take_while(|(left, right)| left == right)
        .count()
}

fn edit_distance(left: &str, right: &str) -> usize {
    let left: Vec<char> = left.chars().collect();
    let right: Vec<char> = right.chars().collect();
    let mut previous: Vec<usize> = (0..=right.len()).collect();
    let mut current = vec![0; right.len() + 1];
    for (i, left_char) in left.iter().enumerate() {
        current[0] = i + 1;
        for (j, right_char) in right.iter().enumerate() {
            let substitution = previous[j] + usize::from(left_char != right_char);
            current[j + 1] = substitution.min(previous[j + 1] + 1).min(current[j] + 1);
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[right.len()]
}

fn bounded_error_text(message: &str) -> String {
    let collapsed = refact_core::string_utils::redact_sensitive(message)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if collapsed.chars().count() <= MAX_STEP_PARSE_ERROR_CHARS {
        return collapsed;
    }
    let kept = collapsed
        .chars()
        .take(MAX_STEP_PARSE_ERROR_CHARS)
        .collect::<String>();
    format!("{kept}...")
}
