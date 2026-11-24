mod delivery;
mod event;
mod journal;
mod participant;
mod projection;
mod roster;
mod router;
mod source_snapshot;

pub use delivery::DeliveryAttempt;
pub use event::{RoomAuthor, RoomEvent, RoomEventDraft, RoomEventKind, ROOM_EVENT_SCHEMA_VERSION};
pub use journal::{
    AppendOutcome, DurableRoomJournal, RoomJournal, RoomJournalSnapshot, RoomJournalStore,
};
pub use participant::{Participant, RoomParticipant, RoomRoster};
pub use projection::{project_assistant_message, PublicProjectionInput, PublicRoomEvent};
pub use roster::{spawn_order, validate_roster, validate_spawn, RosterMember};
pub use router::{replay_deliveries, route_event, RoomDelivery};
pub use source_snapshot::{
    SourceDeliveryVersion, SourceFile, SourceIdentity, SourceSnapshot, SourceSnapshotError,
    SOURCE_SNAPSHOT_SCHEMA_VERSION,
};

#[derive(Debug, thiserror::Error)]
pub enum RoomError {
    #[error("invalid room event: {0}")]
    Validation(String),
    #[error("room event conflict: {0}")]
    Conflict(String),
    #[error("unsupported room schema version: {0}")]
    UnsupportedSchema(u32),
    #[error("room journal store: {0}")]
    Store(String),
}
