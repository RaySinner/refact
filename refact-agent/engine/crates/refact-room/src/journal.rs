use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::event::validate_schema;
use crate::{
    PublicRoomEvent, RoomError, RoomEvent, RoomEventDraft, RoomEventKind, ROOM_EVENT_SCHEMA_VERSION,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppendOutcome {
    Appended(RoomEvent),
    Duplicate(RoomEvent),
}

impl AppendOutcome {
    pub fn event(&self) -> &RoomEvent {
        match self {
            Self::Appended(event) | Self::Duplicate(event) => event,
        }
    }

    pub fn was_appended(&self) -> bool {
        matches!(self, Self::Appended(_))
    }
}

#[derive(Debug, Clone, Default)]
pub struct RoomJournal {
    events: Vec<RoomEvent>,
}

impl RoomJournal {
    pub fn from_events(events: Vec<RoomEvent>) -> Result<Self, RoomError> {
        let mut next_sequences = HashMap::<&str, u64>::new();
        let mut ids = HashSet::<(&str, &str)>::new();
        for event in &events {
            event.validate()?;
            let expected = next_sequences
                .get(event.room_id.as_str())
                .copied()
                .unwrap_or(1);
            if event.room_seq != expected {
                return Err(RoomError::Validation(format!(
                    "room '{}' expected sequence {}, got {}",
                    event.room_id, expected, event.room_seq
                )));
            }
            if !ids.insert((&event.room_id, &event.event_id)) {
                return Err(RoomError::Conflict(format!(
                    "duplicate event id '{}' in room '{}'",
                    event.event_id, event.room_id
                )));
            }
            next_sequences.insert(
                &event.room_id,
                expected
                    .checked_add(1)
                    .ok_or_else(|| RoomError::Validation("room sequence overflow".to_string()))?,
            );
        }
        Ok(Self { events })
    }

    pub fn append(&mut self, draft: RoomEventDraft) -> Result<AppendOutcome, RoomError> {
        draft.validate()?;
        if let Some(existing) = self
            .events
            .iter()
            .find(|event| event.room_id == draft.room_id && event.event_id == draft.event_id)
        {
            return duplicate(existing, &draft);
        }
        if draft.kind == RoomEventKind::AssistantMessage && !draft.turn_id.is_empty() {
            if let Some(existing) = self.events.iter().find(|event| {
                event.room_id == draft.room_id
                    && event.kind == RoomEventKind::AssistantMessage
                    && event.turn_id == draft.turn_id
                    && event.author == draft.author
                    && event.body == draft.body
            }) {
                return Ok(AppendOutcome::Duplicate(existing.clone()));
            }
        }
        let last_seq = self
            .events
            .iter()
            .filter(|event| event.room_id == draft.room_id)
            .map(|event| event.room_seq)
            .max()
            .unwrap_or(0);
        let room_seq = last_seq
            .checked_add(1)
            .ok_or_else(|| RoomError::Validation("room sequence overflow".to_string()))?;
        let event = RoomEvent::from_draft(draft, room_seq);
        self.events.push(event.clone());
        Ok(AppendOutcome::Appended(event))
    }

    pub fn events(&self) -> &[RoomEvent] {
        &self.events
    }

    pub fn replay(&self, room_id: &str, limit: usize) -> Vec<RoomEvent> {
        let matching = self
            .events
            .iter()
            .filter(|event| event.room_id == room_id)
            .collect::<Vec<_>>();
        matching[matching.len().saturating_sub(limit)..]
            .iter()
            .map(|event| (*event).clone())
            .collect()
    }

    pub fn public_replay(&self, room_id: &str, limit: usize) -> Vec<PublicRoomEvent> {
        let matching = self
            .events
            .iter()
            .filter(|event| event.room_id == room_id)
            .filter_map(PublicRoomEvent::from_event)
            .collect::<Vec<_>>();
        matching[matching.len().saturating_sub(limit)..].to_vec()
    }
}

fn duplicate(existing: &RoomEvent, draft: &RoomEventDraft) -> Result<AppendOutcome, RoomError> {
    if existing.to_draft() == *draft {
        return Ok(AppendOutcome::Duplicate(existing.clone()));
    }
    Err(RoomError::Conflict(format!(
        "event id '{}' already exists with a different payload",
        draft.event_id
    )))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RoomJournalSnapshot {
    pub schema_version: u32,
    pub revision: u64,
    pub events: Vec<RoomEvent>,
}

impl Default for RoomJournalSnapshot {
    fn default() -> Self {
        Self {
            schema_version: ROOM_EVENT_SCHEMA_VERSION,
            revision: 0,
            events: Vec::new(),
        }
    }
}

impl RoomJournalSnapshot {
    pub fn validate(&self) -> Result<(), RoomError> {
        validate_schema(self.schema_version)?;
        RoomJournal::from_events(self.events.clone())?;
        Ok(())
    }
}

#[async_trait]
pub trait RoomJournalStore: Send + Sync {
    async fn load(&self, room_id: &str) -> Result<RoomJournalSnapshot, RoomError>;

    async fn compare_and_swap(
        &self,
        room_id: &str,
        expected_revision: u64,
        next: &RoomJournalSnapshot,
    ) -> Result<bool, RoomError>;
}

pub struct DurableRoomJournal<S> {
    store: Arc<S>,
    room_id: String,
}

impl<S> Clone for DurableRoomJournal<S> {
    fn clone(&self) -> Self {
        Self {
            store: self.store.clone(),
            room_id: self.room_id.clone(),
        }
    }
}

impl<S: RoomJournalStore + 'static> DurableRoomJournal<S> {
    pub fn new(store: Arc<S>, room_id: impl Into<String>) -> Self {
        Self {
            store,
            room_id: room_id.into(),
        }
    }

    pub async fn append(&self, draft: RoomEventDraft) -> Result<AppendOutcome, RoomError> {
        if draft.room_id != self.room_id {
            return Err(RoomError::Validation(format!(
                "event room '{}' does not match journal room '{}'",
                draft.room_id, self.room_id
            )));
        }
        loop {
            let snapshot = self.store.load(&self.room_id).await?;
            snapshot.validate()?;
            if snapshot
                .events
                .iter()
                .any(|event| event.room_id != self.room_id)
            {
                return Err(RoomError::Validation(
                    "persisted snapshot contains another room".to_string(),
                ));
            }
            let mut journal = RoomJournal::from_events(snapshot.events)?;
            let outcome = journal.append(draft.clone())?;
            if !outcome.was_appended() {
                return Ok(outcome);
            }
            let next = RoomJournalSnapshot {
                schema_version: ROOM_EVENT_SCHEMA_VERSION,
                revision: snapshot.revision.checked_add(1).ok_or_else(|| {
                    RoomError::Validation("journal revision overflow".to_string())
                })?,
                events: journal.events,
            };
            if self
                .store
                .compare_and_swap(&self.room_id, snapshot.revision, &next)
                .await?
            {
                return Ok(outcome);
            }
        }
    }

    pub async fn events(&self) -> Result<Vec<RoomEvent>, RoomError> {
        let snapshot = self.store.load(&self.room_id).await?;
        snapshot.validate()?;
        Ok(snapshot.events)
    }

    pub async fn replay(&self, limit: usize) -> Result<Vec<RoomEvent>, RoomError> {
        let snapshot = self.store.load(&self.room_id).await?;
        snapshot.validate()?;
        Ok(RoomJournal::from_events(snapshot.events)?.replay(&self.room_id, limit))
    }

    pub async fn public_replay(&self, limit: usize) -> Result<Vec<PublicRoomEvent>, RoomError> {
        let snapshot = self.store.load(&self.room_id).await?;
        snapshot.validate()?;
        Ok(RoomJournal::from_events(snapshot.events)?.public_replay(&self.room_id, limit))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use async_trait::async_trait;
    use tokio::sync::Mutex;

    use super::*;
    use crate::{
        project_assistant_message, DeliveryAttempt, PublicProjectionInput, RoomAuthor,
        RoomParticipant, RoomRoster,
    };

    #[derive(Default)]
    struct MemoryStore {
        snapshots: Mutex<HashMap<String, RoomJournalSnapshot>>,
    }

    #[async_trait]
    impl RoomJournalStore for MemoryStore {
        async fn load(&self, room_id: &str) -> Result<RoomJournalSnapshot, RoomError> {
            Ok(self
                .snapshots
                .lock()
                .await
                .get(room_id)
                .cloned()
                .unwrap_or_default())
        }

        async fn compare_and_swap(
            &self,
            room_id: &str,
            expected_revision: u64,
            next: &RoomJournalSnapshot,
        ) -> Result<bool, RoomError> {
            next.validate()?;
            let mut snapshots = self.snapshots.lock().await;
            let current = snapshots.get(room_id).cloned().unwrap_or_default();
            if current.revision != expected_revision || next.revision != expected_revision + 1 {
                return Ok(false);
            }
            snapshots.insert(room_id.to_string(), next.clone());
            Ok(true)
        }
    }

    fn author() -> RoomAuthor {
        RoomAuthor {
            participant_id: "writer-chat".to_string(),
            role: "coder".to_string(),
            display_name: "Coder".to_string(),
        }
    }

    fn draft(id: &str, body: &str) -> RoomEventDraft {
        RoomEventDraft {
            room_id: "room-1".to_string(),
            event_id: id.to_string(),
            author: author(),
            body: body.to_string(),
            source_message_id: format!("message-{id}"),
            turn_id: format!("turn-{id}"),
            created_at: "2026-11-24T00:00:00Z".to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn serde_defaults_and_event_roundtrip() {
        let legacy: RoomEventDraft = serde_json::from_str("{}").unwrap();
        assert_eq!(legacy.schema_version, ROOM_EVENT_SCHEMA_VERSION);
        assert_eq!(legacy.kind, RoomEventKind::AssistantMessage);
        assert!(legacy.source_revision.is_none());

        let event = RoomEvent::from_draft(draft("event-1", "Привет, комната 🌞"), 1);
        let encoded = serde_json::to_string(&event).unwrap();
        let decoded: RoomEvent = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, event);
        assert_eq!(decoded.body, "Привет, комната 🌞");
    }

    #[test]
    fn in_memory_sequences_are_monotonic_and_deduplicated() {
        let mut journal = RoomJournal::default();
        assert_eq!(
            journal
                .append(draft("event-1", "one"))
                .unwrap()
                .event()
                .room_seq,
            1
        );
        assert_eq!(
            journal
                .append(draft("event-2", "two"))
                .unwrap()
                .event()
                .room_seq,
            2
        );
        let duplicate = journal.append(draft("event-1", "one")).unwrap();
        assert!(!duplicate.was_appended());
        assert_eq!(duplicate.event().room_seq, 1);
        assert!(journal.append(draft("event-1", "changed")).is_err());
        assert_eq!(journal.events().len(), 2);
    }

    #[test]
    fn bounded_replay_does_not_truncate_journal_and_hides_non_public_records() {
        let mut journal = RoomJournal::default();
        for index in 0..5 {
            journal
                .append(draft(&format!("event-{index}"), &format!("body-{index}")))
                .unwrap();
        }
        let mut membership = draft("membership-1", "private roster details");
        membership.kind = RoomEventKind::Membership;
        journal.append(membership).unwrap();

        let replay = journal.replay("room-1", 2);
        assert_eq!(replay.len(), 2);
        assert_eq!(replay[0].event_id, "event-4");
        assert_eq!(replay[1].event_id, "membership-1");
        assert_eq!(journal.events().len(), 6);

        let public = journal.public_replay("room-1", 2);
        assert_eq!(public.len(), 2);
        assert_eq!(public[0].body, "body-3");
        assert_eq!(public[1].body, "body-4");
        assert!(journal.public_replay("room-1", 0).is_empty());
    }

    #[test]
    fn public_projection_accepts_only_completed_non_empty_assistant_text() {
        let input = PublicProjectionInput {
            room_id: "room-1".to_string(),
            event_id: "event-1".to_string(),
            role: "assistant".to_string(),
            completed: true,
            body: "safe text 🌟".to_string(),
            author: author(),
            source_message_id: "message-1".to_string(),
            turn_id: "turn-1".to_string(),
            created_at: "now".to_string(),
            source_revision: Some("revision".to_string()),
            source_digest: Some("digest".to_string()),
        };
        let draft = project_assistant_message(input.clone()).unwrap();
        assert_eq!(draft.body, "safe text 🌟");
        assert_eq!(draft.source_revision.as_deref(), Some("revision"));

        for invalid in [
            PublicProjectionInput {
                role: "tool".to_string(),
                ..input.clone()
            },
            PublicProjectionInput {
                role: "diff".to_string(),
                ..input.clone()
            },
            PublicProjectionInput {
                role: "context_file".to_string(),
                ..input.clone()
            },
            PublicProjectionInput {
                completed: false,
                ..input.clone()
            },
            PublicProjectionInput {
                body: " \n\t".to_string(),
                ..input.clone()
            },
        ] {
            assert!(project_assistant_message(invalid).is_none());
        }

        let event = RoomEvent::from_draft(draft, 1);
        let public = PublicRoomEvent::from_event(&event).unwrap();
        let json = serde_json::to_string(&public).unwrap();
        for forbidden in [
            "reasoning",
            "thinking",
            "tool_calls",
            "tool_output",
            "context_files",
            "diff",
        ] {
            assert!(!json.contains(forbidden));
        }
        let mut private = event;
        private.kind = RoomEventKind::Delivery;
        assert!(PublicRoomEvent::from_event(&private).is_none());
    }

    #[test]
    fn same_body_in_one_turn_is_deduplicated() {
        let mut journal = RoomJournal::default();
        let first = draft("event-1", "same body");
        journal.append(first.clone()).unwrap();
        let second = RoomEventDraft {
            event_id: "event-2".to_string(),
            ..first
        };
        let duplicate = journal.append(second).unwrap();
        assert!(!duplicate.was_appended());
        assert_eq!(duplicate.event().event_id, "event-1");
    }

    #[test]
    fn delivery_keys_are_deterministic_per_recipient_and_attempt() {
        let first = DeliveryAttempt::new("room-1", "event-1", "recipient-1");
        assert_eq!(first, first.transport_retry());
        assert_eq!(first.logical_attempt, 0);
        assert!(first.previous_delivery_id.is_none());
        assert_ne!(
            first,
            DeliveryAttempt::new("room-1", "event-1", "recipient-2")
        );

        let linked = first.linked_attempt();
        assert_eq!(linked.logical_attempt, 1);
        assert_ne!(linked.delivery_id, first.delivery_id);
        assert_eq!(
            linked.previous_delivery_id.as_deref(),
            Some(first.delivery_id.as_str())
        );
        assert_eq!(linked, linked.transport_retry());
    }

    #[test]
    fn roster_requires_unique_participants_and_exactly_one_writer() {
        let writer = RoomParticipant {
            participant_id: "writer".to_string(),
            writes: true,
            ..Default::default()
        };
        let advisor = RoomParticipant {
            participant_id: "advisor".to_string(),
            writes: false,
            ..Default::default()
        };
        let valid = RoomRoster {
            room_id: "room-1".to_string(),
            participants: vec![writer.clone(), advisor.clone()],
        };
        valid.validate().unwrap();
        assert!(RoomRoster {
            participants: vec![writer.clone(), writer.clone()],
            ..valid.clone()
        }
        .validate()
        .is_err());
        assert!(RoomRoster {
            participants: vec![writer, advisor.clone()],
            ..valid.clone()
        }
        .validate()
        .is_ok());
        assert!(RoomRoster {
            participants: vec![advisor.clone(), advisor],
            ..valid
        }
        .validate()
        .is_err());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_and_reloaded_appends_preserve_all_events() {
        let store = Arc::new(MemoryStore::default());
        let journal = DurableRoomJournal::new(store.clone(), "room-1");
        let mut tasks = Vec::new();
        for index in 0..16 {
            let journal = journal.clone();
            tasks.push(tokio::spawn(async move {
                let id = format!("event-{index}");
                journal
                    .append(draft(&id, &format!("body-{index}")))
                    .await
                    .unwrap();
                journal
                    .append(draft(&id, &format!("body-{index}")))
                    .await
                    .unwrap();
            }));
        }
        for task in tasks {
            task.await.unwrap();
        }

        drop(journal);
        let reloaded = DurableRoomJournal::new(store, "room-1");
        let events = reloaded.events().await.unwrap();
        assert_eq!(events.len(), 16);
        assert_eq!(
            events
                .iter()
                .map(|event| event.room_seq)
                .collect::<Vec<_>>(),
            (1..=16).collect::<Vec<_>>()
        );
        let old = events.clone();
        reloaded
            .append(draft("event-16", "after reload"))
            .await
            .unwrap();
        let after = reloaded.events().await.unwrap();
        assert_eq!(&after[..16], old.as_slice());
        assert_eq!(after[16].room_seq, 17);
    }

    #[test]
    fn standalone_manifest_has_no_engine_or_transport_dependencies() {
        let manifest =
            std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml")).unwrap();
        for forbidden in [
            "refact-core",
            "refact-runtime-api",
            "refact-tasks",
            "refact-uhp",
            "reqwest",
            "rusqlite",
            "GlobalContext",
        ] {
            assert!(!manifest.contains(forbidden));
        }
    }
}
