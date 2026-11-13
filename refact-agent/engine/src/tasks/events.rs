use std::sync::Arc;

use crate::global_context::GlobalContext;

pub use refact_tasks::events::{TaskEvent, TaskEventEnvelope};
pub use refact_tasks::events_emit::{emit_task_event_for_services, emit_task_event_via};
pub use refact_runtime_api::task_storage::{
    emit_task_updated_via, enrich_task_meta_with_session_state,
    enrich_task_with_session_state_for_services,
};
use refact_tasks::types::TaskMeta;

pub async fn emit_task_event(gcx: Arc<GlobalContext>, event: TaskEvent) {
    emit_task_event_via(&gcx.task_events_tx, &gcx.task_events_seq, event).await;
}

pub async fn enrich_task_with_session_state(gcx: Arc<GlobalContext>, task: &mut TaskMeta) {
    enrich_task_meta_with_session_state(&gcx.chat_sessions, task).await;
}

pub async fn emit_task_updated(gcx: Arc<GlobalContext>, task_id: String, meta: TaskMeta) {
    emit_task_updated_via(
        &gcx.chat_sessions,
        &gcx.task_events_tx,
        &gcx.task_events_seq,
        task_id,
        meta,
    )
    .await;
}

pub async fn emit_task_comments_changed(gcx: Arc<GlobalContext>, task_id: &str, card_id: &str) {
    emit_task_event(
        gcx,
        TaskEvent::TaskCommentsChanged {
            task_id: task_id.to_string(),
            card_id: card_id.to_string(),
        },
    )
    .await;
}

pub async fn emit_task_document_changed(
    gcx: Arc<GlobalContext>,
    task_id: &str,
    slug: Option<&str>,
) {
    emit_task_event(
        gcx,
        TaskEvent::TaskDocumentChanged {
            task_id: task_id.to_string(),
            slug: slug.map(str::to_string),
        },
    )
    .await;
}

pub async fn emit_task_memories_changed(gcx: Arc<GlobalContext>, task_id: &str) {
    emit_task_event(
        gcx,
        TaskEvent::TaskMemoriesChanged {
            task_id: task_id.to_string(),
        },
    )
    .await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn session_state_enrichment_does_not_list_task_trajectories() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let mut task = TaskMeta {
            schema_version: 1,
            id: "task-index-free".to_string(),
            name: "Task".to_string(),
            status: refact_tasks::types::TaskStatus::Active,
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
            cards_total: 0,
            cards_done: 0,
            cards_failed: 0,
            agents_active: 0,
            base_branch: None,
            base_commit: None,
            default_agent_model: None,
            is_name_generated: false,
            last_agents_summary_at: None,
            planner_session_state: Some("stale".to_string()),
        };

        enrich_task_with_session_state(gcx.clone(), &mut task).await;
        let counters = gcx.trajectory_index_coordinator.listing_counters_for_test();

        assert!(task.planner_session_state.is_none());
        assert_eq!(
            counters
                .calls_by_caller
                .iter()
                .find(|(caller, _)| *caller == "task_trajectory_api")
                .unwrap()
                .1,
            0
        );
    }
}
