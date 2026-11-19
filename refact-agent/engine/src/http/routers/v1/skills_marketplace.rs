pub use refact_http::routers::v1::skills_marketplace::*;

#[cfg(test)]
use axum::extract::{Query, State};

#[cfg(test)]
mod tests {
    use super::*;
    use refact_ext_runtime::ext_marketplace::configure_source;
    use crate::global_context::tests::make_test_gcx;

    #[tokio::test]
    async fn test_list_skills_marketplace_includes_embedded_source() {
        let gcx = make_test_gcx().await;
        let config_dir = gcx.config_dir.clone();
        configure_source(&config_dir, "refact-starter-skills", Some(true))
            .await
            .unwrap();

        let result = handle_v1_skills_marketplace_get(
            axum::extract::State(crate::app_state::app_state_from_gcx(gcx.clone()).await),
            Query(SkillsMarketplaceQuery {
                source: None,
                q: None,
            }),
        )
        .await
        .unwrap();
        let value = result.0;
        assert!(value["items"].as_array().unwrap().len() >= 1);
    }
}
