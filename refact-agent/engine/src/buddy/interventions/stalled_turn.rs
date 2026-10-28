use std::sync::Arc;

use chrono::Utc;
use tokio::sync::Mutex as AMutex;

use crate::app_state::AppState;
use crate::chat::types::{ChatSession, GoalSnapshot, GoalStatus};
use refact_buddy_core::stalled_turn::{
    classify_stalled_turn, last_message_snippet, stalled_turn_fact, TurnContext,
    StalledTurnSignal, StalledTurnVerdict,
};

/// Collect everything the S-1 classifier needs about a finished assistant turn.
/// Pure extraction from the session: no board loads, no LLM calls, no locks
/// held beyond the session read itself.
pub(super) fn collect_turn_context(
    session: &ChatSession,
    tool_calls_count: usize,
) -> TurnContext<'_> {
    let chat_mode = {
        let mode = session.thread.mode.trim();
        (!mode.is_empty()).then_some(mode)
    };
    let goal_active = session.goal.as_ref().is_some_and(|goal| {
        goal.active
            && !matches!(
                goal.status,
                GoalStatus::Completed | GoalStatus::Stopped | GoalStatus::Transferred
            )
    });
    let card_doing = is_task_agent_chat(session);
    TurnContext {
        chat_id: &session.chat_id,
        tool_calls_count,
        has_content: last_assistant_has_visible_text(session),
        is_error: false,
        is_aborted: false,
        chat_mode,
        goal_active,
        card_doing,
        plan_has_pending: crate::chat::plan_role::current_base_plan(session).is_some(),
    }
}

/// Task-agent chat detector, mirroring `crate::chat::generation::recovery::
/// is_task_agent_chat`. A load-bearing board lookup would turn the hook into
/// work, so the card-in-doing check stays cheap: the thread's task meta alone.
fn is_task_agent_chat(session: &ChatSession) -> bool {
    session
        .thread
        .task_meta
        .as_ref()
        .is_some_and(|meta| meta.role == "agents")
}

/// Whether the last assistant message carries non-empty visible text. A
/// reasoning-only turn is `false`, so the classifier sees an EmptyMessage.
fn last_assistant_has_visible_text(session: &ChatSession) -> bool {
    session
        .messages
        .last()
        .filter(|message| message.role == "assistant")
        .is_some_and(|message| !message.content.content_text_only().trim().is_empty())
}

/// Zero-latency stall hook: called straight from the turn-finalisation site.
/// Never blocks generation, never panics: every failure is a warning line.
pub(crate) async fn maybe_detect_stalled_turn(
    app: AppState,
    session_arc: Arc<AMutex<ChatSession>>,
    tool_calls_count: usize,
    turn_index: u64,
) {
    let result =
        detect_and_record_stalled_turn(app, &session_arc, tool_calls_count, turn_index).await;
    if let Err(error) = result {
        tracing::warn!("stalled_turn: hook failed (ignored): {error}");
    }
}

async fn detect_and_record_stalled_turn(
    app: AppState,
    session_arc: &Arc<AMutex<ChatSession>>,
    tool_calls_count: usize,
    turn_index: u64,
) -> Result<(), String> {
    let settings_enabled = {
        let buddy = app.buddy.buddy.lock().await;
        buddy
            .as_ref()
            .map(|svc| svc.settings.stalled_turn_intervention)
            .unwrap_or(false)
    };
    if !settings_enabled {
        return Ok(());
    }

    let snippet = {
        let session = session_arc.lock().await;
        if session.abort_flag.load(std::sync::atomic::Ordering::SeqCst) {
            return Ok(());
        }
        let ctx = collect_turn_context(&session, tool_calls_count);
        match classify_stalled_turn(&ctx) {
            StalledTurnVerdict::NotStalled(_) => return Ok(()),
            StalledTurnVerdict::Stalled(signal) => {
                let snippet = last_message_snippet(&last_assistant_text(&session));
                tracing::info!(
                    chat_id = %signal.chat_id,
                    reason_context = signal.reason_context,
                    turn_index,
                    "stalled_turn: agent turn ended without tool calls while an objective is active"
                );
                (signal, snippet)
            }
        }
    };
    let (signal, snippet) = snippet;

    let fact = stalled_turn_fact(&signal, turn_index, &snippet, Utc::now());
    {
        let mut buddy = app.buddy.buddy.lock().await;
        match buddy.as_mut() {
            Some(svc) => svc.fact_store.ingest(fact),
            None => return Err("buddy service is not running".to_string()),
        }
    }
    handle_stalled_turn(app, &signal, &snippet).await;
    Ok(())
}

/// The intervention itself. S-2 ships the fact + a log line; the Buddy-LLM
/// intervention body replaces this in S-3. Errors inside must never break the
/// generation that triggered the hook.
async fn handle_stalled_turn(_app: AppState, signal: &StalledTurnSignal, snippet: &str) {
    tracing::info!(
        chat_id = %signal.chat_id,
        reason_context = signal.reason_context,
        snippet,
        "stalled_turn: intervention hook fired (LLM intervention pending, S-3)"
    );
}

fn last_assistant_text(session: &ChatSession) -> String {
    session
        .messages
        .last()
        .filter(|message| message.role == "assistant")
        .map(|message| message.content.content_text_only())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::call_validation::{ChatContent, ChatMessage};
    use crate::chat::types::ChatSession;
    use refact_buddy_core::stalled_turn::NotStalledReason;

    fn visible(role: &str, text: &str) -> ChatMessage {
        ChatMessage {
            message_id: uuid::Uuid::new_v4().to_string(),
            role: role.to_string(),
            content: ChatContent::SimpleText(text.to_string()),
            ..Default::default()
        }
    }

    fn session_with_goal() -> ChatSession {
        let mut session = ChatSession::new("chat-1".to_string());
        session.goal = Some(GoalSnapshot {
            active: true,
            status: GoalStatus::Active,
            ..Default::default()
        });
        session
    }

    #[test]
    fn tool_free_turn_with_active_goal_classifies_as_stalled() {
        let mut session = session_with_goal();
        session
            .messages
            .push(visible("assistant", "I think that is everything for now."));
        let ctx = collect_turn_context(&session, 0);
        assert!(matches!(
            classify_stalled_turn(&ctx),
            StalledTurnVerdict::Stalled(_)
        ));
    }

    #[test]
    fn turn_with_tool_calls_is_never_a_stall() {
        let mut session = session_with_goal();
        session.messages.push(ChatMessage {
            role: "assistant".to_string(),
            content: ChatContent::SimpleText("working".to_string()),
            tool_calls: Some(vec![crate::call_validation::ChatToolCall {
                id: "call-1".to_string(),
                function: crate::call_validation::ChatToolFunction {
                    name: "search_pattern".to_string(),
                    arguments: "{}".to_string(),
                },
                tool_type: "function".to_string(),
                index: None,
                extra_content: None,
                started_at_ms: None,
                completed_at_ms: None,
            }]),
            ..Default::default()
        });
        let ctx = collect_turn_context(&session, 1);
        assert_eq!(
            classify_stalled_turn(&ctx),
            StalledTurnVerdict::NotStalled(NotStalledReason::HasToolCalls)
        );
    }

    #[test]
    fn reasoning_only_turn_is_empty_message() {
        let mut session = session_with_goal();
        session.messages.push(ChatMessage {
            role: "assistant".to_string(),
            content: ChatContent::SimpleText("   ".to_string()),
            reasoning_content: Some("I pondered deeply".to_string()),
            ..Default::default()
        });
        let ctx = collect_turn_context(&session, 0);
        assert_eq!(
            classify_stalled_turn(&ctx),
            StalledTurnVerdict::NotStalled(NotStalledReason::EmptyMessage)
        );
    }

    #[test]
    fn completed_goal_is_not_active() {
        let mut session = session_with_goal();
        session.goal.as_mut().unwrap().status = GoalStatus::Completed;
        session.messages.push(visible("assistant", "done"));
        let ctx = collect_turn_context(&session, 0);
        assert_eq!(
            classify_stalled_turn(&ctx),
            StalledTurnVerdict::NotStalled(NotStalledReason::NoActiveObjective)
        );
    }

    #[test]
    fn tool_forbidden_mode_is_not_a_stall() {
        let mut session = session_with_goal();
        session.thread.mode = "chat".to_string();
        session
            .messages
            .push(visible("assistant", "Just a plain answer."));
        let ctx = collect_turn_context(&session, 0);
        assert_eq!(
            classify_stalled_turn(&ctx),
            StalledTurnVerdict::NotStalled(NotStalledReason::ToolsForbiddenMode)
        );
    }
}
