use std::any::Any;
use std::future::Future;
use std::sync::Arc;
use tokio::sync::Mutex as AMutex;

pub trait IntegrationSession: Any + Send + Sync {
    fn as_any_mut(&mut self) -> &mut dyn Any;

    fn is_expired(&self) -> bool;

    fn try_stop(
        &mut self,
        self_arc: Arc<AMutex<Box<dyn IntegrationSession>>>,
    ) -> Box<dyn Future<Output = String> + Send>;
}

pub fn get_session_hashmap_key(integration_name: &str, base_key: &str) -> String {
    format!("{} ⚡ {}", integration_name, base_key)
}
