use std::sync::Arc;
use async_trait::async_trait;
use refact_scheduler_core::CronStore;
use crate::global_context::GlobalContext;
use crate::services::{PathServices, WorkspaceServices};

#[async_trait]
pub trait SchedulerRuntime: Send + Sync {
    async fn active_durable_cron_store_for_services(
        &self,
        paths: &PathServices,
        workspace: &WorkspaceServices,
    ) -> Result<Option<Arc<dyn CronStore>>, String>;
}

pub async fn active_durable_cron_store_for_services(
    gcx: &GlobalContext,
    paths: &PathServices,
    workspace: &WorkspaceServices,
) -> Result<Option<Arc<dyn CronStore>>, String> {
    gcx.scheduler_runtime
        .active_durable_cron_store_for_services(paths, workspace)
        .await
}
