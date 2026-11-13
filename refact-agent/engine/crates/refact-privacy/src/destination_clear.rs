use std::sync::{Arc, RwLock as StdRwLock};

use crate::destination::{Destination, DestinationId, DestinationKind};
use crate::gate::{clear, Cleared, PrivacyAudited, Refusal};
use crate::policy::PolicyLoad;

pub fn clear_for_mcp<T: PrivacyAudited>(
    privacy_policy_load: &Arc<StdRwLock<PolicyLoad>>,
    value: T,
    server_name: &str,
) -> Result<Cleared<T>, Refusal> {
    let destination = Destination {
        id: DestinationId(server_name.to_string()),
        kind: DestinationKind::Mcp,
        display_name: server_name.to_string(),
    };
    let policy = privacy_policy_load
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .policy
        .clone();
    let compiled = policy.compile().map_err(|error| Refusal {
        destination: destination.clone(),
        offending: Vec::new(),
        message: format!("privacy policy failed to compile: {error}"),
    })?;
    clear(value, &destination, &compiled)
}
