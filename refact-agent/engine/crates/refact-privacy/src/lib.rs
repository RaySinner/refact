pub mod destination;
pub mod gate;
pub mod heuristic;
pub mod matching;
pub mod policy;
pub mod record;
pub mod runtime_state;
#[cfg(feature = "test-util")]
pub mod testing;

pub use destination::{Destination, DestinationId, DestinationKind};
pub use gate::{clear, records_from_messages, Cleared, PrivacyAuditError, PrivacyAudited, Refusal};
pub use matching::{compile_patterns, CompiledPolicy, PolicyError};
pub use policy::{
    load_policy, merge_project, migrate_legacy, parse_policy_yaml, LegacyPrivacyPolicy, PolicyLoad,
    PrivacyPolicy, ProviderToolAccess, ShellBehavior, SubagentPolicy, ToolAccess, Zone,
};
pub use record::{Attribution, FileRecord, PrivacyRecord};
pub use runtime_state::{
    observation_runtime_state, record_observation_status, PrivacyMatchCounts,
    PrivacyObservationRuntimeState,
};
