use std::sync::{Arc, RwLock};

use refact_exec::ObservationStatus;

#[derive(Default)]
pub struct PrivacyMatchCounts {
    pub counts: std::collections::BTreeMap<String, usize>,
    pub fingerprint: u64,
    pub computed_at: Option<std::time::Instant>,
    pub computing: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PrivacyObservationRuntimeState {
    pub runtime_available: bool,
    pub last_error: Option<String>,
}

pub fn record_observation_status(
    privacy_observation_runtime: &Arc<RwLock<PrivacyObservationRuntimeState>>,
    status: &ObservationStatus,
) {
    *privacy_observation_runtime.write().unwrap() = observation_runtime_state(status);
}

pub fn observation_runtime_state(status: &ObservationStatus) -> PrivacyObservationRuntimeState {
    match status {
        ObservationStatus::Unavailable(reason) => PrivacyObservationRuntimeState {
            runtime_available: false,
            last_error: Some(reason.clone()),
        },
        ObservationStatus::Observed(_)
        | ObservationStatus::Pending(_)
        | ObservationStatus::Incomplete(_) => PrivacyObservationRuntimeState {
            runtime_available: true,
            last_error: None,
        },
    }
}
