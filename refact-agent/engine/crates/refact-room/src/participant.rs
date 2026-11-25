use std::collections::HashSet;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::roster::validate_complete_roster;
use crate::{DeliveryAttempt, PublicRoomEvent, RoomAuthor, RoomError, RosterMember};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct RoomParticipant {
    pub participant_id: String,
    pub role: String,
    pub display_name: String,
    pub writes: bool,
    pub external_participant_id: Option<String>,
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
        let members = self
            .participants
            .iter()
            .map(|member| RosterMember {
                role: &member.role,
                writes: member.writes,
                claimed: true,
            })
            .collect::<Vec<_>>();
        validate_complete_roster(&members)
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParticipantRecoveryAdvice {
    RecoverResponse { response_id: Option<String> },
    WaitForTerminalAndRetry,
    SessionExpired,
    NoAutomaticRetry,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParticipantRecoveryAction {
    RecoverResponse { response_id: Option<String> },
    Wait,
    StartLinkedAttempt,
    Fail,
}

pub fn participant_recovery_action(
    attempt: &DeliveryAttempt,
    advice: &ParticipantRecoveryAdvice,
) -> ParticipantRecoveryAction {
    match advice {
        ParticipantRecoveryAdvice::RecoverResponse { response_id } => {
            ParticipantRecoveryAction::RecoverResponse {
                response_id: response_id.clone(),
            }
        }
        ParticipantRecoveryAdvice::WaitForTerminalAndRetry => ParticipantRecoveryAction::Wait,
        ParticipantRecoveryAdvice::SessionExpired if attempt.logical_attempt == 0 => {
            ParticipantRecoveryAction::StartLinkedAttempt
        }
        ParticipantRecoveryAdvice::SessionExpired | ParticipantRecoveryAdvice::NoAutomaticRetry => {
            ParticipantRecoveryAction::Fail
        }
    }
}

#[cfg(test)]
mod recovery_tests {
    use super::*;

    #[test]
    fn session_expiry_allows_only_one_linked_attempt() {
        let attempt = DeliveryAttempt::new("room-1", "event-1", "external-1");
        assert_eq!(
            participant_recovery_action(&attempt, &ParticipantRecoveryAdvice::SessionExpired),
            ParticipantRecoveryAction::StartLinkedAttempt
        );
        assert_eq!(
            participant_recovery_action(
                &attempt.linked_attempt(),
                &ParticipantRecoveryAdvice::SessionExpired,
            ),
            ParticipantRecoveryAction::Fail
        );
    }
}
