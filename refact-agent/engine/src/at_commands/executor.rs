use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use refact_chat_api::ContextFile;
use refact_runtime_api::at_tree::TreeNode;
use refact_runtime_api::tool_ctx::ToolCtx;
use refact_runtime_api::{AtCommandExecutor, AtCommandsContext};
use tokio::sync::Mutex as AMutex;

pub struct EngineAtCommandExecutor;

#[async_trait]
impl AtCommandExecutor for EngineAtCommandExecutor {
    async fn file_repair_candidates(
        &self,
        tool: &ToolCtx,
        value: &String,
        top_n: usize,
        fuzzy: bool,
    ) -> Vec<String> {
        super::at_file::file_repair_candidates_impl(tool, value, top_n, fuzzy).await
    }

    async fn return_one_candidate_or_a_good_error(
        &self,
        tool: &ToolCtx,
        file_path: &String,
        candidates: &Vec<String>,
        project_paths: &Vec<PathBuf>,
        dirs: bool,
    ) -> Result<String, String> {
        super::at_file::return_one_candidate_or_a_good_error_impl(
            tool,
            file_path,
            candidates,
            project_paths,
            dirs,
        )
        .await
    }

    async fn execute_at_search(
        &self,
        ccx: Arc<AMutex<AtCommandsContext>>,
        query: &String,
        vecdb_scope_filter_mb: Option<String>,
    ) -> Result<Vec<ContextFile>, String> {
        super::at_search::execute_at_search_impl(ccx, query, vecdb_scope_filter_mb).await
    }

    async fn tree_for_tools_ex(
        &self,
        ccx: Arc<AMutex<AtCommandsContext>>,
        tree: &TreeNode,
        use_ast: bool,
        max_files: usize,
        is_root_query: bool,
        build_truncated: bool,
    ) -> Result<String, String> {
        super::at_tree::tree_for_tools_ex_impl(
            ccx,
            tree,
            use_ast,
            max_files,
            is_root_query,
            build_truncated,
        )
        .await
    }
}
