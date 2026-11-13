pub use refact_tools::tools::tool_task_documents::*;

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
use chrono::{SecondsFormat, Utc};
#[cfg(test)]
#[allow(unused_imports)]
use serde::{Deserialize, Serialize};
#[cfg(test)]
#[allow(unused_imports)]
use serde_json::Value;
#[cfg(test)]
#[allow(unused_imports)]
use tokio::fs;
#[cfg(test)]
#[allow(unused_imports)]
use tokio::sync::Mutex as AMutex;
#[cfg(test)]
#[allow(unused_imports)]
use refact_runtime_api::AtCommandsContext;
#[cfg(test)]
#[allow(unused_imports)]
use refact_runtime_api::GlobalContextCtx;
#[cfg(test)]
#[allow(unused_imports)]
use refact_core::chat_types::{ChatContent, ChatMessage, ContextEnum};
#[cfg(test)]
#[allow(unused_imports)]
use crate::global_context::GlobalContext;
#[cfg(test)]
#[allow(unused_imports)]
use refact_postprocessing::pp_command_output::OutputFilter;
#[cfg(test)]
#[allow(unused_imports)]
use refact_runtime_api::task_storage as storage;
#[cfg(test)]
#[allow(unused_imports)]
use crate::tools::tools_description::{
    json_schema_from_params, Tool, ToolDesc, ToolSource, ToolSourceType,
};

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    async fn temp_documents_dir() -> (TempDir, PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join(".refact/tasks/task-1/documents");
        fs::create_dir_all(&dir).await.unwrap();
        (temp, dir)
    }

    #[test]
    fn slug_validation_accepts_alphanumeric_dash_underscore() {
        assert!(validate_slug("main-plan_1").is_ok());
        assert!(validate_slug("T-23").is_ok());
        assert!(validate_slug("").is_err());
        assert!(validate_slug("main plan").is_err());
        assert!(validate_slug("../plan").is_err());
        assert!(validate_slug("plan.md").is_err());
    }

    #[tokio::test]
    async fn create_get_and_list_round_trip() {
        let (_temp, dir) = temp_documents_dir().await;
        create_document_at(
            &dir,
            "main-plan",
            "Main Plan",
            "plan",
            "Initial body",
            true,
            vec!["T-22".to_string(), "T-23".to_string()],
            "planner",
        )
        .await
        .unwrap();

        let doc = get_document_at(&dir, "main-plan", None).await.unwrap();
        assert_eq!(doc.frontmatter.slug, "main-plan");
        assert_eq!(doc.frontmatter.name, "Main Plan");
        assert_eq!(doc.frontmatter.kind, "plan");
        assert_eq!(doc.frontmatter.author_role, "planner");
        assert!(doc.frontmatter.pinned);
        assert_eq!(doc.frontmatter.version, 1);
        assert_eq!(doc.frontmatter.relevant_cards, vec!["T-22", "T-23"]);
        assert_eq!(doc.content, "Initial body");

        let docs = list_documents_at(&dir).await.unwrap();
        assert_eq!(docs.len(), 1);
        let table = format_doc_list(&docs);
        assert!(table.contains("| slug | name | kind | pinned | version | updated_at |"));
        assert!(table.contains("| main-plan | Main Plan | plan | true | 1 |"));
    }

    #[tokio::test]
    async fn next_available_slug_appends_version_for_collision() {
        let (_temp, dir) = temp_documents_dir().await;
        create_document_at(
            &dir,
            "initial-plan",
            "Initial Plan",
            "plan",
            "v1",
            true,
            Vec::new(),
            "planner",
        )
        .await
        .unwrap();
        create_document_at(
            &dir,
            "initial-plan-2",
            "Initial Plan",
            "plan",
            "v2",
            true,
            Vec::new(),
            "planner",
        )
        .await
        .unwrap();

        let slug = next_available_slug_at(&dir, "initial-plan").await.unwrap();

        assert_eq!(slug, "initial-plan-3");
    }

    #[tokio::test]
    async fn update_creates_history_and_get_version_reads_historical_body() {
        let (_temp, dir) = temp_documents_dir().await;
        create_document_at(
            &dir,
            "spec",
            "Spec",
            "spec",
            "v1",
            true,
            Vec::new(),
            "planner",
        )
        .await
        .unwrap();
        update_document_at(&dir, "spec", "v2").await.unwrap();
        update_document_at(&dir, "spec", "v3").await.unwrap();

        let latest = get_document_at(&dir, "spec", None).await.unwrap();
        assert_eq!(latest.frontmatter.version, 3);
        assert_eq!(latest.content, "v3");

        let version_one = get_document_at(&dir, "spec", Some(1)).await.unwrap();
        assert_eq!(version_one.frontmatter.version, 1);
        assert_eq!(version_one.content, "v1");

        let history = history_document_at(&dir, "spec").await.unwrap();
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].version, 1);
        assert_eq!(history[1].version, 2);
    }

    #[tokio::test]
    async fn history_cap_keeps_last_twenty_snapshots() {
        let (_temp, dir) = temp_documents_dir().await;
        create_document_at(
            &dir,
            "runbook",
            "Runbook",
            "runbook",
            "v1",
            true,
            Vec::new(),
            "planner",
        )
        .await
        .unwrap();
        for version in 2..=26 {
            update_document_at(&dir, "runbook", &format!("v{}", version))
                .await
                .unwrap();
        }

        let history = history_document_at(&dir, "runbook").await.unwrap();
        assert_eq!(history.len(), HISTORY_CAP);
        assert_eq!(history.first().unwrap().version, 6);
        assert_eq!(history.last().unwrap().version, 25);
        assert!(get_document_at(&dir, "runbook", Some(1)).await.is_err());
    }

    #[tokio::test]
    async fn append_and_pin_update_document_versions() {
        let (_temp, dir) = temp_documents_dir().await;
        create_document_at(
            &dir,
            "brief",
            "Brief",
            "brief",
            "",
            true,
            Vec::new(),
            "agents",
        )
        .await
        .unwrap();
        let appended = append_document_at(&dir, "brief", "Notes body")
            .await
            .unwrap();
        assert_eq!(appended.frontmatter.version, 2);
        assert_eq!(appended.content, "## Section\n\nNotes body\n");

        let pinned = pin_document_at(&dir, "brief", false).await.unwrap();
        assert_eq!(pinned.frontmatter.version, 3);
        assert!(!pinned.frontmatter.pinned);

        let history = history_document_at(&dir, "brief").await.unwrap();
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].version, 1);
        assert_eq!(history[1].version, 2);
    }

    #[tokio::test]
    async fn delete_moves_document_to_deleted_history() {
        let (_temp, dir) = temp_documents_dir().await;
        create_document_at(
            &dir,
            "postmortem",
            "Postmortem",
            "postmortem",
            "body",
            true,
            Vec::new(),
            "planner",
        )
        .await
        .unwrap();

        let deleted_path = delete_document_at(&dir, "postmortem").await.unwrap();
        assert!(!document_path(&dir, "postmortem").exists());
        assert!(deleted_path.exists());
        assert!(deleted_path.starts_with(deleted_dir(&dir)));
        assert!(get_document_at(&dir, "postmortem", None).await.is_err());

        let history = history_document_at(&dir, "postmortem").await.unwrap();
        assert_eq!(history.len(), 1);
        assert!(history[0].deleted);
        assert_eq!(history[0].version, 1);
    }

    #[tokio::test]
    async fn invalid_kind_is_rejected() {
        let (_temp, dir) = temp_documents_dir().await;
        let err = create_document_at(
            &dir,
            "bad-kind",
            "Bad Kind",
            "memo",
            "body",
            true,
            Vec::new(),
            "planner",
        )
        .await
        .unwrap_err();
        assert!(err.contains("invalid kind"));
    }

    use refact_runtime_api::task_storage as storage;
    use refact_tasks::types::{TaskMeta, TaskStatus};

    async fn setup_task_gcx(task_id: &str) -> (TempDir, Arc<GlobalContext>) {
        let temp = tempfile::tempdir().unwrap();
        let gcx = crate::global_context::tests::make_test_gcx().await;
        *gcx.documents_state.workspace_folders.lock().unwrap() = vec![temp.path().to_path_buf()];
        let task_dir = temp.path().join(".refact/tasks").join(task_id);
        fs::create_dir_all(&task_dir).await.unwrap();
        let now = chrono::Utc::now().to_rfc3339();
        let meta = TaskMeta {
            schema_version: 1,
            id: task_id.to_string(),
            name: task_id.to_string(),
            status: TaskStatus::Planning,
            created_at: now.clone(),
            updated_at: now,
            cards_total: 0,
            cards_done: 0,
            cards_failed: 0,
            agents_active: 0,
            base_branch: None,
            base_commit: None,
            default_agent_model: None,
            is_name_generated: false,
            last_agents_summary_at: None,
            planner_session_state: None,
        };
        storage::save_task_meta(gcx.clone(), task_id, &meta)
            .await
            .unwrap();
        (temp, gcx)
    }

    #[tokio::test]
    async fn list_task_documents_for_api_returns_summaries_in_creation_order() {
        let (_temp, gcx) = setup_task_gcx("task-docs-list").await;
        let tid = "task-docs-list";
        create_task_document_for_api_for_ctx(&gcx.chat_ctx(gcx.clone()),
            tid,
            CreateDocumentRequest {
                slug: "doc-a".to_string(),
                name: "A".to_string(),
                kind: "plan".to_string(),
                content: "body-a".to_string(),
                pinned: None,
                relevant_cards: None,
            },
        )
        .await
        .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        create_task_document_for_api_for_ctx(&gcx.chat_ctx(gcx.clone()),
            tid,
            CreateDocumentRequest {
                slug: "doc-b".to_string(),
                name: "B".to_string(),
                kind: "spec".to_string(),
                content: "body-b".to_string(),
                pinned: None,
                relevant_cards: None,
            },
        )
        .await
        .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        create_task_document_for_api_for_ctx(&gcx.chat_ctx(gcx.clone()),
            tid,
            CreateDocumentRequest {
                slug: "doc-c".to_string(),
                name: "C".to_string(),
                kind: "brief".to_string(),
                content: "body-c".to_string(),
                pinned: None,
                relevant_cards: None,
            },
        )
        .await
        .unwrap();
        let result = list_task_documents_for_api_for_ctx(&gcx.chat_ctx(gcx.clone()), tid).await.unwrap();
        assert_eq!(result.task_id, tid);
        assert_eq!(result.documents.len(), 3);
        let slugs: Vec<&str> = result.documents.iter().map(|d| d.slug.as_str()).collect();
        assert_eq!(slugs, vec!["doc-a", "doc-b", "doc-c"]);
    }

    #[tokio::test]
    async fn get_task_document_for_api_returns_detail_with_content() {
        let (_temp, gcx) = setup_task_gcx("task-docs-get").await;
        let tid = "task-docs-get";
        create_task_document_for_api_for_ctx(&gcx.chat_ctx(gcx.clone()),
            tid,
            CreateDocumentRequest {
                slug: "my-doc".to_string(),
                name: "My Doc".to_string(),
                kind: "plan".to_string(),
                content: "hello content".to_string(),
                pinned: Some(true),
                relevant_cards: Some(vec!["T-1".to_string()]),
            },
        )
        .await
        .unwrap();
        let detail = get_task_document_for_api_for_ctx(&gcx.chat_ctx(gcx.clone()), tid, "my-doc", None)
            .await
            .unwrap();
        assert_eq!(detail.slug, "my-doc");
        assert_eq!(detail.name, "My Doc");
        assert_eq!(detail.content, "hello content");
        assert!(detail.pinned);
        assert_eq!(detail.relevant_cards, vec!["T-1"]);
    }

    #[tokio::test]
    async fn create_task_document_for_api_rejects_duplicate_slug() {
        let (_temp, gcx) = setup_task_gcx("task-docs-dup").await;
        let tid = "task-docs-dup";
        create_task_document_for_api_for_ctx(&gcx.chat_ctx(gcx.clone()),
            tid,
            CreateDocumentRequest {
                slug: "dup".to_string(),
                name: "Dup".to_string(),
                kind: "spec".to_string(),
                content: "first".to_string(),
                pinned: None,
                relevant_cards: None,
            },
        )
        .await
        .unwrap();
        let err = create_task_document_for_api_for_ctx(&gcx.chat_ctx(gcx.clone()),
            tid,
            CreateDocumentRequest {
                slug: "dup".to_string(),
                name: "Dup Again".to_string(),
                kind: "spec".to_string(),
                content: "second".to_string(),
                pinned: None,
                relevant_cards: None,
            },
        )
        .await
        .unwrap_err();
        assert!(err.contains("already exists"), "error was: {}", err);
    }

    #[tokio::test]
    async fn update_task_document_for_api_increments_version_and_writes_history() {
        let (_temp, gcx) = setup_task_gcx("task-docs-upd").await;
        let tid = "task-docs-upd";
        create_task_document_for_api_for_ctx(&gcx.chat_ctx(gcx.clone()),
            tid,
            CreateDocumentRequest {
                slug: "upd".to_string(),
                name: "Upd".to_string(),
                kind: "plan".to_string(),
                content: "v1".to_string(),
                pinned: None,
                relevant_cards: None,
            },
        )
        .await
        .unwrap();
        let updated = update_task_document_for_api_for_ctx(&gcx.chat_ctx(gcx.clone()), tid, "upd", "v2".to_string(), None)
            .await
            .unwrap();
        assert_eq!(updated.version, 2);
        assert_eq!(updated.content, "v2");
        let hist = history_task_document_for_api_for_ctx(&gcx.chat_ctx(gcx.clone()), tid, "upd")
            .await
            .unwrap();
        assert!(!hist.history.is_empty());
        assert_eq!(hist.history[0].version, 1);
    }

    #[tokio::test]
    async fn update_task_document_for_api_updates_content_and_pin_in_one_version() {
        let (_temp, gcx) = setup_task_gcx("task-docs-update-pin").await;
        let tid = "task-docs-update-pin";
        create_task_document_for_api_for_ctx(&gcx.chat_ctx(gcx.clone()),
            tid,
            CreateDocumentRequest {
                slug: "combined".to_string(),
                name: "Combined".to_string(),
                kind: "plan".to_string(),
                content: "v1".to_string(),
                pinned: Some(true),
                relevant_cards: None,
            },
        )
        .await
        .unwrap();

        let updated = update_task_document_for_api_for_ctx(&gcx.chat_ctx(gcx.clone()),
            tid,
            "combined",
            "v2".to_string(),
            Some(false),
        )
        .await
        .unwrap();

        assert_eq!(updated.version, 2);
        assert_eq!(updated.content, "v2");
        assert!(!updated.pinned);
        let hist = history_task_document_for_api_for_ctx(&gcx.chat_ctx(gcx.clone()), tid, "combined")
            .await
            .unwrap();
        assert_eq!(hist.history.len(), 1);
        assert_eq!(hist.history[0].version, 1);

        let unchanged = update_task_document_for_api_for_ctx(&gcx.chat_ctx(gcx.clone()),
            tid,
            "combined",
            "v2".to_string(),
            Some(false),
        )
        .await
        .unwrap();

        assert_eq!(unchanged.version, 2);
        let hist = history_task_document_for_api_for_ctx(&gcx.chat_ctx(gcx.clone()), tid, "combined")
            .await
            .unwrap();
        assert_eq!(hist.history.len(), 1);
    }

    #[tokio::test]
    async fn pin_task_document_for_api_flips_pinned_flag() {
        let (_temp, gcx) = setup_task_gcx("task-docs-pin").await;
        let tid = "task-docs-pin";
        create_task_document_for_api_for_ctx(&gcx.chat_ctx(gcx.clone()),
            tid,
            CreateDocumentRequest {
                slug: "pin-doc".to_string(),
                name: "Pin Doc".to_string(),
                kind: "brief".to_string(),
                content: "content".to_string(),
                pinned: Some(true),
                relevant_cards: None,
            },
        )
        .await
        .unwrap();
        let unpinned = pin_task_document_for_api_for_ctx(&gcx.chat_ctx(gcx.clone()), tid, "pin-doc", false)
            .await
            .unwrap();
        assert!(!unpinned.pinned);
        assert_eq!(unpinned.version, 2);
        let repinned = pin_task_document_for_api_for_ctx(&gcx.chat_ctx(gcx.clone()), tid, "pin-doc", true)
            .await
            .unwrap();
        assert!(repinned.pinned);
        assert_eq!(repinned.version, 3);
        let history = history_task_document_for_api_for_ctx(&gcx.chat_ctx(gcx.clone()), tid, "pin-doc")
            .await
            .unwrap();
        assert_eq!(history.history.len(), 2);
        assert_eq!(history.history[0].version, 1);
        assert_eq!(history.history[1].version, 2);
    }

    #[tokio::test]
    async fn delete_task_document_for_api_removes_document() {
        let (_temp, gcx) = setup_task_gcx("task-docs-del").await;
        let tid = "task-docs-del";
        create_task_document_for_api_for_ctx(&gcx.chat_ctx(gcx.clone()),
            tid,
            CreateDocumentRequest {
                slug: "del-doc".to_string(),
                name: "Del Doc".to_string(),
                kind: "spec".to_string(),
                content: "content".to_string(),
                pinned: None,
                relevant_cards: None,
            },
        )
        .await
        .unwrap();
        delete_task_document_for_api_for_ctx(&gcx.chat_ctx(gcx.clone()), tid, "del-doc")
            .await
            .unwrap();
        let result = list_task_documents_for_api_for_ctx(&gcx.chat_ctx(gcx.clone()), tid).await.unwrap();
        assert!(result.documents.iter().all(|d| d.slug != "del-doc"));
    }

    #[tokio::test]
    async fn history_task_document_for_api_returns_entries_for_each_version() {
        let (_temp, gcx) = setup_task_gcx("task-docs-hist").await;
        let tid = "task-docs-hist";
        create_task_document_for_api_for_ctx(&gcx.chat_ctx(gcx.clone()),
            tid,
            CreateDocumentRequest {
                slug: "hist-doc".to_string(),
                name: "Hist".to_string(),
                kind: "runbook".to_string(),
                content: "v1".to_string(),
                pinned: None,
                relevant_cards: None,
            },
        )
        .await
        .unwrap();
        update_task_document_for_api_for_ctx(&gcx.chat_ctx(gcx.clone()), tid, "hist-doc", "v2".to_string(), None)
            .await
            .unwrap();
        update_task_document_for_api_for_ctx(&gcx.chat_ctx(gcx.clone()), tid, "hist-doc", "v3".to_string(), None)
            .await
            .unwrap();
        let hist = history_task_document_for_api_for_ctx(&gcx.chat_ctx(gcx.clone()), tid, "hist-doc")
            .await
            .unwrap();
        assert_eq!(hist.history.len(), 2);
        assert_eq!(hist.history[0].version, 1);
        assert_eq!(hist.history[1].version, 2);
    }
}
