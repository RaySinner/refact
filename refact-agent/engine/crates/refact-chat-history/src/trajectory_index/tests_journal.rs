use super::*;
use super::tests::{entry_for_path, write_trajectory};

async fn journal_len(dir: &Path) -> u64 {
    fs::metadata(trajectory_index_journal_path(dir))
        .await
        .map(|metadata| metadata.len())
        .unwrap_or(0)
}

#[test]
fn journal_replay_does_not_apply_regressing_or_duplicate_sequences() {
    let records: Vec<_> = [5, 3, 5, 6]
        .into_iter()
        .map(|seq| TrajectoryIndexJournalPayload {
            seq,
            writer_id: "test".to_string(),
            ts_ms: 0,
            updated_at: seq.to_string(),
            upserts: Vec::new(),
            removes: Vec::new(),
            skipped: None,
        })
        .collect();
    let mut index = TrajectoryIndex::default();
    assert_eq!(replay_journal_records(&mut index, &records[..3], 0), 5);
    assert_eq!(index.updated_at, "5");
    assert_eq!(replay_journal_records(&mut index, &records, 5), 6);
    assert_eq!(index.updated_at, "6");
    assert_eq!(index.applied_seq, 6);
}

#[tokio::test]
async fn interleaved_coordinator_flushes_have_strictly_increasing_journal_sequences() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("trajectories");
    let path = write_trajectory(&dir, "chat-0", "Initial", "agent").await;
    let first = TrajectoryIndexCoordinator::new();
    let second = TrajectoryIndexCoordinator::new();
    first
        .upsert(&dir, entry_for_path(&dir, &path).await)
        .await
        .unwrap();
    first.flush_all().await.unwrap();
    second.snapshot(&dir, None).await.unwrap();
    for n in 1..=6 {
        let coordinator = if n % 2 == 0 { &first } else { &second };
        let path = write_trajectory(&dir, &format!("chat-{n}"), "New", "agent").await;
        coordinator
            .upsert(&dir, entry_for_path(&dir, &path).await)
            .await
            .unwrap();
        coordinator.flush_all().await.unwrap();
    }
    let bytes = fs::read(trajectory_index_journal_path(&dir)).await.unwrap();
    let scan = scan_trajectory_index_journal(&bytes).unwrap();
    let seqs: Vec<_> = scan.records.iter().map(|record| record.seq).collect();
    assert_eq!(seqs.len(), 6);
    assert!(seqs.windows(2).all(|pair| pair[0] < pair[1]), "{seqs:?}");
    assert_eq!(
        read_trajectory_index(&dir)
            .await
            .unwrap()
            .unwrap()
            .entries
            .len(),
        7
    );
}

#[tokio::test]
async fn legacy_checkpoint_preserves_journal_commits_after_its_snapshot() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("trajectories");
    let path = write_trajectory(&dir, "chat-1", "One", "agent").await;
    let coordinator = TrajectoryIndexCoordinator::new();
    coordinator
        .upsert(&dir, entry_for_path(&dir, &path).await)
        .await
        .unwrap();
    coordinator.flush_all().await.unwrap();
    let stale = read_trajectory_index(&dir).await.unwrap().unwrap();
    let path = write_trajectory(&dir, "chat-2", "Two", "agent").await;
    coordinator
        .upsert(&dir, entry_for_path(&dir, &path).await)
        .await
        .unwrap();
    coordinator.flush_all().await.unwrap();
    assert!(journal_len(&dir).await > 0);
    write_trajectory_index_atomic(&dir, &stale).await.unwrap();
    assert_eq!(journal_len(&dir).await, 0);
    let persisted = read_trajectory_index(&dir).await.unwrap().unwrap();
    assert_eq!(persisted.entries.len(), 2);
    assert!(persisted.applied_seq > stale.applied_seq);
}

#[tokio::test]
async fn legacy_checkpoint_preserves_already_compacted_commits() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("trajectories");
    let path = write_trajectory(&dir, "chat-1", "One", "agent").await;
    upsert_trajectory_index_entry(&dir, entry_for_path(&dir, &path).await)
        .await
        .unwrap();
    let mut stale = read_trajectory_index(&dir).await.unwrap().unwrap();
    let coordinator = TrajectoryIndexCoordinator::new();
    coordinator.snapshot(&dir, None).await.unwrap();
    let path = write_trajectory(&dir, "chat-2", "Two", "agent").await;
    coordinator
        .upsert(&dir, entry_for_path(&dir, &path).await)
        .await
        .unwrap();
    coordinator.flush_all().await.unwrap();
    assert!(journal_len(&dir).await > 0);
    let latest = read_trajectory_index(&dir).await.unwrap().unwrap();
    write_trajectory_index_atomic(&dir, &latest).await.unwrap();
    assert_eq!(journal_len(&dir).await, 0);
    // A's pending deletion must apply, without deleting B's compacted insert.
    stale.entries.clear();
    write_trajectory_index_atomic(&dir, &stale).await.unwrap();
    let persisted = read_trajectory_index(&dir).await.unwrap().unwrap();
    assert_eq!(
        persisted
            .entries
            .iter()
            .map(|entry| entry.id.as_str())
            .collect::<Vec<_>>(),
        vec!["chat-2"]
    );
    assert_eq!(persisted.applied_seq, latest.applied_seq);
}

#[tokio::test]
async fn legacy_checkpoint_does_not_resurrect_compacted_removals_or_updates() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("trajectories");
    for id in ["removed", "updated", "unchanged"] {
        let path = write_trajectory(&dir, id, "Old", "agent").await;
        upsert_trajectory_index_entry(&dir, entry_for_path(&dir, &path).await)
            .await
            .unwrap();
    }
    let mut stale = read_trajectory_index(&dir).await.unwrap().unwrap();
    let mut latest = stale.clone();
    latest.entries.retain(|entry| entry.id != "removed");
    latest
        .entries
        .iter_mut()
        .find(|entry| entry.id == "updated")
        .unwrap()
        .title = "New".into();
    write_trajectory_index_atomic(&dir, &latest).await.unwrap();
    stale
        .entries
        .iter_mut()
        .find(|entry| entry.id == "unchanged")
        .unwrap()
        .title = "Local".into();
    write_trajectory_index_atomic(&dir, &stale).await.unwrap();
    let persisted = read_trajectory_index(&dir).await.unwrap().unwrap();
    assert!(!persisted.entries.iter().any(|entry| entry.id == "removed"));
    assert_eq!(
        persisted
            .entries
            .iter()
            .find(|entry| entry.id == "updated")
            .unwrap()
            .title,
        "New"
    );
    assert_eq!(
        persisted
            .entries
            .iter()
            .find(|entry| entry.id == "unchanged")
            .unwrap()
            .title,
        "Local"
    );
}

#[tokio::test]
async fn persisted_reader_and_checkpoint_writer_wait_for_file_lock() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().to_path_buf();
    let index = TrajectoryIndex {
        schema_version: TRAJECTORY_INDEX_SCHEMA_VERSION,
        updated_at: Utc::now().to_rfc3339(),
        ..Default::default()
    };
    write_trajectory_index_atomic(&dir, &index).await.unwrap();
    let lock_dir = dir.clone();
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let blocker = tokio::task::spawn_blocking(move || {
        let mut lock = file_lock::open_lock(&lock_dir.join(".index.json.lock")).unwrap();
        let _guard = file_lock::try_lock(&mut lock).unwrap();
        ready_tx.send(()).unwrap();
        release_rx.recv().unwrap();
    });
    ready_rx.await.unwrap();
    let read_dir = dir.clone();
    let mut reader =
        tokio::task::spawn_blocking(move || read_persisted_trajectory_index_sync(&read_dir));
    let write_dir = dir.clone();
    let mut writer =
        tokio::spawn(async move { write_trajectory_index_atomic(&write_dir, &index).await });
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut reader)
            .await
            .is_err()
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut writer)
            .await
            .is_err()
    );
    release_tx.send(()).unwrap();
    blocker.await.unwrap();
    reader.await.unwrap().unwrap();
    writer.await.unwrap().unwrap();
}

#[tokio::test]
async fn journal_replay_recovers_unflushed_commits_after_a_restart() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("trajectories");
    let path_1 = write_trajectory(&dir, "chat-1", "One", "agent").await;
    let coordinator = TrajectoryIndexCoordinator::new();
    coordinator
        .upsert(&dir, entry_for_path(&dir, &path_1).await)
        .await
        .unwrap();
    coordinator.flush_all().await.unwrap();
    let checkpoint_bytes = fs::read(trajectory_index_path(&dir)).await.unwrap();

    let path_2 = write_trajectory(&dir, "chat-2", "Two", "agent").await;
    coordinator
        .upsert(&dir, entry_for_path(&dir, &path_2).await)
        .await
        .unwrap();
    coordinator.flush_all().await.unwrap();

    assert!(
        journal_len(&dir).await > 0,
        "the batch must reach the journal"
    );
    assert_eq!(
        fs::read(trajectory_index_path(&dir)).await.unwrap(),
        checkpoint_bytes,
        "an appended batch must not rewrite the checkpoint"
    );
    let checkpoint: TrajectoryIndex = serde_json::from_slice(&checkpoint_bytes).unwrap();
    assert_eq!(checkpoint.entries.len(), 1);
    drop(coordinator);

    let restarted = TrajectoryIndexCoordinator::new();
    let mut ids: Vec<String> = restarted
        .snapshot(&dir, None)
        .await
        .unwrap()
        .entries
        .into_iter()
        .map(|entry| entry.id)
        .collect();
    ids.sort();
    assert_eq!(ids, vec!["chat-1".to_string(), "chat-2".to_string()]);
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
async fn journal_tolerates_a_torn_last_line() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("trajectories");
    let path_1 = write_trajectory(&dir, "chat-1", "One", "agent").await;
    let coordinator = TrajectoryIndexCoordinator::new();
    coordinator
        .upsert(&dir, entry_for_path(&dir, &path_1).await)
        .await
        .unwrap();
    coordinator.flush_all().await.unwrap();
    let path_2 = write_trajectory(&dir, "chat-2", "Two", "agent").await;
    coordinator
        .upsert(&dir, entry_for_path(&dir, &path_2).await)
        .await
        .unwrap();
    coordinator.flush_all().await.unwrap();

    let journal_path = trajectory_index_journal_path(&dir);
    let mut journal = fs::read(&journal_path).await.unwrap();
    journal.extend_from_slice(br#"{"schema":1,"seq":99,"writer_id":"tor"#);
    fs::write(&journal_path, journal).await.unwrap();

    let index = read_trajectory_index(&dir).await.unwrap().unwrap();
    assert_eq!(index.entries.len(), 2);
    assert!(index.entries.iter().any(|entry| entry.id == "chat-2"));

    let path_3 = write_trajectory(&dir, "chat-3", "Three", "agent").await;
    let restarted = TrajectoryIndexCoordinator::new();
    restarted
        .upsert(&dir, entry_for_path(&dir, &path_3).await)
        .await
        .unwrap();
    restarted.flush_all().await.unwrap();
    assert_eq!(
        read_trajectory_index(&dir)
            .await
            .unwrap()
            .unwrap()
            .entries
            .len(),
        3
    );
}

#[tokio::test]
async fn corrupt_journal_line_rebuilds_the_index_from_files() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("trajectories");
    let path_1 = write_trajectory(&dir, "chat-1", "One", "agent").await;
    let coordinator = TrajectoryIndexCoordinator::new();
    coordinator
        .upsert(&dir, entry_for_path(&dir, &path_1).await)
        .await
        .unwrap();
    coordinator.flush_all().await.unwrap();
    let path_2 = write_trajectory(&dir, "chat-2", "Two", "agent").await;
    coordinator
        .upsert(&dir, entry_for_path(&dir, &path_2).await)
        .await
        .unwrap();
    coordinator.flush_all().await.unwrap();

    let journal_path = trajectory_index_journal_path(&dir);
    let mut journal = b"{\"schema\":1,\"seq\":1,\"corrupt\":true}\n".to_vec();
    journal.extend_from_slice(&fs::read(&journal_path).await.unwrap());
    fs::write(&journal_path, journal).await.unwrap();

    assert!(read_trajectory_index(&dir).await.is_err());

    let restarted = TrajectoryIndexCoordinator::new();
    let mut ids: Vec<String> = restarted
        .list_entries(&dir, None)
        .await
        .unwrap()
        .into_iter()
        .map(|entry| entry.id)
        .collect();
    ids.sort();
    assert_eq!(ids, vec!["chat-1".to_string(), "chat-2".to_string()]);
    assert_eq!(journal_len(&dir).await, 0, "recovery checkpoints the index");
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
async fn checkpoint_truncates_the_journal_and_advances_the_watermark() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("trajectories");
    let path_1 = write_trajectory(&dir, "chat-1", "One", "agent").await;
    let coordinator = TrajectoryIndexCoordinator::new();
    coordinator
        .upsert(&dir, entry_for_path(&dir, &path_1).await)
        .await
        .unwrap();
    coordinator.flush_all().await.unwrap();
    let path_2 = write_trajectory(&dir, "chat-2", "Two", "agent").await;
    coordinator
        .upsert(&dir, entry_for_path(&dir, &path_2).await)
        .await
        .unwrap();
    coordinator.flush_all().await.unwrap();
    assert!(journal_len(&dir).await > 0);

    coordinator.checkpoint_all().await.unwrap();

    assert_eq!(journal_len(&dir).await, 0);
    let raw = fs::read(trajectory_index_path(&dir)).await.unwrap();
    let checkpoint: TrajectoryIndex = serde_json::from_slice(&raw).unwrap();
    assert_eq!(checkpoint.entries.len(), 2);
    assert!(
        checkpoint.applied_seq > 0,
        "the checkpoint must carry the journal watermark"
    );
}

#[tokio::test]
async fn a_shrunken_journal_forces_a_full_reload() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("trajectories");
    let path_1 = write_trajectory(&dir, "chat-1", "One", "agent").await;
    let first = TrajectoryIndexCoordinator::new();
    let second = TrajectoryIndexCoordinator::new();
    first
        .upsert(&dir, entry_for_path(&dir, &path_1).await)
        .await
        .unwrap();
    first.flush_all().await.unwrap();
    assert_eq!(second.snapshot(&dir, None).await.unwrap().entries.len(), 1);

    let path_2 = write_trajectory(&dir, "chat-2", "Two", "agent").await;
    first
        .upsert(&dir, entry_for_path(&dir, &path_2).await)
        .await
        .unwrap();
    first.flush_all().await.unwrap();

    let path_3 = write_trajectory(&dir, "chat-3", "Three", "agent").await;
    second
        .upsert(&dir, entry_for_path(&dir, &path_3).await)
        .await
        .unwrap();
    second.flush_all().await.unwrap();

    first.checkpoint_all().await.unwrap();
    assert_eq!(journal_len(&dir).await, 0);

    let path_4 = write_trajectory(&dir, "chat-4", "Four", "agent").await;
    second
        .upsert(&dir, entry_for_path(&dir, &path_4).await)
        .await
        .unwrap();
    second.flush_all().await.unwrap();

    let mut ids: Vec<String> = read_trajectory_index(&dir)
        .await
        .unwrap()
        .unwrap()
        .entries
        .into_iter()
        .map(|entry| entry.id)
        .collect();
    ids.sort();
    assert_eq!(
        ids,
        vec![
            "chat-1".to_string(),
            "chat-2".to_string(),
            "chat-3".to_string(),
            "chat-4".to_string()
        ]
    );
}

#[tokio::test]
async fn a_thousand_commits_produce_a_handful_of_checkpoint_rewrites() {
    const COMMITS: usize = 1_000;

    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("trajectories");
    let path = write_trajectory(&dir, "chat-1", "One", "agent").await;
    let listing_counter_scope = TrajectoryIndexListingCounterScope::local();
    let coordinator =
        TrajectoryIndexCoordinator::with_listing_counter_scope(listing_counter_scope.clone());
    let entry = entry_for_path(&dir, &path).await;

    for commit in 0..COMMITS {
        let mut entry = entry.clone();
        entry.message_count = commit + 1;
        coordinator.upsert(&dir, entry).await.unwrap();
        coordinator.flush_all().await.unwrap();
    }

    let counters = listing_counter_scope.snapshot();
    assert!(
        counters.checkpoint_writes <= 4,
        "expected a handful of checkpoints, got {}",
        counters.checkpoint_writes
    );
    assert!(
        counters.journal_appends >= u64::try_from(COMMITS).unwrap() - 4,
        "expected the commits to reach the journal, got {}",
        counters.journal_appends
    );
    let index = read_trajectory_index(&dir).await.unwrap().unwrap();
    assert_eq!(index.entries.len(), 1);
    assert_eq!(index.entries[0].message_count, COMMITS);
}
