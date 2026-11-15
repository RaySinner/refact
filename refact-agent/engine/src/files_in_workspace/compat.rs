use super::*;

use crate::file_filter::is_valid_file;
use crate::files_correction::registered_alias_paths;

pub(crate) fn normalize_path_for_workspace_state(cache_dir: &Path, path: &Path) -> PathBuf {
    let worktree_mappings = crate::files_correction::registered_worktree_path_mappings(cache_dir);
    crate::files_correction::normalize_path_for_unscoped_root_selection(path, &worktree_mappings)
        .unwrap_or_else(|| crate::files_correction::canonical_path(path.to_string_lossy()))
}

pub(crate) fn normalize_path_for_index_store(cache_dir: &Path, path: &Path) -> PathBuf {
    let worktree_mappings = crate::files_correction::registered_worktree_path_mappings(cache_dir);
    crate::files_correction::normalize_path_for_unscoped_root_selection(path, &worktree_mappings)
        .unwrap_or_else(|| normalize_path_for_workspace_state(cache_dir, path))
}

pub(crate) fn event_path_is_valid_file(
    read_path: &PathBuf,
    store_path: &PathBuf,
    roots: &[PathBuf],
    global_config_roots: &[PathBuf],
) -> bool {
    if path_is_refact_internal(store_path) {
        return false;
    }
    if crate::file_filter::is_generated_index_path_with_global_config_roots(
        store_path,
        global_config_roots,
    ) {
        return false;
    }
    let scan_root = roots
        .iter()
        .filter(|root| store_path.starts_with(root))
        .max_by_key(|root| root.components().count());
    match scan_root {
        Some(root) => {
            if is_valid_file(read_path, true, false).is_err() {
                return false;
            }
            let rel_path = store_path
                .strip_prefix(root)
                .unwrap_or(store_path.as_path());
            !(path_has_hidden_component(rel_path) && !path_has_allowed_hidden_component(rel_path))
        }
        None => is_valid_file(read_path, false, false).is_ok(),
    }
}

pub(crate) async fn remove_memory_document_for_path_impl(gcx: Arc<GlobalContext>, path: &PathBuf) -> bool {
    remove_memory_document_for_path_for_ctx(&gcx.chat_ctx(gcx.clone()), path).await
}

pub async fn remove_memory_document_for_path_for_ctx(ctx: &ChatCtx, path: &PathBuf) -> bool {
    let canonical_path = crate::files_correction::canonical_path(path.to_string_lossy());
    let normalized_path = normalize_path_for_workspace_state(ctx.paths.cache_dir.as_path(), path);
    let mut doc_map = ctx.workspace.memory_document_map.lock().await;
    let mut removed = doc_map.remove(&canonical_path).is_some();
    if normalized_path != canonical_path {
        removed |= doc_map.remove(&normalized_path).is_some();
    }
    removed
}

pub(crate) async fn remove_memory_documents_under_path_impl(gcx: Arc<GlobalContext>, path: &PathBuf) -> usize {
    remove_memory_documents_under_path_for_ctx(&gcx.chat_ctx(gcx.clone()), path).await
}

pub async fn remove_memory_documents_under_path_for_ctx(ctx: &ChatCtx, path: &PathBuf) -> usize {
    let canonical_path = crate::files_correction::canonical_path(path.to_string_lossy());
    let normalized_path = normalize_path_for_workspace_state(ctx.paths.cache_dir.as_path(), path);
    let mut doc_map = ctx.workspace.memory_document_map.lock().await;
    let paths_to_remove = doc_map
        .keys()
        .filter(|p| p.starts_with(&canonical_path) || p.starts_with(&normalized_path))
        .cloned()
        .collect::<Vec<_>>();
    let removed = paths_to_remove.len();
    for path in paths_to_remove {
        doc_map.remove(&path);
    }
    removed
}

pub(crate) async fn get_file_text_from_memory_or_disk_impl(
    global_context: Arc<GlobalContext>,
    file_path: &PathBuf,
) -> Result<String, String> {
    let read_context = prepare_file_read_context(global_context.clone()).await;
    get_file_text_from_memory_or_disk_with_context(global_context, file_path, &read_context, None)
        .await
}

pub(crate) async fn get_file_text_from_memory_or_disk_for_model_context_impl(
    global_context: Arc<GlobalContext>,
    file_path: &PathBuf,
) -> Result<String, String> {
    let read_context = prepare_file_read_context(global_context.clone()).await;
    get_file_text_from_memory_or_disk_for_model_context_with_context(
        global_context,
        file_path,
        &read_context,
        None,
    )
    .await
}

pub fn strictest_zone_for_path(
    policy: &refact_privacy::CompiledPolicy,
    path: &Path,
    workspace_roots: &[PathBuf],
    worktree_mappings: &[crate::files_correction::RegisteredWorktreePathMapping],
) -> refact_privacy::Zone {
    let aliases = registered_alias_paths(path, worktree_mappings);
    let roots = workspace_roots.iter().map(PathBuf::as_path).chain(
        worktree_mappings
            .iter()
            .flat_map(|mapping| [mapping.root.as_path(), mapping.source_root.as_path()]),
    );
    policy.strictest_zone_for_paths_with_roots(aliases, roots)
}

pub(crate) fn check_file_privacy_with_context_impl(
    read_context: &FileReadContext,
    path: &Path,
) -> Result<(), String> {
    for alias in registered_alias_paths(path, &read_context.worktree_mappings) {
        check_file_privacy(
            read_context.privacy_settings.clone(),
            &alias,
            &FilePrivacyLevel::AllowToSendAnywhere,
        )?;
    }
    Ok(())
}

pub(crate) fn check_file_privacy_for_read_preparation_with_context_impl(
    read_context: &FileReadContext,
    path: &Path,
) -> Result<(), String> {
    for alias in registered_alias_paths(path, &read_context.worktree_mappings) {
        check_file_privacy_for_read_preparation(read_context.privacy_settings.clone(), &alias)?;
    }
    Ok(())
}

pub(crate) fn file_read_context_for(
    privacy_settings: Arc<PrivacySettings>,
    cache_dir: &std::path::Path,
) -> FileReadContext {
    FileReadContext {
        runtime: Arc::new(EngineFilesRuntime),
        privacy_settings,
        worktree_mappings: crate::files_correction::registered_worktree_path_mappings(cache_dir),
    }
}

pub(crate) async fn prepare_file_read_context_impl(
    global_context: Arc<GlobalContext>,
) -> FileReadContext {
    let privacy_settings = load_privacy_if_needed(global_context.clone()).await;
    file_read_context_for(privacy_settings, global_context.cache_dir.as_path())
}

pub(crate) async fn get_file_text_from_memory_or_disk_with_context(
    global_context: Arc<GlobalContext>,
    file_path: &PathBuf,
    read_context: &FileReadContext,
    max_bytes: Option<usize>,
) -> Result<String, String> {
    get_file_text_from_memory_or_disk_with_context_impl(
        global_context,
        file_path,
        read_context,
        max_bytes,
        false,
    )
    .await
}

pub(crate) async fn get_file_text_from_memory_or_disk_for_model_context_with_context_impl(
    global_context: Arc<GlobalContext>,
    file_path: &PathBuf,
    read_context: &FileReadContext,
    max_bytes: Option<usize>,
) -> Result<String, String> {
    get_file_text_from_memory_or_disk_with_context_impl(
        global_context,
        file_path,
        read_context,
        max_bytes,
        true,
    )
    .await
}

pub(crate) async fn get_file_text_from_memory_or_disk_with_context_impl(
    global_context: Arc<GlobalContext>,
    file_path: &PathBuf,
    read_context: &FileReadContext,
    max_bytes: Option<usize>,
    for_model_context: bool,
) -> Result<String, String> {
    let requested_path = crate::files_correction::canonical_path(file_path.to_string_lossy());
    let mapped_path = crate::files_correction::normalize_path_for_unscoped_root_selection(
        &requested_path,
        &read_context.worktree_mappings,
    )
    .unwrap_or_else(|| requested_path.clone());
    if for_model_context {
        check_file_privacy_for_read_preparation_with_context(read_context, &requested_path)?;
    } else {
        check_file_privacy_with_context(read_context, &requested_path)?;
    }

    let doc = {
        let doc_map = global_context
            .documents_state
            .memory_document_map
            .lock()
            .await;
        doc_map.get(&requested_path).cloned().or_else(|| {
            if mapped_path != requested_path {
                doc_map.get(&mapped_path).cloned()
            } else {
                None
            }
        })
    };
    if let Some(doc) = doc {
        let doc = doc.read().await;
        if let Some(text) = doc.doc_text.as_ref() {
            if let Some(limit) = max_bytes {
                if text.len_bytes() > limit {
                    return Err(format!("File exceeds {} byte search limit", limit));
                }
            }
            return Ok(text.to_string());
        }
    }
    let text = match max_bytes {
        Some(limit) => {
            read_file_from_disk_without_privacy_check_limited(&requested_path, limit).await
        }
        None => read_file_from_disk_without_privacy_check(&requested_path).await,
    }
    .map_err(|e| format!("Not found in memory, not found on disk: {}", e))?;
    Ok(text.to_string())
}

pub(crate) async fn check_file_privacy_for_send_impl(
    global_context: Arc<GlobalContext>,
    file_path: &PathBuf,
) -> Result<(), String> {
    let read_context = prepare_file_read_context(global_context).await;
    check_file_privacy_with_context(&read_context, file_path)
}

pub(crate) async fn check_file_privacy_for_model_context_impl(
    global_context: Arc<GlobalContext>,
    file_path: &PathBuf,
) -> Result<(), String> {
    let read_context = prepare_file_read_context(global_context).await;
    check_file_privacy_for_read_preparation_with_context(&read_context, file_path)
}

pub(crate) async fn filter_privacy_allowed_files_impl(
    global_context: Arc<GlobalContext>,
    files: Vec<PathBuf>,
) -> Vec<PathBuf> {
    let read_context = prepare_file_read_context(global_context).await;
    files
        .into_iter()
        .filter(|path| check_file_privacy_with_context(&read_context, path).is_ok())
        .collect()
}

pub(crate) async fn filter_privacy_allowed_files_for_model_context_impl(
    global_context: Arc<GlobalContext>,
    files: Vec<PathBuf>,
) -> Vec<PathBuf> {
    let read_context = prepare_file_read_context(global_context).await;
    files
        .into_iter()
        .filter(|path| {
            check_file_privacy_for_read_preparation_with_context(&read_context, path).is_ok()
        })
        .collect()
}

pub async fn update_document_text_from_disk(
    doc: &mut Document,
    gcx: Arc<GlobalContext>,
) -> Result<(), String> {
    match read_file_from_disk(load_privacy_if_needed(gcx.clone()).await, &doc.doc_path).await {
        Ok(res) => {
            doc.doc_text = Some(res);
            return Ok(());
        }
        Err(e) => return Err(e),
    }
}

pub async fn get_document_text_or_read_from_disk(
    doc: &mut Document,
    gcx: Arc<GlobalContext>,
) -> Result<String, String> {
    if doc.doc_text.is_some() {
        return Ok(doc.doc_text.as_ref().unwrap().to_string());
    }
    read_file_from_disk(load_privacy_if_needed(gcx.clone()).await, &doc.doc_path)
        .await
        .map(|x| x.to_string())
}

pub(crate) async fn mem_overwrite_or_create_document(
    global_context: Arc<GlobalContext>,
    document: Document,
) -> (Arc<ARwLock<Document>>, Arc<AMutex<f64>>, bool) {
    let cx = global_context.clone();
    let mut doc_map = cx.documents_state.memory_document_map.lock().await;
    if let Some(existing_doc) = doc_map.get_mut(&document.doc_path) {
        *existing_doc.write().await = document;
        (
            existing_doc.clone(),
            cx.documents_state.cache_dirty.clone(),
            false,
        )
    } else {
        let path = document.doc_path.clone();
        let darc = Arc::new(ARwLock::new(document));
        doc_map.insert(path, darc.clone());
        (darc, cx.documents_state.cache_dirty.clone(), true)
    }
}

pub(crate) async fn read_file_from_disk_without_privacy_check(path: &PathBuf) -> Result<Rope, String> {
    tokio::fs::read_to_string(path)
        .await
        .map(|x| Rope::from_str(&x))
        .map_err(|e| {
            format!(
                "failed to read file {}: {}",
                refact_core::nicer_logs::last_n_chars(&path.display().to_string(), 30),
                e
            )
        })
}

pub(crate) async fn read_file_from_disk_without_privacy_check_limited(
    path: &PathBuf,
    max_bytes: usize,
) -> Result<Rope, String> {
    if std::fs::metadata(path).is_ok_and(|metadata| metadata.len() > max_bytes as u64) {
        return Err(format!("File exceeds {} byte search limit", max_bytes));
    }
    let file = tokio::fs::File::open(path).await.map_err(|error| {
        format!(
            "failed to read file {}: {}",
            refact_core::nicer_logs::last_n_chars(&path.display().to_string(), 30),
            error
        )
    })?;
    let read_limit = u64::try_from(max_bytes)
        .unwrap_or(u64::MAX - 1)
        .saturating_add(1);
    let mut bytes = Vec::with_capacity(max_bytes.min(64 * 1024).saturating_add(1));
    file.take(read_limit)
        .read_to_end(&mut bytes)
        .await
        .map_err(|error| {
            format!(
                "failed to read file {}: {}",
                refact_core::nicer_logs::last_n_chars(&path.display().to_string(), 30),
                error
            )
        })?;
    if bytes.len() > max_bytes {
        return Err(format!("File exceeds {} byte search limit", max_bytes));
    }
    let text = String::from_utf8(bytes).map_err(|error| {
        format!(
            "failed to read file {} as UTF-8: {}",
            refact_core::nicer_logs::last_n_chars(&path.display().to_string(), 30),
            error
        )
    })?;
    Ok(Rope::from_str(&text))
}

pub async fn read_file_from_disk(
    privacy_settings: Arc<PrivacySettings>,
    path: &PathBuf,
) -> Result<Rope, String> {
    check_file_privacy(
        privacy_settings,
        path,
        &FilePrivacyLevel::AllowToSendAnywhere,
    )?;
    read_file_from_disk_without_privacy_check(path).await
}

pub use refact_files::vcs_detect::{detect_vcs_for_a_file_path, get_vcs_type};

// Slow version of version control detection:
// async fn is_git_repo(directory: &PathBuf) -> bool {
//     Command::new("git")
//         .arg("rev-parse")
//         .arg("--is-inside-work-tree")
//         .current_dir(directory)
//         .output()
//         .await
//         .map(|output| output.status.success())
//         .unwrap_or(false)
// }
// async fn is_svn_repo(directory: &PathBuf) -> bool {
//     Command::new("svn")
//         .arg("info")
//         .current_dir(directory)
//         .output()
//         .await
//         .map(|output| output.status.success())
//         .unwrap_or(false)
// }
// async fn is_hg_repo(directory: &PathBuf) -> bool {
//     Command::new("hg")
//         .arg("root")
//         .current_dir(directory)
//         .output()
//         .await
//         .map(|output| output.status.success())
//         .unwrap_or(false)
// }
