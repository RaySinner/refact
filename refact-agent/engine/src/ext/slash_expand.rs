use crate::app_state::AppState;
use crate::integrations::mcp::mcp_prompts::{execute_mcp_prompt, MCP_PROMPT_PREFIX};

pub use refact_runtime_api::ext_slash_expand::{
    expand_slash_command_non_mcp, ExpandedCommand, SkillActivationInfo,
};

pub async fn expand_slash_command(
    app: AppState,
    raw_input: &str,
) -> Result<Option<ExpandedCommand>, String> {
    if let Some(expanded) = expand_slash_command_non_mcp(app.clone(), raw_input).await? {
        return Ok(Some(expanded));
    }
    expand_mcp_prompt_command(app, raw_input).await
}

async fn expand_mcp_prompt_command(
    app: AppState,
    raw_input: &str,
) -> Result<Option<ExpandedCommand>, String> {
    let char_bytes: Vec<(usize, char)> = raw_input.char_indices().collect();
    for (char_idx, &(byte_pos, ch)) in char_bytes.iter().enumerate() {
        if ch != '/' {
            continue;
        }
        if char_idx > 0 && !char_bytes[char_idx - 1].1.is_whitespace() {
            continue;
        }
        let name_byte_start = byte_pos + 1;
        let name_byte_end = char_bytes[char_idx + 1..]
            .iter()
            .find(|(_, c)| c.is_whitespace())
            .map(|(b, _)| *b)
            .unwrap_or(raw_input.len());
        let cmd_name = &raw_input[name_byte_start..name_byte_end];
        if !cmd_name.starts_with(MCP_PROMPT_PREFIX) {
            continue;
        }
        let args_str = raw_input[name_byte_end..].trim().to_string();
        let prefix = &raw_input[..byte_pos];
        match execute_mcp_prompt(app.gcx.clone(), cmd_name, &args_str, 30).await {
            Ok(expanded_body) => {
                return Ok(Some(ExpandedCommand {
                    expanded_text: format!("{}{}", prefix, expanded_body),
                    model_override: None,
                    allowed_tools: vec![],
                    source_command: cmd_name.to_string(),
                    context_fork: None,
                    skill_to_activate: None,
                }));
            }
            Err(e) => {
                tracing::warn!("MCP prompt expansion failed for {}: {}", cmd_name, e);
            }
        }
    }
    Ok(None)
}
