use refact_runtime_api::AtCommandsContext;
use refact_runtime_api::GlobalContextCtx;
use refact_runtime_api::at_command_executor::{
    file_repair_candidates, return_one_candidate_or_a_good_error,
};
use refact_core::diff_chunk::DiffChunk;
use refact_files::path_utils::{canonicalize_normalized_path, preprocess_path_for_normalization};
use refact_runtime_api::files_correction::{
    check_if_its_inside_a_workspace_or_config, correct_to_nearest_dir_path,
    get_unscoped_project_dirs, resolve_codegraph_queue_path,
};
use refact_worktrees::path_mappings::registered_worktree_path_mappings;
use refact_runtime_api::files_in_workspace::get_file_text_from_memory_or_disk;
use refact_runtime_api::global_context::GlobalContext;
use refact_privacy::check_file_privacy;
use refact_core::privacy_types::{FilePrivacyLevel, PrivacySettings};
use refact_tasks::types::ScopeGuardMode;
use refact_runtime_api::tool_ctx::ToolCtx;
use refact_worktrees::scope::ExecutionScope;
use regex::{Match, Regex};
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::Mutex as AMutex;
use tracing::warn;

pub use refact_file_edit_core::text_edit::{
    edit_result_summary, find_match_lines, insert_at_anchor, normalize_line_endings,
    parse_bool_arg, parse_line_ranges, parse_string_arg, replace_between_anchors,
    restore_line_endings, strip_line_number_prefixes, AnchorMode, LineRange,
};
pub use refact_scope_utils::{
    append_scope_warnings, scope_warnings_to_tool_message, scoped_path_warnings,
};

#[derive(Debug, Clone)]
pub struct ResolvedToolPath {
    pub path: PathBuf,
    pub warnings: Vec<String>,
}

pub fn active_execution_scope(scope: Option<&ExecutionScope>) -> Option<&ExecutionScope> {
    scope.filter(|scope| scope.is_enforced())
}

pub fn resolve_path_with_scope(
    raw_path: &Path,
    privacy_settings: Arc<PrivacySettings>,
    execution_scope: Option<&ExecutionScope>,
    require_existing: bool,
) -> Option<Result<ResolvedToolPath, String>> {
    let scope = active_execution_scope(execution_scope)?;
    let scoped = if require_existing {
        scope.resolve_existing_path(raw_path)
    } else {
        scope.resolve_creatable_path(raw_path)
    };
    let scoped = match scoped {
        Ok(scoped) => scoped,
        Err(e) => {
            return Some(Err(format!(
                "⚠️ Cannot resolve '{}' in active worktree '{}': {}",
                raw_path.display(),
                scope.effective_root().display(),
                e
            )))
        }
    };
    if let Err(e) = check_file_privacy(
        privacy_settings,
        &scoped.path,
        &FilePrivacyLevel::AllowToSendAnywhere,
    ) {
        return Some(Err(format!(
            "⚠️ Cannot access '{}' (blocked by privacy: {}). Active worktree root: '{}'",
            scoped.path.display(),
            e,
            scope.effective_root().display()
        )));
    }
    let warnings = scoped_path_warnings(&scoped, scope);
    Some(Ok(ResolvedToolPath {
        path: scoped.path,
        warnings,
    }))
}

pub async fn check_scope_guard(
    ccx: &Arc<AMutex<AtCommandsContext>>,
    edit_path: &Path,
) -> Result<(), String> {
    let (ctx, task_id, card_id) = {
        let ccx_lock = ccx.lock().await;
        let meta = match ccx_lock.task_meta.as_ref() {
            Some(meta) if meta.role == "agents" => meta.clone(),
            _ => return Ok(()),
        };
        let card_id = match meta.card_id.as_ref() {
            Some(card_id) => card_id.clone(),
            None => return Ok(()),
        };
        (ccx_lock.app.chat_ctx(), meta.task_id, card_id)
    };

    let board = refact_runtime_api::task_storage::load_board_for_ctx(&ctx, &task_id).await?;
    let card = match board.get_card(&card_id) {
        Some(card) => card,
        None => return Ok(()),
    };

    if matches!(card.scope_guard_mode, ScopeGuardMode::Off) {
        return Ok(());
    }
    if card.target_files.is_empty() {
        return Ok(());
    }

    let canon_path = dunce::canonicalize(edit_path)
        .map(|path| dunce::simplified(&path).to_path_buf())
        .unwrap_or_else(|_| edit_path.to_path_buf());
    let canon = canon_path.to_string_lossy().to_string();
    let allowed = card.target_files.iter().any(|target| {
        let target = Path::new(target.trim_start_matches(|c| c == '/' || c == '\\'));
        canon_path.ends_with(target)
    });
    if allowed {
        return Ok(());
    }

    let is_test = canon.contains("/tests/")
        || canon.ends_with("_test.rs")
        || canon.ends_with(".test.ts")
        || canon.ends_with(".test.tsx");
    if is_test {
        return Ok(());
    }

    match card.scope_guard_mode {
        ScopeGuardMode::Warn => {
            tracing::warn!(
                "Scope warning: edited {} not in target_files for card {}",
                canon,
                card_id
            );
            Ok(())
        }
        ScopeGuardMode::Reject => Err(format!(
            "Path {} is outside the card's target_files scope. Add it to target_files or change scope_guard_mode to 'warn'.",
            canon
        )),
        ScopeGuardMode::Off => Ok(()),
    }
}

pub async fn parse_path_for_update(
    gcx: Arc<GlobalContext>,
    args: &HashMap<String, Value>,
    privacy_settings: Arc<PrivacySettings>,
    execution_scope: Option<&ExecutionScope>,
) -> Result<ResolvedToolPath, String> {
    let tool = gcx.tool_ctx(gcx.clone());
    let s = parse_string_arg(args, "path", "Provide absolute path to file")?;
    let raw_path = preprocess_path_for_normalization(s.trim().to_string());
    if let Some(resolved) = resolve_path_with_scope(
        Path::new(&raw_path),
        privacy_settings.clone(),
        execution_scope,
        true,
    ) {
        return resolved;
    }

    let candidates = file_repair_candidates(&tool, &raw_path, 3, false).await;
    let path = return_one_candidate_or_a_good_error(
        &tool,
        &raw_path,
        &candidates,
        &get_unscoped_project_dirs(gcx.clone()).await,
        false,
    )
    .await
    .map(|f| canonicalize_normalized_path(PathBuf::from(f)))?;

    if check_file_privacy(
        privacy_settings,
        &path,
        &FilePrivacyLevel::AllowToSendAnywhere,
    )
    .is_err()
    {
        return Err(format!(
            "⚠️ Cannot update {:?} (blocked by privacy). 💡 Choose file in allowed directory",
            path
        ));
    }
    if !path.exists() {
        return Err(format!(
            "⚠️ File {:?} not found. 💡 Use create_textdoc() for new files",
            path
        ));
    }
    Ok(ResolvedToolPath {
        path,
        warnings: Vec::new(),
    })
}

pub async fn parse_path_for_create(
    gcx: Arc<GlobalContext>,
    args: &HashMap<String, Value>,
    privacy_settings: Arc<PrivacySettings>,
    execution_scope: Option<&ExecutionScope>,
) -> Result<ResolvedToolPath, String> {
    let tool = gcx.tool_ctx(gcx.clone());
    let s = parse_string_arg(args, "path", "Provide absolute path for new file")?;
    let raw_string = preprocess_path_for_normalization(s.trim().to_string());
    let raw_path = PathBuf::from(&raw_string);

    raw_path.file_name().ok_or_else(|| {
        format!(
            "⚠️ Path '{}' has no filename. 💡 Include filename: /path/to/file.ext",
            s.trim()
        )
    })?;

    if let Some(resolved) =
        resolve_path_with_scope(&raw_path, privacy_settings.clone(), execution_scope, false)
    {
        return resolved;
    }

    let filename = raw_path
        .file_name()
        .ok_or_else(|| {
            format!(
                "⚠️ Path '{}' has no filename. 💡 Include filename: /path/to/file.ext",
                s.trim()
            )
        })?
        .to_string_lossy()
        .to_string();

    let path = if !raw_path.is_absolute() {
        if let Some(parent) = raw_path.parent().filter(|p| !p.as_os_str().is_empty()) {
            let parent_str = parent.to_string_lossy().to_string();
            let candidates = correct_to_nearest_dir_path(gcx.clone(), &parent_str, false, 3).await;
            let parent_dir = return_one_candidate_or_a_good_error(
                &tool,
                &parent_str,
                &candidates,
                &get_unscoped_project_dirs(gcx.clone()).await,
                true,
            )
            .await?;
            canonicalize_normalized_path(PathBuf::from(parent_dir).join(&filename))
        } else {
            return Err(format!(
                "⚠️ Path '{}' is not absolute. 💡 Use full path like /project/src/file.ext",
                s.trim()
            ));
        }
    } else {
        let path = canonicalize_normalized_path(raw_path);
        check_if_its_inside_a_workspace_or_config(gcx.clone(), &path).await?;
        path
    };

    if check_file_privacy(
        privacy_settings,
        &path,
        &FilePrivacyLevel::AllowToSendAnywhere,
    )
    .is_err()
    {
        return Err(format!(
            "⚠️ Cannot create {:?} (blocked by privacy). 💡 Choose path in allowed directory",
            path
        ));
    }
    Ok(ResolvedToolPath {
        path,
        warnings: Vec::new(),
    })
}

pub fn convert_edit_to_diffchunks(
    path: PathBuf,
    before: &String,
    after: &String,
) -> Result<Vec<DiffChunk>, String> {
    let before_lines: Vec<&str> = before.lines().collect();
    let after_lines: Vec<&str> = after.lines().collect();
    let diffs = diff::lines(before, after);
    let mut line_num = 0;
    let mut old_line_num = 0;
    let mut new_line_num = 0;
    let mut current_chunk_lines_remove = Vec::new();
    let mut current_chunk_lines_add = Vec::new();
    let mut current_chunk_line_nums = Vec::new();
    let mut current_chunk_old_line_nums = Vec::new();
    let mut current_chunk_new_line_nums = Vec::new();
    let mut current_chunk_is_plus = Vec::new();
    let mut diff_chunks = Vec::new();

    let context_line = |lines: &[&str], index: usize| -> Option<String> {
        lines.get(index).map(|line| format!("{}\n", line))
    };

    let flush_changes = |lines_remove: &Vec<String>,
                         lines_add: &Vec<String>,
                         line_nums: &Vec<usize>,
                         old_line_nums: &Vec<usize>,
                         new_line_nums: &Vec<usize>,
                         is_plus: &Vec<bool>|
     -> Option<DiffChunk> {
        if lines_remove.is_empty() && lines_add.is_empty() {
            return None;
        }

        let lines_remove = lines_remove.join("");
        let lines_add = lines_add.join("");

        let min_line_index = line_nums.iter().min().copied().unwrap_or(0);
        let line1 = min_line_index + 1;

        let max_old_line_index = old_line_nums.iter().max().copied();
        let max_new_line_index = new_line_nums.iter().max().copied();

        let line2 = line_nums
            .iter()
            .zip(is_plus.iter())
            .map(|(&num, &is_plus)| if is_plus { num + 1 } else { num + 2 })
            .max()
            .unwrap_or(1);

        let lines_before = old_line_nums
            .iter()
            .min()
            .or_else(|| new_line_nums.iter().min())
            .and_then(|index| index.checked_sub(1))
            .and_then(|index| context_line(&before_lines, index));
        let after_context_index = max_new_line_index
            .map(|index| index + 1)
            .or_else(|| max_old_line_index.map(|index| index));
        let lines_after = after_context_index.and_then(|index| context_line(&after_lines, index));

        Some(DiffChunk {
            file_name: path.to_string_lossy().to_string(),
            file_name_rename: None,
            file_action: "edit".to_string(),
            line1,
            line2,
            lines_remove,
            lines_add,
            lines_before,
            lines_after,
            ..Default::default()
        })
    };

    for diff in diffs {
        match diff {
            diff::Result::Left(l) => {
                current_chunk_lines_remove.push(format!("{}\n", l));
                current_chunk_line_nums.push(line_num);
                current_chunk_old_line_nums.push(old_line_num);
                current_chunk_is_plus.push(false);
                old_line_num += 1;
                line_num += 1;
            }
            diff::Result::Right(r) => {
                current_chunk_lines_add.push(format!("{}\n", r));
                current_chunk_line_nums.push(line_num);
                current_chunk_new_line_nums.push(new_line_num);
                current_chunk_is_plus.push(true);
                new_line_num += 1;
            }
            diff::Result::Both(_, _) => {
                if let Some(chunk) = flush_changes(
                    &current_chunk_lines_remove,
                    &current_chunk_lines_add,
                    &current_chunk_line_nums,
                    &current_chunk_old_line_nums,
                    &current_chunk_new_line_nums,
                    &current_chunk_is_plus,
                ) {
                    diff_chunks.push(chunk);
                }
                current_chunk_lines_remove.clear();
                current_chunk_lines_add.clear();
                current_chunk_line_nums.clear();
                current_chunk_old_line_nums.clear();
                current_chunk_new_line_nums.clear();
                current_chunk_is_plus.clear();
                old_line_num += 1;
                new_line_num += 1;
                line_num += 1;
            }
        }
    }

    if let Some(chunk) = flush_changes(
        &current_chunk_lines_remove,
        &current_chunk_lines_add,
        &current_chunk_line_nums,
        &current_chunk_old_line_nums,
        &current_chunk_new_line_nums,
        &current_chunk_is_plus,
    ) {
        diff_chunks.push(chunk);
    }

    Ok(diff_chunks)
}

pub async fn fast_enqueue_for_edit(
    gcx: Arc<GlobalContext>,
    docs: &[PathBuf],
) -> Result<(), String> {
    fast_enqueue_for_edit_for_ctx(&gcx.tool_ctx(gcx.clone()), docs).await
}

pub async fn fast_enqueue_for_edit_for_ctx(
    ctx: &ToolCtx,
    docs: &[PathBuf],
) -> Result<(), String> {
    if docs.is_empty() {
        return Ok(());
    }
    let codegraph_mb = ctx.codeintel.codegraph.lock().await.clone();
    if let Some(service) = codegraph_mb {
        let worktree_mappings = registered_worktree_path_mappings(ctx.paths.cache_dir.as_path());
        let paths: Vec<_> = docs
            .iter()
            .map(|doc| resolve_codegraph_queue_path(doc, &worktree_mappings))
            .collect();
        service.enqueue_paths_with_read_paths(&paths);
    }
    Ok(())
}

pub async fn write_file(
    gcx: Arc<GlobalContext>,
    path: &PathBuf,
    file_text: &String,
    dry: bool,
    expected_preimage: Option<&str>,
) -> Result<(String, String), String> {
    write_file_for_ctx(
        &gcx.tool_ctx(gcx.clone()),
        path,
        file_text,
        dry,
        expected_preimage,
    )
    .await
}

pub async fn write_file_for_ctx(
    ctx: &ToolCtx,
    path: &PathBuf,
    file_text: &String,
    dry: bool,
    expected_preimage: Option<&str>,
) -> Result<(String, String), String> {
    use crate::tools::file_edit::undo_history::record_before_edit;

    let parent = path.parent().ok_or(format!(
        "Failed to Add: {:?}. Path is invalid.\nReason: path must have had a parent directory",
        path
    ))?;

    if !parent.exists() {
        if !dry {
            tokio::fs::create_dir_all(&parent).await.map_err(|e| {
                let err = format!("Failed to Add: {:?}; Its parent dir {:?} did not exist and attempt to create it failed.\nERROR: {}", path, parent, e);
                warn!("{err}");
                err
            })?;
        }
    }

    let before_text = if path.exists() {
        ctx.files.get_file_text_from_memory_or_disk(path).await?
    } else {
        "".to_string()
    };

    if let Some(expected) = expected_preimage {
        if normalize_line_endings(&before_text) != normalize_line_endings(expected) {
            return Err(format!(
                "⚠️ {:?} was modified since last read. 💡 Use cat() to re-read the file and retry",
                path
            ));
        }
    }

    if !dry {
        record_before_edit(path, &before_text);
        tokio::fs::write(&path, file_text).await.map_err(|e| {
            let err = format!("Failed to write file: {:?}\nERROR: {}", path, e);
            warn!("{err}");
            err
        })?;
        ctx.files.remove_memory_document_for_path(path).await;
    }

    Ok((before_text, file_text.to_string()))
}

pub async fn str_replace(
    gcx: Arc<GlobalContext>,
    path: &PathBuf,
    old_str: &String,
    new_str: &String,
    replace_multiple: bool,
    dry: bool,
) -> Result<(String, String), String> {
    if old_str.is_empty() {
        return Err("⚠️ old_str cannot be empty. 💡 Provide the exact text to replace".to_string());
    }
    let file_content = get_file_text_from_memory_or_disk(gcx.clone(), path).await?;
    let has_crlf = file_content.contains("\r\n");

    let normalized_content = normalize_line_endings(&file_content);
    let normalized_old_str = strip_line_number_prefixes(&normalize_line_endings(old_str));
    if normalized_old_str.is_empty() {
        return Err("⚠️ old_str is empty after stripping line-number prefixes. 💡 Provide actual source content, not just line numbers".to_string());
    }

    let occurrences = normalized_content.matches(&normalized_old_str).count();
    if occurrences == 0 {
        let trimmed_old = normalized_old_str.trim();
        let trimmed_match = normalized_content.contains(trimmed_old) && !trimmed_old.is_empty();
        let hint = if trimmed_match {
            "Whitespace mismatch detected. 💡 Check leading/trailing spaces, or use update_textdoc_anchored()"
        } else {
            "💡 Use cat() to verify content, or try update_textdoc_anchored() with shorter anchors"
        };
        return Err(format!("⚠️ old_str not found in {:?}. {}", path, hint));
    }
    if !replace_multiple && occurrences > 1 {
        let lines = find_match_lines(&normalized_content, &normalized_old_str);
        return Err(format!(
            "⚠️ {} occurrences at lines {:?}. 💡 Add surrounding context to make unique, or set multiple:true",
            occurrences, lines
        ));
    }

    let normalized_new_str = normalize_line_endings(new_str);
    let new_content = if replace_multiple {
        normalized_content.replace(&normalized_old_str, &normalized_new_str)
    } else {
        normalized_content.replacen(&normalized_old_str, &normalized_new_str, 1)
    };
    let new_file_content = restore_line_endings(&new_content, has_crlf);
    write_file(
        gcx.clone(),
        path,
        &new_file_content,
        dry,
        Some(&file_content),
    )
    .await?;
    Ok((file_content, new_file_content))
}

pub async fn str_replace_anchored(
    gcx: Arc<GlobalContext>,
    path: &PathBuf,
    mode: AnchorMode,
    anchor1: &str,
    anchor2: Option<&str>,
    content: &str,
    multiple: bool,
    dry: bool,
) -> Result<(String, String), String> {
    if anchor1.is_empty() {
        return Err(
            "⚠️ Anchor cannot be empty. 💡 Provide unique text to locate edit position".to_string(),
        );
    }
    let file_content = get_file_text_from_memory_or_disk(gcx.clone(), path).await?;
    let has_crlf = file_content.contains("\r\n");

    let normalized = normalize_line_endings(&file_content);
    let anchor1_n = normalize_line_endings(anchor1);
    let content_n = normalize_line_endings(content);

    let result = match mode {
        AnchorMode::ReplaceBetween => {
            let anchor2_str = anchor2.ok_or("⚠️ anchor_after required for replace_between mode")?;
            if anchor2_str.is_empty() {
                return Err("⚠️ anchor_after cannot be empty".to_string());
            }
            let anchor2_n = normalize_line_endings(anchor2_str);
            replace_between_anchors(&normalized, &anchor1_n, &anchor2_n, &content_n, multiple)?
        }
        AnchorMode::InsertAfter => {
            insert_at_anchor(&normalized, &anchor1_n, &content_n, multiple, true)?
        }
        AnchorMode::InsertBefore => {
            insert_at_anchor(&normalized, &anchor1_n, &content_n, multiple, false)?
        }
    };

    let new_file_content = restore_line_endings(&result, has_crlf);
    write_file(
        gcx.clone(),
        path,
        &new_file_content,
        dry,
        Some(&file_content),
    )
    .await?;
    Ok((file_content, new_file_content))
}

pub async fn str_replace_lines(
    gcx: Arc<GlobalContext>,
    path: &PathBuf,
    new_content: &String,
    ranges_str: &str,
    dry: bool,
) -> Result<(String, String), String> {
    let file_content = get_file_text_from_memory_or_disk(gcx.clone(), path).await?;
    let has_crlf = file_content.contains("\r\n");

    let normalized_content = normalize_line_endings(&file_content);
    let mut lines: Vec<String> = normalized_content.lines().map(|s| s.to_string()).collect();
    let total_lines = lines.len();

    let ranges = parse_line_ranges(ranges_str, total_lines)?;
    let normalized_new_content = normalize_line_endings(new_content);

    if ranges.len() == 1 {
        let range = &ranges[0];
        if range.end > total_lines && !(total_lines == 0 && range.start == 1) {
            return Err(format!(
                "⚠️ Range end {} exceeds file length ({} lines). 💡 Use cat() to check file, or ':' for end",
                range.end, total_lines
            ));
        }
        let start_idx = range.start - 1;
        let end_idx = range.end.min(total_lines);
        let new_lines: Vec<String> = normalized_new_content
            .lines()
            .map(|s| s.to_string())
            .collect();
        lines.splice(start_idx..end_idx, new_lines);
    } else {
        let content_parts: Vec<&str> = normalized_new_content
            .split("---RANGE_SEPARATOR---")
            .collect();

        if content_parts.len() != ranges.len() {
            return Err(format!(
                "⚠️ {} content parts but {} ranges. 💡 Separate content with '---RANGE_SEPARATOR---'",
                content_parts.len(), ranges.len()
            ));
        }

        let mut indexed: Vec<(usize, LineRange)> = ranges.into_iter().enumerate().collect();
        indexed.sort_by(|a, b| b.1.start.cmp(&a.1.start));

        for (orig_idx, range) in indexed {
            if range.end > lines.len() {
                return Err(format!(
                    "⚠️ Range {}:{} exceeds current length ({} lines). 💡 Check ranges",
                    range.start,
                    range.end,
                    lines.len()
                ));
            }
            let start_idx = range.start - 1;
            let end_idx = range.end;
            let new_lines: Vec<String> = content_parts[orig_idx]
                .lines()
                .map(|s| s.to_string())
                .collect();
            lines.splice(start_idx..end_idx, new_lines);
        }
    }

    let new_content_joined = lines.join("\n");
    let new_file_content = if normalized_content.ends_with('\n') {
        restore_line_endings(&format!("{}\n", new_content_joined), has_crlf)
    } else {
        restore_line_endings(&new_content_joined, has_crlf)
    };

    write_file(
        gcx.clone(),
        path,
        &new_file_content,
        dry,
        Some(&file_content),
    )
    .await?;
    Ok((file_content, new_file_content))
}

pub async fn str_replace_regex(
    gcx: Arc<GlobalContext>,
    path: &PathBuf,
    pattern: &Regex,
    replacement: &String,
    multiple: bool,
    expected_matches: Option<usize>,
    literal_replacement: bool,
    dry: bool,
) -> Result<(String, String), String> {
    let file_content = get_file_text_from_memory_or_disk(gcx.clone(), path).await?;
    let has_crlf = file_content.contains("\r\n");

    let normalized_content = normalize_line_endings(&file_content);
    let normalized_replacement = normalize_line_endings(replacement);
    let matches: Vec<Match> = pattern.find_iter(&normalized_content).collect();
    let occurrences = matches.len();

    if occurrences == 0 {
        return Err(format!(
            "⚠️ Pattern not found in {:?}. 💡 Use cat() to check content, try update_textdoc_anchored()",
            path
        ));
    }
    if let Some(expected) = expected_matches {
        if occurrences != expected {
            return Err(format!(
                "⚠️ Expected {} matches, found {}. 💡 Adjust pattern or expected_matches",
                expected, occurrences
            ));
        }
    }
    if !multiple && occurrences > 1 {
        let lines: Vec<usize> = matches
            .iter()
            .map(|m| normalized_content[..m.start()].lines().count() + 1)
            .collect();
        return Err(format!(
            "⚠️ {} matches at lines {:?}. 💡 Make pattern more specific, or set multiple:true",
            occurrences, lines
        ));
    }

    let new_content = if literal_replacement {
        let rep = regex::NoExpand(normalized_replacement.as_str());
        if multiple {
            pattern.replace_all(&normalized_content, rep).to_string()
        } else {
            pattern.replace(&normalized_content, rep).to_string()
        }
    } else if multiple {
        pattern
            .replace_all(&normalized_content, normalized_replacement.as_str())
            .to_string()
    } else {
        pattern
            .replace(&normalized_content, normalized_replacement.as_str())
            .to_string()
    };
    let new_file_content = restore_line_endings(&new_content, has_crlf);
    write_file(
        gcx.clone(),
        path,
        &new_file_content,
        dry,
        Some(&file_content),
    )
    .await?;
    Ok((file_content, new_file_content))
}

