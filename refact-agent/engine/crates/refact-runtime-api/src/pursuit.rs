pub use refact_chat_api::pursuit::*;

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use refact_chat_api::goal_verifier::{
        apply_goal_verdict, GoalVerificationApplyOutcome, GoalVerifierReply, GoalVerdict,
    };
    use refact_chat_api::{
        ChatCommand, ChatMessage, ChatSession, GoalAutonomy, GoalBudget, GoalStatus, SessionState,
    };

    use super::*;
    use crate::chat_queue_helpers::{command_triggers_generation, note_user_turn_resets_goal_pursuit};

    fn session_with_goal() -> ChatSession {
        let mut session = ChatSession::new("pursuit-test".to_string());
        session.install_goal("agent", "finish the work", true, GoalBudget::default());
        session
    }

    fn request(source: PursuitSource) -> PursuitRequest {
        PursuitRequest::new(source, "test", 7, true)
    }

    fn context(now: Instant) -> PursuitArbitrationContext {
        PursuitArbitrationContext {
            now_ms: 10_000,
            now_instant: now,
            stall_grace: Duration::from_secs(5),
            no_token_grace: Duration::from_secs(5),
        }
    }

    #[test]
    fn pursuit_autonomy_off_suppresses_turn_and_buddy_is_visible_only() {
        let mut session = session_with_goal();
        session.goal.as_mut().unwrap().autonomy = GoalAutonomy::Off;
        let now = Instant::now();

        assert_eq!(
            arbitrate_pursuit(
                &session,
                &request(PursuitSource::InlineTurnEnd),
                context(now)
            ),
            PursuitDecision::Suppress(PursuitSuppressReason::AutonomyOff)
        );
        assert_eq!(
            arbitrate_pursuit(&session, &request(PursuitSource::Buddy), context(now)),
            PursuitDecision::VisibleOnly
        );
    }

    #[test]
    fn pursuit_legacy_active_goal_defaults_to_on_and_starts() {
        let session = session_with_goal();
        assert_eq!(session.goal.as_ref().unwrap().autonomy, GoalAutonomy::On);
        assert_eq!(
            arbitrate_pursuit(
                &session,
                &request(PursuitSource::InlineTurnEnd),
                context(Instant::now()),
            ),
            PursuitDecision::StartTurn
        );
    }

    #[test]
    fn pursuit_explicit_user_turn_bypasses_autonomy_gate() {
        let mut session = session_with_goal();
        session.goal.as_mut().unwrap().autonomy = GoalAutonomy::Off;
        note_user_turn_resets_goal_pursuit(&mut session);
        let command = ChatCommand::UserMessage {
            content: serde_json::json!("continue"),
            attachments: vec![],
            context_files: vec![],
            suppress_auto_enrichment: false,
            client_message_id: None,
            push: None,
        };
        assert!(command_triggers_generation(&command));
    }

    #[test]
    fn pursuit_duplicate_evidence_is_deduped_across_sources() {
        let mut session = session_with_goal();
        let original = request(PursuitSource::GoalMonitor);
        let mut message = ChatMessage::default();
        stamp_pursuit_message(&mut message, &original, 1);
        session.add_message(message);
        let now = Instant::now();

        assert_eq!(
            arbitrate_pursuit(&session, &request(PursuitSource::Buddy), context(now)),
            PursuitDecision::Suppress(PursuitSuppressReason::DuplicateEvidence)
        );
        let fresh = PursuitRequest::new(PursuitSource::Buddy, "new evidence", 8, true);
        assert_eq!(
            arbitrate_pursuit(&session, &fresh, context(now)),
            PursuitDecision::StartTurn
        );
    }

    #[test]
    fn pursuit_waiting_input_and_live_heartbeat_are_not_stalled() {
        let mut waiting = session_with_goal();
        waiting.set_runtime_state(SessionState::WaitingUserInput, None);
        assert_eq!(
            arbitrate_pursuit(
                &waiting,
                &request(PursuitSource::GoalMonitor),
                context(Instant::now()),
            ),
            PursuitDecision::Suppress(PursuitSuppressReason::WaitingForInput)
        );

        let mut live = session_with_goal();
        let now = Instant::now();
        live.start_stream();
        live.last_activity = now - Duration::from_secs(30);
        live.last_stream_delta_at = Some(now);
        assert_eq!(
            arbitrate_pursuit(&live, &request(PursuitSource::GoalMonitor), context(now)),
            PursuitDecision::Suppress(PursuitSuppressReason::LiveWork)
        );
    }

    #[test]
    fn pursuit_budget_and_quiescence_stop_new_turns_without_failing_goal() {
        let mut exhausted = session_with_goal();
        exhausted.goal.as_mut().unwrap().budget.max_turns = Some(1);
        exhausted.goal.as_mut().unwrap().progress.turns_used = 1;
        assert_eq!(
            arbitrate_pursuit(
                &exhausted,
                &request(PursuitSource::InlineTurnEnd),
                context(Instant::now()),
            ),
            PursuitDecision::Suppress(PursuitSuppressReason::BudgetExhausted(
                GoalStatus::BudgetExhausted,
            ))
        );
        assert_eq!(
            exhausted.goal.as_ref().unwrap().resolution,
            refact_chat_api::GoalResolution::Active
        );

        let mut quiescent = session_with_goal();
        quiescent.goal.as_mut().unwrap().progress.no_progress_turns = 3;
        assert_eq!(
            arbitrate_pursuit(
                &quiescent,
                &request(PursuitSource::InlineTurnEnd),
                context(Instant::now()),
            ),
            PursuitDecision::Suppress(PursuitSuppressReason::Quiescent)
        );
        assert_eq!(quiescent.goal_status, Some(GoalStatus::Active));
    }

    #[test]
    fn pursuit_verifier_rearm_uses_gate_and_dedupes_pending_wake() {
        let mut session = session_with_goal();
        session.goal_set_status(GoalStatus::Verifying);
        let outcome = apply_goal_verdict(
            &mut session,
            "agent_finish",
            GoalVerifierReply {
                verdict: GoalVerdict::Unmet(vec!["tests missing".to_string()]),
                verifier_reply: "GOAL: UNMET\n- tests missing".to_string(),
                tokens: 10,
            },
            None,
        );
        assert_eq!(outcome, GoalVerificationApplyOutcome::Rearmed);
        assert!(session.delivery_wake_sources.contains("goal"));
        assert!(session.goal_nudge_pending_for_current_goal());
        assert_eq!(
            arbitrate_pursuit(
                &session,
                &request(PursuitSource::GoalMonitor),
                context(Instant::now()),
            ),
            PursuitDecision::Suppress(PursuitSuppressReason::Cooldown)
        );
    }

    #[test]
    fn pursuit_verifier_rearm_autonomy_off_is_visible_without_wake() {
        let mut session = session_with_goal();
        session.goal.as_mut().unwrap().autonomy = GoalAutonomy::Off;
        session.goal_set_status(GoalStatus::Verifying);
        let outcome = apply_goal_verdict(
            &mut session,
            "agent_finish",
            GoalVerifierReply {
                verdict: GoalVerdict::Unmet(vec!["still open".to_string()]),
                verifier_reply: "GOAL: UNMET\n- still open".to_string(),
                tokens: 10,
            },
            None,
        );
        assert_eq!(outcome, GoalVerificationApplyOutcome::Rearmed);
        assert!(session.delivery_wake_sources.is_empty());
        let event = session.messages.last().unwrap();
        assert_eq!(event.extra["pursuit"]["wake_requested"], false);
    }

    #[test]
    fn pursuit_silent_tail_keeps_legacy_task_agent_behavior_without_goal() {
        let session = ChatSession::new("silent-tail".to_string());
        let now = Instant::now();
        assert_eq!(
            arbitrate_pursuit(&session, &request(PursuitSource::SilentTail), context(now)),
            PursuitDecision::StartTurn
        );
        assert_eq!(
            arbitrate_pursuit(&session, &request(PursuitSource::GoalMonitor), context(now)),
            PursuitDecision::Suppress(PursuitSuppressReason::NoGoal)
        );
    }

    #[test]
    fn pursuit_visible_only_request_never_starts_turn() {
        let session = session_with_goal();
        let request = PursuitRequest::new(PursuitSource::Buddy, "advice", 7, false);
        assert_eq!(
            arbitrate_pursuit(&session, &request, context(Instant::now())),
            PursuitDecision::VisibleOnly
        );
    }
}
