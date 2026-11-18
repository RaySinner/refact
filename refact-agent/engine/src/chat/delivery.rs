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
    ChatSession, DeliveryOutcome, PendingDelivery, PushMode, TrajectoryCommitIntent,
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_state::EngineChatSessionFacade;
    use refact_runtime_api::ChatSessionFacade;
    use crate::call_validation::ChatMessage;

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
