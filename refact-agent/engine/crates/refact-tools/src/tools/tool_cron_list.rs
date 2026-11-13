use refact_scheduler_core::schedule::scheduler_timezone;
use refact_runtime_api::scheduler::active_durable_cron_store_for_services;
use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use serde_json::{json, Value};
use tokio::sync::Mutex as AMutex;

use refact_runtime_api::AtCommandsContext;
use refact_core::chat_types::{ChatContent, ChatMessage, ContextEnum};
use refact_scheduler_core::{
 cron_task_response, next_run_ms,
    session_cron_store, CronStore, Job,
};
use crate::tools::tools_description::{Tool, ToolDesc, ToolSource, ToolSourceType};

pub struct ToolCronList {
    pub config_path: String,
    #[cfg(any(test, feature = "test-util"))]
    pub test_session_store: Option<Arc<dyn CronStore>>,
    #[cfg(any(test, feature = "test-util"))]
    pub test_durable_store: Option<Arc<dyn CronStore>>,
}

impl ToolCronList {
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

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum CronListScope {
    Session,
    Durable,
    All,
}

#[async_trait]
impl Tool for ToolCronList {
    async fn tool_execute(
        &mut self,
        ccx: Arc<AMutex<AtCommandsContext>>,
        tool_call_id: &String,
        args: &HashMap<String, Value>,
    ) -> Result<(bool, Vec<ContextEnum>), String> {
        let scope = parse_scope(args)?;
        let now_ms = Utc::now().timestamp_millis().max(0) as u64;
        let tz = scheduler_timezone();

        let mut tasks: Vec<Value> = Vec::new();

        if scope == CronListScope::Session || scope == CronListScope::All {
            let session_tasks = self.session_store().list().await;
            tasks.extend(
                session_tasks
                    .iter()
                    .map(|t| task_value(t, now_ms, tz))
                    .collect::<Result<Vec<_>, _>>()?,
            );
        }

        if scope == CronListScope::Durable || scope == CronListScope::All {
            if let Some(store) = self.durable_store(ccx).await? {
                let durable_tasks = store.list().await;
                tasks.extend(
                    durable_tasks
                        .iter()
                        .map(|t| task_value(t, now_ms, tz))
                        .collect::<Result<Vec<_>, _>>()?,
                );
            }
        }

        tasks.sort_by(|a, b| {
            a["next_fire_at_ms"]
                .as_u64()
                .unwrap_or(0)
                .cmp(&b["next_fire_at_ms"].as_u64().unwrap_or(0))
                .then_with(|| {
                    a["id"]
                        .as_str()
                        .unwrap_or("")
                        .cmp(b["id"].as_str().unwrap_or(""))
                })
        });

        let text = serde_json::to_string_pretty(&tasks).map_err(|e| e.to_string())?;
        Ok((
            false,
            vec![ContextEnum::ChatMessage(ChatMessage {
                role: "tool".to_string(),
                content: ChatContent::SimpleText(text),
                tool_calls: None,
                tool_call_id: tool_call_id.clone(),
                ..Default::default()
            })],
        ))
    }

    fn tool_description(&self) -> ToolDesc {
        ToolDesc {
            name: "cron_list".to_string(),
            display_name: "Cron List".to_string(),
            source: ToolSource {
                source_type: ToolSourceType::Builtin,
                config_path: self.config_path.clone(),
            },
            experimental: false,
            allow_parallel: true,
            description: "List scheduled tasks with target chat and mode, optionally filtering by session-only or durable scope."
                .to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "scope": {
                        "type": "string",
                        "enum": ["session", "durable", "all"],
                        "default": "all"
                    }
                },
                "required": []
            }),
            output_schema: None,
            annotations: None,
        }
    }

    fn tool_depends_on(&self) -> Vec<String> {
        vec![]
    }

    fn has_config_path(&self) -> Option<String> {
        Some(self.config_path.clone())
    }
}

fn parse_scope(args: &HashMap<String, Value>) -> Result<CronListScope, String> {
    match args.get("scope") {
        Some(Value::String(scope)) if scope.trim().is_empty() => Ok(CronListScope::All),
        Some(Value::String(scope)) => match scope.trim() {
            "session" => Ok(CronListScope::Session),
            "durable" => Ok(CronListScope::Durable),
            "all" => Ok(CronListScope::All),
            other => Err(format!(
                "Invalid scope `{other}`. Must be one of: session, durable, all"
            )),
        },
        Some(value) => Err(format!("argument `scope` is not a string: {value:?}")),
        None => Ok(CronListScope::All),
    }
}

fn task_value(task: &Job, now_ms: u64, tz: chrono_tz::Tz) -> Result<Value, String> {
    let next_fire_at_ms = next_run_ms(task, now_ms, tz).unwrap_or(0);
    serde_json::to_value(cron_task_response(task, next_fire_at_ms)).map_err(|e| e.to_string())
}

#[cfg(any(test, feature = "test-util"))]
pub fn set_job_isolated(task: &mut Job) {
    task.set_isolated();
}
