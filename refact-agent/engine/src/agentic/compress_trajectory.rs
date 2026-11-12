pub use refact_agentic::compress_trajectory::{CompressedTrajectory, source_records};

use crate::call_validation::ChatMessage;
use crate::global_context::GlobalContext;
use std::sync::Arc;

pub async fn compress_trajectory(
    gcx: Arc<GlobalContext>,
    messages: &Vec<ChatMessage>,
    parent_chat_id: Option<&str>,
) -> Result<CompressedTrajectory, String> {
    let input = refact_agentic::compress_trajectory::reconstruction_input(messages)?;
    let outcome = crate::agentic::mode_transition::reconstruct_context(
        gcx,
        crate::agentic::mode_transition::ReconstructionRequest {
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
