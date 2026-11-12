use super::*;

use crate::file_filter::is_valid_file;
use crate::files_blocklist::IndexingEverywhere;

pub(crate) async fn _run_command(
    cmd: &str,
    args: &[&str],
    path: &PathBuf,
    filter_out_status: bool,
) -> Option<Vec<PathBuf>> {
    info!("{} EXEC {} {}", path.display(), cmd, args.join(" "));
    let output = tokio::process::Command::new(cmd)
        .args(args)
        .current_dir_simplified(path)
        .output()
        .await
        .ok()?;

    if !output.status.success() {
        return None;
    }

    String::from_utf8(output.stdout.clone()).ok().map(|s| {
        s.lines()
            .map(|line| {
                let trimmed = line.trim();
                if filter_out_status && trimmed.len() > 1 {
                    path.join(&trimmed[1..].trim())
                } else {
                    path.join(line)
                }
            })
            .collect()
    })
}

pub(crate) async fn ls_files_under_version_control(path: &PathBuf) -> Option<Vec<PathBuf>> {
    if path.join(".git").exists() {
        git_ls_files(path)
    } else if path.join(".hg").exists() && which("hg").is_ok() {
        // Mercurial repository
        _run_command(
            "hg",
            &[
                "status",
                "--added",
                "--modified",
                "--clean",
                "--unknown",
                "--no-status",
            ],
            path,
            false,
        )
        .await
    } else if path.join(".svn").exists() && which("svn").is_ok() {
        // SVN repository
        let files_under_vc = _run_command("svn", &["list", "-R"], path, false).await;
        let files_changed = _run_command("svn", &["status"], path, true).await;
        Some(
            files_under_vc
                .unwrap_or_default()
                .into_iter()
                .chain(files_changed.unwrap_or_default().into_iter())
                .collect(),
        )
    } else {
        None
    }
}

pub(crate) fn is_valid_file_for_scan(
    path: &PathBuf,
    scan_root: &Path,
    allow_hidden_folders: bool,
    ignore_size_thresholds: bool,
    global_config_roots: &[PathBuf],
) -> Result<(), Box<dyn std::error::Error>> {
    if path_is_refact_internal(path) {
        return Err(".refact internal path".into());
    }
    if crate::file_filter::is_generated_index_path_with_global_config_roots(
        path,
        global_config_roots,
    ) {
        return Err("generated index.json".into());
    }
    is_valid_file(path, true, ignore_size_thresholds)?;
    if !allow_hidden_folders {
        let rel_path = path.strip_prefix(scan_root).unwrap_or(path.as_path());
        if path_has_hidden_component(rel_path) && !path_has_allowed_hidden_component(rel_path) {
            return Err("Parent dir starts with a dot".into());
        }
    }
    Ok(())
}

pub(crate) async fn _ls_files_under_version_control_recursive(
    all_files: &mut Vec<PathBuf>,
    vcs_folders: &mut Vec<PathBuf>,
    avoid_dups: &mut HashSet<PathBuf>,
    indexing_everywhere: &mut IndexingEverywhere,
    path: PathBuf,
    allow_files_in_hidden_folders: bool,
    ignore_size_thresholds: bool,
    check_blocklist: bool,
    global_config_roots: &[PathBuf],
) {
    let scan_root = crate::files_correction::canonical_path(&path.to_string_lossy().to_string());
    let mut candidates: Vec<PathBuf> = vec![scan_root.clone()];
    let mut rejected_reasons: HashMap<String, usize> = HashMap::new();
    let mut blocklisted_dirs_cnt: usize = 0;
    while !candidates.is_empty() {
        let checkme = candidates.pop().unwrap();
        if checkme.is_file() {
            let maybe_valid = is_valid_file_for_scan(
                &checkme,
                &scan_root,
                allow_files_in_hidden_folders,
                ignore_size_thresholds,
                global_config_roots,
            );
            match maybe_valid {
                Ok(_) => {
                    all_files.push(checkme.clone());
                }
                Err(e) => {
                    rejected_reasons
                        .entry(e.to_string())
                        .and_modify(|x| *x += 1)
                        .or_insert(1);
                    continue;
                }
            }
        }
        if checkme.is_dir() {
            if avoid_dups.contains(&checkme) {
                continue;
            }
            avoid_dups.insert(checkme.clone());
            if get_vcs_type(&checkme).is_some() {
                vcs_folders.push(checkme.clone());
            }
            if let Some(v) = ls_files_under_version_control(&checkme).await {
                // Has version control
                let indexing_yaml_path = checkme.join(".refact").join("indexing.yaml");
                if indexing_yaml_path.exists() {
                    match crate::files_blocklist::load_indexing_yaml(
                        &indexing_yaml_path,
                        Some(&checkme),
                    )
                    .await
                    {
                        Ok(indexing_settings) => {
                            for d in indexing_settings.additional_indexing_dirs.iter() {
                                let cp = crate::files_correction::canonical_path(d.as_str());
                                candidates.push(cp);
                            }
                            indexing_everywhere
                                .vcs_indexing_settings_map
                                .insert(checkme.to_string_lossy().to_string(), indexing_settings);
                        }
                        Err(e) => {
                            tracing::error!(
                                "failed to load indexing.yaml in {}: {}",
                                checkme.display(),
                                e
                            );
                        }
                    };
                }
                for x in v.iter() {
                    let indexing_settings = indexing_everywhere.indexing_for_path(x);
                    let rel_for_blocklist = x.strip_prefix(&scan_root).unwrap_or(x);
                    if check_blocklist && is_blocklisted(&indexing_settings, rel_for_blocklist) {
                        blocklisted_dirs_cnt += 1;
                        continue;
                    }
                    let maybe_valid = is_valid_file_for_scan(
                        x,
                        &scan_root,
                        allow_files_in_hidden_folders,
                        ignore_size_thresholds,
                        global_config_roots,
                    );
                    match maybe_valid {
                        Ok(_) => {
                            all_files.push(x.clone());
                        }
                        Err(e) => {
                            rejected_reasons
                                .entry(e.to_string())
                                .and_modify(|x| *x += 1)
                                .or_insert(1);
                        }
                    }
                }
            } else {
                // Don't have version control
                let indexing_settings = indexing_everywhere.indexing_for_path(&checkme);
                let rel_for_blocklist = checkme.strip_prefix(&scan_root).unwrap_or(&checkme);
                if check_blocklist && is_blocklisted(&indexing_settings, rel_for_blocklist) {
                    blocklisted_dirs_cnt += 1;
                    continue;
                }
                let new_paths: Vec<PathBuf> = WalkDir::new(checkme.clone())
                    .max_depth(1)
                    .into_iter()
                    .filter_map(|e| e.ok())
                    .map(|e| {
                        crate::files_correction::canonical_path(
                            &e.path().to_string_lossy().to_string(),
                        )
                    })
                    .filter(|e| e != &checkme)
                    .collect();
                candidates.extend(new_paths);
            }
        }
    }
    info!("when inspecting {:?} rejected files reasons:", path);
    for (reason, count) in &rejected_reasons {
        info!("    {:>6} {}", count, reason);
    }
    if rejected_reasons.is_empty() {
        info!("    no bad files at all");
    }
    info!(
        "also the loop bumped into {} blocklisted dirs",
        blocklisted_dirs_cnt
    );
}

pub async fn retrieve_files_in_workspace_folders(
    proj_folders: Vec<PathBuf>,
    indexing_everywhere: &mut IndexingEverywhere,
    allow_files_in_hidden_folders: bool,
    ignore_size_thresholds: bool,
    global_config_roots: &[PathBuf],
) -> (Vec<PathBuf>, Vec<PathBuf>) {
    let mut all_files: Vec<PathBuf> = Vec::new();
    let mut vcs_folders: Vec<PathBuf> = Vec::new();
    let mut avoid_dups: HashSet<PathBuf> = HashSet::new();
    for proj_folder in proj_folders {
        _ls_files_under_version_control_recursive(
            &mut all_files,
            &mut vcs_folders,
            &mut avoid_dups,
            indexing_everywhere,
            proj_folder.clone(),
            allow_files_in_hidden_folders,
            ignore_size_thresholds,
            true,
            global_config_roots,
        )
        .await;
    }
    info!("in all workspace folders, VCS roots found:");
    for vcs_folder in vcs_folders.iter() {
        info!("    {}", vcs_folder.display());
    }
    (all_files, vcs_folders)
}

pub(crate) fn git_root_at_or_above_dir_cached(
    mut dir: PathBuf,
    cache: &mut HashMap<PathBuf, Option<PathBuf>>,
) -> Option<PathBuf> {
    let mut visited = Vec::new();
    loop {
        if let Some(root) = cache.get(&dir).cloned() {
            for visited_dir in visited {
                cache.insert(visited_dir, root.clone());
            }
            return root;
        }
        visited.push(dir.clone());
        if dir.join(".git").exists() {
            let root = Some(dir.clone());
            for visited_dir in visited {
                cache.insert(visited_dir, root.clone());
            }
            return root;
        }
        if !dir.pop() {
            for visited_dir in visited {
                cache.insert(visited_dir, None);
            }
            return None;
        }
    }
}

pub(crate) fn matching_workspace_folder(path: &Path, workspace_folders: &[PathBuf]) -> Option<usize> {
    workspace_folders
        .iter()
        .enumerate()
        .filter(|(_, folder)| path.starts_with(folder))
        .max_by_key(|(_, folder)| folder.components().count())
        .map(|(idx, _)| idx)
}

pub(crate) fn order_workspace_files_for_initial_enqueue(
    files: Vec<PathBuf>,
    workspace_folders: &[PathBuf],
) -> Vec<PathBuf> {
    let workspace_folders = workspace_folders
        .iter()
        .map(|folder| canonical_path(folder.to_string_lossy()))
        .collect::<Vec<_>>();
    let mut git_root_cache = HashMap::new();
    let workspace_git_roots = workspace_folders
        .iter()
        .map(|folder| git_root_at_or_above_dir_cached(folder.clone(), &mut git_root_cache))
        .collect::<Vec<_>>();
    let mut primary = Vec::new();
    let mut nested = Vec::new();

    for file in files {
        let workspace_idx = matching_workspace_folder(&file, &workspace_folders);
        let is_nested = workspace_idx
            .and_then(|idx| workspace_git_roots.get(idx).and_then(|root| root.as_ref()))
            .and_then(|workspace_root| {
                let file_dir = file.parent().unwrap_or(file.as_path()).to_path_buf();
                git_root_at_or_above_dir_cached(file_dir, &mut git_root_cache)
                    .map(|file_root| (workspace_root.clone(), file_root))
            })
            .is_some_and(|(workspace_root, file_root)| {
                file_root != workspace_root && file_root.starts_with(&workspace_root)
            });
        if is_nested {
            nested.push(file);
        } else {
            primary.push(file);
        }
    }
    primary.extend(nested);
    primary
}

pub fn is_path_to_enqueue_valid(path: &PathBuf) -> Result<(), String> {
    let extension = path.extension().unwrap_or_default();
    if !SOURCE_FILE_EXTENSIONS.contains(&extension.to_str().unwrap_or_default()) {
        return Err(format!("Unsupported file extension {:?}", extension).into());
    }
    Ok(())
}

pub(crate) async fn enqueue_some_docs_with_read_paths_impl(
    gcx: Arc<GlobalContext>,
    paths: &[QueuedPath],
    force: bool,
) {
    if paths.is_empty() {
        return;
    }
    let worktree_mappings =
        crate::files_correction::registered_worktree_path_mappings(gcx.cache_dir.as_path());
    let workspace_files_changed = normalize_workspace_files_for_unscoped(&gcx, &worktree_mappings);
    let queue_paths = paths
        .iter()
        .map(|path| resolve_codegraph_queue_path(Path::new(&path.read_path), &worktree_mappings))
        .collect::<Vec<_>>();
    enqueue_resolved_docs(gcx, queue_paths, workspace_files_changed, force).await;
}

pub(crate) async fn enqueue_some_docs(gcx: Arc<GlobalContext>, paths: &Vec<String>, force: bool) {
    let worktree_mappings =
        crate::files_correction::registered_worktree_path_mappings(gcx.cache_dir.as_path());
    let workspace_files_changed = normalize_workspace_files_for_unscoped(&gcx, &worktree_mappings);
    let queue_paths = paths
        .iter()
        .map(|path| resolve_codegraph_queue_path(Path::new(path), &worktree_mappings))
        .collect::<Vec<_>>();
    enqueue_resolved_docs(gcx, queue_paths, workspace_files_changed, force).await;
}

pub(crate) async fn enqueue_resolved_docs(
    gcx: Arc<GlobalContext>,
    queue_paths: Vec<QueuedPath>,
    workspace_files_changed: bool,
    force: bool,
) {
    let queue_paths = queue_paths
        .into_iter()
        .filter(|path| {
            !crate::file_filter::is_transient_tmp_path(Path::new(path.store_path.as_str()))
        })
        .collect::<Vec<_>>();
    let normalized_paths = queue_paths
        .iter()
        .map(|path| path.store_path.clone())
        .collect::<Vec<_>>();
    info!(
        "detected {} modified/added/removed files",
        normalized_paths.len()
    );
    for d in normalized_paths.iter().take(5) {
        info!("    {}", crate::nicer_logs::last_n_chars(&d, 30));
    }
    if normalized_paths.len() > 5 {
        info!("    ...");
    }
    crate::indexing_routing::route_index_enqueue(gcx.clone(), &normalized_paths, force, true).await;
    let roots = crate::indexing_routing::memory_plane_roots(gcx.clone()).await;
    let (_memory_paths, code_paths) =
        crate::indexing_routing::partition_paths(&normalized_paths, &roots);
    if !code_paths.is_empty() {
        let code_path_set = code_paths.into_iter().collect::<HashSet<_>>();
        let code_queue_paths = queue_paths
            .iter()
            .filter(|path| code_path_set.contains(&path.store_path))
            .cloned()
            .collect::<Vec<_>>();
        let codegraph = gcx.codegraph.lock().await.clone();
        match codegraph {
            Some(service) => service.enqueue_paths_with_read_paths(&code_queue_paths),
            None => {
                tracing::warn!(
                    "codegraph unavailable; skipping {} code file(s) (memory-plane vec_db never receives code)",
                    code_queue_paths.len()
                );
            }
        }
    }
    let cache_correction_arc =
        crate::files_correction::files_cache_rebuild_as_needed(gcx.clone()).await;
    let mut moar_files: Vec<PathBuf> = Vec::new();
    let mut removed_files: HashSet<PathBuf> = HashSet::new();
    for path in &queue_paths {
        let store_path = PathBuf::from(&path.store_path);
        let read_path = PathBuf::from(&path.read_path);
        if read_path.exists()
            && cache_correction_arc
                .filenames
                .find_matches(&store_path)
                .len()
                == 0
        {
            moar_files.push(store_path);
        } else if !read_path.exists() {
            removed_files.insert(store_path);
        }
    }
    if workspace_files_changed || !moar_files.is_empty() || !removed_files.is_empty() {
        info!("this made file cache dirty");
        let dirty_arc = {
            let mut workspace_files = gcx.documents_state.workspace_files.lock().unwrap();
            if !removed_files.is_empty() {
                workspace_files.retain(|path| !removed_files.contains(path));
            }
            workspace_files.extend(moar_files);
            let mut seen = HashSet::new();
            workspace_files.retain(|path| seen.insert(path.clone()));
            gcx.documents_state.cache_dirty.clone()
        };
        crate::files_correction::mark_files_cache_dirty(&dirty_arc).await;
    }
}

pub(crate) fn normalize_workspace_files_for_unscoped(
    gcx: &Arc<GlobalContext>,
    worktree_mappings: &[crate::files_correction::RegisteredWorktreePathMapping],
) -> bool {
    let mut seen = HashSet::new();
    let normalized = {
        let workspace_files = gcx.documents_state.workspace_files.lock().unwrap();
        workspace_files
            .iter()
            .filter_map(|path| {
                crate::files_correction::normalize_path_for_unscoped_paths(path, worktree_mappings)
            })
            .filter(|path| seen.insert(path.clone()))
            .collect::<Vec<_>>()
    };
    let mut workspace_files = gcx.documents_state.workspace_files.lock().unwrap();
    if *workspace_files == normalized {
        return false;
    }
    *workspace_files = normalized;
    true
}

pub(crate) async fn enqueue_all_files_from_workspace_folders_impl(
    gcx: Arc<GlobalContext>,
    wake_up_indexers: bool,
    vecdb_only: bool,
) -> i32 {
    let folders = crate::files_correction::get_unscoped_project_dirs(gcx.clone()).await;

    crate::file_index::spawn_build(
        gcx.file_index.clone(),
        folders.clone(),
        gcx.shutdown_flag.clone(),
    );

    info!(
        "enqueue_all_files_from_workspace_folders started files search with {} folders",
        folders.len()
    );
    let mut indexing_everywhere =
        crate::files_blocklist::reload_global_indexing_only(gcx.clone()).await;
    let global_config_roots = vec![gcx.config_dir.clone()];
    let (all_files, vcs_folders) = retrieve_files_in_workspace_folders(
        folders.clone(),
        &mut indexing_everywhere,
        false,
        false,
        &global_config_roots,
    )
    .await;
    let all_files = order_workspace_files_for_initial_enqueue(all_files, &folders);
    info!(
        "enqueue_all_files_from_workspace_folders found {} files => workspace_files",
        all_files.len()
    );
    let workspace_vcs_roots = vcs_folders.clone();

    let mut old_workspace_files = Vec::new();
    let cache_dirty = {
        {
            let mut workspace_files = gcx.documents_state.workspace_files.lock().unwrap();
            std::mem::swap(&mut *workspace_files, &mut old_workspace_files);
            workspace_files.extend(all_files.clone());
        }
        {
            let mut roots = gcx.documents_state.workspace_vcs_roots.lock().unwrap();
            *roots = workspace_vcs_roots;
        }
        update_git_branch_heads_for_roots(&gcx, &vcs_folders);
        gcx.documents_state.cache_dirty.clone()
    };

    crate::files_correction::mark_files_cache_dirty(&cache_dirty).await;

    let current_paths = all_files.iter().cloned().collect::<HashSet<_>>();
    let removed_paths = old_workspace_files
        .iter()
        .filter(|path| !current_paths.contains(*path))
        .cloned()
        .collect::<Vec<_>>();
    let mut updated_paths: IndexSet<String> = IndexSet::new();
    updated_paths.extend(
        all_files
            .iter()
            .map(|file| file.to_string_lossy().to_string()),
    );
    let paths_nodups: Vec<String> = updated_paths.into_iter().collect();

    crate::indexing_routing::route_index_enqueue(
        gcx.clone(),
        &paths_nodups,
        wake_up_indexers,
        vecdb_only,
    )
    .await;
    if !vecdb_only {
        for path in removed_paths {
            on_did_delete(gcx.clone(), &path).await;
        }
    }

    all_files.len() as i32
}

pub async fn on_workspaces_init(gcx: Arc<GlobalContext>) -> i32 {
    // Called from lsp and lsp_like
    // Not called from main.rs as part of initialization
    let folders = gcx
        .documents_state
        .workspace_folders
        .lock()
        .unwrap()
        .clone();
    let old_app_searchable_id = gcx.app_searchable_id.lock().unwrap().clone();
    let new_app_searchable_id = get_app_searchable_id(&folders);
    if old_app_searchable_id != new_app_searchable_id {
        *gcx.app_searchable_id.lock().unwrap() = get_app_searchable_id(&folders);
    }
    // Project competitor import runs only here for normal startup and workspace add/remove changes.
    let _ = crate::ext::competitor_import::run_project_import(
        crate::app_state::app_state_from_gcx(gcx.clone()).await,
    )
    .await;
    watcher_init(gcx.clone()).await;
    let files_enqueued = enqueue_all_files_from_workspace_folders(gcx.clone(), false, false).await;

    crate::git::checkpoints::enqueue_init_shadow_repos(gcx.clone()).await;

    crate::chat::start_trajectory_watcher(gcx.clone());

    let _ = load_integrations(gcx.clone(), &["**/mcp_*".to_string()]).await;

    files_enqueued
}

pub async fn on_did_open(
    gcx: Arc<GlobalContext>,
    cpath: &PathBuf,
    text: &String,
    _language_id: &String,
) {
    if path_is_refact_internal(cpath) {
        return;
    }
    gcx.file_index.refresh(cpath);
    let normalized_path = normalize_path_for_workspace_state(gcx.cache_dir.as_path(), cpath);
    let mut doc = Document::new(&normalized_path);
    doc.update_text(text);
    info!(
        "on_did_open {}",
        crate::nicer_logs::last_n_chars(&cpath.display().to_string(), 30)
    );
    let (_doc_arc, dirty_arc, mark_dirty) =
        mem_overwrite_or_create_document(gcx.clone(), doc).await;
    if mark_dirty {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs_f64();
        *dirty_arc.lock().await = now;
    }
    *gcx.documents_state.active_file_path.lock().await = Some(cpath.clone());
}

pub async fn on_did_close(gcx: Arc<GlobalContext>, cpath: &PathBuf) {
    on_did_close_for_ctx(&gcx.chat_ctx(gcx.clone()), cpath).await
}

pub async fn on_did_close_for_ctx(ctx: &ChatCtx, cpath: &PathBuf) {
    info!(
        "on_did_close {}",
        crate::nicer_logs::last_n_chars(&cpath.display().to_string(), 30)
    );
    if !remove_memory_document_for_path_for_ctx(ctx, cpath).await {
        tracing::error!(
            "on_did_close: failed to remove from memory_document_map {:?}",
            normalize_path_for_workspace_state(ctx.paths.cache_dir.as_path(), cpath).display()
        );
    }
}

pub async fn on_did_change(gcx: Arc<GlobalContext>, path: &PathBuf, text: &String) {
    if path_is_refact_internal(path) {
        return;
    }
    gcx.file_index.refresh(path);
    let t0 = Instant::now();
    let normalized_path = normalize_path_for_workspace_state(gcx.cache_dir.as_path(), path);
    let (doc_arc, dirty_arc, mark_dirty) = {
        let mut doc = Document::new(&normalized_path);
        doc.update_text(text);
        let (doc_arc, dirty_arc, set_mark_dirty) =
            mem_overwrite_or_create_document(gcx.clone(), doc).await;
        (doc_arc, dirty_arc, set_mark_dirty)
    };

    if mark_dirty {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs_f64();
        *dirty_arc.lock().await = now;
    }

    *gcx.documents_state.active_file_path.lock().await = Some(path.clone());

    let mut go_ahead = true;
    {
        let is_it_good = is_valid_file(&normalized_path, false, false);
        if is_it_good.is_err() {
            info!(
                "{:?} ignoring changes: {}",
                normalized_path,
                is_it_good.err().unwrap()
            );
            go_ahead = false;
        }
    }

    let cpath = doc_arc
        .read()
        .await
        .doc_path
        .clone()
        .to_string_lossy()
        .to_string();
    if go_ahead {
        enqueue_some_docs(gcx.clone(), &vec![cpath], false).await;
    }

    info!(
        "on_did_change {}, total time {:.3}s",
        crate::nicer_logs::last_n_chars(&path.to_string_lossy().to_string(), 30),
        t0.elapsed().as_secs_f32()
    );
}

pub async fn on_did_delete(gcx: Arc<GlobalContext>, path: &PathBuf) {
    if path_is_refact_internal(path) {
        return;
    }
    gcx.file_index.remove(path);
    info!(
        "on_did_delete {}",
        crate::nicer_logs::last_n_chars(&path.to_string_lossy().to_string(), 30)
    );

    let (vec_db_module, codegraph, dirty_arc) = {
        let cx = gcx.clone();
        remove_memory_document_for_path(cx.clone(), path).await;
        let codegraph = cx.codegraph.lock().await.clone();
        (
            cx.vec_db.clone(),
            codegraph,
            cx.documents_state.cache_dirty.clone(),
        )
    };

    crate::files_correction::mark_files_cache_dirty(&dirty_arc).await;

    let delete_path = normalize_path_for_index_store(gcx.cache_dir.as_path(), path);

    match *vec_db_module.lock().await {
        Some(ref mut db) => match db.remove_file(&delete_path).await {
            Ok(_) => {}
            Err(err) => info!("VECDB Error removing: {}", err),
        },
        None => {}
    }
    if let Some(service) = &codegraph {
        let _ = service
            .remove_path(&delete_path.to_string_lossy().to_string())
            .await;
    }
}

pub async fn add_folder(gcx: Arc<GlobalContext>, fpath: &PathBuf) {
    let canonical_path =
        crate::files_correction::canonical_path(fpath.to_string_lossy().to_string());
    let was_added = {
        let documents_state = &gcx.documents_state;
        let mut folders = documents_state.workspace_folders.lock().unwrap();
        if folders.iter().any(|p| *p == canonical_path) {
            false
        } else {
            folders.push(canonical_path.clone());
            true
        }
    };
    if was_added {
        gcx.enrichment_generation.fetch_add(1, Ordering::SeqCst);
        tracing::info!("Added folder {} to workspace", canonical_path.display());
        on_workspaces_init(gcx.clone()).await;
    } else {
        tracing::debug!(
            "Folder {} already in workspace, skipping",
            canonical_path.display()
        );
    }
}

pub async fn remove_folder(gcx: Arc<GlobalContext>, path: &PathBuf) {
    let canonical_path =
        crate::files_correction::canonical_path(path.to_string_lossy().to_string());
    let was_removed = {
        let documents_state = &gcx.documents_state;
        let mut folders = documents_state.workspace_folders.lock().unwrap();
        let before = folders.len();
        folders.retain(|p| *p != canonical_path && *p != *path);
        folders.len() < before
    };
    if was_removed {
        gcx.enrichment_generation.fetch_add(1, Ordering::SeqCst);
        tracing::info!("Removed folder {} from workspace", path.display());
        on_workspaces_init(gcx.clone()).await;
    } else {
        tracing::debug!("Folder {} not found in workspace, skipping", path.display());
    }
}

pub async fn files_in_workspace_init_task(gcx: Arc<GlobalContext>) {
    let previous_folders = gcx
        .documents_state
        .workspace_folders
        .lock()
        .unwrap()
        .clone();
    let ev = crate::buddy::actor::make_runtime_event(
        "indexing",
        "Indexing project files...",
        "indexer",
        "indexing",
        "started",
        None,
    );
    crate::buddy::actor::buddy_enqueue_event(
        crate::app_state::app_state_from_gcx(gcx.clone()).await,
        ev,
    )
    .await;
    let file_count = enqueue_all_files_from_workspace_folders(gcx.clone(), true, false).await;
    let current_folders = gcx
        .documents_state
        .workspace_folders
        .lock()
        .unwrap()
        .clone();
    let added = current_folders
        .iter()
        .filter(|folder| !previous_folders.contains(folder))
        .map(|folder| folder.to_string_lossy().to_string())
        .collect::<Vec<_>>();
    let removed = previous_folders
        .iter()
        .filter(|folder| !current_folders.contains(folder))
        .map(|folder| folder.to_string_lossy().to_string())
        .collect::<Vec<_>>();
    if !added.is_empty() || !removed.is_empty() {
        let user_activity = gcx.user_activity.clone();
        if let Ok(mut ring) = user_activity.try_lock() {
            ring.push(UserAction::WorkspaceChanged {
                folders_added: added,
                folders_removed: removed,
                ts: Utc::now(),
            });
        };
    }
    enqueue_all_docs_from_jsonl_but_read_first(gcx.clone(), true, false).await;
    crate::git::checkpoints::enqueue_init_shadow_repos(gcx.clone()).await;
    let ev = crate::buddy::actor::make_runtime_event(
        "indexing",
        &format!("Workspace indexed: {} files", file_count),
        "indexer",
        "indexing",
        "completed",
        None,
    );
    crate::buddy::actor::buddy_enqueue_event(crate::app_state::app_state_from_gcx(gcx).await, ev)
        .await;
}
