use std::sync::Arc;

use refact_chat_api::{DeliveryOutcome, MessageProvenance, PendingDelivery, PushMode};
use refact_core::chat_types::ChatMessage;
use refact_room::{
    replay_deliveries, route_event, DeliveryAttempt, DurableRoomJournal, PublicRoomEvent,
    RoomAuthor, RoomDelivery, RoomError, RoomEventDraft, RoomEventKind, RoomParticipant,
};
use refact_tasks::types::BoardCard;
use tokio::sync::Mutex as AMutex;

use crate::app_state::AppState;
use crate::room_event_store::{project_chat_message, ChatMessageRoomContext, JsonRoomEventStore};

const ROOM_REPLAY_LIMIT: usize = 20;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoomPublicationReport {
    pub event_id: String,
    pub delivered: Vec<String>,
    pub failed: Vec<(String, String)>,
}

pub async fn publish_latest_assistant_message(
    app: AppState,
    session_arc: Arc<AMutex<refact_chat_api::ChatSession>>,
) -> Result<Option<RoomPublicationReport>, String> {
    let (message, task_id, card_id, session_chat_id) = {
        let session = session_arc.lock().await;
        let Some(message) = session
            .messages
            .iter()
            .rev()
            .find(|message| message.role == "assistant")
            .cloned()
        else {
            return Ok(None);
        };
        let Some(meta) = session.thread.task_meta.clone() else {
            return Ok(None);
        };
        let Some(card_id) = meta.card_id else {
            return Ok(None);
        };
        (message, meta.task_id, card_id, session.chat_id.clone())
    };
    let Some(room) = load_room(&app, &task_id, &card_id).await? else {
        return Ok(None);
    };
    let Some(author) = refact_chat_api::room_provenance::room_member_provenance(
        &room.card,
        &message_provenance_chat_id(&message, &session_chat_id),
    ) else {
        return Ok(None);
    };
    let turn_id = message.message_id.clone();
    let event_id = format!("assistant:{turn_id}");
    let Some(draft) = project_chat_message(
        &message,
        &author,
        ChatMessageRoomContext {
            room_id: room.room_id.clone(),
            event_id,
            turn_id,
            created_at: chrono::Utc::now().to_rfc3339(),
            completed: true,
            source_revision: None,
            source_digest: None,
        },
    ) else {
        return Ok(None);
    };
    publish_draft(app, room, draft, PushMode::Append, true)
        .await
        .map(Some)
}

pub async fn publish_explicit_message(
    app: AppState,
    session_arc: Arc<AMutex<refact_chat_api::ChatSession>>,
    tool_call_id: &str,
    text: &str,
    target: Option<&str>,
    push: PushMode,
) -> Result<RoomPublicationReport, String> {
    let (task_id, card_id, turn_id) = {
        let session = session_arc.lock().await;
        let meta = session
            .thread
            .task_meta
            .clone()
            .ok_or_else(|| "Room message requires task metadata".to_string())?;
        let card_id = meta
            .card_id
            .ok_or_else(|| "Room message requires card metadata".to_string())?;
        let turn_id = session
            .messages
            .iter()
            .rev()
            .find(|message| {
                message.role == "assistant"
                    && message
                        .tool_calls
                        .as_ref()
                        .is_some_and(|calls| calls.iter().any(|call| call.id == tool_call_id))
            })
            .map(|message| message.message_id.clone())
            .unwrap_or_else(|| tool_call_id.to_string());
        (meta.task_id, card_id, turn_id)
    };
    let chat_id = session_arc.lock().await.chat_id.clone();
    let room = load_room(&app, &task_id, &card_id)
        .await?
        .ok_or_else(|| "Calling chat is not a member of this room".to_string())?;
    let author = refact_chat_api::room_provenance::room_member_provenance(&room.card, &chat_id)
        .ok_or_else(|| "Calling chat is not a member of this room".to_string())?;
    let mut source = ChatMessage::new("assistant".to_string(), text.to_string());
    source.message_id = format!("explicit:{tool_call_id}");
    let mut draft = project_chat_message(
        &source,
        &author,
        ChatMessageRoomContext {
            room_id: room.room_id.clone(),
            event_id: source.message_id.clone(),
            turn_id,
            created_at: chrono::Utc::now().to_rfc3339(),
            completed: true,
            source_revision: None,
            source_digest: None,
        },
    )
    .ok_or_else(|| "Room message must not be empty".to_string())?;
    if let Some(target) = target {
        let resolved = room
            .card
            .team()
            .iter()
            .find(|member| {
                member.agent_chat_id.as_deref() != Some(chat_id.as_str())
                    && (member.agent_id.as_deref() == Some(target)
                        || member.agent_chat_id.as_deref() == Some(target))
            })
            .and_then(|member| member.agent_chat_id.clone())
            .or_else(|| {
                room.card
                    .team()
                    .iter()
                    .find(|member| {
                        member.agent_chat_id.as_deref() != Some(chat_id.as_str())
                            && member.role.trim().eq_ignore_ascii_case(target.trim())
                    })
                    .and_then(|member| member.agent_chat_id.clone())
            })
            .or_else(|| {
                room.participants
                    .iter()
                    .find(|participant| {
                        participant.external_participant_id.is_some()
                            && participant.participant_id != chat_id
                            && (participant.participant_id == target
                                || participant.role.trim().eq_ignore_ascii_case(target.trim()))
                    })
                    .map(|participant| participant.participant_id.clone())
            })
            .ok_or_else(|| format!("Room recipient '{target}' is unavailable"))?;
        draft.recipient_id = Some(resolved);
    }
    publish_draft(app, room, draft, push, true).await
}

pub async fn replay_room_history(
    app: AppState,
    session_arc: Arc<AMutex<refact_chat_api::ChatSession>>,
) -> Result<(), String> {
    let (task_id, card_id, chat_id) = {
        let session = session_arc.lock().await;
        let Some(meta) = session.thread.task_meta.clone() else {
            return Ok(());
        };
        let Some(card_id) = meta.card_id else {
            return Ok(());
        };
        (meta.task_id, card_id, session.chat_id.clone())
    };
    let Some(room) = load_room(&app, &task_id, &card_id).await? else {
        return Ok(());
    };
    let Some(participant) = room
        .participants
        .iter()
        .find(|participant| participant.participant_id == chat_id)
        .cloned()
    else {
        return Ok(());
    };
    let journal = room_journal(&app, &room.room_id);
    let events = journal.events().await.map_err(room_error)?;
    let deliveries = replay_deliveries(&events, &participant, ROOM_REPLAY_LIMIT);
    for delivery in deliveries {
        deliver_one(&app, &journal, delivery, PushMode::Append, false).await;
    }
    Ok(())
}

pub async fn retry_room_delivery(
    app: AppState,
    task_id: &str,
    card_id: &str,
    event_id: &str,
    recipient_id: &str,
    linked: bool,
) -> Result<RoomPublicationReport, String> {
    let room = load_room(&app, task_id, card_id)
        .await?
        .ok_or_else(|| "Room is unavailable".to_string())?;
    let journal = room_journal(&app, &room.room_id);
    let event = journal
        .events()
        .await
        .map_err(room_error)?
        .into_iter()
        .find(|event| event.event_id == event_id)
        .ok_or_else(|| format!("Room event '{event_id}' is unavailable"))?;
    let mut delivery = route_event(&event, &room.participants)
        .map_err(room_error)?
        .into_iter()
        .find(|delivery| delivery.recipient.participant_id == recipient_id)
        .ok_or_else(|| format!("Room recipient '{recipient_id}' is unavailable"))?;
    if linked {
        delivery.attempt = delivery.attempt.linked_attempt();
    }
    let mut report = RoomPublicationReport {
        event_id: event_id.to_string(),
        delivered: Vec::new(),
        failed: Vec::new(),
    };
    deliver_routed(
        &app,
        &room,
        &journal,
        delivery,
        PushMode::Append,
        true,
        &mut report,
    )
    .await;
    Ok(report)
}

struct RoomContext {
    room_id: String,
    card: BoardCard,
    participants: Vec<RoomParticipant>,
}

async fn load_room(
    app: &AppState,
    task_id: &str,
    card_id: &str,
) -> Result<Option<RoomContext>, String> {
    let board = crate::task_storage::load_board(app.gcx.clone(), task_id).await?;
    let Some(card) = board.get_card(card_id).cloned() else {
        return Ok(None);
    };
    if !card.is_room() {
        return Ok(None);
    }
    let mut participants = Vec::new();
    for member in card.team() {
        if let Some(participant_id) = member.agent_chat_id.clone() {
            let provenance = refact_chat_api::room_provenance::member_provenance(&card, member);
            participants.push(RoomParticipant {
                participant_id,
                role: provenance.role,
                display_name: provenance.display_name,
                writes: member.writes.unwrap_or(false),
                external_participant_id: None,
            });
            continue;
        }
        let Some(participant_id) = member.agent_id.clone() else {
            continue;
        };
        if let Err(error) = app.external_participants.participant(&participant_id) {
            if matches!(
                error,
                crate::external_participant::ExternalParticipantError::NotFound(_)
            ) {
                continue;
            }
            return Err(error.to_string());
        }
        if member.writes() {
            return Err("external participants are always read-only".to_string());
        }
        let provenance = refact_chat_api::room_provenance::member_provenance(&card, member);
        participants.push(RoomParticipant {
            participant_id: participant_id.clone(),
            role: provenance.role,
            display_name: provenance.display_name,
            writes: false,
            external_participant_id: Some(participant_id),
        });
    }
    Ok(Some(RoomContext {
        room_id: format!("{task_id}:{card_id}"),
        card,
        participants,
    }))
}

fn room_journal(app: &AppState, room_id: &str) -> DurableRoomJournal<JsonRoomEventStore> {
    DurableRoomJournal::new(
        Arc::new(JsonRoomEventStore::new(
            app.gcx.cache_dir.join("room-events"),
        )),
        room_id,
    )
}

async fn publish_draft(
    app: AppState,
    room: RoomContext,
    draft: RoomEventDraft,
    push: PushMode,
    wake: bool,
) -> Result<RoomPublicationReport, String> {
    let journal = room_journal(&app, &room.room_id);
    route_event(
        &refact_room::RoomEvent::from_draft(draft.clone(), 1),
        &room.participants,
    )
    .map_err(room_error)?;
    let outcome = journal.append(draft).await.map_err(room_error)?;
    let deliveries = route_event(outcome.event(), &room.participants).map_err(room_error)?;
    let mut report = RoomPublicationReport {
        event_id: outcome.event().event_id.clone(),
        delivered: Vec::new(),
        failed: Vec::new(),
    };
    for delivery in deliveries {
        deliver_routed(&app, &room, &journal, delivery, push, wake, &mut report).await;
    }
    Ok(report)
}

async fn deliver_routed(
    app: &AppState,
    room: &RoomContext,
    journal: &DurableRoomJournal<JsonRoomEventStore>,
    delivery: RoomDelivery,
    push: PushMode,
    wake: bool,
    report: &mut RoomPublicationReport,
) {
    if delivery.recipient.external_participant_id.is_none() {
        deliver_one_with_report(app, journal, delivery, push, wake, report).await;
        return;
    }
    let recipient_id = delivery.recipient.participant_id.clone();
    let writer_root = room.card.team().iter().find_map(|member| {
        (member.writes() && member.agent_chat_id.is_some())
            .then(|| member.agent_worktree.clone())
            .flatten()
    });
    let Some(writer_root) = writer_root else {
        if let Err(error) = record_receipt(journal, &delivery, "failed").await {
            tracing::warn!("Failed to record external room delivery failure: {error}");
        }
        report.failed.push((
            recipient_id,
            "external advisor requires the writer worktree".to_string(),
        ));
        return;
    };
    match crate::room_uhp::deliver_external_advisor(
        app,
        std::path::Path::new(&writer_root),
        delivery.clone(),
    )
    .await
    {
        Ok(publication) => {
            let terminal =
                publication.status == crate::external_participant::ExternalDeliveryStatus::Terminal;
            let receipt = if terminal { "delivered" } else { "pending" };
            if let Err(error) = record_receipt(journal, &delivery, receipt).await {
                tracing::warn!("Failed to record external room delivery receipt: {error}");
            }
            if terminal {
                report.delivered.push(recipient_id);
            }
            for draft in publication.drafts {
                if let Err(error) =
                    publish_advisor_response(app, room, journal, draft, push, wake).await
                {
                    tracing::warn!("Failed to publish external advisor response: {error}");
                }
            }
        }
        Err(error) => {
            if let Err(receipt_error) = record_receipt(journal, &delivery, "failed").await {
                tracing::warn!("Failed to record external room delivery failure: {receipt_error}");
            }
            report.failed.push((recipient_id, error));
        }
    }
}

async fn publish_advisor_response(
    app: &AppState,
    room: &RoomContext,
    journal: &DurableRoomJournal<JsonRoomEventStore>,
    draft: RoomEventDraft,
    push: PushMode,
    wake: bool,
) -> Result<(), String> {
    let outcome = journal.append(draft).await.map_err(room_error)?;
    let deliveries = route_event(outcome.event(), &room.participants).map_err(room_error)?;
    for delivery in deliveries {
        deliver_one(app, journal, delivery, push, wake).await;
    }
    Ok(())
}

async fn deliver_one(
    app: &AppState,
    journal: &DurableRoomJournal<JsonRoomEventStore>,
    delivery: RoomDelivery,
    push: PushMode,
    wake: bool,
) {
    let mut report = RoomPublicationReport {
        event_id: delivery.event.event_id.clone(),
        delivered: Vec::new(),
        failed: Vec::new(),
    };
    deliver_one_with_report(app, journal, delivery, push, wake, &mut report).await;
}

async fn deliver_one_with_report(
    app: &AppState,
    journal: &DurableRoomJournal<JsonRoomEventStore>,
    delivery: RoomDelivery,
    push: PushMode,
    wake: bool,
    report: &mut RoomPublicationReport,
) {
    let recipient_id = delivery.recipient.participant_id.clone();
    let message = public_message(&delivery.event, &delivery.attempt);
    let result = refact_chat_api::delivery::deliver_to_chat(
        app.clone(),
        &recipient_id,
        PendingDelivery::with_id(
            delivery.attempt.idempotency_key.clone(),
            vec![message],
            push,
            "agents.room_message",
            wake,
        ),
    )
    .await;
    let receipt = match &result {
        Ok(DeliveryOutcome::Duplicate) => "duplicate",
        Ok(_) => "delivered",
        Err(_) => "failed",
    };
    if let Err(error) = record_receipt(journal, &delivery, receipt).await {
        tracing::warn!("Failed to record room delivery receipt: {error}");
    }
    match result {
        Ok(_) => report.delivered.push(recipient_id),
        Err(error) => {
            tracing::warn!(
                "Room delivery {} to {} failed: {}",
                delivery.attempt.delivery_id,
                recipient_id,
                error
            );
            report.failed.push((recipient_id, error));
        }
    }
}

fn public_message(event: &PublicRoomEvent, attempt: &DeliveryAttempt) -> ChatMessage {
    let author = MessageProvenance {
        chat_id: event.author.participant_id.clone(),
        role: event.author.role.clone(),
        display_name: event.author.display_name.clone(),
        card_id: event
            .room_id
            .rsplit_once(':')
            .map(|(_, card_id)| card_id.to_string()),
    };
    let label = if event.author.display_name.trim().is_empty() {
        event.author.role.as_str()
    } else {
        event.author.display_name.as_str()
    };
    let mut message = refact_chat_api::internal_roles::event(
        refact_chat_api::internal_roles::EventSubkind::SystemNotice,
        "agents.room_message",
        serde_json::json!({
            "room_id": event.room_id,
            "room_seq": event.room_seq,
            "event_id": event.event_id,
            "from": event.author.participant_id,
            "provenance": author,
            "delivery": attempt,
        }),
        format!(
            "[message from room {label} ({})]\n{}",
            event.author.participant_id, event.body
        ),
    );
    refact_chat_api::attach_provenance(&mut message, author);
    message
}

async fn record_receipt(
    journal: &DurableRoomJournal<JsonRoomEventStore>,
    delivery: &RoomDelivery,
    receipt: &str,
) -> Result<(), refact_room::RoomError> {
    journal
        .append(RoomEventDraft {
            room_id: delivery.event.room_id.clone(),
            event_id: format!("receipt:{}:{receipt}", delivery.attempt.delivery_id),
            kind: RoomEventKind::Delivery,
            author: RoomAuthor {
                participant_id: delivery.event.author.participant_id.clone(),
                role: delivery.event.author.role.clone(),
                display_name: delivery.event.author.display_name.clone(),
            },
            body: String::new(),
            recipient_id: Some(delivery.recipient.participant_id.clone()),
            source_message_id: delivery.event.event_id.clone(),
            turn_id: delivery.event.turn_id.clone(),
            created_at: chrono::Utc::now().to_rfc3339(),
            delivery: Some(delivery.attempt.clone()),
            delivery_error: (receipt == "failed").then(|| "delivery failed".to_string()),
            ..Default::default()
        })
        .await?;
    Ok(())
}

fn message_provenance_chat_id(message: &ChatMessage, fallback: &str) -> String {
    refact_chat_api::provenance_from_message(message)
        .map(|provenance| provenance.chat_id)
        .filter(|chat_id| !chat_id.is_empty())
        .unwrap_or_else(|| fallback.to_string())
}

fn room_error(error: RoomError) -> String {
    error.to_string()
}
