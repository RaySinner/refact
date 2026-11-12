use super::*;

use crate::files_blocklist::{
    is_blocklisted, reload_indexing_everywhere_if_needed, IndexingEverywhere,
};
use crate::files_correction::{canonical_path, resolve_codegraph_queue_path};

pub(crate) fn debounced_file_event_kind_for_path(
    is_remove_event: bool,
    read_path: &PathBuf,
    store_path: &PathBuf,
    roots: &[PathBuf],
    global_config_roots: &[PathBuf],
) -> Option<DebouncedFileEventKind> {
    if is_remove_event {
        return store_path
            .extension()
            .is_some()
            .then_some(DebouncedFileEventKind::Remove);
    }
    if read_path.exists()
        && event_path_is_valid_file(read_path, store_path, roots, global_config_roots)
    {
        Some(DebouncedFileEventKind::Upsert)
    } else if !read_path.exists() && store_path.extension().is_some() {
        Some(DebouncedFileEventKind::Remove)
    } else {
        None
    }
}

#[derive(Debug, Clone)]
pub(crate) struct PendingFileEvent {
    store_path: PathBuf,
    read_path: PathBuf,
    kind: DebouncedFileEventKind,
}

impl PendingFileEvent {
    fn queued_path(&self) -> QueuedPath {
        QueuedPath::new(
            self.store_path.to_string_lossy().to_string(),
            self.read_path.to_string_lossy().to_string(),
        )
    }

    fn kind_order(&self) -> u8 {
        match self.kind {
            DebouncedFileEventKind::Remove => 0,
            DebouncedFileEventKind::Upsert => 1,
        }
    }
}

pub(crate) fn read_git_head(repo_path: &Path) -> Option<String> {
    let head_path = repo_path.join(".git").join("HEAD");
    if let Some(head) = std::fs::read_to_string(&head_path)
        .ok()
        .map(|s| s.trim().to_string())
    {
        return Some(head);
    }
    let repository = git2::Repository::open(repo_path).ok()?;
    repository
        .head()
        .ok()
        .and_then(|head| head.target().map(|oid| oid.to_string()))
}

pub(crate) fn update_git_branch_heads_for_roots(gcx: &Arc<GlobalContext>, repo_paths: &[PathBuf]) {
    let repo_paths = repo_paths
        .iter()
        .map(|path| canonical_path(path.to_string_lossy()))
        .collect::<HashSet<_>>();
    let mut heads = gcx.documents_state.git_branch_heads.lock().unwrap();
    heads.retain(|repo_path, _| repo_paths.contains(repo_path));
    for repo_path in repo_paths {
        if let Some(head) = read_git_head(&repo_path) {
            heads.insert(repo_path, head);
        } else {
            heads.remove(&repo_path);
        }
    }
}

pub(crate) fn is_git_head_path(p: &Path) -> bool {
    p.file_name().map(|n| n == "HEAD").unwrap_or(false)
        && p.parent()
            .and_then(|pp| pp.file_name())
            .map(|n| n == ".git")
            .unwrap_or(false)
}

#[cfg(not(test))]
pub(crate) const FILE_EVENT_DEBOUNCE_WINDOW: Duration = Duration::from_millis(500);
#[cfg(test)]
pub(crate) const FILE_EVENT_DEBOUNCE_WINDOW: Duration = Duration::from_millis(100);
pub(crate) const FILE_EVENT_DEBOUNCE_RETAIN: Duration = Duration::from_secs(10);
#[cfg(not(test))]
pub(crate) const BRANCH_HEAD_DEBOUNCE_WINDOW: Duration = Duration::from_secs(2);
#[cfg(test)]
pub(crate) const BRANCH_HEAD_DEBOUNCE_WINDOW: Duration = Duration::from_millis(100);

#[derive(Debug, Clone)]
pub(crate) struct BranchHeadChange {
    repo_path: PathBuf,
    old_head: Option<String>,
    new_head: Option<String>,
}

pub(crate) fn resolve_head_text_to_oid(
    repository: &git2::Repository,
    head_text: &str,
) -> Result<git2::Oid, String> {
    let head_text = head_text.trim();
    if let Some(ref_name) = head_text.strip_prefix("ref:") {
        let ref_name = ref_name.trim();
        return repository
            .refname_to_id(ref_name)
            .or_else(|_| repository.revparse_single(ref_name).map(|obj| obj.id()))
            .map_err(|e| format!("resolve {ref_name}: {e}"));
    }
    git2::Oid::from_str(head_text)
        .or_else(|_| repository.revparse_single(head_text).map(|obj| obj.id()))
        .map_err(|e| format!("resolve {head_text}: {e}"))
}

pub(crate) fn changed_files_between_heads(
    repo_path: &Path,
    old_head: Option<&str>,
    new_head: Option<&str>,
) -> Result<Vec<PathBuf>, String> {
    let old_head = old_head.ok_or_else(|| "previous HEAD is unknown".to_string())?;
    let new_head = new_head.ok_or_else(|| "new HEAD is unknown".to_string())?;
    if old_head == new_head {
        return Ok(Vec::new());
    }
    let repository = git2::Repository::open(repo_path).map_err(|e| format!("git open: {e}"))?;
    let old_oid = resolve_head_text_to_oid(&repository, old_head)?;
    let new_oid = resolve_head_text_to_oid(&repository, new_head)?;
    if old_oid == new_oid {
        return Ok(Vec::new());
    }
    let old_tree = repository
        .find_object(old_oid, None)
        .and_then(|obj| obj.peel_to_tree())
        .map_err(|e| format!("read old tree: {e}"))?;
    let new_tree = repository
        .find_object(new_oid, None)
        .and_then(|obj| obj.peel_to_tree())
        .map_err(|e| format!("read new tree: {e}"))?;
    let mut diff_options = git2::DiffOptions::new();
    diff_options.include_typechange(true);
    let mut diff = repository
        .diff_tree_to_tree(Some(&old_tree), Some(&new_tree), Some(&mut diff_options))
        .map_err(|e| format!("git diff: {e}"))?;
    let mut find_options = git2::DiffFindOptions::new();
    find_options
        .renames(true)
        .rename_threshold(50)
        .rename_limit(200);
    if let Err(err) = diff.find_similar(Some(&mut find_options)) {
        tracing::debug!("branch switch rename detection skipped: {err}");
    }

    let mut paths = IndexSet::new();
    for delta in diff.deltas() {
        match delta.status() {
            git2::Delta::Added | git2::Delta::Copied => {
                if let Some(path) = delta.new_file().path() {
                    paths.insert(canonical_path(repo_path.join(path).to_string_lossy()));
                }
            }
            git2::Delta::Deleted => {
                if let Some(path) = delta.old_file().path() {
                    paths.insert(canonical_path(repo_path.join(path).to_string_lossy()));
                }
            }
            git2::Delta::Renamed => {
                if let Some(path) = delta.old_file().path() {
                    paths.insert(canonical_path(repo_path.join(path).to_string_lossy()));
                }
                if let Some(path) = delta.new_file().path() {
                    paths.insert(canonical_path(repo_path.join(path).to_string_lossy()));
                }
            }
            _ => {
                if let Some(path) = delta.new_file().path().or_else(|| delta.old_file().path()) {
                    paths.insert(canonical_path(repo_path.join(path).to_string_lossy()));
                }
            }
        }
    }
    Ok(paths.into_iter().collect())
}

pub(crate) fn record_git_head_change(gcx: &Arc<GlobalContext>, repo_path: &Path) -> Option<BranchHeadChange> {
    let repo_path = canonical_path(repo_path.to_string_lossy());
    let new_head = read_git_head(&repo_path);
    let mut heads = gcx.documents_state.git_branch_heads.lock().unwrap();
    let old_head = heads.get(&repo_path).cloned();
    if new_head == old_head {
        return None;
    }
    tracing::info!(
        "git HEAD changed in {}: {:?} -> {:?}",
        repo_path.display(),
        old_head,
        new_head
    );
    match &new_head {
        Some(h) => {
            heads.insert(repo_path.clone(), h.clone());
        }
        None => {
            heads.remove(&repo_path);
        }
    }
    Some(BranchHeadChange {
        repo_path,
        old_head,
        new_head,
    })
}

pub(crate) fn record_pending_git_head_change(
    gcx: &Arc<GlobalContext>,
    repo_path: &Path,
    now: Instant,
) -> Option<PathBuf> {
    let repo_path = canonical_path(repo_path.to_string_lossy());
    let new_head = read_git_head(&repo_path);
    let old_head = {
        let mut heads = gcx.documents_state.git_branch_heads.lock().unwrap();
        let old_head = heads.get(&repo_path).cloned();
        if new_head == old_head {
            return None;
        }
        tracing::info!(
            "git HEAD changed in {}: {:?} -> {:?}",
            repo_path.display(),
            old_head,
            new_head
        );
        match &new_head {
            Some(h) => {
                heads.insert(repo_path.clone(), h.clone());
            }
            None => {
                heads.remove(&repo_path);
            }
        }
        old_head
    };
    let mut pending = gcx.documents_state.branch_head_debounce.lock().unwrap();
    pending
        .entry(repo_path.clone())
        .and_modify(|change| {
            change.new_head = new_head.clone();
            change.last_event = now;
        })
        .or_insert(PendingBranchHeadChange {
            old_head,
            new_head,
            last_event: now,
        });
    Some(repo_path)
}

pub(crate) async fn enqueue_branch_head_changes(gcx: Arc<GlobalContext>, changes: Vec<BranchHeadChange>) {
    let mut docs = IndexSet::new();
    for change in &changes {
        match changed_files_between_heads(
            &change.repo_path,
            change.old_head.as_deref(),
            change.new_head.as_deref(),
        ) {
            Ok(paths) => {
                docs.extend(paths.into_iter().map(|p| p.to_string_lossy().to_string()));
            }
            Err(err) => {
                tracing::warn!(
                    "Branch switch diff failed for {}: {}; triggering full workspace reindex",
                    change.repo_path.display(),
                    err
                );
                enqueue_all_files_from_workspace_folders(gcx, true, false).await;
                return;
            }
        }
    }
    let docs = docs.into_iter().collect::<Vec<_>>();
    tracing::info!(
        "Branch switch detected, enqueueing {} changed file(s)",
        docs.len()
    );
    for doc in &docs {
        let path = PathBuf::from(doc);
        if !path.exists() {
            on_did_delete(gcx.clone(), &path).await;
        }
    }
    enqueue_some_docs(gcx, &docs, true).await;
}

pub(crate) fn schedule_branch_head_debounce(gcx: Arc<GlobalContext>, repo_path: PathBuf) {
    let mut tasks = gcx
        .documents_state
        .branch_head_debounce_tasks
        .lock()
        .unwrap();
    if tasks.contains_key(&repo_path) {
        return;
    }
    let task_gcx = gcx.clone();
    let task_repo_path = repo_path.clone();
    let handle = tokio::spawn(async move {
        flush_branch_head_debounce(task_gcx, task_repo_path).await;
    });
    tasks.insert(repo_path, handle);
}

pub(crate) async fn flush_branch_head_debounce(gcx: Arc<GlobalContext>, repo_path: PathBuf) {
    loop {
        if gcx.shutdown_flag.load(Ordering::Relaxed) {
            gcx.documents_state
                .branch_head_debounce_tasks
                .lock()
                .unwrap()
                .remove(&repo_path);
            return;
        }
        tokio::time::sleep(BRANCH_HEAD_DEBOUNCE_WINDOW).await;
        if gcx.shutdown_flag.load(Ordering::Relaxed) {
            gcx.documents_state
                .branch_head_debounce_tasks
                .lock()
                .unwrap()
                .remove(&repo_path);
            return;
        }
        let change = {
            let now = Instant::now();
            let mut pending = gcx.documents_state.branch_head_debounce.lock().unwrap();
            let Some(existing) = pending.get(&repo_path) else {
                gcx.documents_state
                    .branch_head_debounce_tasks
                    .lock()
                    .unwrap()
                    .remove(&repo_path);
                return;
            };
            if now.saturating_duration_since(existing.last_event) < BRANCH_HEAD_DEBOUNCE_WINDOW {
                continue;
            }
            let pending_change = pending.remove(&repo_path).unwrap();
            gcx.documents_state
                .branch_head_debounce_tasks
                .lock()
                .unwrap()
                .remove(&repo_path);
            pending_change
        };
        if change.old_head != change.new_head {
            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis() as u64;
            gcx.documents_state
                .branch_reindex_last_ts
                .store(now_ms, Ordering::Relaxed);
            enqueue_branch_head_changes(
                gcx,
                vec![BranchHeadChange {
                    repo_path,
                    old_head: change.old_head,
                    new_head: change.new_head,
                }],
            )
            .await;
        }
        return;
    }
}

pub(crate) async fn on_git_head_change(gcx_weak: Weak<GlobalContext>, event: Event) {
    let gcx = match gcx_weak.upgrade() {
        Some(gcx) => gcx,
        None => return,
    };

    let repo_paths: Vec<PathBuf> = event
        .paths
        .iter()
        .filter(|p| is_git_head_path(p))
        .filter_map(|p| p.parent()?.parent())
        .map(|p| canonical_path(p.to_string_lossy()))
        .collect();

    if repo_paths.is_empty() {
        return;
    }

    let now = Instant::now();
    for repo_path in repo_paths {
        if let Some(repo_path) = record_pending_git_head_change(&gcx, &repo_path, now) {
            schedule_branch_head_debounce(gcx.clone(), repo_path);
        }
    }
}

pub async fn on_explicit_branch_change(gcx: Arc<GlobalContext>, repo_path: &PathBuf) {
    let change = record_git_head_change(&gcx, repo_path);
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    gcx.documents_state
        .branch_reindex_last_ts
        .store(now_ms, Ordering::Relaxed);

    if let Some(change) = change {
        tracing::info!(
            "Explicit branch change notification for {}, enqueueing changed files",
            change.repo_path.display()
        );
        enqueue_branch_head_changes(gcx, vec![change]).await;
    }
}

pub(crate) fn schedule_debounced_file_event(
    gcx: Arc<GlobalContext>,
    queue_path: QueuedPath,
    kind: DebouncedFileEventKind,
    now: Instant,
) {
    {
        let mut debounce = gcx.documents_state.file_event_debounce.lock().unwrap();
        debounce.retain(|_, event| {
            now.saturating_duration_since(event.at) <= FILE_EVENT_DEBOUNCE_RETAIN
        });
        debounce.insert(
            PathBuf::from(queue_path.store_path),
            DebouncedFileEvent {
                read_path: PathBuf::from(queue_path.read_path),
                at: now,
                kind,
            },
        );
    }

    let mut task = gcx.documents_state.file_event_debounce_task.lock().unwrap();
    let should_spawn = task
        .as_ref()
        .map(|handle| handle.is_finished())
        .unwrap_or(true);
    if should_spawn {
        let task_gcx = gcx.clone();
        *task = Some(tokio::spawn(async move {
            flush_debounced_file_event_worker(task_gcx).await;
        }));
    }
    drop(task);
    gcx.documents_state.file_event_debounce_notify.notify_one();
}

pub(crate) fn take_all_debounced_file_events(gcx: &Arc<GlobalContext>) -> Vec<PendingFileEvent> {
    let mut debounce = gcx.documents_state.file_event_debounce.lock().unwrap();
    let mut events = debounce
        .drain()
        .map(|(store_path, event)| PendingFileEvent {
            store_path,
            read_path: event.read_path,
            kind: event.kind,
        })
        .collect::<Vec<_>>();
    events.sort_by(|a, b| {
        a.kind_order()
            .cmp(&b.kind_order())
            .then_with(|| a.store_path.cmp(&b.store_path))
    });
    events
}

pub(crate) fn take_due_debounced_file_events(
    gcx: &Arc<GlobalContext>,
    now: Instant,
) -> (Vec<PendingFileEvent>, Option<Duration>) {
    let mut debounce = gcx.documents_state.file_event_debounce.lock().unwrap();
    debounce
        .retain(|_, event| now.saturating_duration_since(event.at) <= FILE_EVENT_DEBOUNCE_RETAIN);
    let mut due_paths = Vec::new();
    let mut wait_for: Option<Duration> = None;
    for (store_path, event) in debounce.iter() {
        let elapsed = now.saturating_duration_since(event.at);
        if elapsed >= FILE_EVENT_DEBOUNCE_WINDOW {
            due_paths.push(store_path.clone());
        } else {
            let remaining = FILE_EVENT_DEBOUNCE_WINDOW.saturating_sub(elapsed);
            wait_for = Some(wait_for.map_or(remaining, |current| current.min(remaining)));
        }
    }
    let mut due = Vec::new();
    for path in due_paths {
        if let Some(event) = debounce.remove(&path) {
            due.push(PendingFileEvent {
                store_path: path,
                read_path: event.read_path,
                kind: event.kind,
            });
        }
    }
    due.sort_by(|a, b| {
        a.kind_order()
            .cmp(&b.kind_order())
            .then_with(|| a.store_path.cmp(&b.store_path))
    });
    (due, wait_for)
}

pub(crate) fn finish_file_event_debounce_worker(gcx: &Arc<GlobalContext>) -> bool {
    let debounce = gcx.documents_state.file_event_debounce.lock().unwrap();
    if debounce.is_empty() {
        *gcx.documents_state.file_event_debounce_task.lock().unwrap() = None;
        true
    } else {
        false
    }
}

pub(crate) async fn wait_for_shutdown_flag(shutdown_flag: Arc<AtomicBool>) {
    while !shutdown_flag.load(Ordering::Relaxed) {
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

pub(crate) async fn wait_for_file_debounce_signal(gcx: &Arc<GlobalContext>, wait_for: Option<Duration>) {
    let notify = gcx.documents_state.file_event_debounce_notify.clone();
    let shutdown_flag = gcx.shutdown_flag.clone();
    match wait_for {
        Some(wait_for) => {
            tokio::select! {
                _ = tokio::time::sleep(wait_for) => {}
                _ = notify.notified() => {}
                _ = wait_for_shutdown_flag(shutdown_flag) => {}
            }
        }
        None => {
            tokio::select! {
                _ = notify.notified() => {}
                _ = wait_for_shutdown_flag(shutdown_flag) => {}
            }
        }
    }
}

pub(crate) async fn flush_debounced_file_paths(gcx: Arc<GlobalContext>, events: Vec<PendingFileEvent>) {
    if events.is_empty() {
        return;
    }
    for event in &events {
        if matches!(event.kind, DebouncedFileEventKind::Remove) || !event.read_path.exists() {
            on_did_delete(gcx.clone(), &event.store_path).await;
        }
    }
    let docs = events
        .into_iter()
        .map(|event| event.queued_path())
        .collect::<Vec<_>>();
    enqueue_some_docs_with_read_paths(gcx, &docs, false).await;
}

pub(crate) async fn flush_debounced_file_event_worker(gcx: Arc<GlobalContext>) {
    loop {
        if gcx.shutdown_flag.load(Ordering::Relaxed) {
            let paths = take_all_debounced_file_events(&gcx);
            flush_debounced_file_paths(gcx.clone(), paths).await;
            if finish_file_event_debounce_worker(&gcx) {
                return;
            }
            continue;
        }

        let (paths, wait_for) = take_due_debounced_file_events(&gcx, Instant::now());
        if !paths.is_empty() {
            flush_debounced_file_paths(gcx.clone(), paths).await;
            continue;
        }

        if gcx
            .documents_state
            .file_event_debounce
            .lock()
            .unwrap()
            .is_empty()
        {
            if finish_file_event_debounce_worker(&gcx) {
                return;
            }
            continue;
        }
        wait_for_file_debounce_signal(&gcx, wait_for).await;
    }
}

pub async fn file_watcher_event(event: Event, gcx_weak: Weak<GlobalContext>) {
    async fn update_directory_watch_registration(
        gcx: Arc<GlobalContext>,
        indexing_everywhere: Arc<IndexingEverywhere>,
        path: PathBuf,
        should_watch: bool,
    ) {
        let indexing_everywhere = indexing_everywhere.as_ref();
        let path = path.as_path();
        let watcher_lock = {
            gcx.documents_state
                .fs_watcher
                .lock()
                .unwrap()
                .as_ref()
                .cloned()
        };
        let Some(watcher_lock) = watcher_lock else {
            return;
        };

        let canonical_path =
            crate::files_correction::canonicalize_normalized_path(path.to_path_buf());
        if should_watch {
            let mut watch_folders = gcx
                .documents_state
                .workspace_vcs_roots
                .lock()
                .unwrap()
                .clone();
            watch_folders.extend(
                gcx.documents_state
                    .workspace_folders
                    .lock()
                    .unwrap()
                    .clone(),
            );
            let unscoped_watch_folders =
                crate::files_correction::get_unscoped_project_dirs(gcx.clone()).await;
            watch_folders.extend(unscoped_watch_folders);
            let watch_folders = watch_folders
                .into_iter()
                .map(crate::files_correction::canonicalize_normalized_path)
                .collect::<Vec<_>>();
            let scan_root = watch_folders
                .iter()
                .filter(|root| canonical_path.starts_with(root))
                .max_by_key(|root| root.components().count())
                .cloned();
            let Some(scan_root) = scan_root else {
                return;
            };
            let registration =
                match classify_watch_subtree(&scan_root, &canonical_path, indexing_everywhere) {
                    WatchSubtreeClass::Excluded => None,
                    WatchSubtreeClass::CleanSubtree => Some(WatchRegistration {
                        path: canonical_path,
                        mode: WatchRegistrationMode::Recursive,
                    }),
                    WatchSubtreeClass::Mixed => Some(WatchRegistration {
                        path: canonical_path,
                        mode: WatchRegistrationMode::NonRecursive,
                    }),
                };
            if let Some(registration) = registration {
                register_watch_path(&watcher_lock, &registration).await;
            }
        } else {
            unregister_watch_path(&watcher_lock, &canonical_path).await;
        }
    }

    fn spawn_watch_registration(
        gcx: Arc<GlobalContext>,
        indexing_everywhere: Arc<IndexingEverywhere>,
        path: PathBuf,
        should_watch: bool,
    ) {
        let tasks = gcx.documents_state.watch_registration_tasks.clone();
        let handle = tokio::spawn(update_directory_watch_registration(
            gcx,
            indexing_everywhere,
            path,
            should_watch,
        ));
        let mut tasks = tasks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        tasks.retain(|task| !task.is_finished());
        tasks.push(handle);
    }

    async fn on_file_change(gcx_weak: Weak<GlobalContext>, event: Event) {
        let gcx = match gcx_weak.clone().upgrade() {
            Some(gcx) => gcx,
            None => return,
        };
        let indexing_everywhere_arc = reload_indexing_everywhere_if_needed(gcx.clone()).await;
        let maybe_created_dir = match &event.kind {
            EventKind::Create(CreateKind::Folder | CreateKind::Any) => event.paths.first().cloned(),
            EventKind::Modify(ModifyKind::Name(_)) => event.paths.last().cloned(),
            _ => None,
        }
        .filter(|path| path.is_dir());
        let maybe_removed_dir = match &event.kind {
            EventKind::Remove(RemoveKind::Folder) => event.paths.first().cloned(),
            EventKind::Modify(ModifyKind::Name(_))
                if event.paths.len() == 2 && event.paths[1].is_dir() =>
            {
                event.paths.first().cloned()
            }
            _ => None,
        };
        if let Some(path) = maybe_removed_dir {
            spawn_watch_registration(gcx.clone(), indexing_everywhere_arc.clone(), path, false);
        }
        if let Some(path) = maybe_created_dir {
            spawn_watch_registration(gcx.clone(), indexing_everywhere_arc.clone(), path, true);
        }
        if event.paths.iter().any(|p| path_triggers_registry_reload(p)) {
            crate::yaml_configs::customization_registry::invalidate_all_registry_caches(
                gcx.clone(),
            )
            .await;
        }
        if event
            .paths
            .iter()
            .any(|p| path_triggers_tool_catalog_reload(p))
        {
            if event.paths.iter().any(|p| {
                p.components()
                    .any(|component| component == Component::Normal("integrations.d".as_ref()))
            }) {
                gcx.tool_catalog_generations.advance_integrations();
            }
            if event
                .paths
                .iter()
                .any(|path| path.file_name().is_some_and(|name| name == "privacy.yaml"))
            {
                gcx.tool_catalog_generations.advance_privacy();
            }
        }
        if event
            .paths
            .iter()
            .any(|path| !path_is_refact_internal(path))
        {
            gcx.enrichment_generation.fetch_add(1, Ordering::SeqCst);
        }
        let mut blocklist_roots = gcx
            .documents_state
            .workspace_vcs_roots
            .lock()
            .unwrap()
            .clone();
        blocklist_roots
            .extend(crate::files_correction::get_unscoped_project_dirs(gcx.clone()).await);
        blocklist_roots = blocklist_roots
            .into_iter()
            .map(crate::files_correction::canonicalize_normalized_path)
            .collect();
        blocklist_roots.sort();
        blocklist_roots.dedup();
        let worktree_mappings =
            crate::files_correction::registered_worktree_path_mappings(gcx.cache_dir.as_path());
        let global_config_roots = vec![gcx.config_dir.clone()];
        let debounce_now = Instant::now();
        let is_remove_event = matches!(&event.kind, EventKind::Remove(_));
        for p in &event.paths {
            if path_is_refact_internal(p) {
                continue;
            }
            let queue_path = resolve_codegraph_queue_path(p, &worktree_mappings);
            let store_path = PathBuf::from(&queue_path.store_path);
            let read_path = PathBuf::from(&queue_path.read_path);
            let indexing_settings = indexing_everywhere_arc.indexing_for_path(&store_path);
            let blocklist_path = path_for_blocklist(&store_path, &blocklist_roots);
            if is_blocklisted(&indexing_settings, &blocklist_path) {
                continue;
            }
            if crate::file_filter::is_generated_index_path_with_global_config_roots(
                &store_path,
                &global_config_roots,
            ) {
                continue;
            }
            if let Some(kind) = debounced_file_event_kind_for_path(
                is_remove_event,
                &read_path,
                &store_path,
                &blocklist_roots,
                &global_config_roots,
            ) {
                schedule_debounced_file_event(gcx.clone(), queue_path, kind, debounce_now);
            }
        }
    }
    async fn on_dot_git_dir_change(gcx_weak: Weak<GlobalContext>, event: Event) {
        if let Some(gcx) = gcx_weak.clone().upgrade() {
            // Get the path before .git component, and check if repo associated exists
            let repo_paths = event
                .paths
                .iter()
                .filter_map(|p| {
                    p.components()
                        .position(|c| c == Component::Normal(".git".as_ref()))
                        .map(|i| {
                            let repo_p = p.components().take(i).collect::<PathBuf>();
                            canonical_path(repo_p.to_string_lossy())
                        })
                })
                .map(|p| {
                    let exists = p.join(".git").exists();
                    (p.clone(), exists)
                })
                .collect::<Vec<_>>();

            if repo_paths.is_empty() {
                return;
            }

            let workspace_vcs_roots = gcx.documents_state.workspace_vcs_roots.clone();

            let mut should_reindex = false;
            {
                let mut workspace_vcs_roots_locked = workspace_vcs_roots.lock().unwrap();
                for (repo_path, exists_in_disk) in repo_paths {
                    if exists_in_disk && !workspace_vcs_roots_locked.contains(&repo_path) {
                        tracing::info!(
                            "Found .git folder in workspace: {}",
                            repo_path.to_string_lossy()
                        );
                        should_reindex = true;
                        workspace_vcs_roots_locked.push(repo_path);
                    } else if !exists_in_disk && workspace_vcs_roots_locked.contains(&repo_path) {
                        tracing::info!(
                            "Removed .git folder from workspace: {}",
                            repo_path.to_string_lossy()
                        );
                        should_reindex = true;
                        workspace_vcs_roots_locked.retain(|p| p != &repo_path);
                    }
                }
            }

            if should_reindex {
                tracing::info!("Reindexing all files");
                enqueue_all_files_from_workspace_folders(gcx, false, false).await;
            }
        }
    }

    match event.kind {
        // We may receive specific event that a folder is being added/removed, but not the .git itself, this happens on Unix systems
        EventKind::Create(CreateKind::Folder) | EventKind::Remove(RemoveKind::Folder)
            if event.paths.iter().any(|p| {
                p.components()
                    .any(|c| c == Component::Normal(".git".as_ref()))
            }) =>
        {
            on_dot_git_dir_change(gcx_weak.clone(), event).await
        }

        // In Windows, we receive generic events (Any subtype), but we receive them about each exact folder
        EventKind::Create(CreateKind::Any)
        | EventKind::Modify(ModifyKind::Any)
        | EventKind::Remove(RemoveKind::Any)
            if event.paths.iter().any(|p| p.ends_with(".git")) =>
        {
            on_dot_git_dir_change(gcx_weak, event).await
        }

        EventKind::Create(_)
        | EventKind::Modify(_)
        | EventKind::Remove(_)
        | EventKind::Access(AccessKind::Close(AccessMode::Write))
            if event.paths.iter().any(|p| is_git_head_path(p)) =>
        {
            on_git_head_change(gcx_weak.clone(), event).await
        }

        EventKind::Create(_)
        | EventKind::Modify(_)
        | EventKind::Remove(_)
        | EventKind::Access(AccessKind::Close(AccessMode::Write)) => {
            on_file_change(gcx_weak.clone(), event).await
        }

        EventKind::Other | EventKind::Any | EventKind::Access(_) => {}
    }
}
