//! Frozen-request-prefix helpers shared by the engine trajectory store and the
//! session runtime. Pure functions only: they operate on `ChatSession` and
//! `ChatMessage` values, which both live in this crate's dependency set.

use refact_chat_api::{ChatMessage, ChatSession, FrozenRequestPrefix};
use refact_core::chat_types::ChatContent;

pub fn normalize_system_prompt(system_prompt: Option<String>) -> Option<String> {
    system_prompt.filter(|text| !text.trim().is_empty())
}

pub fn frozen_prefix_has_system_prompt(prefix: &FrozenRequestPrefix) -> bool {
    prefix
        .system_prompt
        .as_ref()
        .is_some_and(|text| !text.trim().is_empty())
}

pub fn frozen_prefix_has_tools(prefix: &FrozenRequestPrefix) -> bool {
    prefix.tools_canonical.is_some()
}

pub fn is_mode_transition_or_handoff(link_type: Option<&str>) -> bool {
    matches!(link_type, Some("mode_transition" | "handoff"))
}

pub fn frozen_prefix_is_complete(prefix: &FrozenRequestPrefix) -> bool {
    frozen_prefix_has_system_prompt(prefix) && frozen_prefix_has_tools(prefix)
}

pub fn first_system_prompt(messages: &[ChatMessage]) -> Option<String> {
    messages.iter().find_map(|message| {
        if message.role == "system" {
            match &message.content {
                ChatContent::SimpleText(text) if !text.trim().is_empty() => Some(text.clone()),
                _ => None,
            }
        } else {
            None
        }
    })
}

pub fn parsed_rfc3339_utc(value: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    chrono::DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|value| value.with_timezone(&chrono::Utc))
}

pub fn raw_frozen_prefix_has_tools(value: Option<&serde_json::Value>) -> bool {
    value
        .and_then(|value| value.get("tools_canonical"))
        .is_some_and(|value| !value.is_null())
}

pub fn legacy_incomplete_prefix_from_messages(
    messages: &[ChatMessage],
) -> Option<FrozenRequestPrefix> {
    first_system_prompt(messages)
        .map(|system_prompt| new_legacy_incomplete_frozen_request_prefix(Some(system_prompt)))
}

pub fn repair_transition_frozen_prefix(
    link_type: Option<&str>,
    messages: &[ChatMessage],
    frozen_request_prefix: Option<FrozenRequestPrefix>,
    raw_frozen_prefix_had_tools: bool,
    trajectory_created_at: Option<&str>,
    provider_state_present: bool,
) -> (Option<FrozenRequestPrefix>, bool) {
    let is_transition = is_mode_transition_or_handoff(link_type);
    let Some(prefix) = frozen_request_prefix else {
        let repaired = is_transition && (provider_state_present || raw_frozen_prefix_had_tools);
        return (legacy_incomplete_prefix_from_messages(messages), repaired);
    };
    if !is_transition {
        return (Some(prefix), false);
    }

    let first_system = first_system_prompt(messages);
    let system_mismatch = match (
        prefix
            .system_prompt
            .as_ref()
            .filter(|text| !text.trim().is_empty()),
        first_system.as_ref(),
    ) {
        (Some(frozen_system), Some(first_system)) => frozen_system != first_system,
        _ => false,
    };
    let missing_system_with_tools =
        !frozen_prefix_has_system_prompt(&prefix) && frozen_prefix_has_tools(&prefix);
    let prefix_created_at_invalid = parsed_rfc3339_utc(&prefix.created_at).is_none();
    let invalid_created_at_with_state =
        prefix_created_at_invalid && (provider_state_present || frozen_prefix_has_tools(&prefix));
    let trajectory_created_at_invalid = trajectory_created_at
        .map(|created_at| parsed_rfc3339_utc(created_at).is_none())
        .unwrap_or(true);
    let invalid_trajectory_created_at_with_state = trajectory_created_at_invalid
        && (provider_state_present || frozen_prefix_has_tools(&prefix));
    // Transition prefixes older than trajectory creation are copied by definition: new transition
    // snapshots start without a prefix, and target-chat lazy prefixes are created after trajectory creation.
    let prefix_created_before_trajectory = trajectory_created_at
        .and_then(parsed_rfc3339_utc)
        .zip(parsed_rfc3339_utc(&prefix.created_at))
        .is_some_and(|(trajectory_created_at, prefix_created_at)| {
            prefix_created_at < trajectory_created_at
        });

    if !(system_mismatch
        || prefix_created_before_trajectory
        || invalid_created_at_with_state
        || invalid_trajectory_created_at_with_state
        || missing_system_with_tools)
    {
        return (Some(prefix), false);
    }

    (
        first_system
            .map(|system_prompt| new_legacy_incomplete_frozen_request_prefix(Some(system_prompt))),
        true,
    )
}

pub fn new_frozen_request_prefix(
    system_prompt: Option<String>,
    tools_canonical: serde_json::Value,
) -> FrozenRequestPrefix {
    FrozenRequestPrefix {
        schema_version: 1,
        created_at: chrono::Utc::now().to_rfc3339(),
        system_prompt: normalize_system_prompt(system_prompt),
        tools_canonical: Some(tools_canonical),
    }
}

pub fn new_legacy_incomplete_frozen_request_prefix(
    system_prompt: Option<String>,
) -> FrozenRequestPrefix {
    FrozenRequestPrefix {
        schema_version: 1,
        created_at: chrono::Utc::now().to_rfc3339(),
        system_prompt: normalize_system_prompt(system_prompt),
        tools_canonical: None,
    }
}

pub fn ensure_frozen_prefix(
    session: &mut ChatSession,
    system_prompt: Option<String>,
    tools_canonical: Option<serde_json::Value>,
) -> Option<FrozenRequestPrefix> {
    let system_prompt = normalize_system_prompt(system_prompt);
    if system_prompt.is_none() && tools_canonical.is_none() {
        return None;
    }

    match session.thread.frozen_request_prefix.as_mut() {
        Some(prefix) => {
            if frozen_prefix_is_complete(prefix) {
                return None;
            }

            let mut changed = false;
            if !frozen_prefix_has_system_prompt(prefix) {
                if let Some(system_prompt) = system_prompt {
                    prefix.system_prompt = Some(system_prompt);
                    changed = true;
                }
            }
            if !frozen_prefix_has_tools(prefix) {
                if let Some(tools_canonical) = tools_canonical {
                    prefix.tools_canonical = Some(tools_canonical);
                    changed = true;
                }
            }
            if changed {
                let prefix = prefix.clone();
                session.increment_version();
                Some(prefix)
            } else {
                None
            }
        }
        None => {
            let prefix = match tools_canonical {
                Some(tools_canonical) => new_frozen_request_prefix(system_prompt, tools_canonical),
                None => new_legacy_incomplete_frozen_request_prefix(system_prompt),
            };
            session.thread.frozen_request_prefix = Some(prefix.clone());
            session.increment_version();
            Some(prefix)
        }
    }
}
