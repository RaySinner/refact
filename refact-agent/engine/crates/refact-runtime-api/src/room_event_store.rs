use std::io::Write;
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use refact_chat_api::{ChatMessage, MessageProvenance};
use refact_core::chat_types::ChatContent;
use refact_core::file_lock;
use refact_room::{
    project_assistant_message, PublicProjectionInput, RoomAuthor, RoomError, RoomEventDraft,
    RoomJournalSnapshot, RoomJournalStore,
};
use sha2::{Digest, Sha256};
use tempfile::NamedTempFile;

#[derive(Debug, Clone)]
pub struct JsonRoomEventStore {
    directory: PathBuf,
}

impl JsonRoomEventStore {
    pub fn new(directory: impl Into<PathBuf>) -> Self {
        Self {
            directory: directory.into(),
        }
    }

    fn journal_path(&self, room_id: &str) -> PathBuf {
        let digest = hex::encode(Sha256::digest(room_id.as_bytes()));
        self.directory.join(format!("room-{digest}.json"))
    }

    fn lock_path(&self, room_id: &str) -> PathBuf {
        let digest = hex::encode(Sha256::digest(room_id.as_bytes()));
        self.directory.join(format!(".room-{digest}.lock"))
    }
}

#[async_trait]
impl RoomJournalStore for JsonRoomEventStore {
    async fn load(&self, room_id: &str) -> Result<RoomJournalSnapshot, RoomError> {
        let path = self.journal_path(room_id);
        let lock_path = self.lock_path(room_id);
        tokio::task::spawn_blocking(move || read_locked(&path, &lock_path))
            .await
            .map_err(|error| RoomError::Store(format!("journal load task failed: {error}")))?
    }

    async fn compare_and_swap(
        &self,
        room_id: &str,
        expected_revision: u64,
        next: &RoomJournalSnapshot,
    ) -> Result<bool, RoomError> {
        next.validate()?;
        if next.events.iter().any(|event| event.room_id != room_id) {
            return Err(RoomError::Validation(
                "persisted snapshot contains another room".to_string(),
            ));
        }
        let path = self.journal_path(room_id);
        let lock_path = self.lock_path(room_id);
        let next = next.clone();
        tokio::task::spawn_blocking(move || {
            let mut lock = file_lock::open_lock(&lock_path).map_err(|error| {
                RoomError::Store(format!("failed to open journal lock: {error}"))
            })?;
            let _guard = file_lock::lock_blocking(&mut lock)
                .map_err(|error| RoomError::Store(format!("failed to lock journal: {error}")))?;
            let current = read_unlocked(&path)?;
            let expected_next = expected_revision
                .checked_add(1)
                .ok_or_else(|| RoomError::Validation("journal revision overflow".to_string()))?;
            if current.revision != expected_revision || next.revision != expected_next {
                return Ok(false);
            }
            write_atomic(&path, &next)?;
            Ok(true)
        })
        .await
        .map_err(|error| RoomError::Store(format!("journal write task failed: {error}")))?
    }
}

fn read_locked(path: &Path, lock_path: &Path) -> Result<RoomJournalSnapshot, RoomError> {
    let mut lock = file_lock::open_lock(lock_path)
        .map_err(|error| RoomError::Store(format!("failed to open journal lock: {error}")))?;
    let _guard = file_lock::lock_blocking(&mut lock)
        .map_err(|error| RoomError::Store(format!("failed to lock journal: {error}")))?;
    read_unlocked(path)
}

fn read_unlocked(path: &Path) -> Result<RoomJournalSnapshot, RoomError> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(RoomJournalSnapshot::default());
        }
        Err(error) => {
            return Err(RoomError::Store(format!(
                "failed to read {}: {error}",
                path.display()
            )));
        }
    };
    let snapshot: RoomJournalSnapshot = serde_json::from_slice(&bytes).map_err(|error| {
        RoomError::Store(format!("failed to parse {}: {error}", path.display()))
    })?;
    snapshot.validate()?;
    Ok(snapshot)
}

fn write_atomic(path: &Path, snapshot: &RoomJournalSnapshot) -> Result<(), RoomError> {
    let parent = path
        .parent()
        .ok_or_else(|| RoomError::Store("journal path has no parent".to_string()))?;
    std::fs::create_dir_all(parent).map_err(|error| {
        RoomError::Store(format!("failed to create journal directory: {error}"))
    })?;
    let encoded = serde_json::to_vec(snapshot)
        .map_err(|error| RoomError::Store(format!("failed to encode journal: {error}")))?;
    let mut temporary = NamedTempFile::new_in(parent).map_err(|error| {
        RoomError::Store(format!("failed to create journal temporary file: {error}"))
    })?;
    temporary
        .write_all(&encoded)
        .and_then(|_| temporary.as_file().sync_all())
        .map_err(|error| RoomError::Store(format!("failed to write journal: {error}")))?;
    temporary
        .persist(path)
        .map_err(|error| RoomError::Store(format!("failed to replace journal: {error}")))?;
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatMessageRoomContext {
    pub room_id: String,
    pub event_id: String,
    pub turn_id: String,
    pub created_at: String,
    pub completed: bool,
    pub source_revision: Option<String>,
    pub source_digest: Option<String>,
}

pub fn project_chat_message(
    message: &ChatMessage,
    provenance: &MessageProvenance,
    context: ChatMessageRoomContext,
) -> Option<RoomEventDraft> {
    let body = match &message.content {
        ChatContent::SimpleText(text) => text.clone(),
        ChatContent::Multimodal(elements) => elements
            .iter()
            .filter(|element| element.m_type == "text")
            .map(|element| element.m_content.clone())
            .collect::<Vec<_>>()
            .join("\n\n"),
        ChatContent::ContextFiles(_) => return None,
    };
    project_assistant_message(PublicProjectionInput {
        room_id: context.room_id,
        event_id: context.event_id,
        role: message.role.clone(),
        completed: context.completed,
        body,
        author: RoomAuthor {
            participant_id: provenance.chat_id.clone(),
            role: provenance.role.clone(),
            display_name: provenance.display_name.clone(),
        },
        source_message_id: message.message_id.clone(),
        turn_id: context.turn_id,
        created_at: context.created_at,
        source_revision: context.source_revision,
        source_digest: context.source_digest,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use refact_core::chat_types::MultimodalElement;
    use refact_room::{DurableRoomJournal, RoomAuthor, RoomEvent};

    use super::*;

    fn provenance() -> MessageProvenance {
        MessageProvenance {
            chat_id: "writer-chat".to_string(),
            role: "coder".to_string(),
            display_name: "Coder".to_string(),
            card_id: Some("card-1".to_string()),
        }
    }

    fn context(event_id: &str) -> ChatMessageRoomContext {
        ChatMessageRoomContext {
            room_id: "room-1".to_string(),
            event_id: event_id.to_string(),
            turn_id: format!("turn-{event_id}"),
            created_at: "2026-11-24T00:00:00Z".to_string(),
            completed: true,
            source_revision: Some("revision-1".to_string()),
            source_digest: Some("digest-1".to_string()),
        }
    }

    fn draft(event_id: &str, body: &str) -> RoomEventDraft {
        RoomEventDraft {
            room_id: "room-1".to_string(),
            event_id: event_id.to_string(),
            author: RoomAuthor {
                participant_id: "writer-chat".to_string(),
                role: "coder".to_string(),
                display_name: "Coder".to_string(),
            },
            body: body.to_string(),
            source_message_id: format!("message-{event_id}"),
            turn_id: format!("turn-{event_id}"),
            created_at: "2026-11-24T00:00:00Z".to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn chat_mapping_keeps_only_public_assistant_text_and_attribution() {
        let mut message = ChatMessage::new("assistant".to_string(), "public answer 🌞".to_string());
        message.message_id = "message-1".to_string();
        message.reasoning_content = Some("private reasoning".to_string());
        message.thinking_blocks = Some(vec![serde_json::json!({"thinking": "secret thought"})]);
        message.extra.insert(
            "private_tool_output".to_string(),
            serde_json::json!({"secret": "raw result"}),
        );

        let draft = project_chat_message(&message, &provenance(), context("event-1")).unwrap();
        assert_eq!(draft.body, "public answer 🌞");
        assert_eq!(draft.author.participant_id, "writer-chat");
        assert_eq!(draft.author.display_name, "Coder");
        assert_eq!(draft.source_message_id, "message-1");
        let encoded = serde_json::to_string(&draft).unwrap();
        for forbidden in [
            "private reasoning",
            "secret thought",
            "raw result",
            "reasoning_content",
            "thinking_blocks",
            "tool_calls",
            "private_tool_output",
        ] {
            assert!(!encoded.contains(forbidden));
        }

        for role in ["tool", "diff", "context_file", "user"] {
            let message = ChatMessage::new(role.to_string(), "private payload".to_string());
            assert!(project_chat_message(&message, &provenance(), context("event-2")).is_none());
        }
        let mut incomplete = context("event-3");
        incomplete.completed = false;
        assert!(project_chat_message(&message, &provenance(), incomplete).is_none());
        let empty = ChatMessage::new("assistant".to_string(), " \n".to_string());
        assert!(project_chat_message(&empty, &provenance(), context("event-4")).is_none());
        let mut context_files = ChatMessage::new("assistant".to_string(), String::new());
        context_files.content = ChatContent::ContextFiles(vec![refact_chat_api::ContextFile {
            file_name: "private.rs".to_string(),
            file_content: "private source".to_string(),
            line1: 1,
            line2: 1,
            file_rev: None,
            symbols: Vec::new(),
            gradient_type: -1,
            usefulness: 0.0,
            skip_pp: false,
        }]);
        assert!(project_chat_message(&context_files, &provenance(), context("event-5")).is_none());
    }

    #[test]
    fn chat_mapping_excludes_non_text_multimodal_data() {
        let mut message = ChatMessage::new("assistant".to_string(), String::new());
        message.content = ChatContent::Multimodal(vec![
            MultimodalElement {
                m_type: "text".to_string(),
                m_content: "visible text".to_string(),
            },
            MultimodalElement {
                m_type: "image".to_string(),
                m_content: "private image bytes".to_string(),
            },
        ]);
        let draft = project_chat_message(&message, &provenance(), context("event-1")).unwrap();
        assert_eq!(draft.body, "visible text");
        assert!(!serde_json::to_string(&draft)
            .unwrap()
            .contains("private image bytes"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn file_store_serializes_concurrent_handles_and_reloads_without_duplicates() {
        let temp = tempfile::tempdir().unwrap();
        let mut tasks = Vec::new();
        for index in 0..12 {
            let store = Arc::new(JsonRoomEventStore::new(temp.path()));
            tasks.push(tokio::spawn(async move {
                let journal = DurableRoomJournal::new(store, "room-1");
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

        let reloaded =
            DurableRoomJournal::new(Arc::new(JsonRoomEventStore::new(temp.path())), "room-1");
        let events = reloaded.events().await.unwrap();
        assert_eq!(events.len(), 12);
        assert_eq!(
            events
                .iter()
                .map(|event| event.room_seq)
                .collect::<Vec<_>>(),
            (1..=12).collect::<Vec<_>>()
        );
        let replay = reloaded.public_replay(3).await.unwrap();
        assert_eq!(replay.len(), 3);
        assert_eq!(reloaded.events().await.unwrap().len(), 12);
    }

    #[tokio::test]
    async fn legacy_snapshot_defaults_are_non_destructive() {
        let temp = tempfile::tempdir().unwrap();
        let store = JsonRoomEventStore::new(temp.path());
        let old = refact_room::RoomEvent::from_draft(draft("event-old", "old body"), 1);
        let mut old_json = serde_json::to_value(&old).unwrap();
        old_json.as_object_mut().unwrap().remove("schema_version");
        old_json.as_object_mut().unwrap().remove("source_revision");
        old_json.as_object_mut().unwrap().remove("source_digest");
        std::fs::write(
            store.journal_path("room-1"),
            serde_json::to_vec(&serde_json::json!({"events": [old_json]})).unwrap(),
        )
        .unwrap();

        let journal = DurableRoomJournal::new(Arc::new(store), "room-1");
        let loaded = journal.events().await.unwrap();
        assert_eq!(loaded, vec![old.clone()]);
        journal
            .append(draft("event-new", "new body"))
            .await
            .unwrap();
        let after = journal.events().await.unwrap();
        assert_eq!(after[0], old);
        assert_eq!(after[1].room_seq, 2);
    }

    #[test]
    fn adapter_delegates_snapshot_validation_to_domain() {
        let mut invalid = RoomJournalSnapshot::default();
        invalid.events.push(RoomEvent::default());
        assert!(invalid.validate().is_err());
    }
}
