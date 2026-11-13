use std::sync::{Arc, RwLock as StdRwLock};

use crate::destination::{Destination, DestinationId, DestinationKind};
use crate::gate::{clear, Cleared, PrivacyAudited, Refusal};
use crate::policy::PolicyLoad;
use refact_core::llm_types::{BaseModelRecord, EmbeddingModelRecord};

pub trait DestinationExt<T> {
    fn from_model_record(model_rec: &T) -> Destination;
}

impl DestinationExt<BaseModelRecord> for Destination {
    fn from_model_record(model_rec: &BaseModelRecord) -> Destination {
        Destination {
            id: DestinationId(
                model_rec
                    .id
                    .split_once('/')
                    .map_or(model_rec.id.as_str(), |(provider, _)| provider)
                    .to_string(),
            ),
            kind: DestinationKind::Provider,
            display_name: model_rec.id.clone(),
        }
    }
}

impl DestinationExt<EmbeddingModelRecord> for Destination {
    fn from_model_record(model_rec: &EmbeddingModelRecord) -> Destination {
        Destination::from_model_record(&model_rec.base)
    }
}

pub fn clear_for_model<T: PrivacyAudited>(
    privacy_policy_load: &Arc<StdRwLock<PolicyLoad>>,
    value: T,
    model_rec: &BaseModelRecord,
) -> Result<Cleared<T>, Refusal> {
    let destination = Destination::from_model_record(model_rec);
    clear_for_destination(privacy_policy_load, value, destination)
}

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
    clear_for_destination(privacy_policy_load, value, destination)
}

fn clear_for_destination<T: PrivacyAudited>(
    privacy_policy_load: &Arc<StdRwLock<PolicyLoad>>,
    value: T,
    destination: Destination,
) -> Result<Cleared<T>, Refusal> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{PrivacyPolicy, ShellBehavior, SubagentPolicy, Zone};

    struct EmptyAudit;

    impl PrivacyAudited for EmptyAudit {
        fn privacy_records(
            &self,
        ) -> Result<Vec<(usize, crate::FileRecord)>, crate::PrivacyAuditError> {
            Ok(Vec::new())
        }
    }

    #[test]
    fn model_destination_uses_provider_prefix() {
        let model = BaseModelRecord {
            id: "trusted/model".to_string(),
            ..Default::default()
        };

        let destination = Destination::from_model_record(&model);

        assert_eq!(destination.id.0, "trusted");
        assert_eq!(destination.kind, DestinationKind::Provider);
        assert_eq!(destination.display_name, "trusted/model");
    }

    #[test]
    fn embedding_destination_uses_provider_prefix() {
        let model = EmbeddingModelRecord {
            base: BaseModelRecord {
                id: "embeddings/text-v1".to_string(),
                ..Default::default()
            },
            ..Default::default()
        };

        let destination = Destination::from_model_record(&model);

        assert_eq!(destination.id.0, "embeddings");
        assert_eq!(destination.kind, DestinationKind::Provider);
        assert_eq!(destination.display_name, "embeddings/text-v1");
    }

    #[test]
    fn poisoned_policy_lock_with_invalid_state_fails_closed() {
        let privacy_policy_load: Arc<StdRwLock<PolicyLoad>> =
            Arc::new(StdRwLock::new(PolicyLoad::default()));
        let poison = privacy_policy_load.clone();
        let _ = std::thread::spawn(move || {
            let mut load = poison.write().unwrap();
            load.policy = std::sync::Arc::new(PrivacyPolicy {
                blocked: Vec::new(),
                zones: vec![Zone {
                    name: "normal".to_string(),
                    patterns: vec!["[".to_string()],
                    send_to: vec!["*".to_string()],
                    on_shell_read: ShellBehavior::Withhold,
                }],
                subagents: SubagentPolicy::default(),
                ..Default::default()
            });
            panic!("poison privacy policy lock");
        })
        .join();
        let model = BaseModelRecord {
            id: "trusted/model".to_string(),
            ..Default::default()
        };

        let refusal = match clear_for_model(&privacy_policy_load, EmptyAudit, &model) {
            Ok(_) => panic!("invalid recovered policy must fail closed"),
            Err(refusal) => refusal,
        };

        assert!(refusal.offending.is_empty());
        assert!(refusal.message.contains("privacy policy failed to compile"));
    }
}
