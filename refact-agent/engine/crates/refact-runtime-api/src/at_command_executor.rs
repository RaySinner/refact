use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use refact_chat_api::ContextFile;
use tokio::sync::Mutex as AMutex;

use crate::at_commands_ctx::AtCommandsContext;
use crate::at_tree::TreeNode;
use crate::tool_ctx::ToolCtx;

#[async_trait]
pub trait AtCommandExecutor: Send + Sync {
    async fn file_repair_candidates(
        &self,
        tool: &ToolCtx,
        value: &String,
        top_n: usize,
        fuzzy: bool,
    ) -> Vec<String>;

    async fn return_one_candidate_or_a_good_error(
        &self,
        tool: &ToolCtx,
        file_path: &String,
        candidates: &Vec<String>,
        project_paths: &Vec<PathBuf>,
        dirs: bool,
    ) -> Result<String, String>;

    async fn execute_at_search(
        &self,
        ccx: Arc<AMutex<AtCommandsContext>>,
        query: &String,
        vecdb_scope_filter_mb: Option<String>,
    ) -> Result<Vec<ContextFile>, String>;

    async fn tree_for_tools_ex(
        &self,
        ccx: Arc<AMutex<AtCommandsContext>>,
        tree: &TreeNode,
        use_ast: bool,
        max_files: usize,
        is_root_query: bool,
        build_truncated: bool,
    ) -> Result<String, String>;
}

pub async fn file_repair_candidates(
    tool: &ToolCtx,
    value: &String,
    top_n: usize,
    fuzzy: bool,
) -> Vec<String> {
    tool.at_command_executor
        .file_repair_candidates(tool, value, top_n, fuzzy)
        .await
}

pub async fn return_one_candidate_or_a_good_error(
    tool: &ToolCtx,
    file_path: &String,
    candidates: &Vec<String>,
    project_paths: &Vec<PathBuf>,
    dirs: bool,
) -> Result<String, String> {
    tool.at_command_executor
        .return_one_candidate_or_a_good_error(tool, file_path, candidates, project_paths, dirs)
        .await
}

pub async fn execute_at_search(
    ccx: Arc<AMutex<AtCommandsContext>>,
    query: &String,
    vecdb_scope_filter_mb: Option<String>,
) -> Result<Vec<ContextFile>, String> {
    let executor = ccx.lock().await.tool.at_command_executor.clone();
    executor
        .execute_at_search(ccx, query, vecdb_scope_filter_mb)
        .await
}

pub async fn tree_for_tools_ex(
    ccx: Arc<AMutex<AtCommandsContext>>,
    tree: &TreeNode,
    use_ast: bool,
    max_files: usize,
    is_root_query: bool,
    build_truncated: bool,
) -> Result<String, String> {
    let executor = ccx.lock().await.tool.at_command_executor.clone();
    executor
        .tree_for_tools_ex(
            ccx,
            tree,
            use_ast,
            max_files,
            is_root_query,
            build_truncated,
        )
        .await
}
