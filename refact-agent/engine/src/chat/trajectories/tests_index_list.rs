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
    fn internal_trace_events_are_not_displayable_chats() {
        let event_with_link_type = |link_type: Option<String>| -> TrajectoryEvent {
            let mut value = serde_json::json!({
                "type": "trajectory_updated",
                "id": "trace-chat",
            });
            if let Some(link_type) = link_type {
                value["link_type"] = serde_json::Value::String(link_type);
            }
            serde_json::from_value(value).unwrap()
        };

        assert!(!trajectory_event_is_displayable_chat(
            &event_with_link_type(Some(internal_trace_link_type("title_generation")))
        ));
        assert!(trajectory_event_is_displayable_chat(&event_with_link_type(
            None
        )));
    }

    #[tokio::test]
    async fn task_agent_index_stays_in_its_own_agent_folder() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let agent_dir = dir
            .path()
            .join(".refact")
            .join("tasks")
            .join("task-index-scope")
            .join("trajectories")
            .join("agents")
            .join("agent-7");
        tokio::fs::create_dir_all(&agent_dir).await.unwrap();

        let resolved = index_dir_for_trajectory_file(gcx, &agent_dir.join("agent-chat.json"))
            .await
            .expect("index dir must resolve");

        assert_eq!(
            resolved, agent_dir,
            "a task agent index must not be relocated to the shared agents/ folder"
        );
    }

    #[tokio::test]
    async fn find_trajectory_parent_id_reports_root_child_and_unknown() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;

        save_trajectory_snapshot(
            gcx.clone(),
            test_snapshot(
                "root-parent-id-chat",
                "Root",
                vec![ChatMessage::new("user".to_string(), "root".to_string())],
            ),
        )
        .await
        .unwrap();

        let mut child = test_snapshot(
            "child-parent-id-chat",
            "Child",
            vec![ChatMessage::new("user".to_string(), "child".to_string())],
        );
        child.parent_id = Some("root-parent-id-chat".to_string());
        save_trajectory_snapshot(gcx.clone(), child).await.unwrap();

        assert_eq!(
            find_trajectory_parent_id(gcx.clone(), "root-parent-id-chat").await,
            Some(None)
        );
        assert_eq!(
            find_trajectory_parent_id(gcx.clone(), "child-parent-id-chat").await,
            Some(Some("root-parent-id-chat".to_string()))
        );
        assert_eq!(
            find_trajectory_parent_id(gcx, "missing-parent-id-chat").await,
            None
        );
    }

    #[tokio::test]
    async fn remove_stale_entries_targets_only_the_owning_index_directory() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;

        let root_dir = dir.path().join(".refact").join("trajectories");
        let task_dir = dir
            .path()
            .join(".refact")
            .join("tasks")
            .join("task-1")
            .join("trajectories");
        tokio::fs::create_dir_all(&root_dir).await.unwrap();
        tokio::fs::create_dir_all(&task_dir).await.unwrap();

        let stale_id = "scoped-stale-entry-chat";
        let stale_path = root_dir.join(format!("{stale_id}.json"));
        let stale_value = serde_json::json!({
            "id": stale_id,
            "title": "Stale",
            "created_at": "2026-01-01T00:00:00Z",
            "updated_at": "2026-01-01T00:00:01Z",
            "model": "test-model",
            "mode": "agent",
            "tool_use": "agent",
            "messages": []
        });
        tokio::fs::write(&stale_path, stale_value.to_string())
            .await
            .unwrap();
        trajectory_index::upsert_trajectory_index_entry_from_owned_value_with_rollout(
            &app.chat.trajectory_index_coordinator,
            &root_dir,
            &stale_path,
            stale_value,
            None,
        )
        .await
        .unwrap();
        tokio::fs::remove_file(&stale_path).await.unwrap();

        let fresh_id = "other-dir-chat";
        let fresh_path = task_dir.join(format!("{fresh_id}.json"));
        let fresh_value = serde_json::json!({
            "id": fresh_id,
            "title": "Fresh",
            "created_at": "2026-01-01T00:00:00Z",
            "updated_at": "2026-01-01T00:00:01Z",
            "model": "test-model",
            "mode": "agent",
            "tool_use": "agent",
            "messages": []
        });
        tokio::fs::write(&fresh_path, fresh_value.to_string())
            .await
            .unwrap();
        trajectory_index::upsert_trajectory_index_entry_from_owned_value_with_rollout(
            &app.chat.trajectory_index_coordinator,
            &task_dir,
            &fresh_path,
            fresh_value,
            None,
        )
        .await
        .unwrap();

        remove_stale_trajectory_index_entries(&gcx.chat_ctx(gcx.clone()).clone(), stale_id).await;

        let root_entries = trajectory_index::list_trajectory_entries_with_rollout_for(
            &app.chat.trajectory_index_coordinator,
            &root_dir,
            None,
            trajectory_index::TrajectoryIndexListingCaller::StaleEntryCleanup,
        )
        .await
        .unwrap();
        let task_entries = trajectory_index::list_trajectory_entries_with_rollout_for(
            &app.chat.trajectory_index_coordinator,
            &task_dir,
            None,
            trajectory_index::TrajectoryIndexListingCaller::StaleEntryCleanup,
        )
        .await
        .unwrap();

        assert!(root_entries.iter().all(|entry| entry.id != stale_id));
        assert_eq!(task_entries.len(), 1);
        assert_eq!(task_entries[0].id, fresh_id);
    }

    // Clears the process-wide metadata cache, so it must not run beside the tests that read it.

    #[serial(trajectory_perf)]
    #[test]
    fn trajectory_metadata_cache_lru_evicts_oldest_path() {
        let mut cache = trajectory_metadata_cache()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        cache.clear();
        drop(cache);

        for index in 0..MAX_CACHED_TRAJECTORY_METADATA {
            let path = PathBuf::from(format!("/tmp/trajectory-meta-{index}.json"));
            store_trajectory_metadata_cache_entry(
                &path,
                TrajectoryMetadataCacheEntry {
                    chat_id: format!("chat-{index}"),
                    metadata: serde_json::Map::new(),
                    stamp: TrajectoryFileStamp {
                        len: index as u64,
                        modified_unix_ms: 0,
                    },
                    last_access: 0,
                },
            );
        }

        let oldest = trajectory_metadata_cache_key(&PathBuf::from("/tmp/trajectory-meta-0.json"));
        let newest = trajectory_metadata_cache_key(&PathBuf::from(format!(
            "/tmp/trajectory-meta-{}.json",
            MAX_CACHED_TRAJECTORY_METADATA - 1
        )));
        let extra = PathBuf::from("/tmp/trajectory-meta-extra.json");
        store_trajectory_metadata_cache_entry(
            &extra,
            TrajectoryMetadataCacheEntry {
                chat_id: "chat-extra".to_string(),
                metadata: serde_json::Map::new(),
                stamp: TrajectoryFileStamp {
                    len: 999,
                    modified_unix_ms: 0,
                },
                last_access: 0,
            },
        );

        let extra = trajectory_metadata_cache_key(&extra);
        let cache = trajectory_metadata_cache()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        assert!(!cache.contains_key(&oldest));
        assert!(cache.contains_key(&newest));
        assert!(cache.contains_key(&extra));
    }

    #[tokio::test]
    async fn start_trajectory_watcher_is_idempotent_while_running() {
        reset_test_trajectory_watcher_task();
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;

        start_trajectory_watcher(gcx.clone());
        start_trajectory_watcher(gcx);

        let guard = TRAJECTORY_WATCHER_TASK
            .get_or_init(|| StdMutex::new(None))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        assert!(guard.as_ref().is_some_and(|handle| !handle.is_finished()));

        drop(guard);
        reset_test_trajectory_watcher_task();
    }

    #[test]
    fn internal_traces_are_not_displayable_chats() {
        let internal: TrajectoryListData = serde_json::from_value(serde_json::json!({
            "id": "internal-1",
            "updated_at": "2024-01-01T00:00:00Z",
            "mode": "agent",
            "link_type": "internal:title_generation"
        }))
        .unwrap();
        assert!(!trajectory_list_data_is_displayable_chat(&internal));

        let plain: TrajectoryListData = serde_json::from_value(serde_json::json!({
            "id": "plain-1",
            "updated_at": "2024-01-01T00:00:00Z",
            "mode": "agent"
        }))
        .unwrap();
        assert!(trajectory_list_data_is_displayable_chat(&plain));
    }

    #[serial(trajectory_perf)]
    #[tokio::test]
    async fn watcher_picks_up_external_edit_to_task_planner_trajectory() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;
        let mut rx = app.chat.trajectory_events_tx.subscribe();

        start_trajectory_watcher(gcx.clone());
        wait_for_watcher_start().await;
        drain_trajectory_events(&mut rx).await;

        let chat_id = "planner-watch-chat";
        let path = dir
            .path()
            .join(".refact")
            .join("tasks")
            .join("task-watch")
            .join("trajectories")
            .join("planner")
            .join(format!("{}.json", chat_id));
        write_trajectory_file(&path, chat_id, "Planner Watch", "2024-01-01T00:00:01Z").await;
        if cfg!(target_os = "macos") {
            process_trajectory_change_for_source(
                gcx.clone(),
                chat_id,
                false,
                Some(trajectory_source_identity_from_path(
                    &path,
                    &get_all_task_roots(gcx.clone()).await,
                )),
            )
            .await;
        }

        let event = wait_for_trajectory_event(&mut rx, chat_id).await;
        assert_eq!(event.event_type, "updated");
        assert_eq!(event.title.as_deref(), Some("Planner Watch"));
    }

    #[serial(trajectory_perf)]
    #[tokio::test]
    async fn watcher_ignores_non_trajectory_files_in_tasks_dir() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;
        let mut rx = app.chat.trajectory_events_tx.subscribe();

        start_trajectory_watcher(gcx.clone());
        wait_for_watcher_start().await;
        drain_trajectory_events(&mut rx).await;

        let task_dir = dir.path().join(".refact").join("tasks").join("task-ignore");
        tokio::fs::create_dir_all(&task_dir).await.unwrap();
        tokio::fs::write(task_dir.join("meta.yaml"), "id: task-ignore\n")
            .await
            .unwrap();

        assert_no_trajectory_event_for(&mut rx, std::time::Duration::from_millis(700)).await;
    }

    #[test]
    fn trajectory_dispatch_ignores_generated_index_files() {
        let dir = tempfile::tempdir().unwrap();
        let task_root = dir.path().join(".refact").join("tasks");
        std::fs::create_dir_all(&task_root).unwrap();
        let task_index = task_root
            .join("task-index")
            .join("trajectories")
            .join("agents")
            .join("agent-1")
            .join("index.json");
        let task_chat = task_index.with_file_name("chat-1.json");
        let normal_index = dir
            .path()
            .join(".refact")
            .join("trajectories")
            .join("index.json");

        assert!(!should_dispatch_trajectory_path(
            &task_index,
            &[task_root.clone()]
        ));
        assert!(!should_dispatch_trajectory_path(
            &normal_index,
            &[task_root.clone()]
        ));
        assert!(should_dispatch_trajectory_path(&task_chat, &[task_root]));
    }

    #[serial(trajectory_perf)]
    #[tokio::test]
    async fn watcher_picks_up_new_task_dir_created_after_startup() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;
        let mut rx = app.chat.trajectory_events_tx.subscribe();

        start_trajectory_watcher(gcx.clone());
        wait_for_watcher_start().await;
        drain_trajectory_events(&mut rx).await;

        let chat_id = "new-task-agent-chat";
        let path = dir
            .path()
            .join(".refact")
            .join("tasks")
            .join("task-created-later")
            .join("trajectories")
            .join("agents")
            .join("agent-1")
            .join(format!("{}.json", chat_id));
        write_trajectory_file(&path, chat_id, "New Task Agent", "2024-01-01T00:00:02Z").await;
        if cfg!(target_os = "macos") {
            process_trajectory_change_for_source(
                gcx.clone(),
                chat_id,
                false,
                Some(trajectory_source_identity_from_path(
                    &path,
                    &get_all_task_roots(gcx.clone()).await,
                )),
            )
            .await;
        }

        let event = wait_for_trajectory_event(&mut rx, chat_id).await;
        assert_eq!(event.event_type, "updated");
        assert_eq!(event.title.as_deref(), Some("New Task Agent"));
    }

    #[tokio::test]
    async fn paginated_list_includes_task_planner_and_agent_trajectories() {
        let dir = tempfile::tempdir().unwrap();
        let (_gcx, app) = make_app_with_workspace(dir.path()).await;
        let root = dir.path().join(".refact");

        write_trajectory_file(
            &root.join("trajectories").join("project-chat.json"),
            "project-chat",
            "Project Chat",
            "2024-01-01T00:00:03Z",
        )
        .await;
        write_trajectory_file(
            &root
                .join("tasks")
                .join("task-list")
                .join("trajectories")
                .join("planner")
                .join("planner-chat.json"),
            "planner-chat",
            "Planner Chat",
            "2024-01-01T00:00:02Z",
        )
        .await;
        write_trajectory_file(
            &root
                .join("tasks")
                .join("task-list")
                .join("trajectories")
                .join("agents")
                .join("agent-1")
                .join("agent-chat.json"),
            "agent-chat",
            "Agent Chat",
            "2024-01-01T00:00:01Z",
        )
        .await;

        let response = handle_v1_trajectories_list(
            State(app),
            axum::extract::Query(TrajectoriesListQuery {
                limit: Some(10),
                cursor: None,
                displayable_only: None,
            }),
        )
        .await
        .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = hyper::body::to_bytes(response.into_body()).await.unwrap();
        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let items = payload["items"].as_array().unwrap();
        let ids: std::collections::HashSet<_> = items
            .iter()
            .filter_map(|item| item["id"].as_str())
            .collect();

        assert_eq!(payload["total_count"].as_u64(), Some(3));
        assert!(ids.contains("project-chat"));
        assert!(ids.contains("planner-chat"));
        assert!(ids.contains("agent-chat"));

        let planner = items
            .iter()
            .find(|item| item["id"].as_str() == Some("planner-chat"))
            .unwrap();
        assert_eq!(planner["task_id"].as_str(), Some("task-list"));
        assert_eq!(planner["task_role"].as_str(), Some("planner"));

        let agent = items
            .iter()
            .find(|item| item["id"].as_str() == Some("agent-chat"))
            .unwrap();
        assert_eq!(agent["task_id"].as_str(), Some("task-list"));
        assert_eq!(agent["task_role"].as_str(), Some("agents"));
        assert_eq!(agent["agent_id"].as_str(), Some("agent-1"));
    }

    #[tokio::test]
    async fn paginated_list_displayable_only_filters_task_and_subagent_trajectories() {
        let dir = tempfile::tempdir().unwrap();
        let (_gcx, app) = make_app_with_workspace(dir.path()).await;
        let root = dir.path().join(".refact");

        write_trajectory_file(
            &root.join("trajectories").join("project-chat.json"),
            "project-chat",
            "Project Chat",
            "2024-01-01T00:00:05Z",
        )
        .await;

        let subagent_path = root.join("trajectories").join("subagent-chat.json");
        let mut subagent =
            sample_trajectory("subagent-chat", "Subagent Chat", "2024-01-01T00:00:04Z");
        subagent["parent_id"] = json!("project-chat");
        subagent["link_type"] = json!("subagent");
        tokio::fs::create_dir_all(subagent_path.parent().unwrap())
            .await
            .unwrap();
        tokio::fs::write(&subagent_path, serde_json::to_string(&subagent).unwrap())
            .await
            .unwrap();

        let planner_path = root
            .join("tasks")
            .join("task-list")
            .join("trajectories")
            .join("planner")
            .join("planner-chat.json");
        let mut planner =
            sample_trajectory("planner-chat", "Planner Chat", "2024-01-01T00:00:035Z");
        planner["mode"] = json!("task_planner");
        planner["task_meta"] = json!({ "task_id": "task-list", "role": "planner" });
        tokio::fs::create_dir_all(planner_path.parent().unwrap())
            .await
            .unwrap();
        tokio::fs::write(&planner_path, serde_json::to_string(&planner).unwrap())
            .await
            .unwrap();

        let review_path = root
            .join("tasks")
            .join("task-list")
            .join("trajectories")
            .join("planner")
            .join("review-chat.json");
        let mut review = sample_trajectory("review-chat", "Review Chat", "2024-01-01T00:00:045Z");
        review["mode"] = json!("review");
        review["parent_id"] = json!("planner-chat");
        review["link_type"] = json!("mode_transition");
        review["task_meta"] = json!({ "task_id": "task-list", "role": "planner" });
        tokio::fs::write(&review_path, serde_json::to_string(&review).unwrap())
            .await
            .unwrap();

        let task_path = root
            .join("trajectories")
            .join("legacy-task-agent-chat.json");
        let mut legacy_task = sample_trajectory(
            "legacy-task-agent-chat",
            "Legacy Task Agent Chat",
            "2024-01-01T00:00:02Z",
        );
        legacy_task["mode"] = json!("task_agent");
        tokio::fs::write(&task_path, serde_json::to_string(&legacy_task).unwrap())
            .await
            .unwrap();

        let buddy_path = root.join("trajectories").join("buddy-chat.json");
        let mut buddy = sample_trajectory("buddy-chat", "Buddy Chat", "2024-01-01T00:00:015Z");
        buddy["buddy_meta"] = json!({ "is_buddy_chat": true });
        tokio::fs::write(&buddy_path, serde_json::to_string(&buddy).unwrap())
            .await
            .unwrap();

        let missing_link_path = root.join("trajectories").join("missing-link-child.json");
        let mut missing_link_child = sample_trajectory(
            "missing-link-child",
            "Missing Link Child",
            "2024-01-01T00:00:01Z",
        );
        missing_link_child["parent_id"] = json!("project-chat");
        tokio::fs::write(
            &missing_link_path,
            serde_json::to_string(&missing_link_child).unwrap(),
        )
        .await
        .unwrap();

        let page = list_trajectories_page(app, 10, None, true).await.unwrap();
        let ids: std::collections::HashSet<_> =
            page.items.iter().map(|item| item.id.as_str()).collect();

        assert_eq!(
            ids,
            std::collections::HashSet::from(["project-chat", "review-chat"])
        );
        assert_eq!(page.total_count, 2);
        assert!(!page.has_more);
    }

    #[tokio::test]
    async fn all_trajectories_displayable_only_filters_subchats_without_parent() {
        let dir = tempfile::tempdir().unwrap();
        let (_gcx, app) = make_app_with_workspace(dir.path()).await;
        let root = dir.path().join(".refact");

        write_trajectory_file(
            &root.join("trajectories").join("project-chat.json"),
            "project-chat",
            "Project Chat",
            "2024-01-01T00:00:05Z",
        )
        .await;

        let linked_path = root.join("trajectories").join("subchat-linked.json");
        let mut linked = sample_trajectory("subchat-linked", "Subchat", "2024-01-01T00:00:04Z");
        linked["parent_id"] = json!("project-chat");
        linked["link_type"] = json!("subagent");
        tokio::fs::create_dir_all(linked_path.parent().unwrap())
            .await
            .unwrap();
        tokio::fs::write(&linked_path, serde_json::to_string(&linked).unwrap())
            .await
            .unwrap();

        let orphan_path = root.join("trajectories").join("subchat-orphan.json");
        let mut orphan = sample_trajectory("subchat-orphan", "Subchat", "2024-01-01T00:00:03Z");
        orphan["link_type"] = json!("subagent");
        tokio::fs::write(&orphan_path, serde_json::to_string(&orphan).unwrap())
            .await
            .unwrap();

        let handoff_path = root.join("trajectories").join("handoff-chat.json");
        let mut handoff = sample_trajectory("handoff-chat", "Handoff Chat", "2024-01-01T00:00:02Z");
        handoff["parent_id"] = json!("project-chat");
        handoff["link_type"] = json!("handoff");
        tokio::fs::write(&handoff_path, serde_json::to_string(&handoff).unwrap())
            .await
            .unwrap();

        let listed = list_all_trajectories_meta(app.clone(), true).await.unwrap();
        let ids: std::collections::HashSet<_> =
            listed.iter().map(|item| item.id.as_str()).collect();
        assert_eq!(
            ids,
            std::collections::HashSet::from(["project-chat", "handoff-chat"])
        );

        let unfiltered = list_all_trajectories_meta(app, false).await.unwrap();
        let unfiltered_ids: std::collections::HashSet<_> =
            unfiltered.iter().map(|item| item.id.as_str()).collect();
        assert!(
            unfiltered_ids.contains("subchat-orphan") && unfiltered_ids.contains("subchat-linked"),
            "displayable_only=false must keep returning every trajectory"
        );
    }

    #[test]
    fn subagentic_link_types_are_hidden_without_a_parent_id() {
        let orphan_subchat = |link_type: &str| -> TrajectoryListData {
            TrajectoryListData {
                id: "subchat-orphan".to_string(),
                updated_at: "2024-01-01T00:00:00Z".to_string(),
                mode: Some("agent".to_string()),
                extra: serde_json::from_value(json!({ "link_type": link_type })).unwrap(),
            }
        };

        for link_type in ["subagent", "gather_files", "review_agent"] {
            assert!(
                !trajectory_list_data_is_displayable_chat(&orphan_subchat(link_type)),
                "{link_type} without parent_id must stay hidden"
            );
        }

        for link_type in ["handoff", "mode_transition", "branch"] {
            assert!(
                trajectory_list_data_is_displayable_chat(&orphan_subchat(link_type)),
                "{link_type} must remain visible"
            );
        }
    }

    #[test]
    fn displayable_chat_predicates_follow_mode_not_task_scope() {
        let review = TrajectoryListData {
            id: "review-chat".to_string(),
            updated_at: "2024-01-01T00:00:00Z".to_string(),
            mode: Some("review".to_string()),
            extra: serde_json::from_value(json!({
                "parent_id": "planner-chat",
                "link_type": "mode_transition",
                "task_meta": { "task_id": "task-list", "role": "planner" }
            }))
            .unwrap(),
        };
        assert!(trajectory_list_data_is_displayable_chat(&review));

        let planner = TrajectoryListData {
            id: "planner-chat".to_string(),
            updated_at: "2024-01-01T00:00:00Z".to_string(),
            mode: Some("task_planner".to_string()),
            extra: serde_json::from_value(json!({
                "task_meta": { "task_id": "task-list", "role": "planner" }
            }))
            .unwrap(),
        };
        assert!(!trajectory_list_data_is_displayable_chat(&planner));

        let buddy = TrajectoryListData {
            id: "buddy-chat".to_string(),
            updated_at: "2024-01-01T00:00:00Z".to_string(),
            mode: Some("review".to_string()),
            extra: serde_json::from_value(json!({ "buddy_meta": { "is_buddy_chat": true } }))
                .unwrap(),
        };
        assert!(!trajectory_list_data_is_displayable_chat(&buddy));

        let subagent = TrajectoryListData {
            id: "subagent-chat".to_string(),
            updated_at: "2024-01-01T00:00:00Z".to_string(),
            mode: Some("agent".to_string()),
            extra: serde_json::from_value(json!({
                "parent_id": "project-chat",
                "link_type": "subagent"
            }))
            .unwrap(),
        };
        assert!(!trajectory_list_data_is_displayable_chat(&subagent));

        let review_event: TrajectoryEvent = serde_json::from_value(json!({
            "type": "updated",
            "id": "review-chat",
            "mode": "review",
            "task_id": "task-list",
            "parent_id": "planner-chat",
            "link_type": "mode_transition"
        }))
        .unwrap();
        assert!(trajectory_event_is_displayable_chat(&review_event));

        let planner_event: TrajectoryEvent = serde_json::from_value(json!({
            "type": "updated",
            "id": "planner-chat",
            "mode": "task_planner",
            "task_id": "task-list"
        }))
        .unwrap();
        assert!(!trajectory_event_is_displayable_chat(&planner_event));
    }

    #[tokio::test]
    async fn paginated_list_backfills_when_hydration_skips_changed_candidate() {
        let dir = tempfile::tempdir().unwrap();
        let (_gcx, app) = make_app_with_workspace(dir.path()).await;
        let root = dir.path().join(".refact").join("trajectories");
        write_trajectory_file(
            &root.join("top-chat.json"),
            "top-chat",
            "Top Chat",
            "2024-01-01T00:00:03Z",
        )
        .await;
        write_trajectory_file(
            &root.join("skip-chat.json"),
            "skip-chat",
            "Skip Chat",
            "2024-01-01T00:00:02Z",
        )
        .await;
        write_trajectory_file(
            &root.join("backfill-chat.json"),
            "backfill-chat",
            "Backfill Chat",
            "2024-01-01T00:00:01Z",
        )
        .await;
        tokio::fs::write(
            root.join("skip-chat.json"),
            serde_json::to_string(&sample_trajectory(
                "other-chat",
                "Changed",
                "2024-01-01T00:00:02Z",
            ))
            .unwrap(),
        )
        .await
        .unwrap();

        let candidates = vec![
            TrajectoryListCandidate {
                id: "top-chat".to_string(),
                updated_at: "2024-01-01T00:00:03Z".to_string(),
                path: root.join("top-chat.json"),
                indexed_meta: None,
                indexed_file_len: None,
                indexed_file_modified_unix_ms: None,
            },
            TrajectoryListCandidate {
                id: "skip-chat".to_string(),
                updated_at: "2024-01-01T00:00:02Z".to_string(),
                path: root.join("skip-chat.json"),
                indexed_meta: None,
                indexed_file_len: None,
                indexed_file_modified_unix_ms: None,
            },
            TrajectoryListCandidate {
                id: "backfill-chat".to_string(),
                updated_at: "2024-01-01T00:00:01Z".to_string(),
                path: root.join("backfill-chat.json"),
                indexed_meta: None,
                indexed_file_len: None,
                indexed_file_modified_unix_ms: None,
            },
        ];
        let task_roots = Vec::new();

        let (items, has_more) = hydrate_trajectory_list_page(app, candidates, 2, &task_roots).await;
        let ids: Vec<_> = items.iter().map(|item| item.id.as_str()).collect();

        assert_eq!(ids, vec!["top-chat", "backfill-chat"]);
        assert!(!has_more);
    }

    #[test]
    fn test_validate_trajectory_id_rejects_path_traversal() {
        assert!(validate_trajectory_id("../etc/passwd").is_err());
        assert!(validate_trajectory_id("..").is_err());
        assert!(validate_trajectory_id("a/../b").is_err());
    }

