pub use refact_http::routers::v1::privacy::*;

#[cfg(test)]
#[allow(unused_imports)]
use std::sync::Arc;
#[cfg(test)]
#[allow(unused_imports)]
use hyper::StatusCode;
#[cfg(test)]
#[allow(unused_imports)]
use refact_core::call_validation::ChatMessage;
#[cfg(test)]
#[allow(unused_imports)]
use refact_privacy::{FileRecord, PrivacyPolicy};
#[cfg(test)]
#[allow(unused_imports)]
use refact_runtime_api::app_state::AppState;

#[cfg(test)]
fn platform_supported_for_tests() -> bool {
    cfg!(all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ))
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::Request;
    use hyper::body::to_bytes;
    use serde_json::{json, Value};
    use tokio::sync::Mutex as AMutex;
    use tower::ServiceExt;

    use super::*;
    use crate::chat::types::ChatSession;
    use crate::privacy::{FilePrivacySettings, PrivacySettings};

    async fn test_app() -> (tempfile::TempDir, tempfile::TempDir, AppState) {
        let cache = tempfile::tempdir().unwrap();
        let config = tempfile::tempdir().unwrap();
        let gcx = crate::global_context::tests::make_test_gcx_with_dirs(
            cache.path().to_path_buf(),
            config.path().to_path_buf(),
        )
        .await;
        *gcx.privacy_settings.write().unwrap() = Arc::new(PrivacySettings {
            privacy_rules: FilePrivacySettings {
                blocked: Vec::new(),
                only_send_to_servers_I_control: Vec::new(),
            },
            loaded_ts: 0,
        });
        (cache, config, crate::app_state::app_state_from_gcx(gcx).await)
    }

    async fn json_request(router: axum::Router, request: Request<Body>) -> (StatusCode, Value) {
        let response = router.oneshot(request).await.unwrap();
        let status = response.status();
        let body = to_bytes(response.into_body()).await.unwrap();
        (status, serde_json::from_slice(&body).unwrap())
    }

    fn post_json(uri: &str, body: Value) -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri(uri)
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap()
    }

    fn policy(secret_path: &str) -> Value {
        json!({
            "blocked": [],
            "zones": [
                {
                    "name": "secrets",
                    "patterns": [secret_path],
                    "send_to": ["trusted"],
                    "on_shell_read": "withhold"
                },
                {
                    "name": "normal",
                    "patterns": ["**"],
                    "send_to": ["*"],
                    "on_shell_read": "withhold"
                }
            ],
            "subagents": { "report_declassifies": true }
        })
    }

    #[tokio::test]
    async fn privacy_policy_post_defers_discovery_and_get_returns_complete_response() {
        let (_cache, config, app) = test_app().await;
        let secret = config.path().join("secret.env");
        let normal = config.path().join("normal.rs");
        tokio::fs::write(&secret, "secret").await.unwrap();
        tokio::fs::write(&normal, "normal").await.unwrap();
        *app.gcx.documents_state.workspace_files.lock().unwrap() =
            vec![secret.clone(), normal.clone()];
        tokio::fs::write(
            config.path().join("privacy.yaml"),
            "hooks:\n  trusted_projects:\n    - /kept\n",
        )
        .await
        .unwrap();
        let router = crate::http::routers::make_refact_http_server(app);

        let (status, posted) = json_request(
            router.clone(),
            post_json("/v1/privacy/policy", policy(&secret.to_string_lossy())),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(posted["policy"]["zones"][0]["name"], "secrets");
        assert_eq!(posted["match_counts"], json!({}));
        assert_eq!(posted["destinations"], json!([]));
        assert_eq!(posted["discovery_deferred"], true);
        assert!(posted["error"].is_null());
        assert_eq!(
            posted["source_paths"],
            json!([config
                .path()
                .join("privacy.yaml")
                .to_string_lossy()
                .into_owned()])
        );
        assert_eq!(posted["has_project_overrides"], false);

        let (status, loaded) = json_request(
            router,
            Request::builder()
                .uri("/v1/privacy/policy")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(loaded["policy"], posted["policy"]);
        assert_eq!(loaded["discovery_deferred"], false);
        assert_eq!(loaded["match_counts"]["secrets"], 1);
        assert_eq!(loaded["match_counts"]["normal"], 1);
        assert!(loaded["destinations"]
            .as_array()
            .unwrap()
            .iter()
            .any(|destination| destination["id"] == "trusted"));
        let saved = tokio::fs::read_to_string(config.path().join("privacy.yaml"))
            .await
            .unwrap();
        let saved: serde_yaml::Value = serde_yaml::from_str(&saved).unwrap();
        assert_eq!(saved["hooks"]["trusted_projects"][0], "/kept");
    }

    #[tokio::test]
    async fn privacy_policy_save_replaces_and_backs_up_malformed_config() {
        let (_cache, config, app) = test_app().await;
        let path = config.path().join("privacy.yaml");
        let malformed = "privacy_rules: [";
        tokio::fs::write(&path, malformed).await.unwrap();
        let router = crate::http::routers::make_refact_http_server(app);

        let (status, response) =
            json_request(router, post_json("/v1/privacy/policy", policy("**/.env"))).await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(response["policy"]["zones"][0]["name"], "secrets");
        let saved = tokio::fs::read_to_string(&path).await.unwrap();
        let saved: serde_yaml::Value = serde_yaml::from_str(&saved).unwrap();
        assert_eq!(saved["privacy_rules"]["zones"][0]["name"], "secrets");
        let mut backups = tokio::fs::read_dir(config.path()).await.unwrap();
        let mut backup_paths = Vec::new();
        while let Some(entry) = backups.next_entry().await.unwrap() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with("privacy.yaml.corrupt-") {
                backup_paths.push(entry.path());
            }
        }
        assert_eq!(backup_paths.len(), 1);
        assert_eq!(
            tokio::fs::read_to_string(&backup_paths[0]).await.unwrap(),
            malformed
        );
    }

    #[tokio::test]
    async fn privacy_policy_get_surfaces_last_load_error() {
        let (_cache, config, app) = test_app().await;
        tokio::fs::write(config.path().join("privacy.yaml"), "privacy_rules: [")
            .await
            .unwrap();
        let router = crate::http::routers::make_refact_http_server(app);

        let (status, response) = json_request(
            router,
            Request::builder()
                .uri("/v1/privacy/policy")
                .body(Body::empty())
                .unwrap(),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert!(response["error"]
            .as_str()
            .unwrap()
            .contains("failed to parse"));
    }

    #[tokio::test]
    async fn privacy_status_before_observation_is_not_optimistic() {
        let (_cache, config, app) = test_app().await;
        tokio::fs::write(config.path().join("privacy.yaml"), "privacy_rules: [")
            .await
            .unwrap();
        let router = crate::http::routers::make_refact_http_server(app);

        let (status, response) = json_request(
            router,
            Request::builder()
                .uri("/v1/privacy/status")
                .body(Body::empty())
                .unwrap(),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(response["platform"], std::env::consts::OS);
        assert!(response["config_error"]
            .as_str()
            .unwrap()
            .contains("failed to parse"));
        assert_eq!(
            response["observation"]["platform_supported"],
            platform_supported_for_tests()
        );
        assert_eq!(response["observation"]["runtime_available"], false);
        assert!(response["observation"]["last_error"].is_null());
    }

    #[tokio::test]
    async fn privacy_status_keeps_platform_support_when_runtime_is_unavailable() {
        let (_cache, _config, app) = test_app().await;
        crate::privacy::record_observation_status(
            &app.gcx.privacy_observation_runtime,
            &refact_exec::ObservationStatus::Unavailable("ptrace denied".to_string()),
        );
        let router = crate::http::routers::make_refact_http_server(app);

        let (status, response) = json_request(
            router,
            Request::builder()
                .uri("/v1/privacy/status")
                .body(Body::empty())
                .unwrap(),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            response["observation"]["platform_supported"],
            platform_supported_for_tests()
        );
        assert_eq!(response["observation"]["runtime_available"], false);
        assert_eq!(response["observation"]["last_error"], "ptrace denied");
    }

    #[tokio::test]
    async fn privacy_derived_lists_sorted_entries_and_clears_them() {
        let (_cache, _config, app) = test_app().await;
        let session = ChatSession::new("chat-derived".to_string());
        session.derived_privacy_zones.write().unwrap().insert(
            std::path::PathBuf::from("/proj/b.txt"),
            crate::privacy::records::DerivedZone {
                zone: "secrets".to_string(),
                origin: Some("/proj/secret.env".to_string()),
            },
        );
        session.derived_privacy_zones.write().unwrap().insert(
            std::path::PathBuf::from("/proj/a.txt"),
            crate::privacy::records::DerivedZone {
                zone: "secrets".to_string(),
                origin: None,
            },
        );
        app.gcx
            .chat_sessions
            .write()
            .await
            .insert("chat-derived".to_string(), Arc::new(AMutex::new(session)));
        let router = crate::http::routers::make_refact_http_server(app);

        let (status, listed) = json_request(
            router.clone(),
            post_json("/v1/privacy/derived", json!({"chat_id": "chat-derived"})),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(listed["chat_id"], "chat-derived");
        assert_eq!(listed["entries"][0]["path"], "/proj/a.txt");
        assert_eq!(listed["entries"][0]["zone"], "secrets");
        assert!(listed["entries"][0]["origin"].is_null());
        assert_eq!(listed["entries"][1]["path"], "/proj/b.txt");
        assert_eq!(listed["entries"][1]["origin"], "/proj/secret.env");

        let (status, cleared) = json_request(
            router.clone(),
            post_json(
                "/v1/privacy/derived/clear",
                json!({"chat_id": "chat-derived", "paths": ["/proj/a.txt"]}),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(cleared["cleared"], 1);
        assert_eq!(cleared["remaining"], 1);

        let (status, cleared) = json_request(
            router.clone(),
            post_json(
                "/v1/privacy/derived/clear",
                json!({"chat_id": "chat-derived"}),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(cleared["cleared"], 1);
        assert_eq!(cleared["remaining"], 0);

        let (status, _) = json_request(
            router,
            post_json("/v1/privacy/derived", json!({"chat_id": "missing"})),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn privacy_inspect_reports_allowed_messages_and_blocking_records() {
        let (_cache, config, app) = test_app().await;
        let secret = config.path().join("secret.env");
        let parsed: PrivacyPolicy =
            serde_json::from_value(policy(&secret.to_string_lossy())).unwrap();
        *app.gcx.privacy_policy_load.write().unwrap() = refact_privacy::PolicyLoad {
            policy: Arc::new(parsed),
            error: None,
            source_paths: Vec::new(),
        };
        let mut message = ChatMessage::new("user".to_string(), "use the secret".to_string());
        refact_privacy::attach_record(
            &mut message,
            FileRecord {
                path: secret.to_string_lossy().into_owned(),
                zone: "secrets".to_string(),
                attribution: refact_privacy::Attribution::Declared,
            },
        );
        let mut session = ChatSession::new("chat-1".to_string());
        session.messages.push(message);
        app.gcx
            .chat_sessions
            .write()
            .await
            .insert("chat-1".to_string(), Arc::new(AMutex::new(session)));
        let router = crate::http::routers::make_refact_http_server(app);
        let destination = |id: &str| {
            json!({
                "id": id,
                "kind": "provider",
                "display_name": id
            })
        };

        let (status, blocked) = json_request(
            router.clone(),
            post_json(
                "/v1/privacy/inspect",
                json!({"chat_id": "chat-1", "destination": destination("untrusted")}),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(blocked["sendable"], false);
        assert_eq!(blocked["would_send"], json!([]));
        assert_eq!(blocked["blocked"][0]["record_index"], 0);
        assert_eq!(blocked["blocked"][0]["record"]["zone"], "secrets");

        let (status, allowed) = json_request(
            router,
            post_json(
                "/v1/privacy/inspect",
                json!({"chat_id": "chat-1", "destination": destination("trusted")}),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(allowed["sendable"], true);
        assert_eq!(allowed["would_send"].as_array().unwrap().len(), 1);
        assert!(allowed["would_send"][0]["privacy"].is_null());
        assert_eq!(allowed["blocked"], json!([]));
    }
}
