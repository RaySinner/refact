pub use refact_tools::tools::task_tool_helpers::*;

#[cfg(test)]
use std::collections::HashMap;
#[cfg(test)]
use std::sync::Arc;
#[cfg(test)]
use chrono::Utc;
#[cfg(test)]
use serde_json::Value;
#[cfg(test)]
use tokio::sync::Mutex as AMutex;
#[cfg(test)]
use refact_runtime_api::AtCommandsContext;
#[cfg(test)]
use refact_runtime_api::GlobalContextCtx;
#[cfg(test)]
use refact_chat_api::TaskMeta;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_state::AppState;
    use serde_json::json;

    fn args(items: &[(&str, Value)]) -> HashMap<String, Value> {
        items
            .iter()
            .map(|(key, value)| ((*key).to_string(), value.clone()))
            .collect()
    }

    async fn ccx_with_meta(
        meta: Option<TaskMeta>,
        chat_id: &str,
    ) -> Arc<AMutex<AtCommandsContext>> {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        Arc::new(AMutex::new(
            crate::at_commands::at_commands::at_commands_new_from_app(
                refact_runtime_api::app_state_from_gcx(gcx).await,
                4096,
                20,
                false,
                vec![],
                chat_id.to_string(),
                None,
                "model".to_string(),
                meta,
                None,
            )
            .await,
        ))
    }

    fn task_meta(role: &str) -> TaskMeta {
        TaskMeta {
            task_id: "task-1".to_string(),
            role: role.to_string(),
            agent_id: (role == "agents").then(|| "agent-1".to_string()),
            card_id: (role == "agents").then(|| "T-1".to_string()),
            planner_chat_id: Some("planner-chat".to_string()),
        }
    }

    #[test]
    fn task_tool_helpers_parse_required_and_optional_strings() {
        let args = args(&[
            ("name", json!("  card-1  ")),
            ("empty", json!("   ")),
            ("null", Value::Null),
        ]);

        assert_eq!(required_string(&args, "name").unwrap(), "card-1");
        assert_eq!(optional_string(&args, "name"), Some("card-1".to_string()));
        assert_eq!(optional_string(&args, "empty"), None);
        assert_eq!(optional_string(&args, "missing"), None);
        assert_eq!(
            required_string(&args, "empty").unwrap_err(),
            "Missing 'empty'"
        );
        assert_eq!(
            required_string(&args, "null").unwrap_err(),
            "Missing 'null'"
        );
    }

    #[test]
    fn task_tool_helpers_truncate_chars_is_unicode_safe() {
        assert_eq!(truncate_chars("abc", 5), "abc");
        assert_eq!(truncate_chars("ab😀cd", 4), "ab😀…");
        assert_eq!(truncate_chars("abc", 0), "");
    }

    #[test]
    fn task_tool_helpers_human_age_clamps_future_timestamps() {
        assert_eq!(human_age(Utc::now() + chrono::Duration::seconds(5)), "now");
    }

    #[test]
    fn human_age_at_is_deterministic() {
        let now = Utc::now();
        assert_eq!(
            human_age_at(now - chrono::Duration::seconds(30), now),
            "30s ago"
        );
        assert_eq!(
            human_age_at(now - chrono::Duration::seconds(30), now),
            "30s ago"
        );
        assert_eq!(
            human_age_at(now - chrono::Duration::minutes(5), now),
            "5m ago"
        );
        assert_eq!(
            human_age_at(now - chrono::Duration::hours(2), now),
            "2h ago"
        );
        assert_eq!(human_age_at(now - chrono::Duration::days(3), now), "3d ago");
        assert_eq!(
            human_age_at(now + chrono::Duration::seconds(10), now),
            "now"
        );
    }

    #[tokio::test]
    async fn task_tool_helpers_require_bound_planner_task_accepts_bound_task() {
        let ccx = ccx_with_meta(Some(task_meta("planner")), "planner-chat").await;

        let task_id = require_bound_planner_task(&ccx, &args(&[("task_id", json!("task-1"))]))
            .await
            .unwrap();

        assert_eq!(task_id, "task-1");
    }

    #[tokio::test]
    async fn task_tool_helpers_require_bound_planner_task_rejects_override() {
        let ccx = ccx_with_meta(Some(task_meta("planner")), "planner-chat").await;

        let err = require_bound_planner_task(&ccx, &args(&[("task_id", json!("task-2"))]))
            .await
            .unwrap_err();

        assert_eq!(
            err,
            "task_id override is not allowed from this planner chat"
        );
    }

    #[tokio::test]
    async fn task_tool_helpers_require_bound_planner_task_requires_planner_role() {
        let ccx = ccx_with_meta(Some(task_meta("agents")), "agent-chat").await;

        let err = require_bound_planner_task(&ccx, &HashMap::new())
            .await
            .unwrap_err();

        assert!(err.contains("task planner"));
    }

    #[tokio::test]
    async fn task_tool_helpers_require_agent_task_meta_requires_agent_role() {
        let ccx = ccx_with_meta(Some(task_meta("agents")), "agent-chat").await;
        let meta = require_agent_task_meta(&ccx).await.unwrap();
        assert_eq!(meta.task_id, "task-1");
        assert_eq!(meta.card_id.as_deref(), Some("T-1"));

        let planner_ccx = ccx_with_meta(Some(task_meta("planner")), "planner-chat").await;
        let err = require_agent_task_meta(&planner_ccx).await.unwrap_err();
        assert_eq!(err, "task agent context is required");
    }

    async fn gcx_with_chat_models(
        model_names: &[&str],
    ) -> Arc<crate::global_context::GlobalContext> {
        use crate::caps::{ChatModelRecord, CodeAssistantCaps};
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let mut caps = CodeAssistantCaps::default();
        for &name in model_names {
            caps.chat_models
                .insert(name.to_string(), Arc::new(ChatModelRecord::default()));
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        {
            let mut state = gcx.caps_state.write().await;
            state.caps = Some(Arc::new(caps));
            state.last_attempted_ts = now;
        }
        gcx
    }

    #[tokio::test]
    async fn preflight_passes_for_configured_chat_model() {
        let gcx = gcx_with_chat_models(&["provider/test-model"]).await;
        preflight_agent_model(&gcx.chat_ctx(gcx.clone()), "provider/test-model")
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn preflight_fails_with_helpful_message_for_unknown_model() {
        let gcx = gcx_with_chat_models(&["provider/available-model"]).await;
        let err = preflight_agent_model(&gcx.chat_ctx(gcx.clone()), "provider/missing-model")
            .await
            .unwrap_err();
        assert!(
            err.contains("'provider/missing-model'"),
            "error should name the model: {err}"
        );
        assert!(
            err.contains("provider/available-model"),
            "error should list alternatives: {err}"
        );
    }

    #[tokio::test]
    async fn spawn_agent_fails_before_worktree_when_model_unavailable() {
        let gcx = gcx_with_chat_models(&[]).await;
        let cache_dir = gcx.cache_dir.clone();
        let err = preflight_agent_model(&gcx.chat_ctx(gcx.clone()), "unavailable/model")
            .await
            .unwrap_err();
        assert!(
            err.contains("'unavailable/model'"),
            "error should name the model: {err}"
        );
        assert!(
            err.contains("no chat models configured"),
            "error should indicate no models: {err}"
        );
        assert!(
            !cache_dir.join("worktrees").exists(),
            "no worktree directory should be created before preflight passes"
        );
    }

    #[tokio::test]
    async fn restart_agent_fails_before_worktree_when_model_unavailable() {
        let gcx = gcx_with_chat_models(&[]).await;
        let cache_dir = gcx.cache_dir.clone();
        let err = preflight_agent_model(&gcx.chat_ctx(gcx.clone()), "unavailable/model")
            .await
            .unwrap_err();
        assert!(
            err.contains("'unavailable/model'"),
            "error should name the model: {err}"
        );
        assert!(
            !cache_dir.join("worktrees").exists(),
            "no worktree directory should be created before preflight passes"
        );
    }
}
