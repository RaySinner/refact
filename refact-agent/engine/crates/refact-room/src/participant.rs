use std::collections::HashSet;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::{DeliveryAttempt, PublicRoomEvent, RoomAuthor, RoomError};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct RoomParticipant {
    pub participant_id: String,
    pub role: String,
    pub display_name: String,
    pub writes: bool,
}

impl RoomParticipant {
    pub fn author(&self) -> RoomAuthor {
        RoomAuthor {
            participant_id: self.participant_id.clone(),
            role: self.role.clone(),
            display_name: self.display_name.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct RoomRoster {
    pub room_id: String,
    pub participants: Vec<RoomParticipant>,
}

impl RoomRoster {
    pub fn validate(&self) -> Result<(), RoomError> {
        if self.room_id.trim().is_empty() || self.participants.is_empty() {
            return Err(RoomError::Validation(
                "room roster requires a room and participants".to_string(),
            ));
        }
        let mut ids = HashSet::new();
        for participant in &self.participants {
            if participant.participant_id.trim().is_empty()
                || !ids.insert(participant.participant_id.as_str())
            {
                return Err(RoomError::Validation(
                    "participant ids must be non-empty and unique".to_string(),
                ));
            }
        }
        if self
            .participants
            .iter()
            .filter(|member| member.writes)
            .count()
            != 1
        {
            return Err(RoomError::Validation(
                "room roster requires exactly one writer".to_string(),
            ));
        }
        Ok(())
    }
}

#[async_trait]
pub trait Participant: Send + Sync {
    fn participant(&self) -> &RoomParticipant;

    async fn deliver(
        &self,
        event: PublicRoomEvent,
        attempt: DeliveryAttempt,
    ) -> Result<(), RoomError>;
}
