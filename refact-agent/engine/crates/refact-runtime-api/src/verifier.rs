use std::path::{Path, PathBuf};
use std::time::Duration;

use async_trait::async_trait;
use refact_chat_api::verifier::{ExpectedCardState, VerifyCardRequest};
use refact_chat_api::verify_cmd::VerifyCommandPolicy;
use refact_tasks::types::{VerificationResult, VerifierReport};

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

    /// Seam over the engine verification executor. The engine implementation forwards
    /// to `chat::verifier::run_verification_argv_impl`; the post-merge check lives in
    /// this crate and can only reach it through here.
    #[allow(clippy::too_many_arguments)]
    async fn run_verification_argv(
        &self,
        ctx: &ChatCtx,
        worktree: &Path,
        command: &str,
        cwd: Option<PathBuf>,
        env: Vec<(String, String)>,
        argv: Vec<String>,
        timeout: Duration,
        drain_timeout: Duration,
    ) -> VerificationResult;

    /// Deny rules of the active shell policy, loaded by the engine (the policy file is
    /// engine-owned state). Used to screen post-merge verification commands.
    async fn verification_command_policy(&self, ctx: &ChatCtx) -> VerifyCommandPolicy;
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
