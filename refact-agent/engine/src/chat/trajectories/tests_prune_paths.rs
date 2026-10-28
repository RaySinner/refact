use super::*;
use super::test_support::*;
    use crate::chat::diagnostics::{is_ui_only_message, make_ui_only_error_message};
    use crate::chat::perf_diagnostics::{self, MemoryPerfSink, PerfClock, PerfRecorder};
    use crate::chat::types::{
        ActiveCommandContext, ChatEvent, CompressionPhase, CompressionReason, EventEnvelope,
    };
    use crate::vecdb::vdb_structs::{EmbeddingModelConfig, SearchResult, VecDbStatus, VecdbRecord};
    use async_trait::async_trait;
    use refact_core::vecdb_types::VecdbSearch;
    use refact_chat_api::{
        BuddyThreadMeta, ClaudeCodeIdentity, FrozenRequestPrefix, GoalBudget, GoalProgress,
        GoalStatus,
    };
    use serial_test::serial;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::sync::Mutex as StdMutex;
    #[tokio::test]
    async fn prune_buddy_conversations_spares_recently_modified_overflow() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("conversations");
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let newest = write_conversation_file(&dir, "newest", 100).await;
        let recent_overflow = write_conversation_file(&dir, "recent-overflow", 2 * 3600).await;
        let old_overflow = write_conversation_file(&dir, "old-overflow", 3 * 86_400).await;

        let removed = prune_buddy_conversations_in_dir(&dir, 1).await.unwrap();

        assert_eq!(removed, 1);
        assert!(newest.exists());
        assert!(recent_overflow.exists());
        assert!(!old_overflow.exists());
    }

    #[tokio::test]
    async fn prune_buddy_conversations_noop_under_keep_limit_and_for_missing_dir() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("conversations");
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let kept = write_conversation_file(&dir, "chat-a", 100).await;

        let removed = prune_buddy_conversations_in_dir(&dir, 2).await.unwrap();

        assert_eq!(removed, 0);
        assert!(kept.exists());

        let missing = prune_buddy_conversations_in_dir(&temp.path().join("nope"), 2)
            .await
            .unwrap();
        assert_eq!(missing, 0);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn prune_buddy_conversations_skips_symlinked_entries() {
        let temp = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let dir = temp.path().join("conversations");
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let real = write_conversation_file(&dir, "real", 3 * 86_400).await;
        let outside_file = write_conversation_file(outside.path(), "outside", 3 * 86_400).await;
        std::os::unix::fs::symlink(&outside_file, dir.join("linked.json")).unwrap();

        let removed = prune_buddy_conversations_in_dir(&dir, 0).await.unwrap();

        assert_eq!(removed, 1);
        assert!(!real.exists());
        assert!(outside_file.exists());
        assert!(dir.join("linked.json").exists());
    }

    #[tokio::test]
    async fn new_root_chat_saves_into_its_own_folder() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let chat_id = "layout-root-chat";

        save_trajectory_snapshot(
            gcx.clone(),
            test_snapshot(
                chat_id,
                "Root",
                vec![ChatMessage::new("user".to_string(), "hi".to_string())],
            ),
        )
        .await
        .unwrap();

        let nested = nested_trajectory_path(dir.path(), chat_id, chat_id);
        assert!(tokio::fs::try_exists(&nested).await.unwrap());
        let flat = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        assert!(!tokio::fs::try_exists(&flat).await.unwrap());
        assert!(load_trajectory_for_chat(&gcx.chat_ctx(gcx.clone()), chat_id).await.is_some());
    }

    #[serial(trajectory_perf)]
    #[tokio::test]
    async fn performance_diagnostics_cover_snapshot_save_boundaries_without_raw_id_or_path() {
        let _lock = serial_test_guard();
        let (_guard, sink, recorder) = install_perf_recorder();
        let dir = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;
        let chat_id = "performance-save-chat";
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        {
            let mut session = session_arc.lock().await;
            session.messages.push(ChatMessage::new(
                "user".to_string(),
                "diagnostic content remains private".to_string(),
            ));
            session.increment_version();
        }

        let snapshot = {
            let session = session_arc.lock().await;
            trajectory_snapshot_from_session(&session)
        };
        save_trajectory_snapshot(gcx, snapshot).await.unwrap();
        drop(app);

        let events = sink.events();
        let components: std::collections::HashSet<_> =
            events.iter().map(|event| event.component).collect();
        assert!(components.contains("trajectory.snapshot"));
        assert!(components.contains("trajectory.serialize"));
        assert!(components.contains("trajectory.atomic_write"));
        assert!(components.contains("trajectory.commit"));
        assert_eq!(
            events
                .iter()
                .filter(|event| event.component == "trajectory.serialize")
                .count(),
            1
        );
        assert_eq!(
            events
                .iter()
                .find(|event| event.component == "trajectory.serialize")
                .expect("serialize metric")
                .elapsed_us,
            2
        );
        assert!(events.iter().all(|event| event.outcome == "success"));
        assert!(events.iter().all(|event| {
            !format!("{event:?}").contains(chat_id)
                && !format!("{event:?}").contains(dir.path().to_str().unwrap())
        }));
        let expected_chat_hash = recorder.hash_identity_for_test(chat_id);
        assert!(events
            .iter()
            .any(|event| { event.chat_id_hash.as_deref() == Some(expected_chat_hash.as_str()) }));
    }

    #[serial(trajectory_perf)]
    #[tokio::test]
    async fn performance_diagnostics_record_failed_trajectory_commit() {
        let _lock = serial_test_guard();
        let (_guard, sink, recorder) = install_perf_recorder();
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let chat_id = "invalid/id";
        let result = save_trajectory_snapshot(gcx, test_snapshot(chat_id, "Bad", Vec::new())).await;

        assert!(result.is_err());
        let expected_chat_hash = recorder.hash_identity_for_test(chat_id);
        let events = sink
            .events()
            .into_iter()
            .filter(|event| {
                event.component == PerfComponent::TrajectoryCommit.as_str()
                    && event.outcome == PerfOutcome::Failure.as_str()
                    && event.chat_id_hash.as_deref() == Some(expected_chat_hash.as_str())
            })
            .collect::<Vec<_>>();
        assert_eq!(events.len(), 1);
    }

    #[serial(trajectory_perf)]
    #[tokio::test]
    async fn trajectory_metadata_path_failure_records_one_serialize_failure() {
        let _lock = serial_test_guard();
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let chat_id = "serialize-path-failure";
        let path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        write_trajectory_file(&path, "different-id", "Existing", "2024-01-01T00:00:00Z").await;
        let before = tokio::fs::read_to_string(&path).await.unwrap();
        let (_guard, sink, _) = install_perf_recorder();

        let result = save_trajectory_snapshot(
            gcx,
            test_snapshot(
                chat_id,
                "New",
                vec![ChatMessage::new("user".to_string(), "hi".to_string())],
            ),
        )
        .await;

        assert!(result.is_err());
        assert_eq!(tokio::fs::read_to_string(&path).await.unwrap(), before);
        let serialize_events: Vec<_> = sink
            .events()
            .into_iter()
            .filter(|event| event.component == "trajectory.serialize")
            .collect();
        assert_eq!(serialize_events.len(), 1);
        assert_eq!(serialize_events[0].outcome, "failure");
    }

    #[serial(trajectory_perf)]
    #[tokio::test]
    async fn trajectory_message_serialization_failure_preserves_existing_file() {
        let _lock = serial_test_guard();
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let chat_id = "serialize-message-failure";
        let path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        write_trajectory_file(&path, chat_id, "Durable", "2024-01-01T00:00:00Z").await;
        let before = tokio::fs::read_to_string(&path).await.unwrap();
        let (_guard, sink, _) = install_perf_recorder();
        let message = ChatMessage {
            message_id: "unserializable-message".to_string(),
            role: "assistant".to_string(),
            content: ChatContent::SimpleText("private message content".to_string()),
            ..Default::default()
        };
        set_test_message_serialization_failure(Some(message.message_id.clone()));

        let error = save_trajectory_snapshot(gcx, test_snapshot(chat_id, "New", vec![message]))
            .await
            .unwrap_err();
        set_test_message_serialization_failure(None);

        assert!(error.contains("index 0"));
        assert!(error.contains("unserializable-message"));
        assert!(error.contains("assistant"));
        assert!(!error.contains("private message content"));
        assert_eq!(tokio::fs::read_to_string(&path).await.unwrap(), before);
        let serialize_events: Vec<_> = sink
            .events()
            .into_iter()
            .filter(|event| event.component == "trajectory.serialize")
            .collect();
        assert_eq!(serialize_events.len(), 1);
        assert_eq!(serialize_events[0].outcome, "failure");
    }

    #[tokio::test]
    async fn trajectory_commit_round_trips_every_message() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let chat_id = "serialize-all-messages";
        let messages = vec![
            ChatMessage {
                message_id: "message-user".to_string(),
                role: "user".to_string(),
                content: ChatContent::SimpleText("question".to_string()),
                ..Default::default()
            },
            ChatMessage {
                message_id: "message-assistant".to_string(),
                role: "assistant".to_string(),
                content: ChatContent::SimpleText("answer".to_string()),
                reasoning_content: Some("reasoning".to_string()),
                tool_calls: Some(vec![crate::call_validation::ChatToolCall {
                    id: "call-1".to_string(),
                    index: Some(0),
                    function: crate::call_validation::ChatToolFunction {
                        name: "sleep".to_string(),
                        arguments: "{}".to_string(),
                    },
                    tool_type: "function".to_string(),
                    extra_content: None,
                    started_at_ms: Some(1_700_000_000_000),
                    completed_at_ms: Some(1_700_000_008_000),
                }]),
                ..Default::default()
            },
            ChatMessage {
                message_id: "message-tool".to_string(),
                role: "tool".to_string(),
                content: ChatContent::SimpleText("result".to_string()),
                tool_call_id: "call-1".to_string(),
                ..Default::default()
            },
        ];
        let expected = serde_json::to_value(&messages).unwrap();

        save_trajectory_snapshot(gcx.clone(), test_snapshot(chat_id, "All", messages))
            .await
            .unwrap();

        let path = nested_trajectory_path(dir.path(), chat_id, chat_id);
        let saved: serde_json::Value =
            serde_json::from_str(&tokio::fs::read_to_string(path).await.unwrap()).unwrap();
        assert_eq!(saved["messages"], expected);
        let loaded = load_trajectory_for_chat(&gcx.chat_ctx(gcx.clone()), chat_id).await.unwrap();
        let tool_call = loaded.messages[1]
            .tool_calls
            .as_ref()
            .unwrap()
            .first()
            .unwrap();
        assert_eq!(tool_call.started_at_ms, Some(1_700_000_000_000));
        assert_eq!(tool_call.completed_at_ms, Some(1_700_000_008_000));
    }

    #[tokio::test]
    async fn subchat_saves_into_root_chat_folder() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let root_id = "layout-parent-chat";
        let child_id = "subchat-layout-child";

        let mut snapshot = test_snapshot(
            child_id,
            "Child",
            vec![ChatMessage::new("user".to_string(), "hi".to_string())],
        );
        snapshot.root_chat_id = Some(root_id.to_string());
        snapshot.parent_id = Some(root_id.to_string());
        snapshot.link_type = Some("subagent".to_string());
        save_trajectory_snapshot(gcx.clone(), snapshot)
            .await
            .unwrap();

        let nested = nested_trajectory_path(dir.path(), root_id, child_id);
        assert!(
            tokio::fs::try_exists(&nested).await.unwrap(),
            "child must live in the root chat folder"
        );
        assert!(load_trajectory_for_chat(&gcx.chat_ctx(gcx.clone()), child_id).await.is_some());
    }

    #[tokio::test]
    async fn resaving_a_trajectory_keeps_the_original_created_at() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let chat_id = "created-at-stable-on-resave";

        let mut first = test_snapshot(
            chat_id,
            "First",
            vec![ChatMessage::new("user".to_string(), "hi".to_string())],
        );
        first.created_at = "2024-01-01T00:00:00Z".to_string();
        save_trajectory_snapshot(gcx.clone(), first).await.unwrap();
        let path = nested_trajectory_path(dir.path(), chat_id, chat_id);
        let first_raw: serde_json::Value =
            serde_json::from_str(&tokio::fs::read_to_string(&path).await.unwrap()).unwrap();
        let first_created_at = first_raw["created_at"].as_str().unwrap().to_string();
        let first_updated_at = first_raw["updated_at"].as_str().unwrap().to_string();

        let mut second = test_snapshot(
            chat_id,
            "Second",
            vec![
                ChatMessage::new("user".to_string(), "hi".to_string()),
                ChatMessage::new("assistant".to_string(), "there".to_string()),
            ],
        );
        second.created_at = "2025-06-01T00:00:00Z".to_string();
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        save_trajectory_snapshot(gcx, second).await.unwrap();
        let second_raw: serde_json::Value =
            serde_json::from_str(&tokio::fs::read_to_string(&path).await.unwrap()).unwrap();

        assert_eq!(first_created_at, "2024-01-01T00:00:00Z");
        assert_eq!(second_raw["created_at"].as_str().unwrap(), first_created_at);
        assert_ne!(second_raw["updated_at"].as_str().unwrap(), first_updated_at);
    }

    #[tokio::test]
    async fn resaving_child_without_root_id_updates_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let root_id = "layout-resave-root";
        let child_id = "subchat-layout-resave-child";

        let mut first = test_snapshot(
            child_id,
            "Child",
            vec![ChatMessage::new("user".to_string(), "hi".to_string())],
        );
        first.root_chat_id = Some(root_id.to_string());
        first.parent_id = Some(root_id.to_string());
        first.link_type = Some("subagent".to_string());
        save_trajectory_snapshot(gcx.clone(), first).await.unwrap();

        let mut second = test_snapshot(
            child_id,
            "Child",
            vec![
                ChatMessage::new("user".to_string(), "hi".to_string()),
                ChatMessage::new("assistant".to_string(), "there".to_string()),
            ],
        );
        second.root_chat_id = None;
        second.parent_id = None;
        save_trajectory_snapshot(gcx.clone(), second).await.unwrap();

        let nested = nested_trajectory_path(dir.path(), root_id, child_id);
        let duplicate = nested_trajectory_path(dir.path(), child_id, child_id);
        assert!(tokio::fs::try_exists(&nested).await.unwrap());
        assert!(
            !tokio::fs::try_exists(&duplicate).await.unwrap(),
            "a re-save without root_chat_id must update in place, not fork a duplicate file"
        );

        let loaded = load_trajectory_for_chat(&gcx.chat_ctx(gcx.clone()), child_id)
            .await
            .expect("child must still resolve");
        assert_eq!(loaded.messages.len(), 2);
        assert_same_path(&loaded.source_path, &nested);
    }

    #[tokio::test]
    async fn legacy_flat_trajectory_keeps_being_written_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let chat_id = "layout-legacy-chat";
        let flat = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        write_trajectory_file(&flat, chat_id, "Legacy", "2024-01-01T00:00:00Z").await;

        save_trajectory_snapshot(
            gcx.clone(),
            test_snapshot(
                chat_id,
                "Legacy Updated",
                vec![ChatMessage::new("user".to_string(), "hi".to_string())],
            ),
        )
        .await
        .unwrap();

        let saved: serde_json::Value =
            serde_json::from_str(&tokio::fs::read_to_string(&flat).await.unwrap()).unwrap();
        assert_eq!(saved["title"], "Legacy Updated");
        let nested = nested_trajectory_path(dir.path(), chat_id, chat_id);
        assert!(
            !tokio::fs::try_exists(&nested).await.unwrap(),
            "legacy chats must not be duplicated into a folder"
        );
    }

    #[tokio::test]
    async fn nested_trajectory_is_indexed_and_resolvable_through_the_index() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let chat_id = "layout-indexed-chat";

        save_trajectory_snapshot(
            gcx.clone(),
            test_snapshot(
                chat_id,
                "Indexed",
                vec![ChatMessage::new("user".to_string(), "hi".to_string())],
            ),
        )
        .await
        .unwrap();

        AppState::from_gcx(gcx.clone())
            .await
            .chat
            .trajectory_index_coordinator
            .flush_all()
            .await
            .unwrap();

        let root = dir.path().join(".refact").join("trajectories");
        assert!(
            !tokio::fs::try_exists(root.join(chat_id).join("index.json"))
                .await
                .unwrap(),
            "index.json must not be created inside a chat folder"
        );

        let index = trajectory_index::read_trajectory_index(&root)
            .await
            .unwrap()
            .expect("root index should exist");
        let entry = index
            .entries
            .iter()
            .find(|entry| entry.id == chat_id)
            .expect("nested chat must be indexed");
        assert_eq!(entry.file_name, format!("{chat_id}/{chat_id}.json"));
        assert!(
            trajectory_index::trajectory_index_entry_is_fresh(&root, entry).await,
            "a nested index entry must not be treated as stale"
        );
        assert_eq!(
            trajectory_index::trajectory_file_path_for_entry(&root, entry),
            nested_trajectory_path(dir.path(), chat_id, chat_id)
        );
    }

    #[tokio::test]
    async fn id_mismatched_legacy_flat_slot_refuses_the_save() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let chat_id = "layout-squatted-chat";

        let root = dir.path().join(".refact").join("trajectories");
        tokio::fs::create_dir_all(&root).await.unwrap();
        tokio::fs::write(
            root.join(format!("{chat_id}.json")),
            serde_json::to_string(&serde_json::json!({
                "id": "some-other-chat",
                "title": "Squatter",
                "created_at": "2026-01-01T00:00:00Z",
                "messages": [],
            }))
            .unwrap(),
        )
        .await
        .unwrap();

        let result = save_trajectory_snapshot(
            gcx.clone(),
            test_snapshot(
                chat_id,
                "New",
                vec![ChatMessage::new("user".to_string(), "hi".to_string())],
            ),
        )
        .await;

        assert!(
            result.is_err(),
            "an id-mismatched legacy flat slot must refuse the save, not write elsewhere"
        );
        assert!(
            !tokio::fs::try_exists(nested_trajectory_path(dir.path(), chat_id, chat_id))
                .await
                .unwrap(),
            "refusal must not leave a nested copy behind"
        );
    }

    #[tokio::test]
    async fn internal_traces_are_not_written_into_the_index() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let root_id = "layout-trace-owner";
        let trace_id = "subchat-layout-trace";

        save_trajectory_snapshot(
            gcx.clone(),
            test_snapshot(
                root_id,
                "Owner",
                vec![ChatMessage::new("user".to_string(), "hi".to_string())],
            ),
        )
        .await
        .unwrap();

        let mut trace = test_snapshot(
            trace_id,
            "Trace",
            vec![ChatMessage::new("user".to_string(), "hi".to_string())],
        );
        trace.root_chat_id = Some(root_id.to_string());
        trace.link_type = Some(internal_trace_link_type("title_generation"));
        save_trajectory_snapshot(gcx.clone(), trace).await.unwrap();

        AppState::from_gcx(gcx.clone())
            .await
            .chat
            .trajectory_index_coordinator
            .flush_all()
            .await
            .unwrap();

        assert!(
            tokio::fs::try_exists(nested_trajectory_path(dir.path(), root_id, trace_id))
                .await
                .unwrap(),
            "the trace must still be written into the owning conversation folder"
        );

        let root = dir.path().join(".refact").join("trajectories");
        let index = trajectory_index::read_trajectory_index(&root)
            .await
            .unwrap()
            .expect("root index should exist");
        assert!(
            index.entries.iter().any(|entry| entry.id == root_id),
            "the owning chat stays indexed"
        );
        assert!(
            !index.entries.iter().any(|entry| entry.id == trace_id),
            "internal traces must not grow the index"
        );
    }

    #[test]
    fn internal_traces_are_not_enqueued_for_vectorization() {
        assert!(!should_vectorize_trajectory(
            &TrajectorySourceIdentity::Normal,
            Some("internal:title_generation")
        ));
        assert!(!should_vectorize_trajectory(
            &TrajectorySourceIdentity::Normal,
            Some("internal:commit_message")
        ));
        assert!(should_vectorize_trajectory(
            &TrajectorySourceIdentity::Normal,
            Some("subagent")
        ));
        assert!(should_vectorize_trajectory(
            &TrajectorySourceIdentity::Normal,
            None
        ));
    }

    #[test]
    fn top_level_link_type_classification_preserves_semantics() {
        let temp = tempfile::tempdir().unwrap();

        // Happy path: a top-level internal link_type is classified as an internal trace.
        assert!(parsed_top_level_link_type_is_internal(write_temp_json(
            &temp,
            "internal.json",
            r#"{"id":"a","link_type":"internal:title_generation","messages":[]}"#,
        )));

        // A link_type that is not the internal one.
        assert!(!parsed_top_level_link_type_is_internal(write_temp_json(
            &temp,
            "subagent.json",
            r#"{"id":"a","link_type":"subagent","messages":[]}"#,
        )));

        // No link_type field at all.
        assert!(!parsed_top_level_link_type_is_internal(write_temp_json(
            &temp,
            "missing.json",
            r#"{"id":"a","messages":[]}"#,
        )));

        // A null link_type is treated as absent, not internal.
        assert!(!parsed_top_level_link_type_is_internal(write_temp_json(
            &temp,
            "null.json",
            r#"{"id":"a","link_type":null}"#,
        )));

        // Not valid JSON.
        assert!(!parsed_top_level_link_type_is_internal(write_temp_json(
            &temp,
            "corrupt.json",
            "{not json at all",
        )));

        // A truncated document whose link_type *is* internal must still be rejected, so that a
        // torn write is never mistaken for a complete internal trace and pruned.
        assert!(!parsed_top_level_link_type_is_internal(write_temp_json(
            &temp,
            "truncated.json",
            r#"{"link_type":"internal:broken""#,
        )));

        // Valid JSON whose top level is not a map.
        assert!(!parsed_top_level_link_type_is_internal(write_temp_json(
            &temp,
            "array.json",
            r#"["link_type","internal:title_generation"]"#,
        )));
        assert!(!parsed_top_level_link_type_is_internal(write_temp_json(
            &temp,
            "scalar.json",
            r#""internal:title_generation""#,
        )));

        // A nested link_type must not be read as a top-level one.
        assert!(!parsed_top_level_link_type_is_internal(write_temp_json(
            &temp,
            "nested.json",
            r#"{"messages":[{"role":"user","content":"\"link_type\": \"internal:spoof\""}]}"#,
        )));

        // A missing file, and a directory, are both "not an internal trace".
        assert!(!parsed_top_level_link_type_is_internal(
            temp.path().join("does-not-exist.json")
        ));
        assert!(!parsed_top_level_link_type_is_internal(
            temp.path().to_path_buf()
        ));
    }

    #[test]
    fn top_level_link_type_stops_reading_once_a_non_internal_value_is_known() {
        struct CountingReader<R> {
            inner: R,
            bytes_read: Arc<AtomicUsize>,
        }

        impl<R: std::io::Read> std::io::Read for CountingReader<R> {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                let read = self.inner.read(buf)?;
                self.bytes_read.fetch_add(read, Ordering::Relaxed);
                Ok(read)
            }
        }

        // `link_type` comes first, followed by a megabyte of payload that the classifier has no
        // reason to parse once the answer is already known.
        let trailing = "x".repeat(1024 * 1024);
        let document = format!(r#"{{"link_type":"subagent","messages":"{trailing}"}}"#);

        let bytes_read = Arc::new(AtomicUsize::new(0));
        let reader = CountingReader {
            inner: std::io::Cursor::new(document.clone().into_bytes()),
            bytes_read: bytes_read.clone(),
        };

        assert!(!top_level_link_type_is_internal_from_reader(reader));

        let bytes_read = bytes_read.load(Ordering::Relaxed);
        assert!(
            bytes_read < 64 * 1024,
            "expected an early exit after link_type, but read {bytes_read} of {} bytes",
            document.len()
        );
    }

    #[test]
    fn top_level_link_type_reads_are_buffered() {
        struct ChunkCountingReader<R> {
            inner: R,
            reads: Arc<AtomicUsize>,
        }

        impl<R: std::io::Read> std::io::Read for ChunkCountingReader<R> {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                self.reads.fetch_add(1, Ordering::Relaxed);
                self.inner.read(buf)
            }
        }

        // An internal trace deliberately does *not* early-exit, so the whole document is consumed.
        // Unbuffered, serde_json's `IoRead` would issue roughly one `read()` call per byte.
        let padding = "y".repeat(256 * 1024);
        let document = format!(r#"{{"messages":"{padding}","link_type":"internal:test"}}"#);

        let reads = Arc::new(AtomicUsize::new(0));
        let reader = ChunkCountingReader {
            inner: std::io::Cursor::new(document.clone().into_bytes()),
            reads: reads.clone(),
        };

        assert!(top_level_link_type_is_internal_from_reader(reader));

        let reads = reads.load(Ordering::Relaxed);
        assert!(
            reads < document.len() / 100,
            "expected buffered reads, but made {reads} read() calls for {} bytes",
            document.len()
        );
    }

    #[tokio::test]
    async fn internal_trace_pruning_keeps_newest_and_preserves_displayable_files() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path();
        let displayable = dir.join("displayable.json");
        tokio::fs::write(
            &displayable,
            serde_json::json!({"id":"displayable","link_type":"subagent"}).to_string(),
        )
        .await
        .unwrap();
        for index in 0..4 {
            let path = dir.join(format!("internal-{index}.json"));
            let payload = if index == 0 {
                format!(
                    "{{\"id\":\"internal-0\",\"messages\":[{{\"role\":\"user\",\"content\":\"{}\"}}],\"link_type\":\"internal:title_generation\"}}",
                    "x".repeat((64 * 1024) + 1024)
                )
            } else {
                serde_json::json!({
                    "id": format!("internal-{index}"),
                    "messages": [{"role":"user","content":"trace"}],
                    "link_type": "internal:title_generation"
                })
                .to_string()
            };
            tokio::fs::write(&path, payload).await.unwrap();
            let modified = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(index + 1);
            std::fs::File::options()
                .write(true)
                .open(&path)
                .unwrap()
                .set_modified(modified)
                .unwrap();
        }

        let removed = prune_internal_traces_in_folder(dir, 2).await.unwrap();

        assert_eq!(removed, 2);
        assert!(displayable.exists());
        assert!(!dir.join("internal-0.json").exists());
        assert!(!dir.join("internal-1.json").exists());
        assert!(dir.join("internal-2.json").exists());
        assert!(dir.join("internal-3.json").exists());
    }

    #[tokio::test]
    async fn internal_trace_pruning_ignores_message_content_and_invalid_json() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path();
        let user_trajectory = dir.join("user.json");
        let corrupt = dir.join("corrupt.json");
        let truncated = dir.join("truncated.json");
        let old_trace = dir.join("old-trace.json");
        let new_trace = dir.join("new-trace.json");

        tokio::fs::write(
            &user_trajectory,
            serde_json::json!({
                "id": "user",
                "messages": [{"role": "user", "content": "\"link_type\": \"internal:spoof\""}]
            })
            .to_string(),
        )
        .await
        .unwrap();
        tokio::fs::write(&corrupt, "{\"link_type\":\"internal:broken\"")
            .await
            .unwrap();
        tokio::fs::write(&truncated, "{\"messages\":[")
            .await
            .unwrap();
        for (path, id, modified_secs) in
            [(&old_trace, "old-trace", 1), (&new_trace, "new-trace", 2)]
        {
            tokio::fs::write(
                path,
                serde_json::json!({
                    "id": id,
                    "link_type": "internal:title_generation",
                    "messages": []
                })
                .to_string(),
            )
            .await
            .unwrap();
            std::fs::File::options()
                .write(true)
                .open(path)
                .unwrap()
                .set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(modified_secs))
                .unwrap();
        }

        let removed = prune_internal_traces_in_folder(dir, 1).await.unwrap();

        assert_eq!(removed, 1);
        assert!(user_trajectory.exists());
        assert!(corrupt.exists());
        assert!(truncated.exists());
        assert!(!old_trace.exists());
        assert!(new_trace.exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn internal_trace_pruning_skips_symlinked_entries() {
        let temp = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let dir = temp.path();
        let outside_trace = outside.path().join("outside.json");
        let real_trace = dir.join("real.json");
        tokio::fs::write(
            &outside_trace,
            serde_json::json!({"id": "outside", "link_type": "internal:test"}).to_string(),
        )
        .await
        .unwrap();
        tokio::fs::write(
            &real_trace,
            serde_json::json!({"id": "real", "link_type": "internal:test"}).to_string(),
        )
        .await
        .unwrap();
        std::os::unix::fs::symlink(&outside_trace, dir.join("linked.json")).unwrap();

        let removed = prune_internal_traces_in_folder(dir, 0).await.unwrap();

        assert_eq!(removed, 1);
        assert!(!real_trace.exists());
        assert!(outside_trace.exists());
        assert!(dir.join("linked.json").exists());
    }

    #[test]
    fn internal_trace_prune_throttle_is_bounded_and_isolates_directories() {
        assert_prune_throttle_is_bounded_and_isolates_directories();
    }

    #[test]
    fn buddy_conversations_prune_throttle_is_bounded_and_isolates_directories() {
        assert_prune_throttle_is_bounded_and_isolates_directories();
    }

    #[test]
    fn index_entry_file_names_accept_one_folder_level_only() {
        assert!(trajectory_index::index_entry_file_name_is_valid("a.json"));
        assert!(trajectory_index::index_entry_file_name_is_valid(
            "root/a.json"
        ));
        assert!(!trajectory_index::index_entry_file_name_is_valid(
            "a/b/c.json"
        ));
        assert!(!trajectory_index::index_entry_file_name_is_valid(
            "../a.json"
        ));
        assert!(!trajectory_index::index_entry_file_name_is_valid(
            "root/index.json"
        ));
        assert!(!trajectory_index::index_entry_file_name_is_valid("a.txt"));
        assert!(!trajectory_index::index_entry_file_name_is_valid(
            "C:/evil.json"
        ));
        assert!(!trajectory_index::index_entry_file_name_is_valid(
            "/etc/evil.json"
        ));
    }

