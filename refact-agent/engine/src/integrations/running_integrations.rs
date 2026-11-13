use crate::integrations::integr_abstract::IntegrationTrait;

pub use refact_runtime_api::integrations_runtime::load_integrations;

/// The engine side of the `IntegrationProvider` seam: it owns the concrete integration
/// constructors (`integration_from_name`, `integrations_list`) that lower crates cannot
/// reach. Installed through the runtime-api `ContextFactory`.
pub struct EngineIntegrationProvider;

impl refact_runtime_api::integrations_runtime::IntegrationProvider for EngineIntegrationProvider {
    fn integration_names(&self, allow_experimental: bool) -> Vec<String> {
        crate::integrations::integrations_list(allow_experimental)
            .into_iter()
            .map(str::to_string)
            .collect()
    }

    fn create_integration(
        &self,
        name: &str,
    ) -> Result<Box<dyn IntegrationTrait + Send + Sync>, String> {
        crate::integrations::integration_from_name(name)
    }
}
