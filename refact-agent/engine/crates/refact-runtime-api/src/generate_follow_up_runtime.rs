//! Follow-up suggestion generation through the `ChatCtx` seams: the subagent
//! config comes from `mode_config`, the model call from `subchat`. The buddy
//! workflow wrapper stays in the engine (it needs `AppState`); everything
//! inside the closure lives here. Prompt/parse helpers stay in
//! `refact_agentic::generate_follow_up_message`.

use refact_agentic::generate_follow_up_message::{
    last_response_text, make_conversation, parse_follow_up_response, system_prompt, FollowUpResponse,
    SUBAGENT_ID,
};
use refact_chat_api::{ChatMessage, TraceParent};

use crate::chat_ctx::ChatCtx;

pub async fn generate_follow_up_message_once(
    ctx: ChatCtx,
    messages: Vec<ChatMessage>,
    chat_id: &str,
) -> Result<FollowUpResponse, String> {
    let subagent_config = ctx
        .mode_config
        .get_subagent_config(SUBAGENT_ID, None)
        .await
        .ok_or_else(|| format!("subagent config '{}' not found", SUBAGENT_ID))?;

    let system_prompt = system_prompt(subagent_config.messages.system_prompt.as_deref())?;

    let result = ctx
        .subchat
        .run_subchat_once(
            SUBAGENT_ID,
            make_conversation(&messages, &system_prompt),
            TraceParent::chat(chat_id),
        )
        .await?;

    let response = last_response_text(&result.messages)?;

    tracing::info!("follow-up model says {:?}", response);

    parse_follow_up_response(&response)
}
