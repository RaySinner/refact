pub use refact_runtime_api::files_blocklist::*;

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn forced_reload_replaces_the_cached_indexing_settings() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let config_path = gcx.config_dir.join("indexing.yaml");

        tokio::fs::write(&config_path, "blocklist:\n  - first/*\n")
            .await
            .unwrap();
        let first = reload_indexing_everywhere_now(gcx.clone()).await;
        assert_eq!(first.global.blocklist, vec!["first/*"]);

        tokio::fs::write(&config_path, "blocklist:\n  - second/*\n")
            .await
            .unwrap();
        let second = reload_indexing_everywhere_now(gcx.clone()).await;
        let cached = gcx.indexing_everywhere.read().unwrap().clone();

        assert_eq!(second.global.blocklist, vec!["second/*"]);
        assert_eq!(cached.global.blocklist, vec!["second/*"]);
    }
}
