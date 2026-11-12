use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::stream::{self, StreamExt};
use tracing::{error, info, warn};

use refact_codegraph::{
    lang_from_path, CodeGraphService, Counts, DatabaseRecovery, QueuedPath, WalCheckpointMode,
};

use crate::files_correction::get_project_dirs;
use crate::files_in_workspace::{
    check_file_privacy_for_send, enqueue_all_files_from_workspace_folders,
    get_file_text_from_memory_or_disk,
};
use crate::global_context::GlobalContext;

pub const CODEGRAPH_DB_FILE: &str = "codegraph.sqlite";
const DRAIN_BATCH: usize = 512;
const DRAIN_CONCURRENCY: usize = 8;
const BATCH_TX_SIZE: usize = 64;
const PROGRESS_REPORT_GRANULARITY: usize = 100;
pub const CONNECT_EVERY_BATCHES: u32 = 8;
pub const CONNECT_EVERY_SECS: u64 = 30;
pub const WAL_TRUNCATE_MIN_INTERVAL_SECS: u64 = 60;

pub fn progress_bucket(remaining: usize) -> usize {
    (remaining + PROGRESS_REPORT_GRANULARITY - 1) / PROGRESS_REPORT_GRANULARITY
}

pub fn should_report_unprocessed(remaining: usize, reported_unprocessed: &mut usize) -> bool {
    if remaining == 0 || progress_bucket(remaining) == progress_bucket(*reported_unprocessed) {
        return false;
    }
    *reported_unprocessed = remaining;
    true
}

pub fn should_connect_usages(batches_since: u32, elapsed: Duration) -> bool {
    batches_since >= CONNECT_EVERY_BATCHES || elapsed >= Duration::from_secs(CONNECT_EVERY_SECS)
}

pub fn should_truncate_wal(last_truncate: Option<Instant>, now: Instant) -> bool {
    last_truncate.map_or(true, |last| {
        now.saturating_duration_since(last) >= Duration::from_secs(WAL_TRUNCATE_MIN_INTERVAL_SECS)
    })
}

pub fn completion_message(counts: &Counts, parse_failures: usize) -> String {
    let base = format!(
        "codegraph: index complete — {} nodes, {} edges, {} files",
        counts.nodes, counts.edges, counts.files
    );
    if parse_failures == 0 {
        base
    } else {
        format!("{base}, {parse_failures} files failed to parse (symbols may be missing)")
    }
}

const PARSE_FAILURE_SAMPLE: usize = 10;

async fn report_parse_failures(service: &Arc<CodeGraphService>) {
    let count = service.run_parse_failures();
    if count == 0 {
        return;
    }
    match service
        .parse_failures(refact_codegraph::PARSE_FAILURE_REPORT_LIMIT)
        .await
    {
        Ok(failures) => {
            let sample = failures
                .iter()
                .take(PARSE_FAILURE_SAMPLE)
                .map(|failure| format!("{} ({})", failure.path, failure.reason))
                .collect::<Vec<_>>()
                .join(", ");
            warn!("codegraph: {count} files failed to parse during this run; sample: {sample}");
        }
        Err(err) => warn!("codegraph: {count} files failed to parse; listing them failed: {err}"),
    }
}

fn path_is_genuinely_absent(path: &Path) -> bool {
    matches!(std::fs::metadata(path), Err(err) if err.kind() == std::io::ErrorKind::NotFound)
}

async fn remove_missing_path(
    service: Arc<CodeGraphService>,
    store_path: &str,
    err: String,
) -> Result<(), String> {
    service
        .remove_path(store_path)
        .await
        .map_err(|remove_err| format!("codegraph: remove {store_path} failed: {remove_err}"))
        .map(|_| {
            warn!("codegraph: removed missing path after read failure: {store_path}: {err}");
        })
}

pub fn stale_stored_paths(stored_paths: Vec<String>) -> Vec<String> {
    stored_paths
        .into_iter()
        .filter(|path| path_is_genuinely_absent(Path::new(path)))
        .collect()
}

pub async fn reconcile_deleted_paths(service: Arc<CodeGraphService>) -> Result<usize, String> {
    let stale_paths = stale_stored_paths(service.all_paths().await?);
    let removed = stale_paths.len();
    for path in stale_paths {
        service.remove_path(&path).await?;
    }
    Ok(removed)
}

pub async fn prepare_index_entry_or_remove_path(
    gcx: Arc<GlobalContext>,
    service: Arc<CodeGraphService>,
    path: QueuedPath,
) -> Result<Option<(String, String, String)>, String> {
    let store_path = path.store_path;
    let read_path = path.read_path;
    let store_path_buf = PathBuf::from(&store_path);
    let read_path_buf = PathBuf::from(&read_path);
    if let Err(err) =
        check_file_privacy_for_send(gcx.clone(), &store_path_buf).await
    {
        if path_is_genuinely_absent(&store_path_buf) {
            return remove_missing_path(service, &store_path, err)
                .await
                .map(|_| None);
        }
        warn!("codegraph: read {read_path} failed for {store_path}, keeping existing index: {err}");
        return Ok(None);
    }
    match get_file_text_from_memory_or_disk(gcx, &read_path_buf).await {
        Ok(text) => {
            let lang = lang_from_path(&store_path);
            Ok(Some((store_path, text, lang.to_string())))
        }
        Err(err) if path_is_genuinely_absent(&store_path_buf) => {
            remove_missing_path(service, &store_path, err)
                .await
                .map(|_| None)
        }
        Err(err) => {
            warn!("codegraph: read {read_path} failed for {store_path}, keeping existing index: {err}");
            Ok(None)
        }
    }
}

fn database_is_gone(err: &str) -> bool {
    err.contains(refact_codegraph::DB_UNAVAILABLE_ERROR)
        || err.contains(refact_codegraph::store::DB_IN_USE_ERROR)
}

async fn submit_index_entries(
    gcx: &Arc<GlobalContext>,
    service: &Arc<CodeGraphService>,
    entries: Vec<(String, String, String)>,
) -> bool {
    let mut all_indexed = true;
    for chunk in entries.chunks(BATCH_TX_SIZE) {
        let chunk_entries = chunk.to_vec();
        if let Err(err) = service.index_files_batch(&chunk_entries).await {
            // Splitting the batch only helps when one file is at fault. A replaced or unavailable
            // database fails every single-file retry too, so re-queue the chunk and give up now.
            if database_is_gone(&err) || service.database_replaced() {
                error!("codegraph: index batch abandoned, database unavailable: {err}");
                *gcx.codegraph_error.lock().unwrap() = err;
                let paths = chunk_entries
                    .iter()
                    .map(|(path, _, _)| path.clone())
                    .collect::<Vec<_>>();
                service.enqueue_files(&paths);
                return false;
            }
            error!("codegraph: index batch failed: {err}");
            for entry in chunk_entries {
                let single = vec![entry];
                if let Err(err) = service.index_files_batch(&single).await {
                    let entry = &single[0];
                    let path = &entry.0;
                    let err = format!("codegraph: index {path} failed: {err}");
                    error!("{err}");
                    *gcx.codegraph_error.lock().unwrap() = err;
                    all_indexed = false;
                }
            }
        }
    }
    all_indexed
}

pub async fn process_index_batch(
    gcx: Arc<GlobalContext>,
    service: Arc<CodeGraphService>,
    batch: Vec<QueuedPath>,
) {
    let batch_len = batch.len();
    let mut results = stream::iter(batch)
        .map(|path| {
            let gcx = gcx.clone();
            let service = service.clone();
            async move { prepare_index_entry_or_remove_path(gcx, service, path).await }
        })
        .buffer_unordered(DRAIN_CONCURRENCY);

    let mut entries = Vec::with_capacity(batch_len);
    let mut all_prepared = true;
    while let Some(result) = results.next().await {
        match result {
            Ok(Some(entry)) => entries.push(entry),
            Ok(None) => {}
            Err(err) => {
                error!("{err}");
                *gcx.codegraph_error.lock().unwrap() = err;
                all_prepared = false;
            }
        }
    }
    let all_indexed = submit_index_entries(&gcx, &service, entries).await;
    if all_prepared && all_indexed {
        gcx.codegraph_error.lock().unwrap().clear();
    }
    service.record_index_completions(batch_len);
}

async fn connect_usages(gcx: &Arc<GlobalContext>, service: &Arc<CodeGraphService>) {
    if let Err(err) = service.connect_usages().await {
        error!("codegraph: connect_usages failed: {err}");
        *gcx.codegraph_error.lock().unwrap() = err;
    } else {
        *gcx.codegraph_error.lock().unwrap() = String::new();
    }
}

async fn maybe_periodic_connect_usages(
    gcx: &Arc<GlobalContext>,
    service: &Arc<CodeGraphService>,
    batches_since_connect: u32,
    elapsed: Duration,
) -> bool {
    if !should_connect_usages(batches_since_connect, elapsed) {
        return false;
    }
    let dirty_count = match service.dirty_usage_path_count().await {
        Ok(dirty_count) => dirty_count,
        Err(err) => {
            error!("codegraph: dirty usage check failed: {err}");
            return false;
        }
    };
    if dirty_count == 0 && batches_since_connect < CONNECT_EVERY_BATCHES {
        return false;
    }
    connect_usages(gcx, service).await;
    info!("codegraph: periodic connect_usages ({dirty_count} dirty)");
    true
}

pub async fn codegraph_db_path(gcx: Arc<GlobalContext>) -> PathBuf {
    let project_dirs = get_project_dirs(gcx.clone()).await;
    if let Some(root) = project_dirs.first() {
        let project_hash = refact_worktrees::service::project_hash_for_path(root);
        gcx.cache_dir
            .join("codegraph")
            .join(project_hash)
            .join(CODEGRAPH_DB_FILE)
    } else {
        gcx.cache_dir.join("codegraph").join(CODEGRAPH_DB_FILE)
    }
}

pub const STALE_STORE_MAX_AGE: Duration = Duration::from_secs(30 * 24 * 60 * 60);

fn newest_mtime_in_dir(dir: &Path) -> Option<std::time::SystemTime> {
    let entries = std::fs::read_dir(dir).ok()?;
    entries
        .flatten()
        .filter_map(|entry| entry.metadata().ok())
        .filter(|metadata| metadata.is_file())
        .filter_map(|metadata| metadata.modified().ok())
        .max()
}

pub fn sweep_stale_codegraph_stores(
    active_db_path: &Path,
    now: std::time::SystemTime,
) -> Vec<PathBuf> {
    let Some(active_dir) = active_db_path.parent() else {
        return Vec::new();
    };
    let Some(stores_root) = active_dir.parent() else {
        return Vec::new();
    };
    if stores_root.file_name().and_then(|name| name.to_str()) != Some("codegraph") {
        return Vec::new();
    }
    let Ok(entries) = std::fs::read_dir(stores_root) else {
        return Vec::new();
    };
    let mut removed = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path == active_dir || !path.is_dir() {
            continue;
        }
        if !path.join(CODEGRAPH_DB_FILE).is_file() {
            continue;
        }
        let stale = newest_mtime_in_dir(&path)
            .and_then(|modified| now.duration_since(modified).ok())
            .is_some_and(|age| age > STALE_STORE_MAX_AGE);
        if !stale {
            continue;
        }
        // Another engine holding the shared lease is still writing into this store, and unlinking
        // its files would leave it indexing a deleted inode.
        let db_file = path.join(CODEGRAPH_DB_FILE);
        match refact_codegraph::store::try_acquire_exclusive_lease(&db_file) {
            Ok(Some(lease)) => {
                // The lease stays alive across the removal: dropping it first would let another
                // engine open the store between the probe and the unlink.
                let deleted = std::fs::remove_dir_all(&path).is_ok();
                drop(lease);
                if deleted {
                    removed.push(path);
                }
            }
            Ok(None) => {
                info!("codegraph: stale store {path:?} is in use by another engine, keeping it");
            }
            Err(err) => {
                warn!("codegraph: stale store {path:?} lease probe failed: {err}");
            }
        }
    }
    removed
}

pub async fn codegraph_init(gcx: Arc<GlobalContext>) {
    let db_path = codegraph_db_path(gcx.clone()).await;
    let sweep_path = db_path.clone();
    let sweep = tokio::task::spawn_blocking(move || {
        sweep_stale_codegraph_stores(&sweep_path, std::time::SystemTime::now())
    })
    .await;
    match sweep {
        Ok(removed) if !removed.is_empty() => {
            info!(
                "codegraph: removed {} stale store(s): {removed:?}",
                removed.len()
            );
        }
        Ok(_) => {}
        Err(err) => warn!("codegraph: stale store sweep failed: {err}"),
    }
    match CodeGraphService::open(db_path.clone()) {
        Ok(service) => {
            *gcx.codegraph.lock().await = Some(Arc::new(service));
            *gcx.codegraph_error.lock().unwrap() = String::new();
            info!("codegraph: store ready at {db_path:?}");
        }
        Err(err) => {
            error!("codegraph: failed to open store at {db_path:?}: {err}");
            *gcx.codegraph_error.lock().unwrap() = err;
        }
    }
}

/// Another engine bumping the schema deletes and recreates the sqlite files, which would leave
/// this process writing into an unlinked inode forever. Reopen both connections on the file that
/// now exists and replay the workspace into it; the pending queue is kept as-is.
async fn recover_if_database_replaced(
    gcx: &Arc<GlobalContext>,
    service: &Arc<CodeGraphService>,
) -> bool {
    match service.recover_replaced_database().await {
        DatabaseRecovery::NotNeeded | DatabaseRecovery::Deferred => false,
        DatabaseRecovery::Recovered => {
            info!("codegraph: database was replaced on disk, reopened both connections");
            gcx.codegraph_error.lock().unwrap().clear();
            enqueue_all_files_from_workspace_folders(
                gcx.clone(),
                true,
                false,
            )
            .await;
            match reconcile_deleted_paths(service.clone()).await {
                Ok(removed) if removed > 0 => {
                    info!("codegraph: removed {removed} stale stored files after recovery");
                }
                Ok(_) => {}
                Err(err) => {
                    error!("codegraph: deletion reconciliation after recovery failed: {err}");
                    *gcx.codegraph_error.lock().unwrap() = err;
                }
            }
            true
        }
        DatabaseRecovery::Failed(err) => {
            error!("codegraph: reopen after replacement failed: {err}");
            *gcx.codegraph_error.lock().unwrap() = err;
            true
        }
    }
}

pub async fn codegraph_background_task(gcx: Arc<GlobalContext>) {
    let service = match gcx.codegraph.lock().await.clone() {
        Some(service) => service,
        None => return,
    };

    let queue_len_before_enqueue = service.queue_len();
    enqueue_all_files_from_workspace_folders(gcx.clone(), true, false)
        .await;
    let initial_enqueued = service.queue_len().saturating_sub(queue_len_before_enqueue);
    info!("codegraph: start_background_tasks, enqueued {initial_enqueued} initial files");

    match reconcile_deleted_paths(service.clone()).await {
        Ok(removed) if removed > 0 => info!("codegraph: removed {removed} stale stored files"),
        Ok(_) => {}
        Err(err) => {
            error!("codegraph: startup deletion reconciliation failed: {err}");
            *gcx.codegraph_error.lock().unwrap() = err;
        }
    }

    let mut reported_unprocessed = 0;
    let mut batches_since_connect: u32 = 0;
    let mut last_connect = Instant::now();
    let mut idle_wal_checkpoint_pending = true;
    let mut last_wal_truncate: Option<Instant> = None;

    loop {
        if gcx.shutdown_flag.load(Ordering::Relaxed) {
            break;
        }

        recover_if_database_replaced(&gcx, &service).await;
        if !service.is_available() {
            // A failed reopen must not turn into a busy loop: wait out the idle cadence and let
            // the next pass retry, keeping the queue intact meanwhile.
            let shutdown_flag = gcx.shutdown_flag.clone();
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs(3)) => {}
                _ = async move {
                    while !shutdown_flag.load(Ordering::Relaxed) {
                        tokio::time::sleep(Duration::from_millis(50)).await;
                    }
                } => break,
            }
            continue;
        }

        let remaining = service.queue_len();
        if should_report_unprocessed(remaining, &mut reported_unprocessed) {
            info!("codegraph: {remaining} unprocessed files in queue");
        }

        let batch = service.drain_batch_entries(DRAIN_BATCH);
        if batch.is_empty() {
            reported_unprocessed = 0;
            match service.has_dirty_usage_paths().await {
                Ok(true) => {
                    connect_usages(&gcx, &service).await;
                    batches_since_connect = 0;
                    last_connect = Instant::now();
                    idle_wal_checkpoint_pending = true;
                }
                Ok(false) => {
                    if idle_wal_checkpoint_pending
                        && should_truncate_wal(last_wal_truncate, Instant::now())
                    {
                        service.checkpoint_wal(WalCheckpointMode::Truncate).await;
                        idle_wal_checkpoint_pending = false;
                        last_wal_truncate = Some(Instant::now());
                    }
                }
                Err(err) => error!("codegraph: dirty usage check failed: {err}"),
            }
            if !service.is_initial_index_done() {
                service.mark_initial_index_done();
                report_parse_failures(&service).await;
                let parse_failures = service.run_parse_failures();
                match service.counts().await {
                    Ok(counts) => {
                        info!("{}", completion_message(&counts, parse_failures));
                    }
                    Err(err) => {
                        error!("codegraph: index complete counts failed: {err}");
                        *gcx.codegraph_error.lock().unwrap() = err;
                    }
                }
            }
            let shutdown_flag = gcx.shutdown_flag.clone();
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs(3)) => {}
                _ = service.wait_for_enqueue() => {}
                _ = async move {
                    while !shutdown_flag.load(Ordering::Relaxed) {
                        tokio::time::sleep(Duration::from_millis(50)).await;
                    }
                } => break,
            }
            continue;
        }

        process_index_batch(gcx.clone(), service.clone(), batch).await;
        batches_since_connect = batches_since_connect.saturating_add(1);
        idle_wal_checkpoint_pending = true;
        if gcx.shutdown_flag.load(Ordering::Relaxed) {
            break;
        }
        if maybe_periodic_connect_usages(
            &gcx,
            &service,
            batches_since_connect,
            last_connect.elapsed(),
        )
        .await
        {
            batches_since_connect = 0;
            last_connect = Instant::now();
        }
    }
}

