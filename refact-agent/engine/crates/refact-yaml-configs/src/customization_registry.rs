use std::path::{Path, PathBuf};
use crate::customization_types::*;
use crate::project_configs_bootstrap::{compute_checksum, get_default_checksum};

pub async fn load_registry_from_dir(dir: &Path) -> ProjectRegistry {
    let mut registry = ProjectRegistry::default();

    load_modes(&dir.join("modes"), &mut registry, false).await;
    load_subagents(&dir.join("subagents"), &mut registry, false).await;
    load_toolbox_commands(&dir.join("toolbox_commands"), &mut registry).await;
    load_code_lens(&dir.join("code_lens"), &mut registry).await;

    registry
}

const BUILTIN_SUBAGENT_TOOL_IDS: &[&str] = &[
    "subagent",
    "agent_list",
    "agent_status",
    "agent_wait",
    "agent_result",
    "agent_cancel",
];

pub fn is_builtin_subagent_tool_id(id: &str) -> bool {
    BUILTIN_SUBAGENT_TOOL_IDS.contains(&id)
}

pub fn should_expose_subagent_as_config_tool(config: &SubagentConfig) -> bool {
    config.expose_as_tool && !config.has_code && !is_builtin_subagent_tool_id(&config.id)
}

pub async fn load_merged_registry(global_dir: &Path, local_dir: Option<&Path>) -> ProjectRegistry {
    let global_registry = load_registry_from_dir(global_dir).await;

    let local_registry = match local_dir {
        Some(dir) => {
            let refact_dir = dir.join(".refact");
            load_local_registry_skip_defaults(&refact_dir).await
        }
        None => ProjectRegistry::default(),
    };

    merge_registries(global_registry, local_registry)
}

async fn load_local_registry_skip_defaults(refact_dir: &Path) -> ProjectRegistry {
    let mut registry = ProjectRegistry::default();

    load_modes_skip_defaults(&refact_dir.join("modes"), &mut registry).await;
    load_subagents_skip_defaults(&refact_dir.join("subagents"), &mut registry).await;
    load_toolbox_commands_skip_defaults(&refact_dir.join("toolbox_commands"), &mut registry).await;
    load_code_lens_skip_defaults(&refact_dir.join("code_lens"), &mut registry).await;

    registry
}

pub fn merge_registries(global: ProjectRegistry, local: ProjectRegistry) -> ProjectRegistry {
    let mut merged = global;

    for (id, config) in local.modes {
        merged.modes.insert(id, config);
    }

    let mut all_overrides = local.mode_overrides;
    all_overrides.extend(merged.mode_overrides);
    merged.mode_overrides = all_overrides;

    for (id, config) in local.subagents {
        merged.subagents.insert(id, config);
    }

    let mut all_subagent_overrides = local.subagent_overrides;
    all_subagent_overrides.extend(merged.subagent_overrides);
    merged.subagent_overrides = all_subagent_overrides;

    for (id, config) in local.toolbox_commands {
        merged.toolbox_commands.insert(id, config);
    }

    for (id, config) in local.code_lens {
        merged.code_lens.insert(id, config);
    }

    merged.errors.extend(local.errors);

    merged
}

async fn load_modes(dir: &Path, registry: &mut ProjectRegistry, skip_defaults: bool) {
    let paths = collect_yaml_paths(dir).await;

    for path in paths {
        if skip_defaults && is_unchanged_default(&path, "modes").await {
            continue;
        }
        match load_yaml_file::<ModeConfig>(&path).await {
            Ok(config) => {
                if config.base.is_some() && config.match_models.is_some() {
                    registry.mode_overrides.push(config);
                } else if registry.modes.contains_key(&config.id) {
                    registry.errors.push(RegistryError {
                        file_path: path.display().to_string(),
                        error: format!("duplicate mode id '{}'", config.id),
                    });
                } else {
                    registry.modes.insert(config.id.clone(), config);
                }
            }
            Err(e) => {
                registry.errors.push(RegistryError {
                    file_path: path.display().to_string(),
                    error: e,
                });
            }
        }
    }
}

async fn load_modes_skip_defaults(dir: &Path, registry: &mut ProjectRegistry) {
    load_modes(dir, registry, true).await;
}

async fn load_subagents(dir: &Path, registry: &mut ProjectRegistry, skip_defaults: bool) {
    let paths = collect_yaml_paths(dir).await;

    for path in paths {
        if skip_defaults && is_unchanged_default(&path, "subagents").await {
            continue;
        }
        match load_yaml_file::<SubagentConfig>(&path).await {
            Ok(config) => {
                if config.base.is_some() && config.match_models.is_some() {
                    registry.subagent_overrides.push(config);
                } else if registry.subagents.contains_key(&config.id) {
                    registry.errors.push(RegistryError {
                        file_path: path.display().to_string(),
                        error: format!("duplicate subagent id '{}'", config.id),
                    });
                } else {
                    registry.subagents.insert(config.id.clone(), config);
                }
            }
            Err(e) => {
                registry.errors.push(RegistryError {
                    file_path: path.display().to_string(),
                    error: e,
                });
            }
        }
    }
}

async fn load_subagents_skip_defaults(dir: &Path, registry: &mut ProjectRegistry) {
    load_subagents(dir, registry, true).await;
}

async fn load_toolbox_commands(dir: &Path, registry: &mut ProjectRegistry) {
    load_toolbox_commands_inner(dir, registry, false).await;
}

async fn load_toolbox_commands_skip_defaults(dir: &Path, registry: &mut ProjectRegistry) {
    load_toolbox_commands_inner(dir, registry, true).await;
}

async fn load_toolbox_commands_inner(
    dir: &Path,
    registry: &mut ProjectRegistry,
    skip_defaults: bool,
) {
    let paths = collect_yaml_paths(dir).await;

    for path in paths {
        if skip_defaults && is_unchanged_default(&path, "toolbox_commands").await {
            continue;
        }
        match load_yaml_file::<ToolboxCommandConfig>(&path).await {
            Ok(config) => {
                if registry.toolbox_commands.contains_key(&config.id) {
                    registry.errors.push(RegistryError {
                        file_path: path.display().to_string(),
                        error: format!("duplicate toolbox_command id '{}'", config.id),
                    });
                } else {
                    registry.toolbox_commands.insert(config.id.clone(), config);
                }
            }
            Err(e) => {
                registry.errors.push(RegistryError {
                    file_path: path.display().to_string(),
                    error: e,
                });
            }
        }
    }
}

async fn load_code_lens(dir: &Path, registry: &mut ProjectRegistry) {
    load_code_lens_inner(dir, registry, false).await;
}

async fn load_code_lens_skip_defaults(dir: &Path, registry: &mut ProjectRegistry) {
    load_code_lens_inner(dir, registry, true).await;
}

async fn load_code_lens_inner(dir: &Path, registry: &mut ProjectRegistry, skip_defaults: bool) {
    let paths = collect_yaml_paths(dir).await;

    for path in paths {
        if skip_defaults && is_unchanged_default(&path, "code_lens").await {
            continue;
        }
        match load_yaml_file::<CodeLensConfig>(&path).await {
            Ok(config) => {
                if registry.code_lens.contains_key(&config.id) {
                    registry.errors.push(RegistryError {
                        file_path: path.display().to_string(),
                        error: format!("duplicate code_lens id '{}'", config.id),
                    });
                } else {
                    registry.code_lens.insert(config.id.clone(), config);
                }
            }
            Err(e) => {
                registry.errors.push(RegistryError {
                    file_path: path.display().to_string(),
                    error: e,
                });
            }
        }
    }
}

async fn collect_yaml_paths(dir: &Path) -> Vec<PathBuf> {
    let mut entries = match tokio::fs::read_dir(dir).await {
        Ok(e) => e,
        Err(_) => return vec![],
    };

    let mut paths = Vec::new();
    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        if path
            .extension()
            .map(|e| e == "yaml" || e == "yml")
            .unwrap_or(false)
        {
            let effectively_empty = tokio::fs::read_to_string(&path)
                .await
                .map(|c| c.trim().is_empty())
                .unwrap_or(false);
            if !effectively_empty {
                paths.push(path);
            }
        }
    }
    paths.sort();
    paths
}

async fn load_yaml_file<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, String> {
    let content = tokio::fs::read_to_string(path)
        .await
        .map_err(|e| format!("Failed to read file: {}", e))?;
    serde_yaml::from_str(&content).map_err(|e| format!("Failed to parse YAML: {}", e))
}

async fn is_unchanged_default(path: &Path, kind: &str) -> bool {
    let filename = match path.file_name().and_then(|f| f.to_str()) {
        Some(f) => f,
        None => return false,
    };

    let default_checksum = match get_default_checksum(kind, filename) {
        Some(c) => c,
        None => return false,
    };

    let content = match tokio::fs::read_to_string(path).await {
        Ok(c) => c,
        Err(_) => return false,
    };

    compute_checksum(&content) == default_checksum
}

fn select_mode_override<'a>(
    registry: &'a ProjectRegistry,
    mode_id: &str,
    model_id: &str,
) -> Option<&'a ModeConfig> {
    registry
        .mode_overrides
        .iter()
        .filter(|o| o.base.as_deref() == Some(mode_id))
        .filter_map(|o| {
            o.match_models
                .as_ref()
                .and_then(|patterns| best_matching_pattern_specificity(model_id, patterns))
                .map(|specificity| (specificity, o))
        })
        .fold(None, best_override_by_specificity)
        .map(|(_, o)| o)
}

pub fn mode_override_tool_drift(
    registry: &ProjectRegistry,
    mode_id: &str,
    model_id: Option<&str>,
) -> Option<(String, Vec<String>)> {
    let base = registry.modes.get(mode_id)?;
    let overlay = select_mode_override(registry, mode_id, model_id?)?;
    let override_config = overlay.override_config.as_ref()?;
    override_config.tools_replace.as_ref()?;

    let resolved = base.apply_override(override_config);
    let dropped = crate::mode_validation::dropped_tool_names(&base.tools, &resolved.tools);
    if dropped.is_empty() {
        None
    } else {
        Some((overlay.id.clone(), dropped))
    }
}

pub fn resolve_mode_for_model(
    registry: &ProjectRegistry,
    mode_id: &str,
    model_id: Option<&str>,
) -> Option<ModeConfig> {
    let base = registry.modes.get(mode_id)?;

    let model_id = match model_id {
        Some(m) => m,
        None => return Some(base.clone()),
    };

    let matching_override = select_mode_override(registry, mode_id, model_id);

    match matching_override {
        Some(override_config) => {
            if let Some(ref ov) = override_config.override_config {
                Some(base.apply_override(ov))
            } else {
                Some(base.clone())
            }
        }
        None => Some(base.clone()),
    }
}

pub fn resolve_subagent_for_model(
    registry: &ProjectRegistry,
    subagent_id: &str,
    model_id: Option<&str>,
) -> Option<SubagentConfig> {
    let base = registry.subagents.get(subagent_id)?;

    let model_id = match model_id {
        Some(m) => m,
        None => return Some(base.clone()),
    };

    let matching_override = registry
        .subagent_overrides
        .iter()
        .filter(|o| o.base.as_deref() == Some(subagent_id))
        .filter_map(|o| {
            o.match_models
                .as_ref()
                .and_then(|patterns| best_matching_pattern_specificity(model_id, patterns))
                .map(|specificity| (specificity, o))
        })
        .fold(None, best_override_by_specificity)
        .map(|(_, o)| o);

    match matching_override {
        Some(override_config) => Some(base.apply_override(override_config)),
        None => Some(base.clone()),
    }
}

pub fn model_matches_pattern(model_id: &str, pattern: &str) -> bool {
    let canonical = refact_core::model_caps::canonicalize_model_name(model_id);
    let candidates = [
        canonical.original.as_str(),
        canonical.provider_stripped.as_str(),
        canonical.base_model.as_str(),
        canonical.last_segment.as_str(),
        canonical.last_segment_base.as_str(),
    ];

    candidates
        .iter()
        .any(|c| model_matches_pattern_single(c, pattern))
        || {
            let pattern_norm = normalize_model_match_str(pattern);
            candidates
                .iter()
                .any(|c| model_matches_pattern_single(&normalize_model_match_str(c), &pattern_norm))
        }
}

fn normalize_model_match_str(s: &str) -> String {
    s.to_lowercase().replace('.', "-")
}

fn model_matches_pattern_single(model_id: &str, pattern: &str) -> bool {
    if !pattern.contains('*') {
        return model_id == pattern;
    }

    let segments: Vec<_> = pattern
        .split('*')
        .filter(|segment| !segment.is_empty())
        .collect();
    if segments.is_empty() {
        return true;
    }

    let mut remaining = model_id;
    for (index, segment) in segments.iter().enumerate() {
        if index == 0 && !pattern.starts_with('*') {
            if !remaining.starts_with(segment) {
                return false;
            }
            remaining = &remaining[segment.len()..];
        } else if index == segments.len() - 1 && !pattern.ends_with('*') {
            return remaining.ends_with(segment);
        } else if let Some(segment_pos) = remaining.find(segment) {
            remaining = &remaining[segment_pos + segment.len()..];
        } else {
            return false;
        }
    }

    true
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ModelPatternSpecificity {
    exact: bool,
    wildcard_rank: u8,
    literal_chars: usize,
}

impl ModelPatternSpecificity {
    fn for_pattern(pattern: &str) -> Self {
        let exact = !pattern.contains('*');
        Self {
            exact,
            wildcard_rank: if exact {
                0
            } else {
                wildcard_specificity_rank(pattern)
            },
            literal_chars: pattern.chars().filter(|c| *c != '*').count(),
        }
    }

    fn is_more_specific_than(self, other: Self) -> bool {
        if self.exact && other.exact {
            false
        } else if self.exact != other.exact {
            self.exact
        } else if self.wildcard_rank != other.wildcard_rank {
            self.wildcard_rank > other.wildcard_rank
        } else {
            self.literal_chars > other.literal_chars
        }
    }
}

fn wildcard_specificity_rank(pattern: &str) -> u8 {
    if is_specific_wildcard(pattern) {
        3
    } else if is_contains_wildcard(pattern) {
        2
    } else {
        1
    }
}

fn is_specific_wildcard(pattern: &str) -> bool {
    let literal = pattern.replace('*', "");
    literal
        .chars()
        .any(|c| c.is_ascii_digit() || c == '.' || c == '/' || c == '_')
}

fn is_contains_wildcard(pattern: &str) -> bool {
    pattern.starts_with('*') && pattern.ends_with('*') && pattern.chars().any(|c| c != '*')
}

fn best_matching_pattern_specificity(
    model_id: &str,
    patterns: &[String],
) -> Option<ModelPatternSpecificity> {
    patterns
        .iter()
        .filter(|pattern| model_matches_pattern(model_id, pattern))
        .map(|pattern| ModelPatternSpecificity::for_pattern(pattern))
        .fold(None, |best, specificity| match best {
            Some(best) if !specificity.is_more_specific_than(best) => Some(best),
            _ => Some(specificity),
        })
}

fn best_override_by_specificity<'a, T>(
    best: Option<(ModelPatternSpecificity, &'a T)>,
    candidate: (ModelPatternSpecificity, &'a T),
) -> Option<(ModelPatternSpecificity, &'a T)> {
    match best {
        Some(best) if !candidate.0.is_more_specific_than(best.0) => Some(best),
        _ => Some(candidate),
    }
}
