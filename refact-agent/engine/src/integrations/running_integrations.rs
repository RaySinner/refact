use std::sync::Arc;
use indexmap::IndexMap;

use async_trait::async_trait;

use crate::custom_error::YamlError;
use crate::global_context::GlobalContext;
use crate::integrations::integr_abstract::IntegrationTrait;

use refact_integrations::running::{IntegrationRecordInput, IntegrationSetupSink};

/// Loads and set up integrations from config files.
///
/// If `include_paths_matching` is `None`, all integrations are loaded,
/// otherwise only those matching `include_paths_matching` glob patterns.
pub async fn load_integrations(
    gcx: Arc<GlobalContext>,
    include_paths_matching: &[String],
) -> (
    IndexMap<String, Box<dyn IntegrationTrait + Send + Sync>>,
    Vec<YamlError>,
) {
    let active_project_path = refact_runtime_api::files_correction::get_active_project_path(gcx.clone()).await;
    let (config_dirs, global_config_dir) =
        crate::integrations::setting_up_integrations::get_config_dirs(
            gcx.clone(),
            &active_project_path,
        )
        .await;
    let (integrations_yaml_path, is_inside_container, allow_experimental) = {
        (
            gcx.cmdline.integrations_yaml.clone(),
            gcx.cmdline.inside_container,
            gcx.cmdline.experimental,
        )
    };

    let mut error_log: Vec<YamlError> = Vec::new();
    let lst: Vec<&str> = crate::integrations::integrations_list(allow_experimental);
    let vars_for_replacements =
        crate::integrations::setting_up_integrations::get_vars_for_replacements(
            gcx.clone(),
            &mut error_log,
        )
        .await;
    let records = crate::integrations::setting_up_integrations::read_integrations_d(
        &config_dirs,
        &global_config_dir,
        &integrations_yaml_path,
        &vars_for_replacements,
        &lst,
        &mut error_log,
        include_paths_matching,
        false,
    );

    let record_inputs: Vec<IntegrationRecordInput> = records
        .into_iter()
        .map(|rec| IntegrationRecordInput {
            integr_name: rec.integr_name,
            integr_config_path: rec.integr_config_path,
            integr_config_exists: rec.integr_config_exists,
            on_your_laptop: rec.on_your_laptop,
            when_isolated: rec.when_isolated,
            config_unparsed: rec.config_unparsed,
        })
        .collect();

    let mut sink = IntegrationMapSink {
        gcx: gcx.clone(),
        integrations_map: IndexMap::new(),
    };
    let load_errors = refact_integrations::running::load_integrations(
        record_inputs,
        is_inside_container,
        &mut sink,
    )
    .await;
    error_log.extend(load_errors);

    (sink.integrations_map, error_log)
}

struct IntegrationMapSink {
    gcx: Arc<GlobalContext>,
    integrations_map: IndexMap<String, Box<dyn IntegrationTrait + Send + Sync>>,
}

#[async_trait]
impl IntegrationSetupSink for IntegrationMapSink {
    async fn setup_integration(
        &mut self,
        integr_name: &str,
        integr_config_path: String,
        config_unparsed: &serde_json::Value,
    ) -> Result<(), serde_json::Error> {
        let mut integr = match crate::integrations::integration_from_name(integr_name) {
            Ok(x) => x,
            Err(e) => {
                tracing::error!("don't have integration {}: {}", integr_name, e);
                return Ok(());
            }
        };
        integr
            .integr_settings_apply(self.gcx.clone(), integr_config_path.clone(), config_unparsed)
            .await?;
        self.integrations_map
            .insert(integr_name.to_string(), integr);
        Ok(())
    }
}
