//! Goal-verifier completion gate. The orchestration (block, begin, interrupt,
//! verdict apply) is session-state work over `refact_chat_api::goal_verifier`;
//! the LLM call itself goes through the `GoalVerifierRunner` seam installed by
//! the engine in `ChatCtx::goal_verifier`.

use std::sync::Arc;

use std::sync::atomic::Ordering;

use tokio::sync::Mutex as AMutex;

use refact_chat_api::goal_verifier::{
    abort_goal_verification, apply_goal_budget_exhausted_terminal, apply_goal_verdict_guarded,
    begin_goal_verification_if_needed, handle_verifier_failure, should_verify_goal_on_done,
    GoalCompletionGateOutcome, GoalVerificationApplyOutcome, GoalVerificationBegin,
};
use refact_chat_api::types::{epoch_ms_now, ChatSession};
use refact_chat_api::GoalStatus;

use crate::app_state::AppState;
use crate::goal_verifier_runtime::run_goal_verifier;

pub async fn verify_goal_before_completion(
    app: AppState,
    session_arc: Arc<AMutex<ChatSession>>,
    trigger: &str,
) -> GoalCompletionGateOutcome {
    let (begin, epoch, fork_trajectory_version) = {
        let mut session = session_arc.lock().await;
        let now_ms = epoch_ms_now();
        if session
            .goal_verification_blocked_until_ms
            .is_some_and(|until| now_ms < until)
            && should_verify_goal_on_done(&session)
        {
            return GoalCompletionGateOutcome::VerificationUnavailable;
        }
        let begin = begin_goal_verification_if_needed(&mut session);
        (
            begin,
            session.goal_ledger_last_seq(),
            session.trajectory_version,
        )
    };
    match begin {
        GoalVerificationBegin::Started => {}
        GoalVerificationBegin::BudgetExhausted => {
            let mut session = session_arc.lock().await;
            let status = apply_goal_budget_exhausted_terminal(&mut session, trigger)
                .unwrap_or(GoalStatus::BudgetExhausted);
            return GoalCompletionGateOutcome::BudgetExhausted(status);
        }
        GoalVerificationBegin::NoActiveGoal => return GoalCompletionGateOutcome::Passthrough,
    }
    {
        let session = session_arc.lock().await;
        if session.user_interrupt_flag.load(Ordering::SeqCst) {
            drop(session);
            abort_goal_verification(session_arc.clone()).await;
            return GoalCompletionGateOutcome::Aborted;
        }
    }

    let reply = match run_goal_verifier(app, session_arc.clone()).await {
        Ok(reply) => reply,
        Err(error) => {
            let aborted = {
                let session = session_arc.lock().await;
                session.user_interrupt_flag.load(Ordering::SeqCst) || error == "Aborted"
            };
            if aborted {
                abort_goal_verification(session_arc.clone()).await;
                return GoalCompletionGateOutcome::Aborted;
            }
            let mut session = session_arc.lock().await;
            return handle_verifier_failure(&mut session, trigger, &error);
        }
    };

    {
        let session = session_arc.lock().await;
        if session.user_interrupt_flag.load(Ordering::SeqCst) {
            drop(session);
            abort_goal_verification(session_arc.clone()).await;
            return GoalCompletionGateOutcome::Aborted;
        }
    }

    let mut session = session_arc.lock().await;
    match apply_goal_verdict_guarded(
        &mut session,
        trigger,
        reply,
        Some(epoch),
        fork_trajectory_version,
    ) {
        GoalVerificationApplyOutcome::Finalized => GoalCompletionGateOutcome::Finalized,
        GoalVerificationApplyOutcome::Rearmed => GoalCompletionGateOutcome::Rearmed,
        GoalVerificationApplyOutcome::Continued => GoalCompletionGateOutcome::Rearmed,
        GoalVerificationApplyOutcome::Stalled => GoalCompletionGateOutcome::VerificationStalled,
        GoalVerificationApplyOutcome::Superseded => GoalCompletionGateOutcome::Rearmed,
        GoalVerificationApplyOutcome::NoGoal => GoalCompletionGateOutcome::Passthrough,
    }
}
