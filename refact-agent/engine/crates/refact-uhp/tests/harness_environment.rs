use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, Uri};
use axum::routing::any;
use axum::Router;
use futures::stream;
use refact_uhp::{
    Client, CreateResponse, Discovery, EnvironmentBuild, EnvironmentBuildStatus, EnvironmentConfig,
    EnvironmentGitImport, EnvironmentStatus, HarnessConfig, IdempotencyKey, RequestScope, UhpError,
    IDEMPOTENCY_KEY_HEADER, UHP_VERSION, UHP_VERSION_HEADER,
};
use serde_json::{json, Value};
use tokio::sync::oneshot;

struct MockRequest {
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Vec<u8>,
}

impl MockRequest {
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap()
    }
}

struct MockReply {
    status: StatusCode,
    body: Vec<u8>,
    headers: HeaderMap,
}

impl MockReply {
    fn json(body: Value) -> Self {
        Self {
            status: StatusCode::OK,
            body: serde_json::to_vec(&body).unwrap(),
            headers: HeaderMap::new(),
        }
    }

    fn status(mut self, status: StatusCode) -> Self {
        self.status = status;
        self
    }

    fn header(mut self, name: &'static str, value: &'static str) -> Self {
        self.headers.insert(name, HeaderValue::from_static(value));
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
            move |method: Method, uri: Uri, headers: HeaderMap, body: Bytes| {
                let handler = handler.clone();
                async move {
                    let mut reply = handler(MockRequest {
                        method,
                        uri,
                        headers,
                        body: body.to_vec(),
                    });
                    reply
                        .headers
                        .insert(UHP_VERSION_HEADER, HeaderValue::from_static(UHP_VERSION));
                    (reply.status, reply.headers, reply.body)
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

fn discovery(capabilities: Value) -> Discovery {
    serde_json::from_value(json!({
        "object": "uhp.discovery",
        "protocol": "uhp",
        "versions": [UHP_VERSION],
        "default_version": UHP_VERSION,
        "conformance_class": "full",
        "capabilities": capabilities,
        "implementation": {"name": "mock", "version": "1"}
    }))
    .unwrap()
}

fn full_discovery() -> Discovery {
    discovery(json!({
        "harness_management": true,
        "harness_idempotency": true,
        "harness_retirement": true,
        "environments": true
    }))
}

fn scope() -> RequestScope {
    RequestScope::new("participant-1", "credential-profile-1")
}

fn key(value: &str) -> IdempotencyKey {
    IdempotencyKey::new(value).unwrap()
}

fn harness_json() -> Value {
    json!({
        "id": "chrn_1",
        "object": "harness",
        "name": "Reviewer",
        "base": "opaque-base",
        "baseLabel": "Opaque",
        "defaultModel": "model-1",
        "environment": "henv_1",
        "disabledTools": ["shell"],
        "createdAt": 1,
        "futureHarnessField": {"preserved": true}
    })
}

fn environment_json(status: &str, active: Option<u64>) -> Value {
    json!({
        "id": "henv_1",
        "object": "environment",
        "name": "Review snapshot",
        "slug": "review-snapshot",
        "description": "caller supplied",
        "entry": "review",
        "status": status,
        "mount": "/env/review-snapshot",
        "version": active,
        "latestVersion": 2,
        "files": {"count": 2, "bytes": 7},
        "packages": [{"manager": "npm", "name": "reviewer", "version": "1.0.0"}],
        "declared": {"npm": [{"spec": "reviewer@1.0.0", "name": "reviewer", "version": "1.0.0"}]},
        "runtime": {"python": "3.12"},
        "versions": [
            {"version": 1, "status": "ready", "started_at": 1, "finished_at": 2, "error": ""},
            {"version": 2, "status": "building", "started_at": 3, "finished_at": null}
        ],
        "build": {"version": 2, "status": "building", "stage": "copying files"},
        "createdAt": 1,
        "updatedAt": 3,
        "futureEnvironmentField": [1, 2, 3]
    })
}

fn build_json(status: &str) -> Value {
    json!({
        "id": "henv_1",
        "object": "environment.build",
        "version": 2,
        "status": status,
        "started_at": 3,
        "finished_at": if status == "building" { Value::Null } else { json!(4) },
        "error": "",
        "stage": if status == "building" { "copying files" } else { "" },
        "log": "build log",
        "packages": [{"manager": "npm", "name": "reviewer", "version": "1.0.0"}],
        "files": 2,
        "bytes": 7,
        "futureBuildField": true
    })
}

fn response_json() -> Value {
    json!({
        "id": "resp_1",
        "object": "response",
        "created_at": 1,
        "status": "completed",
        "model": "model-1",
        "output": [],
        "store": true,
        "metadata": {"harness_id": "chrn_1", "environment": "henv_1"}
    })
}

#[test]
fn harness_environment_dto_tolerates_unknown_build_status() {
    let build: EnvironmentBuild = serde_json::from_value(json!({
        "version": 9,
        "status": "future_status",
        "future": {"preserved": true}
    }))
    .unwrap();
    assert_eq!(build.status, EnvironmentBuildStatus::Unknown);
    assert_eq!(build.extra["future"], json!({"preserved": true}));
}

#[tokio::test]
async fn harness_environment_discovers_harnesses_and_model_availability() {
    let server = MockServer::start(|request| match request.uri.path() {
        "/api/harness/v1/harnesses" => MockReply::json(json!({
            "harnesses": [harness_json()],
            "futureListField": true
        })),
        "/api/harness/v1/harnesses/chrn_1" => {
            MockReply::json(harness_json()).header("etag", "\"harness-v1\"")
        }
        "/api/harness/v1/models" => MockReply::json(json!({
            "backends": {
                "backend-1": {
                    "default": "model-1",
                    "models": [{"id": "model-1", "available": true, "default": true}]
                }
            },
            "futureCatalogField": true
        })),
        "/api/harness/v1/harnesses/chrn_1/models" => MockReply::json(json!({
            "harness_id": "chrn_1",
            "backend": "backend-1",
            "default": "model-1",
            "fallback": "model-1",
            "models": [
                {"id": "model-1", "available": true, "default": true, "futureModelField": 1},
                {"id": "model-2", "available": false, "default": false}
            ]
        })),
        "/api/harness/v1/harnesses/chrn_other/models" => MockReply::json(json!({
            "harness_id": "chrn_1",
            "models": [{"id": "model-1", "available": true}]
        })),
        path => panic!("unexpected path {path}"),
    })
    .await;
    let client = Client::new(&server.base_url, None).unwrap();

    let harnesses = client.list_harnesses().await.unwrap();
    assert_eq!(harnesses.len(), 1);
    assert_eq!(harnesses[0].base, "opaque-base");
    assert_eq!(
        harnesses[0].extra["futureHarnessField"],
        json!({"preserved": true})
    );
    let versioned = client.get_harness("chrn_1").await.unwrap();
    assert_eq!(versioned.etag.as_deref(), Some("\"harness-v1\""));
    let catalog = client.list_models().await.unwrap();
    assert_eq!(catalog.extra["futureCatalogField"], json!(true));
    assert_eq!(catalog.backends["backend-1"].models[0].id, "model-1");
    let models = client.list_harness_models("chrn_1").await.unwrap();
    assert_eq!(models.default_model.as_deref(), Some("model-1"));
    assert_eq!(models.fallback.as_deref(), Some("model-1"));
    assert_eq!(
        models.model("model-1").unwrap().extra["futureModelField"],
        json!(1)
    );
    client
        .require_harness_model("chrn_1", "model-1")
        .await
        .unwrap();
    assert!(matches!(
        client.require_harness_model("chrn_1", "model-2").await,
        Err(UhpError::ModelUnavailable { model, .. }) if model == "model-2"
    ));
    assert!(matches!(
        client.require_harness_model("chrn_other", "model-1").await,
        Err(UhpError::InvalidResponse {
            operation: "list harness models",
            ..
        })
    ));
}

#[tokio::test]
async fn harness_environment_refuses_unadvertised_capabilities_before_transport() {
    let requests = Arc::new(AtomicUsize::new(0));
    let captured = requests.clone();
    let server = MockServer::start(move |_| {
        captured.fetch_add(1, Ordering::SeqCst);
        MockReply::json(json!({}))
    })
    .await;
    let client = Client::new(&server.base_url, None).unwrap();
    let config = HarnessConfig::new("opaque-base").environment("henv_1");

    for result in [
        client
            .create_harness(&discovery(json!({})), &config, &key("create"), &scope())
            .await
            .map(|_| ()),
        client
            .update_harness(&discovery(json!({})), "chrn_1", &config, None)
            .await
            .map(|_| ()),
        client
            .retire_harness(&discovery(json!({})), "chrn_1")
            .await
            .map(|_| ()),
        client
            .create_environment(&discovery(json!({})), &EnvironmentConfig::new("snapshot"))
            .await
            .map(|_| ()),
        client
            .list_environments(&discovery(json!({})))
            .await
            .map(|_| ()),
    ] {
        assert!(matches!(
            result,
            Err(UhpError::CapabilityUnsupported { .. })
        ));
    }
    let management_only = discovery(json!({"harness_management": true}));
    assert!(matches!(
        client
            .create_harness(&management_only, &config, &key("idempotency"), &scope())
            .await,
        Err(UhpError::CapabilityUnsupported {
            capability: "harness_idempotency",
            ..
        })
    ));
    let without_environments = discovery(json!({
        "harness_management": true,
        "harness_idempotency": true
    }));
    assert!(matches!(
        client
            .create_harness(
                &without_environments,
                &config,
                &key("environment"),
                &scope()
            )
            .await,
        Err(UhpError::CapabilityUnsupported {
            capability: "environments",
            ..
        })
    ));
    assert_eq!(requests.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn harness_environment_create_is_idempotent_in_a_separate_namespace() {
    let posts = Arc::new(AtomicUsize::new(0));
    let executions = Arc::new(AtomicUsize::new(0));
    let seen = Arc::new(Mutex::new(None::<Value>));
    let captured_posts = posts.clone();
    let captured_executions = executions.clone();
    let captured_seen = seen.clone();
    let server = MockServer::start(move |request| {
        assert_eq!(
            request.headers.get(IDEMPOTENCY_KEY_HEADER).unwrap(),
            "shared-key"
        );
        match request.uri.path() {
            "/api/harness/v1/harnesses" => {
                captured_posts.fetch_add(1, Ordering::SeqCst);
                let body = request.json();
                let mut seen = captured_seen.lock().unwrap();
                if seen.as_ref() == Some(&body) {
                    return MockReply::json(harness_json());
                }
                if seen.is_some() {
                    return MockReply::json(json!({
                        "error": {"type": "invalid_request_error", "code": "idempotency_key_reused", "message": "changed"}
                    }))
                    .status(StatusCode::CONFLICT);
                }
                *seen = Some(body);
                captured_executions.fetch_add(1, Ordering::SeqCst);
                MockReply::json(harness_json())
            }
            "/api/harness/v1/responses" => MockReply::json(response_json()),
            path => panic!("unexpected path {path}"),
        }
    })
    .await;
    let client = Client::new(&server.base_url, None).unwrap();
    let config = HarnessConfig::new("opaque-base")
        .name("Reviewer")
        .default_model("model-1");
    let shared_key = key("shared-key");

    client
        .create_response(
            &CreateResponse::new("review", "chrn_1"),
            &shared_key,
            &scope(),
        )
        .await
        .unwrap();
    let first = client
        .create_harness(&full_discovery(), &config, &shared_key, &scope())
        .await
        .unwrap();
    let retry = client
        .create_harness(&full_discovery(), &config, &shared_key, &scope())
        .await
        .unwrap();
    assert_eq!(first.id, retry.id);
    assert_eq!(posts.load(Ordering::SeqCst), 2);
    assert_eq!(executions.load(Ordering::SeqCst), 1);
    assert!(matches!(
        client
            .create_harness(
                &full_discovery(),
                &HarnessConfig::new("changed-base"),
                &shared_key,
                &scope()
            )
            .await,
        Err(UhpError::FingerprintConflict { .. })
    ));
}

#[tokio::test]
async fn harness_environment_maps_unsupported_base_and_update_conflicts() {
    let server = MockServer::start(|request| match (request.method.clone(), request.uri.path()) {
        (Method::POST, "/api/harness/v1/harnesses") => MockReply::json(json!({
            "error": {"type": "invalid_request_error", "code": "unsupported_base", "message": "no"}
        }))
        .status(StatusCode::UNPROCESSABLE_ENTITY),
        (Method::PUT, "/api/harness/v1/harnesses/chrn_1") => {
            if request.headers.get("if-match").unwrap() == "\"stale\"" {
                MockReply::json(json!({
                    "error": {"type": "invalid_request_error", "code": "precondition_failed", "message": "stale"}
                }))
                .status(StatusCode::PRECONDITION_FAILED)
            } else {
                MockReply::json(harness_json()).header("etag", "\"harness-v2\"")
            }
        }
        (Method::DELETE, "/api/harness/v1/harnesses/chrn_1") => MockReply::json(json!({
            "id": "chrn_1",
            "deleted": true,
            "sessions": "retired",
            "restorableUntil": 1234,
            "futureRetirementField": true
        })),
        value => panic!("unexpected request {value:?}"),
    })
    .await;
    let client = Client::new(&server.base_url, None).unwrap();
    let config = HarnessConfig::new("opaque-base");
    assert!(matches!(
        client
            .create_harness(&full_discovery(), &config, &key("base"), &scope())
            .await,
        Err(UhpError::UnsupportedBase { base }) if base == "opaque-base"
    ));
    assert!(matches!(
        client
            .update_harness(&full_discovery(), "chrn_1", &config, Some("\"stale\""))
            .await,
        Err(UhpError::HarnessPreconditionFailed { harness_id }) if harness_id == "chrn_1"
    ));
    let updated = client
        .update_harness(&full_discovery(), "chrn_1", &config, Some("\"harness-v1\""))
        .await
        .unwrap();
    assert_eq!(updated.etag.as_deref(), Some("\"harness-v2\""));
    let retired = client
        .retire_harness(&full_discovery(), "chrn_1")
        .await
        .unwrap();
    assert_eq!(retired.restorable_until, Some(1234));
    assert_eq!(retired.extra["futureRetirementField"], json!(true));
}

#[tokio::test]
async fn harness_environment_streams_import_builds_and_activates_versions() {
    let imported = Arc::new(Mutex::new(Vec::new()));
    let captured = imported.clone();
    let server = MockServer::start(move |request| {
        let path = request.uri.path();
        match (request.method.clone(), path) {
            (Method::POST, "/api/harness/v1/environments") => {
                let body = request.json();
                assert_eq!(body["name"], "Review snapshot");
                assert_eq!(body["description"], "caller supplied");
                MockReply::json(environment_json("empty", None))
            }
            (Method::GET, "/api/harness/v1/environments") => MockReply::json(json!({
                "environments": [environment_json("ready", Some(1))]
            })),
            (Method::POST, "/api/harness/v1/environments/henv_1/import") => {
                assert_eq!(request.uri.query(), Some("replace=1"));
                assert_eq!(
                    request.headers.get("content-type").unwrap(),
                    "application/zip"
                );
                captured.lock().unwrap().push(request.body);
                MockReply::json(json!({
                    "written": 2,
                    "skipped": 1,
                    "count": 2,
                    "bytes": 7,
                    "futureImportField": "kept"
                }))
            }
            (Method::POST, "/api/harness/v1/environments/henv_1/build") => {
                MockReply::json(build_json("building"))
            }
            (Method::GET, "/api/harness/v1/environments/henv_1/builds/2") => {
                MockReply::json(build_json("ready"))
            }
            (Method::GET, "/api/harness/v1/environments/henv_1/versions") => {
                MockReply::json(json!({
                    "id": "henv_1",
                    "active": 2,
                    "versions": [
                        {"version": 1, "status": "ready"},
                        {"version": 2, "status": "ready"}
                    ],
                    "futureVersionsField": true
                }))
            }
            (Method::POST, "/api/harness/v1/environments/henv_1/versions/1/activate") => {
                let mut environment = environment_json("ready", Some(1));
                environment["versions"][1]["status"] = json!("ready");
                MockReply::json(environment)
            }
            (Method::GET, "/api/harness/v1/environments/henv_1") => {
                MockReply::json(environment_json("building", Some(1)))
            }
            value => panic!("unexpected request {value:?}"),
        }
    })
    .await;
    let client = Client::new(&server.base_url, None).unwrap();
    let discovery = full_discovery();

    let created = client
        .create_environment(
            &discovery,
            &EnvironmentConfig::new("Review snapshot").description("caller supplied"),
        )
        .await
        .unwrap();
    assert_eq!(created.status, EnvironmentStatus::Empty);
    assert!(matches!(
        created.ensure_ready(),
        Err(UhpError::EnvironmentNotReady { .. })
    ));
    assert_eq!(client.list_environments(&discovery).await.unwrap().len(), 1);
    let bytes = stream::iter(vec![Ok(vec![0, 1, 2]), Ok(vec![3, 4, 5, 6])]);
    let imported_result = client
        .import_environment_stream(
            &discovery,
            "henv_1",
            Box::pin(bytes),
            "application/zip",
            true,
        )
        .await
        .unwrap();
    assert_eq!(imported_result.skipped, Some(1));
    assert_eq!(imported_result.extra["futureImportField"], json!("kept"));
    assert_eq!(
        imported.lock().unwrap().as_slice(),
        [vec![0, 1, 2, 3, 4, 5, 6]]
    );

    let started = client
        .build_environment(&discovery, "henv_1")
        .await
        .unwrap();
    assert_eq!(started.status, EnvironmentBuildStatus::Building);
    assert_eq!(started.extra["futureBuildField"], json!(true));
    let finished = client
        .get_environment_build(&discovery, "henv_1", 2)
        .await
        .unwrap();
    assert_eq!(finished.status, EnvironmentBuildStatus::Ready);
    let versions = client
        .list_environment_versions(&discovery, "henv_1")
        .await
        .unwrap();
    assert_eq!(versions.active, Some(2));
    assert_eq!(versions.extra["futureVersionsField"], json!(true));
    let activated = client
        .activate_environment_version(&discovery, "henv_1", 1)
        .await
        .unwrap();
    assert_eq!(activated.version, Some(1));
    let building_with_ready_active = client
        .get_ready_environment(&discovery, "henv_1")
        .await
        .unwrap();
    assert_eq!(
        building_with_ready_active.status,
        EnvironmentStatus::Building
    );
}

#[tokio::test]
async fn harness_environment_imports_git_and_rejects_json_archive_content_type() {
    let server = MockServer::start(|request| {
        assert_eq!(
            request.uri.path(),
            "/api/harness/v1/environments/henv_1/import"
        );
        let body = request.json();
        assert_eq!(body["git"]["url"], "https://example.invalid/repo");
        assert_eq!(body["git"]["ref"], "main");
        assert_eq!(body["replace"], true);
        MockReply::json(json!({"written": 1}))
    })
    .await;
    let client = Client::new(&server.base_url, None).unwrap();
    client
        .import_environment_git(
            &full_discovery(),
            "henv_1",
            &EnvironmentGitImport::new("https://example.invalid/repo")
                .reference("main")
                .replace(),
        )
        .await
        .unwrap();
    assert!(matches!(
        client
            .import_environment_bytes(
                &full_discovery(),
                "henv_1",
                b"not used".to_vec(),
                "application/json",
                false
            )
            .await,
        Err(UhpError::InvalidResponse {
            operation: "import environment archive",
            ..
        })
    ));
}

#[tokio::test]
async fn harness_environment_preserves_typed_not_ready_busy_and_unavailable_errors() {
    let server = MockServer::start(|request| {
        let code = match request.uri.path() {
            "/api/harness/v1/environments/henv_1/build" => "environment_busy",
            "/api/harness/v1/environments/henv_1/versions/2/activate" => "environment_not_ready",
            "/api/harness/v1/environments/henv_1" => "environment_unavailable",
            path => panic!("unexpected path {path}"),
        };
        let status = if code == "environment_unavailable" {
            StatusCode::SERVICE_UNAVAILABLE
        } else {
            StatusCode::CONFLICT
        };
        MockReply::json(json!({
            "error": {"type": "server_error", "code": code, "message": "typed"}
        }))
        .status(status)
    })
    .await;
    let client = Client::new(&server.base_url, None).unwrap();
    assert!(matches!(
        client.build_environment(&full_discovery(), "henv_1").await,
        Err(UhpError::EnvironmentBusy { environment_id }) if environment_id == "henv_1"
    ));
    assert!(matches!(
        client
            .activate_environment_version(&full_discovery(), "henv_1", 2)
            .await,
        Err(UhpError::EnvironmentNotReady { version: None, .. })
    ));
    assert!(matches!(
        client.get_environment(&full_discovery(), "henv_1").await,
        Err(UhpError::EnvironmentUnavailable { status: 503, .. })
    ));
}

#[tokio::test]
async fn harness_environment_task_uses_metadata_environment_only() {
    let bodies = Arc::new(Mutex::new(Vec::new()));
    let captured = bodies.clone();
    let server = MockServer::start(move |request| {
        captured.lock().unwrap().push(request.json());
        MockReply::json(response_json())
    })
    .await;
    let client = Client::new(&server.base_url, None).unwrap();
    client
        .create_response(
            &CreateResponse::new("review", "chrn_1").environment("henv_1"),
            &key("task-environment"),
            &scope(),
        )
        .await
        .unwrap();
    let bodies = bodies.lock().unwrap();
    assert_eq!(bodies[0]["metadata"]["environment"], "henv_1");
    assert!(bodies[0].get("environment").is_none());
}

#[tokio::test]
async fn harness_environment_rejects_activation_version_mismatch() {
    let server = MockServer::start(|_| {
        let mut environment = environment_json("ready", Some(1));
        environment["versions"][1]["status"] = json!("ready");
        MockReply::json(environment)
    })
    .await;
    let client = Client::new(&server.base_url, None).unwrap();
    assert!(matches!(
        client
            .activate_environment_version(&full_discovery(), "henv_1", 2)
            .await,
        Err(UhpError::InvalidResponse {
            operation: "activate environment version",
            ..
        })
    ));
}
