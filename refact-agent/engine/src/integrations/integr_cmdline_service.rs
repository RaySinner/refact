pub use refact_runtime_api::integrations::cmdline_service::*;

#[cfg(test)]
use std::collections::HashMap;
#[cfg(test)]
use std::sync::Arc;

#[cfg(test)]
use serde_json::Value;
#[cfg(test)]
use tokio::sync::Mutex as AMutex;

#[cfg(test)]
use crate::at_commands::at_commands::AtCommandsContext;
#[cfg(test)]
use crate::call_validation::{ChatContent, ChatMessage, ContextEnum};
#[cfg(test)]
use crate::exec::{ExecServiceLookup, ExecStatus};
#[cfg(test)]
use crate::global_context::GlobalContext;
#[cfg(test)]
use crate::integrations::integr_cmdline::CmdlineToolConfig;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_state::AppState;
    use crate::integrations::integr_cmdline::CmdlineParam;
    use refact_runtime_api::Tool;

    fn args(entries: Vec<(&str, Value)>) -> HashMap<String, Value> {
        entries
            .into_iter()
            .map(|(key, value)| (key.to_string(), value))
            .collect()
    }

    async fn test_ccx() -> (Arc<GlobalContext>, Arc<AMutex<AtCommandsContext>>) {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let ccx = crate::at_commands::at_commands::at_commands_new_with_abort(
            crate::app_state::app_state_from_gcx(gcx.clone()).await,
            4096,
            20,
            false,
            Vec::new(),
            "chat".to_string(),
            None,
            "model".to_string(),
            None,
            None,
            None,
        )
        .await;
        (gcx, Arc::new(AMutex::new(ccx)))
    }

    fn long_running_command(output: &str) -> String {
        if cfg!(target_os = "windows") {
            format!("[Console]::Out.Write('{output}'); Start-Sleep -Seconds 30")
        } else {
            format!("printf {output:?}; sleep 30")
        }
    }

    fn only_message(messages: Vec<ContextEnum>) -> ChatMessage {
        match messages.into_iter().next().unwrap() {
            ContextEnum::ChatMessage(message) => message,
            ContextEnum::ContextFile(_) => panic!("expected chat message"),
        }
    }

    fn text(message: &ChatMessage) -> String {
        match &message.content {
            ChatContent::SimpleText(text) => text.clone(),
            _ => String::new(),
        }
    }

    #[tokio::test]
    async fn custom_service_uses_exec_registry_not_integration_sessions() {
        let (gcx, ccx) = test_ccx().await;
        let mut tool = ToolService {
            name: "service_api".to_string(),
            cfg: CmdlineToolConfig {
                command: long_running_command("custom-ready"),
                command_workdir: String::new(),
                startup_wait: 1,
                parameters: Vec::new(),
                ..CmdlineToolConfig::default()
            },
            ..ToolService::default()
        };
        let (_, messages) = tool
            .tool_execute(ccx, &"tool_call".to_string(), &args(vec![]))
            .await
            .unwrap();
        let message = only_message(messages);
        assert!(text(&message).contains("exec_service_service_api"));
        assert!(gcx.integration_sessions.lock().await.is_empty());
        let snapshot = gcx
            .exec_registry
            .find_service(ExecServiceLookup::new("service_api").with_chat_id("chat"))
            .await
            .unwrap();
        let snapshot = gcx
            .exec_registry
            .kill(&snapshot.meta.process_id)
            .await
            .unwrap();
        assert_eq!(snapshot.status, ExecStatus::Killed);
    }

    #[test]
    fn custom_service_args_still_validate_required_params() {
        let cfg = CmdlineToolConfig {
            parameters: vec![CmdlineParam {
                name: "target".to_string(),
                param_type: "string".to_string(),
                description: String::new(),
            }],
            parameters_required: Some(vec!["target".to_string()]),
            ..CmdlineToolConfig::default()
        };
        let err = parse_service_tool_args(&args(vec![]), &cfg).unwrap_err();
        assert_eq!(err, "Missing required argument `target`");
    }
}
