pub mod chat_pattern;
pub mod diagnostic_cluster;
pub mod provider_health;
pub mod task_health;
pub mod trajectory_clutter;

use chrono::{DateTime, Utc};

pub struct ObserverContext {
    pub project_root: std::path::PathBuf,
    pub now: DateTime<Utc>,
}

pub struct Ephemeral<T>(T);

impl<T> Ephemeral<T> {
    pub fn new(value: T) -> Self {
        Self(value)
    }

    pub fn as_ref(&self) -> &T {
        &self.0
    }
}

impl<T> std::fmt::Debug for Ephemeral<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("<ephemeral>")
    }
}
