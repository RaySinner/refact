use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex, OnceLock, Weak};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use git2::{Oid, Repository};
use petgraph::algo::tarjan_scc;
use petgraph::graph::{DiGraph, NodeIndex};
use serde::Serialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::sync::Mutex as AMutex;

use refact_runtime_api::AtCommandsContext;
use refact_runtime_api::at_command_executor::{
    file_repair_candidates, return_one_candidate_or_a_good_error,
};
use refact_core::chat_types::{ChatContent, ChatMessage, ContextEnum};
use refact_code_intel_api::*;
use refact_files::path_utils::{canonicalize_normalized_path, preprocess_path_for_normalization};
use refact_runtime_api::files_correction::{get_project_dirs, resolve_codegraph_queue_path};
use refact_worktrees::path_mappings::registered_worktree_path_mappings;
use refact_postprocessing::pp_command_output::OutputFilter;
use refact_runtime_api::scope_utils::resolve_existing_path_with_execution_scope_for_model_context;
use crate::tools::tools_description::{
    json_schema_from_params, Tool, ToolDesc, ToolSource, ToolSourceType,
};

mod tools;
mod health;
mod graph;
mod git_risk;
mod pr_blast;
mod clones;
mod dead_code;
mod code_map;
mod code_why;

#[allow(unused_imports)]
pub use self::tools::*;
#[allow(unused_imports)]
pub use self::health::*;
#[allow(unused_imports)]
pub use self::graph::*;
#[allow(unused_imports)]
pub use self::git_risk::*;
#[allow(unused_imports)]
pub use self::pr_blast::*;
#[allow(unused_imports)]
pub use self::clones::*;
#[allow(unused_imports)]
pub use self::dead_code::*;
#[allow(unused_imports)]
pub use self::code_map::*;
#[allow(unused_imports)]
pub use self::code_why::*;
const GIT_CACHE_TTL: Duration = Duration::from_secs(30);
const PR_BLAST_REVIEWER_LIMIT: usize = 3;
const PR_BLAST_BOT_AUTHOR_PATTERNS: [&str; 3] = ["[bot]", "agent@", "noreply"];
const DEAD_CODE_HISTORY_COMMITS: usize = 1000;
const DEAD_CODE_DEFAULT_LIMIT: usize = 50;
const DEAD_CODE_MAX_LIMIT: usize = 500;
const GIT_META_FUNCTION_FACT_TOP_K: usize = 10;
const GIT_STABLE_AGE_DAYS: i64 = 180;
const GIT_HISTORY_MAX_COMMITS: usize = 1000;
const GIT_RISK_RECENT_COMMITS: usize = 50;
const GIT_RISK_RECENT_COMMIT_LIMIT: usize = 10;
const CODE_MAP_DEFAULT_BUDGET_TOKENS: usize = 12_000;
const CODE_MAP_MAX_PAGES_WITHOUT_QUERY: usize = 60;
const CODE_MAP_QUERY_LIMIT: usize = 12;
const CODE_MAP_LINK_LIMIT: usize = 8;
const CODE_MAP_MIN_MODULE_FILES: usize = 2;
const CODE_MAP_MIN_MODULE_SIZE: u64 = 2;
const CODE_MAP_LINK_EDGE_KINDS: [&str; 3] = ["calls", "inherits", "route_handler"];
const HEALTH_SNAPSHOT_META_KEY: &str = "health_snapshots";
const HEALTH_SNAPSHOT_LIMIT: usize = 30;
pub const HEALTH_ANALYSIS_CACHE_CAPACITY: usize = 256;
const HEALTH_GRAPH_SNAPSHOT_MAX_ATTEMPTS: usize = 2;

pub type GitCacheKey = (PathBuf, Option<Oid>, usize);

pub use refact_code_intel_api::SuggestedReviewer;

pub use refact_code_intel_api::PrBlastIndexState;

#[derive(Default)]
pub struct GitMiningCache {
    pub history: HashMap<GitCacheKey, (Instant, refact_git_intel::GitIntel)>,
}
pub use refact_code_intel_api::HealthImpactContributor;

pub use refact_code_intel_api::RecentCommitRiskSummary;

pub fn git_cache() -> &'static StdMutex<GitMiningCache> {
    static CACHE: OnceLock<StdMutex<GitMiningCache>> = OnceLock::new();
    CACHE.get_or_init(|| StdMutex::new(GitMiningCache::default()))
}

#[derive(Default)]
pub struct BlameFactsCache {
    pub repo_head: Option<(PathBuf, Option<Oid>)>,
    pub facts: HashMap<(String, u64), Vec<refact_git_intel::blame::RangeGitFacts>>,
}
fn blame_facts_cache() -> &'static StdMutex<BlameFactsCache> {
    static CACHE: OnceLock<StdMutex<BlameFactsCache>> = OnceLock::new();
    CACHE.get_or_init(|| StdMutex::new(BlameFactsCache::default()))
}

fn blame_ranges_hash(ranges: &[(String, usize, usize)]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    ranges.hash(&mut hasher);
    hasher.finish()
}

fn cached_blame_range_facts(
    repo_root: &Path,
    file: &str,
    ranges: &[(String, usize, usize)],
) -> Vec<refact_git_intel::blame::RangeGitFacts> {
    let Ok(head) = git_head_oid(repo_root) else {
        return refact_git_intel::blame::range_facts(repo_root, file, ranges).unwrap_or_default();
    };
    let key = (file.to_string(), blame_ranges_hash(ranges));
    {
        let mut cache = blame_facts_cache().lock().unwrap();
        let head_key = Some((repo_root.to_path_buf(), head));
        if cache.repo_head != head_key {
            cache.repo_head = head_key;
            cache.facts.clear();
        } else if let Some(facts) = cache.facts.get(&key) {
            return facts.clone();
        }
    }
    let facts = refact_git_intel::blame::range_facts(repo_root, file, ranges).unwrap_or_default();
    {
        let mut cache = blame_facts_cache().lock().unwrap();
        if cache.repo_head == Some((repo_root.to_path_buf(), head)) {
            cache.facts.insert(key, facts.clone());
        }
    }
    facts
}

fn tool_message(tool_call_id: &String, text: String) -> Vec<ContextEnum> {
    vec![ContextEnum::ChatMessage(ChatMessage {
        role: "tool".to_string(),
        content: ChatContent::SimpleText(text),
        tool_calls: None,
        tool_call_id: tool_call_id.clone(),
        output_filter: Some(OutputFilter::no_limits()),
        ..Default::default()
    })]
}

fn tool_message_with_records(
    gcx: &Arc<refact_runtime_api::global_context::GlobalContext>,
    tool_call_id: &String,
    text: String,
    paths: impl IntoIterator<Item = PathBuf>,
) -> Result<Vec<ContextEnum>, String> {
    let mut messages = tool_message(tool_call_id, text);
    let records = refact_runtime_api::gcx_helpers::declared_file_records(gcx, paths)?;
    if let Some(ContextEnum::ChatMessage(message)) = messages.first_mut() {
        refact_privacy::merge_records(message, records);
    }
    Ok(messages)
}

fn codegraph_dependency() -> Vec<String> {
    vec!["codegraph".to_string()]
}

fn string_arg(args: &HashMap<String, Value>, key: &str) -> Result<String, String> {
    match args.get(key) {
        Some(Value::String(s)) => Ok(s.clone()),
        Some(v) => Err(format!("argument `{key}` is not a string: {v:?}")),
        None => Err(format!("argument `{key}` is missing")),
    }
}

fn string_list_arg(args: &HashMap<String, Value>, key: &str) -> Result<Vec<String>, String> {
    let values = match args.get(key) {
        Some(Value::Array(items)) => items
            .iter()
            .enumerate()
            .map(|(index, item)| {
                item.as_str()
                    .map(|item| item.trim().to_string())
                    .ok_or_else(|| format!("argument `{key}[{index}]` is not a string: {item:?}"))
            })
            .collect::<Result<Vec<_>, _>>()?,
        Some(Value::String(value)) => {
            let trimmed = value.trim();
            if trimmed.starts_with('[') {
                serde_json::from_str::<Vec<String>>(trimmed)
                    .map_err(|err| format!("argument `{key}` is not a string array: {err}"))?
            } else {
                trimmed
                    .split(',')
                    .map(|item| item.trim().to_string())
                    .collect()
            }
        }
        Some(value) => return Err(format!("argument `{key}` is not a string array: {value:?}")),
        None => return Err(format!("argument `{key}` is missing")),
    };
    Ok(values
        .into_iter()
        .filter(|value| !value.is_empty())
        .collect())
}

fn optional_usize_arg(
    args: &HashMap<String, Value>,
    key: &str,
    default: usize,
) -> Result<usize, String> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(default),
        Some(Value::String(value)) if value.trim().is_empty() => Ok(default),
        Some(Value::String(value)) => value
            .trim()
            .parse::<usize>()
            .map_err(|_| format!("argument `{key}` must be a positive integer")),
        Some(Value::Number(value)) => value
            .as_u64()
            .and_then(|value| usize::try_from(value).ok())
            .ok_or_else(|| format!("argument `{key}` must be a positive integer")),
        Some(value) => Err(format!(
            "argument `{key}` must be a positive integer: {value:?}"
        )),
    }
}

fn optional_f64_arg(args: &HashMap<String, Value>, key: &str, default: f64) -> Result<f64, String> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(default),
        Some(Value::String(value)) if value.trim().is_empty() => Ok(default),
        Some(Value::String(value)) => value
            .trim()
            .parse::<f64>()
            .map_err(|_| format!("argument `{key}` must be a number")),
        Some(Value::Number(value)) => value
            .as_f64()
            .ok_or_else(|| format!("argument `{key}` must be a number")),
        Some(value) => Err(format!("argument `{key}` must be a number: {value:?}")),
    }
}

fn optional_string_arg(args: &HashMap<String, Value>, key: &str) -> Result<Option<String>, String> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => {
            let value = value.trim();
            if value.is_empty() {
                Ok(None)
            } else {
                Ok(Some(value.replace('\\', "/")))
            }
        }
        Some(value) => Err(format!("argument `{key}` must be a string: {value:?}")),
    }
}

#[derive(Debug, Clone)]
pub struct ResolvedCodegraphFile {
    pub read_path: PathBuf,
    pub indexed_path: String,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodegraphProjectRoots {
    pub git_root: PathBuf,
    pub indexed_root: PathBuf,
    pub analytics_scope_enforced: bool,
}
async fn resolve_codegraph_file(
    ccx: Arc<AMutex<AtCommandsContext>>,
    raw_path: &str,
) -> Result<ResolvedCodegraphFile, String> {
    let (gcx, tool, top_n, execution_scope) = {
        let ccx = ccx.lock().await;
        (
            ccx.app.gcx.clone(),
            ccx.tool.clone(),
            ccx.top_n,
            ccx.execution_scope.clone(),
        )
    };
    let read_path = if let Some(resolved) =
        resolve_existing_path_with_execution_scope_for_model_context(
            &tool,
            execution_scope.as_ref(),
            raw_path,
        )
        .await?
    {
        if resolved.outside_absolute_path {
            return Err(format!(
                "Path '{}' is outside the active worktree; CodeGraph file analysis is limited to the current project",
                resolved.path.display()
            ));
        }
        resolved.path
    } else {
        let normalized = if PathBuf::from(raw_path).is_absolute() {
            refact_files::path_utils::canonical_path(raw_path)
                .to_string_lossy()
                .to_string()
        } else {
            preprocess_path_for_normalization(raw_path.to_string())
        };
        let candidates = file_repair_candidates(&tool, &normalized, top_n, false).await;
        let resolved = return_one_candidate_or_a_good_error(
            &tool,
            &normalized,
            &candidates,
            &get_project_dirs(gcx.clone()).await,
            false,
        )
        .await?;
        PathBuf::from(resolved)
    };
    if !read_path.is_file() {
        return Err(format!("Path '{}' is not a file", read_path.display()));
    }
    refact_runtime_api::files_in_workspace::check_file_privacy_for_model_context(gcx.clone(), &read_path)
        .await?;
    let indexed_path =
        if let Some(scope) = execution_scope.as_ref().filter(|scope| scope.is_enforced()) {
            scoped_indexed_path(scope, &read_path)?
                .to_string_lossy()
                .to_string()
        } else {
            let mappings = registered_worktree_path_mappings(gcx.cache_dir.as_path());
            resolve_codegraph_queue_path(&read_path, &mappings).store_path
        };
    Ok(ResolvedCodegraphFile {
        read_path,
        indexed_path,
    })
}

async fn resolve_codegraph_filter_path(
    ccx: Arc<AMutex<AtCommandsContext>>,
    raw_path: &str,
) -> Result<String, String> {
    let normalized_raw = raw_path.trim().replace('\\', "/");
    let (gcx, execution_scope) = {
        let ccx = ccx.lock().await;
        (ccx.app.gcx.clone(), ccx.execution_scope.clone())
    };
    if let Some(scope) = execution_scope.as_ref().filter(|scope| scope.is_enforced()) {
        scope.ensure_active_root()?;
        if !normalized_raw.contains('/') {
            return Ok(normalized_raw);
        }
        let resolved = scope.resolve_creatable_path(Path::new(&normalized_raw))?;
        if resolved.outside_absolute_path {
            return Err(format!(
                "Path '{}' is outside the active worktree; CodeGraph analysis is limited to the current project",
                resolved.path.display()
            ));
        }
        refact_runtime_api::files_in_workspace::check_file_privacy_for_model_context(gcx, &resolved.path)
            .await?;
        return Ok(scoped_indexed_path(scope, &resolved.path)?
            .to_string_lossy()
            .to_string());
    }

    if let Ok(resolved) = resolve_codegraph_file(ccx, &normalized_raw).await {
        return Ok(resolved.indexed_path);
    }

    let normalized = PathBuf::from(&normalized_raw);
    if normalized.is_absolute() {
        let normalized = canonicalize_normalized_path(normalized);
        let project_dirs = get_project_dirs(gcx.clone()).await;
        if !project_dirs.iter().any(|root| normalized.starts_with(root)) {
            return Err(format!(
                "Path '{}' is outside of project directories:\n{:?}",
                normalized.display(),
                project_dirs
            ));
        }
        refact_runtime_api::files_in_workspace::check_file_privacy_for_model_context(gcx, &normalized).await?;
        return Ok(normalized.to_string_lossy().to_string());
    }
    Ok(preprocess_path_for_normalization(normalized_raw))
}

pub fn scoped_indexed_path(
    scope: &refact_worktrees::scope::ExecutionScope,
    scoped_path: &Path,
) -> Result<PathBuf, String> {
    let relative = scoped_path
        .strip_prefix(scope.effective_root())
        .map_err(|_| {
            format!(
                "Path '{}' is outside the active worktree '{}'",
                scoped_path.display(),
                scope.effective_root().display()
            )
        })?;
    Ok(canonicalize_normalized_path(
        scope.source_workspace_root().join(relative),
    ))
}

pub fn scoped_codegraph_project_roots(
    execution_scope: Option<&refact_worktrees::scope::ExecutionScope>,
) -> Result<Option<CodegraphProjectRoots>, String> {
    let Some(scope) = execution_scope.filter(|scope| scope.is_enforced()) else {
        return Ok(None);
    };
    scope.ensure_active_root()?;
    Ok(Some(CodegraphProjectRoots {
        git_root: scope.effective_root().to_path_buf(),
        indexed_root: scope.source_workspace_root().to_path_buf(),
        analytics_scope_enforced: true,
    }))
}

async fn codegraph_project_roots(
    ccx: Arc<AMutex<AtCommandsContext>>,
) -> Result<Option<CodegraphProjectRoots>, String> {
    let (gcx, execution_scope) = {
        let ccx = ccx.lock().await;
        (ccx.app.gcx.clone(), ccx.execution_scope.clone())
    };
    if let Some(roots) = scoped_codegraph_project_roots(execution_scope.as_ref())? {
        return Ok(Some(roots));
    }
    Ok(project_dir(gcx).await.map(|root| CodegraphProjectRoots {
        git_root: root.clone(),
        indexed_root: root,
        analytics_scope_enforced: false,
    }))
}

fn analytics_scope_root(roots: Option<&CodegraphProjectRoots>) -> Option<&Path> {
    roots
        .filter(|roots| roots.analytics_scope_enforced)
        .map(|roots| roots.indexed_root.as_path())
}

const CODE_INTEL_NOISE_DIRS: [&str; 10] = [
    "node_modules",
    "target",
    "dist",
    "build",
    "out",
    "coverage",
    ".next",
    "storybook-static",
    ".storybook-static",
    "vendor",
];

pub fn code_intel_relative_path(indexed_root: &Path, path: &str) -> Option<PathBuf> {
    let normalized = PathBuf::from(path.replace('\\', "/"));
    let relative = if normalized.is_absolute() {
        normalized.strip_prefix(indexed_root).ok()?.to_path_buf()
    } else {
        normalized
    };
    if relative.as_os_str().is_empty()
        || relative.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir
                    | std::path::Component::RootDir
                    | std::path::Component::Prefix(_)
            )
        })
    {
        return None;
    }
    Some(relative)
}

pub fn code_intel_path_allowed(indexed_root: &Path, path: &str) -> bool {
    let Some(relative) = code_intel_relative_path(indexed_root, path) else {
        return false;
    };
    !relative.components().any(|component| {
        let component = component.as_os_str().to_string_lossy();
        CODE_INTEL_NOISE_DIRS
            .iter()
            .any(|noise| component.eq_ignore_ascii_case(noise))
    })
}

#[cfg(any(test, feature = "test-util"))]
pub fn scoped_graph_data(
    data: &refact_codegraph::analytics::GraphData,
    indexed_root: &Path,
) -> refact_codegraph::analytics::GraphData {
    let nodes = data
        .nodes
        .iter()
        .filter(|(_, _, path)| code_intel_path_allowed(indexed_root, path))
        .cloned()
        .collect::<Vec<_>>();
    let ids = nodes.iter().map(|(id, _, _)| *id).collect::<HashSet<_>>();
    let edges = data
        .edges
        .iter()
        .filter(|(src, dst, _)| ids.contains(src) && ids.contains(dst))
        .cloned()
        .collect();
    refact_codegraph::analytics::GraphData { nodes, edges }
}

fn scoped_graph_paths(
    data: &refact_codegraph::analytics::GraphData,
    indexed_root: &Path,
) -> HashSet<String> {
    data.nodes
        .iter()
        .filter(|(_, _, path)| code_intel_path_allowed(indexed_root, path))
        .map(|(_, _, path)| path.clone())
        .collect()
}

pub async fn scoped_clone_analysis(
    analysis: &CloneAnalysis,
    indexed_root: &Path,
) -> Result<CloneAnalysis, String> {
    let text_by_path = analysis
        .text_by_path
        .iter()
        .filter(|(path, _)| code_intel_path_allowed(indexed_root, path))
        .map(|(path, text)| (path.clone(), text.clone()))
        .collect::<HashMap<_, _>>();
    let generation = analysis.generation;
    let triples = text_by_path
        .iter()
        .map(|(path, (lang, text))| (path.clone(), lang.clone(), text.clone()))
        .collect::<Vec<_>>();
    tokio::task::spawn_blocking(move || {
        let scoped = refact_codehealth::duplication::cross_file_analysis(&triples);
        CloneAnalysis {
            generation,
            clones: scoped.clones,
            duplication_pct: scoped.duplication_pct,
            files: text_by_path.len(),
            text_by_path,
            tokens_by_path: scoped.tokens_by_path,
            duplicated_tokens_by_path: scoped.duplicated_tokens_by_path,
        }
    })
    .await
    .map_err(|e| format!("scoped cross-file clone analysis join: {e}"))
}

#[cfg(any(test, feature = "test-util"))]
pub fn scoped_dead_symbols(
    dead: Vec<refact_codegraph::dead_code::DeadSymbol>,
    indexed_root: Option<&Path>,
) -> Vec<refact_codegraph::dead_code::DeadSymbol> {
    dead.into_iter()
        .filter(|symbol| {
            indexed_root.is_none_or(|root| code_intel_path_allowed(root, &symbol.path))
        })
        .collect()
}

pub fn scoped_code_map_inputs(
    indexed_root: &Path,
    files_text: Vec<(String, String)>,
    node_records: Vec<(i64, String, String, String, Option<String>)>,
    edges: Vec<refact_codegraph::analytics::GraphEdge>,
    centrality: refact_codegraph::analytics::FileCentrality,
) -> (
    Vec<(String, String)>,
    Vec<(i64, String, String, String, Option<String>)>,
    Vec<refact_codegraph::analytics::GraphEdge>,
    refact_codegraph::analytics::FileCentrality,
) {
    let files_text = files_text
        .into_iter()
        .filter(|(path, _)| code_intel_path_allowed(indexed_root, path))
        .collect();
    let node_records = node_records
        .into_iter()
        .filter(|(_, _, _, path, _)| code_intel_path_allowed(indexed_root, path))
        .collect::<Vec<_>>();
    let ids = node_records
        .iter()
        .map(|(id, _, _, _, _)| *id)
        .collect::<HashSet<_>>();
    let edges: Vec<refact_codegraph::analytics::GraphEdge> = edges
        .into_iter()
        .filter(|(src, dst, _)| ids.contains(src) && ids.contains(dst))
        .collect();
    (files_text, node_records, edges, centrality)
}

pub async fn project_dir(gcx: Arc<refact_runtime_api::global_context::GlobalContext>) -> Option<PathBuf> {
    refact_runtime_api::files_correction::get_project_dirs(gcx)
        .await
        .into_iter()
        .next()
}

pub fn git_head_oid(repo_path: &Path) -> Result<Option<Oid>, String> {
    let repo = Repository::open(repo_path).map_err(|e| format!("git open: {e}"))?;
    let head = repo.head();
    match head {
        Ok(head) => Ok(head.target()),
        Err(error) if refact_git_intel::is_empty_head_error(&error) => Ok(None),
        Err(error) => Err(format!("git head: {error}")),
    }
}

fn git_head_unchanged(repo_path: &Path, expected: &Option<Oid>) -> bool {
    git_head_oid(repo_path)
        .ok()
        .is_some_and(|current| &current == expected)
}

pub fn cached_mine_history(
    repo_path: &Path,
    max_commits: usize,
) -> Result<refact_git_intel::GitIntel, String> {
    cached_mine_history_with(repo_path, max_commits, |path, _head, base, max| {
        refact_git_intel::mine_history_incremental(path, base, max)
    })
}

pub async fn cached_mine_history_async(
    repo_path: &Path,
    max_commits: usize,
) -> Result<refact_git_intel::GitIntel, String> {
    let repo_path = repo_path.to_path_buf();
    tokio::task::spawn_blocking(move || cached_mine_history(&repo_path, max_commits))
        .await
        .map_err(|e| format!("git history mining join: {e}"))?
}

pub fn cached_mine_history_with<F>(
    repo_path: &Path,
    max_commits: usize,
    mine: F,
) -> Result<refact_git_intel::GitIntel, String>
where
    F: FnOnce(
        &Path,
        Option<Oid>,
        Option<refact_git_intel::GitIntel>,
        usize,
    ) -> Result<refact_git_intel::GitIntel, String>,
{
    let head = git_head_oid(repo_path)?;
    let key = (repo_path.to_path_buf(), head.clone(), max_commits);
    let now = Instant::now();
    let base = {
        let cache = git_cache().lock().unwrap();
        if let Some(intel) = cache
            .history
            .get(&key)
            .filter(|(created, _)| now.duration_since(*created) <= GIT_CACHE_TTL)
            .map(|(_, intel)| intel.clone())
        {
            return Ok(intel);
        }
        cache
            .history
            .iter()
            .filter(|((cached_path, _, cached_max), _)| {
                cached_path == repo_path && *cached_max == max_commits
            })
            .max_by_key(|(_, (created, _))| *created)
            .map(|(_, (_, intel))| intel.clone())
    };
    let intel = mine(repo_path, head.clone(), base, max_commits)?;
    if git_head_unchanged(repo_path, &head) {
        let mut cache = git_cache().lock().unwrap();
        cache.history.insert(key, (Instant::now(), intel.clone()));
    }
    Ok(intel)
}

pub fn co_change_count(
    intel: Option<&refact_git_intel::GitIntel>,
    a: &str,
    b: &str,
    project_root: Option<&Path>,
) -> u32 {
    let Some(intel) = intel else {
        return 0;
    };
    let key_a = co_change_key(a, project_root);
    let key_b = co_change_key(b, project_root);
    intel
        .co_change
        .get(&(key_a.clone(), key_b.clone()))
        .or_else(|| intel.co_change.get(&(key_b, key_a)))
        .copied()
        .unwrap_or(0)
}

fn co_change_key(path: &str, project_root: Option<&Path>) -> String {
    match project_root {
        Some(root) => {
            refact_git_intel::paths::repo_relative_or_basename(path, &root.to_string_lossy())
        }
        None => refact_git_intel::paths::normalize_separators(path),
    }
}

fn current_unix_ts() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs().min(i64::MAX as u64) as i64)
        .unwrap_or(0)
}

pub use refact_code_intel_api::HealthFinding;

pub const CODE_WHY_HISTORY_COMMITS: usize = 2000;
const CODE_WHY_SIGNIFICANT_COMMITS: usize = 500;
pub const CODE_WHY_SOURCE_LIMIT: usize = 800;
const CODE_WHY_MATCH_LIMIT: usize = 20;
const CODE_WHY_RELATED_PER_MATCH: usize = 4;
