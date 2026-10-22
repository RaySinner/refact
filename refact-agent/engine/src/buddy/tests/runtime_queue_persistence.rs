use super::*;

#[tokio::test]
async fn duplicate_diagnostic_coalesces_without_history_spam() {
    let mut svc = make_service();
    let ctx = DiagnosticContext {
        error_type: "frontend".to_string(),
        error_message: "Repeated render error".to_string(),
        source_file: Some("frontend/window_error".to_string()),
        tool_name: None,
        chat_id: Some("chat-1".to_string()),
        model_id: None,
        collected_at: chrono::Utc::now().to_rfc3339(),
        severity: DiagnosticSeverity::High,
        occurrences: None,
    };
    let mut later = ctx.clone();
    later.collected_at = (chrono::Utc::now() + chrono::Duration::seconds(1)).to_rfc3339();

    svc.add_diagnostic(ctx);
    let event_id = svc.runtime_queue.items.front().unwrap().id.clone();
    assert!(svc.dismiss_runtime_event_by_id(&event_id));
    svc.add_diagnostic(later);

    assert_eq!(svc.recent_diagnostics.len(), 1);
    assert_eq!(svc.runtime_queue.items.len(), 1);
    assert!(svc.runtime_queue.items.front().unwrap().dismissed);
}

#[test]
fn test_queue_eviction_drops_oldest_low_priority() {
    use super::runtime_queue::RuntimeQueue;
    use super::actor::make_runtime_event;
    let mut queue = RuntimeQueue::new();
    for i in 0..50 {
        let mut ev = make_runtime_event(
            "signal",
            &format!("low-{}", i),
            "src",
            &format!("low-key-{}", i),
            "started",
            None,
        );
        ev.priority = "low".to_string();
        queue.enqueue(ev);
    }
    for i in 0..55 {
        let ev = make_runtime_event(
            "signal",
            &format!("normal-{}", i),
            "src",
            &format!("normal-key-{}", i),
            "started",
            Some("normal"),
        );
        queue.enqueue(ev);
    }
    assert!(queue.items.len() <= 100);
    assert!(
        !queue.items.iter().any(|e| e.title == "low-0"),
        "oldest low-priority item must be evicted first"
    );
    assert!(
        queue.items.iter().any(|e| e.title == "low-49"),
        "newest low-priority items should survive"
    );
}

#[tokio::test]
async fn test_ledger_skips_empty_chat_id() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    super::storage::bootstrap_buddy_storage(root).await.unwrap();

    let bad_path = root.join(".refact/buddy/chats/conversations/no_id.json");
    let bad_json = serde_json::json!({
        "title": "Missing ID", "kind": "chat",
        "created_at": "2024-01-01T00:00:00Z", "messages": []
    });
    super::storage::atomic_write_json(&bad_path, &bad_json)
        .await
        .unwrap();

    let good_path = root.join(".refact/buddy/chats/conversations/has_id.json");
    let good_json = serde_json::json!({
        "chat_id": "has_id", "title": "Good Chat", "kind": "chat",
        "created_at": "2024-01-02T00:00:00Z", "messages": []
    });
    super::storage::atomic_write_json(&good_path, &good_json)
        .await
        .unwrap();

    let entries = super::conversation_ledger::list_all_buddy_conversations(root, None).await;
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].id, "has_id");
    assert!(!tokio::fs::try_exists(&bad_path).await.unwrap());
    assert!(tokio::fs::try_exists(&good_path).await.unwrap());
}

#[tokio::test]
async fn test_ledger_lists_id_only_conversation_and_repairs_alias() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    super::storage::bootstrap_buddy_storage(root).await.unwrap();

    let path = root.join(".refact/buddy/chats/conversations/id_only.json");
    let json = serde_json::json!({
        "id": "id_only", "title": "Recovered Chat", "kind": "chat",
        "created_at": "2024-01-01T00:00:00Z",
        "last_message_at": "2024-01-02T00:00:00Z",
        "messages": [{"role": "user", "content": "hi"}]
    });
    super::storage::atomic_write_json(&path, &json)
        .await
        .unwrap();

    let entries = super::conversation_ledger::list_all_buddy_conversations(root, None).await;
    let entry = entries.iter().find(|entry| entry.id == "id_only").unwrap();

    assert_eq!(entry.title, "Recovered Chat");
    assert_eq!(entry.message_count, 1);
    assert!(tokio::fs::try_exists(&path).await.unwrap());
    let repaired: serde_json::Value =
        serde_json::from_str(&tokio::fs::read_to_string(&path).await.unwrap()).unwrap();
    assert_eq!(repaired["id"].as_str(), Some("id_only"));
    assert_eq!(repaired["chat_id"].as_str(), Some("id_only"));
}

#[tokio::test]
async fn test_ledger_missing_id_and_chat_id_is_quarantined() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    super::storage::bootstrap_buddy_storage(root).await.unwrap();

    let path = root.join(".refact/buddy/chats/conversations/missing_ids.json");
    let json = serde_json::json!({
        "title": "Missing IDs", "kind": "chat",
        "created_at": "2024-01-01T00:00:00Z",
        "messages": [{"role": "user", "content": "hi"}]
    });
    super::storage::atomic_write_json(&path, &json)
        .await
        .unwrap();

    let (entries, diagnostics) =
        super::conversation_ledger::list_all_buddy_conversations_with_diagnostics(root, None).await;

    assert!(entries.iter().all(|entry| entry.title != "Missing IDs"));
    assert!(!tokio::fs::try_exists(&path).await.unwrap());
    let quarantined = root.join(".refact/buddy/chats/conversations/missing_ids.json.corrupt");
    assert!(tokio::fs::try_exists(&quarantined).await.unwrap());
    assert_eq!(diagnostics.missing_id, 1);
    assert_eq!(diagnostics.quarantined, 1);
}

#[test]
fn test_workflow_label_mapping() {
    assert_eq!(
        super::workflows::workflow_label("commit_message"),
        "commit message generation"
    );
    assert_eq!(
        super::workflows::workflow_label("follow_up"),
        "follow-up suggestions"
    );
    assert_eq!(
        super::workflows::workflow_label("compress_trajectory"),
        "chat compression"
    );
    assert_eq!(
        super::workflows::workflow_label("memo_extraction"),
        "memo extraction"
    );
    assert_eq!(
        super::workflows::workflow_label("kg_enrich"),
        "knowledge graph enrichment"
    );
    assert_eq!(
        super::workflows::workflow_label("kg_deprecate"),
        "knowledge cleanup"
    );
    assert_eq!(
        super::workflows::workflow_label("title_generating"),
        "title generation"
    );
    assert_eq!(
        super::workflows::workflow_label("unknown_workflow"),
        "unknown_workflow"
    );
}

#[test]
fn test_event_title_passthrough_for_overlong_title() {
    use super::actor::make_runtime_event;
    let long_title = "A".repeat(201);
    let ev = make_runtime_event("signal", &long_title, "src", "key", "started", None);
    assert_eq!(
        ev.title, long_title,
        "make_runtime_event stores overlong titles as-is"
    );
    assert!(
        ev.title.len() > 200,
        "test input must exercise overlong title passthrough"
    );
    let chat_label: String = "Some very long chat title that goes on and on and on and on and on"
        .chars()
        .take(60)
        .collect();
    let ev2 = make_runtime_event(
        "chat_started",
        &format!("Started: {}", chat_label),
        "chat",
        "chat_123",
        "started",
        None,
    );
    assert!(
        ev2.title.len() <= 120,
        "chat started event title must be under 120 chars"
    );
}

#[test]
fn test_runtime_event_chat_id_default_none() {
    use super::actor::make_runtime_event;
    let ev = make_runtime_event(
        "indexing",
        "Indexing...",
        "indexer",
        "indexing",
        "started",
        None,
    );
    assert!(ev.chat_id.is_none(), "default event must have no chat_id");
}

#[test]
fn test_runtime_event_chat_id_serialized_when_set() {
    use super::actor::make_runtime_event;
    let mut ev = make_runtime_event(
        "chat_error",
        "Error",
        "chat",
        "chat_abc",
        "failed",
        Some("high"),
    );
    ev.chat_id = Some("abc-123".to_string());
    let json = serde_json::to_string(&ev).unwrap();
    assert!(
        json.contains("\"chat_id\":\"abc-123\""),
        "chat_id must be serialized when set"
    );
}

#[test]
fn test_runtime_event_chat_id_skipped_when_none() {
    use super::actor::make_runtime_event;
    let ev = make_runtime_event(
        "chat_completed",
        "Done",
        "chat",
        "chat_abc",
        "completed",
        None,
    );
    let json = serde_json::to_string(&ev).unwrap();
    assert!(
        !json.contains("chat_id"),
        "chat_id must be skipped when None"
    );
}

#[test]
fn test_chat_error_event_includes_chat_id() {
    use super::actor::make_runtime_event;
    let chat_id = "test-chat-xyz";
    let mut ev = make_runtime_event(
        "chat_error",
        "Error in 'Test chat': something failed",
        "chat",
        &format!("chat_{}", chat_id),
        "failed",
        Some("high"),
    );
    ev.chat_id = Some(chat_id.to_string());
    assert_eq!(ev.chat_id.as_deref(), Some(chat_id));
    assert_eq!(ev.status, "failed");
}

// =============================================================================
// Runtime queue persistence — JSONL log invariants
// =============================================================================
/// Enqueue an event then dismiss it; the JSONL log must replay to a queue
/// where the event is still present and `dismissed = true`.
#[tokio::test]
async fn test_runtime_queue_dismissal_survives_restart() {
    use super::actor::make_runtime_event;
    use super::storage::{append_runtime_record, load_runtime_queue, RuntimeQueueRecord};

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    let mut ev = make_runtime_event(
        "error",
        "boom",
        "frontend/window_error",
        "key-1",
        "failed",
        Some("high"),
    );
    let id = ev.id.clone();
    append_runtime_record(root, &RuntimeQueueRecord::Event { event: ev.clone() })
        .await
        .unwrap();

    // Dismissal is recorded as a fresh Event with dismissed=true.
    ev.dismissed = true;
    append_runtime_record(root, &RuntimeQueueRecord::Event { event: ev.clone() })
        .await
        .unwrap();

    let queue = load_runtime_queue(root).await;
    let restored = queue
        .items
        .iter()
        .find(|e| e.id == id)
        .expect("event missing");
    assert!(restored.dismissed, "dismissal must survive replay");
}

/// Fill the queue past its cap, write a `Removed` tombstone for an evicted
/// event, then verify replay matches the in-memory survivors.
#[tokio::test]
async fn test_runtime_queue_eviction_tombstones_replay() {
    use super::actor::make_runtime_event;
    use super::storage::{append_runtime_record, load_runtime_queue, RuntimeQueueRecord};

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    let mut victim = make_runtime_event(
        "signal",
        "doomed",
        "src",
        "victim-key",
        "started",
        Some("low"),
    );
    victim.priority = "low".to_string();
    let victim_id = victim.id.clone();
    append_runtime_record(
        root,
        &RuntimeQueueRecord::Event {
            event: victim.clone(),
        },
    )
    .await
    .unwrap();

    let survivor = make_runtime_event(
        "signal",
        "kept",
        "src",
        "survivor-key",
        "started",
        Some("normal"),
    );
    let survivor_id = survivor.id.clone();
    append_runtime_record(
        root,
        &RuntimeQueueRecord::Event {
            event: survivor.clone(),
        },
    )
    .await
    .unwrap();

    // Eviction tombstone for the victim.
    append_runtime_record(
        root,
        &RuntimeQueueRecord::Removed {
            id: victim_id.clone(),
        },
    )
    .await
    .unwrap();

    let queue = load_runtime_queue(root).await;
    assert!(
        !queue.items.iter().any(|e| e.id == victim_id),
        "evicted event must not resurrect"
    );
    assert!(
        queue.items.iter().any(|e| e.id == survivor_id),
        "survivor must remain"
    );
}

/// Two writes for the same id; on disk, the LATER record (in file order) wins.
/// Because all production writes are funneled through one writer task, file
/// order matches in-memory order — this test pins down the contract.
#[tokio::test]
async fn test_runtime_queue_replay_uses_latest_event() {
    use super::actor::make_runtime_event;
    use super::storage::{append_runtime_record, load_runtime_queue, RuntimeQueueRecord};

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    let mut ev = make_runtime_event(
        "error",
        "first title",
        "src",
        "race-key",
        "started",
        Some("normal"),
    );
    let id = ev.id.clone();
    append_runtime_record(root, &RuntimeQueueRecord::Event { event: ev.clone() })
        .await
        .unwrap();

    ev.title = "second title".to_string();
    ev.status = "completed".to_string();
    append_runtime_record(root, &RuntimeQueueRecord::Event { event: ev.clone() })
        .await
        .unwrap();

    let queue = load_runtime_queue(root).await;
    let restored = queue.items.iter().find(|e| e.id == id).expect("missing");
    assert_eq!(restored.title, "second title");
    assert_eq!(restored.status, "completed");
}

/// `now_playing` is its own JSONL record kind. Setting it, then clearing it,
/// must survive a round-trip.
#[tokio::test]
async fn test_runtime_queue_now_playing_persists() {
    use super::actor::make_runtime_event;
    use super::storage::{append_runtime_record, load_runtime_queue, RuntimeQueueRecord};

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    let np = make_runtime_event(
        "streaming",
        "live",
        "chat",
        "np-key",
        "started",
        Some("normal"),
    );
    let np_id = np.id.clone();
    append_runtime_record(
        root,
        &RuntimeQueueRecord::NowPlaying {
            event: Some(np.clone()),
        },
    )
    .await
    .unwrap();

    let queue = load_runtime_queue(root).await;
    assert_eq!(
        queue.now_playing.as_ref().map(|e| e.id.clone()),
        Some(np_id.clone()),
        "now_playing must round-trip"
    );

    append_runtime_record(root, &RuntimeQueueRecord::NowPlaying { event: None })
        .await
        .unwrap();
    let queue = load_runtime_queue(root).await;
    assert!(
        queue.now_playing.is_none(),
        "clearing now_playing must persist"
    );
}

#[test]
fn buddy_service_expire_runtime_events_persists_removed_tombstones() {
    use super::actor::{make_runtime_event, RuntimeQueueWriteOp};
    use super::storage::RuntimeQueueRecord;

    let (events_tx, _) = broadcast::channel(16);
    let (queue_tx, mut queue_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut svc = BuddyService::new(
        std::env::temp_dir().join(format!("buddy-test-{}", uuid::Uuid::new_v4())),
        default_buddy_state(),
        BuddySettings::default(),
        Vec::new(),
        super::runtime_queue::RuntimeQueue::new(),
        events_tx,
        Some(queue_tx),
    );
    let mut event = make_runtime_event(
        "signal",
        "expired",
        "src",
        "expired-key",
        "completed",
        Some("normal"),
    );
    event.id = "expired-runtime-event".to_string();
    event.created_at = "2024-01-01T00:00:00Z".to_string();
    event.ttl_ms = Some(1000);
    svc.enqueue_runtime_event(event);

    let now = chrono::DateTime::parse_from_rfc3339("2024-01-01T00:00:02Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    let removed = svc.expire_runtime_events_at(now);

    assert_eq!(removed, vec!["expired-runtime-event".to_string()]);
    assert!(svc.runtime_queue.items.is_empty());
    let mut saw_tombstone = false;
    while let Ok(op) = queue_rx.try_recv() {
        if let RuntimeQueueWriteOp::Append(RuntimeQueueRecord::Removed { id }) = op {
            if id == "expired-runtime-event" {
                saw_tombstone = true;
            }
        }
    }
    assert!(saw_tombstone);
}

#[test]
fn buddy_service_stale_runtime_prune_persists_removed_tombstones() {
    use super::actor::{make_runtime_event, RuntimeQueueWriteOp};
    use super::storage::RuntimeQueueRecord;

    let (events_tx, _) = broadcast::channel(16);
    let (queue_tx, mut queue_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut svc = BuddyService::new(
        std::env::temp_dir().join(format!("buddy-test-{}", uuid::Uuid::new_v4())),
        default_buddy_state(),
        BuddySettings::default(),
        Vec::new(),
        super::runtime_queue::RuntimeQueue::new(),
        events_tx,
        Some(queue_tx),
    );
    let mut stale = make_runtime_event(
        "chat_completed",
        "old completed chat",
        "chat",
        "stale-runtime-key",
        "completed",
        Some("normal"),
    );
    stale.id = "stale-runtime-event".to_string();
    stale.created_at = "2024-01-01T00:00:00Z".to_string();
    stale.ttl_ms = None;
    svc.enqueue_runtime_event(stale);

    let now = chrono::DateTime::parse_from_rfc3339("2024-01-01T02:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    let removed = svc.expire_runtime_events_at(now);

    assert_eq!(removed, vec!["stale-runtime-event".to_string()]);
    assert!(svc.runtime_queue.items.is_empty());
    let mut saw_event_append = false;
    let mut saw_tombstone = false;
    while let Ok(op) = queue_rx.try_recv() {
        match op {
            RuntimeQueueWriteOp::Append(RuntimeQueueRecord::Event { event }) => {
                if event.id == "stale-runtime-event" {
                    saw_event_append = true;
                }
            }
            RuntimeQueueWriteOp::Append(RuntimeQueueRecord::Removed { id }) => {
                if id == "stale-runtime-event" {
                    saw_tombstone = true;
                }
            }
            _ => {}
        }
    }
    assert!(saw_event_append);
    assert!(saw_tombstone);
}

/// Backward-compat: legacy logs that contain bare `BuddyRuntimeEvent` JSON
/// objects (no `kind` tag) must still load successfully.
#[tokio::test]
async fn test_runtime_queue_loads_legacy_format() {
    use super::actor::make_runtime_event;

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let path = root.join(".refact/buddy/runtime_queue.jsonl");
    tokio::fs::create_dir_all(path.parent().unwrap())
        .await
        .unwrap();

    let ev = make_runtime_event(
        "error",
        "legacy",
        "src",
        "legacy-key",
        "failed",
        Some("high"),
    );
    let id = ev.id.clone();
    let line = format!("{}\n", serde_json::to_string(&ev).unwrap());
    tokio::fs::write(&path, line).await.unwrap();

    let queue = super::storage::load_runtime_queue(root).await;
    assert!(
        queue.items.iter().any(|e| e.id == id),
        "legacy bare-event line must be readable"
    );
}

#[tokio::test]
async fn test_stale_runtime_load_pruning_compacts_removed_records() {
    use super::actor::make_runtime_event;
    use super::storage::{append_runtime_record, load_runtime_queue, RuntimeQueueRecord};

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let mut stale = make_runtime_event(
        "chat_completed",
        "old completed chat",
        "chat",
        "stale-load-key",
        "completed",
        Some("normal"),
    );
    stale.id = "stale-load-event".to_string();
    stale.created_at = (chrono::Utc::now() - Duration::hours(2)).to_rfc3339();
    stale.ttl_ms = None;
    append_runtime_record(root, &RuntimeQueueRecord::Event { event: stale })
        .await
        .unwrap();

    let queue = load_runtime_queue(root).await;

    assert!(queue.items.is_empty());
    let content = tokio::fs::read_to_string(root.join(".refact/buddy/runtime_queue.jsonl"))
        .await
        .unwrap();
    assert!(!content.contains("stale-load-event"));
}

// =============================================================================
// Redaction strength
// =============================================================================
#[test]
fn test_redact_handles_multiple_secrets_and_case() {
    let input = "first Bearer abc def then bearer XYZ and api_key=p1 plus API_KEY=p2 token=tok1";
    let output = super::actor::redact_sensitive(input);
    assert!(
        !output.contains("abc"),
        "first Bearer secret leaked: {}",
        output
    );
    assert!(
        !output.contains("XYZ"),
        "lowercase bearer leaked: {}",
        output
    );
    assert!(!output.contains("p1"), "first api_key leaked: {}", output);
    assert!(
        !output.contains("p2"),
        "second case-variant api_key leaked: {}",
        output
    );
    assert!(!output.contains("tok1"), "token leaked: {}", output);
    // Should redact at least 4 distinct secrets in this string.
    let redactions = output.matches("[REDACTED]").count();
    assert!(
        redactions >= 4,
        "expected >=4 redactions, got {}: {}",
        redactions,
        output
    );
}

#[tokio::test]
async fn generated_buddy_speech_with_secret_uses_fallback() {
    let (service, _renderer) =
        crate::buddy::voice_service::test_voice_service_with_responses(vec![Some(
            "Tiny alarm: Bearer sk-VERYSECRET1234567890 is wobbling.".to_string(),
        )]);
    let _guard = crate::buddy::voice_service::install_test_voice_service(service).await;
    let app =
        crate::app_state::AppState::from_gcx(crate::global_context::tests::make_test_gcx().await)
            .await;
    let fallback = "Quest complete: safe fallback sparkle.".to_string();

    let speech = super::actor::render_buddy_speech(
        app,
        super::types::BuddyPersonalityProfile::default(),
        "Pixel".to_string(),
        BuddyPulse::default(),
        Some("secret_speech_test".to_string()),
        "workflow summary".to_string(),
        crate::buddy::voice_service::SpeechIntent::QuestComplete,
        fallback.clone(),
    )
    .await;

    assert_eq!(speech.text, fallback);
    assert!(!speech.text.contains("VERYSECRET"));
    assert!(!speech.text.contains("[REDACTED"));
}

#[tokio::test]
async fn generated_runtime_event_title_and_description_with_secret_use_fallbacks() {
    let (service, _renderer) =
        crate::buddy::voice_service::test_voice_service_with_responses(vec![Some(
            "Runtime Bearer sk-RUNTIMESECRET1234567890".to_string(),
        )]);
    let _guard = crate::buddy::voice_service::install_test_voice_service(service).await;
    let app =
        crate::app_state::AppState::from_gcx(crate::global_context::tests::make_test_gcx().await)
            .await;
    let fallback_title = "Safe runtime title".to_string();
    let fallback_description = "Safe runtime description".to_string();

    let (title, description) = super::actor::render_buddy_runtime_event(
        app,
        super::types::BuddyPersonalityProfile::default(),
        "Pixel".to_string(),
        BuddyPulse::default(),
        Some("secret_runtime_test".to_string()),
        "Description Bearer sk-DESCRIPTIONSECRET1234567890".to_string(),
        "completed",
        fallback_title.clone(),
        Some(fallback_description.clone()),
    )
    .await;

    assert_eq!(title, fallback_title);
    assert_eq!(description.as_deref(), Some(fallback_description.as_str()));
    let rendered = format!("{} {}", title, description.unwrap_or_default());
    assert!(!rendered.contains("RUNTIMESECRET"));
    assert!(!rendered.contains("DESCRIPTIONSECRET"));
    assert!(!rendered.contains("[REDACTED"));
}

#[tokio::test]
async fn generated_activity_title_with_secret_uses_fallback() {
    let (service, _renderer) = crate::buddy::voice_service::test_voice_service_with_responses(
        vec![Some("Activity token=ACTIVITYSECRET".to_string())],
    );
    let _guard = crate::buddy::voice_service::install_test_voice_service(service).await;
    let app =
        crate::app_state::AppState::from_gcx(crate::global_context::tests::make_test_gcx().await)
            .await;
    let fallback = "Safe activity title".to_string();

    let title = super::actor::render_buddy_activity_title(
        app,
        super::types::BuddyPersonalityProfile::default(),
        "Pixel".to_string(),
        BuddyPulse::default(),
        Some("secret_activity_test".to_string()),
        "workflow summary".to_string(),
        crate::buddy::voice_service::VoiceIntent::ActivityTitle,
        fallback.clone(),
    )
    .await;

    assert_eq!(title, fallback);
    assert!(!title.contains("ACTIVITYSECRET"));
    assert!(!title.contains("[REDACTED"));
}

#[tokio::test]
async fn normal_generated_buddy_speech_is_preserved() {
    let generated = "Tiny boots on, checking the path.";
    let (service, _renderer) = crate::buddy::voice_service::test_voice_service_with_responses(
        vec![Some(generated.to_string())],
    );
    let _guard = crate::buddy::voice_service::install_test_voice_service(service).await;
    let app =
        crate::app_state::AppState::from_gcx(crate::global_context::tests::make_test_gcx().await)
            .await;

    let speech = super::actor::render_buddy_speech(
        app,
        super::types::BuddyPersonalityProfile::default(),
        "Pixel".to_string(),
        BuddyPulse::default(),
        Some("normal_speech_test".to_string()),
        "workflow summary".to_string(),
        crate::buddy::voice_service::SpeechIntent::QuestAccept,
        "fallback should not be used".to_string(),
    )
    .await;

    assert_eq!(speech.text, generated);
}

#[tokio::test]
async fn generated_buddy_speech_with_secret_fallback_uses_static_safe_text() {
    let (service, _renderer) =
        crate::buddy::voice_service::test_voice_service_with_responses(vec![Some(
            "Speech Bearer sk-GENERATEDSECRET1234567890".to_string(),
        )]);
    let _guard = crate::buddy::voice_service::install_test_voice_service(service).await;
    let app =
        crate::app_state::AppState::from_gcx(crate::global_context::tests::make_test_gcx().await)
            .await;

    let speech = super::actor::render_buddy_speech(
        app,
        super::types::BuddyPersonalityProfile::default(),
        "Pixel".to_string(),
        BuddyPulse::default(),
        Some("secret_fallback_speech_test".to_string()),
        "workflow summary".to_string(),
        crate::buddy::voice_service::SpeechIntent::QuestComplete,
        "Fallback Bearer sk-FALLBACKSECRET1234567890".to_string(),
    )
    .await;

    assert_eq!(
        speech.text,
        "Tiny gremlin update: something needs attention."
    );
    assert!(!speech.text.contains("GENERATEDSECRET"));
    assert!(!speech.text.contains("FALLBACKSECRET"));
    assert!(!speech.text.contains("[REDACTED"));
}

#[tokio::test]
async fn runtime_event_secret_fallbacks_use_static_safe_text() {
    let (service, _renderer) = crate::buddy::voice_service::test_voice_service_with_responses(
        vec![Some("Runtime token=GENERATEDSECRET".to_string())],
    );
    let _guard = crate::buddy::voice_service::install_test_voice_service(service).await;
    let app =
        crate::app_state::AppState::from_gcx(crate::global_context::tests::make_test_gcx().await)
            .await;

    let (title, description) = super::actor::render_buddy_runtime_event(
        app,
        super::types::BuddyPersonalityProfile::default(),
        "Pixel".to_string(),
        BuddyPulse::default(),
        Some("secret_fallback_runtime_test".to_string()),
        "Workflow Bearer sk-DESCRIPTIONGENERATED1234567890".to_string(),
        "completed",
        "Fallback title Bearer sk-TITLESECRET1234567890".to_string(),
        Some("Fallback description token=DESCRIPTIONSECRET".to_string()),
    )
    .await;

    assert_eq!(title, "Buddy update");
    assert_eq!(description.as_deref(), Some("Buddy has an update ready."));
    let rendered = format!("{} {}", title, description.unwrap_or_default());
    assert!(!rendered.contains("GENERATEDSECRET"));
    assert!(!rendered.contains("DESCRIPTIONGENERATED"));
    assert!(!rendered.contains("TITLESECRET"));
    assert!(!rendered.contains("DESCRIPTIONSECRET"));
    assert!(!rendered.contains("[REDACTED"));
}

#[tokio::test]
async fn oversized_fallback_buddy_speech_is_capped() {
    let (service, _renderer) = crate::buddy::voice_service::test_voice_service_with_responses(
        vec![Some("Bearer sk-FORCEFALLBACK1234567890".to_string())],
    );
    let _guard = crate::buddy::voice_service::install_test_voice_service(service).await;
    let app =
        crate::app_state::AppState::from_gcx(crate::global_context::tests::make_test_gcx().await)
            .await;

    let fallback = "f".repeat(400);
    let speech = super::actor::render_buddy_speech(
        app,
        super::types::BuddyPersonalityProfile::default(),
        "Pixel".to_string(),
        BuddyPulse::default(),
        Some("oversized_fallback_speech_test".to_string()),
        "workflow summary".to_string(),
        crate::buddy::voice_service::SpeechIntent::QuestAccept,
        fallback,
    )
    .await;

    assert_eq!(speech.text.chars().count(), 280);
    assert!(speech.text.chars().all(|c| c == 'f'));
}

#[tokio::test]
async fn runtime_event_safe_generated_title_keeps_safe_generated_description() {
    let generated = "Generated safe title";
    let (service, _renderer) = crate::buddy::voice_service::test_voice_service_with_responses(
        vec![Some(generated.to_string())],
    );
    let _guard = crate::buddy::voice_service::install_test_voice_service(service).await;
    let app =
        crate::app_state::AppState::from_gcx(crate::global_context::tests::make_test_gcx().await)
            .await;
    let workflow_summary = "Generated safe description".to_string();

    let (title, description) = super::actor::render_buddy_runtime_event(
        app,
        super::types::BuddyPersonalityProfile::default(),
        "Pixel".to_string(),
        BuddyPulse::default(),
        Some("safe_runtime_description_test".to_string()),
        workflow_summary.clone(),
        "completed",
        "Fallback title".to_string(),
        Some("Fallback description".to_string()),
    )
    .await;

    assert_eq!(title, generated);
    assert_eq!(description.as_deref(), Some(workflow_summary.as_str()));
}

#[tokio::test]
async fn runtime_event_unsafe_generated_title_ignores_safe_summary_without_fallback() {
    let (service, _renderer) = crate::buddy::voice_service::test_voice_service_with_responses(
        vec![Some("Runtime token=GENERATEDSECRET".to_string())],
    );
    let _guard = crate::buddy::voice_service::install_test_voice_service(service).await;
    let app =
        crate::app_state::AppState::from_gcx(crate::global_context::tests::make_test_gcx().await)
            .await;

    let (title, description) = super::actor::render_buddy_runtime_event(
        app,
        super::types::BuddyPersonalityProfile::default(),
        "Pixel".to_string(),
        BuddyPulse::default(),
        Some("unsafe_title_safe_summary_test".to_string()),
        "Safe workflow summary".to_string(),
        "completed",
        "Safe fallback title".to_string(),
        None,
    )
    .await;

    assert_eq!(title, "Safe fallback title");
    assert_eq!(description.as_deref(), Some("Buddy has an update ready."));
}

#[tokio::test]
async fn runtime_event_unsafe_generated_title_with_safe_fallbacks_uses_fallbacks() {
    let (service, _renderer) = crate::buddy::voice_service::test_voice_service_with_responses(
        vec![Some("Runtime token=GENERATEDSECRET".to_string())],
    );
    let _guard = crate::buddy::voice_service::install_test_voice_service(service).await;
    let app =
        crate::app_state::AppState::from_gcx(crate::global_context::tests::make_test_gcx().await)
            .await;

    let (title, description) = super::actor::render_buddy_runtime_event(
        app,
        super::types::BuddyPersonalityProfile::default(),
        "Pixel".to_string(),
        BuddyPulse::default(),
        Some("unsafe_title_safe_fallbacks_test".to_string()),
        "Safe workflow summary".to_string(),
        "completed",
        "Safe fallback title".to_string(),
        Some("Safe fallback description".to_string()),
    )
    .await;

    assert_eq!(title, "Safe fallback title");
    assert_eq!(description.as_deref(), Some("Safe fallback description"));
}

#[tokio::test]
async fn oversized_runtime_title_and_description_fallbacks_are_capped() {
    let (service, _renderer) = crate::buddy::voice_service::test_voice_service_with_responses(
        vec![Some("Bearer sk-FORCEFALLBACK1234567890".to_string())],
    );
    let _guard = crate::buddy::voice_service::install_test_voice_service(service).await;
    let app =
        crate::app_state::AppState::from_gcx(crate::global_context::tests::make_test_gcx().await)
            .await;

    let (title, description) = super::actor::render_buddy_runtime_event(
        app,
        super::types::BuddyPersonalityProfile::default(),
        "Pixel".to_string(),
        BuddyPulse::default(),
        Some("oversized_runtime_test".to_string()),
        "workflow summary".to_string(),
        "completed",
        "t".repeat(200),
        Some("d".repeat(700)),
    )
    .await;

    assert_eq!(title.chars().count(), 120);
    assert_eq!(description.unwrap().chars().count(), 500);
}
