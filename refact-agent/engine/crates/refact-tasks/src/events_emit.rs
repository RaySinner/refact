use std::sync::atomic::Ordering;
use std::sync::Arc;

use crate::events::{TaskEvent, TaskEventEnvelope};

pub async fn emit_task_event_via(
    task_events_tx: &Option<tokio::sync::broadcast::Sender<TaskEventEnvelope>>,
    task_events_seq: &Option<Arc<std::sync::atomic::AtomicU64>>,
    event: TaskEvent,
) {
    if let (Some(tx), Some(seq_counter)) = (task_events_tx, task_events_seq) {
        let seq = seq_counter.fetch_add(1, Ordering::SeqCst);
        let envelope = TaskEventEnvelope { seq, event };
        let _ = tx.send(envelope);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn emit_task_event_via_sends_envelope_with_increasing_seq() {
        let (tx, mut rx) = tokio::sync::broadcast::channel(4);
        let seq = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let tx_opt = Some(tx);
        let seq_opt = Some(seq.clone());

        emit_task_event_via(&tx_opt, &seq_opt, TaskEvent::Heartbeat { ts: "t1".into() }).await;
        emit_task_event_via(&tx_opt, &seq_opt, TaskEvent::Heartbeat { ts: "t2".into() }).await;

        let first = rx.recv().await.unwrap();
        let second = rx.recv().await.unwrap();
        assert_eq!(first.seq, 0);
        assert_eq!(second.seq, 1);
        assert_eq!(seq.load(std::sync::atomic::Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn emit_task_event_via_without_channel_or_counter_is_noop() {
        let seq = Arc::new(std::sync::atomic::AtomicU64::new(5));
        let seq_opt = Some(seq.clone());

        emit_task_event_via(&None, &seq_opt, TaskEvent::Heartbeat { ts: "t".into() }).await;
        emit_task_event_via(&Some(tokio::sync::broadcast::channel(1).0), &None, TaskEvent::Heartbeat { ts: "t".into() }).await;

        assert_eq!(seq.load(std::sync::atomic::Ordering::SeqCst), 5);
    }
}
