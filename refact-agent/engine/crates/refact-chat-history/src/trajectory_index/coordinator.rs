use super::*;

pub(super) const MAX_CACHED_TRAJECTORY_DIRECTORIES: usize = 4096;
#[cfg_attr(test, allow(dead_code))]
pub(super) const DIRECTORY_GENERATION_TTL: Duration = Duration::from_millis(500);
pub(super) const JOURNAL_CHECKPOINT_MAX_BYTES: u64 = 4 * 1024 * 1024;
pub(super) const JOURNAL_CHECKPOINT_MAX_AGE: Duration = Duration::from_secs(5 * 60);
pub(super) const JOURNAL_IDLE_CHECKPOINT_DELAY: Duration = Duration::from_secs(30);
pub(super) fn default_directory_generation_ttl() -> Duration {
    #[cfg(test)]
    {
        Duration::ZERO
    }
    #[cfg(not(test))]
    {
        DIRECTORY_GENERATION_TTL
    }
}

pub(super) static TRAJECTORY_INDEX_CACHE_ACCESS_COUNTER: AtomicU64 = AtomicU64::new(1);

pub(super) fn next_trajectory_index_cache_access_stamp() -> u64 {
    TRAJECTORY_INDEX_CACHE_ACCESS_COUNTER.fetch_add(1, Ordering::Relaxed)
}

pub(super) static TRAJECTORY_INDEX_LOCKS: std::sync::OnceLock<
    AMutex<HashMap<String, Arc<AMutex<()>>>>,
> = std::sync::OnceLock::new();

#[derive(Clone)]
pub(super) struct CachedLegacyTrajectoryIndex {
    pub(super) index: TrajectoryIndex,
    pub(super) generation: DirectoryGeneration,
    pub(super) last_access: u64,
}

pub(super) static LEGACY_TRAJECTORY_INDEX_CACHE: std::sync::OnceLock<
    AMutex<HashMap<PathBuf, CachedLegacyTrajectoryIndex>>,
> = std::sync::OnceLock::new();

pub(super) fn legacy_trajectory_index_cache(
) -> &'static AMutex<HashMap<PathBuf, CachedLegacyTrajectoryIndex>> {
    LEGACY_TRAJECTORY_INDEX_CACHE.get_or_init(|| AMutex::new(HashMap::new()))
}

pub(super) async fn cache_legacy_trajectory_index(dir: &Path, index: TrajectoryIndex) {
    let generation = match directory_generation(dir, index_file_names(&index)).await {
        Ok(generation) => generation,
        Err(_) => {
            invalidate_legacy_trajectory_index_cache(dir).await;
            return;
        }
    };
    let mut cache = legacy_trajectory_index_cache().lock().await;
    let key = dir.to_path_buf();
    let last_access = next_trajectory_index_cache_access_stamp();
    if let Some(entry) = cache.get_mut(&key) {
        entry.index = index;
        entry.generation = generation;
        entry.last_access = last_access;
        return;
    }
    if !cache.contains_key(&key) && cache.len() >= MAX_CACHED_TRAJECTORY_DIRECTORIES {
        if let Some(stale) = cache
            .iter()
            .min_by_key(|(_, entry)| entry.last_access)
            .map(|(path, _)| path.clone())
        {
            cache.remove(&stale);
        }
    }
    cache.insert(
        key,
        CachedLegacyTrajectoryIndex {
            index,
            generation,
            last_access,
        },
    );
}

pub(super) async fn invalidate_legacy_trajectory_index_cache(dir: &Path) {
    legacy_trajectory_index_cache().lock().await.remove(dir);
}

pub(super) fn get_trajectory_index_locks() -> &'static AMutex<HashMap<String, Arc<AMutex<()>>>> {
    TRAJECTORY_INDEX_LOCKS.get_or_init(|| AMutex::new(HashMap::new()))
}

pub(super) async fn get_trajectory_index_lock(dir: &Path) -> Arc<AMutex<()>> {
    let key = dir.to_string_lossy().to_string();
    let mut locks = get_trajectory_index_locks().lock().await;
    locks
        .entry(key)
        .or_insert_with(|| Arc::new(AMutex::new(())))
        .clone()
}

pub(super) const INDEX_LOCK_TIMEOUT: Duration = Duration::from_secs(2);
pub(super) const INDEX_LOCK_RETRY: Duration = Duration::from_millis(25);
pub(super) const COORDINATOR_FLUSH_INTERVAL: Duration = Duration::from_millis(250);

#[derive(Clone)]
pub(super) enum TrajectoryIndexMutation {
    Upsert(TrajectoryIndexEntry),
    Remove(HashSet<String>),
    Reconcile(Option<TrajectorySourceIdentity>),
}

#[derive(Clone)]
pub(super) struct PendingTrajectoryIndexMutation {
    sequence: u64,
    mutation: TrajectoryIndexMutation,
}

#[derive(Default)]
pub(super) struct TrajectoryIndexDirectoryState {
    pub(super) index: Option<TrajectoryIndex>,
    pub(super) generation: Option<DirectoryGeneration>,
    pub(super) generation_verified_at: Option<Instant>,
    pub(super) loaded: bool,
    pub(super) recovery_flush_required: bool,
    pub(super) last_access: u64,
    pub(super) next_sequence: u64,
    pub(super) pending: Vec<PendingTrajectoryIndexMutation>,
    pub(super) journal_cursor: Option<TrajectoryIndexJournalCursor>,
    pub(super) journal_started_at: Option<Instant>,
    pub(super) persisted: Option<TrajectoryIndex>,
}

impl TrajectoryIndexDirectoryState {
    pub(super) fn generation_is_within_ttl(&self, now: Instant, ttl: Duration) -> bool {
        !ttl.is_zero()
            && self
                .generation_verified_at
                .is_some_and(|verified_at| now.duration_since(verified_at) < ttl)
    }
}

#[derive(Clone)]
pub struct TrajectoryIndexCoordinator {
    pub(super) directories:
        Arc<AMutex<HashMap<PathBuf, Arc<AMutex<TrajectoryIndexDirectoryState>>>>>,
    lock_timeout: Duration,
    lock_retry: Duration,
    generation_ttl: Duration,
    pub(super) listing_counter_scope: TrajectoryIndexListingCounterScope,
}

impl Default for TrajectoryIndexCoordinator {
    fn default() -> Self {
        Self::new()
    }
}

pub(super) fn coordinator_span(
    component: PerfComponent,
    dir: &Path,
) -> Option<perf_diagnostics::PerfSpan> {
    if !perf_diagnostics::is_enabled() {
        return None;
    }
    let index_path = trajectory_index_path(dir);
    Some(perf_diagnostics::span(component, None, Some(&index_path)))
}

pub(super) fn finish_coordinator_span(
    span: Option<perf_diagnostics::PerfSpan>,
    outcome: PerfOutcome,
    item_count: Option<u64>,
) {
    if let Some(span) = span {
        span.finish(outcome, None, item_count, None, None);
    }
}

pub(super) fn record_coordinator_count(component: PerfComponent) {
    if !perf_diagnostics::is_enabled() {
        return;
    }
    perf_diagnostics::record(
        component,
        None,
        PerfOutcome::Success,
        0,
        None,
        Some(1),
        None,
    );
}

impl TrajectoryIndexCoordinator {
    pub fn new() -> Self {
        Self {
            directories: Arc::new(AMutex::new(HashMap::new())),
            lock_timeout: INDEX_LOCK_TIMEOUT,
            lock_retry: INDEX_LOCK_RETRY,
            generation_ttl: default_directory_generation_ttl(),
            listing_counter_scope: TrajectoryIndexListingCounterScope::Global,
        }
    }

    #[cfg(test)]
    pub(super) fn with_timing(lock_timeout: Duration, lock_retry: Duration) -> Self {
        Self {
            directories: Arc::new(AMutex::new(HashMap::new())),
            lock_timeout,
            lock_retry,
            generation_ttl: default_directory_generation_ttl(),
            listing_counter_scope: TrajectoryIndexListingCounterScope::Global,
        }
    }

    pub fn with_listing_counter_scope(
        listing_counter_scope: TrajectoryIndexListingCounterScope,
    ) -> Self {
        Self {
            directories: Arc::new(AMutex::new(HashMap::new())),
            lock_timeout: INDEX_LOCK_TIMEOUT,
            lock_retry: INDEX_LOCK_RETRY,
            generation_ttl: default_directory_generation_ttl(),
            listing_counter_scope,
        }
    }

    #[cfg(test)]
    pub(super) fn with_generation_ttl(mut self, generation_ttl: Duration) -> Self {
        self.generation_ttl = generation_ttl;
        self
    }

    pub fn new_with_local_listing_counters() -> Self {
        Self::with_listing_counter_scope(TrajectoryIndexListingCounterScope::local())
    }

    pub fn listing_counters_for_test(&self) -> TrajectoryIndexListingCounters {
        self.listing_counter_scope.snapshot()
    }

    pub(super) fn record_listing_caller(&self, caller: TrajectoryIndexListingCaller) {
        self.listing_counter_scope.record_listing_caller(caller);
    }

    pub(super) async fn directory_state(
        &self,
        dir: &Path,
    ) -> Result<Arc<AMutex<TrajectoryIndexDirectoryState>>, String> {
        let key = canonicalize_normalized_path(dir.to_path_buf());
        let mut directories = self.directories.lock().await;
        if let Some(state) = directories.get(&key) {
            if let Ok(mut state_guard) = state.try_lock() {
                state_guard.last_access = next_trajectory_index_cache_access_stamp();
            }
            return Ok(state.clone());
        }
        if directories.len() >= MAX_CACHED_TRAJECTORY_DIRECTORIES {
            let stale = directories
                .iter()
                .filter_map(|(path, state)| {
                    let state = state.try_lock().ok()?;
                    state
                        .pending
                        .is_empty()
                        .then_some((path.clone(), state.last_access))
                })
                .min_by_key(|(_, last_access)| *last_access)
                .map(|(path, _)| path);
            let Some(stale) = stale else {
                return Err(
                    "Trajectory index directory cache is full with pending writes".to_string(),
                );
            };
            directories.remove(&stale);
            self.listing_counter_scope.record_directory_eviction();
        }
        let state = Arc::new(AMutex::new(TrajectoryIndexDirectoryState {
            last_access: next_trajectory_index_cache_access_stamp(),
            ..Default::default()
        }));
        directories.insert(key, state.clone());
        Ok(state)
    }

    pub(super) async fn ensure_loaded(
        &self,
        dir: &Path,
        source_hint: Option<TrajectorySourceIdentity>,
    ) -> Result<Arc<AMutex<TrajectoryIndexDirectoryState>>, String> {
        let state = self.directory_state(dir).await?;
        if state.lock().await.loaded {
            return Ok(state);
        }

        let load_span = coordinator_span(PerfComponent::TrajectoryIndexCoordinatorLoad, dir);
        let index_read_span = coordinator_span(PerfComponent::TrajectoryIndexRead, dir);
        // A cold flush can contend during this load, before its flush lock span exists.
        // Capture the recorder here rather than on the blocking worker.
        let lock_wait_span = coordinator_span(PerfComponent::TrajectoryIndexLockWait, dir);
        let listing_counter_scope = self.listing_counter_scope.clone();
        let dir = dir.to_path_buf();
        let loaded = tokio::task::spawn_blocking(move || {
            let result = load_trajectory_index_for_coordinator_sync(
                &dir,
                source_hint,
                index_read_span,
                lock_wait_span,
                &listing_counter_scope,
            );
            let item_count = result
                .as_ref()
                .ok()
                .map(|loaded| loaded.index.entries.len() as u64);
            finish_coordinator_span(
                load_span,
                if result.is_ok() {
                    PerfOutcome::Success
                } else {
                    PerfOutcome::Failure
                },
                item_count,
            );
            result
        })
        .await
        .map_err(|error| format!("Trajectory index coordinator load task failed: {error}"))??;

        let mut state_guard = state.lock().await;
        if !state_guard.loaded {
            let mut index = loaded.index;
            state_guard.persisted = Some(index.clone());
            for pending in &state_guard.pending {
                if !matches!(pending.mutation, TrajectoryIndexMutation::Reconcile(_)) {
                    apply_mutation_to_index(&mut index, &pending.mutation)?;
                }
            }
            state_guard.index = Some(index);
            state_guard.generation = Some(loaded.generation);
            state_guard.generation_verified_at = Some(Instant::now());
            state_guard.loaded = true;
            state_guard.journal_cursor = Some(loaded.cursor);
            if loaded.needs_flush {
                push_pending_mutation(
                    &mut state_guard,
                    TrajectoryIndexMutation::Reconcile(loaded.source_hint),
                );
                state_guard.recovery_flush_required = true;
            }
        }
        drop(state_guard);
        Ok(state)
    }

    pub async fn snapshot(
        &self,
        dir: &Path,
        source_hint: Option<TrajectorySourceIdentity>,
    ) -> Result<TrajectoryIndex, String> {
        let state = self.ensure_loaded(dir, source_hint).await?;
        let snapshot = state
            .lock()
            .await
            .index
            .clone()
            .ok_or_else(|| "Trajectory index coordinator did not load an index".to_string());
        snapshot
    }

    pub async fn upsert(&self, dir: &Path, entry: TrajectoryIndexEntry) -> Result<(), String> {
        validate_trajectory_index_entry(&entry)?;
        let state = self.directory_state(dir).await?;
        let mut state_guard = state.lock().await;
        if let Some(index) = state_guard.index.as_mut() {
            apply_mutation_to_index(index, &TrajectoryIndexMutation::Upsert(entry.clone()))?;
        }
        refresh_generation_for_entry(&mut state_guard, dir, &entry.file_name);
        push_pending_mutation(&mut state_guard, TrajectoryIndexMutation::Upsert(entry));
        Ok(())
    }

    pub async fn remove(&self, dir: &Path, chat_ids: HashSet<String>) -> Result<(), String> {
        if chat_ids.is_empty() {
            return Ok(());
        }
        let state = self.directory_state(dir).await?;
        let mut state_guard = state.lock().await;
        if let Some(index) = state_guard.index.as_mut() {
            apply_mutation_to_index(index, &TrajectoryIndexMutation::Remove(chat_ids.clone()))?;
        }
        state_guard.generation = None;
        state_guard.generation_verified_at = None;
        push_pending_mutation(&mut state_guard, TrajectoryIndexMutation::Remove(chat_ids));
        Ok(())
    }

    pub async fn reconcile(
        &self,
        dir: &Path,
        source_hint: Option<TrajectorySourceIdentity>,
    ) -> Result<TrajectoryIndex, String> {
        let state = self.ensure_loaded(dir, source_hint.clone()).await?;
        let index = state
            .lock()
            .await
            .index
            .clone()
            .ok_or_else(|| "Trajectory index coordinator did not load an index".to_string())?;
        let reconcile_span =
            coordinator_span(PerfComponent::TrajectoryIndexCoordinatorReconcile, dir);
        let dir = dir.to_path_buf();
        let reconcile_dir = dir.clone();
        let source_hint_for_reconcile = source_hint.clone();
        let listing_counter_scope = self.listing_counter_scope.clone();
        let reconciled = tokio::task::spawn_blocking(move || {
            let result = reconcile_trajectory_index_sync(
                &reconcile_dir,
                index,
                source_hint_for_reconcile,
                &listing_counter_scope,
            );
            let item_count = result
                .as_ref()
                .ok()
                .map(|(index, _)| index.entries.len() as u64);
            finish_coordinator_span(
                reconcile_span,
                if result.is_ok() {
                    PerfOutcome::Success
                } else {
                    PerfOutcome::Failure
                },
                item_count,
            );
            result
        })
        .await
        .map_err(|error| {
            format!("Trajectory index coordinator reconcile task failed: {error}")
        })??;

        let mut state_guard = state.lock().await;
        let mut index = reconciled.0;
        for pending in &state_guard.pending {
            if !matches!(pending.mutation, TrajectoryIndexMutation::Reconcile(_)) {
                apply_mutation_to_index(&mut index, &pending.mutation)?;
            }
        }
        state_guard.index = Some(index.clone());
        if reconciled.1 {
            push_pending_mutation(
                &mut state_guard,
                TrajectoryIndexMutation::Reconcile(source_hint),
            );
        }
        drop(state_guard);
        let generation = directory_generation(dir.as_path(), index_file_names(&index)).await?;
        let mut state_guard = state.lock().await;
        state_guard.generation = Some(generation);
        state_guard.generation_verified_at = Some(Instant::now());
        drop(state_guard);
        Ok(index)
    }

    pub async fn list_entries(
        &self,
        dir: &Path,
        source_hint: Option<TrajectorySourceIdentity>,
    ) -> Result<Vec<TrajectoryIndexEntry>, String> {
        let state = self.ensure_loaded(dir, source_hint.clone()).await?;
        if state.lock().await.recovery_flush_required {
            self.flush_directory(dir).await?;
        }
        let now = Instant::now();
        let (entries, indexed_file_names, generation, within_ttl) = {
            let state_guard = state.lock().await;
            let index = state_guard
                .index
                .as_ref()
                .ok_or_else(|| "Trajectory index coordinator did not load an index".to_string())?;
            (
                index.entries.clone(),
                index_file_names(index),
                state_guard.generation.clone(),
                state_guard.generation_is_within_ttl(now, self.generation_ttl),
            )
        };
        let fresh = match generation {
            Some(_) if within_ttl => true,
            Some(generation) => {
                self.listing_counter_scope.record_generation_check();
                let fresh = cached_index_is_fresh(dir, indexed_file_names, generation)
                    .await
                    .unwrap_or(false);
                if fresh {
                    state.lock().await.generation_verified_at = Some(Instant::now());
                }
                fresh
            }
            None => false,
        };
        if !fresh {
            let recovery_flush_required = matches!(
                read_trajectory_index_with_counter_scope(dir, &self.listing_counter_scope).await,
                Ok(None) | Err(_)
            );
            let recovery_source_hint = source_hint.clone();
            let entries = self.reconcile(dir, source_hint).await?.entries;
            if recovery_flush_required {
                let mut state_guard = state.lock().await;
                push_pending_mutation(
                    &mut state_guard,
                    TrajectoryIndexMutation::Reconcile(recovery_source_hint),
                );
                state_guard.recovery_flush_required = true;
                drop(state_guard);
                self.flush_directory(dir).await?;
            }
            return Ok(entries);
        }
        self.listing_counter_scope.record_listing_cache_hit();
        record_coordinator_count(PerfComponent::TrajectoryIndexCacheHit);
        Ok(entries)
    }

    pub async fn flush_directory(&self, dir: &Path) -> Result<(), String> {
        self.flush_directory_inner(dir, false).await
    }

    pub async fn checkpoint_directory(&self, dir: &Path) -> Result<(), String> {
        self.flush_directory_inner(dir, true).await
    }

    pub(super) async fn flush_directory_inner(
        &self,
        dir: &Path,
        force_checkpoint: bool,
    ) -> Result<(), String> {
        let state = self.ensure_loaded(dir, None).await?;
        let flush_serializer = get_trajectory_index_lock(dir).await;
        let _flush_guard = flush_serializer.lock().await;
        let (pending, last_sequence, cursor, cached, journal_started_at) = {
            let state_guard = state.lock().await;
            let last_sequence = state_guard.pending.last().map(|last| last.sequence);
            if last_sequence.is_none() && !force_checkpoint {
                return Ok(());
            }
            (
                state_guard.pending.clone(),
                last_sequence.unwrap_or(0),
                state_guard.journal_cursor.clone(),
                state_guard.persisted.clone(),
                state_guard.journal_started_at,
            )
        };
        let force_checkpoint = force_checkpoint
            || journal_started_at
                .is_some_and(|started_at| started_at.elapsed() >= JOURNAL_CHECKPOINT_MAX_AGE);
        let dir = dir.to_path_buf();
        let flush_dir = dir.clone();
        let lock_timeout = self.lock_timeout;
        let lock_retry = self.lock_retry;
        let flush_span = coordinator_span(PerfComponent::TrajectoryIndexCoordinatorFlush, &dir);
        let reconcile_span = pending
            .iter()
            .any(|pending| matches!(pending.mutation, TrajectoryIndexMutation::Reconcile(_)))
            .then(|| coordinator_span(PerfComponent::TrajectoryIndexCoordinatorReconcile, &dir))
            .flatten();
        let index_read_span = coordinator_span(PerfComponent::TrajectoryIndexRead, &dir);
        let lock_wait_span = coordinator_span(PerfComponent::TrajectoryIndexLockWait, &dir);
        let index_write_span = coordinator_span(PerfComponent::TrajectoryIndexWrite, &dir);
        let listing_counter_scope = self.listing_counter_scope.clone();
        let outcome = tokio::task::spawn_blocking(move || {
            let result = flush_trajectory_index_mutations_sync(
                &flush_dir,
                pending,
                cursor
                    .zip(cached)
                    .map(|(cursor, index)| SyncedPersistedIndex {
                        index,
                        cursor,
                        checkpoint_present: true,
                        rebuilt: false,
                    }),
                force_checkpoint,
                FlushLockOptions {
                    timeout: lock_timeout,
                    retry: lock_retry,
                },
                FlushSpans {
                    index_read: index_read_span,
                    lock_wait: lock_wait_span,
                    index_write: index_write_span,
                    reconcile: reconcile_span,
                },
                &listing_counter_scope,
            );
            let item_count = result
                .as_ref()
                .ok()
                .map(|outcome| outcome.index.entries.len() as u64);
            finish_coordinator_span(
                flush_span,
                if result.is_ok() {
                    PerfOutcome::Success
                } else {
                    PerfOutcome::Failure
                },
                item_count,
            );
            result
        })
        .await
        .map_err(|error| format!("Trajectory index coordinator flush task failed: {error}"))??;

        let mut state_guard = state.lock().await;
        state_guard
            .pending
            .retain(|pending| pending.sequence > last_sequence);
        state_guard.journal_cursor = Some(outcome.cursor);
        if outcome.checkpointed {
            state_guard.journal_started_at = None;
        } else if outcome.appended && state_guard.journal_started_at.is_none() {
            state_guard.journal_started_at = Some(Instant::now());
        }
        let mut index = outcome.index;
        state_guard.persisted = Some(index.clone());
        for pending in &state_guard.pending {
            if !matches!(pending.mutation, TrajectoryIndexMutation::Reconcile(_)) {
                apply_mutation_to_index(&mut index, &pending.mutation)?;
            }
        }
        refresh_generation_after_index_write(&mut state_guard, &dir, &index);
        state_guard.index = Some(index);
        state_guard.recovery_flush_required = false;
        Ok(())
    }

    pub async fn flush_all(&self) -> Result<(), String> {
        let directories: Vec<PathBuf> = self.directories.lock().await.keys().cloned().collect();
        let mut first_error = None;
        for directory in directories {
            if let Err(error) = self.flush_directory(&directory).await {
                first_error.get_or_insert(error);
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    pub async fn checkpoint_all(&self) -> Result<(), String> {
        let directories: Vec<PathBuf> = self.directories.lock().await.keys().cloned().collect();
        let mut first_error = None;
        for directory in directories {
            if let Err(error) = self.checkpoint_directory(&directory).await {
                first_error.get_or_insert(error);
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    pub(super) async fn has_pending_mutations(&self) -> bool {
        let directories: Vec<Arc<AMutex<TrajectoryIndexDirectoryState>>> =
            self.directories.lock().await.values().cloned().collect();
        for state in directories {
            if !state.lock().await.pending.is_empty() {
                return true;
            }
        }
        false
    }
}

pub async fn trajectory_index_coordinator_background_task(
    coordinator: Arc<TrajectoryIndexCoordinator>,
    shutdown_flag: Arc<std::sync::atomic::AtomicBool>,
) {
    let mut idle_since = Instant::now();
    while !shutdown_flag.load(std::sync::atomic::Ordering::Relaxed) {
        tokio::time::sleep(COORDINATOR_FLUSH_INTERVAL).await;
        if coordinator.has_pending_mutations().await {
            idle_since = Instant::now();
        }
        if let Err(error) = coordinator.flush_all().await {
            tracing::warn!("trajectory index coordinator flush failed: {error}");
        }
        if idle_since.elapsed() >= JOURNAL_IDLE_CHECKPOINT_DELAY {
            idle_since = Instant::now();
            if shutdown_flag.load(std::sync::atomic::Ordering::Relaxed) {
                break;
            }
            if let Err(error) = coordinator.checkpoint_all().await {
                tracing::warn!("trajectory index coordinator idle checkpoint failed: {error}");
            }
        }
    }
    if let Err(error) = coordinator.flush_all().await {
        tracing::warn!("trajectory index coordinator shutdown flush failed: {error}");
    }
    if let Err(error) = coordinator.checkpoint_all().await {
        tracing::warn!("trajectory index coordinator shutdown checkpoint failed: {error}");
    }
}

pub(super) fn push_pending_mutation(
    state: &mut TrajectoryIndexDirectoryState,
    mutation: TrajectoryIndexMutation,
) {
    state.next_sequence += 1;
    state.pending.push(PendingTrajectoryIndexMutation {
        sequence: state.next_sequence,
        mutation,
    });
    record_coordinator_count(PerfComponent::TrajectoryIndexEnqueue);
}

pub(super) struct CoordinatorLoadResult {
    index: TrajectoryIndex,
    generation: DirectoryGeneration,
    needs_flush: bool,
    source_hint: Option<TrajectorySourceIdentity>,
    cursor: TrajectoryIndexJournalCursor,
}

pub(super) fn load_trajectory_index_for_coordinator_sync(
    dir: &Path,
    source_hint: Option<TrajectorySourceIdentity>,
    index_read_span: Option<perf_diagnostics::PerfSpan>,
    lock_wait_span: Option<perf_diagnostics::PerfSpan>,
    listing_counter_scope: &TrajectoryIndexListingCounterScope,
) -> Result<CoordinatorLoadResult, String> {
    std::fs::create_dir_all(dir)
        .map_err(|error| format!("Failed to create trajectory directory {:?}: {error}", dir))?;
    with_trajectory_index_file_lock(
        dir,
        INDEX_LOCK_TIMEOUT,
        INDEX_LOCK_RETRY,
        lock_wait_span,
        || {
            load_trajectory_index_for_coordinator_locked_sync(
                dir,
                source_hint,
                index_read_span,
                listing_counter_scope,
            )
        },
    )
}

// Used by flush while it already owns the cross-process lock; never reacquire it.
pub(super) fn load_trajectory_index_for_coordinator_locked_sync(
    dir: &Path,
    source_hint: Option<TrajectorySourceIdentity>,
    index_read_span: Option<perf_diagnostics::PerfSpan>,
    listing_counter_scope: &TrajectoryIndexListingCounterScope,
) -> Result<CoordinatorLoadResult, String> {
    let index_read = read_persisted_trajectory_index_locked_sync(dir);
    let item_count = index_read
        .as_ref()
        .ok()
        .and_then(|loaded| loaded.as_ref())
        .map(|loaded| loaded.index.entries.len() as u64);
    finish_coordinator_span(
        index_read_span,
        if index_read.is_ok() {
            PerfOutcome::Success
        } else {
            PerfOutcome::Failure
        },
        item_count,
    );
    match index_read {
        Ok(Some(loaded)) if loaded.checkpoint_present => {
            let generation = directory_generation_sync(dir, index_file_names(&loaded.index))?;
            Ok(CoordinatorLoadResult {
                index: loaded.index,
                generation,
                needs_flush: false,
                source_hint,
                cursor: loaded.cursor,
            })
        }
        Ok(Some(loaded)) => {
            let generation = directory_generation_sync(dir, index_file_names(&loaded.index))?;
            Ok(CoordinatorLoadResult {
                index: loaded.index,
                generation,
                needs_flush: true,
                source_hint,
                cursor: loaded.cursor,
            })
        }
        Ok(None) | Err(_) => {
            let (entries, skipped_files) =
                scan_trajectory_index_data_sync(dir, source_hint.clone(), listing_counter_scope)?;
            let index = TrajectoryIndex {
                schema_version: TRAJECTORY_INDEX_SCHEMA_VERSION,
                updated_at: Utc::now().to_rfc3339(),
                entries,
                skipped_files,
                applied_seq: 0,
                baseline: None,
            };
            let generation = directory_generation_sync(dir, index_file_names(&index))?;
            Ok(CoordinatorLoadResult {
                index,
                generation,
                needs_flush: true,
                source_hint,
                cursor: TrajectoryIndexJournalCursor::default(),
            })
        }
    }
}

pub(super) fn read_trajectory_index_sync(dir: &Path) -> Result<Option<TrajectoryIndex>, String> {
    let path = trajectory_index_path(dir);
    let content = match std::fs::read_to_string(&path) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(format!(
                "Failed to read trajectory index {:?}: {error}",
                path
            ))
        }
    };
    let index = serde_json::from_str::<TrajectoryIndex>(&content)
        .map_err(|error| format!("Failed to parse trajectory index {:?}: {error}", path))?;
    if index.schema_version != TRAJECTORY_INDEX_SCHEMA_VERSION {
        return Err(format!(
            "Unsupported trajectory index schema version {} in {:?}",
            index.schema_version, path
        ));
    }
    Ok(Some(index))
}

pub(super) fn apply_mutation_to_index(
    index: &mut TrajectoryIndex,
    mutation: &TrajectoryIndexMutation,
) -> Result<(), String> {
    match mutation {
        TrajectoryIndexMutation::Upsert(entry) => {
            validate_trajectory_index_entry(entry)?;
            index.entries.retain(|existing| existing.id != entry.id);
            index
                .skipped_files
                .retain(|skipped| skipped.file_name != entry.file_name);
            index.entries.push(entry.clone());
        }
        TrajectoryIndexMutation::Remove(chat_ids) => {
            index.entries.retain(|entry| !chat_ids.contains(&entry.id));
        }
        TrajectoryIndexMutation::Reconcile(_) => {}
    }
    index.schema_version = TRAJECTORY_INDEX_SCHEMA_VERSION;
    index.updated_at = Utc::now().to_rfc3339();
    Ok(())
}

pub(super) fn validate_trajectory_index_entry(entry: &TrajectoryIndexEntry) -> Result<(), String> {
    if !index_entry_file_name_is_valid(&entry.file_name)
        || Path::new(&entry.file_name)
            .file_stem()
            .and_then(|stem| stem.to_str())
            != Some(entry.id.as_str())
    {
        return Err(format!(
            "Invalid trajectory index entry path: {}",
            entry.file_name
        ));
    }
    Ok(())
}

pub(super) fn reconcile_trajectory_index_sync(
    dir: &Path,
    index: TrajectoryIndex,
    source_hint: Option<TrajectorySourceIdentity>,
    listing_counter_scope: &TrajectoryIndexListingCounterScope,
) -> Result<(TrajectoryIndex, bool), String> {
    let disk_files = scan_trajectory_dir_files_sync(dir, listing_counter_scope)?;
    let skipped_by_file: HashMap<String, TrajectoryIndexSkippedFile> = index
        .skipped_files
        .iter()
        .cloned()
        .map(|skipped| (skipped.file_name.clone(), skipped))
        .collect();
    let entries_by_file: HashMap<String, TrajectoryIndexEntry> = index
        .entries
        .iter()
        .cloned()
        .map(|entry| (entry.file_name.clone(), entry))
        .collect();
    let mut entries = Vec::with_capacity(disk_files.len());
    let mut skipped_files = Vec::new();
    let mut changed = false;

    for disk in &disk_files {
        if let Some(entry) = entries_by_file.get(&disk.file_name) {
            if entry.file_len == disk.file_len
                && entry.file_modified_unix_ms == disk.file_modified_unix_ms
            {
                entries.push(entry.clone());
                continue;
            }
        }
        if let Some(skipped) = skipped_by_file.get(&disk.file_name) {
            if skipped.file_len == disk.file_len
                && skipped.file_modified_unix_ms == disk.file_modified_unix_ms
            {
                skipped_files.push(skipped.clone());
                continue;
            }
        }
        let path = dir.join(&disk.file_name);
        match read_and_index_single_trajectory_sync(dir, &path, source_hint.clone()) {
            SingleTrajectoryIndexResult::Indexed(entry) => {
                entries.push(entry);
                changed = true;
            }
            SingleTrajectoryIndexResult::Skipped => {
                skipped_files.push(TrajectoryIndexSkippedFile {
                    file_name: disk.file_name.clone(),
                    file_len: disk.file_len,
                    file_modified_unix_ms: disk.file_modified_unix_ms,
                });
                changed = true;
            }
            SingleTrajectoryIndexResult::TransientFailure => {
                if let Some(entry) = entries_by_file.get(&disk.file_name) {
                    entries.push(entry.clone());
                } else if let Some(skipped) = skipped_by_file.get(&disk.file_name) {
                    skipped_files.push(skipped.clone());
                }
            }
        }
    }

    let disk_names: HashSet<&str> = disk_files
        .iter()
        .map(|disk| disk.file_name.as_str())
        .collect();
    changed |= entries_by_file
        .keys()
        .chain(skipped_by_file.keys())
        .any(|file_name| !disk_names.contains(file_name.as_str()));
    let entries = dedupe_entries_by_id(entries);
    Ok((
        TrajectoryIndex {
            schema_version: TRAJECTORY_INDEX_SCHEMA_VERSION,
            updated_at: if changed {
                Utc::now().to_rfc3339()
            } else {
                index.updated_at
            },
            entries,
            skipped_files,
            applied_seq: index.applied_seq,
            baseline: None,
        },
        changed,
    ))
}

pub(super) fn with_trajectory_index_file_lock<T>(
    dir: &Path,
    timeout: Duration,
    retry: Duration,
    lock_wait_span: Option<perf_diagnostics::PerfSpan>,
    operation: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    let lock_path = dir.join(format!(".{TRAJECTORY_INDEX_FILE}.lock"));
    let lock = file_lock::open_lock(&lock_path).map_err(|error| {
        format!(
            "Failed to open trajectory index lock {:?}: {error}",
            lock_path
        )
    });
    let mut lock = match lock {
        Ok(lock) => lock,
        Err(error) => {
            finish_coordinator_span(lock_wait_span, PerfOutcome::Failure, None);
            return Err(error);
        }
    };
    let deadline = Instant::now() + timeout;
    loop {
        match file_lock::try_lock(&mut lock) {
            Ok(_guard) => {
                finish_coordinator_span(lock_wait_span, PerfOutcome::Success, None);
                return operation();
            }
            Err(error) if file_lock::is_already_locked(&error) && Instant::now() < deadline => {
                std::thread::sleep(retry);
            }
            Err(error) if file_lock::is_already_locked(&error) => {
                finish_coordinator_span(lock_wait_span, PerfOutcome::Failure, None);
                return Err(format!(
                    "Timed out waiting for trajectory index lock {:?}",
                    lock_path
                ));
            }
            Err(error) => {
                finish_coordinator_span(lock_wait_span, PerfOutcome::Failure, None);
                return Err(format!(
                    "Failed to acquire trajectory index lock {:?}: {error}",
                    lock_path
                ));
            }
        }
    }
}

pub(super) struct SyncedPersistedIndex {
    index: TrajectoryIndex,
    cursor: TrajectoryIndexJournalCursor,
    checkpoint_present: bool,
    rebuilt: bool,
}

pub(super) fn sync_persisted_index_for_flush_sync(
    dir: &Path,
    cached: Option<SyncedPersistedIndex>,
    source_hint: Option<TrajectorySourceIdentity>,
    index_read_span: Option<perf_diagnostics::PerfSpan>,
    listing_counter_scope: &TrajectoryIndexListingCounterScope,
) -> Result<SyncedPersistedIndex, String> {
    let journal_path = trajectory_index_journal_path(dir);
    let checkpoint_stamp = regular_file_generation(&trajectory_index_path(dir))?;
    let journal_len = regular_file_generation(&journal_path)?
        .map(|generation| generation.file_len)
        .unwrap_or(0);
    if let (Some(cached), true) = (cached, checkpoint_stamp.is_some()) {
        let cursor = cached.cursor;
        if cursor.checkpoint == checkpoint_stamp && journal_len >= cursor.journal_offset {
            let tail =
                read_trajectory_index_journal_tail_sync(&journal_path, cursor.journal_offset);
            if let Ok(tail) = tail {
                if let Ok(scan) = scan_trajectory_index_journal(&tail) {
                    let mut index = cached.index;
                    let last_seq =
                        replay_journal_records(&mut index, &scan.records, cursor.last_seq);
                    finish_coordinator_span(index_read_span, PerfOutcome::Skipped, Some(0));
                    return Ok(SyncedPersistedIndex {
                        index,
                        cursor: TrajectoryIndexJournalCursor {
                            checkpoint: checkpoint_stamp,
                            journal_offset: cursor.journal_offset + scan.consumed,
                            last_seq,
                        },
                        checkpoint_present: true,
                        rebuilt: false,
                    });
                }
            }
        }
    }
    let loaded = load_trajectory_index_for_coordinator_locked_sync(
        dir,
        source_hint,
        index_read_span,
        listing_counter_scope,
    )?;
    Ok(SyncedPersistedIndex {
        index: loaded.index,
        cursor: loaded.cursor,
        checkpoint_present: !loaded.needs_flush,
        rebuilt: loaded.needs_flush,
    })
}

pub(super) struct TrajectoryIndexFlushOutcome {
    index: TrajectoryIndex,
    cursor: TrajectoryIndexJournalCursor,
    checkpointed: bool,
    appended: bool,
}

pub(super) fn trajectory_index_delta(
    baseline_entries: &[TrajectoryIndexEntry],
    index: &TrajectoryIndex,
) -> (Vec<TrajectoryIndexEntry>, Vec<String>) {
    let baseline: HashMap<&str, &TrajectoryIndexEntry> = baseline_entries
        .iter()
        .map(|entry| (entry.id.as_str(), entry))
        .collect();
    let upserts: Vec<TrajectoryIndexEntry> = index
        .entries
        .iter()
        .filter(|entry| baseline.get(entry.id.as_str()) != Some(entry))
        .cloned()
        .collect();
    let current: HashSet<&str> = index
        .entries
        .iter()
        .map(|entry| entry.id.as_str())
        .collect();
    let mut removes: Vec<String> = baseline
        .keys()
        .filter(|id| !current.contains(*id))
        .map(|id| (*id).to_string())
        .collect();
    removes.sort();
    (upserts, removes)
}

pub(super) struct FlushLockOptions {
    timeout: Duration,
    retry: Duration,
}

pub(super) struct FlushSpans {
    index_read: Option<perf_diagnostics::PerfSpan>,
    lock_wait: Option<perf_diagnostics::PerfSpan>,
    index_write: Option<perf_diagnostics::PerfSpan>,
    reconcile: Option<perf_diagnostics::PerfSpan>,
}

// Caller owns the cross-process lock for replacement and journal truncation.
pub(super) fn checkpoint_index_sync(
    dir: &Path,
    synced: SyncedPersistedIndex,
    index_write_span: Option<perf_diagnostics::PerfSpan>,
    listing_counter_scope: &TrajectoryIndexListingCounterScope,
) -> Result<TrajectoryIndexFlushOutcome, String> {
    let mut index = synced.index;
    index.applied_seq = synced.cursor.last_seq;
    let item_count = index.entries.len() as u64;
    let write_result = write_trajectory_index_atomic_sync(dir, &index)
        .and_then(|_| truncate_trajectory_index_journal_sync(dir));
    finish_coordinator_span(
        index_write_span,
        if write_result.is_ok() {
            PerfOutcome::Success
        } else {
            PerfOutcome::Failure
        },
        Some(item_count),
    );
    write_result?;
    listing_counter_scope.record_checkpoint_write();
    record_coordinator_count(PerfComponent::TrajectoryIndexCheckpoint);
    let cursor = TrajectoryIndexJournalCursor {
        checkpoint: regular_file_generation(&trajectory_index_path(dir))?,
        journal_offset: 0,
        last_seq: index.applied_seq,
    };
    Ok(TrajectoryIndexFlushOutcome {
        index,
        cursor,
        checkpointed: true,
        appended: false,
    })
}

pub(super) fn flush_trajectory_index_mutations_sync(
    dir: &Path,
    pending: Vec<PendingTrajectoryIndexMutation>,
    cached: Option<SyncedPersistedIndex>,
    force_checkpoint: bool,
    lock: FlushLockOptions,
    spans: FlushSpans,
    listing_counter_scope: &TrajectoryIndexListingCounterScope,
) -> Result<TrajectoryIndexFlushOutcome, String> {
    std::fs::create_dir_all(dir)
        .map_err(|error| format!("Failed to create trajectory directory {:?}: {error}", dir))?;
    with_trajectory_index_file_lock(dir, lock.timeout, lock.retry, spans.lock_wait, || {
        let synced = sync_persisted_index_for_flush_sync(
            dir,
            cached,
            None,
            spans.index_read,
            listing_counter_scope,
        )?;
        let mut cursor = synced.cursor;
        let checkpoint_present = synced.checkpoint_present;
        let rebuilt = synced.rebuilt;
        let baseline_entries = synced.index.entries.clone();
        let baseline_skipped = synced.index.skipped_files.clone();
        let mut index = synced.index;
        let mut reconcile_span = spans.reconcile;
        for pending in pending {
            match pending.mutation {
                TrajectoryIndexMutation::Reconcile(source_hint) => {
                    let reconciled = reconcile_trajectory_index_sync(
                        dir,
                        index,
                        source_hint,
                        listing_counter_scope,
                    );
                    let item_count = reconciled
                        .as_ref()
                        .ok()
                        .map(|(index, _)| index.entries.len() as u64);
                    finish_coordinator_span(
                        reconcile_span.take(),
                        if reconciled.is_ok() {
                            PerfOutcome::Success
                        } else {
                            PerfOutcome::Failure
                        },
                        item_count,
                    );
                    index = reconciled?.0;
                }
                mutation => apply_mutation_to_index(&mut index, &mutation)?,
            }
        }
        let (upserts, removes) = trajectory_index_delta(&baseline_entries, &index);
        let skipped_changed = index.skipped_files != baseline_skipped;
        let has_delta = !upserts.is_empty() || !removes.is_empty() || skipped_changed;
        let journal_len = regular_file_generation(&trajectory_index_journal_path(dir))?
            .map(|generation| generation.file_len)
            .unwrap_or(0);
        if !has_delta && !rebuilt && checkpoint_present && journal_len == 0 {
            finish_coordinator_span(spans.index_write, PerfOutcome::Skipped, Some(0));
            index.applied_seq = cursor.last_seq;
            return Ok(TrajectoryIndexFlushOutcome {
                index,
                cursor,
                checkpointed: false,
                appended: false,
            });
        }
        let needs_checkpoint = force_checkpoint
            || rebuilt
            || !checkpoint_present
            || journal_len >= JOURNAL_CHECKPOINT_MAX_BYTES;
        if !has_delta && !needs_checkpoint {
            finish_coordinator_span(spans.index_write, PerfOutcome::Skipped, Some(0));
            index.applied_seq = cursor.last_seq;
            return Ok(TrajectoryIndexFlushOutcome {
                index,
                cursor,
                checkpointed: false,
                appended: false,
            });
        }
        if needs_checkpoint {
            return checkpoint_index_sync(
                dir,
                SyncedPersistedIndex {
                    index,
                    cursor,
                    checkpoint_present,
                    rebuilt,
                },
                spans.index_write,
                listing_counter_scope,
            );
        }
        let seq = cursor.last_seq + 1;
        let payload = TrajectoryIndexJournalPayload {
            seq,
            writer_id: trajectory_index_writer_id().to_string(),
            ts_ms: Utc::now().timestamp_millis(),
            updated_at: index.updated_at.clone(),
            upserts,
            removes,
            skipped: skipped_changed.then(|| index.skipped_files.clone()),
        };
        let item_count = (payload.upserts.len() + payload.removes.len()) as u64;
        let append_result = append_trajectory_index_journal_sync(dir, journal_len, payload);
        finish_coordinator_span(
            spans.index_write,
            if append_result.is_ok() {
                PerfOutcome::Success
            } else {
                PerfOutcome::Failure
            },
            Some(item_count),
        );
        let journal_offset = append_result?;
        listing_counter_scope.record_journal_append();
        record_coordinator_count(PerfComponent::TrajectoryIndexJournalAppend);
        index.applied_seq = seq;
        cursor.journal_offset = journal_offset;
        cursor.last_seq = seq;
        Ok(TrajectoryIndexFlushOutcome {
            index,
            cursor,
            checkpointed: false,
            appended: true,
        })
    })
}
