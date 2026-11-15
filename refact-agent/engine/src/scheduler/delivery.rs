pub use refact_runtime_api::scheduler_delivery::*;

#[cfg(test)]
use refact_core::chat_types::PushMode;
#[cfg(test)]
use serde_json::json;
#[cfg(test)]
use crate::app_state::AppState;
#[cfg(test)]
use crate::chat::internal_roles::event;
#[cfg(test)]
use super::types::Action;
#[cfg(test)]
use super::types::AgentTarget;
#[cfg(test)]
use super::types::Delivery;
#[cfg(test)]
use super::types::Job;

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::routing::post;
    use axum::{Json, Router};
    use serde_json::Value;
    use tokio::sync::Mutex as AMutex;

    use super::*;
    use crate::chat::internal_roles::EVENT_ROLE;
    use crate::chat::types::ChatSession;
    use crate::scheduler::types::{CronRunRecord, Trigger};

    fn command_job(delivery: Delivery) -> Job {
        Job {
            id: "delivery-job".to_string(),
            description: "Deliver frogs".to_string(),
            enabled: true,
            durable: false,
            created_at_ms: 1_000,
            recurring: true,
            trigger: Trigger::Cron {
                expr: "*/5 * * * *".to_string(),
                tz: None,
            },
            action: Action::Command {
                argv: vec!["printf".to_string(), "frogs".to_string()],
                target: AgentTarget::ExistingChat {
                    chat_id: "chat-1".to_string(),
                },
                cwd: None,
                env: None,
                timeout_secs: None,
            },
            delivery,
            push: Default::default(),
            last_fired_at_ms: Some(2_000),
            fire_count: 2,
            last_status: Some("fired".to_string()),
            last_error: None,
            last_delivery_error: None,
            recent_runs: vec![CronRunRecord {
                at_ms: 2_000,
                status: "fired".to_string(),
                error: None,
            }],
            paused_at_ms: None,
            trigger_at_ms: None,
            auto_expire_after_ms: crate::scheduler::DEFAULT_RECURRING_AUTO_EXPIRE_AFTER_MS,
            retry_attempts: 0,
        }
    }

    async fn app_with_session() -> (tempfile::TempDir, AppState) {
        let workspace = tempfile::tempdir().unwrap();
        let gcx = crate::global_context::tests::make_test_gcx().await;
        *gcx.documents_state.workspace_folders.lock().unwrap() =
            vec![workspace.path().to_path_buf()];
        let session = Arc::new(AMutex::new(ChatSession::new("chat-1".to_string())));
        gcx.chat_sessions
            .write()
            .await
            .insert("chat-1".to_string(), session);
        (workspace, crate::app_state::app_state_from_gcx(gcx).await)
    }

    #[test]
    fn old_job_defaults_to_append_and_explicit_push_roundtrips() {
        let job = command_job(Delivery::Chat);
        let mut raw = serde_json::to_value(&job).unwrap();
        raw.as_object_mut().unwrap().remove("push");
        assert_eq!(
            serde_json::from_value::<Job>(raw.clone()).unwrap().push,
            PushMode::Append
        );
        for (wire, mode) in [
            ("preempt", PushMode::Preempt),
            ("append", PushMode::Append),
            ("when_idle", PushMode::WhenIdle),
        ] {
            raw["push"] = json!(wire);
            let job: Job = serde_json::from_value(raw.clone()).unwrap();
            assert_eq!(job.push, mode);
            assert_eq!(
                serde_json::from_value::<Job>(serde_json::to_value(&job).unwrap()).unwrap(),
                job
            );
        }
    }

    #[tokio::test]
    async fn busy_command_output_and_error_are_ordered_deduplicated_notices() {
        for failed in [false, true] {
            let (_workspace, app) = app_with_session().await;
            let arc = app
                .chat
                .sessions
                .read()
                .await
                .get("chat-1")
                .cloned()
                .unwrap();
            arc.lock().await.start_stream().unwrap();
            let mut job = command_job(Delivery::Chat);
            if failed {
                job.last_status = Some("error".into());
            }
            deliver(&app, &job, "result").await.unwrap();
            deliver(&app, &job, "result").await.unwrap();
            let mut session = arc.lock().await;
            assert_eq!(session.pending_deliveries.len(), 1);
            assert!(!session.pending_deliveries[0].wake);
            session.finish_stream(Some("stop".into()));
            session.drain_pending_deliveries();
            if failed {
                assert_eq!(session.messages.len(), 1);
                assert_eq!(
                    session.messages[0].extra["event"]["subkind"],
                    "system_notice"
                );
            } else {
                assert_eq!(session.messages.len(), 2);
                assert_eq!(session.messages[0].extra["event"]["subkind"], "cron_fire");
                assert_eq!(session.messages[1].content.content_text_only(), "result");
            }
        }
    }

    #[tokio::test]
    async fn chat_delivery_injects_command_output() {
        let (_workspace, app) = app_with_session().await;
        let job = command_job(Delivery::Chat);

        deliver(&app, &job, "hello frogs").await.unwrap();

        let session_arc = app
            .gcx
            .chat_sessions
            .read()
            .await
            .get("chat-1")
            .cloned()
            .unwrap();
        let session = session_arc.lock().await;
        assert!(session.messages.iter().any(|message| {
            message.role == EVENT_ROLE
                && message.extra["event"]["subkind"].as_str() == Some("cron_fire")
                && message.extra["event"]["payload"]["task_id"].as_str() == Some("delivery-job")
        }));
        let output = session
            .messages
            .iter()
            .find(|message| message.role == "plain_text")
            .unwrap();
        assert_eq!(output.content.content_text_only(), "hello frogs");
    }

    #[tokio::test]
    async fn chat_delivery_empty_output_is_silent() {
        let (_workspace, app) = app_with_session().await;
        let job = command_job(Delivery::Chat);

        deliver(&app, &job, "").await.unwrap();

        let session_arc = app
            .gcx
            .chat_sessions
            .read()
            .await
            .get("chat-1")
            .cloned()
            .unwrap();
        let session = session_arc.lock().await;
        assert!(session.messages.is_empty());
    }

    #[tokio::test]
    async fn none_delivery_is_noop() {
        let (_workspace, app) = app_with_session().await;
        let job = command_job(Delivery::None);

        deliver(&app, &job, "hello frogs").await.unwrap();

        let session_arc = app
            .gcx
            .chat_sessions
            .read()
            .await
            .get("chat-1")
            .cloned()
            .unwrap();
        let session = session_arc.lock().await;
        assert!(session.messages.is_empty());
    }

    #[tokio::test]
    async fn webhook_delivery_posts_json_with_auth() {
        let received = Arc::new(AMutex::new(Vec::<(Option<String>, Value)>::new()));
        let handler_received = received.clone();
        let router = Router::new().route(
            "/hook",
            post(
                move |headers: axum::http::HeaderMap, Json(body): Json<Value>| {
                    let handler_received = handler_received.clone();
                    async move {
                        let auth = headers
                            .get(axum::http::header::AUTHORIZATION)
                            .and_then(|value| value.to_str().ok())
                            .map(str::to_string);
                        handler_received.lock().await.push((auth, body));
                        axum::http::StatusCode::OK
                    }
                },
            ),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = axum::Server::from_tcp(listener.into_std().unwrap())
            .unwrap()
            .serve(router.into_make_service());
        let server_task = tokio::spawn(server);
        let app = crate::app_state::app_state_from_gcx(crate::global_context::tests::make_test_gcx().await).await;
        let job = command_job(Delivery::Webhook {
            url: format!("http://127.0.0.1:{port}/hook"),
            token: Some("secret".to_string()),
        });

        deliver(&app, &job, "hello webhook").await.unwrap();

        let received = received.lock().await;
        assert_eq!(received.len(), 1);
        assert_eq!(received[0].0.as_deref(), Some("Bearer secret"));
        assert_eq!(received[0].1["job_id"], json!("delivery-job"));
        assert_eq!(received[0].1["description"], json!("Deliver frogs"));
        assert_eq!(received[0].1["status"], json!("fired"));
        assert_eq!(received[0].1["output"], json!("hello webhook"));
        assert!(received[0].1["ts"].as_u64().unwrap() > 0);
        server_task.abort();
    }

    #[tokio::test]
    async fn notifier_delivery_posts_to_configured_telegram() {
        let temp = tempfile::tempdir().unwrap();
        let received = Arc::new(AMutex::new(Vec::<Value>::new()));
        let handler_received = received.clone();
        let router = Router::new().route(
            "/botsecret-token/sendMessage",
            post(move |Json(body): Json<Value>| {
                let handler_received = handler_received.clone();
                async move {
                    handler_received.lock().await.push(body);
                    axum::http::StatusCode::OK
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = axum::Server::from_tcp(listener.into_std().unwrap())
            .unwrap()
            .serve(router.into_make_service());
        let server_task = tokio::spawn(server);
        let gcx = crate::global_context::tests::make_test_gcx().await;
        *gcx.documents_state.workspace_folders.lock().unwrap() =
            vec![crate::files_correction::canonicalize_normalized_path(
                temp.path().to_path_buf(),
            )];
        let config_dir = temp.path().join(".refact").join("integrations.d");
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::write(
            config_dir.join("notifier_telegram.yaml"),
            format!(
                "bot_token: secret-token\ndefault_chat_id: default-chat\napi_base_for_test: http://127.0.0.1:{port}\n"
            ),
        )
        .unwrap();
        let app = crate::app_state::app_state_from_gcx(gcx).await;
        let job = command_job(Delivery::Notifier {
            integration_id: "notifier_telegram".to_string(),
            target: Some("target-chat".to_string()),
        });

        deliver(&app, &job, "hello notifier").await.unwrap();

        let received = received.lock().await;
        assert_eq!(received.len(), 1);
        assert_eq!(received[0]["chat_id"], json!("target-chat"));
        assert_eq!(received[0]["text"], json!("hello notifier"));
        server_task.abort();
    }

    #[tokio::test]
    async fn notifier_delivery_missing_integration_errors() {
        let app = crate::app_state::app_state_from_gcx(crate::global_context::tests::make_test_gcx().await).await;
        let job = command_job(Delivery::Notifier {
            integration_id: "notifier_telegram".to_string(),
            target: None,
        });

        let err = deliver(&app, &job, "hello notifier").await.unwrap_err();

        assert!(err.contains("integration `notifier_telegram` not found"));
    }
}
