use serde::{Deserialize, Serialize};

use crate::RoomError;

pub const ROOM_EVENT_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RoomEventKind {
    #[default]
    AssistantMessage,
    Membership,
    Delivery,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct RoomAuthor {
    pub participant_id: String,
    pub role: String,
    pub display_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RoomEventDraft {
    pub schema_version: u32,
    pub room_id: String,
    pub event_id: String,
    pub kind: RoomEventKind,
    pub author: RoomAuthor,
    pub body: String,
    pub source_message_id: String,
    pub turn_id: String,
    pub created_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_revision: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_digest: Option<String>,
}

impl Default for RoomEventDraft {
    fn default() -> Self {
        Self {
            schema_version: ROOM_EVENT_SCHEMA_VERSION,
            room_id: String::new(),
            event_id: String::new(),
            kind: RoomEventKind::AssistantMessage,
            author: RoomAuthor::default(),
            body: String::new(),
            source_message_id: String::new(),
            turn_id: String::new(),
            created_at: String::new(),
            source_revision: None,
            source_digest: None,
        }
    }
}

impl RoomEventDraft {
    pub fn validate(&self) -> Result<(), RoomError> {
        validate_schema(self.schema_version)?;
        require("room_id", &self.room_id)?;
        require("event_id", &self.event_id)?;
        require("author.participant_id", &self.author.participant_id)?;
        if self.kind == RoomEventKind::AssistantMessage && self.body.trim().is_empty() {
            return Err(RoomError::Validation(
                "assistant_message body must not be empty".to_string(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RoomEvent {
    pub schema_version: u32,
    pub room_id: String,
    pub room_seq: u64,
    pub event_id: String,
    pub kind: RoomEventKind,
    pub author: RoomAuthor,
    pub body: String,
    pub source_message_id: String,
    pub turn_id: String,
    pub created_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_revision: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_digest: Option<String>,
}

impl Default for RoomEvent {
    fn default() -> Self {
        Self {
            schema_version: ROOM_EVENT_SCHEMA_VERSION,
            room_id: String::new(),
            room_seq: 0,
            event_id: String::new(),
            kind: RoomEventKind::AssistantMessage,
            author: RoomAuthor::default(),
            body: String::new(),
            source_message_id: String::new(),
            turn_id: String::new(),
            created_at: String::new(),
            source_revision: None,
            source_digest: None,
        }
    }
}

impl RoomEvent {
    pub fn from_draft(draft: RoomEventDraft, room_seq: u64) -> Self {
        Self {
            schema_version: draft.schema_version,
            room_id: draft.room_id,
            room_seq,
            event_id: draft.event_id,
            kind: draft.kind,
            author: draft.author,
            body: draft.body,
            source_message_id: draft.source_message_id,
            turn_id: draft.turn_id,
            created_at: draft.created_at,
            source_revision: draft.source_revision,
            source_digest: draft.source_digest,
        }
    }

    pub fn to_draft(&self) -> RoomEventDraft {
        RoomEventDraft {
            schema_version: self.schema_version,
            room_id: self.room_id.clone(),
            event_id: self.event_id.clone(),
            kind: self.kind,
            author: self.author.clone(),
            body: self.body.clone(),
            source_message_id: self.source_message_id.clone(),
            turn_id: self.turn_id.clone(),
            created_at: self.created_at.clone(),
            source_revision: self.source_revision.clone(),
            source_digest: self.source_digest.clone(),
        }
    }

    pub fn validate(&self) -> Result<(), RoomError> {
        self.to_draft().validate()?;
        if self.room_seq == 0 {
            return Err(RoomError::Validation(
                "room_seq must be greater than zero".to_string(),
            ));
        }
        Ok(())
    }
}

pub(crate) fn validate_schema(schema_version: u32) -> Result<(), RoomError> {
    if schema_version != ROOM_EVENT_SCHEMA_VERSION {
        return Err(RoomError::UnsupportedSchema(schema_version));
    }
    Ok(())
}

fn require(field: &str, value: &str) -> Result<(), RoomError> {
    if value.trim().is_empty() {
        return Err(RoomError::Validation(format!("{field} must not be empty")));
    }
    Ok(())
}
