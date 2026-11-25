use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use refact_uhp::{
    CreateResponse, Discovery, EvidenceCheck, EvidenceCheckStatus, ExternalEvidenceRecord, Harness,
    HarnessConfig, HarnessRetirement, IdempotencyKey, Model, RecoveryBinding, RequestScope,
    Response, RetiredSessions, UhpDeliveryRequestState, UhpDeliveryState, UhpError,
    UhpSessionBinding, VersionedHarness,
};
use serde_json::json;

use super::*;

const SECRET: &str = "SECRET-EXTERNAL-PARTICIPANT-CANARY";

struct MockTransport {
    calls: Mutex<Vec<&'static str>>,
    fail_discovery: AtomicBool,
    disable_management: AtomicBool,
}

impl MockTransport {
    fn new() -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            fail_discovery: AtomicBool::new(false),
            disable_management: AtomicBool::new(false),
        }
    }

    fn calls(&self) -> Vec<&'static str> {
        self.calls.lock().unwrap().clone()
    }
}

fn discovery() -> Discovery {
    serde_json::from_value(json!({
        "object": "uhp.discovery",
        "protocol": "uhp",
        "versions": ["2026-10-19"],
        "default_version": "2026-10-19",
        "conformance_class": "full",
        "implementation": {"name": "harness-router", "version": "0.41.25"},
        "capabilities": {
            "streaming": true,
            "sessions": true,
            "cancellation": true,
            "files_input": true,
            "files_output": true,
            "session_listing": true,
            "harness_management": true,
            "harness_retirement": true,
            "idempotency": true,
            "harness_idempotency": true,
            "environments": true
        }
    }))
    .unwrap()
}

fn response(id: &str, status: &str) -> Response {
    serde_json::from_value(json!({
        "id": id,
        "object": "response",
        "created_at": 20,
        "status": status,
        "output": [],
        "model": "model-1",
        "store": true,
        "metadata": {"harness_id": "chrn_1", "session_id": "session-1"}
    }))
    .unwrap()
}

fn harness(config: &HarnessConfig) -> Harness {
    Harness {
        id: "chrn_1".to_string(),
        name: config.name.clone().unwrap_or_default(),
        base: config.base.clone(),
        default_model: config.default_model.clone(),
        ..Harness::default()
    }
}

#[async_trait]
impl ExternalParticipantTransport for MockTransport {
    async fn discover(&self, _: &ExternalConnection) -> Result<Discovery, UhpError> {
        self.calls.lock().unwrap().push("discover");
        if self.fail_discovery.load(Ordering::SeqCst) {
            return Err(UhpError::InvalidResponse {
                operation: "discovery",
                message: SECRET.to_string(),
            });
        }
        let mut discovery = discovery();
        if self.disable_management.load(Ordering::SeqCst) {
            discovery.capabilities.harness_management = false;
            discovery.capabilities.harness_idempotency = false;
        }
        Ok(discovery)
    }

    async fn test_model(
        &self,
        _: &ExternalConnection,
        _: &str,
        model: &str,
    ) -> Result<Model, UhpError> {
        self.calls.lock().unwrap().push("test");
        Ok(Model {
            id: model.to_string(),
            available: true,
            ..Model::default()
        })
    }

    async fn provision(
        &self,
        _: &ExternalConnection,
        _: &Discovery,
        config: &HarnessConfig,
        _: &IdempotencyKey,
        _: &RequestScope,
    ) -> Result<Harness, UhpError> {
        self.calls.lock().unwrap().push("provision");
        Ok(harness(config))
    }

    async fn update(
        &self,
        _: &ExternalConnection,
        _: &Discovery,
        _: &str,
        config: &HarnessConfig,
        _: Option<&str>,
    ) -> Result<VersionedHarness, UhpError> {
        self.calls.lock().unwrap().push("update");
        Ok(VersionedHarness {
            harness: harness(config),
            etag: Some("revision-2".to_string()),
        })
    }

    async fn cancel(
        &self,
        _: &ExternalConnection,
        target: &ExternalCancelTarget,
    ) -> Result<Option<Response>, UhpError> {
        self.calls.lock().unwrap().push("cancel");
        match target {
            ExternalCancelTarget::Response(id) => Ok(Some(response(id, "cancelled"))),
            ExternalCancelTarget::Session(_) => Ok(None),
        }
    }

    async fn retry(
        &self,
        _: &ExternalConnection,
        _: &CreateResponse,
        _: &IdempotencyKey,
        _: &RequestScope,
        binding: &RecoveryBinding,
    ) -> Result<Response, UhpError> {
        self.calls.lock().unwrap().push("retry");
        Ok(response(
            binding.response_id.as_deref().unwrap_or("resp-recovered"),
            "completed",
        ))
    }

    async fn publish_snapshot(
        &self,
        _: &ExternalConnection,
        _: &Discovery,
        _: &str,
        _: Vec<u8>,
    ) -> Result<u64, UhpError> {
        self.calls.lock().unwrap().push("publish_snapshot");
        Ok(1)
    }

    async fn retire(
        &self,
        _: &ExternalConnection,
        _: &Discovery,
        _: &str,
    ) -> Result<HarnessRetirement, UhpError> {
        self.calls.lock().unwrap().push("retire");
        Ok(HarnessRetirement {
            deleted: true,
            sessions: RetiredSessions::Kept,
            ..HarnessRetirement::default()
        })
    }
}

fn config() -> ExternalParticipantConfig {
    ExternalParticipantConfig {
        id: "participant-1".to_string(),
        name: "Reviewer".to_string(),
        base: "opaque-base".to_string(),
        harness_id: None,
        server_root: "https://uhp.example.test".to_string(),
        model: "model-1".to_string(),
        credential_profile_ref: "credential/external-1".to_string(),
        environment_id: Some("henv_1".to_string()),
        prompt: "Review only".to_string(),
        disabled_tools: vec!["shell".to_string(), "apply_patch".to_string()],
        max_step: Some(20),
        timeout_seconds: Some(300),
        enabled: true,
        writes: false,
        cli_pin: "1.2.3".to_string(),
        server_image_digest: "sha256:0123456789abcdef".to_string(),
    }
}

#[tokio::test]
async fn external_participant_empty_registry_defaults_to_internal_agents() {
    let directory = tempfile::tempdir().unwrap();
    let registry = ExternalParticipantRegistry::with_transport(
        directory.path(),
        Arc::new(MockTransport::new()),
    );
    let snapshot = registry.snapshot().unwrap();
    assert_eq!(snapshot.mode, ExternalRegistryMode::InternalAgents);
    assert!(snapshot.participants.is_empty());
}

#[tokio::test]
async fn external_participant_crud_reloads_and_never_creates_a_local_agent() {
    let directory = tempfile::tempdir().unwrap();
    let transport = Arc::new(MockTransport::new());
    let registry = ExternalParticipantRegistry::with_transport(directory.path(), transport.clone());
    let created = registry.create(config()).await.unwrap();
    assert_eq!(created.state, ExternalParticipantState::Staged);
    assert!(!created.config.writes);
    assert_eq!(created.agent_chat_id, None);
    assert!(registry.create(config()).await.is_err());

    let mut changed = config();
    changed.name = "Second reviewer".to_string();
    registry
        .update_configuration(&created.config.id, changed)
        .await
        .unwrap();
    let reloaded = ExternalParticipantRegistry::with_transport(directory.path(), transport);
    assert_eq!(
        reloaded.participant("participant-1").unwrap().config.name,
        "Second reviewer"
    );
    assert!(reloaded.participant("missing").is_err());
    reloaded.remove("participant-1").await.unwrap();
    assert_eq!(
        reloaded.snapshot().unwrap().mode,
        ExternalRegistryMode::InternalAgents
    );
}

#[tokio::test]
async fn external_participant_writer_is_rejected_by_backend_validation() {
    let directory = tempfile::tempdir().unwrap();
    let registry = ExternalParticipantRegistry::with_transport(
        directory.path(),
        Arc::new(MockTransport::new()),
    );
    let mut writer = config();
    writer.writes = true;
    assert!(matches!(
        registry.create(writer).await,
        Err(ExternalParticipantError::ExternalWriterForbidden)
    ));
    assert!(registry.snapshot().unwrap().participants.is_empty());
}

#[tokio::test]
async fn external_participant_credentials_are_write_only_and_errors_are_sanitized() {
    let directory = tempfile::tempdir().unwrap();
    let transport = Arc::new(MockTransport::new());
    let registry = ExternalParticipantRegistry::with_transport(directory.path(), transport.clone());
    registry.create(config()).await.unwrap();
    registry
        .write_credential("participant-1", SECRET)
        .await
        .unwrap();

    let read_model = serde_json::to_string(&registry.snapshot().unwrap()).unwrap();
    assert!(!read_model.contains(SECRET));
    let persisted_registry =
        std::fs::read_to_string(directory.path().join("external-participants.json")).unwrap();
    assert!(!persisted_registry.contains(SECRET));
    assert!(
        std::fs::read_to_string(directory.path().join("secrets.yaml"))
            .unwrap()
            .contains(SECRET)
    );

    transport.fail_discovery.store(true, Ordering::SeqCst);
    let error = registry.discover("participant-1").await.unwrap_err();
    assert!(!error.to_string().contains(SECRET));
    let failed = registry.participant("participant-1").unwrap();
    assert_eq!(failed.state, ExternalParticipantState::Failed);
    assert!(!serde_json::to_string(&failed).unwrap().contains(SECRET));
}

#[tokio::test]
async fn external_participant_discovery_provision_and_update_remain_unverified_and_idempotent() {
    let directory = tempfile::tempdir().unwrap();
    let transport = Arc::new(MockTransport::new());
    let registry = ExternalParticipantRegistry::with_transport(directory.path(), transport.clone());
    let staged = registry.create(config()).await.unwrap();
    assert!(!staged.actions.provision.enabled);
    assert!(staged.actions.provision.reason.is_some());

    let discovered = registry.discover("participant-1").await.unwrap();
    assert_eq!(discovered.state, ExternalParticipantState::Unverified);
    assert!(discovered.actions.provision.enabled);
    assert!(discovered.actions.cancel.enabled);
    assert!(registry.authorize_delivery("participant-1").is_err());

    let provisioned = registry
        .provision("participant-1", "provision-participant-1")
        .await
        .unwrap();
    assert_eq!(provisioned.state, ExternalParticipantState::Unverified);
    assert_eq!(provisioned.config.harness_id.as_deref(), Some("chrn_1"));
    assert!(!provisioned.actions.provision.enabled);
    assert!(provisioned.actions.update.enabled);
    let registry = ExternalParticipantRegistry::with_transport(directory.path(), transport.clone());
    registry
        .provision("participant-1", "provision-participant-1")
        .await
        .unwrap();
    assert_eq!(
        transport
            .calls()
            .into_iter()
            .filter(|call| *call == "provision")
            .count(),
        1
    );

    assert_eq!(
        registry.test_model("participant-1").await.unwrap().state,
        ExternalParticipantState::Unverified
    );
    assert_eq!(
        registry.update_remote("participant-1").await.unwrap().state,
        ExternalParticipantState::Unverified
    );
    assert!(registry.authorize_delivery("participant-1").is_err());

    let mut changed = config();
    changed.harness_id = Some("chrn_1".to_string());
    changed.prompt = "Changed requirements".to_string();
    registry
        .update_configuration("participant-1", changed)
        .await
        .unwrap();
    assert!(matches!(
        registry
            .provision("participant-1", "provision-participant-1")
            .await,
        Err(ExternalParticipantError::ProvisionConflict)
    ));
}

#[tokio::test]
async fn external_participant_missing_capabilities_disable_actions_and_reject_provision() {
    let directory = tempfile::tempdir().unwrap();
    let transport = Arc::new(MockTransport::new());
    transport.disable_management.store(true, Ordering::SeqCst);
    let registry = ExternalParticipantRegistry::with_transport(directory.path(), transport.clone());
    registry.create(config()).await.unwrap();
    let discovered = registry.discover("participant-1").await.unwrap();
    assert!(!discovered.actions.provision.enabled);
    assert!(discovered
        .actions
        .provision
        .reason
        .as_deref()
        .unwrap()
        .contains("harness management"));
    assert!(matches!(
        registry
            .provision("participant-1", "provision-participant-1")
            .await,
        Err(ExternalParticipantError::CapabilityUnsupported {
            capability: "harness_management",
            action: "provision"
        })
    ));
    assert!(!transport.calls().contains(&"provision"));
}

fn evidence(id: &str, status: EvidenceCheckStatus) -> ExternalEvidenceRecord {
    ExternalEvidenceRecord {
        smoke_suite_id: id.to_string(),
        server_version: "0.41.25".to_string(),
        cli_version: "1.2.3".to_string(),
        model_version: "model-1".to_string(),
        checks: vec![EvidenceCheck {
            name: "live_read_only_smoke".to_string(),
            status,
        }],
        generated_at: 30,
        sanitized_report_location: format!("evidence/{id}.json"),
    }
}

#[tokio::test]
async fn external_participant_only_matching_all_pass_live_evidence_allows_read_only_delivery() {
    let directory = tempfile::tempdir().unwrap();
    let registry = ExternalParticipantRegistry::with_transport(
        directory.path(),
        Arc::new(MockTransport::new()),
    );
    registry.create(config()).await.unwrap();
    registry.discover("participant-1").await.unwrap();
    registry
        .provision("participant-1", "provision-participant-1")
        .await
        .unwrap();
    registry
        .set_environment_version("participant-1", 4)
        .await
        .unwrap();

    assert!(registry
        .record_live_evidence(
            "participant-1",
            evidence("smoke-skip", EvidenceCheckStatus::Skip)
        )
        .await
        .is_err());
    assert!(registry.authorize_delivery("participant-1").is_err());
    let verified = registry
        .record_live_evidence(
            "participant-1",
            evidence("smoke-live", EvidenceCheckStatus::Pass),
        )
        .await
        .unwrap();
    assert_eq!(verified.state, ExternalParticipantState::Verified);
    let authorization = registry.authorize_delivery("participant-1").unwrap();
    assert!(!authorization.writes);
    assert_eq!(authorization.agent_chat_id, None);
    assert_eq!(authorization.environment_version, 4);
    assert_eq!(registry.details("participant-1").unwrap().evidence.len(), 1);

    registry.discover("participant-1").await.unwrap();
    assert!(registry.authorize_delivery("participant-1").is_err());
}

fn delivery(request: &CreateResponse) -> ExternalDeliveryRecord {
    let scope = RequestScope::new("participant-1", "credential/external-1");
    ExternalDeliveryRecord {
        delivery: UhpDeliveryState {
            delivery_id: "delivery-1".to_string(),
            room_id: "room-1".to_string(),
            event_id: "event-1".to_string(),
            recipient_id: "participant-1".to_string(),
            logical_attempt: 3,
            request_fingerprint: request.fingerprint(&scope).unwrap(),
            idempotency_key: IdempotencyKey::new("delivery-key-1").unwrap(),
            request_state: UhpDeliveryRequestState::Failed,
            response_id: Some("resp-1".to_string()),
            previous_response_id: None,
            session_id: Some("session-1".to_string()),
            source_revision: Some("commit-1".to_string()),
            source_digest: Some("ab".repeat(32)),
            environment_version: Some(1),
            error_class: Some("stream_interrupted".to_string()),
            created_at: 1,
            updated_at: 2,
        },
        retrying: false,
        source_freshness: ExternalSourceFreshness::Stale,
    }
}

#[tokio::test]
async fn external_participant_retry_cancel_and_retire_preserve_sanitized_inspection() {
    let directory = tempfile::tempdir().unwrap();
    let transport = Arc::new(MockTransport::new());
    let registry = ExternalParticipantRegistry::with_transport(directory.path(), transport.clone());
    registry.create(config()).await.unwrap();
    registry.discover("participant-1").await.unwrap();
    registry
        .provision("participant-1", "provision-participant-1")
        .await
        .unwrap();
    registry
        .upsert_session(UhpSessionBinding {
            participant_id: "participant-1".to_string(),
            session_id: "session-1".to_string(),
            last_response_id: Some("resp-1".to_string()),
            last_terminal_status: None,
            created_at: 1,
            updated_at: 2,
            replay_cursor: Some(4),
        })
        .await
        .unwrap();
    let request = CreateResponse::new("public room body", "chrn_1").model("model-1");
    let record = delivery(&request);
    registry.upsert_delivery(record.clone()).await.unwrap();

    let registry = ExternalParticipantRegistry::with_transport(directory.path(), transport.clone());
    let details = registry.details("participant-1").unwrap();
    assert_eq!(
        details.deliveries[0].status,
        ExternalDeliveryStatus::Terminal
    );
    assert_eq!(details.deliveries[0].logical_attempt, 3);
    assert_eq!(
        details.deliveries[0].source_freshness,
        ExternalSourceFreshness::Stale
    );
    assert_eq!(details.sessions[0].replay_cursor, Some(4));

    let mut projection = record.clone();
    projection.delivery.request_state = UhpDeliveryRequestState::Pending;
    assert_eq!(
        ExternalDeliveryRead::from_record(&projection).status,
        ExternalDeliveryStatus::Queued
    );
    projection.delivery.request_state = UhpDeliveryRequestState::InProgress;
    assert_eq!(
        ExternalDeliveryRead::from_record(&projection).status,
        ExternalDeliveryStatus::Busy
    );
    projection.retrying = true;
    assert_eq!(
        ExternalDeliveryRead::from_record(&projection).status,
        ExternalDeliveryStatus::Retrying
    );

    let retried = registry
        .retry_delivery("participant-1", "delivery-1", &request)
        .await
        .unwrap();
    assert_eq!(retried.request_state, UhpDeliveryRequestState::Completed);
    assert_eq!(retried.logical_attempt, 3);
    assert_eq!(retried.response_id.as_deref(), Some("resp-1"));
    assert!(!serde_json::to_string(&retried)
        .unwrap()
        .contains("public room body"));

    registry
        .cancel(
            "participant-1",
            ExternalCancelTarget::Response("resp-1".to_string()),
        )
        .await
        .unwrap();
    let details = registry.details("participant-1").unwrap();
    assert_eq!(
        details.deliveries[0].request_state,
        UhpDeliveryRequestState::Cancelled
    );
    assert_eq!(
        details.sessions[0].last_terminal_status,
        Some(refact_uhp::ResponseStatus::Cancelled)
    );

    let retired = registry.retire("participant-1").await.unwrap();
    assert!(retired.deleted);
    let participant = registry.participant("participant-1").unwrap();
    assert!(participant.retired);
    assert!(!participant.config.enabled);
    assert!(registry.authorize_delivery("participant-1").is_err());
    assert_eq!(
        transport.calls(),
        vec!["discover", "provision", "retry", "cancel", "retire"]
    );
}

#[tokio::test]
async fn external_participant_retry_rejects_changed_request_fingerprint() {
    let directory = tempfile::tempdir().unwrap();
    let transport = Arc::new(MockTransport::new());
    let registry = ExternalParticipantRegistry::with_transport(directory.path(), transport.clone());
    registry.create(config()).await.unwrap();
    registry.discover("participant-1").await.unwrap();
    registry
        .provision("participant-1", "provision-participant-1")
        .await
        .unwrap();
    registry
        .upsert_session(UhpSessionBinding {
            participant_id: "participant-1".to_string(),
            session_id: "session-1".to_string(),
            last_response_id: Some("resp-1".to_string()),
            last_terminal_status: None,
            created_at: 1,
            updated_at: 2,
            replay_cursor: None,
        })
        .await
        .unwrap();
    let request = CreateResponse::new("public room body", "chrn_1").model("model-1");
    registry.upsert_delivery(delivery(&request)).await.unwrap();
    let changed = CreateResponse::new("different body", "chrn_1").model("model-1");
    assert!(registry
        .retry_delivery("participant-1", "delivery-1", &changed)
        .await
        .is_err());
    assert!(!transport.calls().contains(&"retry"));
}
