pub use refact_agentic::generate_code_edit::{remove_markdown_fences, SUBAGENT_ID};

use crate::global_context::GlobalContext;
use crate::subchat::{run_subchat_once, TraceParent};
use crate::yaml_configs::customization_registry::get_subagent_config;
use std::sync::Arc;

pub async fn generate_code_edit(
    gcx: Arc<GlobalContext>,
    code: &str,
    instruction: &str,
    cursor_file: &str,
    cursor_line: i32,
) -> Result<String, String> {
    refact_agentic::generate_code_edit::validate_request(code, instruction)?;

    let subagent_config = get_subagent_config(gcx.clone(), SUBAGENT_ID, None)
        .await
        .ok_or_else(|| format!("subagent config '{}' not found", SUBAGENT_ID))?;

    let messages = refact_agentic::generate_code_edit::build_messages(
        subagent_config.messages.system_prompt.as_deref(),
        code,
        instruction,
        cursor_file,
        cursor_line,
    )?;

    let result = run_subchat_once(gcx, SUBAGENT_ID, messages, TraceParent::unattributed())
        .await
        .map_err(|e| format!("Error generating code edit: {}", e))?;

    refact_agentic::generate_code_edit::extract_edited_code(&result.messages)
}
