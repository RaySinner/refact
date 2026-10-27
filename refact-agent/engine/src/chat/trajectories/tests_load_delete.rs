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
    async fn trajectory_path_helpers_and_save_reject_malformed_chat_id() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        tokio::fs::create_dir_all(dir.path().join(".refact").join("trajectories"))
            .await
            .unwrap();
        tokio::fs::create_dir_all(
            dir.path()
                .join(".refact")
                .join("buddy")
                .join("chats")
                .join("conversations"),
        )
        .await
        .unwrap();
        tokio::fs::write(dir.path().join(".refact").join("bad.json"), "{}")
            .await
            .unwrap();
        tokio::fs::write(
            dir.path()
                .join(".refact")
                .join("buddy")
                .join("chats")
                .join("bad.json"),
            "{}",
        )
        .await
        .unwrap();

        assert!(find_trajectory_path(gcx.clone(), "../bad").await.is_none());
        assert!(find_trajectory_or_buddy_path(gcx.clone(), "../bad")
            .await
            .is_none());

        let err = save_trajectory_snapshot(
            gcx,
            test_snapshot(
                "../save-bad",
                "Bad",
                vec![ChatMessage::new("user".to_string(), "hello".to_string())],
            ),
        )
        .await
        .unwrap_err();
        assert_eq!(err, "Invalid trajectory id");
        assert!(
            !tokio::fs::try_exists(dir.path().join(".refact").join("save-bad.json"))
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn generic_trajectory_delete_does_not_delete_buddy_conversation_file() {
        let dir = tempfile::tempdir().unwrap();
        let (_gcx, app) = make_app_with_workspace(dir.path()).await;
        let chat_id = "buddy-delete-isolated";
        let buddy_path = dir
            .path()
            .join(".refact")
            .join("buddy")
            .join("chats")
            .join("conversations")
            .join(format!("{}.json", chat_id));
        write_buddy_conversation_file(&buddy_path, chat_id, "Keep Buddy").await;

        let err = handle_v1_trajectories_delete(State(app), AxumPath(chat_id.to_string()))
            .await
            .unwrap_err();

        assert_eq!(err.status_code, StatusCode::NOT_FOUND);
        assert!(tokio::fs::try_exists(&buddy_path).await.unwrap());
    }

    #[tokio::test]
    async fn generic_trajectory_delete_does_not_emit_buddy_fallback_update() {
        let dir = tempfile::tempdir().unwrap();
        let (_gcx, app) = make_app_with_workspace(dir.path()).await;
        let mut rx = app.chat.trajectory_events_tx.subscribe();
        let chat_id = "buddy-delete-no-fallback-update";
        let normal_path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        let buddy_path = dir
            .path()
            .join(".refact")
            .join("buddy")
            .join("chats")
            .join("conversations")
            .join(format!("{chat_id}.json"));
        write_trajectory_file(
            &normal_path,
            chat_id,
            "Delete Normal",
            "2024-01-01T00:00:01Z",
        )
        .await;
        write_buddy_conversation_file(&buddy_path, chat_id, "Keep Buddy").await;

        handle_v1_trajectories_delete(State(app), AxumPath(chat_id.to_string()))
            .await
            .unwrap();

        let event = wait_for_trajectory_event(&mut rx, chat_id).await;
        assert_eq!(event.event_type, "deleted");
        assert_eq!(event.title, None);
        assert!(!tokio::fs::try_exists(&normal_path).await.unwrap());
        assert!(tokio::fs::try_exists(&buddy_path).await.unwrap());
    }

    #[tokio::test]
    async fn explicit_buddy_inclusive_lookup_loads_buddy_conversation_file() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let chat_id = "buddy-read-inclusive";
        let buddy_path = dir
            .path()
            .join(".refact")
            .join("buddy")
            .join("chats")
            .join("conversations")
            .join(format!("{}.json", chat_id));
        write_buddy_conversation_file(&buddy_path, chat_id, "Readable Buddy").await;

        assert!(find_trajectory_path(gcx.clone(), chat_id).await.is_none());
        assert_same_optional_path(
            find_trajectory_or_buddy_path(gcx.clone(), chat_id).await,
            &buddy_path,
        );
        let loaded = load_trajectory_for_chat(gcx, chat_id).await.unwrap();

        assert_eq!(loaded.thread.title, "Readable Buddy");
        assert_eq!(
            loaded.thread.buddy_meta.unwrap().buddy_chat_kind,
            "investigation"
        );
        assert_eq!(loaded.messages.len(), 1);
    }

    #[tokio::test]
    async fn empty_new_chat_pending_cap_survives_real_save_load_and_model_late() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let chat_id = "empty-pending-cap";
        let mut snapshot = test_snapshot(chat_id, "", Vec::new());
        snapshot.auto_compression_cap_pending = true;
        save_trajectory_snapshot(gcx.clone(), snapshot)
            .await
            .unwrap();
        let mut loaded = load_trajectory_for_chat(gcx.clone(), chat_id)
            .await
            .unwrap();
        assert!(loaded.messages.is_empty());
        assert!(loaded.thread.auto_compression_cap_pending);
        assert!(loaded.thread.resolve_pending_compression_cap(Some(1000)));
        assert_eq!(loaded.thread.auto_compression_cap, Some(900));
        let path = find_trajectory_path(gcx.clone(), chat_id).await.unwrap();
        let mut value: serde_json::Value =
            serde_json::from_slice(&tokio::fs::read(&path).await.unwrap()).unwrap();
        value
            .as_object_mut()
            .unwrap()
            .remove("auto_compression_cap_pending");
        tokio::fs::write(path, serde_json::to_vec(&value).unwrap())
            .await
            .unwrap();
        let mut legacy = load_trajectory_for_chat(gcx, chat_id).await.unwrap();
        assert!(legacy.messages.is_empty());
        assert!(!legacy.thread.resolve_pending_compression_cap(Some(1000)));
        assert_eq!(legacy.thread.auto_compression_cap, None);
    }

    #[tokio::test]
    async fn auto_compression_cap_survives_real_trajectory_save_and_load() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let chat_id = "auto-compression-cap-roundtrip";
        let mut snapshot = test_snapshot(
            chat_id,
            "Auto compression cap",
            vec![ChatMessage::new("user".to_string(), "hello".to_string())],
        );
        snapshot.auto_compression_cap = Some(96_000);

        save_trajectory_snapshot(gcx.clone(), snapshot)
            .await
            .unwrap();
        let loaded = load_trajectory_for_chat(gcx.clone(), chat_id)
            .await
            .unwrap();
        assert_eq!(loaded.thread.auto_compression_cap, Some(96_000));

        let path = find_trajectory_path(gcx.clone(), chat_id).await.unwrap();
        let mut value: serde_json::Value =
            serde_json::from_slice(&tokio::fs::read(&path).await.unwrap()).unwrap();
        value
            .as_object_mut()
            .unwrap()
            .remove("auto_compression_cap");
        value
            .as_object_mut()
            .unwrap()
            .remove("auto_compression_cap_pending");
        tokio::fs::write(&path, serde_json::to_vec(&value).unwrap())
            .await
            .unwrap();
        let mut legacy = load_trajectory_for_chat(gcx, chat_id).await.unwrap();
        assert_eq!(legacy.thread.auto_compression_cap, None);
        assert!(!legacy.thread.auto_compression_cap_pending);
        assert!(!legacy.thread.resolve_pending_compression_cap(Some(128_000)));
        assert_eq!(legacy.thread.auto_compression_cap, None);
    }

    #[tokio::test]
    async fn pending_new_chat_cap_survives_empty_first_save_and_model_late_load() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let chat_id = "pending-new-chat-cap";
        let mut snapshot = test_snapshot(chat_id, "", Vec::new());
        snapshot.auto_compression_cap_pending = true;
        snapshot.context_tokens_cap = Some(100_000);
        save_trajectory_snapshot(gcx.clone(), snapshot)
            .await
            .unwrap();
        let mut loaded = load_trajectory_for_chat(gcx.clone(), chat_id)
            .await
            .unwrap();
        assert!(loaded.messages.is_empty());
        assert!(loaded.thread.auto_compression_cap_pending);
        assert!(!loaded.thread.resolve_pending_compression_cap(None));
        assert_eq!(loaded.thread.auto_compression_cap, None);
        assert!(loaded.thread.resolve_pending_compression_cap(Some(80_000)));
        assert_eq!(loaded.thread.auto_compression_cap, Some(72_000));
        save_trajectory_as(gcx.clone(), &loaded.thread, &loaded.messages).await;
        let restored = load_trajectory_for_chat(gcx, chat_id).await.unwrap();
        assert_eq!(restored.thread.auto_compression_cap, Some(72_000));
        assert!(!restored.thread.auto_compression_cap_pending);
    }

    #[tokio::test]
    async fn new_empty_chat_default_provenance_survives_restart_before_model_selection() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let chat_id = "new-chat-pending-default";
        let mut snapshot = test_snapshot(chat_id, "", Vec::new());
        snapshot.auto_compression_cap_pending = true;
        snapshot.context_tokens_cap = Some(80_000);
        save_trajectory_snapshot(gcx.clone(), snapshot)
            .await
            .unwrap();
        let mut loaded = load_trajectory_for_chat(gcx.clone(), chat_id)
            .await
            .unwrap();
        assert!(loaded.messages.is_empty());
        assert_eq!(loaded.thread.auto_compression_cap, None);
        assert!(loaded.thread.auto_compression_cap_pending);
        assert!(loaded.thread.resolve_pending_compression_cap(Some(100_000)));
        assert_eq!(loaded.thread.auto_compression_cap, Some(72_000));

        // An empty legacy file is not evidence that this is a new chat.
        let path = find_trajectory_path(gcx.clone(), chat_id).await.unwrap();
        let mut value: serde_json::Value =
            serde_json::from_slice(&tokio::fs::read(&path).await.unwrap()).unwrap();
        value
            .as_object_mut()
            .unwrap()
            .remove("auto_compression_cap_pending");
        tokio::fs::write(path, serde_json::to_vec(&value).unwrap())
            .await
            .unwrap();
        let mut legacy = load_trajectory_for_chat(gcx, chat_id).await.unwrap();
        assert!(legacy.messages.is_empty());
        assert!(!legacy.thread.resolve_pending_compression_cap(Some(100_000)));
        assert_eq!(legacy.thread.auto_compression_cap, None);
    }

    #[tokio::test]
    async fn explicit_auto_compression_cap_makes_empty_chat_save_worthy() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let chat_id = "empty-chat-auto-compression-cap";
        let mut snapshot = test_snapshot(chat_id, "Empty capped chat", Vec::new());
        snapshot.auto_compression_cap = Some(64_000);

        save_trajectory_snapshot(gcx.clone(), snapshot)
            .await
            .unwrap();
        let loaded = load_trajectory_for_chat(gcx, chat_id).await.unwrap();
        assert!(loaded.messages.is_empty());
        assert_eq!(loaded.thread.auto_compression_cap, Some(64_000));
    }

    #[serial(trajectory_perf)]
    #[serial(trajectory_perf)]
    #[tokio::test]
    async fn save_trajectory_as_is_durable_with_the_detached_writer() {
        let _lock = serial_test_guard();
        let _writer = TrajectoryWriterRolloutGuard::set(true);
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let thread = ThreadParams {
            id: "save-trajectory-as-durable".to_string(),
            title: "Durable direct save".to_string(),
            ..Default::default()
        };

        save_trajectory_as(
            gcx.clone(),
            &thread,
            &[ChatMessage::new(
                "user".to_string(),
                "persisted".to_string(),
            )],
        )
        .await;

        let loaded = load_trajectory_for_chat(gcx, &thread.id).await;
        assert!(loaded.is_some());
        assert_eq!(
            loaded.unwrap().messages[0].content.content_text_only(),
            "persisted"
        );
    }

    #[tokio::test]
    async fn empty_chat_can_clear_previously_saved_auto_compression_cap() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let chat_id = "empty-chat-clear-auto-compression-cap";
        let mut thread = ThreadParams {
            id: chat_id.to_string(),
            title: "Empty capped chat".to_string(),
            auto_compression_cap: Some(64_000),
            ..Default::default()
        };

        save_trajectory_as(gcx.clone(), &thread, &[]).await;
        tokio::time::timeout(Duration::from_secs(60), async {
            loop {
                if load_trajectory_for_chat(gcx.clone(), chat_id)
                    .await
                    .is_some()
                {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("checkpoint should persist the empty capped chat");
        assert_eq!(
            load_trajectory_for_chat(gcx.clone(), chat_id)
                .await
                .unwrap()
                .thread
                .auto_compression_cap,
            Some(64_000)
        );

        thread.auto_compression_cap = None;
        save_trajectory_as(gcx.clone(), &thread, &[]).await;
        tokio::time::timeout(Duration::from_secs(60), async {
            loop {
                if load_trajectory_for_chat(gcx.clone(), chat_id)
                    .await
                    .is_some_and(|loaded| loaded.thread.auto_compression_cap.is_none())
                {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("checkpoint should clear the persisted cap");
        assert_eq!(
            load_trajectory_for_chat(gcx, chat_id)
                .await
                .unwrap()
                .thread
                .auto_compression_cap,
            None
        );
    }

    #[tokio::test]
    async fn generic_loader_ignores_buddy_only_fallback() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let chat_id = "buddy-generic-loader-isolated";
        let buddy_path = dir
            .path()
            .join(".refact")
            .join("buddy")
            .join("chats")
            .join("conversations")
            .join(format!("{chat_id}.json"));
        write_buddy_conversation_file(&buddy_path, chat_id, "Only Buddy").await;

        assert!(load_generic_trajectory_for_chat(gcx.clone(), chat_id)
            .await
            .is_none());
        let loaded = load_trajectory_for_chat(gcx, chat_id).await.unwrap();

        assert_eq!(loaded.thread.title, "Only Buddy");
        assert!(loaded.thread.buddy_meta.is_some());
    }

    #[tokio::test]
    async fn generic_watcher_remove_with_buddy_fallback_emits_deleted() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;
        let mut rx = app.chat.trajectory_events_tx.subscribe();
        let chat_id = "buddy-watcher-remove-fallback";
        let buddy_path = dir
            .path()
            .join(".refact")
            .join("buddy")
            .join("chats")
            .join("conversations")
            .join(format!("{chat_id}.json"));
        write_buddy_conversation_file(&buddy_path, chat_id, "Fallback Buddy").await;

        process_trajectory_change(&gcx.chat_ctx(gcx.clone()), chat_id, true).await;

        let event = wait_for_trajectory_event(&mut rx, chat_id).await;
        assert_eq!(event.event_type, "deleted");
        assert_eq!(event.title, None);
        assert_eq!(event.mode, None);
        assert!(tokio::fs::try_exists(&buddy_path).await.unwrap());
    }

    #[tokio::test]
    async fn generic_watcher_remove_with_loadable_fallback_emits_updated() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;
        let mut rx = app.chat.trajectory_events_tx.subscribe();
        let chat_id = "watcher-remove-generic-fallback-updated";
        let fallback_path = dir
            .path()
            .join(".refact")
            .join("tasks")
            .join("task-watch-fallback")
            .join("trajectories")
            .join("planner")
            .join(format!("{chat_id}.json"));
        write_trajectory_file_with_metadata(
            &fallback_path,
            chat_id,
            "Watcher Fallback",
            "2024-01-01T00:00:01Z",
            "fallback after remove",
        )
        .await;

        process_trajectory_change(&gcx.chat_ctx(gcx.clone()), chat_id, true).await;

        let event = wait_for_trajectory_event(&mut rx, chat_id).await;
        assert_eq!(event.event_type, "updated");
        assert_eq!(event.title.as_deref(), Some("Watcher Fallback"));
        assert_eq!(event.task_id.as_deref(), Some("task-watch-fallback"));
        assert_eq!(event.task_role.as_deref(), Some("planner"));
        assert!(tokio::fs::try_exists(&fallback_path).await.unwrap());
    }

    #[tokio::test]
    async fn trajectory_id_mismatch_is_not_loaded() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join("safe.json");
        write_trajectory_file(&path, "other-chat", "Mismatched", "2024-01-01T00:00:00Z").await;

        assert!(load_trajectory_for_chat(gcx, "safe").await.is_none());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn symlinked_normal_trajectory_root_is_ignored_for_load() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let chat_id = "symlink-load-ignored";
        let refact_dir = dir.path().join(".refact");
        tokio::fs::create_dir_all(&refact_dir).await.unwrap();
        std::os::unix::fs::symlink(outside.path(), refact_dir.join("trajectories")).unwrap();
        let outside_path = outside.path().join(format!("{chat_id}.json"));
        write_trajectory_file(
            &outside_path,
            chat_id,
            "Outside Symlink Load",
            "2024-01-01T00:00:00Z",
        )
        .await;

        assert!(find_trajectory_path(gcx.clone(), chat_id).await.is_none());
        assert!(load_trajectory_for_chat(gcx, chat_id).await.is_none());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn symlinked_normal_trajectory_root_is_not_used_for_save() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let chat_id = "symlink-save-blocked";
        let refact_dir = dir.path().join(".refact");
        tokio::fs::create_dir_all(&refact_dir).await.unwrap();
        std::os::unix::fs::symlink(outside.path(), refact_dir.join("trajectories")).unwrap();
        let outside_path = outside.path().join(format!("{chat_id}.json"));
        write_trajectory_file(
            &outside_path,
            chat_id,
            "Keep Outside Save",
            "2024-01-01T00:00:00Z",
        )
        .await;
        let before = tokio::fs::read_to_string(&outside_path).await.unwrap();

        let err = save_trajectory_snapshot(
            gcx,
            test_snapshot(
                chat_id,
                "Should Not Escape",
                vec![ChatMessage::new("user".to_string(), "hello".to_string())],
            ),
        )
        .await
        .unwrap_err();

        assert!(err.contains("Refusing to use non-real directory component"));
        assert_eq!(
            tokio::fs::read_to_string(outside_path).await.unwrap(),
            before
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn symlinked_normal_trajectory_root_is_not_used_for_delete() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let (_gcx, app) = make_app_with_workspace(dir.path()).await;
        let chat_id = "symlink-delete-blocked";
        let refact_dir = dir.path().join(".refact");
        tokio::fs::create_dir_all(&refact_dir).await.unwrap();
        std::os::unix::fs::symlink(outside.path(), refact_dir.join("trajectories")).unwrap();
        let outside_path = outside.path().join(format!("{chat_id}.json"));
        write_trajectory_file(
            &outside_path,
            chat_id,
            "Keep Outside Delete",
            "2024-01-01T00:00:00Z",
        )
        .await;

        let err = handle_v1_trajectories_delete(State(app), AxumPath(chat_id.to_string()))
            .await
            .unwrap_err();

        assert_eq!(err.status_code, StatusCode::NOT_FOUND);
        assert!(tokio::fs::try_exists(outside_path).await.unwrap());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn symlinked_normal_trajectory_file_is_not_followed() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;
        let chat_id = "symlink-file-blocked";
        let trajectories_dir = dir.path().join(".refact").join("trajectories");
        tokio::fs::create_dir_all(&trajectories_dir).await.unwrap();
        let outside_path = outside.path().join(format!("{chat_id}.json"));
        write_trajectory_file(
            &outside_path,
            chat_id,
            "Keep Outside File",
            "2024-01-01T00:00:00Z",
        )
        .await;
        let before = tokio::fs::read_to_string(&outside_path).await.unwrap();
        std::os::unix::fs::symlink(
            &outside_path,
            trajectories_dir.join(format!("{chat_id}.json")),
        )
        .unwrap();

        assert!(find_trajectory_path(gcx.clone(), chat_id).await.is_none());
        let delete_err = handle_v1_trajectories_delete(State(app), AxumPath(chat_id.to_string()))
            .await
            .unwrap_err();
        assert_eq!(delete_err.status_code, StatusCode::NOT_FOUND);

        let save_err = save_trajectory_snapshot(
            gcx,
            test_snapshot(
                chat_id,
                "Should Not Follow File",
                vec![ChatMessage::new("user".to_string(), "hello".to_string())],
            ),
        )
        .await
        .unwrap_err();

        assert!(save_err.contains("Refusing symlink child file"));
        assert_eq!(
            tokio::fs::read_to_string(outside_path).await.unwrap(),
            before
        );
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn macos_system_temp_symlink_roots_are_allowed_for_save() {
        let dir = tempfile::tempdir_in(std::env::temp_dir()).unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let chat_id = "macos-var-save-ok";

        save_trajectory_snapshot(
            gcx,
            test_snapshot(
                chat_id,
                "macOS /var Save",
                vec![ChatMessage::new("user".to_string(), "hello".to_string())],
            ),
        )
        .await
        .unwrap();

        assert!(tokio::fs::try_exists(
            dir.path()
                .join(".refact")
                .join("trajectories")
                .join(format!("{chat_id}.json"))
        )
        .await
        .unwrap());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn symlinked_task_trajectory_root_is_ignored_for_load_and_delete() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;
        let chat_id = "symlink-task-root-blocked";
        let tasks_dir = dir.path().join(".refact").join("tasks");
        tokio::fs::create_dir_all(tasks_dir.parent().unwrap())
            .await
            .unwrap();
        std::os::unix::fs::symlink(outside.path(), &tasks_dir).unwrap();
        let outside_path = outside
            .path()
            .join("task-symlink-root")
            .join("trajectories")
            .join("planner")
            .join(format!("{chat_id}.json"));
        write_trajectory_file(
            &outside_path,
            chat_id,
            "Outside Task Root",
            "2024-01-01T00:00:00Z",
        )
        .await;

        assert!(find_trajectory_path(gcx.clone(), chat_id).await.is_none());
        assert!(load_trajectory_for_chat(gcx, chat_id).await.is_none());
        let err = handle_v1_trajectories_delete(State(app), AxumPath(chat_id.to_string()))
            .await
            .unwrap_err();
        assert_eq!(err.status_code, StatusCode::NOT_FOUND);
        assert!(tokio::fs::try_exists(outside_path).await.unwrap());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn symlinked_task_trajectory_agent_dir_is_ignored_for_load_and_delete() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;
        let chat_id = "symlink-task-agent-blocked";
        let agents_dir = dir
            .path()
            .join(".refact")
            .join("tasks")
            .join("task-symlink-agent")
            .join("trajectories")
            .join("agents");
        tokio::fs::create_dir_all(&agents_dir).await.unwrap();
        std::os::unix::fs::symlink(outside.path(), agents_dir.join("agent-1")).unwrap();
        let outside_path = outside.path().join(format!("{chat_id}.json"));
        write_trajectory_file(
            &outside_path,
            chat_id,
            "Outside Task Agent",
            "2024-01-01T00:00:00Z",
        )
        .await;

        assert!(find_trajectory_path(gcx.clone(), chat_id).await.is_none());
        assert!(load_trajectory_for_chat(gcx, chat_id).await.is_none());
        let err = handle_v1_trajectories_delete(State(app), AxumPath(chat_id.to_string()))
            .await
            .unwrap_err();
        assert_eq!(err.status_code, StatusCode::NOT_FOUND);
        assert!(tokio::fs::try_exists(outside_path).await.unwrap());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn symlinked_buddy_conversations_dir_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let chat_id = "symlink-buddy-blocked";
        let buddy_parent = dir.path().join(".refact").join("buddy").join("chats");
        tokio::fs::create_dir_all(&buddy_parent).await.unwrap();
        std::os::unix::fs::symlink(outside.path(), buddy_parent.join("conversations")).unwrap();
        let outside_path = outside.path().join(format!("{chat_id}.json"));
        write_buddy_conversation_file(&outside_path, chat_id, "Outside Buddy").await;

        assert!(find_trajectory_or_buddy_path(gcx.clone(), chat_id)
            .await
            .is_none());
        assert!(load_trajectory_for_chat(gcx, chat_id).await.is_none());
    }

    #[tokio::test]
    async fn trajectory_id_mismatch_normal_candidate_does_not_shadow_task_candidate() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let chat_id = "safe-task-shadow";
        let normal_path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        let task_path = dir
            .path()
            .join(".refact")
            .join("tasks")
            .join("task-shadow")
            .join("trajectories")
            .join("planner")
            .join(format!("{chat_id}.json"));
        write_trajectory_file(
            &normal_path,
            "other-chat",
            "Mismatched Normal",
            "2024-01-01T00:00:00Z",
        )
        .await;
        write_trajectory_file(&task_path, chat_id, "Valid Task", "2024-01-01T00:00:01Z").await;

        assert_same_optional_path(find_trajectory_path(gcx.clone(), chat_id).await, &task_path);
        let loaded = load_trajectory_for_chat(gcx, chat_id).await.unwrap();
        assert_eq!(loaded.thread.title, "Valid Task");
    }

    #[tokio::test]
    async fn schema_incomplete_normal_candidate_does_not_shadow_task_candidate() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let chat_id = "schema-task-shadow";
        let normal_path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        let task_path = dir
            .path()
            .join(".refact")
            .join("tasks")
            .join("task-schema-shadow")
            .join("trajectories")
            .join("planner")
            .join(format!("{chat_id}.json"));
        write_schema_incomplete_trajectory_file(&normal_path, chat_id).await;
        write_trajectory_file(&task_path, chat_id, "Valid Task", "2024-01-01T00:00:01Z").await;

        assert_same_optional_path(find_trajectory_path(gcx.clone(), chat_id).await, &task_path);
        let loaded = load_trajectory_for_chat(gcx, chat_id).await.unwrap();
        assert_eq!(loaded.thread.title, "Valid Task");
    }

    #[tokio::test]
    async fn schema_incomplete_candidate_only_is_not_loaded() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let chat_id = "schema-incomplete-only";
        let path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        write_schema_incomplete_trajectory_file(&path, chat_id).await;

        assert!(find_trajectory_path(gcx.clone(), chat_id).await.is_none());
        assert!(load_trajectory_for_chat(gcx, chat_id).await.is_none());
    }

    #[tokio::test]
    async fn trajectory_id_mismatch_workspace_candidate_does_not_shadow_global_candidate() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let chat_id = "safe-global-shadow";
        let workspace_path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        let global_dir = get_global_trajectories_dir(gcx.clone()).await;
        let global_path = global_dir.join(format!("{chat_id}.json"));
        write_trajectory_file(
            &workspace_path,
            "other-chat",
            "Mismatched Workspace",
            "2024-01-01T00:00:00Z",
        )
        .await;
        write_trajectory_file(
            &global_path,
            chat_id,
            "Valid Global",
            "2024-01-01T00:00:01Z",
        )
        .await;

        assert_same_optional_path(
            find_trajectory_path(gcx.clone(), chat_id).await,
            &global_path,
        );
        let loaded = load_trajectory_for_chat(gcx, chat_id).await.unwrap();
        assert_eq!(loaded.thread.title, "Valid Global");
    }

    #[tokio::test]
    async fn trajectory_id_mismatch_delete_skips_invalid_higher_candidate() {
        let dir = tempfile::tempdir().unwrap();
        let (_gcx, app) = make_app_with_workspace(dir.path()).await;
        let chat_id = "delete-shadow";
        let normal_path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        let task_path = dir
            .path()
            .join(".refact")
            .join("tasks")
            .join("task-delete-shadow")
            .join("trajectories")
            .join("planner")
            .join(format!("{chat_id}.json"));
        write_trajectory_file(
            &normal_path,
            "other-chat",
            "Keep Mismatched",
            "2024-01-01T00:00:00Z",
        )
        .await;
        write_trajectory_file(&task_path, chat_id, "Delete Valid", "2024-01-01T00:00:01Z").await;

        handle_v1_trajectories_delete(State(app), AxumPath(chat_id.to_string()))
            .await
            .unwrap();

        assert!(tokio::fs::try_exists(&normal_path).await.unwrap());
        assert!(!tokio::fs::try_exists(&task_path).await.unwrap());
    }

    #[tokio::test]
    async fn generic_list_does_not_use_active_buddy_state_error_or_worktree() {
        let dir = tempfile::tempdir().unwrap();
        let (_gcx, app) = make_app_with_workspace(dir.path()).await;
        let chat_id = "list-active-buddy-clean";
        let normal_path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        write_trajectory_file(
            &normal_path,
            chat_id,
            "Generic List Item",
            "2024-01-01T00:00:01Z",
        )
        .await;
        let listed_without_session = list_all_trajectories_meta(app.clone(), false)
            .await
            .unwrap();
        let item_without_session = listed_without_session
            .iter()
            .find(|item| item.id == chat_id)
            .cloned()
            .unwrap();
        assert_eq!(item_without_session.session_state, None);
        assert!(item_without_session.worktree.is_none());

        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        {
            let mut session = session_arc.lock().await;
            session.thread.mode = "buddy".to_string();
            session.thread.buddy_meta = Some(buddy_thread_meta());
            session.thread.worktree = Some(trajectory_worktree_sample_with_id("buddy-wt"));
            session.runtime.state = SessionState::Generating;
            session.runtime.error = Some("buddy list error".to_string());
        }
        app.chat
            .sessions
            .write()
            .await
            .insert(chat_id.to_string(), session_arc);

        let listed = list_all_trajectories_meta(app, false).await.unwrap();
        let item = listed
            .iter()
            .find(|item| item.id == chat_id)
            .cloned()
            .unwrap();

        assert_eq!(item.session_state, None);
        assert!(item.worktree.is_none());
        assert_eq!(
            serde_json::to_value(&item).unwrap(),
            serde_json::to_value(&item_without_session).unwrap()
        );
    }

    #[tokio::test]
    async fn generic_list_keeps_normal_active_session_state_and_worktree_enrichment() {
        let dir = tempfile::tempdir().unwrap();
        let (_gcx, app) = make_app_with_workspace(dir.path()).await;
        let chat_id = "list-normal-active-enriched";
        let normal_path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        write_trajectory_file(
            &normal_path,
            chat_id,
            "Normal List Item",
            "2024-01-01T00:00:01Z",
        )
        .await;
        let worktree = trajectory_worktree_sample_with_id("normal-wt");
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        {
            let mut session = session_arc.lock().await;
            session.thread.worktree = Some(worktree.clone());
            session.runtime.state = SessionState::Generating;
        }
        app.chat
            .sessions
            .write()
            .await
            .insert(chat_id.to_string(), session_arc);

        let listed = list_all_trajectories_meta(app, false).await.unwrap();
        let item = listed.iter().find(|item| item.id == chat_id).unwrap();

        assert_eq!(item.session_state.as_deref(), Some("generating"));
        assert_eq!(
            item.worktree.as_ref().map(|meta| meta.id.as_str()),
            Some("normal-wt")
        );
    }

    #[tokio::test]
    async fn generic_list_normal_trajectory_ignores_active_task_state_error_and_worktree() {
        let dir = tempfile::tempdir().unwrap();
        let (_gcx, app) = make_app_with_workspace(dir.path()).await;
        let chat_id = "list-normal-active-task-clean";
        let normal_path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        write_trajectory_file(
            &normal_path,
            chat_id,
            "Normal Collision List Item",
            "2024-01-01T00:00:01Z",
        )
        .await;
        let task_meta = task_meta(
            "task-list-normal-collision",
            "agents",
            Some("agent-list-normal"),
            Some("card-list-normal"),
            None,
        );
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        {
            let mut session = session_arc.lock().await;
            session.thread.task_meta = Some(task_meta);
            session.thread.worktree = Some(trajectory_worktree_sample_with_id("task-list-wt"));
            session.runtime.state = SessionState::Generating;
            session.runtime.error = Some("task list error".to_string());
        }
        app.chat
            .sessions
            .write()
            .await
            .insert(chat_id.to_string(), session_arc);

        let listed = list_all_trajectories_meta(app, false).await.unwrap();
        let item = listed.iter().find(|item| item.id == chat_id).unwrap();

        assert_eq!(item.session_state, None);
        assert!(item.worktree.is_none());
    }

    #[tokio::test]
    async fn generic_list_task_trajectory_ignores_active_normal_state_error_and_worktree() {
        let dir = tempfile::tempdir().unwrap();
        let (_gcx, app) = make_app_with_workspace(dir.path()).await;
        let chat_id = "list-task-active-normal-clean";
        let task_meta = task_meta(
            "task-list-active-normal",
            "agents",
            Some("agent-list-task"),
            Some("card-list-task"),
            None,
        );
        let task_path = dir
            .path()
            .join(".refact")
            .join("tasks")
            .join("task-list-active-normal")
            .join("trajectories")
            .join("agents")
            .join("agent-list-task")
            .join(format!("{chat_id}.json"));
        write_task_trajectory_file_with_user_message(
            &task_path,
            chat_id,
            "Task Collision List Item",
            "task list message",
            &task_meta,
        )
        .await;
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        {
            let mut session = session_arc.lock().await;
            session.thread.worktree = Some(trajectory_worktree_sample_with_id("normal-list-wt"));
            session.runtime.state = SessionState::Generating;
            session.runtime.error = Some("normal list error".to_string());
        }
        app.chat
            .sessions
            .write()
            .await
            .insert(chat_id.to_string(), session_arc);

        let listed = list_all_trajectories_meta(app, false).await.unwrap();
        let item = listed.iter().find(|item| item.id == chat_id).unwrap();

        assert_eq!(item.task_id.as_deref(), Some("task-list-active-normal"));
        assert_eq!(item.task_role.as_deref(), Some("agents"));
        assert_eq!(item.session_state, None);
        assert!(item.worktree.is_none());
    }

    #[tokio::test]
    async fn generic_list_keeps_task_active_session_state_and_worktree_enrichment() {
        let dir = tempfile::tempdir().unwrap();
        let (_gcx, app) = make_app_with_workspace(dir.path()).await;
        let chat_id = "list-task-active-enriched";
        let task_meta = task_meta(
            "task-list-active-enriched",
            "agents",
            Some("agent-list-enriched"),
            Some("card-list-enriched"),
            None,
        );
        let task_path = dir
            .path()
            .join(".refact")
            .join("tasks")
            .join("task-list-active-enriched")
            .join("trajectories")
            .join("agents")
            .join("agent-list-enriched")
            .join(format!("{chat_id}.json"));
        write_task_trajectory_file_with_user_message(
            &task_path,
            chat_id,
            "Task List Item",
            "task list message",
            &task_meta,
        )
        .await;
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        {
            let mut session = session_arc.lock().await;
            session.thread.task_meta = Some(task_meta);
            session.thread.worktree =
                Some(trajectory_worktree_sample_with_id("task-list-enriched-wt"));
            session.runtime.state = SessionState::Generating;
        }
        app.chat
            .sessions
            .write()
            .await
            .insert(chat_id.to_string(), session_arc);

        let listed = list_all_trajectories_meta(app, false).await.unwrap();
        let item = listed.iter().find(|item| item.id == chat_id).unwrap();

        assert_eq!(item.session_state.as_deref(), Some("generating"));
        assert_eq!(
            item.worktree.as_ref().map(|meta| meta.id.as_str()),
            Some("task-list-enriched-wt")
        );
    }

    #[tokio::test]
    async fn http_delete_higher_priority_trajectory_emits_updated_from_fallback() {
        let dir = tempfile::tempdir().unwrap();
        let (_gcx, app) = make_app_with_workspace(dir.path()).await;
        let mut rx = app.chat.trajectory_events_tx.subscribe();
        let chat_id = "http-delete-fallback-updated";
        let normal_path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        let task_path = dir
            .path()
            .join(".refact")
            .join("tasks")
            .join("task-fallback")
            .join("trajectories")
            .join("planner")
            .join(format!("{chat_id}.json"));
        write_trajectory_file(
            &normal_path,
            chat_id,
            "Deleted Higher Priority",
            "2024-01-01T00:00:02Z",
        )
        .await;
        write_trajectory_file_with_metadata(
            &task_path,
            chat_id,
            "Fallback Lower Priority",
            "2024-01-01T00:00:01Z",
            "fallback message",
        )
        .await;
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        {
            let mut session = session_arc.lock().await;
            session.thread.title = "Active Normal Collision".to_string();
            session.runtime.state = SessionState::Generating;
            session.runtime.error = Some("busy fallback state".to_string());
        }
        app.chat
            .sessions
            .write()
            .await
            .insert(chat_id.to_string(), session_arc);

        handle_v1_trajectories_delete(State(app), AxumPath(chat_id.to_string()))
            .await
            .unwrap();

        let event = wait_for_trajectory_event(&mut rx, chat_id).await;
        assert_eq!(event.event_type, "updated");
        assert_eq!(event.updated_at.as_deref(), Some("2024-01-01T00:00:01Z"));
        assert_eq!(event.title.as_deref(), Some("Fallback Lower Priority"));
        assert_eq!(event.is_title_generated, Some(true));
        assert_eq!(event.message_count, Some(1));
        assert_eq!(event.parent_id.as_deref(), Some("parent-fallback"));
        assert_eq!(event.link_type.as_deref(), Some("handoff"));
        assert_eq!(event.root_chat_id.as_deref(), Some("root-fallback"));
        assert_eq!(event.task_id.as_deref(), Some("task-fallback"));
        assert_eq!(event.task_role.as_deref(), Some("planner"));
        assert_eq!(event.card_id, None);
        assert_eq!(event.model.as_deref(), Some("fallback-model"));
        assert_eq!(event.mode.as_deref(), Some("task_planner"));
        assert_eq!(event.session_state.as_deref(), Some("idle"));
        assert_eq!(event.error, None);
        assert_ne!(event.title.as_deref(), Some("Active Normal Collision"));
        assert!(!tokio::fs::try_exists(&normal_path).await.unwrap());
        assert!(tokio::fs::try_exists(&task_path).await.unwrap());
    }

