use std::path::PathBuf;

use hashbrown::HashMap;

use crate::global_context::GlobalContext;
use refact_runtime_api::GlobalContextCtx;

pub use refact_agentic::generate_commit_message::{remove_fencing, SUBAGENT_ID};

pub async fn generate_commit_message_by_diff(
    gcx: std::sync::Arc<GlobalContext>,
    diff: &String,
    commit_message_prompt: &Option<String>,
    parent_chat_id: Option<&str>,
) -> Result<String, String> {
    refact_runtime_api::commit_message_runtime::generate_commit_message_by_diff(
        gcx.chat_ctx(gcx.clone()),
        diff,
        commit_message_prompt,
        parent_chat_id,
    )
    .await
}

pub async fn _generate_commit_message_for_projects(
    gcx: std::sync::Arc<GlobalContext>,
) -> Result<HashMap<PathBuf, String>, String> {
    refact_runtime_api::commit_message_runtime::generate_commit_message_for_projects(
        gcx.chat_ctx(gcx.clone()),
    )
    .await
}
