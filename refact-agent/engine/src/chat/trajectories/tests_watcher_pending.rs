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
    async fn normal_save_under_session_lock_does_not_reenter_or_leak_runtime() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;
        let mut rx = app.chat.trajectory_events_tx.subscribe();
        let id = "locked-snapshot-publication";
        let session = Arc::new(AMutex::new(ChatSession::new(id.into())));
        app.chat
            .sessions
            .write()
            .await
            .insert(id.into(), session.clone());
        let mut locked = session.lock().await;
        locked.runtime.state = SessionState::Generating;
        locked.runtime.error = Some("not snapshot runtime".into());
        tokio::time::timeout(
            std::time::Duration::from_secs(10),
            save_trajectory_snapshot(
                gcx,
                test_snapshot(
                    id,
                    "Snapshot",
                    vec![ChatMessage::new("user".into(), "keep".into())],
                ),
            ),
        )
        .await
        .expect("saving must not reacquire the held session lock")
        .unwrap();
        let event = wait_for_trajectory_event(&mut rx, id).await;
        assert_eq!(event.session_state, None);
        assert_eq!(event.error, None);
        drop(locked);
    }

    #[tokio::test]
    async fn normal_save_sse_keeps_matching_active_normal_state_error_and_worktree() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;
        let mut rx = app.chat.trajectory_events_tx.subscribe();
        let chat_id = "normal-save-active-normal-enriched";
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        {
            let mut session = session_arc.lock().await;
            session.thread.worktree = Some(trajectory_worktree_sample_with_id(
                "normal-save-enriched-wt",
            ));
            session.runtime.state = SessionState::Generating;
            session.runtime.error = Some("normal save enriched error".to_string());
        }
        app.chat
            .sessions
            .write()
            .await
            .insert(chat_id.to_string(), session_arc);

        save_trajectory_snapshot(
            gcx,
            test_snapshot(
                chat_id,
                "Normal Save Enriched",
                vec![ChatMessage::new(
                    "user".to_string(),
                    "normal save".to_string(),
                )],
            ),
        )
        .await
        .unwrap();

        let event = wait_for_trajectory_event(&mut rx, chat_id).await;
        assert_eq!(event.event_type, "updated");
        assert_eq!(event.session_state.as_deref(), Some("generating"));
        assert_eq!(event.error.as_deref(), Some("normal save enriched error"));
        assert_eq!(
            event.worktree.as_ref().map(|worktree| worktree.id.as_str()),
            Some("normal-save-enriched-wt")
        );
    }

    #[tokio::test]
    async fn task_http_save_sse_ignores_active_normal_state_error_and_worktree() {
        let dir = tempfile::tempdir().unwrap();
        let (_gcx, app) = make_app_with_workspace(dir.path()).await;
        let mut rx = app.chat.trajectory_events_tx.subscribe();
        let chat_id = "task-http-save-active-normal-clean";
        let task_id = "task-http-save-active-normal";
        tokio::fs::create_dir_all(dir.path().join(".refact").join("tasks").join(task_id))
            .await
            .unwrap();
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        {
            let mut session = session_arc.lock().await;
            session.thread.worktree = Some(trajectory_worktree_sample_with_id("normal-save-wt"));
            session.runtime.state = SessionState::Generating;
            session.runtime.error = Some("normal save error".to_string());
        }
        app.chat
            .sessions
            .write()
            .await
            .insert(chat_id.to_string(), session_arc);
        let mut payload = sample_trajectory(chat_id, "Task Http Save", "2024-01-01T00:00:01Z");
        payload["task_meta"] = json!({
            "task_id": task_id,
            "role": "agents",
            "agent_id": "agent-http-save",
            "card_id": "card-http-save"
        });

        handle_v1_trajectories_save(
            State(app),
            AxumPath(chat_id.to_string()),
            hyper::body::Bytes::from(serde_json::to_vec(&payload).unwrap()),
        )
        .await
        .unwrap();

        let event = wait_for_trajectory_event(&mut rx, chat_id).await;
        assert_eq!(event.event_type, "created");
        assert_eq!(event.task_id.as_deref(), Some(task_id));
        assert_eq!(event.session_state.as_deref(), Some("idle"));
        assert_eq!(event.error, None);
        assert!(event.worktree.is_none());
    }

    #[tokio::test]
    async fn task_http_save_sse_keeps_matching_active_task_state_error_and_worktree() {
        let dir = tempfile::tempdir().unwrap();
        let (_gcx, app) = make_app_with_workspace(dir.path()).await;
        let mut rx = app.chat.trajectory_events_tx.subscribe();
        let chat_id = "task-http-save-active-task-enriched";
        let task_id = "task-http-save-active-task";
        tokio::fs::create_dir_all(dir.path().join(".refact").join("tasks").join(task_id))
            .await
            .unwrap();
        let task_meta = task_meta(
            task_id,
            "agents",
            Some("agent-http-save-enriched"),
            Some("card-http-save-enriched"),
            None,
        );
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        {
            let mut session = session_arc.lock().await;
            session.thread.task_meta = Some(task_meta);
            session.thread.worktree =
                Some(trajectory_worktree_sample_with_id("task-save-enriched-wt"));
            session.runtime.state = SessionState::Generating;
            session.runtime.error = Some("task save enriched error".to_string());
        }
        app.chat
            .sessions
            .write()
            .await
            .insert(chat_id.to_string(), session_arc);
        let mut payload =
            sample_trajectory(chat_id, "Task Http Save Enriched", "2024-01-01T00:00:01Z");
        payload["task_meta"] = json!({
            "task_id": task_id,
            "role": "agents",
            "agent_id": "agent-http-save-enriched",
            "card_id": "card-http-save-enriched"
        });

        handle_v1_trajectories_save(
            State(app),
            AxumPath(chat_id.to_string()),
            hyper::body::Bytes::from(serde_json::to_vec(&payload).unwrap()),
        )
        .await
        .unwrap();

        let event = wait_for_trajectory_event(&mut rx, chat_id).await;
        assert_eq!(event.event_type, "created");
        assert_eq!(event.task_id.as_deref(), Some(task_id));
        assert_eq!(event.session_state.as_deref(), Some("generating"));
        assert_eq!(event.error.as_deref(), Some("task save enriched error"));
        assert_eq!(
            event.worktree.as_ref().map(|worktree| worktree.id.as_str()),
            Some("task-save-enriched-wt")
        );
    }

    #[tokio::test]
    async fn task_update_with_active_normal_same_id_emits_update_without_session_mutation() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;
        let mut rx = app.chat.trajectory_events_tx.subscribe();
        let chat_id = "task-update-active-normal-collision";
        let task_meta = task_meta(
            "task-update-normal-collision",
            "agents",
            Some("agent-1"),
            Some("card-1"),
            None,
        );
        let task_path = dir
            .path()
            .join(".refact")
            .join("tasks")
            .join("task-update-normal-collision")
            .join("trajectories")
            .join("agents")
            .join("agent-1")
            .join(format!("{chat_id}.json"));
        write_task_trajectory_file_with_user_message(
            &task_path,
            chat_id,
            "Task Updated",
            "task replacement message",
            &task_meta,
        )
        .await;
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        let mut chat_rx = {
            let mut session = session_arc.lock().await;
            session.thread.title = "Active Normal".to_string();
            session.messages.push(ChatMessage::new(
                "user".to_string(),
                "keep active normal".to_string(),
            ));
            session.runtime.state = SessionState::Generating;
            session.trajectory_dirty = true;
            session.subscribe()
        };
        app.chat
            .sessions
            .write()
            .await
            .insert(chat_id.to_string(), session_arc.clone());

        process_trajectory_change_for_source(
            &gcx.chat_ctx(gcx.clone()),
            chat_id,
            false,
            Some(TrajectorySourceIdentity::from_task_meta(&task_meta)),
        )
        .await;

        let event = wait_for_trajectory_event(&mut rx, chat_id).await;
        assert_eq!(event.event_type, "updated");
        assert_eq!(event.title.as_deref(), Some("Task Updated"));
        assert_eq!(
            event.task_id.as_deref(),
            Some("task-update-normal-collision")
        );
        assert_eq!(event.session_state.as_deref(), Some("idle"));

        let session = session_arc.lock().await;
        assert_eq!(session.external_reload_pending, None);
        assert_eq!(session.thread.title, "Active Normal");
        assert!(session.thread.task_meta.is_none());
        assert_eq!(session.messages.len(), 1);
        assert_eq!(
            session.messages[0].content.content_text_only(),
            "keep active normal"
        );
        drop(session);
        assert_no_chat_event_for(&mut chat_rx, std::time::Duration::from_millis(100)).await;
    }

    #[tokio::test]
    async fn normal_update_with_active_task_same_id_emits_update_without_session_mutation() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;
        let mut rx = app.chat.trajectory_events_tx.subscribe();
        let chat_id = "normal-update-active-task-collision";
        let normal_path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        write_trajectory_file_with_user_message(
            &normal_path,
            chat_id,
            "Normal Updated",
            "normal replacement message",
        )
        .await;
        let active_task_meta = task_meta(
            "task-active-normal-update-collision",
            "agents",
            Some("agent-2"),
            Some("card-2"),
            None,
        );
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        let mut chat_rx = {
            let mut session = session_arc.lock().await;
            session.thread.title = "Active Task".to_string();
            session.thread.task_meta = Some(active_task_meta.clone());
            session.messages.push(ChatMessage::new(
                "user".to_string(),
                "keep active task".to_string(),
            ));
            session.runtime.state = SessionState::Generating;
            session.trajectory_dirty = true;
            session.subscribe()
        };
        app.chat
            .sessions
            .write()
            .await
            .insert(chat_id.to_string(), session_arc.clone());

        process_trajectory_change_for_source(
            &gcx.chat_ctx(gcx.clone()),
            chat_id,
            false,
            Some(TrajectorySourceIdentity::Normal),
        )
        .await;

        let event = wait_for_trajectory_event(&mut rx, chat_id).await;
        assert_eq!(event.event_type, "updated");
        assert_eq!(event.title.as_deref(), Some("Normal Updated"));
        assert_eq!(event.task_id, None);
        assert_eq!(event.session_state.as_deref(), Some("idle"));

        let session = session_arc.lock().await;
        assert_eq!(session.external_reload_pending, None);
        assert_eq!(session.thread.title, "Active Task");
        assert_eq!(session.thread.task_meta, Some(active_task_meta));
        assert_eq!(session.messages.len(), 1);
        assert_eq!(
            session.messages[0].content.content_text_only(),
            "keep active task"
        );
        drop(session);
        assert_no_chat_event_for(&mut chat_rx, std::time::Duration::from_millis(100)).await;
    }

    #[tokio::test]
    async fn task_path_hint_update_with_normal_same_id_loads_task_without_session_mutation() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;
        let mut rx = app.chat.trajectory_events_tx.subscribe();
        let chat_id = "task-path-hint-update-normal-collision";
        let normal_path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        write_trajectory_file_with_user_message(
            &normal_path,
            chat_id,
            "Normal Same Id",
            "normal must not be loaded",
        )
        .await;
        let task_meta = task_meta(
            "task-path-hint-update",
            "agents",
            Some("agent-path-hint"),
            Some("card-path-hint"),
            None,
        );
        let task_path = dir
            .path()
            .join(".refact")
            .join("tasks")
            .join("task-path-hint-update")
            .join("trajectories")
            .join("agents")
            .join("agent-path-hint")
            .join(format!("{chat_id}.json"));
        write_task_trajectory_file_with_user_message(
            &task_path,
            chat_id,
            "Task Path Hint Updated",
            "task path hint replacement",
            &task_meta,
        )
        .await;
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        let mut chat_rx = {
            let mut session = session_arc.lock().await;
            session.thread.title = "Active Normal Path Hint".to_string();
            session.messages.push(ChatMessage::new(
                "user".to_string(),
                "keep active normal path hint".to_string(),
            ));
            session.runtime.state = SessionState::Generating;
            session.trajectory_dirty = true;
            session.subscribe()
        };
        app.chat
            .sessions
            .write()
            .await
            .insert(chat_id.to_string(), session_arc.clone());

        process_trajectory_change_for_source(
            &gcx.chat_ctx(gcx.clone()),
            chat_id,
            false,
            Some(trajectory_source_identity_from_path(
                &task_path,
                &get_all_task_roots(&gcx.chat_ctx(gcx.clone())).await,
            )),
        )
        .await;

        let event = wait_for_trajectory_event(&mut rx, chat_id).await;
        assert_eq!(event.event_type, "updated");
        assert_eq!(event.title.as_deref(), Some("Task Path Hint Updated"));
        assert_eq!(event.task_id.as_deref(), Some("task-path-hint-update"));
        assert_eq!(event.task_role.as_deref(), Some("agents"));
        assert_eq!(event.agent_id.as_deref(), Some("agent-path-hint"));
        assert_eq!(event.card_id.as_deref(), Some("card-path-hint"));
        assert_eq!(event.session_state.as_deref(), Some("idle"));

        let session = session_arc.lock().await;
        assert_eq!(session.external_reload_pending, None);
        assert_eq!(session.thread.title, "Active Normal Path Hint");
        assert!(session.thread.task_meta.is_none());
        assert_eq!(session.messages.len(), 1);
        assert_eq!(
            session.messages[0].content.content_text_only(),
            "keep active normal path hint"
        );
        drop(session);
        assert_no_chat_event_for(&mut chat_rx, std::time::Duration::from_millis(100)).await;
    }

    #[tokio::test]
    async fn planner_chat_id_path_hint_does_not_use_file_stem() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _app) = make_app_with_workspace(dir.path()).await;
        let task_path = dir
            .path()
            .join(".refact")
            .join("tasks")
            .join("task-path-hint-planner")
            .join("trajectories")
            .join("planner")
            .join("misleading-file-stem.json");
        tokio::fs::create_dir_all(task_path.parent().unwrap())
            .await
            .unwrap();
        tokio::fs::write(&task_path, "{}").await.unwrap();

        let source =
            trajectory_source_identity_from_path(&task_path, &get_all_task_roots(&gcx.chat_ctx(gcx.clone())).await);

        assert_eq!(
            source,
            TrajectorySourceIdentity::task(
                "task-path-hint-planner".to_string(),
                "planner".to_string(),
                None,
                None,
                None,
            )
        );
    }

    #[tokio::test]
    async fn planner_chat_id_verified_root_requires_planner_for_same_task() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _app) = make_app_with_workspace(dir.path()).await;
        let child_meta = task_meta(
            "task-root-check",
            "subchats",
            None,
            None,
            Some("stale-planner"),
        );
        let planner_path = dir
            .path()
            .join(".refact")
            .join("tasks")
            .join("task-root-check")
            .join("trajectories")
            .join("planner")
            .join("planner-root-check.json");
        write_task_trajectory_file_with_user_message(
            &planner_path,
            "planner-root-check",
            "Planner Root Check",
            "planner root",
            &task_meta(
                "task-root-check",
                "planner",
                None,
                None,
                Some("planner-root-check"),
            ),
        )
        .await;

        assert_eq!(
            verified_planner_linked_root_chat_id(&gcx.chat_ctx(gcx.clone()),
                "child-root-check",
                Some("normal-root"),
                &child_meta,
            )
            .await,
            None
        );
        assert_eq!(
            verified_planner_linked_root_chat_id(&gcx.chat_ctx(gcx.clone()),
                "child-root-check",
                Some("planner-root-check"),
                &child_meta,
            )
            .await
            .as_deref(),
            Some("planner-root-check")
        );
    }

    #[tokio::test]
    async fn normal_path_update_with_task_same_id_loads_normal_without_session_mutation() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;
        let mut rx = app.chat.trajectory_events_tx.subscribe();
        let chat_id = "normal-path-update-task-collision";
        let task_meta = task_meta(
            "task-normal-path-update",
            "agents",
            Some("agent-normal-path"),
            Some("card-normal-path"),
            None,
        );
        let task_path = dir
            .path()
            .join(".refact")
            .join("tasks")
            .join("task-normal-path-update")
            .join("trajectories")
            .join("agents")
            .join("agent-normal-path")
            .join(format!("{chat_id}.json"));
        write_task_trajectory_file_with_user_message(
            &task_path,
            chat_id,
            "Task Same Id",
            "task must not be loaded",
            &task_meta,
        )
        .await;
        let normal_path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        write_trajectory_file_with_user_message(
            &normal_path,
            chat_id,
            "Normal Path Updated",
            "normal path replacement",
        )
        .await;
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        let mut chat_rx = {
            let mut session = session_arc.lock().await;
            session.thread.title = "Active Task Path Hint".to_string();
            session.thread.task_meta = Some(task_meta.clone());
            session.messages.push(ChatMessage::new(
                "user".to_string(),
                "keep active task path hint".to_string(),
            ));
            session.runtime.state = SessionState::Generating;
            session.trajectory_dirty = true;
            session.subscribe()
        };
        app.chat
            .sessions
            .write()
            .await
            .insert(chat_id.to_string(), session_arc.clone());

        process_trajectory_change_for_source(
            &gcx.chat_ctx(gcx.clone()),
            chat_id,
            false,
            Some(TrajectorySourceIdentity::Normal),
        )
        .await;

        let event = wait_for_trajectory_event(&mut rx, chat_id).await;
        assert_eq!(event.event_type, "updated");
        assert_eq!(event.title.as_deref(), Some("Normal Path Updated"));
        assert_eq!(event.task_id, None);
        assert_eq!(event.session_state.as_deref(), Some("idle"));

        let session = session_arc.lock().await;
        assert_eq!(session.external_reload_pending, None);
        assert_eq!(session.thread.title, "Active Task Path Hint");
        assert_eq!(session.thread.task_meta, Some(task_meta));
        assert_eq!(session.messages.len(), 1);
        assert_eq!(
            session.messages[0].content.content_text_only(),
            "keep active task path hint"
        );
        drop(session);
        assert_no_chat_event_for(&mut chat_rx, std::time::Duration::from_millis(100)).await;
    }

    #[tokio::test]
    async fn task_scan_collects_path_derived_task_source() {
        let dir = tempfile::tempdir().unwrap();
        let (_gcx, _) = make_app_with_workspace(dir.path()).await;
        let task_roots = vec![dir.path().join(".refact").join("tasks")];
        let chat_id = "scan-task-source";
        let path = task_roots[0]
            .join("task-scan-source")
            .join("trajectories")
            .join("agents")
            .join("agent-scan-source")
            .join(format!("{chat_id}.json"));
        write_trajectory_file(&path, chat_id, "Scan Task", "2024-01-01T00:00:01Z").await;

        let mut sources =
            collect_task_trajectory_sources_under_path(&task_roots[0], &task_roots).await;
        sources.sort_by(|left, right| left.0.cmp(&right.0));

        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0].0, chat_id);
        assert_eq!(
            sources[0].1,
            TrajectorySourceIdentity::task(
                "task-scan-source".to_string(),
                "agents".to_string(),
                Some("agent-scan-source".to_string()),
                None,
                None,
            )
        );
        assert_eq!(sources[0].2, path);
    }

    #[test]
    fn watcher_overflow_schedules_recovery_scans_and_resets_drop_counter() {
        let normal_root = PathBuf::from("normal-trajectories");
        let task_root = PathBuf::from("task-trajectories");
        let dropped_events = AtomicUsize::new(3);
        let mut pending_scans = std::collections::HashMap::new();

        schedule_trajectory_watcher_overflow_recovery(
            &mut pending_scans,
            &dropped_events,
            &[normal_root.clone(), task_root.clone()],
        );

        assert_eq!(dropped_events.load(Ordering::Relaxed), 0);
        assert_eq!(pending_scans.len(), 2);
        assert!(pending_scans.contains_key(&normal_root));
        assert!(pending_scans.contains_key(&task_root));
    }

    #[test]
    fn watcher_pending_debounce_keys_by_chat_and_source() {
        let chat_id = "debounce-source-collision".to_string();
        let task_source = TrajectorySourceIdentity::task(
            "task-debounce".to_string(),
            "agents".to_string(),
            Some("agent-debounce".to_string()),
            None,
            None,
        );
        let mut pending = TrajectoryPendingMap::new();
        let now = Instant::now();

        insert_pending_trajectory_change(
            &mut pending,
            chat_id.clone(),
            false,
            TrajectorySourceIdentity::Normal,
            PathBuf::from("normal.json"),
            now,
        );
        insert_pending_trajectory_change(
            &mut pending,
            chat_id.clone(),
            true,
            task_source.clone(),
            PathBuf::from("task.json"),
            now,
        );

        assert_eq!(pending.len(), 2);
        let normal = pending
            .get(&(chat_id.clone(), TrajectorySourceIdentity::Normal))
            .unwrap();
        assert_eq!(normal.last_event_at, now);
        assert!(!normal.is_remove);
        assert_eq!(normal.path, PathBuf::from("normal.json"));
        let task = pending.get(&(chat_id, task_source)).unwrap();
        assert_eq!(task.last_event_at, now);
        assert!(task.is_remove);
        assert_eq!(task.path, PathBuf::from("task.json"));
    }

    #[tokio::test]
    async fn busy_active_task_matching_update_stores_task_pending_source_with_normal_collision() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;
        let chat_id = "busy-task-update-pending-source";
        let normal_path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        write_trajectory_file_with_user_message(
            &normal_path,
            chat_id,
            "Normal Same Id",
            "normal same-id collision",
        )
        .await;
        let task_meta = task_meta(
            "task-pending-source",
            "agents",
            Some("agent-pending-source"),
            Some("card-pending-source"),
            None,
        );
        let task_path = dir
            .path()
            .join(".refact")
            .join("tasks")
            .join("task-pending-source")
            .join("trajectories")
            .join("agents")
            .join("agent-pending-source")
            .join(format!("{chat_id}.json"));
        write_task_trajectory_file_with_user_message(
            &task_path,
            chat_id,
            "Task Same Id",
            "task same-id update",
            &task_meta,
        )
        .await;
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        {
            let mut session = session_arc.lock().await;
            session.thread.task_meta = Some(task_meta.clone());
            session.runtime.state = SessionState::Generating;
            session.trajectory_dirty = true;
        }
        app.chat
            .sessions
            .write()
            .await
            .insert(chat_id.to_string(), session_arc.clone());

        process_trajectory_change_for_source(
            &gcx.chat_ctx(gcx.clone()),
            chat_id,
            false,
            Some(TrajectorySourceIdentity::from_task_meta(&task_meta)),
        )
        .await;

        let session = session_arc.lock().await;
        assert_eq!(
            session.external_reload_pending,
            Some(ExternalReloadPending::update(
                TrajectorySourceIdentity::from_task_meta(&task_meta)
            ))
        );
        assert_eq!(session.thread.task_meta, Some(task_meta));
    }

    #[tokio::test]
    async fn busy_active_normal_matching_update_stores_normal_pending_source_with_task_collision() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;
        let chat_id = "busy-normal-update-pending-source";
        let task_meta = task_meta(
            "task-normal-pending-source",
            "agents",
            Some("agent-normal-pending"),
            Some("card-normal-pending"),
            None,
        );
        let task_path = dir
            .path()
            .join(".refact")
            .join("tasks")
            .join("task-normal-pending-source")
            .join("trajectories")
            .join("agents")
            .join("agent-normal-pending")
            .join(format!("{chat_id}.json"));
        write_task_trajectory_file_with_user_message(
            &task_path,
            chat_id,
            "Task Same Id",
            "task same-id collision",
            &task_meta,
        )
        .await;
        let normal_path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        write_trajectory_file_with_user_message(
            &normal_path,
            chat_id,
            "Normal Same Id",
            "normal same-id update",
        )
        .await;
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        {
            let mut session = session_arc.lock().await;
            session.runtime.state = SessionState::Generating;
            session.trajectory_dirty = true;
        }
        app.chat
            .sessions
            .write()
            .await
            .insert(chat_id.to_string(), session_arc.clone());

        process_trajectory_change_for_source(
            &gcx.chat_ctx(gcx.clone()),
            chat_id,
            false,
            Some(TrajectorySourceIdentity::Normal),
        )
        .await;

        let session = session_arc.lock().await;
        assert_eq!(
            session.external_reload_pending,
            Some(ExternalReloadPending::update(
                TrajectorySourceIdentity::Normal
            ))
        );
        assert!(session.thread.task_meta.is_none());
    }

    #[tokio::test]
    async fn pending_task_update_applies_task_despite_normal_priority() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let chat_id = "pending-task-update-normal-priority";
        let normal_path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        write_trajectory_file_with_user_message(
            &normal_path,
            chat_id,
            "Normal Higher Priority",
            "normal must not apply",
        )
        .await;
        let task_meta = task_meta(
            "task-pending-update-apply",
            "agents",
            Some("agent-pending-apply"),
            Some("card-pending-apply"),
            None,
        );
        let task_path = dir
            .path()
            .join(".refact")
            .join("tasks")
            .join("task-pending-update-apply")
            .join("trajectories")
            .join("agents")
            .join("agent-pending-apply")
            .join(format!("{chat_id}.json"));
        write_task_trajectory_file_with_user_message(
            &task_path,
            chat_id,
            "Task Pending Applied",
            "task pending applied",
            &task_meta,
        )
        .await;
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        {
            let mut session = session_arc.lock().await;
            session.thread.task_meta = Some(task_meta.clone());
            session.thread.title = "Busy Task Before Pending Apply".to_string();
            session.messages.push(ChatMessage::new(
                "user".to_string(),
                "old busy task".to_string(),
            ));
            session.external_reload_pending = Some(ExternalReloadPending::update(
                TrajectorySourceIdentity::from_task_meta(&task_meta),
            ));
            session.runtime.state = SessionState::Idle;
            session.trajectory_dirty = false;
        }

        check_external_reload_pending(&gcx.chat_ctx(gcx.clone()), session_arc.clone()).await;

        let session = session_arc.lock().await;
        assert_eq!(session.external_reload_pending, None);
        assert_eq!(session.thread.title, "Task Pending Applied");
        assert_eq!(session.thread.task_meta, Some(task_meta));
        assert_eq!(session.messages.len(), 1);
        assert_eq!(
            session.messages[0].content.content_text_only(),
            "task pending applied"
        );
    }

    #[tokio::test]
    async fn pending_normal_update_applies_normal_despite_task_collision() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let chat_id = "pending-normal-update-task-collision";
        let task_meta = task_meta(
            "task-pending-normal-collision",
            "agents",
            Some("agent-pending-normal"),
            Some("card-pending-normal"),
            None,
        );
        let task_path = dir
            .path()
            .join(".refact")
            .join("tasks")
            .join("task-pending-normal-collision")
            .join("trajectories")
            .join("agents")
            .join("agent-pending-normal")
            .join(format!("{chat_id}.json"));
        write_task_trajectory_file_with_user_message(
            &task_path,
            chat_id,
            "Task Same Id",
            "task must not apply",
            &task_meta,
        )
        .await;
        let normal_path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        write_trajectory_file_with_user_message(
            &normal_path,
            chat_id,
            "Normal Pending Applied",
            "normal pending applied",
        )
        .await;
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        {
            let mut session = session_arc.lock().await;
            session.thread.title = "Busy Normal Before Pending Apply".to_string();
            session.messages.push(ChatMessage::new(
                "user".to_string(),
                "old busy normal".to_string(),
            ));
            session.external_reload_pending = Some(ExternalReloadPending::update(
                TrajectorySourceIdentity::Normal,
            ));
            session.runtime.state = SessionState::Idle;
            session.trajectory_dirty = false;
        }

        check_external_reload_pending(&gcx.chat_ctx(gcx.clone()), session_arc.clone()).await;

        let session = session_arc.lock().await;
        assert_eq!(session.external_reload_pending, None);
        assert_eq!(session.thread.title, "Normal Pending Applied");
        assert!(session.thread.task_meta.is_none());
        assert_eq!(session.messages.len(), 1);
        assert_eq!(
            session.messages[0].content.content_text_only(),
            "normal pending applied"
        );
    }

    #[tokio::test]
    async fn pending_task_update_missing_source_clears_while_normal_same_id_exists() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let chat_id = "pending-task-update-missing-normal-exists";
        let normal_path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        write_trajectory_file_with_user_message(
            &normal_path,
            chat_id,
            "Normal Same Id Exists",
            "normal must not replace missing task",
        )
        .await;
        let task_meta = task_meta(
            "task-missing-pending-update",
            "agents",
            Some("agent-missing-pending"),
            Some("card-missing-pending"),
            None,
        );
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        {
            let mut session = session_arc.lock().await;
            session.thread.task_meta = Some(task_meta.clone());
            session.thread.title = "Active Task Missing Pending Source".to_string();
            session.messages.push(ChatMessage::new(
                "user".to_string(),
                "keep active task when matching source missing".to_string(),
            ));
            session.external_reload_pending = Some(ExternalReloadPending::update(
                TrajectorySourceIdentity::from_task_meta(&task_meta),
            ));
            session.runtime.state = SessionState::Idle;
            session.trajectory_dirty = false;
        }

        check_external_reload_pending(&gcx.chat_ctx(gcx.clone()), session_arc.clone()).await;

        let session = session_arc.lock().await;
        assert_eq!(session.external_reload_pending, None);
        assert_eq!(session.thread.title, "Active Task Missing Pending Source");
        assert_eq!(session.thread.task_meta, Some(task_meta));
        assert_eq!(session.messages.len(), 1);
        assert_eq!(
            session.messages[0].content.content_text_only(),
            "keep active task when matching source missing"
        );
    }

    #[tokio::test]
    async fn pending_update_source_mismatch_clears_without_loading_other_source() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let chat_id = "pending-update-source-mismatch-clears";
        let normal_path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        write_trajectory_file_with_user_message(
            &normal_path,
            chat_id,
            "Mismatched Normal Pending",
            "normal mismatch must not apply",
        )
        .await;
        let task_meta = task_meta(
            "task-pending-mismatch",
            "agents",
            Some("agent-pending-mismatch"),
            Some("card-pending-mismatch"),
            None,
        );
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        {
            let mut session = session_arc.lock().await;
            session.thread.task_meta = Some(task_meta.clone());
            session.thread.title = "Active Task Mismatched Pending".to_string();
            session.messages.push(ChatMessage::new(
                "user".to_string(),
                "keep task on mismatched pending".to_string(),
            ));
            session.external_reload_pending = Some(ExternalReloadPending::update(
                TrajectorySourceIdentity::Normal,
            ));
            session.runtime.state = SessionState::Idle;
            session.trajectory_dirty = false;
        }

        check_external_reload_pending(&gcx.chat_ctx(gcx.clone()), session_arc.clone()).await;

        let session = session_arc.lock().await;
        assert_eq!(session.external_reload_pending, None);
        assert_eq!(session.thread.title, "Active Task Mismatched Pending");
        assert_eq!(session.thread.task_meta, Some(task_meta));
        assert_eq!(session.messages.len(), 1);
        assert_eq!(
            session.messages[0].content.content_text_only(),
            "keep task on mismatched pending"
        );
    }

    #[tokio::test]
    async fn pending_delete_source_mismatch_clears_without_loading_other_source() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let chat_id = "pending-delete-source-mismatch-clears";
        let normal_path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        write_trajectory_file_with_user_message(
            &normal_path,
            chat_id,
            "Normal Same Id For Delete Mismatch",
            "normal delete mismatch must not apply",
        )
        .await;
        let task_meta = task_meta(
            "task-delete-mismatch",
            "agents",
            Some("agent-delete-mismatch"),
            Some("card-delete-mismatch"),
            None,
        );
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        {
            let mut session = session_arc.lock().await;
            session.thread.title = "Active Normal Delete Mismatch".to_string();
            session.messages.push(ChatMessage::new(
                "user".to_string(),
                "keep normal on mismatched delete".to_string(),
            ));
            session.external_reload_pending = Some(ExternalReloadPending::delete(
                TrajectorySourceIdentity::from_task_meta(&task_meta),
            ));
            session.runtime.state = SessionState::Idle;
            session.trajectory_dirty = false;
        }

        check_external_reload_pending(&gcx.chat_ctx(gcx.clone()), session_arc.clone()).await;

        let session = session_arc.lock().await;
        assert_eq!(session.external_reload_pending, None);
        assert_eq!(session.thread.title, "Active Normal Delete Mismatch");
        assert!(session.thread.task_meta.is_none());
        assert_eq!(session.messages.len(), 1);
        assert_eq!(
            session.messages[0].content.content_text_only(),
            "keep normal on mismatched delete"
        );
    }

    #[tokio::test]
    async fn task_delete_with_active_normal_same_id_does_not_clear_session() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;
        let mut rx = app.chat.trajectory_events_tx.subscribe();
        let chat_id = "task-delete-active-normal-collision";
        let task_meta = task_meta(
            "task-delete-normal-collision",
            "agents",
            Some("agent-3"),
            Some("card-3"),
            None,
        );
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        let mut chat_rx = {
            let mut session = session_arc.lock().await;
            session.thread.title = "Active Normal Delete".to_string();
            session.messages.push(ChatMessage::new(
                "user".to_string(),
                "keep normal during task delete".to_string(),
            ));
            session.runtime.state = SessionState::Idle;
            session.trajectory_dirty = false;
            session.subscribe()
        };
        app.chat
            .sessions
            .write()
            .await
            .insert(chat_id.to_string(), session_arc.clone());

        process_trajectory_change_for_source(
            &gcx.chat_ctx(gcx.clone()),
            chat_id,
            true,
            Some(TrajectorySourceIdentity::from_task_meta(&task_meta)),
        )
        .await;

        let event = wait_for_trajectory_event(&mut rx, chat_id).await;
        assert_eq!(event.event_type, "deleted");
        let session = session_arc.lock().await;
        assert_eq!(session.external_reload_pending, None);
        assert_eq!(session.thread.title, "Active Normal Delete");
        assert_eq!(session.messages.len(), 1);
        assert_eq!(
            session.messages[0].content.content_text_only(),
            "keep normal during task delete"
        );
        drop(session);
        assert_no_chat_event_for(&mut chat_rx, std::time::Duration::from_millis(100)).await;
    }

    #[tokio::test]
    async fn task_delete_with_normal_fallback_emits_updated_without_loading_normal_into_task() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;
        let mut rx = app.chat.trajectory_events_tx.subscribe();
        let chat_id = "task-delete-normal-fallback-no-task-mutation";
        let normal_path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        write_trajectory_file_with_user_message(
            &normal_path,
            chat_id,
            "Normal Fallback After Task Delete",
            "normal fallback should stay out of task session",
        )
        .await;
        let task_meta = task_meta(
            "task-delete-normal-fallback",
            "agents",
            Some("agent-delete-normal-fallback"),
            Some("card-delete-normal-fallback"),
            None,
        );
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        let mut chat_rx = {
            let mut session = session_arc.lock().await;
            session.thread.title = "Active Task Before Delete".to_string();
            session.thread.task_meta = Some(task_meta.clone());
            session.messages.push(ChatMessage::new(
                "user".to_string(),
                "keep active task after task delete".to_string(),
            ));
            session.runtime.state = SessionState::Idle;
            session.trajectory_dirty = false;
            session.subscribe()
        };
        app.chat
            .sessions
            .write()
            .await
            .insert(chat_id.to_string(), session_arc.clone());

        process_trajectory_change_for_source(
            &gcx.chat_ctx(gcx.clone()),
            chat_id,
            true,
            Some(TrajectorySourceIdentity::from_task_meta(&task_meta)),
        )
        .await;

        let event = wait_for_trajectory_event(&mut rx, chat_id).await;
        assert_eq!(event.event_type, "updated");
        assert_eq!(
            event.title.as_deref(),
            Some("Normal Fallback After Task Delete")
        );
        assert_eq!(event.task_id, None);
        assert_eq!(event.task_role, None);
        assert_eq!(event.session_state.as_deref(), Some("idle"));
        assert!(tokio::fs::try_exists(&normal_path).await.unwrap());

        let session = session_arc.lock().await;
        assert_eq!(session.external_reload_pending, None);
        assert!(session.messages.is_empty());
        assert_eq!(session.thread.id, chat_id);
        assert!(session.thread.task_meta.is_none());
        assert_eq!(session.thread.title, ThreadParams::default().title);
        drop(session);

        let json = chat_rx.recv().await.unwrap();
        let envelope: EventEnvelope = serde_json::from_str(&json).unwrap();
        match envelope.event {
            ChatEvent::Snapshot {
                thread, messages, ..
            } => {
                assert_eq!(thread.id, chat_id);
                assert!(thread.task_meta.is_none());
                assert!(messages.is_empty());
            }
            other => panic!("expected Snapshot, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn busy_active_task_pending_delete_resolves_without_loading_normal_fallback() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;
        let mut rx = app.chat.trajectory_events_tx.subscribe();
        let chat_id = "busy-task-delete-normal-fallback-pending";
        let normal_path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        write_trajectory_file_with_user_message(
            &normal_path,
            chat_id,
            "Normal Fallback While Task Busy",
            "normal fallback while task busy",
        )
        .await;
        let task_meta = task_meta(
            "task-busy-delete-normal-fallback",
            "agents",
            Some("agent-busy-delete-normal-fallback"),
            Some("card-busy-delete-normal-fallback"),
            None,
        );
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        let mut chat_rx = {
            let mut session = session_arc.lock().await;
            session.thread.title = "Busy Task Before Delete".to_string();
            session.thread.task_meta = Some(task_meta.clone());
            session.messages.push(ChatMessage::new(
                "user".to_string(),
                "keep busy task after delete".to_string(),
            ));
            session.runtime.state = SessionState::Generating;
            session.trajectory_dirty = true;
            session.subscribe()
        };
        app.chat
            .sessions
            .write()
            .await
            .insert(chat_id.to_string(), session_arc.clone());

        process_trajectory_change_for_source(
            &gcx.chat_ctx(gcx.clone()),
            chat_id,
            true,
            Some(TrajectorySourceIdentity::from_task_meta(&task_meta)),
        )
        .await;

        let event = wait_for_trajectory_event(&mut rx, chat_id).await;
        assert_eq!(event.event_type, "updated");
        assert_eq!(
            event.title.as_deref(),
            Some("Normal Fallback While Task Busy")
        );
        {
            let session = session_arc.lock().await;
            assert_eq!(
                session.external_reload_pending,
                Some(ExternalReloadPending::delete(
                    TrajectorySourceIdentity::from_task_meta(&task_meta)
                ))
            );
            assert_eq!(session.thread.title, "Busy Task Before Delete");
            assert_eq!(session.thread.task_meta, Some(task_meta.clone()));
            assert_eq!(session.messages.len(), 1);
        }

        {
            let mut session = session_arc.lock().await;
            session.runtime.state = SessionState::Idle;
            session.trajectory_dirty = false;
        }
        check_external_reload_pending(&gcx.chat_ctx(gcx.clone()), session_arc.clone()).await;

        let session = session_arc.lock().await;
        assert_eq!(session.external_reload_pending, None);
        assert!(session.messages.is_empty());
        assert_eq!(session.thread.id, chat_id);
        assert!(session.thread.task_meta.is_none());
        assert_eq!(session.thread.title, ThreadParams::default().title);
        drop(session);

        let json = chat_rx.recv().await.unwrap();
        let envelope: EventEnvelope = serde_json::from_str(&json).unwrap();
        match envelope.event {
            ChatEvent::Snapshot {
                thread, messages, ..
            } => {
                assert_eq!(thread.id, chat_id);
                assert!(thread.task_meta.is_none());
                assert!(messages.is_empty());
            }
            other => panic!("expected Snapshot, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn normal_delete_with_active_task_same_id_does_not_clear_session() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;
        let mut rx = app.chat.trajectory_events_tx.subscribe();
        let chat_id = "normal-delete-active-task-collision";
        let active_task_meta = task_meta(
            "task-active-normal-delete-collision",
            "agents",
            Some("agent-4"),
            Some("card-4"),
            None,
        );
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        let mut chat_rx = {
            let mut session = session_arc.lock().await;
            session.thread.title = "Active Task Delete".to_string();
            session.thread.task_meta = Some(active_task_meta.clone());
            session.messages.push(ChatMessage::new(
                "user".to_string(),
                "keep task during normal delete".to_string(),
            ));
            session.runtime.state = SessionState::Idle;
            session.trajectory_dirty = false;
            session.subscribe()
        };
        app.chat
            .sessions
            .write()
            .await
            .insert(chat_id.to_string(), session_arc.clone());

        process_trajectory_change_for_source(
            &gcx.chat_ctx(gcx.clone()),
            chat_id,
            true,
            Some(TrajectorySourceIdentity::Normal),
        )
        .await;

        let event = wait_for_trajectory_event(&mut rx, chat_id).await;
        assert_eq!(event.event_type, "deleted");
        let session = session_arc.lock().await;
        assert_eq!(session.external_reload_pending, None);
        assert_eq!(session.thread.title, "Active Task Delete");
        assert_eq!(session.thread.task_meta, Some(active_task_meta));
        assert_eq!(session.messages.len(), 1);
        assert_eq!(
            session.messages[0].content.content_text_only(),
            "keep task during normal delete"
        );
        drop(session);
        assert_no_chat_event_for(&mut chat_rx, std::time::Duration::from_millis(100)).await;
    }

    #[tokio::test]
    async fn same_task_identity_update_and_delete_still_mutate_or_pend_active_task() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;
        let chat_id = "same-task-update-delete";
        let task_meta = task_meta(
            "task-same-source",
            "agents",
            Some("agent-5"),
            Some("card-5"),
            None,
        );
        let task_path = dir
            .path()
            .join(".refact")
            .join("tasks")
            .join("task-same-source")
            .join("trajectories")
            .join("agents")
            .join("agent-5")
            .join(format!("{chat_id}.json"));
        write_task_trajectory_file_with_user_message(
            &task_path,
            chat_id,
            "Same Task Updated",
            "same task replacement",
            &task_meta,
        )
        .await;
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        {
            let mut session = session_arc.lock().await;
            session.thread.task_meta = Some(task_meta.clone());
            session.thread.title = "Old Same Task".to_string();
            session.runtime.state = SessionState::Idle;
            session.trajectory_dirty = false;
        }
        app.chat
            .sessions
            .write()
            .await
            .insert(chat_id.to_string(), session_arc.clone());

        process_trajectory_change_for_source(
            &gcx.chat_ctx(gcx.clone()),
            chat_id,
            false,
            Some(TrajectorySourceIdentity::from_task_meta(&task_meta)),
        )
        .await;
        {
            let session = session_arc.lock().await;
            assert_eq!(session.external_reload_pending, None);
            assert_eq!(session.thread.title, "Same Task Updated");
            assert_eq!(session.thread.task_meta, Some(task_meta.clone()));
            assert_eq!(
                session.messages[0].content.content_text_only(),
                "same task replacement"
            );
        }
        tokio::fs::remove_file(&task_path).await.unwrap();
        {
            let mut session = session_arc.lock().await;
            session.runtime.state = SessionState::Generating;
            session.trajectory_dirty = true;
        }
        process_trajectory_change_for_source(
            &gcx.chat_ctx(gcx.clone()),
            chat_id,
            true,
            Some(TrajectorySourceIdentity::from_task_meta(&task_meta)),
        )
        .await;
        let session = session_arc.lock().await;
        assert_eq!(
            session.external_reload_pending,
            Some(ExternalReloadPending::delete(
                TrajectorySourceIdentity::from_task_meta(&task_meta)
            ))
        );
        assert_eq!(session.thread.title, "Same Task Updated");
    }

    #[tokio::test]
    async fn same_normal_identity_update_and_delete_still_mutate_or_pend_active_normal() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;
        let chat_id = "same-normal-update-delete";
        let normal_path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        write_trajectory_file_with_user_message(
            &normal_path,
            chat_id,
            "Same Normal Updated",
            "same normal replacement",
        )
        .await;
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        {
            let mut session = session_arc.lock().await;
            session.thread.title = "Old Same Normal".to_string();
            session.runtime.state = SessionState::Idle;
            session.trajectory_dirty = false;
        }
        app.chat
            .sessions
            .write()
            .await
            .insert(chat_id.to_string(), session_arc.clone());

        process_trajectory_change_for_source(
            &gcx.chat_ctx(gcx.clone()),
            chat_id,
            false,
            Some(TrajectorySourceIdentity::Normal),
        )
        .await;
        {
            let session = session_arc.lock().await;
            assert_eq!(session.external_reload_pending, None);
            assert_eq!(session.thread.title, "Same Normal Updated");
            assert!(session.thread.task_meta.is_none());
            assert_eq!(
                session.messages[0].content.content_text_only(),
                "same normal replacement"
            );
        }
        tokio::fs::remove_file(&normal_path).await.unwrap();
        {
            let mut session = session_arc.lock().await;
            session.runtime.state = SessionState::Generating;
            session.trajectory_dirty = true;
        }
        process_trajectory_change_for_source(
            &gcx.chat_ctx(gcx.clone()),
            chat_id,
            true,
            Some(TrajectorySourceIdentity::Normal),
        )
        .await;
        let session = session_arc.lock().await;
        assert_eq!(
            session.external_reload_pending,
            Some(ExternalReloadPending::delete(
                TrajectorySourceIdentity::Normal
            ))
        );
        assert_eq!(session.thread.title, "Same Normal Updated");
    }

    #[tokio::test]
    async fn generic_watcher_update_missing_file_emits_no_event() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;
        let mut rx = app.chat.trajectory_events_tx.subscribe();

        process_trajectory_change(&gcx.chat_ctx(gcx.clone()), "missing-update-no-event", false).await;

        assert_no_trajectory_event_for(&mut rx, std::time::Duration::from_millis(100)).await;
    }

    #[tokio::test]
    async fn generic_watcher_update_schema_incomplete_file_emits_no_event() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;
        let mut rx = app.chat.trajectory_events_tx.subscribe();
        let chat_id = "schema-update-no-event";
        let path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        write_schema_incomplete_trajectory_file(&path, chat_id).await;

        process_trajectory_change(&gcx.chat_ctx(gcx.clone()), chat_id, false).await;

        assert_no_trajectory_event_for(&mut rx, std::time::Duration::from_millis(100)).await;
    }

    #[tokio::test]
    async fn generic_watcher_update_id_mismatched_file_emits_no_event() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;
        let mut rx = app.chat.trajectory_events_tx.subscribe();
        let chat_id = "mismatch-update-no-event";
        let path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        write_trajectory_file(&path, "other-chat", "Mismatched", "2024-01-01T00:00:01Z").await;

        process_trajectory_change(&gcx.chat_ctx(gcx.clone()), chat_id, false).await;

        assert_no_trajectory_event_for(&mut rx, std::time::Duration::from_millis(100)).await;
    }

    #[tokio::test]
    async fn generic_watcher_update_valid_file_emits_metadata_rich_updated() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;
        let mut rx = app.chat.trajectory_events_tx.subscribe();
        let chat_id = "valid-update-rich-event";
        let path = dir
            .path()
            .join(".refact")
            .join("tasks")
            .join("task-update-rich")
            .join("trajectories")
            .join("planner")
            .join(format!("{chat_id}.json"));
        write_trajectory_file_with_metadata(
            &path,
            chat_id,
            "Watcher Update Rich",
            "2024-01-01T00:00:01Z",
            "valid update message",
        )
        .await;

        process_trajectory_change(&gcx.chat_ctx(gcx.clone()), chat_id, false).await;

        let event = wait_for_trajectory_event(&mut rx, chat_id).await;
        assert_eq!(event.event_type, "updated");
        assert_eq!(event.updated_at.as_deref(), Some("2024-01-01T00:00:01Z"));
        assert_eq!(event.title.as_deref(), Some("Watcher Update Rich"));
        assert_eq!(event.is_title_generated, Some(true));
        assert_eq!(event.message_count, Some(1));
        assert_eq!(event.parent_id.as_deref(), Some("parent-fallback"));
        assert_eq!(event.link_type.as_deref(), Some("handoff"));
        assert_eq!(event.root_chat_id.as_deref(), Some("root-fallback"));
        assert_eq!(event.task_id.as_deref(), Some("task-update-rich"));
        assert_eq!(event.task_role.as_deref(), Some("planner"));
        assert_eq!(event.model.as_deref(), Some("fallback-model"));
        assert_eq!(event.mode.as_deref(), Some("task_planner"));
        assert_eq!(event.session_state.as_deref(), Some("idle"));
    }

    #[tokio::test]
    async fn generic_watcher_update_missing_file_busy_generic_sets_pending_without_event() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;
        let mut rx = app.chat.trajectory_events_tx.subscribe();
        let chat_id = "missing-update-busy-pending";
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        {
            let mut session = session_arc.lock().await;
            session.runtime.state = SessionState::Generating;
            session.trajectory_dirty = false;
        }
        app.chat
            .sessions
            .write()
            .await
            .insert(chat_id.to_string(), session_arc.clone());

        process_trajectory_change(&gcx.chat_ctx(gcx.clone()), chat_id, false).await;

        let session = session_arc.lock().await;
        assert_eq!(
            session.external_reload_pending,
            Some(ExternalReloadPending::update(
                TrajectorySourceIdentity::Normal
            ))
        );
        drop(session);
        assert_no_trajectory_event_for(&mut rx, std::time::Duration::from_millis(100)).await;
    }

    #[tokio::test]
    async fn generic_watcher_update_missing_file_active_buddy_sets_no_pending_or_event() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;
        let mut rx = app.chat.trajectory_events_tx.subscribe();
        let chat_id = "missing-update-active-buddy-noop";
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        {
            let mut session = session_arc.lock().await;
            session.thread.mode = "buddy".to_string();
            session.thread.buddy_meta = Some(buddy_thread_meta());
            session.runtime.state = SessionState::Generating;
            session.trajectory_dirty = false;
        }
        app.chat
            .sessions
            .write()
            .await
            .insert(chat_id.to_string(), session_arc.clone());

        process_trajectory_change(&gcx.chat_ctx(gcx.clone()), chat_id, false).await;

        let session = session_arc.lock().await;
        assert_eq!(session.external_reload_pending, None);
        drop(session);
        assert_no_trajectory_event_for(&mut rx, std::time::Duration::from_millis(100)).await;
    }

    #[tokio::test]
    async fn immediate_external_delete_clears_existing_pending_reload_state() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;

        for (chat_id, pending) in [
            (
                "immediate-delete-clears-pending-update",
                ExternalReloadPending::update(TrajectorySourceIdentity::Normal),
            ),
            (
                "immediate-delete-clears-pending-delete",
                ExternalReloadPending::delete(TrajectorySourceIdentity::Normal),
            ),
        ] {
            let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
            {
                let mut session = session_arc.lock().await;
                session.messages.push(ChatMessage::new(
                    "user".to_string(),
                    "before immediate delete".to_string(),
                ));
                session.external_reload_pending = Some(pending);
                session.runtime.state = SessionState::Idle;
                session.trajectory_dirty = false;
                session.wake_up_at = Some(chrono::Utc::now() + chrono::Duration::minutes(10));
                session.waiting_for_card_ids = vec!["card-waiting".to_string()];
                session.thread.reactive_compact_attempts = Some(2);
            }
            app.chat
                .sessions
                .write()
                .await
                .insert(chat_id.to_string(), session_arc.clone());

            process_trajectory_change(&gcx.chat_ctx(gcx.clone()).clone(), chat_id, true).await;

            let session = session_arc.lock().await;
            assert_eq!(session.external_reload_pending, None);
            assert!(session.messages.is_empty());
            assert_eq!(session.thread.id, chat_id);
            assert!(session.wake_up_at.is_none());
            assert!(session.waiting_for_card_ids.is_empty());
            assert_eq!(session.thread.reactive_compact_attempts, None);
        }
    }

    #[tokio::test]
    async fn external_delete_then_save_does_not_persist_stale_delete_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;
        let chat_id = "external-delete-save-fresh-metadata";
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        let stale_created_at = "2024-01-01T00:00:00Z";
        {
            let mut session = session_arc.lock().await;
            session.messages.push(ChatMessage::new(
                "user".to_string(),
                "before external delete".to_string(),
            ));
            session.created_at = stale_created_at.to_string();
            session.wake_up_at = Some(chrono::Utc::now() + chrono::Duration::minutes(10));
            session.waiting_for_card_ids = vec!["card-stale".to_string()];
            session.thread.reactive_compact_attempts = Some(2);
            session.runtime.state = SessionState::Idle;
            session.trajectory_dirty = false;
        }
        app.chat
            .sessions
            .write()
            .await
            .insert(chat_id.to_string(), session_arc.clone());

        process_trajectory_change(&gcx.chat_ctx(gcx.clone()).clone(), chat_id, true).await;
        {
            let mut session = session_arc.lock().await;
            session.add_message(ChatMessage::new(
                "user".to_string(),
                "after external delete".to_string(),
            ));
        }
        try_save_trajectory(app, session_arc.clone()).await.unwrap();

        let path = nested_trajectory_path(dir.path(), chat_id, chat_id);
        let saved: serde_json::Value =
            serde_json::from_str(&tokio::fs::read_to_string(&path).await.unwrap()).unwrap();

        assert_ne!(saved["created_at"].as_str(), Some(stale_created_at));
        assert!(saved.get("wake_up_at").is_none());
        assert!(saved.get("waiting_for_card_ids").is_none());
        assert!(saved.get("reactive_compact_attempts").is_none());
        assert_eq!(saved["messages"].as_array().unwrap().len(), 1);
        assert_eq!(saved["messages"][0]["content"], "after external delete");
    }

    #[tokio::test]
    async fn pending_external_update_missing_file_clears_pending_state() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let chat_id = "pending-update-missing-file";
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        {
            let mut session = session_arc.lock().await;
            session.messages.push(ChatMessage::new(
                "user".to_string(),
                "keep after missing update".to_string(),
            ));
            session.external_reload_pending = Some(ExternalReloadPending::update(
                TrajectorySourceIdentity::Normal,
            ));
            session.runtime.state = SessionState::Idle;
            session.trajectory_dirty = false;
        }

        check_external_reload_pending(&gcx.chat_ctx(gcx.clone()), session_arc.clone()).await;

        let session = session_arc.lock().await;
        assert_eq!(session.external_reload_pending, None);
        assert_eq!(session.messages.len(), 1);
        assert_eq!(
            session.messages[0].content.content_text_only(),
            "keep after missing update"
        );
    }

    #[test]
    fn trajectory_snapshot_from_session_preserves_ui_only_diagnostics() {
        let mut session = ChatSession::new("ui-only-snapshot".to_string());
        let diagnostic = make_ui_only_error_message("context_length_exceeded");
        session
            .messages
            .push(ChatMessage::new("user".to_string(), "Hello".to_string()));
        session.messages.push(diagnostic.clone());

        let snapshot = trajectory_snapshot_from_session(&session);

        assert_eq!(snapshot.messages.len(), 2);
        assert!(snapshot.messages.iter().any(|message| {
            message.message_id == diagnostic.message_id
                && message.role == "error"
                && is_ui_only_message(message)
        }));
    }

