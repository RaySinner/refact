use std::collections::{BTreeSet, HashSet};

pub fn unknown_tool_names(declared: &[String], registered: &HashSet<String>) -> Vec<String> {
    declared
        .iter()
        .filter(|name| !registered.contains(name.as_str()))
        .cloned()
        .collect::<BTreeSet<String>>()
        .into_iter()
        .collect()
}

pub fn dropped_tool_names(base_tools: &[String], resolved_tools: &[String]) -> Vec<String> {
    let kept: HashSet<&str> = resolved_tools.iter().map(|name| name.as_str()).collect();
    base_tools
        .iter()
        .filter(|name| !kept.contains(name.as_str()))
        .cloned()
        .collect::<BTreeSet<String>>()
        .into_iter()
        .collect()
}
