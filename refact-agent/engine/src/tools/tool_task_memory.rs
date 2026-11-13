pub use refact_tools::tools::tool_task_memory::*;

#[cfg(test)]
#[allow(unused_imports)]
use std::collections::{HashMap, HashSet};
#[cfg(test)]
#[allow(unused_imports)]
use std::fmt;
#[cfg(test)]
#[allow(unused_imports)]
use std::path::{Component, Path, PathBuf};
#[cfg(test)]
#[allow(unused_imports)]
use std::str::FromStr;
#[cfg(test)]
#[allow(unused_imports)]
use std::sync::Arc;
#[cfg(test)]
#[allow(unused_imports)]
use async_trait::async_trait;
#[cfg(test)]
#[allow(unused_imports)]
use chrono::{DateTime, Duration, Local, Utc};
#[cfg(test)]
#[allow(unused_imports)]
use serde::{Deserialize, Deserializer, Serialize, Serializer};
#[cfg(test)]
#[allow(unused_imports)]
use serde_json::Value;
#[cfg(test)]
#[allow(unused_imports)]
use serde_yaml::{Mapping as YamlMapping, Value as YamlValue};
#[cfg(test)]
#[allow(unused_imports)]
use tokio::fs;
#[cfg(test)]
#[allow(unused_imports)]
use tokio::io::AsyncWriteExt;
#[cfg(test)]
#[allow(unused_imports)]
use tokio::sync::Mutex as AMutex;
#[cfg(test)]
#[allow(unused_imports)]
use tracing::info;
#[cfg(test)]
#[allow(unused_imports)]
use uuid::Uuid;
#[cfg(test)]
#[allow(unused_imports)]
use walkdir::WalkDir;
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
use refact_core::knowledge_index::{KnowledgeSearchFilters, KnowledgeSearchHit};
#[cfg(test)]
#[allow(unused_imports)]
use refact_runtime_api::knowledge_index::build_knowledge_index_for_ctx;
#[cfg(test)]
#[allow(unused_imports)]
use refact_postprocessing::pp_command_output::OutputFilter;
#[cfg(test)]
#[allow(unused_imports)]
use refact_runtime_api::ChatCtx;
#[cfg(test)]
#[allow(unused_imports)]
use refact_runtime_api::task_storage::find_task_dir_for_ctx;
#[cfg(test)]
#[allow(unused_imports)]
use crate::tools::tools_description::{
    Tool, ToolDesc, ToolSource, ToolSourceType, json_schema_from_params,
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_state::AppState;
    use refact_runtime_api::AtCommandsContext;
    use refact_chat_api::TaskMeta;
    use crate::tools::tools_description::Tool;
    use chrono::TimeZone;
    use serde_json::json;
    use std::sync::Arc;
    use tokio::sync::Mutex as AMutex;

    const MEMORY_FILE: &str = "2026-05-22_023548_edf49905_master-plan.md";

    fn args(items: &[(&str, Value)]) -> HashMap<String, Value> {
        items
            .iter()
            .map(|(key, value)| ((*key).to_string(), value.clone()))
            .collect()
    }

    async fn write_memory(dir: &Path, file_name: &str, frontmatter: TaskMemoryFrontmatter) {
        write_memory_with_body(dir, file_name, frontmatter, "Body").await;
    }

    async fn write_memory_with_body(
        dir: &Path,
        file_name: &str,
        frontmatter: TaskMemoryFrontmatter,
        body: &str,
    ) {
        tokio::fs::create_dir_all(dir).await.unwrap();
        tokio::fs::write(dir.join(file_name), render_memory_file(&frontmatter, body))
            .await
            .unwrap();
    }

    async fn read_frontmatter(path: &Path) -> TaskMemoryFrontmatter {
        let text = tokio::fs::read_to_string(path).await.unwrap();
        parse_memory_file(&text).unwrap().0
    }

    async fn make_ccx(gcx: Arc<GlobalContext>, role: &str) -> Arc<AMutex<AtCommandsContext>> {
        Arc::new(AMutex::new(
            crate::at_commands::at_commands::at_commands_new_from_app(
                refact_runtime_api::app_state_from_gcx(gcx).await,
                4096,
                20,
                false,
                vec![],
                "planner-chat".to_string(),
                None,
                "model".to_string(),
                Some(TaskMeta {
                    task_id: "task-1".to_string(),
                    role: role.to_string(),
                    agent_id: None,
                    card_id: None,
                    planner_chat_id: None,
                }),
                None,
            )
            .await,
        ))
    }

    async fn make_task_with_memory(
        role: &str,
    ) -> (
        tempfile::TempDir,
        Arc<GlobalContext>,
        Arc<AMutex<AtCommandsContext>>,
        PathBuf,
    ) {
        let temp = tempfile::tempdir().unwrap();
        let gcx = crate::global_context::tests::make_test_gcx().await;
        *gcx.documents_state.workspace_folders.lock().unwrap() = vec![temp.path().to_path_buf()];
        let task_dir = temp.path().join(".refact/tasks/task-1");
        let memories_dir = task_dir.join(MEMORIES_DIR);
        tokio::fs::create_dir_all(&task_dir).await.unwrap();
        write_memory(
            &memories_dir,
            MEMORY_FILE,
            TaskMemoryFrontmatter {
                task_id: Some("task-1".to_string()),
                title: Some("Master Plan".to_string()),
                ..Default::default()
            },
        )
        .await;
        let ccx = make_ccx(gcx.clone(), role).await;
        (temp, gcx, ccx, memories_dir.join(MEMORY_FILE))
    }

    fn inbox_memory(
        path: &str,
        created_at: DateTime<Utc>,
        kind: MemoryKind,
        namespace: MemoryNamespace,
        body: &str,
    ) -> TaskMemoryInboxEntry {
        TaskMemoryInboxEntry {
            path: PathBuf::from(path),
            frontmatter: TaskMemoryFrontmatter {
                created_at: Some(created_at.to_rfc3339()),
                kind,
                namespace,
                ..Default::default()
            },
            body: body.to_string(),
            created_at,
            created_at_known: true,
        }
    }

    #[test]
    fn memory_enums_parse_display_round_trip() {
        for kind in [
            MemoryKind::Decision,
            MemoryKind::Spec,
            MemoryKind::Finding,
            MemoryKind::Gotcha,
            MemoryKind::Risk,
            MemoryKind::Handoff,
            MemoryKind::Progress,
            MemoryKind::Postmortem,
            MemoryKind::Brief,
            MemoryKind::Freeform,
        ] {
            assert_eq!(kind.to_string().parse::<MemoryKind>().unwrap(), kind);
        }

        for namespace in [
            MemoryNamespace::Global,
            MemoryNamespace::Task,
            MemoryNamespace::Card("T-1".to_string()),
            MemoryNamespace::Agent("A-1".to_string()),
        ] {
            assert_eq!(
                namespace.to_string().parse::<MemoryNamespace>().unwrap(),
                namespace
            );
        }

        for status in [
            MemoryStatus::Active,
            MemoryStatus::Archived,
            MemoryStatus::Superseded,
        ] {
            assert_eq!(status.to_string().parse::<MemoryStatus>().unwrap(), status);
        }
    }

    #[test]
    fn memory_namespace_card_serializes_with_prefix() {
        let namespace = MemoryNamespace::Card("T-N".to_string());
        let value = serde_json::to_value(&namespace).unwrap();
        assert_eq!(value, json!("card:T-N"));
        let parsed: MemoryNamespace = serde_json::from_value(value).unwrap();
        assert_eq!(parsed, namespace);
    }

    #[test]
    fn legacy_memory_file_loads_new_fields_with_defaults() {
        let content = "---\ncreated_at: 2026-05-22T00:00:00Z\ntask_id: task-1\nrole: agents\ntags: [old, memory]\n---\n\nLegacy body";
        let (frontmatter, body) = parse_memory_file(content).unwrap();

        assert_eq!(frontmatter.kind, MemoryKind::Freeform);
        assert_eq!(frontmatter.namespace, MemoryNamespace::Task);
        assert!(!frontmatter.pinned);
        assert_eq!(frontmatter.status, MemoryStatus::Active);
        assert_eq!(frontmatter.supersedes, None);
        assert_eq!(
            frontmatter.tags,
            vec!["old".to_string(), "memory".to_string()]
        );
        assert_eq!(body, "Legacy body");
    }

    #[test]
    fn frontmatter_writer_omits_default_new_fields() {
        let frontmatter = TaskMemoryFrontmatter {
            created_at: Some("2026-05-22T00:00:00Z".to_string()),
            task_id: Some("task-1".to_string()),
            role: Some("planner".to_string()),
            ..Default::default()
        };
        let yaml = frontmatter.to_yaml_block();

        assert!(!yaml.contains("kind:"));
        assert!(!yaml.contains("namespace:"));
        assert!(!yaml.contains("pinned:"));
        assert!(!yaml.contains("supersedes:"));
        assert!(!yaml.contains("status:"));
    }

    #[tokio::test]
    async fn supersedes_updates_referenced_memory_status() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("old.md");
        let frontmatter = TaskMemoryFrontmatter {
            title: Some("Old".to_string()),
            kind: MemoryKind::Finding,
            ..Default::default()
        };
        tokio::fs::write(&path, render_memory_file(&frontmatter, "Old body"))
            .await
            .unwrap();

        let updated_path = mark_memory_superseded(temp.path(), "old.md").await.unwrap();

        assert_eq!(updated_path, path);
        let text = tokio::fs::read_to_string(&path).await.unwrap();
        let (updated_frontmatter, body) = parse_memory_file(&text).unwrap();
        assert_eq!(updated_frontmatter.kind, MemoryKind::Finding);
        assert_eq!(updated_frontmatter.status, MemoryStatus::Superseded);
        assert_eq!(body, "Old body");
    }

    #[tokio::test]
    async fn task_mem_pin_unpin_round_trip() {
        let (_temp, _gcx, ccx, path) = make_task_with_memory("planner").await;
        let mut tool = ToolTaskMemoryPin::new();

        tool.tool_execute(
            ccx.clone(),
            &"call".to_string(),
            &args(&[("memory_id", json!("master-plan")), ("pinned", json!(true))]),
        )
        .await
        .unwrap();
        assert!(read_frontmatter(&path).await.pinned);

        tool.tool_execute(
            ccx,
            &"call".to_string(),
            &args(&[("memory_id", json!(MEMORY_FILE)), ("pinned", json!(false))]),
        )
        .await
        .unwrap();
        assert!(!read_frontmatter(&path).await.pinned);
    }

    #[tokio::test]
    async fn task_mem_archive_moves_file() {
        let (_temp, _gcx, ccx, path) = make_task_with_memory("planner").await;
        let mut tool = ToolTaskMemoryArchive::new();

        tool.tool_execute(
            ccx,
            &"call".to_string(),
            &args(&[("memory_id", json!("master-plan"))]),
        )
        .await
        .unwrap();

        let archived_path = path
            .parent()
            .unwrap()
            .join(ARCHIVED_MEMORIES_DIR)
            .join(MEMORY_FILE);
        assert!(!path.exists());
        assert!(archived_path.exists());
        assert_eq!(
            read_frontmatter(&archived_path).await.status,
            MemoryStatus::Archived
        );
    }

    #[tokio::test]
    async fn task_mem_unarchive_moves_file_back() {
        let (_temp, _gcx, ccx, path) = make_task_with_memory("planner").await;
        let mut archive = ToolTaskMemoryArchive::new();
        let mut unarchive = ToolTaskMemoryUnarchive::new();

        archive
            .tool_execute(
                ccx.clone(),
                &"call".to_string(),
                &args(&[("memory_id", json!("master-plan"))]),
            )
            .await
            .unwrap();
        unarchive
            .tool_execute(
                ccx,
                &"call".to_string(),
                &args(&[("memory_id", json!("master-plan"))]),
            )
            .await
            .unwrap();

        let archived_path = path
            .parent()
            .unwrap()
            .join(ARCHIVED_MEMORIES_DIR)
            .join(MEMORY_FILE);
        assert!(path.exists());
        assert!(!archived_path.exists());
        assert_eq!(read_frontmatter(&path).await.status, MemoryStatus::Active);
    }

    #[tokio::test]
    async fn task_mem_pin_rejects_non_planner_role() {
        let (_temp, _gcx, ccx, _path) = make_task_with_memory("agents").await;
        let mut tool = ToolTaskMemoryPin::new();

        let err = tool
            .tool_execute(
                ccx,
                &"call".to_string(),
                &args(&[("memory_id", json!("master-plan")), ("pinned", json!(true))]),
            )
            .await
            .unwrap_err();

        assert!(err.contains("can only be called by the task planner"));
    }

    #[tokio::test]
    async fn memory_inbox_cursor_read_write_round_trip() {
        let temp = tempfile::tempdir().unwrap();
        let cursor = parse_rfc3339_utc("2026-05-22T00:00:00Z").unwrap();

        assert_eq!(read_memory_inbox_cursor(temp.path()).await.unwrap(), None);
        write_memory_inbox_cursor(temp.path(), cursor)
            .await
            .unwrap();

        assert_eq!(
            read_memory_inbox_cursor(temp.path()).await.unwrap(),
            Some(cursor)
        );
        assert_eq!(
            tokio::fs::read_to_string(temp.path().join(MEMORY_INBOX_CURSOR_FILE))
                .await
                .unwrap(),
            cursor.to_rfc3339()
        );
    }

    #[test]
    fn new_memories_filter_sorts_and_limits() {
        let cursor = parse_rfc3339_utc("2026-05-22T00:00:00Z").unwrap();
        let newer = parse_rfc3339_utc("2026-05-22T02:00:00Z").unwrap();
        let newest = parse_rfc3339_utc("2026-05-22T03:00:00Z").unwrap();
        let older = parse_rfc3339_utc("2026-05-21T23:00:00Z").unwrap();
        let memories = vec![
            inbox_memory(
                "newest.md",
                newest,
                MemoryKind::Finding,
                MemoryNamespace::Task,
                "newest",
            ),
            inbox_memory(
                "newer.md",
                newer,
                MemoryKind::Finding,
                MemoryNamespace::Task,
                "newer",
            ),
            inbox_memory(
                "older.md",
                older,
                MemoryKind::Finding,
                MemoryNamespace::Task,
                "older",
            ),
        ];

        let filtered = new_memories_since(&memories, cursor, 1);

        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].memory_id(), "newest.md");
    }

    #[test]
    fn stale_candidate_detection_uses_progress_age_and_namespace() {
        let now = parse_rfc3339_utc("2026-05-22T00:00:00Z").unwrap();
        let old = now - Duration::days(8);
        let fresh = now - Duration::days(3);
        let memories = vec![
            inbox_memory(
                "old-progress.md",
                old,
                MemoryKind::Progress,
                MemoryNamespace::Task,
                "old",
            ),
            inbox_memory(
                "global-progress.md",
                old,
                MemoryKind::Progress,
                MemoryNamespace::Global,
                "global",
            ),
            inbox_memory(
                "old-decision.md",
                old,
                MemoryKind::Decision,
                MemoryNamespace::Task,
                "decision",
            ),
            inbox_memory(
                "fresh-progress.md",
                fresh,
                MemoryKind::Progress,
                MemoryNamespace::Task,
                "fresh",
            ),
        ];

        let stale = stale_memory_candidates(&memories, now);

        assert_eq!(stale.len(), 1);
        assert_eq!(stale[0].memory_id(), "old-progress.md");
    }

    #[test]
    fn duplicate_pair_detection_uses_body_token_overlap() {
        let now = parse_rfc3339_utc("2026-05-22T00:00:00Z").unwrap();
        let left_body = "---\ntitle: Left\n---\n\nalpha beta gamma delta epsilon zeta";
        let right_body = "---\ntitle: Different\n---\n\nalpha beta gamma delta epsilon extra";
        let other_body = "kappa lambda mu nu xi omicron";
        let memories = vec![
            inbox_memory(
                "left.md",
                now,
                MemoryKind::Finding,
                MemoryNamespace::Task,
                left_body,
            ),
            inbox_memory(
                "right.md",
                now,
                MemoryKind::Finding,
                MemoryNamespace::Task,
                right_body,
            ),
            inbox_memory(
                "other.md",
                now,
                MemoryKind::Finding,
                MemoryNamespace::Task,
                other_body,
            ),
        ];

        let pairs = duplicate_memory_pairs(&memories);

        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].left_path, PathBuf::from("left.md"));
        assert_eq!(pairs[0].right_path, PathBuf::from("right.md"));
        assert!(pairs[0].overlap_percent > 70);
    }

    #[tokio::test]
    async fn task_mem_inbox_renders_all_sections() {
        let (_temp, _gcx, ccx, path) = make_task_with_memory("planner").await;
        let memories_dir = path.parent().unwrap().to_path_buf();
        let now = Utc::now();
        write_memory_with_body(
            &memories_dir,
            "new.md",
            TaskMemoryFrontmatter {
                created_at: Some((now - Duration::hours(2)).to_rfc3339()),
                title: Some("New Decision".to_string()),
                kind: MemoryKind::Decision,
                namespace: MemoryNamespace::Card("T-1".to_string()),
                card_id: Some("T-1".to_string()),
                ..Default::default()
            },
            "shared alpha beta gamma delta epsilon zeta",
        )
        .await;
        write_memory_with_body(
            &memories_dir,
            "duplicate.md",
            TaskMemoryFrontmatter {
                created_at: Some((now - Duration::hours(3)).to_rfc3339()),
                title: Some("Duplicate Decision".to_string()),
                kind: MemoryKind::Decision,
                namespace: MemoryNamespace::Card("T-1".to_string()),
                card_id: Some("T-1".to_string()),
                ..Default::default()
            },
            "shared alpha beta gamma delta epsilon extra",
        )
        .await;
        write_memory_with_body(
            &memories_dir,
            "stale.md",
            TaskMemoryFrontmatter {
                created_at: Some((now - Duration::days(8)).to_rfc3339()),
                title: Some("Old Progress".to_string()),
                kind: MemoryKind::Progress,
                namespace: MemoryNamespace::Task,
                ..Default::default()
            },
            "old progress",
        )
        .await;
        let mut tool = ToolTaskMemoryInbox::new();

        let (_, contexts) = tool
            .tool_execute(
                ccx,
                &"call".to_string(),
                &args(&[("since", json!((now - Duration::days(2)).to_rfc3339()))]),
            )
            .await
            .unwrap();
        let output = match &contexts[0] {
            ContextEnum::ChatMessage(message) => match &message.content {
                ChatContent::SimpleText(text) => text.clone(),
                _ => panic!("expected text output"),
            },
            _ => panic!("expected chat message"),
        };

        assert!(output.contains("# Memory Inbox"));
        assert!(output.contains("## New since"));
        assert!(output.contains("New Decision"));
        assert!(output.contains("## Stale candidates (1)"));
        assert!(output.contains("stale.md"));
        assert!(output.contains("## Possible duplicates (1 pairs)"));
        assert!(output.contains("token overlap"));
        assert!(output.contains("## Actions"));
    }

    #[tokio::test]
    async fn task_mem_triage_done_persists_cursor() {
        let (_temp, _gcx, ccx, path) = make_task_with_memory("planner").await;
        let task_dir = path.parent().unwrap().parent().unwrap().to_path_buf();
        let cursor = parse_rfc3339_utc("2026-05-22T04:05:06Z").unwrap();
        let mut tool = ToolTaskMemoryTriageDone::new();

        tool.tool_execute(
            ccx,
            &"call".to_string(),
            &args(&[("cursor", json!(cursor.to_rfc3339()))]),
        )
        .await
        .unwrap();

        assert_eq!(
            read_memory_inbox_cursor(&task_dir).await.unwrap(),
            Some(cursor)
        );
    }

    #[tokio::test]
    async fn task_mem_inbox_rejects_non_planner_role() {
        let (_temp, _gcx, ccx, _path) = make_task_with_memory("agents").await;
        let mut tool = ToolTaskMemoryInbox::new();

        let err = tool
            .tool_execute(ccx, &"call".to_string(), &args(&[]))
            .await
            .unwrap_err();

        assert!(err.contains("can only be called by the task planner"));
    }

    #[tokio::test]
    async fn memory_without_created_at_uses_mtime_and_respects_cursor() {
        let temp = tempfile::tempdir().unwrap();
        let memories_dir = temp.path().join(MEMORIES_DIR);
        tokio::fs::create_dir_all(&memories_dir).await.unwrap();
        let path = memories_dir.join("missing-created-at.md");
        tokio::fs::write(
            &path,
            render_memory_file(
                &TaskMemoryFrontmatter {
                    title: Some("Missing Created At".to_string()),
                    ..Default::default()
                },
                "Body",
            ),
        )
        .await
        .unwrap();
        let mtime = Utc.with_ymd_and_hms(2026, 5, 22, 1, 0, 0).single().unwrap();
        filetime::set_file_mtime(
            &path,
            filetime::FileTime::from_unix_time(mtime.timestamp(), 0),
        )
        .unwrap();

        let (memories, warnings) = load_task_memory_inbox_entries(&memories_dir).await.unwrap();

        assert!(warnings.is_empty());
        assert_eq!(memories.len(), 1);
        assert_eq!(memories[0].created_at, mtime);
        assert!(memories[0].created_at_known);
        assert_eq!(
            new_memories_since(&memories, mtime - Duration::seconds(1), 10).len(),
            1
        );
        assert!(new_memories_since(&memories, mtime, 10).is_empty());
    }

    #[tokio::test]
    async fn malformed_memory_warns_and_inbox_continues() {
        let temp = tempfile::tempdir().unwrap();
        let memories_dir = temp.path().join(MEMORIES_DIR);
        tokio::fs::create_dir_all(&memories_dir).await.unwrap();
        tokio::fs::write(
            memories_dir.join("valid.md"),
            render_memory_file(
                &TaskMemoryFrontmatter {
                    created_at: Some("2026-05-22T00:00:00Z".to_string()),
                    title: Some("Valid".to_string()),
                    ..Default::default()
                },
                "valid body",
            ),
        )
        .await
        .unwrap();
        tokio::fs::write(
            memories_dir.join("malformed.md"),
            "---\ntags: [unterminated\n---\n\nbad body",
        )
        .await
        .unwrap();

        let (memories, warnings) = load_task_memory_inbox_entries(&memories_dir).await.unwrap();
        let output = render_memory_inbox(
            parse_rfc3339_utc("2026-05-21T00:00:00Z").unwrap(),
            parse_rfc3339_utc("2026-05-22T01:00:00Z").unwrap(),
            &memories,
            &[],
            &[],
            &warnings,
        );

        assert_eq!(memories.len(), 1);
        assert_eq!(warnings.len(), 1);
        assert!(output.contains("Valid"));
        assert!(output.contains("## Warnings (1 skipped)"));
        assert!(output.contains("malformed.md"));
        assert!(output.contains("Failed to parse memory frontmatter"));
    }

    #[test]
    fn duplicate_detection_caps_large_memory_sets() {
        let now = parse_rfc3339_utc("2026-05-22T00:00:00Z").unwrap();
        let memories = (0..600)
            .map(|idx| {
                inbox_memory(
                    &format!("memory-{idx}.md"),
                    now,
                    MemoryKind::Finding,
                    MemoryNamespace::Task,
                    "shared alpha beta gamma delta epsilon zeta",
                )
            })
            .collect::<Vec<_>>();

        let pairs = duplicate_memory_pairs(&memories);

        assert_eq!(pairs.len(), MAX_DUPLICATE_COMPARISONS);
        assert!(pairs.iter().all(|pair| pair.overlap_percent == 100));
    }

    #[test]
    fn card_id_auto_sets_memory_namespace() {
        assert_eq!(
            resolve_memory_namespace(None, Some("T-9")).unwrap(),
            MemoryNamespace::Card("T-9".to_string())
        );
        assert_eq!(
            resolve_memory_namespace(Some("task"), Some("T-9")).unwrap(),
            MemoryNamespace::Task
        );
        assert_eq!(
            resolve_memory_namespace(None, None).unwrap(),
            MemoryNamespace::Task
        );
    }

    #[tokio::test]
    async fn task_mem_search_filters_by_kind() {
        let temp = tempfile::tempdir().unwrap();
        let memories_dir = temp.path().join(".refact/tasks/task-1/memories");
        tokio::fs::create_dir_all(&memories_dir).await.unwrap();
        tokio::fs::write(
            memories_dir.join("decision.md"),
            "---\ntitle: Decision\ntask_id: task-1\nkind: decision\n---\n\nshared needle",
        )
        .await
        .unwrap();
        tokio::fs::write(
            memories_dir.join("risk.md"),
            "---\ntitle: Risk\ntask_id: task-1\nkind: risk\n---\n\nshared needle",
        )
        .await
        .unwrap();
        let gcx = crate::global_context::tests::make_test_gcx().await;
        *gcx.documents_state.workspace_folders.lock().unwrap() = vec![temp.path().to_path_buf()];

        let hits = search_task_memories_and_documents(&gcx.chat_ctx(gcx.clone()),
            "needle",
            KnowledgeSearchFilters {
                scope: Some("task".to_string()),
                kind: Some("decision".to_string()),
                task_id: Some("task-1".to_string()),
                ..Default::default()
            },
            10,
        )
        .await;

        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].card.title, "Decision");
    }

    #[tokio::test]
    async fn task_mem_search_finds_tag_filename_and_content() {
        let temp = tempfile::tempdir().unwrap();
        let memories_dir = temp.path().join(".refact/tasks/task-1/memories");
        tokio::fs::create_dir_all(&memories_dir).await.unwrap();
        tokio::fs::write(
            memories_dir.join("filename-match.md"),
            "---\ntitle: Filename\ntask_id: task-1\nkind: finding\ntags: [tag-match]\n---\n\ncontent-match",
        )
        .await
        .unwrap();
        let gcx = crate::global_context::tests::make_test_gcx().await;
        *gcx.documents_state.workspace_folders.lock().unwrap() = vec![temp.path().to_path_buf()];

        for query in ["tag-match", "filename-match", "content-match"] {
            let hits = search_task_memories_and_documents(&gcx.chat_ctx(gcx.clone()),
                query,
                KnowledgeSearchFilters {
                    scope: Some("task".to_string()),
                    task_id: Some("task-1".to_string()),
                    ..Default::default()
                },
                10,
            )
            .await;
            assert_eq!(hits.len(), 1, "query {query}");
            assert_eq!(hits[0].card.title, "Filename");
        }
    }

    #[tokio::test]
    async fn handle_task_memory_facets_returns_sorted_deduplicated_namespaces() {
        let temp = tempfile::tempdir().unwrap();
        let gcx = crate::global_context::tests::make_test_gcx().await;
        *gcx.documents_state.workspace_folders.lock().unwrap() = vec![temp.path().to_path_buf()];
        let task_dir = temp.path().join(".refact/tasks/task-1");
        let memories_dir = task_dir.join(MEMORIES_DIR);
        tokio::fs::create_dir_all(&memories_dir).await.unwrap();

        for (file, namespace, kind, tags) in &[
            (
                "mem-a.md",
                MemoryNamespace::Task,
                MemoryKind::Decision,
                vec!["planner".to_string(), "search".to_string()],
            ),
            (
                "mem-b.md",
                MemoryNamespace::Global,
                MemoryKind::Spec,
                vec!["search".to_string()],
            ),
            (
                "mem-c.md",
                MemoryNamespace::Task,
                MemoryKind::Decision,
                vec!["planner".to_string()],
            ),
        ] {
            tokio::fs::write(
                memories_dir.join(file),
                render_memory_file(
                    &TaskMemoryFrontmatter {
                        namespace: namespace.clone(),
                        kind: *kind,
                        tags: tags.clone(),
                        ..Default::default()
                    },
                    "body",
                ),
            )
            .await
            .unwrap();
        }

        let result = task_memory_facets_for_api(&gcx.chat_ctx(gcx.clone()), "task-1").await.unwrap();
        assert_eq!(result.task_id, "task-1");
        assert_eq!(result.namespaces, vec!["global", "task"]);
        assert_eq!(result.kinds, vec!["decision", "spec"]);
        assert_eq!(result.tags, vec!["planner", "search"]);
        assert_eq!(result.total_count, 3);
    }

    #[tokio::test]
    async fn handle_task_memory_facets_counts_pinned_correctly() {
        let temp = tempfile::tempdir().unwrap();
        let gcx = crate::global_context::tests::make_test_gcx().await;
        *gcx.documents_state.workspace_folders.lock().unwrap() = vec![temp.path().to_path_buf()];
        let task_dir = temp.path().join(".refact/tasks/task-1");
        let memories_dir = task_dir.join(MEMORIES_DIR);
        tokio::fs::create_dir_all(&memories_dir).await.unwrap();

        for (file, pinned) in &[
            ("pinned.md", true),
            ("active.md", false),
            ("also-pinned.md", true),
        ] {
            tokio::fs::write(
                memories_dir.join(file),
                render_memory_file(
                    &TaskMemoryFrontmatter {
                        pinned: *pinned,
                        ..Default::default()
                    },
                    "body",
                ),
            )
            .await
            .unwrap();
        }

        let result = task_memory_facets_for_api(&gcx.chat_ctx(gcx.clone()), "task-1").await.unwrap();
        assert_eq!(result.total_count, 3);
        assert_eq!(result.pinned_count, 2);
    }

    #[tokio::test]
    async fn handle_task_memory_facets_does_not_load_memory_bodies() {
        let temp = tempfile::tempdir().unwrap();
        let gcx = crate::global_context::tests::make_test_gcx().await;
        *gcx.documents_state.workspace_folders.lock().unwrap() = vec![temp.path().to_path_buf()];
        let task_dir = temp.path().join(".refact/tasks/task-1");
        let memories_dir = task_dir.join(MEMORIES_DIR);
        tokio::fs::create_dir_all(&memories_dir).await.unwrap();

        tokio::fs::write(
            memories_dir.join("mem.md"),
            render_memory_file(
                &TaskMemoryFrontmatter {
                    kind: MemoryKind::Risk,
                    namespace: MemoryNamespace::Task,
                    tags: vec!["critical".to_string()],
                    ..Default::default()
                },
                "very large body content that should not appear in facets response",
            ),
        )
        .await
        .unwrap();

        let result = task_memory_facets_for_api(&gcx.chat_ctx(gcx.clone()), "task-1").await.unwrap();
        let serialized = serde_json::to_string(&result).unwrap();
        assert!(
            !serialized.contains("very large body content"),
            "facets response must not include memory body content"
        );
        assert_eq!(result.kinds, vec!["risk"]);
        assert_eq!(result.namespaces, vec!["task"]);
        assert_eq!(result.tags, vec!["critical"]);
    }
}
