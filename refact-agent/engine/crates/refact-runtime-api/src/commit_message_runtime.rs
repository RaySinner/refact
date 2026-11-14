//! Commit-message generation orchestration: the subchat call plus the buddy
//! workflow wrapper. The pure diff/prompt/message helpers stay in
//! `refact_agentic::generate_commit_message`; everything here only needs the
//! `ChatCtx` seams (subchat launcher, mode config, buddy workflow sink).

use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use refact_agentic::generate_commit_message::{build_messages, extract_commit_message, SUBAGENT_ID};
use refact_chat_api::TraceParent;

use crate::chat_ctx::ChatCtx;
use crate::WorkflowFuture;

pub async fn generate_commit_message_by_diff(
    ctx: ChatCtx,
    diff: &String,
    commit_message_prompt: &Option<String>,
    parent_chat_id: Option<&str>,
) -> Result<String, String> {
    generate_commit_message_by_diff_with_abort(
        ctx,
        diff,
        commit_message_prompt,
        None,
        parent_chat_id,
    )
    .await
}

pub async fn generate_commit_message_by_diff_with_abort(
    ctx: ChatCtx,
    diff: &String,
    commit_message_prompt: &Option<String>,
    abort_flag: Option<Arc<AtomicBool>>,
    parent_chat_id: Option<&str>,
) -> Result<String, String> {
    refact_agentic::generate_commit_message::validate_diff(diff)?;
    let diff = diff.clone();
    let commit_message_prompt = commit_message_prompt.clone();
    let ctx2 = ctx.clone();
    let parent_chat_id = parent_chat_id.map(|id| id.to_string());
    let work: Box<dyn FnOnce() -> WorkflowFuture + Send + 'static> = Box::new(move || {
        Box::pin(async move {
            let subagent_config = ctx2
                .mode_config
                .get_subagent_config(SUBAGENT_ID, None)
                .await
                .ok_or_else(|| format!("subagent config '{}' not found", SUBAGENT_ID))?;

            let messages = build_messages(
                &diff,
                commit_message_prompt.as_deref(),
                subagent_config.prompts.diff_only.as_deref(),
                subagent_config.prompts.diff_with_user_text.as_deref(),
            )?;

            let result = ctx2
                .subchat
                .run_subchat_once_with_abort(
                    SUBAGENT_ID,
                    messages,
                    abort_flag,
                    TraceParent::from_parts(parent_chat_id.as_deref(), None),
                )
                .await
                .map_err(|e| format!("Error: {}", e))?;

            extract_commit_message(&result.messages)
        })
    });
    ctx.buddy_event_sink
        .run_workflow("commit_message", "📦", 5, "Commit message", 50, work)
        .await
}
