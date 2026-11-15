pub use refact_runtime_api::integrations::notifier::integr_notifier_telegram::*;

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::routing::post;
    use axum::{Json, Router};
    use serde_json::Value;
    use tokio::sync::Mutex as AMutex;

    use super::*;
    use refact_runtime_api::integrations::notifier::NotifierBackend;
    use serde_json::json;

    #[test]
    fn telegram_schema_validates_and_registration_resolves() {
        let integration = crate::integrations::integration_from_name(INTEGRATION_ID).unwrap();
        let schema: serde_yaml::Value = serde_yaml::from_str(integration.integr_schema()).unwrap();
        let schema_json = serde_json::to_value(schema).unwrap();
        let schema_struct: crate::integrations::yaml_schema::ISchema =
            serde_json::from_value(schema_json.clone()).unwrap();

        assert!(schema_struct.fields.contains_key("bot_token"));
        assert!(schema_struct.fields.contains_key("default_chat_id"));
        assert_eq!(
            schema_json["fields"]["bot_token"]["f_extra"]["password"],
            json!(true)
        );
        assert!(crate::integrations::integrations_list(true).contains(&INTEGRATION_ID));
    }

    #[tokio::test]
    async fn telegram_send_posts_expected_request() {
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
        let notifier = TelegramNotifier::with_api_base(
            TelegramNotifierConfig {
                bot_token: "secret-token".to_string(),
                default_chat_id: "default-chat".to_string(),
                ..TelegramNotifierConfig::default()
            },
            reqwest::Client::new(),
            format!("http://127.0.0.1:{port}"),
        );

        notifier
            .send(Some("target-chat"), "hello frogs")
            .await
            .unwrap();

        let received = received.lock().await;
        assert_eq!(received.len(), 1);
        assert_eq!(received[0]["chat_id"], json!("target-chat"));
        assert_eq!(received[0]["text"], json!("hello frogs"));
        server_task.abort();
    }

    #[tokio::test]
    async fn telegram_send_uses_default_target_and_redacts_token_on_failure() {
        let router = Router::new().route(
            "/botsecret-token/sendMessage",
            post(|| async { (axum::http::StatusCode::BAD_REQUEST, "bad secret-token") }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = axum::Server::from_tcp(listener.into_std().unwrap())
            .unwrap()
            .serve(router.into_make_service());
        let server_task = tokio::spawn(server);
        let notifier = TelegramNotifier::with_api_base(
            TelegramNotifierConfig {
                bot_token: "secret-token".to_string(),
                default_chat_id: "default-chat".to_string(),
                ..TelegramNotifierConfig::default()
            },
            reqwest::Client::new(),
            format!("http://127.0.0.1:{port}"),
        );

        let err = notifier.send(None, "hello frogs").await.unwrap_err();

        assert!(!err.contains("secret-token"));
        assert!(err.contains("[REDACTED]"));
        server_task.abort();
    }
}
