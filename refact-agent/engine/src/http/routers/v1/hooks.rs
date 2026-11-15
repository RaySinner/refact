pub use refact_http::routers::v1::hooks::*;

#[cfg(test)]
#[allow(unused_imports)]
use refact_runtime_api::GlobalContextCtx;
#[cfg(test)]
#[allow(unused_imports)]
use std::sync::Arc;
#[cfg(test)]
#[allow(unused_imports)]
use axum::http::StatusCode;
#[cfg(test)]
#[allow(unused_imports)]
use axum::routing::post;
#[cfg(test)]
#[allow(unused_imports)]
use axum::Router;
#[cfg(test)]
#[allow(unused_imports)]
use hyper::{Body, Request};
#[cfg(test)]
#[allow(unused_imports)]
use serde_json::{json, Value};
#[cfg(test)]
#[allow(unused_imports)]
use tokio::sync::Mutex as AMutex;
#[cfg(test)]
#[allow(unused_imports)]
use tower::ServiceExt;
#[cfg(test)]
#[allow(unused_imports)]
use uuid::Uuid;
#[cfg(test)]
#[allow(unused_imports)]
use refact_core::chat_types::PushMode;
#[cfg(test)]
#[allow(unused_imports)]
use refact_core::custom_error::ScratchError;
#[cfg(test)]
#[allow(unused_imports)]
use refact_runtime_api::app_state::AppState;
#[cfg(test)]
#[allow(unused_imports)]
use refact_runtime_api::scheduler_runner::session_cron_store;
#[cfg(test)]
#[allow(unused_imports)]
use refact_scheduler_core::store::InMemoryCronStore;
#[cfg(test)]
#[allow(unused_imports)]
use refact_scheduler_core::types::{Action, AgentTarget, Delivery, Job, Trigger};
#[cfg(test)]
#[allow(unused_imports)]
use refact_scheduler_core::store::CronStore;
#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::routing::post;
    use hyper::{Body, Request};
    use tokio::sync::Mutex as AMutex;
    use tower::ServiceExt;

    use super::*;
    use crate::chat::types::ChatSession;
    use crate::scheduler::InMemoryCronStore;

    async fn test_app() -> (tempfile::TempDir, AppState) {
        let workspace = tempfile::tempdir().unwrap();
        let gcx = crate::global_context::tests::make_test_gcx().await;
        *gcx.documents_state.workspace_folders.lock().unwrap() =
            vec![workspace.path().to_path_buf()];
        (workspace, crate::app_state::app_state_from_gcx(gcx).await)
    }

    fn router(app: AppState) -> axum::Router {
        axum::Router::new()
            .route("/hooks/fire", post(handle_v1_hooks_fire))
            .with_state(app)
    }

    async fn request_json(
        router: axum::Router,
        body: Value,
        token: Option<&str>,
    ) -> (StatusCode, Value) {
        let mut builder = Request::builder()
            .method("POST")
            .uri("/hooks/fire")
            .header("content-type", "application/json");
        if let Some(token) = token {
            builder = builder.header("Authorization", format!("Bearer {token}"));
        }
        let response = router
            .oneshot(builder.body(Body::from(body.to_string())).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let body = hyper::body::to_bytes(response.into_body()).await.unwrap();
        (status, serde_json::from_slice(&body).unwrap())
    }

    async fn add_session(app: &AppState, chat_id: &str) {
        app.gcx.chat_sessions.write().await.insert(
            chat_id.to_string(),
            Arc::new(AMutex::new(ChatSession::new(chat_id.to_string()))),
        );
    }

    #[cfg(not(target_os = "windows"))]
    fn stdout_command() -> Vec<String> {
        vec!["printf".to_string(), "hi".to_string()]
    }

    #[cfg(target_os = "windows")]
    fn stdout_command() -> Vec<String> {
        vec!["cmd".to_string(), "/C".to_string(), "echo hi".to_string()]
    }

    #[cfg(not(target_os = "windows"))]
    fn failing_command() -> Vec<String> {
        vec!["sh".to_string(), "-c".to_string(), "exit 7".to_string()]
    }

    #[cfg(target_os = "windows")]
    fn failing_command() -> Vec<String> {
        vec!["cmd".to_string(), "/C".to_string(), "exit 7".to_string()]
    }

    #[tokio::test]
    async fn hooks_fire_auth_requires_daemon_bearer_when_configured() {
        let workspace = tempfile::tempdir().unwrap();
        let mut gcx = crate::global_context::tests::make_test_gcx().await;
        *gcx.documents_state.workspace_folders.lock().unwrap() =
            vec![workspace.path().to_path_buf()];
        Arc::get_mut(&mut gcx).unwrap().cmdline.daemon_auth_token = Some("secret".to_string());
        let app = crate::app_state::app_state_from_gcx(gcx).await;
        let router = router(app);

        let (status, _) =
            request_json(router.clone(), json!({"kind":"wake","text":"hello"}), None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);

        let (status, _) = request_json(
            router.clone(),
            json!({"kind":"wake","text":"hello"}),
            Some("wrong-secret"),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);

        let (status, value) = request_json(
            router,
            json!({"kind":"wake","text":"hello"}),
            Some("secret"),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(value["inline"]["kind"], json!("wake"));
    }

    #[tokio::test]
    async fn hooks_fire_wake_injects_system_notice() {
        let (_workspace, app) = test_app().await;
        add_session(&app, "chat-existing").await;
        let router = router(app.clone());

        let (status, value) =
            request_json(router, json!({"kind":"wake","text":"wake up"}), None).await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(value["inline"]["chat_id"], json!("chat-existing"));
        let session_arc = app
            .gcx
            .chat_sessions
            .read()
            .await
            .get("chat-existing")
            .cloned()
            .unwrap();
        let session = session_arc.lock().await;
        let message = session.messages.last().unwrap();
        assert_eq!(message.role, "event");
        assert_eq!(message.extra["event"]["subkind"], json!("system_notice"));
        assert_eq!(message.content.content_text_only(), "wake up");
    }

    #[tokio::test]
    async fn busy_wake_hook_defaults_to_append_without_generation_wake() {
        let (_workspace, app) = test_app().await;
        add_session(&app, "chat-existing").await;
        let arc = app
            .chat
            .sessions
            .read()
            .await
            .get("chat-existing")
            .cloned()
            .unwrap();
        arc.lock().await.start_stream().unwrap();
        let (status, _) = request_json(
            router(app.clone()),
            json!({"kind":"wake","text":"queued notice"}),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let mut session = arc.lock().await;
        assert_eq!(session.pending_deliveries.len(), 1);
        assert_eq!(session.pending_deliveries[0].push, PushMode::Append);
        assert!(!session.pending_deliveries[0].wake);
        session.finish_stream(Some("stop".into()));
        session.drain_pending_deliveries();
        assert_eq!(
            session.messages.last().unwrap().content.content_text_only(),
            "queued notice"
        );
    }

    #[tokio::test]
    async fn hooks_fire_agent_creates_isolated_turn() {
        let (_workspace, app) = test_app().await;
        let router = router(app.clone());

        let (status, value) = request_json(
            router,
            json!({"kind":"agent","message":"ship it","mode":"agent","model":"model-1"}),
            None,
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(value["inline"]["kind"], json!("agent"));
        let job_id = value["inline"]["job_id"].as_str().unwrap();
        let prefix = format!("cron_{job_id}_");
        let sessions = app.gcx.chat_sessions.read().await;
        let chat_id = sessions
            .keys()
            .find(|chat_id| chat_id.starts_with(&prefix))
            .cloned()
            .unwrap();
        let session_arc = sessions.get(&chat_id).cloned().unwrap();
        drop(sessions);
        let session = session_arc.lock().await;
        let queued = session.pending_deliveries.iter().any(|delivery| {
            delivery.messages.iter().any(|message| {
                message.role == "user" && message.content.content_text_only() == "ship it"
            })
        });
        let added = session.messages.iter().any(|message| {
            message.role == "user" && message.content.content_text_only() == "ship it"
        });
        assert!(queued || added);
    }

    #[tokio::test]
    async fn hooks_fire_agent_accepts_type_delivery_alias() {
        let (_workspace, app) = test_app().await;
        let router = router(app.clone());

        let (status, value) = request_json(
            router,
            json!({"kind":"agent","message":"ship typed","deliver":{"type":"chat"}}),
            None,
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(value["inline"]["kind"], json!("agent"));
        let job_id = value["inline"]["job_id"].as_str().unwrap();
        let prefix = format!("cron_{job_id}_");
        let sessions = app.gcx.chat_sessions.read().await;
        assert!(sessions.keys().any(|chat_id| chat_id.starts_with(&prefix)));
    }

    #[tokio::test]
    async fn hooks_fire_hook_id_runs_matching_webhook_jobs_only() {
        let (_workspace, app) = test_app().await;
        let store = session_cron_store();
        let hook_id = format!("deploy-{}", Uuid::now_v7());
        let mut matching = Job::new_cron_agent_chat(
            String::new(),
            String::new(),
            "Run hook command".to_string(),
            true,
            false,
            unix_now_ms(),
        );
        matching.id = format!("hook_test_{}", Uuid::now_v7());
        matching.trigger = Trigger::Webhook {
            hook_id: hook_id.clone(),
        };
        matching.action = Action::Command {
            argv: stdout_command(),
            target: AgentTarget::Isolated,
            cwd: None,
            env: None,
            timeout_secs: Some(5),
        };
        matching.delivery = Delivery::None;
        let matching_id = matching.id.clone();
        store.add(matching).await.unwrap();

        let mut other = Job::new_cron_agent_chat(
            String::new(),
            String::new(),
            "Other hook command".to_string(),
            true,
            false,
            unix_now_ms(),
        );
        other.id = format!("hook_test_other_{}", Uuid::now_v7());
        other.trigger = Trigger::Webhook {
            hook_id: "other".to_string(),
        };
        other.action = Action::Command {
            argv: stdout_command(),
            target: AgentTarget::Isolated,
            cwd: None,
            env: None,
            timeout_secs: Some(5),
        };
        other.delivery = Delivery::None;
        let other_id = other.id.clone();
        store.add(other).await.unwrap();

        let router = router(app);
        let (status, value) =
            request_json(router, json!({"kind":"wake","hook_id": hook_id}), None).await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(value["inline"], Value::Null);
        assert_eq!(value["hook_jobs"]["matched"], json!(1));
        assert_eq!(value["hook_jobs"]["fired"], json!(1));
        let matching = store.get(&matching_id).await.unwrap();
        assert_eq!(matching.last_status.as_deref(), Some("fired"));
        assert_eq!(matching.fire_count, 1);
        assert_eq!(matching.trigger_at_ms, None);
        let other = store.get(&other_id).await.unwrap();
        assert_eq!(other.last_status, None);
        store.remove(&matching_id).await.unwrap();
        store.remove(&other_id).await.unwrap();
    }

    #[tokio::test]
    async fn hooks_fire_one_shot_failed_job_reports_not_fired() {
        let (_workspace, app) = test_app().await;
        let store = session_cron_store();
        let hook_id = format!("deploy-fail-{}", Uuid::now_v7());
        let mut matching = Job::new_cron_agent_chat(
            String::new(),
            String::new(),
            "Run failing hook command".to_string(),
            false,
            false,
            unix_now_ms(),
        );
        matching.id = format!("hook_test_fail_{}", Uuid::now_v7());
        matching.trigger = Trigger::Webhook {
            hook_id: hook_id.clone(),
        };
        matching.action = Action::Command {
            argv: failing_command(),
            target: AgentTarget::Isolated,
            cwd: None,
            env: None,
            timeout_secs: Some(5),
        };
        matching.delivery = Delivery::None;
        matching.auto_expire_after_ms = 0;
        let matching_id = matching.id.clone();
        store.add(matching).await.unwrap();

        let router = router(app);
        let (status, value) =
            request_json(router, json!({"kind":"wake","hook_id": hook_id}), None).await;

        assert_eq!(status, StatusCode::FAILED_DEPENDENCY);
        assert!(value["detail"].as_str().unwrap().contains("none fired"));
        assert!(store.get(&matching_id).await.is_none());
    }

    #[tokio::test]
    async fn hooks_fire_unknown_hook_id_returns_not_found() {
        let (_workspace, app) = test_app().await;
        let hook_id = format!("missing-{}", Uuid::now_v7());
        let router = router(app);

        let (status, value) =
            request_json(router, json!({"kind":"wake","hook_id": hook_id}), None).await;

        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(value["detail"].as_str().unwrap().contains("hook not found"));
    }

    #[tokio::test]
    async fn hooks_fire_agent_and_hook_id_do_both() {
        let (_workspace, app) = test_app().await;
        let store = session_cron_store();
        let hook_id = format!("deploy-{}", Uuid::now_v7());
        let mut matching = Job::new_cron_agent_chat(
            String::new(),
            String::new(),
            "Run hook command".to_string(),
            true,
            false,
            unix_now_ms(),
        );
        matching.id = format!("hook_test_both_{}", Uuid::now_v7());
        matching.trigger = Trigger::Webhook {
            hook_id: hook_id.clone(),
        };
        matching.action = Action::Command {
            argv: stdout_command(),
            target: AgentTarget::Isolated,
            cwd: None,
            env: None,
            timeout_secs: Some(5),
        };
        matching.delivery = Delivery::None;
        let matching_id = matching.id.clone();
        store.add(matching).await.unwrap();

        let router = router(app.clone());
        let (status, value) = request_json(
            router,
            json!({"kind":"agent","message":"ship both","hook_id": hook_id}),
            None,
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(value["inline"]["kind"], json!("agent"));
        assert_eq!(value["hook_jobs"]["matched"], json!(1));
        assert_eq!(value["hook_jobs"]["fired"], json!(1));
        let matching = store.get(&matching_id).await.unwrap();
        assert_eq!(matching.last_status.as_deref(), Some("fired"));
        let job_id = value["inline"]["job_id"].as_str().unwrap();
        let prefix = format!("cron_{job_id}_");
        let sessions = app.gcx.chat_sessions.read().await;
        assert!(sessions.keys().any(|chat_id| chat_id.starts_with(&prefix)));
        drop(sessions);
        store.remove(&matching_id).await.unwrap();
    }

    #[tokio::test]
    async fn jobs_by_hook_id_matches_webhook_triggers_only() {
        let store = Arc::new(InMemoryCronStore::new());
        let mut matching = Job::new_cron_agent_chat(
            "*/5 * * * *".to_string(),
            "Check".to_string(),
            "Check".to_string(),
            true,
            false,
            1,
        );
        matching.id = "matching".to_string();
        matching.trigger = Trigger::Webhook {
            hook_id: "deploy".to_string(),
        };
        let mut nonmatching = matching.clone();
        nonmatching.id = "nonmatching".to_string();
        nonmatching.trigger = Trigger::Webhook {
            hook_id: "other".to_string(),
        };
        let mut timed = matching.clone();
        timed.id = "timed".to_string();
        timed.trigger = Trigger::Cron {
            expr: "*/5 * * * *".to_string(),
            tz: None,
        };
        store.add(matching).await.unwrap();
        store.add(nonmatching).await.unwrap();
        store.add(timed).await.unwrap();

        let jobs = store.jobs_by_hook_id("deploy").await;

        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].id, "matching");
    }
}
