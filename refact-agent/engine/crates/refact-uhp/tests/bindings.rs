use refact_uhp::{
    Capabilities, EvidenceCheck, EvidenceCheckStatus, ExternalEvidenceRecord,
    ExternalParticipantBinding, HarnessConfig, IdempotencyKey, RequestScope, ResponseStatus,
    UhpBindingRecords, UhpDeliveryRequestState, UhpDeliveryState, UhpError, UhpSessionBinding,
    UHP_VERSION,
};

fn participant() -> ExternalParticipantBinding {
    ExternalParticipantBinding {
        participant_id: "participant-1".to_string(),
        server_root: "https://uhp.example.test".to_string(),
        server_implementation: "harness-router".to_string(),
        server_version: "0.41.25".to_string(),
        image_digest: "sha256:0123456789abcdef".to_string(),
        harness_id: "chrn_1".to_string(),
        base: "opaque-base".to_string(),
        cli_pin: "1.2.3".to_string(),
        model: "model-1".to_string(),
        credential_profile_ref: "credential-profile-1".to_string(),
        environment_id: "henv_1".to_string(),
        environment_version: Some(1),
        protocol_version: UHP_VERSION.to_string(),
        capability_snapshot: Capabilities {
            environments: true,
            ..Capabilities::default()
        },
    }
}

fn bindings() -> UhpBindingRecords {
    let participant = participant();
    let fingerprint = HarnessConfig::new("opaque-base")
        .fingerprint(&RequestScope::new("participant-1", "credential-profile-1"))
        .unwrap();
    let mut records = UhpBindingRecords::new();
    records
        .participants
        .insert(participant.participant_id.clone(), participant);
    records.sessions.insert(
        "session-1".to_string(),
        UhpSessionBinding {
            participant_id: "participant-1".to_string(),
            session_id: "session-1".to_string(),
            last_response_id: Some("response-1".to_string()),
            last_terminal_status: Some(ResponseStatus::Completed),
            created_at: 1,
            updated_at: 2,
            replay_cursor: Some(10),
        },
    );
    records.deliveries.insert(
        "delivery-1".to_string(),
        UhpDeliveryState {
            delivery_id: "delivery-1".to_string(),
            room_id: "room-1".to_string(),
            event_id: "event-1".to_string(),
            recipient_id: "participant-1".to_string(),
            logical_attempt: 0,
            request_fingerprint: fingerprint,
            idempotency_key: IdempotencyKey::new("delivery-key-1").unwrap(),
            request_state: UhpDeliveryRequestState::Completed,
            response_id: Some("response-1".to_string()),
            session_id: Some("session-1".to_string()),
            source_revision: Some("commit-1".to_string()),
            source_digest: Some("ab".repeat(32)),
            environment_version: Some(1),
            error_class: None,
            created_at: 1,
            updated_at: 2,
        },
    );
    records.evidence.insert(
        "smoke-1".to_string(),
        ExternalEvidenceRecord {
            smoke_suite_id: "smoke-1".to_string(),
            server_version: "0.41.25".to_string(),
            cli_version: "1.2.3".to_string(),
            model_version: "model-1".to_string(),
            checks: vec![EvidenceCheck {
                name: "snapshot freshness".to_string(),
                status: EvidenceCheckStatus::Pass,
            }],
            generated_at: 3,
            sanitized_report_location: "evidence/smoke-1.json".to_string(),
        },
    );
    records
}

#[test]
fn bindings_round_trip_preserves_snapshot_and_capability_evidence() {
    let records = bindings();
    records.validate().unwrap();
    let encoded = serde_json::to_vec(&records).unwrap();
    let decoded: UhpBindingRecords = serde_json::from_slice(&encoded).unwrap();
    decoded.validate().unwrap();
    assert_eq!(decoded, records);
    assert_eq!(
        decoded.deliveries["delivery-1"].source_digest.as_deref(),
        Some("ab".repeat(32).as_str())
    );
}

#[test]
fn bindings_refuse_unknown_environment_capability() {
    let mut participant = participant();
    participant.capability_snapshot.environments = false;
    assert!(matches!(
        participant.validate(),
        Err(UhpError::CapabilityUnsupported {
            capability: "environments",
            ..
        })
    ));
}

#[test]
fn bindings_require_source_identity_for_external_delivery() {
    let mut records = bindings();
    records
        .deliveries
        .get_mut("delivery-1")
        .unwrap()
        .source_digest = None;
    assert!(matches!(
        records.validate(),
        Err(UhpError::InvalidBinding(message)) if message.contains("source_digest")
    ));
}

#[test]
fn bindings_reject_cross_participant_session_recovery() {
    let mut records = bindings();
    records
        .sessions
        .get_mut("session-1")
        .unwrap()
        .participant_id = "participant-2".to_string();
    assert!(records.validate().is_err());
}

#[test]
fn bindings_reject_raw_errors_and_reused_delivery_versions() {
    let mut records = bindings();
    records
        .deliveries
        .get_mut("delivery-1")
        .unwrap()
        .error_class = Some("provider failed with token=secret".to_string());
    assert!(records.validate().is_err());

    let mut records = bindings();
    let mut second = records.deliveries["delivery-1"].clone();
    second.delivery_id = "delivery-2".to_string();
    second.idempotency_key = IdempotencyKey::new("delivery-key-2").unwrap();
    records
        .deliveries
        .insert(second.delivery_id.clone(), second);
    assert!(records.validate().is_err());
}

#[test]
fn bindings_reject_credentials_in_server_url() {
    let mut participant = participant();
    participant.server_root = "https://user:password@uhp.example.test".to_string();
    assert!(participant.validate().is_err());
}
