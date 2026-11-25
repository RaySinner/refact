use serde::{Deserialize, Serialize};

use crate::supervision::{
    EvidenceId, InterventionCandidate, InterventionDelivery, InterventionRecord,
    InterventionVerdict, MessageBody, SupervisionProjection, SuppressionReason, MAX_EVIDENCE_IDS,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SupervisionArbiterReply {
    pub verdict: InterventionVerdict,
    pub evidence_event_ids: Vec<EvidenceId>,
    pub confidence: u8,
    pub suggested_action: MessageBody,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedSupervisionAdvice {
    pub verdict: InterventionVerdict,
    pub evidence_event_ids: Vec<EvidenceId>,
    pub confidence: u8,
    pub suggested_action: MessageBody,
}

pub fn validate_arbiter_reply(
    candidate: &InterventionRecord,
    projection: &SupervisionProjection,
    reply: SupervisionArbiterReply,
    min_confidence: u8,
) -> Result<ValidatedSupervisionAdvice, String> {
    if reply.confidence > 100 || min_confidence > 100 {
        return Err("confidence must be between 0 and 100".to_string());
    }
    if reply.evidence_event_ids.is_empty() || reply.evidence_event_ids.len() > MAX_EVIDENCE_IDS {
        return Err("arbiter evidence_event_ids must be non-empty and bounded".to_string());
    }
    let mut ids = reply.evidence_event_ids.clone();
    ids.sort();
    ids.dedup();
    if ids.len() != reply.evidence_event_ids.len() {
        return Err("arbiter evidence_event_ids contain duplicates".to_string());
    }
    for id in &ids {
        if !candidate.evidence.ids().contains(id) {
            return Err("arbiter cited evidence outside the candidate".to_string());
        }
        if !projection
            .detection_events()
            .any(|event| event.seq == id.seq && &event.chat_id == &id.chat_id)
        {
            return Err("arbiter cited evidence outside the projection".to_string());
        }
    }
    if reply.verdict == InterventionVerdict::Confirmed {
        if reply.confidence < min_confidence {
            return Err("confirmed advice is below the confidence threshold".to_string());
        }
        if reply.suggested_action.as_str().trim().is_empty() {
            return Err("confirmed advice requires an actionable next step".to_string());
        }
    }
    Ok(ValidatedSupervisionAdvice {
        verdict: reply.verdict,
        evidence_event_ids: ids,
        confidence: reply.confidence,
        suggested_action: reply.suggested_action,
    })
}

pub fn off_goal_needs_confirmation(
    records: &[InterventionRecord],
    candidate: &InterventionRecord,
) -> bool {
    let Some(key) = candidate.policy_key.as_ref() else {
        return true;
    };
    if key.trigger != InterventionCandidate::OffGoal {
        return false;
    }
    !records.iter().any(|record| {
        let Some(previous) = record.policy_key.as_ref() else {
            return false;
        };
        previous.chat_id == key.chat_id
            && previous.goal_revision == key.goal_revision
            && previous.trigger == InterventionCandidate::OffGoal
            && previous.evidence_hash != key.evidence_hash
            && record.detected_at <= candidate.detected_at
            && record.verdict == InterventionVerdict::Confirmed
            && matches!(
                record.delivery,
                InterventionDelivery::Suppressed {
                    reason: SuppressionReason::NeedsConfirmation
                }
            )
    })
}

pub fn delivery_budget_allows(
    records: &[InterventionRecord],
    chat_id: &str,
    now: u64,
    min_interval_secs: u64,
    max_per_hour: u32,
) -> bool {
    let mut sent_at = records
        .iter()
        .filter(|record| {
            record
                .policy_key
                .as_ref()
                .is_some_and(|key| key.chat_id.as_str() == chat_id)
                && matches!(record.delivery, InterventionDelivery::Sent { .. })
        })
        .filter_map(|record| match &record.delivery {
            InterventionDelivery::Sent { sent_at, .. } => Some(*sent_at),
            InterventionDelivery::Suppressed { .. } => None,
        })
        .collect::<Vec<_>>();
    sent_at.sort_unstable();
    let hourly = sent_at
        .iter()
        .filter(|at| now.saturating_sub(**at) < 3600)
        .count();
    if hourly >= max_per_hour as usize {
        return false;
    }
    sent_at
        .last()
        .is_none_or(|last| now.saturating_sub(*last) >= min_interval_secs)
}

pub fn trigger_label(candidate: InterventionCandidate) -> &'static str {
    match candidate {
        InterventionCandidate::StalledTurn => "stalled turn",
        InterventionCandidate::Loop => "repeated tool loop",
        InterventionCandidate::Stagnation => "stagnation",
        InterventionCandidate::OffGoal => "possible off-goal work",
        InterventionCandidate::ProviderDisconnect => "terminal provider disconnect",
    }
}

pub fn format_advice(
    candidate: &InterventionRecord,
    advice: &ValidatedSupervisionAdvice,
) -> String {
    let evidence = advice
        .evidence_event_ids
        .iter()
        .map(|id| id.seq.to_string())
        .collect::<Vec<_>>()
        .join(", ");
    MessageBody::new(&format!(
        "Buddy supervision: {}.\nEvidence events: {}.\nNext step: {}",
        trigger_label(candidate.candidate),
        evidence,
        advice.suggested_action.as_str()
    ))
    .as_str()
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::supervision::{
        GoalFacts, InterventionDelivery, MessageRole, SupervisionEvidence, SupervisionEvent,
        SupervisionHash, SupervisionKind, SupervisionPolicyKey, SupervisionReference,
        SupervisionVersion,
    };

    fn fixture() -> (
        SupervisionProjection,
        InterventionRecord,
        SupervisionArbiterReply,
    ) {
        let goal = SupervisionEvent::new(
            1,
            "chat-1",
            100,
            SupervisionKind::Goal {
                facts: Some(GoalFacts {
                    revision: 4,
                    active: true,
                    autonomy: true,
                    prompt_enabled: true,
                    can_pursue: true,
                }),
            },
        );
        let message = SupervisionEvent::new(
            2,
            "chat-1",
            101,
            SupervisionKind::Message {
                role: MessageRole::Assistant,
                message_id: SupervisionReference::new("m2"),
                body: Some(MessageBody::new("unrelated work")),
            },
        );
        let mut projection = SupervisionProjection::new("chat-1");
        projection.append(goal.clone()).unwrap();
        projection.append(message.clone()).unwrap();
        let candidate = InterventionRecord {
            schema_version: SupervisionVersion,
            candidate: InterventionCandidate::OffGoal,
            evidence: SupervisionEvidence::from_events(&[goal, message]).unwrap(),
            detector_version: SupervisionReference::new("test.v1"),
            policy_key: None,
            verdict: InterventionVerdict::Inconclusive,
            delivery: InterventionDelivery::Suppressed {
                reason: SuppressionReason::ShadowMode,
            },
            detected_at: 102,
            decided_at: 102,
        };
        let reply = SupervisionArbiterReply {
            verdict: InterventionVerdict::Confirmed,
            evidence_event_ids: candidate.evidence.ids().to_vec(),
            confidence: 90,
            suggested_action: MessageBody::new("Return to the goal and inspect the failing test."),
        };
        (projection, candidate, reply)
    }

    #[test]
    fn structured_verdict_requires_known_evidence_confidence_and_action() {
        let (projection, candidate, reply) = fixture();
        let valid = validate_arbiter_reply(&candidate, &projection, reply.clone(), 80).unwrap();
        assert_eq!(valid.evidence_event_ids.len(), 2);
        assert!(format_advice(&candidate, &valid).contains("Next step:"));

        let mut missing = reply.clone();
        missing.evidence_event_ids.clear();
        assert!(validate_arbiter_reply(&candidate, &projection, missing, 80).is_err());
        let mut foreign = reply.clone();
        foreign.evidence_event_ids[0].seq = 99;
        assert!(validate_arbiter_reply(&candidate, &projection, foreign, 80).is_err());
        let mut vague = reply.clone();
        vague.suggested_action = MessageBody::new(" ");
        assert!(validate_arbiter_reply(&candidate, &projection, vague, 80).is_err());
        let mut weak = reply;
        weak.confidence = 79;
        assert!(validate_arbiter_reply(&candidate, &projection, weak, 80).is_err());
    }

    #[test]
    fn verdict_json_is_strict_and_redacted() {
        let (projection, candidate, _) = fixture();
        let malformed = r#"{"verdict":"confirmed","evidence_event_ids":[],"confidence":90,"suggested_action":"x","reasoning":"private"}"#;
        assert!(serde_json::from_str::<SupervisionArbiterReply>(malformed).is_err());
        let json = serde_json::to_string(&candidate).unwrap();
        assert!(json.contains("off_goal"));
        let (_, _, reply) = fixture();
        let serialized = serde_json::to_string(&reply).unwrap();
        assert!(!serialized.contains("reasoning"));
        assert!(validate_arbiter_reply(&candidate, &projection, reply, 80).is_ok());
    }

    #[test]
    fn off_goal_requires_new_evidence_at_the_same_goal_revision() {
        let (_, mut first, _) = fixture();
        first.policy_key = Some(SupervisionPolicyKey::new("chat-1", 4, &first));
        first.verdict = InterventionVerdict::Confirmed;
        first.delivery = InterventionDelivery::Suppressed {
            reason: SuppressionReason::NeedsConfirmation,
        };
        let mut second = first.clone();
        second.policy_key.as_mut().unwrap().evidence_hash =
            SupervisionHash::normalized_result(&serde_json::json!("new evidence"));
        assert!(!off_goal_needs_confirmation(&[first.clone()], &second));
        let mut changed_goal = second.clone();
        changed_goal.policy_key.as_mut().unwrap().goal_revision = 5;
        assert!(off_goal_needs_confirmation(&[first.clone()], &changed_goal));
        assert!(off_goal_needs_confirmation(&[], &second));
        assert!(off_goal_needs_confirmation(&[], &second));
    }

    #[test]
    fn sent_budget_survives_journal_replay() {
        let (_, mut record, _) = fixture();
        record.policy_key = Some(SupervisionPolicyKey::new("chat-1", 4, &record));
        record.delivery = InterventionDelivery::Sent {
            delivery_id: SupervisionReference::new("delivery-1"),
            message_id: SupervisionReference::new("notice-1"),
            sent_at: 1_000,
        };
        assert!(!delivery_budget_allows(
            &[record.clone()],
            "chat-1",
            1_100,
            300,
            6
        ));
        assert!(delivery_budget_allows(
            &[record.clone()],
            "chat-1",
            1_300,
            300,
            6
        ));
        assert!(!delivery_budget_allows(&[record], "chat-1", 4_700, 0, 1));
    }
}
