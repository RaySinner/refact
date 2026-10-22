use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex, OnceLock};
use std::time::{Duration, Instant, UNIX_EPOCH};
use serde::Serialize;
use tokio::fs;
use uuid::Uuid;

use crate::chat::types::{TrajectoryCommitIntent, TrajectorySourceIdentity};

pub async fn atomic_write_file(tmp_path: &Path, dest_path: &Path) -> Result<(), String> {
    #[cfg(windows)]
    {
        if fs::try_exists(dest_path)
            .await
            .map_err(|e| format!("Failed to inspect destination file: {}", e))?
        {
            let backup_extension = format!(
                "{}.replace.{}",
                dest_path
                    .extension()
                    .and_then(|ext| ext.to_str())
                    .unwrap_or("tmp"),
                Uuid::new_v4().simple()
            );
            let backup_path = dest_path.with_extension(backup_extension);
            fs::rename(dest_path, &backup_path)
                .await
                .map_err(|e| format!("Failed to move existing file aside: {}", e))?;
            match fs::rename(tmp_path, dest_path).await {
                Ok(()) => {
                    let _ = fs::remove_file(&backup_path).await;
                    return Ok(());
                }
                Err(e) => {
                    let _ = fs::rename(&backup_path, dest_path).await;
                    return Err(format!("Failed to rename: {}", e));
                }
            }
        }
    }
    fs::rename(tmp_path, dest_path)
        .await
        .map_err(|e| format!("Failed to rename: {}", e))
}

pub(super) fn unique_trajectory_tmp_path(file_path: &Path) -> PathBuf {
    let random = Uuid::new_v4().simple().to_string();
    file_path.with_extension(format!("json.tmp.{}", &random[..8]))
}

async fn atomic_write_json_with_tmp_path(
    path: &Path,
    tmp_path: &Path,
    json_result: Result<String, String>,
    write_error_prefix: Option<&str>,
    self_write_source: Option<TrajectorySourceIdentity>,
) -> Result<(), String> {
    let result = async {
        let json = json_result?;
        let self_write = match self_write_source {
            Some(source) if trajectory_watcher_self_write_rollout_enabled() => {
                register_trajectory_self_write(path, source, &json).await
            }
            None => None,
            Some(_) => None,
        };
        if let Err(error) = fs::write(tmp_path, &json).await {
            discard_trajectory_self_write(self_write);
            return Err(write_error_prefix
                .map(|prefix| format!("{}: {}", prefix, error))
                .unwrap_or_else(|| error.to_string()));
        }
        if let Err(error) = atomic_write_file(tmp_path, path).await {
            discard_trajectory_self_write(self_write);
            return Err(error);
        }
        complete_trajectory_self_write(self_write).await;
        Ok(())
    }
    .await;
    if result.is_err() {
        let _ = fs::remove_file(tmp_path).await;
    }
    result
}

pub const TRAJECTORY_WRITER_ENV: &str = "REFACT_TRAJECTORY_WRITER";
pub const TRAJECTORY_WATCHER_SELF_WRITE_ENV: &str = "REFACT_TRAJECTORY_WATCHER_SELF_WRITE";

static TRAJECTORY_WRITER_ROLLOUT: OnceLock<bool> = OnceLock::new();

pub(super) fn rollout_switch_enabled<F>(cache: &OnceLock<bool>, read: F) -> bool
where
    F: FnOnce() -> bool,
{
    *cache.get_or_init(read)
}

pub fn trajectory_writer_rollout_enabled() -> bool {
    #[cfg(any(test, feature = "bench"))]
    if let Some(enabled) = *trajectory_writer_rollout_override()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
    {
        return enabled;
    }
    rollout_switch_enabled(&TRAJECTORY_WRITER_ROLLOUT, || {
        trajectory_writer_rollout_enabled_for(
            std::env::var(TRAJECTORY_WRITER_ENV)
                .ok()
                .as_deref()
                .or_else(|| {
                    crate::runtime_settings::current()
                        .trajectory_writer_enabled
                        .then_some("1")
                }),
        )
    })
}

pub(crate) fn trajectory_writer_rollout_enabled_for(value: Option<&str>) -> bool {
    value.is_some_and(|value| {
        let value = value.trim();
        value == "1"
            || value.eq_ignore_ascii_case("true")
            || value.eq_ignore_ascii_case("yes")
            || value.eq_ignore_ascii_case("on")
    })
}

#[cfg(any(test, feature = "bench"))]
fn trajectory_writer_rollout_override() -> &'static StdMutex<Option<bool>> {
    static OVERRIDE: OnceLock<StdMutex<Option<bool>>> = OnceLock::new();
    OVERRIDE.get_or_init(|| StdMutex::new(None))
}

#[cfg(any(test, feature = "bench"))]
pub(crate) struct TrajectoryWriterRolloutGuard(Option<bool>);

#[cfg(any(test, feature = "bench"))]
impl TrajectoryWriterRolloutGuard {
    pub(crate) fn set(enabled: bool) -> Self {
        let mut override_value = trajectory_writer_rollout_override()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let previous = *override_value;
        *override_value = Some(enabled);
        Self(previous)
    }
}

#[cfg(any(test, feature = "bench"))]
impl Drop for TrajectoryWriterRolloutGuard {
    fn drop(&mut self) {
        *trajectory_writer_rollout_override()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = self.0.take();
    }
}

pub fn trajectory_watcher_self_write_rollout_enabled() -> bool {
    trajectory_watcher_self_write_rollout_enabled_for(
        std::env::var(TRAJECTORY_WATCHER_SELF_WRITE_ENV)
            .ok()
            .as_deref()
            .or_else(|| {
                crate::runtime_settings::current()
                    .trajectory_watcher_self_write_enabled
                    .then_some("1")
            }),
    )
}

pub(crate) fn trajectory_watcher_self_write_rollout_enabled_for(value: Option<&str>) -> bool {
    value.is_some_and(|value| {
        let value = value.trim();
        value == "1"
            || value.eq_ignore_ascii_case("true")
            || value.eq_ignore_ascii_case("yes")
            || value.eq_ignore_ascii_case("on")
    })
}

const TRAJECTORY_SELF_WRITE_TTL: Duration = Duration::from_secs(10);
const TRAJECTORY_SELF_WRITE_MAX_RECORDS: usize = 512;

#[derive(Clone, Debug, PartialEq, Eq)]
struct TrajectoryWriteFingerprint {
    file_len: u64,
    file_modified_unix_ms: i64,
    content_hash: [u8; 16],
}

#[derive(Clone)]
struct TrajectorySelfWriteHandle {
    key: (PathBuf, TrajectorySourceIdentity),
    nonce: Uuid,
}

struct TrajectorySelfWriteRecord {
    nonce: Uuid,
    created_at: Instant,
    expected_content_hash: [u8; 16],
    fingerprint: Option<TrajectoryWriteFingerprint>,
}

type TrajectorySelfWriteRegistry = std::collections::HashMap<
    (PathBuf, TrajectorySourceIdentity),
    std::collections::VecDeque<TrajectorySelfWriteRecord>,
>;

fn trajectory_self_writes() -> &'static StdMutex<TrajectorySelfWriteRegistry> {
    static WRITES: OnceLock<StdMutex<TrajectorySelfWriteRegistry>> = OnceLock::new();
    WRITES.get_or_init(|| StdMutex::new(std::collections::HashMap::new()))
}

fn watcher_trajectory_source(source: &TrajectorySourceIdentity) -> TrajectorySourceIdentity {
    match source {
        TrajectorySourceIdentity::Task {
            task_id,
            role,
            agent_id,
            ..
        } => TrajectorySourceIdentity::task(
            task_id.clone(),
            role.clone(),
            agent_id.clone(),
            None,
            None,
        ),
        source => source.clone(),
    }
}

fn prune_trajectory_self_writes(registry: &mut TrajectorySelfWriteRegistry, now: Instant) {
    registry.retain(|_, records| {
        records.retain(|record| now.duration_since(record.created_at) < TRAJECTORY_SELF_WRITE_TTL);
        !records.is_empty()
    });
    while registry
        .values()
        .map(|records| records.len())
        .sum::<usize>()
        > TRAJECTORY_SELF_WRITE_MAX_RECORDS
    {
        let oldest = registry
            .iter()
            .filter_map(|(key, records)| {
                records
                    .front()
                    .map(|record| (key.clone(), record.created_at))
            })
            .min_by_key(|(_, created_at)| *created_at);
        let Some((key, _)) = oldest else {
            break;
        };
        if let Some(records) = registry.get_mut(&key) {
            records.pop_front();
            if records.is_empty() {
                registry.remove(&key);
            }
        }
    }
}

async fn canonical_trajectory_write_path(path: &Path) -> Option<PathBuf> {
    let parent = path.parent()?;
    let file_name = path.file_name()?;
    let parent = fs::canonicalize(parent).await.ok()?;
    Some(dunce::simplified(&parent).join(file_name))
}

async fn trajectory_write_fingerprint(path: &Path) -> Option<TrajectoryWriteFingerprint> {
    let before = fs::symlink_metadata(path).await.ok()?;
    if before.file_type().is_symlink() || !before.is_file() {
        return None;
    }
    let before_modified = before
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_millis();
    let before_modified = i64::try_from(before_modified).ok()?;
    let content = fs::read(path).await.ok()?;
    let after = fs::symlink_metadata(path).await.ok()?;
    if after.file_type().is_symlink() || !after.is_file() || after.len() != before.len() {
        return None;
    }
    let after_modified = after
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_millis();
    let after_modified = i64::try_from(after_modified).ok()?;
    if after_modified != before_modified || content.len() as u64 != after.len() {
        return None;
    }
    Some(TrajectoryWriteFingerprint {
        file_len: after.len(),
        file_modified_unix_ms: after_modified,
        content_hash: md5::compute(content).0,
    })
}

async fn trajectory_fingerprint_matches(
    path: &Path,
    expected: &TrajectoryWriteFingerprint,
) -> bool {
    let before = match fs::symlink_metadata(path).await {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => metadata,
        _ => return false,
    };
    let before_modified = match before
        .modified()
        .ok()
        .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
        .and_then(|duration| i64::try_from(duration.as_millis()).ok())
    {
        Some(modified) => modified,
        None => return false,
    };
    if before.len() != expected.file_len || before_modified != expected.file_modified_unix_ms {
        return false;
    }
    let content = match fs::read(path).await {
        Ok(content) if md5::compute(&content).0 == expected.content_hash => content,
        _ => return false,
    };
    let after = match fs::symlink_metadata(path).await {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => metadata,
        _ => return false,
    };
    let after_modified = match after
        .modified()
        .ok()
        .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
        .and_then(|duration| i64::try_from(duration.as_millis()).ok())
    {
        Some(modified) => modified,
        None => return false,
    };
    after.len() == before.len()
        && after_modified == before_modified
        && content.len() as u64 == after.len()
}

async fn register_trajectory_self_write(
    path: &Path,
    source: TrajectorySourceIdentity,
    content: &str,
) -> Option<TrajectorySelfWriteHandle> {
    let key = (
        canonical_trajectory_write_path(path).await?,
        watcher_trajectory_source(&source),
    );
    let nonce = Uuid::new_v4();
    let mut registry = trajectory_self_writes()
        .lock()
        .expect("trajectory self-write registry poisoned");
    prune_trajectory_self_writes(&mut registry, Instant::now());
    registry
        .entry(key.clone())
        .or_default()
        .push_back(TrajectorySelfWriteRecord {
            nonce,
            created_at: Instant::now(),
            expected_content_hash: md5::compute(content).0,
            fingerprint: None,
        });
    prune_trajectory_self_writes(&mut registry, Instant::now());
    Some(TrajectorySelfWriteHandle { key, nonce })
}

fn discard_trajectory_self_write(handle: Option<TrajectorySelfWriteHandle>) {
    let Some(handle) = handle else {
        return;
    };
    let mut registry = trajectory_self_writes()
        .lock()
        .expect("trajectory self-write registry poisoned");
    if let Some(records) = registry.get_mut(&handle.key) {
        records.retain(|record| record.nonce != handle.nonce);
        if records.is_empty() {
            registry.remove(&handle.key);
        }
    }
}

async fn complete_trajectory_self_write(handle: Option<TrajectorySelfWriteHandle>) {
    let Some(handle) = handle else {
        return;
    };
    let fingerprint = trajectory_write_fingerprint(&handle.key.0).await;
    let mut registry = trajectory_self_writes()
        .lock()
        .expect("trajectory self-write registry poisoned");
    prune_trajectory_self_writes(&mut registry, Instant::now());
    let Some(records) = registry.get_mut(&handle.key) else {
        return;
    };
    let Some(record) = records
        .iter_mut()
        .find(|record| record.nonce == handle.nonce)
    else {
        return;
    };
    if fingerprint
        .as_ref()
        .is_some_and(|fingerprint| fingerprint.content_hash == record.expected_content_hash)
    {
        record.fingerprint = fingerprint;
    } else {
        records.retain(|record| record.nonce != handle.nonce);
    }
    if records.is_empty() {
        registry.remove(&handle.key);
    }
}

pub(super) async fn consume_trajectory_self_write(
    path: &Path,
    source: &TrajectorySourceIdentity,
) -> bool {
    let Some(path) = canonical_trajectory_write_path(path).await else {
        return false;
    };
    let key = (path, watcher_trajectory_source(source));
    let fingerprints = {
        let mut registry = trajectory_self_writes()
            .lock()
            .expect("trajectory self-write registry poisoned");
        prune_trajectory_self_writes(&mut registry, Instant::now());
        registry
            .get(&key)
            .map(|records| {
                records
                    .iter()
                    .filter_map(|record| record.fingerprint.clone())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    };
    let matched = futures::future::join_all(
        fingerprints
            .iter()
            .map(|fingerprint| trajectory_fingerprint_matches(&key.0, fingerprint)),
    )
    .await
    .into_iter()
    .position(|matches| matches);
    let Some(matched) = matched else {
        return false;
    };
    let mut registry = trajectory_self_writes()
        .lock()
        .expect("trajectory self-write registry poisoned");
    prune_trajectory_self_writes(&mut registry, Instant::now());
    let Some(records) = registry.get_mut(&key) else {
        return false;
    };
    if let Some(position) = records
        .iter()
        .position(|record| record.fingerprint.as_ref() == fingerprints.get(matched))
    {
        records.remove(position);
        if records.is_empty() {
            registry.remove(&key);
        }
        true
    } else {
        false
    }
}

fn clear_trajectory_self_writes(watched_roots: &[PathBuf]) {
    let mut registry = trajectory_self_writes()
        .lock()
        .expect("trajectory self-write registry poisoned");
    prune_trajectory_self_writes(&mut registry, Instant::now());
    registry.retain(|(path, _), _| !watched_roots.iter().any(|root| path.starts_with(root)));
}

pub(super) struct TrajectorySelfWriteScope {
    pub(super) watched_roots: Vec<PathBuf>,
}

impl Drop for TrajectorySelfWriteScope {
    fn drop(&mut self) {
        clear_trajectory_self_writes(&self.watched_roots);
    }
}

#[cfg(test)]
pub(crate) fn clear_all_trajectory_self_writes() {
    trajectory_self_writes()
        .lock()
        .expect("trajectory self-write registry poisoned")
        .clear();
}

pub(super) async fn atomic_write_json(
    path: &PathBuf,
    data: &impl Serialize,
    source: TrajectorySourceIdentity,
) -> Result<(), String> {
    let tmp_path = unique_trajectory_tmp_path(path);
    let json_result = serde_json::to_string(data).map_err(|e| e.to_string());
    atomic_write_json_with_tmp_path(path, &tmp_path, json_result, None, Some(source)).await
}
