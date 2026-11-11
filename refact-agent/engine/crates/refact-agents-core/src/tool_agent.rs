use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use serde_json::{Map, Value};

use crate::host::AgentHost;
use crate::spawn::emit_background_agent_update;
use crate::types::{AgentCompletion, BgAgentKind, CreateAgentRequest};

#[derive(Debug, Clone)]
pub struct ToolAgentSpec {
    pub tool_name: String,
    pub title: String,
    pub prompt: String,
    pub model: String,
    pub model_type: Option<String>,
    pub target_files: Vec<String>,
    pub child_chat_id: String,
    pub parent_tool_call_id: String,
}

#[derive(Clone)]
pub struct ToolAgentHandle<H: AgentHost> {
    app: H,
    agent_id: String,
    parent_chat_id: String,
    spec: ToolAgentSpec,
    abort_flag: Arc<AtomicBool>,
}

impl<H: AgentHost> ToolAgentHandle<H> {
    pub async fn begin(
        app: &H,
        parent_chat_id: &str,
        parent_root_chat_id: &str,
        spec: ToolAgentSpec,
    ) -> Result<Self, String> {
        let request = CreateAgentRequest {
            parent_chat_id: parent_chat_id.to_string(),
            parent_root_chat_id: Some(parent_root_chat_id.to_string()),
            parent_tool_call_id: Some(spec.parent_tool_call_id.clone()),
            kind: BgAgentKind::Tool,
            config_name: spec.tool_name.clone(),
            title: spec.title.clone(),
            prompt: spec.prompt.clone(),
            target_files: spec.target_files.clone(),
            model: spec.model.clone(),
            model_type: spec.model_type.clone(),
            goal_summary: None,
            plan_present: false,
            worktree_id: None,
            worktree_branch: None,
        };
        let (record, abort_flag, _notify) = app.agents().create(request).await?;
        let record = app
            .agents()
            .mark_running(&record.agent_id, spec.child_chat_id.clone())
            .await?;
        let app = (*app).clone();
        emit_background_agent_update(app.clone(), &record).await;
        Ok(Self {
            app,
            agent_id: record.agent_id,
            parent_chat_id: parent_chat_id.to_string(),
            spec,
            abort_flag,
        })
    }

    pub fn agent_id(&self) -> &str {
        &self.agent_id
    }

    pub async fn progress(&self, text: impl Into<String>, step: u32) {
        if let Ok(record) = self
            .app
            .agents()
            .update_activity(&self.agent_id, Some(text.into()), Some(step), None)
            .await
        {
            emit_background_agent_update(self.app.clone(), &record).await;
        }
    }

    pub async fn current_tool(&self, tool: Option<String>) {
        if let Ok(record) = self
            .app
            .agents()
            .update_activity(&self.agent_id, None, None, Some(tool))
            .await
        {
            emit_background_agent_update(self.app.clone(), &record).await;
        }
    }

    pub async fn add_usage(&self, tokens: u64, cost_usd: Option<f64>) {
        if let Ok(record) = self
            .app
            .agents()
            .add_usage(&self.agent_id, tokens, cost_usd)
            .await
        {
            emit_background_agent_update(self.app.clone(), &record).await;
        }
    }

    pub fn aborted(&self) -> bool {
        self.abort_flag.load(Ordering::SeqCst)
    }

    pub async fn complete(self, summary: String) -> Result<(), String> {
        let record = self
            .app
            .agents()
            .mark_completed(
                &self.agent_id,
                AgentCompletion {
                    result_summary: summary,
                    edited_files: Vec::new(),
                    diff_summary: None,
                    conflict_summary: None,
                    child_chat_id: None,
                },
            )
            .await?;
        emit_background_agent_update(self.app.clone(), &record).await;
        Ok(())
    }

    pub async fn fail(self, error: String) -> Result<(), String> {
        let record = self
            .app
            .agents()
            .mark_failed(&self.agent_id, error)
            .await?;
        emit_background_agent_update(self.app.clone(), &record).await;
        Ok(())
    }

    pub async fn cancelled(self, reason: Option<String>) -> Result<(), String> {
        let record = self
            .app
            .agents()
            .mark_cancelled(&self.agent_id, reason)
            .await?;
        emit_background_agent_update(self.app.clone(), &record).await;
        Ok(())
    }

    pub fn extras(&self, status: &str, model_type: Option<&str>) -> Map<String, Value> {
        Map::from_iter([
            (
                "background_agent_id".to_string(),
                Value::from(self.agent_id.as_str()),
            ),
            (
                "background_agent_kind".to_string(),
                Value::from(BgAgentKind::Tool.as_str()),
            ),
            ("background_agent_status".to_string(), Value::from(status)),
            (
                "child_chat_id".to_string(),
                Value::from(self.spec.child_chat_id.as_str()),
            ),
            (
                "parent_chat_id".to_string(),
                Value::from(self.parent_chat_id.as_str()),
            ),
            ("model".to_string(), Value::from(self.spec.model.as_str())),
            (
                "model_type".to_string(),
                model_type.map(Value::from).unwrap_or(Value::Null),
            ),
            ("title".to_string(), Value::from(self.spec.title.as_str())),
        ])
    }
}
