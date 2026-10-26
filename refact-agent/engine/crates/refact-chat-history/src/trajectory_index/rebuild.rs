use super::*;

pub(super) fn dedupe_entries_by_id(
    entries: Vec<TrajectoryIndexEntry>,
) -> Vec<TrajectoryIndexEntry> {
    let mut position_by_id: HashMap<String, usize> = HashMap::new();
    let mut deduped: Vec<TrajectoryIndexEntry> = Vec::with_capacity(entries.len());
    for entry in entries {
        match position_by_id.get(&entry.id) {
            Some(&position) => {
                if entry.file_modified_unix_ms > deduped[position].file_modified_unix_ms {
                    deduped[position] = entry;
                }
            }
            None => {
                position_by_id.insert(entry.id.clone(), deduped.len());
                deduped.push(entry);
            }
        }
    }
    deduped
}

pub(super) struct DiskTrajectoryFile {
    pub(super) file_name: String,
    pub(super) file_len: u64,
    pub(super) file_modified_unix_ms: i64,
}

pub(super) fn scan_trajectory_dir_files_sync(
    dir: &Path,
    listing_counter_scope: &TrajectoryIndexListingCounterScope,
) -> Result<Vec<DiskTrajectoryFile>, String> {
    listing_counter_scope.record_directory_scan();
    let read_dir = match std::fs::read_dir(dir) {
        Ok(read_dir) => read_dir,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(format!(
                "Failed to read trajectory directory {:?}: {error}",
                dir
            ))
        }
    };
    let mut scan_targets = vec![(dir.to_path_buf(), None)];
    for entry in read_dir.flatten() {
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() && !file_type.is_symlink() && !name.starts_with('.') {
            scan_targets.push((entry.path(), Some(name)));
        }
    }
    let mut files = Vec::new();
    for (scan_dir, prefix) in scan_targets {
        let Ok(entries) = std::fs::read_dir(scan_dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(file_name) = entry.file_name().into_string() else {
                continue;
            };
            if file_name == TRAJECTORY_INDEX_FILE
                || file_name.starts_with('.')
                || !file_name.ends_with(".json")
            {
                continue;
            }
            let Ok(metadata) = std::fs::symlink_metadata(entry.path()) else {
                continue;
            };
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                continue;
            }
            let Ok(file_modified_unix_ms) = unix_modified_ms(&metadata) else {
                continue;
            };
            let file_name = prefix
                .as_ref()
                .map(|prefix| format!("{prefix}/{file_name}"))
                .unwrap_or(file_name);
            files.push(DiskTrajectoryFile {
                file_name,
                file_len: metadata.len(),
                file_modified_unix_ms,
            });
        }
    }
    Ok(files)
}

pub(super) fn read_and_index_single_trajectory_sync(
    dir: &Path,
    path: &Path,
    source_hint: Option<TrajectorySourceIdentity>,
) -> SingleTrajectoryIndexResult {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => metadata,
        _ => return SingleTrajectoryIndexResult::TransientFailure,
    };
    if metadata.len() == 0 {
        return SingleTrajectoryIndexResult::TransientFailure;
    }
    let content = match std::fs::read_to_string(path) {
        Ok(content) => content,
        Err(_) => return SingleTrajectoryIndexResult::TransientFailure,
    };
    let value = match serde_json::from_str::<serde_json::Value>(&content) {
        Ok(value) => value,
        Err(error) => {
            tracing::warn!("Failed to parse trajectory {:?}: {}", path, error);
            return SingleTrajectoryIndexResult::Skipped;
        }
    };
    if value
        .get("link_type")
        .and_then(|value| value.as_str())
        .is_some_and(|link_type| link_type.starts_with("internal:"))
    {
        return SingleTrajectoryIndexResult::Skipped;
    }
    match entry_from_trajectory_value(dir, path, &value, source_hint) {
        Ok(entry) => SingleTrajectoryIndexResult::Indexed(entry),
        Err(error) => {
            tracing::debug!("Skipping non-indexable trajectory {:?}: {}", path, error);
            SingleTrajectoryIndexResult::Skipped
        }
    }
}

pub(super) fn scan_trajectory_index_data_sync(
    dir: &Path,
    source_hint: Option<TrajectorySourceIdentity>,
    listing_counter_scope: &TrajectoryIndexListingCounterScope,
) -> Result<(Vec<TrajectoryIndexEntry>, Vec<TrajectoryIndexSkippedFile>), String> {
    let disk_files = scan_trajectory_dir_files_sync(dir, listing_counter_scope)?;
    let mut entries = Vec::new();
    let mut skipped_files = Vec::new();
    for disk in disk_files {
        let path = dir.join(&disk.file_name);
        match read_and_index_single_trajectory_sync(dir, &path, source_hint.clone()) {
            SingleTrajectoryIndexResult::Indexed(entry) => entries.push(entry),
            SingleTrajectoryIndexResult::Skipped => {
                skipped_files.push(TrajectoryIndexSkippedFile {
                    file_name: disk.file_name,
                    file_len: disk.file_len,
                    file_modified_unix_ms: disk.file_modified_unix_ms,
                })
            }
            SingleTrajectoryIndexResult::TransientFailure => {}
        }
    }
    Ok((dedupe_entries_by_id(entries), skipped_files))
}

pub(super) async fn scan_trajectory_dir_files(
    dir: &Path,
    listing_counter_scope: &TrajectoryIndexListingCounterScope,
) -> Result<Vec<DiskTrajectoryFile>, String> {
    listing_counter_scope.record_directory_scan();
    let dir = dir.to_path_buf();
    let dir_for_err = dir.clone();
    tokio::task::spawn_blocking(move || {
        let read_dir = match std::fs::read_dir(&dir) {
            Ok(read_dir) => read_dir,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => {
                return Err(format!(
                    "Failed to read trajectory directory {:?}: {e}",
                    dir
                ))
            }
        };
        let mut scan_targets: Vec<(std::path::PathBuf, Option<String>)> = vec![(dir.clone(), None)];
        for entry in read_dir.flatten() {
            let name = match entry.file_name().into_string() {
                Ok(name) => name,
                Err(_) => continue,
            };
            if name.starts_with('.') {
                continue;
            }
            match entry.file_type() {
                Ok(file_type) if file_type.is_dir() && !file_type.is_symlink() => {
                    scan_targets.push((entry.path(), Some(name)));
                }
                _ => {}
            }
        }

        let mut files = Vec::new();
        for (scan_dir, prefix) in scan_targets {
            let scan_read_dir = match std::fs::read_dir(&scan_dir) {
                Ok(read_dir) => read_dir,
                Err(_) => continue,
            };
            for entry in scan_read_dir {
                let entry = match entry {
                    Ok(entry) => entry,
                    Err(_) => continue,
                };
                let file_name = match entry.file_name().into_string() {
                    Ok(name) => name,
                    Err(_) => continue,
                };
                if file_name == TRAJECTORY_INDEX_FILE
                    || file_name.starts_with('.')
                    || !file_name.ends_with(".json")
                {
                    continue;
                }
                let metadata = match std::fs::symlink_metadata(entry.path()) {
                    Ok(metadata) => metadata,
                    Err(_) => continue,
                };
                if metadata.file_type().is_symlink() || !metadata.is_file() {
                    continue;
                }
                let file_modified_unix_ms = match unix_modified_ms(&metadata) {
                    Ok(ms) => ms,
                    Err(_) => continue,
                };
                let relative_name = match &prefix {
                    Some(prefix) => format!("{prefix}/{file_name}"),
                    None => file_name,
                };
                files.push(DiskTrajectoryFile {
                    file_name: relative_name,
                    file_len: metadata.len(),
                    file_modified_unix_ms,
                });
            }
        }
        Ok(files)
    })
    .await
    .map_err(|e| format!("Failed to scan trajectory directory {:?}: {e}", dir_for_err))?
}

pub(super) async fn read_and_index_single_trajectory(
    dir: &Path,
    path: &Path,
    source_hint: Option<TrajectorySourceIdentity>,
) -> SingleTrajectoryIndexResult {
    const INTERNAL_LINK_TAIL_BYTES: u64 = 64 * 1024;

    let mut file = match fs::File::open(path).await {
        Ok(file) => file,
        Err(_) => return SingleTrajectoryIndexResult::TransientFailure,
    };
    let file_len = match file.metadata().await {
        Ok(metadata) => metadata.len(),
        Err(_) => return SingleTrajectoryIndexResult::TransientFailure,
    };
    let tail_start = file_len.saturating_sub(INTERNAL_LINK_TAIL_BYTES);
    if file.seek(SeekFrom::Start(tail_start)).await.is_err() {
        return SingleTrajectoryIndexResult::TransientFailure;
    }
    let mut tail = Vec::new();
    if file.read_to_end(&mut tail).await.is_err() {
        return SingleTrajectoryIndexResult::TransientFailure;
    }
    let tail = String::from_utf8_lossy(&tail);
    if tail.lines().any(|line| {
        let Some(value) = line
            .strip_suffix(',')
            .unwrap_or(line)
            .strip_prefix("  \"link_type\": ")
        else {
            return false;
        };
        serde_json::from_str::<String>(value).is_ok_and(|value| value.starts_with("internal:"))
    }) {
        return SingleTrajectoryIndexResult::Skipped;
    }

    let content = match fs::read_to_string(path).await {
        Ok(content) => content,
        Err(_) => return SingleTrajectoryIndexResult::TransientFailure,
    };
    let dir = dir.to_path_buf();
    let path = path.to_path_buf();
    match tokio::task::spawn_blocking(move || {
        let value = serde_json::from_str::<serde_json::Value>(&content).map_err(|e| {
            tracing::warn!("Failed to parse trajectory {:?}: {}", path, e);
        })?;
        if value
            .get("link_type")
            .and_then(|value| value.as_str())
            .is_some_and(|link_type| link_type.starts_with("internal:"))
        {
            return Err(());
        }
        entry_from_trajectory_value(&dir, &path, &value, source_hint).map_err(|e| {
            tracing::debug!("Skipping non-indexable trajectory {:?}: {}", path, e);
        })
    })
    .await
    {
        Ok(Ok(entry)) => SingleTrajectoryIndexResult::Indexed(entry),
        Ok(Err(())) => SingleTrajectoryIndexResult::Skipped,
        Err(_) => SingleTrajectoryIndexResult::TransientFailure,
    }
}

pub(super) enum SingleTrajectoryIndexResult {
    Indexed(TrajectoryIndexEntry),
    Skipped,
    TransientFailure,
}

pub(super) async fn scan_trajectory_index_data(
    dir: &Path,
    source_hint: Option<TrajectorySourceIdentity>,
) -> Result<(Vec<TrajectoryIndexEntry>, Vec<TrajectoryIndexSkippedFile>), String> {
    let disk_files =
        scan_trajectory_dir_files(dir, &GLOBAL_TRAJECTORY_INDEX_LISTING_COUNTER_SCOPE).await?;
    let mut entries = Vec::new();
    let mut skipped_files = Vec::new();
    for disk in disk_files {
        let path = dir.join(&disk.file_name);
        match read_and_index_single_trajectory(dir, &path, source_hint.clone()).await {
            SingleTrajectoryIndexResult::Indexed(entry) => entries.push(entry),
            SingleTrajectoryIndexResult::Skipped => {
                skipped_files.push(TrajectoryIndexSkippedFile {
                    file_name: disk.file_name,
                    file_len: disk.file_len,
                    file_modified_unix_ms: disk.file_modified_unix_ms,
                });
            }
            SingleTrajectoryIndexResult::TransientFailure => {}
        }
    }
    Ok((dedupe_entries_by_id(entries), skipped_files))
}

pub async fn rebuild_trajectory_index_from_disk(
    dir: &Path,
    source_hint: Option<TrajectorySourceIdentity>,
) -> Result<Vec<TrajectoryIndexEntry>, String> {
    if !perf_diagnostics::is_enabled() {
        return rebuild_trajectory_index_from_disk_inner(dir, source_hint).await;
    }
    let index_path = trajectory_index_path(dir);
    let span = perf_diagnostics::span(
        PerfComponent::TrajectoryIndexRebuild,
        None,
        Some(&index_path),
    );
    let result = rebuild_trajectory_index_from_disk_inner(dir, source_hint).await;
    span.finish(
        if result.is_ok() {
            PerfOutcome::Success
        } else {
            PerfOutcome::Failure
        },
        None,
        result.as_ref().ok().map(|entries| entries.len() as u64),
        None,
        None,
    );
    result
}

pub(super) async fn rebuild_trajectory_index_from_disk_inner(
    dir: &Path,
    source_hint: Option<TrajectorySourceIdentity>,
) -> Result<Vec<TrajectoryIndexEntry>, String> {
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
    let baseline = read_trajectory_index(dir)
        .await
        .ok()
        .flatten()
        .and_then(|index| index.baseline);
    let (entries, skipped_files) = scan_trajectory_index_data(dir, source_hint).await?;
    let index = TrajectoryIndex {
        schema_version: TRAJECTORY_INDEX_SCHEMA_VERSION,
        updated_at: Utc::now().to_rfc3339(),
        entries: entries.clone(),
        skipped_files,
        applied_seq: 0,
        baseline,
    };
    write_trajectory_index_atomic_owned(dir, index).await?;
    Ok(entries)
}
