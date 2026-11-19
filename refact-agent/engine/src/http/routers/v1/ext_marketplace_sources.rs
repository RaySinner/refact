pub use refact_http::routers::v1::ext_marketplace_sources::*;

#[cfg(test)]
use axum::extract::State;
#[cfg(test)]
use serde_json::json;

#[cfg(test)]
mod tests {
    use super::*;
    use refact_ext_runtime::ext_marketplace::load_all_sources;
    use crate::global_context::tests::make_test_gcx;

    #[tokio::test]
    async fn test_add_marketplace_source_persists() {
        let gcx = make_test_gcx().await;
        let body = serde_json::to_vec(&serde_json::json!({
            "url": "https://github.com/example/repo",
            "enabled": true,
        }))
        .unwrap();

        let result = handle_v1_ext_marketplace_sources_post(
            axum::extract::State(crate::app_state::app_state_from_gcx(gcx.clone()).await),
            hyper::body::Bytes::from(body),
        )
        .await;
        assert!(result.is_ok());

        let config_dir = gcx.config_dir.clone();
        let sources = load_all_sources(&config_dir).await.unwrap();
        assert!(sources.iter().any(|s| s.id == "example-repo"));
    }

    #[tokio::test]
    async fn test_add_marketplace_source_rejects_non_root_url() {
        let gcx = make_test_gcx().await;
        let body = serde_json::to_vec(&serde_json::json!({
            "url": "https://github.com/example/repo/tree/main",
            "enabled": true,
        }))
        .unwrap();

        let result = handle_v1_ext_marketplace_sources_post(
            axum::extract::State(crate::app_state::app_state_from_gcx(gcx.clone()).await),
            hyper::body::Bytes::from(body),
        )
        .await;
        assert!(result.is_err());
    }
}
