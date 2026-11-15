use std::path::Path;
use std::sync::Arc;

use refact_core::memory_plane::MemoryPlaneRoots;

use crate::global_context::GlobalContext;

pub use crate::memories_runtime::memory_plane_roots_for_ctx;

pub async fn memory_plane_roots(gcx: Arc<GlobalContext>) -> MemoryPlaneRoots {
    crate::memories_runtime::memory_plane_roots(gcx).await
}

pub fn partition_paths(paths: &[String], roots: &MemoryPlaneRoots) -> (Vec<String>, Vec<String>) {
    let mut memory_paths = Vec::new();
    let mut code_paths = Vec::new();
    for path in paths {
        if roots.classify_file(Path::new(path)).is_some() {
            memory_paths.push(path.clone());
        } else {
            code_paths.push(path.clone());
        }
    }
    (memory_paths, code_paths)
}

pub async fn route_index_enqueue(
    gcx: Arc<GlobalContext>,
    paths: &[String],
    process_immediately: bool,
    vecdb_only: bool,
) {
    let paths: Vec<String> = paths
        .iter()
        .filter(|path| !refact_files::file_filter::is_transient_tmp_path(Path::new(path.as_str())))
        .cloned()
        .collect();
    if paths.is_empty() {
        return;
    }

    let roots = memory_plane_roots(gcx.clone()).await;
    let (memory_paths, code_paths) = partition_paths(&paths, &roots);

    let vec_db = gcx.vec_db.clone();

    if !memory_paths.is_empty() {
        if let Some(ref mut db) = *vec_db.lock().await {
            db.vectorizer_enqueue_files(&memory_paths, process_immediately, roots.clone())
                .await;
        }
    }

    if vecdb_only {
        return;
    }

    if !code_paths.is_empty() {
        let codegraph = gcx.codegraph.lock().await.clone();
        match codegraph {
            Some(service) => service.enqueue_files(&code_paths),
            None => {
                tracing::warn!(
                    "codegraph unavailable; skipping {} code file(s) (memory-plane vec_db never receives code)",
                    code_paths.len()
                );
            }
        }
    }
}
