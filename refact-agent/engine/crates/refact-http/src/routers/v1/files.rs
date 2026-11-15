use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::UNIX_EPOCH;

use axum::extract::{Query, State};
use axum::Json;
use git2::Repository;
use hyper::StatusCode;
use serde::{Deserialize, Serialize};

use refact_runtime_api::app_state::AppState;
use refact_core::custom_error::ScratchError;
use refact_files::indexing_settings::{is_blocklisted, IndexingEverywhere};
use refact_runtime_api::files_correction::{
    check_if_its_inside_a_workspace_worktree_or_config, get_unscoped_project_dirs,
};
use refact_worktrees::path_mappings::{
    registered_worktree_path_mappings, WorktreePathMappings,
};
use refact_runtime_api::files_in_workspace::{
    check_file_privacy_for_send, strictest_zone_for_path,
};
use refact_runtime_api::global_context::GlobalContext;

pub const PRIVACY_BLOCKED_PREFIX: &str = "Blocked by privacy rules:";

pub const DEFAULT_MAX_ENTRIES: usize = 2_000;
pub const MAX_ENTRIES: usize = 5_000;
pub const MAX_CONTENT_BYTES: usize = 1024 * 1024;
pub const BINARY_PROBE_BYTES: usize = 8 * 1024;

#[derive(Debug, Deserialize)]
pub struct TreeQuery {
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    max_entries: Option<usize>,
}

#[derive(Debug, Serialize)]
pub struct TreeEntry {
    name: String,
    path: String,
    kind: &'static str,
    size: Option<u64>,
    #[serde(default)]
    ignored: bool,
    privacy_zone: ResolvedPrivacyZone,
}

#[derive(Debug, Serialize)]
pub struct ResolvedPrivacyZone {
    name: String,
    send_to: Vec<String>,
    on_shell_read: refact_privacy::ShellBehavior,
}

impl From<refact_privacy::Zone> for ResolvedPrivacyZone {
    fn from(zone: refact_privacy::Zone) -> Self {
        Self {
            name: zone.name,
            send_to: zone.send_to,
            on_shell_read: zone.on_shell_read,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct TreeResponse {
    path: String,
    entries: Vec<TreeEntry>,
    truncated: bool,
}

#[derive(Debug, Deserialize)]
pub struct ReadQuery {
    path: String,
    #[serde(default)]
    line_start: Option<usize>,
    #[serde(default)]
    line_end: Option<usize>,
}

#[derive(Debug, Deserialize)]
pub struct WriteRequest {
    path: String,
    content: String,
    #[serde(default)]
    expected_mtime_ms: Option<u64>,
}

#[derive(Debug, Serialize)]
pub struct WriteResponse {
    path: String,
    size: u64,
    mtime_ms: u64,
}

#[derive(Debug, Serialize)]
pub struct ReadResponse {
    path: String,
    content: String,
    language: Option<String>,
    size: u64,
    truncated: bool,
    line_start: Option<usize>,
    line_end: Option<usize>,
    mtime_ms: u64,
    #[serde(skip_serializing_if = "is_false")]
    binary: bool,
}

fn is_false(value: &bool) -> bool {
    !*value
}

fn io_error(status: StatusCode, action: &str, path: &Path, error: std::io::Error) -> ScratchError {
    ScratchError::new(
        status,
        format!("Failed to {action} '{}': {error}", path.display()),
    )
}

async fn validated_existing_path(
    gcx: Arc<GlobalContext>,
    requested: &Path,
) -> Result<PathBuf, ScratchError> {
    if !requested.is_absolute() {
        return Err(ScratchError::new(
            StatusCode::BAD_REQUEST,
            "Path must be absolute".to_string(),
        ));
    }
    check_if_its_inside_a_workspace_worktree_or_config(gcx.clone(), requested)
        .await
        .map_err(|error| ScratchError::new(StatusCode::FORBIDDEN, error))?;
    let canonical = tokio::fs::canonicalize(requested)
        .await
        .map(|path| dunce::simplified(&path).to_path_buf())
        .map_err(|error| io_error(StatusCode::NOT_FOUND, "resolve path", requested, error))?;
    check_if_its_inside_a_workspace_worktree_or_config(gcx, &canonical)
        .await
        .map_err(|error| ScratchError::new(StatusCode::FORBIDDEN, error))?;
    Ok(canonical)
}

async fn privacy_checked_path(gcx: Arc<GlobalContext>, path: &PathBuf) -> Result<(), ScratchError> {
    check_file_privacy_for_send(gcx, path)
        .await
        .map_err(|error| {
            ScratchError::new(
                StatusCode::FORBIDDEN,
                format!("{PRIVACY_BLOCKED_PREFIX} {error}"),
            )
        })
}

pub fn clamped_tree_limit(max_entries: Option<usize>) -> usize {
    max_entries.unwrap_or(DEFAULT_MAX_ENTRIES).min(MAX_ENTRIES)
}

fn hidden_or_heavy_directory(name: &str, is_dir: bool) -> bool {
    is_dir && (name.starts_with('.') || matches!(name, "node_modules" | "target"))
}

fn blocklisted_entry(
    indexing: &IndexingEverywhere,
    directory: &Path,
    path: &Path,
    is_dir: bool,
) -> bool {
    let settings = indexing.indexing_for_path(path);
    let relative = path.strip_prefix(directory).unwrap_or(path);
    if is_blocklisted(&settings, relative) {
        return true;
    }
    if is_dir {
        let relative_child = relative.join("__refact_files_entry__");
        return is_blocklisted(&settings, &relative_child);
    }
    false
}

fn sort_entries(entries: &mut [TreeEntry]) {
    entries.sort_by(|left, right| {
        let left_dir = left.kind == "dir";
        let right_dir = right.kind == "dir";
        right_dir.cmp(&left_dir).then_with(|| {
            left.name
                .to_lowercase()
                .cmp(&right.name.to_lowercase())
                .then_with(|| left.name.cmp(&right.name))
        })
    });
}

struct TreePrivacyResolver {
    policy: refact_privacy::CompiledPolicy,
    workspace_roots: Vec<PathBuf>,
    worktree_mappings: WorktreePathMappings,
}

impl TreePrivacyResolver {
    async fn new(gcx: Arc<GlobalContext>) -> Result<Self, ScratchError> {
        refact_runtime_api::privacy::load_privacy_if_needed(gcx.clone()).await;
        let policy = gcx.privacy_policy_load.read().unwrap().policy.clone();
        let compiled = policy.compile().map_err(|error| {
            ScratchError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to compile privacy policy: {error}"),
            )
        })?;
        Ok(Self {
            policy: compiled,
            workspace_roots: get_unscoped_project_dirs(gcx.clone()).await,
            worktree_mappings: registered_worktree_path_mappings(gcx.cache_dir.as_path()),
        })
    }

    fn zone_for_path(&self, path: &Path) -> ResolvedPrivacyZone {
        strictest_zone_for_path(
            &self.policy,
            path,
            &self.workspace_roots,
            &self.worktree_mappings,
        )
        .into()
    }
}

async fn workspace_root_entries(
    roots: &[PathBuf],
    privacy: &TreePrivacyResolver,
) -> Result<Vec<TreeEntry>, ScratchError> {
    let mut entries = Vec::new();
    for root in roots {
        let canonical = tokio::fs::canonicalize(root)
            .await
            .map(|path| dunce::simplified(&path).to_path_buf())
            .map_err(|error| {
                io_error(
                    StatusCode::NOT_FOUND,
                    "resolve workspace root",
                    &root,
                    error,
                )
            })?;
        let name = canonical
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| canonical.to_string_lossy().to_string());
        entries.push(TreeEntry {
            name,
            path: canonical.to_string_lossy().to_string(),
            kind: "dir",
            size: None,
            ignored: false,
            privacy_zone: privacy.zone_for_path(&canonical),
        });
    }
    sort_entries(&mut entries);
    Ok(entries)
}

async fn list_dir_core(
    directory: &Path,
    indexing: &IndexingEverywhere,
    privacy: &TreePrivacyResolver,
) -> Result<Vec<TreeEntry>, ScratchError> {
    let repository = Repository::discover(directory).ok();
    let mut read_dir = tokio::fs::read_dir(directory).await.map_err(|error| {
        io_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "list directory",
            directory,
            error,
        )
    })?;
    let mut entries = Vec::new();
    while let Some(entry) = read_dir.next_entry().await.map_err(|error| {
        io_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "list directory",
            directory,
            error,
        )
    })? {
        let name = entry.file_name().to_string_lossy().to_string();
        let file_type = entry.file_type().await.map_err(|error| {
            io_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "inspect entry",
                &entry.path(),
                error,
            )
        })?;
        if file_type.is_symlink() || (!file_type.is_dir() && !file_type.is_file()) {
            continue;
        }
        let path = entry.path();
        let is_dir = file_type.is_dir();
        if hidden_or_heavy_directory(&name, is_dir) {
            continue;
        }
        if blocklisted_entry(indexing, directory, &path, is_dir) {
            continue;
        }
        let ignored = repository
            .as_ref()
            .and_then(|repository| repository.workdir().map(|workdir| (repository, workdir)))
            .and_then(|(repository, workdir)| {
                path.strip_prefix(workdir)
                    .ok()
                    .map(|path| (repository, path))
            })
            .and_then(|(repository, path)| repository.status_should_ignore(path).ok())
            .unwrap_or(false);
        let size = if is_dir {
            None
        } else {
            Some(
                entry
                    .metadata()
                    .await
                    .map_err(|error| {
                        io_error(
                            StatusCode::INTERNAL_SERVER_ERROR,
                            "inspect entry",
                            &path,
                            error,
                        )
                    })?
                    .len(),
            )
        };
        entries.push(TreeEntry {
            name,
            path: path.to_string_lossy().to_string(),
            kind: if is_dir { "dir" } else { "file" },
            size,
            ignored,
            privacy_zone: privacy.zone_for_path(&path),
        });
    }
    sort_entries(&mut entries);
    Ok(entries)
}

fn truncate_entries(mut entries: Vec<TreeEntry>, limit: usize) -> (Vec<TreeEntry>, bool) {
    let truncated = entries.len() > limit;
    entries.truncate(limit);
    (entries, truncated)
}

fn validate_line_range(
    line_start: Option<usize>,
    line_end: Option<usize>,
) -> Result<(), ScratchError> {
    if line_start == Some(0) || line_end == Some(0) {
        return Err(ScratchError::new(
            StatusCode::BAD_REQUEST,
            "Line numbers are 1-based".to_string(),
        ));
    }
    if matches!((line_start, line_end), (Some(start), Some(end)) if start > end) {
        return Err(ScratchError::new(
            StatusCode::BAD_REQUEST,
            "line_start must not exceed line_end".to_string(),
        ));
    }
    Ok(())
}

fn slice_lines(text: &str, line_start: Option<usize>, line_end: Option<usize>) -> String {
    if line_start.is_none() && line_end.is_none() {
        return text.to_string();
    }
    let start = line_start.unwrap_or(1);
    let end = line_end.unwrap_or(usize::MAX);
    text.split_inclusive('\n')
        .enumerate()
        .filter_map(|(index, line)| {
            let line_number = index + 1;
            (line_number >= start && line_number <= end).then_some(line)
        })
        .collect()
}

fn truncate_content(mut content: String) -> (String, bool) {
    if content.len() <= MAX_CONTENT_BYTES {
        return (content, false);
    }
    let mut end = MAX_CONTENT_BYTES;
    while !content.is_char_boundary(end) {
        end -= 1;
    }
    content.truncate(end);
    (content, true)
}

fn language_for_path(path: &Path) -> Option<String> {
    let file_name = path.file_name()?.to_string_lossy().to_lowercase();
    let language = match file_name.as_str() {
        "dockerfile" => "dockerfile",
        "makefile" => "makefile",
        _ => match path.extension()?.to_string_lossy().to_lowercase().as_str() {
            "c" | "h" => "c",
            "cc" | "cpp" | "cxx" | "hpp" | "hxx" => "cpp",
            "cs" => "csharp",
            "css" => "css",
            "go" => "go",
            "html" | "htm" => "html",
            "java" => "java",
            "js" | "cjs" | "mjs" => "javascript",
            "jsx" => "javascriptreact",
            "json" => "json",
            "kt" | "kts" => "kotlin",
            "lua" => "lua",
            "md" | "mdx" => "markdown",
            "php" => "php",
            "py" => "python",
            "r" => "r",
            "rb" => "ruby",
            "rs" => "rust",
            "scss" => "scss",
            "sh" | "bash" | "zsh" => "shellscript",
            "sql" => "sql",
            "swift" => "swift",
            "toml" => "toml",
            "txt" => "plaintext",
            "ts" => "typescript",
            "tsx" => "typescriptreact",
            "xml" => "xml",
            "yaml" | "yml" => "yaml",
            _ => return None,
        },
    };
    Some(language.to_string())
}

fn mtime_ms(metadata: &std::fs::Metadata) -> u64 {
    metadata
        .modified()
        .ok()
        .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_millis().min(u64::MAX as u128) as u64)
        .unwrap_or(0)
}

async fn read_file_core(
    path: &Path,
    line_start: Option<usize>,
    line_end: Option<usize>,
) -> Result<ReadResponse, ScratchError> {
    validate_line_range(line_start, line_end)?;
    let metadata = tokio::fs::metadata(path)
        .await
        .map_err(|error| io_error(StatusCode::NOT_FOUND, "inspect file", path, error))?;
    if !metadata.is_file() {
        return Err(ScratchError::new(
            StatusCode::BAD_REQUEST,
            format!("Path '{}' is not a file", path.display()),
        ));
    }
    let bytes = tokio::fs::read(path)
        .await
        .map_err(|error| io_error(StatusCode::INTERNAL_SERVER_ERROR, "read file", path, error))?;
    let binary = bytes[..bytes.len().min(BINARY_PROBE_BYTES)].contains(&0);
    let text = if binary {
        None
    } else {
        std::str::from_utf8(&bytes).ok()
    };
    let language = language_for_path(path);
    let path = path.to_string_lossy().to_string();
    if let Some(text) = text {
        let (content, truncated) = truncate_content(slice_lines(text, line_start, line_end));
        return Ok(ReadResponse {
            path,
            content,
            language,
            size: metadata.len(),
            truncated,
            line_start,
            line_end,
            mtime_ms: mtime_ms(&metadata),
            binary: false,
        });
    }
    Ok(ReadResponse {
        path,
        content: String::new(),
        language,
        size: metadata.len(),
        truncated: false,
        line_start,
        line_end,
        mtime_ms: mtime_ms(&metadata),
        binary: true,
    })
}

pub async fn handle_v1_files_tree(
    State(app): State<AppState>,
    Query(query): Query<TreeQuery>,
) -> Result<Json<TreeResponse>, ScratchError> {
    let gcx = app.gcx.clone();
    let limit = clamped_tree_limit(query.max_entries);
    let raw_path = query.path.unwrap_or_default();
    let privacy = TreePrivacyResolver::new(gcx.clone()).await?;
    if raw_path.is_empty() {
        let entries = workspace_root_entries(&privacy.workspace_roots, &privacy).await?;
        let (entries, truncated) = truncate_entries(entries, limit);
        return Ok(Json(TreeResponse {
            path: String::new(),
            entries,
            truncated,
        }));
    }
    let path = validated_existing_path(gcx.clone(), Path::new(&raw_path)).await?;
    if !path.is_dir() {
        return Err(ScratchError::new(
            StatusCode::BAD_REQUEST,
            format!("Path '{}' is not a directory", path.display()),
        ));
    }
    let indexing = refact_runtime_api::files_blocklist::reload_indexing_everywhere_if_needed(gcx).await;
    let entries = list_dir_core(&path, &indexing, &privacy).await?;
    let (entries, truncated) = truncate_entries(entries, limit);
    Ok(Json(TreeResponse {
        path: path.to_string_lossy().to_string(),
        entries,
        truncated,
    }))
}

pub async fn handle_v1_files_read(
    State(app): State<AppState>,
    Query(query): Query<ReadQuery>,
) -> Result<Json<ReadResponse>, ScratchError> {
    validate_line_range(query.line_start, query.line_end)?;
    let requested = PathBuf::from(&query.path);
    let path = validated_existing_path(app.gcx.clone(), &requested).await?;
    privacy_checked_path(app.gcx.clone(), &requested).await?;
    privacy_checked_path(app.gcx, &path).await?;
    Ok(Json(
        read_file_core(&path, query.line_start, query.line_end).await?,
    ))
}

pub async fn handle_v1_files_write(
    State(app): State<AppState>,
    Json(request): Json<WriteRequest>,
) -> Result<Json<WriteResponse>, ScratchError> {
    if request.content.len() > MAX_CONTENT_BYTES {
        return Err(ScratchError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            format!("File content exceeds the {MAX_CONTENT_BYTES} byte editing limit"),
        ));
    }
    let requested = PathBuf::from(&request.path);
    let path = validated_existing_path(app.gcx.clone(), &requested).await?;
    privacy_checked_path(app.gcx.clone(), &requested).await?;
    privacy_checked_path(app.gcx.clone(), &path).await?;

    let metadata = tokio::fs::metadata(&path)
        .await
        .map_err(|error| io_error(StatusCode::NOT_FOUND, "inspect file", &path, error))?;
    if !metadata.is_file() {
        return Err(ScratchError::new(
            StatusCode::BAD_REQUEST,
            format!("Path '{}' is not a file", path.display()),
        ));
    }
    if let Some(expected_mtime_ms) = request.expected_mtime_ms {
        let current_mtime_ms = mtime_ms(&metadata);
        if current_mtime_ms != expected_mtime_ms {
            return Err(ScratchError::new(
                StatusCode::CONFLICT,
                format!(
                    "File '{}' changed on disk since it was loaded",
                    path.display()
                ),
            ));
        }
    }

    refact_tools::tools::file_edit::auxiliary::write_file(
        app.gcx.clone(),
        &path,
        &request.content,
        false,
        None,
    )
    .await
    .map_err(|error| ScratchError::new(StatusCode::INTERNAL_SERVER_ERROR, error))?;

    let metadata = tokio::fs::metadata(&path).await.map_err(|error| {
        io_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "inspect file",
            &path,
            error,
        )
    })?;
    Ok(Json(WriteResponse {
        path: path.to_string_lossy().to_string(),
        size: metadata.len(),
        mtime_ms: mtime_ms(&metadata),
    }))
}
