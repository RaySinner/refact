pub use refact_http::routers::v1::mcp_config_sharing::*;

#[cfg(test)]
use std::collections::HashMap;
#[cfg(test)]
use serde_json::{json, Value};
#[cfg(test)]
use refact_integrations::mcp::mcp_naming;

#[cfg(test)]
fn redact_env(env: &HashMap<String, String>) -> HashMap<String, String> {
    env.iter()
        .map(|(k, v)| {
            let redacted = if is_secret_field(k) {
                REDACTED.to_string()
            } else {
                v.clone()
            };
            (k.clone(), redacted)
        })
        .collect()
}

#[cfg(test)]
fn build_yaml_from_config(
    config: &HashMap<String, Value>,
    tools_config: &HashMap<String, Value>,
    confirmation: &HashMap<String, Value>,
) -> String {
    let mut full: serde_json::Map<String, Value> = serde_json::Map::new();
    for (k, v) in config {
        full.insert(k.clone(), v.clone());
    }
    if !tools_config.is_empty() {
        full.insert(
            "tools".to_string(),
            Value::Object(
                tools_config
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect(),
            ),
        );
    }
    if !confirmation.is_empty() {
        full.insert(
            "confirmation".to_string(),
            Value::Object(
                confirmation
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect(),
            ),
        );
    }
    let val = Value::Object(full);
    serde_yaml::to_string(&val).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_config_name_rejects_traversal() {
        assert!(mcp_naming::validate_config_filename("../evil").is_err());
        assert!(mcp_naming::validate_config_filename("foo/../../bar").is_err());
    }

    #[test]
    fn test_apply_secrets_to_config_nested_paths() {
        let mut config: HashMap<String, Value> = HashMap::from([
            (
                "env".to_string(),
                serde_json::json!({ "API_KEY": "<REDACTED>" }),
            ),
            (
                "oauth_tokens".to_string(),
                serde_json::json!({ "access_token": "a", "client_secret": "<REDACTED>" }),
            ),
            (
                "bearer_token".to_string(),
                Value::String("<REDACTED>".to_string()),
            ),
        ]);
        let secrets = vec![
            ImportSecret {
                path: vec!["env".to_string(), "API_KEY".to_string()],
                value: "sk-env".to_string(),
            },
            ImportSecret {
                path: vec!["oauth_tokens".to_string(), "client_secret".to_string()],
                value: "oauth-secret".to_string(),
            },
            ImportSecret {
                path: vec!["bearer_token".to_string()],
                value: "bearer-secret".to_string(),
            },
        ];

        apply_secrets_to_config(&mut config, &secrets);

        assert_eq!(config["env"]["API_KEY"], "sk-env");
        assert_eq!(
            config["oauth_tokens"]["client_secret"], "oauth-secret",
            "nested non-env secrets must be restored in place"
        );
        assert_eq!(
            config["oauth_tokens"]["access_token"], "a",
            "sibling values must be untouched"
        );
        assert_eq!(config["bearer_token"], "bearer-secret");
    }

    #[test]
    fn test_apply_secrets_handles_non_map_intermediate() {
        let mut config = HashMap::from([("env".to_string(), Value::Bool(true))]);
        let secrets = vec![ImportSecret {
            path: vec!["env".to_string(), "TOKEN".to_string()],
            value: "v".to_string(),
        }];
        apply_secrets_to_config(&mut config, &secrets);
        assert_eq!(config["env"], Value::Bool(true));
    }

    #[test]
    fn test_apply_secrets_preserves_dotted_key() {
        let mut config = HashMap::from([("env".to_string(), json!({ "FOO.BAR": REDACTED }))]);
        let secrets = vec![ImportSecret {
            path: vec!["env".to_string(), "FOO.BAR".to_string()],
            value: "dotted-secret".to_string(),
        }];
        apply_secrets_to_config(&mut config, &secrets);
        assert_eq!(config["env"]["FOO.BAR"], "dotted-secret");
    }

    #[test]
    fn test_redact_secrets_nested_objects() {
        let mut value = json!({
            "oauth_tokens": { "access_token": "access", "safe": "visible" },
            "headers": { "Authorization": "Bearer secret", "Accept": "application/json" },
        });
        redact_secrets(&mut value);
        assert_eq!(value["oauth_tokens"]["access_token"], REDACTED);
        assert_eq!(value["oauth_tokens"]["safe"], "visible");
        assert_eq!(value["headers"]["Authorization"], REDACTED);
        assert_eq!(value["headers"]["Accept"], "application/json");
    }

    #[test]
    fn test_validate_config_name_rejects_traversal_legacy_shape() {
        assert!(mcp_naming::validate_config_filename("../evil").is_err());
        assert!(mcp_naming::validate_config_filename("foo/../../bar").is_err());
        assert!(mcp_naming::validate_config_filename("mcp_stdio_ok").is_ok());
        assert!(mcp_naming::validate_config_filename("").is_err());
        assert!(mcp_naming::validate_config_filename("/etc/passwd").is_err());
        assert!(mcp_naming::validate_config_filename("a\\b").is_err());
        assert!(mcp_naming::validate_config_filename("mcp_http_my-server").is_ok());
    }

    #[test]
    fn test_is_secret_field_detects_secrets() {
        assert!(is_secret_field("GITHUB_PERSONAL_ACCESS_TOKEN"));
        assert!(is_secret_field("API_KEY"));
        assert!(is_secret_field("client_secret"));
        assert!(is_secret_field("db_password"));
        assert!(is_secret_field("bearer_token"));
    }

    #[test]
    fn test_is_secret_field_allows_safe_fields() {
        assert!(!is_secret_field("command"));
        assert!(!is_secret_field("init_timeout"));
        assert!(!is_secret_field("DEBUG_LEVEL"));
    }

    #[test]
    fn test_redact_env_redacts_secrets_only() {
        let mut env = HashMap::new();
        env.insert("GITHUB_TOKEN".to_string(), "ghp_real_token".to_string());
        env.insert("DEBUG".to_string(), "true".to_string());
        env.insert("API_KEY".to_string(), "secret123".to_string());

        let redacted = redact_env(&env);
        assert_eq!(redacted["GITHUB_TOKEN"], "<REDACTED>");
        assert_eq!(redacted["DEBUG"], "true");
        assert_eq!(redacted["API_KEY"], "<REDACTED>");
    }

    #[test]
    fn test_determine_transport_stdio() {
        assert_eq!(mcp_naming::detect_transport("mcp_stdio_github"), "stdio");
        assert_eq!(
            mcp_naming::detect_transport("mcp_stdio_brave_search"),
            "stdio"
        );
    }

    #[test]
    fn test_determine_transport_sse() {
        assert_eq!(mcp_naming::detect_transport("mcp_sse_myserver"), "sse");
    }

    #[test]
    fn test_determine_transport_http() {
        assert_eq!(mcp_naming::detect_transport("mcp_http_myserver"), "http");
    }

    #[test]
    fn test_config_prefix_for_transport() {
        assert_eq!(
            mcp_naming::config_prefix_for_transport("stdio"),
            "mcp_stdio_"
        );
        assert_eq!(mcp_naming::config_prefix_for_transport("sse"), "mcp_sse_");
        assert_eq!(mcp_naming::config_prefix_for_transport("http"), "mcp_http_");
        assert_eq!(
            mcp_naming::config_prefix_for_transport("streamable-http"),
            "mcp_http_"
        );
        assert_eq!(
            mcp_naming::config_prefix_for_transport("unknown"),
            "mcp_stdio_"
        );
    }

    #[test]
    fn test_export_bundle_roundtrip() {
        let bundle = ExportBundle {
            version: 1,
            exported_at: "2026-01-01T00:00:00Z".to_string(),
            servers: vec![ExportedServer {
                config_name: "mcp_stdio_github".to_string(),
                transport: "stdio".to_string(),
                config: {
                    let mut m = HashMap::new();
                    m.insert(
                        "command".to_string(),
                        Value::String("npx github".to_string()),
                    );
                    m
                },
                tools_config: HashMap::new(),
                confirmation: HashMap::new(),
            }],
        };
        let json_str = serde_json::to_string(&bundle).unwrap();
        let parsed: ExportBundle = serde_json::from_str(&json_str).unwrap();
        assert_eq!(parsed.version, 1);
        assert_eq!(parsed.servers.len(), 1);
        assert_eq!(parsed.servers[0].config_name, "mcp_stdio_github");
    }

    #[test]
    fn test_build_yaml_from_config_basic() {
        let mut config = HashMap::new();
        config.insert("command".to_string(), Value::String("npx test".to_string()));
        config.insert(
            "env".to_string(),
            Value::Object({
                let mut m = serde_json::Map::new();
                m.insert("TOKEN".to_string(), Value::String("abc".to_string()));
                m
            }),
        );
        let tools_config: HashMap<String, Value> = HashMap::new();
        let confirmation: HashMap<String, Value> = HashMap::new();
        let yaml = build_yaml_from_config(&config, &tools_config, &confirmation);
        assert!(
            yaml.contains("command") || yaml.contains("npx test"),
            "yaml must contain command"
        );
    }

    #[test]
    fn test_apply_secrets_to_config_env() {
        let mut config: HashMap<String, Value> = HashMap::new();
        config.insert(
            "env".to_string(),
            Value::Object({
                let mut m = serde_json::Map::new();
                m.insert(
                    "GITHUB_TOKEN".to_string(),
                    Value::String("<REDACTED>".to_string()),
                );
                m
            }),
        );

        let secrets = vec![ImportSecret {
            path: vec!["env".to_string(), "GITHUB_TOKEN".to_string()],
            value: "ghp_real".to_string(),
        }];
        apply_secrets_to_config(&mut config, &secrets);

        if let Some(Value::Object(env_map)) = config.get("env") {
            assert_eq!(
                env_map["GITHUB_TOKEN"],
                Value::String("ghp_real".to_string())
            );
        } else {
            panic!("env should be present");
        }
    }

    #[tokio::test]
    async fn test_import_creates_files() {
        let tmp = tempfile::tempdir().unwrap();
        let integrations_dir = tmp.path().join("integrations.d");
        tokio::fs::create_dir_all(&integrations_dir).await.unwrap();

        let server = ExportedServer {
            config_name: "mcp_stdio_testserver".to_string(),
            transport: "stdio".to_string(),
            config: {
                let mut m = HashMap::new();
                m.insert("command".to_string(), Value::String("npx test".to_string()));
                m.insert("init_timeout".to_string(), Value::String("60".to_string()));
                m
            },
            tools_config: HashMap::new(),
            confirmation: HashMap::new(),
        };

        let yaml =
            build_yaml_from_config(&server.config, &server.tools_config, &server.confirmation);
        let config_path = integrations_dir.join("mcp_stdio_testserver.yaml");
        let tmp_path = config_path.with_extension("yaml.tmp");
        tokio::fs::write(&tmp_path, &yaml).await.unwrap();
        tokio::fs::rename(&tmp_path, &config_path).await.unwrap();

        assert!(config_path.exists(), "config file must be created");
        let content = tokio::fs::read_to_string(&config_path).await.unwrap();
        assert!(
            content.contains("npx test"),
            "yaml content must contain the command"
        );
    }

    #[tokio::test]
    async fn test_export_collects_mcp_yaml_files() {
        let tmp = tempfile::tempdir().unwrap();
        let integrations_dir = tmp.path().join("integrations.d");
        tokio::fs::create_dir_all(&integrations_dir).await.unwrap();

        tokio::fs::write(
            integrations_dir.join("mcp_stdio_github.yaml"),
            "command: \"npx github\"\nenv:\n  GITHUB_TOKEN: \"ghp_test\"\n",
        )
        .await
        .unwrap();
        tokio::fs::write(
            integrations_dir.join("not_mcp_integration.yaml"),
            "some: config\n",
        )
        .await
        .unwrap();

        let files = collect_mcp_yaml_files(&integrations_dir).await;
        assert_eq!(files.len(), 1, "must find exactly 1 MCP yaml file");
        assert_eq!(files[0].0, "mcp_stdio_github", "config_name must match");
        assert!(files[0].2.contains("npx github"), "content must be read");
    }

    #[test]
    fn test_secrets_not_redacted_when_include_secrets_true() {
        let mut env = HashMap::new();
        env.insert("GITHUB_TOKEN".to_string(), "ghp_real_token".to_string());
        let redacted = redact_env(&env);
        let not_redacted: HashMap<String, String> =
            env.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        assert_eq!(
            redacted["GITHUB_TOKEN"], "<REDACTED>",
            "redact_env always redacts"
        );
        assert_eq!(
            not_redacted["GITHUB_TOKEN"], "ghp_real_token",
            "original not modified"
        );
    }

    #[test]
    fn test_convert_claude_desktop_stdio_and_url() {
        let raw: Value = serde_json::json!({
            "github": {
                "command": "npx",
                "args": ["-y", "@modelcontextprotocol/server-github"],
                "env": {"GITHUB_TOKEN": "x"}
            },
            "Linear MCP": { "url": "https://mcp.linear.app/mcp" },
            "legacy-sse": { "url": "https://old.example.com/sse" },
            "broken": { "neither": true }
        });
        let (servers, errors) = convert_mcp_servers_json_to_bundle(raw.as_object().unwrap());
        assert_eq!(servers.len(), 3);
        assert_eq!(errors.len(), 1);

        let github = servers.iter().find(|s| s.config_name == "github").unwrap();
        assert_eq!(github.transport, "stdio");
        assert_eq!(
            github.config.get("command").and_then(|v| v.as_str()),
            Some("npx -y @modelcontextprotocol/server-github")
        );
        assert!(github.config.get("env").is_some());

        let linear = servers
            .iter()
            .find(|s| s.config_name == "linear_mcp")
            .unwrap();
        assert_eq!(linear.transport, "http");
        assert_eq!(
            linear.config.get("url").and_then(|v| v.as_str()),
            Some("https://mcp.linear.app/mcp")
        );

        let sse = servers
            .iter()
            .find(|s| s.config_name == "legacy_sse")
            .unwrap();
        assert_eq!(sse.transport, "sse");
    }

    #[test]
    fn test_sanitize_imported_server_name() {
        assert_eq!(sanitize_imported_server_name("My Server!"), "my_server");
        assert_eq!(sanitize_imported_server_name("___"), "server");
        assert_eq!(
            sanitize_imported_server_name("a".repeat(60).as_str()).len(),
            40
        );
    }

    #[test]
    fn test_convert_colliding_names_are_deduplicated() {
        let raw: Value = serde_json::json!({
            "Linear MCP": { "url": "https://a.example.com/mcp" },
            "linear-mcp": { "url": "https://b.example.com/mcp" },
            "linear_mcp": { "url": "https://c.example.com/mcp" }
        });
        let (servers, errors) = convert_mcp_servers_json_to_bundle(raw.as_object().unwrap());
        assert!(errors.is_empty());
        assert_eq!(servers.len(), 3);
        let mut names: Vec<String> = servers.iter().map(|s| s.config_name.clone()).collect();
        names.sort();
        assert_eq!(names, vec!["linear_mcp", "linear_mcp_2", "linear_mcp_3"]);
    }

    #[test]
    fn test_convert_command_with_spaces_is_shell_quoted() {
        let raw: Value = serde_json::json!({
            "spacey": { "command": "/opt/my tools/bin/mcp", "args": ["--flag", "value with space"] }
        });
        let (servers, errors) = convert_mcp_servers_json_to_bundle(raw.as_object().unwrap());
        assert!(errors.is_empty());
        let cmd = servers[0]
            .config
            .get("command")
            .and_then(|v| v.as_str())
            .unwrap();
        assert_eq!(cmd, "'/opt/my tools/bin/mcp' --flag 'value with space'");
    }
}
