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
    async fn trajectory_list_rebuilds_corrupt_cached_index_and_skips_index_json() {
        let dir = tempfile::tempdir().unwrap();
        let (_gcx, app) = make_app_with_workspace(dir.path()).await;
        let root = dir.path().join(".refact").join("trajectories");
        tokio::fs::create_dir_all(&root).await.unwrap();
        write_trajectory_file(
            &root.join("indexed-chat.json"),
            "indexed-chat",
            "Indexed",
            "2026-01-01T00:00:00Z",
        )
        .await;

        assert!(list_trajectories_page(app.clone(), 10, None, false)
            .await
            .unwrap()
            .items
            .iter()
            .any(|item| item.id == "indexed-chat"));

        tokio::fs::write(root.join("index.json"), "not-json")
            .await
            .unwrap();

        let page = list_trajectories_page(app, 10, None, false).await.unwrap();

        assert!(page.items.iter().any(|item| item.id == "indexed-chat"));
        let index = crate::chat::trajectory_index::read_trajectory_index(&root)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(index.entries.len(), 1);
        assert_eq!(index.entries[0].file_name, "indexed-chat.json");
    }

    #[tokio::test]
    async fn trajectory_id_mismatch_open_creates_fresh_session_without_file_state() {
        let dir = tempfile::tempdir().unwrap();
        let (_gcx, app) = make_app_with_workspace(dir.path()).await;
        let path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join("safe.json");
        write_trajectory_file(&path, "other-chat", "Mismatched", "2024-01-01T00:00:00Z").await;

        let session_arc = crate::chat::get_or_create_session_with_trajectory(
            app.clone(),
            &app.chat.sessions,
            "safe",
        )
        .await;
        let session = session_arc.lock().await;

        assert_eq!(session.chat_id, "safe");
        assert_eq!(session.thread.id, "safe");
        assert_ne!(session.thread.title, "Mismatched");
        assert!(session.messages.is_empty());
    }

    #[tokio::test]
    async fn pristine_session_is_born_clean_and_heals_from_late_trajectory() {
        let dir = tempfile::tempdir().unwrap();
        let (_gcx, app) = make_app_with_workspace(dir.path()).await;
        let chat_id = "subchat-late-trajectory";

        let session_arc = crate::chat::get_or_create_session_with_trajectory(
            app.clone(),
            &app.chat.sessions,
            chat_id,
        )
        .await;
        let mut chat_rx = {
            let session = session_arc.lock().await;
            assert!(session.messages.is_empty());
            assert!(!session.trajectory_dirty);
            session.subscribe()
        };

        let path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        write_trajectory_file_with_user_message(&path, chat_id, "Child Chat", "child work").await;

        let reopened = crate::chat::get_or_create_session_with_trajectory(
            app.clone(),
            &app.chat.sessions,
            chat_id,
        )
        .await;
        assert!(Arc::ptr_eq(&session_arc, &reopened));
        {
            let session = reopened.lock().await;
            assert_eq!(session.messages.len(), 1);
            assert_eq!(session.thread.title, "Child Chat");
        }

        let json = chat_rx.recv().await.unwrap();
        let envelope: EventEnvelope = serde_json::from_str(&json).unwrap();
        match envelope.event {
            ChatEvent::Snapshot { messages, .. } => {
                assert_eq!(messages.len(), 1);
            }
            other => panic!("expected Snapshot, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn pristine_session_save_does_not_overwrite_late_trajectory() {
        let dir = tempfile::tempdir().unwrap();
        let (_gcx, app) = make_app_with_workspace(dir.path()).await;
        let chat_id = "subchat-cleanup-no-clobber";

        let session_arc = crate::chat::get_or_create_session_with_trajectory(
            app.clone(),
            &app.chat.sessions,
            chat_id,
        )
        .await;

        let path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        write_trajectory_file_with_user_message(&path, chat_id, "Child Chat", "child work").await;

        let saved = try_save_trajectory(app.clone(), session_arc.clone())
            .await
            .unwrap();
        assert!(saved);

        let raw: serde_json::Value =
            serde_json::from_str(&tokio::fs::read_to_string(&path).await.unwrap()).unwrap();
        assert_eq!(raw["messages"].as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn stale_refresh_skips_busy_dirty_or_drafting_sessions() {
        let dir = tempfile::tempdir().unwrap();
        let (_gcx, app) = make_app_with_workspace(dir.path()).await;
        let chat_id = "subchat-refresh-guards";

        let path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        write_trajectory_file_with_user_message(&path, chat_id, "Child Chat", "child work").await;

        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        app.chat
            .sessions
            .write()
            .await
            .insert(chat_id.to_string(), session_arc.clone());

        {
            let mut session = session_arc.lock().await;
            session.trajectory_dirty = true;
        }
        assert!(!refresh_session_from_trajectory_if_stale(app.clone(), chat_id, false).await);

        {
            let mut session = session_arc.lock().await;
            session.trajectory_dirty = false;
            session.runtime.state = SessionState::Generating;
        }
        assert!(!refresh_session_from_trajectory_if_stale(app.clone(), chat_id, false).await);

        {
            let mut session = session_arc.lock().await;
            session.runtime.state = SessionState::Idle;
            session.draft_message = Some(ChatMessage::new("user".to_string(), "draft".to_string()));
        }
        assert!(!refresh_session_from_trajectory_if_stale(app.clone(), chat_id, false).await);

        {
            let mut session = session_arc.lock().await;
            session.draft_message = None;
        }
        assert!(refresh_session_from_trajectory_if_stale(app.clone(), chat_id, false).await);
        let session = session_arc.lock().await;
        assert_eq!(session.messages.len(), 1);
    }

    #[tokio::test]
    async fn completed_subchat_refresh_replaces_seed_messages() {
        let dir = tempfile::tempdir().unwrap();
        let (_gcx, app) = make_app_with_workspace(dir.path()).await;
        let chat_id = "subchat-refresh-final";

        let path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        write_trajectory_file_with_user_message(&path, chat_id, "Child Chat", "seed prompt").await;

        let session_arc = crate::chat::get_or_create_session_with_trajectory(
            app.clone(),
            &app.chat.sessions,
            chat_id,
        )
        .await;
        {
            let session = session_arc.lock().await;
            assert_eq!(session.messages.len(), 1);
            assert!(!session.trajectory_dirty);
        }

        let mut trajectory = sample_trajectory(chat_id, "Child Chat", "2024-01-01T00:00:02Z");
        trajectory["messages"] = json!([
            { "role": "user", "content": "seed prompt" },
            { "role": "assistant", "content": "final answer" }
        ]);
        tokio::fs::write(&path, serde_json::to_string(&trajectory).unwrap())
            .await
            .unwrap();

        assert!(!refresh_session_from_trajectory_if_stale(app.clone(), chat_id, false).await);
        assert!(refresh_session_from_trajectory_if_stale(app.clone(), chat_id, true).await);

        let session = session_arc.lock().await;
        assert_eq!(session.messages.len(), 2);
    }

    #[tokio::test]
    async fn buddy_child_refresh_replaces_seed_after_completion() {
        let dir = tempfile::tempdir().unwrap();
        let (_gcx, app) = make_app_with_workspace(dir.path()).await;
        let chat_id = "buddy-refresh-final";

        let buddy_path = dir
            .path()
            .join(".refact")
            .join("buddy")
            .join("chats")
            .join("conversations")
            .join(format!("{chat_id}.json"));
        write_buddy_conversation_file(&buddy_path, chat_id, "Buddy Child").await;

        let session_arc = crate::chat::get_or_create_session_with_trajectory(
            app.clone(),
            &app.chat.sessions,
            chat_id,
        )
        .await;
        {
            let session = session_arc.lock().await;
            assert_eq!(session.messages.len(), 1);
            assert!(session.thread.buddy_meta.is_some());
        }

        tokio::fs::write(
            &buddy_path,
            serde_json::to_string(&json!({
                "id": chat_id,
                "chat_id": chat_id,
                "title": "Buddy Child",
                "model": "model",
                "mode": "buddy",
                "tool_use": "agent",
                "created_at": "2024-01-01T00:00:00Z",
                "updated_at": "2024-01-01T00:00:02Z",
                "include_project_info": true,
                "checkpoints_enabled": false,
                "messages": [
                    {"role":"user","content":"hello buddy"},
                    {"role":"assistant","content":"buddy final answer"}
                ],
                "buddy_meta": {"is_buddy_chat": true, "buddy_chat_kind": "investigation"}
            }))
            .unwrap(),
        )
        .await
        .unwrap();

        assert!(refresh_session_from_trajectory_if_stale(app.clone(), chat_id, true).await);

        let session = session_arc.lock().await;
        assert_eq!(session.messages.len(), 2);
        assert!(session.thread.buddy_meta.is_some());
    }

    #[tokio::test]
    async fn reopening_child_chat_replaces_stale_seed_messages() {
        let dir = tempfile::tempdir().unwrap();
        let (_gcx, app) = make_app_with_workspace(dir.path()).await;
        let chat_id = "subchat-reopen-refresh";

        let path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        let mut trajectory = sample_trajectory(chat_id, "Child Chat", "2024-01-01T00:00:01Z");
        trajectory["parent_id"] = json!("parent-chat");
        trajectory["link_type"] = json!("subagent");
        trajectory["messages"] = json!([{ "role": "user", "content": "seed prompt" }]);
        tokio::fs::create_dir_all(path.parent().unwrap())
            .await
            .unwrap();
        tokio::fs::write(&path, serde_json::to_string(&trajectory).unwrap())
            .await
            .unwrap();

        let session_arc = crate::chat::get_or_create_session_with_trajectory(
            app.clone(),
            &app.chat.sessions,
            chat_id,
        )
        .await;
        {
            let session = session_arc.lock().await;
            assert_eq!(session.messages.len(), 1);
            assert_eq!(session.thread.parent_id.as_deref(), Some("parent-chat"));
        }

        trajectory["messages"] = json!([
            { "role": "user", "content": "seed prompt" },
            { "role": "assistant", "content": "final answer" }
        ]);
        tokio::fs::write(&path, serde_json::to_string(&trajectory).unwrap())
            .await
            .unwrap();

        let reopened = crate::chat::get_or_create_session_with_trajectory(
            app.clone(),
            &app.chat.sessions,
            chat_id,
        )
        .await;
        assert!(Arc::ptr_eq(&session_arc, &reopened));
        let session = reopened.lock().await;
        assert_eq!(session.messages.len(), 2);
    }

    #[tokio::test]
    async fn trajectory_id_missing_is_not_loaded() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join("missing-id.json");
        let mut payload = sample_trajectory("missing-id", "Missing Id", "2024-01-01T00:00:00Z");
        payload.as_object_mut().unwrap().remove("id");
        tokio::fs::create_dir_all(path.parent().unwrap())
            .await
            .unwrap();
        tokio::fs::write(&path, serde_json::to_string(&payload).unwrap())
            .await
            .unwrap();

        assert!(load_trajectory_for_chat(gcx, "missing-id").await.is_none());
    }

    #[tokio::test]
    async fn trajectory_id_mismatch_is_not_listed() {
        let dir = tempfile::tempdir().unwrap();
        let (_gcx, app) = make_app_with_workspace(dir.path()).await;
        let root = dir.path().join(".refact").join("trajectories");
        write_trajectory_file(
            &root.join("safe.json"),
            "other-chat",
            "Mismatched",
            "2024-01-01T00:00:00Z",
        )
        .await;
        write_trajectory_file(
            &root.join("valid-chat.json"),
            "valid-chat",
            "Valid",
            "2024-01-01T00:00:01Z",
        )
        .await;

        let listed = list_all_trajectories_meta(app, false).await.unwrap();
        let ids: std::collections::HashSet<_> =
            listed.iter().map(|item| item.id.as_str()).collect();

        assert!(ids.contains("valid-chat"));
        assert!(!ids.contains("other-chat"));
    }

    #[test]
    fn test_validate_trajectory_id_rejects_forward_slash() {
        assert!(validate_trajectory_id("a/b").is_err());
        assert!(validate_trajectory_id("/absolute").is_err());
    }

    #[test]
    fn test_validate_trajectory_id_rejects_backslash() {
        assert!(validate_trajectory_id("a\\b").is_err());
        assert!(validate_trajectory_id("\\windows\\path").is_err());
    }

    #[test]
    fn test_validate_trajectory_id_rejects_null_byte() {
        assert!(validate_trajectory_id("test\0id").is_err());
    }

    #[test]
    fn test_validate_trajectory_id_accepts_valid() {
        assert!(validate_trajectory_id("abc-123").is_ok());
        assert!(validate_trajectory_id("chat_456").is_ok());
        assert!(validate_trajectory_id("550e8400-e29b-41d4-a716-446655440000").is_ok());
        assert!(validate_trajectory_id("planner-task-1").is_ok());
        assert!(validate_trajectory_id("A1b2C3").is_ok());
    }

    #[test]
    fn test_validate_trajectory_id_rejects_empty() {
        assert!(validate_trajectory_id("").is_err());
    }

    #[test]
    fn test_validate_trajectory_id_rejects_too_long() {
        let long_id = "a".repeat(129);
        assert!(validate_trajectory_id(&long_id).is_err());
        let max_id = "a".repeat(128);
        assert!(validate_trajectory_id(&max_id).is_ok());
    }

    #[test]
    fn test_validate_trajectory_id_rejects_invalid_chars() {
        assert!(validate_trajectory_id("has space").is_err());
        assert!(validate_trajectory_id("has.dot").is_err());
        assert!(validate_trajectory_id("has@symbol").is_err());
        assert!(validate_trajectory_id("has#hash").is_err());
    }

    #[test]
    fn test_is_placeholder_title_new_chat() {
        assert!(is_placeholder_title("New Chat"));
        assert!(is_placeholder_title("new chat"));
        assert!(is_placeholder_title("NEW CHAT"));
        assert!(is_placeholder_title("  New Chat  "));
    }

    #[test]
    fn test_is_placeholder_title_untitled() {
        assert!(is_placeholder_title("untitled"));
        assert!(is_placeholder_title("Untitled"));
        assert!(is_placeholder_title("UNTITLED"));
    }

    #[test]
    fn test_is_placeholder_title_empty() {
        assert!(is_placeholder_title(""));
        assert!(is_placeholder_title("   "));
    }

    #[test]
    fn test_is_placeholder_title_real_titles() {
        assert!(!is_placeholder_title("Fix authentication bug"));
        assert!(!is_placeholder_title("Refactor database module"));
        assert!(!is_placeholder_title("New feature implementation"));
    }

    #[test]
    fn test_clean_generated_title_strips_quotes() {
        assert_eq!(clean_generated_title("\"Hello World\""), "Hello World");
        assert_eq!(clean_generated_title("'Hello World'"), "Hello World");
        assert_eq!(clean_generated_title("`Hello World`"), "Hello World");
    }

    #[test]
    fn test_clean_generated_title_strips_asterisks() {
        assert_eq!(clean_generated_title("*Bold Title*"), "Bold Title");
        assert_eq!(clean_generated_title("**Strong Title**"), "Strong Title");
    }

    #[test]
    fn test_clean_generated_title_collapses_whitespace() {
        assert_eq!(clean_generated_title("Hello   World"), "Hello World");
        assert_eq!(
            clean_generated_title("  Multiple   Spaces  "),
            "Multiple Spaces"
        );
    }

    #[test]
    fn test_clean_generated_title_removes_newlines() {
        assert_eq!(clean_generated_title("Hello\nWorld"), "Hello World");
        assert_eq!(
            clean_generated_title("Line1\nLine2\nLine3"),
            "Line1 Line2 Line3"
        );
    }

    #[test]
    fn test_clean_generated_title_truncates_long() {
        let long_title = "A".repeat(100);
        let result = clean_generated_title(&long_title);
        assert!(result.len() <= 60);
        assert!(result.ends_with("..."));
    }

    #[test]
    fn test_clean_generated_title_preserves_short() {
        let short_title = "Short Title";
        let result = clean_generated_title(short_title);
        assert_eq!(result, "Short Title");
        assert!(!result.ends_with("..."));
    }

    #[test]
    fn test_trajectory_meta_title_truncates_oversized_stored_title() {
        let long_title = "A".repeat(1024 * 1024);
        let result = trajectory_meta_title(&long_title);
        assert_eq!(result.chars().count(), TRAJECTORY_META_TITLE_MAX_CHARS);
        assert!(result.ends_with("..."));
    }

    #[test]
    fn test_trajectory_meta_uses_bounded_title() {
        let data = TrajectoryData {
            id: "big-title-chat".to_string(),
            title: "B".repeat(1024 * 1024),
            created_at: "2024-01-01T00:00:00Z".to_string(),
            updated_at: "2024-01-01T00:00:00Z".to_string(),
            model: "model".to_string(),
            mode: "agent".to_string(),
            tool_use: "agent".to_string(),
            messages: Vec::new(),
            extra: serde_json::Map::new(),
        };
        let meta = trajectory_data_to_meta(&data);
        assert_eq!(meta.title.chars().count(), TRAJECTORY_META_TITLE_MAX_CHARS);
    }

    #[test]
    fn test_cursor_precedes_item_matches_descending_order() {
        assert!(cursor_precedes_item(
            ("2024-01-01T00:00:00Z", "a"),
            ("2024-01-02T00:00:00Z", "b"),
        ));
        assert!(cursor_precedes_item(
            ("2024-01-01T00:00:00Z", "a"),
            ("2024-01-01T00:00:00Z", "b"),
        ));
        assert!(!cursor_precedes_item(
            ("2024-01-03T00:00:00Z", "a"),
            ("2024-01-02T00:00:00Z", "b"),
        ));
    }

    #[test]
    fn test_extract_first_user_message_string_content() {
        let messages = vec![
            json!({"role": "system", "content": "You are helpful"}),
            json!({"role": "user", "content": "Hello there"}),
        ];
        let result = extract_first_user_message(&messages);
        assert_eq!(result, Some("Hello there".to_string()));
    }

    #[test]
    fn test_extract_first_user_message_array_content_text() {
        let messages =
            vec![json!({"role": "user", "content": [{"type": "text", "text": "Array text"}]})];
        let result = extract_first_user_message(&messages);
        assert_eq!(result, Some("Array text".to_string()));
    }

    #[test]
    fn test_extract_first_user_message_array_content_m_content() {
        let messages = vec![
            json!({"role": "user", "content": [{"m_type": "text", "m_content": "M content"}]}),
        ];
        let result = extract_first_user_message(&messages);
        assert_eq!(result, Some("M content".to_string()));
    }

    #[test]
    fn test_extract_first_user_message_skips_empty() {
        let messages = vec![
            json!({"role": "user", "content": "   "}),
            json!({"role": "user", "content": "Second message"}),
        ];
        let result = extract_first_user_message(&messages);
        assert_eq!(result, Some("Second message".to_string()));
    }

    #[test]
    fn test_extract_first_user_message_truncates() {
        let long_message = "A".repeat(300);
        let messages = vec![json!({"role": "user", "content": long_message})];
        let result = extract_first_user_message(&messages);
        assert!(result.is_some());
        assert!(result.unwrap().len() <= 200);
    }

    #[test]
    fn test_extract_first_user_message_no_user() {
        let messages = vec![
            json!({"role": "system", "content": "System prompt"}),
            json!({"role": "assistant", "content": "Hello"}),
        ];
        let result = extract_first_user_message(&messages);
        assert!(result.is_none());
    }

    #[test]
    fn test_build_title_generation_context_skips_tool_messages() {
        let messages = vec![
            json!({"role": "user", "content": "User message"}),
            json!({"role": "tool", "content": "Tool result"}),
            json!({"role": "assistant", "content": "Response"}),
        ];
        let context = build_title_generation_context(&messages);
        assert!(context.contains("User message"));
        assert!(context.contains("Response"));
        assert!(!context.contains("Tool result"));
    }

    #[test]
    fn test_build_title_generation_context_skips_context_file() {
        let messages = vec![
            json!({"role": "user", "content": "Question"}),
            json!({"role": "context_file", "content": "File contents"}),
        ];
        let context = build_title_generation_context(&messages);
        assert!(context.contains("Question"));
        assert!(!context.contains("File contents"));
    }

    #[test]
    fn build_title_generation_context_skips_hidden_plan_and_event_roles() {
        let messages = vec![
            json!({"role": "user", "content": "Visible user request"}),
            json!({
                "role": "plan",
                "content": "Hidden base plan must not title the chat",
                "extra": {"plan": {"mode": "agent", "version": 1}}
            }),
            json!({
                "role": "event",
                "content": "Hidden plan delta must not title the chat",
                "extra": {
                    "event": {
                        "subkind": "plan_delta",
                        "source": "tool.update_plan",
                        "payload": {"seq": 1}
                    }
                }
            }),
            json!({"role": "assistant", "content": "Visible assistant response"}),
        ];

        let context = build_title_generation_context(&messages);

        assert!(context.contains("Visible user request"));
        assert!(context.contains("Visible assistant response"));
        assert!(!context.contains("Hidden base plan"));
        assert!(!context.contains("Hidden plan delta"));
    }

    #[test]
    fn build_title_generation_context_skips_ui_only_error() {
        let messages = vec![
            json!({"role": "error", "content": "context_length_exceeded", "_ui_only": true}),
            json!({"role": "user", "content": "Implement title filtering"}),
        ];
        let context = build_title_generation_context(&messages);
        assert!(context.contains("Implement title filtering"));
        assert!(!context.contains("context_length_exceeded"));
    }

    #[test]
    fn build_title_generation_context_skips_ui_only_reactive_compaction_report() {
        let messages = vec![
            json!({
                "role": "summarization",
                "content": "Legacy diagnostic report",
                "summarization_tier": "legacy_reactive",
                "_ui_only": true
            }),
            json!({"role": "user", "content": "Fix sanitizers"}),
        ];
        let context = build_title_generation_context(&messages);
        assert!(context.contains("Fix sanitizers"));
        assert!(!context.contains("Legacy diagnostic report"));
    }

    #[test]
    fn build_title_generation_context_skips_extra_ui_only_reactive_report() {
        let messages = vec![
            json!({
                "role": "summarization",
                "content": "Legacy diagnostic report",
                "summarization_tier": "legacy_reactive",
                "extra": {"_ui_only": true}
            }),
            json!({"role": "user", "content": "Fix sanitizers"}),
        ];
        let context = build_title_generation_context(&messages);
        assert!(context.contains("Fix sanitizers"));
        assert!(!context.contains("Legacy diagnostic report"));
    }

    #[test]
    fn test_build_title_generation_context_limits_messages() {
        let messages: Vec<_> = (0..10)
            .map(|i| json!({"role": "user", "content": format!("Message {}", i)}))
            .collect();
        let context = build_title_generation_context(&messages);
        assert!(context.contains("Message 0"));
        assert!(context.contains("Message 5"));
        assert!(!context.contains("Message 9"));
    }

    #[test]
    fn test_build_title_generation_context_truncates_long_messages() {
        let long_content = "A".repeat(1000);
        let messages = vec![json!({"role": "user", "content": long_content})];
        let context = build_title_generation_context(&messages);
        assert!(context.len() < 600);
    }

    #[test]
    fn test_fix_tool_call_indexes_sets_missing() {
        use crate::call_validation::{ChatToolCall, ChatToolFunction};
        let mut messages = vec![ChatMessage {
            role: "assistant".to_string(),
            tool_calls: Some(vec![
                ChatToolCall {
                    id: "call_1".to_string(),
                    index: None,
                    function: ChatToolFunction {
                        name: "test".to_string(),
                        arguments: "{}".to_string(),
                    },
                    tool_type: "function".to_string(),
                    extra_content: None,
                    started_at_ms: None,
                    completed_at_ms: None,
                },
                ChatToolCall {
                    id: "call_2".to_string(),
                    index: None,
                    function: ChatToolFunction {
                        name: "test2".to_string(),
                        arguments: "{}".to_string(),
                    },
                    tool_type: "function".to_string(),
                    extra_content: None,
                    started_at_ms: None,
                    completed_at_ms: None,
                },
            ]),
            ..Default::default()
        }];
        fix_tool_call_indexes(&mut messages);
        let tool_calls = messages[0].tool_calls.as_ref().unwrap();
        assert_eq!(tool_calls[0].index, Some(0));
        assert_eq!(tool_calls[1].index, Some(1));
    }

    #[test]
    fn test_fix_tool_call_indexes_preserves_existing() {
        use crate::call_validation::{ChatToolCall, ChatToolFunction};
        let mut messages = vec![ChatMessage {
            role: "assistant".to_string(),
            tool_calls: Some(vec![ChatToolCall {
                id: "call_1".to_string(),
                index: Some(5),
                function: ChatToolFunction {
                    name: "test".to_string(),
                    arguments: "{}".to_string(),
                },
                tool_type: "function".to_string(),
                extra_content: None,
                started_at_ms: None,
                completed_at_ms: None,
            }]),
            ..Default::default()
        }];
        fix_tool_call_indexes(&mut messages);
        let tool_calls = messages[0].tool_calls.as_ref().unwrap();
        assert_eq!(tool_calls[0].index, Some(5));
    }

    #[test]
    fn test_calculate_token_totals_from_messages_with_usage() {
        let messages = vec![
            json!({
                "role": "assistant",
                "content": "Hello",
                "usage": {
                    "prompt_tokens": 10,
                    "completion_tokens": 5,
                    "total_tokens": 15,
                    "cache_read_input_tokens": 3,
                    "cache_creation_input_tokens": 2,
                    "metering_usd": {
                        "prompt_usd": 0.001,
                        "generated_usd": 0.002,
                        "total_usd": 0.003
                    }
                }
            }),
            json!({
                "role": "assistant",
                "content": "World",
                "usage": {
                    "prompt_tokens": 20,
                    "completion_tokens": 10,
                    "total_tokens": 30,
                    "metering_usd": {
                        "prompt_usd": 0.002,
                        "generated_usd": 0.004,
                        "total_usd": 0.006
                    }
                }
            }),
        ];
        let totals = calculate_token_totals_from_messages(&messages);
        assert_eq!(totals.prompt_tokens, 30);
        assert_eq!(totals.completion_tokens, 15);
        assert_eq!(totals.total_tokens, 45);
        assert_eq!(totals.cache_read_tokens, 3);
        assert_eq!(totals.cache_creation_tokens, 2);
        let cost = totals.cost_usd.unwrap();
        assert!((cost - 0.009).abs() < 1e-9);
    }

    #[test]
    fn test_calculate_token_totals_from_messages_no_usage() {
        let messages = vec![
            json!({"role": "user", "content": "Hello"}),
            json!({"role": "assistant", "content": "Hi"}),
        ];
        let totals = calculate_token_totals_from_messages(&messages);
        assert_eq!(totals.prompt_tokens, 0);
        assert_eq!(totals.completion_tokens, 0);
        assert_eq!(totals.total_tokens, 0);
        assert_eq!(totals.cache_read_tokens, 0);
        assert_eq!(totals.cache_creation_tokens, 0);
        assert!(totals.cost_usd.is_none());
    }

    #[test]
    fn test_calculate_token_totals_from_messages_null_usage() {
        let messages = vec![json!({"role": "assistant", "content": "Hi", "usage": null})];
        let totals = calculate_token_totals_from_messages(&messages);
        assert_eq!(totals.prompt_tokens, 0);
        assert!(totals.cost_usd.is_none());
    }

    #[test]
    fn test_calculate_token_totals_from_messages_alias_keys() {
        let messages = vec![json!({
            "role": "assistant",
            "content": "Hi",
            "usage": {
                "prompt_tokens": 5,
                "completion_tokens": 3,
                "total_tokens": 8,
                "cache_read_tokens": 7,
                "cache_creation_tokens": 4,
            }
        })];
        let totals = calculate_token_totals_from_messages(&messages);
        assert_eq!(totals.cache_read_tokens, 7);
        assert_eq!(totals.cache_creation_tokens, 4);
    }

    #[test]
    fn test_calculate_token_totals_from_chat_messages_with_usage() {
        use crate::call_validation::{ChatUsage, MeteringUsd};
        let messages = vec![
            ChatMessage {
                role: "assistant".to_string(),
                usage: Some(ChatUsage {
                    prompt_tokens: 100,
                    completion_tokens: 50,
                    total_tokens: 150,
                    cache_read_tokens: Some(10),
                    cache_creation_tokens: Some(5),
                    metering_usd: Some(MeteringUsd {
                        prompt_usd: 0.01,
                        generated_usd: 0.02,
                        cache_read_usd: None,
                        cache_creation_usd: None,
                        total_usd: 0.03,
                    }),
                }),
                ..Default::default()
            },
            ChatMessage {
                role: "assistant".to_string(),
                usage: Some(ChatUsage {
                    prompt_tokens: 200,
                    completion_tokens: 100,
                    total_tokens: 300,
                    cache_read_tokens: None,
                    cache_creation_tokens: None,
                    metering_usd: None,
                }),
                ..Default::default()
            },
        ];
        let totals = calculate_token_totals_from_chat_messages(&messages);
        assert_eq!(totals.prompt_tokens, 300);
        assert_eq!(totals.completion_tokens, 150);
        assert_eq!(totals.total_tokens, 450);
        assert_eq!(totals.cache_read_tokens, 10);
        assert_eq!(totals.cache_creation_tokens, 5);
        let cost = totals.cost_usd.unwrap();
        assert!((cost - 0.03).abs() < 1e-9);
    }

    #[test]
    fn test_calculate_token_totals_from_chat_messages_no_usage() {
        let messages = vec![ChatMessage {
            role: "user".to_string(),
            ..Default::default()
        }];
        let totals = calculate_token_totals_from_chat_messages(&messages);
        assert_eq!(totals.prompt_tokens, 0);
        assert!(totals.cost_usd.is_none());
    }

    #[tokio::test]
    async fn external_remove_clears_checking_compression_state_and_snapshot() {
        assert_external_remove_clears_active_compression(CompressionPhase::Checking).await;
    }

