use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};

use refact_core::chat_types::{normalize_file_name, ChatContent, ChatMessage};
use refact_core::diff_chunk::DiffChunk;
use refact_core::privacy_types::PrivacySettings;
use refact_exec::path_enrichment::{CollectedPathEnrichment, PathEnrichment};
use refact_exec::ObservationStatus;
use refact_files::file_index::FileIndex;
use refact_worktrees::path_mappings::{
    registered_alias_paths, registered_worktree_path_mappings, RegisteredWorktreePathMapping,
};

use crate::derived::{new_derived_privacy_zones, DerivedPrivacyZones, DerivedZone};
use crate::destination::Destination;
use crate::heuristic::attribute_shell_command;
use crate::legacy::check_file_privacy_for_read_preparation;
use crate::matching::CompiledPolicy;
use crate::message_records::merge_records;
use crate::policy::{PolicyLoad, PrivacyPolicy, ShellBehavior};
use crate::record::{Attribution, FileRecord};
use crate::runtime_state::{observation_runtime_state, PrivacyObservationRuntimeState};

pub const SHELL_WITHHELD_MESSAGE: &str = "Output withheld by user privacy policy — the command ran, but its output read guarded files and cannot be shown.";
pub const SHELL_APPROVAL_MESSAGE: &str = "Output awaiting user approval — this command read guarded files.";

const INHERITED_ZONE_NOTE: &str = "\nEntries marked inherited carry a session-derived label taken from an earlier guarded read in this chat rather than from a configured rule, and they clear when the session ends or via POST /v1/privacy/derived/clear.";

const MAX_LISTED_GUARDED_READS: usize = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellReadDecision {
    Pass,
    Ask,
}

#[derive(Clone)]
pub struct PrivacyRecordScope {
    pub privacy_policy_load: Arc<RwLock<PolicyLoad>>,
    pub privacy_settings: Arc<RwLock<Arc<PrivacySettings>>>,
    pub privacy_observation_runtime: Arc<RwLock<PrivacyObservationRuntimeState>>,
    pub file_index: Arc<FileIndex>,
    pub workspace_folders: Arc<Mutex<Vec<PathBuf>>>,
    pub cache_dir: PathBuf,
}

impl PrivacyRecordScope {
    fn policy(&self) -> Arc<PrivacyPolicy> {
        self.privacy_policy_load.read().unwrap().policy.clone()
    }

    fn workspace_folders(&self) -> Vec<PathBuf> {
        self.workspace_folders.lock().unwrap().clone()
    }
}

pub async fn filter_path_enrichment_for_model_context_with_scope(
    scope: &PrivacyRecordScope,
    destination: &Destination,
    derived_zones: &DerivedPrivacyZones,
    collected: CollectedPathEnrichment,
) -> PathEnrichment {
    let mut metadata = collected.metadata;
    let mut references = Vec::new();
    let mut seen = HashSet::new();
    let compiled_policy = scope.policy().compile().ok();
    let worktree_mappings = registered_worktree_path_mappings(scope.cache_dir.as_path());
    let privacy_settings = scope.privacy_settings.read().unwrap().clone();
    for candidate in collected.candidates {
        if check_read_preparation(
            &privacy_settings,
            &worktree_mappings,
            &candidate.canonical_path,
        )
        .is_err()
            || !compiled_policy.as_ref().is_some_and(|compiled| {
                path_allowed_for_destination(
                    scope,
                    compiled,
                    &worktree_mappings,
                    &candidate.canonical_path,
                    destination,
                    derived_zones,
                )
            })
        {
            metadata.withheld_count += 1;
            continue;
        }
        let key = (
            candidate.reference.path.clone(),
            candidate.reference.line1,
            candidate.reference.line2,
            candidate.reference.column1,
            candidate.reference.column2,
        );
        if seen.insert(key) {
            references.push(candidate.reference);
        }
    }
    metadata.references = references;
    metadata
}

fn check_read_preparation(
    privacy_settings: &Arc<PrivacySettings>,
    mappings: &[RegisteredWorktreePathMapping],
    path: &Path,
) -> Result<(), String> {
    for alias in registered_alias_paths(path, mappings) {
        check_file_privacy_for_read_preparation(privacy_settings.clone(), &alias)?;
    }
    Ok(())
}

fn path_allowed_for_destination(
    scope: &PrivacyRecordScope,
    compiled: &CompiledPolicy,
    mappings: &[RegisteredWorktreePathMapping],
    path: &Path,
    destination: &Destination,
    derived_zones: &DerivedPrivacyZones,
) -> bool {
    let zone = zone_for_record_path(scope, compiled, path, mappings, derived_zones);
    zone != "blocked"
        && compiled
            .zone_named(&zone)
            .map(|candidate| destination.matches_send_to(&candidate.send_to))
            .unwrap_or(false)
}

fn zone_for_record_path(
    scope: &PrivacyRecordScope,
    compiled: &CompiledPolicy,
    path: &Path,
    mappings: &[RegisteredWorktreePathMapping],
    derived_zones: &DerivedPrivacyZones,
) -> String {
    let candidates = record_path_candidates(scope, path, mappings);
    let static_zone =
        compiled.strictest_zone_for_paths_with_roots(&candidates, privacy_roots(scope, mappings));
    let derived_zone_name = {
        let derived_zones = derived_zones
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        strictest_named_zone(
            compiled,
            candidates
                .iter()
                .filter_map(|candidate| derived_zones.get(candidate))
                .map(|derived| derived.zone.as_str()),
        )
        .map(str::to_string)
    };
    let Some(derived_zone_name) = derived_zone_name else {
        return static_zone.name.clone();
    };
    if compare_zone_strictness(
        compiled,
        &derived_zone_name,
        zone_destinations(compiled, &derived_zone_name),
        &static_zone.name,
        &static_zone.send_to,
    ) == Ordering::Less
    {
        derived_zone_name
    } else {
        static_zone.name.clone()
    }
}

pub fn derived_zone_for_path(
    scope: &PrivacyRecordScope,
    path: &Path,
    derived_zones: &DerivedPrivacyZones,
) -> Option<DerivedZone> {
    let mappings = registered_worktree_path_mappings(scope.cache_dir.as_path());
    derived_zone_for_path_with(scope, path, &mappings, derived_zones)
}

fn derived_zone_for_path_with(
    scope: &PrivacyRecordScope,
    path: &Path,
    mappings: &[RegisteredWorktreePathMapping],
    derived_zones: &DerivedPrivacyZones,
) -> Option<DerivedZone> {
    let candidates = record_path_candidates(scope, path, mappings);
    let derived_zones = derived_zones
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    candidates
        .iter()
        .find_map(|candidate| derived_zones.get(candidate).cloned())
}

fn record_path_candidates(
    scope: &PrivacyRecordScope,
    path: &Path,
    mappings: &[RegisteredWorktreePathMapping],
) -> Vec<PathBuf> {
    let mut candidates = registered_alias_paths(path, mappings);
    candidates.push(path.to_path_buf());
    if let Some(facts) = scope.file_index.get(path) {
        candidates.extend(scope.file_index.aliases_of(&facts, path));
    }
    let workspaces = scope.workspace_folders();
    let aliases = candidates.clone();
    candidates.extend(workspaces.iter().flat_map(|workspace| {
        aliases
            .iter()
            .filter_map(move |alias| alias.strip_prefix(workspace).ok().map(Path::to_path_buf))
    }));
    candidates.sort();
    candidates.dedup();
    candidates
}

fn privacy_roots(
    scope: &PrivacyRecordScope,
    mappings: &[RegisteredWorktreePathMapping],
) -> Vec<PathBuf> {
    let mut roots = scope.workspace_folders();
    roots.extend(
        mappings
            .iter()
            .flat_map(|mapping| [mapping.root.clone(), mapping.source_root.clone()]),
    );
    roots.sort();
    roots.dedup();
    roots
}

fn compare_named_zones(compiled: &CompiledPolicy, left: &str, right: &str) -> Ordering {
    compare_zone_strictness(
        compiled,
        left,
        zone_destinations(compiled, left),
        right,
        zone_destinations(compiled, right),
    )
}

fn compare_zone_strictness(
    compiled: &CompiledPolicy,
    left_name: &str,
    left_destinations: &[String],
    right_name: &str,
    right_destinations: &[String],
) -> Ordering {
    match (
        destinations_strict_subset(left_destinations, right_destinations),
        destinations_strict_subset(right_destinations, left_destinations),
    ) {
        (true, false) => Ordering::Less,
        (false, true) => Ordering::Greater,
        _ => compare_zone_order(compiled, left_name, right_name),
    }
}

fn compare_zone_order(compiled: &CompiledPolicy, left_name: &str, right_name: &str) -> Ordering {
    compiled
        .zone_index_named(left_name)
        .unwrap_or(usize::MAX)
        .cmp(&compiled.zone_index_named(right_name).unwrap_or(usize::MAX))
        .then_with(|| left_name.cmp(right_name))
}

fn strictest_named_zone<'a, I>(compiled: &CompiledPolicy, names: I) -> Option<&'a str>
where
    I: IntoIterator<Item = &'a str>,
{
    let mut names = names.into_iter();
    let first = names.next()?;
    let minimums = names.fold(vec![first], |mut minimums, candidate| {
        let candidate_destinations = zone_destinations(compiled, candidate);
        if minimums.iter().any(|minimum| {
            destinations_strict_subset(zone_destinations(compiled, minimum), candidate_destinations)
        }) {
            return minimums;
        }
        minimums.retain(|minimum| {
            !destinations_strict_subset(
                candidate_destinations,
                zone_destinations(compiled, minimum),
            )
        });
        minimums.push(candidate);
        minimums
    });
    minimums
        .into_iter()
        .min_by(|left, right| compare_zone_order(compiled, left, right))
}

fn zone_destinations<'a>(compiled: &'a CompiledPolicy, name: &str) -> &'a [String] {
    compiled
        .zone_named(name)
        .map(|zone| zone.send_to.as_slice())
        .unwrap_or(&[])
}

fn destinations_strict_subset(left: &[String], right: &[String]) -> bool {
    let left_wildcard = left.iter().any(|destination| destination == "*");
    let right_wildcard = right.iter().any(|destination| destination == "*");
    if left_wildcard {
        return false;
    }
    if right_wildcard {
        return true;
    }
    left.iter().all(|destination| right.contains(destination))
        && right.iter().any(|destination| !left.contains(destination))
}

pub async fn apply_shell_observation_with_scope(
    scope: &PrivacyRecordScope,
    command: &str,
    cwd: &Path,
    destination: &Destination,
    observation: ObservationStatus,
    derived_zones: &DerivedPrivacyZones,
    message: &mut ChatMessage,
) -> Result<ShellReadDecision, String> {
    *scope.privacy_observation_runtime.write().unwrap() = observation_runtime_state(&observation);
    let policy = scope.policy();
    let records = match observation {
        ObservationStatus::Observed(access) => {
            classify_observed_access(scope, &policy, access, derived_zones).await?
        }
        ObservationStatus::Pending(access) => {
            message.extra.insert(
                "privacy_observation".to_string(),
                serde_json::json!({
                    "status": "pending",
                    "degraded": false,
                    "incomplete": true,
                }),
            );
            classify_observed_access(scope, &policy, access, derived_zones).await?
        }
        ObservationStatus::Incomplete(access) => {
            message.extra.insert(
                "privacy_observation".to_string(),
                serde_json::json!({
                    "status": "incomplete",
                    "degraded": false,
                    "incomplete": true,
                }),
            );
            classify_observed_access(scope, &policy, access, derived_zones).await?
        }
        ObservationStatus::Unavailable(reason) => {
            let compiled = policy.compile().map_err(|error| error.to_string())?;
            let heuristic = attribute_shell_command(command, cwd, &compiled);
            message.extra.insert(
                "privacy_observation".to_string(),
                serde_json::json!({
                    "status": "unavailable",
                    "reason": reason,
                    "degraded": true,
                    "incomplete": heuristic.incomplete,
                }),
            );
            merge_records(message, heuristic.files);
            return Ok(ShellReadDecision::Pass);
        }
    };
    merge_records(message, records.clone());

    let offending: Vec<(&FileRecord, ShellBehavior)> = records
        .iter()
        .filter_map(|record| {
            if record.zone == "blocked" {
                return Some((record, ShellBehavior::Deny));
            }
            policy
                .zones
                .iter()
                .find(|zone| zone.name == record.zone)
                .filter(|zone| !destination.matches_send_to(&zone.send_to))
                .map(|zone| (record, zone.on_shell_read))
        })
        .collect();
    let behavior =
        offending
            .iter()
            .map(|(_, behavior)| *behavior)
            .fold(None, |decision, behavior| match (decision, behavior) {
                (_, ShellBehavior::Deny) => Some(ShellBehavior::Deny),
                (Some(ShellBehavior::Deny), _) => Some(ShellBehavior::Deny),
                (_, ShellBehavior::Withhold) => Some(ShellBehavior::Withhold),
                (Some(ShellBehavior::Withhold), _) => Some(ShellBehavior::Withhold),
                (_, ShellBehavior::Ask) => Some(ShellBehavior::Ask),
            });
    let origins = offending_record_origins(scope, &offending, derived_zones);
    let guarded = guarded_read_list(&offending, &origins);
    let inherited_note = if origins.is_empty() {
        ""
    } else {
        INHERITED_ZONE_NOTE
    };
    let target = destination.id.0.as_str();
    match behavior {
        Some(ShellBehavior::Deny) => Err(format!(
            "Denied by user privacy policy — the command read guarded files, so its output was discarded:\n{guarded}\nThe command already ran; re-running it unchanged will be denied again. Scope it so it does not read those paths, or ask the user to relax the zone.{inherited_note}"
        )),
        Some(ShellBehavior::Withhold) => {
            retain_local_shell_output(
                message,
                &format!(
                    "Output withheld by user privacy policy — the command ran, but its output cannot be sent to \"{target}\" because it read guarded files:\n{guarded}\nAny side effects already happened, so do not re-run it just to retry. To see output, re-run it scoped so it does not read those paths, or ask the user to allow these zones for \"{target}\".{inherited_note}"
                ),
                false,
            );
            Ok(ShellReadDecision::Pass)
        }
        Some(ShellBehavior::Ask) => {
            retain_local_shell_output(
                message,
                &format!(
                    "Output awaiting user approval — the command ran, but its output stays hidden until the user approves it, because it read guarded files:\n{guarded}{inherited_note}"
                ),
                true,
            );
            Ok(ShellReadDecision::Ask)
        }
        None => Ok(ShellReadDecision::Pass),
    }
}

fn offending_record_origins(
    scope: &PrivacyRecordScope,
    offending: &[(&FileRecord, ShellBehavior)],
    derived_zones: &DerivedPrivacyZones,
) -> HashMap<String, String> {
    if offending.is_empty()
        || derived_zones
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .is_empty()
    {
        return HashMap::new();
    }
    let mappings = registered_worktree_path_mappings(scope.cache_dir.as_path());
    let mut origins = HashMap::new();
    for (record, _) in offending {
        if origins.contains_key(&record.path) {
            continue;
        }
        let derived = derived_zone_for_path_with(
            scope,
            Path::new(record.path.as_str()),
            &mappings,
            derived_zones,
        );
        if let Some(origin) = derived
            .filter(|derived| derived.zone == record.zone)
            .and_then(|derived| derived.origin)
        {
            origins.insert(record.path.clone(), origin);
        }
    }
    origins
}

async fn classify_observed_access(
    scope: &PrivacyRecordScope,
    policy: &PrivacyPolicy,
    access: refact_exec::ObservedAccess,
    derived_zones: &DerivedPrivacyZones,
) -> Result<Vec<FileRecord>, String> {
    let scope = scope.clone();
    let policy = policy.clone();
    let derived_zones = derived_zones.clone();
    tokio::task::spawn_blocking(move || {
        let records = observed_file_records_with_derived(&scope, access.reads, &derived_zones)?;
        inherit_observed_write_zones(&scope, &policy, &records, access.writes, &derived_zones)?;
        Ok(records)
    })
    .await
    .map_err(|error| format!("privacy classification task failed: {error}"))?
}

fn guarded_read_list(
    offending: &[(&FileRecord, ShellBehavior)],
    origins: &HashMap<String, String>,
) -> String {
    let mut listed: Vec<String> = Vec::new();
    let mut seen: Vec<(&str, &str)> = Vec::new();
    let mut hidden = 0usize;
    for (record, _) in offending {
        let key = (record.path.as_str(), record.zone.as_str());
        if seen.contains(&key) {
            continue;
        }
        seen.push(key);
        if listed.len() < MAX_LISTED_GUARDED_READS {
            listed.push(match origins.get(&record.path) {
                Some(origin) => format!(
                    "  - {} (zone \"{}\", inherited from \"{origin}\")",
                    record.path, record.zone
                ),
                None => format!("  - {} (zone \"{}\")", record.path, record.zone),
            });
        } else {
            hidden += 1;
        }
    }
    if hidden > 0 {
        listed.push(format!("  - ...and {hidden} more"));
    }
    listed.join("\n")
}

fn retain_local_shell_output(message: &mut ChatMessage, replacement: &str, ask_pending: bool) {
    let full_output = message.content.content_text_only();
    message.extra.insert(
        "privacy_shell".to_string(),
        serde_json::json!({
            "withheld": !ask_pending,
            "ask_pending": ask_pending,
            "local_only_output": full_output,
        }),
    );
    message.content = ChatContent::SimpleText(replacement.to_string());
}

pub fn shell_ask_pending(message: &ChatMessage) -> bool {
    message
        .extra
        .get("privacy_shell")
        .and_then(|value| value.get("ask_pending"))
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
}

pub fn resolve_shell_ask(message: &mut ChatMessage, accepted: bool) -> bool {
    if !shell_ask_pending(message) {
        return false;
    }
    let full_output = message
        .extra
        .get("privacy_shell")
        .and_then(|value| value.get("local_only_output"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string();
    let Some(shell) = message
        .extra
        .get_mut("privacy_shell")
        .and_then(serde_json::Value::as_object_mut)
    else {
        return false;
    };
    shell.insert("ask_pending".to_string(), serde_json::Value::Bool(false));
    shell.insert("approved".to_string(), serde_json::Value::Bool(accepted));
    shell.insert("withheld".to_string(), serde_json::Value::Bool(!accepted));
    message.content = ChatContent::SimpleText(if accepted {
        full_output
    } else {
        SHELL_WITHHELD_MESSAGE.to_string()
    });
    true
}

pub fn file_record(
    scope: &PrivacyRecordScope,
    path: &Path,
    attribution: Attribution,
    derived_zones: &DerivedPrivacyZones,
) -> Result<FileRecord, String> {
    let compiled = scope
        .policy()
        .compile()
        .map_err(|error| error.to_string())?;
    let mappings = registered_worktree_path_mappings(scope.cache_dir.as_path());
    Ok(file_record_with(
        scope,
        &compiled,
        &mappings,
        path,
        attribution,
        derived_zones,
    ))
}

fn file_record_with(
    scope: &PrivacyRecordScope,
    compiled: &CompiledPolicy,
    mappings: &[RegisteredWorktreePathMapping],
    path: &Path,
    attribution: Attribution,
    derived_zones: &DerivedPrivacyZones,
) -> FileRecord {
    FileRecord {
        path: normalize_file_name(path.to_string_lossy().into_owned()),
        zone: zone_for_record_path(scope, compiled, path, mappings, derived_zones),
        attribution,
    }
}

pub fn declared_file_record(scope: &PrivacyRecordScope, path: &Path) -> Result<FileRecord, String> {
    file_record(
        scope,
        path,
        Attribution::Declared,
        &new_derived_privacy_zones(),
    )
}

pub fn declared_file_records_with_scope(
    scope: &PrivacyRecordScope,
    paths: impl IntoIterator<Item = PathBuf>,
) -> Result<Vec<FileRecord>, String> {
    let compiled = scope
        .policy()
        .compile()
        .map_err(|error| error.to_string())?;
    let mappings = registered_worktree_path_mappings(scope.cache_dir.as_path());
    let derived_zones = new_derived_privacy_zones();
    let mut records = Vec::new();
    let mut seen = HashSet::new();
    for path in paths {
        let record = file_record_with(
            scope,
            &compiled,
            &mappings,
            &path,
            Attribution::Declared,
            &derived_zones,
        );
        if seen.insert(record.clone()) {
            records.push(record);
        }
    }
    Ok(records)
}

pub fn observed_file_records(
    scope: &PrivacyRecordScope,
    paths: impl IntoIterator<Item = PathBuf>,
) -> Result<Vec<FileRecord>, String> {
    observed_file_records_with_derived(scope, paths, &new_derived_privacy_zones())
}

fn observed_file_records_with_derived(
    scope: &PrivacyRecordScope,
    paths: impl IntoIterator<Item = PathBuf>,
    derived_zones: &DerivedPrivacyZones,
) -> Result<Vec<FileRecord>, String> {
    let compiled = scope
        .policy()
        .compile()
        .map_err(|error| error.to_string())?;
    let mappings = registered_worktree_path_mappings(scope.cache_dir.as_path());
    let mut records = Vec::new();
    let mut seen = HashSet::new();
    for path in paths {
        let record = file_record_with(
            scope,
            &compiled,
            &mappings,
            &path,
            Attribution::Observed,
            derived_zones,
        );
        if seen.insert(record.clone()) {
            records.push(record);
        }
    }
    Ok(records)
}

fn is_taintable_write_target(path: &Path) -> bool {
    std::fs::metadata(path)
        .map(|metadata| metadata.is_file())
        .unwrap_or(false)
        && !is_vcs_metadata_churn(path)
}

fn is_vcs_metadata_churn(path: &Path) -> bool {
    let components = path
        .components()
        .filter_map(|component| match component {
            std::path::Component::Normal(name) => name.to_str(),
            _ => None,
        })
        .collect::<Vec<_>>();
    components
        .iter()
        .enumerate()
        .any(|(index, name)| match *name {
            ".hg" | ".svn" => true,
            ".git" => components.get(index + 1) != Some(&"objects"),
            _ => false,
        })
}

pub fn attach_declared_output_files(
    scope: &PrivacyRecordScope,
    messages: &mut [ChatMessage],
) -> Result<(), String> {
    for message in messages {
        let paths = match &message.content {
            ChatContent::ContextFiles(files) => files
                .iter()
                .map(|file| PathBuf::from(&file.file_name))
                .collect::<Vec<_>>(),
            ChatContent::SimpleText(text) if message.role == "diff" => {
                serde_json::from_str::<Vec<DiffChunk>>(text)
                    .unwrap_or_default()
                    .into_iter()
                    .flat_map(|chunk| {
                        std::iter::once(PathBuf::from(chunk.file_name)).chain(
                            chunk
                                .file_name_rename
                                .filter(|path| !path.is_empty())
                                .map(PathBuf::from),
                        )
                    })
                    .collect()
            }
            _ => continue,
        };
        let records = declared_file_records_with_scope(scope, paths)?;
        merge_records(message, records);
    }
    Ok(())
}

fn inherit_observed_write_zones(
    scope: &PrivacyRecordScope,
    policy: &PrivacyPolicy,
    reads: &[FileRecord],
    writes: impl IntoIterator<Item = PathBuf>,
    derived_zones: &DerivedPrivacyZones,
) -> Result<(), String> {
    let compiled = policy.compile().map_err(|error| error.to_string())?;
    let Some(zone_name) = strictest_named_zone(
        &compiled,
        reads
            .iter()
            .filter(|record| record.zone != "normal")
            .map(|record| record.zone.as_str()),
    ) else {
        return Ok(());
    };
    let origin = reads
        .iter()
        .find(|record| record.zone == zone_name)
        .map(|record| record.path.clone());
    let mappings = registered_worktree_path_mappings(scope.cache_dir.as_path());
    let mut derived_zones = derived_zones
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    for path in writes {
        if !is_taintable_write_target(&path) {
            continue;
        }
        for candidate in record_path_candidates(scope, &path, &mappings) {
            let replace = derived_zones
                .get(&candidate)
                .map(|current| {
                    compare_named_zones(&compiled, zone_name, &current.zone) == Ordering::Less
                })
                .unwrap_or(true);
            if replace {
                derived_zones.insert(
                    candidate,
                    DerivedZone {
                        zone: zone_name.to_string(),
                        origin: origin.clone(),
                    },
                );
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::{SubagentPolicy, Zone};

    #[test]
    fn named_zone_strictness_uses_inclusion_then_zone_order() {
        let policy = PrivacyPolicy {
            blocked: Vec::new(),
            zones: vec![
                Zone {
                    name: "z-earlier".to_string(),
                    patterns: vec!["earlier.txt".to_string()],
                    send_to: vec!["provider-a".to_string(), "provider-b".to_string()],
                    on_shell_read: ShellBehavior::Withhold,
                },
                Zone {
                    name: "a-later-equal".to_string(),
                    patterns: vec!["equal.txt".to_string()],
                    send_to: vec!["provider-b".to_string(), "provider-a".to_string()],
                    on_shell_read: ShellBehavior::Withhold,
                },
                Zone {
                    name: "later-narrow".to_string(),
                    patterns: vec!["narrow.txt".to_string()],
                    send_to: vec!["provider-a".to_string()],
                    on_shell_read: ShellBehavior::Withhold,
                },
                Zone {
                    name: "normal".to_string(),
                    patterns: vec!["*".to_string()],
                    send_to: vec!["*".to_string()],
                    on_shell_read: ShellBehavior::Withhold,
                },
            ],
            subagents: SubagentPolicy::default(),
            ..Default::default()
        };
        let compiled = policy.compile().expect("policy should compile");

        assert_eq!(
            compare_named_zones(&compiled, "a-later-equal", "z-earlier"),
            Ordering::Greater
        );
        assert_eq!(
            compare_named_zones(&compiled, "later-narrow", "z-earlier"),
            Ordering::Less
        );
        for names in [
            ["z-earlier", "a-later-equal", "later-narrow"],
            ["later-narrow", "a-later-equal", "z-earlier"],
        ] {
            assert_eq!(strictest_named_zone(&compiled, names), Some("later-narrow"));
        }
    }
}
