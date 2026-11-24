use std::sync::{Arc, Mutex};

use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, Uri};
use axum::routing::any;
use axum::Router;
use refact_uhp::{
    completed_assistant_bodies, Client, CreateResponse, IdempotencyKey, OutputItem, RequestScope,
    ResponseStatus, StreamEventKind, StreamFailureKind, IDEMPOTENCY_KEY_HEADER, UHP_VERSION,
    UHP_VERSION_HEADER,
};
use serde_json::{json, Value};
use tokio::sync::oneshot;

struct MockRequest {
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Value,
}

struct MockServer {
    base_url: String,
    shutdown: Option<oneshot::Sender<()>>,
}

impl MockServer {
    async fn start(body: String, captured: Arc<Mutex<Vec<MockRequest>>>) -> Self {
        let app = Router::new().fallback(any(
            move |method: Method, uri: Uri, headers: HeaderMap, request_body: String| {
                let response_body = body.clone();
                let captured = captured.clone();
                async move {
                    captured.lock().unwrap().push(MockRequest {
                        method,
                        uri,
                        headers,
                        body: serde_json::from_str(&request_body).unwrap_or(Value::Null),
                    });
                    let mut headers = HeaderMap::new();
                    headers.insert(UHP_VERSION_HEADER, HeaderValue::from_static(UHP_VERSION));
                    headers.insert(
                        axum::http::header::CONTENT_TYPE,
                        HeaderValue::from_static("text/event-stream"),
                    );
                    (StatusCode::OK, headers, response_body)
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

fn response_json(id: &str, status: &str) -> Value {
    let completed = status == "completed";
    json!({
        "id": id,
        "object": "response",
        "created_at": 1_786_400_000u64,
        "status": status,
        "error": if status == "failed" { json!({"type":"harness_error","code":"harness_error","message":"failed"}) } else { Value::Null },
        "incomplete_details": null,
        "previous_response_id": null,
        "model": "model-1",
        "output": if completed { json!([{
            "id": "msg-1",
            "type": "message",
            "role": "assistant",
            "status": "completed",
            "content": [{"type": "output_text", "text": "terminal public body", "annotations": []}]
        }]) } else { json!([]) },
        "store": true,
        "usage": null,
        "metadata": {"session_id": "session-1", "harness_id": "harness-1"}
    })
}

fn event(sequence_number: u64, event_type: &str, response: Value) -> String {
    format!(
        "data: {}\n\n",
        json!({"type": event_type, "sequence_number": sequence_number, "response": response})
    )
}

fn scope() -> RequestScope {
    RequestScope::new("participant-1", "credential-profile-1")
}

fn key(value: &str) -> IdempotencyKey {
    IdempotencyKey::new(value).unwrap()
}

#[tokio::test]
async fn streaming_accepts_progress_unknowns_items_and_one_terminal() {
    let mut body = event(
        0,
        "response.created",
        response_json("resp-stream", "in_progress"),
    );
    body.push_str(
        "data: {\"type\":\"response.future\",\"sequence_number\":1,\"additive\":true}\n\n",
    );
    body.push_str("data: {\"type\":\"response.output_item.added\",\"sequence_number\":2,\"output_index\":0,\"item\":{\"type\":\"future_item\",\"private\":true}}\n\n");
    body.push_str("data: {\"type\":\"response.output_text.delta\",\"sequence_number\":3,\"item_id\":\"msg-1\",\"output_index\":0,\"content_index\":0,\"delta\":\"private partial\"}\n\n");
    body.push_str("data: {\"type\":\"response.output_item.done\",\"sequence_number\":4,\"output_index\":0,\"item\":{\"id\":\"msg-1\",\"type\":\"message\",\"role\":\"assistant\",\"status\":\"completed\",\"content\":[{\"type\":\"output_text\",\"text\":\"terminal public body\"}],\"future\":1}}\n\n");
    body.push_str("data: {\"type\":\"error\",\"sequence_number\":5,\"code\":\"temporary\",\"message\":\"still running\",\"param\":null}\n\n");
    body.push_str(&event(
        6,
        "response.completed",
        response_json("resp-stream", "completed"),
    ));
    let captured = Arc::new(Mutex::new(Vec::new()));
    let server = MockServer::start(body, captured.clone()).await;
    let client = Client::new(&server.base_url, None).unwrap();
    let mut stream = client
        .create_response_stream(
            &CreateResponse::new("review", "harness-1").streaming(),
            &key("stream-key"),
            &scope(),
        )
        .await
        .unwrap();

    let mut events = Vec::new();
    while let Some(event) = stream.next_event().await.unwrap() {
        events.push(event);
    }
    assert_eq!(events.len(), 7);
    assert!(matches!(events[0].kind, StreamEventKind::Created(_)));
    assert!(matches!(
        &events[1].kind,
        StreamEventKind::Other { event_type } if event_type == "response.future"
    ));
    assert!(matches!(
        events[2].kind,
        StreamEventKind::OutputItemAdded {
            item: OutputItem::Unknown,
            ..
        }
    ));
    assert!(matches!(
        &events[3].kind,
        StreamEventKind::OutputTextDelta { delta, .. } if delta == "private partial"
    ));
    assert!(matches!(
        events[4].kind,
        StreamEventKind::OutputItemDone {
            item: OutputItem::Message(_),
            ..
        }
    ));
    assert!(matches!(
        events[5].kind,
        StreamEventKind::NonTerminalError(_)
    ));
    let StreamEventKind::Terminal(response) = &events[6].kind else {
        panic!("last event must be terminal")
    };
    assert_eq!(response.status, ResponseStatus::Completed);
    assert_eq!(
        completed_assistant_bodies(response),
        ["terminal public body"]
    );

    let captured = captured.lock().unwrap();
    assert_eq!(captured.len(), 1);
    assert_eq!(captured[0].method, Method::POST);
    assert_eq!(captured[0].uri.path(), "/api/harness/v1/responses");
    assert_eq!(captured[0].body["stream"], true);
    assert_eq!(captured[0].body["store"], true);
    assert_eq!(
        captured[0].headers.get(IDEMPOTENCY_KEY_HEADER).unwrap(),
        "stream-key"
    );
}

#[tokio::test]
async fn streaming_sequence_gap_is_not_success_and_preserves_recovery_id() {
    let mut body = event(
        0,
        "response.created",
        response_json("resp-gap", "in_progress"),
    );
    body.push_str("data: {\"type\":\"response.output_text.delta\",\"sequence_number\":2,\"item_id\":\"msg-1\",\"output_index\":0,\"content_index\":0,\"delta\":\"lost prefix\"}\n\n");
    let server = MockServer::start(body, Arc::new(Mutex::new(Vec::new()))).await;
    let client = Client::new(&server.base_url, None).unwrap();
    let stream = client
        .create_response_stream(
            &CreateResponse::new("review", "harness-1").streaming(),
            &key("gap-key"),
            &scope(),
        )
        .await
        .unwrap();
    let error = stream.complete().await.unwrap_err();
    assert_eq!(error.response_id.as_deref(), Some("resp-gap"));
    assert!(matches!(
        error.kind,
        StreamFailureKind::SequenceGap {
            expected: 1,
            actual: 2
        }
    ));
}

#[tokio::test]
async fn streaming_missing_terminal_is_not_success() {
    let body = event(
        0,
        "response.created",
        response_json("resp-open", "in_progress"),
    );
    let server = MockServer::start(body, Arc::new(Mutex::new(Vec::new()))).await;
    let client = Client::new(&server.base_url, None).unwrap();
    let stream = client
        .create_response_stream(
            &CreateResponse::new("review", "harness-1").streaming(),
            &key("open-key"),
            &scope(),
        )
        .await
        .unwrap();
    let error = stream.complete().await.unwrap_err();
    assert_eq!(error.response_id.as_deref(), Some("resp-open"));
    assert!(matches!(error.kind, StreamFailureKind::MissingTerminal));
}

#[tokio::test]
async fn streaming_requires_created_first_and_terminal_last() {
    let body = event(
        0,
        "response.completed",
        response_json("resp-early", "completed"),
    );
    let server = MockServer::start(body, Arc::new(Mutex::new(Vec::new()))).await;
    let client = Client::new(&server.base_url, None).unwrap();
    let stream = client
        .create_response_stream(
            &CreateResponse::new("review", "harness-1").streaming(),
            &key("early-key"),
            &scope(),
        )
        .await
        .unwrap();
    assert!(matches!(
        stream.complete().await.unwrap_err().kind,
        StreamFailureKind::MissingCreated
    ));

    let mut body = event(
        0,
        "response.created",
        response_json("resp-extra", "in_progress"),
    );
    body.push_str(&event(
        1,
        "response.completed",
        response_json("resp-extra", "completed"),
    ));
    body.push_str("data: {\"type\":\"response.future\",\"sequence_number\":2}\n\n");
    let server = MockServer::start(body, Arc::new(Mutex::new(Vec::new()))).await;
    let client = Client::new(&server.base_url, None).unwrap();
    let stream = client
        .create_response_stream(
            &CreateResponse::new("review", "harness-1").streaming(),
            &key("extra-key"),
            &scope(),
        )
        .await
        .unwrap();
    assert!(matches!(
        stream.complete().await.unwrap_err().kind,
        StreamFailureKind::EventAfterTerminal
    ));
}

#[tokio::test]
async fn streaming_malformed_json_permanently_poisons_the_stream() {
    let mut body = event(
        0,
        "response.created",
        response_json("resp-malformed", "in_progress"),
    );
    body.push_str("data: {not-json}\n\n");
    body.push_str(&event(
        2,
        "response.completed",
        response_json("resp-malformed", "completed"),
    ));
    let server = MockServer::start(body, Arc::new(Mutex::new(Vec::new()))).await;
    let client = Client::new(&server.base_url, None).unwrap();
    let mut stream = client
        .create_response_stream(
            &CreateResponse::new("review", "harness-1").streaming(),
            &key("malformed-key"),
            &scope(),
        )
        .await
        .unwrap();
    assert!(matches!(
        stream.next_event().await.unwrap().unwrap().kind,
        StreamEventKind::Created(_)
    ));
    let error = stream.next_event().await.unwrap_err();
    assert_eq!(error.response_id.as_deref(), Some("resp-malformed"));
    assert!(matches!(error.kind, StreamFailureKind::MalformedJson(_)));
    assert!(stream.next_event().await.unwrap().is_none());
}

#[tokio::test]
async fn streaming_cancelled_is_determined_by_nested_response_status() {
    let mut body = event(
        0,
        "response.created",
        response_json("resp-cancel", "in_progress"),
    );
    body.push_str(&event(
        1,
        "response.failed",
        response_json("resp-cancel", "cancelled"),
    ));
    let server = MockServer::start(body, Arc::new(Mutex::new(Vec::new()))).await;
    let client = Client::new(&server.base_url, None).unwrap();
    let response = client
        .create_response_stream(
            &CreateResponse::new("review", "harness-1").streaming(),
            &key("cancel-stream-key"),
            &scope(),
        )
        .await
        .unwrap()
        .complete()
        .await
        .unwrap();
    assert_eq!(response.status, ResponseStatus::Cancelled);
    assert!(completed_assistant_bodies(&response).is_empty());
}

#[tokio::test]
async fn streaming_dropping_client_does_not_send_cancellation() {
    let body = event(
        0,
        "response.created",
        response_json("resp-drop", "in_progress"),
    );
    let captured = Arc::new(Mutex::new(Vec::new()));
    let server = MockServer::start(body, captured.clone()).await;
    let client = Client::new(&server.base_url, None).unwrap();
    let stream = client
        .create_response_stream(
            &CreateResponse::new("review", "harness-1").streaming(),
            &key("drop-key"),
            &scope(),
        )
        .await
        .unwrap();
    drop(stream);
    assert_eq!(captured.lock().unwrap().len(), 1);
}
