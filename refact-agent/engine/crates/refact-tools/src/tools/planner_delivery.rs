// Shared planner -> task-agent live message delivery.
//
// Every planner-side producer (agent_steer, planner_reply, task_broadcast,
// pause_agent, resume_agent) targets a single board card and has to answer the
// same questions: is the card still the card we validated, is its agent chat
// still the same chat, is there a live session, and did the message actually
// get delivered. This module owns that reserve/revalidate/deliver sequence and
// is the only place in the tools layer that touches the unified delivery API,
// so a signature change lands in one file.

use std::collections::HashMap;
use std::sync::Arc;

use chrono::Utc;
use serde_json::Value;

use refact_core::chat_types::{ChatContent, ChatMessage};
use refact_runtime_api::ChatCtx;
use refact_runtime_api::task_storage as storage;
use refact_tasks::types::{BoardCard, StatusUpdate};
use refact_core::chat_types::{DeliveryOutcome, PendingDelivery, PushMode};
use refact_runtime_api::{ChatSessionFacade, SessionState};

pub fn push_mode_schema() -> Value {
    PushMode::schema()
}

pub fn parse_push_mode(args: &HashMap<String, Value>) -> Result<PushMode, String> {
    PushMode::from_args(args)
}

pub fn push_mode_label(push: PushMode) -> &'static str {
    match push {
        PushMode::Preempt => "preempt",
        PushMode::Append => "append",
        PushMode::WhenIdle => "when_idle",
    }
}

pub fn planner_user_message(content: String) -> ChatMessage {
    ChatMessage {
        role: "user".to_string(),
        content: ChatContent::SimpleText(content),
        ..Default::default()
    }
}

pub fn delivery(
    messages: Vec<ChatMessage>,
    push: PushMode,
    source: &str,
    wake: bool,
) -> PendingDelivery {
    PendingDelivery::new(messages, push, source.to_string(), wake)
}

/// Attach a caller-chosen stable id so the delivery layer can drop a repeat of
/// the same logical message (a retried planner_reply, for example).
pub fn with_dedupe_id(mut delivery: PendingDelivery, id: String) -> PendingDelivery {
    delivery.id = id;
    delivery
}

pub fn single_message_delivery(
    content: String,
    push: PushMode,
    source: &str,
    wake: bool,
) -> PendingDelivery {
    delivery(vec![planner_user_message(content)], push, source, wake)
}

#[derive(Clone)]
pub struct CardTarget {
    pub card_id: String,
    pub title: String,
    pub chat_id: String,
}

impl CardTarget {
    pub fn from_card(card: &BoardCard, chat_id: String) -> Self {
        Self {
            card_id: card.id.clone(),
            title: card.title.clone(),
            chat_id,
        }
    }
}

/// What must still be true about the card at delivery time.
#[derive(Clone, Copy)]
pub struct CardGuard {
    /// Require the card to still be in the `doing` column.
    pub require_doing: bool,
    /// Probe the session first and report "no live session" instead of
    /// delivering into a chat that no longer exists.
    pub require_live_session: bool,
}

impl CardGuard {
    pub fn doing_with_live_session() -> Self {
        Self {
            require_doing: true,
            require_live_session: true,
        }
    }

    pub fn doing() -> Self {
        Self {
            require_doing: true,
            require_live_session: false,
        }
    }
}

/// Honest per-card result. `Skipped`/`NoLiveSession`/`Failed` all mean the
/// message did NOT reach the agent; only `Delivered` may be reported as success
/// and it still distinguishes a fresh delivery from a deduped repeat.
pub enum DeliveryReport {
    Delivered {
        outcome: DeliveryOutcome,
        prior_state: Option<SessionState>,
    },
    NoLiveSession,
    Skipped {
        reason: String,
    },
    Failed {
        error: String,
    },
}

impl DeliveryReport {
    /// True only when this call actually handed a new message to the agent.
    pub fn reached_agent(&self) -> bool {
        matches!(
            self,
            DeliveryReport::Delivered {
                outcome: DeliveryOutcome::Queued | DeliveryOutcome::Delivered,
                ..
            }
        )
    }

    pub fn short_status(&self) -> String {
        match self {
            DeliveryReport::Delivered { outcome, .. } => match outcome {
                DeliveryOutcome::Queued => "queued".to_string(),
                DeliveryOutcome::Delivered => "delivered".to_string(),
                DeliveryOutcome::Duplicate => {
                    "not delivered (duplicate of an already-sent message)".to_string()
                }
            },
            DeliveryReport::NoLiveSession => "not delivered (no live agent session)".to_string(),
            DeliveryReport::Skipped { reason } => format!("not delivered ({})", reason),
            DeliveryReport::Failed { error } => format!("not delivered (failed: {})", error),
        }
    }

    pub fn describe(&self, action: &str) -> String {
        match self {
            DeliveryReport::Delivered {
                outcome,
                prior_state,
            } => {
                let state = prior_state
                    .map(|state| state.to_string())
                    .unwrap_or_else(|| "unavailable".to_string());
                match outcome {
                    DeliveryOutcome::Duplicate => format!(
                        "{} message was NOT delivered: the delivery layer recognised it as a duplicate of an already-sent message.",
                        action
                    ),
                    _ => format!(
                        "{} message {}; prior agent state: {}",
                        action,
                        self.short_status(),
                        state
                    ),
                }
            }
            DeliveryReport::NoLiveSession => format!(
                "{} message was NOT delivered: no live agent session was found.",
                action
            ),
            DeliveryReport::Skipped { reason } => {
                format!("{} message was NOT delivered: {}.", action, reason)
            }
            DeliveryReport::Failed { error } => {
                format!(
                    "{} message was NOT delivered: delivery failed: {}",
                    action, error
                )
            }
        }
    }
}

pub fn stale_card_reason(
    card: Option<&BoardCard>,
    card_id: &str,
    expected_chat_id: &str,
    require_doing: bool,
) -> Option<String> {
    let Some(card) = card else {
        return Some(format!("card {} no longer exists", card_id));
    };
    if require_doing && card.column != "doing" {
        return Some(format!(
            "card {} is now in column '{}'",
            card_id, card.column
        ));
    }
    if card.agent_chat_id.as_deref() != Some(expected_chat_id) {
        return Some(format!(
            "agent_chat_id changed from '{}' to '{}'",
            expected_chat_id,
            card.agent_chat_id.as_deref().unwrap_or("none")
        ));
    }
    None
}

async fn card_is_stale(
    ctx: &ChatCtx,
    task_id: &str,
    target: &CardTarget,
    require_doing: bool,
) -> Result<Option<String>, String> {
    let board = storage::load_board_for_ctx(ctx, task_id).await?;
    Ok(stale_card_reason(
        board.get_card(&target.card_id),
        &target.card_id,
        &target.chat_id,
        require_doing,
    ))
}

/// Revalidate the card, optionally probe for a live session, revalidate once
/// more to close the window opened by that probe, then deliver.
pub async fn deliver_to_card(
    ctx: &ChatCtx,
    facade: Arc<dyn ChatSessionFacade>,
    task_id: &str,
    target: &CardTarget,
    guard: CardGuard,
    pending: PendingDelivery,
) -> DeliveryReport {
    match card_is_stale(ctx, task_id, target, guard.require_doing).await {
        Ok(Some(reason)) => return DeliveryReport::Skipped { reason },
        Ok(None) => {}
        Err(error) => {
            return DeliveryReport::Failed {
                error: format!("could not re-read the board: {}", error),
            }
        }
    }

    let mut prior_state = None;
    if guard.require_live_session {
        match facade.session_state(&target.chat_id).await {
            Ok(None) => return DeliveryReport::NoLiveSession,
            Ok(state) => prior_state = state,
            Err(error) => {
                return DeliveryReport::Failed {
                    error: format!("could not read the agent session state: {}", error),
                }
            }
        }

        match card_is_stale(ctx, task_id, target, guard.require_doing).await {
            Ok(Some(reason)) => return DeliveryReport::Skipped { reason },
            Ok(None) => {}
            Err(error) => {
                return DeliveryReport::Failed {
                    error: format!("could not re-read the board: {}", error),
                }
            }
        }
    }

    match facade.deliver_messages(&target.chat_id, pending).await {
        Ok(outcome) => DeliveryReport::Delivered {
            outcome,
            prior_state,
        },
        Err(error) => DeliveryReport::Failed { error },
    }
}

const CARD_SKIP_PREFIX: &str = "planner_delivery_skip:";

/// Append a status update only while the card still matches the target,
/// reserving it for this send. `Ok(Some(reason))` means the card moved on and
/// nothing was written.
pub async fn reserve_card_status(
    ctx: &ChatCtx,
    task_id: &str,
    target: &CardTarget,
    status_message: String,
    require_doing: bool,
    touch_heartbeat: bool,
) -> Result<Option<String>, String> {
    let card_id = target.card_id.clone();
    let chat_id = target.chat_id.clone();
    let timestamp = Utc::now().to_rfc3339();
    let result = storage::update_board_atomic_for_ctx(ctx, task_id, move |board| {
        if let Some(reason) =
            stale_card_reason(board.get_card(&card_id), &card_id, &chat_id, require_doing)
        {
            return Err(format!("{}{}", CARD_SKIP_PREFIX, reason));
        }
        let card = board
            .get_card_mut(&card_id)
            .ok_or_else(|| format!("{}card {} no longer exists", CARD_SKIP_PREFIX, card_id))?;
        if touch_heartbeat {
            card.last_heartbeat_at = Some(timestamp.clone());
        }
        card.status_updates.push(StatusUpdate {
            timestamp: timestamp.clone(),
            message: status_message.clone(),
        });
        Ok(())
    })
    .await;

    match result {
        Ok((_, ())) => Ok(None),
        Err(error) => match error.strip_prefix(CARD_SKIP_PREFIX) {
            Some(reason) => Ok(Some(reason.to_string())),
            None => Err(error),
        },
    }
}

/// Append a follow-up status update (delivered / failed) for a card that is
/// still bound to the same agent chat. Silently does nothing when the card has
/// since been rebound, so late writes never land on a different agent's card.
pub async fn record_card_status(
    ctx: &ChatCtx,
    task_id: &str,
    target: &CardTarget,
    status_message: String,
) -> Result<(), String> {
    let card_id = target.card_id.clone();
    let chat_id = target.chat_id.clone();
    let timestamp = Utc::now().to_rfc3339();
    storage::update_board_atomic_for_ctx(ctx, task_id, move |board| {
        let card = board
            .get_card_mut(&card_id)
            .ok_or_else(|| format!("Card {} not found", card_id))?;
        if card.agent_chat_id.as_deref() != Some(chat_id.as_str()) {
            return Ok(());
        }
        card.status_updates.push(StatusUpdate {
            timestamp: timestamp.clone(),
            message: status_message.clone(),
        });
        Ok(())
    })
    .await
    .map(|_| ())
}

pub fn truncate_chars(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect()
}

