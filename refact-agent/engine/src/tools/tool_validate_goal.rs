pub use refact_tools::tools::tool_validate_goal::*;

#[cfg(test)]
#[allow(unused_imports)]
use std::collections::HashMap;

#[cfg(test)]
#[allow(unused_imports)]
use std::sync::Arc;

#[cfg(test)]
#[allow(unused_imports)]
use async_trait::async_trait;

#[cfg(test)]
#[allow(unused_imports)]
use serde_json::{json, Map, Value};

#[cfg(test)]
#[allow(unused_imports)]
use tokio::sync::Mutex as AMutex;

#[cfg(test)]
#[allow(unused_imports)]
use refact_runtime_api::AtCommandsContext;

#[cfg(test)]
#[allow(unused_imports)]
use refact_core::chat_types::{ChatContent, ChatMessage, ContextEnum};

#[cfg(test)]
#[allow(unused_imports)]
use refact_chat_api::goal_verifier::{
    apply_goal_verdict_guarded_with_delivery, begin_goal_verification_if_needed,
    GoalVerificationApplyOutcome, GoalVerificationBegin, GoalVerdict,
};

#[cfg(test)]
#[allow(unused_imports)]
use refact_runtime_api::goal_verifier_runtime::run_goal_verifier;

#[cfg(test)]
#[allow(unused_imports)]
use refact_chat_api::types::{ChatSession, PushMode};

#[cfg(test)]
#[allow(unused_imports)]
use crate::tools::tools_description::{Tool, ToolDesc, ToolSource, ToolSourceType};

#[cfg(test)]
async fn reset_after_verifier_error(session_arc: Arc<AMutex<ChatSession>>, error: &str) {
    reset_after_verifier_error_with_delivery(session_arc, error, PushMode::Append, None).await;
}
#[cfg(test)]
mod tests {
    use super::*;
    use refact_chat_api::GoalBudget;

    use crate::app_state::AppState;
    use crate::chat::goal_verifier::{apply_goal_verdict, GoalVerifierReply};
    use refact_chat_api::types::{GoalStatus, SessionState};
    use crate::tools::tools_list::get_tools_for_mode;

    const CHAT_ID: &str = "validate-goal-chat";

    async fn ccx_for_session(
        session: ChatSession,
    ) -> (
        Arc<crate::global_context::GlobalContext>,
        Arc<AMutex<AtCommandsContext>>,
    ) {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        refact_yaml_configs::project_configs_bootstrap::global_configs_try_create_all(
            &gcx.config_dir,
        )
        .await
        .unwrap();
        gcx.chat_sessions
            .write()
            .await
            .insert(CHAT_ID.to_string(), Arc::new(AMutex::new(session)));
        (gcx.clone(), make_ccx(gcx).await)
    }

    async fn make_ccx(
        gcx: Arc<crate::global_context::GlobalContext>,
    ) -> Arc<AMutex<AtCommandsContext>> {
        Arc::new(AMutex::new(
            crate::at_commands::at_commands::at_commands_new_from_app(
                refact_runtime_api::app_state_from_gcx(gcx).await,
                4096,
                20,
                false,
                vec![],
                CHAT_ID.to_string(),
                None,
                "model".to_string(),
                None,
                None,
            )
            .await,
        ))
    }

    fn tool_content(result: (bool, Vec<ContextEnum>)) -> String {
        assert!(!result.0);
        match result.1.into_iter().next().expect("tool output") {
            ContextEnum::ChatMessage(message) => message.content.content_text_only(),
            ContextEnum::ContextFile(_) => panic!("expected chat message"),
        }
    }

    fn session_with_goal() -> ChatSession {
        let mut session = ChatSession::new(CHAT_ID.to_string());
        session.install_goal(
            "agent",
            "ship feature\n- tests pass",
            true,
            GoalBudget::default(),
        );
        session
    }

    fn finite_budget() -> GoalBudget {
        GoalBudget {
            max_turns: Some(1),
            max_minutes: None,
            max_tokens: None,
            max_cost_cents: None,
            cooldown_ms: 1_500,
            no_progress_token_threshold: 50,
            no_progress_turns: Some(1),
            explicit: true,
        }
    }

    fn session_with_exhausted_goal() -> ChatSession {
        let mut session = ChatSession::new(CHAT_ID.to_string());
        session.install_goal("agent", "ship feature\n- tests pass", true, finite_budget());
        session.goal.as_mut().unwrap().progress.turns_used = 1;
        session.refresh_goal_runtime_mirror();
        session
    }

    #[tokio::test]
    async fn available_in_plan_supporting_modes() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        refact_yaml_configs::project_configs_bootstrap::global_configs_try_create_all(
            &gcx.config_dir,
        )
        .await
        .unwrap();

        for mode in ["agent", "task_agent", "task_planner"] {
            let has_tool = get_tools_for_mode(gcx.clone(), mode, None)
                .await
                .into_iter()
                .any(|tool| tool.tool_description().name == "validate_goal");
            assert!(has_tool, "{mode} should expose validate_goal");
        }
        for mode in ["NO_TOOLS", "shell", "explore"] {
            let has_tool = get_tools_for_mode(gcx.clone(), mode, None)
                .await
                .into_iter()
                .any(|tool| tool.tool_description().name == "validate_goal");
            assert!(!has_tool, "{mode} should not expose validate_goal");
        }
    }

    #[tokio::test]
    async fn no_goal_returns_clean_message() {
        let (gcx, ccx) = ccx_for_session(ChatSession::new(CHAT_ID.to_string())).await;
        let mut tool = ToolValidateGoal {
            config_path: String::new(),
        };

        let content = tool_content(
            tool.tool_execute(ccx, &"call".to_string(), &HashMap::new())
                .await
                .unwrap(),
        );

        assert_eq!(content, "No active goal to validate.");
        let session_arc = gcx
            .chat_sessions
            .read()
            .await
            .get(CHAT_ID)
            .cloned()
            .unwrap();
        let session = session_arc.lock().await;
        assert!(session.goal.is_none());
        assert!(session.command_queue.is_empty());
    }

    #[tokio::test]
    async fn missing_child_session_is_restored_from_trajectory() {
        let workspace = tempfile::tempdir().unwrap();
        let gcx = crate::global_context::tests::make_test_gcx().await;
        *gcx.documents_state.workspace_folders.lock().unwrap() =
            vec![workspace.path().to_path_buf()];
        let app = refact_runtime_api::app_state_from_gcx(gcx.clone()).await;
        let mut session = ChatSession::new("subchat-validate-goal".to_string());
        session.install_goal("agent", "ship feature", true, GoalBudget::default());
        let snapshot =
            refact_chat_history::trajectory_snapshot::trajectory_snapshot_from_session(&session);
        refact_runtime_api::trajectory_runtime::save_trajectory_snapshot(gcx.clone(), snapshot)
            .await
            .unwrap();
        app.chat
            .sessions
            .write()
            .await
            .remove("subchat-validate-goal");
        let ccx = Arc::new(AMutex::new(
            crate::at_commands::at_commands::at_commands_new_from_app(
                app.clone(),
                4096,
                20,
                false,
                vec![],
                "subchat-validate-goal".to_string(),
                None,
                "model".to_string(),
                None,
                None,
            )
            .await,
        ));
        let mut tool = ToolValidateGoal {
            config_path: String::new(),
        };

        let content = tool_content(
            tool.tool_execute(ccx, &"call".to_string(), &HashMap::new())
                .await
                .unwrap(),
        );

        assert!(!content.contains("chat session"));
        assert_ne!(content, "No active goal to validate.");
        assert!(content.contains("GOAL") || content.contains("Goal validation"));
        assert!(app
            .chat
            .sessions
            .read()
            .await
            .contains_key("subchat-validate-goal"));
        let session = app
            .chat
            .sessions
            .read()
            .await
            .get("subchat-validate-goal")
            .cloned()
            .unwrap();
        assert_eq!(
            session.lock().await.goal.as_ref().unwrap().content,
            "ship feature"
        );
    }

    #[tokio::test]
    async fn verifying_goal_returns_clean_message_without_verifier_call() {
        let mut session = session_with_goal();
        session.goal_set_status(GoalStatus::Verifying);
        let (gcx, ccx) = ccx_for_session(session).await;
        let mut tool = ToolValidateGoal {
            config_path: String::new(),
        };

        let content = tool_content(
            tool.tool_execute(ccx, &"call".to_string(), &HashMap::new())
                .await
                .unwrap(),
        );

        assert_eq!(content, "No active goal to validate.");
        let session_arc = gcx
            .chat_sessions
            .read()
            .await
            .get(CHAT_ID)
            .cloned()
            .unwrap();
        let session = session_arc.lock().await;
        assert_eq!(session.goal_status, Some(GoalStatus::Verifying));
        assert!(session.goal.as_ref().unwrap().attempts.is_empty());
    }

    #[tokio::test]
    async fn inactive_goal_returns_clean_message_without_status_change() {
        let mut session = session_with_goal();
        session.goal_set_status(GoalStatus::Paused);
        let (gcx, ccx) = ccx_for_session(session).await;
        let mut tool = ToolValidateGoal {
            config_path: String::new(),
        };

        let content = tool_content(
            tool.tool_execute(ccx, &"call".to_string(), &HashMap::new())
                .await
                .unwrap(),
        );

        assert_eq!(content, "No active goal to validate.");
        let session_arc = gcx
            .chat_sessions
            .read()
            .await
            .get(CHAT_ID)
            .cloned()
            .unwrap();
        let session = session_arc.lock().await;
        let goal = session.goal.as_ref().unwrap();
        assert_eq!(goal.status, GoalStatus::Paused);
        assert!(goal.attempts.is_empty());
        assert!(session.command_queue.is_empty());
    }

    #[tokio::test]
    async fn budget_exhausted_goal_returns_distinct_message_without_verifier_call() {
        let (gcx, ccx) = ccx_for_session(session_with_exhausted_goal()).await;
        let mut tool = ToolValidateGoal {
            config_path: String::new(),
        };

        let content = tool_content(
            tool.tool_execute(ccx, &"call".to_string(), &HashMap::new())
                .await
                .unwrap(),
        );

        assert_eq!(content, budget_exhausted_message());
        assert_ne!(content, "No active goal to validate.");
        let session_arc = gcx
            .chat_sessions
            .read()
            .await
            .get(CHAT_ID)
            .cloned()
            .unwrap();
        let session = session_arc.lock().await;
        let goal = session.goal.as_ref().unwrap();
        assert_eq!(goal.status, GoalStatus::Active);
        assert!(goal.attempts.is_empty());
        assert!(session.post_tool_side_effects.is_empty());
    }

    #[tokio::test]
    async fn verifier_error_cleanup_restores_goal_status_without_forcing_idle() {
        let mut session = session_with_goal();
        session.set_runtime_state(SessionState::ExecutingTools, None);
        session.goal_set_status(GoalStatus::Verifying);
        let session_arc = Arc::new(AMutex::new(session));

        reset_after_verifier_error(session_arc.clone(), "verifier model unavailable").await;

        let session = session_arc.lock().await;
        assert_eq!(session.goal_status, Some(GoalStatus::Active));
        assert_eq!(session.runtime.state, SessionState::ExecutingTools);
        assert!(session.goal_verification_blocked_until_ms.is_some());
    }

    #[test]
    fn begin_validate_goal_sets_verifying_for_active_goal_only() {
        let mut session = session_with_goal();

        let begin = begin_validate_goal(&mut session);

        assert_eq!(begin, GoalVerificationBegin::Started);
        assert_eq!(session.goal_status, Some(GoalStatus::Verifying));

        let mut paused = session_with_goal();
        paused.goal_set_status(GoalStatus::Paused);
        let begin = begin_validate_goal(&mut paused);

        assert_eq!(begin, GoalVerificationBegin::NoActiveGoal);
        assert_eq!(paused.goal_status, Some(GoalStatus::Paused));
    }

    #[test]
    fn met_verdict_finalizes_and_formats_tool_message() {
        let mut session = session_with_goal();
        session.goal_set_status(GoalStatus::Verifying);
        let reply = GoalVerifierReply {
            verdict: GoalVerdict::Met,
            verifier_reply: "GOAL: MET".to_string(),
            tokens: 11,
        };
        let verdict = reply.verdict.clone();
        let verifier_reply = reply.verifier_reply.clone();

        let outcome = apply_goal_verdict(&mut session, "validate_goal", reply, None);
        let content = validation_content(outcome, &verdict, &verifier_reply);

        assert_eq!(outcome, GoalVerificationApplyOutcome::Finalized);
        assert_eq!(session.runtime.state, SessionState::Idle);
        assert_eq!(session.goal_status, Some(GoalStatus::Completed));
        assert_eq!(session.messages.len(), 2);
        assert!(session.post_tool_side_effects.is_empty());
        assert_eq!(
            session.messages.last().unwrap().extra["event"]["payload"]["kind"],
            json!("verified")
        );
        assert_eq!(
            content,
            "GOAL MET — goal marked complete; pursuit disabled.\n\nGOAL: MET"
        );
        assert_eq!(
            validation_extra(outcome, &verdict)["validate_goal"]["verdict"],
            json!("met")
        );
    }

    #[test]
    fn unmet_verdict_continues_and_formats_gaps() {
        let mut session = session_with_goal();
        session.set_runtime_state(SessionState::ExecutingTools, None);
        session.goal_set_status(GoalStatus::Verifying);
        let reply = GoalVerifierReply {
            verdict: GoalVerdict::Unmet(vec!["missing tests".to_string(), "docs".to_string()]),
            verifier_reply: "GOAL: UNMET\n- missing tests\n- docs".to_string(),
            tokens: 13,
        };
        let verdict = reply.verdict.clone();
        let verifier_reply = reply.verifier_reply.clone();

        let outcome = apply_goal_verdict(&mut session, "validate_goal", reply, None);
        let content = validation_content(outcome, &verdict, &verifier_reply);

        assert_eq!(outcome, GoalVerificationApplyOutcome::Continued);
        assert_eq!(session.runtime.state, SessionState::ExecutingTools);
        assert_eq!(session.goal_status, Some(GoalStatus::Active));
        assert_eq!(session.goal.as_ref().unwrap().progress.no_progress_turns, 1);
        assert_eq!(
            session.goal.as_ref().unwrap().attempts[0].gaps,
            vec!["missing tests", "docs"]
        );
        assert_eq!(session.messages.len(), 2);
        assert!(session.post_tool_side_effects.is_empty());
        assert_eq!(
            session.messages.last().unwrap().extra["event"]["payload"]["kind"],
            json!("verification_gaps")
        );
        assert!(session.command_queue.is_empty());
        assert_eq!(
            validation_extra(outcome, &verdict)["validate_goal"]["verdict"],
            json!("unmet")
        );
        assert_eq!(
            content,
            "GOAL NOT YET MET — remaining gaps:\n- missing tests\n- docs\n\nGOAL: UNMET\n- missing tests\n- docs"
        );
    }
}
