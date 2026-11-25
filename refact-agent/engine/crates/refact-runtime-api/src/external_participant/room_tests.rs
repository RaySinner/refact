use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use refact_room::{DeliveryAttempt, PublicRoomEvent, SourceFile, SourceSnapshot};
use refact_uhp::{
    CreateResponse, Discovery, EvidenceCheck, EvidenceCheckStatus, ExternalEvidenceRecord, Harness,
    HarnessConfig, HarnessRetirement, IdempotencyKey, Model, RecoveryBinding, RequestScope,
    Response, ResponseStatus, UhpError, UhpErrorObject, VersionedHarness,
};
use serde_json::{json, Value};

use super::*;
use crate::room_snapshot_export::ExportedSourceSnapshot;

#[derive(Clone)]
struct Call {
    server_root: String,
    request: Value,
    key: String,
    binding: RecoveryBinding,
}

enum Script {
    Response(Response),
    Error(&'static str),
}

struct RoomTransport {
    calls: Mutex<Vec<Call>>,
    scripts: Mutex<VecDeque<Script>>,
    environment_version: AtomicU64,
}

impl RoomTransport {
    fn new() -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            scripts: Mutex::new(VecDeque::new()),
            environment_version: AtomicU64::new(1),
        }
    }

    fn script(&self, script: Script) {
        self.scripts.lock().unwrap().push_back(script);
    }

    fn calls_for(&self, server_root: &str) -> Vec<Call> {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|call| call.server_root == server_root)
            .cloned()
            .collect()
    }
}

#[async_trait]
impl ExternalParticipantTransport for RoomTransport {
    async fn discover(&self, _: &ExternalConnection) -> Result<Discovery, UhpError> {
        Ok(discovery())
    }

    async fn test_model(
        &self,
        _: &ExternalConnection,
        _: &str,
        model: &str,
    ) -> Result<Model, UhpError> {
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
        Ok(Harness {
            id: "chrn_1".to_string(),
            base: config.base.clone(),
            ..Harness::default()
        })
    }

    async fn update(
        &self,
        _: &ExternalConnection,
        _: &Discovery,
        _: &str,
        _: &HarnessConfig,
        _: Option<&str>,
    ) -> Result<VersionedHarness, UhpError> {
        unreachable!()
    }

    async fn cancel(
        &self,
        _: &ExternalConnection,
        _: &ExternalCancelTarget,
    ) -> Result<Option<Response>, UhpError> {
        unreachable!()
    }

    async fn retry(
        &self,
        connection: &ExternalConnection,
        request: &CreateResponse,
        key: &IdempotencyKey,
        _: &RequestScope,
        binding: &RecoveryBinding,
    ) -> Result<Response, UhpError> {
        let request = serde_json::to_value(request).unwrap();
        self.calls.lock().unwrap().push(Call {
            server_root: connection.server_root.clone(),
            request: request.clone(),
            key: key.as_str().to_string(),
            binding: binding.clone(),
        });
        match self.scripts.lock().unwrap().pop_front() {
            Some(Script::Response(response)) => Ok(response),
            Some(Script::Error(code)) => Err(UhpError::Server {
                status: 409,
                error: UhpErrorObject {
                    code: code.to_string(),
                    ..UhpErrorObject::default()
                },
            }),
            None => {
                let server = connection.server_root.rsplit('/').next().unwrap();
                let id = binding.response_id.clone().unwrap_or_else(|| {
                    format!(
                        "resp-{server}-{}",
                        self.calls_for(&connection.server_root).len()
                    )
                });
                Ok(response(
                    &id,
                    "session-shared",
                    request.get("previous_response_id").and_then(Value::as_str),
                    ResponseStatus::Completed,
                ))
            }
        }
    }

    async fn publish_snapshot(
        &self,
        _: &ExternalConnection,
        _: &Discovery,
        _: &str,
        _: Vec<u8>,
    ) -> Result<u64, UhpError> {
        Ok(self.environment_version.fetch_add(1, Ordering::SeqCst) + 1)
    }

    async fn retire(
        &self,
        _: &ExternalConnection,
        _: &Discovery,
        _: &str,
    ) -> Result<HarnessRetirement, UhpError> {
        unreachable!()
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
            "idempotency": true,
            "harness_idempotency": true,
            "environments": true
        }
    }))
    .unwrap()
}

fn response(
    id: &str,
    session_id: &str,
    previous_response_id: Option<&str>,
    status: ResponseStatus,
) -> Response {
    serde_json::from_value(json!({
        "id": id,
        "object": "response",
        "created_at": 20,
        "status": status,
        "previous_response_id": previous_response_id,
        "output": [
            {
                "type": "reasoning",
                "id": "private-reasoning",
                "summary": [{"type": "summary_text", "text": "PRIVATE-REASONING"}]
            },
            {
                "type": "function_call",
                "id": "private-tool",
                "arguments": "PRIVATE-TOOL-ARGUMENTS",
                "artifact_url": "https://principal-only.example/artifact"
            },
            {
                "type": "message",
                "id": format!("message-{id}"),
                "role": "assistant",
                "status": "completed",
                "content": [{"type": "output_text", "text": "PUBLIC-ADVISOR-REVIEW"}]
            }
        ],
        "model": "model-1",
        "store": true,
        "metadata": {"harness_id": "chrn_1", "session_id": session_id}
    }))
    .unwrap()
}

async fn add_verified(registry: &ExternalParticipantRegistry, id: &str) {
    registry
        .create(ExternalParticipantConfig {
            id: id.to_string(),
            name: id.to_string(),
            base: "opaque-base".to_string(),
            harness_id: None,
            server_root: format!("https://uhp.example.test/{id}"),
            model: "model-1".to_string(),
            credential_profile_ref: format!("credential/{id}"),
            environment_id: Some(format!("henv-{id}")),
            prompt: "Review only".to_string(),
            disabled_tools: vec!["shell".to_string(), "apply_patch".to_string()],
            max_step: Some(10),
            timeout_seconds: Some(60),
            enabled: true,
            writes: false,
            cli_pin: "1.2.3".to_string(),
            server_image_digest: "sha256:0123456789abcdef".to_string(),
        })
        .await
        .unwrap();
    registry.discover(id).await.unwrap();
    registry
        .provision(id, &format!("provision-{id}"))
        .await
        .unwrap();
    registry.set_environment_version(id, 1).await.unwrap();
    registry
        .record_live_evidence(
            id,
            ExternalEvidenceRecord {
                smoke_suite_id: format!("smoke-{id}"),
                server_version: "0.41.25".to_string(),
                cli_version: "1.2.3".to_string(),
                model_version: "model-1".to_string(),
                checks: vec![EvidenceCheck {
                    name: "live_read_only".to_string(),
                    status: EvidenceCheckStatus::Pass,
                }],
                generated_at: 1,
                sanitized_report_location: format!("evidence/{id}.json"),
            },
        )
        .await
        .unwrap();
}

fn exported() -> ExportedSourceSnapshot {
    ExportedSourceSnapshot {
        snapshot: SourceSnapshot::new(
            "commit-1",
            vec![SourceFile::new(
                "src/lib.rs",
                b"pub fn writer() {}\n".to_vec(),
            )],
        )
        .unwrap(),
        archive: vec![1, 2, 3],
    }
}

fn event(id: &str, seq: u64) -> PublicRoomEvent {
    PublicRoomEvent {
        room_id: "room-1".to_string(),
        room_seq: seq,
        event_id: id.to_string(),
        author: refact_room::RoomAuthor {
            participant_id: "writer".to_string(),
            role: "coder".to_string(),
            display_name: "Writer".to_string(),
        },
        body: "PUBLIC-ROOM-BODY".to_string(),
        ..Default::default()
    }
}

fn attempt(event_id: &str, recipient_id: &str) -> DeliveryAttempt {
    DeliveryAttempt::new("room-1", event_id, recipient_id)
}

#[tokio::test]
async fn room_uhp_two_advisors_have_independent_chains_and_public_only_idempotent_responses() {
    let directory = tempfile::tempdir().unwrap();
    let transport = Arc::new(RoomTransport::new());
    let registry = ExternalParticipantRegistry::with_transport(directory.path(), transport.clone());
    add_verified(&registry, "advisor-a").await;
    add_verified(&registry, "advisor-b").await;

    for participant in ["advisor-a", "advisor-b"] {
        let completed = registry
            .deliver_room_event(
                &event("event-1", 1),
                &attempt("event-1", participant),
                &exported(),
            )
            .await
            .unwrap();
        assert_eq!(completed.completed_bodies, ["PUBLIC-ADVISOR-REVIEW"]);
    }
    for participant in ["advisor-a", "advisor-b"] {
        registry
            .deliver_room_event(
                &event("event-2", 2),
                &attempt("event-2", participant),
                &exported(),
            )
            .await
            .unwrap();
    }
    let a = transport.calls_for("https://uhp.example.test/advisor-a");
    let b = transport.calls_for("https://uhp.example.test/advisor-b");
    assert_eq!(a[1].request["previous_response_id"], "resp-advisor-a-1");
    assert_eq!(b[1].request["previous_response_id"], "resp-advisor-b-1");
    for call in a.iter().chain(&b) {
        let input = call.request["input"].as_str().unwrap();
        assert!(input.contains("PUBLIC-ROOM-BODY"));
        assert!(!input.contains("PRIVATE-REASONING"));
        assert!(!input.contains("PRIVATE-TOOL-ARGUMENTS"));
        assert!(!input.contains("principal-only.example"));
    }
    let before = transport.calls.lock().unwrap().len();
    let duplicate = registry
        .deliver_room_event(
            &event("event-1", 1),
            &attempt("event-1", "advisor-a"),
            &exported(),
        )
        .await
        .unwrap();
    assert_eq!(duplicate.completed_bodies, ["PUBLIC-ADVISOR-REVIEW"]);
    assert_eq!(transport.calls.lock().unwrap().len(), before + 1);
    let duplicate_call = transport.calls_for("https://uhp.example.test/advisor-a");
    assert_eq!(duplicate_call[0].key, duplicate_call[2].key);
    assert!(duplicate_call[2].binding.response_id.is_some());
    let a_details = registry.details("advisor-a").unwrap();
    let b_details = registry.details("advisor-b").unwrap();
    assert_eq!(a_details.sessions.len(), 1);
    assert_eq!(b_details.sessions.len(), 1);
    assert_eq!(a_details.sessions[0].session_id, "session-shared");
    assert_eq!(b_details.sessions[0].session_id, "session-shared");
    let persisted =
        std::fs::read_to_string(directory.path().join("external-participants.json")).unwrap();
    for forbidden in [
        "PRIVATE-REASONING",
        "PRIVATE-TOOL-ARGUMENTS",
        "principal-only.example",
        "PUBLIC-ADVISOR-REVIEW",
        "PUBLIC-ROOM-BODY",
    ] {
        assert!(!persisted.contains(forbidden));
    }
}

#[tokio::test]
async fn room_uhp_busy_restart_get_recovery_and_session_expiry_are_durable() {
    let directory = tempfile::tempdir().unwrap();
    let transport = Arc::new(RoomTransport::new());
    let registry = ExternalParticipantRegistry::with_transport(directory.path(), transport.clone());
    add_verified(&registry, "advisor-a").await;

    transport.script(Script::Error("session_busy"));
    let busy = registry
        .deliver_room_event(
            &event("event-1", 1),
            &attempt("event-1", "advisor-a"),
            &exported(),
        )
        .await
        .unwrap();
    assert_eq!(busy.delivery.status, ExternalDeliveryStatus::Queued);
    let first = transport.calls_for("https://uhp.example.test/advisor-a");
    assert_eq!(first.len(), 1);

    transport.script(Script::Response(response(
        "resp-a-1",
        "session-a",
        None,
        ResponseStatus::Completed,
    )));
    let registry = ExternalParticipantRegistry::with_transport(directory.path(), transport.clone());
    let recovered = registry
        .deliver_room_event(
            &event("event-1", 1),
            &attempt("event-1", "advisor-a"),
            &exported(),
        )
        .await
        .unwrap();
    assert_eq!(recovered.completed_bodies, ["PUBLIC-ADVISOR-REVIEW"]);
    let calls = transport.calls_for("https://uhp.example.test/advisor-a");
    assert_eq!(calls[0].key, calls[1].key);
    assert_eq!(
        calls[0].binding.request_fingerprint,
        calls[1].binding.request_fingerprint
    );
    assert_eq!(transport.environment_version.load(Ordering::SeqCst), 2);

    transport.script(Script::Response(response(
        "resp-a-2",
        "session-a",
        Some("resp-a-1"),
        ResponseStatus::InProgress,
    )));
    let in_progress = registry
        .deliver_room_event(
            &event("event-2", 2),
            &attempt("event-2", "advisor-a"),
            &exported(),
        )
        .await
        .unwrap();
    assert_eq!(in_progress.delivery.status, ExternalDeliveryStatus::Busy);
    transport.script(Script::Response(response(
        "resp-a-2",
        "session-a",
        Some("resp-a-1"),
        ResponseStatus::Completed,
    )));
    let registry = ExternalParticipantRegistry::with_transport(directory.path(), transport.clone());
    registry
        .deliver_room_event(
            &event("event-2", 2),
            &attempt("event-2", "advisor-a"),
            &exported(),
        )
        .await
        .unwrap();
    let calls = transport.calls_for("https://uhp.example.test/advisor-a");
    assert_eq!(calls[3].binding.response_id.as_deref(), Some("resp-a-2"));
    assert_eq!(calls[2].key, calls[3].key);

    transport.script(Script::Error("session_expired"));
    transport.script(Script::Response(response(
        "resp-a-3",
        "session-a-new",
        None,
        ResponseStatus::Completed,
    )));
    let linked = registry
        .deliver_room_event(
            &event("event-3", 3),
            &attempt("event-3", "advisor-a"),
            &exported(),
        )
        .await
        .unwrap();
    assert_eq!(linked.delivery.logical_attempt, 1);
    let calls = transport.calls_for("https://uhp.example.test/advisor-a");
    assert!(calls[5].request["previous_response_id"].is_null());
    assert_ne!(calls[4].key, calls[5].key);
}

#[tokio::test]
async fn room_uhp_partial_failure_does_not_block_the_other_advisor() {
    let directory = tempfile::tempdir().unwrap();
    let transport = Arc::new(RoomTransport::new());
    let registry = ExternalParticipantRegistry::with_transport(directory.path(), transport.clone());
    add_verified(&registry, "advisor-a").await;
    add_verified(&registry, "advisor-b").await;
    transport.script(Script::Error("provider_unavailable"));
    assert!(registry
        .deliver_room_event(
            &event("event-1", 1),
            &attempt("event-1", "advisor-a"),
            &exported()
        )
        .await
        .is_err());
    let other = registry
        .deliver_room_event(
            &event("event-1", 1),
            &attempt("event-1", "advisor-b"),
            &exported(),
        )
        .await
        .unwrap();
    assert_eq!(other.completed_bodies, ["PUBLIC-ADVISOR-REVIEW"]);
}
