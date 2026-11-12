use std::path::{Component, Path, PathBuf};

use crate::file_filter::is_refact_codegraph_path;

pub const WATCH_BLOCKLIST_PROBE_ROOT: &str = "__refact_watch_root__";
pub const WATCH_BLOCKLIST_PROBE_LEAF: &str = "__refact_watch_probe__";

pub fn path_has_hidden_component(path: &Path) -> bool {
    path.components().any(|component| {
        matches!(component, Component::Normal(name) if name.to_string_lossy().starts_with('.'))
    })
}

pub fn path_has_allowed_hidden_component(path: &Path) -> bool {
    let parts = path
        .components()
        .filter_map(|component| match component {
            Component::Normal(part) => Some(part.to_string_lossy().to_string()),
            _ => None,
        })
        .collect::<Vec<_>>();
    if parts.first().is_some_and(|part| part == ".config")
        && parts.get(1).is_some_and(|part| part == "refact")
    {
        return true;
    }
    path.components().any(|component| {
        matches!(component, Component::Normal(name) if name.to_string_lossy() == ".refact")
    })
}

pub fn path_for_blocklist(path: &Path, roots: &[PathBuf]) -> PathBuf {
    roots
        .iter()
        .filter_map(|root| path.strip_prefix(root).ok().map(|rel| (root, rel)))
        .max_by_key(|(root, _)| root.components().count())
        .map(|(_, rel)| rel.to_path_buf())
        .unwrap_or_else(|| path.to_path_buf())
}

pub fn path_is_refact_import_internal(path: &Path) -> bool {
    let mut last_was_refact = false;
    for component in path.components() {
        if last_was_refact && component == Component::Normal("imports".as_ref()) {
            return true;
        }
        last_was_refact = component == Component::Normal(".refact".as_ref());
    }
    false
}

pub const REFACT_RUNTIME_STATE_ENTRIES: &[&str] = &[
    "state.json",
    "runtime_queue.jsonl",
    "trajectories",
    "chats",
    "buddy",
];

pub const REFACT_RUNTIME_STATE_TOP_LEVEL_ENTRIES: &[&str] = &[
    "stats",
    "shell_audit.jsonl",
    "scheduled_tasks.json",
    "background_agents",
    "vecdb",
    "review_scratch",
    "visual_baselines",
];

pub fn refact_runtime_entry_matches(entries: &[&str], name: &str) -> bool {
    entries.iter().any(|entry| {
        name == *entry
            || name
                .strip_prefix(entry)
                .is_some_and(|rest| rest.starts_with('.'))
    })
}

pub fn path_is_refact_runtime_state(path: &Path) -> bool {
    let mut seen_refact = false;
    let mut directly_under_refact = false;
    for component in path.components() {
        let Component::Normal(name) = component else {
            continue;
        };
        let name = name.to_string_lossy();
        if seen_refact && refact_runtime_entry_matches(REFACT_RUNTIME_STATE_ENTRIES, &name) {
            return true;
        }
        if directly_under_refact
            && refact_runtime_entry_matches(REFACT_RUNTIME_STATE_TOP_LEVEL_ENTRIES, &name)
        {
            return true;
        }
        directly_under_refact = name == ".refact";
        if directly_under_refact {
            seen_refact = true;
        }
    }
    false
}

pub fn path_is_refact_internal(path: &Path) -> bool {
    path_is_refact_import_internal(path)
        || path_is_refact_runtime_state(path)
        || is_refact_codegraph_path(path)
}

pub fn path_triggers_registry_reload(path: &Path) -> bool {
    if path_is_refact_internal(path) {
        return false;
    }
    if !path
        .components()
        .any(|c| c == Component::Normal(".refact".as_ref()))
    {
        return false;
    }
    path.components().any(|c| {
        c == Component::Normal("modes".as_ref())
            || c == Component::Normal("subagents".as_ref())
            || c == Component::Normal("toolbox_commands".as_ref())
            || c == Component::Normal("code_lens".as_ref())
    })
}

pub fn path_triggers_tool_catalog_reload(path: &Path) -> bool {
    let is_integration = path
        .components()
        .any(|component| component == Component::Normal("integrations.d".as_ref()));
    let is_privacy = path.file_name().is_some_and(|name| name == "privacy.yaml");
    is_integration || is_privacy
}
