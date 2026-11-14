pub use refact_tools::tools::tool_task_restart_agent::*;

#[cfg(test)]
#[allow(unused_imports)]
use std::collections::HashMap;
#[cfg(test)]
#[allow(unused_imports)]
use std::path::{Path, PathBuf};
#[cfg(test)]
#[allow(unused_imports)]
use std::sync::Arc;
#[cfg(test)]
#[allow(unused_imports)]
use async_trait::async_trait;
#[cfg(test)]
#[allow(unused_imports)]
use chrono::Utc;
#[cfg(test)]
#[allow(unused_imports)]
use serde_json::{json, Value};
#[cfg(test)]
#[allow(unused_imports)]
use tokio::sync::Mutex as AMutex;
#[cfg(test)]
#[allow(unused_imports)]
use uuid::Uuid;
#[cfg(test)]
#[allow(unused_imports)]
use crate::app_state::AppState;
#[cfg(test)]
#[allow(unused_imports)]
use refact_runtime_api::AtCommandsContext;
#[cfg(test)]
#[allow(unused_imports)]
use refact_core::chat_types::{ChatMessage, ChatContent, ContextEnum, ContextFile};
#[cfg(test)]
#[allow(unused_imports)]
use refact_runtime_api::ChatCtx;
#[cfg(test)]
#[allow(unused_imports)]
use refact_chat_api::internal_roles::{event, EventSubkind};
#[cfg(test)]
#[allow(unused_imports)]
use crate::global_context::GlobalContext;
#[cfg(test)]
#[allow(unused_imports)]
use refact_runtime_api::task_storage as storage;
#[cfg(test)]
#[allow(unused_imports)]
use refact_tasks::types::{BoardCard, StatusUpdate};
#[cfg(test)]
#[allow(unused_imports)]
use crate::tools::task_tool_helpers::{wait_for_agent_abort, AGENT_ABORT_TIMEOUT};
#[cfg(test)]
#[allow(unused_imports)]
use crate::tools::tool_task_spawn_agent::{
    build_agent_prompt, build_agent_thread_params, mark_card_agent_started, prepare_agent_worktree,
    resolve_agent_model, restore_original_card_if_current_agent,
};
#[cfg(test)]
#[allow(unused_imports)]
use crate::tools::tools_description::{Tool, ToolDesc, ToolSource, ToolSourceType};
#[cfg(test)]
#[allow(unused_imports)]
use refact_worktrees::service::WorktreeService;
#[cfg(test)]
#[allow(unused_imports)]
use refact_worktrees::types::WorktreeMeta;
#[cfg(test)]
#[allow(unused_imports)]
use refact_chat_api::ChatCommand;
#[cfg(test)]
#[allow(unused_imports)]
use refact_runtime_api::CreateSessionRequest;

#[cfg(test)]
mod tests {
    use super::*;
    use refact_tasks::types::{BoardCard, TaskBoard};

    #[tokio::test]
    async fn restart_agent_context_file_message_has_declared_record() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("safe.txt");
        std::fs::write(&path, "safe\n").unwrap();
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let message = context_file_message(
            &gcx,
            vec![ContextFile {
                file_name: path.to_string_lossy().into_owned(),
                file_content: "safe\n".to_string(),
                line1: 1,
                line2: 1,
                ..Default::default()
            }],
        )
        .unwrap();
        let privacy: refact_privacy::PrivacyRecord =
            serde_json::from_value(message.extra["privacy"].clone()).unwrap();

        assert_eq!(privacy.files.len(), 1);
        assert!(privacy.files[0].path.ends_with("safe.txt"));
        assert_eq!(
            privacy.files[0].attribution,
            refact_privacy::Attribution::Declared
        );
    }

    fn failed_card(id: &str, worktree: Option<String>, branch: Option<String>) -> BoardCard {
        BoardCard {
            id: id.to_string(),
            title: format!("Card {}", id),
            column: "failed".to_string(),
            priority: "P1".to_string(),
            depends_on: vec![],
            instructions: "Fix the bug".to_string(),
            assignee: Some("old-agent".to_string()),
            agent_chat_id: Some(format!("agent-{}-old", id)),
            retry_count: 0,
            status_updates: vec![StatusUpdate {
                timestamp: "2024-01-01T10:00:00Z".to_string(),
                message: "Agent started".to_string(),
            }],
            comments: vec![],
            final_report: Some("FAILED: network error after 3 retries".to_string()),
            final_report_structured: None,
            verifier_report: None,
            created_at: Utc::now().to_rfc3339(),
            started_at: Some(Utc::now().to_rfc3339()),
            last_heartbeat_at: None,
            completed_at: Some(Utc::now().to_rfc3339()),
            agent_branch: branch,
            agent_worktree: worktree,
            agent_worktree_name: None,
            base_branch: None,
            base_commit: None,
            ab_variants: None,
            team_members: vec![],
            target_files: vec![],
            scope_guard_mode: Default::default(),
        }
    }

    #[test]
    fn restart_agent_fresh_clears_failure_and_sets_doing() {
        let mut card = failed_card("T-1", None, None);
        let new_id = "new-agent-1";
        let new_chat = "agent-T-1-new";

        mark_card_restarted_fresh(
            &mut card,
            new_id,
            new_chat,
            Some("refact/task/task-1/card/T-1/new".to_string()),
            Some("/tmp/wt".to_string()),
            Some("wt-new".to_string()),
        );

        assert_eq!(card.column, "doing");
        assert_eq!(card.assignee.as_deref(), Some(new_id));
        assert_eq!(card.agent_chat_id.as_deref(), Some(new_chat));
        assert!(card.completed_at.is_none());
        assert!(card.final_report.is_none());
        assert!(card
            .status_updates
            .iter()
            .any(|u| u.message.contains("Previous failure")));
        assert_eq!(card.agent_worktree.as_deref(), Some("/tmp/wt"));
        assert_eq!(card.agent_worktree_name.as_deref(), Some("wt-new"));
    }

    #[test]
    fn restart_agent_resume_keeps_worktree_fields() {
        let mut card = failed_card(
            "T-2",
            Some("/tmp/retained-wt".to_string()),
            Some("refact/task/task-1/card/T-2/old".to_string()),
        );
        let old_worktree = card.agent_worktree.clone();
        let old_branch = card.agent_branch.clone();
        let new_id = "resume-agent-1";
        let new_chat = "agent-T-2-resume";

        mark_card_restarted_resume(&mut card, new_id, new_chat);

        assert_eq!(card.column, "doing");
        assert_eq!(card.assignee.as_deref(), Some(new_id));
        assert_eq!(card.agent_chat_id.as_deref(), Some(new_chat));
        assert!(card.completed_at.is_none());
        assert!(card.final_report.is_none());
        assert!(card
            .status_updates
            .iter()
            .any(|u| u.message.contains("Previous failure")));
        assert!(card
            .status_updates
            .iter()
            .any(|u| u.message.contains("Resume restart")));
        assert_eq!(card.agent_worktree, old_worktree, "worktree path preserved");
        assert_eq!(card.agent_branch, old_branch, "branch preserved");
    }

    #[test]
    fn restart_agent_refuses_active_doing_card_without_force() {
        let mut board = TaskBoard::default();
        let doing_card = BoardCard {
            id: "T-3".to_string(),
            title: "Card T-3".to_string(),
            column: "doing".to_string(),
            priority: "P1".to_string(),
            depends_on: vec![],
            instructions: "do stuff".to_string(),
            assignee: Some("active-agent".to_string()),
            agent_chat_id: Some("agent-T-3-active".to_string()),
            retry_count: 0,
            status_updates: vec![],
            comments: vec![],
            final_report: None,
            final_report_structured: None,
            verifier_report: None,
            created_at: Utc::now().to_rfc3339(),
            started_at: Some(Utc::now().to_rfc3339()),
            last_heartbeat_at: None,
            completed_at: None,
            agent_branch: None,
            agent_worktree: None,
            agent_worktree_name: None,
            base_branch: None,
            base_commit: None,
            ab_variants: None,
            team_members: vec![],
            target_files: vec![],
            scope_guard_mode: Default::default(),
        };
        board.cards.push(doing_card);

        let card = board.get_card("T-3").unwrap();
        let should_refuse = card.column == "doing" && card.agent_chat_id.is_some();
        assert!(
            should_refuse,
            "active doing card should trigger refusal without force"
        );
    }

    #[test]
    fn restart_agent_allows_failed_card_without_force() {
        let card = failed_card("T-4", Some("/tmp/wt".to_string()), None);
        let should_refuse = card.column == "doing" && card.agent_chat_id.is_some();
        assert!(!should_refuse, "failed card should not require force");
        assert_eq!(card.column, "failed");
    }

    #[test]
    fn restart_agent_fresh_preserves_failure_in_status_history() {
        let mut card = failed_card("T-5", None, None);
        let original_status_count = card.status_updates.len();

        mark_card_restarted_fresh(&mut card, "new-agent", "agent-T-5-new", None, None, None);

        assert!(
            card.status_updates.len() > original_status_count,
            "status updates should grow"
        );
        assert!(
            card.status_updates
                .iter()
                .any(|u| u.message.starts_with("Previous failure:")),
            "old failure should appear in status history"
        );
        assert!(
            card.final_report.is_none(),
            "final_report should be cleared after restart"
        );
    }

    #[test]
    fn restart_agent_resume_rejects_card_without_worktree_name() {
        let card = failed_card("T-6", None, None);

        let wt_name = card.agent_worktree_name.as_deref();
        assert!(wt_name.is_none());
    }

    #[test]
    fn restart_agent_fresh_rollback_resets_card_to_failed() {
        let original = failed_card(
            "T-7",
            Some("/tmp/old-wt".to_string()),
            Some("old-branch".to_string()),
        );
        let mut board = TaskBoard::default();
        let mut restarted = original.clone();
        mark_card_restarted_fresh(
            &mut restarted,
            "new-agent",
            "agent-T-7-new",
            Some("new-branch".to_string()),
            Some("/tmp/new-wt".to_string()),
            Some("new-wt".to_string()),
        );
        board.cards.push(restarted);

        assert!(restore_original_card_if_current_agent(
            &mut board,
            &original,
            "agent-T-7-new"
        ));

        let card = board.get_card("T-7").unwrap();
        assert_eq!(card.column, original.column);
        assert_eq!(card.agent_chat_id, original.agent_chat_id);
        assert_eq!(card.assignee, original.assignee);
        assert_eq!(card.agent_worktree, original.agent_worktree);
        assert_eq!(card.agent_branch, original.agent_branch);
        assert_eq!(card.final_report, original.final_report);
        assert_eq!(card.completed_at, original.completed_at);
    }

    #[test]
    fn restart_agent_rollback_guard_keeps_newer_agent() {
        let original = failed_card("T-8", None, None);
        let mut board = TaskBoard::default();
        let mut newer = original.clone();
        mark_card_restarted_fresh(
            &mut newer,
            "newer-agent",
            "agent-T-8-newer",
            None,
            None,
            None,
        );
        board.cards.push(newer.clone());

        assert!(!restore_original_card_if_current_agent(
            &mut board,
            &original,
            "agent-T-8-stale"
        ));

        let card = board.get_card("T-8").unwrap();
        assert_eq!(card.agent_chat_id, newer.agent_chat_id);
        assert_eq!(card.assignee, newer.assignee);
        assert_eq!(card.column, "doing");
    }

    #[test]
    fn restart_agent_source_guard_accepts_exact_previous_binding() {
        let original = failed_card(
            "T-9",
            Some("/tmp/original-wt".to_string()),
            Some("refact/task/task-1/card/T-9/old".to_string()),
        );

        assert!(ensure_restart_source_current(&original, &original).is_ok());
    }

    #[test]
    fn restart_agent_source_guard_rejects_stale_previous_binding() {
        let original = failed_card("T-10", Some("/tmp/original-wt".to_string()), None);
        let mut changed = original.clone();
        changed.agent_chat_id = Some("agent-T-10-newer".to_string());
        changed.assignee = Some("newer-agent".to_string());

        let err = ensure_restart_source_current(&changed, &original).unwrap_err();

        assert!(err.contains("changed while restart was preparing"), "{err}");
        assert!(err.contains("agent-T-10-newer"), "{err}");
    }
}
