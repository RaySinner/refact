use std::sync::Arc;

use crate::call_validation::ChatMessage;
use crate::global_context::GlobalContext;
use refact_runtime_api::GlobalContextCtx;

pub use refact_agentic::generate_follow_up_message::{
    make_conversation, FollowUpResponse, SUBAGENT_ID,
};

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
            refact_runtime_api::generate_follow_up_runtime::generate_follow_up_message_once(
                gcx2.chat_ctx(gcx2.clone()),
                messages,
                &chat_id,
            )
            .await
        },
    )
    .await
}
