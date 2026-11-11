use std::collections::HashMap;

use async_trait::async_trait;
use refact_core::custom_error::YamlError;
use serde::de::Error as _;
use serde_json::Value;

#[derive(Debug, Clone, Default)]
pub struct IntegrationRecordInput {
    pub integr_name: String,
    pub integr_config_path: String,
    pub integr_config_exists: bool,
    pub on_your_laptop: bool,
    pub when_isolated: bool,
    pub config_unparsed: Value,
}

#[async_trait]
pub trait IntegrationSetupSink: Send {
    async fn setup_integration(
        &mut self,
        integr_name: &str,
        integr_config_path: String,
        config_unparsed: &Value,
    ) -> Result<(), serde_json::Error>;
}

/// Loads and set up integrations from config records.
///
/// Records whose config file is missing, or whose availability flags do not match
/// `inside_container`, are skipped. Every surviving record is handed to the sink;
/// a failed setup is reported as a [`YamlError`].
pub async fn load_integrations(
    records: Vec<IntegrationRecordInput>,
    inside_container: bool,
    sink: &mut dyn IntegrationSetupSink,
) -> Vec<YamlError> {
    let mut error_log: Vec<YamlError> = Vec::new();

    for rec in records {
        if !inside_container && !rec.on_your_laptop {
            continue;
        }
        if inside_container && !rec.when_isolated {
            continue;
        }
        if !rec.integr_config_exists {
            continue;
        }
        if let Err(err) = sink
            .setup_integration(&rec.integr_name, rec.integr_config_path.clone(), &rec.config_unparsed)
            .await
        {
            let error_line = err.line();
            error_log.push(YamlError {
                path: rec.integr_config_path.clone(),
                error_line,
                error_msg: format!("failed to apply settings: {}", err),
            });
        }
    }

    for e in error_log.iter() {
        tracing::error!("{e}");
    }

    error_log
}

/// Minimal in-memory sink used by tests: records the (name, path) pairs it accepted
/// and the errors it was told to fail with.
#[derive(Default)]
pub struct RecordingSink {
    pub accepted: Vec<(String, String)>,
    pub fail_for: HashMap<String, String>,
}

#[async_trait]
impl IntegrationSetupSink for RecordingSink {
    async fn setup_integration(
        &mut self,
        integr_name: &str,
        integr_config_path: String,
        _config_unparsed: &Value,
    ) -> Result<(), serde_json::Error> {
        if let Some(msg) = self.fail_for.get(integr_name) {
            return Err(serde::de::Error::custom(msg.clone()));
        }
        self.accepted
            .push((integr_name.to_string(), integr_config_path));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(
        name: &str,
        exists: bool,
        on_laptop: bool,
        when_isolated: bool,
    ) -> IntegrationRecordInput {
        IntegrationRecordInput {
            integr_name: name.to_string(),
            integr_config_path: format!("/cfg/{}.yaml", name),
            integr_config_exists: exists,
            on_your_laptop: on_laptop,
            when_isolated: when_isolated,
            config_unparsed: Value::Null,
        }
    }

    #[tokio::test]
    async fn skips_missing_configs_and_mismatched_availability() {
        let records = vec![
            record("a", true, true, true),
            record("b", false, true, true),
            record("c", true, false, true),
            record("d", true, true, false),
        ];
        let mut sink = RecordingSink::default();
        let errors = load_integrations(records.clone(), false, &mut sink).await;
        assert!(errors.is_empty());
        assert_eq!(
            sink.accepted,
            vec![("a".to_string(), "/cfg/a.yaml".to_string())]
        );

        let mut sink = RecordingSink::default();
        let errors = load_integrations(records, true, &mut sink).await;
        assert!(errors.is_empty());
        assert_eq!(
            sink.accepted,
            vec![("a".to_string(), "/cfg/a.yaml".to_string())]
        );
    }

    #[tokio::test]
    async fn failed_setup_is_reported_with_path_line_and_message() {
        let records = vec![record("bad", true, true, true)];
        let mut sink = RecordingSink::default();
        sink.fail_for
            .insert("bad".to_string(), "wrong field".to_string());
        let errors = load_integrations(records, false, &mut sink).await;
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].path, "/cfg/bad.yaml");
        assert_eq!(errors[0].error_msg, "failed to apply settings: wrong field");
    }
}
