use serde::{Deserialize, Serialize};

use crate::{RoomAuthor, RoomEvent, RoomEventDraft, RoomEventKind, ROOM_EVENT_SCHEMA_VERSION};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicProjectionInput {
    pub room_id: String,
    pub event_id: String,
    pub role: String,
    pub completed: bool,
    pub body: String,
    pub author: RoomAuthor,
    pub source_message_id: String,
    pub turn_id: String,
    pub created_at: String,
    pub source_revision: Option<String>,
    pub source_digest: Option<String>,
}

pub fn project_assistant_message(input: PublicProjectionInput) -> Option<RoomEventDraft> {
    if input.role != "assistant" || !input.completed || input.body.trim().is_empty() {
        return None;
    }
    Some(RoomEventDraft {
        schema_version: ROOM_EVENT_SCHEMA_VERSION,
        room_id: input.room_id,
        event_id: input.event_id,
        kind: RoomEventKind::AssistantMessage,
        author: input.author,
        body: input.body,
        recipient_id: None,
        source_message_id: input.source_message_id,
        turn_id: input.turn_id,
        created_at: input.created_at,
        source_revision: input.source_revision,
        source_digest: input.source_digest,
        delivery: None,
        delivery_error: None,
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PublicRoomEvent {
    pub schema_version: u32,
    pub room_id: String,
    pub room_seq: u64,
    pub event_id: String,
    pub kind: RoomEventKind,
    pub author: RoomAuthor,
    pub body: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recipient_id: Option<String>,
    pub source_message_id: String,
    pub turn_id: String,
    pub created_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_revision: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_digest: Option<String>,
}

impl Default for PublicRoomEvent {
    fn default() -> Self {
        Self {
            schema_version: ROOM_EVENT_SCHEMA_VERSION,
            room_id: String::new(),
            room_seq: 0,
            event_id: String::new(),
            kind: RoomEventKind::AssistantMessage,
            author: RoomAuthor::default(),
            body: String::new(),
            recipient_id: None,
            source_message_id: String::new(),
            turn_id: String::new(),
            created_at: String::new(),
            source_revision: None,
            source_digest: None,
        }
    }
}

impl PublicRoomEvent {
    pub fn from_event(event: &RoomEvent) -> Option<Self> {
        if event.kind != RoomEventKind::AssistantMessage || event.body.trim().is_empty() {
            return None;
        }
        Some(Self {
            schema_version: event.schema_version,
            room_id: event.room_id.clone(),
            room_seq: event.room_seq,
            event_id: event.event_id.clone(),
            kind: event.kind,
            author: event.author.clone(),
            body: event.body.clone(),
            recipient_id: event.recipient_id.clone(),
            source_message_id: event.source_message_id.clone(),
            turn_id: event.turn_id.clone(),
            created_at: event.created_at.clone(),
            source_revision: event.source_revision.clone(),
            source_digest: event.source_digest.clone(),
        })
    }
}
