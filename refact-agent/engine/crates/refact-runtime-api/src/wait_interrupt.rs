use std::sync::{Arc, Mutex as StdMutex};
use tokio::sync::Mutex as AMutex;

use crate::AtCommandsContext;
use refact_chat_api::WaitInterruptCause;

#[derive(Clone)]
pub struct WaitInterrupt {
    pub abort_flag: Arc<std::sync::atomic::AtomicBool>,
    pub wait_flag: Arc<std::sync::atomic::AtomicBool>,
    pub cause: Arc<StdMutex<WaitInterruptCause>>,
    pub notify: Option<Arc<tokio::sync::Notify>>,
}

impl WaitInterrupt {
    pub async fn from_context(ccx: &Arc<AMutex<AtCommandsContext>>) -> Self {
        let (app, chat_id, abort_flag) = {
            let context = ccx.lock().await;
            (
                context.app.clone(),
                context.chat_id.clone(),
                context.abort_flag.clone(),
            )
        };
        let session = app.chat.sessions.read().await.get(&chat_id).cloned();
        let (wait_flag, cause, notify) = if let Some(session) = session {
            let session = session.lock().await;
            (
                session.wait_interrupt_flag.clone(),
                session.wait_interrupt_cause.clone(),
                Some(session.abort_notify.clone()),
            )
        } else {
            (
                Arc::new(std::sync::atomic::AtomicBool::new(false)),
                Arc::new(StdMutex::new(WaitInterruptCause::MessageArrived)),
                None,
            )
        };
        Self {
            abort_flag,
            wait_flag,
            cause,
            notify,
        }
    }

    pub fn interrupted(&self) -> bool {
        self.abort_flag.load(std::sync::atomic::Ordering::SeqCst)
            || self.wait_flag.load(std::sync::atomic::Ordering::SeqCst)
    }

    pub fn reason(&self, elapsed_seconds: f64) -> String {
        let wait_cause = self
            .cause
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .label();
        let cause = if self.wait_flag.load(std::sync::atomic::Ordering::SeqCst) {
            wait_cause
        } else {
            "user aborted"
        };
        format!("interrupted after {elapsed_seconds:.1}s: {cause}")
    }

    pub async fn wait(&self) {
        loop {
            let notified = async {
                if let Some(notify) = &self.notify {
                    notify.notified().await;
                } else {
                    std::future::pending::<()>().await;
                }
            };
            tokio::pin!(notified);
            if self.interrupted() {
                return;
            }
            tokio::select! {
                _ = &mut notified => {},
                _ = tokio::time::sleep(std::time::Duration::from_millis(50)) => {},
            }
        }
    }
}
