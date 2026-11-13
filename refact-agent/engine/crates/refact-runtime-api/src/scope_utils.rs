use std::collections::HashSet;
use std::path::{Path, PathBuf};

pub use refact_scope_utils::{
    dedup_notices, format_scope_notices, path_with_sep, scoped_path_notices, ScopedFiles,
    ScopedResolvedPath, ScopedScopeFilter,
};

use refact_core::chat_types::ContextFile;
use refact_files::path_utils::canonical_path;
use refact_files::workspace::{ls_files, ls_files_limited, LsFilesLimited};
use crate::tool_ctx::ToolCtx;
use refact_worktrees::scope::ExecutionScope;

/// Convert a PathBuf to a String with forward slashes on all platforms.
fn path_to_string(p: &Path) -> String {
    refact_core::chat_types::normalize_file_name(p.to_string_lossy().into_owned())
}

async fn get_workspace_files(tool: &ToolCtx) -> Vec<PathBuf> {
    let files = tool.workspace.workspace_files.lock().unwrap().clone();
    let project_dirs = tool.files.project_dirs().await;
    files
        .into_iter()
        .filter(|file| project_dirs.iter().any(|dir| file.starts_with(dir)))
        .collect()
}

async fn resolve_scope_legacy_limited(
    tool: &ToolCtx,
    scope: &str,
    max_entries: usize,
    abort: Option<&std::sync::atomic::AtomicBool>,
) -> Result<ScopedFiles, String> {
    if abort.is_some_and(|flag| flag.load(std::sync::atomic::Ordering::Relaxed)) {
        return Err("⚠️ File listing aborted before completion (cancelled by caller).".to_string());
    }
    let mut files = resolve_scope_legacy(tool, scope).await?;
    let truncated = files.len() > max_entries;
    files.truncate(max_entries);
    Ok(ScopedFiles {
        files,
        notices: truncated
            .then(|| truncation_notice(max_entries))
            .into_iter()
            .collect(),
    })
}

pub async fn resolve_existing_path_with_execution_scope(
    tool: &ToolCtx,
    execution_scope: Option<&ExecutionScope>,
    raw: &str,
) -> Result<Option<ScopedResolvedPath>, String> {
    resolve_existing_path_with_execution_scope_impl(tool, execution_scope, raw, false).await
}

pub async fn resolve_existing_path_with_execution_scope_for_model_context(
    tool: &ToolCtx,
    execution_scope: Option<&ExecutionScope>,
    raw: &str,
) -> Result<Option<ScopedResolvedPath>, String> {
    resolve_existing_path_with_execution_scope_impl(tool, execution_scope, raw, true).await
}

async fn resolve_existing_path_with_execution_scope_impl(
    tool: &ToolCtx,
    execution_scope: Option<&ExecutionScope>,
    raw: &str,
    for_model_context: bool,
) -> Result<Option<ScopedResolvedPath>, String> {
    let Some(scope) = execution_scope else {
        return Ok(None);
    };
    if !scope.is_enforced() {
        return Ok(None);
    }
    let scoped = scope.resolve_existing_path(&PathBuf::from(raw))?;
    if scoped.path.is_file() {
        if for_model_context {
            tool.files
                .check_file_privacy_for_model_context(&scoped.path)
                .await?;
        } else {
            tool.files.check_file_privacy_for_send(&scoped.path).await?;
        }
    }
    Ok(Some(ScopedResolvedPath {
        path: scoped.path.clone(),
        notices: scoped_path_notices(&scoped),
        outside_absolute_path: scoped.outside_absolute_path,
    }))
}

async fn list_files_under_dir(
    tool: &ToolCtx,
    dir: &PathBuf,
    recursive: bool,
    privacy_filter: bool,
) -> Result<Vec<PathBuf>, String> {
    let indexing_everywhere = tool.files.indexing_everywhere().await;
    let files = ls_files(&indexing_everywhere, dir, recursive)?;
    if privacy_filter {
        Ok(tool.files.filter_privacy_allowed_files(files).await)
    } else {
        Ok(files)
    }
}

pub async fn list_scoped_files_under_dir(
    tool: &ToolCtx,
    dir: &PathBuf,
    recursive: bool,
    privacy_filter: bool,
) -> Result<Vec<PathBuf>, String> {
    list_files_under_dir(tool, dir, recursive, privacy_filter).await
}

pub fn is_worktree_root_alias(scope: &str) -> bool {
    let scope = scope.trim().trim_end_matches(&['/', '\\'][..]);
    scope.is_empty() || scope == "." || scope == "workspace"
}

pub async fn list_execution_scope_root(
    tool: &ToolCtx,
    execution_scope: &ExecutionScope,
    recursive: bool,
) -> Result<Vec<PathBuf>, String> {
    execution_scope.ensure_active_root()?;
    list_files_under_dir(
        tool,
        &execution_scope.effective_root().to_path_buf(),
        recursive,
        true,
    )
    .await
}

pub struct ScopedListingLimited {
    pub files: Vec<PathBuf>,
    pub truncated: bool,
}

pub fn truncation_notice(max_entries: usize) -> String {
    format!(
        "⚠️ Listing truncated after examining {} directory entries. 💡 Narrow the scope to a subdirectory to see the rest.",
        max_entries
    )
}

async fn list_files_under_dir_limited(
    tool: &ToolCtx,
    dir: &PathBuf,
    recursive: bool,
    privacy_filter: bool,
    for_model_context: bool,
    max_entries: usize,
    abort: Option<&std::sync::atomic::AtomicBool>,
) -> Result<ScopedListingLimited, String> {
    let indexing_everywhere = tool.files.indexing_everywhere().await;
    let LsFilesLimited {
        files,
        truncated,
        aborted,
    } = ls_files_limited(&indexing_everywhere, dir, recursive, max_entries, abort)?;
    if aborted {
        return Err(format!(
            "⚠️ Directory listing of '{}' was aborted before completion.",
            dir.display()
        ));
    }
    let files = if for_model_context {
        tool.files
            .filter_privacy_allowed_files_for_model_context(files)
            .await
    } else if privacy_filter {
        tool.files.filter_privacy_allowed_files(files).await
    } else {
        files
    };
    Ok(ScopedListingLimited { files, truncated })
}

async fn list_files_under_dir_limited_for_model_context(
    tool: &ToolCtx,
    dir: &PathBuf,
    recursive: bool,
    max_entries: usize,
    abort: Option<&std::sync::atomic::AtomicBool>,
) -> Result<ScopedListingLimited, String> {
    list_files_under_dir_limited(tool, dir, recursive, true, true, max_entries, abort).await
}

pub async fn list_scoped_files_under_dir_limited(
    tool: &ToolCtx,
    dir: &PathBuf,
    recursive: bool,
    privacy_filter: bool,
    max_entries: usize,
    abort: Option<&std::sync::atomic::AtomicBool>,
) -> Result<ScopedListingLimited, String> {
    list_files_under_dir_limited(
        tool,
        dir,
        recursive,
        privacy_filter,
        false,
        max_entries,
        abort,
    )
    .await
}

pub async fn list_scoped_files_under_dir_limited_for_model_context(
    tool: &ToolCtx,
    dir: &PathBuf,
    recursive: bool,
    max_entries: usize,
    abort: Option<&std::sync::atomic::AtomicBool>,
) -> Result<ScopedListingLimited, String> {
    list_files_under_dir_limited_for_model_context(tool, dir, recursive, max_entries, abort).await
}

pub async fn list_execution_scope_root_limited(
    tool: &ToolCtx,
    execution_scope: &ExecutionScope,
    recursive: bool,
    max_entries: usize,
    abort: Option<&std::sync::atomic::AtomicBool>,
) -> Result<ScopedListingLimited, String> {
    execution_scope.ensure_active_root()?;
    list_files_under_dir_limited(
        tool,
        &execution_scope.effective_root().to_path_buf(),
        recursive,
        true,
        false,
        max_entries,
        abort,
    )
    .await
}

async fn resolve_scope_legacy(tool: &ToolCtx, scope: &str) -> Result<Vec<String>, String> {
    if scope == "workspace" {
        return Ok(get_workspace_files(tool)
            .await
            .into_iter()
            .map(|f| path_to_string(&f))
            .collect());
    }

    let project_dirs = tool.files.project_dirs().await;
    let scope_string = scope.to_string();
    let scope_is_dir = scope.ends_with('/') || scope.ends_with('\\');

    if scope_is_dir {
        let candidates = tool
            .files
            .correct_to_nearest_dir_path(&scope_string, false, 10)
            .await;
        let dir_path = tool
            .files
            .return_one_candidate_or_a_good_error(&scope_string, &candidates, &project_dirs, true)
            .await?;

        let dir_path_with_sep = if dir_path.ends_with(std::path::MAIN_SEPARATOR) {
            dir_path.clone()
        } else {
            format!("{}{}", dir_path, std::path::MAIN_SEPARATOR)
        };
        return Ok(get_workspace_files(tool)
            .await
            .into_iter()
            .filter(|f| {
                f.to_string_lossy().starts_with(&dir_path_with_sep)
                    || f.to_string_lossy() == dir_path
            })
            .map(|f| path_to_string(&f))
            .collect());
    }

    let file_candidates = tool
        .files
        .file_repair_candidates(&scope_string, 10, false)
        .await;
    match tool
        .files
        .return_one_candidate_or_a_good_error(&scope_string, &file_candidates, &project_dirs, false)
        .await
    {
        Ok(file_path) => Ok(vec![file_path]),
        Err(file_err) => {
            let dir_candidates = tool
                .files
                .correct_to_nearest_dir_path(&scope_string, false, 10)
                .await;
            match tool
                .files
                .return_one_candidate_or_a_good_error(
                    &scope_string,
                    &dir_candidates,
                    &project_dirs,
                    true,
                )
                .await
            {
                Ok(dir_path) => {
                    let dir_path_with_sep = if dir_path.ends_with(std::path::MAIN_SEPARATOR) {
                        dir_path.clone()
                    } else {
                        format!("{}{}", dir_path, std::path::MAIN_SEPARATOR)
                    };
                    Ok(get_workspace_files(tool)
                        .await
                        .into_iter()
                        .filter(|f| {
                            f.to_string_lossy().starts_with(&dir_path_with_sep)
                                || f.to_string_lossy() == dir_path
                        })
                        .map(|f| path_to_string(&f))
                        .collect())
                }
                Err(_) => Err(file_err),
            }
        }
    }
}

#[allow(dead_code)]
pub async fn resolve_scope(tool: &ToolCtx, scope: &str) -> Result<Vec<String>, String> {
    resolve_scope_legacy(tool, scope).await
}

pub async fn resolve_scope_with_execution_scope(
    tool: &ToolCtx,
    execution_scope: Option<&ExecutionScope>,
    scope: &str,
) -> Result<ScopedFiles, String> {
    let Some(execution_scope) = execution_scope else {
        return Ok(ScopedFiles {
            files: resolve_scope_legacy(tool, scope).await?,
            notices: vec![],
        });
    };
    if !execution_scope.is_enforced() {
        return Ok(ScopedFiles {
            files: resolve_scope_legacy(tool, scope).await?,
            notices: vec![],
        });
    }

    if is_worktree_root_alias(scope) {
        let files = list_execution_scope_root(tool, execution_scope, true)
            .await?
            .into_iter()
            .map(|file| path_to_string(&file))
            .collect();
        return Ok(ScopedFiles {
            files,
            notices: vec![],
        });
    }

    let scope_is_dir = scope.ends_with('/') || scope.ends_with('\\');
    let scoped = execution_scope.resolve_existing_path(&PathBuf::from(scope))?;
    if scoped.path.is_file() {
        if scope_is_dir {
            return Err(format!(
                "⚠️ '{}' is a file, not a directory. 💡 Remove the trailing slash or use a directory scope",
                scope
            ));
        }
        tool.files.check_file_privacy_for_send(&scoped.path).await?;
        return Ok(ScopedFiles {
            files: vec![path_to_string(&scoped.path)],
            notices: scoped_path_notices(&scoped),
        });
    }
    if !scoped.path.is_dir() {
        return Err(format!(
            "Path '{}' is not a file or directory",
            scoped.path.display()
        ));
    }

    let files = list_files_under_dir(tool, &scoped.path, true, true)
        .await?
        .into_iter()
        .map(|file| path_to_string(&file))
        .collect();

    Ok(ScopedFiles {
        files,
        notices: scoped_path_notices(&scoped),
    })
}

pub async fn resolve_scope_with_execution_scope_limited(
    tool: &ToolCtx,
    execution_scope: Option<&ExecutionScope>,
    scope: &str,
    max_entries: usize,
    abort: Option<&std::sync::atomic::AtomicBool>,
) -> Result<ScopedFiles, String> {
    resolve_scope_with_execution_scope_limited_impl(
        tool,
        execution_scope,
        scope,
        max_entries,
        abort,
        false,
    )
    .await
}

pub async fn resolve_scope_with_execution_scope_limited_for_model_context(
    tool: &ToolCtx,
    execution_scope: Option<&ExecutionScope>,
    scope: &str,
    max_entries: usize,
    abort: Option<&std::sync::atomic::AtomicBool>,
) -> Result<ScopedFiles, String> {
    resolve_scope_with_execution_scope_limited_impl(
        tool,
        execution_scope,
        scope,
        max_entries,
        abort,
        true,
    )
    .await
}

async fn resolve_scope_with_execution_scope_limited_impl(
    tool: &ToolCtx,
    execution_scope: Option<&ExecutionScope>,
    scope: &str,
    max_entries: usize,
    abort: Option<&std::sync::atomic::AtomicBool>,
    for_model_context: bool,
) -> Result<ScopedFiles, String> {
    let Some(execution_scope) = execution_scope else {
        if for_model_context {
            let scoped =
                resolve_scope_legacy_limited(tool, scope, max_entries, abort).await?;
            let files = tool
                .files
                .filter_privacy_allowed_files_for_model_context(
                    scoped.files.into_iter().map(PathBuf::from).collect(),
                )
                .await
                .into_iter()
                .map(|path| path_to_string(&path))
                .collect();
            return Ok(ScopedFiles {
                files,
                notices: scoped.notices,
            });
        }
        return resolve_scope_legacy_limited(tool, scope, max_entries, abort).await;
    };
    if !execution_scope.is_enforced() {
        if for_model_context {
            let scoped =
                resolve_scope_legacy_limited(tool, scope, max_entries, abort).await?;
            let files = tool
                .files
                .filter_privacy_allowed_files_for_model_context(
                    scoped.files.into_iter().map(PathBuf::from).collect(),
                )
                .await
                .into_iter()
                .map(|path| path_to_string(&path))
                .collect();
            return Ok(ScopedFiles {
                files,
                notices: scoped.notices,
            });
        }
        return resolve_scope_legacy_limited(tool, scope, max_entries, abort).await;
    }

    if is_worktree_root_alias(scope) {
        execution_scope.ensure_active_root()?;
        let root = execution_scope.effective_root().to_path_buf();
        let listing = if for_model_context {
            list_files_under_dir_limited_for_model_context(tool, &root, true, max_entries, abort)
                .await?
        } else {
            list_files_under_dir_limited(tool, &root, true, true, false, max_entries, abort).await?
        };
        let mut notices = vec![];
        if listing.truncated {
            notices.push(truncation_notice(max_entries));
        }
        return Ok(ScopedFiles {
            files: listing
                .files
                .into_iter()
                .map(|file| path_to_string(&file))
                .collect(),
            notices,
        });
    }

    let scope_is_dir = scope.ends_with('/') || scope.ends_with('\\');
    let scoped = execution_scope.resolve_existing_path(&PathBuf::from(scope))?;
    if scoped.path.is_file() {
        if scope_is_dir {
            return Err(format!(
                "⚠️ '{}' is a file, not a directory. 💡 Remove the trailing slash or use a directory scope",
                scope
            ));
        }
        if for_model_context {
            tool.files
                .check_file_privacy_for_model_context(&scoped.path)
                .await?;
        } else {
            tool.files.check_file_privacy_for_send(&scoped.path).await?;
        }
        return Ok(ScopedFiles {
            files: vec![path_to_string(&scoped.path)],
            notices: scoped_path_notices(&scoped),
        });
    }
    if !scoped.path.is_dir() {
        return Err(format!(
            "Path '{}' is not a file or directory",
            scoped.path.display()
        ));
    }

    let listing = if for_model_context {
        list_files_under_dir_limited_for_model_context(
            tool,
            &scoped.path,
            true,
            max_entries,
            abort,
        )
        .await?
    } else {
        list_files_under_dir_limited(tool, &scoped.path, true, true, false, max_entries, abort)
            .await?
    };
    let mut notices = scoped_path_notices(&scoped);
    if listing.truncated {
        notices.push(truncation_notice(max_entries));
    }

    Ok(ScopedFiles {
        files: listing
            .files
            .into_iter()
            .map(|file| path_to_string(&file))
            .collect(),
        notices,
    })
}

pub fn escape_path_for_scope_filter(path: &str) -> String {
    path.replace('"', "")
        .replace('%', r"\%")
        .replace('_', r"\_")
}

async fn create_scope_filter_legacy(
    tool: &ToolCtx,
    scope: &str,
) -> Result<Option<String>, String> {
    if scope == "workspace" {
        return Ok(None);
    }

    let project_dirs = tool.files.project_dirs().await;
    let scope_string = scope.to_string();
    let scope_is_dir = scope.ends_with('/') || scope.ends_with('\\');

    if scope_is_dir {
        let candidates = tool
            .files
            .correct_to_nearest_dir_path(&scope_string, false, 10)
            .await;
        let dir_path = tool
            .files
            .return_one_candidate_or_a_good_error(&scope_string, &candidates, &project_dirs, true)
            .await?;

        let dir_path_with_sep = if dir_path.ends_with(std::path::MAIN_SEPARATOR) {
            dir_path.clone()
        } else {
            format!("{}{}", dir_path, std::path::MAIN_SEPARATOR)
        };
        return Ok(Some(format!(
            r#"(scope LIKE "{}%" ESCAPE '\')"#,
            escape_path_for_scope_filter(&dir_path_with_sep)
        )));
    }

    let file_candidates = tool
        .files
        .file_repair_candidates(&scope_string, 10, false)
        .await;
    match tool
        .files
        .return_one_candidate_or_a_good_error(&scope_string, &file_candidates, &project_dirs, false)
        .await
    {
        Ok(file_path) => Ok(Some(format!(
            "(scope = \"{}\")",
            escape_path_for_scope_filter(&file_path)
        ))),
        Err(file_err) => {
            let dir_candidates = tool
                .files
                .correct_to_nearest_dir_path(&scope_string, false, 10)
                .await;
            match tool
                .files
                .return_one_candidate_or_a_good_error(
                    &scope_string,
                    &dir_candidates,
                    &project_dirs,
                    true,
                )
                .await
            {
                Ok(dir_path) => {
                    let dir_path_with_sep = if dir_path.ends_with(std::path::MAIN_SEPARATOR) {
                        dir_path.clone()
                    } else {
                        format!("{}{}", dir_path, std::path::MAIN_SEPARATOR)
                    };
                    Ok(Some(format!(
                        r#"(scope LIKE "{}%" ESCAPE '\')"#,
                        escape_path_for_scope_filter(&dir_path_with_sep)
                    )))
                }
                Err(_) => Err(file_err),
            }
        }
    }
}

#[allow(dead_code)]
pub async fn create_scope_filter(
    tool: &ToolCtx,
    scope: &str,
) -> Result<Option<String>, String> {
    create_scope_filter_legacy(tool, scope).await
}

fn indexed_path_for_scoped_path(execution_scope: &ExecutionScope, path: &Path) -> PathBuf {
    if path.starts_with(execution_scope.effective_root()) {
        if let Ok(relative) = path.strip_prefix(execution_scope.effective_root()) {
            return execution_scope.source_workspace_root().join(relative);
        }
    }
    path.to_path_buf()
}

pub fn is_worktree_only_path(execution_scope: &ExecutionScope, path: &Path) -> bool {
    if !path.starts_with(execution_scope.effective_root()) {
        return false;
    }
    let Ok(relative) = path.strip_prefix(execution_scope.effective_root()) else {
        return false;
    };
    !execution_scope
        .source_workspace_root()
        .join(relative)
        .is_file()
}

pub struct WorktreeOnlyListingLimited {
    pub files: Vec<String>,
    pub candidate_truncated: bool,
    pub traversal_truncated: bool,
}

/// Resolve a scope with a hard traversal budget, then apply the candidate cap only after
/// privacy/ignore filtering and source-to-worktree mapping have established that a file exists
/// solely in the active worktree.
pub async fn resolve_worktree_only_scope_limited(
    tool: &ToolCtx,
    execution_scope: &ExecutionScope,
    scope: &str,
    max_candidates: usize,
    max_visited_entries: usize,
    abort: Option<&std::sync::atomic::AtomicBool>,
) -> Result<WorktreeOnlyListingLimited, String> {
    let traversal_notice = truncation_notice(max_visited_entries);
    let scoped = resolve_scope_with_execution_scope_limited(
        tool,
        Some(execution_scope),
        scope,
        max_visited_entries,
        abort,
    )
    .await?;
    let traversal_truncated = scoped
        .notices
        .iter()
        .any(|notice| notice == &traversal_notice);
    let mut files = Vec::new();
    for (idx, file) in scoped.files.into_iter().enumerate() {
        if (idx & 0x3f) == 0
            && abort.is_some_and(|flag| flag.load(std::sync::atomic::Ordering::Relaxed))
        {
            return Err("⚠️ Worktree-only file listing was aborted before completion.".to_string());
        }
        if is_worktree_only_path(execution_scope, Path::new(&file)) {
            files.push(file);
        }
    }
    files.sort();
    let candidate_truncated = files.len() > max_candidates;
    files.truncate(max_candidates);

    Ok(WorktreeOnlyListingLimited {
        files,
        candidate_truncated,
        traversal_truncated,
    })
}

pub async fn create_scope_filter_with_execution_scope(
    tool: &ToolCtx,
    execution_scope: Option<&ExecutionScope>,
    scope: &str,
) -> Result<ScopedScopeFilter, String> {
    create_scope_filter_with_execution_scope_impl(tool, execution_scope, scope, false).await
}

pub async fn create_scope_filter_with_execution_scope_for_model_context(
    tool: &ToolCtx,
    execution_scope: Option<&ExecutionScope>,
    scope: &str,
) -> Result<ScopedScopeFilter, String> {
    create_scope_filter_with_execution_scope_impl(tool, execution_scope, scope, true).await
}

async fn create_scope_filter_with_execution_scope_impl(
    tool: &ToolCtx,
    execution_scope: Option<&ExecutionScope>,
    scope: &str,
    for_model_context: bool,
) -> Result<ScopedScopeFilter, String> {
    let Some(execution_scope) = execution_scope else {
        return Ok(ScopedScopeFilter {
            filter: create_scope_filter_legacy(tool, scope).await?,
            notices: vec![],
        });
    };
    if !execution_scope.is_enforced() {
        return Ok(ScopedScopeFilter {
            filter: create_scope_filter_legacy(tool, scope).await?,
            notices: vec![],
        });
    }

    execution_scope.ensure_active_root()?;
    if is_worktree_root_alias(scope) {
        return Ok(ScopedScopeFilter {
            filter: Some(format!(
                r#"(scope LIKE "{}%" ESCAPE '\')"#,
                escape_path_for_scope_filter(&path_with_sep(
                    execution_scope.source_workspace_root()
                ))
            )),
            notices: vec![],
        });
    }

    let scope_is_dir = scope.ends_with('/') || scope.ends_with('\\');
    let scoped = execution_scope.resolve_existing_path(&PathBuf::from(scope))?;
    if scoped.path.is_file() {
        if for_model_context {
            tool.files
                .check_file_privacy_for_model_context(&scoped.path)
                .await?;
        } else {
            tool.files.check_file_privacy_for_send(&scoped.path).await?;
        }
    }
    let indexed_path = indexed_path_for_scoped_path(execution_scope, &scoped.path);
    let filter = if scoped.path.is_dir() || scope_is_dir {
        Some(format!(
            r#"(scope LIKE "{}%" ESCAPE '\')"#,
            escape_path_for_scope_filter(&path_with_sep(&indexed_path))
        ))
    } else {
        Some(format!(
            "(scope = \"{}\")",
            escape_path_for_scope_filter(&indexed_path.to_string_lossy())
        ))
    };
    Ok(ScopedScopeFilter {
        filter,
        notices: scoped_path_notices(&scoped),
    })
}

pub async fn remap_context_file_for_execution_scope(
    tool: &ToolCtx,
    execution_scope: Option<&ExecutionScope>,
    context_file: ContextFile,
) -> Result<Option<(ContextFile, Vec<String>)>, String> {
    remap_context_file_for_execution_scope_impl(tool, execution_scope, context_file, false).await
}

pub async fn remap_context_file_for_execution_scope_for_model_context(
    tool: &ToolCtx,
    execution_scope: Option<&ExecutionScope>,
    context_file: ContextFile,
) -> Result<Option<(ContextFile, Vec<String>)>, String> {
    remap_context_file_for_execution_scope_impl(tool, execution_scope, context_file, true).await
}

async fn remap_context_file_for_execution_scope_impl(
    tool: &ToolCtx,
    execution_scope: Option<&ExecutionScope>,
    mut context_file: ContextFile,
    for_model_context: bool,
) -> Result<Option<(ContextFile, Vec<String>)>, String> {
    let Some(execution_scope) = execution_scope else {
        if for_model_context {
            tool.files
                .check_file_privacy_for_model_context(&PathBuf::from(&context_file.file_name))
                .await?;
        }
        return Ok(Some((context_file, vec![])));
    };
    if !execution_scope.is_enforced() {
        if for_model_context {
            tool.files
                .check_file_privacy_for_model_context(&PathBuf::from(&context_file.file_name))
                .await?;
        }
        return Ok(Some((context_file, vec![])));
    }

    execution_scope.ensure_active_root()?;
    let raw_path = PathBuf::from(&context_file.file_name);
    if !raw_path.is_absolute() {
        return Ok(Some((context_file, vec![])));
    }
    let normalized_path =
        dunce::simplified(&canonical_path(context_file.file_name.clone())).to_path_buf();

    if normalized_path.starts_with(execution_scope.effective_root()) {
        if for_model_context {
            tool.files
                .check_file_privacy_for_model_context(&normalized_path)
                .await?;
        } else {
            tool.files.check_file_privacy_for_send(&normalized_path).await?;
        }
        context_file.file_name = path_to_string(&normalized_path);
        return Ok(Some((context_file, vec![])));
    }

    for source_root in [
        execution_scope.source_workspace_root().to_path_buf(),
        execution_scope.repo_root().to_path_buf(),
    ] {
        if normalized_path.starts_with(&source_root) {
            if let Ok(relative) = normalized_path.strip_prefix(&source_root) {
                let worktree_path = execution_scope.effective_root().join(relative);
                if worktree_path.is_file() {
                    let worktree_path = dunce::simplified(&canonical_path(
                        worktree_path.to_string_lossy().to_string(),
                    ))
                    .to_path_buf();
                    if for_model_context {
                        tool.files
                            .check_file_privacy_for_model_context(&worktree_path)
                            .await?;
                    } else {
                        tool.files.check_file_privacy_for_send(&worktree_path).await?;
                    }
                    let notice = format!(
                        "⚠️ AST/VecDB result was mapped from source checkout to active worktree: {} -> {}",
                        normalized_path.display(),
                        worktree_path.display()
                    );
                    context_file.file_name = path_to_string(&worktree_path);
                    return Ok(Some((context_file, vec![notice])));
                }
            }
            return Ok(None);
        }
    }

    if for_model_context {
        tool.files
            .check_file_privacy_for_model_context(&normalized_path)
            .await?;
    } else {
        tool.files.check_file_privacy_for_send(&normalized_path).await?;
    }
    context_file.file_name = path_to_string(&normalized_path);
    Ok(Some((
        context_file,
        vec![format!(
            "⚠️ STRONG NOTICE: result path is outside active worktree; content comes from outside active worktree: {}",
            normalized_path.display()
        )],
    )))
}

pub async fn remap_context_files_for_execution_scope(
    tool: &ToolCtx,
    execution_scope: Option<&ExecutionScope>,
    context_files: Vec<ContextFile>,
) -> Result<(Vec<ContextFile>, Vec<String>), String> {
    remap_context_files_for_execution_scope_impl(tool, execution_scope, context_files, false).await
}

pub async fn remap_context_files_for_execution_scope_for_model_context(
    tool: &ToolCtx,
    execution_scope: Option<&ExecutionScope>,
    context_files: Vec<ContextFile>,
) -> Result<(Vec<ContextFile>, Vec<String>), String> {
    remap_context_files_for_execution_scope_impl(tool, execution_scope, context_files, true).await
}

async fn remap_context_files_for_execution_scope_impl(
    tool: &ToolCtx,
    execution_scope: Option<&ExecutionScope>,
    context_files: Vec<ContextFile>,
    for_model_context: bool,
) -> Result<(Vec<ContextFile>, Vec<String>), String> {
    let mut remapped = Vec::new();
    let mut notices = Vec::new();
    let mut seen = HashSet::new();

    for context_file in context_files {
        if let Some((context_file, mut file_notices)) = remap_context_file_for_execution_scope_impl(
            tool,
            execution_scope,
            context_file,
            for_model_context,
        )
        .await?
        {
            let key = format!(
                "{}:{}:{}:{:?}",
                context_file.file_name,
                context_file.line1,
                context_file.line2,
                context_file.symbols
            );
            if seen.insert(key) {
                remapped.push(context_file);
            }
            notices.append(&mut file_notices);
        }
    }

    Ok((remapped, dedup_notices(notices)))
}

pub fn validate_scope_files(files: Vec<String>, scope: &str) -> Result<Vec<String>, String> {
    if files.is_empty() {
        Err(format!(
            "⚠️ No files found in scope '{}'. 💡 Use 'workspace' for all files, 'dir/' (trailing slash) for directories, or check path exists",
            scope
        ))
    } else {
        Ok(files)
    }
}

