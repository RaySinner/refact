use std::sync::atomic::Ordering;
use std::sync::Arc;

use serde_json::json;
use tokio::sync::Mutex as AMutex;
use uuid::Uuid;

use refact_core::chat_types::{
    ChatContent, ChatMessage, DeliveryOutcome, PendingDelivery, PushMode, ReasoningEffort,
    SamplingParameters,
};
use refact_core::string_utils::safe_truncate;

use crate::ThreadParams;

use crate::internal_roles::{self, EventSubkind};
use crate::types::epoch_ms_now;
use crate::types::{
    ChatSession, CriterionVerdict, GoalAttempt, GoalCriterion, GoalEvent, GoalStatus, SessionState,
};
use crate::types::GoalSnapshotBudgetExt;

pub const GOAL_VERIFIER_SOURCE: &str = "chat.goal_verifier";
pub const INCONCLUSIVE_GAP: &str = "verification inconclusive; continue";
pub const GOAL_VERIFIER_BLOCKED_BACKOFF_MS: u64 = 300_000;
pub const GOAL_VERIFIER_MAX_TOKENS: usize = 1024;

/// Consecutive no-progress verifier verdicts that mark the goal quiescent. The
/// engine re-exports this so `goal_monitor` and the verifier cannot drift apart.
pub const QUIESCENCE_NUDGES: u32 = 3;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GoalVerdict {
    Met,
    Unmet(Vec<String>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoalVerifierReply {
    pub verdict: GoalVerdict,
    pub verifier_reply: String,
    pub tokens: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GoalVerificationBegin {
    Started,
    NoActiveGoal,
    BudgetExhausted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GoalVerificationApplyOutcome {
    Finalized,
    Rearmed,
    Continued,
    Stalled,
    Superseded,
    NoGoal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GoalCompletionGateOutcome {
    Passthrough,
    Finalized,
    Rearmed,
    BudgetExhausted(GoalStatus),
    VerificationUnavailable,
    VerificationStalled,
    Aborted,
}

pub fn should_verify_goal_on_done(session: &ChatSession) -> bool {
    session.goal.as_ref().is_some_and(|goal| {
        goal.active
            && matches!(
                goal.status,
                GoalStatus::Active | GoalStatus::BudgetExhausted | GoalStatus::NoProgress
            )
    })
}

pub fn begin_goal_verification_if_needed(session: &mut ChatSession) -> GoalVerificationBegin {
    let Some(goal) = session.goal.as_ref() else {
        return GoalVerificationBegin::NoActiveGoal;
    };
    if !goal.active {
        return GoalVerificationBegin::NoActiveGoal;
    }
    if matches!(
        goal.status,
        GoalStatus::BudgetExhausted | GoalStatus::NoProgress
    ) {
        return GoalVerificationBegin::BudgetExhausted;
    }
    if goal.status != GoalStatus::Active {
        return GoalVerificationBegin::NoActiveGoal;
    }
    if goal.goal_budget_exhausted() {
        return GoalVerificationBegin::BudgetExhausted;
    }
    session.goal_set_status(GoalStatus::Verifying);
    GoalVerificationBegin::Started
}

pub fn goal_verification_message(goal_text: &str, criteria: &[GoalCriterion]) -> ChatMessage {
    let criteria_block = if criteria.is_empty() {
        String::new()
    } else {
        let list = criteria
            .iter()
            .map(|criterion| {
                let hint = criterion
                    .verify_hint
                    .as_deref()
                    .map(|hint| format!(" (verify: {hint})"))
                    .unwrap_or_default();
                format!("- [{}] {}{}", criterion.id, criterion.text, hint)
            })
            .collect::<Vec<_>>()
            .join("\n");
        format!(
            "\n\nStructured criteria:\n{list}\n\nFor EACH criterion also output a line `CRITERION <id>: MET` or `CRITERION <id>: UNMET — <short note>`."
        )
    };
    ChatMessage {
        message_id: Uuid::new_v4().to_string(),
        role: "user".to_string(),
        content: ChatContent::SimpleText(format!(
            "[goal verification]\n\nSynthesized goal:\n{goal_text}\n\nAcceptance criteria:\nUse every explicit criterion in the synthesized goal above.{criteria_block}\n\nJudge ONLY whether the work so far FULLY satisfies the goal and every criterion. Respond EXACTLY: first line `GOAL: MET` or `GOAL: UNMET`; if UNMET, a bullet list of unmet requirements; call NO tools."
        )),
        extra: serde_json::Map::from_iter([(
            "goal_verification".to_string(),
            json!({"hidden": true}),
        )]),
        ..Default::default()
    }
}

pub fn parse_goal_verdict(answer: &str, tool_called: bool) -> GoalVerdict {
    if tool_called {
        return inconclusive_verdict();
    }
    let mut lines = answer
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty());
    let Some(first) = lines.next() else {
        return inconclusive_verdict();
    };
    if first.eq_ignore_ascii_case("GOAL: MET") {
        return GoalVerdict::Met;
    }
    if first.eq_ignore_ascii_case("GOAL: UNMET") {
        let criteria_verdicts = parse_criteria_verdicts(answer);
        let gaps = lines
            .filter(|line| !line.starts_with("CRITERION "))
            .map(|line| line.trim_start_matches(['-', '*', '•', ' ']).trim())
            .filter(|line| !line.is_empty())
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        if !gaps.is_empty() {
            GoalVerdict::Unmet(gaps)
        } else {
            let criterion_gaps = criteria_verdicts
                .into_iter()
                .filter(|criterion| !criterion.met)
                .map(|criterion| {
                    if criterion.note.is_empty() {
                        format!("{}: unmet", criterion.id)
                    } else {
                        format!("{}: {}", criterion.id, criterion.note)
                    }
                })
                .collect::<Vec<_>>();
            if criterion_gaps.is_empty() {
                inconclusive_verdict()
            } else {
                GoalVerdict::Unmet(criterion_gaps)
            }
        }
    } else {
        inconclusive_verdict()
    }
}

fn inconclusive_verdict() -> GoalVerdict {
    GoalVerdict::Unmet(vec![INCONCLUSIVE_GAP.to_string()])
}

pub fn parse_criteria_verdicts(answer: &str) -> Vec<CriterionVerdict> {
    answer
        .lines()
        .filter_map(|line| {
            let rest = line.trim().strip_prefix("CRITERION ")?;
            let (id, verdict) = rest.split_once(':')?;
            let verdict = verdict.trim();
            let (met, note) = if let Some(note) = verdict.strip_prefix("UNMET") {
                (false, note)
            } else if let Some(note) = verdict.strip_prefix("MET") {
                (true, note)
            } else {
                return None;
            };
            Some(CriterionVerdict {
                id: id.trim().trim_matches(['[', ']']).to_string(),
                met,
                note: note.trim_start_matches(['—', '-', ':', ' ']).to_string(),
            })
        })
        .collect()
}

pub fn goal_text_for_verifier(session: &ChatSession) -> Result<String, String> {
    crate::session_goal_role::try_synthesize_current_goal(session)?
        .or_else(|| session.goal.as_ref().map(|goal| goal.content.clone()))
        .ok_or_else(|| "no active goal to verify".to_string())
}

pub fn verifier_sampling_parameters(thread: &ThreadParams) -> SamplingParameters {
    SamplingParameters {
        max_new_tokens: GOAL_VERIFIER_MAX_TOKENS,
        temperature: Some(0.0),
        boost_reasoning: false,
        reasoning_effort: thread
            .reasoning_effort
            .as_ref()
            .and_then(|s| match s.as_str() {
                "low" => Some(ReasoningEffort::Low),
                "medium" => Some(ReasoningEffort::Medium),
                "high" => Some(ReasoningEffort::High),
                "xhigh" => Some(ReasoningEffort::XHigh),
                "max" => Some(ReasoningEffort::Max),
                _ => None,
            }),
        thinking_budget: thread.thinking_budget,
        ..Default::default()
    }
}

pub async fn verifier_cancel_requested(session_arc: &Arc<AMutex<ChatSession>>) -> bool {
    let session = session_arc.lock().await;
    session.user_interrupt_flag.load(Ordering::SeqCst)
}

/// Applies a verifier verdict only when the parent chat history is still the
/// one the verifier forked from. The goal-status epoch alone cannot see queue
/// commands, edits, or compaction rewrites that landed mid-verification; a
/// `trajectory_version` mismatch means the verdict may describe messages that
/// no longer exist, so the goal is re-armed for a fresh verification instead.
pub fn apply_goal_verdict_guarded(
    session: &mut ChatSession,
    trigger: &str,
    reply: GoalVerifierReply,
    epoch: Option<u64>,
    fork_trajectory_version: u64,
) -> GoalVerificationApplyOutcome {
    apply_goal_verdict_guarded_with_delivery(
        session,
        trigger,
        reply,
        epoch,
        fork_trajectory_version,
        PushMode::Append,
        None,
    )
}

pub fn apply_goal_verdict_guarded_with_delivery(
    session: &mut ChatSession,
    trigger: &str,
    reply: GoalVerifierReply,
    epoch: Option<u64>,
    fork_trajectory_version: u64,
    push: PushMode,
    tool_call_id: Option<&str>,
) -> GoalVerificationApplyOutcome {
    if session.goal.is_some() && session.trajectory_version != fork_trajectory_version {
        if session.goal_status == Some(GoalStatus::Verifying) {
            session.goal_set_status(GoalStatus::Active);
        }
        return GoalVerificationApplyOutcome::Superseded;
    }
    apply_goal_verdict_with_delivery(session, trigger, reply, epoch, push, tool_call_id)
}

pub fn apply_goal_verdict(
    session: &mut ChatSession,
    trigger: &str,
    reply: GoalVerifierReply,
    epoch: Option<u64>,
) -> GoalVerificationApplyOutcome {
    apply_goal_verdict_with_delivery(session, trigger, reply, epoch, PushMode::Append, None)
}

fn apply_goal_verdict_with_delivery(
    session: &mut ChatSession,
    trigger: &str,
    reply: GoalVerifierReply,
    epoch: Option<u64>,
    push: PushMode,
    tool_call_id: Option<&str>,
) -> GoalVerificationApplyOutcome {
    if session.goal.is_none() {
        return GoalVerificationApplyOutcome::NoGoal;
    }
    if let Some(epoch) = epoch {
        if session.goal_status_changed_since(epoch) {
            return GoalVerificationApplyOutcome::Superseded;
        }
    }
    let held_status = match session.goal_status {
        Some(status @ (GoalStatus::Paused | GoalStatus::Stopped)) => Some(status),
        _ => None,
    };

    let at_ms = epoch_ms_now();
    session.goal_record_verifier_attempt(reply.tokens);
    let rearm_candidate = held_status.is_none() && trigger != "validate_goal";
    let (verdict_text, gaps, event_kind, event_text) = match &reply.verdict {
        GoalVerdict::Met => (
            "met".to_string(),
            Vec::new(),
            "verified".to_string(),
            "Goal verification passed.".to_string(),
        ),
        GoalVerdict::Unmet(gaps) => (
            "unmet".to_string(),
            gaps.clone(),
            "verification_gaps".to_string(),
            format!("Goal verification found gaps:\n{}", gaps.join("\n")),
        ),
    };
    let criteria_verdicts = parse_criteria_verdicts(&reply.verifier_reply);
    session.goal_push_attempt(GoalAttempt {
        at_ms,
        trigger: trigger.to_string(),
        verdict: verdict_text,
        gaps: gaps.clone(),
        verifier_reply: reply.verifier_reply,
        criteria_verdicts,
    });
    let mut payload = json!({"kind": event_kind, "at_ms": at_ms, "gaps": gaps});
    if event_kind == "verification_gaps" && rearm_candidate {
        payload["account_progress"] = json!(true);
    }
    let event = internal_roles::event(
        EventSubkind::GoalPursuit,
        GOAL_VERIFIER_SOURCE,
        payload,
        event_text,
    );
    let outcome = match reply.verdict {
        GoalVerdict::Met => {
            session.goal_set_status(GoalStatus::Completed);
            if trigger != "validate_goal" {
                session.set_runtime_state(SessionState::Completed, None);
            }
            GoalVerificationApplyOutcome::Finalized
        }
        GoalVerdict::Unmet(_) => {
            if let Some(status) = held_status {
                session.goal_set_status(status);
                session.set_runtime_state(SessionState::Idle, None);
                GoalVerificationApplyOutcome::Continued
            } else {
                session.goal_set_status(GoalStatus::Active);
                session.goal_note_no_progress_turn();
                if trigger == "validate_goal" {
                    GoalVerificationApplyOutcome::Continued
                } else if session.goal_status != Some(GoalStatus::Active)
                    || session.goal.as_ref().is_some_and(|goal| {
                        goal.progress.no_progress_turns
                            >= QUIESCENCE_NUDGES
                    })
                {
                    GoalVerificationApplyOutcome::Stalled
                } else {
                    session.set_runtime_state(SessionState::Idle, None);
                    GoalVerificationApplyOutcome::Rearmed
                }
            }
        }
    };
    deliver_verifier_event(
        session,
        event,
        push,
        tool_call_id,
        outcome == GoalVerificationApplyOutcome::Rearmed,
    );
    outcome
}

pub fn handle_verifier_failure(
    session: &mut ChatSession,
    trigger: &str,
    error: &str,
) -> GoalCompletionGateOutcome {
    handle_verifier_failure_with_delivery(session, trigger, error, PushMode::Append, None)
}

pub fn handle_verifier_failure_with_delivery(
    session: &mut ChatSession,
    trigger: &str,
    error: &str,
    push: PushMode,
    tool_call_id: Option<&str>,
) -> GoalCompletionGateOutcome {
    let at_ms = epoch_ms_now();
    if session.goal_status == Some(GoalStatus::Verifying) {
        session.goal_set_status(GoalStatus::Active);
    }
    session.goal_verification_blocked_until_ms =
        Some(at_ms.saturating_add(GOAL_VERIFIER_BLOCKED_BACKOFF_MS));
    let reason = safe_truncate(error, 200);
    let event = internal_roles::event(
        EventSubkind::GoalPursuit,
        GOAL_VERIFIER_SOURCE,
        json!({"kind": "verification_blocked", "trigger": trigger, "at_ms": at_ms}),
        format!("Goal verification unavailable: {reason}"),
    );
    deliver_verifier_event(session, event, push, tool_call_id, false);
    GoalCompletionGateOutcome::VerificationUnavailable
}

pub fn apply_goal_budget_exhausted_terminal(
    session: &mut ChatSession,
    trigger: &str,
) -> Option<GoalStatus> {
    let at_ms = epoch_ms_now();
    let goal = session.goal.as_ref()?;
    let status = if matches!(
        goal.status,
        GoalStatus::BudgetExhausted | GoalStatus::NoProgress
    ) {
        goal.status
    } else {
        goal.goal_budget_exhaustion_status_at(at_ms)
            .unwrap_or(GoalStatus::BudgetExhausted)
    };
    let kind = terminal_goal_kind(status);
    let already_recorded = session.goal.as_ref().is_some_and(|goal| {
        goal.events
            .iter()
            .any(|event| event.kind == "goal_pursuit" && event.text.contains(kind))
    });
    session.goal_set_status(status);
    if !already_recorded {
        let message = goal_budget_exhausted_terminal_event(status, trigger, at_ms);
        session.goal_push_event(GoalEvent {
            at_ms,
            kind: "goal_pursuit".to_string(),
            text: message.content.content_text_only(),
        });
        deliver_verifier_event(session, message, PushMode::Append, None, false);
    }
    session.set_runtime_state(SessionState::Completed, None);
    Some(status)
}

fn terminal_goal_kind(status: GoalStatus) -> &'static str {
    match status {
        GoalStatus::NoProgress => "no_progress",
        _ => "budget_exhausted",
    }
}

fn goal_budget_exhausted_terminal_event(
    status: GoalStatus,
    trigger: &str,
    at_ms: u64,
) -> ChatMessage {
    let kind = terminal_goal_kind(status);
    internal_roles::event(
        EventSubkind::GoalPursuit,
        GOAL_VERIFIER_SOURCE,
        json!({
            "kind": kind,
            "trigger": trigger,
            "at_ms": at_ms,
        }),
        format!("Goal pursuit stopped before completion: {kind}."),
    )
}

pub async fn abort_goal_verification(session_arc: Arc<AMutex<ChatSession>>) {
    let mut session = session_arc.lock().await;
    if session.goal_status == Some(GoalStatus::Verifying) {
        session.goal_set_status(GoalStatus::Active);
    }
    session.set_runtime_state(SessionState::Idle, None);
}

fn deliver_verifier_event(
    session: &mut ChatSession,
    event: ChatMessage,
    push: PushMode,
    tool_call_id: Option<&str>,
    wake: bool,
) {
    let mut delivery = PendingDelivery::new(vec![event], push, GOAL_VERIFIER_SOURCE, wake);
    delivery.after_tool_call_id = tool_call_id.map(str::to_string);
    match session.enqueue_delivery(delivery) {
        Ok(DeliveryOutcome::Delivered) if wake => {
            session.queue_notify.notify_one();
        }
        Err(error) => tracing::warn!(%error, "Goal verifier delivery rejected"),
        _ => {}
    }
}
