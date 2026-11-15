use std::sync::Arc;

use async_trait::async_trait;
use refact_scheduler_core::CronStore;

pub struct EngineSchedulerRuntime;

#[async_trait]
impl crate::scheduler::SchedulerRuntime for EngineSchedulerRuntime {
    async fn active_durable_cron_store_for_services(
        &self,
        paths: &crate::services::PathServices,
        workspace: &crate::services::WorkspaceServices,
    ) -> Result<Option<Arc<dyn CronStore>>, String> {
        crate::scheduler::cron_store_for_project(
            crate::files_correction::get_active_project_path_for_services(paths, workspace).await,
        )
    }
}
