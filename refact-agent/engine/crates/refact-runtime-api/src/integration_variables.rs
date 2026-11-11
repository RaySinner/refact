use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use refact_core::custom_error::YamlError;
use refact_files::path_utils::canonical_path;

use crate::global_context::GlobalContext;

pub async fn get_vars_for_replacements(
    gcx: Arc<GlobalContext>,
    error_log: &mut Vec<YamlError>,
) -> HashMap<String, String> {
    let (config_dir, variables_yaml, secrets_yaml) = {
        (
            gcx.config_dir.clone(),
            gcx.cmdline.variables_yaml.clone(),
            gcx.cmdline.secrets_yaml.clone(),
        )
    };
    let (variables_yaml_path, secrets_yaml_path) =
        resolve_yaml_paths(&config_dir, &variables_yaml, &secrets_yaml);
    load_variables_from_paths(&variables_yaml_path, &secrets_yaml_path, error_log).await
}

fn resolve_yaml_paths(
    config_dir: &PathBuf,
    variables_yaml: &str,
    secrets_yaml: &str,
) -> (PathBuf, PathBuf) {
    let variables_yaml_path = if variables_yaml.is_empty() {
        config_dir.join("variables.yaml")
    } else {
        canonical_path(variables_yaml)
    };
    let secrets_yaml_path = if secrets_yaml.is_empty() {
        config_dir.join("secrets.yaml")
    } else {
        canonical_path(secrets_yaml)
    };
    (variables_yaml_path, secrets_yaml_path)
}

async fn load_variables_from_paths(
    variables_yaml_path: &PathBuf,
    secrets_yaml_path: &PathBuf,
    error_log: &mut Vec<YamlError>,
) -> HashMap<String, String> {
    let mut variables = HashMap::new();

    // Helper function to read and parse a YAML file
    async fn read_and_parse_yaml(
        path: &PathBuf,
        error_log: &mut Vec<YamlError>,
    ) -> Result<HashMap<String, String>, ()> {
        if !path.exists() {
            return Ok(HashMap::new());
        }

        match fs::read_to_string(path) {
            Ok(content) => match serde_yaml::from_str::<HashMap<String, String>>(&content) {
                Ok(parsed_yaml) => Ok(parsed_yaml),
                Err(e) => {
                    tracing::warn!("Failed to parse {}: {}", path.display(), e);
                    error_log.push(YamlError {
                        path: path.to_string_lossy().to_string(),
                        error_line: e.location().map(|loc| loc.line()).unwrap_or(0),
                        error_msg: format!("Failed to parse {}: {}", path.display(), e),
                    });
                    Err(())
                }
            },
            Err(e) => {
                tracing::info!("Failed to read {}: {}", path.display(), e);
                error_log.push(YamlError {
                    path: path.to_string_lossy().to_string(),
                    error_line: 0,
                    error_msg: format!("Failed to read {}: {}", path.display(), e),
                });
                Err(())
            }
        }
    }

    // Read and parse secrets.yaml
    if let Ok(secrets_yaml) = read_and_parse_yaml(secrets_yaml_path, error_log).await {
        variables.extend(secrets_yaml);
    }

    // Read and parse variables.yaml
    if let Ok(variables_yaml) = read_and_parse_yaml(variables_yaml_path, error_log).await {
        variables.extend(variables_yaml);
    }

    variables
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn write_yaml(dir: &Path, name: &str, content: &str) -> PathBuf {
        let path = dir.join(name);
        fs::write(&path, content).unwrap();
        path
    }

    fn assert_same_file(left: &Path, right: &Path) {
        assert_eq!(left.canonicalize().unwrap(), right.canonicalize().unwrap());
    }

    #[tokio::test]
    async fn defaults_read_variables_and_secrets_from_config_dir() {
        let dir = tempfile::tempdir().unwrap();
        write_yaml(dir.path(), "variables.yaml", "VAR_ONLY: from-variables\n");
        write_yaml(dir.path(), "secrets.yaml", "SECRET_ONLY: from-secrets\n");

        let (variables_path, secrets_path) = resolve_yaml_paths(&dir.path().to_path_buf(), "", "");
        assert_same_file(&variables_path, &dir.path().join("variables.yaml"));
        assert_same_file(&secrets_path, &dir.path().join("secrets.yaml"));

        let mut error_log = Vec::new();
        let vars =
            load_variables_from_paths(&variables_path, &secrets_path, &mut error_log).await;
        assert!(error_log.is_empty());
        assert_eq!(
            vars.get("VAR_ONLY").map(String::as_str),
            Some("from-variables")
        );
        assert_eq!(
            vars.get("SECRET_ONLY").map(String::as_str),
            Some("from-secrets")
        );
    }

    #[tokio::test]
    async fn explicit_absolute_paths_override_config_dir_defaults() {
        let config_dir = tempfile::tempdir().unwrap();
        let explicit_dir = tempfile::tempdir().unwrap();
        write_yaml(config_dir.path(), "variables.yaml", "SHOULD_NOT_LOAD: config\n");
        let explicit_vars = write_yaml(explicit_dir.path(), "my-vars.yaml", "VAR: explicit-vars\n");
        let explicit_secrets =
            write_yaml(explicit_dir.path(), "my-secrets.yaml", "SECRET: explicit-secrets\n");

        let (variables_path, secrets_path) = resolve_yaml_paths(
            &config_dir.path().to_path_buf(),
            explicit_vars.to_string_lossy().as_ref(),
            explicit_secrets.to_string_lossy().as_ref(),
        );
        assert_same_file(&variables_path, &explicit_vars);
        assert_same_file(&secrets_path, &explicit_secrets);

        let mut error_log = Vec::new();
        let vars =
            load_variables_from_paths(&variables_path, &secrets_path, &mut error_log).await;
        assert!(error_log.is_empty());
        assert_eq!(vars.get("VAR").map(String::as_str), Some("explicit-vars"));
        assert_eq!(
            vars.get("SECRET").map(String::as_str),
            Some("explicit-secrets")
        );
        assert!(vars.get("SHOULD_NOT_LOAD").is_none());
    }

    #[tokio::test]
    async fn explicit_paths_normalize_parent_components() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("sub")).unwrap();
        write_yaml(dir.path(), "variables.yaml", "VAR: v\n");
        write_yaml(dir.path(), "secrets.yaml", "SECRET: s\n");
        let twisted = dir.path().join("sub").join("..").join("variables.yaml");

        let (variables_path, secrets_path) =
            resolve_yaml_paths(&dir.path().to_path_buf(), twisted.to_string_lossy().as_ref(), "");
        assert_same_file(&variables_path, &dir.path().join("variables.yaml"));
        assert_same_file(&secrets_path, &dir.path().join("secrets.yaml"));
    }

    #[test]
    fn explicit_relative_path_is_not_joined_onto_config_dir() {
        let config_dir = tempfile::tempdir().unwrap();
        let (variables_path, _secrets_path) =
            resolve_yaml_paths(&config_dir.path().to_path_buf(), "variables.yaml", "");
        assert_ne!(variables_path, config_dir.path().join("variables.yaml"));
    }

    #[tokio::test]
    async fn variables_win_over_secrets_on_key_collision() {
        let dir = tempfile::tempdir().unwrap();
        let secrets = write_yaml(
            dir.path(),
            "secrets.yaml",
            "TOKEN: from-secrets\nSECRET_ONLY: s\n",
        );
        let variables = write_yaml(
            dir.path(),
            "variables.yaml",
            "TOKEN: from-variables\nVAR_ONLY: v\n",
        );

        let mut error_log = Vec::new();
        let vars = load_variables_from_paths(&variables, &secrets, &mut error_log).await;
        assert!(error_log.is_empty());
        assert_eq!(vars.get("TOKEN").map(String::as_str), Some("from-variables"));
        assert_eq!(vars.get("SECRET_ONLY").map(String::as_str), Some("s"));
        assert_eq!(vars.get("VAR_ONLY").map(String::as_str), Some("v"));
    }

    #[tokio::test]
    async fn missing_yaml_files_are_tolerated() {
        let dir = tempfile::tempdir().unwrap();
        let variables = dir.path().join("missing-vars.yaml");
        let secrets = dir.path().join("missing-secrets.yaml");

        let mut error_log = Vec::new();
        let vars = load_variables_from_paths(&variables, &secrets, &mut error_log).await;
        assert!(error_log.is_empty());
        assert!(vars.is_empty());
    }

    #[tokio::test]
    async fn malformed_yaml_reports_path_line_and_message() {
        let dir = tempfile::tempdir().unwrap();
        let broken = write_yaml(dir.path(), "broken.yaml", "foo: bar\nbaz: [1, 2\n");
        let absent = dir.path().join("absent.yaml");

        let mut error_log = Vec::new();
        let vars = load_variables_from_paths(&absent, &broken, &mut error_log).await;
        assert!(vars.is_empty());
        assert_eq!(error_log.len(), 1);
        assert_eq!(error_log[0].path, broken.to_string_lossy().to_string());
        assert!(error_log[0].error_line > 0);
        assert!(error_log[0].error_msg.starts_with("Failed to parse "));
    }

    #[tokio::test]
    async fn unreadable_yaml_path_reports_read_error_with_zero_line() {
        let dir = tempfile::tempdir().unwrap();
        let mut error_log = Vec::new();
        let vars = load_variables_from_paths(
            &dir.path().join("absent.yaml"),
            &dir.path().to_path_buf(),
            &mut error_log,
        )
        .await;
        assert!(vars.is_empty());
        assert_eq!(error_log.len(), 1);
        assert_eq!(error_log[0].error_line, 0);
        assert!(error_log[0].error_msg.starts_with("Failed to read "));
    }

    #[tokio::test]
    async fn secrets_are_processed_before_variables() {
        let secrets_dir = tempfile::tempdir().unwrap();
        let variables_dir = tempfile::tempdir().unwrap();
        let mut error_log = Vec::new();
        let vars = load_variables_from_paths(
            &variables_dir.path().to_path_buf(),
            &secrets_dir.path().to_path_buf(),
            &mut error_log,
        )
        .await;
        assert!(vars.is_empty());
        assert_eq!(error_log.len(), 2);
        assert_eq!(
            error_log[0].path,
            secrets_dir.path().to_string_lossy().to_string()
        );
        assert_eq!(
            error_log[1].path,
            variables_dir.path().to_string_lossy().to_string()
        );
    }
}
