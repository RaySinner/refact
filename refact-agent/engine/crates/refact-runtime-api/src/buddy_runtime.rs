use std::sync::Arc;

use refact_buddy_core::actor::BuddyMutation;
use refact_buddy_core::events::BuddyEvent;
use refact_buddy_core::types::BuddyRuntimeEvent;

use crate::app_state::AppState;
use crate::ctx_traits::BuddyServicesHandle;
use crate::services::BuddyServices;

pub fn buddy_services_from_handle(handle: &Arc<dyn BuddyServicesHandle>) -> BuddyServices {
    handle
        .as_any()
        .downcast_ref::<BuddyServices>()
        .expect("engine buddy services handle")
        .clone()
}

async fn buddy_apply_for_services(buddy: &BuddyServices, m: BuddyMutation) {
    let buddy_arc = buddy.buddy.clone();
    let mut lock = buddy_arc.lock().await;
    let Some(svc) = lock.as_mut() else { return };
    if let Some(ev) = m.runtime_event {
        svc.enqueue_runtime_event(ev);
    }
    if m.xp > 0 {
        svc.grant_xp(m.xp);
    }
    if let Some(activity) = m.activity {
        svc.add_activity(activity);
    }
    if let Some(mood) = m.mood {
        svc.state.semantic.mood = mood;
        svc.dirty = true;
        let _ = svc.events_tx.send(BuddyEvent::StateUpdated {
            state: svc.state.clone(),
        });
    }
}

pub async fn buddy_apply(app: AppState, m: BuddyMutation) {
    let buddy = app.chat_ctx().buddy;
    buddy_apply_for_services(&buddy, m).await;
}

async fn buddy_enqueue_event_for_services(buddy: &BuddyServices, event: BuddyRuntimeEvent) {
    let buddy_arc = buddy.buddy.clone();
    let mut lock = buddy_arc.lock().await;
    if let Some(svc) = lock.as_mut() {
        svc.enqueue_runtime_event(event);
    }
}

pub async fn buddy_enqueue_event(app: AppState, event: BuddyRuntimeEvent) {
    let buddy = app.chat_ctx().buddy;
    buddy_enqueue_event_for_services(&buddy, event).await;
}

async fn report_error_persisted_with_model_for_services(
    buddy: &BuddyServices,
    error_type: &str,
    error_msg: &str,
    source: Option<&str>,
    chat_id: Option<&str>,
    model_id: Option<&str>,
) {
    let buddy_arc = buddy.buddy.clone();
    let mut lock = buddy_arc.lock().await;
    if let Some(svc) = lock.as_mut() {
        svc.report_error_with_model(error_type, error_msg, source, chat_id, model_id);
    }
}

pub async fn report_error_persisted(
    app: AppState,
    error_type: &str,
    error_msg: &str,
    source: Option<&str>,
    chat_id: Option<&str>,
) {
    report_error_persisted_with_model(app, error_type, error_msg, source, chat_id, None).await;
}

pub async fn report_error_persisted_with_model(
    app: AppState,
    error_type: &str,
    error_msg: &str,
    source: Option<&str>,
    chat_id: Option<&str>,
    model_id: Option<&str>,
) {
    let buddy = app.chat_ctx().buddy;
    report_error_persisted_with_model_for_services(
        &buddy,
        error_type,
        error_msg,
        source,
        chat_id,
        model_id,
    )
    .await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use refact_buddy_core::actor::BuddyService;
    use refact_buddy_core::state::default_buddy_state;
    use refact_buddy_core::types::BuddyActivity;

    fn fixture_buddy_services(
        dir: &std::path::Path,
    ) -> (
        BuddyServices,
        tokio::sync::broadcast::Receiver<refact_buddy_core::events::DiagnosticBuddyEvent>,
    ) {
        let (events_tx, events_rx) =
            tokio::sync::broadcast::channel::<refact_buddy_core::events::DiagnosticBuddyEvent>(64);
        let service = BuddyService::new(
            dir.to_path_buf(),
            default_buddy_state(),
            refact_buddy_core::settings::BuddySettings::default(),
            Vec::new(),
            refact_buddy_core::runtime_queue::RuntimeQueue::new(),
            events_tx.clone(),
            None,
        );
        (
            BuddyServices {
                buddy: Arc::new(tokio::sync::Mutex::new(Some(service))),
                buddy_events_tx: events_tx,
                user_activity: Arc::new(tokio::sync::Mutex::new(
                    refact_buddy_core::user_activity::UserActivityRing::new(
                        dir.to_path_buf(),
                        200,
                    ),
                )),
            },
            events_rx,
        )
    }

    struct PassthroughHandle(BuddyServices);

    impl BuddyServicesHandle for PassthroughHandle {
        fn as_any(&self) -> &dyn std::any::Any {
            &self.0
        }
    }

    struct MismatchHandle;

    impl BuddyServicesHandle for MismatchHandle {
        fn as_any(&self) -> &dyn std::any::Any {
            &()
        }
    }

    fn mutation_full() -> BuddyMutation {
        BuddyMutation {
            runtime_event: Some(refact_buddy_core::actor::make_runtime_event(
                "workflow", "started", "test", "key", "started", None,
            )),
            xp: 3,
            activity: Some(BuddyActivity {
                icon: "✨".to_string(),
                title: "did a thing".to_string(),
                description: "desc".to_string(),
                timestamp: "2026-01-01T00:00:00Z".to_string(),
                activity_type: "test".to_string(),
                chat_id: None,
                failure_category: None,
                failure_summary: None,
            }),
            mood: Some("pleased".to_string()),
        }
    }

    #[test]
    fn handle_downcast_clones_and_shares_the_service_arc() {
        let dir = tempfile::tempdir().unwrap();
        let (services, _rx) = fixture_buddy_services(dir.path());
        let handle: Arc<dyn BuddyServicesHandle> = Arc::new(PassthroughHandle(services.clone()));

        let restored = buddy_services_from_handle(&handle);

        assert!(Arc::ptr_eq(&restored.buddy, &services.buddy));
        assert!(Arc::ptr_eq(&restored.user_activity, &services.user_activity));
    }

    #[test]
    #[should_panic(expected = "engine buddy services handle")]
    fn handle_downcast_panics_on_mismatch() {
        let handle: Arc<dyn BuddyServicesHandle> = Arc::new(MismatchHandle);
        let _ = buddy_services_from_handle(&handle);
    }

    #[tokio::test]
    async fn missing_service_is_a_noop_for_all_helpers() {
        let dir = tempfile::tempdir().unwrap();
        let (services, _rx) = fixture_buddy_services(dir.path());
        *services.buddy.lock().await = None;

        buddy_apply_for_services(&services, mutation_full()).await;
        buddy_enqueue_event_for_services(&services,
            refact_buddy_core::actor::make_runtime_event(
                "workflow", "second", "test", "key-2", "started", None,
            ),
        )
        .await;
        report_error_persisted_with_model_for_services(
            &services,
            "test_error",
            "boom",
            Some("buddy_runtime.rs"),
            None,
            None,
        )
        .await;
    }

    #[tokio::test]
    async fn mutation_applies_side_effects_in_order_and_emits_state_updated() {
        let dir = tempfile::tempdir().unwrap();
        let (services, mut rx) = fixture_buddy_services(dir.path());

        buddy_apply_for_services(&services, mutation_full()).await;

        let buddy_arc = services.buddy.clone();
        let lock = buddy_arc.lock().await;
        let svc = lock.as_ref().unwrap();
        assert_eq!(svc.runtime_queue.items.len(), 1);
        assert_eq!(svc.runtime_queue.items[0].signal_type, "workflow");
        assert_eq!(svc.state.progression.xp, 3);
        assert_eq!(svc.state.recent_activities[0].title, "did a thing");
        assert_eq!(svc.state.semantic.mood, "pleased");
        assert!(svc.dirty);
        drop(lock);

        let mut saw_state_updated = false;
        while let Ok(event) = rx.try_recv() {
            if matches!(
                event,
                refact_buddy_core::events::BuddyEvent::StateUpdated { .. }
            ) {
                saw_state_updated = true;
            }
        }
        assert!(saw_state_updated);
        assert!(saw_state_updated);
    }

    #[tokio::test]
    async fn enqueue_event_persists_into_runtime_queue() {
        let dir = tempfile::tempdir().unwrap();
        let (services, _rx) = fixture_buddy_services(dir.path());

        buddy_enqueue_event_for_services(
            &services,
            refact_buddy_core::actor::make_runtime_event(
                "workflow", "enqueued", "test", "key-e", "started", None,
            ),
        )
        .await;

        let buddy_arc = services.buddy.clone();
        let lock = buddy_arc.lock().await;
        let svc = lock.as_ref().unwrap();
        assert_eq!(svc.runtime_queue.items.len(), 1);
        assert_eq!(svc.runtime_queue.items[0].title, "enqueued");
    }

    #[tokio::test]
    async fn report_error_persists_a_diagnostic_and_activity() {
        let dir = tempfile::tempdir().unwrap();
        let (services, _rx) = fixture_buddy_services(dir.path());

        report_error_persisted_with_model_for_services(
            &services,
            "browser_error",
            "boom",
            Some("buddy_runtime.rs"),
            Some("chat-1"),
            Some("model-x"),
        )
        .await;

        let buddy_arc = services.buddy.clone();
        let lock = buddy_arc.lock().await;
        let svc = lock.as_ref().unwrap();
        assert_eq!(svc.recent_diagnostics.len(), 1);
        assert_eq!(svc.recent_diagnostics[0].error_type, "browser_error");
        assert_eq!(svc.recent_diagnostics[0].model_id.as_deref(), Some("model-x"));
        assert_eq!(svc.recent_diagnostics[0].chat_id.as_deref(), Some("chat-1"));
        assert_eq!(svc.state.recent_activities[0].activity_type, "error");
    }
}
