use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, Uri};
use axum::routing::any;
use axum::Router;
use refact_uhp::{
    completed_assistant_bodies, Client, CreateResponse, IdempotencyKey, RequestScope, UhpError,
    IDEMPOTENCY_KEY_HEADER, UHP_VERSION, UHP_VERSION_HEADER,
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
    version: Option<&'static str>,
    body: Value,
}

impl MockReply {
    fn json(body: Value) -> Self {
        Self {
            status: StatusCode::OK,
            version: Some(UHP_VERSION),
            body,
        }
    }

    fn status(mut self, status: StatusCode) -> Self {
        self.status = status;
        self
    }

    fn version(mut self, version: &'static str) -> Self {
        self.version = Some(version);
        self
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
                    if let Some(version) = reply.version {
                        headers.insert(UHP_VERSION_HEADER, HeaderValue::from_static(version));
                    }
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

fn response_json(id: &str) -> Value {
    json!({
        "id": id,
        "object": "response",
        "created_at": 1_786_400_000u64,
        "status": "completed",
        "error": null,
        "incomplete_details": null,
        "previous_response_id": null,
        "model": "model-1",
        "output": [{
            "id": "msg-1",
            "type": "message",
            "role": "assistant",
            "status": "completed",
            "content": [{"type": "output_text", "text": "review complete", "annotations": []}]
        }],
        "store": true,
        "usage": {"input_tokens": 10, "output_tokens": 5, "total_tokens": 15},
        "metadata": {"session_id": "session-1", "harness_id": "harness-1"}
    })
}

fn scope() -> RequestScope {
    RequestScope::new("participant-1", "credential-profile-1")
}

fn key(value: &str) -> IdempotencyKey {
    IdempotencyKey::new(value).unwrap()
}

#[tokio::test]
async fn discovery_negotiates_version_and_reads_capabilities() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let captured = requests.clone();
    let server = MockServer::start(move |request| {
        captured
            .lock()
            .unwrap()
            .push((request.method, request.uri, request.headers.clone()));
        MockReply::json(json!({
            "object": "uhp.discovery",
            "protocol": "uhp",
            "versions": [UHP_VERSION, "2026-09-12"],
            "default_version": UHP_VERSION,
            "conformance_class": "full",
            "capabilities": {
                "streaming": true,
                "sessions": true,
                "cancellation": true,
                "idempotency": true,
                "future_capability": true
            },
            "implementation": {"name": "mock", "version": "1"},
            "additive": {"ignored": true}
        }))
    })
    .await;
    let client = Client::new(&server.base_url, Some("secret-token".to_string())).unwrap();
    let discovery = client.discover().await.unwrap();

    assert!(discovery.capability("streaming"));
    assert!(discovery.capability("idempotency"));
    assert!(discovery.capability("future_capability"));
    assert!(!discovery.capability("environments"));
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].0, Method::GET);
    assert_eq!(requests[0].1.path(), "/api/harness/v1/uhp");
    assert_eq!(requests[0].2.get(UHP_VERSION_HEADER).unwrap(), UHP_VERSION);
    assert!(requests[0].2.get("authorization").is_none());
}

#[tokio::test]
async fn discovery_rejects_response_version_mismatch() {
    let server = MockServer::start(|_| MockReply::json(json!({})).version("2026-09-12")).await;
    let client = Client::new(&server.base_url, None).unwrap();
    let error = client.discover().await.unwrap_err();
    assert!(matches!(
        error,
        UhpError::VersionMismatch { actual: Some(actual), .. } if actual == "2026-09-12"
    ));
}

#[tokio::test]
async fn normalizes_base_url_matrix_for_all_endpoints() {
    for has_prefix in [false, true] {
        for trailing_slash in [false, true] {
            let requests = Arc::new(Mutex::new(Vec::new()));
            let captured = requests.clone();
            let server = MockServer::start(move |request| {
                let is_discovery = request.uri.path().ends_with("/v1/uhp");
                captured.lock().unwrap().push(request.uri);
                if is_discovery {
                    MockReply::json(json!({
                        "object": "uhp.discovery",
                        "protocol": "uhp",
                        "versions": [UHP_VERSION],
                        "default_version": UHP_VERSION,
                        "conformance_class": "full",
                        "capabilities": {},
                        "implementation": {"name": "mock", "version": "1"}
                    }))
                } else {
                    MockReply::json(response_json("resp-matrix"))
                }
            })
            .await;
            let prefix = if has_prefix {
                server.base_url.clone()
            } else {
                server.base_url.trim_end_matches("/api/harness").to_string()
            };
            let base_url = if trailing_slash {
                format!("{prefix}/")
            } else {
                prefix
            };
            let client = Client::new(&base_url, None).unwrap();

            client.discover().await.unwrap();
            client
                .create_response(
                    &CreateResponse::new("matrix", "harness-1"),
                    &key("matrix-key"),
                    &scope(),
                )
                .await
                .unwrap();
            client.get_response("resp-matrix").await.unwrap();

            let expected_prefix = if has_prefix { "/api/harness" } else { "" };
            let requests = requests.lock().unwrap();
            let paths = requests
                .iter()
                .map(|uri| uri.path().to_string())
                .collect::<Vec<_>>();
            assert_eq!(
                paths,
                [
                    format!("{expected_prefix}/v1/uhp"),
                    format!("{expected_prefix}/v1/responses"),
                    format!("{expected_prefix}/v1/responses/resp-matrix"),
                ],
                "base URL: {base_url}"
            );
        }
    }
}

#[tokio::test]
async fn creates_non_streaming_task_and_continuation() {
    let bodies = Arc::new(Mutex::new(Vec::new()));
    let captured = bodies.clone();
    let server = MockServer::start(move |request| {
        assert_eq!(request.method, Method::POST);
        assert_eq!(request.uri.path(), "/api/harness/v1/responses");
        assert_eq!(
            request.headers.get("authorization").unwrap(),
            "Bearer token-1"
        );
        let previous = request.body["previous_response_id"]
            .as_str()
            .map(str::to_string);
        assert_eq!(
            request.headers.get(IDEMPOTENCY_KEY_HEADER).unwrap(),
            if previous.is_some() {
                "delivery-2"
            } else {
                "delivery-1"
            }
        );
        captured.lock().unwrap().push(request.body);
        let mut response = response_json(if previous.as_deref() == Some("resp-1") {
            "resp-2"
        } else {
            "resp-1"
        });
        response["previous_response_id"] = previous.map_or(Value::Null, |id| json!(id));
        MockReply::json(response)
    })
    .await;
    let client = Client::new(&server.base_url, Some("token-1".to_string())).unwrap();
    let first = client
        .create_response(
            &CreateResponse::new("review this", "harness-1")
                .model("model-1")
                .max_step(4)
                .timeout_seconds(120),
            &key("delivery-1"),
            &scope(),
        )
        .await
        .unwrap();
    let second = client
        .create_response(
            &CreateResponse::new("continue", "harness-1")
                .model("model-1")
                .previous_response_id(&first.id),
            &key("delivery-2"),
            &scope(),
        )
        .await
        .unwrap();

    assert_eq!(second.previous_response_id.as_deref(), Some("resp-1"));
    assert_eq!(second.metadata.session_id.as_deref(), Some("session-1"));
    assert_eq!(completed_assistant_bodies(&second), ["review complete"]);
    let bodies = bodies.lock().unwrap();
    assert_eq!(bodies.len(), 2);
    assert_eq!(bodies[0]["input"], "review this");
    assert_eq!(bodies[0]["metadata"]["harness_id"], "harness-1");
    assert_eq!(bodies[0]["stream"], false);
    assert_eq!(bodies[0]["store"], true);
    assert_eq!(bodies[0]["max_step"], 4);
    assert_eq!(bodies[0]["timeout_seconds"], 120);
    assert_eq!(bodies[1]["previous_response_id"], "resp-1");
    for body in bodies.iter() {
        assert!(body.get("tools").is_none());
        assert!(body.get("include").is_none());
    }
}

#[tokio::test]
async fn idempotent_retry_reuses_result_without_second_execution() {
    let posts = Arc::new(AtomicUsize::new(0));
    let executions = Arc::new(AtomicUsize::new(0));
    let seen = Arc::new(Mutex::new(None::<(String, Value)>));
    let handler = {
        let posts = posts.clone();
        let executions = executions.clone();
        let seen = seen.clone();
        move |request: MockRequest| {
            posts.fetch_add(1, Ordering::SeqCst);
            let key = request
                .headers
                .get(IDEMPOTENCY_KEY_HEADER)
                .unwrap()
                .to_str()
                .unwrap()
                .to_string();
            let mut seen = seen.lock().unwrap();
            let action = match seen.as_ref() {
                Some((original_key, original_body))
                    if original_key == &key && original_body == &request.body =>
                {
                    0
                }
                Some((original_key, _)) if original_key == &key => 1,
                _ => 2,
            };
            if action == 1 {
                return MockReply::json(json!({
                    "error": {
                        "type": "invalid_request_error",
                        "code": "idempotency_key_reused",
                        "message": "different request"
                    }
                }))
                .status(StatusCode::CONFLICT);
            }
            if action == 2 {
                executions.fetch_add(1, Ordering::SeqCst);
                *seen = Some((key, request.body));
            }
            MockReply::json(response_json("resp-idempotent"))
        }
    };
    let server = MockServer::start(handler).await;
    let client = Client::new(&server.base_url, None).unwrap();
    let request = CreateResponse::new("one expensive task", "harness-1");
    let first = client
        .create_response(&request, &key("stable-key"), &scope())
        .await
        .unwrap();
    let retry = client
        .create_response(&request, &key("stable-key"), &scope())
        .await
        .unwrap();

    assert_eq!(first.id, retry.id);
    assert_eq!(posts.load(Ordering::SeqCst), 2);
    assert_eq!(executions.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn reads_stored_response() {
    let server = MockServer::start(|request| {
        assert_eq!(request.method, Method::GET);
        assert_eq!(request.uri.path(), "/api/harness/v1/responses/resp-stored");
        MockReply::json(response_json("resp-stored"))
    })
    .await;
    let client = Client::new(&server.base_url, None).unwrap();
    let response = client.get_response("resp-stored").await.unwrap();
    assert_eq!(response.id, "resp-stored");
    assert!(response.store);
}

#[tokio::test]
async fn tolerates_unknown_fields_output_items_and_content_parts() {
    let mut payload = response_json("resp-additive");
    payload["future_response_field"] = json!({"nested": true});
    payload["output"] = json!([
        {"type": "future_output", "private_future_data": "ignored"},
        {"id": "rs-1", "type": "reasoning", "summary": [{"text": "private"}]},
        {
            "id": "msg-additive",
            "type": "message",
            "role": "assistant",
            "status": "completed",
            "future_message_field": true,
            "content": [
                {"type": "future_part", "text": "ignored"},
                {"type": "output_text", "text": "public", "future_part_field": 1}
            ]
        }
    ]);
    let server = MockServer::start(move |_| MockReply::json(payload.clone())).await;
    let client = Client::new(&server.base_url, None).unwrap();
    let response = client.get_response("resp-additive").await.unwrap();
    assert_eq!(response.output.len(), 3);
    assert_eq!(completed_assistant_bodies(&response), ["public"]);
}

#[tokio::test]
async fn public_filter_excludes_private_incomplete_and_non_assistant_output() {
    let mut payload = response_json("resp-private");
    payload["output"] = json!([
        {"id": "rs", "type": "reasoning", "summary": [{"type": "summary_text", "text": "secret reasoning"}]},
        {"id": "fc", "type": "function_call", "name": "read_file", "arguments": "secret args"},
        {"id": "fco", "type": "function_call_output", "output": "secret result"},
        {"id": "incomplete", "type": "message", "role": "assistant", "status": "in_progress", "content": [{"type": "output_text", "text": "partial"}]},
        {"id": "user", "type": "message", "role": "user", "status": "completed", "content": [{"type": "output_text", "text": "user body"}]},
        {"id": "empty", "type": "message", "role": "assistant", "status": "completed", "content": [{"type": "output_text", "text": "  "}]},
        {"id": "safe", "type": "message", "role": "assistant", "status": "completed", "content": [{"type": "output_text", "text": "first "}, {"type": "output_text", "text": "second"}]}
    ]);
    let server = MockServer::start(move |_| MockReply::json(payload.clone())).await;
    let client = Client::new(&server.base_url, None).unwrap();
    let response = client.get_response("resp-private").await.unwrap();
    assert_eq!(completed_assistant_bodies(&response), ["first second"]);

    let mut failed = response;
    failed.status = refact_uhp::ResponseStatus::Failed;
    assert!(completed_assistant_bodies(&failed).is_empty());
}

#[tokio::test]
async fn fingerprint_conflict_blocks_changed_request_before_transport() {
    let posts = Arc::new(AtomicUsize::new(0));
    let handler_posts = posts.clone();
    let server = MockServer::start(move |_| {
        handler_posts.fetch_add(1, Ordering::SeqCst);
        MockReply::json(response_json("resp-original"))
    })
    .await;
    let client = Client::new(&server.base_url, None).unwrap();
    let original = CreateResponse::new("secret prompt canary", "harness-1")
        .model("model-1")
        .environment("env-1");
    let idempotency_key = key("same-key");
    client
        .create_response(&original, &idempotency_key, &scope())
        .await
        .unwrap();
    let fingerprint = original.fingerprint(&scope()).unwrap();
    assert_eq!(fingerprint.as_str().len(), 64);
    assert!(!fingerprint.as_str().contains("secret"));
    assert!(!fingerprint.as_str().contains("canary"));

    let changed = vec![
        CreateResponse::new("changed prompt", "harness-1")
            .model("model-1")
            .environment("env-1"),
        CreateResponse::new("secret prompt canary", "harness-1")
            .model("model-2")
            .environment("env-1"),
        CreateResponse::new("secret prompt canary", "harness-1")
            .model("model-1")
            .environment("env-1")
            .previous_response_id("resp-previous"),
        CreateResponse::new("secret prompt canary", "harness-1")
            .model("model-1")
            .environment("env-2"),
    ];
    for request in changed {
        assert!(matches!(
            client
                .create_response(&request, &idempotency_key, &scope())
                .await,
            Err(UhpError::FingerprintConflict { .. })
        ));
    }
    assert_eq!(posts.load(Ordering::SeqCst), 1);

    let left = CreateResponse::new("same", "harness-1")
        .metadata("z", json!(1))
        .metadata("a", json!({"b": 2, "a": 1}));
    let right = CreateResponse::new("same", "harness-1")
        .metadata("a", json!({"a": 1, "b": 2}))
        .metadata("z", json!(1));
    assert_eq!(
        left.fingerprint(&scope()).unwrap(),
        right.fingerprint(&scope()).unwrap()
    );
}

#[tokio::test]
async fn typed_server_error_preserves_error_envelope() {
    let server = MockServer::start(|_| {
        MockReply::json(json!({
            "error": {
                "type": "invalid_request_error",
                "code": "idempotency_key_reused",
                "message": "key already has another body",
                "param": null,
                "detail": {"request_id": "req-1"},
                "unknown_error_field": true
            }
        }))
        .status(StatusCode::CONFLICT)
    })
    .await;
    let client = Client::new(&server.base_url, None).unwrap();
    let error = client
        .create_response(
            &CreateResponse::new("task", "harness-1"),
            &key("key-1"),
            &scope(),
        )
        .await
        .unwrap_err();
    match error {
        UhpError::Server { status, error } => {
            assert_eq!(status, 409);
            assert_eq!(error.error_type, "invalid_request_error");
            assert_eq!(error.code, "idempotency_key_reused");
        }
        other => panic!("unexpected error: {other:?}"),
    }
}

#[tokio::test]
async fn malformed_success_schema_is_not_a_success() {
    let server = MockServer::start(|_| MockReply::json(json!({"object": "response"}))).await;
    let client = Client::new(&server.base_url, None).unwrap();
    assert!(matches!(
        client.get_response("resp-broken").await,
        Err(UhpError::Schema { .. })
    ));
}

#[tokio::test]
async fn transport_failure_is_typed() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let client = Client::new(&format!("http://{address}"), None).unwrap();
    assert!(matches!(
        client.discover().await,
        Err(UhpError::Transport { .. })
    ));
}
