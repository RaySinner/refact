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
    async fn frozen_request_prefix_migrates_system_prompt_from_legacy_trajectory() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let traj_dir = dir.path().join(".refact").join("trajectories");
        tokio::fs::create_dir_all(&traj_dir).await.unwrap();
        tokio::fs::write(
            traj_dir.join("legacy-frozen-prefix.json"),
            r#"{
                "id":"legacy-frozen-prefix",
                "title":"Legacy",
                "created_at":"2024-01-01T00:00:00Z",
                "updated_at":"2024-01-01T00:00:00Z",
                "model":"model",
                "mode":"agent",
                "tool_use":"agent",
                "messages":[
                    {"role":"system","content":"legacy system"},
                    {"role":"user","content":"hello"}
                ],
                "include_project_info":true,
                "checkpoints_enabled":true
            }"#,
        )
        .await
        .unwrap();

        let loaded = load_trajectory_for_chat(gcx, "legacy-frozen-prefix")
            .await
            .unwrap();
        let prefix = loaded.thread.frozen_request_prefix.unwrap();
        assert_eq!(prefix.system_prompt.as_deref(), Some("legacy system"));
        assert!(prefix.tools_canonical.is_none());
        assert!(loaded.thread.claude_code_identity.is_none());
    }

    #[tokio::test]
    async fn mode_transition_load_discards_stale_copied_frozen_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let traj_dir = dir.path().join(".refact").join("trajectories");
        tokio::fs::create_dir_all(&traj_dir).await.unwrap();
        tokio::fs::write(
            traj_dir.join("transition-stale-frozen.json"),
            serde_json::to_string(&json!({
                "id":"transition-stale-frozen",
                "title":"Transition",
                "created_at":"2024-01-01T00:00:00Z",
                "updated_at":"2024-01-01T00:00:00Z",
                "model":"model",
                "mode":"task_planner",
                "tool_use":"agent",
                "parent_id":"source-chat",
                "link_type":"mode_transition",
                "previous_response_id":"resp_source",
                "messages":[
                    {"role":"system","content":"target task planner system"},
                    {"role":"user","content":"hello"}
                ],
                "frozen_request_prefix": {
                    "schema_version": 1,
                    "created_at": "2026-05-29T00:00:00Z",
                    "system_prompt": "old planning system",
                    "tools_canonical": [{"type":"function","function":{"name":"old_tool"}}]
                },
                "claude_code_identity": {
                    "device_id":"source-device",
                    "session_id":"source-session"
                },
                "include_project_info":true,
                "checkpoints_enabled":true
            }))
            .unwrap(),
        )
        .await
        .unwrap();

        let loaded = load_trajectory_for_chat(gcx, "transition-stale-frozen")
            .await
            .unwrap();
        let prefix = loaded.thread.frozen_request_prefix.unwrap();
        assert_eq!(
            prefix.system_prompt.as_deref(),
            Some("target task planner system")
        );
        assert!(prefix.tools_canonical.is_none());
        assert!(loaded.thread.claude_code_identity.is_none());
        assert!(loaded.thread.previous_response_id.is_none());
    }

    #[tokio::test]
    async fn mode_transition_no_frozen_prefix_with_copied_provider_state_is_repaired() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let traj_dir = dir.path().join(".refact").join("trajectories");
        tokio::fs::create_dir_all(&traj_dir).await.unwrap();
        tokio::fs::write(
            traj_dir.join("transition-no-prefix-provider.json"),
            serde_json::to_string(&json!({
                "id":"transition-no-prefix-provider",
                "title":"Transition",
                "created_at":"2024-01-02T00:00:00Z",
                "updated_at":"2024-01-02T00:00:00Z",
                "model":"model",
                "mode":"task_planner",
                "tool_use":"agent",
                "parent_id":"source-chat",
                "link_type":"mode_transition",
                "previous_response_id":"resp_source",
                "messages":[
                    {"role":"system","content":"target task planner system"},
                    {"role":"user","content":"hello"}
                ],
                "claude_code_identity": {
                    "device_id":"source-device",
                    "session_id":"source-session"
                },
                "include_project_info":true,
                "checkpoints_enabled":true
            }))
            .unwrap(),
        )
        .await
        .unwrap();

        let loaded = load_trajectory_for_chat(gcx, "transition-no-prefix-provider")
            .await
            .unwrap();
        let prefix = loaded.thread.frozen_request_prefix.unwrap();
        assert_eq!(
            prefix.system_prompt.as_deref(),
            Some("target task planner system")
        );
        assert!(prefix.tools_canonical.is_none());
        assert!(loaded.thread.claude_code_identity.is_none());
        assert!(loaded.thread.previous_response_id.is_none());
    }

    #[tokio::test]
    async fn mode_transition_matching_system_with_stale_tools_is_repaired_by_timestamp() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let traj_dir = dir.path().join(".refact").join("trajectories");
        tokio::fs::create_dir_all(&traj_dir).await.unwrap();
        tokio::fs::write(
            traj_dir.join("transition-stale-tools-timestamp.json"),
            serde_json::to_string(&json!({
                "id":"transition-stale-tools-timestamp",
                "title":"Transition",
                "created_at":"2024-01-02T00:00:00Z",
                "updated_at":"2024-01-02T00:00:00Z",
                "model":"model",
                "mode":"task_planner",
                "tool_use":"agent",
                "parent_id":"source-chat",
                "link_type":"mode_transition",
                "messages":[
                    {"role":"system","content":"target task planner system"},
                    {"role":"user","content":"hello"}
                ],
                "frozen_request_prefix": {
                    "schema_version": 1,
                    "created_at": "2024-01-01T00:00:00Z",
                    "system_prompt": "target task planner system",
                    "tools_canonical": [{"type":"function","function":{"name":"old_source_tool"}}]
                },
                "include_project_info":true,
                "checkpoints_enabled":true
            }))
            .unwrap(),
        )
        .await
        .unwrap();

        let loaded = load_trajectory_for_chat(gcx, "transition-stale-tools-timestamp")
            .await
            .unwrap();
        let prefix = loaded.thread.frozen_request_prefix.unwrap();
        assert_eq!(
            prefix.system_prompt.as_deref(),
            Some("target task planner system")
        );
        assert!(prefix.tools_canonical.is_none());
        assert!(loaded.thread.claude_code_identity.is_none());
        assert!(loaded.thread.previous_response_id.is_none());
    }

    #[tokio::test]
    async fn mode_transition_malformed_frozen_prefix_with_provider_identity_is_repaired() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let traj_dir = dir.path().join(".refact").join("trajectories");
        tokio::fs::create_dir_all(&traj_dir).await.unwrap();
        tokio::fs::write(
            traj_dir.join("transition-malformed-prefix-provider.json"),
            serde_json::to_string(&json!({
                "id":"transition-malformed-prefix-provider",
                "title":"Transition",
                "created_at":"2024-01-02T00:00:00Z",
                "updated_at":"2024-01-02T00:00:00Z",
                "model":"model",
                "mode":"task_planner",
                "tool_use":"agent",
                "parent_id":"source-chat",
                "link_type":"mode_transition",
                "previous_response_id":"resp_source",
                "messages":[
                    {"role":"system","content":"target task planner system"},
                    {"role":"user","content":"hello"}
                ],
                "frozen_request_prefix": {
                    "schema_version": "bad",
                    "created_at": 7,
                    "system_prompt": ["source system"],
                    "tools_canonical": [{"type":"function","function":{"name":"old_source_tool"}}]
                },
                "claude_code_identity": {
                    "device_id":"source-device",
                    "session_id":"source-session"
                },
                "include_project_info":true,
                "checkpoints_enabled":true
            }))
            .unwrap(),
        )
        .await
        .unwrap();

        let loaded = load_trajectory_for_chat(gcx, "transition-malformed-prefix-provider")
            .await
            .unwrap();
        let prefix = loaded.thread.frozen_request_prefix.unwrap();
        assert_eq!(
            prefix.system_prompt.as_deref(),
            Some("target task planner system")
        );
        assert!(prefix.tools_canonical.is_none());
        assert!(loaded.thread.claude_code_identity.is_none());
        assert!(loaded.thread.previous_response_id.is_none());
    }

    #[tokio::test]
    async fn mode_transition_invalid_prefix_timestamp_with_provider_state_is_repaired() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let traj_dir = dir.path().join(".refact").join("trajectories");
        tokio::fs::create_dir_all(&traj_dir).await.unwrap();
        tokio::fs::write(
            traj_dir.join("transition-invalid-prefix-time-provider.json"),
            serde_json::to_string(&json!({
                "id":"transition-invalid-prefix-time-provider",
                "title":"Transition",
                "created_at":"2024-01-02T00:00:00Z",
                "updated_at":"2024-01-02T00:00:00Z",
                "model":"model",
                "mode":"task_planner",
                "tool_use":"agent",
                "parent_id":"source-chat",
                "link_type":"mode_transition",
                "previous_response_id":"resp_source",
                "messages":[
                    {"role":"system","content":"target task planner system"},
                    {"role":"user","content":"hello"}
                ],
                "frozen_request_prefix": {
                    "schema_version": 1,
                    "created_at": "not-a-date",
                    "system_prompt": "target task planner system",
                    "tools_canonical": [{"type":"function","function":{"name":"old_source_tool"}}]
                },
                "claude_code_identity": {
                    "device_id":"source-device",
                    "session_id":"source-session"
                },
                "include_project_info":true,
                "checkpoints_enabled":true
            }))
            .unwrap(),
        )
        .await
        .unwrap();

        let loaded = load_trajectory_for_chat(gcx, "transition-invalid-prefix-time-provider")
            .await
            .unwrap();
        let prefix = loaded.thread.frozen_request_prefix.unwrap();
        assert_eq!(
            prefix.system_prompt.as_deref(),
            Some("target task planner system")
        );
        assert!(prefix.tools_canonical.is_none());
        assert!(loaded.thread.claude_code_identity.is_none());
        assert!(loaded.thread.previous_response_id.is_none());
        assert!(loaded.transition_identity_repaired);
    }

    #[tokio::test]
    async fn mode_transition_invalid_trajectory_created_at_with_tools_and_provider_state_is_repaired(
    ) {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let traj_dir = dir.path().join(".refact").join("trajectories");
        tokio::fs::create_dir_all(&traj_dir).await.unwrap();
        tokio::fs::write(
            traj_dir.join("transition-invalid-trajectory-time-provider.json"),
            serde_json::to_string(&json!({
                "id":"transition-invalid-trajectory-time-provider",
                "title":"Transition",
                "created_at":"not-a-date",
                "updated_at":"2024-01-02T00:00:00Z",
                "model":"model",
                "mode":"task_planner",
                "tool_use":"agent",
                "parent_id":"source-chat",
                "link_type":"mode_transition",
                "previous_response_id":"resp_source",
                "messages":[
                    {"role":"system","content":"target task planner system"},
                    {"role":"user","content":"hello"}
                ],
                "frozen_request_prefix": {
                    "schema_version": 1,
                    "created_at": "2024-01-03T00:00:00Z",
                    "system_prompt": "target task planner system",
                    "tools_canonical": [{"type":"function","function":{"name":"old_source_tool"}}]
                },
                "claude_code_identity": {
                    "device_id":"source-device",
                    "session_id":"source-session"
                },
                "include_project_info":true,
                "checkpoints_enabled":true
            }))
            .unwrap(),
        )
        .await
        .unwrap();

        let loaded = load_trajectory_for_chat(gcx, "transition-invalid-trajectory-time-provider")
            .await
            .unwrap();
        let prefix = loaded.thread.frozen_request_prefix.unwrap();
        assert_eq!(
            prefix.system_prompt.as_deref(),
            Some("target task planner system")
        );
        assert!(prefix.tools_canonical.is_none());
        assert!(loaded.thread.claude_code_identity.is_none());
        assert!(loaded.thread.previous_response_id.is_none());
        assert!(loaded.transition_identity_repaired);
    }

    #[tokio::test]
    async fn mode_transition_missing_trajectory_created_at_with_tools_and_provider_state_is_repaired(
    ) {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let traj_dir = dir.path().join(".refact").join("trajectories");
        tokio::fs::create_dir_all(&traj_dir).await.unwrap();
        tokio::fs::write(
            traj_dir.join("transition-missing-trajectory-time-provider.json"),
            serde_json::to_string(&json!({
                "id":"transition-missing-trajectory-time-provider",
                "title":"Transition",
                "updated_at":"2024-01-02T00:00:00Z",
                "model":"model",
                "mode":"task_planner",
                "tool_use":"agent",
                "parent_id":"source-chat",
                "link_type":"mode_transition",
                "previous_response_id":"resp_source",
                "messages":[
                    {"role":"system","content":"target task planner system"},
                    {"role":"user","content":"hello"}
                ],
                "frozen_request_prefix": {
                    "schema_version": 1,
                    "created_at": "2024-01-03T00:00:00Z",
                    "system_prompt": "target task planner system",
                    "tools_canonical": [{"type":"function","function":{"name":"old_source_tool"}}]
                },
                "claude_code_identity": {
                    "device_id":"source-device",
                    "session_id":"source-session"
                },
                "include_project_info":true,
                "checkpoints_enabled":true
            }))
            .unwrap(),
        )
        .await
        .unwrap();

        let loaded = load_trajectory_for_chat(gcx, "transition-missing-trajectory-time-provider")
            .await
            .unwrap();
        let prefix = loaded.thread.frozen_request_prefix.unwrap();
        assert_eq!(
            prefix.system_prompt.as_deref(),
            Some("target task planner system")
        );
        assert!(prefix.tools_canonical.is_none());
        assert!(loaded.thread.claude_code_identity.is_none());
        assert!(loaded.thread.previous_response_id.is_none());
        assert!(loaded.transition_identity_repaired);
    }

    #[tokio::test]
    async fn mode_transition_missing_frozen_system_with_tools_is_repaired() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let traj_dir = dir.path().join(".refact").join("trajectories");
        tokio::fs::create_dir_all(&traj_dir).await.unwrap();
        tokio::fs::write(
            traj_dir.join("transition-missing-system-tools.json"),
            serde_json::to_string(&json!({
                "id":"transition-missing-system-tools",
                "title":"Transition",
                "created_at":"2024-01-01T00:00:00Z",
                "updated_at":"2024-01-01T00:00:00Z",
                "model":"model",
                "mode":"task_planner",
                "tool_use":"agent",
                "parent_id":"source-chat",
                "link_type":"mode_transition",
                "messages":[
                    {"role":"system","content":"target task planner system"},
                    {"role":"user","content":"hello"}
                ],
                "frozen_request_prefix": {
                    "schema_version": 1,
                    "created_at": "2024-01-02T00:00:00Z",
                    "system_prompt": null,
                    "tools_canonical": [{"type":"function","function":{"name":"old_source_tool"}}]
                },
                "include_project_info":true,
                "checkpoints_enabled":true
            }))
            .unwrap(),
        )
        .await
        .unwrap();

        let loaded = load_trajectory_for_chat(gcx, "transition-missing-system-tools")
            .await
            .unwrap();
        let prefix = loaded.thread.frozen_request_prefix.unwrap();
        assert_eq!(
            prefix.system_prompt.as_deref(),
            Some("target task planner system")
        );
        assert!(prefix.tools_canonical.is_none());
        assert!(loaded.transition_identity_repaired);
    }

    #[tokio::test]
    async fn handoff_load_discards_stale_copied_frozen_prefix_and_provider_state() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let traj_dir = dir.path().join(".refact").join("trajectories");
        tokio::fs::create_dir_all(&traj_dir).await.unwrap();
        tokio::fs::write(
            traj_dir.join("handoff-stale-frozen-provider.json"),
            serde_json::to_string(&json!({
                "id":"handoff-stale-frozen-provider",
                "title":"Handoff",
                "created_at":"2024-01-02T00:00:00Z",
                "updated_at":"2024-01-02T00:00:00Z",
                "model":"model",
                "mode":"agent",
                "tool_use":"agent",
                "parent_id":"source-chat",
                "link_type":"handoff",
                "previous_response_id":"resp_source",
                "messages":[
                    {"role":"system","content":"target handoff system"},
                    {"role":"user","content":"hello"}
                ],
                "frozen_request_prefix": {
                    "schema_version": 1,
                    "created_at": "2024-01-01T00:00:00Z",
                    "system_prompt": "source system",
                    "tools_canonical": [{"type":"function","function":{"name":"old_source_tool"}}]
                },
                "claude_code_identity": {
                    "device_id":"source-device",
                    "session_id":"source-session"
                },
                "include_project_info":true,
                "checkpoints_enabled":true
            }))
            .unwrap(),
        )
        .await
        .unwrap();

        let loaded = load_trajectory_for_chat(gcx, "handoff-stale-frozen-provider")
            .await
            .unwrap();
        let prefix = loaded.thread.frozen_request_prefix.unwrap();
        assert_eq!(
            prefix.system_prompt.as_deref(),
            Some("target handoff system")
        );
        assert!(prefix.tools_canonical.is_none());
        assert!(loaded.thread.claude_code_identity.is_none());
        assert!(loaded.thread.previous_response_id.is_none());
        assert!(loaded.transition_identity_repaired);
    }

    #[tokio::test]
    async fn mode_transition_preserves_legitimate_prefix_created_after_trajectory() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let traj_dir = dir.path().join(".refact").join("trajectories");
        tokio::fs::create_dir_all(&traj_dir).await.unwrap();
        tokio::fs::write(
            traj_dir.join("transition-legitimate-prefix.json"),
            serde_json::to_string(&json!({
                "id":"transition-legitimate-prefix",
                "title":"Transition",
                "created_at":"2024-01-01T00:00:00Z",
                "updated_at":"2024-01-01T00:00:00Z",
                "model":"model",
                "mode":"task_planner",
                "tool_use":"agent",
                "parent_id":"source-chat",
                "link_type":"mode_transition",
                "messages":[
                    {"role":"system","content":"target task planner system"},
                    {"role":"user","content":"hello"}
                ],
                "frozen_request_prefix": {
                    "schema_version": 1,
                    "created_at": "2024-01-02T00:00:00Z",
                    "system_prompt": "target task planner system",
                    "tools_canonical": [{"type":"function","function":{"name":"target_tool"}}]
                },
                "include_project_info":true,
                "checkpoints_enabled":true
            }))
            .unwrap(),
        )
        .await
        .unwrap();

        let loaded = load_trajectory_for_chat(gcx, "transition-legitimate-prefix")
            .await
            .unwrap();
        let prefix = loaded.thread.frozen_request_prefix.unwrap();
        assert_eq!(
            prefix.tools_canonical,
            Some(json!([{"type":"function","function":{"name":"target_tool"}}]))
        );
        assert!(!loaded.transition_identity_repaired);
    }

    #[tokio::test]
    async fn ordinary_branch_load_preserves_copied_frozen_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let traj_dir = dir.path().join(".refact").join("trajectories");
        tokio::fs::create_dir_all(&traj_dir).await.unwrap();
        tokio::fs::write(
            traj_dir.join("branch-preserve-frozen.json"),
            serde_json::to_string(&json!({
                "id":"branch-preserve-frozen",
                "title":"Branch",
                "created_at":"2024-01-01T00:00:00Z",
                "updated_at":"2024-01-01T00:00:00Z",
                "model":"model",
                "mode":"agent",
                "tool_use":"agent",
                "parent_id":"source-chat",
                "link_type":"branch",
                "previous_response_id":"resp_source",
                "messages":[
                    {"role":"system","content":"branch system"},
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
                "include_project_info":true,
                "checkpoints_enabled":true
            }))
            .unwrap(),
        )
        .await
        .unwrap();

        let loaded = load_trajectory_for_chat(gcx, "branch-preserve-frozen")
            .await
            .unwrap();
        let prefix = loaded.thread.frozen_request_prefix.unwrap();
        assert_eq!(prefix.system_prompt.as_deref(), Some("source system"));
        assert_eq!(
            prefix.tools_canonical,
            Some(json!([{"type":"function","function":{"name":"source_tool"}}]))
        );
        assert!(loaded.thread.claude_code_identity.is_some());
        assert_eq!(
            loaded.thread.previous_response_id.as_deref(),
            Some("resp_source")
        );
    }

    #[tokio::test]
    async fn mode_transition_open_persists_repaired_frozen_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let (_gcx, app) = make_app_with_workspace(dir.path()).await;
        let chat_id = "transition-open-repair";
        let trajectories_dir = dir.path().join(".refact").join("trajectories");
        tokio::fs::create_dir_all(&trajectories_dir).await.unwrap();
        let path = trajectories_dir.join(format!("{chat_id}.json"));
        tokio::fs::write(
            &path,
            serde_json::to_string(&json!({
                "id": chat_id,
                "title": "Transition Open",
                "model": "model",
                "mode": "task_planner",
                "tool_use": "agent",
                "parent_id": "source-chat",
                "link_type": "mode_transition",
                "previous_response_id": "resp_source",
                "messages": [
                    {"role":"system","content":"target task planner system"},
                    {"role":"user","content":"hello"}
                ],
                "frozen_request_prefix": {
                    "schema_version": 1,
                    "created_at": "2026-05-29T00:00:00Z",
                    "system_prompt": "old planning system",
                    "tools_canonical": [{"type":"function","function":{"name":"old_tool"}}]
                },
                "claude_code_identity": {
                    "device_id":"source-device",
                    "session_id":"source-session"
                },
                "created_at": "2024-01-01T00:00:00Z",
                "updated_at": "2024-01-01T00:00:00Z",
                "include_project_info": true,
                "checkpoints_enabled": true,
                "browser_meta": {
                    "browser_runtime_id": "browser-open",
                    "tab_urls": ["https://example.com/open"],
                    "active_tab_id": "tab-open",
                    "attach_screenshot_on_send": true
                },
                "custom_future_field": {
                    "keep": true,
                    "nested": {"value": 43}
                }
            }))
            .unwrap(),
        )
        .await
        .unwrap();

        let session_arc = crate::chat::get_or_create_session_with_trajectory(
            app.chat_ctx(),
            &app.chat.sessions,
            chat_id,
        )
        .await;
        {
            let session = session_arc.lock().await;
            let prefix = session.thread.frozen_request_prefix.as_ref().unwrap();
            assert_eq!(
                prefix.system_prompt.as_deref(),
                Some("target task planner system")
            );
            assert!(prefix.tools_canonical.is_none());
            assert!(session.thread.claude_code_identity.is_none());
        }

        let raw: serde_json::Value =
            serde_json::from_str(&tokio::fs::read_to_string(path).await.unwrap()).unwrap();
        assert_eq!(
            raw["frozen_request_prefix"]["system_prompt"],
            "target task planner system"
        );
        assert!(raw["frozen_request_prefix"]["tools_canonical"].is_null());
        assert!(raw.get("claude_code_identity").is_none());
        assert!(raw.get("previous_response_id").is_none());
        assert_eq!(raw["browser_meta"]["browser_runtime_id"], "browser-open");
        assert_eq!(raw["custom_future_field"]["nested"]["value"], 43);
    }

    #[tokio::test]
    async fn mode_transition_no_system_provider_cleanup_persists_on_open() {
        let dir = tempfile::tempdir().unwrap();
        let (_gcx, app) = make_app_with_workspace(dir.path()).await;
        let chat_id = "transition-no-system-provider-open";
        let trajectories_dir = dir.path().join(".refact").join("trajectories");
        tokio::fs::create_dir_all(&trajectories_dir).await.unwrap();
        let path = trajectories_dir.join(format!("{chat_id}.json"));
        tokio::fs::write(
            &path,
            serde_json::to_string(&json!({
                "id": chat_id,
                "title": "Transition Open",
                "model": "model",
                "mode": "task_planner",
                "tool_use": "agent",
                "parent_id": "source-chat",
                "link_type": "mode_transition",
                "previous_response_id": "resp_source",
                "messages": [
                    {"role":"user","content":"hello without system"}
                ],
                "claude_code_identity": {
                    "device_id":"source-device",
                    "session_id":"source-session"
                },
                "created_at": "2024-01-01T00:00:00Z",
                "updated_at": "2024-01-01T00:00:00Z",
                "include_project_info": true,
                "checkpoints_enabled": true,
                "browser_meta": {
                    "browser_runtime_id": "browser-immediate",
                    "tab_urls": ["https://example.com/immediate"],
                    "active_tab_id": "tab-immediate",
                    "attach_screenshot_on_send": false
                },
                "custom_future_field": {
                    "keep": true,
                    "nested": {"value": 44}
                }
            }))
            .unwrap(),
        )
        .await
        .unwrap();

        let session_arc = crate::chat::get_or_create_session_with_trajectory(
            app.chat_ctx(),
            &app.chat.sessions,
            chat_id,
        )
        .await;
        {
            let session = session_arc.lock().await;
            assert!(session.thread.frozen_request_prefix.is_none());
            assert!(session.thread.claude_code_identity.is_none());
            assert!(session.thread.previous_response_id.is_none());
        }

        let raw: serde_json::Value =
            serde_json::from_str(&tokio::fs::read_to_string(path).await.unwrap()).unwrap();
        assert!(raw.get("frozen_request_prefix").is_none());
        assert!(raw.get("claude_code_identity").is_none());
        assert!(raw.get("previous_response_id").is_none());
        assert_eq!(
            raw["browser_meta"]["browser_runtime_id"],
            "browser-immediate"
        );
        assert_eq!(raw["custom_future_field"]["nested"]["value"], 44);
    }

    #[tokio::test]
    async fn mode_transition_empty_provider_cleanup_persists_on_open() {
        let dir = tempfile::tempdir().unwrap();
        let (_gcx, app) = make_app_with_workspace(dir.path()).await;
        let chat_id = "transition-empty-provider-open";
        let trajectories_dir = dir.path().join(".refact").join("trajectories");
        tokio::fs::create_dir_all(&trajectories_dir).await.unwrap();
        let path = trajectories_dir.join(format!("{chat_id}.json"));
        tokio::fs::write(
            &path,
            serde_json::to_string(&json!({
                "id": chat_id,
                "title": "Transition Open",
                "model": "model",
                "mode": "task_planner",
                "tool_use": "agent",
                "parent_id": "source-chat",
                "link_type": "mode_transition",
                "previous_response_id": "resp_source",
                "messages": [],
                "claude_code_identity": {
                    "device_id":"source-device",
                    "session_id":"source-session"
                },
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
            app.chat_ctx(),
            &app.chat.sessions,
            chat_id,
        )
        .await;
        {
            let session = session_arc.lock().await;
            assert!(session.messages.is_empty());
            assert!(session.thread.frozen_request_prefix.is_none());
            assert!(session.thread.claude_code_identity.is_none());
            assert!(session.thread.previous_response_id.is_none());
        }

        let raw: serde_json::Value =
            serde_json::from_str(&tokio::fs::read_to_string(path).await.unwrap()).unwrap();
        assert_eq!(raw["messages"].as_array().unwrap().len(), 0);
        assert!(raw.get("frozen_request_prefix").is_none());
        assert!(raw.get("claude_code_identity").is_none());
        assert!(raw.get("previous_response_id").is_none());
    }

    #[tokio::test]
    async fn mode_transition_empty_provider_cleanup_persists_to_original_global_path_on_open() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;
        let chat_id = "transition-empty-provider-global-open";
        let global_trajectories_dir = get_global_trajectories_dir(gcx.clone()).await;
        tokio::fs::create_dir_all(&global_trajectories_dir)
            .await
            .unwrap();
        let path = global_trajectories_dir.join(format!("{chat_id}.json"));
        tokio::fs::write(
            &path,
            serde_json::to_string(&json!({
                "id": chat_id,
                "title": "Transition Global Open",
                "model": "model",
                "mode": "task_planner",
                "tool_use": "agent",
                "parent_id": "source-chat",
                "link_type": "mode_transition",
                "previous_response_id": "resp_source",
                "messages": [],
                "claude_code_identity": {
                    "device_id":"source-device",
                    "session_id":"source-session"
                },
                "created_at": "2024-01-01T00:00:00Z",
                "updated_at": "2024-01-01T00:00:00Z",
                "include_project_info": true,
                "checkpoints_enabled": true,
                "browser_meta": {
                    "browser_runtime_id": "browser-global",
                    "tab_urls": ["https://example.com/global"],
                    "active_tab_id": "tab-global",
                    "attach_screenshot_on_send": true
                },
                "custom_future_field": {
                    "keep": true,
                    "nested": {"value": 46}
                }
            }))
            .unwrap(),
        )
        .await
        .unwrap();
        let project_path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));

        let session_arc = crate::chat::get_or_create_session_with_trajectory(
            app.chat_ctx(),
            &app.chat.sessions,
            chat_id,
        )
        .await;
        {
            let session = session_arc.lock().await;
            assert!(session.messages.is_empty());
            assert!(session.thread.frozen_request_prefix.is_none());
            assert!(session.thread.claude_code_identity.is_none());
            assert!(session.thread.previous_response_id.is_none());
        }

        let raw: serde_json::Value =
            serde_json::from_str(&tokio::fs::read_to_string(&path).await.unwrap()).unwrap();
        assert_eq!(raw["messages"].as_array().unwrap().len(), 0);
        assert!(raw.get("frozen_request_prefix").is_none());
        assert!(raw.get("claude_code_identity").is_none());
        assert!(raw.get("previous_response_id").is_none());
        assert_eq!(raw["browser_meta"]["browser_runtime_id"], "browser-global");
        assert_eq!(raw["custom_future_field"]["nested"]["value"], 46);
        assert!(!tokio::fs::try_exists(project_path).await.unwrap());
    }

    #[tokio::test]
    async fn mode_transition_global_placeholder_title_save_uses_original_path_without_workspace() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let chat_id = "transition-global-placeholder-title-save";
        let global_trajectories_dir = get_global_trajectories_dir(gcx.clone()).await;
        tokio::fs::create_dir_all(&global_trajectories_dir)
            .await
            .unwrap();
        let path = global_trajectories_dir.join(format!("{chat_id}.json"));
        tokio::fs::write(
            &path,
            serde_json::to_string(&json!({
                "id": chat_id,
                "title": "New Chat",
                "model": "model",
                "mode": "task_planner",
                "tool_use": "agent",
                "parent_id": "source-chat",
                "link_type": "mode_transition",
                "previous_response_id": "resp_source",
                "messages": [{"role":"user","content":"hello from global"}],
                "claude_code_identity": {
                    "device_id":"source-device",
                    "session_id":"source-session"
                },
                "created_at": "2024-01-01T00:00:00Z",
                "updated_at": "2024-01-01T00:00:00Z",
                "include_project_info": true,
                "checkpoints_enabled": true,
                "isTitleGenerated": false
            }))
            .unwrap(),
        )
        .await
        .unwrap();
        let mut snapshot = test_snapshot(
            chat_id,
            "New Chat",
            vec![ChatMessage::new(
                "user".to_string(),
                "hello from global".to_string(),
            )],
        );
        snapshot.mode = "task_planner".to_string();
        snapshot.parent_id = Some("source-chat".to_string());
        snapshot.link_type = Some("mode_transition".to_string());
        snapshot.previous_response_id = Some("resp_source".to_string());
        snapshot.claude_code_identity = Some(ClaudeCodeIdentity {
            device_id: "source-device".to_string(),
            session_id: "source-session".to_string(),
        });
        snapshot.is_title_generated = false;
        let project_dir_error = get_trajectories_dir(gcx.clone()).await.unwrap_err();
        assert_eq!(project_dir_error, "No workspace folder found");

        save_trajectory_snapshot(gcx.clone(), snapshot)
            .await
            .unwrap();
        wait_for_file_title(&path, "hello from global").await;

        let raw: serde_json::Value =
            serde_json::from_str(&tokio::fs::read_to_string(&path).await.unwrap()).unwrap();
        assert_eq!(raw["messages"].as_array().unwrap().len(), 1);
        assert_same_optional_path(
            find_trajectory_or_buddy_path(gcx.clone(), chat_id).await,
            &path,
        );
        let all_dirs = get_all_trajectories_dirs(gcx).await;
        assert_eq!(all_dirs, vec![global_trajectories_dir]);
    }

    #[tokio::test]
    async fn no_meta_snapshot_does_not_overwrite_buddy_conversation_collision() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let chat_id = "buddy-normal-collision";
        let buddy_path = dir
            .path()
            .join(".refact")
            .join("buddy")
            .join("chats")
            .join("conversations")
            .join(format!("{chat_id}.json"));
        write_buddy_conversation_file(&buddy_path, chat_id, "Keep Buddy Collision").await;

        save_trajectory_snapshot(
            gcx.clone(),
            test_snapshot(
                chat_id,
                "Normal Chat",
                vec![ChatMessage::new(
                    "user".to_string(),
                    "hello normal".to_string(),
                )],
            ),
        )
        .await
        .unwrap();

        let normal_path = nested_trajectory_path(dir.path(), chat_id, chat_id);
        assert!(tokio::fs::try_exists(&normal_path).await.unwrap());
        let buddy_raw: serde_json::Value =
            serde_json::from_str(&tokio::fs::read_to_string(&buddy_path).await.unwrap()).unwrap();
        assert_eq!(buddy_raw["title"], "Keep Buddy Collision");
        assert!(buddy_raw.get("buddy_meta").is_some());
        assert_same_optional_path(
            find_trajectory_path(gcx.clone(), chat_id).await,
            &normal_path,
        );
        assert_ne!(
            find_trajectory_or_buddy_path(gcx, chat_id)
                .await
                .map(|path| normalized_test_path(&path)),
            Some(normalized_test_path(&buddy_path))
        );
    }

    #[tokio::test]
    async fn no_meta_snapshot_does_not_overwrite_task_trajectory_collision() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let chat_id = "task-normal-collision";
        let task_path = dir
            .path()
            .join(".refact")
            .join("tasks")
            .join("task-collision")
            .join("trajectories")
            .join("agents")
            .join("agent-1")
            .join(format!("{chat_id}.json"));
        write_trajectory_file(
            &task_path,
            chat_id,
            "Keep Task Collision",
            "2024-01-01T00:00:00Z",
        )
        .await;
        let task_before = tokio::fs::read_to_string(&task_path).await.unwrap();

        save_trajectory_snapshot(
            gcx.clone(),
            test_snapshot(
                chat_id,
                "Normal Chat",
                vec![ChatMessage::new(
                    "user".to_string(),
                    "hello normal".to_string(),
                )],
            ),
        )
        .await
        .unwrap();

        let normal_path = nested_trajectory_path(dir.path(), chat_id, chat_id);
        assert!(tokio::fs::try_exists(&normal_path).await.unwrap());
        assert_eq!(
            tokio::fs::read_to_string(&task_path).await.unwrap(),
            task_before
        );
        let normal_raw: serde_json::Value =
            serde_json::from_str(&tokio::fs::read_to_string(&normal_path).await.unwrap()).unwrap();
        assert_eq!(normal_raw["title"], "Normal Chat");
        let task_raw: serde_json::Value =
            serde_json::from_str(&tokio::fs::read_to_string(&task_path).await.unwrap()).unwrap();
        assert_eq!(task_raw["title"], "Keep Task Collision");
        assert_same_optional_path(
            find_trajectory_path(gcx.clone(), chat_id).await,
            &normal_path,
        );
        assert_ne!(
            find_trajectory_path(gcx, chat_id)
                .await
                .map(|path| normalized_test_path(&path)),
            Some(normalized_test_path(&task_path))
        );
    }

    #[tokio::test]
    async fn mode_transition_new_empty_snapshot_without_existing_file_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let chat_id = "transition-new-empty-skip";

        save_trajectory_snapshot(gcx, test_snapshot(chat_id, "New Empty", Vec::new()))
            .await
            .unwrap();

        let path = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join(format!("{chat_id}.json"));
        assert!(!tokio::fs::try_exists(path).await.unwrap());
    }

    #[tokio::test]
    async fn mode_transition_external_reload_persists_provider_cleanup() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;
        let chat_id = "transition-external-reload-provider";
        let trajectories_dir = dir.path().join(".refact").join("trajectories");
        tokio::fs::create_dir_all(&trajectories_dir).await.unwrap();
        let path = trajectories_dir.join(format!("{chat_id}.json"));
        tokio::fs::write(
            &path,
            serde_json::to_string(&json!({
                "id": chat_id,
                "title": "Transition External Reload",
                "model": "model",
                "mode": "task_planner",
                "tool_use": "agent",
                "parent_id": "source-chat",
                "link_type": "mode_transition",
                "previous_response_id": "resp_source",
                "messages": [],
                "claude_code_identity": {
                    "device_id":"source-device",
                    "session_id":"source-session"
                },
                "created_at": "2024-01-01T00:00:00Z",
                "updated_at": "2024-01-01T00:00:00Z",
                "include_project_info": true,
                "checkpoints_enabled": true,
                "browser_meta": {
                    "browser_runtime_id": "browser-immediate",
                    "tab_urls": ["https://example.com/immediate"],
                    "active_tab_id": "tab-immediate",
                    "attach_screenshot_on_send": false
                },
                "custom_future_field": {
                    "keep": true,
                    "nested": {"value": 44}
                }
            }))
            .unwrap(),
        )
        .await
        .unwrap();
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        app.chat
            .sessions
            .write()
            .await
            .insert(chat_id.to_string(), session_arc.clone());

        process_trajectory_change(&gcx.chat_ctx(gcx.clone()), chat_id, false).await;

        {
            let session = session_arc.lock().await;
            assert!(session.messages.is_empty());
            assert!(session.thread.frozen_request_prefix.is_none());
            assert!(session.thread.claude_code_identity.is_none());
            assert!(session.thread.previous_response_id.is_none());
            assert!(!session.trajectory_dirty);
        }
        let raw: serde_json::Value =
            serde_json::from_str(&tokio::fs::read_to_string(path).await.unwrap()).unwrap();
        assert_eq!(raw["messages"].as_array().unwrap().len(), 0);
        assert!(raw.get("frozen_request_prefix").is_none());
        assert!(raw.get("claude_code_identity").is_none());
        assert!(raw.get("previous_response_id").is_none());
        assert_eq!(
            raw["browser_meta"]["browser_runtime_id"],
            "browser-immediate"
        );
        assert_eq!(raw["custom_future_field"]["nested"]["value"], 44);
    }

    #[tokio::test]
    async fn mode_transition_pending_external_reload_persists_provider_cleanup_when_idle() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;
        let chat_id = "transition-pending-external-reload-provider";
        let trajectories_dir = dir.path().join(".refact").join("trajectories");
        tokio::fs::create_dir_all(&trajectories_dir).await.unwrap();
        let path = trajectories_dir.join(format!("{chat_id}.json"));
        tokio::fs::write(
            &path,
            serde_json::to_string(&json!({
                "id": chat_id,
                "title": "Pending Transition External Reload",
                "model": "model",
                "mode": "task_planner",
                "tool_use": "agent",
                "parent_id": "source-chat",
                "link_type": "mode_transition",
                "previous_response_id": "resp_source",
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
                "browser_meta": {
                    "browser_runtime_id": "browser-pending",
                    "tab_urls": ["https://example.com/pending"],
                    "active_tab_id": "tab-pending",
                    "attach_screenshot_on_send": true
                },
                "custom_future_field": {
                    "keep": true,
                    "nested": {"value": 45}
                }
            }))
            .unwrap(),
        )
        .await
        .unwrap();
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

        process_trajectory_change(&gcx.chat_ctx(gcx.clone()).clone(), chat_id, false).await;
        {
            let session = session_arc.lock().await;
            assert_eq!(
                session.external_reload_pending,
                Some(ExternalReloadPending::update(
                    TrajectorySourceIdentity::Normal
                ))
            );
            assert!(session.thread.claude_code_identity.is_none());
            assert!(session.thread.frozen_request_prefix.is_none());
        }

        {
            let mut session = session_arc.lock().await;
            session.runtime.state = SessionState::Idle;
            session.trajectory_dirty = false;
        }
        check_external_reload_pending(&gcx.chat_ctx(gcx.clone()), session_arc.clone()).await;

        {
            let session = session_arc.lock().await;
            assert_eq!(session.external_reload_pending, None);
            let prefix = session.thread.frozen_request_prefix.as_ref().unwrap();
            assert_eq!(
                prefix.system_prompt.as_deref(),
                Some("target task planner system")
            );
            assert!(prefix.tools_canonical.is_none());
            assert!(session.thread.claude_code_identity.is_none());
            assert!(session.thread.previous_response_id.is_none());
            assert!(!session.trajectory_dirty);
        }
        let raw: serde_json::Value =
            serde_json::from_str(&tokio::fs::read_to_string(path).await.unwrap()).unwrap();
        assert_eq!(
            raw["frozen_request_prefix"]["system_prompt"],
            "target task planner system"
        );
        assert!(raw["frozen_request_prefix"]["tools_canonical"].is_null());
        assert!(raw.get("claude_code_identity").is_none());
        assert!(raw.get("previous_response_id").is_none());
        assert_eq!(raw["browser_meta"]["browser_runtime_id"], "browser-pending");
        assert_eq!(raw["custom_future_field"]["nested"]["value"], 45);
    }

    #[tokio::test]
    async fn mode_transition_no_active_session_external_change_persists_provider_cleanup() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let chat_id = "transition-no-active-session-repair";
        let trajectories_dir = dir.path().join(".refact").join("trajectories");
        tokio::fs::create_dir_all(&trajectories_dir).await.unwrap();
        let path = trajectories_dir.join(format!("{chat_id}.json"));
        tokio::fs::write(
            &path,
            serde_json::to_string(&json!({
                "id": chat_id,
                "title": "No Active Session Repair",
                "model": "model",
                "mode": "task_planner",
                "tool_use": "agent",
                "parent_id": "source-chat",
                "link_type": "mode_transition",
                "previous_response_id": "resp_source",
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
                "browser_meta": {
                    "browser_runtime_id": "browser-1",
                    "tab_urls": ["https://example.com"],
                    "active_tab_id": "tab-1",
                    "attach_screenshot_on_send": true
                },
                "custom_future_field": {
                    "keep": true,
                    "nested": {"value": 42}
                }
            }))
            .unwrap(),
        )
        .await
        .unwrap();

        process_trajectory_change(&gcx.chat_ctx(gcx.clone()), chat_id, false).await;

        let raw: serde_json::Value =
            serde_json::from_str(&tokio::fs::read_to_string(path).await.unwrap()).unwrap();
        assert_eq!(
            raw["frozen_request_prefix"]["system_prompt"],
            "target task planner system"
        );
        assert!(raw["frozen_request_prefix"]["tools_canonical"].is_null());
        assert!(raw.get("claude_code_identity").is_none());
        assert!(raw.get("previous_response_id").is_none());
        assert_eq!(raw["browser_meta"]["browser_runtime_id"], "browser-1");
        assert_eq!(
            raw["browser_meta"]["tab_urls"],
            json!(["https://example.com"])
        );
        assert_eq!(raw["custom_future_field"]["nested"]["value"], 42);
    }

    #[tokio::test]
    async fn repair_source_id_mismatch_errors_without_writing() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let chat_id = "repair-source-original";
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
                "title": "Repair Source Original",
                "model": "model",
                "mode": "task_planner",
                "tool_use": "agent",
                "parent_id": "source-chat",
                "link_type": "mode_transition",
                "previous_response_id": "resp_source",
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
                "custom_future_field": {"keep": true}
            }))
            .unwrap(),
        )
        .await
        .unwrap();

        let mut loaded = load_trajectory_for_chat(gcx.clone(), chat_id)
            .await
            .unwrap();
        assert!(loaded.transition_identity_repaired);
        apply_mode_defaults_to_thread(
            gcx.clone(),
            &mut loaded.thread,
            loaded.auto_approve_editing_tools_present,
            loaded.auto_approve_dangerous_commands_present,
        )
        .await;
        let repair_patch = loaded.repair_patch();
        let replacement = serde_json::to_string(&json!({
            "id": "repair-source-replacement",
            "title": "Replacement Must Stay",
            "model": "model",
            "mode": "agent",
            "tool_use": "agent",
            "messages": [],
            "created_at": "2024-01-01T00:00:00Z",
            "updated_at": "2024-01-01T00:00:00Z",
            "include_project_info": true,
            "checkpoints_enabled": true,
            "previous_response_id": "replacement-response"
        }))
        .unwrap();
        tokio::fs::write(&path, &replacement).await.unwrap();

        let err = persist_loaded_trajectory_repair_raw(&gcx.chat_ctx(gcx.clone()).clone(), &repair_patch)
            .await
            .unwrap_err();

        assert!(err.contains("Trajectory source id mismatch for repair"));
        assert_eq!(tokio::fs::read_to_string(path).await.unwrap(), replacement);
    }

