pub use refact_runtime_api::chat_delivery_runtime::*;

// The delivery tests stay in the engine: they build a real `AppState` from
// `make_test_gcx`, which only the engine can construct.
#[cfg(test)]
#[allow(unused_imports)]
use std::sync::atomic::Ordering;
#[cfg(test)]
#[allow(unused_imports)]
use std::sync::Arc;
#[cfg(test)]
#[allow(unused_imports)]
use tokio::sync::Mutex as AMutex;
#[cfg(test)]
#[allow(unused_imports)]
use crate::call_validation::ChatMessage;
#[cfg(test)]
#[allow(unused_imports)]
use crate::chat::types::{
    ChatSession, DeliveryOutcome, GoalStatus, PendingDelivery, PushMode, TrajectoryCommitIntent,
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_state::EngineChatSessionFacade;
    use crate::call_validation::ChatMessage;
    use refact_runtime_api::ChatSessionFacade;

    async fn stalled_turn_session(
        app: &crate::app_state::AppState,
        chat_id: &str,
    ) -> Arc<AMutex<ChatSession>> {
        let mut session = ChatSession::new(chat_id.into());
        session.install_goal(
            "agent",
            "finish the card",
            true,
            refact_chat_api::GoalBudget::default(),
        );
        let session = Arc::new(AMutex::new(session));
        app.chat
            .sessions
            .write()
            .await
            .insert(chat_id.into(), session.clone());
        session
    }

    fn stalled_turn_delivery(session: &ChatSession, chat_id: &str) -> PendingDelivery {
        let mut delivery =
            refact_runtime_api::buddy_interventions::stalled_turn::intervention_delivery(
                chat_id,
                4,
                "Finish the card.",
            )
            .unwrap();
        assert_eq!(
            refact_runtime_api::buddy_interventions::stalled_turn::arbitrate_stalled_turn_delivery(
                session,
                &mut delivery,
                1_000,
            ),
            refact_chat_api::pursuit::PursuitDecision::StartTurn
        );
        delivery
    }

    #[tokio::test]
    async fn stalled_turn_pursuit_is_revalidated_at_actual_enqueue() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let workspace = tempfile::tempdir().unwrap();
        *gcx.documents_state.workspace_folders.lock().unwrap() =
            vec![workspace.path().to_path_buf()];
        let app = crate::app_state::app_state_from_gcx(gcx).await;

        for status in [GoalStatus::Paused, GoalStatus::Verifying] {
            let chat_id = format!("stalled-turn-enqueue-{status:?}");
            let session = stalled_turn_session(&app, &chat_id).await;
            let delivery = stalled_turn_delivery(&*session.lock().await, &chat_id);
            session.lock().await.goal_set_status(status);

            assert_eq!(
                deliver_to_chat(app.clone(), &chat_id, delivery.clone())
                    .await
                    .unwrap(),
                DeliveryOutcome::Duplicate
            );
            let session = session.lock().await;
            assert!(session.pending_deliveries.is_empty());
            assert!(!session.delivered_delivery_ids.contains(&delivery.id));
            assert!(session.delivery_wake_sources.is_empty());
            assert_eq!(session.messages.len(), 1);
        }

        for status in [GoalStatus::Paused, GoalStatus::Verifying] {
            let chat_id = format!("stalled-turn-drain-{status:?}");
            let session = stalled_turn_session(&app, &chat_id).await;
            let delivery = stalled_turn_delivery(&*session.lock().await, &chat_id);
            session.lock().await.draft_message =
                Some(ChatMessage::new("assistant".into(), "working".into()));
            assert_eq!(
                deliver_to_chat(app.clone(), &chat_id, delivery.clone())
                    .await
                    .unwrap(),
                DeliveryOutcome::Queued
            );

            let mut session = session.lock().await;
            session.goal_set_status(status);
            session.draft_message = None;
            let (delivered, wake) = session.drain_pending_deliveries();
            assert!(delivered.is_empty());
            assert!(!wake);
            assert!(session.pending_deliveries.is_empty());
            assert!(!session.delivered_delivery_ids.contains(&delivery.id));
            assert!(session.delivery_wake_sources.is_empty());
            assert_eq!(session.messages.len(), 1);
        }

        let session = stalled_turn_session(&app, "stalled-turn-valid").await;
        let delivery = stalled_turn_delivery(&*session.lock().await, "stalled-turn-valid");
        {
            let mut locked = session.lock().await;
            locked.pending_deliveries.push_back(delivery.clone());
            let mut current = delivery.clone();
            assert_eq!(
                refact_chat_api::pursuit::revalidate_stalled_turn_delivery(
                    &locked,
                    &mut current,
                    1_000,
                ),
                Some(refact_chat_api::pursuit::PursuitDecision::StartTurn)
            );
            locked.pending_deliveries.clear();
        }
        assert_eq!(
            deliver_to_chat(app.clone(), "stalled-turn-valid", delivery.clone(),)
                .await
                .unwrap(),
            DeliveryOutcome::Delivered
        );
        {
            let session = session.lock().await;
            assert!(session.delivered_delivery_ids.contains(&delivery.id));
            assert!(delivery.wake);
            assert_eq!(
                session.messages.last().unwrap().extra["pursuit"]["wake_requested"],
                true
            );
            assert!(session.delivery_wake_sources.contains("external"));
        }

        let session = stalled_turn_session(&app, "stalled-turn-autonomy-off").await;
        let delivery = stalled_turn_delivery(&*session.lock().await, "stalled-turn-autonomy-off");
        {
            let mut session = session.lock().await;
            let goal = session.goal.as_mut().unwrap();
            goal.autonomy = refact_chat_api::GoalAutonomy::Off;
            goal.revision += 1;
        }
        assert_eq!(
            deliver_to_chat(app.clone(), "stalled-turn-autonomy-off", delivery.clone(),)
                .await
                .unwrap(),
            DeliveryOutcome::Delivered
        );
        {
            let session = session.lock().await;
            let message = session.messages.last().unwrap();
            assert_eq!(message.extra["delivery"]["id"], delivery.id);
            assert_eq!(message.extra["delivery"]["push"], "when_idle");
            assert_eq!(message.extra["pursuit"]["wake_requested"], false);
            assert!(session.delivery_wake_sources.is_empty());
        }

        let session = stalled_turn_session(&app, "stalled-turn-other-pending").await;
        let delivery = stalled_turn_delivery(&*session.lock().await, "stalled-turn-other-pending");
        {
            let mut session = session.lock().await;
            session.draft_message = Some(ChatMessage::new("assistant".into(), "working".into()));
            assert_eq!(
                session.enqueue_delivery(PendingDelivery::with_id(
                    "other-correction",
                    vec![ChatMessage::new("user".into(), "correct course".into())],
                    PushMode::Append,
                    "test",
                    true,
                )),
                Ok(DeliveryOutcome::Queued)
            );
        }
        assert_eq!(
            deliver_to_chat(app.clone(), "stalled-turn-other-pending", delivery.clone(),)
                .await
                .unwrap(),
            DeliveryOutcome::Duplicate
        );
        let session = session.lock().await;
        assert_eq!(session.pending_deliveries.len(), 1);
        assert_eq!(session.pending_deliveries[0].id, "other-correction");
        assert!(!session.delivered_delivery_ids.contains(&delivery.id));
    }

    #[tokio::test]
    async fn stalled_turn_non_delivered_outcome_releases_reserved_slot() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let workspace = tempfile::tempdir().unwrap();
        *gcx.documents_state.workspace_folders.lock().unwrap() =
            vec![workspace.path().to_path_buf()];
        let app = crate::app_state::app_state_from_gcx(gcx).await;
        let budget = refact_runtime_api::buddy_interventions::stalled_turn::StalledTurnBudget {
            min_interval_secs: 3_600,
            max_per_hour: 1,
        };
        let session = stalled_turn_session(&app, "stalled-turn-slot-release").await;
        let delivery = stalled_turn_delivery(&*session.lock().await, "stalled-turn-slot-release");
        let reserved_at = chrono::Utc::now();
        assert!(
            refact_runtime_api::buddy_interventions::stalled_turn::reserve_stalled_turn_slot(
                "stalled-turn-slot-release",
                reserved_at,
                budget,
            )
            .await
        );
        session.lock().await.goal_set_status(GoalStatus::Paused);
        let outcome = deliver_to_chat(app.clone(), "stalled-turn-slot-release", delivery.clone())
            .await
            .unwrap();
        assert_eq!(outcome, DeliveryOutcome::Duplicate);
        assert!(
            refact_runtime_api::buddy_interventions::stalled_turn::release_stalled_turn_slot_for_non_delivery(
                &session,
                "stalled-turn-slot-release",
                &delivery.id,
                outcome,
                reserved_at,
            )
            .await
        );
        assert!(
            refact_runtime_api::buddy_interventions::stalled_turn::reserve_stalled_turn_slot(
                "stalled-turn-slot-release",
                reserved_at + chrono::Duration::seconds(1),
                budget,
            )
            .await
        );

        let session = stalled_turn_session(&app, "stalled-turn-real-duplicate").await;
        let delivery = stalled_turn_delivery(&*session.lock().await, "stalled-turn-real-duplicate");
        assert_eq!(
            deliver_to_chat(app.clone(), "stalled-turn-real-duplicate", delivery.clone(),)
                .await
                .unwrap(),
            DeliveryOutcome::Delivered
        );
        let reserved_at = chrono::Utc::now();
        assert!(
            refact_runtime_api::buddy_interventions::stalled_turn::reserve_stalled_turn_slot(
                "stalled-turn-real-duplicate",
                reserved_at,
                budget,
            )
            .await
        );
        assert!(
            !refact_runtime_api::buddy_interventions::stalled_turn::release_stalled_turn_slot_for_non_delivery(
                &session,
                "stalled-turn-real-duplicate",
                &delivery.id,
                DeliveryOutcome::Duplicate,
                reserved_at,
            )
            .await
        );
        assert!(
            !refact_runtime_api::buddy_interventions::stalled_turn::reserve_stalled_turn_slot(
                "stalled-turn-real-duplicate",
                reserved_at + chrono::Duration::seconds(1),
                budget,
            )
            .await
        );
    }

    #[tokio::test]
    async fn delivery_failed_save_and_duplicate_retry_remain_unacknowledged() {
        for queued in [false, true] {
            let workspace = tempfile::tempdir().unwrap();
            let config = tempfile::tempdir().unwrap();
            let gcx = crate::global_context::tests::make_test_gcx_with_dirs(
                workspace.path().join("cache"),
                config.path().to_path_buf(),
            )
            .await;
            *gcx.documents_state.workspace_folders.lock().unwrap() =
                vec![workspace.path().to_path_buf()];
            let app = crate::app_state::app_state_from_gcx(gcx).await;
            let id = "durable-delivery";
            let session = Arc::new(AMutex::new(ChatSession::new(id.into())));
            session.lock().await.thread.title = "Delivery acceptance".into();
            if queued {
                session.lock().await.start_stream().unwrap();
            }
            app.chat
                .sessions
                .write()
                .await
                .insert(id.into(), session.clone());
            let directory = workspace
                .path()
                .join(".refact")
                .join("trajectories")
                .join(id);
            tokio::fs::create_dir_all(directory.parent().unwrap())
                .await
                .unwrap();
            tokio::fs::write(&directory, "block directory creation")
                .await
                .unwrap();
            let delivery = PendingDelivery::with_id(
                "retry",
                vec![ChatMessage::new("user".into(), "keep".into())],
                PushMode::Append,
                "test",
                false,
            );
            assert!(deliver_to_chat(app.clone(), id, delivery.clone())
                .await
                .is_err());
            assert!(deliver_to_chat(app.clone(), id, delivery.clone())
                .await
                .is_err());
            {
                let session = session.lock().await;
                assert_eq!(session.pending_deliveries.len(), usize::from(queued));
                assert_eq!(session.messages.len(), usize::from(!queued));
                assert!(session.trajectory_dirty);
            }
            tokio::fs::remove_file(directory).await.unwrap();
            assert_eq!(
                deliver_to_chat(app.clone(), id, delivery).await.unwrap(),
                DeliveryOutcome::Duplicate
            );
            let saved = crate::chat::trajectories::load_trajectory_for_chat(&app.chat_ctx(), id)
                .await
                .unwrap();
            assert_eq!(saved.pending_deliveries.len(), usize::from(queued));
            assert_eq!(saved.messages.len(), usize::from(!queued));
            assert!(!session.lock().await.trajectory_dirty);
        }
    }

    #[tokio::test]
    async fn delivery_facade_reports_actual_outcomes_and_rejects_missing_target() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let workspace = tempfile::tempdir().unwrap();
        *gcx.documents_state.workspace_folders.lock().unwrap() =
            vec![workspace.path().to_path_buf()];
        let app = crate::app_state::app_state_from_gcx(gcx.clone()).await;
        let session = Arc::new(AMutex::new(ChatSession::new("delivery-facade".into())));
        app.chat
            .sessions
            .write()
            .await
            .insert("delivery-facade".into(), session.clone());
        let facade = EngineChatSessionFacade::new(gcx);
        let make = |id: &str| {
            PendingDelivery::with_id(
                id,
                vec![ChatMessage::new("user".into(), "delivery".into())],
                PushMode::Append,
                "test",
                false,
            )
        };
        assert!(facade
            .deliver_messages("missing-delivery-target", make("missing"))
            .await
            .is_err());
        assert_eq!(
            facade
                .deliver_messages("delivery-facade", make("one"))
                .await
                .unwrap(),
            DeliveryOutcome::Delivered
        );
        assert_eq!(
            facade
                .deliver_messages("delivery-facade", make("one"))
                .await
                .unwrap(),
            DeliveryOutcome::Duplicate
        );
        session.lock().await.start_stream().unwrap();
        assert_eq!(
            facade
                .deliver_messages("delivery-facade", make("two"))
                .await
                .unwrap(),
            DeliveryOutcome::Queued
        );
        assert!(!session.lock().await.abort_flag.load(Ordering::SeqCst));
        facade
            .update_pending_delivery("delivery-facade", "two", Some(PushMode::Preempt), false)
            .await
            .unwrap();
        let session = session.lock().await;
        assert!(session.pending_deliveries.is_empty());
        assert!(session.draft_message.is_none());
        assert!(session.abort_flag.load(Ordering::SeqCst));
    }
}
