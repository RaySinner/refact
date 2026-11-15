use std::sync::Arc;

use crate::global_context::GlobalContext;

pub mod destinations;
mod gcx_wrappers;
pub mod heuristic;
pub mod records;

pub use refact_core::privacy_types::{FilePrivacyLevel, FilePrivacySettings, PrivacySettings};
pub use refact_privacy::runtime_state::{
    observation_runtime_state, record_observation_status, PrivacyMatchCounts,
    PrivacyObservationRuntimeState,
};
pub use refact_privacy::{check_file_privacy, PolicyLoad, PrivacyPolicy};
pub(crate) use refact_privacy::check_file_privacy_for_read_preparation;
pub use refact_runtime_api::privacy::{
    global_only_policy, load_privacy_if_needed, load_privacy_if_needed_for_policy,
    save_privacy_policy,
};

pub async fn warn_observation_degraded_once(gcx: Arc<GlobalContext>, reason: &str) {
    crate::buddy::observers::privacy_degraded::warn_once(gcx, reason).await;
}
