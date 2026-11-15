pub use refact_agentic::compress_trajectory::{CompressedTrajectory, source_records};

use refact_agentic::mode_transition::ReconstructionRequest;
use refact_chat_api::ChatMessage;

use crate::global_context::GlobalContext;

pub async fn compress_trajectory(
    gcx: std::sync::Arc<GlobalContext>,
    messages: &Vec<ChatMessage>,
    parent_chat_id: Option<&str>,
) -> Result<CompressedTrajectory, String> {
    let input = refact_agentic::compress_trajectory::reconstruction_input(messages)?;
    let outcome = crate::mode_transition_runtime::reconstruct_context(
        gcx,
        ReconstructionRequest {
            messages: &input,
            target_mode: "agent",
            target_mode_description: "Continue the current conversation",
            parent_chat_id,
            model_override: None,
            abort_flag: None,
            hints: None,
            target_budget_symbols: None,
            preserve_goal_messages: true,
        },
    )
    .await?;
    refact_agentic::compress_trajectory::compress(&input, &outcome.messages)
}
