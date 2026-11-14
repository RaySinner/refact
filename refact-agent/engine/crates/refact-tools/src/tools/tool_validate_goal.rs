use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Map, Value};
use tokio::sync::Mutex as AMutex;

use refact_runtime_api::AtCommandsContext;
use refact_core::chat_types::{ChatContent, ChatMessage, ContextEnum};
use refact_chat_api::goal_verifier::{
    apply_goal_verdict_guarded_with_delivery, begin_goal_verification_if_needed,
    GoalVerificationApplyOutcome, GoalVerificationBegin, GoalVerdict,
};
use refact_runtime_api::goal_verifier_runtime::run_goal_verifier;
use refact_chat_api::types::{ChatSession, PushMode};
use crate::tools::tools_description::{Tool, ToolDesc, ToolSource, ToolSourceType};

pub struct ToolValidateGoal {
    pub config_path: String,
}

#[async_trait]
impl Tool for ToolValidateGoal {
    fn tool_description(&self) -> ToolDesc {
        ToolDesc {
            name: "validate_goal".to_string(),
            display_name: "Validate Goal".to_string(),
            source: ToolSource {
                source_type: ToolSourceType::Builtin,
                config_path: self.config_path.clone(),
            },
            experimental: false,
            allow_parallel: false,
            description: "Validate the chat's active goal against its success criteria. If met, the goal is marked complete and pursuit stops; otherwise returns the remaining gaps.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {"push": PushMode::schema()},
                "required": [],
            }),
            output_schema: None,
            annotations: None,
        }
    }

    async fn tool_execute(
        &mut self,
        ccx: Arc<AMutex<AtCommandsContext>>,
        tool_call_id: &String,
        args: &HashMap<String, Value>,
    ) -> Result<(bool, Vec<ContextEnum>), String> {
        let push = PushMode::from_args(args)?;
        let (app, chat_id) = {
            let ccx = ccx.lock().await;
            (ccx.app.clone(), ccx.chat_id.clone())
        };
        let sessions_map = app.chat.sessions.clone();
        let session_arc = {
            let sessions = sessions_map.read().await;
            sessions.get(&chat_id).cloned()
        };
        let session_arc = match session_arc {
            Some(session) => session,
            None if refact_runtime_api::session_runtime::try_restore_session_if_trajectory_exists(
                app.chat_ctx(),
                &app.chat.sessions,
                &chat_id,
            )
            .await =>
            {
                let sessions = sessions_map.read().await;
                sessions
                    .get(&chat_id)
                    .cloned()
                    .ok_or_else(|| format!("chat session `{chat_id}` not found"))?
            }
            None => return Err(format!("chat session `{chat_id}` not found")),
        };

        let (begin, epoch, fork_trajectory_version) = {
            let mut session = session_arc.lock().await;
            let begin = begin_validate_goal(&mut session);
            (
                begin,
                session.goal_ledger_last_seq(),
                session.trajectory_version,
            )
        };
        match begin {
            GoalVerificationBegin::Started => {}
            GoalVerificationBegin::NoActiveGoal => {
                return tool_message(tool_call_id, "No active goal to validate.".to_string());
            }
            GoalVerificationBegin::BudgetExhausted => {
                return tool_message(tool_call_id, budget_exhausted_message());
            }
        }

        let reply = match run_goal_verifier(app, session_arc.clone()).await {
            Ok(reply) => reply,
            Err(error) => {
                reset_after_verifier_error_with_delivery(
                    session_arc.clone(),
                    &error,
                    push,
                    Some(tool_call_id),
                )
                .await;
                return tool_message(
                    tool_call_id,
                    format!("Goal validation could not run: {error}"),
                );
            }
        };

        let verdict = reply.verdict.clone();
        let verifier_reply = reply.verifier_reply.clone();
        let outcome = {
            let mut session = session_arc.lock().await;
            apply_goal_verdict_guarded_with_delivery(
                &mut session,
                "validate_goal",
                reply,
                Some(epoch),
                fork_trajectory_version,
                push,
                Some(tool_call_id),
            )
        };
        let content = validation_content(outcome, &verdict, &verifier_reply);
        let extra = validation_extra(outcome, &verdict);
        tool_message_with_extra(tool_call_id, content, extra)
    }
}

pub fn begin_validate_goal(session: &mut ChatSession) -> GoalVerificationBegin {
    begin_goal_verification_if_needed(session)
}

pub fn budget_exhausted_message() -> String {
    "Goal budget exhausted — cannot validate; the goal has hit its configured budget. Adjust the budget (set_goal_budget) or stop the goal.".to_string()
}


pub async fn reset_after_verifier_error_with_delivery(
    session_arc: Arc<AMutex<ChatSession>>,
    error: &str,
    push: PushMode,
    tool_call_id: Option<&str>,
) {
    let mut session = session_arc.lock().await;
    let _ = refact_chat_api::goal_verifier::handle_verifier_failure_with_delivery(
        &mut session,
        "validate_goal",
        error,
        push,
        tool_call_id,
    );
}

pub fn validation_content(
    outcome: GoalVerificationApplyOutcome,
    verdict: &GoalVerdict,
    verifier_reply: &str,
) -> String {
    match outcome {
        GoalVerificationApplyOutcome::Finalized => {
            format!("GOAL MET — goal marked complete; pursuit disabled.\n\n{verifier_reply}")
        }
        GoalVerificationApplyOutcome::Superseded => {
            "Goal state changed during validation; verdict discarded.".to_string()
        }
        GoalVerificationApplyOutcome::Rearmed
        | GoalVerificationApplyOutcome::Continued
        | GoalVerificationApplyOutcome::Stalled => {
            let gaps = match verdict {
                GoalVerdict::Unmet(gaps) => gaps.as_slice(),
                GoalVerdict::Met => &[],
            };
            format!(
                "GOAL NOT YET MET — remaining gaps:\n{}\n\n{}",
                format_gaps(gaps),
                verifier_reply
            )
        }
        GoalVerificationApplyOutcome::NoGoal => "No active goal to validate.".to_string(),
    }
}

pub fn validation_extra(
    outcome: GoalVerificationApplyOutcome,
    verdict: &GoalVerdict,
) -> Map<String, Value> {
    let marker = match (outcome, verdict) {
        (GoalVerificationApplyOutcome::Finalized, GoalVerdict::Met) => Some("met"),
        (
            GoalVerificationApplyOutcome::Rearmed
            | GoalVerificationApplyOutcome::Continued
            | GoalVerificationApplyOutcome::Stalled,
            GoalVerdict::Unmet(_),
        ) => Some("unmet"),
        _ => None,
    };
    marker
        .map(|verdict| Map::from_iter([("validate_goal".to_string(), json!({"verdict": verdict}))]))
        .unwrap_or_default()
}

fn format_gaps(gaps: &[String]) -> String {
    if gaps.is_empty() {
        return "- verification inconclusive; continue".to_string();
    }
    gaps.iter()
        .map(|gap| format!("- {gap}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn tool_message(tool_call_id: &str, content: String) -> Result<(bool, Vec<ContextEnum>), String> {
    tool_message_with_extra(tool_call_id, content, Map::new())
}

fn tool_message_with_extra(
    tool_call_id: &str,
    content: String,
    extra: Map<String, Value>,
) -> Result<(bool, Vec<ContextEnum>), String> {
    Ok((
        false,
        vec![ContextEnum::ChatMessage(ChatMessage {
            role: "tool".to_string(),
            content: ChatContent::SimpleText(content),
            tool_call_id: tool_call_id.to_string(),
            extra,
            ..Default::default()
        })],
    ))
}

