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
    ChatSession, CriterionVerdict, CriterionVerdictStatus, GoalAttempt, GoalCriterion, GoalEvent,
    GoalStatus, SessionState,
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
    Impossible(Vec<String>),
    Inconclusive(Vec<String>),
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
    Failed,
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
            "\n\nStructured criteria:\n{list}\n\nFor EACH criterion also output exactly one line `CRITERION <id>: MET`, `CRITERION <id>: UNMET — <short note>`, `CRITERION <id>: IMPOSSIBLE — <evidence>`, or `CRITERION <id>: INCONCLUSIVE — <short note>`."
        )
    };
    ChatMessage {
        message_id: Uuid::new_v4().to_string(),
        role: "user".to_string(),
        content: ChatContent::SimpleText(format!(
            "[goal verification]\n\nSynthesized goal:\n{goal_text}\n\nAcceptance criteria:\nUse every explicit criterion in the synthesized goal above.{criteria_block}\n\nJudge ONLY whether the work so far FULLY satisfies the goal and every criterion. Respond EXACTLY: first line `GOAL: MET`, `GOAL: UNMET`, `GOAL: IMPOSSIBLE`, or `GOAL: INCONCLUSIVE`; for UNMET list unmet requirements, for IMPOSSIBLE provide evidence, and for INCONCLUSIVE explain what could not be determined; call NO tools."
        )),
        extra: serde_json::Map::from_iter([(
            "goal_verification".to_string(),
            json!({"hidden": true}),
        )]),
        ..Default::default()
    }
}

pub fn parse_goal_verdict(answer: &str, tool_called: bool) -> GoalVerdict {
    parse_goal_verdict_with_criteria(answer, tool_called, &[])
}

pub fn parse_goal_verdict_with_criteria(
    answer: &str,
    tool_called: bool,
    criteria: &[GoalCriterion],
) -> GoalVerdict {
    if tool_called {
        return inconclusive_verdict();
    }
    let Some(first) = answer.lines().map(str::trim).find(|line| !line.is_empty()) else {
        return inconclusive_verdict();
    };
    if first.eq_ignore_ascii_case("GOAL: MET") {
        return validate_met_criteria(answer, criteria);
    }
    if first.eq_ignore_ascii_case("GOAL: UNMET") {
        let criteria_verdicts = parse_criteria_verdicts(answer);
        let gaps = verdict_gaps(answer);
        if !gaps.is_empty() {
            GoalVerdict::Unmet(gaps)
        } else {
            let criterion_gaps =
                criterion_gaps(criteria_verdicts.iter(), CriterionVerdictStatus::Unmet);
            if criterion_gaps.is_empty() {
                inconclusive_verdict()
            } else {
                GoalVerdict::Unmet(criterion_gaps)
            }
        }
    } else if first.eq_ignore_ascii_case("GOAL: IMPOSSIBLE") {
        let mut evidence = verdict_gaps(answer);
        if evidence.is_empty() {
            evidence = criterion_gaps(
                parse_criteria_verdicts(answer).iter(),
                CriterionVerdictStatus::Impossible,
            );
        }
        if evidence.is_empty() {
            GoalVerdict::Inconclusive(vec!["impossible verdict requires evidence".to_string()])
        } else {
            GoalVerdict::Impossible(evidence)
        }
    } else if first.eq_ignore_ascii_case("GOAL: INCONCLUSIVE") {
        let mut gaps = verdict_gaps(answer);
        if gaps.is_empty() {
            gaps = criterion_gaps(
                parse_criteria_verdicts(answer).iter(),
                CriterionVerdictStatus::Inconclusive,
            );
        }
        if gaps.is_empty() {
            inconclusive_verdict()
        } else {
            GoalVerdict::Inconclusive(gaps)
        }
    } else {
        inconclusive_verdict()
    }
}

fn inconclusive_verdict() -> GoalVerdict {
    GoalVerdict::Inconclusive(vec![INCONCLUSIVE_GAP.to_string()])
}

fn verdict_gaps(answer: &str) -> Vec<String> {
    answer
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .skip(1)
        .filter(|line| !line.starts_with("CRITERION "))
        .map(|line| line.trim_start_matches(['-', '*', '•', ' ']).trim())
        .filter(|line| !line.is_empty())
        .map(ToString::to_string)
        .collect()
}

fn criterion_gaps<'a>(
    verdicts: impl Iterator<Item = &'a CriterionVerdict>,
    status: CriterionVerdictStatus,
) -> Vec<String> {
    verdicts
        .filter(|criterion| {
            criterion.status() == status
                && (status != CriterionVerdictStatus::Impossible || !criterion.note.is_empty())
        })
        .map(|criterion| {
            let fallback = match status {
                CriterionVerdictStatus::Met => "met",
                CriterionVerdictStatus::Unmet => "unmet",
                CriterionVerdictStatus::Impossible => "impossible",
                CriterionVerdictStatus::Inconclusive => "inconclusive",
            };
            if criterion.note.is_empty() {
                format!("{}: {fallback}", criterion.id)
            } else {
                format!("{}: {}", criterion.id, criterion.note)
            }
        })
        .collect()
}

fn validate_met_criteria(answer: &str, criteria: &[GoalCriterion]) -> GoalVerdict {
    let verdicts = parse_criteria_verdicts(answer);
    let unmet = criterion_gaps(verdicts.iter(), CriterionVerdictStatus::Unmet);
    if !unmet.is_empty() {
        return GoalVerdict::Unmet(unmet);
    }
    let impossible = criterion_gaps(verdicts.iter(), CriterionVerdictStatus::Impossible);
    if !impossible.is_empty() {
        return GoalVerdict::Impossible(impossible);
    }
    let inconclusive = criterion_gaps(verdicts.iter(), CriterionVerdictStatus::Inconclusive);
    if !inconclusive.is_empty() {
        return GoalVerdict::Inconclusive(inconclusive);
    }
    let mut missing = Vec::new();
    for criterion in criteria {
        let matches = verdicts
            .iter()
            .filter(|verdict| verdict.id == criterion.id)
            .collect::<Vec<_>>();
        if matches.len() != 1 || matches[0].status() != CriterionVerdictStatus::Met {
            missing.push(format!(
                "{}: criterion verdict missing, unrecognized, or ambiguous",
                criterion.id
            ));
        }
    }
    if missing.is_empty() {
        GoalVerdict::Met
    } else {
        GoalVerdict::Inconclusive(missing)
    }
}

pub fn parse_criteria_verdicts(answer: &str) -> Vec<CriterionVerdict> {
    answer
        .lines()
        .filter_map(|line| {
            let rest = line.trim().strip_prefix("CRITERION ")?;
            let (id, verdict) = rest.split_once(':')?;
            let verdict = verdict.trim();
            let mut parts = verdict.splitn(2, char::is_whitespace);
            let status = match parts.next()? {
                "MET" => CriterionVerdictStatus::Met,
                "UNMET" => CriterionVerdictStatus::Unmet,
                "IMPOSSIBLE" => CriterionVerdictStatus::Impossible,
                "INCONCLUSIVE" => CriterionVerdictStatus::Inconclusive,
                _ => return None,
            };
            let note = parts.next().unwrap_or_default();
            Some(CriterionVerdict {
                id: id.trim().trim_matches(['[', ']']).to_string(),
                met: status == CriterionVerdictStatus::Met,
                status: Some(status),
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
    let mut reply = reply;
    let criteria = session
        .goal
        .as_ref()
        .map(|goal| goal.criteria.clone())
        .unwrap_or_default();
    if reply.verdict == GoalVerdict::Met {
        let is_met_reply = reply
            .verifier_reply
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .is_some_and(|line| line.eq_ignore_ascii_case("GOAL: MET"));
        reply.verdict = if is_met_reply {
            validate_met_criteria(&reply.verifier_reply, &criteria)
        } else {
            inconclusive_verdict()
        };
    } else if matches!(&reply.verdict, GoalVerdict::Impossible(evidence) if evidence.is_empty()) {
        reply.verdict =
            GoalVerdict::Inconclusive(vec!["impossible verdict requires evidence".to_string()]);
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
        GoalVerdict::Impossible(evidence) => (
            "impossible".to_string(),
            evidence.clone(),
            "verification_impossible".to_string(),
            format!(
                "Goal verification found the goal impossible:\n{}",
                evidence.join("\n")
            ),
        ),
        GoalVerdict::Inconclusive(gaps) => (
            "inconclusive".to_string(),
            gaps.clone(),
            "verification_inconclusive".to_string(),
            format!("Goal verification was inconclusive:\n{}", gaps.join("\n")),
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
        GoalVerdict::Impossible(_) => {
            session.goal_mark_failed("verification_impossible");
            if trigger != "validate_goal" {
                session.set_runtime_state(SessionState::Completed, None);
            }
            GoalVerificationApplyOutcome::Failed
        }
        GoalVerdict::Inconclusive(_) => {
            if let Some(status) = held_status {
                session.goal_set_status(status);
            } else if !matches!(
                session.goal_status,
                Some(GoalStatus::BudgetExhausted | GoalStatus::NoProgress)
            ) {
                session.goal_set_status_reason(GoalStatus::Paused, "verification_inconclusive");
            }
            session.set_runtime_state(SessionState::Idle, None);
            GoalVerificationApplyOutcome::Stalled
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
                    || session
                        .goal
                        .as_ref()
                        .is_some_and(|goal| goal.progress.no_progress_turns >= QUIESCENCE_NUDGES)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{GoalBudget, GoalResolution};

    fn criteria() -> Vec<GoalCriterion> {
        vec![
            GoalCriterion {
                id: "C1".to_string(),
                text: "tests pass".to_string(),
                verify_hint: None,
            },
            GoalCriterion {
                id: "C2".to_string(),
                text: "docs updated".to_string(),
                verify_hint: None,
            },
        ]
    }

    fn session_with_criteria() -> ChatSession {
        let mut session = ChatSession::new("goal-verifier-domain".to_string());
        session.install_goal("agent", "ship feature", true, GoalBudget::default());
        session.goal.as_mut().unwrap().criteria = criteria();
        session.goal_set_status(GoalStatus::Verifying);
        session
    }

    #[test]
    fn structured_met_rejects_conflicting_unmet_criterion() {
        let verdict = parse_goal_verdict_with_criteria(
            "GOAL: MET\nCRITERION C1: MET\nCRITERION C2: UNMET — docs missing",
            false,
            &criteria(),
        );

        assert_eq!(
            verdict,
            GoalVerdict::Unmet(vec!["C2: docs missing".to_string()])
        );
        let mut session = session_with_criteria();
        let outcome = apply_goal_verdict(
            &mut session,
            "task_done",
            GoalVerifierReply {
                verdict: GoalVerdict::Met,
                verifier_reply: "GOAL: MET\nCRITERION C1: MET\nCRITERION C2: UNMET — docs missing"
                    .to_string(),
                tokens: 1,
            },
            None,
        );
        assert_eq!(outcome, GoalVerificationApplyOutcome::Rearmed);
        assert_eq!(session.goal_status, Some(GoalStatus::Active));
        assert_eq!(
            session.goal.as_ref().unwrap().resolution,
            GoalResolution::Active
        );
    }

    #[test]
    fn structured_met_requires_complete_recognized_criteria() {
        for answer in [
            "GOAL: MET\nCRITERION C1: MET",
            "GOAL: MET\nCRITERION C1: MET\nCRITERION C2: UNKNOWN",
            "GOAL: MET\nCRITERION C1: MET\nCRITERION C2: METAL",
            "GOAL: MET\nCRITERION C1: MET\nCRITERION C2: MET\nCRITERION C2: MET",
        ] {
            let verdict = parse_goal_verdict_with_criteria(answer, false, &criteria());
            assert!(matches!(verdict, GoalVerdict::Inconclusive(_)), "{answer}");
        }
        assert_eq!(
            parse_goal_verdict_with_criteria(
                "GOAL: MET\nCRITERION C1: MET\nCRITERION C2: MET",
                false,
                &criteria(),
            ),
            GoalVerdict::Met
        );
    }

    #[test]
    fn impossible_requires_evidence_and_marks_failed_for_manual_and_automatic() {
        assert!(matches!(
            parse_goal_verdict("GOAL: IMPOSSIBLE", false),
            GoalVerdict::Inconclusive(_)
        ));
        for trigger in ["task_done", "validate_goal"] {
            let mut session = session_with_criteria();
            let outcome = apply_goal_verdict(
                &mut session,
                trigger,
                GoalVerifierReply {
                    verdict: GoalVerdict::Impossible(vec!["requirements conflict".to_string()]),
                    verifier_reply: "GOAL: IMPOSSIBLE\n- requirements conflict".to_string(),
                    tokens: 2,
                },
                None,
            );
            assert_eq!(outcome, GoalVerificationApplyOutcome::Failed);
            let goal = session.goal.as_ref().unwrap();
            assert_eq!(goal.status, GoalStatus::Stopped);
            assert_eq!(goal.resolution, GoalResolution::Failed);
            assert_eq!(goal.stop_reason.as_deref(), Some("verification_impossible"));
            assert_eq!(goal.attempts[0].verdict, "impossible");
            assert_eq!(goal.attempts[0].gaps, vec!["requirements conflict"]);
            let replayed = crate::goal_ledger::reduce_goal_ledger(&session.goal_ledger).unwrap();
            assert_eq!(replayed.resolution, GoalResolution::Failed);
            assert_eq!(replayed.status, GoalStatus::Stopped);
        }
    }

    #[test]
    fn inconclusive_and_malformed_pause_without_success_or_failure() {
        for answer in [
            "GOAL: INCONCLUSIVE\n- evidence unavailable",
            "not a verdict",
        ] {
            let verdict = parse_goal_verdict(answer, false);
            assert!(matches!(verdict, GoalVerdict::Inconclusive(_)));
            let mut session = session_with_criteria();
            let outcome = apply_goal_verdict(
                &mut session,
                "task_done",
                GoalVerifierReply {
                    verdict,
                    verifier_reply: answer.to_string(),
                    tokens: 2,
                },
                None,
            );
            assert_eq!(outcome, GoalVerificationApplyOutcome::Stalled);
            let goal = session.goal.as_ref().unwrap();
            assert_eq!(goal.status, GoalStatus::Paused);
            assert_eq!(goal.resolution, GoalResolution::Active);
            assert_eq!(
                goal.stop_reason.as_deref(),
                Some("verification_inconclusive")
            );
            assert_eq!(goal.attempts[0].verdict, "inconclusive");
            assert_eq!(goal.progress.no_progress_turns, 0);
            assert_ne!(session.runtime.state, SessionState::Completed);
        }
    }

    #[test]
    fn provider_failure_stays_active_and_distinct_from_budget_exhaustion() {
        let mut session = session_with_criteria();
        let outcome = handle_verifier_failure(&mut session, "task_done", "provider unavailable");
        assert_eq!(outcome, GoalCompletionGateOutcome::VerificationUnavailable);
        assert_eq!(
            session.goal.as_ref().unwrap().resolution,
            GoalResolution::Active
        );
        assert_eq!(session.goal_status, Some(GoalStatus::Active));
        assert!(session.goal_verification_blocked_until_ms.is_some());
        assert_ne!(session.goal_status, Some(GoalStatus::BudgetExhausted));
    }

    #[test]
    fn inconclusive_budget_exhaustion_is_not_failed() {
        let mut session = ChatSession::new("goal-verifier-budget".to_string());
        session.install_goal(
            "agent",
            "ship feature",
            true,
            GoalBudget {
                max_turns: Some(1),
                ..Default::default()
            },
        );
        session.goal_set_status(GoalStatus::Verifying);

        let outcome = apply_goal_verdict(
            &mut session,
            "task_done",
            GoalVerifierReply {
                verdict: GoalVerdict::Inconclusive(vec![INCONCLUSIVE_GAP.to_string()]),
                verifier_reply: "not a verdict".to_string(),
                tokens: 1,
            },
            None,
        );

        assert_eq!(outcome, GoalVerificationApplyOutcome::Stalled);
        assert_eq!(session.goal_status, Some(GoalStatus::BudgetExhausted));
        assert_eq!(
            session.goal.as_ref().unwrap().resolution,
            GoalResolution::Active
        );
    }

    #[test]
    fn stale_impossible_verdict_does_not_fail_goal() {
        let mut session = session_with_criteria();
        let epoch = session.goal_ledger_last_seq();
        session.goal_set_status_reason(GoalStatus::Stopped, "user_stop");

        let outcome = apply_goal_verdict(
            &mut session,
            "task_done",
            GoalVerifierReply {
                verdict: GoalVerdict::Impossible(vec!["requirements conflict".to_string()]),
                verifier_reply: "GOAL: IMPOSSIBLE\n- requirements conflict".to_string(),
                tokens: 1,
            },
            Some(epoch),
        );

        assert_eq!(outcome, GoalVerificationApplyOutcome::Superseded);
        assert_eq!(session.goal_status, Some(GoalStatus::Stopped));
        assert_eq!(
            session.goal.as_ref().unwrap().resolution,
            GoalResolution::Cancelled
        );
        assert!(session.goal.as_ref().unwrap().attempts.is_empty());
    }

    #[test]
    fn legacy_criterion_wire_still_deserializes() {
        let unmet: CriterionVerdict = serde_json::from_value(json!({
            "id": "C1",
            "met": false,
            "note": "missing"
        }))
        .unwrap();
        assert_eq!(unmet.status(), CriterionVerdictStatus::Unmet);
        let met: CriterionVerdict =
            serde_json::from_value(json!({"id": "C2", "met": true})).unwrap();
        assert_eq!(met.status(), CriterionVerdictStatus::Met);
    }
}
