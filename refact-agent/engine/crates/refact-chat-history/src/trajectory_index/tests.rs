use super::*;
use refact_perf::perf_diagnostics::{self, MemoryPerfSink, PerfClock, PerfComponent, PerfRecorder};
use serde_json::json;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

struct TestClock {
    now: AtomicU64,
}

impl TestClock {
    fn new() -> Self {
        Self {
            now: AtomicU64::new(0),
        }
    }
}

impl PerfClock for TestClock {
    fn now_us(&self) -> u64 {
        self.now.fetch_add(1, Ordering::SeqCst)
    }
}

fn install_perf_recorder() -> (perf_diagnostics::TestRecorderGuard, Arc<MemoryPerfSink>) {
    let sink = Arc::new(MemoryPerfSink::new());
    let recorder = Arc::new(PerfRecorder::with_salt(
        Arc::new(TestClock::new()),
        sink.clone(),
        [3; 32],
    ));
    (perf_diagnostics::install_test_recorder(recorder), sink)
}

pub(super) async fn write_trajectory(dir: &Path, id: &str, title: &str, mode: &str) -> PathBuf {
    fs::create_dir_all(dir).await.unwrap();
    let path = dir.join(format!("{id}.json"));
    let value = json!({
        "id": id,
        "title": title,
        "created_at": "2026-01-01T00:00:00Z",
        "updated_at": "2026-01-01T00:00:01Z",
        "model": "test-model",
        "mode": mode,
        "tool_use": "agent",
        "messages": [{"role":"user","content":"hello"}],
        "root_chat_id": id
    });
    fs::write(&path, serde_json::to_string_pretty(&value).unwrap())
        .await
        .unwrap();
    path
}

#[tokio::test]
async fn rebuild_creates_index_from_files_and_skips_index_json() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("trajectories");
    write_trajectory(&dir, "chat-1", "One", "agent").await;
    write_trajectory(&dir, "chat-2", "Two", "agent").await;
    fs::write(trajectory_index_path(&dir), "{}").await.unwrap();

    let entries = rebuild_trajectory_index_from_disk(&dir, None)
        .await
        .unwrap();

    assert_eq!(entries.len(), 2);
    let index = read_trajectory_index(&dir).await.unwrap().unwrap();
    assert_eq!(index.entries.len(), 2);
    assert!(index
        .entries
        .iter()
        .all(|entry| entry.file_name != TRAJECTORY_INDEX_FILE));
}

#[serial_test::serial(trajectory_perf)]
#[tokio::test]
async fn performance_diagnostics_cover_index_success_and_failure_boundaries() {
    let _lock = perf_diagnostics::PERF_RECORDER_TEST_LOCK.lock().unwrap();
    let (_guard, sink) = install_perf_recorder();
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("trajectories");
    write_trajectory(&dir, "chat-1", "One", "agent").await;

    let entry = entry_from_trajectory_value(
        &dir,
        &dir.join("chat-1.json"),
        &serde_json::from_str(&fs::read_to_string(dir.join("chat-1.json")).await.unwrap()).unwrap(),
        None,
    )
    .unwrap();
    upsert_trajectory_index_entry(&dir, entry).await.unwrap();
    rebuild_trajectory_index_from_disk(&dir, None)
        .await
        .unwrap();

    fs::write(trajectory_index_path(&dir), "not-json")
        .await
        .unwrap();
    assert!(read_trajectory_index(&dir).await.is_err());

    let events = sink.events();
    let components: Vec<_> = events.iter().map(|event| event.component).collect();
    assert!(components.contains(&PerfComponent::TrajectoryIndexLockWait.as_str()));
    assert!(components.contains(&PerfComponent::TrajectoryIndexRead.as_str()));
    assert!(components.contains(&PerfComponent::TrajectoryIndexWrite.as_str()));
    assert!(components.contains(&PerfComponent::TrajectoryIndexRebuild.as_str()));
    assert!(events.iter().any(|event| event.outcome == "failure"));
    assert!(events.iter().all(|event| event.path_hash.is_some()));
}

#[serial_test::serial(trajectory_perf)]
#[tokio::test]
async fn coordinator_performance_diagnostics_cover_load_reconcile_flush_and_enqueue() {
    let _lock = perf_diagnostics::PERF_RECORDER_TEST_LOCK.lock().unwrap();
    let (_guard, sink) = install_perf_recorder();
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("trajectories");
    let path_1 = write_trajectory(&dir, "chat-1", "One", "agent").await;
    let coordinator = TrajectoryIndexCoordinator::new();

    coordinator
        .upsert(&dir, entry_for_path(&dir, &path_1).await)
        .await
        .unwrap();
    coordinator.list_entries(&dir, None).await.unwrap();

    write_trajectory(&dir, "chat-2", "Two", "agent").await;
    coordinator.reconcile(&dir, None).await.unwrap();
    coordinator.flush_all().await.unwrap();

    let events = sink.events();
    let components: Vec<_> = events.iter().map(|event| event.component).collect();
    for component in [
        PerfComponent::TrajectoryIndexCoordinatorLoad,
        PerfComponent::TrajectoryIndexCoordinatorReconcile,
        PerfComponent::TrajectoryIndexCoordinatorFlush,
        PerfComponent::TrajectoryIndexEnqueue,
        PerfComponent::TrajectoryIndexRead,
        PerfComponent::TrajectoryIndexLockWait,
        PerfComponent::TrajectoryIndexWrite,
    ] {
        assert!(
            components.contains(&component.as_str()),
            "missing {component:?}"
        );
    }
    assert!(events.iter().all(|event| {
        event.path_hash.is_some()
            || matches!(
                event.component,
                "trajectory.index_enqueue" | "trajectory.index_cache_hit"
            )
    }));
}

#[serial_test::serial(trajectory_perf)]
#[tokio::test]
async fn coordinator_cache_hit_records_a_counter_without_an_index_read() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("trajectories");
    let path = write_trajectory(&dir, "chat-1", "One", "agent").await;
    let coordinator = TrajectoryIndexCoordinator::new();
    coordinator
        .upsert(&dir, entry_for_path(&dir, &path).await)
        .await
        .unwrap();
    coordinator.flush_all().await.unwrap();
    coordinator.list_entries(&dir, None).await.unwrap();

    let _lock = perf_diagnostics::PERF_RECORDER_TEST_LOCK.lock().unwrap();
    let (_guard, sink) = install_perf_recorder();
    assert_eq!(coordinator.list_entries(&dir, None).await.unwrap().len(), 1);

    let events = sink.events();
    assert_eq!(events.len(), 1);
    assert_eq!(
        events[0].component,
        PerfComponent::TrajectoryIndexCacheHit.as_str()
    );
    assert_eq!(events[0].item_count, Some(1));
}

#[serial_test::serial(trajectory_perf)]
#[tokio::test]
async fn coordinator_advisory_lock_wait_is_recorded_for_a_contended_flush() {
    let _lock = perf_diagnostics::PERF_RECORDER_TEST_LOCK.lock().unwrap();
    let (_guard, sink) = install_perf_recorder();
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("trajectories");
    let path = write_trajectory(&dir, "chat-1", "One", "agent").await;
    let blocker = file_lock::open_lock(&dir.join(".index.json.lock")).unwrap();
    let mut blocker = blocker;
    let blocker_guard = file_lock::try_lock(&mut blocker).unwrap();
    let coordinator = TrajectoryIndexCoordinator::with_timing(
        Duration::from_millis(25),
        Duration::from_millis(5),
    );
    coordinator
        .upsert(&dir, entry_for_path(&dir, &path).await)
        .await
        .unwrap();

    assert!(coordinator.flush_all().await.is_err());

    let events = sink.events();
    assert!(events.iter().any(|event| {
        event.component == PerfComponent::TrajectoryIndexLockWait.as_str()
            && event.outcome == PerfOutcome::Failure.as_str()
            && event.path_hash.is_some()
    }));
    drop(blocker_guard);
}

#[serial_test::serial(trajectory_perf)]
#[tokio::test]
async fn coordinator_diagnostics_are_inactive_without_a_recorder() {
    let _lock = perf_diagnostics::PERF_RECORDER_TEST_LOCK.lock().unwrap();
    let telemetry = perf_diagnostics::process_telemetry();
    let was_enabled = telemetry.enabled();
    telemetry.set_enabled(false);
    telemetry.reset();

    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("trajectories");
    let path = write_trajectory(&dir, "chat-1", "One", "agent").await;
    let coordinator = TrajectoryIndexCoordinator::new();
    coordinator
        .upsert(&dir, entry_for_path(&dir, &path).await)
        .await
        .unwrap();
    coordinator.list_entries(&dir, None).await.unwrap();
    coordinator.flush_all().await.unwrap();

    assert!(telemetry
        .snapshot()
        .components
        .iter()
        .all(|component| component.sample_count == 0));
    telemetry.set_enabled(was_enabled);
}

#[tokio::test]
async fn corrupt_index_rebuilds_from_files() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("trajectories");
    write_trajectory(&dir, "chat-1", "One", "agent").await;
    fs::write(trajectory_index_path(&dir), "not json")
        .await
        .unwrap();

    let entries = list_trajectory_entries_from_index_or_rebuild(&dir, None)
        .await
        .unwrap();

    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].id, "chat-1");
}

#[tokio::test]
async fn valid_index_rebuilds_when_new_trajectory_appears_on_disk() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("trajectories");
    write_trajectory(&dir, "chat-1", "One", "agent").await;
    assert_eq!(
        list_trajectory_entries_from_index_or_rebuild(&dir, None)
            .await
            .unwrap()
            .len(),
        1
    );

    write_trajectory(&dir, "chat-2", "Two", "agent").await;
    let entries = list_trajectory_entries_from_index_or_rebuild(&dir, None)
        .await
        .unwrap();

    assert_eq!(entries.len(), 2);
    let index = read_trajectory_index(&dir).await.unwrap().unwrap();
    assert_eq!(index.entries.len(), 2);
}

async fn write_raw_trajectory(dir: &Path, file_stem: &str, id: &str) -> PathBuf {
    fs::create_dir_all(dir).await.unwrap();
    let path = dir.join(format!("{file_stem}.json"));
    let value = json!({
        "id": id,
        "title": "Backup",
        "created_at": "2026-01-01T00:00:00Z",
        "updated_at": "2026-01-01T00:00:01Z",
        "model": "test-model",
        "mode": "agent",
        "tool_use": "agent",
        "messages": [{"role":"user","content":"hi"}]
    });
    fs::write(&path, serde_json::to_string_pretty(&value).unwrap())
        .await
        .unwrap();
    path
}

#[tokio::test]
async fn non_indexable_backup_files_do_not_force_rebuild() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("trajectories");
    write_trajectory(&dir, "chat-1", "One", "agent").await;
    write_raw_trajectory(&dir, "chat-1_initial", "chat-1").await;

    let first = list_trajectory_entries_from_index_or_rebuild(&dir, None)
        .await
        .unwrap();
    assert_eq!(first.len(), 1, "only the real trajectory is indexed");
    assert_eq!(first[0].id, "chat-1");

    let index_after_first = read_trajectory_index(&dir).await.unwrap().unwrap();
    let updated_at_marker = index_after_first.updated_at.clone();
    assert_eq!(index_after_first.entries.len(), 1);

    for _ in 0..3 {
        let entries = list_trajectory_entries_from_index_or_rebuild(&dir, None)
            .await
            .unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].id, "chat-1");
        let index = read_trajectory_index(&dir).await.unwrap().unwrap();
        assert_eq!(
            index.updated_at, updated_at_marker,
            "index must not be rewritten when nothing indexable changed"
        );
    }
}

#[tokio::test]
async fn internal_trace_skip_is_cached_and_changed_visible_file_is_indexed() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("trajectories");
    fs::create_dir_all(&dir).await.unwrap();
    let path = dir.join("trace.json");
    let internal = format!(
        concat!(
            "{{\n",
            "  \"id\": \"trace\",\n",
            "  \"title\": \"Trace\",\n",
            "  \"created_at\": \"2026-01-01T00:00:00Z\",\n",
            "  \"updated_at\": \"2026-01-01T00:00:01Z\",\n",
            "  \"model\": \"test-model\",\n",
            "  \"mode\": \"agent\",\n",
            "  \"tool_use\": \"agent\",\n",
            "  \"messages\": [{{\"role\":\"user\",\"content\":\"{}\"}}],\n",
            "  \"link_type\": \"internal:tool-trace\"\n",
            "}}"
        ),
        "x".repeat(128 * 1024)
    );
    fs::write(&path, internal).await.unwrap();

    assert!(list_trajectory_entries_from_index_or_rebuild(&dir, None)
        .await
        .unwrap()
        .is_empty());
    let first_index = read_trajectory_index(&dir).await.unwrap().unwrap();
    assert_eq!(first_index.skipped_files.len(), 1);
    assert_eq!(first_index.skipped_files[0].file_name, "trace.json");
    let marker = first_index.updated_at;

    assert!(list_trajectory_entries_from_index_or_rebuild(&dir, None)
        .await
        .unwrap()
        .is_empty());
    assert_eq!(
        read_trajectory_index(&dir)
            .await
            .unwrap()
            .unwrap()
            .updated_at,
        marker
    );

    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    write_trajectory(&dir, "trace", "Visible", "agent").await;
    let entries = list_trajectory_entries_from_index_or_rebuild(&dir, None)
        .await
        .unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].title, "Visible");
    let final_index = read_trajectory_index(&dir).await.unwrap().unwrap();
    assert!(final_index.skipped_files.is_empty());
    assert_eq!(final_index.entries.len(), 1);
}

#[tokio::test]
async fn unchanged_directory_does_not_rewrite_index() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("trajectories");
    write_trajectory(&dir, "chat-1", "One", "agent").await;
    write_trajectory(&dir, "chat-2", "Two", "agent").await;

    assert_eq!(
        list_trajectory_entries_from_index_or_rebuild(&dir, None)
            .await
            .unwrap()
            .len(),
        2
    );
    let marker = read_trajectory_index(&dir)
        .await
        .unwrap()
        .unwrap()
        .updated_at;

    let entries = list_trajectory_entries_from_index_or_rebuild(&dir, None)
        .await
        .unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(
        read_trajectory_index(&dir)
            .await
            .unwrap()
            .unwrap()
            .updated_at,
        marker
    );
}

#[tokio::test]
async fn modified_trajectory_is_reindexed() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("trajectories");
    write_trajectory(&dir, "chat-1", "One", "agent").await;
    let entries = list_trajectory_entries_from_index_or_rebuild(&dir, None)
        .await
        .unwrap();
    assert_eq!(entries[0].title, "One");

    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    let path = dir.join("chat-1.json");
    let value = json!({
        "id": "chat-1",
        "title": "One Renamed",
        "created_at": "2026-01-01T00:00:00Z",
        "updated_at": "2026-01-02T00:00:01Z",
        "model": "test-model",
        "mode": "agent",
        "tool_use": "agent",
        "messages": [{"role":"user","content":"hello again"}]
    });
    fs::write(&path, serde_json::to_string_pretty(&value).unwrap())
        .await
        .unwrap();

    let entries = list_trajectory_entries_from_index_or_rebuild(&dir, None)
        .await
        .unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].title, "One Renamed");
    let index = read_trajectory_index(&dir).await.unwrap().unwrap();
    assert_eq!(index.entries.len(), 1);
    assert_eq!(index.entries[0].title, "One Renamed");
}

#[tokio::test]
async fn upsert_after_missing_index_preserves_existing_siblings() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("trajectories");
    let path_1 = write_trajectory(&dir, "chat-1", "One", "agent").await;
    write_trajectory(&dir, "chat-2", "Two", "agent").await;
    let value: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&path_1).await.unwrap()).unwrap();

    upsert_trajectory_index_entry_from_value(&dir, &path_1, &value, None)
        .await
        .unwrap();
    let entries = list_trajectory_entries_from_index_or_rebuild(&dir, None)
        .await
        .unwrap();

    assert_eq!(entries.len(), 2);
    assert!(entries.iter().any(|entry| entry.id == "chat-2"));
}

#[tokio::test]
async fn stale_entry_removed_when_file_deleted() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("trajectories");
    let path = write_trajectory(&dir, "chat-1", "One", "agent").await;
    let value: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&path).await.unwrap()).unwrap();
    upsert_trajectory_index_entry_from_value(&dir, &path, &value, None)
        .await
        .unwrap();
    fs::remove_file(path).await.unwrap();

    let entries = list_trajectory_entries_from_index_or_rebuild(&dir, None)
        .await
        .unwrap();

    assert!(entries.is_empty());
    let index = read_trajectory_index(&dir).await.unwrap().unwrap();
    assert!(index.entries.is_empty());
}

#[tokio::test]
async fn extraction_rejects_mismatched_id_vs_filename() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("trajectories");
    fs::create_dir_all(&dir).await.unwrap();
    let path = dir.join("chat-1.json");
    let value = json!({
        "id": "chat-2",
        "title": "Bad",
        "created_at": "2026-01-01T00:00:00Z",
        "updated_at": "2026-01-01T00:00:01Z",
        "model": "test-model",
        "mode": "agent",
        "tool_use": "agent",
        "messages": []
    });
    fs::write(&path, serde_json::to_string(&value).unwrap())
        .await
        .unwrap();

    assert!(entry_from_trajectory_value(&dir, &path, &value, None).is_err());
}

#[tokio::test]
async fn displayable_filter_marks_task_buddy_and_child_links_non_displayable() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("trajectories");
    for (id, mode, extra) in [
        ("task-agent", "task_agent", json!({})),
        ("task-planner", "task_planner", json!({})),
        ("buddy", "agent", json!({"buddy_meta": {"x": true}})),
        (
            "child",
            "agent",
            json!({"parent_id": "root", "link_type": "subagent"}),
        ),
    ] {
        fs::create_dir_all(&dir).await.unwrap();
        let path = dir.join(format!("{id}.json"));
        let mut value = json!({
            "id": id,
            "title": id,
            "created_at": "2026-01-01T00:00:00Z",
            "updated_at": "2026-01-01T00:00:01Z",
            "model": "test-model",
            "mode": mode,
            "tool_use": "agent",
            "messages": []
        });
        for (k, v) in extra.as_object().unwrap() {
            value[k] = v.clone();
        }
        fs::write(&path, serde_json::to_string(&value).unwrap())
            .await
            .unwrap();
        let entry = entry_from_trajectory_value(&dir, &path, &value, None).unwrap();
        assert!(!entry.displayable_chat, "{id} should be hidden");
    }
}

#[tokio::test]
async fn task_source_roundtrips_context_fields() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("task/trajectories/planner");
    fs::create_dir_all(&dir).await.unwrap();
    let path = dir.join("planner-chat.json");
    let value = json!({
        "id": "planner-chat",
        "title": "Planner",
        "created_at": "2026-01-01T00:00:00Z",
        "updated_at": "2026-01-01T00:00:01Z",
        "model": "test-model",
        "mode": "task_planner",
        "tool_use": "agent",
        "messages": [],
        "task_meta": {
            "task_id": "task-1",
            "role": "planner",
            "agent_id": null,
            "card_id": "card-1",
            "planner_chat_id": "planner-chat"
        }
    });
    fs::write(&path, serde_json::to_string(&value).unwrap())
        .await
        .unwrap();

    let entry = entry_from_trajectory_value(&dir, &path, &value, None).unwrap();
    let meta = meta_from_entry(&dir, &entry);

    assert_eq!(meta.task_id.as_deref(), Some("task-1"));
    assert_eq!(meta.task_role.as_deref(), Some("planner"));
    assert_eq!(meta.card_id.as_deref(), Some("card-1"));
    assert!(matches!(entry.source, TrajectoryIndexSource::Task { .. }));
}

pub(super) async fn entry_for_path(dir: &Path, path: &Path) -> TrajectoryIndexEntry {
    let value = serde_json::from_str(&fs::read_to_string(path).await.unwrap()).unwrap();
    entry_from_trajectory_value(dir, path, &value, None).unwrap()
}

#[tokio::test]
async fn probe_external_new_file_before_upsert_is_still_listed() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("trajectories");
    let path = write_trajectory(&dir, "chat-1", "One", "agent").await;
    let coordinator = TrajectoryIndexCoordinator::new();
    coordinator
        .upsert(&dir, entry_for_path(&dir, &path).await)
        .await
        .unwrap();
    coordinator.flush_all().await.unwrap();
    assert_eq!(coordinator.list_entries(&dir, None).await.unwrap().len(), 1);

    // an external writer drops a new trajectory into the same directory
    tokio::time::sleep(Duration::from_millis(20)).await;
    write_trajectory(&dir, "chat-2", "Two", "agent").await;

    // our own process then saves chat-1 again
    let rewritten = write_trajectory(&dir, "chat-1", "Renamed", "agent").await;
    coordinator
        .upsert(&dir, entry_for_path(&dir, &rewritten).await)
        .await
        .unwrap();

    let ids: Vec<String> = coordinator
        .list_entries(&dir, None)
        .await
        .unwrap()
        .into_iter()
        .map(|entry| entry.id)
        .collect();
    assert!(
        ids.contains(&"chat-2".to_string()),
        "listing lost the externally created trajectory: {ids:?}"
    );
}

#[tokio::test]
async fn coordinator_coalesces_last_upsert_and_shutdown_flush() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("trajectories");
    let path = write_trajectory(&dir, "chat-1", "One", "agent").await;
    let coordinator = TrajectoryIndexCoordinator::new();
    coordinator
        .upsert(&dir, entry_for_path(&dir, &path).await)
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(20)).await;
    write_trajectory(&dir, "chat-1", "Renamed", "agent").await;
    coordinator
        .upsert(&dir, entry_for_path(&dir, &path).await)
        .await
        .unwrap();
    coordinator.flush_all().await.unwrap();

    let index = read_trajectory_index(&dir).await.unwrap().unwrap();
    assert_eq!(index.entries.len(), 1);
    assert_eq!(index.entries[0].title, "Renamed");
}

#[tokio::test]
async fn coordinator_enqueues_before_loading_or_writing_the_index() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("trajectories");
    let path = write_trajectory(&dir, "chat-1", "One", "agent").await;
    let coordinator = TrajectoryIndexCoordinator::new();

    coordinator
        .upsert(&dir, entry_for_path(&dir, &path).await)
        .await
        .unwrap();

    assert!(!trajectory_index_path(&dir).exists());
    assert_eq!(coordinator.directories.lock().await.len(), 1);
    coordinator.flush_all().await.unwrap();
    assert_eq!(
        read_trajectory_index(&dir)
            .await
            .unwrap()
            .unwrap()
            .entries
            .len(),
        1
    );
}

#[tokio::test]
async fn coordinator_recovers_an_unflushed_committed_trajectory_on_restart() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("trajectories");
    let path = write_trajectory(&dir, "chat-1", "One", "agent").await;
    let coordinator = TrajectoryIndexCoordinator::new();
    coordinator
        .upsert(&dir, entry_for_path(&dir, &path).await)
        .await
        .unwrap();
    drop(coordinator);

    let restarted = TrajectoryIndexCoordinator::new();
    let entries = restarted.list_entries(&dir, None).await.unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].id, "chat-1");
    restarted.flush_all().await.unwrap();
    assert_eq!(
        read_trajectory_index(&dir)
            .await
            .unwrap()
            .unwrap()
            .entries
            .len(),
        1
    );
}

#[test]
fn coordinator_rollout_switch_defaults_on_and_retains_legacy_fallback() {
    assert!(TrajectoryRuntimeSettings::default().trajectory_index_coordinator_enabled);
    assert!(trajectory_index_coordinator_rollout_enabled_for(Some("1")));
    for disabled in [Some("0"), Some("false"), Some("off")] {
        assert!(!trajectory_index_coordinator_rollout_enabled_for(disabled));
    }
    for enabled in [Some("1"), Some("true"), Some("YES"), Some("on")] {
        assert!(trajectory_index_coordinator_rollout_enabled_for(enabled));
    }
}

#[tokio::test]
async fn coordinator_orders_remove_then_upsert_and_upsert_then_remove() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("trajectories");
    let path = write_trajectory(&dir, "chat-1", "One", "agent").await;
    let entry = entry_for_path(&dir, &path).await;
    let coordinator = TrajectoryIndexCoordinator::new();
    let ids = HashSet::from(["chat-1".to_string()]);

    coordinator.remove(&dir, ids.clone()).await.unwrap();
    coordinator.upsert(&dir, entry.clone()).await.unwrap();
    coordinator.flush_all().await.unwrap();
    assert_eq!(
        read_trajectory_index(&dir)
            .await
            .unwrap()
            .unwrap()
            .entries
            .len(),
        1
    );

    coordinator.upsert(&dir, entry).await.unwrap();
    coordinator.remove(&dir, ids).await.unwrap();
    coordinator.flush_all().await.unwrap();
    assert!(read_trajectory_index(&dir)
        .await
        .unwrap()
        .unwrap()
        .entries
        .is_empty());
}

#[tokio::test]
async fn coordinator_failed_flush_retains_pending_mutations_for_retry() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("trajectories");
    let path = write_trajectory(&dir, "chat-1", "One", "agent").await;
    let coordinator = TrajectoryIndexCoordinator::new();
    coordinator
        .upsert(&dir, entry_for_path(&dir, &path).await)
        .await
        .unwrap();

    let blocker = file_lock::open_lock(&dir.join(".index.json.lock")).unwrap();
    let mut blocker = blocker;
    let _guard = file_lock::try_lock(&mut blocker).unwrap();
    let fast = TrajectoryIndexCoordinator::with_timing(
        Duration::from_millis(25),
        Duration::from_millis(5),
    );
    fast.upsert(&dir, entry_for_path(&dir, &path).await)
        .await
        .unwrap();
    assert!(fast.flush_all().await.is_err());
    drop(_guard);
    fast.flush_all().await.unwrap();

    assert_eq!(
        read_trajectory_index(&dir)
            .await
            .unwrap()
            .unwrap()
            .entries
            .len(),
        1
    );
}

#[tokio::test]
async fn coordinator_reconciles_external_files_and_reuses_clean_snapshot() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("trajectories");
    let path_1 = write_trajectory(&dir, "chat-1", "One", "agent").await;
    let coordinator = TrajectoryIndexCoordinator::new();
    coordinator
        .upsert(&dir, entry_for_path(&dir, &path_1).await)
        .await
        .unwrap();
    coordinator.flush_all().await.unwrap();
    let first = coordinator.snapshot(&dir, None).await.unwrap();
    let second = coordinator.snapshot(&dir, None).await.unwrap();
    assert_eq!(first.updated_at, second.updated_at);

    write_trajectory(&dir, "chat-2", "Two", "agent").await;
    let reconciled = coordinator.reconcile(&dir, None).await.unwrap();
    assert_eq!(reconciled.entries.len(), 2);
    coordinator.flush_all().await.unwrap();
    assert_eq!(
        read_trajectory_index(&dir)
            .await
            .unwrap()
            .unwrap()
            .entries
            .len(),
        2
    );
}

#[tokio::test]
async fn coordinator_snapshot_does_not_reparse_clean_indexed_trajectories() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("trajectories");
    let path = write_trajectory(&dir, "chat-1", "One", "agent").await;
    let coordinator = TrajectoryIndexCoordinator::new();
    coordinator
        .upsert(&dir, entry_for_path(&dir, &path).await)
        .await
        .unwrap();
    coordinator.flush_all().await.unwrap();
    fs::write(&path, "invalid JSON").await.unwrap();

    let snapshot = coordinator.snapshot(&dir, None).await.unwrap();
    assert_eq!(snapshot.entries.len(), 1);
    assert_eq!(snapshot.entries[0].id, "chat-1");
}

#[tokio::test]
async fn legacy_cache_lru_evicts_oldest_directory() {
    let temp = tempfile::tempdir().unwrap();
    legacy_trajectory_index_cache().lock().await.clear();

    for index in 0..MAX_CACHED_TRAJECTORY_DIRECTORIES {
        let dir = temp.path().join(format!("dir-{index}"));
        cache_legacy_trajectory_index(&dir, TrajectoryIndex::default()).await;
    }

    let oldest = temp.path().join("dir-0");
    let newest = temp
        .path()
        .join(format!("dir-{}", MAX_CACHED_TRAJECTORY_DIRECTORIES - 1));
    let extra = temp.path().join("dir-extra");
    cache_legacy_trajectory_index(&extra, TrajectoryIndex::default()).await;

    let cache = legacy_trajectory_index_cache().lock().await;
    assert!(!cache.contains_key(&oldest));
    assert!(cache.contains_key(&newest));
    assert!(cache.contains_key(&extra));
}

#[tokio::test]
async fn coordinator_directory_state_lru_evicts_oldest_clean_directory() {
    let coordinator = TrajectoryIndexCoordinator::new();
    let temp = tempfile::tempdir().unwrap();

    for index in 0..MAX_CACHED_TRAJECTORY_DIRECTORIES {
        let dir = temp.path().join(format!("dir-{index}"));
        coordinator.directory_state(&dir).await.unwrap();
    }

    let extra = temp.path().join("dir-extra");
    coordinator.directory_state(&extra).await.unwrap();

    let directories = coordinator.directories.lock().await;
    assert!(!directories.contains_key(&temp.path().join("dir-0")));
    assert!(directories.contains_key(
        &temp
            .path()
            .join(format!("dir-{}", MAX_CACHED_TRAJECTORY_DIRECTORIES - 1))
    ));
    assert!(directories.contains_key(&extra));
}

#[tokio::test]
async fn write_trajectory_index_atomic_owned_inner_cleans_temp_file_on_failure() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("trajectories");
    fs::create_dir_all(&dir).await.unwrap();
    let error = write_trajectory_index_atomic_owned_inner_with_rename_failure(
        &dir,
        TrajectoryIndex::default(),
        true,
    )
    .await
    .unwrap_err();
    assert!(error.contains("Failed to rename"));

    let mut entries = tokio::fs::read_dir(&dir).await.unwrap();
    while let Some(entry) = entries.next_entry().await.unwrap() {
        assert!(!entry
            .file_name()
            .to_string_lossy()
            .contains(".index.json.tmp-"));
    }
}

#[tokio::test]
async fn coordinator_listing_avoids_reads_and_scans_for_unchanged_directory() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("trajectories");
    let path = write_trajectory(&dir, "chat-1", "One", "agent").await;
    let listing_counter_scope = TrajectoryIndexListingCounterScope::local();
    let coordinator =
        TrajectoryIndexCoordinator::with_listing_counter_scope(listing_counter_scope.clone());
    coordinator
        .upsert(&dir, entry_for_path(&dir, &path).await)
        .await
        .unwrap();
    coordinator.flush_all().await.unwrap();
    coordinator.list_entries(&dir, None).await.unwrap();

    listing_counter_scope.reset();
    let entries = coordinator.list_entries(&dir, None).await.unwrap();
    let counters = listing_counter_scope.snapshot();

    assert_eq!(entries.len(), 1);
    assert_eq!(counters.index_reads, 0);
    assert_eq!(counters.directory_scans, 0);
    assert_eq!(counters.cache_hits, 1);
}

#[tokio::test]
async fn legacy_listing_avoids_reads_and_scans_for_unchanged_directory() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("trajectories");
    write_trajectory(&dir, "chat-1", "One", "agent").await;
    let listing_counter_scope = TrajectoryIndexListingCounterScope::local();
    list_trajectory_entries_from_index_or_rebuild_with_counter_scope(
        &dir,
        None,
        &listing_counter_scope,
    )
    .await
    .unwrap();

    listing_counter_scope.reset();
    let entries = list_trajectory_entries_from_index_or_rebuild_with_counter_scope(
        &dir,
        None,
        &listing_counter_scope,
    )
    .await
    .unwrap();
    let counters = listing_counter_scope.snapshot();

    assert_eq!(entries.len(), 1);
    assert_eq!(counters.index_reads, 0);
    assert_eq!(counters.directory_scans, 0);
    assert_eq!(counters.cache_hits, 1);
}

#[tokio::test]
async fn legacy_cached_listing_reflects_external_creation_and_deletion() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("trajectories");
    let path = write_trajectory(&dir, "chat-1", "One", "agent").await;

    assert_eq!(
        list_trajectory_entries_from_index_or_rebuild(&dir, None)
            .await
            .unwrap()
            .len(),
        1
    );

    write_trajectory(&dir, "chat-2", "Two", "agent").await;
    assert_eq!(
        list_trajectory_entries_from_index_or_rebuild(&dir, None)
            .await
            .unwrap()
            .len(),
        2
    );

    fs::remove_file(path).await.unwrap();
    let entries = list_trajectory_entries_from_index_or_rebuild(&dir, None)
        .await
        .unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].id, "chat-2");
}

#[tokio::test]
async fn cached_listings_reflect_created_modified_and_deleted_trajectories() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("trajectories");
    let path = write_trajectory(&dir, "chat-1", "One", "agent").await;
    let coordinator = TrajectoryIndexCoordinator::new();
    coordinator
        .upsert(&dir, entry_for_path(&dir, &path).await)
        .await
        .unwrap();
    coordinator.flush_all().await.unwrap();
    assert_eq!(coordinator.list_entries(&dir, None).await.unwrap().len(), 1);

    write_trajectory(&dir, "chat-2", "Two", "agent").await;
    assert_eq!(coordinator.list_entries(&dir, None).await.unwrap().len(), 2);

    tokio::time::sleep(Duration::from_millis(20)).await;
    write_trajectory(&dir, "chat-1", "Renamed", "agent").await;
    let entries = coordinator.list_entries(&dir, None).await.unwrap();
    assert_eq!(
        entries
            .iter()
            .find(|entry| entry.id == "chat-1")
            .unwrap()
            .title,
        "Renamed"
    );

    fs::remove_file(dir.join("chat-2.json")).await.unwrap();
    let entries = coordinator.list_entries(&dir, None).await.unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].id, "chat-1");
}

#[tokio::test]
async fn coordinator_scans_only_changed_directories_across_repeated_polls() {
    const DIRECTORY_COUNT: usize = 24;
    const POLL_COUNT: usize = 12;

    let temp = tempfile::tempdir().unwrap();
    let listing_counter_scope = TrajectoryIndexListingCounterScope::local();
    let coordinator =
        TrajectoryIndexCoordinator::with_listing_counter_scope(listing_counter_scope.clone());
    let mut dirs = Vec::new();
    for number in 0..DIRECTORY_COUNT {
        let dir = temp.path().join(format!("trajectories-{number}"));
        write_trajectory(&dir, "chat-1", "One", "agent").await;
        coordinator.list_entries(&dir, None).await.unwrap();
        dirs.push(dir);
    }

    listing_counter_scope.reset();
    for _ in 0..POLL_COUNT {
        for dir in &dirs {
            assert_eq!(coordinator.list_entries(dir, None).await.unwrap().len(), 1);
        }
    }
    let unchanged = listing_counter_scope.snapshot();
    assert_eq!(unchanged.directory_scans, 0);
    assert_eq!(
        unchanged.cache_hits,
        u64::try_from(DIRECTORY_COUNT * POLL_COUNT).unwrap()
    );

    write_trajectory(&dirs[0], "chat-2", "Two", "agent").await;
    assert_eq!(
        coordinator
            .list_entries(&dirs[0], None)
            .await
            .unwrap()
            .len(),
        2
    );
    for dir in &dirs[1..] {
        assert_eq!(coordinator.list_entries(dir, None).await.unwrap().len(), 1);
    }
    let changed = listing_counter_scope.snapshot();
    assert_eq!(changed.directory_scans, 1);
    assert_eq!(
        changed.cache_hits,
        u64::try_from(DIRECTORY_COUNT * POLL_COUNT + DIRECTORY_COUNT - 1).unwrap()
    );
}

#[tokio::test]
async fn upsert_of_a_rewritten_trajectory_keeps_the_listing_fresh() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("trajectories");
    let path = write_trajectory(&dir, "chat-1", "One", "agent").await;
    let listing_counter_scope = TrajectoryIndexListingCounterScope::local();
    let coordinator =
        TrajectoryIndexCoordinator::with_listing_counter_scope(listing_counter_scope.clone());
    coordinator
        .upsert(&dir, entry_for_path(&dir, &path).await)
        .await
        .unwrap();
    coordinator.flush_all().await.unwrap();
    coordinator.list_entries(&dir, None).await.unwrap();

    tokio::time::sleep(Duration::from_millis(20)).await;
    let rewritten = write_trajectory(&dir, "chat-1", "Renamed", "agent").await;
    coordinator
        .upsert(&dir, entry_for_path(&dir, &rewritten).await)
        .await
        .unwrap();
    let subchat = write_trajectory(&dir.join("chat-1"), "subchat-1", "Sub", "agent").await;
    coordinator
        .upsert(&dir, entry_for_path(&dir, &subchat).await)
        .await
        .unwrap();
    listing_counter_scope.reset();

    let entries = coordinator.list_entries(&dir, None).await.unwrap();

    assert_eq!(
        entries
            .iter()
            .find(|entry| entry.id == "chat-1")
            .unwrap()
            .title,
        "Renamed"
    );
    assert!(entries.iter().any(|entry| entry.id == "subchat-1"));
    let counters = listing_counter_scope.snapshot();
    assert_eq!(counters.directory_scans, 0);
    assert_eq!(counters.cache_hits, 1);
}

#[tokio::test]
async fn external_subchat_created_before_a_nested_upsert_is_still_listed() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("trajectories");
    let path = write_trajectory(&dir, "chat-1", "One", "agent").await;
    fs::create_dir_all(dir.join("chat-1")).await.unwrap();
    let scope = TrajectoryIndexListingCounterScope::local();
    let coordinator = TrajectoryIndexCoordinator::with_listing_counter_scope(scope.clone());
    coordinator
        .upsert(&dir, entry_for_path(&dir, &path).await)
        .await
        .unwrap();
    coordinator.flush_all().await.unwrap();
    coordinator.list_entries(&dir, None).await.unwrap();

    write_trajectory(&dir.join("chat-1"), "subchat-external", "External", "agent").await;
    let mine = write_trajectory(&dir.join("chat-1"), "subchat-mine", "Mine", "agent").await;
    coordinator
        .upsert(&dir, entry_for_path(&dir, &mine).await)
        .await
        .unwrap();
    scope.reset();
    let entries = coordinator.list_entries(&dir, None).await.unwrap();
    assert!(entries.iter().any(|entry| entry.id == "subchat-external"));
    assert!(entries.iter().any(|entry| entry.id == "subchat-mine"));
    assert_eq!(scope.snapshot().directory_scans, 1);
}

#[tokio::test]
async fn ordinary_upsert_in_a_known_child_keeps_the_listing_fresh() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("trajectories");
    write_trajectory(&dir, "chat-1", "One", "agent").await;
    write_trajectory(&dir.join("chat-1"), "subchat-first", "First", "agent").await;
    let scope = TrajectoryIndexListingCounterScope::local();
    let coordinator = TrajectoryIndexCoordinator::with_listing_counter_scope(scope.clone());
    coordinator.list_entries(&dir, None).await.unwrap();
    coordinator.flush_all().await.unwrap();
    let mine = write_trajectory(&dir.join("chat-1"), "subchat-mine", "Mine", "agent").await;
    coordinator
        .upsert(&dir, entry_for_path(&dir, &mine).await)
        .await
        .unwrap();
    scope.reset();
    assert_eq!(coordinator.list_entries(&dir, None).await.unwrap().len(), 3);
    assert_eq!(scope.snapshot().directory_scans, 0);
}

#[tokio::test]
async fn listings_within_the_generation_ttl_skip_filesystem_verification() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("trajectories");
    write_trajectory(&dir, "chat-1", "One", "agent").await;
    let listing_counter_scope = TrajectoryIndexListingCounterScope::local();
    let coordinator =
        TrajectoryIndexCoordinator::with_listing_counter_scope(listing_counter_scope.clone())
            .with_generation_ttl(Duration::from_secs(30));
    coordinator.list_entries(&dir, None).await.unwrap();

    listing_counter_scope.reset();
    for _ in 0..5 {
        assert_eq!(coordinator.list_entries(&dir, None).await.unwrap().len(), 1);
    }
    let counters = listing_counter_scope.snapshot();
    assert_eq!(counters.generation_checks, 0);
    assert_eq!(counters.directory_scans, 0);
    assert_eq!(counters.cache_hits, 5);
}

#[tokio::test]
async fn external_changes_are_seen_once_the_generation_ttl_expires() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("trajectories");
    write_trajectory(&dir, "chat-1", "One", "agent").await;
    let listing_counter_scope = TrajectoryIndexListingCounterScope::local();
    let coordinator =
        TrajectoryIndexCoordinator::with_listing_counter_scope(listing_counter_scope.clone())
            .with_generation_ttl(Duration::from_millis(50));
    coordinator.list_entries(&dir, None).await.unwrap();

    write_trajectory(&dir, "chat-2", "Two", "agent").await;
    tokio::time::sleep(Duration::from_millis(80)).await;
    listing_counter_scope.reset();

    assert_eq!(coordinator.list_entries(&dir, None).await.unwrap().len(), 2);
    let counters = listing_counter_scope.snapshot();
    assert_eq!(counters.generation_checks, 1);
    assert_eq!(counters.directory_scans, 1);
}

#[tokio::test]
async fn flush_without_changes_does_not_rewrite_the_index() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("trajectories");
    let path = write_trajectory(&dir, "chat-1", "One", "agent").await;
    let coordinator = TrajectoryIndexCoordinator::new();
    let entry = entry_for_path(&dir, &path).await;
    coordinator.upsert(&dir, entry.clone()).await.unwrap();
    coordinator.flush_all().await.unwrap();
    let index_path = trajectory_index_path(&dir);
    let before_bytes = fs::read(&index_path).await.unwrap();
    let before_modified = fs::metadata(&index_path).await.unwrap().modified().unwrap();

    tokio::time::sleep(Duration::from_millis(20)).await;
    coordinator.upsert(&dir, entry).await.unwrap();
    coordinator.flush_all().await.unwrap();

    assert_eq!(fs::read(&index_path).await.unwrap(), before_bytes);
    assert_eq!(
        fs::metadata(&index_path).await.unwrap().modified().unwrap(),
        before_modified
    );
    assert_eq!(coordinator.list_entries(&dir, None).await.unwrap().len(), 1);
}

#[tokio::test]
async fn tmp_sweeper_removes_only_old_index_tmp_files() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("trajectories");
    write_trajectory(&dir, "chat-1", "One", "agent").await;
    let old = dir.join(format!("{}old", trajectory_index_tmp_prefix()));
    let fresh = dir.join(format!("{}fresh", trajectory_index_tmp_prefix()));
    let unrelated = dir.join("notes.tmp-old");
    for path in [&old, &fresh, &unrelated] {
        fs::write(path, b"{}").await.unwrap();
    }
    let stale_time = SystemTime::now() - Duration::from_secs(20 * 60);
    for path in [&old, &unrelated] {
        std::fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(stale_time)
            .unwrap();
    }

    let removed = sweep_trajectory_index_tmp_files_in_roots(
        &[dir.clone()],
        TRAJECTORY_INDEX_TMP_SWEEP_MAX_AGE,
    )
    .await;

    assert_eq!(removed, 1);
    assert!(!old.exists());
    assert!(fresh.exists());
    assert!(unrelated.exists());
    assert!(dir.join("chat-1.json").exists());
}

#[tokio::test]
async fn listing_counters_attribute_callers() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("trajectories");
    write_trajectory(&dir, "chat-1", "One", "agent").await;
    assert!(TrajectoryIndexCoordinator::new()
        .listing_counter_scope
        .is_global());
    let listing_counter_scope = TrajectoryIndexListingCounterScope::local();
    let coordinator =
        TrajectoryIndexCoordinator::with_listing_counter_scope(listing_counter_scope.clone());

    listing_counter_scope.reset();
    list_trajectory_entries_with_rollout_for(
        &coordinator,
        &dir,
        None,
        TrajectoryIndexListingCaller::Sidebar,
    )
    .await
    .unwrap();
    list_trajectory_entries_with_rollout_for(
        &coordinator,
        &dir,
        None,
        TrajectoryIndexListingCaller::TaskAgentMonitor,
    )
    .await
    .unwrap();
    let counters = listing_counter_scope.snapshot();

    assert_eq!(
        counters
            .calls_by_caller
            .iter()
            .find(|(caller, _)| *caller == "sidebar")
            .unwrap()
            .1,
        1
    );
    assert_eq!(
        counters
            .calls_by_caller
            .iter()
            .find(|(caller, _)| *caller == "task_agent_monitor")
            .unwrap()
            .1,
        1
    );
}

#[tokio::test]
async fn coordinator_merges_two_process_owned_instances_without_lost_entries() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("trajectories");
    let path_1 = write_trajectory(&dir, "chat-1", "One", "agent").await;
    let path_2 = write_trajectory(&dir, "chat-2", "Two", "agent").await;
    let first = TrajectoryIndexCoordinator::new();
    let second = TrajectoryIndexCoordinator::new();
    first
        .upsert(&dir, entry_for_path(&dir, &path_1).await)
        .await
        .unwrap();
    second
        .upsert(&dir, entry_for_path(&dir, &path_2).await)
        .await
        .unwrap();

    let (first_result, second_result) = tokio::join!(first.flush_all(), second.flush_all());
    first_result.unwrap();
    second_result.unwrap();
    let index = read_trajectory_index(&dir).await.unwrap().unwrap();
    assert_eq!(index.entries.len(), 2);
    assert!(index.entries.iter().any(|entry| entry.id == "chat-1"));
    assert!(index.entries.iter().any(|entry| entry.id == "chat-2"));
}

#[tokio::test]
async fn coordinator_cross_process_child_writer() {
    let Ok(dir) = std::env::var("REFACT_TRAJECTORY_INDEX_CHILD_DIR") else {
        return;
    };
    let id = std::env::var("REFACT_TRAJECTORY_INDEX_CHILD_ID").unwrap();
    let dir = PathBuf::from(dir);
    let path = dir.join(format!("{id}.json"));
    let coordinator = TrajectoryIndexCoordinator::new();
    coordinator
        .upsert(&dir, entry_for_path(&dir, &path).await)
        .await
        .unwrap();
    fs::write(dir.join(format!(".ready-{id}")), "ready")
        .await
        .unwrap();
    let release = dir.join(".release");
    tokio::time::timeout(Duration::from_secs(5), async {
        while !release.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    coordinator.flush_all().await.unwrap();
}

#[tokio::test]
async fn coordinator_cross_process_concurrent_writers_lose_zero_durable_entries() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("trajectories");
    write_trajectory(&dir, "chat-1", "One", "agent").await;
    write_trajectory(&dir, "chat-2", "Two", "agent").await;
    let executable = std::env::current_exe().unwrap();
    let child_dir = dir.to_string_lossy().into_owned();
    let spawn_child = |id: &'static str| {
        let executable = executable.clone();
        let child_dir = child_dir.clone();
        tokio::task::spawn_blocking(move || {
            std::process::Command::new(executable)
                .args([
                    "--exact",
                    "trajectory_index::tests::coordinator_cross_process_child_writer",
                    "--nocapture",
                    "--test-threads=1",
                ])
                .env("REFACT_TRAJECTORY_INDEX_CHILD_DIR", child_dir)
                .env("REFACT_TRAJECTORY_INDEX_CHILD_ID", id)
                .status()
                .unwrap()
        })
    };
    let first = spawn_child("chat-1");
    let second = spawn_child("chat-2");
    tokio::time::timeout(Duration::from_secs(10), async {
        while !dir.join(".ready-chat-1").exists() || !dir.join(".ready-chat-2").exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    fs::write(dir.join(".release"), "release").await.unwrap();
    assert!(first.await.unwrap().success());
    assert!(second.await.unwrap().success());
    let index = read_trajectory_index(&dir).await.unwrap().unwrap();
    assert_eq!(index.entries.len(), 2);
}

#[tokio::test]
async fn coordinator_matches_legacy_upsert_result() {
    let temp = tempfile::tempdir().unwrap();
    let legacy_dir = temp.path().join("legacy");
    let coordinator_dir = temp.path().join("coordinator");
    let legacy_path = write_trajectory(&legacy_dir, "chat-1", "One", "agent").await;
    let coordinator_path = write_trajectory(&coordinator_dir, "chat-1", "One", "agent").await;
    upsert_trajectory_index_entry(&legacy_dir, entry_for_path(&legacy_dir, &legacy_path).await)
        .await
        .unwrap();
    let coordinator = TrajectoryIndexCoordinator::new();
    coordinator
        .upsert(
            &coordinator_dir,
            entry_for_path(&coordinator_dir, &coordinator_path).await,
        )
        .await
        .unwrap();
    coordinator.flush_all().await.unwrap();

    let legacy = read_trajectory_index(&legacy_dir).await.unwrap().unwrap();
    let coordinated = read_trajectory_index(&coordinator_dir)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(legacy.entries.len(), coordinated.entries.len());
    assert_eq!(legacy.entries[0].id, coordinated.entries[0].id);
    assert_eq!(
        legacy.entries[0].file_name,
        coordinated.entries[0].file_name
    );
}

#[tokio::test]
async fn coordinator_rebuilds_corrupt_index_and_retains_skipped_files() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("trajectories");
    write_trajectory(&dir, "chat-1", "One", "agent").await;
    fs::write(
        dir.join("internal.json"),
        json!({
            "id": "internal",
            "title": "Internal",
            "created_at": "2026-01-01T00:00:00Z",
            "updated_at": "2026-01-01T00:00:01Z",
            "model": "test-model",
            "mode": "agent",
            "tool_use": "agent",
            "messages": [],
            "link_type": "internal:test"
        })
        .to_string(),
    )
    .await
    .unwrap();
    fs::write(trajectory_index_path(&dir), "corrupt")
        .await
        .unwrap();
    let coordinator = TrajectoryIndexCoordinator::new();
    let snapshot = coordinator.snapshot(&dir, None).await.unwrap();
    assert_eq!(snapshot.entries.len(), 1);
    assert_eq!(snapshot.skipped_files.len(), 1);
    coordinator.flush_all().await.unwrap();
    let persisted = read_trajectory_index(&dir).await.unwrap().unwrap();
    assert_eq!(persisted.entries.len(), 1);
    assert_eq!(persisted.skipped_files.len(), 1);
}

#[tokio::test]
async fn coordinator_listing_repairs_a_corrupt_cached_index() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("trajectories");
    let path = write_trajectory(&dir, "chat-1", "One", "agent").await;
    let coordinator = TrajectoryIndexCoordinator::new();
    coordinator
        .upsert(&dir, entry_for_path(&dir, &path).await)
        .await
        .unwrap();
    coordinator.flush_all().await.unwrap();
    coordinator.list_entries(&dir, None).await.unwrap();

    fs::write(trajectory_index_path(&dir), "corrupt")
        .await
        .unwrap();

    let entries = coordinator.list_entries(&dir, None).await.unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].id, "chat-1");
    assert_eq!(
        read_trajectory_index(&dir)
            .await
            .unwrap()
            .unwrap()
            .entries
            .len(),
        1
    );
}

#[tokio::test]
async fn coordinator_flush_replaces_a_parseable_schema_v1_index() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("trajectories");
    let path = write_trajectory(&dir, "chat-1", "One", "agent").await;
    let coordinator = TrajectoryIndexCoordinator::new();
    coordinator
        .upsert(&dir, entry_for_path(&dir, &path).await)
        .await
        .unwrap();
    coordinator.flush_all().await.unwrap();

    let raw = fs::read_to_string(trajectory_index_path(&dir))
        .await
        .unwrap();
    let parsed: TrajectoryIndex = serde_json::from_str(&raw).unwrap();
    assert_eq!(parsed.schema_version, TRAJECTORY_INDEX_SCHEMA_VERSION);
    assert!(!dir
        .read_dir()
        .expect("directory exists")
        .any(|entry| entry.is_ok_and(|entry| entry
            .file_name()
            .to_string_lossy()
            .contains(".index.json.tmp"))));
}
