use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use tempfile::NamedTempFile;

use super::{ExternalParticipantError, ExternalParticipantStateData};

#[derive(Clone, Debug)]
pub struct ExternalParticipantStore {
    directory: PathBuf,
}

impl ExternalParticipantStore {
    pub fn new(directory: impl Into<PathBuf>) -> Self {
        Self {
            directory: directory.into(),
        }
    }

    fn state_path(&self) -> PathBuf {
        self.directory.join("external-participants.json")
    }

    fn secrets_path(&self) -> PathBuf {
        self.directory.join("secrets.yaml")
    }

    pub fn load(&self) -> Result<ExternalParticipantStateData, ExternalParticipantError> {
        let bytes = match std::fs::read(self.state_path()) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(ExternalParticipantStateData::default())
            }
            Err(error) => return Err(storage_error("read participant registry", error)),
        };
        let state: ExternalParticipantStateData = serde_json::from_slice(&bytes)
            .map_err(|error| ExternalParticipantError::Storage(error.to_string()))?;
        state.validate()?;
        Ok(state)
    }

    pub fn save(
        &self,
        state: &ExternalParticipantStateData,
    ) -> Result<(), ExternalParticipantError> {
        state.validate()?;
        std::fs::create_dir_all(&self.directory)
            .map_err(|error| storage_error("create participant registry directory", error))?;
        let mut lock =
            refact_core::file_lock::open_lock(&self.directory.join(".external-participants.lock"))
                .map_err(|error| storage_error("open participant registry lock", error))?;
        let _guard = refact_core::file_lock::lock_blocking(&mut lock)
            .map_err(|error| storage_error("lock participant registry", error))?;
        let encoded = serde_json::to_vec(state)
            .map_err(|error| ExternalParticipantError::Storage(error.to_string()))?;
        atomic_write(&self.directory, &self.state_path(), &encoded)
    }

    pub fn write_bearer(
        &self,
        reference: &str,
        bearer: &str,
    ) -> Result<(), ExternalParticipantError> {
        if bearer.is_empty() || bearer.chars().any(char::is_control) {
            return Err(ExternalParticipantError::InvalidConfiguration(
                "credential payload must be non-empty and single-line",
            ));
        }
        std::fs::create_dir_all(&self.directory)
            .map_err(|error| storage_error("create credential directory", error))?;
        let path = self.secrets_path();
        let mut lock =
            refact_core::file_lock::open_lock(&self.directory.join(".external-credentials.lock"))
                .map_err(|error| storage_error("open credential lock", error))?;
        let _guard = refact_core::file_lock::lock_blocking(&mut lock)
            .map_err(|error| storage_error("lock credentials", error))?;
        let mut credentials = read_credentials(&path)?;
        credentials.insert(reference.to_string(), bearer.to_string());
        let encoded = serde_yaml::to_string(&credentials)
            .map_err(|error| ExternalParticipantError::Storage(error.to_string()))?;
        atomic_write(&self.directory, &path, encoded.as_bytes())
    }

    pub fn bearer(&self, reference: &str) -> Result<Option<String>, ExternalParticipantError> {
        Ok(read_credentials(&self.secrets_path())?.remove(reference))
    }
}

fn read_credentials(path: &Path) -> Result<BTreeMap<String, String>, ExternalParticipantError> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(error) => return Err(storage_error("read credentials", error)),
    };
    serde_yaml::from_slice(&bytes)
        .map_err(|error| ExternalParticipantError::Storage(error.to_string()))
}

fn atomic_write(
    directory: &Path,
    path: &Path,
    bytes: &[u8],
) -> Result<(), ExternalParticipantError> {
    let mut temporary = NamedTempFile::new_in(directory)
        .map_err(|error| storage_error("create registry temporary file", error))?;
    temporary
        .write_all(bytes)
        .and_then(|_| temporary.as_file().sync_all())
        .map_err(|error| storage_error("write participant registry", error))?;
    temporary
        .persist(path)
        .map_err(|error| storage_error("replace participant registry", error.error))?;
    Ok(())
}

fn storage_error(operation: &str, error: std::io::Error) -> ExternalParticipantError {
    ExternalParticipantError::Storage(format!("{operation}: {error}"))
}
