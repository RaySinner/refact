use super::*;
use super::tests_common::*;
use crate::exec::types::DEFAULT_EXEC_OUTPUT_LIMIT_BYTES;
use crate::exec::{ExecMode, ExecOwnerMeta, ExecProcessId, ExecProcessMeta};
use crate::yaml_configs::customization_types::{ModeOverride, ModeThreadDefaults, ProjectRegistry};
use serde_json::json;
use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

/// The notice must not fire before the threshold, and must fire once past it.
///
/// `blocked_since` is back-dated rather than sleeping, so this asserts the
/// threshold itself without a 15-minute test.
#[test]
fn queue_wait_notice_waits_for_the_threshold() {
    let mut blocked_since = Some(
        std::time::Instant::now() - QUEUE_WAIT_NOTICE_AFTER + std::time::Duration::from_secs(1),
    );
    assert!(
        queue_wait_notice(&mut blocked_since, QueueWaitCause::Compression).is_none(),
        "a wait just under the threshold must stay silent"
    );

    blocked_since = Some(std::time::Instant::now() - QUEUE_WAIT_NOTICE_AFTER);
    assert!(
        queue_wait_notice(&mut blocked_since, QueueWaitCause::Compression).is_some(),
        "a wait past the threshold must surface a notice"
    );
}

/// An empty `blocked_since` means "first blocked iteration", which by
/// definition has not waited yet. This is what stops the notice from firing
/// the instant a chat goes busy.
#[test]
fn queue_wait_notice_is_silent_on_the_first_blocked_iteration() {
    let mut blocked_since: Option<std::time::Instant> = None;
    assert!(queue_wait_notice(&mut blocked_since, QueueWaitCause::Compression).is_none());
    assert!(
        blocked_since.is_some(),
        "the first blocked iteration must start the wait clock"
    );
}

/// The threshold is the engine's existing compaction deadline, not a second
/// magic number that could drift away from it.
#[test]
fn queue_wait_threshold_is_the_compression_staleness_constant() {
    assert_eq!(
        QUEUE_WAIT_NOTICE_AFTER,
        crate::chat::context_rebuild::COMPRESSION_ATTEMPT_STALE_AFTER
    );
}

/// A compression-caused wait must not read as ordinary queueing: the chat is
/// not progressing, so telling the user it is merely "queued" would be a lie.
#[test]
fn compression_queue_wait_is_distinguishable_from_ordinary_queueing() {
    let compression = QueueWaitCause::Compression.user_message();
    let ordinary = QueueWaitCause::OrdinaryQueueing.user_message();

    let compression_info = compression
        .extra
        .get("error_info")
        .and_then(|value| value.as_object())
        .expect("compression notice carries error_info");
    let ordinary_info = ordinary
        .extra
        .get("error_info")
        .and_then(|value| value.as_object())
        .expect("ordinary notice carries error_info");

    assert_ne!(
        compression_info.get("title").and_then(|v| v.as_str()),
        ordinary_info.get("title").and_then(|v| v.as_str()),
        "a compaction wait must not be titled as ordinary queueing"
    );
    assert!(
        compression_info
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .contains("compaction"),
        "the compression notice must name compaction"
    );
    assert_eq!(
        compression_info
            .get("is_retryable")
            .and_then(|v| v.as_bool()),
        Some(true),
        "a stalled compaction is transient, so the card must say retrying may help"
    );
    assert_eq!(
        ordinary_info.get("is_retryable").and_then(|v| v.as_bool()),
        Some(false),
        "ordinary queueing needs no retry prompt"
    );
}

/// The threshold boundary, driven from an injected duration rather than by
/// back-dating a clock.
///
/// Inclusive at the threshold: the first observation at or past the deadline
/// must notify. Asserting both sides of the boundary is what catches a `>` vs
/// `>=` slip, which would either delay the notice by one wake or fire it one
/// wake early.
#[test]
fn queue_wait_notice_threshold_boundary() {
    let second = std::time::Duration::from_secs(1);

    assert!(
        !should_notify(std::time::Duration::ZERO),
        "a wait that just started must stay silent"
    );
    assert!(
        !should_notify(QUEUE_WAIT_NOTICE_AFTER - second),
        "one second short of the deadline must stay silent"
    );
    assert!(
        should_notify(QUEUE_WAIT_NOTICE_AFTER),
        "the deadline itself must notify"
    );
    assert!(
        should_notify(QUEUE_WAIT_NOTICE_AFTER + second),
        "a wait past the deadline must notify"
    );
    assert!(
        should_notify(std::time::Duration::MAX),
        "an extreme elapsed must not overflow the comparison"
    );
}

/// The cause label is what distinguishes the two cards in `queue_wait.cause`,
/// so it must differ per cause — otherwise a compaction stall and an ordinary
/// long turn would collapse into one dedup entry.
#[test]
fn queue_wait_cause_as_str_is_distinct() {
    assert_ne!(
        QueueWaitCause::Compression.as_str(),
        QueueWaitCause::OrdinaryQueueing.as_str(),
        "the two causes must not share a label"
    );
    assert!(!QueueWaitCause::Compression.as_str().is_empty());
    assert!(!QueueWaitCause::OrdinaryQueueing.as_str().is_empty());
}

/// The card text is the dedup key, so it must stay byte-stable across waits.
/// An elapsed-time counter baked into it would defeat the collapsing and
/// re-append a card on every wake.
#[test]
fn queue_wait_notice_text_is_stable_so_dedup_can_collapse_it() {
    assert_eq!(
        QueueWaitCause::Compression.user_message().content,
        QueueWaitCause::Compression.user_message().content
    );
    assert_ne!(
        QueueWaitCause::Compression.user_message().content,
        QueueWaitCause::OrdinaryQueueing.user_message().content,
        "distinct causes must produce distinct texts so they do not collapse together"
    );
}

/// A chat blocked on a FRESH compression attempt with a user message queued must
/// stay quiet: the threshold has not elapsed, so a notice here would fire on
/// every ordinary wait and train the user to ignore the card.
///
/// The past-the-threshold half cannot be driven from here — `blocked_since` is
/// `std::time::Instant`, which `tokio::time::pause` does not advance — so that
/// half is covered by `queue_wait_notice_waits_for_the_threshold` instead.
#[tokio::test]
async fn compression_blocked_queue_stays_quiet_before_the_threshold() {
    let gcx = crate::global_context::tests::make_test_gcx().await;
    let app = AppState::from_gcx(gcx).await;
    let session_arc = Arc::new(AMutex::new(ChatSession::new("queue-wait".to_string())));
    app.chat
        .sessions
        .write()
        .await
        .insert("queue-wait".to_string(), session_arc.clone());

    let mut session = session_arc.lock().await;
    // Fresh attempt => genuinely blocking, and genuinely not yet stale.
    session.active_compression_attempt = Some(7);
    session.compression_attempt_started_at_ms =
        Some(chrono::Utc::now().timestamp_millis().max(0) as u64);
    session.compression_abort_flag = Some(Arc::new(AtomicBool::new(false)));
    assert!(
        crate::chat::context_rebuild::compression_attempt_active(&session),
        "the fixture must actually block, otherwise this test proves nothing"
    );
    assert_eq!(
        session.enqueue_accepted_command(CommandRequest {
            client_request_id: "waiting-1".to_string(),
            priority: false,
            command: user_message_command("do the thing", None),
        }),
        EnqueueCommandOutcome::Accepted
    );
    drop(session);

    let processor_running = Arc::new(AtomicBool::new(true));
    let handle = tokio::spawn(process_command_queue(
        app.clone(),
        session_arc.clone(),
        processor_running,
    ));

    wait_for_empty_lock(&session_arc).await;
    tokio::task::yield_now().await;

    let session = session_arc.lock().await;
    assert_eq!(
        session.command_queue.len(),
        1,
        "a compression-blocked queue must not consume the queued message"
    );
    assert_eq!(
        session
            .messages
            .iter()
            .filter(|message| message.role == "error")
            .count(),
        0,
        "no notice may appear before the threshold"
    );
    drop(session);

    close_processor(&session_arc, handle).await;
}

#[tokio::test(start_paused = true)]
async fn queue_notify_idle_processors_do_not_wake_without_notifications() {
    let gcx = crate::global_context::tests::make_test_gcx().await;
    let app = AppState::from_gcx(gcx).await;
    let session_arc = Arc::new(AMutex::new(ChatSession::new(
        "queue-notify-idle".to_string(),
    )));
    let processor_running = session_arc.lock().await.queue_processor_running.clone();
    processor_running.store(true, Ordering::SeqCst);
    let handle = tokio::spawn(process_command_queue(
        app,
        session_arc.clone(),
        processor_running,
    ));

    wait_for_empty_lock(&session_arc).await;
    let counters = session_arc.lock().await.queue_processor_counters.clone();
    let empty_locks = counters.empty_locks.load(Ordering::Relaxed);
    let notify_wakes = counters.notify_wakes.load(Ordering::Relaxed);

    tokio::time::advance(std::time::Duration::from_secs(3_600)).await;
    tokio::task::yield_now().await;

    assert_eq!(counters.empty_locks.load(Ordering::Relaxed), empty_locks);
    assert_eq!(counters.notify_wakes.load(Ordering::Relaxed), notify_wakes);
    assert_eq!(counters.timeout_wakes.load(Ordering::Relaxed), 0);

    close_processor(&session_arc, handle).await;
}

#[tokio::test]
async fn queue_notify_processes_commands_enqueued_before_and_after_wait_registration() {
    let gcx = crate::global_context::tests::make_test_gcx().await;
    let app = AppState::from_gcx(gcx.clone()).await;
    let before_wait = Arc::new(AMutex::new(ChatSession::new(
        "queue-notify-before".to_string(),
    )));
    let before_running = before_wait.lock().await.queue_processor_running.clone();
    {
        let mut session = before_wait.lock().await;
        assert_eq!(
            session.enqueue_accepted_command(abort_request("before-wait")),
            EnqueueCommandOutcome::Accepted
        );
    }
    before_running.store(true, Ordering::SeqCst);
    let before_handle = tokio::spawn(process_command_queue(
        app.clone(),
        before_wait.clone(),
        before_running,
    ));
    wait_for_queue_to_drain(&before_wait).await;
    close_processor(&before_wait, before_handle).await;

    let after_wait = Arc::new(AMutex::new(ChatSession::new(
        "queue-notify-after".to_string(),
    )));
    let after_running = after_wait.lock().await.queue_processor_running.clone();
    after_running.store(true, Ordering::SeqCst);
    let after_handle = tokio::spawn(process_command_queue(
        app,
        after_wait.clone(),
        after_running,
    ));
    wait_for_empty_lock(&after_wait).await;
    {
        let mut session = after_wait.lock().await;
        assert_eq!(
            session.enqueue_accepted_command(abort_request("after-wait")),
            EnqueueCommandOutcome::Accepted
        );
    }
    wait_for_queue_to_drain(&after_wait).await;
    let counters = after_wait.lock().await.queue_processor_counters.clone();
    assert!(counters.notify_wakes.load(Ordering::Relaxed) >= 1);
    assert_eq!(counters.timeout_wakes.load(Ordering::Relaxed), 0);
    close_processor(&after_wait, after_handle).await;
}

#[tokio::test]
async fn process_command_queue_restarts_after_enqueue_races_processor_teardown() {
    let gcx = crate::global_context::tests::make_test_gcx().await;
    let app = AppState::from_gcx(gcx).await;
    let session_arc = Arc::new(AMutex::new(ChatSession::new(
        "queue-notify-teardown".to_string(),
    )));
    let processor_running = session_arc.lock().await.queue_processor_running.clone();
    processor_running.store(true, Ordering::SeqCst);
    let handle = tokio::spawn(process_command_queue(
        app,
        session_arc.clone(),
        processor_running,
    ));
    wait_for_empty_lock(&session_arc).await;

    {
        let mut session = session_arc.lock().await;
        session
            .command_queue
            .push_back(abort_request("teardown-race"));
        session.queue_notify.notify_one();
        handle.abort();
    }
    let _ = handle.await;

    wait_for_queue_to_drain(&session_arc).await;
    let counters = session_arc.lock().await.queue_processor_counters.clone();
    assert!(counters.processor_starts.load(Ordering::Relaxed) >= 2);
    assert!(counters.processor_exits.load(Ordering::Relaxed) >= 1);
    let next_processor = session_arc.lock().await.queue_processor_running.clone();
    assert!(next_processor.load(Ordering::SeqCst));
    session_arc.lock().await.close_event_channel();
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while next_processor.load(Ordering::SeqCst) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("restarted queue processor did not exit after session close");
}

#[tokio::test]
async fn process_command_queue_wakes_for_paused_decision_enqueued_after_wait() {
    let gcx = crate::global_context::tests::make_test_gcx().await;
    let app = AppState::from_gcx(gcx).await;
    let session_arc = Arc::new(AMutex::new(ChatSession::new(
        "queue-notify-paused".to_string(),
    )));
    let processor_running = session_arc.lock().await.queue_processor_running.clone();
    {
        let mut session = session_arc.lock().await;
        session.runtime.pause_reasons.push(PauseReason {
            reason_type: "confirmation".to_string(),
            tool_name: "shell".to_string(),
            command: "shell".to_string(),
            rule: "ask".to_string(),
            tool_call_id: "paused-tool".to_string(),
            integr_config_path: None,
        });
        session.set_runtime_state(SessionState::Paused, None);
    }
    processor_running.store(true, Ordering::SeqCst);
    let handle = tokio::spawn(process_command_queue(
        app,
        session_arc.clone(),
        processor_running,
    ));

    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if session_arc
                .lock()
                .await
                .queue_processor_counters
                .empty_locks
                .load(Ordering::Relaxed)
                == 0
            {
                tokio::task::yield_now().await;
                continue;
            }
            return;
        }
    })
    .await
    .expect("paused processor did not wait for a notification");
    {
        let mut session = session_arc.lock().await;
        assert_eq!(
            session.enqueue_accepted_command(CommandRequest {
                client_request_id: "paused-decision".to_string(),
                priority: false,
                command: ChatCommand::ToolDecision {
                    tool_call_id: "paused-tool".to_string(),
                    accepted: false,
                },
            }),
            EnqueueCommandOutcome::Accepted
        );
    }
    wait_for_queue_to_drain(&session_arc).await;
    assert_eq!(session_arc.lock().await.runtime.state, SessionState::Idle);
    close_processor(&session_arc, handle).await;
}

#[tokio::test]
async fn process_command_queue_wakes_for_waiting_ide_callback_enqueued_after_wait() {
    let gcx = crate::global_context::tests::make_test_gcx().await;
    let app = AppState::from_gcx(gcx).await;
    let session_arc = Arc::new(AMutex::new(ChatSession::new(
        "queue-notify-waiting-ide".to_string(),
    )));
    let processor_running = session_arc.lock().await.queue_processor_running.clone();
    {
        let mut session = session_arc.lock().await;
        session.add_message(ChatMessage {
            role: "assistant".to_string(),
            tool_calls: Some(vec![crate::call_validation::ChatToolCall {
                id: "ide-tool".to_string(),
                index: None,
                tool_type: "function".to_string(),
                function: crate::call_validation::ChatToolFunction {
                    name: "ide_action".to_string(),
                    arguments: "{}".to_string(),
                },
                extra_content: None,
                started_at_ms: None,
                completed_at_ms: None,
            }]),
            ..Default::default()
        });
        session.set_runtime_state(SessionState::WaitingIde, None);
    }
    processor_running.store(true, Ordering::SeqCst);
    let handle = tokio::spawn(process_command_queue(
        app,
        session_arc.clone(),
        processor_running,
    ));

    tokio::task::yield_now().await;
    {
        let mut session = session_arc.lock().await;
        assert_eq!(
            session.enqueue_accepted_command(CommandRequest {
                client_request_id: "ide-result".to_string(),
                priority: false,
                command: ChatCommand::IdeToolResult {
                    tool_call_id: "ide-tool".to_string(),
                    content: "accepted".to_string(),
                    tool_failed: false,
                },
            }),
            EnqueueCommandOutcome::Accepted
        );
    }
    wait_for_queue_to_drain(&session_arc).await;
    assert!(session_arc
        .lock()
        .await
        .messages
        .iter()
        .any(|message| { message.role == "tool" && message.tool_call_id == "ide-tool" }));
    close_processor(&session_arc, handle).await;
}

#[tokio::test]
async fn queue_notify_rapid_enqueues_leave_no_stranded_commands() {
    const COMMAND_COUNT: usize = 10_000;

    let gcx = crate::global_context::tests::make_test_gcx().await;
    let app = AppState::from_gcx(gcx).await;
    let session_arc = Arc::new(AMutex::new(ChatSession::new(
        "queue-notify-rapid".to_string(),
    )));
    let processor_running = session_arc.lock().await.queue_processor_running.clone();
    {
        let mut session = session_arc.lock().await;
        for index in 0..COMMAND_COUNT {
            session.command_queue.push_back(CommandRequest {
                client_request_id: format!("rapid-{index}"),
                priority: false,
                command: ChatCommand::Regenerate {},
            });
        }
        session.user_interrupt_flag.store(true, Ordering::SeqCst);
    }
    processor_running.store(true, Ordering::SeqCst);
    let handle = tokio::spawn(process_command_queue(
        app,
        session_arc.clone(),
        processor_running,
    ));

    wait_for_queue_to_drain(&session_arc).await;
    assert_eq!(session_arc.lock().await.command_queue.len(), 0);
    handle.abort();
    let _ = handle.await;
}

#[test]
fn redundant_regenerates_coalesce_until_next_command_boundary() {
    let mut queue = VecDeque::from([
        CommandRequest {
            client_request_id: "regen-1".to_string(),
            priority: true,
            command: ChatCommand::Regenerate {},
        },
        CommandRequest {
            client_request_id: "regen-2".to_string(),
            priority: true,
            command: ChatCommand::Regenerate {},
        },
        CommandRequest {
            client_request_id: "user-1".to_string(),
            priority: false,
            command: ChatCommand::UserMessage {
                content: json!("keep ordering"),
                attachments: vec![],
                context_files: vec![],
                suppress_auto_enrichment: false,
                client_message_id: None,
                push: None,
            },
        },
        CommandRequest {
            client_request_id: "regen-3".to_string(),
            priority: false,
            command: ChatCommand::Regenerate {},
        },
    ]);

    let removed = drain_redundant_regenerates(&mut queue, true);

    assert_eq!(removed, vec!["regen-1", "regen-2"]);
    assert_eq!(queue.len(), 2);
    assert_eq!(queue[0].client_request_id, "user-1");
    assert_eq!(queue[1].client_request_id, "regen-3");
}

#[test]
fn mode_switch_diff_bounds_large_tool_delta() {
    let from = test_mode_config("from");
    let mut to = test_mode_config("to");
    to.tools = (0..100).map(|index| format!("tool-{index}")).collect();

    let diff = mode_switch_diff(&from, &to);
    let added = &diff["tools"]["added"];

    assert_eq!(added["count"], json!(100));
    assert_eq!(
        added["names"].as_array().unwrap().len(),
        MODE_SWITCH_TOOL_NAMES_LIMIT
    );
    assert_eq!(added["truncated"], json!(true));
    assert!(added["names"]
        .as_array()
        .unwrap()
        .contains(&json!("tool-0")));
}

#[tokio::test]
async fn mode_switch_diff_uses_resolved_overlay_defaults() {
    let workspace = tempfile::tempdir().unwrap();
    let gcx = crate::global_context::tests::make_test_gcx().await;
    *gcx.documents_state.workspace_folders.lock().unwrap() =
        vec![workspace.path().to_path_buf()];

    let mut explore = test_mode_config("explore");
    explore.prompt = "explore prompt".to_string();
    explore.thread_defaults = ModeThreadDefaults {
        auto_approve_editing_tools: Some(false),
        auto_approve_dangerous_commands: Some(false),
        ..Default::default()
    };
    let mut agent = test_mode_config("agent");
    agent.prompt = "agent prompt".to_string();
    agent.thread_defaults = ModeThreadDefaults {
        auto_approve_editing_tools: Some(true),
        auto_approve_dangerous_commands: Some(true),
        ..Default::default()
    };
    let mut overlay = test_mode_config("agent-for-test-model");
    overlay.base = Some("agent".to_string());
    overlay.match_models = Some(vec!["test/*".to_string()]);
    overlay.override_config = Some(ModeOverride {
        prompt: Some("overlay prompt".to_string()),
        ..Default::default()
    });

    gcx.project_registry_cache.write().unwrap().insert(
        workspace.path().to_path_buf(),
        ProjectRegistry {
            modes: HashMap::from([
                ("explore".to_string(), explore),
                ("agent".to_string(), agent),
            ]),
            mode_overrides: vec![overlay],
            ..Default::default()
        },
    );

    let app = AppState::from_gcx(gcx).await;
    let mut session = ChatSession::new("mode-switch-overlay".to_string());
    session.thread.mode = "explore".to_string();
    session.thread.model = "test/model".to_string();
    let old_mode = session.thread.mode.clone();
    session.thread.mode = "agent".to_string();

    assert!(
        add_mode_switch_event_and_plan_if_changed(
            app,
            &mut session,
            &old_mode,
            None,
            "chat.session",
        )
        .await
    );

    let payload = &session.messages.last().unwrap().extra["event"]["payload"];
    assert_eq!(payload["diff"]["resolved"], json!(true));
    assert_eq!(
        payload["diff"]["thread_defaults"]["auto_approve_editing_tools"]["to"],
        json!(true)
    );
    assert_eq!(
        payload["diff"]["thread_defaults"]["auto_approve_dangerous_commands"]["to"],
        json!(true)
    );
    assert_eq!(payload["diff"]["system_prompt"]["changed"], json!(true));
}

#[test]
fn test_find_allowed_command_empty_queue() {
    let queue = VecDeque::new();
    assert!(find_allowed_command_while_paused(&queue).is_none());
}

#[tokio::test]
async fn clean_background_processes_command_kills_chat_scoped() {
    let gcx = crate::global_context::tests::make_test_gcx().await;
    let app = AppState::from_gcx(gcx.clone()).await;
    let killed_background = register_running_process(
        &gcx,
        "exec_clean_chat_background",
        ExecMode::Background,
        "chat-clean",
    )
    .await;
    let killed_service = register_running_process(
        &gcx,
        "exec_clean_chat_service",
        ExecMode::Service,
        "chat-clean",
    )
    .await;
    let kept_other_chat = register_running_process(
        &gcx,
        "exec_clean_other_chat_background",
        ExecMode::Background,
        "chat-other",
    )
    .await;
    let kept_foreground = register_running_process(
        &gcx,
        "exec_clean_chat_foreground",
        ExecMode::Foreground,
        "chat-clean",
    )
    .await;
    let session_arc = Arc::new(AMutex::new(ChatSession::new("chat-clean".to_string())));
    let (processor_running, mut rx) = {
        let mut session = session_arc.lock().await;
        let rx = session.subscribe();
        session.command_queue.push_back(CommandRequest {
            client_request_id: "clean-background".to_string(),
            priority: false,
            command: ChatCommand::CleanBackgroundProcesses {
                include_services: true,
            },
        });
        session.emit_queue_update();
        (session.queue_processor_running.clone(), rx)
    };
    processor_running.store(true, Ordering::SeqCst);

    let handle = tokio::spawn(process_command_queue(
        app,
        session_arc.clone(),
        processor_running,
    ));
    let notice = wait_for_cleanup_notice(&mut rx).await;

    assert_eq!(
        notice.content.content_text_only(),
        "Cleared 2 background processes from this chat"
    );
    let event = notice.extra.get("event").unwrap();
    assert_eq!(event["subkind"], json!("system_notice"));
    assert_eq!(event["source"], json!("chat.session"));
    assert_eq!(event["payload"], json!({ "killed_count": 2 }));
    assert!(gcx.exec_registry.get(&killed_background).await.is_none());
    assert!(gcx.exec_registry.get(&killed_service).await.is_none());
    assert!(gcx.exec_registry.get(&kept_other_chat).await.is_some());
    assert!(gcx.exec_registry.get(&kept_foreground).await.is_some());

    {
        let mut session = session_arc.lock().await;
        session.close_event_channel();
        session.queue_notify.notify_waiters();
    }
    tokio::time::timeout(std::time::Duration::from_secs(3), handle)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn chat_started_pushed_on_first_user_message() {
    let gcx = crate::global_context::tests::make_test_gcx().await;
    let app = AppState::from_gcx(gcx.clone()).await;
    push_user_activity(
        app.clone(),
        UserAction::ChatStarted {
            chat_id: "chat-1".to_string(),
            first_user_text_preview: "hello from first user message".chars().take(80).collect(),
            ts: Utc::now(),
        },
    )
    .await;

    let user_activity = app.buddy.user_activity.clone();
    let ring = user_activity.lock().await;
    assert!(ring.snapshot().iter().any(|action| matches!(
        action,
        UserAction::ChatStarted { chat_id, first_user_text_preview, .. }
            if chat_id == "chat-1" && first_user_text_preview == "hello from first user message"
    )));
}

#[test]
fn test_find_allowed_command_no_allowed() {
    let mut queue = VecDeque::new();
    queue.push_back(make_request(ChatCommand::UserMessage {
        content: json!("hi"),
        attachments: vec![],
        context_files: vec![],
        suppress_auto_enrichment: false,
        client_message_id: None,
        push: None,
    }));
    queue.push_back(make_request(ChatCommand::SetParams {
        patch: json!({"model": "gpt-4"}),
    }));
    assert!(find_allowed_command_while_paused(&queue).is_none());
}

#[test]
fn test_find_allowed_command_finds_tool_decision() {
    let mut queue = VecDeque::new();
    queue.push_back(make_request(ChatCommand::UserMessage {
        content: json!("hi"),
        attachments: vec![],
        context_files: vec![],
        suppress_auto_enrichment: false,
        client_message_id: None,
        push: None,
    }));
    queue.push_back(make_request(ChatCommand::ToolDecision {
        tool_call_id: "tc1".into(),
        accepted: true,
    }));
    assert_eq!(find_allowed_command_while_paused(&queue), Some(1));
}

#[test]
fn test_find_allowed_command_finds_tool_decisions() {
    let mut queue = VecDeque::new();
    queue.push_back(make_request(ChatCommand::ToolDecisions {
        decisions: vec![ToolDecisionItem {
            tool_call_id: "tc1".into(),
            accepted: true,
        }],
    }));
    assert_eq!(find_allowed_command_while_paused(&queue), Some(0));
}

#[test]
fn test_find_allowed_command_finds_abort() {
    let mut queue = VecDeque::new();
    queue.push_back(make_request(ChatCommand::UserMessage {
        content: json!("hi"),
        attachments: vec![],
        context_files: vec![],
        suppress_auto_enrichment: false,
        client_message_id: None,
        push: None,
    }));
    queue.push_back(make_request(ChatCommand::UserMessage {
        content: json!("another"),
        attachments: vec![],
        context_files: vec![],
        suppress_auto_enrichment: false,
        client_message_id: None,
        push: None,
    }));
    queue.push_back(make_request(ChatCommand::Abort {}));
    assert_eq!(find_allowed_command_while_paused(&queue), Some(2));
}

#[test]
fn test_find_allowed_command_returns_first_match() {
    let mut queue = VecDeque::new();
    queue.push_back(make_request(ChatCommand::Abort {}));
    queue.push_back(make_request(ChatCommand::ToolDecision {
        tool_call_id: "tc1".into(),
        accepted: true,
    }));
    assert_eq!(find_allowed_command_while_paused(&queue), Some(0));
}

#[test]
fn test_apply_setparams_model() {
    let mut thread = ThreadParams::default();
    thread.model = "old-model".into();
    let patch = json!({"model": "new-model"});
    let (changed, _) = apply_setparams_patch(&mut thread, &patch);
    assert!(changed);
    assert_eq!(thread.model, "new-model");
}

#[test]
fn test_apply_setparams_no_change_same_value() {
    let mut thread = ThreadParams::default();
    thread.model = "gpt-4".into();
    let patch = json!({"model": "gpt-4"});
    let (changed, _) = apply_setparams_patch(&mut thread, &patch);
    assert!(!changed);
}

#[test]
fn test_apply_setparams_mode() {
    let mut thread = ThreadParams::default();
    let patch = json!({"mode": "NO_TOOLS"});
    let (changed, _) = apply_setparams_patch(&mut thread, &patch);
    assert!(changed);
    assert_eq!(thread.mode, "explore");
}

#[test]
fn test_apply_setparams_boost_reasoning() {
    let mut thread = ThreadParams::default();
    let patch = json!({"boost_reasoning": true});
    let (changed, _) = apply_setparams_patch(&mut thread, &patch);
    assert!(changed);
    assert_eq!(thread.boost_reasoning, Some(true));
}

#[test]
fn test_apply_setparams_tool_use() {
    let mut thread = ThreadParams::default();
    let patch = json!({"tool_use": "disabled"});
    let (changed, _) = apply_setparams_patch(&mut thread, &patch);
    assert!(changed);
    assert_eq!(thread.tool_use, "disabled");
}

#[test]
fn test_apply_setparams_context_tokens_cap() {
    let mut thread = ThreadParams::default();
    let patch = json!({"context_tokens_cap": 4096});
    let (changed, _) = apply_setparams_patch(&mut thread, &patch);
    assert!(changed);
    assert_eq!(thread.context_tokens_cap, Some(4096));
}

#[test]
fn test_apply_setparams_context_tokens_cap_null() {
    let mut thread = ThreadParams::default();
    thread.context_tokens_cap = Some(4096);
    let patch = json!({"context_tokens_cap": null});
    let (changed, _) = apply_setparams_patch(&mut thread, &patch);
    assert!(changed);
    assert!(thread.context_tokens_cap.is_none());
}

#[test]
fn test_apply_setparams_reset_auto_compression_cap_on_model_change() {
    let mut thread = ThreadParams::default();
    thread.model = "old-model".into();
    thread.auto_compression_cap = Some(4096);
    thread.auto_compression_cap_pending = false;

    let patch = serde_json::json!({"model": "new-model"});
    let (changed, sanitized) = apply_setparams_patch(&mut thread, &patch);

    assert!(changed);
    assert_eq!(thread.model, "new-model");
    assert_eq!(thread.auto_compression_cap, None);
    assert!(thread.auto_compression_cap_pending);
    assert_eq!(sanitized["auto_compression_cap"], serde_json::Value::Null);
}

#[test]
fn test_apply_setparams_reset_auto_compression_cap_on_context_cap_change() {
    let mut thread = ThreadParams::default();
    thread.context_tokens_cap = Some(8192);
    thread.auto_compression_cap = Some(4096);
    thread.auto_compression_cap_pending = false;

    let patch = serde_json::json!({"context_tokens_cap": 16384});
    let (changed, sanitized) = apply_setparams_patch(&mut thread, &patch);

    assert!(changed);
    assert_eq!(thread.context_tokens_cap, Some(16384));
    assert_eq!(thread.auto_compression_cap, None);
    assert!(thread.auto_compression_cap_pending);
    assert_eq!(sanitized["auto_compression_cap"], serde_json::Value::Null);
}

#[test]
fn test_apply_setparams_does_not_reset_auto_compression_cap_if_explicitly_provided() {
    let mut thread = ThreadParams::default();
    thread.model = "old-model".into();
    thread.auto_compression_cap = Some(4096);
    thread.auto_compression_cap_pending = false;

    // User explicitly changes model AND auto_compression_cap at the same time
    let patch = serde_json::json!({"model": "new-model", "auto_compression_cap": 8192});
    let (changed, sanitized) = apply_setparams_patch(&mut thread, &patch);

    assert!(changed);
    assert_eq!(thread.model, "new-model");
    assert_eq!(thread.auto_compression_cap, Some(8192));
    assert!(!thread.auto_compression_cap_pending);
    assert_eq!(sanitized["auto_compression_cap"], serde_json::json!(8192));
}

#[test]
fn test_apply_setparams_auto_compression_cap_set_null_and_invalid() {
    let mut thread = ThreadParams::default();
    let (changed, _) =
        apply_setparams_patch(&mut thread, &json!({"auto_compression_cap": 4096}));
    assert!(changed);
    assert_eq!(thread.auto_compression_cap, Some(4096));
    assert!(!thread.auto_compression_cap_pending);

    let (changed, _) =
        apply_setparams_patch(&mut thread, &json!({"auto_compression_cap": "invalid"}));
    assert!(!changed);
    assert_eq!(thread.auto_compression_cap, Some(4096));

    let (changed, _) =
        apply_setparams_patch(&mut thread, &json!({"auto_compression_cap": null}));
    assert!(changed);
    assert_eq!(thread.auto_compression_cap, None);
    assert!(thread.auto_compression_cap_pending);
    assert!(thread.resolve_pending_compression_cap(Some(1000)));
    assert_eq!(thread.auto_compression_cap, Some(900));
}

#[test]
fn test_apply_setparams_context_tokens_cap_invalid_type_ignored() {
    let mut thread = ThreadParams::default();
    thread.context_tokens_cap = Some(4096);
    let patch = json!({"context_tokens_cap": "invalid"});
    let (changed, _) = apply_setparams_patch(&mut thread, &patch);
    assert!(!changed);
    assert_eq!(thread.context_tokens_cap, Some(4096)); // Value preserved
}
#[test]
fn test_model_switch_invalidates_auto_compression_cap() {
    let mut thread = ThreadParams::default();
    thread.model = "old-model".into();
    thread.auto_compression_cap = Some(123_000);
    thread.auto_compression_cap_pending = false;

    let patch = json!({"model": "new-model"});
    let (changed, sanitized) = apply_setparams_patch(&mut thread, &patch);

    assert!(changed);
    assert_eq!(thread.model, "new-model");
    assert_eq!(thread.auto_compression_cap, None);
    assert!(thread.auto_compression_cap_pending);
    assert!(sanitized.get("auto_compression_cap").is_some());
    assert!(sanitized["auto_compression_cap"].is_null());
}

#[test]
fn test_context_cap_change_invalidates_auto_compression_cap() {
    let mut thread = ThreadParams::default();
    thread.model = "my-model".into();
    thread.context_tokens_cap = Some(200_000);
    thread.auto_compression_cap = Some(180_000);
    thread.auto_compression_cap_pending = false;

    let patch = json!({"context_tokens_cap": 500_000});
    let (changed, sanitized) = apply_setparams_patch(&mut thread, &patch);

    assert!(changed);
    assert_eq!(thread.context_tokens_cap, Some(500_000));
    assert_eq!(thread.auto_compression_cap, None);
    assert!(thread.auto_compression_cap_pending);
    assert!(sanitized["auto_compression_cap"].is_null());
}

#[test]
fn test_context_cap_null_invalidates_auto_compression_cap() {
    let mut thread = ThreadParams::default();
    thread.context_tokens_cap = Some(200_000);
    thread.auto_compression_cap = Some(180_000);
    thread.auto_compression_cap_pending = false;

    let patch = json!({"context_tokens_cap": null});
    let (changed, sanitized) = apply_setparams_patch(&mut thread, &patch);

    assert!(changed);
    assert!(thread.context_tokens_cap.is_none());
    assert_eq!(thread.auto_compression_cap, None);
    assert!(thread.auto_compression_cap_pending);
    assert!(sanitized["auto_compression_cap"].is_null());
}

#[test]
fn test_explicit_auto_compression_cap_preserves_value_on_model_switch() {
    let mut thread = ThreadParams::default();
    thread.model = "old-model".into();
    thread.auto_compression_cap = Some(100_000);
    thread.auto_compression_cap_pending = false;

    let patch = json!({"model": "new-model", "auto_compression_cap": 250_000});
    let (changed, sanitized) = apply_setparams_patch(&mut thread, &patch);

    assert!(changed);
    assert_eq!(thread.model, "new-model");
    assert_eq!(thread.auto_compression_cap, Some(250_000));
    assert!(!thread.auto_compression_cap_pending);
    assert_eq!(sanitized["auto_compression_cap"], json!(250_000));
}

#[test]
fn test_model_switch_without_prior_cap_sets_pending_auto_compression_cap() {
    let mut thread = ThreadParams::default();
    thread.model = "old-model".into();

    let patch = json!({"model": "new-model"});
    let (changed, sanitized) = apply_setparams_patch(&mut thread, &patch);

    assert!(changed);
    assert_eq!(thread.model, "new-model");
    assert_eq!(thread.context_tokens_cap, None);
    assert_eq!(thread.auto_compression_cap, None);
    assert!(thread.auto_compression_cap_pending);
    assert_eq!(sanitized["auto_compression_cap"], serde_json::Value::Null);
}


#[test]
fn test_apply_setparams_include_project_info() {
    let mut thread = ThreadParams::default();
    let patch = json!({"include_project_info": false});
    let (changed, _) = apply_setparams_patch(&mut thread, &patch);
    assert!(changed);
    assert!(!thread.include_project_info);
}

#[test]
fn test_apply_setparams_checkpoints_enabled() {
    let mut thread = ThreadParams::default();
    let patch = json!({"checkpoints_enabled": false});
    let (changed, _) = apply_setparams_patch(&mut thread, &patch);
    assert!(changed);
    assert!(!thread.checkpoints_enabled);
}

#[test]
fn test_apply_setparams_autonomous_no_confirm_persists_in_trajectory_snapshot() {
    let mut thread = ThreadParams::default();
    let (changed, sanitized) =
        apply_setparams_patch(&mut thread, &json!({"autonomous_no_confirm": true}));

    assert!(changed);
    assert!(thread.autonomous_no_confirm);
    assert_eq!(sanitized["autonomous_no_confirm"], json!(true));

    let snapshot =
        refact_chat_history::trajectory_snapshot::TrajectorySnapshot::from_thread_parts(
            "autonomous-no-confirm".to_string(),
            &thread,
            Vec::new(),
            "2026-08-24T00:00:00Z".to_string(),
            1,
        );
    let encoded = serde_json::to_value(snapshot).unwrap();
    let decoded: refact_chat_history::trajectory_snapshot::TrajectorySnapshot =
        serde_json::from_value(encoded).unwrap();
    assert!(decoded.autonomous_no_confirm);

    let (changed, _) =
        apply_setparams_patch(&mut thread, &json!({"autonomous_no_confirm": "invalid"}));
    assert!(!changed);
    assert!(thread.autonomous_no_confirm);
}

#[test]
fn test_apply_setparams_auto_compact_enabled_persists_in_trajectory_snapshot() {
    let mut thread = ThreadParams::default();
    let (changed, sanitized) =
        apply_setparams_patch(&mut thread, &json!({"auto_compact_enabled": true}));

    assert!(changed);
    assert_eq!(thread.auto_compact_enabled, Some(true));
    assert_eq!(sanitized["auto_compact_enabled"], json!(true));

    let snapshot =
        refact_chat_history::trajectory_snapshot::TrajectorySnapshot::from_thread_parts(
            "auto-compact-enabled".to_string(),
            &thread,
            Vec::new(),
            "2026-08-24T00:00:00Z".to_string(),
            1,
        );
    let encoded = serde_json::to_value(snapshot).unwrap();
    let decoded: refact_chat_history::trajectory_snapshot::TrajectorySnapshot =
        serde_json::from_value(encoded).unwrap();
    assert_eq!(decoded.auto_compact_enabled, Some(true));

    let (changed, _) =
        apply_setparams_patch(&mut thread, &json!({"auto_compact_enabled": "invalid"}));
    assert!(!changed);
    assert_eq!(thread.auto_compact_enabled, Some(true));

    let (changed, _) =
        apply_setparams_patch(&mut thread, &json!({"auto_compact_enabled": null}));
    assert!(changed);
    assert_eq!(thread.auto_compact_enabled, None);
}

#[test]
fn test_apply_setparams_multiple_fields() {
    let mut thread = ThreadParams::default();
    let patch = json!({
        "model": "claude-3",
        "mode": "EXPLORE",
        "boost_reasoning": true,
    });
    let (changed, _) = apply_setparams_patch(&mut thread, &patch);
    assert!(changed);
    assert_eq!(thread.model, "claude-3");
    assert_eq!(thread.mode, "explore");
    assert_eq!(thread.boost_reasoning, Some(true));
}

#[test]
fn test_apply_setparams_mode_canonicalizes_task_agent() {
    let mut thread = ThreadParams::default();
    let patch = json!({"mode": "TASK_AGENT"});
    let (changed, _) = apply_setparams_patch(&mut thread, &patch);
    assert!(changed);
    assert_eq!(thread.mode, "task_agent");
}

#[test]
fn test_apply_setparams_sanitizes_patch() {
    let mut thread = ThreadParams::default();
    let patch = json!({
        "model": "gpt-4",
        "type": "set_params",
        "chat_id": "chat-123",
        "seq": "42"
    });
    let (_, sanitized) = apply_setparams_patch(&mut thread, &patch);
    assert!(sanitized.get("type").is_none());
    assert!(sanitized.get("chat_id").is_none());
    assert!(sanitized.get("seq").is_none());
    assert!(sanitized.get("model").is_some());
}

#[test]
fn test_apply_setparams_empty_patch() {
    let mut thread = ThreadParams::default();
    let original_model = thread.model.clone();
    let patch = json!({});
    let (changed, _) = apply_setparams_patch(&mut thread, &patch);
    assert!(!changed);
    assert_eq!(thread.model, original_model);
}

#[test]
fn test_apply_setparams_invalid_types_ignored() {
    let mut thread = ThreadParams::default();
    thread.model = "original".into();
    let patch = json!({
        "model": 123,
        "boost_reasoning": "not_a_bool",
    });
    let (changed, _) = apply_setparams_patch(&mut thread, &patch);
    assert!(!changed);
    assert_eq!(thread.model, "original");
}

#[test]
fn trajectory_worktree_apply_setparams_detaches_on_null() {
    let mut thread = ThreadParams::default();
    thread.worktree = Some(crate::worktrees::types::WorktreeMeta {
        id: "wt-1".to_string(),
        kind: "task_agent".to_string(),
        root: std::path::PathBuf::from("/tmp/wt"),
        source_workspace_root: std::path::PathBuf::from("/tmp/src"),
        repo_root: std::path::PathBuf::from("/tmp/src"),
        branch: Some("branch".to_string()),
        base_branch: None,
        base_commit: None,
        task_id: None,
        card_id: None,
        agent_id: None,
        enforce: true,
    });
    let patch = json!({"worktree": null});
    let (changed, sanitized) = apply_setparams_patch(&mut thread, &patch);
    assert!(changed);
    assert!(thread.worktree.is_none());
    assert!(sanitized.get("worktree").unwrap().is_null());
}

#[test]
fn trajectory_worktree_apply_setparams_ignores_attach_object() {
    let mut thread = ThreadParams::default();
    let patch = json!({"worktree": {"root": "/tmp/untrusted"}});
    let (changed, sanitized) = apply_setparams_patch(&mut thread, &patch);
    assert!(!changed);
    assert!(thread.worktree.is_none());
    assert!(sanitized.get("worktree").is_none());
}

#[test]
fn worktree_scope_messages_announce_attach_and_detach() {
    let worktree = sample_worktree("wt-msg");

    let enabled = worktree_activation_message(&worktree);
    let disabled = worktree_disabled_message(Some(&worktree));

    assert_eq!(enabled.role, "cd_instruction");
    assert_eq!(enabled.tool_call_id, "worktree_enabled");
    assert!(enabled
        .content
        .content_text_only()
        .contains("WORKTREE_ENABLED"));
    assert!(enabled
        .content
        .content_text_only()
        .contains("Worktree root"));
    assert_eq!(disabled.role, "cd_instruction");
    assert_eq!(disabled.tool_call_id, "worktree_disabled");
    assert!(disabled
        .content
        .content_text_only()
        .contains("WORKTREE_DISABLED"));
    assert!(disabled
        .content
        .content_text_only()
        .contains("main workspace"));
    assert!(disabled.content.content_text_only().contains("wt-msg"));
}

#[tokio::test]
async fn worktree_setparams_attach_by_id_resolves_registry_and_scopes_create() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("repo");
    let cache = temp.path().join("cache");
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
    let service = WorktreeService::new(cache, source.clone()).unwrap();
    let created = service
        .create_worktree(crate::worktrees::types::CreateWorktreeRequest {
            branch: Some("refact/chat/attach-id".to_string()),
            kind: Some("chat".to_string()),
            ..Default::default()
        })
        .await
        .unwrap();
    let mut thread = ThreadParams::default();
    thread.id = "chat-attach".to_string();
    let update = resolve_worktree_setparams_update(
        app,
        "chat-attach",
        &thread,
        &json!({"worktree_id": created.worktree.meta.id.clone()}),
    )
    .await
    .unwrap()
    .unwrap();
    thread.worktree = update.worktree;
    let scope = crate::worktrees::scope::ExecutionScope::from_thread(&thread).unwrap();
    let resolved = scope
        .resolve_creatable_path(Path::new("nested/file.rs"))
        .unwrap();
    assert!(resolved.path.starts_with(&created.worktree.meta.root));
    assert!(!resolved.path.starts_with(&source));
    let registry = service.load_registry().await.unwrap();
    assert_eq!(registry.records[0].references.len(), 1);
    assert_eq!(
        registry.records[0].references[0].chat_id.as_deref(),
        Some("chat-attach")
    );
}

#[tokio::test]
async fn worktree_setparams_same_id_skips_registry_lookup() {
    let gcx = crate::global_context::tests::make_test_gcx().await;
    let app = AppState::from_gcx(gcx).await;
    let mut thread = ThreadParams::default();
    thread.worktree = Some(sample_worktree("wt-existing"));

    let update = resolve_worktree_setparams_update(
        app,
        "chat-existing",
        &thread,
        &json!({"worktree_id": "wt-existing"}),
    )
    .await
    .unwrap();

    assert!(update.is_none());
}

#[tokio::test]
async fn worktree_setparams_detach_clears_registry_reference() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("repo");
    let cache = temp.path().join("cache");
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
    let service = WorktreeService::new(cache, source.clone()).unwrap();
    let created = service
        .create_worktree(crate::worktrees::types::CreateWorktreeRequest {
            branch: Some("refact/chat/detach-id".to_string()),
            chat_id: Some("chat-detach".to_string()),
            kind: Some("chat".to_string()),
            ..Default::default()
        })
        .await
        .unwrap();
    let mut thread = ThreadParams::default();
    thread.worktree = Some(created.worktree.meta.clone());
    let update = resolve_worktree_setparams_update(
        app,
        "chat-detach",
        &thread,
        &json!({"worktree": null}),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(update.worktree.is_none());
    let registry = service.load_registry().await.unwrap();
    assert!(registry.records[0].references.is_empty());
}

#[tokio::test]
async fn worktree_branch_reference_preserves_scope_for_new_chat() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("repo");
    let cache = temp.path().join("cache");
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
    let service = WorktreeService::new(cache, source).unwrap();
    let created = service
        .create_worktree(crate::worktrees::types::CreateWorktreeRequest {
            branch: Some("refact/chat/branch-preserve".to_string()),
            chat_id: Some("source-chat".to_string()),
            kind: Some("chat".to_string()),
            ..Default::default()
        })
        .await
        .unwrap();

    let mut target = ThreadParams::default();
    target.id = "target-chat".to_string();
    let validated =
        add_thread_worktree_reference(app, "target-chat", &target, &created.worktree.meta)
            .await
            .unwrap();

    assert_eq!(validated.id, created.worktree.meta.id);
    let registry = service.load_registry().await.unwrap();
    let record = &registry.records[0];
    assert!(record.references.iter().any(|reference| {
        reference.kind == "chat" && reference.chat_id.as_deref() == Some("source-chat")
    }));
    assert!(record.references.iter().any(|reference| {
        reference.kind == "chat" && reference.chat_id.as_deref() == Some("target-chat")
    }));
}
