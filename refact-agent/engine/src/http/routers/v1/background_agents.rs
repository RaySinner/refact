pub use refact_http::routers::v1::background_agents::*;

#[cfg(test)]
use hyper::StatusCode;
#[cfg(test)]
use serde_json::json;
#[cfg(test)]
use serde_json::Value;
#[cfg(test)]
use refact_core::chat_types::PushMode;
#[cfg(test)]
use crate::app_state::AppState;

#[cfg(test)]
mod tests {
    use super::*;

    use axum::body::Body;
    use axum::http::Request;
    use hyper::body::to_bytes;
    use tower::ServiceExt;

    use crate::agents::types::{BgAgentKind, CreateAgentRequest};

    fn create_request(parent_chat_id: &str, title: &str) -> CreateAgentRequest {
        CreateAgentRequest {
            parent_chat_id: parent_chat_id.to_string(),
            parent_root_chat_id: Some("root".to_string()),
            parent_tool_call_id: None,
            kind: BgAgentKind::Subagent,
            config_name: "subagent".to_string(),
            title: title.to_string(),
            prompt: title.to_string(),
            target_files: vec![],
            model: "test/model".to_string(),
            model_type: None,
            goal_summary: None,
            plan_present: false,
            worktree_id: None,
            worktree_branch: None,
        }
    }

    async fn app() -> AppState {
        crate::app_state::app_state_from_gcx(crate::global_context::tests::make_test_gcx().await).await
    }

    async fn request(app: AppState, method: &str, uri: String, body: Value) -> (StatusCode, Value) {
        let response = crate::http::routers::make_refact_http_server(app)
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(uri)
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let body = to_bytes(response.into_body()).await.unwrap();
        let json = serde_json::from_slice(&body).unwrap_or(Value::Null);
        (status, json)
    }

    #[test]
    fn message_push_defaults_and_validates_modes() {
        let request: MessageRequest = serde_json::from_value(json!({"text": "hello"})).unwrap();
        assert_eq!(request.push, PushMode::Append);
        for mode in [PushMode::Append, PushMode::Preempt, PushMode::WhenIdle] {
            let request: MessageRequest =
                serde_json::from_value(json!({"text": "hello", "push": mode})).unwrap();
            assert_eq!(request.push, mode);
        }
        assert!(
            serde_json::from_value::<MessageRequest>(json!({"text": "hello", "push": true}))
                .is_err()
        );
    }

    #[tokio::test]
    async fn message_preserves_explicit_push_for_queued_agent() {
        let app = app().await;
        let (agent, _, _) = app
            .agents
            .create(create_request("root", "child"))
            .await
            .unwrap();
        let (status, _) = request(
            app.clone(),
            "POST",
            format!("/v1/background-agents/{}/message", agent.agent_id),
            json!({"chat_id": "root", "text": "hello", "push": "when_idle"}),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let record = app.agents.get_any(&agent.agent_id).await.unwrap();
        assert_eq!(record.pending_deliveries.len(), 1);
        assert_eq!(record.pending_deliveries[0].push, PushMode::WhenIdle);
    }

    #[tokio::test]
    async fn message_returns_not_found_and_terminal_conflict() {
        let app = app().await;
        let (status, _) = request(
            app.clone(),
            "POST",
            "/v1/background-agents/missing/message".to_string(),
            json!({ "text": "hello" }),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        let (agent, _, _) = app
            .agents
            .create(create_request("root", "done"))
            .await
            .unwrap();
        app.agents
            .mark_cancelled(&agent.agent_id, Some("done".to_string()))
            .await
            .unwrap();
        let (status, _) = request(
            app,
            "POST",
            format!("/v1/background-agents/{}/message", agent.agent_id),
            json!({ "text": "hello" }),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn get_tree_and_cancel_subtree_return_summaries() {
        let app = app().await;
        let (child, child_abort, _) = app
            .agents
            .create(create_request("root", "child"))
            .await
            .unwrap();
        app.agents
            .mark_running(&child.agent_id, "child-chat".to_string())
            .await
            .unwrap();
        let (grandchild, grandchild_abort, _) = app
            .agents
            .create(create_request("child-chat", "grandchild"))
            .await
            .unwrap();
        app.agents
            .mark_running(&grandchild.agent_id, "grandchild-chat".to_string())
            .await
            .unwrap();

        let (status, tree) = request(
            app.clone(),
            "GET",
            "/v1/background-agents?chat_id=child-chat".to_string(),
            Value::Null,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(tree.as_array().unwrap().len(), 2);

        let (status, cancelled) = request(
            app,
            "POST",
            format!("/v1/background-agents/{}/cancel", child.agent_id),
            json!({}),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            cancelled["cancelled_agent_ids"].as_array().unwrap().len(),
            2
        );
        assert!(child_abort.load(std::sync::atomic::Ordering::SeqCst));
        assert!(grandchild_abort.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[tokio::test]
    async fn message_and_cancel_enforce_present_chat_ownership() {
        let app = app().await;
        let (agent, _, _) = app
            .agents
            .create(create_request("root", "child"))
            .await
            .unwrap();

        let (status, _) = request(
            app.clone(),
            "POST",
            format!("/v1/background-agents/{}/message", agent.agent_id),
            json!({ "chat_id": "other", "text": "hello" }),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);

        let (status, _) = request(
            app.clone(),
            "POST",
            format!("/v1/background-agents/{}/cancel", agent.agent_id),
            json!({ "chat_id": "other" }),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);

        let (status, _) = request(
            app,
            "POST",
            format!("/v1/background-agents/{}/message", agent.agent_id),
            json!({ "chat_id": "root", "text": "hello" }),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
    }
}
