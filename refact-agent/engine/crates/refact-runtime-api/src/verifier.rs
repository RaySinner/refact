use async_trait::async_trait;
use refact_chat_api::verifier::{ExpectedCardState, VerifyCardRequest};
use refact_tasks::types::VerifierReport;

use crate::chat_ctx::ChatCtx;

#[async_trait]
pub trait CardVerifier: Send + Sync {
    async fn load_expected_card_state(
        &self,
        ctx: &ChatCtx,
        task_id: &str,
        card_id: &str,
    ) -> Result<ExpectedCardState, String>;

    async fn verify_card(
        &self,
        ctx: &ChatCtx,
        request: VerifyCardRequest,
    ) -> Result<VerifierReport, String>;

    async fn schedule_card_verifier_after_finish(
        &self,
        ctx: &ChatCtx,
        task_id: String,
        card_id: String,
        expected_state: ExpectedCardState,
    );
}

pub async fn load_expected_card_state_for_ctx(
    ctx: &ChatCtx,
    task_id: &str,
    card_id: &str,
) -> Result<ExpectedCardState, String> {
    ctx.card_verifier
        .load_expected_card_state(ctx, task_id, card_id)
        .await
}

pub async fn verify_card_for_ctx(
    ctx: &ChatCtx,
    request: VerifyCardRequest,
) -> Result<VerifierReport, String> {
    ctx.card_verifier.verify_card(ctx, request).await
}

pub async fn schedule_card_verifier_after_finish_for_ctx(
    ctx: &ChatCtx,
    task_id: String,
    card_id: String,
    expected_state: ExpectedCardState,
) {
    ctx.card_verifier
        .schedule_card_verifier_after_finish(ctx, task_id, card_id, expected_state)
        .await;
}
