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
    async fn external_remove_clears_running_compression_state_and_snapshot() {
        assert_external_remove_clears_active_compression(CompressionPhase::Running).await;
    }

    #[tokio::test]
    async fn immediate_external_delete_with_loadable_trajectory_reloads_instead_of_clearing() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;
        let mut rx = app.chat.trajectory_events_tx.subscribe();
        let chat_id = "immediate-delete-loadable-reloads";
        let path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        write_trajectory_file_with_user_message(
            &path,
            chat_id,
            "Reloaded After Stale Delete",
            "recreated message",
        )
        .await;
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        let mut chat_rx = {
            let mut session = session_arc.lock().await;
            session.messages.push(ChatMessage::new(
                "user".to_string(),
                "old in-memory message".to_string(),
            ));
            session.thread.title = "Old In Memory".to_string();
            session.external_reload_pending = Some(ExternalReloadPending::delete(
                TrajectorySourceIdentity::Normal,
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

        process_trajectory_change(&gcx.chat_ctx(gcx.clone()), chat_id, true).await;

        let event = wait_for_trajectory_event(&mut rx, chat_id).await;
        assert_eq!(event.event_type, "updated");
        assert_eq!(event.title.as_deref(), Some("Reloaded After Stale Delete"));

        let session = session_arc.lock().await;
        assert_eq!(session.external_reload_pending, None);
        assert_eq!(session.thread.title, "Reloaded After Stale Delete");
        assert_eq!(session.messages.len(), 1);
        assert_eq!(
            session.messages[0].content.content_text_only(),
            "recreated message"
        );
        drop(session);

        let json = chat_rx.recv().await.unwrap();
        let envelope: EventEnvelope = serde_json::from_str(&json).unwrap();
        match envelope.event {
            ChatEvent::Snapshot {
                thread, messages, ..
            } => {
                assert_eq!(thread.title, "Reloaded After Stale Delete");
                assert_eq!(messages.len(), 1);
            }
            other => panic!("expected Snapshot, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn immediate_external_true_delete_still_clears_reloadable_session() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;
        let mut rx = app.chat.trajectory_events_tx.subscribe();
        let chat_id = "immediate-true-delete-clears";
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        let mut chat_rx = {
            let mut session = session_arc.lock().await;
            session.messages.push(ChatMessage::new(
                "user".to_string(),
                "before true delete".to_string(),
            ));
            session.thread.title = "Delete Me".to_string();
            session.runtime.state = SessionState::Idle;
            session.trajectory_dirty = false;
            session.subscribe()
        };
        app.chat
            .sessions
            .write()
            .await
            .insert(chat_id.to_string(), session_arc.clone());

        process_trajectory_change(&gcx.chat_ctx(gcx.clone()), chat_id, true).await;

        let event = wait_for_trajectory_event(&mut rx, chat_id).await;
        assert_eq!(event.event_type, "deleted");
        assert_eq!(event.title, None);

        let session = session_arc.lock().await;
        assert_eq!(session.external_reload_pending, None);
        assert!(session.messages.is_empty());
        assert_eq!(session.thread.id, chat_id);
        assert_eq!(session.thread.title, ThreadParams::default().title);
        drop(session);

        let json = chat_rx.recv().await.unwrap();
        let envelope: EventEnvelope = serde_json::from_str(&json).unwrap();
        match envelope.event {
            ChatEvent::Snapshot {
                thread, messages, ..
            } => {
                assert_eq!(thread.id, chat_id);
                assert!(messages.is_empty());
            }
            other => panic!("expected Snapshot, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn final_delete_guard_defers_when_session_is_not_reloadable() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let chat_id = "final-delete-guard-defers";
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        {
            let mut session = session_arc.lock().await;
            session.messages.push(ChatMessage::new(
                "user".to_string(),
                "keep while busy".to_string(),
            ));
            session.external_reload_pending = Some(ExternalReloadPending::delete(
                TrajectorySourceIdentity::Normal,
            ));
            session.runtime.state = SessionState::Generating;
            session.trajectory_dirty = false;
        }

        let outcome = apply_external_delete_with_revalidation(
            &gcx.chat_ctx(gcx.clone()),
            session_arc.clone(),
            chat_id,
            Some(ExternalReloadPending::delete(
                TrajectorySourceIdentity::Normal,
            )),
            None,
        )
        .await;

        let session = session_arc.lock().await;
        assert!(matches!(
            outcome,
            ExternalDeleteRevalidationOutcome::Deleted {
                applied_to_session: false
            }
        ));
        assert_eq!(
            session.external_reload_pending,
            Some(ExternalReloadPending::delete(
                TrajectorySourceIdentity::Normal
            ))
        );
        assert_eq!(session.messages.len(), 1);
        assert_eq!(session.thread.id, chat_id);
    }

    #[tokio::test]
    async fn stale_pending_delete_helper_does_not_overwrite_newer_update_pending() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let chat_id = "stale-delete-keeps-update-pending";
        let path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        write_trajectory_file_with_user_message(
            &path,
            chat_id,
            "Newer Update",
            "newer update message",
        )
        .await;
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        {
            let mut session = session_arc.lock().await;
            session.external_reload_pending = Some(ExternalReloadPending::update(
                TrajectorySourceIdentity::Normal,
            ));
            session.runtime.state = SessionState::Generating;
            session.trajectory_dirty = false;
        }

        let outcome = apply_external_delete_with_revalidation(
            &gcx.chat_ctx(gcx.clone()),
            session_arc.clone(),
            chat_id,
            Some(ExternalReloadPending::delete(
                TrajectorySourceIdentity::Normal,
            )),
            None,
        )
        .await;

        let session = session_arc.lock().await;
        assert!(matches!(
            outcome,
            ExternalDeleteRevalidationOutcome::NoopStalePending
        ));
        assert_eq!(
            session.external_reload_pending,
            Some(ExternalReloadPending::update(
                TrajectorySourceIdentity::Normal
            ))
        );
    }

    #[tokio::test]
    async fn pending_external_delete_applies_when_session_becomes_reloadable() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;
        let mut rx = app.chat.trajectory_events_tx.subscribe();
        let chat_id = "pending-external-delete";
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        let mut chat_rx = {
            let mut session = session_arc.lock().await;
            session.messages.push(ChatMessage::new(
                "user".to_string(),
                "before deferred delete".to_string(),
            ));
            session.thread.title = "Delete Pending".to_string();
            session.thread.reactive_compact_attempts = Some(2);
            session.created_at = "2024-01-01T00:00:00Z".to_string();
            session.wake_up_at = Some(chrono::Utc::now() + chrono::Duration::minutes(10));
            session.waiting_for_card_ids = vec!["card-stale".to_string()];
            session.runtime.state = SessionState::Generating;
            session.trajectory_dirty = true;
            mark_active_compression(&mut session, CompressionPhase::Running);
            session.subscribe()
        };
        app.chat
            .sessions
            .write()
            .await
            .insert(chat_id.to_string(), session_arc.clone());

        process_trajectory_change(&gcx.chat_ctx(gcx.clone()).clone(), chat_id, true).await;
        {
            let session = session_arc.lock().await;
            assert_eq!(
                session.external_reload_pending,
                Some(ExternalReloadPending::delete(
                    TrajectorySourceIdentity::Normal
                ))
            );
            assert_eq!(session.messages.len(), 1);
            assert!(session.active_compression_attempt.is_some());
        }

        let event = wait_for_trajectory_event(&mut rx, chat_id).await;
        assert_eq!(event.event_type, "deleted");

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
        assert_eq!(session.thread.title, ThreadParams::default().title);
        assert_eq!(session.thread.reactive_compact_attempts, None);
        assert_ne!(session.created_at, "2024-01-01T00:00:00Z");
        assert!(session.wake_up_at.is_none());
        assert!(session.waiting_for_card_ids.is_empty());
        assert!(!session.is_compressing);
        assert!(!session.runtime.is_compressing);
        assert_eq!(session.compression_phase, None);
        assert_eq!(session.runtime.compression_phase, None);
        assert_eq!(session.compression_reason, None);
        assert_eq!(session.runtime.compression_reason, None);
        assert_eq!(session.active_compression_attempt, None);
        drop(session);

        let json = chat_rx.recv().await.unwrap();
        let envelope: EventEnvelope = serde_json::from_str(&json).unwrap();
        match envelope.event {
            ChatEvent::Snapshot {
                thread,
                runtime,
                messages,
                ..
            } => {
                assert_eq!(thread.id, chat_id);
                assert_eq!(thread.reactive_compact_attempts, None);
                assert!(messages.is_empty());
                assert!(!runtime.is_compressing);
                assert_eq!(runtime.compression_phase, None);
            }
            other => panic!("expected Snapshot, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn pending_external_delete_revalidates_recreated_trajectory_before_clearing() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;
        let mut rx = app.chat.trajectory_events_tx.subscribe();
        let chat_id = "pending-delete-recreated-reloads";
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        let mut chat_rx = {
            let mut session = session_arc.lock().await;
            session.messages.push(ChatMessage::new(
                "user".to_string(),
                "before stale delete".to_string(),
            ));
            session.thread.title = "Busy Before Delete".to_string();
            session.runtime.state = SessionState::Generating;
            session.trajectory_dirty = true;
            session.subscribe()
        };
        app.chat
            .sessions
            .write()
            .await
            .insert(chat_id.to_string(), session_arc.clone());

        process_trajectory_change(&gcx.chat_ctx(gcx.clone()).clone(), chat_id, true).await;
        {
            let session = session_arc.lock().await;
            assert_eq!(
                session.external_reload_pending,
                Some(ExternalReloadPending::delete(
                    TrajectorySourceIdentity::Normal
                ))
            );
            assert_eq!(session.thread.title, "Busy Before Delete");
        }

        let event = wait_for_trajectory_event(&mut rx, chat_id).await;
        assert_eq!(event.event_type, "deleted");

        let path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        write_trajectory_file_with_user_message(
            &path,
            chat_id,
            "Recreated Before Apply",
            "recreated before pending delete",
        )
        .await;
        {
            let mut session = session_arc.lock().await;
            session.runtime.state = SessionState::Idle;
            session.trajectory_dirty = false;
        }
        check_external_reload_pending(&gcx.chat_ctx(gcx.clone()), session_arc.clone()).await;

        let session = session_arc.lock().await;
        assert_eq!(session.external_reload_pending, None);
        assert_eq!(session.thread.title, "Recreated Before Apply");
        assert_eq!(session.messages.len(), 1);
        assert_eq!(
            session.messages[0].content.content_text_only(),
            "recreated before pending delete"
        );
        drop(session);

        let json = chat_rx.recv().await.unwrap();
        let envelope: EventEnvelope = serde_json::from_str(&json).unwrap();
        match envelope.event {
            ChatEvent::Snapshot {
                thread, messages, ..
            } => {
                assert_eq!(thread.title, "Recreated Before Apply");
                assert_eq!(messages.len(), 1);
            }
            other => panic!("expected Snapshot, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn generic_watcher_remove_does_not_clear_active_buddy_session() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;
        let chat_id = "active-buddy-remove-guard";
        let buddy_path = dir
            .path()
            .join(".refact")
            .join("buddy")
            .join("chats")
            .join("conversations")
            .join(format!("{chat_id}.json"));
        let normal_path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        write_trajectory_file(
            &normal_path,
            chat_id,
            "Removed Normal",
            "2024-01-01T00:00:01Z",
        )
        .await;
        write_buddy_conversation_file(&buddy_path, chat_id, "Active Buddy File").await;
        tokio::fs::remove_file(&normal_path).await.unwrap();
        let mut rx = app.chat.trajectory_events_tx.subscribe();
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        let mut chat_rx = {
            let mut session = session_arc.lock().await;
            session.thread.title = "Active Buddy Session".to_string();
            session.thread.mode = "buddy".to_string();
            session.thread.buddy_meta = Some(buddy_thread_meta());
            session.external_reload_pending = Some(ExternalReloadPending::update(
                TrajectorySourceIdentity::Buddy,
            ));
            session.messages.push(ChatMessage::new(
                "user".to_string(),
                "keep buddy message".to_string(),
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

        process_trajectory_change(&gcx.chat_ctx(gcx.clone()).clone(), chat_id, true).await;
        let event = wait_for_trajectory_event(&mut rx, chat_id).await;
        assert_eq!(event.event_type, "deleted");
        assert_eq!(event.title, None);
        assert!(tokio::fs::try_exists(&buddy_path).await.unwrap());
        assert!(!tokio::fs::try_exists(&normal_path).await.unwrap());

        let session = session_arc.lock().await;
        assert_eq!(
            session.external_reload_pending,
            Some(ExternalReloadPending::update(
                TrajectorySourceIdentity::Buddy
            ))
        );
        assert_eq!(session.thread.title, "Active Buddy Session");
        assert!(session.thread.buddy_meta.is_some());
        assert_eq!(session.messages.len(), 1);
        assert_eq!(
            session.messages[0].content.content_text_only(),
            "keep buddy message"
        );
        drop(session);

        assert_no_chat_event_for(&mut chat_rx, std::time::Duration::from_millis(100)).await;
    }

    #[tokio::test]
    async fn generic_watcher_remove_with_active_buddy_and_generic_fallback_emits_updated() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;
        let mut rx = app.chat.trajectory_events_tx.subscribe();
        let chat_id = "active-buddy-remove-generic-fallback";
        let fallback_path = dir
            .path()
            .join(".refact")
            .join("tasks")
            .join("task-active-buddy-fallback")
            .join("trajectories")
            .join("planner")
            .join(format!("{chat_id}.json"));
        write_trajectory_file_with_metadata(
            &fallback_path,
            chat_id,
            "Active Buddy Fallback",
            "2024-01-01T00:00:01Z",
            "fallback survives active buddy",
        )
        .await;
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        let mut chat_rx = {
            let mut session = session_arc.lock().await;
            session.thread.title = "Active Buddy Session".to_string();
            session.thread.mode = "buddy".to_string();
            session.thread.buddy_meta = Some(buddy_thread_meta());
            session.thread.worktree = Some(trajectory_worktree_sample_with_id("buddy-remove-wt"));
            session.external_reload_pending = Some(ExternalReloadPending::delete(
                TrajectorySourceIdentity::Buddy,
            ));
            session.runtime.state = SessionState::Generating;
            session.runtime.error = Some("buddy remove fallback error".to_string());
            session.messages.push(ChatMessage::new(
                "user".to_string(),
                "keep active buddy with fallback".to_string(),
            ));
            session.trajectory_dirty = false;
            session.subscribe()
        };
        app.chat
            .sessions
            .write()
            .await
            .insert(chat_id.to_string(), session_arc.clone());

        process_trajectory_change(&gcx.chat_ctx(gcx.clone()).clone(), chat_id, true).await;

        let event = wait_for_trajectory_event(&mut rx, chat_id).await;
        assert_eq!(event.event_type, "updated");
        assert_eq!(event.title.as_deref(), Some("Active Buddy Fallback"));
        assert_eq!(event.task_id.as_deref(), Some("task-active-buddy-fallback"));
        assert_eq!(event.task_role.as_deref(), Some("planner"));
        assert_eq!(event.message_count, Some(1));
        assert_eq!(event.session_state.as_deref(), Some("idle"));
        assert_eq!(event.error, None);
        assert_ne!(
            event.worktree.as_ref().map(|worktree| worktree.id.as_str()),
            Some("buddy-remove-wt")
        );

        let session = session_arc.lock().await;
        assert_eq!(
            session.external_reload_pending,
            Some(ExternalReloadPending::delete(
                TrajectorySourceIdentity::Buddy
            ))
        );
        assert_eq!(session.thread.title, "Active Buddy Session");
        assert_eq!(session.thread.mode, "buddy");
        assert!(session.thread.buddy_meta.is_some());
        assert_eq!(session.messages.len(), 1);
        assert_eq!(
            session.messages[0].content.content_text_only(),
            "keep active buddy with fallback"
        );
        drop(session);

        assert_no_chat_event_for(&mut chat_rx, std::time::Duration::from_millis(100)).await;
    }

    #[tokio::test]
    async fn generic_watcher_remove_with_active_buddy_persists_repaired_fallback() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;
        let mut rx = app.chat.trajectory_events_tx.subscribe();
        let chat_id = "active-buddy-remove-repaired-fallback";
        let fallback_path = dir
            .path()
            .join(".refact")
            .join("tasks")
            .join("task-active-buddy-repair-fallback")
            .join("trajectories")
            .join("planner")
            .join(format!("{chat_id}.json"));
        tokio::fs::create_dir_all(fallback_path.parent().unwrap())
            .await
            .unwrap();
        tokio::fs::write(
            &fallback_path,
            serde_json::to_string(&json!({
                "id": chat_id,
                "title": "Active Buddy Repaired Fallback",
                "model": "model",
                "mode": "task_planner",
                "tool_use": "agent",
                "parent_id": "source-chat",
                "link_type": "mode_transition",
                "previous_response_id": "resp_source",
                "messages": [
                    {"role":"system","content":"target task planner system"},
                    {"role":"user","content":"fallback survives active buddy repair"}
                ],
                "frozen_request_prefix": {
                    "schema_version": 1,
                    "created_at": "2026-05-29T00:00:00Z",
                    "system_prompt": "source system",
                    "tools_canonical": [{"type":"function","function":{"name":"source_tool"}}]
                },
                "claude_code_identity": {
                    "device_id":"source-device",
                    "session_id":"source-session"
                },
                "created_at": "2024-01-01T00:00:00Z",
                "updated_at": "2024-01-01T00:00:01Z",
                "include_project_info": true,
                "checkpoints_enabled": true,
                "custom_future_field": {"keep": true}
            }))
            .unwrap(),
        )
        .await
        .unwrap();
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        let mut chat_rx = {
            let mut session = session_arc.lock().await;
            session.thread.title = "Active Buddy Session".to_string();
            session.thread.mode = "buddy".to_string();
            session.thread.buddy_meta = Some(buddy_thread_meta());
            session.external_reload_pending = Some(ExternalReloadPending::delete(
                TrajectorySourceIdentity::Buddy,
            ));
            session.messages.push(ChatMessage::new(
                "user".to_string(),
                "keep active buddy during repaired fallback".to_string(),
            ));
            session.runtime.state = SessionState::Generating;
            session.trajectory_dirty = false;
            session.subscribe()
        };
        app.chat
            .sessions
            .write()
            .await
            .insert(chat_id.to_string(), session_arc.clone());

        process_trajectory_change(&gcx.chat_ctx(gcx.clone()).clone(), chat_id, true).await;

        let event = wait_for_trajectory_event(&mut rx, chat_id).await;
        assert_eq!(event.event_type, "updated");
        assert_eq!(
            event.title.as_deref(),
            Some("Active Buddy Repaired Fallback")
        );
        assert_eq!(
            event.task_id.as_deref(),
            Some("task-active-buddy-repair-fallback")
        );
        assert_eq!(event.session_state.as_deref(), Some("idle"));

        let session = session_arc.lock().await;
        assert_eq!(
            session.external_reload_pending,
            Some(ExternalReloadPending::delete(
                TrajectorySourceIdentity::Buddy
            ))
        );
        assert_eq!(session.thread.title, "Active Buddy Session");
        assert_eq!(session.thread.mode, "buddy");
        assert!(session.thread.buddy_meta.is_some());
        assert_eq!(session.messages.len(), 1);
        assert_eq!(
            session.messages[0].content.content_text_only(),
            "keep active buddy during repaired fallback"
        );
        drop(session);

        let raw: serde_json::Value =
            serde_json::from_str(&tokio::fs::read_to_string(&fallback_path).await.unwrap())
                .unwrap();
        assert_eq!(
            raw["frozen_request_prefix"]["system_prompt"],
            "target task planner system"
        );
        assert!(raw["frozen_request_prefix"]["tools_canonical"].is_null());
        assert!(raw.get("claude_code_identity").is_none());
        assert!(raw.get("previous_response_id").is_none());
        assert_eq!(raw["custom_future_field"]["keep"], true);
        assert_no_chat_event_for(&mut chat_rx, std::time::Duration::from_millis(100)).await;
    }

    #[tokio::test]
    async fn generic_watcher_update_does_not_replace_active_buddy_session() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;
        let chat_id = "active-buddy-update-guard";
        let normal_path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        write_trajectory_file_with_user_message(
            &normal_path,
            chat_id,
            "Normal Replacement",
            "normal replacement message",
        )
        .await;
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        let mut chat_rx = {
            let mut session = session_arc.lock().await;
            session.thread.title = "Active Buddy Session".to_string();
            session.thread.mode = "buddy".to_string();
            session.thread.buddy_meta = Some(buddy_thread_meta());
            session.messages.push(ChatMessage::new(
                "user".to_string(),
                "keep buddy message".to_string(),
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

        process_trajectory_change(&gcx.chat_ctx(gcx.clone()).clone(), chat_id, false).await;
        check_external_reload_pending(&gcx.chat_ctx(gcx.clone()), session_arc.clone()).await;

        let session = session_arc.lock().await;
        assert_eq!(session.external_reload_pending, None);
        assert_eq!(session.thread.title, "Active Buddy Session");
        assert_eq!(session.thread.mode, "buddy");
        assert!(session.thread.buddy_meta.is_some());
        assert_eq!(session.messages.len(), 1);
        assert_eq!(
            session.messages[0].content.content_text_only(),
            "keep buddy message"
        );
        drop(session);

        assert_no_chat_event_for(&mut chat_rx, std::time::Duration::from_millis(100)).await;
    }

    #[tokio::test]
    async fn generic_watcher_update_with_active_buddy_persists_repaired_generic() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;
        let mut rx = app.chat.trajectory_events_tx.subscribe();
        let chat_id = "active-buddy-update-repaired-generic";
        let path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        tokio::fs::create_dir_all(path.parent().unwrap())
            .await
            .unwrap();
        tokio::fs::write(
            &path,
            serde_json::to_string(&json!({
                "id": chat_id,
                "title": "Repaired Generic Update",
                "model": "model",
                "mode": "task_planner",
                "tool_use": "agent",
                "parent_id": "source-chat",
                "link_type": "mode_transition",
                "previous_response_id": "resp_source",
                "messages": [
                    {"role":"system","content":"target task planner system"},
                    {"role":"user","content":"generic update active buddy repair"}
                ],
                "frozen_request_prefix": {
                    "schema_version": 1,
                    "created_at": "2026-05-29T00:00:00Z",
                    "system_prompt": "source system",
                    "tools_canonical": [{"type":"function","function":{"name":"source_tool"}}]
                },
                "claude_code_identity": {
                    "device_id":"source-device",
                    "session_id":"source-session"
                },
                "created_at": "2024-01-01T00:00:00Z",
                "updated_at": "2024-01-01T00:00:01Z",
                "include_project_info": true,
                "checkpoints_enabled": true,
                "custom_future_field": {"keep": true}
            }))
            .unwrap(),
        )
        .await
        .unwrap();
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        let mut chat_rx = {
            let mut session = session_arc.lock().await;
            session.thread.title = "Active Buddy Session".to_string();
            session.thread.mode = "buddy".to_string();
            session.thread.buddy_meta = Some(buddy_thread_meta());
            session.external_reload_pending = Some(ExternalReloadPending::update(
                TrajectorySourceIdentity::Buddy,
            ));
            session.messages.push(ChatMessage::new(
                "user".to_string(),
                "keep active buddy during repaired update".to_string(),
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

        process_trajectory_change(&gcx.chat_ctx(gcx.clone()).clone(), chat_id, false).await;
        check_external_reload_pending(&gcx.chat_ctx(gcx.clone()).clone(), session_arc.clone()).await;

        let event = wait_for_trajectory_event(&mut rx, chat_id).await;
        assert_eq!(event.event_type, "updated");
        assert_eq!(event.title.as_deref(), Some("Repaired Generic Update"));
        assert_eq!(event.session_state.as_deref(), Some("idle"));

        let session = session_arc.lock().await;
        assert_eq!(
            session.external_reload_pending,
            Some(ExternalReloadPending::update(
                TrajectorySourceIdentity::Buddy
            ))
        );
        assert_eq!(session.thread.title, "Active Buddy Session");
        assert_eq!(session.thread.mode, "buddy");
        assert!(session.thread.buddy_meta.is_some());
        assert_eq!(session.messages.len(), 1);
        assert_eq!(
            session.messages[0].content.content_text_only(),
            "keep active buddy during repaired update"
        );
        drop(session);

        let raw: serde_json::Value =
            serde_json::from_str(&tokio::fs::read_to_string(&path).await.unwrap()).unwrap();
        assert_eq!(
            raw["frozen_request_prefix"]["system_prompt"],
            "target task planner system"
        );
        assert!(raw["frozen_request_prefix"]["tools_canonical"].is_null());
        assert!(raw.get("claude_code_identity").is_none());
        assert!(raw.get("previous_response_id").is_none());
        assert_eq!(raw["custom_future_field"]["keep"], true);
        assert_no_chat_event_for(&mut chat_rx, std::time::Duration::from_millis(100)).await;
    }

    #[tokio::test]
    async fn normal_save_sse_ignores_active_task_state_error_and_worktree() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;
        let mut rx = app.chat.trajectory_events_tx.subscribe();
        let chat_id = "normal-save-active-task-clean";
        let task_meta = task_meta(
            "task-normal-save-collision",
            "agents",
            Some("agent-save-collision"),
            Some("card-save-collision"),
            None,
        );
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        {
            let mut session = session_arc.lock().await;
            session.thread.task_meta = Some(task_meta);
            session.thread.worktree = Some(trajectory_worktree_sample_with_id("task-save-wt"));
            session.runtime.state = SessionState::Generating;
            session.runtime.error = Some("task save error".to_string());
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
                "Normal Save Collision",
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
        assert!(matches!(
            event.session_state.as_deref(),
            None | Some("idle")
        ));
        assert_eq!(event.error, None);
        assert!(event.worktree.is_none());
    }

