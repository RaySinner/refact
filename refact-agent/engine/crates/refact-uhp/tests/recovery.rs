use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, Uri};
use axum::routing::any;
use axum::Router;
use refact_uhp::{
    interrupted_resume_advice, retry_advice, Client, CreateResponse, IdempotencyKey,
    InterruptedResumeAdvice, LinkedAttemptReason, RecoveryBinding, RequestScope, Response,
    ResponseStatus, RetryAdvice, SessionBinding, SessionChain, UhpError, UhpErrorObject,
    UnsafeResumeReason, IDEMPOTENCY_KEY_HEADER, UHP_VERSION, UHP_VERSION_HEADER,
};
use serde_json::{json, Value};
use tokio::sync::oneshot;

struct MockRequest {
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Value,
}

struct MockReply {
    status: StatusCode,
    body: Value,
}

impl MockReply {
    fn json(body: Value) -> Self {
        Self {
            status: StatusCode::OK,
            body,
        }
    }
}

struct MockServer {
    base_url: String,
    shutdown: Option<oneshot::Sender<()>>,
}

impl MockServer {
    async fn start<F>(handler: F) -> Self
    where
        F: Fn(MockRequest) -> MockReply + Clone + Send + Sync + 'static,
    {
        let app = Router::new().fallback(any(
            move |method: Method, uri: Uri, headers: HeaderMap, body: String| {
                let handler = handler.clone();
                async move {
                    let reply = handler(MockRequest {
                        method,
                        uri,
                        headers,
                        body: serde_json::from_str(&body).unwrap_or(Value::Null),
                    });
                    let mut headers = HeaderMap::new();
                    headers.insert(UHP_VERSION_HEADER, HeaderValue::from_static(UHP_VERSION));
                    (reply.status, headers, reply.body.to_string())
                }
            },
        ));
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let (shutdown, shutdown_rx) = oneshot::channel();
        tokio::spawn(async move {
            axum::Server::from_tcp(listener)
                .unwrap()
                .serve(app.into_make_service())
                .with_graceful_shutdown(async {
                    let _ = shutdown_rx.await;
                })
                .await
                .unwrap();
        });
        Self {
            base_url: format!("http://{address}/api/harness"),
            shutdown: Some(shutdown),
        }
    }
}

impl Drop for MockServer {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
    }
}

fn response_json(id: &str, status: ResponseStatus) -> Value {
    let status = match status {
        ResponseStatus::InProgress => "in_progress",
        ResponseStatus::Completed => "completed",
        ResponseStatus::Failed => "failed",
        ResponseStatus::Incomplete => "incomplete",
        ResponseStatus::Cancelled => "cancelled",
        ResponseStatus::Unknown => "unknown",
    };
    json!({
        "id": id,
        "object": "response",
        "created_at": 1_786_400_000u64,
        "status": status,
        "error": null,
        "incomplete_details": null,
        "previous_response_id": null,
        "model": "model-1",
        "output": [],
        "store": true,
        "usage": null,
        "metadata": {"session_id": "session-1", "harness_id": "harness-1"}
    })
}

fn response(id: &str, status: ResponseStatus) -> Response {
    serde_json::from_value(response_json(id, status)).unwrap()
}

fn scope() -> RequestScope {
    RequestScope::new("participant-1", "credential-profile-1")
}

fn key(value: &str) -> IdempotencyKey {
    IdempotencyKey::new(value).unwrap()
}

#[tokio::test]
async fn recovery_before_response_id_retries_same_key_and_body() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let captured = requests.clone();
    let server = MockServer::start(move |request| {
        captured.lock().unwrap().push(MockRequest {
            method: request.method,
            uri: request.uri,
            headers: request.headers,
            body: request.body,
        });
        MockReply::json(response_json("resp-recovered", ResponseStatus::Completed))
    })
    .await;
    let client = Client::new(&server.base_url, None).unwrap();
    let request = CreateResponse::new("same expensive prompt", "harness-1")
        .model("model-1")
        .environment("env-1");
    let binding = RecoveryBinding::before_response(request.fingerprint(&scope()).unwrap());
    let recovered = client
        .recover_response(&request, &key("restart-key"), &scope(), &binding)
        .await
        .unwrap();
    assert_eq!(recovered.id, "resp-recovered");

    let changed = CreateResponse::new("changed prompt", "harness-1")
        .model("model-1")
        .environment("env-1");
    assert!(matches!(
        client
            .recover_response(&changed, &key("restart-key"), &scope(), &binding)
            .await,
        Err(UhpError::FingerprintConflict { .. })
    ));
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, Method::POST);
    assert_eq!(requests[0].uri.path(), "/api/harness/v1/responses");
    assert_eq!(requests[0].body["input"], "same expensive prompt");
    assert_eq!(requests[0].body["metadata"]["harness_id"], "harness-1");
    assert_eq!(requests[0].body["metadata"]["environment"], "env-1");
    assert!(requests[0].body.get("environment").is_none());
    assert_eq!(
        requests[0].headers.get(IDEMPOTENCY_KEY_HEADER).unwrap(),
        "restart-key"
    );
}

#[tokio::test]
async fn recovery_after_created_uses_stored_get_without_another_post() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let captured = requests.clone();
    let server = MockServer::start(move |request| {
        captured.lock().unwrap().push((request.method, request.uri));
        MockReply::json(response_json("resp-created", ResponseStatus::Completed))
    })
    .await;
    let client = Client::new(&server.base_url, None).unwrap();
    let request = CreateResponse::new("review", "harness-1").streaming();
    let binding = RecoveryBinding::after_created(
        request.fingerprint(&scope()).unwrap(),
        "resp-created".to_string(),
    );
    let recovered = client
        .recover_response(&request, &key("created-key"), &scope(), &binding)
        .await
        .unwrap();
    assert_eq!(recovered.status, ResponseStatus::Completed);
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].0, Method::GET);
    assert_eq!(
        requests[0].1.path(),
        "/api/harness/v1/responses/resp-created"
    );
}

#[tokio::test]
async fn recovery_cancellation_uses_response_and_session_endpoints() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let captured = requests.clone();
    let server = MockServer::start(move |request| {
        let is_response_cancel = request
            .uri
            .path()
            .ends_with("/v1/responses/resp-cancel/cancel");
        captured.lock().unwrap().push((request.method, request.uri));
        if is_response_cancel {
            MockReply::json(response_json("resp-cancel", ResponseStatus::Cancelled))
        } else {
            MockReply::json(json!({"status": "cancelling"}))
        }
    })
    .await;
    let client = Client::new(&server.base_url, None).unwrap();
    let response = client.cancel_response("resp-cancel").await.unwrap();
    assert_eq!(response.status, ResponseStatus::Cancelled);
    client.cancel_session("session-1").await.unwrap();
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(requests.iter().all(|request| request.0 == Method::POST));
    assert_eq!(
        requests[0].1.path(),
        "/api/harness/v1/responses/resp-cancel/cancel"
    );
    assert_eq!(
        requests[1].1.path(),
        "/api/harness/v1/sessions/session-1/cancel"
    );
}

#[test]
fn recovery_session_busy_and_expired_have_typed_advice() {
    let busy = UhpError::Server {
        status: 409,
        error: UhpErrorObject {
            error_type: "conflict".to_string(),
            code: "session_busy".to_string(),
            message: "busy".to_string(),
            param: None,
            detail: Some(json!({"retry_after_ms": 250, "response_id": "resp-active"})),
            extra: Default::default(),
        },
    };
    assert_eq!(
        retry_advice(&busy),
        RetryAdvice::WaitForTerminalAndRetry {
            retry_after_ms: Some(250),
            in_flight_response_id: Some("resp-active".to_string()),
        }
    );
    let expired = UhpError::Server {
        status: 404,
        error: UhpErrorObject {
            code: "session_expired".to_string(),
            ..Default::default()
        },
    };
    assert_eq!(
        retry_advice(&expired),
        RetryAdvice::StartLinkedAttempt {
            reason: LinkedAttemptReason::SessionExpired,
        }
    );
}

#[tokio::test]
async fn recovery_restart_restores_binding_and_never_unlocks_on_drop() {
    let chain = SessionChain::new();
    let pending = chain.begin().await;
    assert!(chain.binding().await.task_pending);
    drop(pending);

    let waiting_chain = chain.clone();
    let mut waiting = Box::pin(async move { waiting_chain.begin().await });
    assert!(
        tokio::time::timeout(Duration::from_millis(20), &mut waiting)
            .await
            .is_err()
    );

    chain
        .recover_terminal(&response("resp-1", ResponseStatus::Completed))
        .await
        .unwrap();

    // Read the completed state before the waiting begin call starts a new pending task.
    let binding = chain.binding().await;
    let durable = serde_json::to_string(&binding).unwrap();
    let binding: SessionBinding = serde_json::from_str(&durable).unwrap();
    assert_eq!(binding.session_id.as_deref(), Some("session-1"));
    assert_eq!(binding.latest_response_id.as_deref(), Some("resp-1"));
    assert!(!binding.task_pending);
    drop(waiting.await);

    let restored = SessionChain::restore(binding).unwrap();
    let mut continuation = restored.begin().await;
    let request = continuation.request("continue", "harness-1").await.unwrap();
    assert_eq!(
        serde_json::to_value(request).unwrap()["previous_response_id"],
        "resp-1"
    );
}

#[test]
fn recovery_interrupted_response_never_silently_resumes_unsafe_effects() {
    let mut interrupted = response("resp-interrupted", ResponseStatus::Failed);
    interrupted.error = Some(UhpErrorObject {
        error_type: "harness_error".to_string(),
        code: "interrupted".to_string(),
        message: "interrupted".to_string(),
        param: None,
        detail: Some(json!({
            "reason": "interrupted",
            "workspace": "kept",
            "effects_may_repeat": false
        })),
        extra: Default::default(),
    });
    assert_eq!(
        interrupted_resume_advice(&interrupted, true),
        InterruptedResumeAdvice::ExplicitResumeAllowed
    );
    assert_eq!(
        interrupted_resume_advice(&interrupted, false),
        InterruptedResumeAdvice::ExplicitNewAttemptRequired(
            UnsafeResumeReason::ResumeCapabilityMissing
        )
    );
    interrupted.error.as_mut().unwrap().detail = Some(json!({
        "reason": "interrupted",
        "workspace": "checkpoint",
        "effects_may_repeat": true
    }));
    assert_eq!(
        interrupted_resume_advice(&interrupted, true),
        InterruptedResumeAdvice::ExplicitNewAttemptRequired(UnsafeResumeReason::EffectsMayRepeat)
    );
    interrupted.error.as_mut().unwrap().detail = Some(json!({
        "reason": "interrupted",
        "workspace": "lost",
        "effects_may_repeat": false
    }));
    assert_eq!(
        interrupted_resume_advice(&interrupted, true),
        InterruptedResumeAdvice::ExplicitNewAttemptRequired(UnsafeResumeReason::WorkspaceLost)
    );
}
