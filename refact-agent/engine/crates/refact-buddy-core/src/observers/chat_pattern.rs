use chrono::{DateTime, Utc};

use refact_core::chat_types::{ChatContent, ChatMessage};

use crate::observers::Ephemeral;
use crate::types::{BuddyFact, BuddyFactKind};

const RETRY_KEYWORDS: &[&str] = &[
    "actually",
    "wait",
    "no,",
    "sorry",
    "not what i meant",
    "try again",
    "undo",
    "revert",
];

fn message_text(msg: &ChatMessage) -> &str {
    match &msg.content {
        ChatContent::SimpleText(s) => s.as_str(),
        _ => "",
    }
}

pub fn count_retry_streak(messages: &[ChatMessage]) -> u32 {
    let mut streak = 0u32;
    for msg in messages.iter().rev() {
        if msg.role != "user" {
            continue;
        }
        let text = message_text(msg).to_lowercase();
        if text.is_empty() {
            break;
        }
        if RETRY_KEYWORDS.iter().any(|k| text.contains(k)) {
            streak += 1;
        } else {
            break;
        }
    }
    streak
}

pub fn detect_chat_pattern_facts(
    messages: &[ChatMessage],
    chat_id: &str,
    now: DateTime<Utc>,
) -> Vec<BuddyFact> {
    let mut facts = vec![];
    // SECURITY: do not copy message content
    let view = Ephemeral::new(messages);
    let retry_streak = count_retry_streak(view.as_ref());
    // view drops here — no further access
    drop(view);

    if retry_streak >= 3 {
        facts.push(BuddyFact {
            kind: BuddyFactKind::ChatRetryStreak,
            key: format!("chat:retry_streak:{}", chat_id),
            source: "chat_pattern",
            payload: serde_json::json!({
                "chat_id": chat_id,
                "retry_streak": retry_streak,
            }),
            seen_at: now,
            confidence: 0.85,
        });
    }
    facts
}

