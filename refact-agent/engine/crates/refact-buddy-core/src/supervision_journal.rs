use std::collections::BTreeSet;
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::supervision::{
    InterventionDelivery, InterventionRecord, InterventionVerdict, SupervisionHash,
    SuppressionReason,
};

pub const SUPERVISION_JOURNAL_VERSION: u16 = 1;
pub const DEFAULT_JOURNAL_MAX_RECORDS: usize = 128;
pub const DEFAULT_JOURNAL_MAX_BYTES: usize = 256 * 1024;

static JOURNAL_IO_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "u16", into = "u16")]
pub struct SupervisionJournalVersion;

impl TryFrom<u16> for SupervisionJournalVersion {
    type Error = &'static str;

    fn try_from(value: u16) -> Result<Self, Self::Error> {
        if value == SUPERVISION_JOURNAL_VERSION {
            Ok(Self)
        } else {
            Err("unsupported supervision journal version")
        }
    }
}

impl From<SupervisionJournalVersion> for u16 {
    fn from(_: SupervisionJournalVersion) -> Self {
        SUPERVISION_JOURNAL_VERSION
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SupervisionJournalLimits {
    pub max_records: usize,
    pub max_bytes: usize,
}

impl Default for SupervisionJournalLimits {
    fn default() -> Self {
        Self {
            max_records: DEFAULT_JOURNAL_MAX_RECORDS,
            max_bytes: DEFAULT_JOURNAL_MAX_BYTES,
        }
    }
}

impl SupervisionJournalLimits {
    fn validate(self) -> Result<Self, String> {
        if self.max_records == 0 || self.max_bytes == 0 {
            return Err("supervision journal limits must be positive".to_string());
        }
        Ok(self)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SupervisionJournalAppend {
    Appended(SupervisionHash),
    Duplicate(SupervisionHash),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SupervisionReplayOutcome {
    Sent,
    Silent(SuppressionReason),
    FalsePositive {
        delivered: bool,
        suppression_reason: Option<SuppressionReason>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SupervisionReplayEntry {
    pub record: InterventionRecord,
    pub outcome: SupervisionReplayOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SupervisionJournalSnapshot {
    records: Vec<InterventionRecord>,
    current_candidates: BTreeSet<SupervisionHash>,
}

impl SupervisionJournalSnapshot {
    pub fn records(&self) -> &[InterventionRecord] {
        &self.records
    }

    pub fn current_candidates(&self) -> &BTreeSet<SupervisionHash> {
        &self.current_candidates
    }

    pub fn replay(&self) -> Vec<SupervisionReplayEntry> {
        self.records
            .iter()
            .cloned()
            .map(|record| {
                let delivered = matches!(&record.delivery, InterventionDelivery::Sent { .. });
                let suppression_reason = match &record.delivery {
                    InterventionDelivery::Sent { .. } => None,
                    InterventionDelivery::Suppressed { reason } => Some(*reason),
                };
                let outcome = if record.verdict == InterventionVerdict::Dismissed {
                    SupervisionReplayOutcome::FalsePositive {
                        delivered,
                        suppression_reason,
                    }
                } else if delivered {
                    SupervisionReplayOutcome::Sent
                } else {
                    SupervisionReplayOutcome::Silent(
                        suppression_reason.expect("suppressed records have a reason"),
                    )
                };
                SupervisionReplayEntry { record, outcome }
            })
            .collect()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SupervisionJournalFile {
    journal_version: SupervisionJournalVersion,
    records: Vec<InterventionRecord>,
    current_candidates: BTreeSet<SupervisionHash>,
}

impl Default for SupervisionJournalFile {
    fn default() -> Self {
        Self {
            journal_version: SupervisionJournalVersion,
            records: Vec::new(),
            current_candidates: BTreeSet::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct SupervisionJournal {
    project_root: PathBuf,
    limits: SupervisionJournalLimits,
}

impl SupervisionJournal {
    pub fn new(project_root: impl Into<PathBuf>) -> Self {
        Self {
            project_root: project_root.into(),
            limits: SupervisionJournalLimits::default(),
        }
    }

    pub fn with_limits(
        project_root: impl Into<PathBuf>,
        limits: SupervisionJournalLimits,
    ) -> Result<Self, String> {
        Ok(Self {
            project_root: project_root.into(),
            limits: limits.validate()?,
        })
    }

    pub async fn load(&self) -> Result<SupervisionJournalSnapshot, String> {
        journal_transaction(&self.project_root, move |root| {
            let file = read_journal_file(root)?;
            validate_journal_file(&file)?;
            Ok(snapshot_from_file(file))
        })
        .await
    }

    pub async fn append(
        &self,
        record: InterventionRecord,
    ) -> Result<SupervisionJournalAppend, String> {
        let id = record
            .journal_id()
            .map_err(|err| format!("Failed to identify supervision record: {err}"))?;
        let limits = self.limits;
        journal_transaction(&self.project_root, move |root| {
            let mut file = read_journal_file(root)?;
            validate_journal_file(&file)?;
            if file.records.iter().any(|existing| {
                existing
                    .journal_id()
                    .is_ok_and(|existing_id| existing_id == id)
            }) {
                return Ok(SupervisionJournalAppend::Duplicate(id));
            }
            file.records.push(record);
            compact_journal_file(&mut file, limits, Some(&id))?;
            write_journal_file(root, &file)?;
            Ok(SupervisionJournalAppend::Appended(id))
        })
        .await
    }

    pub async fn set_current_candidate(
        &self,
        id: &SupervisionHash,
        current: bool,
    ) -> Result<(), String> {
        let id = id.clone();
        let limits = self.limits;
        journal_transaction(&self.project_root, move |root| {
            let mut file = read_journal_file(root)?;
            validate_journal_file(&file)?;
            if !file
                .records
                .iter()
                .any(|record| record.journal_id().is_ok_and(|record_id| record_id == id))
            {
                return Err("cannot pin an unknown supervision candidate".to_string());
            }
            if current {
                file.current_candidates.insert(id);
            } else {
                file.current_candidates.remove(&id);
            }
            compact_journal_file(&mut file, limits, None)?;
            write_journal_file(root, &file)
        })
        .await
    }

    pub async fn compact(&self) -> Result<(), String> {
        let limits = self.limits;
        journal_transaction(&self.project_root, move |root| {
            let mut file = read_journal_file(root)?;
            validate_journal_file(&file)?;
            compact_journal_file(&mut file, limits, None)?;
            write_journal_file(root, &file)
        })
        .await
    }
}

pub fn supervision_journal_path(project_root: &Path) -> PathBuf {
    project_root.join(".refact/buddy/supervision_journal.v1.json")
}

fn supervision_journal_lock_path(project_root: &Path) -> PathBuf {
    project_root.join(".refact/buddy/supervision_journal.v1.lock")
}

async fn journal_transaction<T, F>(project_root: &Path, body: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce(&Path) -> Result<T, String> + Send + 'static,
{
    let root = project_root.to_path_buf();
    let _order = JOURNAL_IO_LOCK.lock().await;
    tokio::task::spawn_blocking(move || {
        let lock_path = supervision_journal_lock_path(&root);
        let mut lock = refact_core::file_lock::open_lock(&lock_path).map_err(|err| {
            format!("Failed to open supervision journal lock {lock_path:?}: {err}")
        })?;
        let guard = refact_core::file_lock::lock_blocking(&mut lock)
            .map_err(|err| format!("Failed to lock supervision journal {lock_path:?}: {err}"))?;
        let result = body(&root);
        drop(guard);
        result
    })
    .await
    .map_err(|err| format!("supervision journal io task failed: {err}"))?
}

fn read_journal_file(project_root: &Path) -> Result<SupervisionJournalFile, String> {
    let path = supervision_journal_path(project_root);
    match std::fs::read_to_string(&path) {
        Ok(content) => serde_json::from_str(&content)
            .map_err(|err| format!("Failed to parse supervision journal {path:?}: {err}")),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(SupervisionJournalFile::default()),
        Err(err) => Err(format!(
            "Failed to read supervision journal {path:?}: {err}"
        )),
    }
}

fn validate_journal_file(file: &SupervisionJournalFile) -> Result<(), String> {
    let mut ids = BTreeSet::new();
    for record in &file.records {
        let id = record
            .journal_id()
            .map_err(|err| format!("Failed to identify supervision record: {err}"))?;
        if !ids.insert(id) {
            return Err("supervision journal contains a duplicate record".to_string());
        }
    }
    if !file.current_candidates.is_subset(&ids) {
        return Err("supervision journal pins a missing candidate".to_string());
    }
    Ok(())
}

fn snapshot_from_file(file: SupervisionJournalFile) -> SupervisionJournalSnapshot {
    SupervisionJournalSnapshot {
        records: file.records,
        current_candidates: file.current_candidates,
    }
}

fn encoded_journal_len(file: &SupervisionJournalFile) -> Result<usize, String> {
    serde_json::to_vec(file)
        .map(|bytes| bytes.len())
        .map_err(|err| format!("Failed to serialize supervision journal: {err}"))
}

fn compact_journal_file(
    file: &mut SupervisionJournalFile,
    limits: SupervisionJournalLimits,
    additionally_protected: Option<&SupervisionHash>,
) -> Result<(), String> {
    limits.validate()?;
    while file.records.len() > limits.max_records || encoded_journal_len(file)? > limits.max_bytes {
        let oldest_removable = file.records.iter().position(|record| {
            let Ok(id) = record.journal_id() else {
                return false;
            };
            !file.current_candidates.contains(&id) && additionally_protected != Some(&id)
        });
        let Some(index) = oldest_removable else {
            return Err("supervision journal bounds would remove a current candidate".to_string());
        };
        file.records.remove(index);
    }
    Ok(())
}

fn write_journal_file(project_root: &Path, file: &SupervisionJournalFile) -> Result<(), String> {
    let path = supervision_journal_path(project_root);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|err| {
            format!("Failed to create supervision journal directory {parent:?}: {err}")
        })?;
    }
    let bytes = serde_json::to_vec(file)
        .map_err(|err| format!("Failed to serialize supervision journal: {err}"))?;
    let tmp = path.with_extension(format!("json.{}.tmp", uuid::Uuid::new_v4()));
    let write_result = (|| -> Result<(), String> {
        let mut output = std::fs::File::create(&tmp).map_err(|err| {
            format!("Failed to create supervision journal temporary file {tmp:?}: {err}")
        })?;
        output.write_all(&bytes).map_err(|err| {
            format!("Failed to write supervision journal temporary file {tmp:?}: {err}")
        })?;
        output.sync_all().map_err(|err| {
            format!("Failed to sync supervision journal temporary file {tmp:?}: {err}")
        })?;
        drop(output);
        std::fs::rename(&tmp, &path)
            .map_err(|err| format!("Failed to replace supervision journal {path:?}: {err}"))?;
        if let Some(parent) = path.parent() {
            if let Ok(directory) = std::fs::File::open(parent) {
                let _ = directory.sync_all();
            }
        }
        Ok(())
    })();
    if write_result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    write_result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::supervision::{
        InterventionCandidate, InterventionDelivery, InterventionRecord, InterventionVerdict,
        MessageRole, SupervisionEvidence, SupervisionEvent, SupervisionKind, SupervisionReference,
        SupervisionVersion,
    };

    fn record(
        index: u64,
        verdict: InterventionVerdict,
        delivery: InterventionDelivery,
    ) -> InterventionRecord {
        let event = SupervisionEvent::new(
            index + 1,
            &format!("chat-{}", index % 2),
            1_000 + index,
            SupervisionKind::Message {
                role: MessageRole::Assistant,
                message_id: SupervisionReference::new(&format!("message-{index}")),
                body: None,
            },
        );
        InterventionRecord {
            schema_version: SupervisionVersion,
            candidate: InterventionCandidate::Loop,
            evidence: SupervisionEvidence::from_events(&[event]).unwrap(),
            detector_version: SupervisionReference::new("loop.v1"),
            verdict,
            delivery,
            detected_at: 2_000 + index,
            decided_at: 3_000 + index,
        }
    }

    fn sent(index: u64) -> InterventionRecord {
        record(
            index,
            InterventionVerdict::Confirmed,
            InterventionDelivery::Sent {
                delivery_id: SupervisionReference::new(&format!("delivery-{index}")),
                message_id: SupervisionReference::new(&format!("notice-{index}")),
                sent_at: 4_000 + index,
            },
        )
    }

    fn suppressed(
        index: u64,
        verdict: InterventionVerdict,
        reason: SuppressionReason,
    ) -> InterventionRecord {
        record(index, verdict, InterventionDelivery::Suppressed { reason })
    }

    fn limits(max_records: usize, max_bytes: usize) -> SupervisionJournalLimits {
        SupervisionJournalLimits {
            max_records,
            max_bytes,
        }
    }

    #[tokio::test]
    async fn supervision_journal_roundtrip_reload_and_append_dedup() {
        let dir = tempfile::tempdir().unwrap();
        let journal = SupervisionJournal::new(dir.path());
        let first = sent(0);
        let first_id = first.journal_id().unwrap();
        assert_eq!(
            journal.append(first.clone()).await.unwrap(),
            SupervisionJournalAppend::Appended(first_id.clone())
        );
        assert_eq!(
            journal.append(first).await.unwrap(),
            SupervisionJournalAppend::Duplicate(first_id)
        );
        journal
            .append(suppressed(
                1,
                InterventionVerdict::Confirmed,
                SuppressionReason::ShadowMode,
            ))
            .await
            .unwrap();

        let reloaded = SupervisionJournal::new(dir.path()).load().await.unwrap();
        assert_eq!(reloaded.records().len(), 2);
        assert!(matches!(
            &reloaded.records()[0].delivery,
            InterventionDelivery::Sent { .. }
        ));
        assert!(matches!(
            &reloaded.records()[1].delivery,
            InterventionDelivery::Suppressed {
                reason: SuppressionReason::ShadowMode
            }
        ));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn supervision_journal_concurrent_handles_do_not_lose_or_duplicate_records() {
        let dir = tempfile::tempdir().unwrap();
        let journal = SupervisionJournal::new(dir.path());
        let mut tasks = Vec::new();
        for _ in 0..2 {
            for index in 0..12 {
                let journal = journal.clone();
                tasks.push(tokio::spawn(async move {
                    let outcome = journal.append(sent(index)).await.unwrap();
                    assert!(matches!(
                        outcome,
                        SupervisionJournalAppend::Appended(_)
                            | SupervisionJournalAppend::Duplicate(_)
                    ));
                }));
            }
        }
        for task in tasks {
            task.await.unwrap();
        }
        let snapshot = journal.load().await.unwrap();
        assert_eq!(snapshot.records().len(), 12);
        let mut ids = snapshot
            .records()
            .iter()
            .map(|record| record.journal_id().unwrap())
            .collect::<Vec<_>>();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), 12);
    }

    #[tokio::test]
    async fn supervision_journal_bounded_compaction_preserves_current_candidates() {
        let dir = tempfile::tempdir().unwrap();
        let journal = SupervisionJournal::with_limits(dir.path(), limits(2, usize::MAX)).unwrap();
        let first = sent(0);
        let first_id = first.journal_id().unwrap();
        journal.append(first).await.unwrap();
        journal
            .set_current_candidate(&first_id, true)
            .await
            .unwrap();
        journal.append(sent(1)).await.unwrap();
        journal.append(sent(2)).await.unwrap();
        journal.append(sent(3)).await.unwrap();

        let snapshot = journal.load().await.unwrap();
        assert_eq!(snapshot.records().len(), 2);
        assert_eq!(snapshot.records()[0].journal_id().unwrap(), first_id);
        assert_eq!(snapshot.records()[1], sent(3));
        assert_eq!(snapshot.current_candidates().len(), 1);

        let byte_journal = SupervisionJournal::with_limits(dir.path(), limits(32, 1)).unwrap();
        let before = tokio::fs::read(&supervision_journal_path(dir.path()))
            .await
            .unwrap();
        assert!(byte_journal.append(sent(4)).await.is_err());
        let after = tokio::fs::read(&supervision_journal_path(dir.path()))
            .await
            .unwrap();
        assert_eq!(before, after);
    }

    #[tokio::test]
    async fn supervision_journal_compacts_by_serialized_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let sizing = SupervisionJournal::new(dir.path());
        sizing.append(sent(0)).await.unwrap();
        let one_record_bytes = tokio::fs::metadata(supervision_journal_path(dir.path()))
            .await
            .unwrap()
            .len() as usize;
        let max_bytes = one_record_bytes * 2;
        let journal = SupervisionJournal::with_limits(dir.path(), limits(32, max_bytes)).unwrap();
        for index in 1..8 {
            journal.append(sent(index)).await.unwrap();
            assert!(
                tokio::fs::metadata(supervision_journal_path(dir.path()))
                    .await
                    .unwrap()
                    .len() as usize
                    <= max_bytes
            );
        }
        let snapshot = journal.load().await.unwrap();
        assert_eq!(snapshot.records().len(), 2);
        assert_eq!(snapshot.records()[0], sent(6));
        assert_eq!(snapshot.records()[1], sent(7));
    }

    #[tokio::test]
    async fn supervision_journal_replay_distinguishes_delivery_silence_and_false_positives() {
        let dir = tempfile::tempdir().unwrap();
        let journal = SupervisionJournal::new(dir.path());
        journal.append(sent(0)).await.unwrap();
        journal
            .append(suppressed(
                1,
                InterventionVerdict::Confirmed,
                SuppressionReason::ShadowMode,
            ))
            .await
            .unwrap();
        journal
            .append(suppressed(
                2,
                InterventionVerdict::Dismissed,
                SuppressionReason::NotConfirmed,
            ))
            .await
            .unwrap();
        let replay = journal.load().await.unwrap().replay();
        assert_eq!(replay.len(), 3);
        assert_eq!(replay[0].outcome, SupervisionReplayOutcome::Sent);
        assert_eq!(
            replay[1].outcome,
            SupervisionReplayOutcome::Silent(SuppressionReason::ShadowMode)
        );
        assert_eq!(
            replay[2].outcome,
            SupervisionReplayOutcome::FalsePositive {
                delivered: false,
                suppression_reason: Some(SuppressionReason::NotConfirmed),
            }
        );
        assert_eq!(replay[0].record.candidate, InterventionCandidate::Loop);
        assert!(!replay[0].record.evidence.ids().is_empty());
        assert_eq!(replay[2].record.verdict, InterventionVerdict::Dismissed);
    }

    #[tokio::test]
    async fn supervision_journal_malformed_record_fails_without_rewriting() {
        let dir = tempfile::tempdir().unwrap();
        let journal = SupervisionJournal::new(dir.path());
        journal.append(sent(0)).await.unwrap();
        let path = supervision_journal_path(dir.path());
        let mut value: serde_json::Value =
            serde_json::from_str(&tokio::fs::read_to_string(&path).await.unwrap()).unwrap();
        value["records"][0]["tool_output"] = serde_json::json!("PRIVATE_TOOL_CANARY");
        let malformed = serde_json::to_vec(&value).unwrap();
        tokio::fs::write(&path, &malformed).await.unwrap();
        assert!(journal.load().await.is_err());
        assert!(journal.append(sent(1)).await.is_err());
        assert_eq!(tokio::fs::read(&path).await.unwrap(), malformed);
    }

    #[tokio::test]
    async fn supervision_journal_legacy_empty_state_is_non_destructive() {
        let dir = tempfile::tempdir().unwrap();
        let buddy = dir.path().join(".refact/buddy");
        tokio::fs::create_dir_all(&buddy).await.unwrap();
        let state_path = buddy.join("state.json");
        tokio::fs::write(&state_path, b"{\"legacy\":true}")
            .await
            .unwrap();
        let journal = SupervisionJournal::new(dir.path());
        let snapshot = journal.load().await.unwrap();
        assert!(snapshot.records().is_empty());
        assert!(snapshot.current_candidates().is_empty());
        assert!(!supervision_journal_path(dir.path()).exists());
        assert_eq!(
            tokio::fs::read(&state_path).await.unwrap(),
            b"{\"legacy\":true}"
        );
    }

    #[tokio::test]
    async fn supervision_journal_serialization_is_utf8_bounded_and_redacted() {
        let dir = tempfile::tempdir().unwrap();
        let journal = SupervisionJournal::new(dir.path());
        let raw = "Bearer sk-VERYSECRET123456 token=private-value https://example.test?q=canary";
        let event = SupervisionEvent::new(
            1,
            raw,
            1_000,
            SupervisionKind::Message {
                role: MessageRole::Assistant,
                message_id: SupervisionReference::new(raw),
                body: None,
            },
        );
        let sensitive = InterventionRecord {
            schema_version: SupervisionVersion,
            candidate: InterventionCandidate::OffGoal,
            evidence: SupervisionEvidence::from_events(&[event]).unwrap(),
            detector_version: SupervisionReference::new(&format!("{} {}", raw, "🦀".repeat(200))),
            verdict: InterventionVerdict::Confirmed,
            delivery: InterventionDelivery::Sent {
                delivery_id: SupervisionReference::new(raw),
                message_id: SupervisionReference::new(raw),
                sent_at: 2_000,
            },
            detected_at: 1_500,
            decided_at: 1_600,
        };
        journal.append(sensitive).await.unwrap();
        let serialized = tokio::fs::read_to_string(supervision_journal_path(dir.path()))
            .await
            .unwrap();
        for secret in [
            "VERYSECRET",
            "private-value",
            "canary",
            "PRIVATE_TOOL_CANARY",
        ] {
            assert!(!serialized.contains(secret), "leaked {secret}");
        }
        assert!(serialized.contains("REDACTED"));
        assert!(!serialized.contains("tool_output"));
        assert!(!serialized.contains("reasoning"));
        let snapshot = journal.load().await.unwrap();
        assert!(snapshot.records()[0].detector_version.as_str().len() <= 128);
        assert!(snapshot.records()[0]
            .detector_version
            .as_str()
            .is_char_boundary(snapshot.records()[0].detector_version.as_str().len()));
    }
}
