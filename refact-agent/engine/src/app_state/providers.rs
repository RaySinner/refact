#[allow(unused_imports)]
use super::*;

pub struct EngineProjectTreeProvider {
    gcx: SharedGlobalContext,
}

impl EngineProjectTreeProvider {
    pub fn new(gcx: SharedGlobalContext) -> Self {
        Self { gcx }
    }
}

#[async_trait]
impl ProjectTreeProvider for EngineProjectTreeProvider {
    async fn generate_compact_project_tree(&self, max_depth: usize) -> Result<String, String> {
        crate::chat::system_context::generate_compact_project_tree(self.gcx.clone(), max_depth)
            .await
    }
}

pub struct EngineHookRunner {
    gcx: SharedGlobalContext,
}

impl EngineHookRunner {
    pub fn new(gcx: SharedGlobalContext) -> Self {
        Self { gcx }
    }
}

#[async_trait]
impl HookRunner for EngineHookRunner {
    async fn project_dir(&self) -> String {
        let app = app_state_from_gcx(self.gcx.clone()).await;
        crate::ext::hooks_runner::get_project_dir_string(app).await
    }

    async fn run_hooks(
        &self,
        event: crate::ext::hooks::HookEvent,
        payload: crate::ext::hooks_runner::HookPayload,
    ) -> Vec<crate::ext::hooks_runner::HookResult> {
        let app = app_state_from_gcx(self.gcx.clone()).await;
        crate::ext::hooks_runner::run_hooks(app, event, payload).await
    }
}

pub struct EngineModeConfigProvider {
    gcx: SharedGlobalContext,
}

impl EngineModeConfigProvider {
    pub fn new(gcx: SharedGlobalContext) -> Self {
        Self { gcx }
    }
}

#[async_trait]
impl ModeConfigProvider for EngineModeConfigProvider {
    async fn warn_unknown_mode_tools(&self, mode_id: &str, declared: &[String], registered: &std::collections::HashSet<String>) {
        crate::yaml_configs::mode_validation::warn_unknown_mode_tools(self.gcx.clone(), mode_id, declared, registered).await;
    }

    async fn get_project_registry(
        &self,
    ) -> Option<crate::yaml_configs::customization_types::ProjectRegistry> {
        crate::yaml_configs::customization_registry::get_project_registry(self.gcx.clone()).await
    }

    async fn get_mode_config(
        &self,
        mode_id: &str,
        model_id: Option<&str>,
    ) -> Option<crate::yaml_configs::customization_types::ModeConfig> {
        crate::yaml_configs::customization_registry::get_mode_config(
            self.gcx.clone(),
            mode_id,
            model_id,
        )
        .await
    }

    async fn get_subagent_config(
        &self,
        subagent_id: &str,
        model_id: Option<&str>,
    ) -> Option<crate::yaml_configs::customization_types::SubagentConfig> {
        crate::yaml_configs::customization_registry::get_subagent_config(
            self.gcx.clone(),
            subagent_id,
            model_id,
        )
        .await
    }
}

pub struct EngineCapsProvider {
    gcx: SharedGlobalContext,
}

impl EngineCapsProvider {
    pub fn new(gcx: SharedGlobalContext) -> Self {
        Self { gcx }
    }
}

#[async_trait]
impl CapsProvider for EngineCapsProvider {
    async fn try_load_caps_quickly(
        &self,
        max_age_seconds: u64,
    ) -> Result<Arc<crate::caps::CodeAssistantCaps>, crate::custom_error::ScratchError> {
        crate::global_context::try_load_caps_quickly_if_not_present(
            self.gcx.clone(),
            max_age_seconds,
        )
        .await
    }
}

