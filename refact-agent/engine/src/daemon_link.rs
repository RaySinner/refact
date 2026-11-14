pub use refact_runtime_api::daemon_link::*;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::types::{ChatSession, SessionState};
    use crate::exec::{ExecMode, ExecProcessMeta, ExecStatus};
    use axum::routing::post;
    use axum::{Json, Router};
    use serde_json::json;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::sync::Mutex as AMutex;

    #[tokio::test]
    async fn daemon_link_task_returns_immediately_without_endpoint() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let completed = Arc::new(AtomicBool::new(false));
        let check = completed.clone();
        let task = tokio::spawn({
            let gcx = gcx.clone();
            async move {
                daemon_link_task(gcx).await;
                check.store(true, Ordering::SeqCst);
            }
        });
        let _ = tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .expect("daemon_link_task must return immediately when daemon endpoint is empty");
        assert!(completed.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn collect_status_snapshot_counts_runtime_state() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        gcx.lsp_tcp_client_count.store(2, Ordering::SeqCst);
        let mut generating = ChatSession::new("generating".to_string());
        generating.runtime.state = SessionState::Generating;
        let mut idle = ChatSession::new("idle".to_string());
        idle.runtime.state = SessionState::Idle;
        gcx.chat_sessions
            .write()
            .await
            .insert("generating".to_string(), Arc::new(AMutex::new(generating)));
        gcx.chat_sessions
            .write()
            .await
            .insert("idle".to_string(), Arc::new(AMutex::new(idle)));
        let snapshot = gcx
            .exec_registry
            .register(
                ExecProcessMeta::new(ExecMode::Background, "sleep".to_string()),
                1024,
            )
            .await;
        gcx.exec_registry
            .set_status(&snapshot.meta.process_id, ExecStatus::Running)
            .await
            .unwrap();

        let report = collect_status_snapshot(gcx).await;

        assert_eq!(report.lsp_clients, 2);
        assert_eq!(report.busy_chats, 1);
        assert_eq!(report.exec_running, 1);
        assert!(report.last_activity_ts > 0);
    }

    #[tokio::test]
    async fn post_worker_status_sends_bearer_token() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let app = Router::new().route(
            "/daemon/v1/worker-status",
            post(
                |headers: axum::http::HeaderMap, Json(_): Json<WorkerStatusReport>| async move {
                    if headers
                        .get(axum::http::header::AUTHORIZATION)
                        .and_then(|value| value.to_str().ok())
                        == Some("Bearer secret-token")
                    {
                        (axum::http::StatusCode::OK, Json(json!({"success": true})))
                    } else {
                        (
                            axum::http::StatusCode::UNAUTHORIZED,
                            Json(json!({"error": "Unauthorized"})),
                        )
                    }
                },
            ),
        );
        let server = axum::Server::from_tcp(listener.into_std().unwrap())
            .unwrap()
            .serve(app.into_make_service());
        let task = tokio::spawn(server);
        let mut gcx = crate::global_context::tests::make_test_gcx().await;
        Arc::get_mut(&mut gcx).unwrap().cmdline.daemon_auth_token =
            Some("secret-token".to_string());
        let report = WorkerStatusReport {
            project_id: "project".to_string(),
            pid: 42,
            instance_token: "token".to_string(),
            lsp_clients: 0,
            busy_chats: 0,
            exec_running: 0,
            last_activity_ts: 1,
        };

        let result = post_worker_status(
            gcx,
            &format!("http://127.0.0.1:{port}/daemon/v1/worker-status"),
            &report,
        )
        .await;

        assert_eq!(result, Ok(()));
        task.abort();
    }

    #[tokio::test]
    async fn post_worker_status_classifies_auth_and_transient_failures() {
        async fn serve_status(
            status: axum::http::StatusCode,
        ) -> (u16, tokio::task::JoinHandle<()>) {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = listener.local_addr().unwrap().port();
            let app = Router::new().route(
                "/daemon/v1/worker-status",
                post(move |Json(_): Json<WorkerStatusReport>| async move {
                    (status, Json(json!({"success": false})))
                }),
            );
            let server = axum::Server::from_tcp(listener.into_std().unwrap())
                .unwrap()
                .serve(app.into_make_service());
            (
                port,
                tokio::spawn(async move {
                    let _ = server.await;
                }),
            )
        }

        let report = WorkerStatusReport {
            project_id: "project".to_string(),
            pid: 42,
            instance_token: "token".to_string(),
            lsp_clients: 0,
            busy_chats: 0,
            exec_running: 0,
            last_activity_ts: 1,
        };

        let (auth_port, auth_task) = serve_status(axum::http::StatusCode::UNAUTHORIZED).await;
        let auth_result = post_worker_status(
            crate::global_context::tests::make_test_gcx().await,
            &format!("http://127.0.0.1:{auth_port}/daemon/v1/worker-status"),
            &report,
        )
        .await;
        assert!(matches!(auth_result, Err(DaemonLinkError::Auth(_))));
        auth_task.abort();

        let (transient_port, transient_task) =
            serve_status(axum::http::StatusCode::SERVICE_UNAVAILABLE).await;
        let transient_result = post_worker_status(
            crate::global_context::tests::make_test_gcx().await,
            &format!("http://127.0.0.1:{transient_port}/daemon/v1/worker-status"),
            &report,
        )
        .await;
        assert!(matches!(
            transient_result,
            Err(DaemonLinkError::Transient(_))
        ));
        transient_task.abort();
    }
}
