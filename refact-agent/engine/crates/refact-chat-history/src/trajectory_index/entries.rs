use super::*;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TrajectoryIndex {
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
    #[serde(default)]
    pub updated_at: String,
    #[serde(default)]
    pub entries: Vec<TrajectoryIndexEntry>,
    #[serde(default)]
    pub skipped_files: Vec<TrajectoryIndexSkippedFile>,
    #[serde(default)]
    pub applied_seq: u64,
    // Read provenance is process-local: never recursively serialize old snapshots.
    #[serde(skip)]
    pub(super) baseline: Option<Arc<TrajectoryIndex>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TrajectoryIndexSkippedFile {
    pub file_name: String,
    pub file_len: u64,
    pub file_modified_unix_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TrajectoryIndexEntry {
    pub id: String,
    pub file_name: String,
    #[serde(default)]
    pub source: TrajectoryIndexSource,
    pub created_at: String,
    pub updated_at: String,
    pub title: String,
    pub model: String,
    pub mode: String,
    pub message_count: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root_chat_id: Option<String>,
    #[serde(default)]
    pub is_title_generated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<WorktreeMeta>,
    #[serde(default)]
    pub total_lines_added: i64,
    #[serde(default)]
    pub total_lines_removed: i64,
    #[serde(default)]
    pub tasks_total: i32,
    #[serde(default)]
    pub tasks_done: i32,
    #[serde(default)]
    pub tasks_failed: i32,
    #[serde(default)]
    pub total_prompt_tokens: u64,
    #[serde(default)]
    pub total_completion_tokens: u64,
    #[serde(default)]
    pub total_tokens: u64,
    #[serde(default)]
    pub total_cache_read_tokens: u64,
    #[serde(default)]
    pub total_cache_creation_tokens: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_cost_usd: Option<f64>,
    #[serde(default)]
    pub displayable_chat: bool,
    #[serde(default)]
    pub waiting_for_card_ids: Vec<String>,
    #[serde(default)]
    pub file_len: u64,
    #[serde(default)]
    pub file_modified_unix_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TrajectoryIndexSource {
    Normal,
    Task {
        task_id: String,
        role: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        card_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        planner_chat_id: Option<String>,
    },
    Buddy,
}

impl Default for TrajectoryIndexSource {
    fn default() -> Self {
        Self::Normal
    }
}

impl From<&TrajectorySourceIdentity> for TrajectoryIndexSource {
    fn from(value: &TrajectorySourceIdentity) -> Self {
        match value {
            TrajectorySourceIdentity::Normal => Self::Normal,
            TrajectorySourceIdentity::Buddy => Self::Buddy,
            TrajectorySourceIdentity::Task {
                task_id,
                role,
                agent_id,
                card_id,
                planner_chat_id,
            } => Self::Task {
                task_id: task_id.clone(),
                role: role.clone(),
                agent_id: agent_id.clone(),
                card_id: card_id.clone(),
                planner_chat_id: planner_chat_id.clone(),
            },
        }
    }
}

impl From<&TrajectoryIndexSource> for TrajectorySourceIdentity {
    fn from(value: &TrajectoryIndexSource) -> Self {
        match value {
            TrajectoryIndexSource::Normal => Self::Normal,
            TrajectoryIndexSource::Buddy => Self::Buddy,
            TrajectoryIndexSource::Task {
                task_id,
                role,
                agent_id,
                card_id,
                planner_chat_id,
            } => Self::Task {
                task_id: task_id.clone(),
                role: role.clone(),
                agent_id: agent_id.clone(),
                card_id: card_id.clone(),
                planner_chat_id: planner_chat_id.clone(),
            },
        }
    }
}

pub(super) fn default_schema_version() -> u32 {
    TRAJECTORY_INDEX_SCHEMA_VERSION
}

pub fn trajectory_index_journal_path(dir: &Path) -> PathBuf {
    dir.join(TRAJECTORY_INDEX_JOURNAL_FILE)
}

pub fn trajectory_index_path(dir: &Path) -> PathBuf {
    dir.join(TRAJECTORY_INDEX_FILE)
}

pub fn trajectory_file_path_for_entry(dir: &Path, entry: &TrajectoryIndexEntry) -> PathBuf {
    dir.join(&entry.file_name)
}

pub(super) fn write_trajectory_index_atomic_sync(
    dir: &Path,
    index: &TrajectoryIndex,
) -> Result<(), String> {
    write_trajectory_index_atomic_sync_with_rename_failure(dir, index, false)
}

pub(super) fn write_trajectory_index_atomic_sync_with_rename_failure(
    dir: &Path,
    index: &TrajectoryIndex,
    force_rename_failure: bool,
) -> Result<(), String> {
    let path = trajectory_index_path(dir);
    let tmp_path = dir.join(format!(".{}.tmp-{}", TRAJECTORY_INDEX_FILE, Uuid::new_v4()));
    let content = serde_json::to_vec(index)
        .map_err(|error| format!("Failed to serialize trajectory index {:?}: {error}", path))?;
    let result = (|| {
        std::fs::write(&tmp_path, content).map_err(|error| {
            format!(
                "Failed to write temporary trajectory index {:?}: {error}",
                tmp_path
            )
        })?;
        if force_rename_failure {
            return Err("Failed to rename: injected rename failure".to_string());
        }
        #[cfg(windows)]
        if path.exists() {
            let backup_path = dir.join(format!(
                ".{}.replace-{}",
                TRAJECTORY_INDEX_FILE,
                Uuid::new_v4()
            ));
            std::fs::rename(&path, &backup_path).map_err(|error| {
                format!("Failed to move trajectory index aside {:?}: {error}", path)
            })?;
            if let Err(error) = std::fs::rename(&tmp_path, &path) {
                let _ = std::fs::rename(&backup_path, &path);
                return Err(format!(
                    "Failed to replace trajectory index {:?}: {error}",
                    path
                ));
            }
            let _ = std::fs::remove_file(backup_path);
            return Ok(());
        }
        std::fs::rename(&tmp_path, &path)
            .map_err(|error| format!("Failed to rename trajectory index {:?}: {error}", path))
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp_path);
    }
    result
}

pub(super) fn trajectory_index_tmp_prefix() -> String {
    format!(".{TRAJECTORY_INDEX_FILE}.tmp-")
}

pub async fn sweep_trajectory_index_tmp_files(dir: &Path, max_age: Duration) -> usize {
    let prefix = trajectory_index_tmp_prefix();
    let mut read_dir = match fs::read_dir(dir).await {
        Ok(read_dir) => read_dir,
        Err(_) => return 0,
    };
    let now = SystemTime::now();
    let mut removed = 0;
    while let Ok(Some(entry)) = read_dir.next_entry().await {
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        if !name.starts_with(&prefix) {
            continue;
        }
        let path = entry.path();
        let Ok(metadata) = fs::symlink_metadata(&path).await else {
            continue;
        };
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            continue;
        }
        let old_enough = metadata
            .modified()
            .ok()
            .and_then(|modified| now.duration_since(modified).ok())
            .is_some_and(|age| age >= max_age);
        if old_enough && fs::remove_file(&path).await.is_ok() {
            removed += 1;
        }
    }
    removed
}

pub async fn sweep_trajectory_index_tmp_files_in_roots(
    roots: &[PathBuf],
    max_age: Duration,
) -> usize {
    let mut removed = 0;
    for root in roots {
        removed += sweep_trajectory_index_tmp_files(root, max_age).await;
    }
    removed
}

pub(super) fn unix_modified_ms(metadata: &std::fs::Metadata) -> Result<i64, String> {
    let modified = metadata
        .modified()
        .map_err(|e| format!("Failed to read modified time: {e}"))?;
    let duration = modified
        .duration_since(UNIX_EPOCH)
        .map_err(|e| format!("Modified time predates Unix epoch: {e}"))?;
    i64::try_from(duration.as_millis()).map_err(|_| "Modified time is too large".to_string())
}

pub(super) fn file_metadata(path: &Path) -> Result<(u64, i64), String> {
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|e| format!("Failed to read trajectory metadata {:?}: {e}", path))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(format!("Trajectory path is not a regular file: {:?}", path));
    }
    Ok((metadata.len(), unix_modified_ms(&metadata)?))
}

pub fn source_from_hint_or_value(
    value: &serde_json::Value,
    source_hint: Option<TrajectorySourceIdentity>,
) -> TrajectorySourceIdentity {
    match TrajectorySourceIdentity::from_json(value) {
        Ok(TrajectorySourceIdentity::Normal) => {
            source_hint.unwrap_or(TrajectorySourceIdentity::Normal)
        }
        Ok(source) => source,
        Err(_) => source_hint.unwrap_or(TrajectorySourceIdentity::Normal),
    }
}

pub fn entry_from_trajectory_value(
    dir: &Path,
    path: &Path,
    value: &serde_json::Value,
    source_hint: Option<TrajectorySourceIdentity>,
) -> Result<TrajectoryIndexEntry, String> {
    let data = serde_json::from_value::<TrajectoryData>(value.clone())
        .map_err(|e| format!("Failed to parse trajectory {:?}: {e}", path))?;
    if data.id.is_empty() || data.created_at.is_empty() {
        return Err(format!(
            "Trajectory {:?} is missing required metadata",
            path
        ));
    }
    let bare_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| format!("Trajectory path has no UTF-8 file name: {:?}", path))?
        .to_string();
    if bare_name == TRAJECTORY_INDEX_FILE || !bare_name.ends_with(".json") {
        return Err(format!("Not a trajectory JSON file: {:?}", path));
    }
    let file_name = path
        .strip_prefix(dir)
        .ok()
        .and_then(|rel| rel.to_str())
        .map(|rel| rel.replace('\\', "/"))
        .filter(|rel| index_entry_file_name_is_valid(rel))
        .unwrap_or(bare_name);
    let stem = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .ok_or_else(|| format!("Trajectory path has no UTF-8 file stem: {:?}", path))?;
    if stem != data.id {
        return Err(format!(
            "Trajectory id mismatch for {:?}: expected {}, found {}",
            path, stem, data.id
        ));
    }
    let (file_len, file_modified_unix_ms) = file_metadata(path)?;
    let (total_lines_added, total_lines_removed) =
        calculate_line_changes_from_messages(&data.messages);
    let (tasks_total, tasks_done, tasks_failed) =
        calculate_task_progress_from_messages(&data.messages);
    let token_totals = calculate_token_totals_from_messages(&data.messages);
    let source = source_from_hint_or_value(value, source_hint);
    let parent_id = data
        .extra
        .get("parent_id")
        .and_then(|v| v.as_str())
        .map(ToString::to_string);
    let link_type = data
        .extra
        .get("link_type")
        .and_then(|v| v.as_str())
        .map(ToString::to_string);
    let root_chat_id = data
        .extra
        .get("root_chat_id")
        .and_then(|v| v.as_str())
        .map(ToString::to_string);
    let is_title_generated = data
        .extra
        .get("isTitleGenerated")
        .or_else(|| data.extra.get("is_title_generated"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let worktree = data
        .extra
        .get("worktree")
        .and_then(|v| serde_json::from_value::<WorktreeMeta>(v.clone()).ok());
    let waiting_for_card_ids = data
        .extra
        .get("waiting_for_card_ids")
        .and_then(|v| serde_json::from_value::<Vec<String>>(v.clone()).ok())
        .unwrap_or_default();
    let list_data = TrajectoryListData {
        id: data.id.clone(),
        updated_at: data.updated_at.clone(),
        mode: Some(data.mode.clone()),
        extra: data.extra.clone(),
    };

    Ok(TrajectoryIndexEntry {
        id: data.id,
        file_name,
        source: TrajectoryIndexSource::from(&source),
        created_at: data.created_at.clone(),
        updated_at: if data.updated_at.is_empty() {
            data.created_at.clone()
        } else {
            data.updated_at
        },
        title: trajectory_meta_title(&data.title),
        model: data.model,
        mode: data.mode,
        message_count: data.messages.len(),
        parent_id,
        link_type,
        root_chat_id,
        is_title_generated,
        worktree,
        total_lines_added,
        total_lines_removed,
        tasks_total,
        tasks_done,
        tasks_failed,
        total_prompt_tokens: token_totals.prompt_tokens,
        total_completion_tokens: token_totals.completion_tokens,
        total_tokens: token_totals.total_tokens,
        total_cache_read_tokens: token_totals.cache_read_tokens,
        total_cache_creation_tokens: token_totals.cache_creation_tokens,
        total_cost_usd: token_totals.cost_usd,
        displayable_chat: trajectory_list_data_is_displayable_chat(&list_data),
        waiting_for_card_ids,
        file_len,
        file_modified_unix_ms,
    })
}

pub async fn read_trajectory_index(dir: &Path) -> Result<Option<TrajectoryIndex>, String> {
    read_trajectory_index_with_counter_scope(dir, &GLOBAL_TRAJECTORY_INDEX_LISTING_COUNTER_SCOPE)
        .await
}

pub(super) async fn read_trajectory_index_with_counter_scope(
    dir: &Path,
    listing_counter_scope: &TrajectoryIndexListingCounterScope,
) -> Result<Option<TrajectoryIndex>, String> {
    if !perf_diagnostics::is_enabled() {
        return read_trajectory_index_inner(dir, listing_counter_scope).await;
    }
    let path = trajectory_index_path(dir);
    let span = perf_diagnostics::span(PerfComponent::TrajectoryIndexRead, None, Some(&path));
    let result = read_trajectory_index_inner(dir, listing_counter_scope).await;
    span.finish(
        if result.is_ok() {
            PerfOutcome::Success
        } else {
            PerfOutcome::Failure
        },
        None,
        result
            .as_ref()
            .ok()
            .and_then(|index| index.as_ref())
            .map(|index| index.entries.len() as u64),
        None,
        None,
    );
    result
}

pub(super) async fn read_trajectory_index_inner(
    dir: &Path,
    listing_counter_scope: &TrajectoryIndexListingCounterScope,
) -> Result<Option<TrajectoryIndex>, String> {
    listing_counter_scope.record_index_read();
    let path = trajectory_index_path(dir);
    let load_dir = dir.to_path_buf();
    let loaded =
        tokio::task::spawn_blocking(move || read_persisted_trajectory_index_sync(&load_dir))
            .await
            .map_err(|e| format!("Trajectory index parse task failed for {:?}: {e}", path))??;
    Ok(loaded.map(|loaded| {
        let mut index = loaded.index;
        index.baseline = Some(Arc::new(index.clone()));
        index
    }))
}

pub async fn write_trajectory_index_atomic(
    dir: &Path,
    index: &TrajectoryIndex,
) -> Result<(), String> {
    write_trajectory_index_atomic_owned(dir, index.clone()).await
}

pub(super) async fn write_trajectory_index_atomic_owned(
    dir: &Path,
    index: TrajectoryIndex,
) -> Result<(), String> {
    let result = if !perf_diagnostics::is_enabled() {
        write_trajectory_index_atomic_owned_inner(dir, index).await
    } else {
        let path = trajectory_index_path(dir);
        let entry_count = index.entries.len() as u64;
        let span = perf_diagnostics::span(PerfComponent::TrajectoryIndexWrite, None, Some(&path));
        let result = write_trajectory_index_atomic_owned_inner(dir, index).await;
        span.finish(
            if result.is_ok() {
                PerfOutcome::Success
            } else {
                PerfOutcome::Failure
            },
            None,
            Some(entry_count),
            None,
            None,
        );
        result
    };
    if result.is_ok() {
        invalidate_legacy_trajectory_index_cache(dir).await;
    }
    result
}

pub(super) async fn write_trajectory_index_atomic_owned_inner(
    dir: &Path,
    index: TrajectoryIndex,
) -> Result<(), String> {
    write_trajectory_index_atomic_owned_inner_with_rename_failure(dir, index, false).await
}

// Caller holds .index.json.lock. Derive only the caller's edits, never treat
// entries absent from an old snapshot as removals of another writer's commits.
// Rebase on the checkpoint and the journal prefix the caller observed; the
// replay below retains the rule that newer journal commits win conflicts.
// A snapshot without read provenance is additive, not authority to delete
// entries it never observed (e.g. a freshly rebuilt or deserialized snapshot).
pub(super) fn rebase_checkpoint_snapshot_sync(
    dir: &Path,
    mut supplied: TrajectoryIndex,
) -> Result<TrajectoryIndex, String> {
    let mut current = match read_trajectory_index_sync(dir) {
        Ok(Some(current)) => current,
        Ok(None) => return Ok(supplied),
        // A rebuild can repair a corrupt checkpoint, but a read-derived
        // snapshot must not silently replace one that became unreadable.
        Err(_) if supplied.baseline.is_none() => return Ok(supplied),
        Err(error) => return Err(error),
    };
    // The caller may already have observed journal entries that have not yet
    // been compacted. Seed those before applying its delta, otherwise unchanged
    // entries would disappear when the subsequent replay skips its watermark.
    let observed_seq = supplied
        .baseline
        .as_ref()
        .map(|base| base.applied_seq)
        .unwrap_or(supplied.applied_seq);
    let bytes = read_trajectory_index_journal_tail_sync(&trajectory_index_journal_path(dir), 0)?;
    let scan = scan_trajectory_index_journal(&bytes)?;
    let observed: Vec<_> = scan
        .records
        .into_iter()
        .filter(|record| record.seq <= observed_seq)
        .collect();
    let watermark = current.applied_seq;
    replay_journal_records(&mut current, &observed, watermark);
    let baseline = supplied.baseline.take();
    let baseline_entries = baseline
        .as_ref()
        .map(|base| base.entries.as_slice())
        .unwrap_or(&[]);
    let (upserts, removes) = trajectory_index_delta(baseline_entries, &supplied);
    let changed: HashSet<&str> = upserts
        .iter()
        .map(|entry| entry.id.as_str())
        .chain(removes.iter().map(String::as_str))
        .collect();
    current
        .entries
        .retain(|entry| !changed.contains(entry.id.as_str()));
    current.entries.extend(upserts);
    // Skipped files need the same three-way merge, keyed by relative filename.
    let empty = Vec::new();
    let old_skipped = baseline
        .as_ref()
        .map(|base| &base.skipped_files)
        .unwrap_or(&empty);
    let old: HashMap<&str, &TrajectoryIndexSkippedFile> = old_skipped
        .iter()
        .map(|entry| (entry.file_name.as_str(), entry))
        .collect();
    let new: HashMap<&str, &TrajectoryIndexSkippedFile> = supplied
        .skipped_files
        .iter()
        .map(|entry| (entry.file_name.as_str(), entry))
        .collect();
    current
        .skipped_files
        .retain(|entry| old.get(entry.file_name.as_str()) == new.get(entry.file_name.as_str()));
    current.skipped_files.extend(
        supplied
            .skipped_files
            .iter()
            .filter(|entry| old.get(entry.file_name.as_str()) != Some(entry))
            .cloned(),
    );
    current.updated_at = supplied.updated_at;
    current.applied_seq = current.applied_seq.max(observed_seq);
    Ok(current)
}

pub(super) async fn write_trajectory_index_atomic_owned_inner_with_rename_failure(
    dir: &Path,
    index: TrajectoryIndex,
    force_rename_failure: bool,
) -> Result<(), String> {
    let dir = dir.to_path_buf();
    // Keep the entire checkpoint transaction in one blocking task: no file-lock
    // guard crosses an await, and callers must obey TRAJECTORY_INDEX_LOCK_ORDER.
    tokio::task::spawn_blocking(move || {
        std::fs::create_dir_all(&dir)
            .map_err(|error| format!("Failed to create trajectory directory {:?}: {error}", dir))?;
        with_trajectory_index_file_lock(&dir, INDEX_LOCK_TIMEOUT, INDEX_LOCK_RETRY, None, || {
            let mut index = rebase_checkpoint_snapshot_sync(&dir, index)?;
            // A writer may have appended since this snapshot was read. Preserve
            // those commits before replacing the checkpoint and clearing the log.
            let bytes =
                read_trajectory_index_journal_tail_sync(&trajectory_index_journal_path(&dir), 0)?;
            let scan = scan_trajectory_index_journal(&bytes)?;
            let watermark = index.applied_seq;
            replay_journal_records(&mut index, &scan.records, watermark);
            write_trajectory_index_atomic_sync_with_rename_failure(
                &dir,
                &index,
                force_rename_failure,
            )?;
            truncate_trajectory_index_journal_sync(&dir)
        })
    })
    .await
    .map_err(|error| format!("Trajectory index checkpoint task failed: {error}"))?
}

pub fn index_entry_file_name_is_valid(file_name: &str) -> bool {
    if file_name.contains('\\')
        || file_name.contains(':')
        || file_name.starts_with('/')
        || !file_name.ends_with(".json")
    {
        return false;
    }
    let mut parts = file_name.split('/');
    let (Some(first), second, None) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    let (folder, name) = match second {
        Some(name) => (Some(first), name),
        None => (None, first),
    };
    if name.is_empty() || name == TRAJECTORY_INDEX_FILE || name.starts_with('.') {
        return false;
    }
    match folder {
        Some(folder) => {
            !folder.is_empty() && folder != "." && folder != ".." && !folder.contains('/')
        }
        None => true,
    }
}

pub async fn trajectory_index_entry_is_fresh(dir: &Path, entry: &TrajectoryIndexEntry) -> bool {
    if !index_entry_file_name_is_valid(&entry.file_name) {
        return false;
    }
    let path = trajectory_file_path_for_entry(dir, entry);
    let stem_matches = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .is_some_and(|stem| stem == entry.id);
    if !stem_matches {
        return false;
    }
    let metadata = match fs::symlink_metadata(&path).await {
        Ok(metadata) => metadata,
        Err(_) => return false,
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return false;
    }
    let modified_ms = match unix_modified_ms(&metadata) {
        Ok(ms) => ms,
        Err(_) => return false,
    };
    metadata.len() == entry.file_len && modified_ms == entry.file_modified_unix_ms
}
