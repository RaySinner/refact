pub use refact_runtime_api::integrations::setting_up_integrations::*;

#[cfg(test)]
mod tests {
    // use super::*;
    use crate::integrations::yaml_schema::ISchema;
    use serde_yaml;
    use std::fs::File;
    use std::io::Write;

    fn yaml_key(name: &str) -> serde_yaml::Value {
        serde_yaml::Value::String(name.to_string())
    }

    fn access_token_of(value: &serde_yaml::Value) -> Option<String> {
        value
            .as_mapping()?
            .get(&yaml_key("oauth_tokens"))?
            .as_mapping()?
            .get(&yaml_key("access_token"))?
            .as_str()
            .map(|s| s.to_string())
    }

    #[test]
    fn preserve_unknown_fields_restores_omitted_tokens() {
        let existing = "url: https://mcp.example.com/mcp\noauth_tokens:\n  access_token: kept\n";
        let mut new_value: serde_yaml::Value =
            serde_yaml::from_str("url: https://mcp.example.com/mcp\n").unwrap();
        super::preserve_unknown_fields(
            std::path::Path::new("/tmp/x.yaml"),
            existing,
            &mut new_value,
        );
        assert_eq!(access_token_of(&new_value).as_deref(), Some("kept"));
    }

    #[test]
    fn preserve_unknown_fields_respects_caller_supplied_tokens() {
        let existing = "url: https://mcp.example.com/mcp\noauth_tokens:\n  access_token: old\n";
        let mut new_value: serde_yaml::Value = serde_yaml::from_str(
            "url: https://mcp.example.com/mcp\noauth_tokens:\n  access_token: fresh\n",
        )
        .unwrap();
        super::preserve_unknown_fields(
            std::path::Path::new("/tmp/x.yaml"),
            existing,
            &mut new_value,
        );
        assert_eq!(access_token_of(&new_value).as_deref(), Some("fresh"));
    }

    #[test]
    fn preserve_unknown_fields_skips_non_remote_transport() {
        let existing = "url: https://mcp.example.com/mcp\noauth_tokens:\n  access_token: old\n";
        let mut new_value: serde_yaml::Value =
            serde_yaml::from_str("command: uvx mcp-server-fetch\n").unwrap();
        super::preserve_unknown_fields(
            std::path::Path::new("/tmp/x.yaml"),
            existing,
            &mut new_value,
        );
        assert_eq!(access_token_of(&new_value), None);
    }

    #[test]
    fn preserve_unknown_fields_ignores_malformed_existing_config() {
        let mut new_value: serde_yaml::Value =
            serde_yaml::from_str("url: https://mcp.example.com/mcp\n").unwrap();
        super::preserve_unknown_fields(
            std::path::Path::new("/tmp/x.yaml"),
            "\tnot: [valid",
            &mut new_value,
        );
        assert_eq!(access_token_of(&new_value), None);
    }

    #[test]
    fn preserve_unknown_fields_keeps_arbitrary_engine_managed_keys() {
        let existing = "url: https://mcp.example.com/mcp\nsome_future_field:\n  nested: 7\nplain_scalar: kept\n";
        let mut new_value: serde_yaml::Value =
            serde_yaml::from_str("url: https://mcp.example.com/mcp\n").unwrap();
        super::preserve_unknown_fields(
            std::path::Path::new("/tmp/x.yaml"),
            existing,
            &mut new_value,
        );
        let map = new_value.as_mapping().unwrap();
        assert_eq!(
            map.get(&yaml_key("some_future_field"))
                .and_then(|v| v.as_mapping())
                .and_then(|m| m.get(&yaml_key("nested")))
                .and_then(|v| v.as_u64()),
            Some(7)
        );
        assert_eq!(
            map.get(&yaml_key("plain_scalar")).and_then(|v| v.as_str()),
            Some("kept")
        );
    }

    #[test]
    fn preserve_unknown_fields_does_not_override_caller_supplied_keys() {
        let existing = "url: https://old.example.com/mcp\nextra: old\n";
        let mut new_value: serde_yaml::Value =
            serde_yaml::from_str("url: https://new.example.com/mcp\nextra: new\n").unwrap();
        super::preserve_unknown_fields(
            std::path::Path::new("/tmp/x.yaml"),
            existing,
            &mut new_value,
        );
        let map = new_value.as_mapping().unwrap();
        assert_eq!(
            map.get(&yaml_key("url")).and_then(|v| v.as_str()),
            Some("https://new.example.com/mcp")
        );
        assert_eq!(
            map.get(&yaml_key("extra")).and_then(|v| v.as_str()),
            Some("new")
        );
    }

    #[test]
    fn preserve_unknown_fields_never_restores_transport_selectors() {
        let existing = "url: https://mcp.example.com/mcp\ncommand: old-cmd\nnote: keep-me\n";
        let mut new_value: serde_yaml::Value =
            serde_yaml::from_str("command: uvx mcp-server-fetch\n").unwrap();
        super::preserve_unknown_fields(
            std::path::Path::new("/tmp/x.yaml"),
            existing,
            &mut new_value,
        );
        let map = new_value.as_mapping().unwrap();
        assert!(map.get(&yaml_key("url")).is_none());
        assert_eq!(
            map.get(&yaml_key("command")).and_then(|v| v.as_str()),
            Some("uvx mcp-server-fetch")
        );
        assert_eq!(
            map.get(&yaml_key("note")).and_then(|v| v.as_str()),
            Some("keep-me")
        );
    }

    #[test]
    fn preserve_unknown_fields_purges_caller_supplied_tokens_on_non_remote() {
        let existing = "url: https://mcp.example.com/mcp\n";
        let mut new_value: serde_yaml::Value = serde_yaml::from_str(
            "command: uvx mcp-server-fetch\noauth_tokens:\n  access_token: supplied\n",
        )
        .unwrap();
        super::preserve_unknown_fields(
            std::path::Path::new("/tmp/x.yaml"),
            existing,
            &mut new_value,
        );
        assert_eq!(access_token_of(&new_value), None);
    }

    #[test]
    fn leading_marketplace_comments_survives_other_leading_comments() {
        let existing =
            "# hand written note\n# mcp_marketplace_source: refact-bundled\ncommand: x\n";
        assert_eq!(
            super::leading_marketplace_comments(existing),
            "# mcp_marketplace_source: refact-bundled\n"
        );
    }

    #[test]
    fn preserve_unknown_fields_purges_stale_tokens_but_keeps_other_keys() {
        let existing = "url: https://mcp.example.com/mcp\noauth_tokens:\n  access_token: dead\nnote: keep-me\n";
        let mut new_value: serde_yaml::Value =
            serde_yaml::from_str("command: uvx mcp-server-fetch\nurl: ''\n").unwrap();
        super::preserve_unknown_fields(
            std::path::Path::new("/tmp/x.yaml"),
            existing,
            &mut new_value,
        );
        assert_eq!(access_token_of(&new_value), None);
        assert_eq!(
            new_value
                .as_mapping()
                .unwrap()
                .get(&yaml_key("note"))
                .and_then(|v| v.as_str()),
            Some("keep-me")
        );
    }

    #[test]
    fn leading_marketplace_comments_captures_only_the_header() {
        let existing = "# mcp_marketplace_source: refact-bundled\n# mcp_marketplace_server: fetch\ncommand: uvx mcp-server-fetch\n# mcp_marketplace_recipe_hash: late\n";
        assert_eq!(
            super::leading_marketplace_comments(existing),
            "# mcp_marketplace_source: refact-bundled\n# mcp_marketplace_server: fetch\n"
        );
        assert_eq!(super::leading_marketplace_comments("url: https://x\n"), "");
    }

    #[tokio::test]
    async fn write_config_atomically_replaces_contents_without_residue() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcp_thing.yaml");
        std::fs::write(&path, "old: 1\n").unwrap();
        super::write_config_atomically(&path, "new: 2\n")
            .await
            .unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "new: 2\n");
        assert!(!dir.path().join("mcp_thing.tmp").exists());
    }

    #[tokio::test]
    async fn test_integration_schemas() {
        let integrations = crate::integrations::integrations_list(true);
        for name in integrations {
            let integration_box = crate::integrations::integration_from_name(name).unwrap();
            let schema_json = {
                let y: serde_yaml::Value =
                    serde_yaml::from_str(integration_box.integr_schema()).unwrap();
                let j = serde_json::to_value(y).unwrap();
                j
            };
            let schema_yaml: serde_yaml::Value =
                serde_json::from_value(schema_json.clone()).unwrap();
            let compare_me1 = serde_yaml::to_string(&schema_yaml).unwrap();
            eprintln!("schema_json {:?}", schema_json);
            let schema_struct: ISchema = serde_json::from_value(schema_json).unwrap();
            let schema_struct_yaml = serde_json::to_value(&schema_struct).unwrap();
            let compare_me2 = serde_yaml::to_string(&schema_struct_yaml).unwrap();
            if compare_me1 != compare_me2 {
                eprintln!(
                    "schema mismatch for integration `{}`:\nOriginal:\n{}\nSerialized:\n{}",
                    name, compare_me1, compare_me2
                );
                let original_file_path = format!("/tmp/original_schema_{}.yaml", name);
                let serialized_file_path = format!("/tmp/serialized_schema_{}.yaml", name);
                let mut original_file = File::create(&original_file_path).unwrap();
                let mut serialized_file = File::create(&serialized_file_path).unwrap();
                original_file.write_all(compare_me1.as_bytes()).unwrap();
                serialized_file.write_all(compare_me2.as_bytes()).unwrap();
                eprintln!("cat {}", original_file_path);
                eprintln!("cat {}", serialized_file_path);
                eprintln!("diff {} {}", original_file_path, serialized_file_path);
                panic!("oops");
            }
        }
    }
}
