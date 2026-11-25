use std::sync::atomic::AtomicU64;
use std::sync::Arc;

use axum::extract::{Path, State};
use axum::response::Json;
use axum::routing::{get, post};
use axum::Router;
use hyper::StatusCode;
use refact_runtime_api::external_participant::{
    ExternalCancelTarget, ExternalDeliveryRead, ExternalParticipantConfig,
    ExternalParticipantDetails, ExternalParticipantError, ExternalParticipantRegistry,
    ExternalParticipantRegistryRead, ExternalRetirementRead, ExternalSessionRead,
    SanitizedExternalError,
};
use refact_runtime_api::AppState;
use refact_tasks::events::{ExternalParticipantEventError, TaskEvent, TaskEventEnvelope};
use refact_uhp::{CreateResponse, ExternalEvidenceRecord};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

type ApiResult<T> = Result<Json<T>, (StatusCode, Json<SanitizedExternalError>)>;

#[derive(Clone)]
pub struct ExternalParticipantApi {
    registry: Arc<ExternalParticipantRegistry>,
    task_events_tx: broadcast::Sender<TaskEventEnvelope>,
    task_events_seq: Arc<AtomicU64>,
}

impl ExternalParticipantApi {
    pub fn new(
        registry: Arc<ExternalParticipantRegistry>,
        task_events_tx: broadcast::Sender<TaskEventEnvelope>,
        task_events_seq: Arc<AtomicU64>,
    ) -> Self {
        Self {
            registry,
            task_events_tx,
            task_events_seq,
        }
    }

    pub fn from_app(app: &AppState) -> Self {
        Self::new(
            app.external_participants.clone(),
            app.chat.task_events_tx.clone(),
            app.chat.task_events_seq.clone(),
        )
    }
}

#[derive(Deserialize)]
pub struct ExternalParticipantWriteRequest {
    #[serde(flatten)]
    pub config: ExternalParticipantConfig,
    #[serde(default)]
    pub credential: Option<ExternalCredentialWrite>,
}

#[derive(Deserialize)]
pub struct ExternalCredentialWrite {
    pub bearer: String,
}

#[derive(Debug, Deserialize)]
pub struct ExternalEnabledRequest {
    pub enabled: bool,
}

#[derive(Debug, Deserialize)]
pub struct ExternalProvisionRequest {
    pub idempotency_key: String,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct ExternalDeleteResponse {
    pub participant_id: String,
    pub deleted: bool,
    pub retired: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retirement: Option<ExternalRetirementRead>,
}

pub fn external_participants_router(api: ExternalParticipantApi) -> Router<AppState> {
    Router::new()
        .route(
            "/external-participants",
            get(handle_v1_external_participants_list).post(handle_v1_external_participants_create),
        )
        .route(
            "/external-participants/:participant_id",
            get(handle_v1_external_participants_get)
                .put(handle_v1_external_participants_update)
                .delete(handle_v1_external_participants_delete),
        )
        .route(
            "/external-participants/:participant_id/enabled",
            post(handle_v1_external_participants_enabled),
        )
        .route(
            "/external-participants/:participant_id/discovery",
            post(handle_v1_external_participants_discovery),
        )
        .route(
            "/external-participants/:participant_id/test",
            post(handle_v1_external_participants_test),
        )
        .route(
            "/external-participants/:participant_id/provision",
            post(handle_v1_external_participants_provision),
        )
        .route(
            "/external-participants/:participant_id/update",
            post(handle_v1_external_participants_remote_update),
        )
        .route(
            "/external-participants/:participant_id/cancel",
            post(handle_v1_external_participants_cancel),
        )
        .route(
            "/external-participants/:participant_id/sessions",
            get(handle_v1_external_participants_sessions),
        )
        .route(
            "/external-participants/:participant_id/deliveries",
            get(handle_v1_external_participants_deliveries),
        )
        .route(
            "/external-participants/:participant_id/evidence",
            get(handle_v1_external_participants_evidence),
        )
        .route(
            "/external-participants/:participant_id/deliveries/:delivery_id/retry",
            post(handle_v1_external_participants_retry),
        )
        .with_state(api)
}

pub async fn handle_v1_external_participants_list(
    State(api): State<ExternalParticipantApi>,
) -> ApiResult<ExternalParticipantRegistryRead> {
    api.registry.snapshot().map(Json).map_err(api_error)
}

pub async fn handle_v1_external_participants_get(
    State(api): State<ExternalParticipantApi>,
    Path(participant_id): Path<String>,
) -> ApiResult<ExternalParticipantDetails> {
    api.registry
        .details(&participant_id)
        .map(Json)
        .map_err(api_error)
}

pub async fn handle_v1_external_participants_create(
    State(api): State<ExternalParticipantApi>,
    Json(request): Json<ExternalParticipantWriteRequest>,
) -> ApiResult<ExternalParticipantDetails> {
    let participant_id = request.config.id.clone();
    let result = async {
        api.registry.create(request.config).await?;
        if let Some(credential) = request.credential {
            api.registry
                .write_credential(&participant_id, &credential.bearer)
                .await?;
        }
        Ok(())
    }
    .await;
    finish_mutation(&api, &participant_id, "created", result).await
}

pub async fn handle_v1_external_participants_update(
    State(api): State<ExternalParticipantApi>,
    Path(participant_id): Path<String>,
    Json(request): Json<ExternalParticipantWriteRequest>,
) -> ApiResult<ExternalParticipantDetails> {
    let operation_id = participant_id.clone();
    let result = async {
        api.registry
            .update_configuration(&operation_id, request.config)
            .await?;
        if let Some(credential) = request.credential {
            api.registry
                .write_credential(&operation_id, &credential.bearer)
                .await?;
        }
        Ok(())
    }
    .await;
    finish_mutation(&api, &participant_id, "updated", result).await
}

pub async fn handle_v1_external_participants_enabled(
    State(api): State<ExternalParticipantApi>,
    Path(participant_id): Path<String>,
    Json(request): Json<ExternalEnabledRequest>,
) -> ApiResult<ExternalParticipantDetails> {
    let operation_id = participant_id.clone();
    let result = async {
        let mut config = api.registry.participant(&operation_id)?.config;
        config.enabled = request.enabled;
        api.registry
            .update_configuration(&operation_id, config)
            .await?;
        Ok(())
    }
    .await;
    finish_mutation(&api, &participant_id, "enabled_changed", result).await
}

pub async fn handle_v1_external_participants_delete(
    State(api): State<ExternalParticipantApi>,
    Path(participant_id): Path<String>,
) -> ApiResult<ExternalDeleteResponse> {
    let before = api.registry.details(&participant_id).map_err(api_error)?;
    if before.participant.config.harness_id.is_some() {
        match api.registry.retire(&participant_id).await {
            Ok(retirement) => {
                let details = api.registry.details(&participant_id).map_err(api_error)?;
                emit_change(&api, &participant_id, "retired", &details, None);
                Ok(Json(ExternalDeleteResponse {
                    participant_id,
                    deleted: retirement.deleted,
                    retired: true,
                    retirement: Some(retirement),
                }))
            }
            Err(error) => {
                emit_error(&api, &participant_id, "retire_failed", &error);
                Err(api_error(error))
            }
        }
    } else {
        match api.registry.remove(&participant_id).await {
            Ok(()) => {
                emit_change(&api, &participant_id, "deleted", &before, None);
                Ok(Json(ExternalDeleteResponse {
                    participant_id,
                    deleted: true,
                    retired: false,
                    retirement: None,
                }))
            }
            Err(error) => {
                emit_error(&api, &participant_id, "delete_failed", &error);
                Err(api_error(error))
            }
        }
    }
}

pub async fn handle_v1_external_participants_discovery(
    State(api): State<ExternalParticipantApi>,
    Path(participant_id): Path<String>,
) -> ApiResult<ExternalParticipantDetails> {
    let result = api.registry.discover(&participant_id).await.map(|_| ());
    finish_mutation(&api, &participant_id, "discovered", result).await
}

pub async fn handle_v1_external_participants_test(
    State(api): State<ExternalParticipantApi>,
    Path(participant_id): Path<String>,
) -> ApiResult<ExternalParticipantDetails> {
    let result = api.registry.test_model(&participant_id).await.map(|_| ());
    finish_mutation(&api, &participant_id, "model_tested", result).await
}

pub async fn handle_v1_external_participants_provision(
    State(api): State<ExternalParticipantApi>,
    Path(participant_id): Path<String>,
    Json(request): Json<ExternalProvisionRequest>,
) -> ApiResult<ExternalParticipantDetails> {
    let result = api
        .registry
        .provision(&participant_id, &request.idempotency_key)
        .await
        .map(|_| ());
    finish_mutation(&api, &participant_id, "provisioned", result).await
}

pub async fn handle_v1_external_participants_remote_update(
    State(api): State<ExternalParticipantApi>,
    Path(participant_id): Path<String>,
) -> ApiResult<ExternalParticipantDetails> {
    let result = api
        .registry
        .update_remote(&participant_id)
        .await
        .map(|_| ());
    finish_mutation(&api, &participant_id, "remote_updated", result).await
}

pub async fn handle_v1_external_participants_cancel(
    State(api): State<ExternalParticipantApi>,
    Path(participant_id): Path<String>,
    Json(target): Json<ExternalCancelTarget>,
) -> ApiResult<ExternalParticipantDetails> {
    let result = api.registry.cancel(&participant_id, target).await;
    finish_mutation(&api, &participant_id, "cancelled", result).await
}

pub async fn handle_v1_external_participants_retry(
    State(api): State<ExternalParticipantApi>,
    Path((participant_id, delivery_id)): Path<(String, String)>,
    Json(request): Json<CreateResponse>,
) -> ApiResult<ExternalParticipantDetails> {
    let result = api
        .registry
        .retry_delivery(&participant_id, &delivery_id, &request)
        .await
        .map(|_| ());
    finish_mutation(&api, &participant_id, "retried", result).await
}

pub async fn handle_v1_external_participants_sessions(
    State(api): State<ExternalParticipantApi>,
    Path(participant_id): Path<String>,
) -> ApiResult<Vec<ExternalSessionRead>> {
    api.registry
        .details(&participant_id)
        .map(|details| Json(details.sessions))
        .map_err(api_error)
}

pub async fn handle_v1_external_participants_deliveries(
    State(api): State<ExternalParticipantApi>,
    Path(participant_id): Path<String>,
) -> ApiResult<Vec<ExternalDeliveryRead>> {
    api.registry
        .details(&participant_id)
        .map(|details| Json(details.deliveries))
        .map_err(api_error)
}

pub async fn handle_v1_external_participants_evidence(
    State(api): State<ExternalParticipantApi>,
    Path(participant_id): Path<String>,
) -> ApiResult<Vec<ExternalEvidenceRecord>> {
    api.registry
        .details(&participant_id)
        .map(|details| Json(details.evidence))
        .map_err(api_error)
}

async fn finish_mutation(
    api: &ExternalParticipantApi,
    participant_id: &str,
    change: &str,
    result: Result<(), ExternalParticipantError>,
) -> ApiResult<ExternalParticipantDetails> {
    match result {
        Ok(()) => {
            let details = api.registry.details(participant_id).map_err(api_error)?;
            emit_change(api, participant_id, change, &details, None);
            Ok(Json(details))
        }
        Err(error) => {
            emit_error(api, participant_id, &format!("{change}_failed"), &error);
            Err(api_error(error))
        }
    }
}

fn emit_error(
    api: &ExternalParticipantApi,
    participant_id: &str,
    change: &str,
    error: &ExternalParticipantError,
) {
    if let Ok(details) = api.registry.details(participant_id) {
        emit_change(api, participant_id, change, &details, Some(error));
    }
}

fn emit_change(
    api: &ExternalParticipantApi,
    participant_id: &str,
    change: &str,
    details: &ExternalParticipantDetails,
    error: Option<&ExternalParticipantError>,
) {
    let details = serde_json::to_value(details)
        .expect("external participant read models must remain JSON serializable");
    let error = error.map(|error| {
        let error = error.sanitized();
        ExternalParticipantEventError {
            code: error.code,
            message: error.message,
        }
    });
    let seq = api
        .task_events_seq
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let _ = api.task_events_tx.send(TaskEventEnvelope {
        seq,
        event: TaskEvent::ExternalParticipantChanged {
            participant_id: participant_id.to_string(),
            change: change.to_string(),
            details: Some(details),
            error,
        },
    });
}

fn api_error(error: ExternalParticipantError) -> (StatusCode, Json<SanitizedExternalError>) {
    let status = match &error {
        ExternalParticipantError::NotFound(_) => StatusCode::NOT_FOUND,
        ExternalParticipantError::AlreadyExists(_)
        | ExternalParticipantError::ProvisionConflict => StatusCode::CONFLICT,
        ExternalParticipantError::InvalidConfiguration(_) => StatusCode::BAD_REQUEST,
        ExternalParticipantError::ExternalWriterForbidden => StatusCode::FORBIDDEN,
        ExternalParticipantError::InvalidOperation(_)
        | ExternalParticipantError::CapabilityUnsupported { .. }
        | ExternalParticipantError::NotVerified => StatusCode::CONFLICT,
        ExternalParticipantError::Storage(_) => StatusCode::INTERNAL_SERVER_ERROR,
        ExternalParticipantError::Transport(_) => StatusCode::BAD_GATEWAY,
    };
    (status, Json(error.sanitized()))
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Mutex;

    use async_trait::async_trait;
    use axum::body::Body;
    use axum::http::Request;
    use hyper::body::to_bytes;
    use refact_runtime_api::external_participant::{
        ExternalConnection, ExternalDeliveryRecord, ExternalParticipantTransport,
        ExternalSourceFreshness,
    };
    use refact_uhp::{
        Discovery, EvidenceCheck, EvidenceCheckStatus, Harness, HarnessConfig, HarnessRetirement,
        IdempotencyKey, Model, RecoveryBinding, RequestScope, Response, RetiredSessions,
        UhpDeliveryRequestState, UhpDeliveryState, UhpError, UhpSessionBinding, VersionedHarness,
    };
    use serde_json::{json, Value};
    use tempfile::TempDir;
    use tower::ServiceExt;

    use super::*;

    const SECRET: &str = "SECRET-EXT-1B-HTTP-CANARY";

    struct MockTransport {
        disable_management: AtomicBool,
        fail_discovery: AtomicBool,
        calls: Mutex<Vec<&'static str>>,
    }

    impl MockTransport {
        fn new() -> Self {
            Self {
                disable_management: AtomicBool::new(false),
                fail_discovery: AtomicBool::new(false),
                calls: Mutex::new(Vec::new()),
            }
        }

        fn calls(&self) -> Vec<&'static str> {
            self.calls.lock().unwrap().clone()
        }
    }

    struct Fixture {
        _directory: TempDir,
        registry: Arc<ExternalParticipantRegistry>,
        router: Router<AppState>,
        events: broadcast::Receiver<TaskEventEnvelope>,
        transport: Arc<MockTransport>,
    }

    fn fixture() -> Fixture {
        let directory = tempfile::tempdir().unwrap();
        let transport = Arc::new(MockTransport::new());
        let registry = Arc::new(ExternalParticipantRegistry::with_transport(
            directory.path(),
            transport.clone(),
        ));
        let (task_events_tx, events) = broadcast::channel(64);
        let api = ExternalParticipantApi::new(
            registry.clone(),
            task_events_tx,
            Arc::new(AtomicU64::new(0)),
        );
        let router = external_participants_router(api);
        Fixture {
            _directory: directory,
            registry,
            router,
            events,
            transport,
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

    fn write_body() -> Value {
        let mut body = serde_json::to_value(config()).unwrap();
        body["credential"] = json!({"bearer": SECRET});
        body
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

    fn harness(config: &HarnessConfig) -> Harness {
        Harness {
            id: "chrn_1".to_string(),
            name: config.name.clone().unwrap_or_default(),
            base: config.base.clone(),
            default_model: config.default_model.clone(),
            ..Harness::default()
        }
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

    async fn request(
        router: &Router<AppState>,
        method: &str,
        uri: &str,
        body: Value,
    ) -> (StatusCode, Value, String) {
        let body = if body.is_null() {
            Body::empty()
        } else {
            Body::from(body.to_string())
        };
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(uri)
                    .header("content-type", "application/json")
                    .body(body)
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = to_bytes(response.into_body()).await.unwrap();
        let text = String::from_utf8(bytes.to_vec()).unwrap();
        let json = serde_json::from_str(&text).unwrap_or(Value::Null);
        (status, json, text)
    }

    async fn create(fixture: &Fixture) -> Value {
        let (status, body, _) = request(
            &fixture.router,
            "POST",
            "/external-participants",
            write_body(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        body
    }

    fn next_event(fixture: &mut Fixture) -> TaskEventEnvelope {
        fixture.events.try_recv().unwrap()
    }

    fn assert_public_json(value: &str) {
        assert!(!value.contains(SECRET));
        assert!(!value.contains("bearer"));
    }

    #[tokio::test]
    async fn external_participants_crud_secret_events_and_empty_registry() {
        let mut fixture = fixture();
        let (status, empty, _) = request(
            &fixture.router,
            "GET",
            "/external-participants",
            Value::Null,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(empty["mode"], "internal_agents");
        assert_eq!(empty["participants"], json!([]));

        let (status, created, raw) = request(
            &fixture.router,
            "POST",
            "/external-participants",
            write_body(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_public_json(&raw);
        assert_eq!(created["participant"]["state"], "staged");
        assert_eq!(created["participant"]["writes"], false);
        assert!(created["participant"]["agent_chat_id"].is_null());
        let event = next_event(&mut fixture);
        assert_eq!(event.seq, 0);
        let event_json = serde_json::to_string(&event).unwrap();
        assert_public_json(&event_json);
        assert_eq!(event_json.contains("external_participant_changed"), true);

        let mut updated = write_body();
        updated["name"] = json!("Second reviewer");
        updated["credential"] = json!({"bearer": format!("{SECRET}-UPDATED")});
        let (status, body, raw) = request(
            &fixture.router,
            "PUT",
            "/external-participants/participant-1",
            updated,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["participant"]["name"], "Second reviewer");
        assert!(!raw.contains(SECRET));
        assert_public_json(&serde_json::to_string(&next_event(&mut fixture)).unwrap());

        let (status, disabled, _) = request(
            &fixture.router,
            "POST",
            "/external-participants/participant-1/enabled",
            json!({"enabled": false}),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(disabled["participant"]["enabled"], false);
        let _ = next_event(&mut fixture);

        for path in ["sessions", "deliveries", "evidence"] {
            let (status, body, _) = request(
                &fixture.router,
                "GET",
                &format!("/external-participants/participant-1/{path}"),
                Value::Null,
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(body, json!([]));
        }

        let registry_json =
            std::fs::read_to_string(fixture._directory.path().join("external-participants.json"))
                .unwrap();
        assert!(!registry_json.contains(SECRET));

        let (status, deleted, _) = request(
            &fixture.router,
            "DELETE",
            "/external-participants/participant-1",
            Value::Null,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(deleted["deleted"], true);
        assert_eq!(deleted["retired"], false);
        match next_event(&mut fixture).event {
            TaskEvent::ExternalParticipantChanged {
                participant_id,
                change,
                details,
                error,
            } => {
                assert_eq!(participant_id, "participant-1");
                assert_eq!(change, "deleted");
                assert_eq!(details.unwrap()["participant"]["id"], "participant-1");
                assert!(error.is_none());
            }
            _ => panic!("unexpected event"),
        }
        let (status, _, _) = request(
            &fixture.router,
            "GET",
            "/external-participants/participant-1",
            Value::Null,
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn external_participants_writer_is_rejected_without_creating_agent() {
        let mut fixture = fixture();
        let mut body = write_body();
        body["writes"] = json!(true);
        let (status, error, raw) =
            request(&fixture.router, "POST", "/external-participants", body).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(error["code"], "external_writer_forbidden");
        assert_public_json(&raw);
        assert!(fixture.events.try_recv().is_err());
        assert!(fixture.registry.snapshot().unwrap().participants.is_empty());
    }

    #[tokio::test]
    async fn external_participants_discovery_provision_test_update_and_retire() {
        let mut fixture = fixture();
        let created = create(&fixture).await;
        assert_eq!(created["participant"]["state"], "staged");
        let _ = next_event(&mut fixture);

        for (path, body, expected_change) in [
            ("discovery", Value::Null, "discovered"),
            (
                "provision",
                json!({"idempotency_key": "provision-participant-1"}),
                "provisioned",
            ),
            ("test", Value::Null, "model_tested"),
            ("update", Value::Null, "remote_updated"),
        ] {
            let (status, response, raw) = request(
                &fixture.router,
                "POST",
                &format!("/external-participants/participant-1/{path}"),
                body,
            )
            .await;
            assert_eq!(status, StatusCode::OK, "action {path}: {response}");
            assert_eq!(response["participant"]["state"], "unverified");
            assert_public_json(&raw);
            let event = next_event(&mut fixture);
            match event.event {
                TaskEvent::ExternalParticipantChanged {
                    change, details, ..
                } => {
                    assert_eq!(change, expected_change);
                    assert!(details.is_some());
                }
                _ => panic!("unexpected event"),
            }
        }
        let (status, retired, _) = request(
            &fixture.router,
            "DELETE",
            "/external-participants/participant-1",
            Value::Null,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(retired["retired"], true);
        assert_eq!(retired["retirement"]["deleted"], true);
        assert_eq!(next_event(&mut fixture).seq, 5);
        assert_eq!(
            fixture.transport.calls(),
            vec!["discover", "provision", "test", "update", "retire"]
        );
    }

    #[tokio::test]
    async fn external_participants_capability_and_transport_errors_are_typed_and_sanitized() {
        let mut fixture = fixture();
        fixture
            .transport
            .disable_management
            .store(true, Ordering::SeqCst);
        let _ = create(&fixture).await;
        let _ = next_event(&mut fixture);
        let (status, _, _) = request(
            &fixture.router,
            "POST",
            "/external-participants/participant-1/discovery",
            Value::Null,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let _ = next_event(&mut fixture);

        let (status, error, raw) = request(
            &fixture.router,
            "POST",
            "/external-participants/participant-1/provision",
            json!({"idempotency_key": "provision-participant-1"}),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(error["code"], "capability_unsupported");
        assert_public_json(&raw);
        let event = serde_json::to_value(next_event(&mut fixture)).unwrap();
        assert_eq!(event["change"], "provisioned_failed");
        assert_eq!(event["error"]["code"], "capability_unsupported");
        assert_eq!(
            event["details"]["participant"]["actions"]["provision"]["enabled"],
            false
        );

        fixture
            .transport
            .fail_discovery
            .store(true, Ordering::SeqCst);
        let (status, error, raw) = request(
            &fixture.router,
            "POST",
            "/external-participants/participant-1/discovery",
            Value::Null,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_GATEWAY);
        assert_eq!(error["code"], "protocol_response");
        assert_public_json(&raw);
        let event = serde_json::to_string(&next_event(&mut fixture)).unwrap();
        assert!(event.contains("discovered_failed"));
        assert!(event.contains("failed"));
        assert_public_json(&event);
    }

    #[tokio::test]
    async fn external_participants_retry_cancel_and_operational_views() {
        let mut fixture = fixture();
        let _ = create(&fixture).await;
        let _ = next_event(&mut fixture);
        for (path, body) in [
            ("discovery", Value::Null),
            (
                "provision",
                json!({"idempotency_key": "provision-participant-1"}),
            ),
        ] {
            let (status, _, _) = request(
                &fixture.router,
                "POST",
                &format!("/external-participants/participant-1/{path}"),
                body,
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            let _ = next_event(&mut fixture);
        }
        fixture
            .registry
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
        let retry_request = CreateResponse::new("public room body", "chrn_1").model("model-1");
        let scope = RequestScope::new("participant-1", "credential/external-1");
        fixture
            .registry
            .upsert_delivery(ExternalDeliveryRecord {
                delivery: UhpDeliveryState {
                    delivery_id: "delivery-1".to_string(),
                    room_id: "room-1".to_string(),
                    event_id: "event-1".to_string(),
                    recipient_id: "participant-1".to_string(),
                    logical_attempt: 3,
                    request_fingerprint: retry_request.fingerprint(&scope).unwrap(),
                    idempotency_key: IdempotencyKey::new("delivery-key-1").unwrap(),
                    request_state: UhpDeliveryRequestState::Failed,
                    response_id: Some("resp-1".to_string()),
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
            })
            .await
            .unwrap();

        let (status, deliveries, _) = request(
            &fixture.router,
            "GET",
            "/external-participants/participant-1/deliveries",
            Value::Null,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(deliveries[0]["status"], "terminal");
        assert_eq!(deliveries[0]["source_freshness"], "stale");
        assert_eq!(deliveries[0]["logical_attempt"], 3);

        let (status, retried, _) = request(
            &fixture.router,
            "POST",
            "/external-participants/participant-1/deliveries/delivery-1/retry",
            serde_json::to_value(&retry_request).unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(retried["deliveries"][0]["request_state"], "completed");
        let retry_event = serde_json::to_value(next_event(&mut fixture)).unwrap();
        assert_eq!(
            retry_event["details"]["sessions"][0]["session_id"],
            "session-1"
        );

        let (status, cancelled, _) = request(
            &fixture.router,
            "POST",
            "/external-participants/participant-1/cancel",
            json!({"kind": "response", "id": "resp-1"}),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(cancelled["deliveries"][0]["request_state"], "cancelled");
        assert_eq!(
            cancelled["sessions"][0]["last_terminal_status"],
            "cancelled"
        );
        let _ = next_event(&mut fixture);

        fixture
            .registry
            .record_live_evidence(
                "participant-1",
                ExternalEvidenceRecord {
                    smoke_suite_id: "smoke-live".to_string(),
                    server_version: "0.41.25".to_string(),
                    cli_version: "1.2.3".to_string(),
                    model_version: "model-1".to_string(),
                    checks: vec![EvidenceCheck {
                        name: "live_read_only_smoke".to_string(),
                        status: EvidenceCheckStatus::Pass,
                    }],
                    generated_at: 30,
                    sanitized_report_location: "evidence/smoke-live.json".to_string(),
                },
            )
            .await
            .unwrap();
        let (status, evidence, _) = request(
            &fixture.router,
            "GET",
            "/external-participants/participant-1/evidence",
            Value::Null,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(evidence[0]["smoke_suite_id"], "smoke-live");
        assert_eq!(evidence[0]["checks"][0]["status"], "pass");
        let (status, sessions, _) = request(
            &fixture.router,
            "GET",
            "/external-participants/participant-1/sessions",
            Value::Null,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(sessions[0]["busy"], false);
        assert_eq!(sessions[0]["queued"], false);
    }
}
