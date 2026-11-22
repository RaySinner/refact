pub use refact_http::routers::v1::exec::*;

#[cfg(test)]
use std::time::Duration;
#[cfg(test)]
use crate::exec::ExecProcessId;

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use hyper::body::to_bytes;
    use serde_json::{json, Value};
    use tower::ServiceExt;

    use crate::app_state::AppState;
    use crate::http::routers::make_refact_http_server;

    async fn test_app() -> (tempfile::TempDir, AppState) {
        let temp = tempfile::tempdir().unwrap();
        let gcx = crate::global_context::tests::make_test_gcx().await;
        *gcx.documents_state.workspace_folders.lock().unwrap() = vec![temp.path().to_path_buf()];
        (temp, crate::app_state::app_state_from_gcx(gcx).await)
    }

    async fn json_response(router: axum::Router, request: Request<Body>) -> (StatusCode, Value) {
        let response = router.oneshot(request).await.unwrap();
        let status = response.status();
        let body = to_bytes(response.into_body()).await.unwrap();
        let json = serde_json::from_slice(&body).unwrap_or(Value::Null);
        (status, json)
    }

    fn post_json(uri: &str, body: Value) -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri(uri)
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap()
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn default_shell_resolution_follows_unix_ladder() {
        assert_eq!(
            super::resolve_unix_default_shell(Some("/opt/fish"), |candidate| candidate
                == "/opt/fish"),
            "/opt/fish"
        );
        assert_eq!(
            super::resolve_unix_default_shell(Some("/missing/fish"), |candidate| candidate
                == "bash"),
            "bash -l"
        );
        assert_eq!(super::resolve_unix_default_shell(None, |_| false), "sh -l");
    }

    #[tokio::test]
    #[serial_test::parallel(exec_http_env)]
    async fn spawn_requires_exactly_one_command_shape() {
        let (_temp, app) = test_app().await;
        let router = make_refact_http_server(app);

        let (status, _) = json_response(
            router.clone(),
            post_json(
                "/v1/exec/spawn",
                json!({ "command": "echo one", "argv": ["echo", "two"] }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        let (status, _) = json_response(
            router,
            post_json(
                "/v1/exec/spawn",
                json!({ "command": null, "argv": null, "pty": false }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    #[serial_test::parallel(exec_http_env)]
    async fn spawn_default_pty_shell_returns_command_preview() {
        let (_temp, app) = test_app().await;
        let router = make_refact_http_server(app);

        let (status, spawned) = json_response(
            router.clone(),
            post_json("/v1/exec/spawn", json!({ "pty": true })),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            spawned["command_preview"],
            super::default_interactive_shell()
        );
        let process_id = spawned["process_id"].as_str().unwrap();
        let (status, _) = json_response(
            router,
            post_json(&format!("/v1/exec/{process_id}/kill"), json!({})),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
    }

    #[cfg(unix)]
    #[tokio::test]
    #[serial_test::parallel(exec_http_env)]
    async fn spawn_explicit_command_preserves_command_preview() {
        let (_temp, app) = test_app().await;
        let router = make_refact_http_server(app);

        let (status, spawned) = json_response(
            router.clone(),
            post_json(
                "/v1/exec/spawn",
                json!({ "command": "sleep 30", "pty": true }),
            ),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(spawned["command_preview"], "sleep 30");
        let process_id = spawned["process_id"].as_str().unwrap();
        let (status, _) = json_response(
            router,
            post_json(&format!("/v1/exec/{process_id}/kill"), json!({})),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
    }

    #[tokio::test]
    #[serial_test::parallel(exec_http_env)]
    async fn auth_matrix_spawn_rejects_cwd_outside_workspace() {
        let (_temp, app) = test_app().await;
        let outside = tempfile::tempdir().unwrap();
        let router = make_refact_http_server(app);

        let (status, json) = json_response(
            router,
            post_json(
                "/v1/exec/spawn",
                json!({ "command": "echo hi", "pty": false, "cwd": outside.path() }),
            ),
        )
        .await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(json["detail"].as_str().is_some());
    }

    #[tokio::test]
    #[serial_test::serial(exec_http_env)]
    async fn auth_matrix_spawn_forbidden_when_exec_http_disabled() {
        let (_temp, app) = test_app().await;
        let router = make_refact_http_server(app);

        std::env::set_var("REFACT_DISABLE_EXEC_HTTP", "1");
        let result = json_response(
            router,
            post_json(
                "/v1/exec/spawn",
                json!({ "command": "echo hi", "pty": false }),
            ),
        )
        .await;
        std::env::remove_var("REFACT_DISABLE_EXEC_HTTP");

        let (status, json) = result;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert!(json["detail"]
            .as_str()
            .unwrap_or_default()
            .contains("exec HTTP is disabled"));
    }

    #[cfg(unix)]
    #[tokio::test]
    #[serial_test::parallel(exec_http_env)]
    async fn exec_http_lifecycle_is_workspace_scoped() {
        let (_temp, app) = test_app().await;
        let router = make_refact_http_server(app.clone());
        let (status, spawned) = json_response(
            router.clone(),
            post_json(
                "/v1/exec/spawn",
                json!({ "argv": ["sh", "-c", "printf hello; sleep 30"], "pty": false }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let process_id = spawned["process_id"].as_str().unwrap();

        let (status, listed) = json_response(
            router.clone(),
            Request::builder()
                .uri("/v1/exec/list")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(listed["processes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|process| process["process_id"] == process_id));

        tokio::time::sleep(Duration::from_millis(100)).await;
        let (status, read) = json_response(
            router.clone(),
            Request::builder()
                .uri(format!("/v1/exec/{process_id}/read?since_seq=0&limit=10"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(read["chunks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|chunk| chunk["text"].as_str().unwrap().contains("hello")));

        let (status, killed) = json_response(
            router,
            post_json(&format!("/v1/exec/{process_id}/kill"), json!({})),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(killed["status"], "killed");
    }

    #[cfg(unix)]
    #[tokio::test]
    #[serial_test::parallel(exec_http_env)]
    async fn exec_http_spawn_and_list_are_optionally_chat_scoped() {
        let (_temp, app) = test_app().await;
        let router = make_refact_http_server(app.clone());
        let (status, chat_spawned) = json_response(
            router.clone(),
            post_json(
                "/v1/exec/spawn",
                json!({ "command": "sleep 30", "pty": false, "chat_id": "chat-a" }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let chat_process_id = chat_spawned["process_id"].as_str().unwrap();
        let chat_snapshot = app
            .runtime
            .exec_registry
            .get(&crate::exec::ExecProcessId(chat_process_id.to_string()))
            .await
            .unwrap();
        assert_eq!(chat_snapshot.meta.owner.chat_id.as_deref(), Some("chat-a"));

        let (status, legacy_spawned) = json_response(
            router.clone(),
            post_json(
                "/v1/exec/spawn",
                json!({ "command": "sleep 30", "pty": false }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let legacy_process_id = legacy_spawned["process_id"].as_str().unwrap();
        let legacy_snapshot = app
            .runtime
            .exec_registry
            .get(&crate::exec::ExecProcessId(legacy_process_id.to_string()))
            .await
            .unwrap();
        assert_eq!(legacy_snapshot.meta.owner.chat_id, None);

        let (status, chat_list) = json_response(
            router.clone(),
            Request::builder()
                .uri("/v1/exec/list?chat_id=chat-a")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let chat_processes = chat_list["processes"].as_array().unwrap();
        assert!(chat_processes
            .iter()
            .any(|process| process["process_id"] == chat_process_id));
        assert!(!chat_processes
            .iter()
            .any(|process| process["process_id"] == legacy_process_id));

        let (status, other_list) = json_response(
            router.clone(),
            Request::builder()
                .uri("/v1/exec/list?chat_id=chat-b")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(!other_list["processes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|process| process["process_id"] == chat_process_id));

        let (status, workspace_list) = json_response(
            router.clone(),
            Request::builder()
                .uri("/v1/exec/list")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let workspace_processes = workspace_list["processes"].as_array().unwrap();
        assert!(workspace_processes
            .iter()
            .any(|process| process["process_id"] == chat_process_id));
        assert!(workspace_processes
            .iter()
            .any(|process| process["process_id"] == legacy_process_id));

        for (process_id, chat_query) in [
            (chat_process_id, "?chat_id=chat-a"),
            (legacy_process_id, ""),
        ] {
            let (status, _) = json_response(
                router.clone(),
                post_json(
                    &format!("/v1/exec/{process_id}/kill{chat_query}"),
                    json!({}),
                ),
            )
            .await;
            assert_eq!(status, StatusCode::OK);
        }
    }

    #[tokio::test]
    async fn exec_list_scopes_same_chat_across_workspaces_and_legacy_by_workspace() {
        let (temp, app) = test_app().await;
        let foreign_workspace = tempfile::tempdir().unwrap();
        let registry = &app.runtime.exec_registry;

        let same_chat_active = registry
            .register(
                crate::exec::ExecProcessMeta::new(
                    crate::exec::ExecMode::Background,
                    "same chat active".to_string(),
                )
                .with_chat_id("chat-a")
                .with_workspace(temp.path().to_path_buf()),
                1024,
            )
            .await;
        app.runtime
            .exec_registry
            .mark_started(&same_chat_active.meta.process_id)
            .await
            .unwrap();
        let same_chat_foreign = registry
            .register(
                crate::exec::ExecProcessMeta::new(
                    crate::exec::ExecMode::Background,
                    "same chat foreign".to_string(),
                )
                .with_chat_id("chat-a")
                .with_workspace(foreign_workspace.path().to_path_buf()),
                1024,
            )
            .await;
        app.runtime
            .exec_registry
            .mark_exited(&same_chat_foreign.meta.process_id, Some(0))
            .await
            .unwrap();
        let other_chat = registry
            .register(
                crate::exec::ExecProcessMeta::new(
                    crate::exec::ExecMode::Background,
                    "other chat".to_string(),
                )
                .with_chat_id("chat-b")
                .with_workspace(temp.path().to_path_buf()),
                1024,
            )
            .await;
        let legacy_active = registry
            .register(
                crate::exec::ExecProcessMeta::new(
                    crate::exec::ExecMode::Background,
                    "legacy active".to_string(),
                )
                .with_workspace(temp.path().to_path_buf()),
                1024,
            )
            .await;
        let legacy_foreign = registry
            .register(
                crate::exec::ExecProcessMeta::new(
                    crate::exec::ExecMode::Background,
                    "legacy foreign".to_string(),
                )
                .with_workspace(foreign_workspace.path().to_path_buf()),
                1024,
            )
            .await;
        let router = make_refact_http_server(app);

        let (status, chat_list) = json_response(
            router.clone(),
            Request::builder()
                .uri("/v1/exec/list?chat_id=chat-a")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let chat_processes = chat_list["processes"].as_array().unwrap();
        assert!(chat_processes
            .iter()
            .any(|process| {
                process["process_id"] == same_chat_active.meta.process_id.as_str()
            }));
        assert!(chat_processes
            .iter()
            .any(|process| {
                process["process_id"] == same_chat_foreign.meta.process_id.as_str()
            }));
        assert!(!chat_processes
            .iter()
            .any(|process| process["process_id"] == other_chat.meta.process_id.as_str()));
        assert!(!chat_processes
            .iter()
            .any(|process| process["process_id"] == legacy_active.meta.process_id.as_str()));
        assert!(!chat_processes
            .iter()
            .any(|process| process["process_id"] == legacy_foreign.meta.process_id.as_str()));

        for uri in ["/v1/exec/list", "/v1/exec/list?chat_id="] {
            let (status, workspace_list) = json_response(
                router.clone(),
                Request::builder().uri(uri).body(Body::empty()).unwrap(),
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            let workspace_processes = workspace_list["processes"].as_array().unwrap();
            assert!(workspace_processes
                .iter()
                .any(|process| {
                    process["process_id"] == same_chat_active.meta.process_id.as_str()
                }));
            assert!(workspace_processes
                .iter()
                .any(|process| process["process_id"] == legacy_active.meta.process_id.as_str()));
            assert!(!workspace_processes
                .iter()
                .any(|process| {
                    process["process_id"] == same_chat_foreign.meta.process_id.as_str()
                }));
            assert!(!workspace_processes
                .iter()
                .any(|process| process["process_id"] == legacy_foreign.meta.process_id.as_str()));
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    #[serial_test::parallel(exec_http_env)]
    async fn write_stdin_to_running_tty_process_succeeds() {
        let (_temp, app_state) = test_app().await;
        let router = make_refact_http_server(app_state.clone());
        let (status, spawned) = json_response(
            router.clone(),
            post_json(
                "/v1/exec/spawn",
                json!({ "command": "read line; printf 'got:%s\\n' \"$line\"", "pty": true }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let process_id = spawned["process_id"].as_str().unwrap();

        let (status, _) = json_response(
            router.clone(),
            post_json(
                &format!("/v1/exec/{process_id}/resize"),
                json!({ "rows": 40, "cols": 120 }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        let (status, json) = json_response(
            router,
            post_json(
                &format!("/v1/exec/{process_id}/stdin"),
                json!({ "chars": "ribbit\n" }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["process_id"], process_id);
        assert_eq!(json["bytes_written"], "ribbit\n".len());

        let process_id = crate::exec::ExecProcessId(process_id.to_string());
        let _ = app_state
            .runtime
            .exec_registry
            .wait(&process_id)
            .await
            .unwrap();
        let read = app_state
            .runtime
            .exec_registry
            .read(&process_id, 0, None)
            .await;
        assert!(read.chunks.iter().any(|chunk| chunk.text.contains("got:")));
    }

    #[tokio::test]
    async fn process_access_rejects_another_workspace() {
        let (_temp, app) = test_app().await;
        let foreign = tempfile::tempdir().unwrap();
        let snapshot = app
            .runtime
            .exec_registry
            .register(
                crate::exec::ExecProcessMeta::new(
                    crate::exec::ExecMode::Background,
                    "foreign".to_string(),
                )
                .with_workspace(foreign.path().to_path_buf()),
                1024,
            )
            .await;
        let router = make_refact_http_server(app);

        let (status, _) = json_response(
            router,
            Request::builder()
                .uri(format!(
                    "/v1/exec/{}/read",
                    snapshot.meta.process_id.as_str()
                ))
                .body(Body::empty())
                .unwrap(),
        )
        .await;

        assert_eq!(status, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn process_access_honors_chat_without_weakening_workspace_scope() {
        let (temp, app) = test_app().await;
        let owned = app
            .runtime
            .exec_registry
            .register(
                crate::exec::ExecProcessMeta::new(
                    crate::exec::ExecMode::Background,
                    "owned".to_string(),
                )
                .with_chat_id("chat-a")
                .with_workspace(temp.path().to_path_buf()),
                1024,
            )
            .await;
        let legacy = app
            .runtime
            .exec_registry
            .register(
                crate::exec::ExecProcessMeta::new(
                    crate::exec::ExecMode::Background,
                    "legacy".to_string(),
                )
                .with_workspace(temp.path().to_path_buf()),
                1024,
            )
            .await;
        let foreign = tempfile::tempdir().unwrap();
        let foreign_owned = app
            .runtime
            .exec_registry
            .register(
                crate::exec::ExecProcessMeta::new(
                    crate::exec::ExecMode::Background,
                    "foreign".to_string(),
                )
                .with_chat_id("chat-a")
                .with_workspace(foreign.path().to_path_buf()),
                1024,
            )
            .await;
        let router = make_refact_http_server(app);

        for uri in [
            format!(
                "/v1/exec/{}/read?chat_id=chat-a",
                owned.meta.process_id.as_str()
            ),
            format!(
                "/v1/exec/{}/read?chat_id=chat-b",
                legacy.meta.process_id.as_str()
            ),
        ] {
            let (status, _) = json_response(
                router.clone(),
                Request::builder().uri(uri).body(Body::empty()).unwrap(),
            )
            .await;
            assert_eq!(status, StatusCode::OK);
        }

        for uri in [
            format!("/v1/exec/{}/read", owned.meta.process_id.as_str()),
            format!(
                "/v1/exec/{}/read?chat_id=chat-b",
                owned.meta.process_id.as_str()
            ),
        ] {
            let (status, _) = json_response(
                router.clone(),
                Request::builder().uri(uri).body(Body::empty()).unwrap(),
            )
            .await;
            assert_eq!(status, StatusCode::FORBIDDEN);
        }

        let (status, _) = json_response(
            router,
            Request::builder()
                .uri(format!(
                    "/v1/exec/{}/read?chat_id=chat-a",
                    foreign_owned.meta.process_id.as_str()
                ))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
    }

    #[cfg(unix)]
    #[tokio::test]
    #[serial_test::parallel(exec_http_env)]
    async fn chat_scoped_process_routes_honor_query_and_body_chat_ids() {
        let (_temp, app) = test_app().await;
        let router = make_refact_http_server(app);
        let (status, spawned) = json_response(
            router.clone(),
            post_json(
                "/v1/exec/spawn",
                json!({
                    "command": "read line",
                    "pty": true,
                    "chat_id": "chat-a"
                }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let process_id = spawned["process_id"].as_str().unwrap();

        for request in [
            Request::builder()
                .uri(format!("/v1/exec/{process_id}/read?since_seq=0"))
                .body(Body::empty())
                .unwrap(),
            post_json(
                &format!("/v1/exec/{process_id}/resize"),
                json!({ "rows": 40, "cols": 120 }),
            ),
            post_json(
                &format!("/v1/exec/{process_id}/stdin"),
                json!({ "chars": "nope" }),
            ),
            post_json(&format!("/v1/exec/{process_id}/kill"), json!({})),
            Request::builder()
                .uri(format!(
                    "/v1/exec/{process_id}/read?chat_id=chat-b&since_seq=0"
                ))
                .body(Body::empty())
                .unwrap(),
            post_json(
                &format!("/v1/exec/{process_id}/resize"),
                json!({ "rows": 40, "cols": 120, "chat_id": "chat-b" }),
            ),
            post_json(
                &format!("/v1/exec/{process_id}/stdin"),
                json!({ "chars": "nope", "chat_id": "chat-b" }),
            ),
            post_json(
                &format!("/v1/exec/{process_id}/kill?chat_id=chat-b"),
                json!({}),
            ),
        ] {
            let (status, _) = json_response(router.clone(), request).await;
            assert_eq!(status, StatusCode::FORBIDDEN);
        }

        for chat_query in ["", "?chat_id=chat-b"] {
            let response = router
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(format!("/v1/exec/{process_id}/subscribe{chat_query}"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::FORBIDDEN);
        }

        let (status, _) = json_response(
            router.clone(),
            Request::builder()
                .uri(format!(
                    "/v1/exec/{process_id}/read?chat_id=chat-a&since_seq=0"
                ))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        let (status, _) = json_response(
            router.clone(),
            post_json(
                &format!("/v1/exec/{process_id}/resize"),
                json!({ "rows": 40, "cols": 120, "chat_id": "chat-a" }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        let (status, _) = json_response(
            router.clone(),
            post_json(
                &format!("/v1/exec/{process_id}/stdin"),
                json!({ "chars": "hello", "chat_id": "chat-a" }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/v1/exec/{process_id}/subscribe?chat_id=chat-a"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let (status, killed) = json_response(
            router,
            post_json(
                &format!("/v1/exec/{process_id}/kill?chat_id=chat-a"),
                json!({}),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(killed["status"], "killed");
    }

    #[cfg(unix)]
    #[tokio::test]
    #[serial_test::parallel(exec_http_env)]
    async fn subscribe_streams_snapshot_output_and_exit() {
        let (_temp, app) = test_app().await;
        let router = make_refact_http_server(app);
        let (status, spawned) = json_response(
            router.clone(),
            post_json(
                "/v1/exec/spawn",
                json!({ "command": "printf terminal-sse", "pty": false }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let process_id = spawned["process_id"].as_str().unwrap();
        let response = router
            .oneshot(
                Request::builder()
                    .uri(format!("/v1/exec/{process_id}/subscribe"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let mut body = response.into_body();
        let collected = tokio::time::timeout(Duration::from_secs(5), async move {
            let mut text = String::new();
            while let Some(chunk) = body.data().await {
                text.push_str(&String::from_utf8_lossy(&chunk.unwrap()));
                if text.contains("event: exit") {
                    break;
                }
            }
            text
        })
        .await
        .expect("SSE stream should terminate after exit");

        assert!(collected.contains("event: snapshot"));
        assert!(collected.contains("terminal-sse"));
        assert!(collected.contains("event: exit"));
    }

    #[cfg(unix)]
    #[tokio::test]
    #[serial_test::parallel(exec_http_env)]
    async fn subscribe_streams_raw_pty_bytes_with_crlf() {
        let (_temp, app) = test_app().await;
        let router = make_refact_http_server(app);
        let (status, spawned) = json_response(
            router.clone(),
            post_json(
                "/v1/exec/spawn",
                json!({ "command": "printf 'a\\r\\nb\\r\\n'", "pty": true }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let process_id = spawned["process_id"].as_str().unwrap();
        let response = router
            .oneshot(
                Request::builder()
                    .uri(format!("/v1/exec/{process_id}/subscribe?since_seq=0"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let mut body = response.into_body();
        let collected = tokio::time::timeout(Duration::from_secs(5), async move {
            let mut text = String::new();
            while let Some(chunk) = body.data().await {
                text.push_str(&String::from_utf8_lossy(&chunk.unwrap()));
                if text.contains("event: exit") {
                    break;
                }
            }
            text
        })
        .await
        .expect("SSE stream should terminate after exit");

        assert!(collected.contains("event: snapshot"));
        assert!(
            collected.contains("\\r\\n"),
            "SSE payload must keep CRLF byte-faithful for tty: {collected:?}"
        );
        assert!(collected.contains("\"offset\""));
        assert!(collected.contains("event: exit"));
    }

    #[cfg(unix)]
    #[tokio::test]
    #[serial_test::parallel(exec_http_env)]
    async fn read_raw_uses_byte_cursor_for_tty_process() {
        let (_temp, app) = test_app().await;
        let router = make_refact_http_server(app.clone());
        let (status, spawned) = json_response(
            router.clone(),
            post_json(
                "/v1/exec/spawn",
                json!({ "command": "printf 'x\\r\\ny\\r\\n'", "pty": true }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let process_id = spawned["process_id"].as_str().unwrap().to_string();
        let exec_process_id = crate::exec::ExecProcessId(process_id.clone());
        app.runtime
            .exec_registry
            .wait(&exec_process_id)
            .await
            .unwrap();

        let (status, read) = json_response(
            router.clone(),
            Request::builder()
                .uri(format!("/v1/exec/{process_id}/read?raw=true&since_seq=0"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let chunks = read["chunks"].as_array().unwrap();
        assert_eq!(chunks.len(), 1);
        let text = chunks[0]["text"].as_str().unwrap();
        assert!(
            text.contains("\r\n"),
            "raw read must keep CRLF byte-faithful: {text:?}"
        );
        let next_seq = read["next_seq"].as_u64().unwrap();
        assert!(next_seq > 0);
        assert_eq!(chunks[0]["offset"].as_u64().unwrap(), next_seq);
        assert_eq!(chunks[0]["seq"].as_u64().unwrap(), 0);

        let (status, drained) = json_response(
            router,
            Request::builder()
                .uri(format!(
                    "/v1/exec/{process_id}/read?raw=true&since_seq={next_seq}"
                ))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(drained["chunks"].as_array().unwrap().is_empty());
        assert_eq!(drained["next_seq"].as_u64().unwrap(), next_seq);
    }

    #[cfg(unix)]
    #[tokio::test]
    #[serial_test::parallel(exec_http_env)]
    async fn read_raw_flag_is_ignored_for_non_tty_process() {
        let (_temp, app) = test_app().await;
        let router = make_refact_http_server(app.clone());
        let (status, spawned) = json_response(
            router.clone(),
            post_json(
                "/v1/exec/spawn",
                json!({ "command": "printf plain-hello", "pty": false }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let process_id = spawned["process_id"].as_str().unwrap().to_string();
        app.runtime
            .exec_registry
            .wait(&crate::exec::ExecProcessId(process_id.clone()))
            .await
            .unwrap();

        let (status, read) = json_response(
            router,
            Request::builder()
                .uri(format!("/v1/exec/{process_id}/read?raw=true&since_seq=0"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let chunks = read["chunks"].as_array().unwrap();
        assert!(chunks
            .iter()
            .any(|chunk| chunk["text"].as_str().unwrap().contains("plain-hello")));
        assert!(chunks.iter().all(|chunk| chunk.get("offset").is_none()));
    }

    #[cfg(unix)]
    #[tokio::test]
    #[serial_test::parallel(exec_http_env)]
    async fn nonzero_exit_code_is_exposed_by_list_read_and_sse() {
        let (_temp, app) = test_app().await;
        let router = make_refact_http_server(app);
        let (status, spawned) = json_response(
            router.clone(),
            post_json(
                "/v1/exec/spawn",
                json!({ "argv": ["sh", "-c", "exit 3"], "pty": false }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let process_id = spawned["process_id"].as_str().unwrap().to_string();

        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/v1/exec/{process_id}/subscribe"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let mut body = response.into_body();
        let collected = tokio::time::timeout(Duration::from_secs(10), async move {
            let mut text = String::new();
            while let Some(chunk) = body.data().await {
                text.push_str(&String::from_utf8_lossy(&chunk.unwrap()));
                if text.contains("event: exit") {
                    break;
                }
            }
            text
        })
        .await
        .expect("SSE stream should terminate after exit");

        assert!(collected.contains("event: exit"), "{collected:?}");
        assert!(
            collected.contains("\"exit_code\":3"),
            "SSE exit event must carry the exit code: {collected:?}"
        );

        let (status, listed) = json_response(
            router.clone(),
            Request::builder()
                .uri("/v1/exec/list")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let listed_process = listed["processes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|process| process["process_id"] == process_id.as_str())
            .expect("spawned process must be listed")
            .clone();
        assert_eq!(listed_process["status"], "exited");
        assert_eq!(listed_process["exit_code"], json!(3));
        assert!(
            listed_process["ended_at_ms"].as_u64().unwrap_or_default() > 0,
            "list must expose ended_at_ms after exit: {listed_process:?}"
        );

        let (status, read) = json_response(
            router,
            Request::builder()
                .uri(format!("/v1/exec/{process_id}/read?since_seq=0"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(read["status"], "exited");
        assert_eq!(read["exit_code"], json!(3));
        assert!(
            read["ended_at_ms"].as_u64().unwrap_or_default() > 0,
            "read must expose ended_at_ms after exit: {read:?}"
        );
    }
}
