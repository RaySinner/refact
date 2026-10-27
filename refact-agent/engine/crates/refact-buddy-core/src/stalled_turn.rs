use chrono::{DateTime, Utc};

use crate::types::{BuddyFact, BuddyFactKind};

/// Ceiling on the quoted fragment of the agent's last message. The snippet ends
/// up in a fact payload and in Buddy's prompt, so it stays short enough to read
/// in one glance.
pub const MAX_SNIPPET_CHARS: usize = 240;

/// Modes in which the assistant is not expected to call a tool at all. A
/// tool-free answer there is the answer, not a stall.
pub const TOOLS_FORBIDDEN_MODES: [&str; 4] = ["explore", "no_tools", "no-tools", "chat"];

/// Everything the classifier needs to know about one finished assistant turn.
///
/// Primitives and borrows only: the decision is testable without an engine, a
/// session, or a task board.
#[derive(Debug, Clone, Copy)]
pub struct TurnContext<'a> {
    pub chat_id: &'a str,
    pub tool_calls_count: usize,
    pub has_content: bool,
    pub is_error: bool,
    pub is_aborted: bool,
    pub chat_mode: Option<&'a str>,
    pub goal_active: bool,
    pub card_doing: bool,
    pub plan_has_pending: bool,
}

/// Why a finished turn is *not* a stall. Distinct reasons keep a silent Buddy
/// debuggable instead of merely silent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotStalledReason {
    HasToolCalls,
    EmptyMessage,
    ErrorTurn,
    AbortedTurn,
    ToolsForbiddenMode,
    NoActiveObjective,
}

/// A stalled turn, with the piece of context that makes it actionable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StalledTurnSignal {
    pub chat_id: String,
    pub reason_context: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StalledTurnVerdict {
    NotStalled(NotStalledReason),
    Stalled(StalledTurnSignal),
}

/// Classify a finished assistant turn. The order of the checks is the contract:
/// errors and aborts are the engine's own recovery business, a turn with tool
/// calls is work in progress, and an empty or tool-free-mode turn carries
/// nothing to interpret.
pub fn classify_stalled_turn(ctx: &TurnContext) -> StalledTurnVerdict {
    if ctx.is_error {
        return StalledTurnVerdict::NotStalled(NotStalledReason::ErrorTurn);
    }
    if ctx.is_aborted {
        return StalledTurnVerdict::NotStalled(NotStalledReason::AbortedTurn);
    }
    if ctx.tool_calls_count > 0 {
        return StalledTurnVerdict::NotStalled(NotStalledReason::HasToolCalls);
    }
    if !ctx.has_content {
        return StalledTurnVerdict::NotStalled(NotStalledReason::EmptyMessage);
    }
    if ctx
        .chat_mode
        .is_some_and(|mode| TOOLS_FORBIDDEN_MODES.contains(&mode))
    {
        return StalledTurnVerdict::NotStalled(NotStalledReason::ToolsForbiddenMode);
    }

    let reason_context = if ctx.goal_active {
        "goal_active"
    } else if ctx.card_doing {
        "card_doing"
    } else if ctx.plan_has_pending {
        "plan_pending"
    } else {
        return StalledTurnVerdict::NotStalled(NotStalledReason::NoActiveObjective);
    };

    StalledTurnVerdict::Stalled(StalledTurnSignal {
        chat_id: ctx.chat_id.to_string(),
        reason_context,
    })
}

/// The tail of the agent's last message, trimmed and cut on a word boundary so
/// the quoted fragment does not end mid-word.
pub fn last_message_snippet(text: &str) -> String {
    let trimmed = text.trim();
    let truncated = trimmed.chars().count() > MAX_SNIPPET_CHARS;
    let mut snippet: String = trimmed.chars().take(MAX_SNIPPET_CHARS).collect();
    if truncated {
        if let Some(cut) = snippet.rfind(char::is_whitespace) {
            if cut > 0 {
                snippet.truncate(cut);
            }
        }
    }
    snippet.trim_end().to_string()
}

/// The fact Buddy reacts to. `turn_index` keeps two consecutive stalls in the
/// same chat apart, so the second one is not deduped away as a repeat.
pub fn stalled_turn_fact(
    signal: &StalledTurnSignal,
    turn_index: u64,
    snippet: &str,
    now: DateTime<Utc>,
) -> BuddyFact {
    BuddyFact {
        kind: BuddyFactKind::AgentTurnStalled,
        key: format!("stalled_turn:{}:{}", signal.chat_id, turn_index),
        source: "stalled_turn",
        payload: serde_json::json!({
            "chat_id": signal.chat_id,
            "turn_index": turn_index,
            "reason_context": signal.reason_context,
            "snippet": snippet,
        }),
        seen_at: now,
        confidence: 0.8,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-05-15T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    fn base_ctx() -> TurnContext<'static> {
        TurnContext {
            chat_id: "chat-1",
            tool_calls_count: 0,
            has_content: true,
            is_error: false,
            is_aborted: false,
            chat_mode: Some("agent"),
            goal_active: false,
            card_doing: false,
            plan_has_pending: false,
        }
    }

    fn stalled_reason(ctx: &TurnContext) -> &'static str {
        match classify_stalled_turn(ctx) {
            StalledTurnVerdict::Stalled(signal) => signal.reason_context,
            other => panic!("expected a stall, got {other:?}"),
        }
    }

    fn not_stalled_reason(ctx: &TurnContext) -> NotStalledReason {
        match classify_stalled_turn(ctx) {
            StalledTurnVerdict::NotStalled(reason) => reason,
            other => panic!("expected no stall, got {other:?}"),
        }
    }

    #[test]
    fn tool_free_turn_with_active_goal_is_a_stall() {
        let ctx = TurnContext {
            goal_active: true,
            ..base_ctx()
        };
        let verdict = classify_stalled_turn(&ctx);
        assert_eq!(
            verdict,
            StalledTurnVerdict::Stalled(StalledTurnSignal {
                chat_id: "chat-1".to_string(),
                reason_context: "goal_active",
            })
        );
    }

    #[test]
    fn turn_with_tool_calls_is_work_in_progress() {
        let ctx = TurnContext {
            tool_calls_count: 2,
            goal_active: true,
            ..base_ctx()
        };
        assert_eq!(not_stalled_reason(&ctx), NotStalledReason::HasToolCalls);
    }

    #[test]
    fn tools_forbidden_mode_is_never_a_stall() {
        let ctx = TurnContext {
            chat_mode: Some("no_tools"),
            goal_active: true,
            ..base_ctx()
        };
        assert_eq!(
            not_stalled_reason(&ctx),
            NotStalledReason::ToolsForbiddenMode
        );
    }

    #[test]
    fn tool_free_modes_are_all_recognised() {
        for mode in TOOLS_FORBIDDEN_MODES {
            let ctx = TurnContext {
                chat_mode: Some(mode),
                card_doing: true,
                ..base_ctx()
            };
            assert_eq!(
                not_stalled_reason(&ctx),
                NotStalledReason::ToolsForbiddenMode,
                "mode {mode}"
            );
        }
    }

    #[test]
    fn plain_conversation_without_objective_is_not_a_stall() {
        let ctx = TurnContext {
            chat_mode: None,
            ..base_ctx()
        };
        assert_eq!(
            not_stalled_reason(&ctx),
            NotStalledReason::NoActiveObjective
        );
    }

    #[test]
    fn card_in_doing_without_goal_is_a_stall() {
        let ctx = TurnContext {
            card_doing: true,
            ..base_ctx()
        };
        assert_eq!(stalled_reason(&ctx), "card_doing");
    }

    #[test]
    fn pending_plan_without_goal_or_card_is_a_stall() {
        let ctx = TurnContext {
            plan_has_pending: true,
            ..base_ctx()
        };
        assert_eq!(stalled_reason(&ctx), "plan_pending");
    }

    #[test]
    fn goal_wins_over_card_and_plan_as_reason_context() {
        let ctx = TurnContext {
            goal_active: true,
            card_doing: true,
            plan_has_pending: true,
            ..base_ctx()
        };
        assert_eq!(stalled_reason(&ctx), "goal_active");

        let card_wins = TurnContext {
            card_doing: true,
            plan_has_pending: true,
            ..base_ctx()
        };
        assert_eq!(stalled_reason(&card_wins), "card_doing");
    }

    #[test]
    fn error_turn_belongs_to_the_engine_recovery() {
        let ctx = TurnContext {
            is_error: true,
            goal_active: true,
            ..base_ctx()
        };
        assert_eq!(not_stalled_reason(&ctx), NotStalledReason::ErrorTurn);
    }

    #[test]
    fn aborted_turn_is_not_a_stall() {
        let ctx = TurnContext {
            is_aborted: true,
            goal_active: true,
            ..base_ctx()
        };
        assert_eq!(not_stalled_reason(&ctx), NotStalledReason::AbortedTurn);
    }

    #[test]
    fn empty_message_carries_nothing_to_interpret() {
        let ctx = TurnContext {
            has_content: false,
            goal_active: true,
            ..base_ctx()
        };
        assert_eq!(not_stalled_reason(&ctx), NotStalledReason::EmptyMessage);
    }

    #[test]
    fn snippet_keeps_short_text_whole() {
        assert_eq!(
            last_message_snippet("  all done, nothing to add  "),
            "all done, nothing to add"
        );
    }

    #[test]
    fn snippet_truncates_on_a_word_boundary() {
        let text = "lorem ipsum ".repeat(60);
        let snippet = last_message_snippet(&text);
        assert!(snippet.chars().count() <= MAX_SNIPPET_CHARS);
        assert!(snippet.ends_with("ipsum"), "{snippet}");
    }

    #[test]
    fn snippet_handles_a_single_unbroken_token() {
        let text = "x".repeat(MAX_SNIPPET_CHARS + 50);
        let snippet = last_message_snippet(&text);
        assert_eq!(snippet.chars().count(), MAX_SNIPPET_CHARS);
    }

    #[test]
    fn snippet_is_char_safe_for_wide_characters() {
        let text = "привет ".repeat(80);
        let snippet = last_message_snippet(&text);
        assert!(snippet.chars().count() <= MAX_SNIPPET_CHARS);
        assert!(snippet.ends_with("привет"), "{snippet}");
    }

    #[test]
    fn stalled_turn_fact_carries_chat_snippet_and_turn() {
        let signal = StalledTurnSignal {
            chat_id: "chat-1".to_string(),
            reason_context: "goal_active",
        };
        let fact = stalled_turn_fact(&signal, 7, "I think we are done here", now());

        assert_eq!(fact.kind, BuddyFactKind::AgentTurnStalled);
        assert_eq!(fact.key, "stalled_turn:chat-1:7");
        assert_eq!(fact.payload["chat_id"], serde_json::json!("chat-1"));
        assert_eq!(
            fact.payload["snippet"],
            serde_json::json!("I think we are done here")
        );
        assert_eq!(
            fact.payload["reason_context"],
            serde_json::json!("goal_active")
        );
        assert_eq!(fact.payload["turn_index"], serde_json::json!(7));
        assert_eq!(fact.seen_at, now());
    }
}
