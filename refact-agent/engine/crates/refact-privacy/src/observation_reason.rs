use std::sync::{Arc, RwLock};

use crate::derived::DerivedPrivacyZones;
use crate::destination::{Destination, DestinationId, DestinationKind};
use crate::policy::PolicyLoad;

pub fn provider_destination(model_id: &str) -> Destination {
    Destination {
        id: DestinationId(
            model_id
                .split_once('/')
                .map_or(model_id, |(provider, _)| provider)
                .to_string(),
        ),
        kind: DestinationKind::Provider,
        display_name: model_id.to_string(),
    }
}

/// Counts only: never carries paths, patterns or zone names, so it is safe to log.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ObservationReason {
    pub blocked_patterns: usize,
    pub guarded_zones: usize,
    pub derived_labels: usize,
}

impl ObservationReason {
    pub fn needed(&self) -> bool {
        self.blocked_patterns > 0 || self.guarded_zones > 0 || self.derived_labels > 0
    }
}

pub fn shell_observation_reason_for_policy(
    privacy_policy_load: &Arc<RwLock<PolicyLoad>>,
    destination: &Destination,
) -> ObservationReason {
    let policy = privacy_policy_load.read().unwrap().policy.clone();
    ObservationReason {
        blocked_patterns: policy.blocked.len(),
        guarded_zones: policy
            .zones
            .iter()
            .filter(|zone| !destination.matches_send_to(&zone.send_to))
            .count(),
        derived_labels: 0,
    }
}

pub fn shell_observation_reason_for_session_with_policy(
    privacy_policy_load: &Arc<RwLock<PolicyLoad>>,
    destination: &Destination,
    derived_zones: &DerivedPrivacyZones,
) -> ObservationReason {
    let mut reason = shell_observation_reason_for_policy(privacy_policy_load, destination);
    let derived_zone_names = derived_zones
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .values()
        .map(|derived| derived.zone.clone())
        .collect::<Vec<_>>();
    let policy = privacy_policy_load.read().unwrap().policy.clone();
    reason.derived_labels = derived_zone_names
        .iter()
        .filter(|name| {
            *name == "blocked"
                || policy
                    .zones
                    .iter()
                    .find(|zone| zone.name == **name)
                    .is_some_and(|zone| !destination.matches_send_to(&zone.send_to))
        })
        .count();
    reason
}

pub fn shell_observation_needed(
    privacy_policy_load: &Arc<RwLock<PolicyLoad>>,
    destination: &Destination,
) -> bool {
    shell_observation_reason_for_policy(privacy_policy_load, destination).needed()
}

pub fn shell_observation_needed_for_session(
    privacy_policy_load: &Arc<RwLock<PolicyLoad>>,
    destination: &Destination,
    derived_zones: &DerivedPrivacyZones,
) -> bool {
    shell_observation_reason_for_session_with_policy(
        privacy_policy_load,
        destination,
        derived_zones,
    )
    .needed()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::derived::{new_derived_privacy_zones, DerivedZone};
    use crate::policy::{PrivacyPolicy, ShellBehavior, SubagentPolicy, Zone};
    use std::path::PathBuf;

    fn policy_load_with_secret_zone(send_to: &[&str]) -> Arc<RwLock<PolicyLoad>> {
        Arc::new(RwLock::new(PolicyLoad {
            policy: Arc::new(PrivacyPolicy {
                blocked: Vec::new(),
                zones: vec![
                    Zone {
                        name: "secrets".to_string(),
                        patterns: vec!["secret.txt".to_string()],
                        send_to: send_to.iter().map(|value| (*value).to_string()).collect(),
                        on_shell_read: ShellBehavior::Withhold,
                    },
                    Zone {
                        name: "normal".to_string(),
                        patterns: vec!["*".to_string()],
                        send_to: vec!["*".to_string()],
                        on_shell_read: ShellBehavior::Withhold,
                    },
                ],
                subagents: SubagentPolicy::default(),
                ..Default::default()
            }),
            ..Default::default()
        }))
    }

    #[test]
    fn observation_reason_counts_match_the_boolean_decision() {
        let privacy_policy_load = policy_load_with_secret_zone(&["trusted"]);
        let allowed = provider_destination("trusted/model");
        let guarded = provider_destination("untrusted/model");
        let derived_zones = new_derived_privacy_zones();
        assert_eq!(provider_destination("a/b").id.0, "a");

        let allowed_reason =
            shell_observation_reason_for_policy(&privacy_policy_load, &allowed);
        assert_eq!(allowed_reason.blocked_patterns, 0);
        assert_eq!(allowed_reason.guarded_zones, 0);
        assert_eq!(allowed_reason.derived_labels, 0);
        assert!(!allowed_reason.needed());
        assert_eq!(
            allowed_reason.needed(),
            shell_observation_needed(&privacy_policy_load, &allowed)
        );

        let guarded_reason =
            shell_observation_reason_for_policy(&privacy_policy_load, &guarded);
        assert_eq!(guarded_reason.guarded_zones, 1);
        assert!(guarded_reason.needed());
        assert_eq!(
            guarded_reason.needed(),
            shell_observation_needed(&privacy_policy_load, &guarded)
        );

        derived_zones.write().unwrap().insert(
            PathBuf::from("/tmp/workspace/derived.txt"),
            DerivedZone {
                zone: "blocked".to_string(),
                origin: None,
            },
        );
        let session_reason = shell_observation_reason_for_session_with_policy(
            &privacy_policy_load,
            &allowed,
            &derived_zones,
        );
        assert_eq!(session_reason.guarded_zones, 0);
        assert_eq!(session_reason.derived_labels, 1);
        assert!(session_reason.needed());
        assert_eq!(
            session_reason.needed(),
            shell_observation_needed_for_session(
                &privacy_policy_load,
                &allowed,
                &derived_zones
            )
        );
    }
}
