use std::path::Path;
use std::sync::Arc;

use refact_exec::ObservationStatus;
use refact_privacy::model_context::ShellReadDecision;
use refact_privacy::{DerivedPrivacyZones, Destination};

use crate::call_validation::ChatMessage;
use crate::global_context::GlobalContext;

pub use refact_runtime_api::gcx_helpers::{
    declared_file_records, privacy_record_scope, tool_enrichment_from_path_references,
};

pub(crate) use refact_runtime_api::gcx_helpers::filter_path_enrichment_for_model_context;

pub async fn apply_shell_observation(
    gcx: &Arc<GlobalContext>,
    command: &str,
    cwd: &Path,
    destination: &Destination,
    observation: ObservationStatus,
    derived_zones: &DerivedPrivacyZones,
    message: &mut ChatMessage,
) -> Result<ShellReadDecision, String> {
    if let ObservationStatus::Unavailable(reason) = &observation {
        crate::privacy::warn_observation_degraded_once(gcx.clone(), reason).await;
    }
    let scope = privacy_record_scope(gcx);
    refact_privacy::apply_shell_observation_with_scope(
        &scope,
        command,
        cwd,
        destination,
        observation,
        derived_zones,
        message,
    )
    .await
}
