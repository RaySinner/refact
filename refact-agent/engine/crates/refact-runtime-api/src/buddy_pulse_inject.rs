//! Buddy pulse payload/message assembly. The payload model and its pure
//! rendering helpers live in `refact_buddy_core::pulse_inject`; this module adds
//! the `AppState`-shaped reads (diagnostics, user activity, knowledge lessons,
//! recent reports) that the payload needs.

use chrono::Utc;

use refact_buddy_core::actor::redact_sensitive;
use refact_chat_api::ChatMessage;

pub use refact_buddy_core::pulse_inject::*;

use crate::app_state::AppState;

async fn read_lessons(app: &AppState) -> Vec<PulseLesson> {
    let idx_arc = app.gcx.knowledge_index.clone();
    let cards = {
        let idx = idx_arc.lock().await;
        idx.lessons_for_pulse(5)
    };
    cards
        .into_iter()
        .map(|card| {
            let updated = card
                .updated
                .clone()
                .or_else(|| card.created_at.clone())
                .or_else(|| card.created.clone())
                .unwrap_or_default();
            let preview = card
                .description
                .clone()
                .or_else(|| card.summary.clone())
                .unwrap_or_default();
            PulseLesson {
                title: redact_sensitive(&card.title),
                preview: redact_sensitive(&preview),
                tags: card.tags.iter().map(|tag| redact_sensitive(tag)).collect(),
                updated: redact_sensitive(&updated),
            }
        })
        .collect()
}

async fn build_friction(gcx: AppState) -> PulseFriction {
    let buddy_arc = gcx.buddy.buddy.clone();
    let (diagnostics, pulse_diagnostic_types, stuck_tasks) = {
        let lock = buddy_arc.lock().await;
        let Some(service) = lock.as_ref() else {
            return PulseFriction::default();
        };
        (
            service
                .recent_diagnostics
                .iter()
                .map(|diag| (diag.error_type.clone(), diag.collected_at.clone()))
                .collect::<Vec<_>>(),
            service.pulse.diagnostics.top_error_types.clone(),
            service.pulse.tasks.recent_stuck_alert_count_1h(),
        )
    };
    pulse_friction_from_diagnostics(
        &diagnostics,
        &pulse_diagnostic_types,
        stuck_tasks,
        Utc::now(),
    )
}

async fn build_activity_section(gcx: AppState) -> PulseActivitySection {
    let user_activity = gcx.buddy.user_activity.clone();
    let actions = user_activity.lock().await.last_hours(24);
    build_activity_section_from_actions(&actions)
}

pub async fn build_buddy_pulse_payload(gcx: AppState) -> Option<BuddyPulsePayload> {
    let buddy_arc = gcx.buddy.buddy.clone();
    let project_root: std::path::PathBuf = {
        let lock = buddy_arc.lock().await;
        let service = lock.as_ref()?;
        service.project_root.clone()
    };

    let preferences = read_preferences(&project_root).await;
    let lessons = read_lessons(&gcx).await;
    let friction = build_friction(gcx.clone()).await;
    let recent_reports = build_recent_reports(&project_root).await;
    let user_activity = build_activity_section(gcx).await;

    let payload = BuddyPulsePayload {
        preferences,
        lessons,
        friction,
        recent_reports,
        user_activity,
        generated_at: redact_sensitive(&Utc::now().to_rfc3339()),
    };

    if payload_is_empty(&payload) {
        None
    } else {
        Some(payload)
    }
}

pub async fn build_buddy_pulse_message(gcx: AppState) -> Option<ChatMessage> {
    let payload = build_buddy_pulse_payload(gcx).await?;
    Some(pulse_message_from_payload(&payload))
}
