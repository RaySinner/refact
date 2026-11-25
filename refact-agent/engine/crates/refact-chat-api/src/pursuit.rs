use std::time::{Duration, Instant};

use serde_json::json;

use crate::goal_verifier::QUIESCENCE_NUDGES;
use crate::types::{ChatSession, GoalAutonomy, GoalResolution, GoalStatus, SessionState};
use crate::{ChatCommand, ChatMessage, GoalSnapshotBudgetExt};

const DEFAULT_STALL_GRACE: Duration = Duration::from_secs(30);
const DEFAULT_NO_TOKEN_GRACE: Duration = Duration::from_secs(30);
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PursuitSource {
    GoalMonitor,
    InlineTurnEnd,
    VerifierRearm,
    SilentTail,
    Buddy,
}

impl PursuitSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::GoalMonitor => "goal_monitor",
            Self::InlineTurnEnd => "inline_turn_end",
            Self::VerifierRearm => "verifier_rearm",
            Self::SilentTail => "silent_tail",
            Self::Buddy => "buddy",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PursuitRequest {
    pub source: PursuitSource,
    pub reason: String,
    pub evidence_seq: u64,
    pub wake_requested: bool,
}

impl PursuitRequest {
    pub fn new(
        source: PursuitSource,
        reason: impl Into<String>,
        evidence_seq: u64,
        wake_requested: bool,
    ) -> Self {
        Self {
            source,
            reason: reason.into(),
            evidence_seq,
            wake_requested,
        }
    }
}

pub fn pursuit_evidence_seq(session: &ChatSession) -> u64 {
    session.messages.len() as u64
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PursuitSuppressReason {
    Closed,
    NoGoal,
    InactiveGoal,
    NonActiveGoalStatus,
    NonActiveResolution,
    NonActiveExecution,
    Transferred,
    AutonomyOff,
    Snoozed,
    Aborted,
    BudgetExhausted(GoalStatus),
    Compression,
    Cooldown,
    WaitingForInput,
    LiveWork,
    UserMessageQueued,
    PendingCommand,
    NotStalled,
    Quiescent,
    PursuitPending,
    DuplicateEvidence,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PursuitDecision {
    StartTurn,
    VisibleOnly,
    Suppress(PursuitSuppressReason),
}

#[derive(Debug, Clone, Copy)]
pub struct PursuitArbitrationContext {
    pub now_ms: u64,
    pub now_instant: Instant,
    pub stall_grace: Duration,
    pub no_token_grace: Duration,
}

impl PursuitArbitrationContext {
    pub fn new(now_ms: u64) -> Self {
        Self {
            now_ms,
            now_instant: Instant::now(),
            stall_grace: DEFAULT_STALL_GRACE,
            no_token_grace: DEFAULT_NO_TOKEN_GRACE,
        }
    }
}

pub fn arbitrate_pursuit(
    session: &ChatSession,
    request: &PursuitRequest,
    context: PursuitArbitrationContext,
) -> PursuitDecision {
    if session.closed {
        return suppress(PursuitSuppressReason::Closed);
    }
    let Some(goal) = session.goal.as_ref() else {
        if request.source == PursuitSource::SilentTail {
            return arbitrate_without_goal(session, request);
        }
        return suppress(PursuitSuppressReason::NoGoal);
    };
    if !goal.active {
        return suppress(PursuitSuppressReason::InactiveGoal);
    }
    if goal.status != GoalStatus::Active {
        return suppress(PursuitSuppressReason::NonActiveGoalStatus);
    }
    if goal.resolution != GoalResolution::Active {
        return suppress(PursuitSuppressReason::NonActiveResolution);
    }
    if goal.transferred_to.is_some() {
        return suppress(PursuitSuppressReason::Transferred);
    }
    if !goal.execution.allows_scheduling() {
        return suppress(PursuitSuppressReason::NonActiveExecution);
    }
    if goal.autonomy == GoalAutonomy::Off {
        if request.source == PursuitSource::Buddy {
            return PursuitDecision::VisibleOnly;
        }
        return suppress(PursuitSuppressReason::AutonomyOff);
    }
    if !request.wake_requested {
        return PursuitDecision::VisibleOnly;
    }
    if goal
        .snoozed_until_ms
        .is_some_and(|until_ms| context.now_ms < until_ms)
    {
        return suppress(PursuitSuppressReason::Snoozed);
    }
    if session.abort_flag.load(std::sync::atomic::Ordering::SeqCst)
        || session
            .user_interrupt_flag
            .load(std::sync::atomic::Ordering::SeqCst)
    {
        return suppress(PursuitSuppressReason::Aborted);
    }
    if let Some(status) = goal.goal_budget_exhaustion_status_at(context.now_ms) {
        return suppress(PursuitSuppressReason::BudgetExhausted(status));
    }
    if crate::compression_gate::compression_attempt_active(session) {
        return suppress(PursuitSuppressReason::Compression);
    }
    if session
        .goal_verification_blocked_until_ms
        .is_some_and(|until_ms| context.now_ms < until_ms)
        || !goal.goal_nudge_ready_at_with_backoff(context.now_ms)
    {
        return suppress(PursuitSuppressReason::Cooldown);
    }
    if waiting_for_user_or_approval(session) {
        return suppress(PursuitSuppressReason::WaitingForInput);
    }
    if has_live_work(session, request.source, context) {
        return suppress(PursuitSuppressReason::LiveWork);
    }
    if queued_user_message(session) {
        return suppress(PursuitSuppressReason::UserMessageQueued);
    }
    if !session.command_queue.is_empty()
        || !session.delivery_wake_sources.is_empty()
        || session
            .pending_deliveries
            .iter()
            .any(|delivery| delivery.wake)
    {
        return suppress(PursuitSuppressReason::PendingCommand);
    }
    if !source_is_stalled(session, request.source, context) {
        return suppress(PursuitSuppressReason::NotStalled);
    }
    if goal_is_quiescent(session) {
        return suppress(PursuitSuppressReason::Quiescent);
    }
    if session.goal_nudge_pending_for_current_goal() {
        return suppress(PursuitSuppressReason::PursuitPending);
    }
    if duplicate_evidence(session, request, goal.revision) {
        return suppress(PursuitSuppressReason::DuplicateEvidence);
    }
    PursuitDecision::StartTurn
}

fn arbitrate_without_goal(session: &ChatSession, request: &PursuitRequest) -> PursuitDecision {
    if !request.wake_requested {
        return PursuitDecision::VisibleOnly;
    }
    if session.abort_flag.load(std::sync::atomic::Ordering::SeqCst)
        || session
            .user_interrupt_flag
            .load(std::sync::atomic::Ordering::SeqCst)
    {
        return suppress(PursuitSuppressReason::Aborted);
    }
    if waiting_for_user_or_approval(session) {
        return suppress(PursuitSuppressReason::WaitingForInput);
    }
    if queued_user_message(session) {
        return suppress(PursuitSuppressReason::UserMessageQueued);
    }
    if !session.command_queue.is_empty() || !session.delivery_wake_sources.is_empty() {
        return suppress(PursuitSuppressReason::PendingCommand);
    }
    if duplicate_evidence(session, request, 0) {
        return suppress(PursuitSuppressReason::DuplicateEvidence);
    }
    PursuitDecision::StartTurn
}

fn suppress(reason: PursuitSuppressReason) -> PursuitDecision {
    PursuitDecision::Suppress(reason)
}

fn waiting_for_user_or_approval(session: &ChatSession) -> bool {
    matches!(
        session.runtime.state,
        SessionState::Paused | SessionState::WaitingUserInput | SessionState::WaitingIde
    ) || !session.runtime.pause_reasons.is_empty()
        || session.pending_browser_message.is_some()
}

fn queued_user_message(session: &ChatSession) -> bool {
    session
        .command_queue
        .iter()
        .any(|request| matches!(request.command, ChatCommand::UserMessage { .. }))
}

fn has_live_work(
    session: &ChatSession,
    source: PursuitSource,
    context: PursuitArbitrationContext,
) -> bool {
    match source {
        PursuitSource::InlineTurnEnd => session.runtime.state != SessionState::Idle,
        PursuitSource::GoalMonitor => match session.runtime.state {
            SessionState::Generating => {
                session.has_pending_tool_result_window()
                    || session.interruptible_waits > 0
                    || session.runtime.waiting_interruptible
                    || generating_quiet_for(session, context).is_none()
            }
            SessionState::ExecutingTools => true,
            _ => false,
        },
        _ => false,
    }
}

fn source_is_stalled(
    session: &ChatSession,
    source: PursuitSource,
    context: PursuitArbitrationContext,
) -> bool {
    match source {
        PursuitSource::GoalMonitor => match session.runtime.state {
            SessionState::Idle | SessionState::Error | SessionState::Completed => {
                session.last_activity + context.stall_grace <= context.now_instant
            }
            SessionState::Generating => generating_quiet_for(session, context).is_some(),
            _ => false,
        },
        _ => true,
    }
}

fn generating_quiet_for(
    session: &ChatSession,
    context: PursuitArbitrationContext,
) -> Option<Duration> {
    let last_progress = [
        session.last_stream_delta_at,
        session.last_tool_started_at,
        session.last_tool_progress_at,
        Some(session.last_activity),
    ]
    .into_iter()
    .flatten()
    .max()?;
    if last_progress + context.no_token_grace <= context.now_instant {
        Some(context.now_instant.duration_since(last_progress))
    } else {
        None
    }
}

fn goal_is_quiescent(session: &ChatSession) -> bool {
    session.goal.as_ref().is_some_and(|goal| {
        goal.budget.no_progress_turns.is_none_or(|limit| limit == 0)
            && goal.progress.no_progress_turns >= QUIESCENCE_NUDGES
    })
}

fn duplicate_evidence(session: &ChatSession, request: &PursuitRequest, revision: u64) -> bool {
    session
        .messages
        .iter()
        .chain(
            session
                .pending_deliveries
                .iter()
                .flat_map(|delivery| delivery.messages.iter()),
        )
        .any(|message| {
            let Some(pursuit) = message.extra.get("pursuit") else {
                return false;
            };
            pursuit.get("evidence_seq").and_then(|value| value.as_u64())
                == Some(request.evidence_seq)
                && pursuit
                    .get("goal_revision")
                    .and_then(|value| value.as_u64())
                    == Some(revision)
                && pursuit
                    .get("wake_requested")
                    .and_then(|value| value.as_bool())
                    .unwrap_or(false)
        })
}

pub fn stamp_pursuit_message(
    message: &mut ChatMessage,
    request: &PursuitRequest,
    goal_revision: u64,
) {
    message.extra.insert(
        "pursuit".to_string(),
        json!({
            "source": request.source.as_str(),
            "reason": request.reason,
            "evidence_seq": request.evidence_seq,
            "goal_revision": goal_revision,
            "wake_requested": request.wake_requested,
        }),
    );
}
