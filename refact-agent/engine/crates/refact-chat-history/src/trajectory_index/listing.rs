use super::*;

pub async fn upsert_trajectory_index_entry(
    dir: &Path,
    entry: TrajectoryIndexEntry,
) -> Result<(), String> {
    let diagnostics_enabled = perf_diagnostics::is_enabled();
    let lock_span = diagnostics_enabled.then(|| {
        let index_path = trajectory_index_path(dir);
        perf_diagnostics::span(
            PerfComponent::TrajectoryIndexLockWait,
            Some(&entry.id),
            Some(&index_path),
        )
    });
    let lock = get_trajectory_index_lock(dir).await;
    let _guard = lock.lock().await;
    if let Some(lock_span) = lock_span {
        lock_span.finish(PerfOutcome::Success, None, None, None, None);
    }
    let mut index = match read_trajectory_index(dir).await {
        Ok(Some(index)) => index,
        Ok(None) | Err(_) => {
            let (entries, skipped_files) = scan_trajectory_index_data(dir, None).await?;
            TrajectoryIndex {
                schema_version: TRAJECTORY_INDEX_SCHEMA_VERSION,
                updated_at: Utc::now().to_rfc3339(),
                entries,
                skipped_files,
                applied_seq: 0,
                baseline: None,
            }
        }
    };
    index.entries.retain(|existing| existing.id != entry.id);
    index
        .skipped_files
        .retain(|skipped| skipped.file_name != entry.file_name);
    index.entries.push(entry);
    index.updated_at = Utc::now().to_rfc3339();
    write_trajectory_index_atomic_owned(dir, index).await
}

pub async fn upsert_trajectory_index_entry_with_rollout(
    coordinator: &TrajectoryIndexCoordinator,
    dir: &Path,
    entry: TrajectoryIndexEntry,
) -> Result<(), String> {
    if trajectory_index_coordinator_rollout_enabled() {
        coordinator.upsert(dir, entry).await
    } else {
        upsert_trajectory_index_entry(dir, entry).await
    }
}

pub async fn upsert_trajectory_index_entry_from_owned_value_with_rollout(
    coordinator: &TrajectoryIndexCoordinator,
    dir: &Path,
    path: &Path,
    value: serde_json::Value,
    source_hint: Option<TrajectorySourceIdentity>,
) -> Result<(), String> {
    let dir_owned = dir.to_path_buf();
    let path_owned = path.to_path_buf();
    let entry = tokio::task::spawn_blocking(move || {
        entry_from_trajectory_value(&dir_owned, &path_owned, &value, source_hint)
    })
    .await
    .map_err(|e| format!("Trajectory index entry task failed for {:?}: {e}", path))??;
    upsert_trajectory_index_entry_with_rollout(coordinator, dir, entry).await
}

pub async fn upsert_trajectory_index_entry_from_value(
    dir: &Path,
    path: &Path,
    value: &serde_json::Value,
    source_hint: Option<TrajectorySourceIdentity>,
) -> Result<(), String> {
    let entry = entry_from_trajectory_value(dir, path, value, source_hint)?;
    upsert_trajectory_index_entry(dir, entry).await
}

pub async fn upsert_trajectory_index_entry_from_owned_value(
    dir: &Path,
    path: &Path,
    value: serde_json::Value,
    source_hint: Option<TrajectorySourceIdentity>,
) -> Result<(), String> {
    let dir_owned = dir.to_path_buf();
    let path_owned = path.to_path_buf();
    let entry = tokio::task::spawn_blocking(move || {
        entry_from_trajectory_value(&dir_owned, &path_owned, &value, source_hint)
    })
    .await
    .map_err(|e| format!("Trajectory index entry task failed for {:?}: {e}", path))??;
    upsert_trajectory_index_entry(dir, entry).await
}

pub async fn remove_trajectory_index_entries(
    dir: &Path,
    chat_ids: &HashSet<String>,
) -> Result<(), String> {
    if chat_ids.is_empty() {
        return Ok(());
    }
    let diagnostics_enabled = perf_diagnostics::is_enabled();
    let lock_span = diagnostics_enabled.then(|| {
        let index_path = trajectory_index_path(dir);
        perf_diagnostics::span(
            PerfComponent::TrajectoryIndexLockWait,
            None,
            Some(&index_path),
        )
    });
    let lock = get_trajectory_index_lock(dir).await;
    let _guard = lock.lock().await;
    if let Some(lock_span) = lock_span {
        lock_span.finish(PerfOutcome::Success, None, None, None, None);
    }
    let mut index = match read_trajectory_index(dir).await? {
        Some(index) => index,
        None => return Ok(()),
    };
    let before = index.entries.len();
    index.entries.retain(|entry| !chat_ids.contains(&entry.id));
    if index.entries.len() == before {
        return Ok(());
    }
    index.updated_at = Utc::now().to_rfc3339();
    write_trajectory_index_atomic_owned(dir, index).await
}

pub async fn remove_trajectory_index_entry(dir: &Path, chat_id: &str) -> Result<(), String> {
    let diagnostics_enabled = perf_diagnostics::is_enabled();
    let lock_span = diagnostics_enabled.then(|| {
        let index_path = trajectory_index_path(dir);
        perf_diagnostics::span(
            PerfComponent::TrajectoryIndexLockWait,
            Some(chat_id),
            Some(&index_path),
        )
    });
    let lock = get_trajectory_index_lock(dir).await;
    let _guard = lock.lock().await;
    if let Some(lock_span) = lock_span {
        lock_span.finish(PerfOutcome::Success, None, None, None, None);
    }
    let mut index = match read_trajectory_index(dir).await? {
        Some(index) => index,
        None => return Ok(()),
    };
    let before = index.entries.len();
    index.entries.retain(|entry| entry.id != chat_id);
    if index.entries.len() == before {
        return Ok(());
    }
    index.updated_at = Utc::now().to_rfc3339();
    write_trajectory_index_atomic_owned(dir, index).await
}

pub async fn remove_trajectory_index_entries_with_rollout(
    coordinator: &TrajectoryIndexCoordinator,
    dir: &Path,
    chat_ids: &HashSet<String>,
) -> Result<(), String> {
    if trajectory_index_coordinator_rollout_enabled() {
        coordinator.remove(dir, chat_ids.clone()).await
    } else {
        remove_trajectory_index_entries(dir, chat_ids).await
    }
}

pub async fn remove_trajectory_index_entry_with_rollout(
    coordinator: &TrajectoryIndexCoordinator,
    dir: &Path,
    chat_id: &str,
) -> Result<(), String> {
    remove_trajectory_index_entries_with_rollout(
        coordinator,
        dir,
        &HashSet::from([chat_id.to_string()]),
    )
    .await
}

pub async fn list_trajectory_entries_with_rollout(
    coordinator: &TrajectoryIndexCoordinator,
    dir: &Path,
    source_hint: Option<TrajectorySourceIdentity>,
) -> Result<Vec<TrajectoryIndexEntry>, String> {
    list_trajectory_entries_with_rollout_for(
        coordinator,
        dir,
        source_hint,
        TrajectoryIndexListingCaller::ChatApi,
    )
    .await
}

pub async fn list_trajectory_entries_with_rollout_for(
    coordinator: &TrajectoryIndexCoordinator,
    dir: &Path,
    source_hint: Option<TrajectorySourceIdentity>,
    caller: TrajectoryIndexListingCaller,
) -> Result<Vec<TrajectoryIndexEntry>, String> {
    coordinator.record_listing_caller(caller);
    if trajectory_index_coordinator_rollout_enabled() {
        coordinator.list_entries(dir, source_hint).await
    } else {
        list_trajectory_entries_from_index_or_rebuild_with_counter_scope(
            dir,
            source_hint,
            &coordinator.listing_counter_scope,
        )
        .await
    }
}

pub async fn list_trajectory_entries_from_index_or_rebuild(
    dir: &Path,
    source_hint: Option<TrajectorySourceIdentity>,
) -> Result<Vec<TrajectoryIndexEntry>, String> {
    list_trajectory_entries_from_index_or_rebuild_with_counter_scope(
        dir,
        source_hint,
        &GLOBAL_TRAJECTORY_INDEX_LISTING_COUNTER_SCOPE,
    )
    .await
}

pub(super) async fn list_trajectory_entries_from_index_or_rebuild_with_counter_scope(
    dir: &Path,
    source_hint: Option<TrajectorySourceIdentity>,
    listing_counter_scope: &TrajectoryIndexListingCounterScope,
) -> Result<Vec<TrajectoryIndexEntry>, String> {
    let cached = {
        let mut cache = legacy_trajectory_index_cache().lock().await;
        if let Some(entry) = cache.get_mut(dir) {
            entry.last_access = next_trajectory_index_cache_access_stamp();
            Some(entry.clone())
        } else {
            None
        }
    };
    if let Some(cached) = cached {
        if let Ok(true) = cached_index_is_fresh(
            dir,
            index_file_names(&cached.index),
            cached.generation.clone(),
        )
        .await
        {
            listing_counter_scope.record_listing_cache_hit();
            return Ok(cached.index.entries);
        }
    }
    let diagnostics_enabled = perf_diagnostics::is_enabled();
    let lock_span = diagnostics_enabled.then(|| {
        let index_path = trajectory_index_path(dir);
        perf_diagnostics::span(
            PerfComponent::TrajectoryIndexLockWait,
            None,
            Some(&index_path),
        )
    });
    let lock = get_trajectory_index_lock(dir).await;
    let _guard = lock.lock().await;
    if let Some(lock_span) = lock_span {
        lock_span.finish(PerfOutcome::Success, None, None, None, None);
    }
    let disk_files = scan_trajectory_dir_files(dir, listing_counter_scope).await?;

    let (existing_entries, existing_skipped, index_unreadable, baseline) =
        match read_trajectory_index_with_counter_scope(dir, listing_counter_scope).await {
            Ok(Some(index)) => (index.entries, index.skipped_files, false, index.baseline),
            Ok(None) => (Vec::new(), Vec::new(), false, None),
            Err(_) => (Vec::new(), Vec::new(), true, None),
        };

    let skipped_by_file: HashMap<String, TrajectoryIndexSkippedFile> = existing_skipped
        .into_iter()
        .map(|skipped| (skipped.file_name.clone(), skipped))
        .collect();

    let by_file: HashMap<String, TrajectoryIndexEntry> = existing_entries
        .into_iter()
        .map(|entry| (entry.file_name.clone(), entry))
        .collect();

    let mut new_entries: Vec<TrajectoryIndexEntry> = Vec::with_capacity(disk_files.len());
    let mut new_skipped = Vec::new();
    let mut content_changed = index_unreadable;

    for disk in &disk_files {
        if let Some(entry) = by_file.get(&disk.file_name) {
            if entry.file_len == disk.file_len
                && entry.file_modified_unix_ms == disk.file_modified_unix_ms
            {
                new_entries.push(entry.clone());
                continue;
            }
        }
        if let Some(skipped) = skipped_by_file.get(&disk.file_name) {
            if skipped.file_len == disk.file_len
                && skipped.file_modified_unix_ms == disk.file_modified_unix_ms
            {
                new_skipped.push(skipped.clone());
                continue;
            }
        }
        let path = dir.join(&disk.file_name);
        match read_and_index_single_trajectory(dir, &path, source_hint.clone()).await {
            SingleTrajectoryIndexResult::Indexed(entry) => {
                new_entries.push(entry);
                content_changed = true;
            }
            SingleTrajectoryIndexResult::Skipped => {
                new_skipped.push(TrajectoryIndexSkippedFile {
                    file_name: disk.file_name.clone(),
                    file_len: disk.file_len,
                    file_modified_unix_ms: disk.file_modified_unix_ms,
                });
                content_changed = true;
            }
            SingleTrajectoryIndexResult::TransientFailure => {
                if let Some(entry) = by_file.get(&disk.file_name) {
                    new_entries.push(entry.clone());
                } else if let Some(skipped) = skipped_by_file.get(&disk.file_name) {
                    new_skipped.push(skipped.clone());
                }
            }
        }
    }

    if !content_changed {
        let disk_names: HashSet<&str> = disk_files
            .iter()
            .map(|disk| disk.file_name.as_str())
            .collect();
        content_changed = by_file
            .keys()
            .chain(skipped_by_file.keys())
            .any(|file_name| !disk_names.contains(file_name.as_str()));
    }

    let new_entries = dedupe_entries_by_id(new_entries);

    let index = if content_changed {
        let index = TrajectoryIndex {
            schema_version: TRAJECTORY_INDEX_SCHEMA_VERSION,
            updated_at: Utc::now().to_rfc3339(),
            entries: new_entries.clone(),
            skipped_files: new_skipped,
            applied_seq: 0,
            baseline,
        };
        write_trajectory_index_atomic_owned(dir, index.clone()).await?;
        index
    } else {
        let existing = read_trajectory_index_with_counter_scope(dir, listing_counter_scope)
            .await?
            .unwrap_or_default();
        TrajectoryIndex {
            schema_version: TRAJECTORY_INDEX_SCHEMA_VERSION,
            updated_at: existing.updated_at,
            entries: new_entries.clone(),
            skipped_files: new_skipped,
            applied_seq: existing.applied_seq,
            baseline: None,
        }
    };
    cache_legacy_trajectory_index(dir, index).await;
    Ok(new_entries)
}

pub fn list_candidate_from_entry(
    dir: &Path,
    entry: &TrajectoryIndexEntry,
) -> TrajectoryListCandidate {
    TrajectoryListCandidate {
        id: entry.id.clone(),
        updated_at: entry.updated_at.clone(),
        path: trajectory_file_path_for_entry(dir, entry),
        indexed_meta: Some(meta_from_entry(dir, entry)),
        indexed_file_len: Some(entry.file_len),
        indexed_file_modified_unix_ms: Some(entry.file_modified_unix_ms),
    }
}

pub fn meta_from_entry(_dir: &Path, entry: &TrajectoryIndexEntry) -> TrajectoryMeta {
    let source = TrajectorySourceIdentity::from(&entry.source);
    let (task_id, task_role, agent_id, card_id) = match &entry.source {
        TrajectoryIndexSource::Task {
            task_id,
            role,
            agent_id,
            card_id,
            ..
        } => (
            Some(task_id.clone()),
            Some(role.clone()),
            agent_id.clone(),
            card_id.clone(),
        ),
        _ => (None, None, None, None),
    };
    TrajectoryMeta {
        id: entry.id.clone(),
        title: entry.title.clone(),
        created_at: entry.created_at.clone(),
        updated_at: entry.updated_at.clone(),
        model: entry.model.clone(),
        mode: entry.mode.clone(),
        message_count: entry.message_count,
        parent_id: entry.parent_id.clone(),
        link_type: entry.link_type.clone(),
        task_id,
        task_role,
        agent_id,
        card_id,
        session_state: None,
        root_chat_id: entry.root_chat_id.clone(),
        worktree: entry.worktree.clone(),
        total_lines_added: entry.total_lines_added,
        total_lines_removed: entry.total_lines_removed,
        tasks_total: entry.tasks_total,
        tasks_done: entry.tasks_done,
        tasks_failed: entry.tasks_failed,
        total_prompt_tokens: entry.total_prompt_tokens,
        total_completion_tokens: entry.total_completion_tokens,
        total_tokens: entry.total_tokens,
        total_cache_read_tokens: entry.total_cache_read_tokens,
        total_cache_creation_tokens: entry.total_cache_creation_tokens,
        total_cost_usd: entry.total_cost_usd,
        source,
    }
}
