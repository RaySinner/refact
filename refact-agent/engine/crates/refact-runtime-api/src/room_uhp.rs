use std::path::Path;

use refact_privacy::{Destination, DestinationId, DestinationKind};
use refact_room::{RoomDelivery, RoomEventDraft};

use crate::app_state::AppState;
use crate::external_participant::{
    ExternalDeliveryStatus, ExternalRoomDelivery, ExternalSourceFreshness,
};
use crate::room_snapshot::publication_metadata_if_fresh;
use crate::room_snapshot_export::{export_writer_snapshot, SnapshotExportLimits};

pub struct ExternalAdvisorPublication {
    pub delivery_id: String,
    pub status: ExternalDeliveryStatus,
    pub drafts: Vec<RoomEventDraft>,
}

pub async fn deliver_external_advisor(
    app: &AppState,
    writer_root: &Path,
    delivery: RoomDelivery,
) -> Result<ExternalAdvisorPublication, String> {
    let participant_id = delivery
        .recipient
        .external_participant_id
        .as_deref()
        .ok_or_else(|| "external delivery requires a registry participant".to_string())?;
    crate::privacy::load_privacy_if_needed(app.gcx.clone()).await;
    let destination = Destination {
        id: DestinationId(participant_id.to_string()),
        kind: DestinationKind::Provider,
        display_name: delivery.recipient.display_name.clone(),
    };
    let exported = export_snapshot(app, writer_root, &destination)?;
    let completed = app
        .external_participants
        .deliver_room_event(&delivery.event, &delivery.attempt, &exported)
        .await
        .map_err(|error| error.to_string())?;
    let delivery_id = completed.delivery.delivery_id.clone();
    if completed.completed_bodies.is_empty() {
        return Ok(ExternalAdvisorPublication {
            delivery_id,
            status: completed.delivery.status,
            drafts: Vec::new(),
        });
    }
    let current = export_snapshot(app, writer_root, &destination)?;
    if publication_metadata_if_fresh(&completed.source, current.snapshot.identity()).is_err() {
        app.external_participants
            .set_source_freshness(participant_id, &delivery_id, ExternalSourceFreshness::Stale)
            .await
            .map_err(|error| error.to_string())?;
        return Err("refact.room.stale_source".to_string());
    }
    app.external_participants
        .set_source_freshness(participant_id, &delivery_id, ExternalSourceFreshness::Fresh)
        .await
        .map_err(|error| error.to_string())?;
    Ok(ExternalAdvisorPublication {
        delivery_id,
        status: completed.delivery.status.clone(),
        drafts: advisor_drafts(delivery, completed),
    })
}

fn export_snapshot(
    app: &AppState,
    writer_root: &Path,
    destination: &Destination,
) -> Result<crate::room_snapshot_export::ExportedSourceSnapshot, String> {
    let policy = app
        .gcx
        .privacy_policy_load
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .policy
        .compile()
        .map_err(|error| format!("failed to compile writer snapshot privacy policy: {error}"))?;
    let indexing = app
        .gcx
        .indexing_everywhere
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    export_writer_snapshot(
        writer_root,
        &policy,
        &indexing,
        destination,
        SnapshotExportLimits::default(),
    )
}

fn advisor_drafts(delivery: RoomDelivery, completed: ExternalRoomDelivery) -> Vec<RoomEventDraft> {
    let response_id = completed
        .response_id
        .expect("a completed public body has a response id");
    completed
        .completed_bodies
        .into_iter()
        .enumerate()
        .map(|(index, body)| RoomEventDraft {
            room_id: delivery.event.room_id.clone(),
            event_id: format!("external:{response_id}:message:{index}"),
            author: delivery.recipient.author(),
            body,
            recipient_id: Some(delivery.event.author.participant_id.clone()),
            source_message_id: response_id.clone(),
            turn_id: response_id.clone(),
            created_at: chrono::Utc::now().to_rfc3339(),
            source_revision: Some(completed.source.source.source_revision.clone()),
            source_digest: Some(completed.source.source.source_digest.clone()),
            ..Default::default()
        })
        .collect()
}
