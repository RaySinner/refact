use std::collections::BTreeMap;

pub use crate::room_binding_store::JsonUhpBindingStore;
pub use crate::room_snapshot_export::{
    export_writer_snapshot, ExportedSourceSnapshot, SnapshotExportLimits,
};
use refact_room::{SourceDeliveryVersion, SourceIdentity, SourceSnapshotError};
use refact_uhp::{Client, Discovery, EnvironmentBuildStatus};

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
