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
    crate::gcx_helpers::reload_indexing_everywhere_for_gcx(&gcx, false).await
}

pub async fn reload_indexing_everywhere_now(gcx: Arc<GlobalContext>) -> Arc<IndexingEverywhere> {
    crate::gcx_helpers::reload_indexing_everywhere_for_gcx(&gcx, true).await
}

