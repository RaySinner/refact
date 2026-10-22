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
    #[test]
    fn malformed_nested_report_remains_browsable_without_emptying_archive() {
        let raw = serde_json::json!([
            {"role":"user", "content":"ARCHIVED_LOAD_SENTINEL"},
            {"role":"compression_report", "content":"invalid boundary", "compression_report":{
                "kind":"reconstructed_history", "schema_version":1,
                "payload":{"messages":"not an array"}
            }}
        ]);
        let messages = super::parse_stored_messages(raw).unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(
            messages[0].content.content_text_only(),
            "ARCHIVED_LOAD_SENTINEL"
        );
        assert!(refact_core::active_context::active_context(&messages).is_err());
        assert!(super::parse_stored_messages(serde_json::json!([{"role":42}])).is_err());
    }

    #[test]
    fn trajectory_snapshot_excludes_runtime_only_diagnostic_timestamps() {
        let mut session = ChatSession::new("runtime-only-diagnostics".to_string());
        let now = Instant::now();
        session
            .command_enqueued_at
            .insert("request-1".to_string(), now);
        session.stream_started_at = Some(now);
        session.confirmation_paused_at = Some((
            now,
            std::collections::HashSet::from(["tool-call".to_string()]),
        ));

        let serialized = serde_json::to_string(&trajectory_snapshot_from_session(&session))
            .expect("trajectory snapshot serializes");

        assert!(!serialized.contains("command_enqueued_at"));
        assert!(!serialized.contains("stream_started_at"));
        assert!(!serialized.contains("confirmation_paused_at"));
    }

    #[test]
    fn trajectory_commit_intents_are_table_driven() {
        let boundaries = trajectory_commit_contract();
        assert_eq!(
            boundaries,
            [
                (
                    "explicit acknowledged mutations",
                    TrajectoryCommitIntent::Required
                ),
                (
                    "branch handoff mode transition",
                    TrajectoryCommitIntent::Required
                ),
                ("final failed subchat", TrajectoryCommitIntent::Required),
                ("close", TrajectoryCommitIntent::Required),
                ("graceful shutdown", TrajectoryCommitIntent::Required),
                (
                    "assistant tool subchat progress",
                    TrajectoryCommitIntent::Checkpoint,
                ),
                ("stream delta", TrajectoryCommitIntent::Ephemeral),
            ]
        );
        assert!(TrajectoryCommitIntent::Required.requires_durability());
        assert!(TrajectoryCommitIntent::Checkpoint.persists());
        assert!(!TrajectoryCommitIntent::Ephemeral.persists());
    }

    #[test]
    fn trajectory_commit_stale_completion_cannot_clear_newer_dirty_state() {
        let mut session = ChatSession::new("trajectory-commit-stale".to_string());

        session.increment_version();
        assert!(session.complete_trajectory_commit(1));
        assert_eq!(session.trajectory_committed_version, 1);
        assert!(!session.trajectory_dirty);

        session.increment_version();
        assert!(session.complete_trajectory_commit(2));
        assert_eq!(session.trajectory_committed_version, 2);
        assert!(!session.trajectory_dirty);

        session.increment_version();
        assert!(!session.complete_trajectory_commit(1));
        assert_eq!(session.trajectory_committed_version, 2);
        assert!(session.trajectory_dirty);
    }

    #[test]
    fn trajectory_commit_versions_are_isolated_per_chat() {
        let mut chat_a = ChatSession::new("trajectory-commit-a".to_string());
        let mut chat_b = ChatSession::new("trajectory-commit-b".to_string());

        chat_a.increment_version();
        chat_a.complete_trajectory_commit(1);
        chat_a.increment_version();

        chat_b.increment_version();
        chat_b.complete_trajectory_commit(1);
        chat_b.increment_version();
        chat_b.complete_trajectory_commit(2);

        assert_eq!(chat_a.trajectory_committed_version, 1);
        assert!(chat_a.trajectory_dirty);
        assert_eq!(chat_b.trajectory_committed_version, 2);
        assert!(!chat_b.trajectory_dirty);
    }

    #[serial(trajectory_perf)]
    #[tokio::test]
    async fn trajectory_commit_rejects_a_stale_snapshot_before_file_write() {
        let _lock = serial_test_guard();
        let workspace = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(workspace.path()).await;
        let chat_id = "trajectory-commit-file-barrier";
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));

        let stale = {
            let mut session = session_arc.lock().await;
            session.add_message(ChatMessage::new("user".to_string(), "older".to_string()));
            trajectory_snapshot_from_session(&session)
        };
        {
            let mut session = session_arc.lock().await;
            session.add_message(ChatMessage::new("user".to_string(), "newer".to_string()));
        }
        let current = {
            let session = session_arc.lock().await;
            trajectory_snapshot_from_session(&session)
        };

        assert_eq!(
            commit_trajectory_snapshot_for_session(
                gcx.clone(),
                session_arc.clone(),
                current,
                TrajectoryCommitIntent::Required,
            )
            .await
            .unwrap(),
            SessionTrajectoryCommitOutcome::Committed
        );
        assert_eq!(
            commit_trajectory_snapshot_for_session(
                gcx.clone(),
                session_arc.clone(),
                stale,
                TrajectoryCommitIntent::Required,
            )
            .await
            .unwrap(),
            SessionTrajectoryCommitOutcome::SkippedStale
        );

        let loaded = load_trajectory_for_chat(gcx, chat_id).await.unwrap();
        let content = loaded
            .messages
            .iter()
            .map(|message| message.content.content_text_only())
            .collect::<Vec<_>>();
        assert_eq!(content, vec!["older", "newer"]);
    }

    #[serial(trajectory_perf)]
    #[tokio::test]
    async fn trajectory_writer_enabled_checkpoints_coalesce_to_the_latest_session_snapshot() {
        let _lock = serial_test_guard();
        let _writer = TrajectoryWriterRolloutGuard::set(true);
        let workspace = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(workspace.path()).await;
        let chat_id = "checkpoint-writer-coalesces";
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));

        {
            let mut session = session_arc.lock().await;
            session.add_message(ChatMessage::new("user".to_string(), "first".to_string()));
        }
        maybe_save_trajectory_background_with_intent(
            app.clone(),
            session_arc.clone(),
            TrajectoryCommitIntent::Checkpoint,
        );
        {
            let mut session = session_arc.lock().await;
            session.add_message(ChatMessage::new("user".to_string(), "second".to_string()));
        }
        maybe_save_trajectory_background_with_intent(
            app.clone(),
            session_arc.clone(),
            TrajectoryCommitIntent::Checkpoint,
        );

        flush_trajectory_for_session(app, session_arc)
            .await
            .unwrap();
        let loaded = load_trajectory_for_chat(gcx, chat_id).await.unwrap();
        assert_eq!(
            loaded
                .messages
                .iter()
                .map(|message| message.content.content_text_only())
                .collect::<Vec<_>>(),
            vec!["first", "second"]
        );
    }

    #[serial(trajectory_perf)]
    #[tokio::test]
    async fn trajectory_writer_disabled_checkpoint_saves_synchronously() {
        let _lock = serial_test_guard();
        let _writer = TrajectoryWriterRolloutGuard::set(false);
        let workspace = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(workspace.path()).await;
        let chat_id = "checkpoint-writer-legacy";
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));

        session_arc
            .lock()
            .await
            .add_message(ChatMessage::new("user".to_string(), "first".to_string()));
        try_save_trajectory_with_intent(
            app,
            session_arc.clone(),
            TrajectoryCommitIntent::Checkpoint,
        )
        .await
        .unwrap();

        let session = session_arc.lock().await;
        assert!(!session.trajectory_dirty);
        assert!(!session.trajectory_save_in_flight);
        assert_eq!(
            session.trajectory_committed_version,
            session.trajectory_version
        );
        drop(session);
        let loaded = load_trajectory_for_chat(gcx, chat_id).await.unwrap();
        assert_eq!(loaded.messages.len(), 1);
    }

    #[serial(trajectory_perf)]
    #[tokio::test]
    async fn trajectory_writer_background_required_intent_waits_for_its_commit() {
        let _lock = serial_test_guard();
        let _writer = TrajectoryWriterRolloutGuard::set(true);
        let workspace = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(workspace.path()).await;
        let chat_id = "background-required-writer";
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        session_arc
            .lock()
            .await
            .add_message(ChatMessage::new("user".to_string(), "required".to_string()));

        maybe_save_trajectory_background_with_intent(
            app,
            session_arc.clone(),
            TrajectoryCommitIntent::Required,
        );
        tokio::time::timeout(
            Duration::from_secs(5),
            wait_for_trajectory_commit(session_arc.clone(), 1),
        )
        .await
        .expect("background required save should commit")
        .unwrap();

        let session = session_arc.lock().await;
        assert!(!session.trajectory_dirty);
        assert_eq!(
            session.trajectory_committed_version,
            session.trajectory_version
        );
        drop(session);
        assert!(load_trajectory_for_chat(gcx, chat_id).await.is_some());
    }

    #[serial(trajectory_perf)]
    #[tokio::test]
    async fn trajectory_writer_paths_preserve_stale_required_and_shutdown_commits() {
        let _lock = serial_test_guard();
        for enabled in [false, true] {
            let _writer = TrajectoryWriterRolloutGuard::set(enabled);
            let workspace = tempfile::tempdir().unwrap();
            let (gcx, app) = make_app_with_workspace(workspace.path()).await;
            let chat_id = format!("writer-path-{enabled}");
            let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.clone())));

            let stale = {
                let mut session = session_arc.lock().await;
                session.add_message(ChatMessage::new("user".to_string(), "older".to_string()));
                trajectory_snapshot_from_session(&session)
            };
            let current = {
                let mut session = session_arc.lock().await;
                session.add_message(ChatMessage::new("user".to_string(), "newer".to_string()));
                trajectory_snapshot_from_session(&session)
            };
            assert_eq!(
                commit_trajectory_snapshot_for_session(
                    gcx.clone(),
                    session_arc.clone(),
                    current,
                    TrajectoryCommitIntent::Required,
                )
                .await
                .unwrap(),
                SessionTrajectoryCommitOutcome::Committed
            );
            assert_eq!(
                commit_trajectory_snapshot_for_session(
                    gcx.clone(),
                    session_arc.clone(),
                    stale,
                    TrajectoryCommitIntent::Required,
                )
                .await
                .unwrap(),
                SessionTrajectoryCommitOutcome::SkippedStale
            );

            session_arc
                .lock()
                .await
                .add_message(ChatMessage::new("user".to_string(), "shutdown".to_string()));
            app.chat
                .sessions
                .write()
                .await
                .insert(chat_id.clone(), session_arc);
            flush_all_trajectories(app).await.unwrap();

            let loaded = load_trajectory_for_chat(gcx, &chat_id).await.unwrap();
            assert_eq!(
                loaded
                    .messages
                    .iter()
                    .map(|message| message.content.content_text_only())
                    .collect::<Vec<_>>(),
                vec!["older", "newer", "shutdown"]
            );
        }
    }

    #[serial(trajectory_perf)]
    #[tokio::test]
    async fn required_writer_waits_for_a_checkpoint_and_commits_latest_version() {
        let _lock = serial_test_guard();
        let _writer = TrajectoryWriterRolloutGuard::set(true);
        let workspace = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(workspace.path()).await;
        let chat_id = "required-after-checkpoint";
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));

        {
            let mut session = session_arc.lock().await;
            session.add_message(ChatMessage::new(
                "user".to_string(),
                "checkpoint".to_string(),
            ));
        }
        maybe_save_trajectory_background_with_intent(
            app.clone(),
            session_arc.clone(),
            TrajectoryCommitIntent::Checkpoint,
        );
        {
            let mut session = session_arc.lock().await;
            session.add_message(ChatMessage::new("user".to_string(), "required".to_string()));
        }

        try_save_trajectory_with_intent(app, session_arc.clone(), TrajectoryCommitIntent::Required)
            .await
            .unwrap();
        let session = session_arc.lock().await;
        assert!(!session.trajectory_dirty);
        assert_eq!(
            session.trajectory_committed_version,
            session.trajectory_version
        );
        drop(session);
        let loaded = load_trajectory_for_chat(gcx, chat_id).await.unwrap();
        assert_eq!(loaded.messages.len(), 2);
    }

    #[serial(trajectory_perf)]
    #[tokio::test]
    async fn different_session_writers_commit_independently() {
        let workspace = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(workspace.path()).await;
        let left = Arc::new(AMutex::new(ChatSession::new("writer-left".to_string())));
        let right = Arc::new(AMutex::new(ChatSession::new("writer-right".to_string())));
        left.lock()
            .await
            .add_message(ChatMessage::new("user".to_string(), "left".to_string()));
        right
            .lock()
            .await
            .add_message(ChatMessage::new("user".to_string(), "right".to_string()));

        let (left_result, right_result) = tokio::join!(
            flush_trajectory_for_session(app.clone(), left),
            flush_trajectory_for_session(app, right),
        );
        left_result.unwrap();
        right_result.unwrap();
        assert_eq!(
            load_trajectory_for_chat(gcx.clone(), "writer-left")
                .await
                .unwrap()
                .messages[0]
                .content
                .content_text_only(),
            "left"
        );
        assert_eq!(
            load_trajectory_for_chat(gcx, "writer-right")
                .await
                .unwrap()
                .messages[0]
                .content
                .content_text_only(),
            "right"
        );
    }

    #[tokio::test]
    async fn trajectory_commit_finishes_before_blocked_vecdb_enqueue() {
        let workspace = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(workspace.path()).await;
        let enqueued = Arc::new(StdMutex::new(Vec::new()));
        let enqueue_started = Arc::new(Notify::new());
        let release_enqueue = Arc::new(Notify::new());
        *gcx.vec_db.lock().await = Some(Arc::new(BlockingRecordingVecdb {
            enqueued: enqueued.clone(),
            enqueue_started: enqueue_started.clone(),
            release_enqueue: release_enqueue.clone(),
        }));

        let mut save = tokio::spawn(save_trajectory_snapshot(
            gcx.clone(),
            test_snapshot(
                "commit-before-enqueue",
                "Committed",
                vec![ChatMessage::new("user".to_string(), "durable".to_string())],
            ),
        ));

        tokio::time::timeout(Duration::from_secs(60), enqueue_started.notified())
            .await
            .unwrap();
        assert!(load_trajectory_for_chat(gcx, "commit-before-enqueue")
            .await
            .is_some());
        tokio::time::timeout(Duration::from_millis(100), &mut save)
            .await
            .expect("required trajectory commit must not wait for VecDB enqueue")
            .unwrap()
            .unwrap();
        release_enqueue.notify_one();
    }

    #[tokio::test]
    async fn buddy_trajectory_save_is_not_enqueued_for_vectorization() {
        let workspace = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(workspace.path()).await;
        let enqueued = Arc::new(StdMutex::new(Vec::new()));
        let enqueue_started = Arc::new(Notify::new());
        let release_enqueue = Arc::new(Notify::new());
        *gcx.vec_db.lock().await = Some(Arc::new(BlockingRecordingVecdb {
            enqueued: enqueued.clone(),
            enqueue_started: enqueue_started.clone(),
            release_enqueue,
        }));
        let mut snapshot = test_snapshot(
            "buddy-not-vectorized",
            "Buddy",
            vec![ChatMessage::new("user".to_string(), "hello".to_string())],
        );
        snapshot.buddy_meta = Some(buddy_thread_meta());

        save_trajectory_snapshot(gcx, snapshot).await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(100), enqueue_started.notified())
                .await
                .is_err()
        );

        assert!(enqueued.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn internal_trace_save_is_not_enqueued_for_vectorization() {
        let workspace = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(workspace.path()).await;
        let enqueued = Arc::new(StdMutex::new(Vec::new()));
        let enqueue_started = Arc::new(Notify::new());
        let release_enqueue = Arc::new(Notify::new());
        *gcx.vec_db.lock().await = Some(Arc::new(BlockingRecordingVecdb {
            enqueued: enqueued.clone(),
            enqueue_started: enqueue_started.clone(),
            release_enqueue,
        }));
        let mut snapshot = test_snapshot(
            "internal-trace-not-vectorized",
            "Trace",
            vec![ChatMessage::new("user".to_string(), "hello".to_string())],
        );
        snapshot.link_type = Some(internal_trace_link_type("title_generation"));

        save_trajectory_snapshot(gcx, snapshot).await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(100), enqueue_started.notified())
                .await
                .is_err()
        );

        assert!(enqueued.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn vecdb_enqueue_failure_does_not_fail_trajectory_save() {
        let workspace = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(workspace.path()).await;
        let enqueue_started = Arc::new(Notify::new());
        *gcx.vec_db.lock().await = Some(Arc::new(PanickingVecdb {
            enqueue_started: enqueue_started.clone(),
        }));

        save_trajectory_snapshot(
            gcx.clone(),
            test_snapshot(
                "enqueue-failure",
                "Committed",
                vec![ChatMessage::new("user".to_string(), "durable".to_string())],
            ),
        )
        .await
        .unwrap();
        tokio::time::timeout(Duration::from_secs(60), enqueue_started.notified())
            .await
            .expect("best-effort VecDB enqueue should have run");
        assert!(load_trajectory_for_chat(gcx, "enqueue-failure")
            .await
            .is_some());
    }

    #[test]
    fn trajectory_rollout_switches_default_on_and_honor_explicit_false() {
        let defaults = crate::runtime_settings::TrajectoryRuntimeSettings::default();
        assert!(defaults.trajectory_writer_enabled);
        assert!(defaults.trajectory_index_coordinator_enabled);
        assert!(defaults.trajectory_watcher_self_write_enabled);
        assert!(defaults.tool_catalog_snapshots_enabled);
        assert!(defaults.vecdb_path_coalescing_enabled);
        assert!(trajectory_writer_rollout_enabled_for(
            defaults.trajectory_writer_enabled.then_some("1")
        ));
        assert!(trajectory_watcher_self_write_rollout_enabled_for(
            defaults
                .trajectory_watcher_self_write_enabled
                .then_some("1")
        ));
        assert!(trajectory_writer_rollout_enabled_for(Some("1")));
        assert!(trajectory_writer_rollout_enabled_for(Some("true")));
        assert!(!trajectory_writer_rollout_enabled_for(Some("0")));
        assert!(!trajectory_writer_rollout_enabled_for(Some(" false ")));
        assert!(!trajectory_writer_rollout_enabled_for(Some("OFF")));
        assert!(!trajectory_watcher_self_write_rollout_enabled_for(Some(
            "0"
        )));
        assert!(!trajectory_watcher_self_write_rollout_enabled_for(Some(
            "false"
        )));
        assert!(trajectory_watcher_self_write_rollout_enabled_for(Some(
            "on"
        )));
    }

    #[test]
    fn trajectory_writer_rollout_remains_restart_cached() {
        let writer_cache = OnceLock::new();
        assert!(rollout_switch_enabled(&writer_cache, || true));
        assert!(rollout_switch_enabled(&writer_cache, || false));
    }

    #[tokio::test]
    async fn detached_trajectory_writer_keys_preserve_workspace_boundaries() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let snapshot = test_snapshot("writer-key", "Writer key", Vec::new());
        {
            let mut folders = gcx.documents_state.workspace_folders.lock().unwrap();
            *folders = vec![PathBuf::from("alpha|beta"), PathBuf::from("gamma")];
        }
        let first = detached_trajectory_writer_key(&gcx, &snapshot);
        {
            let mut folders = gcx.documents_state.workspace_folders.lock().unwrap();
            *folders = vec![PathBuf::from("alpha"), PathBuf::from("beta|gamma")];
        }
        let second = detached_trajectory_writer_key(&gcx, &snapshot);
        assert_ne!(first, second);
    }

    #[test]
    fn trajectory_writer_ten_thousand_stale_interleavings_preserve_newer_dirty_state() {
        let mut session = ChatSession::new("trajectory-ten-thousand-stale".to_string());
        for _ in 0..10_000 {
            session.increment_version();
            let stale_version = session.trajectory_version;
            session.increment_version();
            let current_version = session.trajectory_version;
            assert!(session.complete_trajectory_commit(current_version));
            session.increment_version();
            assert!(!session.complete_trajectory_commit(stale_version));
            assert_eq!(session.trajectory_committed_version, current_version);
            assert!(session.trajectory_dirty);
            assert!(session.complete_trajectory_commit(session.trajectory_version));
            assert!(!session.trajectory_dirty);
        }
        assert_eq!(session.trajectory_committed_version, 30_000);
    }

    #[serial(trajectory_perf)]
    #[tokio::test]
    async fn trajectory_writer_failed_checkpoint_retries_before_a_required_commit() {
        let _lock = serial_test_guard();
        let _writer = TrajectoryWriterRolloutGuard::set(true);
        let workspace = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(workspace.path()).await;
        let chat_id = "checkpoint-retry-required";
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        let failing_message = ChatMessage {
            message_id: "checkpoint-failure".to_string(),
            role: "user".to_string(),
            content: ChatContent::SimpleText("first".to_string()),
            ..Default::default()
        };
        {
            let mut session = session_arc.lock().await;
            session.add_message(failing_message.clone());
        }
        set_test_message_serialization_failure(Some(failing_message.message_id));
        try_save_trajectory_with_intent(
            app.clone(),
            session_arc.clone(),
            TrajectoryCommitIntent::Checkpoint,
        )
        .await
        .unwrap();
        tokio::time::timeout(Duration::from_secs(60), async {
            loop {
                if session_arc.lock().await.trajectory_save_error.is_some() {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("checkpoint failure should reach the session");
        set_test_message_serialization_failure(None);

        {
            let mut session = session_arc.lock().await;
            session.add_message(ChatMessage::new("user".to_string(), "required".to_string()));
        }
        try_save_trajectory_with_intent(app, session_arc.clone(), TrajectoryCommitIntent::Required)
            .await
            .unwrap();

        let session = session_arc.lock().await;
        assert!(!session.trajectory_dirty);
        assert_eq!(
            session.trajectory_committed_version,
            session.trajectory_version
        );
        drop(session);
        let loaded = load_trajectory_for_chat(gcx, chat_id).await.unwrap();
        assert_eq!(loaded.messages.len(), 2);
    }

    #[tokio::test]
    async fn trajectory_writer_cache_preserves_external_metadata() {
        let workspace = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(workspace.path()).await;
        let chat_id = "detached-writer-cache";
        let first = test_snapshot(
            chat_id,
            "First",
            vec![ChatMessage::new("user".to_string(), "first".to_string())],
        );
        let write_cache = Arc::new(AMutex::new(DetachedTrajectoryWriteCache::default()));
        save_trajectory_snapshot_inner(gcx.clone(), first, Some(write_cache.clone()))
            .await
            .unwrap();
        let path = workspace
            .path()
            .join(".refact")
            .join("trajectories")
            .join(chat_id)
            .join(format!("{chat_id}.json"));
        let mut external: serde_json::Value =
            serde_json::from_str(&tokio::fs::read_to_string(&path).await.unwrap()).unwrap();
        external["external_metadata"] = json!({"preserved": true});
        tokio::fs::write(&path, serde_json::to_string_pretty(&external).unwrap())
            .await
            .unwrap();

        let second = test_snapshot(
            chat_id,
            "Second",
            vec![ChatMessage::new("user".to_string(), "second".to_string())],
        );
        save_trajectory_snapshot_inner(gcx, second, Some(write_cache))
            .await
            .unwrap();

        let saved: serde_json::Value =
            serde_json::from_str(&tokio::fs::read_to_string(path).await.unwrap()).unwrap();
        assert_eq!(saved["external_metadata"], json!({"preserved": true}));
    }

    #[serial(trajectory_perf)]
    #[tokio::test]
    async fn repeated_save_reuses_pretty_round_trip_metadata_without_a_second_disk_read() {
        let _lock = serial_test_guard();
        let workspace = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(workspace.path()).await;
        let chat_id = "metadata-cache-round-trip";
        let path = workspace
            .path()
            .join(".refact")
            .join("trajectories")
            .join(chat_id)
            .join(format!("{chat_id}.json"));
        tokio::fs::create_dir_all(path.parent().unwrap())
            .await
            .unwrap();
        tokio::fs::write(
            &path,
            serde_json::to_string_pretty(&json!({
                "id": chat_id,
                "title": "Before",
                "messages": [],
                "browser_meta": {
                    "browser_runtime_id": "browser-round-trip",
                    "tab_urls": ["https://example.com/round-trip"]
                },
                "custom_future_field": {"nested": {"value": 61}},
                "custom_scalar": "preserve-me"
            }))
            .unwrap(),
        )
        .await
        .unwrap();

        let reads_before = trajectory_metadata_disk_reads_for_chat(chat_id);
        for title in ["First", "Second"] {
            save_trajectory_snapshot(
                gcx.clone(),
                test_snapshot(
                    chat_id,
                    title,
                    vec![ChatMessage::new("user".to_string(), title.to_string())],
                ),
            )
            .await
            .unwrap();
        }
        let reads_after = trajectory_metadata_disk_reads_for_chat(chat_id);
        assert_eq!(
            reads_after - reads_before,
            1,
            "the second save must reuse the cached metadata instead of re-reading from disk"
        );

        let text = tokio::fs::read_to_string(&path).await.unwrap();
        let saved: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(text, serde_json::to_string_pretty(&saved).unwrap());
        assert!(text.contains("\n  \"browser_meta\": {"));
        assert_eq!(saved["title"], "Second");
        assert_eq!(
            saved["browser_meta"]["browser_runtime_id"],
            "browser-round-trip"
        );
        assert_eq!(saved["custom_future_field"]["nested"]["value"], 61);
        assert_eq!(saved["custom_scalar"], "preserve-me");

        let loaded = load_trajectory_for_chat(gcx, chat_id).await.unwrap();
        assert_eq!(loaded.messages.len(), 1);
        assert_eq!(loaded.messages[0].content.content_text_only(), "Second");
    }

    #[tokio::test]
    async fn trajectory_writer_cache_rejects_replaced_trajectory_for_another_chat() {
        let workspace = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(workspace.path()).await;
        let chat_id = "detached-writer-cache-replaced";
        let write_cache = Arc::new(AMutex::new(DetachedTrajectoryWriteCache::default()));
        save_trajectory_snapshot_inner(
            gcx.clone(),
            test_snapshot(
                chat_id,
                "First",
                vec![ChatMessage::new("user".to_string(), "first".to_string())],
            ),
            Some(write_cache.clone()),
        )
        .await
        .unwrap();
        let path = workspace
            .path()
            .join(".refact")
            .join("trajectories")
            .join(chat_id)
            .join(format!("{chat_id}.json"));
        let foreign = json!({
            "id": "other-chat",
            "title": "Foreign",
            "messages": [],
        });
        tokio::fs::write(&path, serde_json::to_string_pretty(&foreign).unwrap())
            .await
            .unwrap();

        let error = save_trajectory_snapshot_inner(
            gcx,
            test_snapshot(
                chat_id,
                "Second",
                vec![ChatMessage::new("user".to_string(), "second".to_string())],
            ),
            Some(write_cache),
        )
        .await
        .unwrap_err();
        assert!(is_trajectory_id_mismatch_error(&error));
        let saved: serde_json::Value =
            serde_json::from_str(&tokio::fs::read_to_string(path).await.unwrap()).unwrap();
        assert_eq!(saved["id"], "other-chat");
    }

    #[serial(trajectory_perf)]
    #[tokio::test]
    async fn trajectory_writer_failure_is_stored_and_reported_to_waiters() {
        let workspace = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(workspace.path()).await;
        let mut snapshot = test_snapshot(
            "detached-writer-failure",
            "Failure",
            vec![ChatMessage::new("user".to_string(), "failure".to_string())],
        );
        snapshot.messages[0].message_id = "detached-writer-failure-message".to_string();
        let writer = Arc::new(AMutex::new(DetachedTrajectoryWriterState {
            pending: Some((1, snapshot.clone())),
            requested_version: 1,
            in_flight: true,
            ..Default::default()
        }));
        set_test_message_serialization_failure(Some(snapshot.messages[0].message_id.clone()));
        run_detached_trajectory_writer(gcx, writer.clone()).await;
        set_test_message_serialization_failure(None);

        let error = wait_for_detached_trajectory_commit(writer.clone(), 1)
            .await
            .unwrap_err();
        assert!(error.contains("test failure"));
        assert_eq!(writer.lock().await.error.as_deref(), Some(error.as_str()));
    }

    #[serial(trajectory_perf)]
    #[serial(trajectory_perf)]
    #[tokio::test]
    async fn detached_trajectory_writer_drains_newer_snapshot_after_a_failed_write() {
        let _lock = serial_test_guard();
        let workspace = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(workspace.path()).await;
        let chat_id = "detached-writer-drains-newer";
        let mut failed = test_snapshot(
            chat_id,
            "Failed",
            vec![ChatMessage::new("user".to_string(), "failed".to_string())],
        );
        failed.messages[0].message_id = "detached-writer-drains-newer-failed".to_string();
        let newer = test_snapshot(
            chat_id,
            "Newer",
            vec![ChatMessage::new("user".to_string(), "newer".to_string())],
        );
        let writer = Arc::new(AMutex::new(DetachedTrajectoryWriterState {
            pending: Some((1, failed.clone())),
            requested_version: 1,
            in_flight: true,
            ..Default::default()
        }));
        let failure_seen = Arc::new(Notify::new());
        let release_failure = Arc::new(Notify::new());
        set_test_detached_trajectory_writer_failure_hook(Some((
            chat_id.to_string(),
            failure_seen.clone(),
            release_failure.clone(),
        )));
        set_test_message_serialization_failure(Some(failed.messages[0].message_id.clone()));
        let failure_seen_wait = failure_seen.notified();
        let task = tokio::spawn(run_detached_trajectory_writer(gcx.clone(), writer.clone()));

        tokio::time::timeout(Duration::from_secs(5), failure_seen_wait)
            .await
            .expect("failed write should pause before recording its outcome");
        set_test_message_serialization_failure(None);
        {
            let mut state = writer.lock().await;
            state.requested_version = 2;
            state.pending = Some((2, newer));
        }
        release_failure.notify_one();
        tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .expect("writer should drain the newer snapshot")
            .unwrap();
        set_test_detached_trajectory_writer_failure_hook(None);

        let state = writer.lock().await;
        assert_eq!(state.committed_version, 2);
        assert_eq!(state.error, None);
        assert!(!state.in_flight);
        drop(state);
        let loaded = load_trajectory_for_chat(gcx, chat_id).await.unwrap();
        assert_eq!(loaded.messages[0].content.content_text_only(), "newer");
    }

    #[serial(trajectory_perf)]
    #[tokio::test]
    async fn trajectory_writer_registry_cleans_completed_and_source_isolated_entries() {
        let _lock = serial_test_guard();
        let _writer = TrajectoryWriterRolloutGuard::set(true);
        let workspace = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(workspace.path()).await;
        let chat_id = "detached-writer-source-isolation";
        let normal = test_snapshot(
            chat_id,
            "Normal",
            vec![ChatMessage::new("user".to_string(), "normal".to_string())],
        );
        let mut buddy = test_snapshot(
            chat_id,
            "Buddy",
            vec![ChatMessage::new("user".to_string(), "buddy".to_string())],
        );
        buddy.mode = "buddy".to_string();
        buddy.buddy_meta = Some(buddy_thread_meta());
        let normal_key = detached_trajectory_writer_key(&gcx, &normal);
        let buddy_key = detached_trajectory_writer_key(&gcx, &buddy);

        let (normal_result, buddy_result) = tokio::join!(
            save_trajectory_snapshot(gcx.clone(), normal),
            save_trajectory_snapshot(gcx.clone(), buddy),
        );
        normal_result.unwrap();
        buddy_result.unwrap();
        assert_ne!(normal_key, buddy_key);
        prune_detached_trajectory_writers();
        let writers = detached_trajectory_writers()
            .lock()
            .expect("detached trajectory writer registry poisoned");
        assert!(!writers.contains_key(&normal_key));
        assert!(!writers.contains_key(&buddy_key));
        drop(writers);

        let normal = load_trajectory_for_chat(gcx.clone(), chat_id)
            .await
            .unwrap();
        assert_eq!(normal.thread.title, "Normal");
        let buddy_path = workspace
            .path()
            .join(".refact")
            .join("buddy")
            .join("chats")
            .join("conversations")
            .join(format!("{chat_id}.json"));
        let buddy: serde_json::Value =
            serde_json::from_str(&tokio::fs::read_to_string(buddy_path).await.unwrap()).unwrap();
        assert_eq!(buddy["title"], "Buddy");
    }

    #[tokio::test]
    async fn trajectory_writer_shutdown_flush_is_bounded_and_reports_blocked_session() {
        let workspace = tempfile::tempdir().unwrap();
        let (_gcx, app) = make_app_with_workspace(workspace.path()).await;
        let session_arc = Arc::new(AMutex::new(ChatSession::new("flush-timeout".to_string())));
        session_arc
            .lock()
            .await
            .add_message(ChatMessage::new("user".to_string(), "pending".to_string()));
        app.chat
            .sessions
            .write()
            .await
            .insert("flush-timeout".to_string(), session_arc.clone());

        let guard = session_arc.lock().await;
        let errors = flush_all_trajectories_with_timeout(app, Duration::from_millis(10))
            .await
            .unwrap_err();
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("timed out"));
        drop(guard);
    }

    #[tokio::test]
    async fn prune_buddy_conversations_keeps_newest_and_updates_index() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("conversations");
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let old_a = write_conversation_file(&dir, "old-a", 4 * 86_400).await;
        let old_b = write_conversation_file(&dir, "old-b", 3 * 86_400).await;
        let new_a = write_conversation_file(&dir, "new-a", 2000).await;
        let new_b = write_conversation_file(&dir, "new-b", 1000).await;
        let index_path = dir.join("index.json");
        let index_entries = ["old-a", "old-b", "new-a", "new-b"]
            .iter()
            .map(|id| {
                serde_json::json!({
                    "id": id,
                    "file_name": format!("{id}.json"),
                    "created_at": "2026-01-01T00:00:00Z",
                    "updated_at": "2026-01-01T00:00:00Z",
                    "title": id,
                    "model": "test-model",
                    "mode": "AGENT",
                    "message_count": 1
                })
            })
            .collect::<Vec<_>>();
        tokio::fs::write(
            &index_path,
            serde_json::json!({
                "schema_version": 1,
                "updated_at": "2026-01-01T00:00:00Z",
                "entries": index_entries
            })
            .to_string(),
        )
        .await
        .unwrap();

        let removed = prune_buddy_conversations_in_dir(&dir, 2).await.unwrap();

        assert_eq!(removed, 2);
        assert!(!old_a.exists());
        assert!(!old_b.exists());
        assert!(new_a.exists());
        assert!(new_b.exists());
        assert!(index_path.exists());
        let index = trajectory_index::read_trajectory_index(&dir)
            .await
            .unwrap()
            .unwrap();
        let ids: Vec<&str> = index
            .entries
            .iter()
            .map(|entry| entry.id.as_str())
            .collect();
        assert_eq!(ids.len(), 2);
        assert!(ids.contains(&"new-a"));
        assert!(ids.contains(&"new-b"));
    }

