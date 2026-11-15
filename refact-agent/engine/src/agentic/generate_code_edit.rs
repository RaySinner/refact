use std::sync::Arc;

use crate::global_context::GlobalContext;
use refact_runtime_api::GlobalContextCtx;

pub use refact_agentic::generate_code_edit::{remove_markdown_fences, SUBAGENT_ID};

pub async fn generate_code_edit(
    gcx: Arc<GlobalContext>,
    code: &str,
    instruction: &str,
    cursor_file: &str,
    cursor_line: i32,
) -> Result<String, String> {
    refact_runtime_api::generate_code_edit_runtime::generate_code_edit(
        gcx.chat_ctx(gcx.clone()),
        code,
        instruction,
        cursor_file,
        cursor_line,
    )
    .await
}
