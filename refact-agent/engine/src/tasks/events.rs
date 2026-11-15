pub use refact_runtime_api::task_events::*;

#[cfg(test)]
use refact_tasks::types::TaskMeta;

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn session_state_enrichment_does_not_list_task_trajectories() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let mut task = TaskMeta {
            schema_version: 1,
            id: "task-index-free".to_string(),
            name: "Task".to_string(),
            status: refact_tasks::types::TaskStatus::Active,
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
            cards_total: 0,
            cards_done: 0,
            cards_failed: 0,
            agents_active: 0,
            base_branch: None,
            base_commit: None,
            default_agent_model: None,
            is_name_generated: false,
            last_agents_summary_at: None,
            planner_session_state: Some("stale".to_string()),
        };

        enrich_task_with_session_state(gcx.clone(), &mut task).await;
        let counters = gcx.trajectory_index_coordinator.listing_counters_for_test();

        assert!(task.planner_session_state.is_none());
        assert_eq!(
            counters
                .calls_by_caller
                .iter()
                .find(|(caller, _)| *caller == "task_trajectory_api")
                .unwrap()
                .1,
            0
        );
    }
}
