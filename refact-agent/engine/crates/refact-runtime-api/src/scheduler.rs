use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use refact_scheduler_core::{CronStore, JsonFileCronStore};

use crate::files_correction::get_active_project_path;
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

pub async fn active_durable_cron_store(
    gcx: Arc<GlobalContext>,
) -> Result<Option<Arc<dyn CronStore>>, String> {
    cron_store_for_project(get_active_project_path(gcx).await)
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

pub fn cron_store_for_project(
    project: Option<PathBuf>,
) -> Result<Option<Arc<dyn CronStore>>, String> {
    match project {
        None => Ok(None),
        Some(path) => JsonFileCronStore::new(path)
            .map(|store| Some(Arc::new(store) as Arc<dyn CronStore>)),
    }
}
