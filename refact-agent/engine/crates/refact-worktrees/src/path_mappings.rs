use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use refact_files::path_utils::canonicalize_normalized_path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegisteredWorktreePathMapping {
    pub root: PathBuf,
    pub source_root: PathBuf,
}

const WORKTREE_MAPPINGS_TTL: std::time::Duration = std::time::Duration::from_secs(5);

pub type WorktreePathMappings = Arc<Vec<RegisteredWorktreePathMapping>>;

static WORKTREE_MAPPINGS_CACHE: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashMap<PathBuf, (Instant, WorktreePathMappings)>>,
> = std::sync::OnceLock::new();

pub fn registered_worktree_path_mappings(cache_dir: &Path) -> WorktreePathMappings {
    let cache = WORKTREE_MAPPINGS_CACHE
        .get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()));
    {
        let guard = cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some((stamped_at, mappings)) = guard.get(cache_dir) {
            if stamped_at.elapsed() < WORKTREE_MAPPINGS_TTL {
                return Arc::clone(mappings);
            }
        }
    }
    let mappings = Arc::new(registered_worktree_path_mappings_uncached(cache_dir));
    cache
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(
            cache_dir.to_path_buf(),
            (Instant::now(), Arc::clone(&mappings)),
        );
    mappings
}

fn registered_worktree_path_mappings_uncached(
    cache_dir: &Path,
) -> Vec<RegisteredWorktreePathMapping> {
    let worktrees_base = crate::service::worktrees_base_dir(cache_dir)
        .unwrap_or_else(|_| cache_dir.join("worktrees"));
    let worktrees_root = canonicalize_normalized_path(worktrees_base);
    let Ok(project_dirs) = std::fs::read_dir(&worktrees_root) else {
        return Vec::new();
    };

    let mut mappings = Vec::new();
    let mut seen = HashSet::new();
    for project_dir in project_dirs.filter_map(Result::ok) {
        let registry_cache_root = canonicalize_normalized_path(project_dir.path());
        let Some(project_hash) = registry_cache_root
            .file_name()
            .and_then(|name| name.to_str())
        else {
            continue;
        };
        let registry_path = project_dir.path().join("index.json");
        let Ok(content) = std::fs::read_to_string(&registry_path) else {
            continue;
        };
        let Ok(registry) =
            serde_json::from_str::<crate::types::WorktreeRegistry>(&content)
        else {
            continue;
        };
        if registry.project_hash != project_hash {
            continue;
        }
        let registry_source = canonicalize_normalized_path(registry.source_workspace_root.clone());
        let source_hash_matches = [&registry_source, &registry.source_workspace_root]
            .into_iter()
            .any(|candidate| {
                crate::service::project_hash_for_path(candidate) == registry.project_hash
            });
        if !registry_source.is_dir() || !source_hash_matches {
            continue;
        }
        for record in registry.records {
            let root = canonicalize_normalized_path(record.meta.root);
            let expected_root =
                canonicalize_normalized_path(registry_cache_root.join(&record.meta.id));
            if root != expected_root {
                continue;
            }
            let source_root = canonicalize_normalized_path(record.meta.source_workspace_root);
            if source_root != registry_source || !source_root.is_dir() {
                continue;
            }
            if seen.insert((root.clone(), source_root.clone())) {
                mappings.push(RegisteredWorktreePathMapping { root, source_root });
            }
        }
    }
    mappings.sort_by(|a, b| {
        b.root
            .components()
            .count()
            .cmp(&a.root.components().count())
    });
    mappings
}

pub fn map_registered_worktree_path(
    path: &Path,
    mappings: &[RegisteredWorktreePathMapping],
) -> Option<PathBuf> {
    let path = canonicalize_normalized_path(path.to_path_buf());
    mappings.iter().find_map(|mapping| {
        path.strip_prefix(&mapping.root)
            .ok()
            .map(|suffix| canonicalize_normalized_path(mapping.source_root.join(suffix)))
    })
}

pub fn normalize_path_for_unscoped_paths(
    path: &Path,
    mappings: &[RegisteredWorktreePathMapping],
) -> Option<PathBuf> {
    if let Some(mapped) = map_registered_worktree_path(path, mappings) {
        return mapped.exists().then_some(mapped);
    }
    Some(canonicalize_normalized_path(path.to_path_buf()))
}

pub fn normalize_path_for_unscoped_root_selection(
    path: &Path,
    mappings: &[RegisteredWorktreePathMapping],
) -> Option<PathBuf> {
    map_registered_worktree_path(path, mappings)
        .or_else(|| Some(canonicalize_normalized_path(path.to_path_buf())))
}

pub fn dedupe_paths(paths: impl IntoIterator<Item = PathBuf>) -> Vec<PathBuf> {
    let mut seen = HashSet::new();
    let mut result = Vec::new();
    for path in paths {
        if seen.insert(path.clone()) {
            result.push(path);
        }
    }
    result
}

pub fn project_dirs_for_unscoped_paths(cache_dir: &Path, project_dirs: &[PathBuf]) -> Vec<PathBuf> {
    let mappings = registered_worktree_path_mappings(cache_dir);
    dedupe_paths(
        project_dirs
            .iter()
            .filter_map(|path| normalize_path_for_unscoped_paths(path, &mappings))
            .filter(|path| path.is_dir()),
    )
}
