pub mod destination;
pub mod destination_clear;
pub mod derived;
pub mod gate;
pub mod message_records;
pub mod heuristic;
pub mod legacy;
pub mod matching;
pub mod model_context;
pub mod observation_reason;
pub mod policy;
pub mod record;
pub mod runtime_state;
#[cfg(feature = "test-util")]
pub mod testing;

pub use destination::{Destination, DestinationId, DestinationKind};
pub use derived::{new_derived_privacy_zones, DerivedPrivacyZones, DerivedZone};
pub use gate::{clear, records_from_messages, Cleared, PrivacyAuditError, PrivacyAudited, Refusal};
pub use legacy::{check_file_privacy, check_file_privacy_for_read_preparation};
pub use matching::{compile_patterns, CompiledPolicy, PolicyError};
pub use observation_reason::{
    provider_destination, shell_observation_needed, shell_observation_needed_for_session,
    shell_observation_reason_for_policy, shell_observation_reason_for_session_with_policy,
    ObservationReason,
};
pub use policy::{
    load_policy, merge_project, migrate_legacy, parse_policy_yaml, LegacyPrivacyPolicy, PolicyLoad,
    PrivacyPolicy, ProviderToolAccess, ShellBehavior, SubagentPolicy, ToolAccess, Zone,
};
pub use message_records::{
    attach_record, carry_records_into, merge_message_records, merge_records, records_to_carry,
};
pub use model_context::{
    apply_shell_observation_with_scope, attach_declared_output_files, declared_file_record,
    declared_file_records_with_scope, derived_zone_for_path, file_record,
    filter_path_enrichment_for_model_context_with_scope, observed_file_records, resolve_shell_ask,
    shell_ask_pending, PrivacyRecordScope, ShellReadDecision, SHELL_APPROVAL_MESSAGE,
    SHELL_WITHHELD_MESSAGE,
};
pub use record::{Attribution, FileRecord, PrivacyRecord};
pub use runtime_state::{
    observation_runtime_state, record_observation_status, PrivacyMatchCounts,
    PrivacyObservationRuntimeState,
};
