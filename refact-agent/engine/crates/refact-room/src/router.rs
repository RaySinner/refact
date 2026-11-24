use crate::{DeliveryAttempt, PublicRoomEvent, RoomError, RoomEvent, RoomEventKind, RoomParticipant};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoomDelivery {
    pub recipient: RoomParticipant,
    pub event: PublicRoomEvent,
    pub attempt: DeliveryAttempt,
}

pub fn route_event(
    event: &RoomEvent,
    participants: &[RoomParticipant],
) -> Result<Vec<RoomDelivery>, RoomError> {
    if event.kind != RoomEventKind::AssistantMessage {
        return Ok(Vec::new());
    }
    let recipients = recipients_for(event, participants)?;
    let public = PublicRoomEvent::from_event(event)
        .ok_or_else(|| RoomError::Validation("event has no public projection".to_string()))?;
    Ok(recipients
        .into_iter()
        .map(|recipient| RoomDelivery {
            attempt: DeliveryAttempt::new(
                event.room_id.clone(),
                event.event_id.clone(),
                recipient.participant_id.clone(),
            ),
            recipient: (*recipient).clone(),
            event: public.clone(),
        })
        .collect())
}

pub fn replay_deliveries(
    events: &[RoomEvent],
    participant: &RoomParticipant,
    limit: usize,
) -> Vec<RoomDelivery> {
    let matching = events
        .iter()
        .filter(|event| {
            event.kind == RoomEventKind::AssistantMessage
                && event.author.participant_id != participant.participant_id
                && event
                    .recipient_id
                    .as_deref()
                    .is_none_or(|recipient| recipient == participant.participant_id)
        })
        .collect::<Vec<_>>();
    matching[matching.len().saturating_sub(limit)..]
        .iter()
        .filter_map(|event| {
            Some(RoomDelivery {
                recipient: participant.clone(),
                event: PublicRoomEvent::from_event(event)?,
                attempt: DeliveryAttempt::new(
                    event.room_id.clone(),
                    event.event_id.clone(),
                    participant.participant_id.clone(),
                ),
            })
        })
        .collect()
}

fn recipients_for<'a>(
    event: &RoomEvent,
    participants: &'a [RoomParticipant],
) -> Result<Vec<&'a RoomParticipant>, RoomError> {
    let eligible = participants
        .iter()
        .filter(|participant| participant.participant_id != event.author.participant_id)
        .collect::<Vec<_>>();
    let Some(target) = event.recipient_id.as_deref() else {
        return Ok(eligible);
    };
    let matches = eligible
        .iter()
        .filter(|participant| {
            participant.participant_id == target
                || participant
                    .display_name
                    .trim()
                    .eq_ignore_ascii_case(target.trim())
                || participant.role.trim().eq_ignore_ascii_case(target.trim())
        })
        .copied()
        .collect::<Vec<_>>();
    if matches.len() == 1 {
        Ok(matches)
    } else if matches.is_empty() {
        Err(RoomError::Validation(format!(
            "room recipient '{target}' is unavailable"
        )))
    } else {
        Err(RoomError::Validation(format!(
            "room recipient '{target}' is ambiguous"
        )))
    }
}

#[cfg(test)]
mod tests {
    use crate::{RoomAuthor, RoomEventDraft};

    use super::*;

    fn participant(id: &str, role: &str) -> RoomParticipant {
        RoomParticipant {
            participant_id: id.to_string(),
            role: role.to_string(),
            display_name: format!("{role} display"),
            writes: role == "coder",
        }
    }

    fn event(id: &str, recipient_id: Option<&str>) -> RoomEvent {
        RoomEvent::from_draft(
            RoomEventDraft {
                room_id: "room-1".to_string(),
                event_id: id.to_string(),
                author: RoomAuthor {
                    participant_id: "writer".to_string(),
                    role: "coder".to_string(),
                    display_name: "Coder".to_string(),
                },
                body: format!("body-{id}"),
                recipient_id: recipient_id.map(str::to_string),
                ..Default::default()
            },
            id.trim_start_matches("event-").parse().unwrap(),
        )
    }

    #[test]
    fn broadcast_and_directed_fanout_exclude_the_author() {
        let participants = vec![
            participant("writer", "coder"),
            participant("reviewer-1", "reviewer"),
            participant("reviewer-2", "reviewer"),
        ];
        let broadcast = route_event(&event("event-1", None), &participants).unwrap();
        assert_eq!(broadcast.len(), 2);
        assert!(broadcast
            .iter()
            .all(|delivery| delivery.recipient.participant_id != "writer"));
        let directed = route_event(&event("event-2", Some("reviewer display")), &participants);
        assert!(directed.is_err());
        let directed = route_event(&event("event-2", Some("reviewer-2")), &participants).unwrap();
        assert_eq!(directed.len(), 1);
        assert_eq!(directed[0].recipient.participant_id, "reviewer-2");
        assert_eq!(directed[0].attempt.logical_attempt, 0);
    }

    #[test]
    fn replay_is_bounded_ordered_and_audience_filtered() {
        let reviewer = participant("reviewer-1", "reviewer");
        let events = vec![
            event("event-1", None),
            event("event-2", Some("reviewer-2")),
            event("event-3", Some("reviewer-1")),
            event("event-4", None),
        ];
        let replay = replay_deliveries(&events, &reviewer, 2);
        assert_eq!(replay.len(), 2);
        assert_eq!(replay[0].event.event_id, "event-3");
        assert_eq!(replay[1].event.event_id, "event-4");
    }

    #[test]
    fn retries_keep_identity_and_linked_attempts_keep_audit_relation() {
        let delivery = route_event(
            &event("event-1", None),
            &[
                participant("writer", "coder"),
                participant("peer", "reviewer"),
            ],
        )
        .unwrap()
        .remove(0);
        assert_eq!(delivery.attempt.transport_retry(), delivery.attempt);
        let linked = delivery.attempt.linked_attempt();
        assert_eq!(linked.logical_attempt, 1);
        assert_eq!(
            linked.previous_delivery_id,
            Some(delivery.attempt.delivery_id)
        );
        assert_ne!(linked.idempotency_key, delivery.attempt.idempotency_key);
    }
}
