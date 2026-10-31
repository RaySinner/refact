use std::sync::{Arc, Mutex as StdMutex};
use tokio::sync::Mutex as AMutex;

use crate::at_commands::at_commands::AtCommandsContext;
use crate::custom_error::MapErrToString;

pub use refact_tool_api::{
    command_should_be_confirmed_by_user, command_should_be_confirmed_by_user_segment_aware,
    command_should_be_denied, command_should_be_denied_segment_aware, extract_command_segments,
    is_strict_compatible, json_schema_from_params, make_openai_tool_value, structural_flags,
    MatchConfirmDeny, MatchConfirmDenyResult, ToolConfig, ToolDesc, ToolGroupCategory, ToolSource,
    ToolSourceType,
};

pub use refact_runtime_api::{Tool, ToolGroup};

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

/// A wait-only cancellation signal; observing it never aborts the chat turn.
#[derive(Clone)]
pub(crate) struct WaitInterrupt {
    pub(crate) abort_flag: Arc<std::sync::atomic::AtomicBool>,
    pub(crate) wait_flag: Arc<std::sync::atomic::AtomicBool>,
    pub(crate) cause: Arc<StdMutex<WaitInterruptCause>>,
    pub(crate) notify: Option<Arc<tokio::sync::Notify>>,
}

impl WaitInterrupt {
    pub(crate) async fn from_context(ccx: &Arc<AMutex<AtCommandsContext>>) -> Self {
        let (app, chat_id, abort_flag) = {
            let context = ccx.lock().await;
            (
                context.app.clone(),
                context.chat_id.clone(),
                context.abort_flag.clone(),
            )
        };
        let session = app.chat.sessions.read().await.get(&chat_id).cloned();
        let (wait_flag, cause, notify) = if let Some(session) = session {
            let session = session.lock().await;
            (
                session.wait_interrupt_flag.clone(),
                session.wait_interrupt_cause.clone(),
                Some(session.abort_notify.clone()),
            )
        } else {
            (
                Arc::new(std::sync::atomic::AtomicBool::new(false)),
                Arc::new(StdMutex::new(WaitInterruptCause::MessageArrived)),
                None,
            )
        };
        Self {
            abort_flag,
            wait_flag,
            cause,
            notify,
        }
    }

    pub(crate) fn interrupted(&self) -> bool {
        self.abort_flag.load(std::sync::atomic::Ordering::SeqCst)
            || self.wait_flag.load(std::sync::atomic::Ordering::SeqCst)
    }

    pub(crate) fn reason(&self, elapsed_seconds: f64) -> String {
        let wait_cause = self
            .cause
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .label();
        let cause = if self.wait_flag.load(std::sync::atomic::Ordering::SeqCst) {
            wait_cause
        } else {
            "user aborted"
        };
        format!("interrupted after {elapsed_seconds:.1}s: {cause}")
    }

    pub(crate) async fn wait(&self) {
        loop {
            // Polling also covers notification races and contexts without a session.
            let notified = async {
                if let Some(notify) = &self.notify {
                    notify.notified().await;
                } else {
                    std::future::pending::<()>().await;
                }
            };
            tokio::pin!(notified);
            if self.interrupted() {
                return;
            }
            tokio::select! {
                _ = &mut notified => {},
                _ = tokio::time::sleep(std::time::Duration::from_millis(50)) => {},
            }
        }
    }
}
