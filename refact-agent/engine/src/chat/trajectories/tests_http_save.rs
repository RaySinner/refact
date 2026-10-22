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
    async fn http_delete_higher_priority_trajectory_repairs_fallback_before_updated_event() {
        let dir = tempfile::tempdir().unwrap();
        let (_gcx, app) = make_app_with_workspace(dir.path()).await;
        let mut rx = app.chat.trajectory_events_tx.subscribe();
        let chat_id = "http-delete-repaired-fallback-updated-at";
        let normal_path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        let task_path = dir
            .path()
            .join(".refact")
            .join("tasks")
            .join("task-repaired-fallback")
            .join("trajectories")
            .join("planner")
            .join(format!("{chat_id}.json"));
        write_trajectory_file(
            &normal_path,
            chat_id,
            "Deleted Higher Priority Repair",
            "2024-01-01T00:00:02Z",
        )
        .await;
        tokio::fs::create_dir_all(task_path.parent().unwrap())
            .await
            .unwrap();
        tokio::fs::write(
            &task_path,
            serde_json::to_string(&json!({
                "id": chat_id,
                "title": "Repaired Lower Priority Fallback",
                "model": "fallback-model",
                "mode": "task_planner",
                "tool_use": "agent",
                "parent_id": "source-chat",
                "link_type": "mode_transition",
                "previous_response_id": "resp_stale",
                "messages": [
                    {"role":"system","content":"target fallback system"},
                    {"role":"user","content":"fallback message"}
                ],
                "frozen_request_prefix": {
                    "schema_version": 1,
                    "created_at": "2026-05-29T00:00:00Z",
                    "system_prompt": "stale source system",
                    "tools_canonical": [{"type":"function","function":{"name":"source_tool"}}]
                },
                "claude_code_identity": {
                    "device_id":"source-device",
                    "session_id":"source-session"
                },
                "created_at": "not-a-date",
                "updated_at": "2024-01-01T00:00:01Z",
                "include_project_info": true,
                "checkpoints_enabled": true,
                "custom_future_field": {"keep": true, "nested": {"value": 73}}
            }))
            .unwrap(),
        )
        .await
        .unwrap();

        handle_v1_trajectories_delete(State(app), AxumPath(chat_id.to_string()))
            .await
            .unwrap();

        let event = wait_for_trajectory_event(&mut rx, chat_id).await;
        assert_eq!(event.event_type, "updated");
        assert_eq!(
            event.title.as_deref(),
            Some("Repaired Lower Priority Fallback")
        );
        assert_eq!(event.task_id.as_deref(), Some("task-repaired-fallback"));
        assert_eq!(event.task_role.as_deref(), Some("planner"));
        assert_ne!(event.updated_at.as_deref(), Some("2024-01-01T00:00:01Z"));
        assert!(!tokio::fs::try_exists(&normal_path).await.unwrap());
        assert!(tokio::fs::try_exists(&task_path).await.unwrap());

        let repaired: serde_json::Value =
            serde_json::from_str(&tokio::fs::read_to_string(&task_path).await.unwrap()).unwrap();
        assert_eq!(event.updated_at.as_deref(), repaired["updated_at"].as_str());
        assert!(parsed_rfc3339_utc(repaired["created_at"].as_str().unwrap()).is_some());
        assert_eq!(
            repaired["frozen_request_prefix"]["system_prompt"],
            "target fallback system"
        );
        assert!(repaired["frozen_request_prefix"]["tools_canonical"].is_null());
        assert!(repaired.get("previous_response_id").is_none());
        assert!(repaired.get("claude_code_identity").is_none());
        assert_eq!(repaired["custom_future_field"]["nested"]["value"], 73);
    }

    #[tokio::test]
    async fn http_delete_normal_with_task_fallback_does_not_reload_task_into_normal_session() {
        let dir = tempfile::tempdir().unwrap();
        let (_gcx, app) = make_app_with_workspace(dir.path()).await;
        let mut rx = app.chat.trajectory_events_tx.subscribe();
        let chat_id = "http-delete-normal-task-fallback-no-normal-mutation";
        let normal_path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        let task_meta = task_meta(
            "task-normal-delete-fallback",
            "agents",
            Some("agent-normal-delete-fallback"),
            Some("card-normal-delete-fallback"),
            None,
        );
        let task_path = dir
            .path()
            .join(".refact")
            .join("tasks")
            .join("task-normal-delete-fallback")
            .join("trajectories")
            .join("agents")
            .join("agent-normal-delete-fallback")
            .join(format!("{chat_id}.json"));
        write_trajectory_file(
            &normal_path,
            chat_id,
            "Deleted Normal",
            "2024-01-01T00:00:02Z",
        )
        .await;
        write_task_trajectory_file_with_user_message(
            &task_path,
            chat_id,
            "Task Fallback After Normal Delete",
            "task fallback should stay out of normal session",
            &task_meta,
        )
        .await;
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        let mut chat_rx = {
            let mut session = session_arc.lock().await;
            session.thread.title = "Active Normal Before Delete".to_string();
            session.messages.push(ChatMessage::new(
                "user".to_string(),
                "keep active normal after normal delete".to_string(),
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

        handle_v1_trajectories_delete(State(app), AxumPath(chat_id.to_string()))
            .await
            .unwrap();

        let event = wait_for_trajectory_event(&mut rx, chat_id).await;
        assert_eq!(event.event_type, "updated");
        assert_eq!(
            event.title.as_deref(),
            Some("Task Fallback After Normal Delete")
        );
        assert_eq!(
            event.task_id.as_deref(),
            Some("task-normal-delete-fallback")
        );
        assert_eq!(event.task_role.as_deref(), Some("agents"));
        assert_eq!(
            event.agent_id.as_deref(),
            Some("agent-normal-delete-fallback")
        );
        assert_eq!(event.session_state.as_deref(), Some("idle"));
        assert!(!tokio::fs::try_exists(&normal_path).await.unwrap());
        assert!(tokio::fs::try_exists(&task_path).await.unwrap());

        let session = session_arc.lock().await;
        assert_eq!(session.external_reload_pending, None);
        assert_eq!(session.thread.title, "Active Normal Before Delete");
        assert!(session.thread.task_meta.is_none());
        assert_eq!(session.messages.len(), 1);
        assert_eq!(
            session.messages[0].content.content_text_only(),
            "keep active normal after normal delete"
        );
        drop(session);
        assert_no_chat_event_for(&mut chat_rx, std::time::Duration::from_millis(100)).await;
    }

    #[tokio::test]
    async fn http_delete_fallback_with_active_buddy_uses_generic_idle_state() {
        let dir = tempfile::tempdir().unwrap();
        let (_gcx, app) = make_app_with_workspace(dir.path()).await;
        let mut rx = app.chat.trajectory_events_tx.subscribe();
        let chat_id = "http-delete-fallback-active-buddy-clean";
        let normal_path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        let task_path = dir
            .path()
            .join(".refact")
            .join("tasks")
            .join("task-buddy-delete-fallback")
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
            "Fallback With Buddy Collision",
            "2024-01-01T00:00:01Z",
            "fallback message",
        )
        .await;
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        {
            let mut session = session_arc.lock().await;
            session.thread.mode = "buddy".to_string();
            session.thread.buddy_meta = Some(buddy_thread_meta());
            session.thread.worktree = Some(trajectory_worktree_sample_with_id("buddy-delete-wt"));
            session.runtime.state = SessionState::Generating;
            session.runtime.error = Some("buddy delete fallback error".to_string());
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
        assert_eq!(
            event.title.as_deref(),
            Some("Fallback With Buddy Collision")
        );
        assert_eq!(event.task_id.as_deref(), Some("task-buddy-delete-fallback"));
        assert_eq!(event.session_state.as_deref(), Some("idle"));
        assert_eq!(event.error, None);
        assert_ne!(
            event.worktree.as_ref().map(|worktree| worktree.id.as_str()),
            Some("buddy-delete-wt")
        );
        assert!(!tokio::fs::try_exists(&normal_path).await.unwrap());
        assert!(tokio::fs::try_exists(&task_path).await.unwrap());
    }

    #[tokio::test]
    async fn http_delete_only_remaining_trajectory_emits_deleted() {
        let dir = tempfile::tempdir().unwrap();
        let (_gcx, app) = make_app_with_workspace(dir.path()).await;
        let mut rx = app.chat.trajectory_events_tx.subscribe();
        let chat_id = "http-delete-only-deleted";
        let normal_path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        write_trajectory_file(
            &normal_path,
            chat_id,
            "Only Remaining",
            "2024-01-01T00:00:01Z",
        )
        .await;

        handle_v1_trajectories_delete(State(app), AxumPath(chat_id.to_string()))
            .await
            .unwrap();

        let event = wait_for_trajectory_event(&mut rx, chat_id).await;
        assert_eq!(event.event_type, "deleted");
        assert_eq!(event.updated_at, None);
        assert_eq!(event.title, None);
        assert_eq!(event.session_state, None);
        assert!(!tokio::fs::try_exists(&normal_path).await.unwrap());
    }

    #[tokio::test]
    async fn trajectory_id_mismatch_save_snapshot_does_not_overwrite_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let chat_id = "save-mismatch-guard";
        let path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        write_trajectory_file(
            &path,
            "other-chat",
            "Keep Mismatched",
            "2024-01-01T00:00:00Z",
        )
        .await;
        let before = tokio::fs::read_to_string(&path).await.unwrap();

        let err = save_trajectory_snapshot(
            gcx,
            test_snapshot(
                chat_id,
                "Should Not Save",
                vec![ChatMessage::new("user".to_string(), "hello".to_string())],
            ),
        )
        .await
        .unwrap_err();

        assert!(err.contains("Existing trajectory file id mismatch"));
        assert_eq!(tokio::fs::read_to_string(path).await.unwrap(), before);
    }

    #[tokio::test]
    async fn ordinary_save_preserves_existing_browser_meta_and_unknown_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let chat_id = "metadata-preserve-browser-custom";
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
                "title": "Existing Metadata",
                "model": "model",
                "mode": "agent",
                "tool_use": "agent",
                "messages": [{"role":"user","content":"old"}],
                "created_at": "2024-01-01T00:00:00Z",
                "updated_at": "2024-01-01T00:00:00Z",
                "include_project_info": true,
                "checkpoints_enabled": true,
                "browser_meta": {
                    "browser_runtime_id": "browser-preserve",
                    "tab_urls": ["https://example.com/preserve"],
                    "active_tab_id": "tab-preserve",
                    "attach_screenshot_on_send": true
                },
                "custom_future_field": {
                    "keep": true,
                    "nested": {"value": 57}
                }
            }))
            .unwrap(),
        )
        .await
        .unwrap();

        save_trajectory_snapshot(
            gcx,
            test_snapshot(
                chat_id,
                "Updated Metadata",
                vec![ChatMessage::new("user".to_string(), "new".to_string())],
            ),
        )
        .await
        .unwrap();

        let saved: serde_json::Value =
            serde_json::from_str(&tokio::fs::read_to_string(path).await.unwrap()).unwrap();
        assert_eq!(saved["title"], "Updated Metadata");
        assert_eq!(
            saved["browser_meta"]["browser_runtime_id"],
            "browser-preserve"
        );
        assert_eq!(saved["custom_future_field"]["nested"]["value"], 57);
    }

    #[tokio::test]
    async fn ordinary_save_removes_stale_provider_identity_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let chat_id = "metadata-clear-stale-provider";
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
                "title": "Provider Existing",
                "model": "model",
                "mode": "agent",
                "tool_use": "agent",
                "messages": [{"role":"user","content":"old"}],
                "created_at": "2024-01-01T00:00:00Z",
                "updated_at": "2024-01-01T00:00:00Z",
                "include_project_info": true,
                "checkpoints_enabled": true,
                "previous_response_id": "resp_stale",
                "frozen_request_prefix": {
                    "schema_version": 1,
                    "created_at": "2026-05-29T00:00:00Z",
                    "system_prompt": "stale system",
                    "tools_canonical": [{"type":"function"}]
                },
                "claude_code_identity": {
                    "device_id": "device-stale",
                    "session_id": "session-stale"
                },
                "custom_future_field": {"keep": true}
            }))
            .unwrap(),
        )
        .await
        .unwrap();

        save_trajectory_snapshot(
            gcx,
            test_snapshot(
                chat_id,
                "Provider Cleared",
                vec![ChatMessage::new("user".to_string(), "new".to_string())],
            ),
        )
        .await
        .unwrap();

        let saved: serde_json::Value =
            serde_json::from_str(&tokio::fs::read_to_string(path).await.unwrap()).unwrap();
        assert_eq!(saved["title"], "Provider Cleared");
        assert!(saved.get("previous_response_id").is_none());
        assert!(saved.get("frozen_request_prefix").is_none());
        assert!(saved.get("claude_code_identity").is_none());
        assert_eq!(saved["custom_future_field"]["keep"], true);
    }

    #[tokio::test]
    async fn title_only_ordinary_save_preserves_unknown_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let chat_id = "metadata-title-only-preserve";
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
                "title": "Old Title",
                "model": "model",
                "mode": "agent",
                "tool_use": "agent",
                "messages": [],
                "created_at": "2024-01-01T00:00:00Z",
                "updated_at": "2024-01-01T00:00:00Z",
                "include_project_info": true,
                "checkpoints_enabled": true,
                "custom_future_field": {"nested": {"value": 58}}
            }))
            .unwrap(),
        )
        .await
        .unwrap();

        save_trajectory_snapshot(gcx, test_snapshot(chat_id, "Retitled", Vec::new()))
            .await
            .unwrap();

        let saved: serde_json::Value =
            serde_json::from_str(&tokio::fs::read_to_string(path).await.unwrap()).unwrap();
        assert_eq!(saved["title"], "Retitled");
        assert_eq!(saved["custom_future_field"]["nested"]["value"], 58);
    }

    #[tokio::test]
    async fn trajectory_id_mismatch_http_save_does_not_overwrite_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let (_gcx, app) = make_app_with_workspace(dir.path()).await;
        let chat_id = "http-save-mismatch-guard";
        let path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        write_trajectory_file(
            &path,
            "other-chat",
            "Keep Mismatched",
            "2024-01-01T00:00:00Z",
        )
        .await;
        let before = tokio::fs::read_to_string(&path).await.unwrap();
        let payload = sample_trajectory(chat_id, "Should Not Save", "2024-01-01T00:00:01Z");

        let err = handle_v1_trajectories_save(
            State(app),
            AxumPath(chat_id.to_string()),
            hyper::body::Bytes::from(serde_json::to_vec(&payload).unwrap()),
        )
        .await
        .unwrap_err();

        assert_eq!(err.status_code, StatusCode::CONFLICT);
        assert_eq!(tokio::fs::read_to_string(path).await.unwrap(), before);
    }

    #[tokio::test]
    async fn http_save_updates_existing_global_trajectory_without_project_shadow() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;
        let chat_id = "http-save-global-original";
        let global_dir = get_global_trajectories_dir(gcx).await;
        let global_path = global_dir.join(format!("{chat_id}.json"));
        write_trajectory_file(
            &global_path,
            chat_id,
            "Global Original",
            "2024-01-01T00:00:00Z",
        )
        .await;
        let mut payload = sample_trajectory(chat_id, "Global Updated", "2024-01-01T00:00:01Z");
        payload["messages"] = json!([{"role":"user","content":"saved globally"}]);

        handle_v1_trajectories_save(
            State(app),
            AxumPath(chat_id.to_string()),
            hyper::body::Bytes::from(serde_json::to_vec(&payload).unwrap()),
        )
        .await
        .unwrap();

        let saved: serde_json::Value =
            serde_json::from_str(&tokio::fs::read_to_string(&global_path).await.unwrap()).unwrap();
        assert_eq!(saved["title"], "Global Updated");
        assert_eq!(saved["messages"].as_array().unwrap().len(), 1);
        let project_path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        assert!(!tokio::fs::try_exists(project_path).await.unwrap());
    }

    #[tokio::test]
    async fn http_save_updates_existing_task_trajectory_without_project_shadow() {
        let dir = tempfile::tempdir().unwrap();
        let (_gcx, app) = make_app_with_workspace(dir.path()).await;
        let chat_id = "http-save-task-original";
        let task_id = "task-http-save";
        let task_path = dir
            .path()
            .join(".refact")
            .join("tasks")
            .join(task_id)
            .join("trajectories")
            .join("agents")
            .join("agent-1")
            .join(format!("{chat_id}.json"));
        write_trajectory_file(&task_path, chat_id, "Task Original", "2024-01-01T00:00:00Z").await;
        let mut payload = sample_trajectory(chat_id, "Task Updated", "2024-01-01T00:00:01Z");
        payload["task_meta"] = json!({
            "task_id": task_id,
            "role": "agents",
            "agent_id": "agent-1",
            "card_id": "card-1"
        });

        handle_v1_trajectories_save(
            State(app),
            AxumPath(chat_id.to_string()),
            hyper::body::Bytes::from(serde_json::to_vec(&payload).unwrap()),
        )
        .await
        .unwrap();

        let saved: serde_json::Value =
            serde_json::from_str(&tokio::fs::read_to_string(&task_path).await.unwrap()).unwrap();
        assert_eq!(saved["title"], "Task Updated");
        assert_eq!(saved["task_meta"]["task_id"], task_id);
        let project_path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        assert!(!tokio::fs::try_exists(project_path).await.unwrap());
    }

    #[tokio::test]
    async fn http_save_updates_existing_buddy_trajectory_without_project_shadow() {
        let dir = tempfile::tempdir().unwrap();
        let (_gcx, app) = make_app_with_workspace(dir.path()).await;
        let chat_id = "http-save-buddy-original";
        let buddy_path = dir
            .path()
            .join(".refact")
            .join("buddy")
            .join("chats")
            .join("conversations")
            .join(format!("{chat_id}.json"));
        write_buddy_conversation_file(&buddy_path, chat_id, "Buddy Original").await;
        let mut payload = sample_trajectory(chat_id, "Buddy Updated", "2024-01-01T00:00:01Z");
        payload["mode"] = json!("buddy");
        payload["buddy_meta"] = json!({
            "is_buddy_chat": true,
            "buddy_chat_kind": "investigation"
        });

        handle_v1_trajectories_save(
            State(app),
            AxumPath(chat_id.to_string()),
            hyper::body::Bytes::from(serde_json::to_vec(&payload).unwrap()),
        )
        .await
        .unwrap();

        let saved: serde_json::Value =
            serde_json::from_str(&tokio::fs::read_to_string(&buddy_path).await.unwrap()).unwrap();
        assert_eq!(saved["title"], "Buddy Updated");
        assert!(saved["buddy_meta"]["is_buddy_chat"].as_bool().unwrap());
        let project_path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        assert!(!tokio::fs::try_exists(project_path).await.unwrap());
    }

    #[tokio::test]
    async fn http_generic_save_with_active_buddy_uses_generic_idle_state() {
        let dir = tempfile::tempdir().unwrap();
        let (_gcx, app) = make_app_with_workspace(dir.path()).await;
        let mut rx = app.chat.trajectory_events_tx.subscribe();
        let chat_id = "http-generic-save-active-buddy-clean";
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        {
            let mut session = session_arc.lock().await;
            session.thread.mode = "buddy".to_string();
            session.thread.buddy_meta = Some(buddy_thread_meta());
            session.thread.worktree = Some(trajectory_worktree_sample_with_id("buddy-save-wt"));
            session.runtime.state = SessionState::Generating;
            session.runtime.error = Some("buddy save error".to_string());
        }
        app.chat
            .sessions
            .write()
            .await
            .insert(chat_id.to_string(), session_arc);
        let mut payload =
            sample_trajectory(chat_id, "Generic Save Collision", "2024-01-01T00:00:01Z");
        payload["messages"] = json!([{ "role": "user", "content": "generic save" }]);

        handle_v1_trajectories_save(
            State(app),
            AxumPath(chat_id.to_string()),
            hyper::body::Bytes::from(serde_json::to_vec(&payload).unwrap()),
        )
        .await
        .unwrap();

        let event = wait_for_trajectory_event(&mut rx, chat_id).await;
        assert_eq!(event.event_type, "created");
        assert_eq!(event.title.as_deref(), Some("Generic Save Collision"));
        assert_eq!(event.session_state.as_deref(), Some("idle"));
        assert_eq!(event.error, None);
        assert!(event.worktree.is_none());
    }

    #[tokio::test]
    async fn http_buddy_save_does_not_emit_generic_trajectory_sse() {
        let dir = tempfile::tempdir().unwrap();
        let (_gcx, app) = make_app_with_workspace(dir.path()).await;
        let mut rx = app.chat.trajectory_events_tx.subscribe();
        let chat_id = "http-buddy-save-no-generic-sse";
        let mut payload = sample_trajectory(chat_id, "Buddy Saved", "2024-01-01T00:00:01Z");
        payload["mode"] = json!("buddy");
        payload["buddy_meta"] = json!({
            "is_buddy_chat": true,
            "buddy_chat_kind": "investigation",
            "workflow_id": null
        });

        handle_v1_trajectories_save(
            State(app),
            AxumPath(chat_id.to_string()),
            hyper::body::Bytes::from(serde_json::to_vec(&payload).unwrap()),
        )
        .await
        .unwrap();

        assert_no_trajectory_event_for(&mut rx, std::time::Duration::from_millis(100)).await;
    }

    #[tokio::test]
    async fn trajectory_save_namespace_generic_save_does_not_overwrite_existing_buddy_file() {
        let dir = tempfile::tempdir().unwrap();
        let (_gcx, app) = make_app_with_workspace(dir.path()).await;
        let chat_id = "namespace-generic-buddy-collision";
        let buddy_path = dir
            .path()
            .join(".refact")
            .join("buddy")
            .join("chats")
            .join("conversations")
            .join(format!("{chat_id}.json"));
        write_buddy_conversation_file(&buddy_path, chat_id, "Keep Buddy").await;
        let before = tokio::fs::read_to_string(&buddy_path).await.unwrap();
        let mut payload = sample_trajectory(chat_id, "Generic Saved", "2024-01-01T00:00:01Z");
        payload["messages"] = json!([{ "role": "user", "content": "generic" }]);

        handle_v1_trajectories_save(
            State(app),
            AxumPath(chat_id.to_string()),
            hyper::body::Bytes::from(serde_json::to_vec(&payload).unwrap()),
        )
        .await
        .unwrap();

        assert_eq!(
            tokio::fs::read_to_string(&buddy_path).await.unwrap(),
            before
        );
        let normal_path = nested_trajectory_path(dir.path(), chat_id, chat_id);
        let saved: serde_json::Value =
            serde_json::from_str(&tokio::fs::read_to_string(&normal_path).await.unwrap()).unwrap();
        assert_eq!(saved["title"], "Generic Saved");
        assert!(saved.get("buddy_meta").is_none());
    }

    #[tokio::test]
    async fn trajectory_save_namespace_generic_save_does_not_overwrite_existing_task_file() {
        let dir = tempfile::tempdir().unwrap();
        let (_gcx, app) = make_app_with_workspace(dir.path()).await;
        let chat_id = "namespace-generic-task-collision";
        let task_path = dir
            .path()
            .join(".refact")
            .join("tasks")
            .join("task-generic-collision")
            .join("trajectories")
            .join("agents")
            .join("agent-1")
            .join(format!("{chat_id}.json"));
        write_trajectory_file(&task_path, chat_id, "Keep Task", "2024-01-01T00:00:00Z").await;
        let before = tokio::fs::read_to_string(&task_path).await.unwrap();
        let payload = sample_trajectory(chat_id, "Generic Saved", "2024-01-01T00:00:01Z");

        handle_v1_trajectories_save(
            State(app),
            AxumPath(chat_id.to_string()),
            hyper::body::Bytes::from(serde_json::to_vec(&payload).unwrap()),
        )
        .await
        .unwrap();

        assert_eq!(tokio::fs::read_to_string(&task_path).await.unwrap(), before);
        let normal_path = nested_trajectory_path(dir.path(), chat_id, chat_id);
        let saved: serde_json::Value =
            serde_json::from_str(&tokio::fs::read_to_string(&normal_path).await.unwrap()).unwrap();
        assert_eq!(saved["title"], "Generic Saved");
        assert!(saved.get("task_meta").is_none());
    }

    #[tokio::test]
    async fn trajectory_save_namespace_task_save_does_not_overwrite_existing_normal_file() {
        let dir = tempfile::tempdir().unwrap();
        let (_gcx, app) = make_app_with_workspace(dir.path()).await;
        let chat_id = "namespace-task-normal-collision";
        let task_id = "task-normal-collision";
        let normal_path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        write_trajectory_file(&normal_path, chat_id, "Keep Normal", "2024-01-01T00:00:00Z").await;
        let before = tokio::fs::read_to_string(&normal_path).await.unwrap();
        let task_dir = dir.path().join(".refact").join("tasks").join(task_id);
        tokio::fs::create_dir_all(&task_dir).await.unwrap();
        let mut payload = sample_trajectory(chat_id, "Task Saved", "2024-01-01T00:00:01Z");
        payload["task_meta"] = json!({
            "task_id": task_id,
            "role": "agents",
            "agent_id": "agent-1",
            "card_id": "card-1"
        });

        handle_v1_trajectories_save(
            State(app),
            AxumPath(chat_id.to_string()),
            hyper::body::Bytes::from(serde_json::to_vec(&payload).unwrap()),
        )
        .await
        .unwrap();

        assert_eq!(
            tokio::fs::read_to_string(&normal_path).await.unwrap(),
            before
        );
        let task_path = task_dir
            .join("trajectories")
            .join("agents")
            .join("agent-1")
            .join(format!("{chat_id}.json"));
        let saved: serde_json::Value =
            serde_json::from_str(&tokio::fs::read_to_string(&task_path).await.unwrap()).unwrap();
        assert_eq!(saved["title"], "Task Saved");
        assert_eq!(saved["task_meta"]["task_id"], task_id);
    }

    #[tokio::test]
    async fn trajectory_save_namespace_buddy_save_does_not_overwrite_existing_normal_or_task_file()
    {
        let dir = tempfile::tempdir().unwrap();
        let (_gcx, app) = make_app_with_workspace(dir.path()).await;
        let chat_id = "namespace-buddy-normal-task-collision";
        let normal_path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        let task_path = dir
            .path()
            .join(".refact")
            .join("tasks")
            .join("task-buddy-collision")
            .join("trajectories")
            .join("planner")
            .join(format!("{chat_id}.json"));
        write_trajectory_file(&normal_path, chat_id, "Keep Normal", "2024-01-01T00:00:00Z").await;
        write_trajectory_file(&task_path, chat_id, "Keep Task", "2024-01-01T00:00:00Z").await;
        let normal_before = tokio::fs::read_to_string(&normal_path).await.unwrap();
        let task_before = tokio::fs::read_to_string(&task_path).await.unwrap();
        let mut payload = sample_trajectory(chat_id, "Buddy Saved", "2024-01-01T00:00:01Z");
        payload["mode"] = json!("buddy");
        payload["buddy_meta"] = json!({
            "is_buddy_chat": true,
            "buddy_chat_kind": "investigation",
            "workflow_id": null
        });

        handle_v1_trajectories_save(
            State(app),
            AxumPath(chat_id.to_string()),
            hyper::body::Bytes::from(serde_json::to_vec(&payload).unwrap()),
        )
        .await
        .unwrap();

        assert_eq!(
            tokio::fs::read_to_string(&normal_path).await.unwrap(),
            normal_before
        );
        assert_eq!(
            tokio::fs::read_to_string(&task_path).await.unwrap(),
            task_before
        );
        let buddy_path = dir
            .path()
            .join(".refact")
            .join("buddy")
            .join("chats")
            .join("conversations")
            .join(format!("{chat_id}.json"));
        let saved: serde_json::Value =
            serde_json::from_str(&tokio::fs::read_to_string(&buddy_path).await.unwrap()).unwrap();
        assert_eq!(saved["title"], "Buddy Saved");
        assert_eq!(saved["buddy_meta"]["is_buddy_chat"], true);
    }

    #[tokio::test]
    async fn trajectory_save_namespace_task_and_buddy_meta_rejects_and_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let (_gcx, app) = make_app_with_workspace(dir.path()).await;
        let chat_id = "namespace-task-buddy-meta-conflict";
        let task_id = "task-meta-conflict";
        tokio::fs::create_dir_all(dir.path().join(".refact").join("tasks").join(task_id))
            .await
            .unwrap();
        let mut payload = sample_trajectory(chat_id, "Meta Conflict", "2024-01-01T00:00:01Z");
        payload["task_meta"] = json!({
            "task_id": task_id,
            "role": "planner",
            "planner_chat_id": chat_id
        });
        payload["buddy_meta"] = json!({
            "is_buddy_chat": true,
            "buddy_chat_kind": "investigation",
            "workflow_id": null
        });

        let err = handle_v1_trajectories_save(
            State(app),
            AxumPath(chat_id.to_string()),
            hyper::body::Bytes::from(serde_json::to_vec(&payload).unwrap()),
        )
        .await
        .unwrap_err();

        assert_eq!(err.status_code, StatusCode::INTERNAL_SERVER_ERROR);
        assert!(err.message.contains("both task_meta and buddy_meta"));
        assert!(!tokio::fs::try_exists(
            dir.path()
                .join(".refact")
                .join("trajectories")
                .join(format!("{chat_id}.json"))
        )
        .await
        .unwrap());
        assert!(!tokio::fs::try_exists(
            dir.path()
                .join(".refact")
                .join("buddy")
                .join("chats")
                .join("conversations")
                .join(format!("{chat_id}.json"))
        )
        .await
        .unwrap());
        assert!(!tokio::fs::try_exists(
            dir.path()
                .join(".refact")
                .join("tasks")
                .join(task_id)
                .join("trajectories")
                .join("planner")
                .join(format!("{chat_id}.json"))
        )
        .await
        .unwrap());
    }

    #[tokio::test]
    async fn trajectory_save_namespace_malformed_non_null_task_meta_rejects_and_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let (_gcx, app) = make_app_with_workspace(dir.path()).await;
        let chat_id = "namespace-malformed-task-meta";
        let mut payload = sample_trajectory(chat_id, "Malformed Task", "2024-01-01T00:00:01Z");
        payload["task_meta"] = json!({"task_id": "task-malformed"});

        let err = handle_v1_trajectories_save(
            State(app),
            AxumPath(chat_id.to_string()),
            hyper::body::Bytes::from(serde_json::to_vec(&payload).unwrap()),
        )
        .await
        .unwrap_err();

        assert_eq!(err.status_code, StatusCode::INTERNAL_SERVER_ERROR);
        assert!(err.message.contains("Invalid task_meta"));
        assert!(!tokio::fs::try_exists(
            dir.path()
                .join(".refact")
                .join("trajectories")
                .join(format!("{chat_id}.json"))
        )
        .await
        .unwrap());
        assert!(!tokio::fs::try_exists(
            dir.path()
                .join(".refact")
                .join("tasks")
                .join("task-malformed")
        )
        .await
        .unwrap());
    }

    #[tokio::test]
    async fn trajectory_id_mismatch_title_generation_does_not_update_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let chat_id = "title-mismatch-guard";
        let trajectories_dir = dir.path().join(".refact").join("trajectories");
        let path = trajectories_dir.join(format!("{chat_id}.json"));
        write_trajectory_file(&path, "other-chat", "New Chat", "2024-01-01T00:00:00Z").await;
        let before = tokio::fs::read_to_string(&path).await.unwrap();

        spawn_title_generation_task(
            gcx,
            chat_id.to_string(),
            vec![json!({"role":"user","content":"Generate guarded title"})],
            path.clone(),
            TrajectorySourceIdentity::Normal,
        );
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        assert_eq!(tokio::fs::read_to_string(path).await.unwrap(), before);
    }

    #[tokio::test]
    async fn title_generation_mismatched_backing_file_does_not_update_active_session() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;
        let chat_id = "title-active-mismatch-guard";
        let trajectories_dir = dir.path().join(".refact").join("trajectories");
        let path = trajectories_dir.join(format!("{chat_id}.json"));
        write_trajectory_file(&path, "other-chat", "New Chat", "2024-01-01T00:00:00Z").await;
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        {
            let mut session = session_arc.lock().await;
            session.thread.title = "New Chat".to_string();
            session.thread.is_title_generated = false;
        }
        app.chat
            .sessions
            .write()
            .await
            .insert(chat_id.to_string(), session_arc.clone());

        spawn_title_generation_task(
            gcx,
            chat_id.to_string(),
            vec![json!({"role":"user","content":"Generate guarded active title"})],
            path.clone(),
            TrajectorySourceIdentity::Normal,
        );
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        let session = session_arc.lock().await;
        assert_eq!(session.thread.title, "New Chat");
        assert!(!session.thread.is_title_generated);
    }

    #[tokio::test]
    async fn generic_title_generation_with_active_buddy_updates_file_and_sse_only() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;
        let mut rx = app.chat.trajectory_events_tx.subscribe();
        let chat_id = "title-generic-active-buddy";
        let trajectories_dir = dir.path().join(".refact").join("trajectories");
        let path = trajectories_dir.join(format!("{chat_id}.json"));
        let expected_title = "Generic collision request";
        write_trajectory_file_with_user_message(&path, chat_id, "New Chat", expected_title).await;
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        {
            let mut session = session_arc.lock().await;
            session.thread.title = "Buddy Session Title".to_string();
            session.thread.mode = "buddy".to_string();
            session.thread.buddy_meta = Some(buddy_thread_meta());
            session.thread.is_title_generated = false;
            session.trajectory_events_tx = Some(app.chat.trajectory_events_tx.clone());
        }
        app.chat
            .sessions
            .write()
            .await
            .insert(chat_id.to_string(), session_arc.clone());

        spawn_title_generation_task(
            gcx,
            chat_id.to_string(),
            vec![json!({"role":"user","content":expected_title})],
            path.clone(),
            TrajectorySourceIdentity::Normal,
        );

        wait_for_file_title(&path, expected_title).await;
        let event = wait_for_trajectory_event(&mut rx, chat_id).await;
        assert_eq!(event.event_type, "updated");
        assert_eq!(event.title.as_deref(), Some(expected_title));
        assert_eq!(event.is_title_generated, Some(true));
        assert_eq!(event.session_state.as_deref(), Some("idle"));
        let session = session_arc.lock().await;
        assert_eq!(session.thread.title, "Buddy Session Title");
        assert!(!session.thread.is_title_generated);
    }

    #[tokio::test]
    async fn buddy_title_generation_with_active_generic_updates_file_without_generic_sse() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;
        let mut rx = app.chat.trajectory_events_tx.subscribe();
        let chat_id = "title-buddy-active-generic";
        let buddy_dir = dir
            .path()
            .join(".refact")
            .join("buddy")
            .join("chats")
            .join("conversations");
        let path = buddy_dir.join(format!("{chat_id}.json"));
        let expected_title = "Buddy collision request";
        write_buddy_conversation_file(&path, chat_id, "New Chat").await;
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        {
            let mut session = session_arc.lock().await;
            session.thread.title = "Generic Session Title".to_string();
            session.thread.is_title_generated = false;
            session.trajectory_events_tx = Some(app.chat.trajectory_events_tx.clone());
        }
        app.chat
            .sessions
            .write()
            .await
            .insert(chat_id.to_string(), session_arc.clone());

        spawn_title_generation_task(
            gcx,
            chat_id.to_string(),
            vec![json!({"role":"user","content":expected_title})],
            path.clone(),
            TrajectorySourceIdentity::Buddy,
        );

        wait_for_file_title(&path, expected_title).await;
        let session = session_arc.lock().await;
        assert_eq!(session.thread.title, "Generic Session Title");
        assert!(!session.thread.is_title_generated);
        drop(session);
        assert_no_trajectory_event_for(&mut rx, std::time::Duration::from_millis(200)).await;
    }

    #[tokio::test]
    async fn task_title_generation_with_active_normal_updates_task_file_only() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;
        let mut rx = app.chat.trajectory_events_tx.subscribe();
        let chat_id = "title-task-active-normal";
        let task_meta = task_meta(
            "task-title-normal",
            "agents",
            Some("agent-1"),
            Some("card-1"),
            None,
        );
        let path = dir
            .path()
            .join(".refact")
            .join("tasks")
            .join("task-title-normal")
            .join("trajectories")
            .join("agents")
            .join("agent-1")
            .join(format!("{chat_id}.json"));
        let expected_title = "Task collision request";
        write_task_trajectory_file_with_user_message(
            &path,
            chat_id,
            "New Chat",
            expected_title,
            &task_meta,
        )
        .await;
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        {
            let mut session = session_arc.lock().await;
            session.thread.title = "Normal Session Title".to_string();
            session.thread.is_title_generated = false;
            session.trajectory_events_tx = Some(app.chat.trajectory_events_tx.clone());
        }
        app.chat
            .sessions
            .write()
            .await
            .insert(chat_id.to_string(), session_arc.clone());

        spawn_title_generation_task(
            gcx,
            chat_id.to_string(),
            vec![json!({"role":"user","content":expected_title})],
            path.clone(),
            TrajectorySourceIdentity::from_task_meta(&task_meta),
        );

        wait_for_file_title(&path, expected_title).await;
        let event = wait_for_trajectory_event(&mut rx, chat_id).await;
        assert_eq!(event.event_type, "updated");
        assert_eq!(event.title.as_deref(), Some(expected_title));
        assert_eq!(event.task_id.as_deref(), Some("task-title-normal"));
        assert_eq!(event.task_role.as_deref(), Some("agents"));
        assert_eq!(event.agent_id.as_deref(), Some("agent-1"));
        assert_eq!(event.card_id.as_deref(), Some("card-1"));
        let session = session_arc.lock().await;
        assert_eq!(session.thread.title, "Normal Session Title");
        assert!(!session.thread.is_title_generated);
    }

    #[tokio::test]
    async fn file_only_title_generation_update_preserves_token_totals() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;
        let mut rx = app.chat.trajectory_events_tx.subscribe();
        let chat_id = "title-file-only-token-totals";
        let path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        let expected_title = "Token rich title request";
        let mut trajectory = sample_trajectory(chat_id, "New Chat", "2024-01-01T00:00:01Z");
        trajectory["messages"] = json!([
            {"role": "user", "content": expected_title},
            {
                "role": "assistant",
                "content": "response",
                "usage": {
                    "prompt_tokens": 11,
                    "completion_tokens": 7,
                    "total_tokens": 18,
                    "cache_read_input_tokens": 3,
                    "cache_creation_input_tokens": 2,
                    "metering_usd": {"total_usd": 0.012}
                }
            },
            {
                "role": "assistant",
                "content": "second response",
                "usage": {
                    "prompt_tokens": 13,
                    "completion_tokens": 5,
                    "total_tokens": 18,
                    "cache_read_tokens": 4,
                    "cache_creation_tokens": 6,
                    "metering_usd": {"total_usd": 0.008}
                }
            }
        ]);
        tokio::fs::create_dir_all(path.parent().unwrap())
            .await
            .unwrap();
        tokio::fs::write(&path, serde_json::to_string(&trajectory).unwrap())
            .await
            .unwrap();

        spawn_title_generation_task(
            gcx,
            chat_id.to_string(),
            vec![json!({"role":"user","content":expected_title})],
            path.clone(),
            TrajectorySourceIdentity::Normal,
        );

        wait_for_file_title(&path, expected_title).await;
        let event = wait_for_trajectory_event(&mut rx, chat_id).await;
        assert_eq!(event.event_type, "updated");
        assert_eq!(event.title.as_deref(), Some(expected_title));
        assert_eq!(event.is_title_generated, Some(true));
        assert_eq!(event.total_prompt_tokens, Some(24));
        assert_eq!(event.total_completion_tokens, Some(12));
        assert_eq!(event.total_tokens, Some(36));
        assert_eq!(event.total_cache_read_tokens, Some(7));
        assert_eq!(event.total_cache_creation_tokens, Some(8));
        assert!((event.total_cost_usd.unwrap() - 0.02).abs() < 1e-9);
    }

    #[tokio::test]
    async fn file_only_title_generation_update_preserves_line_change_totals() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;
        let mut rx = app.chat.trajectory_events_tx.subscribe();
        let chat_id = "title-file-only-line-totals";
        let path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        let expected_title = "Line rich title request";
        let diff_content = serde_json::to_string(&json!([
            {"lines_add": "a\nb\n", "lines_remove": "old\n"},
            {"lines_add": "c\n", "lines_remove": "old2\nold3\n"}
        ]))
        .unwrap();
        let mut trajectory = sample_trajectory(chat_id, "New Chat", "2024-01-01T00:00:01Z");
        trajectory["messages"] = json!([
            {"role": "user", "content": expected_title},
            {"role": "diff", "content": diff_content}
        ]);
        tokio::fs::create_dir_all(path.parent().unwrap())
            .await
            .unwrap();
        tokio::fs::write(&path, serde_json::to_string(&trajectory).unwrap())
            .await
            .unwrap();

        spawn_title_generation_task(
            gcx,
            chat_id.to_string(),
            vec![json!({"role":"user","content":expected_title})],
            path.clone(),
            TrajectorySourceIdentity::Normal,
        );

        wait_for_file_title(&path, expected_title).await;
        let event = wait_for_trajectory_event(&mut rx, chat_id).await;
        assert_eq!(event.title.as_deref(), Some(expected_title));
        assert_eq!(event.total_lines_added, Some(3));
        assert_eq!(event.total_lines_removed, Some(3));
    }

    #[tokio::test]
    async fn task_file_only_title_generation_update_includes_context_and_progress() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;
        let mut rx = app.chat.trajectory_events_tx.subscribe();
        let chat_id = "title-task-file-only-progress";
        let task_meta = task_meta(
            "task-title-progress",
            "agents",
            Some("agent-progress"),
            Some("card-progress"),
            None,
        );
        let path = dir
            .path()
            .join(".refact")
            .join("tasks")
            .join("task-title-progress")
            .join("trajectories")
            .join("agents")
            .join("agent-progress")
            .join(format!("{chat_id}.json"));
        let expected_title = "Task progress title request";
        let tasks_args = serde_json::to_string(&json!({
            "tasks": [
                {"title": "done task", "status": "completed"},
                {"title": "failed task", "status": "failed"},
                {"title": "todo task", "status": "pending"}
            ]
        }))
        .unwrap();
        let mut trajectory = sample_trajectory(chat_id, "New Chat", "2024-01-01T00:00:01Z");
        trajectory["mode"] = json!("task_agent");
        trajectory["task_meta"] = serde_json::to_value(&task_meta).unwrap();
        trajectory["messages"] = json!([
            {"role": "user", "content": expected_title},
            {
                "role": "assistant",
                "content": "setting tasks",
                "tool_calls": [{
                    "id": "tasks-call-1",
                    "function": {"name": "tasks_set", "arguments": tasks_args}
                }]
            },
            {"role": "tool", "tool_call_id": "tasks-call-1", "content": "ok"}
        ]);
        tokio::fs::create_dir_all(path.parent().unwrap())
            .await
            .unwrap();
        tokio::fs::write(&path, serde_json::to_string(&trajectory).unwrap())
            .await
            .unwrap();

        spawn_title_generation_task(
            gcx,
            chat_id.to_string(),
            vec![json!({"role":"user","content":expected_title})],
            path.clone(),
            TrajectorySourceIdentity::from_task_meta(&task_meta),
        );

        wait_for_file_title(&path, expected_title).await;
        let event = wait_for_trajectory_event(&mut rx, chat_id).await;
        assert_eq!(event.event_type, "updated");
        assert_eq!(event.title.as_deref(), Some(expected_title));
        assert_eq!(event.task_id.as_deref(), Some("task-title-progress"));
        assert_eq!(event.task_role.as_deref(), Some("agents"));
        assert_eq!(event.agent_id.as_deref(), Some("agent-progress"));
        assert_eq!(event.card_id.as_deref(), Some("card-progress"));
        assert_eq!(event.tasks_total, Some(3));
        assert_eq!(event.tasks_done, Some(1));
        assert_eq!(event.tasks_failed, Some(1));
    }

    #[tokio::test]
    async fn buddy_file_only_title_generation_emits_no_generic_sse() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;
        let mut rx = app.chat.trajectory_events_tx.subscribe();
        let chat_id = "title-buddy-file-only-no-sse";
        let buddy_dir = dir
            .path()
            .join(".refact")
            .join("buddy")
            .join("chats")
            .join("conversations");
        let path = buddy_dir.join(format!("{chat_id}.json"));
        let expected_title = "Buddy file-only title request";
        write_buddy_conversation_file(&path, chat_id, "New Chat").await;

        spawn_title_generation_task(
            gcx,
            chat_id.to_string(),
            vec![json!({"role":"user","content":expected_title})],
            path.clone(),
            TrajectorySourceIdentity::Buddy,
        );

        wait_for_file_title(&path, expected_title).await;
        assert_no_trajectory_event_for(&mut rx, std::time::Duration::from_millis(200)).await;
    }

    #[tokio::test]
    async fn normal_title_generation_with_active_task_updates_normal_file_only() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;
        let mut rx = app.chat.trajectory_events_tx.subscribe();
        let chat_id = "title-normal-active-task";
        let trajectories_dir = dir.path().join(".refact").join("trajectories");
        let path = trajectories_dir.join(format!("{chat_id}.json"));
        let expected_title = "Normal collision request";
        write_trajectory_file_with_user_message(&path, chat_id, "New Chat", expected_title).await;
        let active_task_meta = task_meta("task-active-title", "planner", None, None, Some(chat_id));
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        {
            let mut session = session_arc.lock().await;
            session.thread.title = "Task Session Title".to_string();
            session.thread.task_meta = Some(active_task_meta);
            session.thread.is_title_generated = false;
            session.trajectory_events_tx = Some(app.chat.trajectory_events_tx.clone());
        }
        app.chat
            .sessions
            .write()
            .await
            .insert(chat_id.to_string(), session_arc.clone());

        spawn_title_generation_task(
            gcx,
            chat_id.to_string(),
            vec![json!({"role":"user","content":expected_title})],
            path.clone(),
            TrajectorySourceIdentity::Normal,
        );

        wait_for_file_title(&path, expected_title).await;
        let event = wait_for_trajectory_event(&mut rx, chat_id).await;
        assert_eq!(event.event_type, "updated");
        assert_eq!(event.title.as_deref(), Some(expected_title));
        assert_eq!(event.task_id, None);
        let session = session_arc.lock().await;
        assert_eq!(session.thread.title, "Task Session Title");
        assert!(!session.thread.is_title_generated);
    }

    #[tokio::test]
    async fn generic_title_generation_with_active_generic_updates_session() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;
        let mut rx = app.chat.trajectory_events_tx.subscribe();
        let chat_id = "title-generic-active-generic";
        let trajectories_dir = dir.path().join(".refact").join("trajectories");
        let path = trajectories_dir.join(format!("{chat_id}.json"));
        let expected_title = "Generic active request";
        write_trajectory_file_with_user_message(&path, chat_id, "New Chat", expected_title).await;
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        {
            let mut session = session_arc.lock().await;
            session.thread.title = "New Chat".to_string();
            session.thread.is_title_generated = false;
            session.messages.push(ChatMessage::new(
                "user".to_string(),
                expected_title.to_string(),
            ));
            session.trajectory_events_tx = Some(app.chat.trajectory_events_tx.clone());
        }
        app.chat
            .sessions
            .write()
            .await
            .insert(chat_id.to_string(), session_arc.clone());

        spawn_title_generation_task(
            gcx,
            chat_id.to_string(),
            vec![json!({"role":"user","content":expected_title})],
            path.clone(),
            TrajectorySourceIdentity::Normal,
        );

        wait_for_session_title(&session_arc, expected_title).await;
        wait_for_file_title(&path, expected_title).await;
        let event = wait_for_trajectory_event(&mut rx, chat_id).await;
        assert_eq!(event.title.as_deref(), Some(expected_title));
    }

    #[tokio::test]
    async fn task_title_generation_with_matching_active_task_updates_session() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;
        let chat_id = "title-task-active-task";
        let task_meta = task_meta(
            "task-title-active",
            "agents",
            Some("agent-2"),
            Some("card-2"),
            None,
        );
        let path = dir
            .path()
            .join(".refact")
            .join("tasks")
            .join("task-title-active")
            .join("trajectories")
            .join("agents")
            .join("agent-2")
            .join(format!("{chat_id}.json"));
        let expected_title = "Task active request";
        write_task_trajectory_file_with_user_message(
            &path,
            chat_id,
            "New Chat",
            expected_title,
            &task_meta,
        )
        .await;
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        {
            let mut session = session_arc.lock().await;
            session.thread.title = "New Chat".to_string();
            session.thread.task_meta = Some(task_meta.clone());
            session.thread.is_title_generated = false;
            session.messages.push(ChatMessage::new(
                "user".to_string(),
                expected_title.to_string(),
            ));
            session.trajectory_events_tx = Some(app.chat.trajectory_events_tx.clone());
        }
        app.chat
            .sessions
            .write()
            .await
            .insert(chat_id.to_string(), session_arc.clone());

        spawn_title_generation_task(
            gcx,
            chat_id.to_string(),
            vec![json!({"role":"user","content":expected_title})],
            path.clone(),
            TrajectorySourceIdentity::from_task_meta(&task_meta),
        );

        wait_for_session_title(&session_arc, expected_title).await;
        wait_for_file_title(&path, expected_title).await;
    }

    #[tokio::test]
    async fn buddy_title_generation_with_active_buddy_updates_session_without_generic_sse() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;
        let mut rx = app.chat.trajectory_events_tx.subscribe();
        let chat_id = "title-buddy-active-buddy";
        let buddy_dir = dir
            .path()
            .join(".refact")
            .join("buddy")
            .join("chats")
            .join("conversations");
        let path = buddy_dir.join(format!("{chat_id}.json"));
        let expected_title = "Buddy active request";
        write_buddy_conversation_file(&path, chat_id, "New Chat").await;
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        {
            let mut session = session_arc.lock().await;
            session.thread.title = "New Chat".to_string();
            session.thread.mode = "buddy".to_string();
            session.thread.buddy_meta = Some(buddy_thread_meta());
            session.thread.is_title_generated = false;
            session.messages.push(ChatMessage::new(
                "user".to_string(),
                expected_title.to_string(),
            ));
            session.trajectory_events_tx = Some(app.chat.trajectory_events_tx.clone());
        }
        app.chat
            .sessions
            .write()
            .await
            .insert(chat_id.to_string(), session_arc.clone());

        spawn_title_generation_task(
            gcx,
            chat_id.to_string(),
            vec![json!({"role":"user","content":expected_title})],
            path.clone(),
            TrajectorySourceIdentity::Buddy,
        );

        wait_for_session_title(&session_arc, expected_title).await;
        wait_for_file_title(&path, expected_title).await;
        assert_no_trajectory_event_for(&mut rx, std::time::Duration::from_millis(200)).await;
    }

    #[tokio::test]
    async fn active_buddy_title_generation_uses_set_title_side_effects_without_generic_sse() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;
        let mut trajectory_rx = app.chat.trajectory_events_tx.subscribe();
        let chat_id = "title-buddy-set-title-event";
        let buddy_dir = dir
            .path()
            .join(".refact")
            .join("buddy")
            .join("chats")
            .join("conversations");
        let path = buddy_dir.join(format!("{chat_id}.json"));
        let expected_title = "Buddy event request";
        write_buddy_conversation_file(&path, chat_id, "New Chat").await;
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        {
            let mut session = session_arc.lock().await;
            session.thread.title = "New Chat".to_string();
            session.thread.mode = "buddy".to_string();
            session.thread.buddy_meta = Some(buddy_thread_meta());
            session.thread.is_title_generated = false;
            session.messages.push(ChatMessage::new(
                "user".to_string(),
                expected_title.to_string(),
            ));
            session.trajectory_events_tx = Some(app.chat.trajectory_events_tx.clone());
        }
        app.chat
            .sessions
            .write()
            .await
            .insert(chat_id.to_string(), session_arc.clone());

        spawn_title_generation_task(
            gcx,
            chat_id.to_string(),
            vec![json!({"role":"user","content":expected_title})],
            path.clone(),
            TrajectorySourceIdentity::Buddy,
        );

        wait_for_session_title(&session_arc, expected_title).await;
        {
            let session = session_arc.lock().await;
            assert!(session.trajectory_version > 0);
        }
        assert_no_trajectory_event_for(&mut trajectory_rx, std::time::Duration::from_millis(200))
            .await;
    }

    #[tokio::test]
    async fn title_generation_backing_file_matcher_rejects_source_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        let chat_id = "title-source-match";
        let generic_path = dir.path().join("generic.json");
        let buddy_path = dir.path().join("buddy.json");
        let task_path = dir.path().join("task.json");
        let invalid_path = dir.path().join("invalid.json");
        let task_meta = task_meta(
            "task-title-source",
            "agents",
            Some("agent-title-source"),
            Some("card-title-source"),
            None,
        );
        let task_source = TrajectorySourceIdentity::from_task_meta(&task_meta);
        write_trajectory_file(
            &generic_path,
            chat_id,
            "Generic Title",
            "2024-01-01T00:00:00Z",
        )
        .await;
        write_buddy_conversation_file(&buddy_path, chat_id, "Buddy Title").await;
        write_task_trajectory_file_with_user_message(
            &task_path,
            chat_id,
            "Task Title",
            "task title source",
            &task_meta,
        )
        .await;
        let mut invalid = sample_trajectory(chat_id, "Invalid", "2024-01-01T00:00:00Z");
        invalid["task_meta"] = serde_json::to_value(&task_meta).unwrap();
        invalid["buddy_meta"] = json!({"is_buddy_chat": true, "buddy_chat_kind": "investigation"});
        tokio::fs::write(&invalid_path, serde_json::to_string(&invalid).unwrap())
            .await
            .unwrap();

        assert!(
            title_generation_backing_file_matches(
                &generic_path,
                chat_id,
                &TrajectorySourceIdentity::Normal,
            )
            .await
        );
        assert!(
            title_generation_backing_file_matches(
                &buddy_path,
                chat_id,
                &TrajectorySourceIdentity::Buddy,
            )
            .await
        );
        assert!(title_generation_backing_file_matches(&task_path, chat_id, &task_source).await);
        assert!(
            !title_generation_backing_file_matches(
                &generic_path,
                chat_id,
                &TrajectorySourceIdentity::Buddy,
            )
            .await
        );
        assert!(
            !title_generation_backing_file_matches(
                &buddy_path,
                chat_id,
                &TrajectorySourceIdentity::Normal,
            )
            .await
        );
        assert!(
            !title_generation_backing_file_matches(
                &task_path,
                chat_id,
                &TrajectorySourceIdentity::Normal,
            )
            .await
        );
        assert!(!title_generation_backing_file_matches(&invalid_path, chat_id, &task_source).await);
    }

    #[test]
    fn trajectory_id_mismatch_list_hydration_recheck_rejects_changed_data() {
        let candidate = TrajectoryListCandidate {
            id: "hydration-chat".to_string(),
            updated_at: "2024-01-01T00:00:00Z".to_string(),
            path: PathBuf::from("hydration-chat.json"),
            indexed_meta: None,
            indexed_file_len: None,
            indexed_file_modified_unix_ms: None,
        };
        let data = TrajectoryData {
            id: "other-chat".to_string(),
            title: "Changed".to_string(),
            created_at: "2024-01-01T00:00:00Z".to_string(),
            updated_at: "2024-01-01T00:00:01Z".to_string(),
            model: "model".to_string(),
            mode: "agent".to_string(),
            tool_use: "agent".to_string(),
            messages: Vec::new(),
            extra: serde_json::Map::new(),
        };

        assert!(!trajectory_list_candidate_matches_hydrated_data(
            &candidate, &data
        ));
    }

