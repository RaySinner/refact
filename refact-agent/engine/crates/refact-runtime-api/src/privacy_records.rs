use std::path::Path;
use std::sync::Arc;

use refact_exec::ObservationStatus;
use refact_privacy::model_context::ShellReadDecision;
use refact_privacy::{DerivedPrivacyZones, Destination};

use refact_chat_api::ChatMessage;

use crate::global_context::GlobalContext;

pub use crate::gcx_helpers::{
    declared_file_records, filter_path_enrichment_for_model_context, privacy_record_scope,
    tool_enrichment_from_path_references,
};

pub use refact_privacy::model_context::{
    apply_shell_observation_with_scope, attach_declared_output_files, declared_file_record,
    declared_file_records_with_scope, derived_zone_for_path, file_record,
    filter_path_enrichment_for_model_context_with_scope, observed_file_records, resolve_shell_ask,
    shell_ask_pending, PrivacyRecordScope, SHELL_APPROVAL_MESSAGE, SHELL_WITHHELD_MESSAGE,
};

pub use refact_privacy::observation_reason::{
    provider_destination, shell_observation_needed, shell_observation_needed_for_session,
    shell_observation_reason_for_policy, shell_observation_reason_for_session_with_policy,
    ObservationReason,
};

pub use refact_privacy::message_records::{
    attach_record, carry_records_into, merge_message_records, merge_records, records_to_carry,
};

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
        warn_observation_degraded_once(gcx.clone(), reason).await;
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

pub async fn warn_observation_degraded_once(gcx: Arc<GlobalContext>, reason: &str) {
    crate::buddy_privacy_degraded::warn_once(gcx, reason).await;
}
