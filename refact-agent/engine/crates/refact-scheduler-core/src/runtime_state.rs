use std::sync::{Arc, OnceLock};
use crate::{CronStore, InMemoryCronStore};

static SESSION_CRON_STORE: OnceLock<Arc<InMemoryCronStore>> = OnceLock::new();
static RUNNER_CHANGE_NOTIFY: OnceLock<Arc<tokio::sync::Notify>> = OnceLock::new();

pub fn session_cron_store() -> Arc<dyn CronStore> {
    SESSION_CRON_STORE
        .get_or_init(|| Arc::new(InMemoryCronStore::new()))
        .clone()
}

pub fn runner_change_notify() -> Arc<tokio::sync::Notify> {
    RUNNER_CHANGE_NOTIFY
        .get_or_init(|| Arc::new(tokio::sync::Notify::new()))
        .clone()
}

pub fn notify_runner_change() {
    runner_change_notify().notify_waiters();
}
