pub use refact_http::routers::v1::performance::*;

#[cfg(test)]
mod tests {
    use hyper::{Body, Request, StatusCode};
    use tower::ServiceExt;

    use super::*;
    use crate::chat::perf_diagnostics::{PerfComponent, PerfOutcome};

    async fn test_router() -> axum::Router {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        crate::chat::perf_diagnostics::clear_process_telemetry_for_test();
        gcx.performance_telemetry.set_enabled(false);
        let app_state = crate::app_state::app_state_from_gcx(gcx).await;
        crate::http::routers::v1::make_v1_router(app_state.clone()).with_state(app_state)
    }

    async fn request(router: axum::Router, request: Request<Body>) -> serde_json::Value {
        let response = router.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        serde_json::from_slice(&hyper::body::to_bytes(response.into_body()).await.unwrap()).unwrap()
    }

    #[tokio::test]
    async fn perf_telemetry_http_reports_disabled_and_runtime_enable_disable_reset() {
        let router = test_router().await;
        let initial = request(
            router.clone(),
            Request::builder()
                .uri("/performance/telemetry")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(initial["schema_version"], 1);
        assert_eq!(initial["enabled"], false);
        assert_eq!(
            initial["components"].as_array().unwrap().len(),
            PerfComponent::ALL.len()
        );
        assert!(initial["rollout_switches"].is_object());

        crate::chat::perf_diagnostics::record(
            PerfComponent::ToolRuntime,
            Some("private-chat"),
            PerfOutcome::Success,
            9,
            None,
            None,
            None,
        );
        let disabled = request(
            router.clone(),
            Request::builder()
                .uri("/performance/telemetry")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(disabled["components"][25]["sample_count"], 0);

        let enabled = request(
            router.clone(),
            Request::builder()
                .method("POST")
                .uri("/performance/telemetry")
                .header("Content-Type", "application/json")
                .body(Body::from(r#"{"enabled":true}"#))
                .unwrap(),
        )
        .await;
        assert_eq!(enabled["enabled"], true);

        crate::chat::perf_diagnostics::record(
            PerfComponent::ToolRuntime,
            Some("private-chat"),
            PerfOutcome::Success,
            9,
            None,
            None,
            None,
        );
        let populated = request(
            router.clone(),
            Request::builder()
                .uri("/performance/telemetry")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(populated["components"][25]["sample_count"], 1);

        let reset = request(
            router.clone(),
            Request::builder()
                .method("POST")
                .uri("/performance/telemetry/reset")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(reset["reset"], true);
        assert_eq!(reset["enabled"], true);

        let disabled = request(
            router,
            Request::builder()
                .method("POST")
                .uri("/performance/telemetry")
                .header("Content-Type", "application/json")
                .body(Body::from(r#"{"enabled":false}"#))
                .unwrap(),
        )
        .await;
        assert_eq!(disabled["enabled"], false);
    }

    #[tokio::test]
    async fn perf_telemetry_http_response_has_no_private_event_fields() {
        let router = test_router().await;
        let response = request(
            router,
            Request::builder()
                .uri("/performance/telemetry")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        let rendered = response.to_string();
        for forbidden in ["chat_id", "path", "query", "prompt", "arguments", "content"] {
            assert!(!rendered.contains(&format!("\"{forbidden}\"")));
        }
    }
}
