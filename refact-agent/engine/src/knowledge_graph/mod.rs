pub use refact_knowledge_graph::kg_structs;
pub use refact_knowledge_graph::kg_structs::KnowledgeFrontmatter;
pub use refact_knowledge_graph::kg_cleanup::{KgFileDeleter, KgGraphBuilder};
pub use refact_runtime_api::knowledge_graph::build_knowledge_graph;

pub mod kg_subchat;

use std::path::PathBuf;
use std::sync::Arc;

use crate::global_context::GlobalContext;

pub async fn knowledge_cleanup_background_task(gcx: Arc<GlobalContext>) {
    let shutdown_flag = gcx.shutdown_flag.clone();
    let cache_dir = gcx.cache_dir.clone();
    let gcx_for_build = gcx.clone();
    let build_graph: KgGraphBuilder = Arc::new(move || {
        let gcx = gcx_for_build.clone();
        Box::pin(async move { build_knowledge_graph(gcx).await })
    });
    let gcx_for_delete = gcx.clone();
    let delete_file: KgFileDeleter = Arc::new(move |path: PathBuf| {
        let gcx = gcx_for_delete.clone();
        Box::pin(async move { crate::memories::delete_document_from_disk(gcx, &path).await })
    });
    refact_knowledge_graph::kg_cleanup::knowledge_cleanup_background_task(
        shutdown_flag,
        cache_dir,
        build_graph,
        delete_file,
    )
    .await
}
pub async fn cleanup_inactive_memories_on_startup(gcx: Arc<GlobalContext>) {
    let gcx_for_build = gcx.clone();
    let build_graph: KgGraphBuilder = Arc::new(move || {
        let gcx = gcx_for_build.clone();
        Box::pin(async move { build_knowledge_graph(gcx).await })
    });
    let gcx_for_delete = gcx.clone();
    let delete_file: KgFileDeleter = Arc::new(move |path: PathBuf| {
        let gcx = gcx_for_delete.clone();
        Box::pin(async move { crate::memories::delete_document_from_disk(gcx, &path).await })
    });
    match refact_knowledge_graph::kg_cleanup::remove_inactive_memories(&build_graph, &delete_file)
        .await
    {
        Ok(0) => tracing::info!("startup memory cleanup: no inactive memories to remove"),
        Ok(deleted) => tracing::info!(
            "startup memory cleanup: removed {} inactive (non-active) memories",
            deleted
        ),
        Err(e) => tracing::warn!("startup memory cleanup failed: {}", e),
    }
}
