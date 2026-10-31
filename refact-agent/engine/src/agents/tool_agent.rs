use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::{Map, Value};

use crate::agents::spawn::emit_background_agent_update;
use crate::agents::types::{AgentCompletion, BgAgentKind, CreateAgentRequest};
use crate::app_state::AppState;

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
pub struct ToolAgentHandle {
    app: AppState,
    agent_id: String,
    parent_chat_id: String,
    spec: ToolAgentSpec,
    abort_flag: Arc<AtomicBool>,
}

impl ToolAgentHandle {
    pub async fn begin(
        app: &AppState,
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
        let (record, abort_flag, _notify) = app.agents.create(request).await?;
        let record = app
            .agents
            .mark_running(&record.agent_id, spec.child_chat_id.clone())
            .await?;
        emit_background_agent_update(app.clone(), &record).await;
        Ok(Self {
            app: app.clone(),
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
            .agents
            .update_activity(&self.agent_id, Some(text.into()), Some(step), None)
            .await
        {
            emit_background_agent_update(self.app.clone(), &record).await;
        }
    }

    pub async fn current_tool(&self, tool: Option<String>) {
        if let Ok(record) = self
            .app
            .agents
            .update_activity(&self.agent_id, None, None, Some(tool))
            .await
        {
            emit_background_agent_update(self.app.clone(), &record).await;
        }
    }

    pub async fn add_usage(&self, tokens: u64, cost_usd: Option<f64>) {
        if let Ok(record) = self
            .app
            .agents
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
            .agents
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
        let record = self.app.agents.mark_failed(&self.agent_id, error).await?;
        emit_background_agent_update(self.app.clone(), &record).await;
        Ok(())
    }

    pub async fn cancelled(self, reason: Option<String>) -> Result<(), String> {
        let record = self
            .app
            .agents
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    use crate::agents::types::BgAgentStatus;
    use crate::chat::types::ChatSession;

    fn spec(child_chat_id: &str) -> ToolAgentSpec {
        ToolAgentSpec {
            tool_name: "review".to_string(),
            title: "Review: auth flow".to_string(),
            prompt: "Review the auth flow".to_string(),
            model: "test/model".to_string(),
            model_type: Some("thinking".to_string()),
            target_files: vec!["src/auth.rs".to_string()],
            child_chat_id: child_chat_id.to_string(),
            parent_tool_call_id: "tool-call-1".to_string(),
        }
    }

    async fn app_with_parent_session(
        parent_chat_id: &str,
    ) -> (AppState, Arc<tokio::sync::Mutex<ChatSession>>) {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let app = crate::app_state::app_state_from_gcx(gcx).await;
        let session = Arc::new(tokio::sync::Mutex::new(ChatSession::new(
            parent_chat_id.to_string(),
        )));
        app.chat
            .sessions
            .write()
            .await
            .insert(parent_chat_id.to_string(), session.clone());
        (app, session)
    }

    async fn begin_handle(
        app: &AppState,
        parent_chat_id: &str,
        root_chat_id: &str,
        child_chat_id: &str,
    ) -> ToolAgentHandle {
        ToolAgentHandle::begin(app, parent_chat_id, root_chat_id, spec(child_chat_id))
            .await
            .expect("begin")
    }

    #[tokio::test]
    async fn begin_registers_a_running_tool_agent_with_the_child_chat() {
        let (app, _session) = app_with_parent_session("parent-begin").await;
        let handle = begin_handle(&app, "parent-begin", "root-begin", "tool-child-begin").await;
        let record = app.agents.get_any(handle.agent_id()).await.expect("record");

        assert_eq!(record.kind, BgAgentKind::Tool);
        assert_eq!(record.child_chat_id.as_deref(), Some("tool-child-begin"));
        assert_eq!(record.parent_tool_call_id.as_deref(), Some("tool-call-1"));
        assert_eq!(record.status, BgAgentStatus::Running);
        assert_eq!(record.config_name, "review");
        assert_eq!(record.model, "test/model");
        assert_eq!(record.model_type.as_deref(), Some("thinking"));
        assert_eq!(record.target_files, vec!["src/auth.rs".to_string()]);
        assert_eq!(record.parent_root_chat_id.as_deref(), Some("root-begin"));
    }

    #[tokio::test]
    async fn begin_emits_a_background_agent_updated_into_the_parent_session() {
        let (app, session) = app_with_parent_session("parent-emit").await;
        let handle =
            begin_handle(&app, "parent-emit", "parent-emit", "tool-child-emit").await;

        let session = session.lock().await;
        let summary = session
            .background_agents
            .get(handle.agent_id())
            .expect("background agent summary");
        assert_eq!(summary.kind, "tool");
        assert_eq!(summary.status, "running");
        assert_eq!(summary.child_chat_id.as_deref(), Some("tool-child-emit"));
        assert_eq!(summary.parent_tool_call_id.as_deref(), Some("tool-call-1"));
    }

    #[tokio::test]
    async fn progress_and_usage_update_the_record() {
        let (app, _session) = app_with_parent_session("parent-progress").await;
        let handle =
            begin_handle(&app, "parent-progress", "root-progress", "tool-child-progress").await;

        handle.progress("reading files", 3).await;
        handle.current_tool(Some("grep: auth".to_string())).await;
        handle.add_usage(120, Some(0.5)).await;

        let record = app
            .agents
            .get_any(handle.agent_id())
            .await
            .expect("record");
        assert_eq!(record.step_count, 3);
        assert_eq!(record.progress.as_deref(), Some("reading files"));
        assert_eq!(record.current_tool.as_deref(), Some("grep: auth"));
        assert_eq!(record.tokens_used, 120);
        assert_eq!(record.cost_usd, Some(0.5));
        assert!(record.last_activity.is_some());
    }

    #[tokio::test]
    async fn complete_marks_terminal_with_summary() {
        let (app, _session) = app_with_parent_session("parent-complete").await;
        let handle =
            begin_handle(&app, "parent-complete", "root-complete", "tool-child-complete").await;
        let agent_id = handle.agent_id().to_string();

        handle
            .complete("review done".to_string())
            .await
            .expect("complete");

        let record = app
            .agents
            .get_any(&agent_id)
            .await
            .expect("record");
        assert_eq!(record.status, BgAgentStatus::Completed);
        assert_eq!(record.result_summary.as_deref(), Some("review done"));
        assert!(record.finished_at.is_some());
        assert_eq!(record.child_chat_id.as_deref(), Some("tool-child-complete"));
    }

    #[tokio::test]
    async fn fail_marks_failed() {
        let (app, _session) = app_with_parent_session("parent-fail").await;
        let handle = begin_handle(&app, "parent-fail", "root-fail", "tool-child-fail").await;
        let agent_id = handle.agent_id().to_string();

        handle.fail("boom".to_string()).await.expect("fail");

        let record = app
            .agents
            .get_any(&agent_id)
            .await
            .expect("record");
        assert_eq!(record.status, BgAgentStatus::Failed);
        assert_eq!(record.error.as_deref(), Some("boom"));
        assert!(record.finished_at.is_some());
    }

    #[tokio::test]
    async fn cancelled_marks_cancelled() {
        let (app, _session) = app_with_parent_session("parent-cancelled").await;
        let handle =
            begin_handle(&app, "parent-cancelled", "root-cancelled", "tool-child-cancelled").await;
        let agent_id = handle.agent_id().to_string();

        handle
            .cancelled(Some("stop".to_string()))
            .await
            .expect("cancelled");

        let record = app
            .agents
            .get_any(&agent_id)
            .await
            .expect("record");
        assert_eq!(record.status, BgAgentStatus::Cancelled);
        assert_eq!(record.error.as_deref(), Some("stop"));
        assert!(record.finished_at.is_some());
    }

    #[tokio::test]
    async fn registry_cancel_sets_the_handle_abort_flag() {
        let (app, _session) = app_with_parent_session("parent-cancel-flag").await;
        let handle =
            begin_handle(&app, "parent-cancel-flag", "root-cancel-flag", "tool-child-flag").await;

        assert!(!handle.aborted());
        app.agents
            .cancel("parent-cancel-flag", handle.agent_id(), None)
            .await
            .expect("cancel");
        assert!(handle.aborted());

        let record = app
            .agents
            .get_any(handle.agent_id())
            .await
            .expect("record");
        assert_eq!(record.status, BgAgentStatus::Cancelled);
    }

    #[tokio::test]
    async fn extras_carry_the_gui_contract_keys() {
        let (app, _session) = app_with_parent_session("parent-extras").await;
        let handle =
            begin_handle(&app, "parent-extras", "root-extras", "tool-child-extras").await;

        let extras = handle.extras("running", Some("thinking"));
        let expected = [
            "background_agent_id",
            "background_agent_kind",
            "background_agent_status",
            "child_chat_id",
            "parent_chat_id",
            "model",
            "model_type",
            "title",
        ];
        assert_eq!(extras.len(), 8);
        for key in expected {
            assert!(extras.contains_key(key), "missing contract key: {key}");
        }
        assert_eq!(extras["background_agent_kind"], json!("tool"));
        assert_eq!(extras["background_agent_id"], json!(handle.agent_id()));
        assert_eq!(extras["background_agent_status"], json!("running"));
        assert_eq!(extras["child_chat_id"], json!("tool-child-extras"));
        assert_eq!(extras["parent_chat_id"], json!("parent-extras"));
        assert_eq!(extras["model"], json!("test/model"));
        assert_eq!(extras["model_type"], json!("thinking"));
        assert_eq!(extras["title"], json!("Review: auth flow"));

        let null_model_type = handle.extras("running", None);
        assert_eq!(null_model_type["model_type"], Value::Null);
    }
}
