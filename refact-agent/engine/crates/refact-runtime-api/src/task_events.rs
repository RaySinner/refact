use std::sync::Arc;

use crate::global_context::GlobalContext;

pub use refact_tasks::events::{TaskEvent, TaskEventEnvelope};
pub use refact_tasks::events_emit::{emit_task_event_for_services, emit_task_event_via};
pub use crate::task_storage::{
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
