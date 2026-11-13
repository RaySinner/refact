use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::sync::Mutex as AMutex;

use refact_runtime_api::AtCommandsContext;
use refact_core::chat_types::{ChatContent, ChatMessage, ContextEnum};
use refact_runtime_api::scheduler::active_durable_cron_store_for_services;
use refact_scheduler_core::{ session_cron_store, CronStore};
use crate::tools::tools_description::{Tool, ToolDesc, ToolSource, ToolSourceType};

pub struct ToolCronDelete {
    pub config_path: String,
    #[cfg(any(test, feature = "test-util"))]
    pub test_session_store: Option<Arc<dyn CronStore>>,
    #[cfg(any(test, feature = "test-util"))]
    pub test_durable_store: Option<Arc<dyn CronStore>>,
}

impl ToolCronDelete {
    pub fn new(config_path: String) -> Self {
        Self {
            config_path,
            #[cfg(any(test, feature = "test-util"))]
            test_session_store: None,
            #[cfg(any(test, feature = "test-util"))]
            test_durable_store: None,
        }
    }

    #[cfg(any(test, feature = "test-util"))]
    pub fn with_stores(
        config_path: String,
        session_store: Arc<dyn CronStore>,
        durable_store: Option<Arc<dyn CronStore>>,
    ) -> Self {
        Self {
            config_path,
            test_session_store: Some(session_store),
            test_durable_store: durable_store,
        }
    }

    pub fn session_store(&self) -> Arc<dyn CronStore> {
        #[cfg(any(test, feature = "test-util"))]
        if let Some(store) = &self.test_session_store {
            return store.clone();
        }
        session_cron_store()
    }

    pub async fn durable_store(
        &self,
        ccx: Arc<AMutex<AtCommandsContext>>,
    ) -> Result<Option<Arc<dyn CronStore>>, String> {
        #[cfg(any(test, feature = "test-util"))]
        if self.test_session_store.is_some() {
            return Ok(self.test_durable_store.clone());
        }
        let (gcx, paths, workspace) = {
            let ccx_lock = ccx.lock().await;
            (ccx_lock.app.gcx.clone(), ccx_lock.app.paths.clone(), ccx_lock.app.workspace.clone())
        };
        active_durable_cron_store_for_services(&gcx, &paths, &workspace).await
    }
}

#[async_trait]
impl Tool for ToolCronDelete {
    fn tool_description(&self) -> ToolDesc {
        ToolDesc {
            name: "cron_delete".to_string(),
            display_name: "Delete Scheduled Task".to_string(),
            source: ToolSource {
                source_type: ToolSourceType::Builtin,
                config_path: self.config_path.clone(),
            },
            experimental: false,
            allow_parallel: false,
            description: "Cancel a scheduled task by ID.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "id": { "type": "string" }
                },
                "required": ["id"]
            }),
            output_schema: None,
            annotations: None,
        }
    }

    async fn tool_execute(
        &mut self,
        ccx: Arc<AMutex<AtCommandsContext>>,
        tool_call_id: &String,
        args: &HashMap<String, Value>,
    ) -> Result<(bool, Vec<ContextEnum>), String> {
        let id = match args.get("id") {
            Some(Value::String(id)) => id.clone(),
            Some(value) => return Err(format!("argument `id` is not a string: {value:?}")),
            None => return Err("argument `id` is missing".to_string()),
        };

        let mut removed = self.session_store().remove(&id).await?;

        if !removed {
            if let Some(durable) = self.durable_store(ccx).await? {
                removed = durable.remove(&id).await?;
            }
        }

        if removed {
            refact_scheduler_core::notify_runner_change();
        }

        Ok((
            false,
            vec![ContextEnum::ChatMessage(ChatMessage {
                role: "tool".to_string(),
                content: ChatContent::SimpleText(json!({ "removed": removed }).to_string()),
                tool_call_id: tool_call_id.clone(),
                tool_failed: Some(false),
                ..Default::default()
            })],
        ))
    }
}
