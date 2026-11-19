pub use refact_http::routers::v1::commands_marketplace::*;

#[cfg(test)]
use axum::extract::State;
#[cfg(test)]
use hyper::StatusCode;
#[cfg(test)]
use serde_json::json;

#[cfg(test)]
mod tests {
    use super::*;
    use refact_ext_runtime::ext_marketplace::configure_source;
    use crate::global_context::tests::make_test_gcx;

    #[tokio::test]
    async fn test_install_command_conflict_maps_to_conflict_status() {
        let gcx = make_test_gcx().await;
        {
            let config_dir = gcx.config_dir.clone();
            configure_source(&config_dir, "refact-starter-commands", Some(true))
                .await
                .unwrap();
            tokio::fs::create_dir_all(config_dir.join("commands"))
                .await
                .unwrap();
            tokio::fs::write(config_dir.join("commands").join("review.md"), "existing")
                .await
                .unwrap();
        }

        let body = serde_json::to_vec(&serde_json::json!({
            "source_id": "refact-starter-commands",
            "item_id": "review",
            "scope": "global",
        }))
        .unwrap();

        let result = handle_v1_commands_marketplace_install(
            axum::extract::State(crate::app_state::app_state_from_gcx(gcx.clone()).await),
            hyper::body::Bytes::from(body),
        )
        .await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.status_code, StatusCode::CONFLICT);
    }
}
