pub use refact_tools::tools::buddy::memory::*;

#[cfg(test)]
#[allow(unused_imports)]
use refact_knowledge_graph::memory_types::normalize_memory_tags;
#[cfg(test)]
#[allow(unused_imports)]
use std::collections::{HashMap, HashSet};
#[cfg(test)]
#[allow(unused_imports)]
use std::path::{Component, Path, PathBuf};
#[cfg(test)]
#[allow(unused_imports)]
use std::sync::Arc;
#[cfg(test)]
#[allow(unused_imports)]
use async_trait::async_trait;
#[cfg(test)]
#[allow(unused_imports)]
use chrono::{Local, Utc};
#[cfg(test)]
#[allow(unused_imports)]
use serde_json::Value;
#[cfg(test)]
#[allow(unused_imports)]
use sha2::{Digest, Sha256};
#[cfg(test)]
#[allow(unused_imports)]
use tokio::sync::Mutex as AMutex;
#[cfg(test)]
#[allow(unused_imports)]
use walkdir::WalkDir;
#[cfg(test)]
#[allow(unused_imports)]
use refact_runtime_api::AtCommandsContext;
#[cfg(test)]
#[allow(unused_imports)]
use refact_buddy_core::memory_lifecycle_model::{ compute_content_hash, normalize_kind, normalize_tags, MemoryCreatePayload, MemoryLifecycleOp, MemoryLifecyclePayload, MemoryOpStatus, MemoryOpType, MemorySource, };
#[cfg(test)]
#[allow(unused_imports)]
use refact_runtime_api::memory_lifecycle_runtime::{ archive_memory_file_checked_for_ctx, find_memory_by_source_for_ctx, propose_supersede_for_near_duplicate_for_ctx, };
#[cfg(test)]
#[allow(unused_imports)]
use refact_buddy_core::autonomous_redaction::redact_and_cap_text;
#[cfg(test)]
#[allow(unused_imports)]
use refact_buddy_core::storage::enqueue_memory_op;
#[cfg(test)]
#[allow(unused_imports)]
use refact_core::chat_types::{ChatContent, ChatMessage, ContextEnum};
#[cfg(test)]
#[allow(unused_imports)]
use refact_runtime_api::ChatCtx;
#[cfg(test)]
#[allow(unused_imports)]
use refact_files::file_filter::KNOWLEDGE_FOLDER_NAME;
#[cfg(test)]
#[allow(unused_imports)]
use refact_runtime_api::files_correction::get_unscoped_project_dirs_for_ctx;
#[cfg(test)]
#[allow(unused_imports)]
use refact_knowledge_graph::kg_structs::KnowledgeFrontmatter;
#[cfg(test)]
#[allow(unused_imports)]
use refact_core::knowledge_index::KnowledgeCard;
#[cfg(test)]
#[allow(unused_imports)]
use refact_runtime_api::memories::{ get_global_knowledge_dir_for_ctx, update_memory_document_frontmatter_for_ctx, };
#[cfg(test)]
#[allow(unused_imports)]
use crate::tools::tools_description::{Tool, ToolDesc, ToolSource, ToolSourceType};

#[cfg(test)]
mod tests {
    use super::*;
    use refact_runtime_api::AtCommandsContext;
    use crate::buddy::storage::load_memory_ops;
    use crate::global_context::GlobalContext;
    use crate::tools::tools_description::Tool;
    use refact_worktrees::types::WorktreeMeta;
    use std::collections::HashMap;

    async fn test_gcx(root: &Path) -> Arc<GlobalContext> {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        {
            let gcx_read = gcx.clone();
            *gcx_read.documents_state.workspace_folders.lock().unwrap() = vec![root.to_path_buf()];
        }
        gcx
    }

    async fn ccx(root: &Path) -> Arc<AMutex<AtCommandsContext>> {
        let gcx = test_gcx(root).await;
        Arc::new(AMutex::new(
            crate::at_commands::at_commands::at_commands_new_from_app(
                refact_runtime_api::app_state_from_gcx(gcx).await,
                4096,
                20,
                false,
                vec![],
                "chat".to_string(),
                None,
                "model".to_string(),
                None,
                Some(WorktreeMeta {
                    id: "wt".to_string(),
                    kind: "chat".to_string(),
                    root: root.to_path_buf(),
                    source_workspace_root: root.to_path_buf(),
                    repo_root: root.to_path_buf(),
                    branch: None,
                    base_branch: None,
                    base_commit: None,
                    task_id: None,
                    card_id: None,
                    agent_id: None,
                    enforce: true,
                }),
            )
            .await,
        ))
    }

    fn text(result: &(bool, Vec<ContextEnum>)) -> String {
        result
            .1
            .iter()
            .filter_map(|item| match item {
                ContextEnum::ChatMessage(message) => match &message.content {
                    ChatContent::SimpleText(text) => Some(text.clone()),
                    _ => None,
                },
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn args(pairs: Vec<(&str, Value)>) -> HashMap<String, Value> {
        pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect()
    }

    fn merge_schema_kind_enum() -> Vec<String> {
        ToolBuddyMemoryMerge {
            config_path: String::new(),
        }
        .tool_description()
        .input_schema
        .get("properties")
        .and_then(|properties| properties.get("kind"))
        .and_then(|kind| kind.get("enum"))
        .and_then(Value::as_array)
        .unwrap()
        .iter()
        .map(|value| value.as_str().unwrap().to_string())
        .collect()
    }

    #[test]
    fn merge_schema_kind_enum_matches_writable_kinds() {
        let writable_kinds = WRITABLE_KINDS
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        assert_eq!(merge_schema_kind_enum(), writable_kinds);
    }

    async fn write_memory(
        root: &Path,
        name: &str,
        title: &str,
        kind: &str,
        tags: &[&str],
        body: &str,
    ) -> PathBuf {
        let dir = root.join(KNOWLEDGE_FOLDER_NAME);
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let path = dir.join(name);
        let content = format!(
            "---\ntitle: {title}\nkind: {kind}\ntags: [{}]\nstatus: active\ncontent_hash: {}\n---\n\n{body}\n",
            tags.join(", "),
            compute_content_hash(body)
        );
        tokio::fs::write(&path, content).await.unwrap();
        path
    }

    #[tokio::test]
    async fn buddy_memory_create_writes_real_file_and_indexes() {
        let dir = tempfile::tempdir().unwrap();
        let ccx = ccx(dir.path()).await;
        let mut tool = ToolBuddyMemoryCreate {
            config_path: String::new(),
        };
        let result = tool
            .tool_execute(
                ccx.clone(),
                &"call".to_string(),
                &args(vec![
                    ("title", serde_json::json!("Create Test")),
                    ("content", serde_json::json!("remember the frog")),
                    ("tags", serde_json::json!(["test"])),
                    ("kind", serde_json::json!("lesson")),
                    ("source_id", serde_json::json!("src-1")),
                ]),
            )
            .await
            .unwrap();
        assert!(text(&result).contains("Created memory:"));
        let files = std::fs::read_dir(dir.path().join(KNOWLEDGE_FOLDER_NAME))
            .unwrap()
            .count();
        assert_eq!(files, 1);
        let gcx = ccx.lock().await.app.gcx.clone();
        let cards = gcx.knowledge_index.lock().await.all_cards();
        assert!(cards.iter().any(|card| card.title == "Create Test"));
        let state = load_memory_ops(dir.path()).await;
        assert_eq!(state.applied_count, 1);
        let memory_path = std::fs::read_dir(dir.path().join(KNOWLEDGE_FOLDER_NAME))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let text = tokio::fs::read_to_string(memory_path).await.unwrap();
        let (frontmatter, _) = KnowledgeFrontmatter::parse(&text);
        assert_eq!(frontmatter.source_id.as_deref(), Some("src-1"));
        assert_eq!(
            frontmatter.source_content_hash.as_deref(),
            Some(compute_content_hash("remember the frog").as_str())
        );
    }

    #[tokio::test]
    async fn buddy_memory_create_skips_same_source_id_reworded_content() {
        let dir = tempfile::tempdir().unwrap();
        let ccx = ccx(dir.path()).await;
        let mut tool = ToolBuddyMemoryCreate {
            config_path: String::new(),
        };
        let base_args = |content: &str| {
            args(vec![
                ("title", serde_json::json!("Source Test")),
                ("content", serde_json::json!(content)),
                ("tags", serde_json::json!(["test"])),
                ("kind", serde_json::json!("lesson")),
                ("source_id", serde_json::json!("same-source")),
            ])
        };
        let first = tool
            .tool_execute(
                ccx.clone(),
                &"call".to_string(),
                &base_args("first wording"),
            )
            .await
            .unwrap();
        let second = tool
            .tool_execute(ccx, &"call".to_string(), &base_args("second wording"))
            .await
            .unwrap();
        assert!(text(&first).contains("Created memory:"));
        assert!(text(&second).contains("Skipped: identical memory exists"));
        assert_eq!(
            std::fs::read_dir(dir.path().join(KNOWLEDGE_FOLDER_NAME))
                .unwrap()
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn buddy_memory_create_skips_when_content_hash_duplicate() {
        let dir = tempfile::tempdir().unwrap();
        write_memory(dir.path(), "old.md", "Old", "lesson", &["dup"], "same body").await;
        let ccx = ccx(dir.path()).await;
        let mut tool = ToolBuddyMemoryCreate {
            config_path: String::new(),
        };
        let result = tool
            .tool_execute(
                ccx,
                &"call".to_string(),
                &args(vec![
                    ("title", serde_json::json!("New")),
                    ("content", serde_json::json!("same body")),
                    ("tags", serde_json::json!([])),
                    ("kind", serde_json::json!("lesson")),
                    ("source_id", serde_json::json!("src-dup")),
                ]),
            )
            .await
            .unwrap();
        assert!(text(&result).contains("Skipped: identical memory exists"));
        assert_eq!(
            std::fs::read_dir(dir.path().join(KNOWLEDGE_FOLDER_NAME))
                .unwrap()
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn buddy_memory_create_ignores_archived_exact_duplicate() {
        let dir = tempfile::tempdir().unwrap();
        let knowledge_dir = dir.path().join(KNOWLEDGE_FOLDER_NAME);
        tokio::fs::create_dir_all(&knowledge_dir).await.unwrap();
        let body = "same archived body";
        tokio::fs::write(
            knowledge_dir.join("archived.md"),
            format!(
                "---\ntitle: Archived\nkind: lesson\nstatus: archived\ncontent_hash: {}\ndeprecated_at: 2026-01-01\n---\n\n{body}\n",
                compute_content_hash(body)
            ),
        )
        .await
        .unwrap();
        let ccx = ccx(dir.path()).await;
        let mut tool = ToolBuddyMemoryCreate {
            config_path: String::new(),
        };
        let result = tool
            .tool_execute(
                ccx,
                &"call".to_string(),
                &args(vec![
                    ("title", serde_json::json!("Fresh")),
                    ("content", serde_json::json!(body)),
                    ("tags", serde_json::json!([])),
                    ("kind", serde_json::json!("lesson")),
                    ("source_id", serde_json::json!("fresh-source")),
                ]),
            )
            .await
            .unwrap();
        assert!(text(&result).contains("Created memory:"));
        assert_eq!(std::fs::read_dir(knowledge_dir).unwrap().count(), 2);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn find_duplicate_content_rejects_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let knowledge_dir = dir.path().join(KNOWLEDGE_FOLDER_NAME);
        tokio::fs::create_dir_all(&knowledge_dir).await.unwrap();
        tokio::fs::write(
            knowledge_dir.join("real.md"),
            "---\ntitle: Real\n---\n\nreal body\n",
        )
        .await
        .unwrap();

        let outside_dir = tempfile::tempdir().unwrap();
        let outside_body = format!("outside symlink body {}", uuid::Uuid::new_v4());
        let outside_file = outside_dir.path().join("outside.md");
        tokio::fs::write(
            &outside_file,
            format!("---\ntitle: Outside\n---\n\n{outside_body}\n"),
        )
        .await
        .unwrap();
        std::os::unix::fs::symlink(&outside_file, knowledge_dir.join("link.md")).unwrap();

        let gcx = test_gcx(dir.path()).await;
        let ctx = gcx.chat_ctx(gcx.clone());
        let found = find_duplicate_content(&ctx, &compute_content_hash(&outside_body)).await;
        assert!(found.is_none());
    }

    #[tokio::test]
    async fn buddy_memory_search_finds_existing_by_kind() {
        let dir = tempfile::tempdir().unwrap();
        write_memory(
            dir.path(),
            "lesson.md",
            "Lesson Hit",
            "lesson",
            &["alpha"],
            "frogs jump",
        )
        .await;
        let ccx = ccx(dir.path()).await;
        let mut tool = ToolBuddyMemorySearch {
            config_path: String::new(),
        };
        let result = tool
            .tool_execute(
                ccx,
                &"call".to_string(),
                &args(vec![
                    ("query", serde_json::json!("frogs")),
                    ("kind", serde_json::json!("lesson")),
                ]),
            )
            .await
            .unwrap();
        assert!(text(&result).contains("Lesson Hit"));
    }

    #[tokio::test]
    async fn scan_cards_caps_at_max_files() {
        let dir = tempfile::tempdir().unwrap();
        for idx in 0..1500 {
            let path = dir.path().join(format!("memory-{idx:04}.md"));
            tokio::fs::write(
                path,
                format!("---\ntitle: Memory {idx}\n---\n\nbody {idx}\n"),
            )
            .await
            .unwrap();
        }

        let cards = scan_cards_in_dirs(vec![dir.path().to_path_buf()]).await;

        assert_eq!(cards.len(), MAX_KNOWLEDGE_SCAN_FILES);
    }

    #[tokio::test]
    async fn buddy_memory_search_finds_existing_by_tag() {
        let dir = tempfile::tempdir().unwrap();
        write_memory(
            dir.path(),
            "tag.md",
            "Tag Hit",
            "insight",
            &["rare"],
            "hidden gem",
        )
        .await;
        let ccx = ccx(dir.path()).await;
        let mut tool = ToolBuddyMemorySearch {
            config_path: String::new(),
        };
        let result = tool
            .tool_execute(
                ccx,
                &"call".to_string(),
                &args(vec![
                    ("query", serde_json::json!("hidden")),
                    ("tags", serde_json::json!(["rare"])),
                ]),
            )
            .await
            .unwrap();
        assert!(text(&result).contains("Tag Hit"));
    }

    #[tokio::test]
    async fn buddy_memory_archive_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_memory(
            dir.path(),
            "archive.md",
            "Archive",
            "lesson",
            &["old"],
            "old body",
        )
        .await;
        let ccx = ccx(dir.path()).await;
        let mut tool = ToolBuddyMemoryArchive {
            config_path: String::new(),
        };
        let args = args(vec![
            ("path", serde_json::json!(path.to_string_lossy())),
            ("reason", serde_json::json!("obsolete")),
        ]);
        let first = tool
            .tool_execute(ccx.clone(), &"call".to_string(), &args)
            .await
            .unwrap();
        let second = tool
            .tool_execute(ccx, &"call".to_string(), &args)
            .await
            .unwrap();
        assert!(text(&first).contains("Archived:"));
        assert!(text(&second).contains("Already archived:"));
    }

    #[tokio::test]
    async fn buddy_memory_archive_rejects_path_outside_knowledge_dir() {
        let dir = tempfile::tempdir().unwrap();
        tokio::fs::create_dir_all(dir.path().join(KNOWLEDGE_FOLDER_NAME))
            .await
            .unwrap();
        let outside = dir.path().join("outside.md");
        tokio::fs::write(&outside, "---\n---\n\nbody")
            .await
            .unwrap();
        let ccx = ccx(dir.path()).await;
        let mut tool = ToolBuddyMemoryArchive {
            config_path: String::new(),
        };
        let err = tool
            .tool_execute(
                ccx,
                &"call".to_string(),
                &args(vec![
                    ("path", serde_json::json!(outside.to_string_lossy())),
                    ("reason", serde_json::json!("bad")),
                ]),
            )
            .await
            .unwrap_err();
        assert!(err.contains("outside knowledge") || err.contains("outside knowledge directories"));
    }

    #[cfg_attr(windows, ignore = "needs symlink privileges Windows does not grant by default")]
    #[tokio::test]
    async fn buddy_memory_archive_rejects_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let target = write_memory(
            dir.path(),
            "target.md",
            "Target",
            "lesson",
            &["old"],
            "body",
        )
        .await;
        let link = dir.path().join(KNOWLEDGE_FOLDER_NAME).join("link.md");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&target, &link).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_file(&target, &link).unwrap();
        let ccx = ccx(dir.path()).await;
        let mut tool = ToolBuddyMemoryArchive {
            config_path: String::new(),
        };
        let err = tool
            .tool_execute(
                ccx,
                &"call".to_string(),
                &args(vec![
                    ("path", serde_json::json!(link.to_string_lossy())),
                    ("reason", serde_json::json!("bad")),
                ]),
            )
            .await
            .unwrap_err();
        assert!(err.contains("symlink"));
    }

    #[tokio::test]
    async fn buddy_memory_retag_preserves_body() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_memory(
            dir.path(),
            "retag.md",
            "Retag",
            "lesson",
            &["old"],
            "keep this body",
        )
        .await;
        let ccx = ccx(dir.path()).await;
        let mut tool = ToolBuddyMemoryRetag {
            config_path: String::new(),
        };
        tool.tool_execute(
            ccx,
            &"call".to_string(),
            &args(vec![
                ("path", serde_json::json!(path.to_string_lossy())),
                ("new_tags", serde_json::json!(["new"])),
            ]),
        )
        .await
        .unwrap();
        let text = tokio::fs::read_to_string(&path).await.unwrap();
        assert!(text.contains("keep this body"));
        assert!(text.contains("new"));
        assert!(text.contains("buddy"));
    }

    #[tokio::test]
    async fn buddy_memory_merge_creates_canonical_and_archives_superseded() {
        let dir = tempfile::tempdir().unwrap();
        let one = write_memory(dir.path(), "one.md", "One", "lesson", &["old"], "one body").await;
        let two = write_memory(dir.path(), "two.md", "Two", "lesson", &["old"], "two body").await;
        let ccx = ccx(dir.path()).await;
        let mut tool = ToolBuddyMemoryMerge {
            config_path: String::new(),
        };
        let result = tool
            .tool_execute(
                ccx,
                &"call".to_string(),
                &args(vec![
                    ("canonical_title", serde_json::json!("Canonical")),
                    ("canonical_content", serde_json::json!("canonical body")),
                    (
                        "superseded_paths",
                        serde_json::json!([one.to_string_lossy(), two.to_string_lossy()]),
                    ),
                    ("tags", serde_json::json!(["merged"])),
                    ("kind", serde_json::json!("lesson")),
                ]),
            )
            .await
            .unwrap();
        assert!(text(&result).contains("Merged 2 memories into"));
        assert!(tokio::fs::read_to_string(&one)
            .await
            .unwrap()
            .contains("status: archived"));
        assert!(tokio::fs::read_to_string(&two)
            .await
            .unwrap()
            .contains("status: archived"));
        assert!(
            std::fs::read_dir(dir.path().join(KNOWLEDGE_FOLDER_NAME))
                .unwrap()
                .count()
                >= 3
        );
    }
}
