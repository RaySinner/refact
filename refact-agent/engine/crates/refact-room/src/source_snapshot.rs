use std::collections::{BTreeMap, HashSet};
use std::path::{Component, Path};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const SOURCE_SNAPSHOT_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceFile {
    pub path: String,
    pub bytes: Vec<u8>,
}

impl SourceFile {
    pub fn new(path: impl Into<String>, bytes: impl Into<Vec<u8>>) -> Self {
        Self {
            path: path.into(),
            bytes: bytes.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Hash)]
pub struct SourceIdentity {
    pub source_revision: String,
    pub source_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceSnapshot {
    identity: SourceIdentity,
    files: Vec<SourceFile>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceDeliveryVersion {
    pub delivery_id: String,
    pub environment_version: u64,
    pub source: SourceIdentity,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SourceSnapshotError {
    #[error("invalid source snapshot: {0}")]
    Validation(String),
    #[error("refact.room.stale_source")]
    StaleSource,
}

impl SourceSnapshot {
    pub fn new(
        source_revision: impl Into<String>,
        mut files: Vec<SourceFile>,
    ) -> Result<Self, SourceSnapshotError> {
        let source_revision = source_revision.into();
        require("source_revision", &source_revision)?;
        files.sort_by(|left, right| left.path.cmp(&right.path));
        let mut paths = HashSet::new();
        let mut hasher = Sha256::new();
        hasher.update(b"refact-room-source-snapshot-v1\0");
        hash_field(&mut hasher, source_revision.as_bytes());
        for file in &files {
            validate_relative_path(&file.path)?;
            if !paths.insert(file.path.as_str()) {
                return Err(SourceSnapshotError::Validation(format!(
                    "duplicate source path '{}'",
                    file.path
                )));
            }
            hash_field(&mut hasher, file.path.as_bytes());
            hasher.update((file.bytes.len() as u64).to_le_bytes());
            hasher.update(&file.bytes);
        }
        Ok(Self {
            identity: SourceIdentity {
                source_revision,
                source_digest: hex::encode(hasher.finalize()),
            },
            files,
        })
    }

    pub fn identity(&self) -> &SourceIdentity {
        &self.identity
    }

    pub fn files(&self) -> &[SourceFile] {
        &self.files
    }

    pub fn bind_delivery(
        &self,
        delivery_id: impl Into<String>,
        environment_version: u64,
        previous: Option<&SourceDeliveryVersion>,
    ) -> Result<SourceDeliveryVersion, SourceSnapshotError> {
        let delivery_id = delivery_id.into();
        require("delivery_id", &delivery_id)?;
        if environment_version == 0 {
            return Err(SourceSnapshotError::Validation(
                "environment_version must be greater than zero".to_string(),
            ));
        }
        if let Some(previous) = previous {
            if previous.delivery_id == delivery_id {
                return Err(SourceSnapshotError::Validation(
                    "a delivery id cannot be rebound to another source version".to_string(),
                ));
            }
            if environment_version <= previous.environment_version {
                return Err(SourceSnapshotError::Validation(
                    "each delivery requires a new environment version".to_string(),
                ));
            }
        }
        Ok(SourceDeliveryVersion {
            delivery_id,
            environment_version,
            source: self.identity.clone(),
        })
    }
}

impl SourceDeliveryVersion {
    pub fn prompt_metadata(&self) -> BTreeMap<String, String> {
        BTreeMap::from([
            (
                "refact.room.schema_version".to_string(),
                SOURCE_SNAPSHOT_SCHEMA_VERSION.to_string(),
            ),
            (
                "refact.room.delivery_id".to_string(),
                self.delivery_id.clone(),
            ),
            (
                "refact.room.source_revision".to_string(),
                self.source.source_revision.clone(),
            ),
            (
                "refact.room.source_digest".to_string(),
                self.source.source_digest.clone(),
            ),
        ])
    }

    pub fn publication_metadata(
        &self,
        current: &SourceIdentity,
    ) -> Result<BTreeMap<String, String>, SourceSnapshotError> {
        if &self.source != current {
            return Err(SourceSnapshotError::StaleSource);
        }
        Ok(self.prompt_metadata())
    }
}

fn hash_field(hasher: &mut Sha256, value: &[u8]) {
    hasher.update((value.len() as u64).to_le_bytes());
    hasher.update(value);
}

fn require(field: &str, value: &str) -> Result<(), SourceSnapshotError> {
    if value.trim().is_empty() {
        return Err(SourceSnapshotError::Validation(format!(
            "{field} must not be empty"
        )));
    }
    Ok(())
}

fn validate_relative_path(value: &str) -> Result<(), SourceSnapshotError> {
    let path = Path::new(value);
    if value.is_empty()
        || value.contains('\0')
        || value.contains('\\')
        || path.is_absolute()
        || value
            .split('/')
            .next()
            .is_some_and(|segment| segment.contains(':'))
        || value
            .split('/')
            .any(|segment| segment.is_empty() || segment == "." || segment == "..")
        || path.components().any(|component| {
            !matches!(component, Component::Normal(_)) || component.as_os_str() == ".."
        })
    {
        return Err(SourceSnapshotError::Validation(format!(
            "source path must be a normalized relative path: '{value}'"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source_snapshot() -> SourceSnapshot {
        SourceSnapshot::new(
            "commit-1",
            vec![
                SourceFile::new("src/lib.rs", b"pub fn changed() {}\n".to_vec()),
                SourceFile::new("README.md", b"committed\n".to_vec()),
            ],
        )
        .unwrap()
    }

    #[test]
    fn source_snapshot_digest_is_deterministic_and_covers_uncommitted_bytes() {
        let mut reversed = source_snapshot();
        reversed.files.reverse();
        let rebuilt = SourceSnapshot::new("commit-1", reversed.files).unwrap();
        assert_eq!(source_snapshot().identity(), rebuilt.identity());

        let changed = SourceSnapshot::new(
            "commit-1",
            vec![
                SourceFile::new("README.md", b"committed\n".to_vec()),
                SourceFile::new("src/lib.rs", b"pub fn dirty() {}\n".to_vec()),
            ],
        )
        .unwrap();
        assert_ne!(source_snapshot().identity(), changed.identity());
    }

    #[test]
    fn source_snapshot_rejects_traversal_absolute_and_duplicate_paths() {
        for path in [
            "../outside",
            "/absolute",
            "C:/absolute",
            "src\\lib.rs",
            "src/./lib.rs",
        ] {
            assert!(
                SourceSnapshot::new("commit-1", vec![SourceFile::new(path, b"x".to_vec())])
                    .is_err()
            );
        }
        assert!(SourceSnapshot::new(
            "commit-1",
            vec![
                SourceFile::new("same", b"a".to_vec()),
                SourceFile::new("same", b"b".to_vec()),
            ],
        )
        .is_err());
    }

    #[test]
    fn source_snapshot_changed_writer_returns_stale_source() {
        let snapshot = source_snapshot();
        let delivery = snapshot.bind_delivery("delivery-1", 1, None).unwrap();
        assert!(delivery.publication_metadata(snapshot.identity()).is_ok());

        let changed = SourceSnapshot::new("commit-2", snapshot.files.clone()).unwrap();
        assert_eq!(
            delivery.publication_metadata(changed.identity()),
            Err(SourceSnapshotError::StaleSource)
        );
    }

    #[test]
    fn source_snapshot_every_delivery_requires_a_new_environment_version() {
        let snapshot = source_snapshot();
        let first = snapshot.bind_delivery("delivery-1", 4, None).unwrap();
        let second = snapshot
            .bind_delivery("delivery-2", 5, Some(&first))
            .unwrap();
        assert_eq!(second.source, first.source);
        assert!(snapshot
            .bind_delivery("delivery-3", 4, Some(&second))
            .is_err());
        assert!(snapshot
            .bind_delivery("delivery-1", 6, Some(&second))
            .is_err());
    }
}
