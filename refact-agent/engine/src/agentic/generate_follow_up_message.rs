pub use refact_agentic::generate_follow_up_message::{
    make_conversation, FollowUpResponse, SUBAGENT_ID,
};

use std::sync::Arc;

use crate::call_validation::ChatMessage;
use crate::global_context::GlobalContext;
use crate::subchat::{run_subchat_once, TraceParent};
use crate::yaml_configs::customization_registry::get_subagent_config;

pub async fn generate_follow_up_message(
    messages: Vec<ChatMessage>,
    gcx: Arc<GlobalContext>,
    _model_id: &str,
    chat_id: &str,
) -> Result<FollowUpResponse, String> {
    let gcx2 = gcx.clone();
    let chat_id = chat_id.to_string();
    crate::buddy::workflows::buddy_wrap_workflow(
        crate::app_state::app_state_from_gcx(gcx).await,
        "follow_up",
        "💡",
        3,
        |_: &FollowUpResponse| "Follow-up suggested".to_string(),
        move || async move {
            let subagent_config = get_subagent_config(gcx2.clone(), SUBAGENT_ID, None)
                .await
                .ok_or_else(|| format!("subagent config '{}' not found", SUBAGENT_ID))?;

            let system_prompt = refact_agentic::generate_follow_up_message::system_prompt(
                subagent_config.messages.system_prompt.as_deref(),
            )?;

            let result = run_subchat_once(
                gcx2,
                SUBAGENT_ID,
                make_conversation(&messages, &system_prompt),
                TraceParent::chat(&chat_id),
            )
            .await?;

            let response = refact_agentic::generate_follow_up_message::last_response_text(
                &result.messages,
            )?;

            tracing::info!("follow-up model says {:?}", response);

            refact_agentic::generate_follow_up_message::parse_follow_up_response(&response)
        },
    )
    .await
}
