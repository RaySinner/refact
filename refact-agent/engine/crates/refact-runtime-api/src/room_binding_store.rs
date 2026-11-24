use std::io::Write;
use std::path::PathBuf;

use refact_uhp::UhpBindingRecords;
use tempfile::NamedTempFile;

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
    use serde_json::json;

    use super::*;

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
