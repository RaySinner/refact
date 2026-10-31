use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use refact_core::chat_types::{ChatUsage, ContextEnum};
use refact_tool_api::{
    command_should_be_confirmed_by_user_segment_aware, command_should_be_denied_segment_aware,
    extract_command_segments, structural_flags, IntegrationConfirmation, MatchConfirmDeny,
    MatchConfirmDenyResult, ToolConfig, ToolDesc, ToolGroupCategory,
};
use serde_json::Value;
use tokio::sync::Mutex as AMutex;

use crate::at_commands_ctx::AtCommandsContext;

pub struct ToolGroup {
    pub name: String,
    pub description: String,
    pub category: ToolGroupCategory,
    pub tools: Vec<Box<dyn Tool + Send>>,
}

#[async_trait]
pub trait Tool: Send + Sync {
    async fn tool_execute(
        &mut self,
        ccx: Arc<AMutex<AtCommandsContext>>,
        tool_call_id: &String,
        args: &HashMap<String, Value>,
    ) -> Result<(bool, Vec<ContextEnum>), String>;

    fn tool_description(&self) -> ToolDesc;

    async fn match_against_confirm_deny(
        &self,
        ccx: Arc<AMutex<AtCommandsContext>>,
        args: &HashMap<String, Value>,
    ) -> Result<MatchConfirmDeny, String> {
        let command_to_match = self
            .command_to_match_against_confirm_deny(ccx.clone(), &args)
            .await
            .map_err(|e| format!("Error getting tool command to match: {}", e))?;

        if !command_to_match.is_empty() {
            if let Some(rules) = &self.confirm_deny_rules() {
                tracing::info!(
                    "confirmation: match {:?} against {:?}",
                    command_to_match,
                    rules
                );
                let (is_denied, deny_rule) =
                    command_should_be_denied_segment_aware(&command_to_match, &rules.deny);
                if is_denied {
                    return Ok(MatchConfirmDeny {
                        result: MatchConfirmDenyResult::DENY,
                        command: command_to_match.clone(),
                        rule: deny_rule.clone(),
                    });
                }
                let (needs_confirmation, confirmation_rule) =
                    command_should_be_confirmed_by_user_segment_aware(
                        &command_to_match,
                        &rules.ask_user,
                    );
                if needs_confirmation {
                    return Ok(MatchConfirmDeny {
                        result: MatchConfirmDenyResult::CONFIRMATION,
                        command: command_to_match.clone(),
                        rule: confirmation_rule.clone(),
                    });
                }
            } else {
                tracing::error!("No confirmation info available for {:?}", command_to_match);
            }
            if let Some(flag) = structural_flags(&extract_command_segments(&command_to_match))
                .into_iter()
                .next()
            {
                return Ok(MatchConfirmDeny {
                    result: MatchConfirmDenyResult::CONFIRMATION,
                    command: command_to_match.clone(),
                    rule: flag.to_string(),
                });
            }
        }
        Ok(MatchConfirmDeny {
            result: MatchConfirmDenyResult::PASS,
            command: command_to_match.clone(),
            rule: "".to_string(),
        })
    }

    async fn command_to_match_against_confirm_deny(
        &self,
        _ccx: Arc<AMutex<AtCommandsContext>>,
        _args: &HashMap<String, Value>,
    ) -> Result<String, String> {
        Ok("".to_string())
    }

    fn confirm_deny_rules(&self) -> Option<IntegrationConfirmation> {
        None
    }

    fn has_config_path(&self) -> Option<String> {
        return None;
    }

    fn config(&self) -> Result<ToolConfig, String> {
        let tool_desc = self.tool_description();

        let tool_name = tool_desc.name;
        let config_path = tool_desc.source.config_path;

        let config = std::fs::read_to_string(config_path)
            .map_err(|e| format!("Error reading config file: {}", e))?;

        let config: serde_yaml::Value = serde_yaml::from_str(&config)
            .map_err(|e| format!("Error parsing config file: {}", e))?;

        let config = config.get("tools").and_then(|tools| tools.get(&tool_name));

        match config {
            None => Ok(ToolConfig::default()),
            Some(config) => {
                let config: ToolConfig = serde_yaml::from_value(config.clone()).unwrap_or_default();
                Ok(config)
            }
        }
    }

    fn tool_depends_on(&self) -> Vec<String> {
        vec![]
    } // "ast", "vecdb"

    #[allow(dead_code)] // Trait method for future usage tracking
    fn usage(&mut self) -> &mut Option<ChatUsage> {
        static mut DEFAULT_USAGE: Option<ChatUsage> = None;
        #[allow(static_mut_refs)]
        unsafe {
            &mut DEFAULT_USAGE
        }
    }
}
