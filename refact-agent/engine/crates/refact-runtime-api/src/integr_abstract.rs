use std::sync::Arc;

use async_trait::async_trait;
use refact_integrations::integr_abstract::IntegrationCommon;

use crate::global_context::GlobalContext;
use crate::Tool;

#[async_trait]
pub trait IntegrationTrait: Send + Sync {
    fn integr_schema(&self) -> &str;
    async fn integr_settings_apply(
        &mut self,
        gcx: Arc<GlobalContext>,
        config_path: String,
        value: &serde_json::Value,
    ) -> Result<(), serde_json::Error>;
    fn integr_settings_as_json(&self) -> serde_json::Value;
    fn integr_common(&self) -> IntegrationCommon;
    async fn integr_tools(&self, integr_name: &str) -> Vec<Box<dyn Tool + Send>>;
}
