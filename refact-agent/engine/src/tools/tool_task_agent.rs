pub use refact_tools::tools::tool_task_agent::*;


#[cfg(test)]
mod tests {
    use super::resolve_task_agent_scope;
    use refact_chat_api::TaskMeta;
    use serde_json::{json, Value};
    use std::collections::HashMap;

    fn bound_meta() -> TaskMeta {
        TaskMeta {
            task_id: "task-1".to_string(),
            role: "agents".to_string(),
            agent_id: Some("agent-1".to_string()),
            card_id: Some("card-1".to_string()),
            planner_chat_id: Some("planner-1".to_string()),
        }
    }

    fn args(items: &[(&str, Value)]) -> HashMap<String, Value> {
        items
            .iter()
            .map(|(key, value)| ((*key).to_string(), value.clone()))
            .collect()
    }

    #[test]
    fn task_agent_scope_rejects_bound_task_id_mismatch() {
        let args = args(&[("task_id", json!("task-2")), ("card_id", json!("card-1"))]);
        let err = resolve_task_agent_scope(Some(&bound_meta()), "agent-chat", &args, true, true)
            .unwrap_err();

        assert!(err.contains("bound task_id 'task-1'"));
    }

    #[test]
    fn task_agent_scope_rejects_bound_card_id_mismatch() {
        let args = args(&[("task_id", json!("task-1")), ("card_id", json!("card-2"))]);
        let err = resolve_task_agent_scope(Some(&bound_meta()), "agent-chat", &args, true, true)
            .unwrap_err();

        assert!(err.contains("bound card_id 'card-1'"));
    }

    #[test]
    fn task_agent_scope_accepts_bound_matching_ids() {
        let args = args(&[("task_id", json!("task-1")), ("card_id", json!("card-1"))]);
        let scope =
            resolve_task_agent_scope(Some(&bound_meta()), "agent-chat", &args, true, true).unwrap();

        assert_eq!(scope.task_id, "task-1");
        assert_eq!(scope.card_id.as_deref(), Some("card-1"));
    }
}
