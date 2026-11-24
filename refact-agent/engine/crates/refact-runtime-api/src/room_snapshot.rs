use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use refact_files::indexing_settings::{is_blocklisted, IndexingEverywhere};
use refact_privacy::{CompiledPolicy, Destination};
use refact_room::{
    SourceDeliveryVersion, SourceFile, SourceIdentity, SourceSnapshot, SourceSnapshotError,
};
use refact_uhp::{Client, Discovery, EnvironmentBuildStatus, UhpBindingRecords};
use tempfile::NamedTempFile;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnapshotExportLimits {
    pub max_files: usize,
    pub max_file_bytes: u64,
    pub max_total_bytes: u64,
}

impl Default for SnapshotExportLimits {
    fn default() -> Self {
        Self {
            max_files: 20_000,
            max_file_bytes: 1024 * 1024,
            max_total_bytes: 64 * 1024 * 1024,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportedSourceSnapshot {
    pub snapshot: SourceSnapshot,
    pub archive: Vec<u8>,
}

pub fn export_writer_snapshot(
    writer_root: &Path,
    privacy: &CompiledPolicy,
    indexing: &IndexingEverywhere,
    destination: &Destination,
    limits: SnapshotExportLimits,
) -> Result<ExportedSourceSnapshot, String> {
    let root = canonical_writer_root(writer_root)?;
    let repository = git2::Repository::open(&root)
        .map_err(|error| format!("failed to open canonical writer repository: {error}"))?;
    let workdir = repository
        .workdir()
        .ok_or_else(|| "canonical writer repository has no workdir".to_string())?;
    let workdir = workdir
        .canonicalize()
        .map_err(|error| format!("failed to canonicalize writer workdir: {error}"))?;
    if dunce::simplified(&workdir) != dunce::simplified(&root) {
        return Err("writer root must be the canonical repository workdir".to_string());
    }
    let revision = repository
        .head()
        .and_then(|head| head.peel_to_commit())
        .map(|commit| commit.id().to_string())
        .map_err(|error| format!("failed to resolve writer HEAD: {error}"))?;
    let mut files = Vec::new();
    let mut total_bytes = 0_u64;
    collect_source_files(
        &root,
        &root,
        privacy,
        indexing,
        destination,
        limits,
        &mut total_bytes,
        &mut files,
    )?;
    let snapshot = SourceSnapshot::new(revision, files).map_err(|error| error.to_string())?;
    let archive = encode_read_only_tar(&snapshot)?;
    Ok(ExportedSourceSnapshot { snapshot, archive })
}

pub fn publication_metadata_if_fresh(
    expected: &SourceDeliveryVersion,
    current: &SourceIdentity,
) -> Result<BTreeMap<String, String>, SourceSnapshotError> {
    expected.publication_metadata(current)
}

pub async fn publish_snapshot_environment(
    client: &Client,
    discovery: &Discovery,
    environment_id: &str,
    archive: Vec<u8>,
) -> Result<u64, String> {
    client
        .import_environment_bytes(
            discovery,
            environment_id,
            archive,
            "application/x-tar",
            true,
        )
        .await
        .map_err(|error| format!("failed to import snapshot environment: {error}"))?;
    let build = client
        .build_environment(discovery, environment_id)
        .await
        .map_err(|error| format!("failed to build snapshot environment: {error}"))?;
    if build.version == 0 {
        return Err("snapshot environment build has no version".to_string());
    }
    let version = build.version;
    for _ in 0..60 {
        let status = client
            .get_environment_build(discovery, environment_id, version)
            .await
            .map_err(|error| format!("failed to read snapshot environment build: {error}"))?;
        match status.status {
            EnvironmentBuildStatus::Ready => {
                let environment = client
                    .activate_environment_version(discovery, environment_id, version)
                    .await
                    .map_err(|error| format!("failed to activate snapshot environment: {error}"))?;
                return Ok(environment.version.unwrap_or(version));
            }
            EnvironmentBuildStatus::Failed => {
                return Err("snapshot environment build failed".to_string());
            }
            EnvironmentBuildStatus::Building | EnvironmentBuildStatus::Unknown => {
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }
        }
    }
    Err("snapshot environment build did not become ready".to_string())
}

fn canonical_writer_root(root: &Path) -> Result<PathBuf, String> {
    let metadata = std::fs::symlink_metadata(root)
        .map_err(|error| format!("failed to inspect writer root: {error}"))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err("writer root must be a real directory, not a symlink".to_string());
    }
    root.canonicalize()
        .map_err(|error| format!("failed to canonicalize writer root: {error}"))
}

#[allow(clippy::too_many_arguments)]
fn collect_source_files(
    root: &Path,
    directory: &Path,
    privacy: &CompiledPolicy,
    indexing: &IndexingEverywhere,
    destination: &Destination,
    limits: SnapshotExportLimits,
    total_bytes: &mut u64,
    files: &mut Vec<SourceFile>,
) -> Result<(), String> {
    let entries = std::fs::read_dir(directory)
        .map_err(|error| format!("failed to enumerate writer source: {error}"))?;
    let mut entries = entries
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("failed to enumerate writer source: {error}"))?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let path = entry.path();
        let relative = path
            .strip_prefix(root)
            .map_err(|_| "writer source escaped its root".to_string())?;
        if excluded_path(relative) {
            continue;
        }
        let metadata = std::fs::symlink_metadata(&path)
            .map_err(|error| format!("failed to inspect writer source: {error}"))?;
        if metadata.file_type().is_symlink() {
            return Err("writer snapshot rejects symbolic links".to_string());
        }
        if metadata.is_dir() {
            collect_source_files(
                root,
                &path,
                privacy,
                indexing,
                destination,
                limits,
                total_bytes,
                files,
            )?;
            continue;
        }
        if !metadata.is_file() {
            return Err("writer snapshot accepts only regular files".to_string());
        }
        if policy_blocks_path(privacy, indexing, destination, root, &path, relative) {
            continue;
        }
        if files.len() >= limits.max_files {
            return Err("writer snapshot exceeds the file count limit".to_string());
        }
        if metadata.len() > limits.max_file_bytes {
            return Err("writer snapshot contains an oversized file".to_string());
        }
        *total_bytes = total_bytes
            .checked_add(metadata.len())
            .ok_or_else(|| "writer snapshot size overflow".to_string())?;
        if *total_bytes > limits.max_total_bytes {
            return Err("writer snapshot exceeds the total size limit".to_string());
        }
        let normalized = normalized_relative_path(relative)?;
        let bytes = std::fs::read(&path)
            .map_err(|error| format!("failed to read writer source: {error}"))?;
        if bytes.len() as u64 != metadata.len() {
            return Err("writer source changed while it was being exported".to_string());
        }
        files.push(SourceFile::new(normalized, bytes));
    }
    Ok(())
}

fn policy_blocks_path(
    privacy: &CompiledPolicy,
    indexing: &IndexingEverywhere,
    destination: &Destination,
    root: &Path,
    path: &Path,
    relative: &Path,
) -> bool {
    let indexing_settings = indexing.indexing_for_path(path);
    if is_blocklisted(&indexing_settings, relative) {
        return true;
    }
    let zone = privacy.strictest_zone_for_paths_with_roots([path, relative], [root]);
    zone.name == "blocked" || !destination.matches_send_to(&zone.send_to)
}

fn excluded_path(path: &Path) -> bool {
    if path.components().any(|component| {
        matches!(
            component
                .as_os_str()
                .to_string_lossy()
                .to_ascii_lowercase()
                .as_str(),
            ".git" | ".hg" | ".svn" | ".refact" | ".ssh" | ".aws" | ".gnupg"
        )
    }) {
        return true;
    }
    let Some(name) = path
        .file_name()
        .map(|name| name.to_string_lossy().to_ascii_lowercase())
    else {
        return false;
    };
    name.starts_with(".env")
        || name.contains("secret")
        || name.contains("credential")
        || matches!(
            name.as_str(),
            ".netrc" | ".npmrc" | ".pypirc" | "id_rsa" | "id_dsa" | "id_ecdsa" | "id_ed25519"
        )
        || [".pem", ".key", ".p12", ".pfx", ".keystore", ".jks"]
            .iter()
            .any(|extension| name.ends_with(extension))
}

fn normalized_relative_path(path: &Path) -> Result<String, String> {
    path.components()
        .map(|component| {
            component
                .as_os_str()
                .to_str()
                .ok_or_else(|| "writer snapshot requires UTF-8 paths".to_string())
        })
        .collect::<Result<Vec<_>, _>>()
        .map(|components| components.join("/"))
}

fn encode_read_only_tar(snapshot: &SourceSnapshot) -> Result<Vec<u8>, String> {
    let mut archive = Vec::new();
    for file in snapshot.files() {
        let mut header = [0_u8; 512];
        write_tar_path(&mut header, &file.path)?;
        write_octal(&mut header[100..108], 0o444)?;
        write_octal(&mut header[108..116], 0)?;
        write_octal(&mut header[116..124], 0)?;
        write_octal(&mut header[124..136], file.bytes.len() as u64)?;
        write_octal(&mut header[136..148], 0)?;
        header[148..156].fill(b' ');
        header[156] = b'0';
        header[257..263].copy_from_slice(b"ustar\0");
        header[263..265].copy_from_slice(b"00");
        let checksum = header.iter().map(|byte| *byte as u64).sum::<u64>();
        write_tar_checksum(&mut header[148..156], checksum)?;
        archive.extend_from_slice(&header);
        archive.extend_from_slice(&file.bytes);
        let padding = (512 - file.bytes.len() % 512) % 512;
        archive.resize(archive.len() + padding, 0);
    }
    archive.resize(archive.len() + 1024, 0);
    Ok(archive)
}

fn write_tar_path(header: &mut [u8; 512], path: &str) -> Result<(), String> {
    let bytes = path.as_bytes();
    if bytes.len() <= 100 {
        header[..bytes.len()].copy_from_slice(bytes);
        return Ok(());
    }
    let split = path
        .char_indices()
        .filter(|(_, character)| *character == '/')
        .map(|(index, _)| index)
        .filter(|index| *index <= 155 && bytes.len() - index - 1 <= 100)
        .last()
        .ok_or_else(|| "writer snapshot path exceeds tar limits".to_string())?;
    header[..bytes.len() - split - 1].copy_from_slice(&bytes[split + 1..]);
    header[345..345 + split].copy_from_slice(&bytes[..split]);
    Ok(())
}

fn write_octal(field: &mut [u8], value: u64) -> Result<(), String> {
    let encoded = format!("{:0width$o}\0", value, width = field.len() - 1);
    if encoded.len() != field.len() {
        return Err("writer snapshot numeric field exceeds tar limits".to_string());
    }
    field.copy_from_slice(encoded.as_bytes());
    Ok(())
}

fn write_tar_checksum(field: &mut [u8], checksum: u64) -> Result<(), String> {
    let encoded = format!("{checksum:06o}\0 ");
    if encoded.len() != field.len() {
        return Err("writer snapshot checksum exceeds tar limits".to_string());
    }
    field.copy_from_slice(encoded.as_bytes());
    Ok(())
}

#[derive(Debug, Clone)]
pub struct JsonUhpBindingStore {
    directory: PathBuf,
}

impl JsonUhpBindingStore {
    pub fn new(directory: impl Into<PathBuf>) -> Self {
        Self {
            directory: directory.into(),
        }
    }

    fn records_path(&self) -> PathBuf {
        self.directory.join("uhp-bindings.json")
    }

    fn lock_path(&self) -> PathBuf {
        self.directory.join(".uhp-bindings.lock")
    }

    pub fn load(&self) -> Result<UhpBindingRecords, String> {
        std::fs::create_dir_all(&self.directory)
            .map_err(|error| format!("failed to create UHP binding directory: {error}"))?;
        let mut lock = refact_core::file_lock::open_lock(&self.lock_path())
            .map_err(|error| format!("failed to open UHP binding lock: {error}"))?;
        let _guard = refact_core::file_lock::lock_blocking(&mut lock)
            .map_err(|error| format!("failed to lock UHP bindings: {error}"))?;
        let path = self.records_path();
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(UhpBindingRecords::new());
            }
            Err(error) => return Err(format!("failed to read UHP bindings: {error}")),
        };
        let records: UhpBindingRecords = serde_json::from_slice(&bytes)
            .map_err(|error| format!("failed to decode UHP bindings: {error}"))?;
        records.validate().map_err(|error| error.to_string())?;
        Ok(records)
    }

    pub fn save(&self, records: &UhpBindingRecords) -> Result<(), String> {
        records.validate().map_err(|error| error.to_string())?;
        std::fs::create_dir_all(&self.directory)
            .map_err(|error| format!("failed to create UHP binding directory: {error}"))?;
        let mut lock = refact_core::file_lock::open_lock(&self.lock_path())
            .map_err(|error| format!("failed to open UHP binding lock: {error}"))?;
        let _guard = refact_core::file_lock::lock_blocking(&mut lock)
            .map_err(|error| format!("failed to lock UHP bindings: {error}"))?;
        let encoded = serde_json::to_vec(records)
            .map_err(|error| format!("failed to encode UHP bindings: {error}"))?;
        let mut temporary = NamedTempFile::new_in(&self.directory)
            .map_err(|error| format!("failed to create UHP binding temporary file: {error}"))?;
        temporary
            .write_all(&encoded)
            .and_then(|_| temporary.as_file().sync_all())
            .map_err(|error| format!("failed to write UHP bindings: {error}"))?;
        temporary
            .persist(self.records_path())
            .map_err(|error| format!("failed to replace UHP bindings: {error}"))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use refact_privacy::{DestinationId, DestinationKind, PrivacyPolicy};
    use serde_json::json;

    use super::*;

    fn destination() -> Destination {
        Destination {
            id: DestinationId("uhp-test".to_string()),
            kind: DestinationKind::Provider,
            display_name: "UHP test".to_string(),
        }
    }

    fn policy() -> CompiledPolicy {
        PrivacyPolicy {
            blocked: vec!["private.txt".to_string()],
            ..PrivacyPolicy::default()
        }
        .compile()
        .unwrap()
    }

    fn repository() -> (tempfile::TempDir, git2::Repository) {
        let directory = tempfile::tempdir().unwrap();
        let repository = git2::Repository::init(directory.path()).unwrap();
        std::fs::create_dir(directory.path().join("src")).unwrap();
        std::fs::write(directory.path().join("README.md"), b"committed marker\n").unwrap();
        std::fs::write(
            directory.path().join("src/lib.rs"),
            b"pub fn original() {}\n",
        )
        .unwrap();
        let mut index = repository.index().unwrap();
        index
            .add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)
            .unwrap();
        index.write().unwrap();
        let tree = repository.find_tree(index.write_tree().unwrap()).unwrap();
        let signature = git2::Signature::now("Snapshot Test", "snapshot@example.test").unwrap();
        repository
            .commit(Some("HEAD"), &signature, &signature, "initial", &tree, &[])
            .unwrap();
        (directory, repository)
    }

    fn export(
        root: &Path,
        privacy: &CompiledPolicy,
        limits: SnapshotExportLimits,
    ) -> Result<ExportedSourceSnapshot, String> {
        export_writer_snapshot(
            root,
            privacy,
            &IndexingEverywhere::default(),
            &destination(),
            limits,
        )
    }

    #[test]
    fn room_snapshot_includes_committed_and_uncommitted_markers_but_not_secrets() {
        let (directory, _repository) = repository();
        std::fs::write(directory.path().join("src/lib.rs"), b"pub fn dirty() {}\n").unwrap();
        std::fs::write(directory.path().join("secrets.yaml"), b"token: canary").unwrap();
        std::fs::write(directory.path().join(".env"), b"TOKEN=canary").unwrap();
        std::fs::write(directory.path().join("private.txt"), b"policy canary").unwrap();
        let exported =
            export(directory.path(), &policy(), SnapshotExportLimits::default()).unwrap();
        let files = exported
            .snapshot
            .files()
            .iter()
            .map(|file| file.path.as_str())
            .collect::<Vec<_>>();
        assert_eq!(files, ["README.md", "src/lib.rs"]);
        assert!(exported
            .snapshot
            .files()
            .iter()
            .any(|file| file.bytes.as_slice() == b"pub fn dirty() {}\n"));
        let archive = String::from_utf8_lossy(&exported.archive);
        assert!(!archive.contains("canary"));
        assert!(!archive.contains(".git"));
        assert_eq!(&exported.archive[100..108], b"0000444\0");
    }

    #[test]
    fn room_snapshot_archive_and_digest_are_reproducible() {
        let (directory, _repository) = repository();
        std::fs::write(directory.path().join("src/lib.rs"), b"pub fn dirty() {}\n").unwrap();
        let first = export(directory.path(), &policy(), SnapshotExportLimits::default()).unwrap();
        let second = export(directory.path(), &policy(), SnapshotExportLimits::default()).unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn room_snapshot_changed_writer_is_stale_and_not_current_evidence() {
        let (directory, _repository) = repository();
        let first = export(directory.path(), &policy(), SnapshotExportLimits::default()).unwrap();
        let delivery = first.snapshot.bind_delivery("delivery-1", 1, None).unwrap();
        std::fs::write(
            directory.path().join("src/lib.rs"),
            b"pub fn changed() {}\n",
        )
        .unwrap();
        let current = export(directory.path(), &policy(), SnapshotExportLimits::default()).unwrap();
        assert_eq!(
            publication_metadata_if_fresh(&delivery, current.snapshot.identity()),
            Err(SourceSnapshotError::StaleSource)
        );
    }

    #[test]
    fn room_snapshot_rejects_oversized_files() {
        let (directory, _repository) = repository();
        let error = export(
            directory.path(),
            &policy(),
            SnapshotExportLimits {
                max_file_bytes: 2,
                ..SnapshotExportLimits::default()
            },
        )
        .unwrap_err();
        assert!(error.contains("oversized"));
    }

    #[cfg(unix)]
    #[test]
    fn room_snapshot_rejects_symlink_escape() {
        let (directory, _repository) = repository();
        let outside = tempfile::NamedTempFile::new().unwrap();
        std::os::unix::fs::symlink(outside.path(), directory.path().join("src/escape.rs")).unwrap();
        let error =
            export(directory.path(), &policy(), SnapshotExportLimits::default()).unwrap_err();
        assert!(error.contains("symbolic links"));
    }

    fn binding_records() -> UhpBindingRecords {
        serde_json::from_value(json!({
            "schema_version": 1,
            "participants": {
                "participant-1": {
                    "participant_id": "participant-1",
                    "server_root": "https://uhp.example.test",
                    "server_implementation": "harness-router",
                    "server_version": "0.41.25",
                    "image_digest": "sha256:0123456789abcdef",
                    "harness_id": "chrn_1",
                    "base": "opaque-base",
                    "cli_pin": "1.2.3",
                    "model": "model-1",
                    "credential_profile_ref": "credential-profile-1",
                    "environment_id": "henv_1",
                    "environment_version": 1,
                    "protocol_version": "2026-10-19",
                    "capability_snapshot": {"environments": true}
                }
            },
            "sessions": {
                "session-1": {
                    "participant_id": "participant-1",
                    "session_id": "session-1",
                    "last_response_id": "response-1",
                    "last_terminal_status": "completed",
                    "created_at": 1,
                    "updated_at": 2,
                    "replay_cursor": 10
                }
            },
            "deliveries": {
                "delivery-1": {
                    "delivery_id": "delivery-1",
                    "room_id": "room-1",
                    "event_id": "event-1",
                    "recipient_id": "participant-1",
                    "logical_attempt": 0,
                    "request_fingerprint": "cd".repeat(32),
                    "idempotency_key": "delivery-key-1",
                    "request_state": "completed",
                    "response_id": "response-1",
                    "session_id": "session-1",
                    "source_revision": "commit-1",
                    "source_digest": "ab".repeat(32),
                    "environment_version": 1,
                    "created_at": 1,
                    "updated_at": 2
                }
            },
            "evidence": {
                "smoke-1": {
                    "smoke_suite_id": "smoke-1",
                    "server_version": "0.41.25",
                    "cli_version": "1.2.3",
                    "model_version": "model-1",
                    "checks": [{"name": "freshness", "status": "pass"}],
                    "generated_at": 3,
                    "sanitized_report_location": "evidence/smoke-1.json"
                }
            }
        }))
        .unwrap()
    }

    #[test]
    fn room_snapshot_binding_store_recovers_all_durable_records() {
        let directory = tempfile::tempdir().unwrap();
        let records = binding_records();
        JsonUhpBindingStore::new(directory.path())
            .save(&records)
            .unwrap();
        let recovered = JsonUhpBindingStore::new(directory.path()).load().unwrap();
        assert_eq!(recovered, records);
    }
}
