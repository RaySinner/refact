use crate::custom_error::MapErrToString;

pub use refact_tool_api::{
    command_should_be_confirmed_by_user, command_should_be_confirmed_by_user_segment_aware,
    command_should_be_denied, command_should_be_denied_segment_aware, extract_command_segments,
    is_strict_compatible, json_schema_from_params, make_openai_tool_value, structural_flags,
    MatchConfirmDeny, MatchConfirmDenyResult, ToolConfig, ToolDesc, ToolGroupCategory, ToolSource,
    ToolSourceType,
};

pub use refact_runtime_api::{Tool, ToolGroup, WaitInterrupt};

pub async fn set_tool_config(
    config_path: String,
    tool_name: String,
    new_config: ToolConfig,
) -> Result<(), String> {
    let config_file = tokio::fs::read_to_string(&config_path)
        .await
        .map_err(|e| format!("Error reading config file: {}", e))?;

    let mut config: serde_yaml::Mapping = serde_yaml::from_str(&config_file)
        .map_err(|e| format!("Error parsing config file: {}", e))?;

    let tools: &mut serde_yaml::Mapping = match config
        .get_mut("tools")
        .and_then(|tools| tools.as_mapping_mut())
    {
        Some(tools) => tools,
        None => {
            config.insert(
                serde_yaml::Value::String("tools".to_string()),
                serde_yaml::Value::Mapping(serde_yaml::Mapping::new()),
            );
            config
                .get_mut("tools")
                .expect("tools was just inserted")
                .as_mapping_mut()
                .expect("tools is a mapping, it was just inserted")
        }
    };

    tools.insert(
        serde_yaml::Value::String(tool_name),
        serde_yaml::to_value(new_config)
            .map_err_with_prefix("ToolConfig should always be serializable.")?,
    );

    tokio::fs::write(config_path, serde_yaml::to_string(&config).unwrap())
        .await
        .map_err(|e| format!("Error writing config file: {}", e))?;

    Ok(())
}

pub use refact_chat_api::WaitInterruptCause;
