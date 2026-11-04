use std::collections::HashSet;
use std::sync::Arc;
use crate::global_context::GlobalContext;
use refact_yaml_configs::customization_types::{ProjectRegistry, ModeConfig, SubagentConfig};


pub async fn get_project_registry(gcx: Arc<GlobalContext>) -> Option<ProjectRegistry> {
    gcx.task_storage_runtime
        .ctx_from_gcx(gcx.clone())
        .mode_config
        .get_project_registry()
        .await
}

pub async fn get_mode_config(
    gcx: Arc<GlobalContext>,
    mode_id: &str,
    model_id: Option<&str>,
) -> Option<ModeConfig> {
    gcx.task_storage_runtime
        .ctx_from_gcx(gcx.clone())
        .mode_config
        .get_mode_config(mode_id, model_id)
        .await
}

pub async fn get_subagent_config(
    gcx: Arc<GlobalContext>,
    subagent_id: &str,
    model_id: Option<&str>,
) -> Option<SubagentConfig> {
    gcx.task_storage_runtime
        .ctx_from_gcx(gcx.clone())
        .mode_config
        .get_subagent_config(subagent_id, model_id)
        .await
}

pub async fn warn_unknown_mode_tools(
    gcx: Arc<GlobalContext>,
    mode_id: &str,
    declared: &[String],
    registered: &HashSet<String>,
) {
    gcx.task_storage_runtime
        .ctx_from_gcx(gcx.clone())
        .mode_config
        .warn_unknown_mode_tools(mode_id, declared, registered)
        .await
}
