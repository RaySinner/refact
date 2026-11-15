//! Code-edit generation through the `ChatCtx` seams: the subagent config comes
//! from `mode_config`, the model call from `subchat`. The prompt/message/parse
//! helpers stay in `refact_agentic::generate_code_edit`.

use refact_chat_api::TraceParent;

use refact_agentic::generate_code_edit::{
    build_messages, extract_edited_code, validate_request, SUBAGENT_ID,
};

use crate::chat_ctx::ChatCtx;

pub async fn generate_code_edit(
    ctx: ChatCtx,
    code: &str,
    instruction: &str,
    cursor_file: &str,
    cursor_line: i32,
) -> Result<String, String> {
    validate_request(code, instruction)?;

    let subagent_config = ctx
        .mode_config
        .get_subagent_config(SUBAGENT_ID, None)
        .await
        .ok_or_else(|| format!("subagent config '{}' not found", SUBAGENT_ID))?;

    let messages = build_messages(
        subagent_config.messages.system_prompt.as_deref(),
        code,
        instruction,
        cursor_file,
        cursor_line,
    )?;

    let result = ctx
        .subchat
        .run_subchat_once(SUBAGENT_ID, messages, TraceParent::unattributed())
        .await
        .map_err(|e| format!("Error generating code edit: {}", e))?;

    extract_edited_code(&result.messages)
}
