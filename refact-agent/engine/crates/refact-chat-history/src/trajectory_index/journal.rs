use super::*;

pub(super) fn default_journal_schema_version() -> u32 {
    TRAJECTORY_INDEX_JOURNAL_SCHEMA_VERSION
}

pub(super) static TRAJECTORY_INDEX_WRITER_ID: std::sync::OnceLock<String> =
    std::sync::OnceLock::new();

pub(super) fn trajectory_index_writer_id() -> &'static str {
    TRAJECTORY_INDEX_WRITER_ID.get_or_init(|| Uuid::new_v4().to_string())
}

pub(super) fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for byte in bytes {
        crc ^= *byte as u32;
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(super) struct TrajectoryIndexJournalPayload {
    pub(super) seq: u64,
    pub(super) writer_id: String,
    pub(super) ts_ms: i64,
    #[serde(default)]
    pub(super) updated_at: String,
    #[serde(default)]
    pub(super) upserts: Vec<TrajectoryIndexEntry>,
    #[serde(default)]
    pub(super) removes: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) skipped: Option<Vec<TrajectoryIndexSkippedFile>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(super) struct TrajectoryIndexJournalRecord {
    #[serde(default = "default_journal_schema_version")]
    schema: u32,
    #[serde(flatten)]
    payload: TrajectoryIndexJournalPayload,
    checksum: u32,
}

pub(super) fn journal_payload_checksum(
    payload: &TrajectoryIndexJournalPayload,
) -> Result<u32, String> {
    let bytes = serde_json::to_vec(payload)
        .map_err(|error| format!("Failed to serialize trajectory index journal batch: {error}"))?;
    Ok(crc32(&bytes))
}

pub(super) fn encode_journal_record(
    payload: TrajectoryIndexJournalPayload,
) -> Result<Vec<u8>, String> {
    let checksum = journal_payload_checksum(&payload)?;
    let record = TrajectoryIndexJournalRecord {
        schema: TRAJECTORY_INDEX_JOURNAL_SCHEMA_VERSION,
        payload,
        checksum,
    };
    let mut line = serde_json::to_vec(&record)
        .map_err(|error| format!("Failed to serialize trajectory index journal record: {error}"))?;
    line.push(b'\n');
    Ok(line)
}

pub(super) struct JournalScan {
    pub(super) records: Vec<TrajectoryIndexJournalPayload>,
    pub(super) consumed: u64,
}

pub(super) fn scan_trajectory_index_journal(bytes: &[u8]) -> Result<JournalScan, String> {
    let mut records = Vec::new();
    let mut consumed = 0u64;
    for chunk in bytes.split_inclusive(|byte| *byte == b'\n') {
        if !chunk.ends_with(b"\n") {
            break;
        }
        consumed += chunk.len() as u64;
        let line = chunk.strip_suffix(b"\n").unwrap_or(chunk);
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if line.iter().all(|byte| byte.is_ascii_whitespace()) {
            continue;
        }
        let record: TrajectoryIndexJournalRecord = serde_json::from_slice(line)
            .map_err(|error| format!("Corrupt trajectory index journal line: {error}"))?;
        if record.schema != TRAJECTORY_INDEX_JOURNAL_SCHEMA_VERSION {
            return Err(format!(
                "Unsupported trajectory index journal schema version {}",
                record.schema
            ));
        }
        if journal_payload_checksum(&record.payload)? != record.checksum {
            return Err("Trajectory index journal checksum mismatch".to_string());
        }
        records.push(record.payload);
    }
    Ok(JournalScan { records, consumed })
}

pub(super) fn apply_journal_payload_to_index(
    index: &mut TrajectoryIndex,
    payload: &TrajectoryIndexJournalPayload,
) {
    if !payload.removes.is_empty() {
        let removed: HashSet<&str> = payload.removes.iter().map(String::as_str).collect();
        index
            .entries
            .retain(|entry| !removed.contains(entry.id.as_str()));
    }
    for entry in &payload.upserts {
        index.entries.retain(|existing| existing.id != entry.id);
        index
            .skipped_files
            .retain(|skipped| skipped.file_name != entry.file_name);
        index.entries.push(entry.clone());
    }
    if let Some(skipped) = &payload.skipped {
        index.skipped_files = skipped.clone();
    }
    if !payload.updated_at.is_empty() {
        index.updated_at = payload.updated_at.clone();
    }
    index.applied_seq = payload.seq;
}

#[derive(Clone, Debug, Default)]
pub(super) struct TrajectoryIndexJournalCursor {
    pub(super) checkpoint: Option<FileGeneration>,
    pub(super) journal_offset: u64,
    pub(super) last_seq: u64,
}

pub(super) struct PersistedTrajectoryIndex {
    pub(super) index: TrajectoryIndex,
    pub(super) cursor: TrajectoryIndexJournalCursor,
    pub(super) checkpoint_present: bool,
}

pub(super) fn read_trajectory_index_journal_tail_sync(
    path: &Path,
    offset: u64,
) -> Result<Vec<u8>, String> {
    use std::io::{Read, Seek};

    let mut file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(format!(
                "Failed to open trajectory index journal {:?}: {error}",
                path
            ))
        }
    };
    if offset > 0 {
        file.seek(SeekFrom::Start(offset)).map_err(|error| {
            format!(
                "Failed to seek trajectory index journal {:?}: {error}",
                path
            )
        })?;
    }
    let mut tail = Vec::new();
    file.read_to_end(&mut tail).map_err(|error| {
        format!(
            "Failed to read trajectory index journal {:?}: {error}",
            path
        )
    })?;
    Ok(tail)
}

pub(super) fn read_persisted_trajectory_index_sync(
    dir: &Path,
) -> Result<Option<PersistedTrajectoryIndex>, String> {
    if !dir.exists() {
        return Ok(None);
    }
    with_trajectory_index_file_lock(dir, INDEX_LOCK_TIMEOUT, INDEX_LOCK_RETRY, None, || {
        read_persisted_trajectory_index_locked_sync(dir)
    })
}

// Caller must hold .index.json.lock for the checkpoint + journal snapshot.
pub(super) fn read_persisted_trajectory_index_locked_sync(
    dir: &Path,
) -> Result<Option<PersistedTrajectoryIndex>, String> {
    let journal_path = trajectory_index_journal_path(dir);
    let checkpoint_stamp = regular_file_generation(&trajectory_index_path(dir))?;
    let checkpoint = read_trajectory_index_sync(dir)?;
    let journal_bytes = read_trajectory_index_journal_tail_sync(&journal_path, 0)?;
    if checkpoint.is_none() && journal_bytes.is_empty() {
        return Ok(None);
    }
    let mut index = checkpoint.clone().unwrap_or_else(|| TrajectoryIndex {
        schema_version: TRAJECTORY_INDEX_SCHEMA_VERSION,
        updated_at: Utc::now().to_rfc3339(),
        entries: Vec::new(),
        skipped_files: Vec::new(),
        applied_seq: 0,
        baseline: None,
    });
    let scan = scan_trajectory_index_journal(&journal_bytes)?;
    let watermark = index.applied_seq;
    let last_seq = replay_journal_records(&mut index, &scan.records, watermark);
    Ok(Some(PersistedTrajectoryIndex {
        index,
        cursor: TrajectoryIndexJournalCursor {
            checkpoint: checkpoint_stamp,
            journal_offset: scan.consumed,
            last_seq,
        },
        checkpoint_present: checkpoint.is_some(),
    }))
}

// Writers allocate sequences while holding .index.json.lock, so a valid journal
// is strictly increasing across writers. Skip against the running high-water
// mark: duplicate or regressing records must never roll back newer state. Use
// exactly the same rule for full replay, incremental replay, and checkpointing.
pub(super) fn replay_journal_records(
    index: &mut TrajectoryIndex,
    records: &[TrajectoryIndexJournalPayload],
    watermark: u64,
) -> u64 {
    let mut last_seq = watermark;
    for payload in records {
        if payload.seq <= last_seq {
            continue;
        }
        apply_journal_payload_to_index(index, payload);
        last_seq = payload.seq;
    }
    index.applied_seq = last_seq;
    last_seq
}

pub(super) fn repair_torn_journal_tail_sync(path: &Path, journal_len: u64) -> Result<u64, String> {
    if journal_len == 0 {
        return Ok(0);
    }
    let last_byte = read_trajectory_index_journal_tail_sync(path, journal_len - 1)?;
    if last_byte.last() == Some(&b'\n') {
        return Ok(journal_len);
    }
    let bytes = read_trajectory_index_journal_tail_sync(path, 0)?;
    let keep = bytes
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map(|position| position as u64 + 1)
        .unwrap_or(0);
    let file = std::fs::OpenOptions::new()
        .write(true)
        .open(path)
        .map_err(|error| {
            format!(
                "Failed to open trajectory index journal for repair {:?}: {error}",
                path
            )
        })?;
    file.set_len(keep).map_err(|error| {
        format!(
            "Failed to truncate torn trajectory index journal {:?}: {error}",
            path
        )
    })?;
    Ok(keep)
}

pub(super) fn append_trajectory_index_journal_sync(
    dir: &Path,
    journal_len: u64,
    payload: TrajectoryIndexJournalPayload,
) -> Result<u64, String> {
    use std::io::Write;

    let path = trajectory_index_journal_path(dir);
    let journal_len = repair_torn_journal_tail_sync(&path, journal_len)?;
    let line = encode_journal_record(payload)?;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|error| {
            format!(
                "Failed to open trajectory index journal {:?}: {error}",
                path
            )
        })?;
    file.write_all(&line).map_err(|error| {
        format!(
            "Failed to append to trajectory index journal {:?}: {error}",
            path
        )
    })?;
    file.flush().map_err(|error| {
        format!(
            "Failed to flush trajectory index journal {:?}: {error}",
            path
        )
    })?;
    Ok(journal_len + line.len() as u64)
}

pub(super) fn truncate_trajectory_index_journal_sync(dir: &Path) -> Result<(), String> {
    let path = trajectory_index_journal_path(dir);
    match std::fs::OpenOptions::new().write(true).open(&path) {
        Ok(file) => file.set_len(0).map_err(|error| {
            format!(
                "Failed to truncate trajectory index journal {:?}: {error}",
                path
            )
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!(
            "Failed to open trajectory index journal {:?}: {error}",
            path
        )),
    }
}
