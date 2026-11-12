use std::collections::HashSet;
use std::path::PathBuf;

use refact_files::file_filter::KNOWLEDGE_FOLDER_NAME;
use refact_knowledge_graph::kg_structs::KnowledgeGraph;

use crate::files_correction::get_project_dirs;
use crate::gcx_helpers::reload_indexing_everywhere_for_gcx;
use crate::global_context::{GlobalContextCtx, SharedGlobalContext};
use crate::memories::get_global_knowledge_dir_for_ctx;

/// Resolves the knowledge dirs and the workspace file list from `GlobalContext` and delegates
/// the pure graph construction to `refact_knowledge_graph::kg_builder::build_knowledge_graph`.
pub async fn build_knowledge_graph(gcx: SharedGlobalContext) -> KnowledgeGraph {
    let project_dirs = get_project_dirs(gcx.clone()).await;
    let mut knowledge_dirs: Vec<PathBuf> = project_dirs
        .iter()
        .map(|d| d.join(KNOWLEDGE_FOLDER_NAME))
        .filter(|d| d.exists())
        .collect();
    let global_dir = get_global_knowledge_dir_for_ctx(&gcx.chat_ctx(gcx.clone()));
    if global_dir.exists() {
        knowledge_dirs.push(global_dir);
    }
    let workspace_files = collect_workspace_files(gcx).await;
    refact_knowledge_graph::kg_builder::build_knowledge_graph(knowledge_dirs, workspace_files).await
}

async fn collect_workspace_files(gcx: SharedGlobalContext) -> HashSet<String> {
    let project_dirs = get_project_dirs(gcx.clone()).await;
    let mut files = HashSet::new();
    for dir in project_dirs {
        let indexing = reload_indexing_everywhere_for_gcx(&gcx, false).await;
        if let Ok(paths) = refact_files::workspace::ls_files(&*indexing, &dir, true) {
            for path in paths {
                if let Ok(rel) = path.strip_prefix(&dir) {
                    files.insert(rel.to_string_lossy().to_string());
                }
                files.insert(path.to_string_lossy().to_string());
            }
        }
    }
    files
}
