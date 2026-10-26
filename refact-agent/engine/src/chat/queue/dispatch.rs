use super::*;

/// A queued message has been waiting longer than
/// `COMPRESSION_ATTEMPT_STALE_AFTER`, so the user is told instead of being left
/// staring at a queue badge.
///
/// The threshold is the compression staleness constant on purpose: it is the
/// same "this should have finished by now" line the engine already draws for a
/// wedged compaction, so the queue cannot invent a second, competing deadline.
pub(crate) const QUEUE_WAIT_NOTICE_AFTER: std::time::Duration =
    crate::chat::context_rebuild::COMPRESSION_ATTEMPT_STALE_AFTER;

/// Whether a long queue wait is caused by compression, which must read
/// differently from ordinary queueing: "queued" would be a lie while the chat is
/// actually blocked on a rebuild.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum QueueWaitCause {
    /// Blocked on context compaction. The chat is not progressing for this
    /// reason at all, so this must not read as normal queueing.
    Compression,
    /// Blocked on a running generation turn, tool calls, or another blocker.
    OrdinaryQueueing,
}

impl QueueWaitCause {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Compression => "context compaction",
            Self::OrdinaryQueueing => "a previous turn",
        }
    }

    /// The user-facing card.
    ///
    /// The text is deliberately STABLE for a given cause: dedup collapses on the
    /// message content, so baking an elapsed-time counter into it would defeat
    /// the dedup and re-append on every wake. The elapsed age goes in `extra`
    /// instead, which dedup ignores.
    pub(crate) fn user_message(self) -> ChatMessage {
        let (category, title, explanation, suggested_action, is_retryable) = match self {
            Self::Compression => (
                "ProviderTransient",
                "Message stuck behind context compaction",
                "Your message is still in the chat and has not been processed. Context \
compaction has been blocking this chat for a long time, so it is not running \
forward on its own. Wait for the compaction to report a result, then send the \
message again.",
                "none",
                true,
            ),
            Self::OrdinaryQueueing => (
                "InvalidRequest",
                "Message waiting behind a long-running step",
                "Your message is still in the chat and has not been processed. A previous \
step has been running for a long time and is still blocking this chat. It \
runs as soon as that step finishes.",
                "none",
                false,
            ),
        };

        let mut message = crate::chat::diagnostics::make_ui_only_error_message_with_info(
            self.as_str(),
            category,
            title,
            explanation,
            suggested_action,
            is_retryable,
        );
        message.extra.insert(
            "queue_wait".to_string(),
            serde_json::json!({
                "source": "chat.queue.process_command_queue",
                "cause": self.as_str(),
            }),
        );
        message
    }
}

/// Decide whether to surface a long-wait notice, given how long this loop has
/// been unable to dequeue.
///
/// Called while holding the session lock, inside the processor's own decision
/// block — this adds no timer and no polling, it only observes the loop's own
/// blocked iterations.
///
/// `blocked_since` is processor-local on purpose: an enqueue timestamp would
/// have to live on the session and is only recorded when perf diagnostics are
/// on, so it cannot be relied on for a user-facing notice.
///
/// The threshold comparison is split into the pure `should_notify` so the boundary
/// is testable from an injected duration. Driving it through `Instant::elapsed()`
/// would force a test to either sleep for 15 minutes or back-date a clock it does
/// not control, and `tokio::time::pause` does not move `std::time::Instant`.
pub(crate) fn queue_wait_notice(
    blocked_since: &mut Option<std::time::Instant>,
    cause: QueueWaitCause,
) -> Option<ChatMessage> {
    let since = *blocked_since.get_or_insert_with(std::time::Instant::now);
    if !should_notify(since.elapsed()) {
        return None;
    }
    Some(cause.user_message())
}

/// Whether a wait of `elapsed` has passed long enough to be worth surfacing.
///
/// Inclusive at the threshold: a wait of exactly `QUEUE_WAIT_NOTICE_AFTER` counts,
/// so the notice fires on the first observation at or past the deadline.
pub(crate) fn should_notify(elapsed: std::time::Duration) -> bool {
    elapsed >= QUEUE_WAIT_NOTICE_AFTER
}

pub fn process_command_queue(
    app: AppState,
    session_arc: Arc<AMutex<ChatSession>>,
    processor_running: Arc<AtomicBool>,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> {
    Box::pin(process_command_queue_inner(
        app,
        session_arc,
        processor_running,
    ))
}

pub(crate) async fn process_command_queue_inner(
    app: AppState,
    session_arc: Arc<AMutex<ChatSession>>,
    processor_running: Arc<AtomicBool>,
) {
    struct ProcessorGuard {
        app: AppState,
        processor_running: Arc<AtomicBool>,
        session_arc: Arc<AMutex<ChatSession>>,
    }

    impl Drop for ProcessorGuard {
        fn drop(&mut self) {
            let app = self.app.clone();
            let processor_running = self.processor_running.clone();
            let session_arc = self.session_arc.clone();
            tokio::spawn(async move {
                let restart = {
                    let session = session_arc.lock().await;
                    session
                        .queue_processor_counters
                        .processor_exits
                        .fetch_add(1, Ordering::Relaxed);
                    processor_running.store(false, Ordering::SeqCst);
                    !session.closed && !session.command_queue.is_empty()
                };
                if restart {
                    if !processor_running.swap(true, Ordering::SeqCst) {
                        tokio::spawn(process_command_queue(app, session_arc, processor_running));
                    }
                }
            });
        }
    }
    let _guard = ProcessorGuard {
        app: app.clone(),
        processor_running,
        session_arc: session_arc.clone(),
    };

    {
        let session = session_arc.lock().await;
        session
            .queue_processor_counters
            .processor_starts
            .fetch_add(1, Ordering::Relaxed);
    }

    // When this processor first found itself unable to dequeue while user work
    // was pending. Processor-local, so it needs no session field and no timer.
    let mut blocked_since: Option<std::time::Instant> = None;

    loop {
        let (command, waiter) = {
            let mut session = session_arc.lock().await;

            if session.closed {
                return;
            }

            let mut waiter = Box::pin(session.queue_notify.clone().notified_owned());
            let _ = Pin::as_mut(&mut waiter).enable();

            let state = session.runtime.state;
            let compression_active =
                crate::chat::context_rebuild::compression_attempt_active(&session);
            let is_busy = state == SessionState::Generating
                || state == SessionState::ExecutingTools
                || (state == SessionState::Starting && session.command_queue.is_empty())
                || session.turn_depth > 0
                || compression_active;

            if is_busy {
                session
                    .queue_processor_counters
                    .empty_locks
                    .fetch_add(1, Ordering::Relaxed);
                // Only a chat actually holding waiting user work is worth a
                // notice; an idle chat awaiting its next message is not.
                let cause = if compression_active {
                    QueueWaitCause::Compression
                } else {
                    QueueWaitCause::OrdinaryQueueing
                };
                if session.command_queue.is_empty() {
                    blocked_since = None;
                } else if let Some(message) = queue_wait_notice(&mut blocked_since, cause) {
                    session.append_error_message_deduped_content(message);
                }
                (None, Some(waiter))
            } else if state == SessionState::WaitingIde {
                // Not blocked: the wait clock restarts, so a chat that recovered
                // and later blocked again does not surface a stale notice.
                blocked_since = None;
                if let Some(idx) = find_allowed_command_while_waiting_ide(&session.command_queue) {
                    let cmd = session.command_queue.remove(idx);
                    if let Some(request) = cmd.as_ref() {
                        session.record_command_queue_wait(&request.client_request_id);
                    }
                    session.emit_queue_update();
                    (cmd, None)
                } else {
                    session
                        .queue_processor_counters
                        .empty_locks
                        .fetch_add(1, Ordering::Relaxed);
                    (None, Some(waiter))
                }
            } else if state == SessionState::Paused {
                blocked_since = None;
                if let Some(idx) = find_allowed_command_while_paused(&session.command_queue) {
                    let cmd = session.command_queue.remove(idx);
                    if let Some(request) = cmd.as_ref() {
                        session.record_command_queue_wait(&request.client_request_id);
                    }
                    session.emit_queue_update();
                    (cmd, None)
                } else {
                    session
                        .queue_processor_counters
                        .empty_locks
                        .fetch_add(1, Ordering::Relaxed);
                    (None, Some(waiter))
                }
            } else if !session.delivery_wake_sources.is_empty() {
                blocked_since = None;
                session.delivery_wake_sources.clear();
                session.abort_flag.store(false, Ordering::SeqCst);
                session.user_interrupt_flag.store(false, Ordering::SeqCst);
                session.set_runtime_state(SessionState::Idle, None);
                (
                    Some(CommandRequest {
                        client_request_id: format!("delivery-wake-{}", uuid::Uuid::new_v4()),
                        priority: false,
                        command: ChatCommand::Regenerate {},
                    }),
                    None,
                )
            } else if session.command_queue.is_empty() {
                blocked_since = None;
                session
                    .queue_processor_counters
                    .empty_locks
                    .fetch_add(1, Ordering::Relaxed);
                (None, Some(waiter))
            } else {
                blocked_since = None;
                let cmd = session.command_queue.pop_front();
                if let Some(ref req) = cmd {
                    session.record_command_queue_wait(&req.client_request_id);
                    if matches!(req.command, ChatCommand::Regenerate {}) {
                        for request_id in
                            drain_redundant_regenerates(&mut session.command_queue, req.priority)
                        {
                            session.record_command_queue_wait(&request_id);
                        }
                    }
                    if command_triggers_generation(&req.command) {
                        session.set_runtime_state(SessionState::Idle, None);
                    }
                }
                session.emit_queue_update();
                (cmd, None)
            }
        };

        if let Some(waiter) = waiter {
            waiter.await;
            let session = session_arc.lock().await;
            session
                .queue_processor_counters
                .notify_wakes
                .fetch_add(1, Ordering::Relaxed);
            continue;
        }

        let Some(request) = command else {
            continue;
        };

        match request.command {
            ChatCommand::SetGoal {
                content,
                budget,
                criteria,
            } => {
                let result = {
                    let mut session = session_arc.lock().await;
                    handle_set_goal_command(&mut session, content, budget, criteria)
                };
                match result {
                    Ok(_) => {
                        maybe_save_trajectory_with_intent(
                            app.clone(),
                            session_arc.clone(),
                            TrajectoryCommitIntent::Required,
                        )
                        .await
                    }
                    Err(error) => {
                        warn!("SetGoal command rejected: {}", error);
                        let mut session = session_arc.lock().await;
                        emit_goal_command_error(&mut session, error);
                    }
                }
            }
            ChatCommand::SetGoalBudget { budget } => {
                let result = {
                    let mut session = session_arc.lock().await;
                    handle_set_goal_budget_command(&mut session, budget)
                };
                match result {
                    Ok(_) => {
                        maybe_save_trajectory_with_intent(
                            app.clone(),
                            session_arc.clone(),
                            TrajectoryCommitIntent::Required,
                        )
                        .await
                    }
                    Err(error) => {
                        warn!("SetGoalBudget command rejected: {}", error);
                        let mut session = session_arc.lock().await;
                        emit_goal_command_error(&mut session, error);
                    }
                }
            }
            ChatCommand::UpdateGoal { note } => {
                let result = {
                    let mut session = session_arc.lock().await;
                    handle_update_goal_command(&mut session, note)
                };
                match result {
                    Ok(_) => {
                        maybe_save_trajectory_with_intent(
                            app.clone(),
                            session_arc.clone(),
                            TrajectoryCommitIntent::Required,
                        )
                        .await
                    }
                    Err(error) => {
                        warn!("UpdateGoal command rejected: {}", error);
                        let mut session = session_arc.lock().await;
                        emit_goal_command_error(&mut session, error);
                    }
                }
            }
            ChatCommand::GoalControl { action } => {
                let result = {
                    let mut session = session_arc.lock().await;
                    handle_goal_control_command(&mut session, action)
                };
                match result {
                    Ok(_) => {
                        maybe_save_trajectory_with_intent(
                            app.clone(),
                            session_arc.clone(),
                            TrajectoryCommitIntent::Required,
                        )
                        .await
                    }
                    Err(error) => {
                        warn!("GoalControl command rejected: {}", error);
                        let mut session = session_arc.lock().await;
                        emit_goal_command_error(&mut session, error);
                    }
                }
            }
            ChatCommand::UserMessage {
                mut content,
                attachments,
                context_files,
                suppress_auto_enrichment,
                client_message_id,
                push: _,
            } => {
                // Keep preparation's stack temporaries out of the generation poll frame.
                let prepared = Box::pin(async {
                    let mut skill_activation_info = None;
                    if let Some(text) = content.as_str() {
                        match expand_slash_command(app.clone(), text).await {
                            Ok(Some(expanded)) => {
                                skill_activation_info = expanded.skill_to_activate;
                                content = serde_json::Value::String(expanded.expanded_text);
                                let mut session = session_arc.lock().await;
                                session.active_command = ActiveCommandContext {
                                    name: expanded.source_command,
                                    allowed_tools: expanded.allowed_tools,
                                    model_override: expanded.model_override,
                                    context_fork: expanded.context_fork,
                                    started_at_index: None,
                                    activation_tool_call_id: None,
                                };
                            }
                            Ok(None) => {
                                // No slash command — only reset active_command when no skill is
                                // active. While a skill is active, active_command carries the
                                // compaction anchor (started_at_index) and must not be wiped on
                                // every normal user message.
                                let mut session = session_arc.lock().await;
                                if session.thread.active_skill.is_none() {
                                    session.active_command = ActiveCommandContext::default();
                                }
                            }
                            Err(e) => {
                                warn!("slash command expansion error: {}", e);
                                let mut session = session_arc.lock().await;
                                if session.thread.active_skill.is_none() {
                                    session.active_command = ActiveCommandContext::default();
                                }
                            }
                        }
                    }

                    let skill_activation_name: Option<String> =
                        skill_activation_info.as_ref().map(|i| i.name.clone());
                    let skill_context_msg = if let Some(info) = skill_activation_info {
                        let body = expand_skill_includes(&info.body, &info.skill_dir).await;
                        let line_count = body.lines().count().max(1);
                        Some(ChatMessage {
                            message_id: Uuid::new_v4().to_string(),
                            role: "context_file".to_string(),
                            content: ChatContent::ContextFiles(vec![ContextFile {
                                file_name: format!("skill://{}", info.name),
                                file_content: body,
                                line1: 1,
                                line2: line_count,
                                file_rev: None,
                                symbols: vec![],
                                gradient_type: 0,
                                usefulness: 95.0,
                                skip_pp: true,
                            }]),
                            tool_call_id: SKILLS_CONTEXT_MARKER.to_string(),
                            ..Default::default()
                        })
                    } else {
                        None
                    };

                    let additional_messages = if !request.priority {
                        let mut session = session_arc.lock().await;
                        let msgs = drain_non_priority_user_messages(&mut session.command_queue);
                        if !msgs.is_empty() {
                            for message in &msgs {
                                session.record_command_queue_wait(&message.client_request_id);
                            }
                            session.emit_queue_update();
                        }
                        msgs
                    } else {
                        Vec::new()
                    };

                    let (checkpoints_enabled, chat_id, latest_checkpoint, worktree) = {
                        let session = session_arc.lock().await;
                        (
                            session.thread.checkpoints_enabled,
                            session.chat_id.clone(),
                            find_latest_checkpoint(&session),
                            session.thread.worktree.clone(),
                        )
                    };

                    let checkpoints = if checkpoints_enabled {
                        create_checkpoint_async(
                            app.clone(),
                            latest_checkpoint.as_ref(),
                            &chat_id,
                            worktree.as_ref(),
                        )
                        .await
                    } else {
                        Vec::new()
                    };

                    let (has_browser_meta, attach_screenshot_on_send, browser_chat_id) = {
                        let session = session_arc.lock().await;
                        let bm = session.thread.browser_meta.as_ref();
                        (
                            bm.is_some(),
                            bm.map_or(false, |m| m.attach_screenshot_on_send),
                            session.chat_id.clone(),
                        )
                    };

                    let browser_ctx_result = browser_context::maybe_insert_browser_context(
                        app.gcx.clone(),
                        &browser_chat_id,
                        has_browser_meta,
                        attach_screenshot_on_send,
                    )
                    .await;

                    let (session_id_for_hook, project_dir_for_hook) = {
                        let session = session_arc.lock().await;
                        let sid = session.chat_id.clone();
                        drop(session);
                        let pd = get_project_dir_string(app.clone()).await;
                        (sid, pd)
                    };
                    let prompt_text = match &content {
                        serde_json::Value::String(s) => s.clone(),
                        other => serde_json::to_string(other).unwrap_or_default(),
                    };
                    let prompt_payload = HookPayload {
                        hook_event_name: "UserPromptSubmit".to_string(),
                        session_id: session_id_for_hook.clone(),
                        project_dir: project_dir_for_hook.clone(),
                        tool_name: None,
                        tool_input: None,
                        tool_output: None,
                        user_prompt: Some(prompt_text),
                        extra: std::collections::HashMap::new(),
                    };
                    let prompt_results =
                        run_hooks(app.clone(), HookEvent::UserPromptSubmit, prompt_payload).await;
                    if let Some(reason) = first_block_reason(&prompt_results) {
                        let mut session = session_arc.lock().await;
                        let compression_phase = session.compression_phase;
                        let compression_reason = session.compression_reason;
                        let event = session.runtime_update_event(
                            crate::chat::types::SessionState::Error,
                            Some(format!("Message blocked by hook: {}", reason)),
                            false,
                            compression_phase,
                            compression_reason,
                        );
                        session.emit(event);
                        session.set_runtime_state(crate::chat::types::SessionState::Idle, None);
                        return false;
                    }

                    let additional_messages = {
                        let mut approved = Vec::new();
                        for additional in additional_messages {
                            let text = if let ChatCommand::UserMessage { ref content, .. } =
                                additional.command
                            {
                                match content {
                                    serde_json::Value::String(s) => s.clone(),
                                    other => serde_json::to_string(other).unwrap_or_default(),
                                }
                            } else {
                                approved.push(additional);
                                continue;
                            };
                            let add_results = run_hooks(
                                app.clone(),
                                HookEvent::UserPromptSubmit,
                                HookPayload {
                                    hook_event_name: "UserPromptSubmit".to_string(),
                                    session_id: session_id_for_hook.clone(),
                                    project_dir: project_dir_for_hook.clone(),
                                    tool_name: None,
                                    tool_input: None,
                                    tool_output: None,
                                    user_prompt: Some(text),
                                    extra: std::collections::HashMap::new(),
                                },
                            )
                            .await;
                            if first_block_reason(&add_results).is_none() {
                                approved.push(additional);
                            }
                        }
                        approved
                    };

                    let is_oversize = browser_ctx_result
                        .as_ref()
                        .map_or(false, |(_, oversize)| *oversize);

                    if is_oversize {
                        if let Some((_, true)) = browser_ctx_result {
                            let snapshot = browser_context::get_browser_context_for_chat(
                                app.gcx.clone(),
                                &browser_chat_id,
                            )
                            .await;
                            if let Some(ref snap) = snapshot {
                                let action_bytes = serde_json::to_string(&snap.actions)
                                    .unwrap_or_default()
                                    .len();
                                let console_bytes = serde_json::to_string(&snap.console)
                                    .unwrap_or_default()
                                    .len();
                                let network_bytes = serde_json::to_string(&snap.network)
                                    .unwrap_or_default()
                                    .len();
                                let mutation_bytes = serde_json::to_string(&snap.mutations)
                                    .unwrap_or_default()
                                    .len();
                                let pending_message_id = Uuid::new_v4().to_string();
                                let mut session = session_arc.lock().await;
                                session.pending_browser_message = Some(PendingBrowserMessage {
                                    pending_message_id: pending_message_id.clone(),
                                    content: content.clone(),
                                    attachments: attachments.clone(),
                                    client_message_id: client_message_id.clone(),
                                    checkpoints: checkpoints.clone(),
                                    context_files: context_files.clone(),
                                    suppress_auto_enrichment,
                                    skill_activation_name: skill_activation_name.clone(),
                                    skill_context_msg: skill_context_msg.clone(),
                                });
                                session.emit(ChatEvent::BrowserContextOversize {
                                    total_bytes: action_bytes
                                        + console_bytes
                                        + network_bytes
                                        + mutation_bytes,
                                    action_count: snap.actions.len(),
                                    action_bytes,
                                    console_count: snap.console.len(),
                                    console_bytes,
                                    network_count: snap.network.len(),
                                    network_bytes,
                                    mutation_bytes,
                                    pending_message_id: pending_message_id.clone(),
                                });
                                session.set_runtime_state(SessionState::WaitingUserInput, None);
                            }
                        }
                        return false;
                    }

                    let mut accepted_user_messages = Vec::new();
                    {
                        let mut session = session_arc.lock().await;

                        if let Some((ctx_messages, _)) = browser_ctx_result {
                            session.add_message(ctx_messages.event);
                            if let Some(screenshot) = ctx_messages.screenshot {
                                session.add_message(screenshot);
                            }
                        }

                        // Set compaction anchor for slash-command skill activation before any skill
                        // messages are added, so deactivate_skill can truncate back to this point.
                        if skill_activation_name.is_some()
                            && session.active_command.started_at_index.is_none()
                        {
                            session.active_command.started_at_index = Some(session.messages.len());
                        }

                        if let Some(skill_msg) = skill_context_msg {
                            session.add_message(skill_msg);
                        }

                        if !context_files.is_empty() {
                            apply_manual_context_files(&mut session, &context_files);
                        }

                        let parsed_content = parse_content_with_attachments(&content, &attachments);
                        accepted_user_messages.push(AcceptedUserMessage {
                            chat_id: session.chat_id.clone(),
                            thread: session.thread.clone(),
                            content: parsed_content.clone(),
                        });
                        let user_message = user_message_with_client_message_id(
                            parsed_content.clone(),
                            checkpoints,
                            client_message_id,
                        );
                        session.add_message(user_message);
                        note_user_turn_resets_goal_pursuit(&mut session);
                        if session.messages.iter().filter(|m| m.role == "user").count() == 1 {
                            let chat_id = session.chat_id.clone();
                            let first_user_text_preview = parsed_content
                                .content_text_only()
                                .chars()
                                .take(80)
                                .collect();
                            drop(session);
                            push_user_activity(
                                app.clone(),
                                UserAction::ChatStarted {
                                    chat_id,
                                    first_user_text_preview,
                                    ts: Utc::now(),
                                },
                            )
                            .await;
                            session = session_arc.lock().await;
                        }

                        if suppress_auto_enrichment && context_files.is_empty() {
                            session.suppress_auto_enrichment_for_next_turn = true;
                        }

                        if let Some(ref skill_name) = skill_activation_name {
                            session.set_active_skill(skill_name.clone());
                        }

                        for additional in additional_messages {
                            if let ChatCommand::UserMessage {
                                content: add_content,
                                attachments: add_attachments,
                                context_files: add_ctx_files,
                                suppress_auto_enrichment: _,
                                client_message_id: add_client_message_id,
                                push: _,
                            } = additional.command
                            {
                                if !add_ctx_files.is_empty() {
                                    apply_manual_context_files(&mut session, &add_ctx_files);
                                }
                                let add_parsed =
                                    parse_content_with_attachments(&add_content, &add_attachments);
                                accepted_user_messages.push(AcceptedUserMessage {
                                    chat_id: session.chat_id.clone(),
                                    thread: session.thread.clone(),
                                    content: add_parsed.clone(),
                                });
                                let add_message = user_message_with_client_message_id(
                                    add_parsed,
                                    Vec::new(),
                                    add_client_message_id,
                                );
                                session.add_message(add_message);
                                note_user_turn_resets_goal_pursuit(&mut session);
                            }
                        }
                    }

                    for accepted_user_message in accepted_user_messages {
                        let _ =
                            maybe_enqueue_chat_reaction(app.clone(), accepted_user_message).await;
                    }

                    maybe_save_trajectory_background_with_intent(
                        app.clone(),
                        session_arc.clone(),
                        TrajectoryCommitIntent::Checkpoint,
                    );
                    true
                })
                .await;
                if !prepared {
                    continue;
                }
                prepare_session_preamble_and_knowledge(app.clone(), session_arc.clone()).await;
                if aborted_before_start_generation(&session_arc).await {
                    continue;
                }
                start_generation(app.clone(), session_arc.clone()).await;
            }
            ChatCommand::RetryFromIndex {
                index,
                content,
                attachments,
            } => {
                let mut session = session_arc.lock().await;
                session.truncate_messages(index);
                let parsed_content = parse_content_with_attachments(&content, &attachments);
                let user_message = ChatMessage {
                    message_id: Uuid::new_v4().to_string(),
                    role: "user".to_string(),
                    content: parsed_content,
                    ..Default::default()
                };
                session.add_message(user_message);
                note_user_turn_resets_goal_pursuit(&mut session);
                drop(session);

                maybe_save_trajectory_background_with_intent(
                    app.clone(),
                    session_arc.clone(),
                    TrajectoryCommitIntent::Checkpoint,
                );
                prepare_session_preamble_and_knowledge(app.clone(), session_arc.clone()).await;
                if aborted_before_start_generation(&session_arc).await {
                    continue;
                }
                start_generation(app.clone(), session_arc.clone()).await;
            }
            ChatCommand::SetParams { patch } => {
                if !patch.is_object() {
                    warn!("SetParams patch must be an object, ignoring");
                    continue;
                }
                let mode_switch_reason = patch
                    .get("reason")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                    .map(str::to_string);
                let (chat_id, thread_before) = {
                    let session = session_arc.lock().await;
                    (session.chat_id.clone(), session.thread.clone())
                };
                let worktree_update = match resolve_worktree_setparams_update(
                    app.clone(),
                    &chat_id,
                    &thread_before,
                    &patch,
                )
                .await
                {
                    Ok(update) => update,
                    Err(e) => {
                        warn!("SetParams worktree update rejected: {}", e);
                        let mut session = session_arc.lock().await;
                        let compression_phase = session.compression_phase;
                        let compression_reason = session.compression_reason;
                        let event = session.runtime_update_event(
                            SessionState::Error,
                            Some(e),
                            false,
                            compression_phase,
                            compression_reason,
                        );
                        session.emit(event);
                        session.set_runtime_state(SessionState::Idle, None);
                        continue;
                    }
                };
                let mut session = session_arc.lock().await;
                let old_model = session.thread.model.clone();
                let old_mode = session.thread.mode.clone();
                let (mut changed, sanitized_patch) =
                    apply_setparams_patch(&mut session.thread, &patch);
                let mode_changed = session.thread.mode != old_mode;
                let worktree_message = worktree_update
                    .as_ref()
                    .filter(|update| update.changed)
                    .map(|update| match update.worktree.as_ref() {
                        Some(worktree) => worktree_activation_message(worktree),
                        None => worktree_disabled_message(update.previous_worktree.as_ref()),
                    });
                if let Some(update) = worktree_update.clone() {
                    session.thread.worktree = update.worktree;
                    changed |= update.changed;
                }

                let title_in_patch = patch.get("title").and_then(|v| v.as_str());
                let is_gen_in_patch = patch.get("is_title_generated").and_then(|v| v.as_bool());
                if let Some(title) = title_in_patch {
                    let is_generated = is_gen_in_patch.unwrap_or(false);
                    session.set_title(title.to_string(), is_generated);
                } else if let Some(is_gen) = is_gen_in_patch {
                    if session.thread.is_title_generated != is_gen {
                        let title = session.thread.title.clone();
                        session.set_title(title, is_gen);
                        changed = true;
                    }
                }
                let mut patch_for_chat_sse = sanitized_patch;
                if let Some(obj) = patch_for_chat_sse.as_object_mut() {
                    obj.remove("title");
                    obj.remove("is_title_generated");
                    if let Some(update) = worktree_update {
                        obj.insert("worktree".to_string(), update.sse_value);
                    }
                }
                session.emit(ChatEvent::ThreadUpdated {
                    params: patch_for_chat_sse,
                });
                if changed {
                    session.increment_version();
                    session.touch();
                }
                if mode_changed {
                    add_mode_switch_event_and_plan_if_changed(
                        app.clone(),
                        &mut session,
                        &old_mode,
                        mode_switch_reason.as_deref(),
                        "chat.session",
                    )
                    .await;
                }
                if mode_changed || session.thread.model != old_model {
                    session.release_turn_only_state();
                    if session.thread.model != old_model {
                        session.invalidate_usage_on_model_switch();
                    }
                }
                if let Some(message) = worktree_message {
                    session.add_message(message);
                }
                drop(session);
                if changed {
                    maybe_save_trajectory_with_intent(
                        app.clone(),
                        session_arc.clone(),
                        TrajectoryCommitIntent::Required,
                    )
                    .await;
                }
            }
            ChatCommand::Abort {} => {
                let mut session = session_arc.lock().await;
                session.suppress_delivery_wakes();
                session.abort_stream();
                let goal_stopped = session.stop_goal_on_manual_abort();
                drop(session);
                if goal_stopped || session_arc.lock().await.trajectory_dirty {
                    maybe_save_trajectory_with_intent(
                        app.clone(),
                        session_arc.clone(),
                        TrajectoryCommitIntent::Required,
                    )
                    .await;
                }
            }
            ChatCommand::CleanBackgroundProcesses { include_services } => {
                let chat_id = {
                    let session = session_arc.lock().await;
                    session.chat_id.clone()
                };
                match crate::chat::session::clean_background_processes_for_chat(
                    app.chat_ctx(),
                    &chat_id,
                    include_services,
                )
                .await
                {
                    Ok(killed) => {
                        let mut session = session_arc.lock().await;
                        session.add_background_process_cleanup_notice(killed.len());
                        drop(session);
                        maybe_save_trajectory_with_intent(
                            app.clone(),
                            session_arc.clone(),
                            TrajectoryCommitIntent::Required,
                        )
                        .await;
                    }
                    Err(error) => {
                        warn!("CleanBackgroundProcesses failed: {}", error);
                        let mut session = session_arc.lock().await;
                        let compression_phase = session.compression_phase;
                        let compression_reason = session.compression_reason;
                        let event = session.runtime_update_event(
                            SessionState::Error,
                            Some(error),
                            false,
                            compression_phase,
                            compression_reason,
                        );
                        session.emit(event);
                        session.set_runtime_state(SessionState::Idle, None);
                    }
                }
            }
            ChatCommand::ToolDecision {
                tool_call_id,
                accepted,
            } => {
                let decisions = vec![ToolDecisionItem {
                    tool_call_id: tool_call_id.clone(),
                    accepted,
                }];
                handle_tool_decisions(app.clone(), session_arc.clone(), &decisions).await;
            }
            ChatCommand::ToolDecisions { decisions } => {
                handle_tool_decisions(app.clone(), session_arc.clone(), &decisions).await;
            }
            ChatCommand::IdeToolResult {
                tool_call_id,
                content,
                tool_failed,
            } => {
                let mut session = session_arc.lock().await;
                let completed = session.record_ide_tool_result(tool_call_id, content, tool_failed);
                if completed {
                    session.release_turn_only_state();
                    session.set_runtime_state(SessionState::Idle, None);
                }
                drop(session);
                if !completed {
                    continue;
                }
                if aborted_before_start_generation(&session_arc).await {
                    continue;
                }
                start_generation(app.clone(), session_arc.clone()).await;
            }
            ChatCommand::UpdateMessage {
                message_id,
                content,
                attachments,
                regenerate,
            } => {
                let mut session = session_arc.lock().await;
                if session.runtime.state == SessionState::Generating {
                    session.abort_stream();
                }
                let parsed_content = parse_content_with_attachments(&content, &attachments);
                if let Some(idx) = session
                    .messages
                    .iter()
                    .position(|m| m.message_id == message_id)
                {
                    let mut updated_msg = session.messages[idx].clone();
                    updated_msg.content = parsed_content;
                    session.update_message(&message_id, updated_msg);
                    if regenerate && idx + 1 < session.messages.len() {
                        session.truncate_messages(idx + 1);
                        session.set_runtime_state(SessionState::Idle, None);
                        session.reactivate_goal_stopped_by_manual_abort();
                        drop(session);
                        maybe_save_trajectory_background_with_intent(
                            app.clone(),
                            session_arc.clone(),
                            TrajectoryCommitIntent::Checkpoint,
                        );
                        prepare_session_preamble_and_knowledge(app.clone(), session_arc.clone())
                            .await;
                        if aborted_before_start_generation(&session_arc).await {
                            continue;
                        }
                        start_generation(app.clone(), session_arc.clone()).await;
                    }
                }
            }
            ChatCommand::RemoveMessage {
                message_id,
                regenerate,
            } => {
                let mut session = session_arc.lock().await;
                if session.runtime.state == SessionState::Generating {
                    session.abort_stream();
                }
                if let Some(idx) = session.remove_message(&message_id) {
                    if regenerate && idx < session.messages.len() {
                        session.truncate_messages(idx);
                        session.set_runtime_state(SessionState::Idle, None);
                        session.reactivate_goal_stopped_by_manual_abort();
                        drop(session);
                        maybe_save_trajectory_background_with_intent(
                            app.clone(),
                            session_arc.clone(),
                            TrajectoryCommitIntent::Checkpoint,
                        );
                        prepare_session_preamble_and_knowledge(app.clone(), session_arc.clone())
                            .await;
                        if aborted_before_start_generation(&session_arc).await {
                            continue;
                        }
                        start_generation(app.clone(), session_arc.clone()).await;
                    }
                }
            }
            ChatCommand::Regenerate {} => {
                {
                    let mut session = session_arc.lock().await;
                    session.reactivate_goal_stopped_by_manual_abort();
                }
                maybe_save_trajectory_background_with_intent(
                    app.clone(),
                    session_arc.clone(),
                    TrajectoryCommitIntent::Checkpoint,
                );
                prepare_session_preamble_and_knowledge(app.clone(), session_arc.clone()).await;
                if aborted_before_start_generation(&session_arc).await {
                    continue;
                }
                start_generation(app.clone(), session_arc.clone()).await;
            }
            ChatCommand::RestoreMessages { messages } => {
                let mut session = session_arc.lock().await;
                session.release_turn_only_state();
                for msg_value in messages {
                    if let Ok(msg) = serde_json::from_value::<ChatMessage>(msg_value) {
                        if !is_allowed_for_restore(&msg) {
                            continue;
                        }
                        let sanitized = sanitize_message_for_restore(&msg);
                        session.add_message(sanitized);
                    }
                }
                drop(session);
                maybe_save_trajectory_with_intent(
                    app.clone(),
                    session_arc.clone(),
                    TrajectoryCommitIntent::Required,
                )
                .await;
            }
            ChatCommand::BranchFromChat {
                source_chat_id,
                up_to_message_id,
            } => {
                if let Err(e) = crate::chat::trajectories::validate_trajectory_id(&source_chat_id) {
                    warn!("BranchFromChat: invalid source_chat_id: {}", e);
                    continue;
                }

                let sessions = app.chat.sessions.clone();

                let source_session_arc = crate::chat::session::get_or_create_session_with_trajectory(
                    app.chat_ctx(),
                    &sessions,
                    &source_chat_id,
                )
                .await;

                let (messages_to_copy, root_id, source_worktree) = {
                    let source_session = source_session_arc.lock().await;
                    let mut msgs = Vec::new();
                    let mut found = false;
                    for m in &source_session.messages {
                        if is_allowed_for_branch(m) {
                            msgs.push(sanitize_message_for_branch(m));
                        }
                        if m.message_id == up_to_message_id {
                            found = true;
                            break;
                        }
                    }
                    if !found {
                        warn!(
                            "BranchFromChat: up_to_message_id '{}' not found in source chat",
                            up_to_message_id
                        );
                        continue;
                    }
                    crate::chat::trajectory_ops::demote_goal_ownership_for_branch(&mut msgs);
                    let root = source_session
                        .thread
                        .root_chat_id
                        .clone()
                        .unwrap_or_else(|| source_chat_id.clone());
                    (msgs, root, source_session.thread.worktree.clone())
                };

                let mut session = session_arc.lock().await;
                session.thread.parent_id = Some(source_chat_id.clone());
                session.thread.link_type = Some("branch".to_string());
                session.thread.root_chat_id = Some(root_id);

                for msg in messages_to_copy {
                    session.add_message(msg);
                }
                let target_thread = session.thread.clone();
                drop(session);
                if let Some(worktree) = source_worktree.as_ref() {
                    if let Some(validated) = add_thread_worktree_reference(
                        app.clone(),
                        &target_thread.id,
                        &target_thread,
                        worktree,
                    )
                    .await
                    {
                        let mut session = session_arc.lock().await;
                        session.thread.worktree = Some(validated);
                    }
                }
                maybe_save_trajectory_with_intent(
                    app.clone(),
                    session_arc.clone(),
                    TrajectoryCommitIntent::Required,
                )
                .await;
            }
            ChatCommand::BrowserContextDecision {
                pending_message_id,
                include_actions,
                include_console,
                include_network,
                include_mutations,
                include_screenshot,
                last_n_actions,
                last_n_console,
                last_n_network,
            } => {
                let pending = {
                    let mut session = session_arc.lock().await;
                    session.pending_browser_message.take()
                };
                let Some(pending) = pending else {
                    warn!("BrowserContextDecision: no pending message found");
                    let mut session = session_arc.lock().await;
                    session.set_runtime_state(SessionState::Idle, None);
                    continue;
                };
                if pending.pending_message_id != pending_message_id {
                    warn!("BrowserContextDecision: pending_message_id mismatch");
                    let mut session = session_arc.lock().await;
                    session.set_runtime_state(SessionState::Idle, None);
                    continue;
                }

                let browser_chat_id = {
                    let session = session_arc.lock().await;
                    session.chat_id.clone()
                };

                let snapshot = browser_context::get_browser_context_for_chat(
                    app.gcx.clone(),
                    &browser_chat_id,
                )
                .await;
                let screenshot = if include_screenshot
                    && snapshot
                        .as_ref()
                        .is_some_and(|snapshot| snapshot.page_changed)
                {
                    browser_context::capture_browser_screenshot(app.gcx.clone(), &browser_chat_id)
                        .await
                } else {
                    None
                };

                {
                    let mut session = session_arc.lock().await;

                    if let Some(mut snap) = snapshot {
                        browser_context::apply_decision_to_snapshot(
                            &mut snap,
                            include_actions,
                            include_console,
                            include_network,
                            include_mutations,
                            last_n_actions,
                            last_n_console,
                            last_n_network,
                        );
                        let ctx_messages =
                            browser_context::make_context_messages(&snap, screenshot);
                        session.add_message(ctx_messages.event);
                        if let Some(screenshot) = ctx_messages.screenshot {
                            session.add_message(screenshot);
                        }
                    }

                    if pending.skill_activation_name.is_some()
                        && session.active_command.started_at_index.is_none()
                    {
                        session.active_command.started_at_index = Some(session.messages.len());
                    }
                    if let Some(skill_msg) = pending.skill_context_msg {
                        session.add_message(skill_msg);
                    }
                    if !pending.context_files.is_empty() {
                        apply_manual_context_files(&mut session, &pending.context_files);
                    }

                    if pending.suppress_auto_enrichment && pending.context_files.is_empty() {
                        session.suppress_auto_enrichment_for_next_turn = true;
                    }

                    let parsed_content =
                        parse_content_with_attachments(&pending.content, &pending.attachments);
                    let user_message = user_message_with_client_message_id(
                        parsed_content,
                        pending.checkpoints,
                        pending.client_message_id,
                    );
                    session.add_message(user_message);
                    note_user_turn_resets_goal_pursuit(&mut session);

                    if let Some(ref skill_name) = pending.skill_activation_name {
                        session.set_active_skill(skill_name.clone());
                    }
                    session.set_runtime_state(SessionState::Idle, None);
                }

                browser_context::commit_browser_cursors(app.gcx.clone(), &browser_chat_id).await;
                maybe_save_trajectory_background_with_intent(
                    app.clone(),
                    session_arc.clone(),
                    TrajectoryCommitIntent::Checkpoint,
                );
                prepare_session_preamble_and_knowledge(app.clone(), session_arc.clone()).await;
                if aborted_before_start_generation(&session_arc).await {
                    continue;
                }
                start_generation(app.clone(), session_arc.clone()).await;
            }
            ChatCommand::DeliverMessages { delivery } => {
                // Normally routed straight into the pending queue by
                // `chat::delivery`; reaching the command loop means a producer
                // pushed it as a plain command, so honour it here too.
                let wake = delivery.wake;
                let outcome = {
                    let mut session = session_arc.lock().await;
                    session.enqueue_delivery(delivery)
                };
                match outcome {
                    Ok(DeliveryOutcome::Delivered) => {
                        note_delivered_messages_reset_goal_pursuit(&session_arc).await;
                        maybe_save_trajectory_with_intent(
                            app.clone(),
                            session_arc.clone(),
                            TrajectoryCommitIntent::Required,
                        )
                        .await;
                        if wake && !aborted_before_start_generation(&session_arc).await {
                            start_generation(app.clone(), session_arc.clone()).await;
                        }
                    }
                    Ok(_) => {
                        maybe_save_trajectory_with_intent(
                            app.clone(),
                            session_arc.clone(),
                            TrajectoryCommitIntent::Required,
                        )
                        .await;
                    }
                    Err(error) => {
                        warn!("DeliverMessages rejected: {}", error);
                        let mut session = session_arc.lock().await;
                        emit_goal_command_error(&mut session, error);
                    }
                }
            }
            ChatCommand::UpdatePendingDelivery {
                delivery_id,
                push,
                cancel,
            } => {
                let chat_id = {
                    let session = session_arc.lock().await;
                    session.chat_id.clone()
                };
                if let Err(error) = crate::chat::delivery::update_pending_delivery_in_chat(
                    app.clone(),
                    &chat_id,
                    &delivery_id,
                    push,
                    cancel,
                )
                .await
                {
                    warn!("UpdatePendingDelivery rejected: {}", error);
                }
            }
        }

        // A command finished: land any delivery whose boundary just opened.
        crate::chat::delivery::drain_deliveries_at_boundary(app.clone(), session_arc.clone()).await;
    }
}

/// Delivered user-visible messages start a fresh pursuit turn, exactly like a
/// genuine user message, without going through the `UserMessage` command path.
pub(crate) async fn note_delivered_messages_reset_goal_pursuit(session_arc: &Arc<AMutex<ChatSession>>) {
    let mut session = session_arc.lock().await;
    note_user_turn_resets_goal_pursuit(&mut session);
}

pub(crate) fn is_plan_delta_event(msg: &ChatMessage) -> bool {
    msg.role == "event"
        && msg
            .extra
            .get("event")
            .and_then(|event| event.get("subkind"))
            .and_then(|subkind| subkind.as_str())
            == Some("plan_delta")
}

pub(crate) fn is_goal_delta_event(msg: &ChatMessage) -> bool {
    msg.role == "event"
        && msg
            .extra
            .get("event")
            .and_then(|event| event.get("subkind"))
            .and_then(|subkind| subkind.as_str())
            == Some("goal_delta")
}

pub(crate) fn is_goal_pursuit_event(msg: &ChatMessage) -> bool {
    msg.role == "event"
        && msg
            .extra
            .get("event")
            .and_then(|event| event.get("subkind"))
            .and_then(|subkind| subkind.as_str())
            == Some("goal_pursuit")
}

pub(crate) fn is_goal_hidden_event(msg: &ChatMessage) -> bool {
    is_goal_delta_event(msg) || is_goal_pursuit_event(msg)
}

pub(crate) fn hidden_role_extra(msg: &ChatMessage) -> serde_json::Map<String, serde_json::Value> {
    if matches!(
        msg.role.as_str(),
        "plan" | "goal" | "event" | "compression_report"
    ) {
        msg.extra.clone()
    } else {
        serde_json::Map::new()
    }
}

pub(crate) fn is_allowed_for_restore(msg: &ChatMessage) -> bool {
    matches!(
        msg.role.as_str(),
        "user" | "assistant" | "system" | "tool" | "plan" | "goal" | "compression_report"
    ) || is_plan_delta_event(msg)
        || is_goal_hidden_event(msg)
}

/// Sanitize message for restoring from external trajectory — strips tool_calls for security
/// and transient metadata.
pub(crate) fn sanitize_message_for_restore(msg: &ChatMessage) -> ChatMessage {
    ChatMessage {
        message_id: Uuid::new_v4().to_string(),
        role: msg.role.clone(),
        content: msg.content.clone(),
        tool_calls: None, // Security: strip tool_calls to prevent prerun of restored messages
        tool_call_id: msg.tool_call_id.clone(),
        tool_failed: msg.tool_failed,
        preserve: msg.preserve,
        reasoning_content: msg.reasoning_content.clone(),
        thinking_blocks: msg.thinking_blocks.clone(),
        citations: msg.citations.clone(),
        server_content_blocks: msg.server_content_blocks.clone(),
        summarized_range: msg.summarized_range,
        summarization_tier: msg.summarization_tier.clone(),
        summarized_token_estimate: msg.summarized_token_estimate,
        extra: hidden_role_extra(msg),
        ..Default::default()
    }
}

/// Sanitize message for branching — preserves the conversation structure (including tool_calls
/// and context_file messages) but strips thinking blocks and transient metadata.
pub(crate) fn sanitize_message_for_branch(msg: &ChatMessage) -> ChatMessage {
    ChatMessage {
        message_id: Uuid::new_v4().to_string(),
        role: msg.role.clone(),
        content: msg.content.clone(),
        tool_calls: msg.tool_calls.clone(),
        tool_call_id: msg.tool_call_id.clone(),
        tool_failed: msg.tool_failed,
        preserve: msg.preserve,
        checkpoints: msg.checkpoints.clone(),
        reasoning_content: msg.reasoning_content.clone(),
        citations: msg.citations.clone(),
        server_content_blocks: msg.server_content_blocks.clone(),
        summarized_range: msg.summarized_range,
        summarization_tier: msg.summarization_tier.clone(),
        summarized_token_estimate: msg.summarized_token_estimate,
        extra: hidden_role_extra(msg),
        ..Default::default()
    }
}

pub(crate) fn is_allowed_for_branch(msg: &ChatMessage) -> bool {
    matches!(
        msg.role.as_str(),
        "user"
            | "assistant"
            | "system"
            | "tool"
            | "context_file"
            | "plan"
            | "goal"
            | "compression_report"
    ) || is_plan_delta_event(msg)
        || is_goal_hidden_event(msg)
}

pub(crate) async fn handle_tool_decisions(
    app: AppState,
    session_arc: Arc<AMutex<ChatSession>>,
    decisions: &[ToolDecisionItem],
) {
    let is_shell_privacy_pause = {
        let session = session_arc.lock().await;
        session
            .runtime
            .pause_reasons
            .iter()
            .any(crate::chat::tools::is_shell_privacy_approval)
    };

    if is_shell_privacy_pause {
        let should_continue = {
            let mut session = session_arc.lock().await;
            let privacy_ids = session
                .runtime
                .pause_reasons
                .iter()
                .filter(|reason| crate::chat::tools::is_shell_privacy_approval(reason))
                .map(|reason| reason.tool_call_id.clone())
                .collect::<std::collections::HashSet<_>>();
            let accepted_ids = decisions
                .iter()
                .filter(|decision| {
                    decision.accepted && privacy_ids.contains(&decision.tool_call_id)
                })
                .map(|decision| decision.tool_call_id.clone())
                .collect::<Vec<_>>();
            let rejected_ids = decisions
                .iter()
                .filter(|decision| {
                    !decision.accepted && privacy_ids.contains(&decision.tool_call_id)
                })
                .map(|decision| decision.tool_call_id.clone())
                .collect::<Vec<_>>();
            let decided = accepted_ids
                .iter()
                .chain(&rejected_ids)
                .cloned()
                .collect::<std::collections::HashSet<_>>();
            let remaining = session
                .runtime
                .pause_reasons
                .iter()
                .filter(|reason| !decided.contains(&reason.tool_call_id))
                .cloned()
                .collect::<Vec<_>>();
            if remaining.is_empty() {
                session.complete_confirmation_wait();
            }

            let updates = session
                .messages
                .iter()
                .filter(|message| decided.contains(&message.tool_call_id))
                .filter_map(|message| {
                    let mut updated = message.clone();
                    let accepted = accepted_ids.contains(&message.tool_call_id);
                    crate::privacy::records::resolve_shell_ask(&mut updated, accepted)
                        .then_some(updated)
                })
                .collect::<Vec<_>>();
            for updated in updates {
                let message_id = updated.message_id.clone();
                session.update_message(&message_id, updated);
            }
            session.add_tool_decision_event("approve", accepted_ids, "once");
            session.add_tool_decision_event("reject", rejected_ids, "once");
            session.drain_post_tool_side_effects();
            let remaining = session
                .runtime
                .pause_reasons
                .iter()
                .filter(|reason| !decided.contains(&reason.tool_call_id))
                .cloned()
                .collect::<Vec<_>>();
            if remaining.is_empty() {
                session.complete_confirmation_wait();
                session.release_turn_only_state();
                session.set_runtime_state(SessionState::Idle, None);
                true
            } else {
                session.set_paused_with_reasons_and_auto_approved(remaining, Vec::new(), None);
                false
            }
        };
        if should_continue && !aborted_before_start_generation(&session_arc).await {
            start_generation(app, session_arc).await;
        }
        return;
    }

    let is_cache_guard_pause = {
        let session = session_arc.lock().await;
        session
            .runtime
            .pause_reasons
            .iter()
            .any(crate::chat::cache_guard::is_cache_guard_pause_reason)
    };

    if is_cache_guard_pause {
        let accepted_any = decisions.iter().any(|d| d.accepted);

        {
            let mut session = session_arc.lock().await;
            if accepted_any {
                session.reset_cache_guard_snapshot();
            }
            let approved_ids = decisions
                .iter()
                .filter(|d| d.accepted)
                .map(|d| d.tool_call_id.clone())
                .collect::<Vec<_>>();
            let rejected_ids = decisions
                .iter()
                .filter(|d| !d.accepted)
                .map(|d| d.tool_call_id.clone())
                .collect::<Vec<_>>();
            session.add_tool_decision_event("approve", approved_ids, "once");
            session.add_tool_decision_event("reject", rejected_ids, "once");
            session.drain_post_tool_side_effects();
            session.runtime.pause_reasons.clear();
            session.runtime.accepted_tool_ids.clear();
            session.runtime.auto_approved_tool_ids.clear();
            session.runtime.paused_message_index = None;
            session.complete_confirmation_wait();
            if accepted_any {
                session.release_turn_only_state();
                session.set_runtime_state(SessionState::Idle, None);
            } else {
                session.set_runtime_state(SessionState::Idle, None);
                session.release_turn_only_state();
            }
        }

        if accepted_any {
            if !aborted_before_start_generation(&session_arc).await {
                start_generation(app.clone(), session_arc.clone()).await;
            }
        } else {
            maybe_save_trajectory_with_intent(
                app.clone(),
                session_arc.clone(),
                TrajectoryCommitIntent::Required,
            )
            .await;
        }
        return;
    }

    let (
        auto_approved_ids,
        has_remaining_pauses,
        tool_calls_to_execute,
        messages,
        thread,
        any_rejected,
    ) = {
        let mut session = session_arc.lock().await;
        let auto_approved = session.runtime.auto_approved_tool_ids.clone();
        let paused_msg_idx = session.runtime.paused_message_index;
        let outcome = session.process_tool_decisions(decisions);
        let any_rejected = !outcome.denied_ids.is_empty();

        for id in &outcome.accepted_ids {
            if !session.runtime.accepted_tool_ids.contains(id) {
                session.runtime.accepted_tool_ids.push(id.clone());
            }
        }

        let remaining = !session.runtime.pause_reasons.is_empty();

        let mut ids_to_execute: std::collections::HashSet<String> =
            session.runtime.accepted_tool_ids.iter().cloned().collect();
        if !any_rejected && !remaining {
            for id in &auto_approved {
                ids_to_execute.insert(id.clone());
            }
        }

        let tool_calls: Vec<crate::call_validation::ChatToolCall> =
            if let Some(msg_idx) = paused_msg_idx {
                session
                    .messages
                    .get(msg_idx)
                    .and_then(|m| m.tool_calls.as_ref())
                    .map(|tcs| {
                        tcs.iter()
                            .filter(|tc| ids_to_execute.contains(&tc.id))
                            .cloned()
                            .collect()
                    })
                    .unwrap_or_default()
            } else {
                session
                    .messages
                    .iter()
                    .filter_map(|m| m.tool_calls.as_ref())
                    .flatten()
                    .filter(|tc| ids_to_execute.contains(&tc.id))
                    .cloned()
                    .collect()
            };

        (
            auto_approved,
            remaining,
            tool_calls,
            session.messages.clone(),
            session.thread.clone(),
            any_rejected,
        )
    };

    if has_remaining_pauses {
        return;
    }

    {
        let mut session = session_arc.lock().await;
        session.runtime.accepted_tool_ids.clear();
        session.runtime.auto_approved_tool_ids.clear();
        session.runtime.paused_message_index = None;
    }

    if any_rejected && !auto_approved_ids.is_empty() {
        let mut session = session_arc.lock().await;
        for id in &auto_approved_ids {
            let already_handled = session
                .messages
                .iter()
                .any(|m| m.role == "tool" && m.tool_call_id == *id);
            if already_handled {
                continue;
            }
            let tool_message = ChatMessage {
                message_id: Uuid::new_v4().to_string(),
                role: "tool".to_string(),
                content: ChatContent::SimpleText(
                    "Tool execution skipped due to user rejection of related tools".to_string(),
                ),
                tool_call_id: id.clone(),
                tool_failed: Some(true),
                ..Default::default()
            };
            session.add_message(tool_message);
        }
        session.drain_post_tool_side_effects();
    }

    let had_tool_calls = !tool_calls_to_execute.is_empty();
    if had_tool_calls {
        let session_catalog = {
            let session = session_arc.lock().await;
            session.tool_catalog.clone()
        };
        let catalog = match session_catalog {
            Some(catalog) => catalog,
            None => {
                app.tool_registry
                    .acquire_tool_catalog(
                        &thread.mode,
                        Some(&thread.model),
                        thread
                            .worktree
                            .as_ref()
                            .map(|worktree| worktree.root.to_string_lossy().into_owned())
                            .as_deref(),
                    )
                    .await
            }
        };
        let tool_calls_to_execute =
            resolve_tool_call_aliases_with_catalog(tool_calls_to_execute, &catalog);
        let turn_tool_pool = acquire_session_turn_tool_pool(
            &app,
            &session_arc,
            &thread,
            &thread.mode,
            Some(&thread.model),
            &catalog,
        )
        .await;

        {
            let mut session = session_arc.lock().await;
            session.set_runtime_state(SessionState::ExecutingTools, None);
        }

        let (tool_results, _) = execute_tools_with_session(
            app.clone(),
            session_arc.clone(),
            &tool_calls_to_execute,
            &messages,
            &thread,
            &thread.mode,
            Some(thread.model.as_str()),
            crate::chat::tools::ExecuteToolsOptions {
                catalog: Some(catalog),
                turn_tool_pool,
                ..Default::default()
            },
        )
        .await;

        let privacy_approvals =
            crate::chat::tools::shell_privacy_approval_reasons(&tool_calls_to_execute, &tool_results);
        if !privacy_approvals.is_empty() {
            let mut session = session_arc.lock().await;
            for result_msg in tool_results {
                session.add_message(result_msg);
            }
            session.drain_post_tool_side_effects();
            session.set_paused_with_reasons_and_auto_approved(privacy_approvals, Vec::new(), None);
            return;
        }

        // Determine tool-requested final state before checking abort.
        // Some tools (ask_questions/task_done/agent_finish) set abort_flag=true as part of
        // normal operation to stop further LLM generation.
        let mut final_state = SessionState::Idle;
        let mut completion_trigger: Option<String> = None;
        for tool_call in &tool_calls_to_execute {
            let tool_name =
                crate::llm::adapters::claude_code_compat::cc_normalize_internal_tool_name(
                    &tool_call.function.name,
                );
            match tool_name.as_str() {
                "ask_questions" | "wait_agents" => final_state = SessionState::WaitingUserInput,
                "task_done" | "finish" => {
                    final_state = SessionState::Completed;
                    completion_trigger = Some(tool_name.to_string());
                }
                "agent_finish" => {
                    final_state = SessionState::Completed;
                    completion_trigger = Some(tool_name.to_string());
                }
                _ => {}
            }
        }

        let tool_initiated_stop = matches!(
            final_state,
            SessionState::Completed | SessionState::WaitingUserInput
        );

        // Check if we were aborted during tool execution
        let was_aborted = {
            let session = session_arc.lock().await;
            session
                .abort_flag
                .load(std::sync::atomic::Ordering::Relaxed)
        };

        let mut verify_completion = false;
        {
            let mut session = session_arc.lock().await;
            for result_msg in tool_results {
                session.add_message(result_msg);
            }
            session.drain_post_tool_side_effects();
            if tool_initiated_stop {
                if final_state == SessionState::Completed
                    && completion_trigger.is_some()
                    && should_verify_goal_on_done(&session)
                {
                    session.set_runtime_state(SessionState::ExecutingTools, None);
                    verify_completion = true;
                } else {
                    session.set_runtime_state(final_state, None);
                }
            } else if was_aborted {
                session.set_runtime_state(SessionState::Idle, None);
                session.release_turn_only_state();
            } else {
                session.release_turn_only_state();
                session.set_runtime_state(SessionState::Idle, None);
            }
        }

        if verify_completion {
            let trigger = completion_trigger.as_deref().unwrap_or("task_done");
            match verify_goal_before_completion(app.clone(), session_arc.clone(), trigger).await {
                GoalCompletionGateOutcome::Passthrough => {
                    let mut session = session_arc.lock().await;
                    session.set_runtime_state(SessionState::Completed, None);
                }
                GoalCompletionGateOutcome::Finalized => {}
                GoalCompletionGateOutcome::Rearmed => {
                    maybe_save_trajectory_with_intent(
                        app.clone(),
                        session_arc.clone(),
                        TrajectoryCommitIntent::Required,
                    )
                    .await;
                    return;
                }
                GoalCompletionGateOutcome::BudgetExhausted(_) => {}
                GoalCompletionGateOutcome::VerificationUnavailable
                | GoalCompletionGateOutcome::VerificationStalled => {
                    let mut session = session_arc.lock().await;
                    session.set_runtime_state(SessionState::Completed, None);
                }
                GoalCompletionGateOutcome::Aborted => {
                    maybe_save_trajectory_with_intent(
                        app.clone(),
                        session_arc.clone(),
                        TrajectoryCommitIntent::Required,
                    )
                    .await;
                    return;
                }
            }
        }

        {
            let mut session = session_arc.lock().await;
            if session.pending_skill_deactivation.is_some() {
                session.perform_skill_deactivation_cleanup();
            }
        }

        if was_aborted || tool_initiated_stop {
            maybe_save_trajectory_with_intent(
                app.clone(),
                session_arc.clone(),
                TrajectoryCommitIntent::Required,
            )
            .await;
        } else {
            maybe_save_trajectory_background_with_intent(
                app.clone(),
                session_arc.clone(),
                TrajectoryCommitIntent::Checkpoint,
            );
        }

        if was_aborted || tool_initiated_stop {
            return;
        }
    }

    if any_rejected {
        {
            let mut session = session_arc.lock().await;
            session.set_runtime_state(SessionState::Idle, None);
            session.release_turn_only_state();
        }
        maybe_save_trajectory_with_intent(app, session_arc, TrajectoryCommitIntent::Required).await;
    } else if had_tool_calls {
        if !aborted_before_start_generation(&session_arc).await {
            start_generation(app, session_arc).await;
        }
    } else {
        {
            let mut session = session_arc.lock().await;
            session.set_runtime_state(SessionState::Idle, None);
            session.release_turn_only_state();
        }
        maybe_save_trajectory_with_intent(app, session_arc, TrajectoryCommitIntent::Required).await;
    }
}
