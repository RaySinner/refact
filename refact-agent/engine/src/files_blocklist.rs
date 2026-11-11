use std::sync::Arc;

use crate::global_context::GlobalContext;

pub use refact_files::indexing_settings::{
    is_blocklisted, load_indexing_yaml, IndexingEverywhere, IndexingSettings,
};

/// Engine wrapper over `refact_files::indexing_settings`: unwraps `GlobalContext` into explicit
/// values and forwards the `GlobalContext` pointer as the crate cache key.
pub async fn reload_global_indexing_only(gcx: Arc<GlobalContext>) -> IndexingEverywhere {
    let (config_dir, indexing_yaml) =
        { (gcx.config_dir.clone(), gcx.cmdline.indexing_yaml.clone()) };
    let global_indexing_path = if indexing_yaml.is_empty() {
        config_dir.join("indexing.yaml")
    } else {
        refact_files::path_utils::canonical_path(indexing_yaml)
    };
    IndexingEverywhere {
        global: load_indexing_yaml(&global_indexing_path, None)
            .await
            .unwrap_or_default(),
        vcs_indexing_settings_map: std::collections::HashMap::new(),
        loaded_ts: 0,
    }
}

pub async fn reload_indexing_everywhere_if_needed(
    gcx: Arc<GlobalContext>,
) -> Arc<IndexingEverywhere> {
    refact_runtime_api::gcx_helpers::reload_indexing_everywhere_for_gcx(&gcx, false).await
}

pub async fn reload_indexing_everywhere_now(gcx: Arc<GlobalContext>) -> Arc<IndexingEverywhere> {
    refact_runtime_api::gcx_helpers::reload_indexing_everywhere_for_gcx(&gcx, true).await
}

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
