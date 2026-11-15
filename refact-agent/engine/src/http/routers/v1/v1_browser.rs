pub use refact_http::routers::v1::v1_browser::*;

#[cfg(test)]
use hyper::StatusCode;

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use hyper::body::to_bytes;
    use tower::ServiceExt;

    #[test]
    fn test_handoff_body_deserialize() {
        let json = r#"{"from_chat_id":"chat-1","to_chat_id":"chat-2"}"#;
        let body: HandoffBody = serde_json::from_str(json).unwrap();
        assert_eq!(body.from_chat_id, "chat-1");
        assert_eq!(body.to_chat_id, "chat-2");
    }

    #[test]
    fn test_format_curl_minimal_get() {
        let entry = refact_integrations::browser_types::NetworkEntry {
            timestamp: 1.0,
            method: "GET".to_string(),
            url: "https://example.com/api?key=123".to_string(),
            resource_type: "Fetch".to_string(),
            status: Some(200),
            ..refact_integrations::browser_types::NetworkEntry::default()
        };
        let curl = format_curl_minimal(&entry);
        assert_eq!(curl, "curl 'https://example.com/api?key=123'");
    }

    #[test]
    fn test_format_curl_minimal_post() {
        let entry = refact_integrations::browser_types::NetworkEntry {
            timestamp: 1.0,
            method: "POST".to_string(),
            url: "https://example.com/api".to_string(),
            resource_type: "XHR".to_string(),
            status: Some(201),
            ..refact_integrations::browser_types::NetworkEntry::default()
        };
        let curl = format_curl_minimal(&entry);
        assert_eq!(curl, "curl -X POST 'https://example.com/api'");
    }

    #[test]
    fn test_format_curl_minimal_empty_method() {
        let entry = refact_integrations::browser_types::NetworkEntry {
            timestamp: 1.0,
            method: String::new(),
            url: "https://example.com".to_string(),
            resource_type: "Document".to_string(),
            status: Some(200),
            ..refact_integrations::browser_types::NetworkEntry::default()
        };
        let curl = format_curl_minimal(&entry);
        assert_eq!(curl, "curl 'https://example.com'");
    }

    #[test]
    fn test_resize_screenshot_small_image() {
        let img = image::RgbImage::new(100, 100);
        let mut buf = Vec::new();
        img.write_to(
            &mut std::io::Cursor::new(&mut buf),
            image::ImageFormat::Jpeg,
        )
        .unwrap();
        let policy = refact_core::image_policy::ImagePolicy::from_metadata(Some(800), Some(800));
        let (result, mime) =
            refact_core::image_policy::resize_to_policy(&buf, "image/jpeg", &policy).unwrap();
        assert!(!result.is_empty());
        assert_eq!(mime, "image/png");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn test_browser_action_route_rejects_invalid_json() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let app_state = crate::app_state::app_state_from_gcx(gcx.clone()).await;
        let router = crate::http::routers::make_refact_http_server(app_state);

        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/browser/action")
                    .header("content-type", "application/json")
                    .body(Body::from("{not valid json"))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);

        let body = to_bytes(response.into_body()).await.unwrap();
        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let detail = payload["detail"].as_str().unwrap_or_default();
        assert!(detail.contains("JSON problem"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn test_browser_action_route_returns_not_found_without_runtime() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let app_state = crate::app_state::app_state_from_gcx(gcx.clone()).await;
        let router = crate::http::routers::make_refact_http_server(app_state);

        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/browser/action")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "chat_id": "missing-chat",
                            "steps": [{"action": "screenshot"}]
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);

        let body = to_bytes(response.into_body()).await.unwrap();
        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let detail = payload["detail"].as_str().unwrap_or_default();
        assert!(detail.contains("No browser runtime for chat_id=missing-chat"));
    }
}
