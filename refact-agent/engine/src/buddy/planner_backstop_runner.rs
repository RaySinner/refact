use std::collections::HashSet;
use chrono::{DateTime, Timelike, Utc};
use refact_buddy_core::planner_backstop::{
    parse_reply_marker, plan_backstop, reply_marker, BackstopPlan, BackstopQuestion, BackstopUrgency,
};
use refact_buddy_core::settings::BuddySettings;
use refact_buddy_core::state::SpeechRotationState;
use refact_core::chat_types::{PendingDelivery, PushMode};

use crate::app_state::AppState;
use crate::chat::delivery::deliver_to_chat;
use crate::chat::internal_roles::{event, EventSubkind};
use crate::tasks::storage::{load_board, update_board_atomic};
use crate::tasks::types::StatusUpdate;

pub const BACKSTOP_SOURCE: &str = "buddy.planner_backstop";
const ASK_PREFIX: &str = "[ASK:";

#[derive(Debug, Clone)]
pub struct BackstopCandidate {
    pub task_id: String,
    pub question: BackstopQuestion,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackstopOutcome {
    Answered { answer: String },
    NotifiedHuman { text: String },
    Skipped { question_id: String, reason: String },
}

fn parse_status_id<'a>(message: &'a str, prefix: &str) -> Option<(&'a str, &'a str)> {
    let rest = message.strip_prefix(prefix)?;
    let end = rest.find(']')?;
    let id = &rest[..end];
    if id.len() != 8 || !id.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    Some((id, rest[end + 1..].trim_start()))
}

fn parse_ask(message: &str) -> Option<(&str, &str)> {
    parse_status_id(message, ASK_PREFIX)
}

fn parse_block_marker(message: &str) -> Option<&str> {
    let rest = message.strip_prefix(ASK_PREFIX)?;
    let id = rest.strip_suffix(":block] agent flagged for planner attention")?;
    if id.len() != 8 || !id.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    Some(id)
}

fn split_question_and_urgency(text: &str) -> (String, BackstopUrgency) {
    let trimmed = text.trim();
    for (suffix, urgency) in [
        (" (urgency=block)", BackstopUrgency::Block),
        (" (urgency=info)", BackstopUrgency::Info),
    ] {
        if let Some(q) = trimmed.strip_suffix(suffix) {
            return (q.to_string(), urgency);
        }
    }
    (trimmed.to_string(), BackstopUrgency::Info)
}

pub async fn collect_backstop_candidates(
    app: &AppState,
    task_ids: &[String],
) -> Vec<BackstopCandidate> {
    let mut candidates = Vec::new();
    for task_id in task_ids {
        let board = match load_board(app.gcx.clone(), task_id).await {
            Ok(b) => b,
            Err(_) => continue,
        };

        let mut replied = HashSet::new();
        let mut block_markers = HashSet::new();

        for card in &board.cards {
            for update in &card.status_updates {
                if let Some((id, _)) = parse_reply_marker(&update.message) {
                    replied.insert((card.id.clone(), id.to_string()));
                }
                if let Some(id) = parse_block_marker(&update.message) {
                    block_markers.insert((card.id.clone(), id.to_string()));
                }
            }
        }

        let mut seen = HashSet::new();
        for card in &board.cards {
            for update in &card.status_updates {
                let Some((id, text)) = parse_ask(&update.message) else {
                    continue;
                };
                let key = (card.id.clone(), id.to_string());
                if replied.contains(&key) || !seen.insert(key.clone()) {
                    continue;
                }
                let (question_text, mut urgency) = split_question_and_urgency(text);
                if block_markers.contains(&key) {
                    urgency = BackstopUrgency::Block;
                }
                let asked_at = DateTime::parse_from_rfc3339(&update.timestamp)
                    .map(|dt| dt.with_timezone(&Utc))
                    .unwrap_or_else(|_| Utc::now());

                candidates.push(BackstopCandidate {
                    task_id: task_id.clone(),
                    question: BackstopQuestion {
                        card_id: card.id.clone(),
                        question_id: id.to_string(),
                        urgency,
                        question: question_text,
                        asked_at,
                    },
                });
            }
        }
    }
    candidates
}

pub async fn maybe_answer_overdue_question(
    app: &AppState,
    settings: BuddySettings,
    rotation: SpeechRotationState,
    auto_quiet_window: Option<(u8, u8)>,
    task_id: &str,
    question: &BackstopQuestion,
) -> BackstopOutcome {
    let board = match load_board(app.gcx.clone(), task_id).await {
        Ok(b) => b,
        Err(e) => {
            return BackstopOutcome::Skipped {
                question_id: question.question_id.clone(),
                reason: format!("board_load_failed: {e}"),
            };
        }
    };

    let Some(card) = board.get_card(&question.card_id) else {
        return BackstopOutcome::Skipped {
            question_id: question.question_id.clone(),
            reason: "card_not_found".to_string(),
        };
    };

    let answered = card.status_updates.iter().any(|u| {
        parse_reply_marker(&u.message)
            .map(|(id, _)| id == question.question_id)
            .unwrap_or(false)
    });

    let target_chat_id = card.agent_chat_id.clone();
    let local_hour = chrono::Local::now().hour();
    let now = Utc::now();

    let plan = plan_backstop(
        &settings,
        &rotation,
        target_chat_id.as_deref(),
        local_hour,
        auto_quiet_window,
        now,
        question,
        answered,
    );

    match plan {
        BackstopPlan::Skip(reason) => BackstopOutcome::Skipped {
            question_id: question.question_id.clone(),
            reason: reason.as_str().to_string(),
        },
        BackstopPlan::Answer { answer } => {
            let reply_status = reply_marker(&question.question_id, &answer);
            let q_id = question.question_id.clone();
            let c_id = question.card_id.clone();
            let _ = update_board_atomic(app.gcx.clone(), task_id, move |board| {
                if let Some(c) = board.get_card_mut(&c_id) {
                    c.status_updates.push(StatusUpdate {
                        timestamp: Utc::now().to_rfc3339(),
                        message: reply_status.clone(),
                    });
                }
                Ok(())
            })
            .await;

            if let Some(chat_id) = target_chat_id {
                let msg = event(
                    EventSubkind::SystemNotice,
                    BACKSTOP_SOURCE,
                    serde_json::json!({
                        "kind": "planner_backstop_answer",
                        "task_id": task_id,
                        "card_id": question.card_id,
                        "question_id": q_id,
                    }),
                    answer.clone(),
                );
                let _ = deliver_to_chat(
                    app.clone(),
                    &chat_id,
                    PendingDelivery::new(vec![msg], PushMode::WhenIdle, BACKSTOP_SOURCE, true),
                )
                .await;
            }

            BackstopOutcome::Answered { answer }
        }
        BackstopPlan::NotifyHuman { text } => {
            let q_id = question.question_id.clone();
            if let Some(chat_id) = target_chat_id {
                let msg = event(
                    EventSubkind::SystemNotice,
                    BACKSTOP_SOURCE,
                    serde_json::json!({
                        "kind": "planner_backstop_notify",
                        "task_id": task_id,
                        "card_id": question.card_id,
                        "question_id": q_id,
                    }),
                    text.clone(),
                );
                let _ = deliver_to_chat(
                    app.clone(),
                    &chat_id,
                    PendingDelivery::new(vec![msg], PushMode::WhenIdle, BACKSTOP_SOURCE, true),
                )
                .await;
            }

            BackstopOutcome::NotifiedHuman { text }
        }
    }
}
