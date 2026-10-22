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
    async fn repair_source_outside_approved_roots_errors_without_writing() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let chat_id = "repair-source-outside-root";
        let outside_path = outside.path().join(format!("{chat_id}.json"));
        write_trajectory_file(
            &outside_path,
            chat_id,
            "Outside Repair Source",
            "2024-01-01T00:00:00Z",
        )
        .await;
        let before = tokio::fs::read_to_string(&outside_path).await.unwrap();
        let repair_patch = TrajectoryRepairPatch {
            chat_id: chat_id.to_string(),
            source_path: outside_path.clone(),
            created_at: "2024-01-01T00:00:00Z".to_string(),
            frozen_request_prefix: None,
            auto_approve_editing_tools: true,
            auto_approve_dangerous_commands: true,
        };

        let err = persist_loaded_trajectory_repair_raw(gcx, &repair_patch)
            .await
            .unwrap_err();

        assert!(err.contains("not in an approved trajectory root"));
        assert_eq!(
            tokio::fs::read_to_string(outside_path).await.unwrap(),
            before
        );
    }

    #[tokio::test]
    async fn mode_transition_repair_persists_to_loaded_task_path_with_normal_collision() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let chat_id = "transition-task-normal-collision-repair";
        let task_path = dir
            .path()
            .join(".refact")
            .join("tasks")
            .join("task-loaded-repair")
            .join("trajectories")
            .join("agents")
            .join("agent-1")
            .join(format!("{chat_id}.json"));
        tokio::fs::create_dir_all(task_path.parent().unwrap())
            .await
            .unwrap();
        tokio::fs::write(
            &task_path,
            serde_json::to_string(&json!({
                "id": chat_id,
                "title": "Task Collision Repair",
                "model": "model",
                "mode": "task_planner",
                "tool_use": "agent",
                "parent_id": "source-chat",
                "link_type": "mode_transition",
                "previous_response_id": "task-response",
                "messages": [
                    {"role":"system","content":"target task planner system"},
                    {"role":"user","content":"hello"}
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
                "updated_at": "2024-01-01T00:00:00Z",
                "include_project_info": true,
                "checkpoints_enabled": true,
                "task_meta": {
                    "task_id": "task-loaded-repair",
                    "role": "agents",
                    "agent_id": "agent-1",
                    "card_id": "T-1"
                },
                "custom_future_field": {"task": true}
            }))
            .unwrap(),
        )
        .await
        .unwrap();

        let mut loaded = load_trajectory_for_chat(gcx.clone(), chat_id)
            .await
            .unwrap();
        assert_same_path(&loaded.source_path, &task_path);
        assert!(loaded.transition_identity_repaired);
        apply_mode_defaults_to_thread(
            gcx.clone(),
            &mut loaded.thread,
            loaded.auto_approve_editing_tools_present,
            loaded.auto_approve_dangerous_commands_present,
        )
        .await;
        let repair_patch = loaded.repair_patch();

        let normal_path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        tokio::fs::create_dir_all(normal_path.parent().unwrap())
            .await
            .unwrap();
        tokio::fs::write(
            &normal_path,
            serde_json::to_string(&json!({
                "id": chat_id,
                "title": "Normal Collision Must Stay",
                "model": "model",
                "mode": "agent",
                "tool_use": "agent",
                "messages": [],
                "created_at": "2024-01-01T00:00:00Z",
                "updated_at": "2024-01-01T00:00:00Z",
                "include_project_info": true,
                "checkpoints_enabled": true,
                "previous_response_id": "normal-response",
                "custom_future_field": {"normal": true}
            }))
            .unwrap(),
        )
        .await
        .unwrap();
        let normal_before = tokio::fs::read_to_string(&normal_path).await.unwrap();

        persist_loaded_trajectory_repair_raw(gcx.clone(), &repair_patch)
            .await
            .unwrap();

        assert_eq!(
            tokio::fs::read_to_string(&normal_path).await.unwrap(),
            normal_before
        );
        let task_raw: serde_json::Value =
            serde_json::from_str(&tokio::fs::read_to_string(&task_path).await.unwrap()).unwrap();
        assert_eq!(
            task_raw["frozen_request_prefix"]["system_prompt"],
            "target task planner system"
        );
        assert!(task_raw["frozen_request_prefix"]["tools_canonical"].is_null());
        assert!(task_raw.get("claude_code_identity").is_none());
        assert!(task_raw.get("previous_response_id").is_none());
        assert_eq!(task_raw["custom_future_field"]["task"], true);
    }

    #[tokio::test]
    async fn handoff_invalid_or_missing_created_at_repairs_and_normalizes_on_open() {
        let dir = tempfile::tempdir().unwrap();
        let (_gcx, app) = make_app_with_workspace(dir.path()).await;
        let trajectories_dir = dir.path().join(".refact").join("trajectories");
        tokio::fs::create_dir_all(&trajectories_dir).await.unwrap();

        for (chat_id, created_at) in [
            ("handoff-invalid-created-at-open", Some(json!("not-a-date"))),
            ("handoff-missing-created-at-open", None),
        ] {
            let path = trajectories_dir.join(format!("{chat_id}.json"));
            let mut payload = json!({
                "id": chat_id,
                "title": "Handoff Open",
                "model": "model",
                "mode": "agent",
                "tool_use": "agent",
                "parent_id": "source-chat",
                "link_type": "handoff",
                "previous_response_id": "resp_source",
                "messages": [
                    {"role":"system","content":"target handoff system"},
                    {"role":"user","content":"hello"}
                ],
                "frozen_request_prefix": {
                    "schema_version": 1,
                    "created_at": "2024-01-03T00:00:00Z",
                    "system_prompt": "target handoff system",
                    "tools_canonical": [{"type":"function","function":{"name":"source_tool"}}]
                },
                "claude_code_identity": {
                    "device_id":"source-device",
                    "session_id":"source-session"
                },
                "updated_at": "2024-01-01T00:00:00Z",
                "include_project_info": true,
                "checkpoints_enabled": true
            });
            if let Some(created_at) = created_at {
                payload["created_at"] = created_at;
            }
            tokio::fs::write(&path, serde_json::to_string(&payload).unwrap())
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
                let prefix = session.thread.frozen_request_prefix.as_ref().unwrap();
                assert_eq!(
                    prefix.system_prompt.as_deref(),
                    Some("target handoff system")
                );
                assert!(prefix.tools_canonical.is_none());
                assert!(session.thread.claude_code_identity.is_none());
                assert!(session.thread.previous_response_id.is_none());
                assert!(parsed_rfc3339_utc(&session.created_at).is_some());
            }

            let raw: serde_json::Value =
                serde_json::from_str(&tokio::fs::read_to_string(&path).await.unwrap()).unwrap();
            let saved_created_at = raw["created_at"].as_str().unwrap();
            assert!(parsed_rfc3339_utc(saved_created_at).is_some());
            assert_ne!(saved_created_at, "not-a-date");
            assert_eq!(
                raw["frozen_request_prefix"]["system_prompt"],
                "target handoff system"
            );
            assert!(raw["frozen_request_prefix"]["tools_canonical"].is_null());
            assert!(raw.get("claude_code_identity").is_none());
            assert!(raw.get("previous_response_id").is_none());
        }
    }

    #[test]
    fn frozen_lazy_ensure_noops_on_empty_inputs_and_preserves_existing_prefix() {
        let mut session = ChatSession::new("lazy-frozen".to_string());
        assert!(ensure_frozen_prefix(&mut session, None, None).is_none());
        assert!(session.thread.frozen_request_prefix.is_none());
        assert_eq!(session.trajectory_version, 0);

        let no_tools = ensure_frozen_prefix(
            &mut session,
            Some("system no tools".to_string()),
            Some(json!([])),
        )
        .expect("known no-tools prefix should be installed");
        assert_eq!(no_tools.tools_canonical, Some(json!([])));
        assert!(frozen_prefix_is_complete(&no_tools));
        let version_after_no_tools = session.trajectory_version;

        let with_tools = ensure_frozen_prefix(
            &mut session,
            Some("system with tools".to_string()),
            Some(json!([{"type":"function","function":{"name":"cat"}}])),
        );
        assert!(with_tools.is_none());
        assert_eq!(session.trajectory_version, version_after_no_tools);
        assert_eq!(
            session
                .thread
                .frozen_request_prefix
                .as_ref()
                .and_then(|prefix| prefix.tools_canonical.clone()),
            Some(json!([]))
        );

        let mut session = ChatSession::new("lazy-frozen".to_string());

        let first = ensure_frozen_prefix(
            &mut session,
            Some("system one".to_string()),
            Some(json!([{"type":"function","function":{"name":"cat"}}])),
        )
        .expect("missing prefix should be installed");

        assert_eq!(first.schema_version, 1);
        assert_eq!(first.system_prompt.as_deref(), Some("system one"));
        assert!(session.thread.frozen_request_prefix.is_some());
        let version_after_first = session.trajectory_version;

        let second = ensure_frozen_prefix(
            &mut session,
            Some("system two".to_string()),
            Some(json!([{"type":"function","function":{"name":"shell"}}])),
        );
        assert!(second.is_none());
        assert_eq!(session.trajectory_version, version_after_first);
        let prefix = session.thread.frozen_request_prefix.as_ref().unwrap();
        assert_eq!(prefix.system_prompt.as_deref(), Some("system one"));
        assert_eq!(
            prefix.tools_canonical,
            Some(json!([{"type":"function","function":{"name":"cat"}}]))
        );
    }

    #[test]
    fn frozen_lazy_ensure_fills_partial_prefix_once() {
        let mut session = ChatSession::new("partial-frozen".to_string());
        let partial = ensure_frozen_prefix(&mut session, Some("system only".to_string()), None)
            .expect("partial prefix should be installed");
        assert_eq!(partial.system_prompt.as_deref(), Some("system only"));
        assert!(partial.tools_canonical.is_none());
        let version_after_partial = session.trajectory_version;

        let filled = ensure_frozen_prefix(
            &mut session,
            Some("replacement system".to_string()),
            Some(json!([{"type":"function","function":{"name":"cat"}}])),
        )
        .expect("partial prefix should be filled");
        assert_eq!(filled.system_prompt.as_deref(), Some("system only"));
        assert_eq!(
            filled.tools_canonical,
            Some(json!([{"type":"function","function":{"name":"cat"}}]))
        );
        assert_eq!(session.trajectory_version, version_after_partial + 1);

        let version_after_fill = session.trajectory_version;
        assert!(ensure_frozen_prefix(
            &mut session,
            Some("new system".to_string()),
            Some(json!([{"type":"function","function":{"name":"shell"}}])),
        )
        .is_none());
        assert_eq!(session.trajectory_version, version_after_fill);
        let prefix = session.thread.frozen_request_prefix.as_ref().unwrap();
        assert_eq!(prefix.system_prompt.as_deref(), Some("system only"));
        assert_eq!(
            prefix.tools_canonical,
            Some(json!([{"type":"function","function":{"name":"cat"}}]))
        );
    }

    #[test]
    fn frozen_complete_existing_prefix_is_never_overwritten() {
        let mut session = ChatSession::new("complete-frozen".to_string());
        let original = FrozenRequestPrefix {
            schema_version: 1,
            created_at: "2026-05-29T00:00:00Z".to_string(),
            system_prompt: Some("original system".to_string()),
            tools_canonical: Some(json!([])),
        };
        session.thread.frozen_request_prefix = Some(original.clone());

        let result = ensure_frozen_prefix(
            &mut session,
            Some("replacement system".to_string()),
            Some(json!([{"type":"function","function":{"name":"cat"}}])),
        );

        assert!(result.is_none());
        assert_eq!(session.thread.frozen_request_prefix, Some(original));
        assert_eq!(session.trajectory_version, 0);
    }

    #[test]
    fn frozen_racy_persist_prefix_helper_is_not_available_in_production() {
        let _helper = persist_frozen_prefix;
    }

    #[tokio::test]
    async fn frozen_legacy_normal_session_migrates_through_session_save() {
        let dir = tempfile::tempdir().unwrap();
        let (_gcx, app) = make_app_with_workspace(dir.path()).await;
        let chat_id = "legacy-freeze-normal";
        let trajectories_dir = dir.path().join(".refact").join("trajectories");
        tokio::fs::create_dir_all(&trajectories_dir).await.unwrap();
        tokio::fs::write(
            trajectories_dir.join(format!("{chat_id}.json")),
            serde_json::to_string(&json!({
                "id": chat_id,
                "title": "Legacy",
                "model": "test/model",
                "mode": "agent",
                "tool_use": "agent",
                "messages": [
                    {"role":"system","content":"legacy system"},
                    {"role":"user","content":"hello"}
                ],
                "created_at": "2024-01-01T00:00:00Z",
                "updated_at": "2024-01-01T00:00:00Z",
                "include_project_info": true,
                "checkpoints_enabled": true
            }))
            .unwrap(),
        )
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
            let prefix = session.thread.frozen_request_prefix.as_ref().unwrap();
            assert_eq!(prefix.system_prompt.as_deref(), Some("legacy system"));
            assert!(prefix.tools_canonical.is_none());
        }

        let raw: serde_json::Value = serde_json::from_str(
            &tokio::fs::read_to_string(trajectories_dir.join(format!("{chat_id}.json")))
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            raw["frozen_request_prefix"]["system_prompt"],
            "legacy system"
        );
        assert!(raw["frozen_request_prefix"]["tools_canonical"].is_null());
        assert_eq!(raw["messages"][0]["content"], "legacy system");
    }

    #[tokio::test]
    async fn frozen_legacy_unknown_tools_can_be_completed_once() {
        let dir = tempfile::tempdir().unwrap();
        let (_gcx, app) = make_app_with_workspace(dir.path()).await;
        let chat_id = "legacy-freeze-complete-once";
        let trajectories_dir = dir.path().join(".refact").join("trajectories");
        tokio::fs::create_dir_all(&trajectories_dir).await.unwrap();
        tokio::fs::write(
            trajectories_dir.join(format!("{chat_id}.json")),
            serde_json::to_string(&json!({
                "id": chat_id,
                "title": "Legacy",
                "model": "test/model",
                "mode": "agent",
                "tool_use": "agent",
                "messages": [
                    {"role":"system","content":"legacy system"},
                    {"role":"user","content":"hello"}
                ],
                "created_at": "2024-01-01T00:00:00Z",
                "updated_at": "2024-01-01T00:00:00Z",
                "include_project_info": true,
                "checkpoints_enabled": true
            }))
            .unwrap(),
        )
        .await
        .unwrap();
        let session_arc = crate::chat::get_or_create_session_with_trajectory(
            app.clone(),
            &app.chat.sessions,
            chat_id,
        )
        .await;

        let mut session = session_arc.lock().await;
        let completed = ensure_frozen_prefix(
            &mut session,
            Some("replacement system".to_string()),
            Some(json!([{"type":"function","function":{"name":"cat"}}])),
        )
        .expect("legacy unknown tools prefix should complete once");
        assert_eq!(completed.system_prompt.as_deref(), Some("legacy system"));
        assert_eq!(
            completed.tools_canonical,
            Some(json!([{"type":"function","function":{"name":"cat"}}]))
        );
        let version_after_complete = session.trajectory_version;

        assert!(ensure_frozen_prefix(
            &mut session,
            Some("new system".to_string()),
            Some(json!([{"type":"function","function":{"name":"shell"}}])),
        )
        .is_none());
        assert_eq!(session.trajectory_version, version_after_complete);
        assert_eq!(
            session
                .thread
                .frozen_request_prefix
                .as_ref()
                .and_then(|prefix| prefix.tools_canonical.clone()),
            Some(json!([{"type":"function","function":{"name":"cat"}}]))
        );
    }

    #[tokio::test]
    async fn frozen_legacy_task_session_migration_does_not_create_generic_copy() {
        let dir = tempfile::tempdir().unwrap();
        let (_gcx, app) = make_app_with_workspace(dir.path()).await;
        let task_id = "task-legacy-freeze";
        let agent_id = "agent-1";
        let chat_id = "legacy-task-freeze";
        let task_path = dir
            .path()
            .join(".refact")
            .join("tasks")
            .join(task_id)
            .join("trajectories")
            .join("agents")
            .join(agent_id)
            .join(format!("{chat_id}.json"));
        tokio::fs::create_dir_all(task_path.parent().unwrap())
            .await
            .unwrap();
        tokio::fs::write(
            &task_path,
            serde_json::to_string(&json!({
                "id": chat_id,
                "title": "Legacy Task",
                "model": "model",
                "mode": "task_agent",
                "tool_use": "agent",
                "messages": [
                    {"role":"system","content":"task system"},
                    {"role":"user","content":"hello task"}
                ],
                "created_at": "2024-01-01T00:00:00Z",
                "updated_at": "2024-01-01T00:00:00Z",
                "include_project_info": true,
                "checkpoints_enabled": true,
                "task_meta": {
                    "task_id": task_id,
                    "role": "agents",
                    "agent_id": agent_id,
                    "card_id": "T-1"
                }
            }))
            .unwrap(),
        )
        .await
        .unwrap();
        tokio::fs::create_dir_all(dir.path().join(".refact").join("tasks").join(task_id))
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
            let prefix = session.thread.frozen_request_prefix.as_ref().unwrap();
            assert_eq!(prefix.system_prompt.as_deref(), Some("task system"));
            assert!(prefix.tools_canonical.is_none());
        }

        let generic_path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        let raw: serde_json::Value =
            serde_json::from_str(&tokio::fs::read_to_string(&task_path).await.unwrap()).unwrap();
        assert!(!tokio::fs::try_exists(generic_path).await.unwrap());
        assert_eq!(raw["frozen_request_prefix"]["system_prompt"], "task system");
        assert!(raw["frozen_request_prefix"]["tools_canonical"].is_null());
    }

    #[tokio::test]
    async fn frozen_empty_normal_snapshot_with_prefix_is_saved() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let mut snapshot = test_snapshot("empty-frozen-normal", "Frozen Empty", vec![]);
        snapshot.frozen_request_prefix = Some(new_frozen_request_prefix(
            Some("empty snapshot system".to_string()),
            json!([]),
        ));

        save_trajectory_snapshot(gcx, snapshot).await.unwrap();

        let path = nested_trajectory_path(dir.path(), "empty-frozen-normal", "empty-frozen-normal");
        let raw: serde_json::Value =
            serde_json::from_str(&tokio::fs::read_to_string(path).await.unwrap()).unwrap();
        assert_eq!(
            raw["frozen_request_prefix"]["system_prompt"],
            "empty snapshot system"
        );
        assert_eq!(raw["messages"].as_array().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn trajectory_updated_at_changes_when_title_changes_without_messages() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;

        let chat_id = "updated-at-title-change";
        let messages = vec![ChatMessage::new("user".to_string(), "Hello".to_string())];
        save_trajectory_snapshot(
            gcx.clone(),
            test_snapshot(chat_id, "First", messages.clone()),
        )
        .await
        .unwrap();
        let path = nested_trajectory_path(dir.path(), chat_id, chat_id);
        let first_raw: serde_json::Value =
            serde_json::from_str(&tokio::fs::read_to_string(&path).await.unwrap()).unwrap();
        let first_updated_at = first_raw["updated_at"].as_str().unwrap().to_string();

        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        save_trajectory_snapshot(gcx, test_snapshot(chat_id, "Retitled", messages))
            .await
            .unwrap();
        let retitled_raw: serde_json::Value =
            serde_json::from_str(&tokio::fs::read_to_string(&path).await.unwrap()).unwrap();
        assert_ne!(
            retitled_raw["updated_at"].as_str().unwrap(),
            first_updated_at
        );
    }

    #[tokio::test]
    async fn trajectory_updated_at_changes_when_worktree_changes() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;

        let chat_id = "updated-at-worktree-change";
        let messages = vec![ChatMessage::new("user".to_string(), "Hello".to_string())];
        save_trajectory_snapshot(
            gcx.clone(),
            test_snapshot(chat_id, "Title", messages.clone()),
        )
        .await
        .unwrap();
        let path = nested_trajectory_path(dir.path(), chat_id, chat_id);
        let first_raw: serde_json::Value =
            serde_json::from_str(&tokio::fs::read_to_string(&path).await.unwrap()).unwrap();
        let first_updated_at = first_raw["updated_at"].as_str().unwrap().to_string();

        let mut snapshot = test_snapshot(chat_id, "Title", messages);
        snapshot.worktree = Some(trajectory_worktree_sample());
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        save_trajectory_snapshot(gcx, snapshot).await.unwrap();
        let changed_raw: serde_json::Value =
            serde_json::from_str(&tokio::fs::read_to_string(&path).await.unwrap()).unwrap();
        assert_ne!(
            changed_raw["updated_at"].as_str().unwrap(),
            first_updated_at
        );
    }

    #[tokio::test]
    async fn trajectory_persistence_preserves_ui_only_messages() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let mut session = ChatSession::new("ui-only-roundtrip".to_string());
        session.created_at = "2024-01-01T00:00:00Z".to_string();
        session.add_message(ChatMessage::new("user".to_string(), "hello".to_string()));
        session.add_message(make_ui_only_error_message("context_length_exceeded"));

        let snapshot = trajectory_snapshot_from_session(&session);
        save_trajectory_snapshot(gcx.clone(), snapshot)
            .await
            .unwrap();

        let loaded = load_trajectory_for_chat(gcx, "ui-only-roundtrip")
            .await
            .unwrap();
        assert_eq!(loaded.messages.len(), 2);
        assert_eq!(loaded.messages[0].role, "user");
        assert_eq!(loaded.messages[1].role, "error");
        assert!(is_ui_only_message(&loaded.messages[1]));
    }

    #[tokio::test]
    async fn cache_guard_snapshot_is_runtime_only_and_not_persisted() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let raw_request_marker = "raw-cache-guard-request-body-marker";
        let mut session = ChatSession::new("cache-guard-runtime-only".to_string());
        session.created_at = "2024-01-01T00:00:00Z".to_string();
        session.cache_guard_snapshot = Some(json!({
            "messages": [{"role": "user", "content": raw_request_marker}],
            "provider_specific_fields": {"semantic": "value"}
        }));
        session.add_message(ChatMessage::new("user".to_string(), "visible".to_string()));

        let snapshot = trajectory_snapshot_from_session(&session);
        let snapshot_messages_json = serde_json::to_string(&snapshot.messages).unwrap();
        assert!(!snapshot_messages_json.contains(raw_request_marker));

        save_trajectory_snapshot(gcx.clone(), snapshot)
            .await
            .unwrap();

        let path = nested_trajectory_path(
            dir.path(),
            "cache-guard-runtime-only",
            "cache-guard-runtime-only",
        );
        let saved_json = tokio::fs::read_to_string(path).await.unwrap();
        assert!(!saved_json.contains("cache_guard_snapshot"));
        assert!(!saved_json.contains(raw_request_marker));
    }

    #[tokio::test]
    async fn reactive_compact_attempts_is_runtime_only_not_persisted() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let mut snapshot = test_snapshot(
            "reactive-attempts-roundtrip",
            "Reactive Attempts",
            vec![ChatMessage::new("user".to_string(), "visible".to_string())],
        );
        snapshot.reactive_compact_attempts = Some(2);
        save_trajectory_snapshot(gcx.clone(), snapshot)
            .await
            .unwrap();

        let traj_path = nested_trajectory_path(
            dir.path(),
            "reactive-attempts-roundtrip",
            "reactive-attempts-roundtrip",
        );
        let saved = tokio::fs::read_to_string(&traj_path).await.unwrap();
        assert!(!saved.contains("reactive_compact_attempts"));

        let mut raw: serde_json::Value = serde_json::from_str(&saved).unwrap();
        raw["reactive_compact_attempts"] = json!(99);
        tokio::fs::write(&traj_path, serde_json::to_string(&raw).unwrap())
            .await
            .unwrap();

        let loaded = load_trajectory_for_chat(gcx, "reactive-attempts-roundtrip")
            .await
            .unwrap();
        assert_eq!(loaded.thread.reactive_compact_attempts, None);
    }

    #[tokio::test]
    async fn pending_deliveries_save_load_preserves_identity_and_clears_delivered() {
        use refact_core::chat_types::{PendingDelivery, PushMode};
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let mut session = ChatSession::new("pending-roundtrip".into());
        session.created_at = "2024-01-01T00:00:00Z".into();
        let mut delivery = PendingDelivery::with_id(
            "stable-id",
            vec![ChatMessage::new("user".into(), "queued".into())],
            PushMode::WhenIdle,
            "test",
            true,
        );
        delivery.enqueued_at_ms = 42;
        session.pending_deliveries.push_back(delivery.clone());
        assert!(!session_is_stale_refresh_candidate(&session, true));
        save_trajectory_snapshot(gcx.clone(), trajectory_snapshot_from_session(&session))
            .await
            .unwrap();
        let loaded = load_trajectory_for_chat(gcx.clone(), "pending-roundtrip")
            .await
            .unwrap();
        assert_eq!(loaded.pending_deliveries, vec![delivery.clone()]);
        let mut restored = ChatSession::new("pending-roundtrip".into());
        restored.restore_pending_deliveries(loaded.pending_deliveries);
        assert_eq!(
            restored.pending_deliveries_for_snapshot(),
            vec![delivery.clone()]
        );
        restored.pending_deliveries.clear();
        restored.messages = delivery.stamp_messages();
        restored.restore_pending_deliveries(vec![delivery]);
        assert!(restored.pending_deliveries.is_empty());
        save_trajectory_snapshot(gcx.clone(), trajectory_snapshot_from_session(&restored))
            .await
            .unwrap();
        assert!(load_trajectory_for_chat(gcx, "pending-roundtrip")
            .await
            .unwrap()
            .pending_deliveries
            .is_empty());
    }

    #[tokio::test]
    async fn trajectory_persistence_keeps_normal_error_messages() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let mut session = ChatSession::new("normal-error-roundtrip".to_string());
        session.created_at = "2024-01-01T00:00:00Z".to_string();
        session.add_message(ChatMessage::new("user".to_string(), "hello".to_string()));
        session.add_message(ChatMessage::new(
            "error".to_string(),
            "LLM failed".to_string(),
        ));

        let snapshot = trajectory_snapshot_from_session(&session);
        save_trajectory_snapshot(gcx.clone(), snapshot)
            .await
            .unwrap();

        let loaded = load_trajectory_for_chat(gcx, "normal-error-roundtrip")
            .await
            .unwrap();
        assert_eq!(loaded.messages.len(), 2);
        assert_eq!(loaded.messages[1].role, "error");
        assert_eq!(loaded.messages[1].content.content_text_only(), "LLM failed");
    }

    #[test]
    fn trajectory_save_uses_unique_tmp() {
        let file_path = PathBuf::from("chat.json");
        let first = unique_trajectory_tmp_path(&file_path);
        let second = unique_trajectory_tmp_path(&file_path);

        assert_ne!(first, second);
        assert_eq!(
            first
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap()
                .len(),
            22
        );
        assert!(first
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap()
            .starts_with("chat.json.tmp."));
        assert!(second
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap()
            .starts_with("chat.json.tmp."));
    }

    #[tokio::test]
    async fn trajectory_save_cleans_up_on_error() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("chat.json");
        let tmp_path = unique_trajectory_tmp_path(&file_path);
        tokio::fs::write(&tmp_path, "stale").await.unwrap();

        let err = atomic_write_json_with_tmp_path(
            &file_path,
            &tmp_path,
            Err("Failed to serialize trajectory: injected".to_string()),
            Some("Failed to write trajectory"),
            None,
        )
        .await
        .unwrap_err();

        assert_eq!(err, "Failed to serialize trajectory: injected");
        assert!(!tmp_path.exists());
        let leftovers = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|entry| entry.ok())
            .filter(|entry| {
                entry
                    .file_name()
                    .to_str()
                    .map(|name| name.contains(".tmp"))
                    .unwrap_or(false)
            })
            .count();
        assert_eq!(leftovers, 0);
    }

    #[serial(trajectory_perf)]
    #[tokio::test]
    async fn trajectory_watcher_self_write_consumes_exact_fingerprint_once() {
        clear_all_trajectory_self_writes();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("self-write.json");
        let content = "{\"id\":\"self-write\"}";
        let handle =
            register_trajectory_self_write(&path, TrajectorySourceIdentity::Normal, content)
                .await
                .unwrap();
        fs::write(&path, content).await.unwrap();
        complete_trajectory_self_write(Some(handle)).await;

        assert!(consume_trajectory_self_write(&path, &TrajectorySourceIdentity::Normal).await);
        assert!(!consume_trajectory_self_write(&path, &TrajectorySourceIdentity::Normal).await);
        clear_all_trajectory_self_writes();
    }

    #[serial(trajectory_perf)]
    #[tokio::test]
    async fn trajectory_watcher_self_write_does_not_consume_modified_or_deleted_files() {
        clear_all_trajectory_self_writes();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("external-write.json");
        let handle = register_trajectory_self_write(
            &path,
            TrajectorySourceIdentity::Normal,
            "{\"id\":\"external-write\"}",
        )
        .await
        .unwrap();
        fs::write(&path, "{\"id\":\"external-write\"}")
            .await
            .unwrap();
        complete_trajectory_self_write(Some(handle)).await;
        fs::write(&path, "{\"id\":\"external-write\",\"title\":\"external\"}")
            .await
            .unwrap();

        assert!(!consume_trajectory_self_write(&path, &TrajectorySourceIdentity::Normal).await);
        fs::remove_file(&path).await.unwrap();
        assert!(!consume_trajectory_self_write(&path, &TrajectorySourceIdentity::Normal).await);
        clear_all_trajectory_self_writes();
    }

    #[serial(trajectory_perf)]
    #[tokio::test]
    async fn trajectory_watcher_self_write_respects_source_and_expires_records() {
        clear_all_trajectory_self_writes();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("same-id.json");
        let normal = TrajectorySourceIdentity::Normal;
        let task = TrajectorySourceIdentity::task(
            "task-source".to_string(),
            "agents".to_string(),
            Some("agent-source".to_string()),
            None,
            None,
        );
        let content = "{\"id\":\"same-id\"}";
        let handle = register_trajectory_self_write(&path, normal.clone(), content)
            .await
            .unwrap();
        fs::write(&path, content).await.unwrap();
        complete_trajectory_self_write(Some(handle)).await;

        assert!(!consume_trajectory_self_write(&path, &task).await);
        {
            let mut registry = trajectory_self_writes()
                .lock()
                .expect("trajectory self-write registry poisoned");
            for records in registry.values_mut() {
                for record in records {
                    record.created_at = Instant::now() - TRAJECTORY_SELF_WRITE_TTL;
                }
            }
        }
        assert!(!consume_trajectory_self_write(&path, &normal).await);
        clear_all_trajectory_self_writes();
    }

    #[serial(trajectory_perf)]
    #[tokio::test]
    async fn trajectory_watcher_self_write_consumes_remove_after_atomic_replacement() {
        clear_all_trajectory_self_writes();
        let dir = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;
        let chat_id = "self-write-remove";
        let path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        write_trajectory_file_with_user_message(&path, chat_id, "Before Self Write", "before")
            .await;
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        {
            let mut session = session_arc.lock().await;
            session.thread.title = "Before Self Write".to_string();
            session
                .messages
                .push(ChatMessage::new("user".to_string(), "before".to_string()));
            session.trajectory_dirty = false;
        }
        app.chat
            .sessions
            .write()
            .await
            .insert(chat_id.to_string(), session_arc.clone());

        let content = tokio::fs::read_to_string(&path).await.unwrap();
        let tmp_path = unique_trajectory_tmp_path(&path);
        atomic_write_json_with_tmp_path(
            &path,
            &tmp_path,
            Ok(content),
            Some("Failed to write trajectory"),
            Some(TrajectorySourceIdentity::Normal),
        )
        .await
        .unwrap();
        process_trajectory_watcher_change(
            gcx,
            chat_id.to_string(),
            true,
            TrajectorySourceIdentity::Normal,
            path,
        )
        .await;

        let session = session_arc.lock().await;
        assert_eq!(session.thread.title, "Before Self Write");
        assert_eq!(session.messages.len(), 1);
        assert_eq!(session.external_reload_pending, None);
        drop(session);
        clear_all_trajectory_self_writes();
    }

    #[serial(trajectory_perf)]
    #[tokio::test]
    async fn trajectory_watcher_self_write_disable_restores_external_reload_path() {
        crate::runtime_settings::reset_for_test();
        clear_all_trajectory_self_writes();
        let dir = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;
        let chat_id = "self-write-disabled";
        let path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        write_trajectory_file_with_user_message(&path, chat_id, "Before Self Write", "before")
            .await;
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        {
            let mut session = session_arc.lock().await;
            session.thread.title = "Before Self Write".to_string();
            session
                .messages
                .push(ChatMessage::new("user".to_string(), "before".to_string()));
            session.trajectory_dirty = false;
        }
        app.chat
            .sessions
            .write()
            .await
            .insert(chat_id.to_string(), session_arc.clone());
        let mut trajectory_events = app.chat.trajectory_events_tx.subscribe();

        let mut disabled = crate::runtime_settings::TrajectoryRuntimeSettings::default();
        disabled.trajectory_watcher_self_write_enabled = false;
        crate::runtime_settings::install_live(&disabled);
        assert!(!trajectory_watcher_self_write_rollout_enabled());

        let content = tokio::fs::read_to_string(&path).await.unwrap();
        let tmp_path = unique_trajectory_tmp_path(&path);
        atomic_write_json_with_tmp_path(
            &path,
            &tmp_path,
            Ok(content),
            Some("Failed to write trajectory"),
            Some(TrajectorySourceIdentity::Normal),
        )
        .await
        .unwrap();
        process_trajectory_watcher_change(
            gcx,
            chat_id.to_string(),
            true,
            TrajectorySourceIdentity::Normal,
            path,
        )
        .await;

        assert_eq!(trajectory_events.try_recv().unwrap().id, chat_id);
        crate::runtime_settings::reset_for_test();
        clear_all_trajectory_self_writes();
    }

    #[serial(trajectory_perf)]
    #[tokio::test]
    async fn trajectory_watcher_self_write_cleanup_keeps_other_watcher_scope() {
        clear_all_trajectory_self_writes();
        let dir = tempfile::tempdir().unwrap();
        let first_root = dir.path().join("first");
        let second_root = dir.path().join("second");
        tokio::fs::create_dir_all(&first_root).await.unwrap();
        tokio::fs::create_dir_all(&second_root).await.unwrap();
        let first_path = first_root.join("first.json");
        let second_path = second_root.join("second.json");
        let first_handle = register_trajectory_self_write(
            &first_path,
            TrajectorySourceIdentity::Normal,
            "{\"id\":\"first\"}",
        )
        .await
        .unwrap();
        let second_handle = register_trajectory_self_write(
            &second_path,
            TrajectorySourceIdentity::Normal,
            "{\"id\":\"second\"}",
        )
        .await
        .unwrap();
        tokio::fs::write(&first_path, "{\"id\":\"first\"}")
            .await
            .unwrap();
        tokio::fs::write(&second_path, "{\"id\":\"second\"}")
            .await
            .unwrap();
        complete_trajectory_self_write(Some(first_handle)).await;
        complete_trajectory_self_write(Some(second_handle)).await;

        clear_trajectory_self_writes(&[dunce::simplified(
            &tokio::fs::canonicalize(&first_root).await.unwrap(),
        )
        .to_path_buf()]);

        assert!(
            !consume_trajectory_self_write(&first_path, &TrajectorySourceIdentity::Normal).await
        );
        assert!(
            consume_trajectory_self_write(&second_path, &TrajectorySourceIdentity::Normal).await
        );
        clear_all_trajectory_self_writes();
    }

    #[test]
    fn trajectory_watcher_ignores_atomic_temporary_files() {
        let dir = tempfile::tempdir().unwrap();
        let task_root = dir.path().join(".refact").join("tasks");
        std::fs::create_dir_all(&task_root).unwrap();
        let temporary = task_root
            .join("task-1")
            .join("trajectories")
            .join("agents")
            .join("agent-1")
            .join("chat.json.tmp.abc123");

        assert!(is_trajectory_temporary_file(&temporary));
        assert!(trajectory_watcher_message_for_path(temporary, false, &[task_root]).is_none());
    }

    #[tokio::test]
    async fn orphaned_temp_file_does_not_replace_last_atomic_commit() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("chat.json");
        let committed = r#"{"id":"chat","messages":[]}"#;
        tokio::fs::write(&file_path, committed).await.unwrap();
        let tmp_path = unique_trajectory_tmp_path(&file_path);
        tokio::fs::write(&tmp_path, r#"{"id":"chat","messages":["partial"]"#)
            .await
            .unwrap();

        let restored = tokio::fs::read_to_string(&file_path).await.unwrap();
        assert_eq!(restored, committed);
        assert!(tmp_path.exists());
        tokio::fs::remove_file(tmp_path).await.unwrap();
    }

    #[tokio::test]
    async fn trajectory_worktree_old_json_without_worktree_loads_none() {
        let dir = tempfile::tempdir().unwrap();
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let app = AppState::from_gcx(gcx.clone()).await;
        {
            *app.workspace
                .documents_state
                .workspace_folders
                .lock()
                .unwrap() = vec![dir.path().to_path_buf()];
        }
        let traj_dir = dir.path().join(".refact").join("trajectories");
        tokio::fs::create_dir_all(&traj_dir).await.unwrap();
        tokio::fs::write(
            traj_dir.join("old-chat.json"),
            r#"{"id":"old-chat","title":"Old","model":"m","mode":"agent","tool_use":"agent","messages":[],"created_at":"2024-01-01T00:00:00Z","updated_at":"2024-01-01T00:00:00Z","include_project_info":true,"checkpoints_enabled":true}"#,
        )
        .await
        .unwrap();

        let loaded = load_trajectory_for_chat(gcx, "old-chat").await.unwrap();
        assert!(loaded.thread.worktree.is_none());
    }

    #[tokio::test]
    async fn trajectory_worktree_unregistered_top_level_metadata_is_stripped() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("repo");
        let cache = dir.path().join("cache");
        std::fs::create_dir_all(&source).unwrap();
        init_repo(&source);
        let gcx = crate::global_context::tests::make_test_gcx_with_dirs(
            cache.clone(),
            std::env::temp_dir().join(format!("refact-cfg-{}", uuid::Uuid::new_v4())),
        )
        .await;
        {
            *gcx.documents_state.workspace_folders.lock().unwrap() = vec![source.clone()];
        }
        let app = AppState::from_gcx(gcx.clone()).await;
        let traj_dir = source.join(".refact").join("trajectories");
        tokio::fs::create_dir_all(&traj_dir).await.unwrap();
        let untrusted = json!({
            "id": "untrusted-wt-chat",
            "title": "Untrusted",
            "model": "m",
            "mode": "agent",
            "tool_use": "agent",
            "messages": [],
            "created_at": "2024-01-01T00:00:00Z",
            "updated_at": "2024-01-01T00:00:00Z",
            "include_project_info": true,
            "checkpoints_enabled": true,
            "worktree": {
                "id": "wt-evil",
                "kind": "chat",
                "root": dir.path().join("evil").to_string_lossy().to_string(),
                "source_workspace_root": source.to_string_lossy().to_string(),
                "repo_root": source.to_string_lossy().to_string(),
                "enforce": true
            }
        });
        tokio::fs::write(
            traj_dir.join("untrusted-wt-chat.json"),
            serde_json::to_string(&untrusted).unwrap(),
        )
        .await
        .unwrap();

        let loaded = load_trajectory_for_chat(gcx.clone(), "untrusted-wt-chat")
            .await
            .unwrap();
        assert!(loaded.thread.worktree.is_none());
        let listed = list_all_trajectories_meta(app, false).await.unwrap();
        let listed_worktree = listed
            .iter()
            .find(|item| item.id == "untrusted-wt-chat")
            .and_then(|item| item.worktree.clone());
        assert!(listed_worktree.is_none());
    }

    #[tokio::test]
    async fn trajectory_worktree_legacy_task_agent_hydrates_from_board_mirror() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("repo");
        let cache = dir.path().join("cache");
        std::fs::create_dir_all(&source).unwrap();
        init_repo(&source);
        let gcx = crate::global_context::tests::make_test_gcx_with_dirs(
            cache.clone(),
            std::env::temp_dir().join(format!("refact-cfg-{}", uuid::Uuid::new_v4())),
        )
        .await;
        {
            *gcx.documents_state.workspace_folders.lock().unwrap() = vec![source.clone()];
        }
        let _app = AppState::from_gcx(gcx.clone()).await;

        let task_id = "task-legacy";
        let agent_id = "agent-1";
        let card_id = "card-1";
        let chat_id = "legacy-agent-chat";
        let task_dir = source.join(".refact").join("tasks").join(task_id);
        tokio::fs::create_dir_all(task_dir.join("trajectories").join("agents").join(agent_id))
            .await
            .unwrap();
        let meta = crate::tasks::types::TaskMeta {
            schema_version: 1,
            id: task_id.to_string(),
            name: "Task".to_string(),
            status: crate::tasks::types::TaskStatus::Active,
            created_at: "2024-01-01T00:00:00Z".to_string(),
            updated_at: "2024-01-01T00:00:00Z".to_string(),
            cards_total: 1,
            cards_done: 0,
            cards_failed: 0,
            agents_active: 1,
            base_branch: Some("main".to_string()),
            base_commit: Some("base123".to_string()),
            default_agent_model: None,
            is_name_generated: true,
            last_agents_summary_at: None,
            planner_session_state: None,
        };
        tokio::fs::write(
            task_dir.join("meta.yaml"),
            serde_yaml::to_string(&meta).unwrap(),
        )
        .await
        .unwrap();
        let project_hash = crate::worktrees::service::project_hash_for_path(
            &dunce::simplified(&source.canonicalize().unwrap()).to_path_buf(),
        );
        let worktree_cache_dir = cache.join("worktrees").join(&project_hash);
        std::fs::create_dir_all(&worktree_cache_dir).unwrap();
        let agent_worktree = worktree_cache_dir.join("agent-worktree");
        let agent_worktree_arg = agent_worktree.to_string_lossy().to_string();
        run_git(
            &source,
            &[
                "worktree",
                "add",
                "-b",
                "refact/task/card",
                &agent_worktree_arg,
                "main",
            ],
        );
        let board = crate::tasks::types::TaskBoard {
            schema_version: 1,
            rev: 1,
            columns: Vec::new(),
            cards: vec![crate::tasks::types::BoardCard {
                id: card_id.to_string(),
                title: "Card".to_string(),
                column: "doing".to_string(),
                priority: "P1".to_string(),
                depends_on: Vec::new(),
                instructions: String::new(),
                assignee: Some(agent_id.to_string()),
                agent_chat_id: Some(chat_id.to_string()),
                retry_count: 0,
                status_updates: Vec::new(),
                comments: vec![],
                final_report: None,
                final_report_structured: None,
                verifier_report: None,
                created_at: "2024-01-01T00:00:00Z".to_string(),
                started_at: None,
                last_heartbeat_at: None,
                completed_at: None,
                agent_branch: Some("refact/task/card".to_string()),
                agent_worktree: Some(agent_worktree.to_string_lossy().to_string()),
                agent_worktree_name: Some("wt-legacy".to_string()),
                base_branch: None,
                base_commit: None,
                ab_variants: None,
                team_members: vec![],
                target_files: Vec::new(),
                scope_guard_mode: Default::default(),
            }],
        };
        tokio::fs::write(
            task_dir.join("board.yaml"),
            serde_yaml::to_string(&board).unwrap(),
        )
        .await
        .unwrap();
        let trajectory = json!({
            "id": chat_id,
            "title": "Legacy Agent",
            "model": "m",
            "mode": "task_agent",
            "tool_use": "agent",
            "messages": [],
            "created_at": "2024-01-01T00:00:00Z",
            "updated_at": "2024-01-01T00:00:00Z",
            "include_project_info": true,
            "checkpoints_enabled": true,
            "task_meta": {
                "task_id": task_id,
                "role": "agents",
                "agent_id": agent_id,
                "card_id": card_id
            }
        });
        tokio::fs::write(
            task_dir
                .join("trajectories")
                .join("agents")
                .join(agent_id)
                .join(format!("{}.json", chat_id)),
            serde_json::to_string(&trajectory).unwrap(),
        )
        .await
        .unwrap();

        let loaded = load_trajectory_for_chat(gcx, chat_id).await.unwrap();
        let worktree = loaded.thread.worktree.unwrap();
        assert_eq!(worktree.id, "wt-legacy");
        assert_eq!(worktree.kind, "task_agent");
        assert_eq!(worktree.root, agent_worktree);
        assert_eq!(worktree.branch.as_deref(), Some("refact/task/card"));
        assert_eq!(worktree.base_branch.as_deref(), Some("main"));
        assert_eq!(worktree.base_commit.as_deref(), Some("base123"));
        assert!(worktree.enforce);
    }

    #[tokio::test]
    async fn trajectory_worktree_legacy_task_agent_rejects_mismatched_chat_identity() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("repo");
        let cache = dir.path().join("cache");
        std::fs::create_dir_all(&source).unwrap();
        init_repo(&source);
        let gcx = crate::global_context::tests::make_test_gcx_with_dirs(
            cache.clone(),
            std::env::temp_dir().join(format!("refact-cfg-{}", uuid::Uuid::new_v4())),
        )
        .await;
        {
            *gcx.documents_state.workspace_folders.lock().unwrap() = vec![source.clone()];
        }
        let _app = AppState::from_gcx(gcx.clone()).await;

        let task_id = "task-legacy-mismatch";
        let agent_id = "agent-1";
        let card_id = "card-1";
        let chat_id = "wrong-agent-chat";
        let task_dir = source.join(".refact").join("tasks").join(task_id);
        tokio::fs::create_dir_all(task_dir.join("trajectories").join("agents").join(agent_id))
            .await
            .unwrap();
        let meta = crate::tasks::types::TaskMeta {
            schema_version: 1,
            id: task_id.to_string(),
            name: "Task".to_string(),
            status: crate::tasks::types::TaskStatus::Active,
            created_at: "2024-01-01T00:00:00Z".to_string(),
            updated_at: "2024-01-01T00:00:00Z".to_string(),
            cards_total: 1,
            cards_done: 0,
            cards_failed: 0,
            agents_active: 1,
            base_branch: Some("main".to_string()),
            base_commit: Some("base123".to_string()),
            default_agent_model: None,
            is_name_generated: true,
            last_agents_summary_at: None,
            planner_session_state: None,
        };
        tokio::fs::write(
            task_dir.join("meta.yaml"),
            serde_yaml::to_string(&meta).unwrap(),
        )
        .await
        .unwrap();
        let agent_worktree = dir.path().join("agent-worktree-mismatch");
        let agent_worktree_arg = agent_worktree.to_string_lossy().to_string();
        run_git(
            &source,
            &[
                "worktree",
                "add",
                "-b",
                "refact/task/card-mismatch",
                &agent_worktree_arg,
                "main",
            ],
        );
        let board = crate::tasks::types::TaskBoard {
            schema_version: 1,
            rev: 1,
            columns: Vec::new(),
            cards: vec![crate::tasks::types::BoardCard {
                id: card_id.to_string(),
                title: "Card".to_string(),
                column: "doing".to_string(),
                priority: "P1".to_string(),
                depends_on: Vec::new(),
                instructions: String::new(),
                assignee: Some(agent_id.to_string()),
                agent_chat_id: Some("actual-agent-chat".to_string()),
                retry_count: 0,
                status_updates: Vec::new(),
                comments: vec![],
                final_report: None,
                final_report_structured: None,
                verifier_report: None,
                created_at: "2024-01-01T00:00:00Z".to_string(),
                started_at: None,
                last_heartbeat_at: None,
                completed_at: None,
                agent_branch: Some("refact/task/card-mismatch".to_string()),
                agent_worktree: Some(agent_worktree.to_string_lossy().to_string()),
                agent_worktree_name: Some("wt-legacy-mismatch".to_string()),
                base_branch: None,
                base_commit: None,
                ab_variants: None,
                team_members: vec![],
                target_files: Vec::new(),
                scope_guard_mode: Default::default(),
            }],
        };
        tokio::fs::write(
            task_dir.join("board.yaml"),
            serde_yaml::to_string(&board).unwrap(),
        )
        .await
        .unwrap();
        let trajectory = json!({
            "id": chat_id,
            "title": "Legacy Agent",
            "model": "m",
            "mode": "task_agent",
            "tool_use": "agent",
            "messages": [],
            "created_at": "2024-01-01T00:00:00Z",
            "updated_at": "2024-01-01T00:00:00Z",
            "include_project_info": true,
            "checkpoints_enabled": true,
            "task_meta": {
                "task_id": task_id,
                "role": "agents",
                "agent_id": agent_id,
                "card_id": card_id
            }
        });
        tokio::fs::write(
            task_dir
                .join("trajectories")
                .join("agents")
                .join(agent_id)
                .join(format!("{}.json", chat_id)),
            serde_json::to_string(&trajectory).unwrap(),
        )
        .await
        .unwrap();

        let loaded = load_trajectory_for_chat(gcx, chat_id).await.unwrap();
        assert!(loaded.thread.worktree.is_none());
    }

    #[tokio::test]
    #[ignore]
    async fn stress_atomic_write_json_pressure_baseline() {
        const MESSAGE_COUNT: usize = 1_000;
        const MESSAGE_SIZE: usize = 1_024;
        const WRITE_RUNS: usize = 120;

        let temp_dir = tempfile::TempDir::new().unwrap();
        let file_path = temp_dir.path().join("stress-trajectory.json");

        let mut messages = Vec::with_capacity(MESSAGE_COUNT);
        for i in 0..MESSAGE_COUNT {
            messages.push(json!({
                "message_id": format!("m{}", i),
                "role": if i % 2 == 0 { "user" } else { "assistant" },
                "content": "x".repeat(MESSAGE_SIZE),
            }));
        }

        let payload = json!({
            "id": "stress-chat",
            "title": "Stress",
            "created_at": "2026-01-01T00:00:00Z",
            "updated_at": "2026-01-01T00:00:00Z",
            "model": "test/model",
            "mode": "agent",
            "tool_use": "agent",
            "messages": messages,
            "isTitleGenerated": false,
        });

        let start = Instant::now();
        for _ in 0..WRITE_RUNS {
            atomic_write_json(&file_path, &payload, TrajectorySourceIdentity::Normal)
                .await
                .unwrap();
        }
        let elapsed = start.elapsed();

        assert!(file_path.exists());
        let content = fs::read_to_string(&file_path).await.unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(
            parsed.get("id").and_then(|v| v.as_str()),
            Some("stress-chat")
        );
        assert_eq!(
            parsed
                .get("messages")
                .and_then(|v| v.as_array())
                .map(|arr| arr.len()),
            Some(MESSAGE_COUNT)
        );

        println!(
            "STRESS_BASELINE trajectory_atomic_write: writes={}, messages={}, msg_size={}, elapsed_ms={}",
            WRITE_RUNS,
            MESSAGE_COUNT,
            MESSAGE_SIZE,
            elapsed.as_millis(),
        );
    }

    #[tokio::test]
    #[ignore]
    async fn stress_trajectory_json_read_parse_baseline() {
        const MESSAGE_COUNT: usize = 1_200;
        const MESSAGE_SIZE: usize = 512;
        const PARSE_RUNS: usize = 400;

        let temp_dir = tempfile::TempDir::new().unwrap();
        let file_path = temp_dir.path().join("stress-parse.json");

        let mut messages = Vec::with_capacity(MESSAGE_COUNT);
        for i in 0..MESSAGE_COUNT {
            messages.push(json!({
                "message_id": format!("msg-{}", i),
                "role": if i % 3 == 0 { "assistant" } else { "user" },
                "content": "y".repeat(MESSAGE_SIZE),
            }));
        }

        let data = TrajectoryData {
            id: "parse-chat".to_string(),
            title: "Parse".to_string(),
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
            model: "test/model".to_string(),
            mode: "agent".to_string(),
            tool_use: "agent".to_string(),
            messages,
            extra: serde_json::Map::new(),
        };

        atomic_write_json(&file_path, &data, TrajectorySourceIdentity::Normal)
            .await
            .unwrap();
        let content = fs::read_to_string(&file_path).await.unwrap();

        let start = Instant::now();
        for _ in 0..PARSE_RUNS {
            let parsed: TrajectoryData = serde_json::from_str(&content).unwrap();
            assert_eq!(parsed.id, "parse-chat");
            assert_eq!(parsed.messages.len(), MESSAGE_COUNT);
        }
        let elapsed = start.elapsed();

        println!(
            "STRESS_BASELINE trajectory_read_parse: parses={}, messages={}, msg_size={}, elapsed_ms={}",
            PARSE_RUNS,
            MESSAGE_COUNT,
            MESSAGE_SIZE,
            elapsed.as_millis(),
        );
    }

    #[serial(trajectory_perf)]
    #[tokio::test]
    async fn trajectory_get_handler_returns_404_for_missing_chat() {
        let dir = tempfile::tempdir().unwrap();
        let (_gcx, app) = make_app_with_workspace(dir.path()).await;
        let err =
            handle_v1_trajectories_get(State(app), AxumPath("nonexistent-chat-id".to_string()))
                .await
                .unwrap_err();
        assert_eq!(err.status_code, StatusCode::NOT_FOUND);
        assert_eq!(err.message, "Trajectory not found");
    }

    #[serial(trajectory_perf)]
    #[tokio::test]
    async fn trajectory_path_handler_returns_404_for_missing_chat() {
        let dir = tempfile::tempdir().unwrap();
        let (_gcx, app) = make_app_with_workspace(dir.path()).await;
        let err =
            handle_v1_trajectory_path(State(app), AxumPath("nonexistent-chat-id".to_string()))
                .await
                .unwrap_err();
        assert_eq!(err.status_code, StatusCode::NOT_FOUND);
    }

    #[serial(trajectory_perf)]
    #[tokio::test]
    async fn trajectory_path_handler_returns_400_for_invalid_id() {
        let dir = tempfile::tempdir().unwrap();
        let (_gcx, app) = make_app_with_workspace(dir.path()).await;
        let err = handle_v1_trajectory_path(State(app), AxumPath("../bad".to_string()))
            .await
            .unwrap_err();
        assert_eq!(err.status_code, StatusCode::BAD_REQUEST);
    }

    #[serial(trajectory_perf)]
    #[tokio::test]
    async fn trajectory_path_handler_returns_normal_trajectory_path() {
        let dir = tempfile::tempdir().unwrap();
        let (_gcx, app) = make_app_with_workspace(dir.path()).await;
        let chat_id = "path-normal";
        let path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        write_trajectory_file(&path, chat_id, "Normal", "2024-01-01T00:00:00Z").await;

        let response = handle_v1_trajectory_path(State(app), AxumPath(chat_id.to_string()))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = hyper::body::to_bytes(response.into_body()).await.unwrap();
        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let returned = payload["path"].as_str().unwrap();
        assert_same_path_str(returned, &path);
    }

    #[serial(trajectory_perf)]
    #[tokio::test]
    async fn trajectory_path_handler_returns_nested_normal_trajectory_path_for_active_session() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;
        let root_chat_id = "path-normal-nested-root";
        let chat_id = "path-normal-nested-active";
        let path = nested_trajectory_path(dir.path(), root_chat_id, chat_id);
        write_trajectory_file(&path, chat_id, "Normal", "2024-01-01T00:00:00Z").await;

        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        session_arc.lock().await.thread.root_chat_id = Some(root_chat_id.to_string());
        gcx.chat_sessions
            .write()
            .await
            .insert(chat_id.to_string(), session_arc);

        let response = handle_v1_trajectory_path(State(app), AxumPath(chat_id.to_string()))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = hyper::body::to_bytes(response.into_body()).await.unwrap();
        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_same_path_str(payload["path"].as_str().unwrap(), &path);
    }

    #[serial(trajectory_perf)]
    #[tokio::test]
    async fn trajectory_path_handler_returns_buddy_path() {
        let dir = tempfile::tempdir().unwrap();
        let (_gcx, app) = make_app_with_workspace(dir.path()).await;
        let chat_id = "path-buddy";
        let buddy_path = dir
            .path()
            .join(".refact")
            .join("buddy")
            .join("chats")
            .join("conversations")
            .join(format!("{chat_id}.json"));
        write_buddy_conversation_file(&buddy_path, chat_id, "Buddy").await;

        let response = handle_v1_trajectory_path(State(app), AxumPath(chat_id.to_string()))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = hyper::body::to_bytes(response.into_body()).await.unwrap();
        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_same_path_str(payload["path"].as_str().unwrap(), &buddy_path);
    }

    #[serial(trajectory_perf)]
    #[tokio::test]
    async fn trajectory_path_handler_does_not_leak_other_source_for_active_buddy_session() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;
        let chat_id = "path-collision-buddy";

        let normal_path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        write_trajectory_file(&normal_path, chat_id, "Normal", "2024-01-01T00:00:00Z").await;

        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        {
            let mut session = session_arc.lock().await;
            session.thread.mode = "buddy".to_string();
            session.thread.buddy_meta = Some(BuddyThreadMeta {
                is_buddy_chat: true,
                buddy_chat_kind: "investigation".to_string(),
                workflow_id: None,
            });
        }
        gcx.chat_sessions
            .write()
            .await
            .insert(chat_id.to_string(), session_arc);

        let err = handle_v1_trajectory_path(State(app), AxumPath(chat_id.to_string()))
            .await
            .unwrap_err();
        assert_eq!(err.status_code, StatusCode::NOT_FOUND);
        assert!(tokio::fs::try_exists(&normal_path).await.unwrap());
    }

    #[serial(trajectory_perf)]
    #[tokio::test]
    async fn trajectory_path_handler_returns_buddy_path_when_active_buddy_session_has_buddy_file() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;
        let chat_id = "path-buddy-resolved";

        let normal_path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        write_trajectory_file(&normal_path, chat_id, "Normal", "2024-01-01T00:00:00Z").await;

        let buddy_path = dir
            .path()
            .join(".refact")
            .join("buddy")
            .join("chats")
            .join("conversations")
            .join(format!("{chat_id}.json"));
        write_buddy_conversation_file(&buddy_path, chat_id, "Buddy").await;

        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        {
            let mut session = session_arc.lock().await;
            session.thread.mode = "buddy".to_string();
            session.thread.buddy_meta = Some(BuddyThreadMeta {
                is_buddy_chat: true,
                buddy_chat_kind: "investigation".to_string(),
                workflow_id: None,
            });
        }
        gcx.chat_sessions
            .write()
            .await
            .insert(chat_id.to_string(), session_arc);

        let response = handle_v1_trajectory_path(State(app), AxumPath(chat_id.to_string()))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = hyper::body::to_bytes(response.into_body()).await.unwrap();
        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let returned = payload["path"].as_str().unwrap();
        assert_same_path_str(returned, &buddy_path);
        assert_ne!(
            normalized_test_path(Path::new(returned)),
            normalized_test_path(&normal_path)
        );
    }

    #[serial(trajectory_perf)]
    #[tokio::test]
    async fn trajectory_path_handler_rejects_malformed_json() {
        let dir = tempfile::tempdir().unwrap();
        let (_gcx, app) = make_app_with_workspace(dir.path()).await;
        let chat_id = "path-malformed";
        let path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        tokio::fs::create_dir_all(path.parent().unwrap())
            .await
            .unwrap();
        tokio::fs::write(&path, b"this is not json").await.unwrap();

        let err = handle_v1_trajectory_path(State(app), AxumPath(chat_id.to_string()))
            .await
            .unwrap_err();
        assert_eq!(err.status_code, StatusCode::NOT_FOUND);
    }

    #[serial(trajectory_perf)]
    #[tokio::test]
    async fn trajectory_path_handler_rejects_id_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        let (_gcx, app) = make_app_with_workspace(dir.path()).await;
        let chat_id = "path-id-mismatch";
        let path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        tokio::fs::create_dir_all(path.parent().unwrap())
            .await
            .unwrap();
        let content = json!({
            "id": "different-id",
            "title": "Wrong id",
            "model": "m",
            "mode": "agent",
            "tool_use": "agent",
            "created_at": "2024-01-01T00:00:00Z",
            "updated_at": "2024-01-01T00:00:00Z",
            "messages": [{"role": "user", "content": "hi"}],
        });
        tokio::fs::write(&path, serde_json::to_string(&content).unwrap())
            .await
            .unwrap();

        let err = handle_v1_trajectory_path(State(app), AxumPath(chat_id.to_string()))
            .await
            .unwrap_err();
        assert_eq!(err.status_code, StatusCode::NOT_FOUND);
    }
