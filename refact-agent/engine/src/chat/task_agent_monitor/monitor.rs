#[allow(unused_imports)]
use super::*;

/// True when `chat_id` is the card's primary agent or any of its room members.
pub(super) fn card_accepts_agent_chat_id(card: &BoardCard, chat_id: &str) -> bool {
    if card.agent_chat_id.as_deref() == Some(chat_id) {
        return true;
    }
    card.team()
        .iter()
        .any(|m| m.agent_chat_id.as_deref() == Some(chat_id))
}

pub(super) fn linked_agent_session_matches(session: &ChatSession, task_id: &str, card: &BoardCard) -> bool {
    let Some(meta) = session.thread.task_meta.as_ref() else {
        return false;
    };

    if meta.role != "agents" || meta.task_id != task_id {
        return false;
    }
    if meta.card_id.as_deref() != Some(card.id.as_str()) {
        return false;
    }
    if card.assignee.as_deref() != meta.agent_id.as_deref() {
        return false;
    }
    true
}

pub(super) fn agent_session_idle_stall_ready(session: &ChatSession, task_id: &str, card: &BoardCard) -> bool {
    if card.column != "doing" || card.agent_chat_id.is_none() {
        return false;
    }
    if !linked_agent_session_matches(session, task_id, card) {
        return false;
    }
    if !matches!(
        session.runtime.state,
        SessionState::Idle | SessionState::Completed
    ) {
        return false;
    }
    if session.closed
        || session.draft_message.is_some()
        || session.pending_browser_message.is_some()
        || !session.command_queue.is_empty()
        || !session.pending_deliveries.is_empty()
        || !session.runtime.pause_reasons.is_empty()
    {
        return false;
    }
    if session
        .user_interrupt_flag
        .load(std::sync::atomic::Ordering::Relaxed)
    {
        return false;
    }
    session.last_activity.elapsed() >= STALL_PLANNER_NOTIFY_GRACE
}

/// Background task that monitors for stuck agents
pub async fn start_agent_monitor(app: AppState) {
    tracing::info!("Starting task agent monitor");

    loop {
        let shutdown_flag = app.runtime.shutdown_flag.clone();
        tokio::select! {
            _ = sleep(MONITOR_INTERVAL) => {}
            _ = async {
                while !shutdown_flag.load(std::sync::atomic::Ordering::SeqCst) {
                    tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;
                }
            } => {
                tracing::info!("Task agent monitor: shutdown detected, stopping");
                return;
            }
        }

        if let Err(e) = check_for_stuck_agents(app.clone()).await {
            tracing::error!("Agent monitor error: {}", e);
        }
    }
}

pub(super) async fn sweep_planner_wake_ups(app: AppState) -> Result<(), String> {
    let now = Utc::now();
    let task_metas = storage::list_tasks_for_ctx(&app.ctx).await?;

    for task_meta in task_metas {
        if task_meta.status != crate::tasks::types::TaskStatus::Active {
            continue;
        }
        let task_id = task_meta.id.clone();

        let all_session_arcs: Vec<_> = {
            let sessions_read = app.chat.sessions.read().await;
            sessions_read.values().cloned().collect()
        };

        for session_arc in all_session_arcs {
            let (should_wake, chat_id) = {
                let session = session_arc.lock().await;
                let is_planner_for_task = session
                    .thread
                    .task_meta
                    .as_ref()
                    .map(|m| m.role == "planner" && m.task_id == task_id)
                    .unwrap_or(false);
                if !is_planner_for_task {
                    (false, String::new())
                } else {
                    let is_waiting = session.runtime.state == SessionState::WaitingUserInput;
                    let past_deadline = session.wake_up_at.map(|t| now >= t).unwrap_or(false);
                    let chat_id = session.chat_id.clone();
                    (is_waiting && past_deadline, chat_id)
                }
            };

            if !should_wake {
                continue;
            }

            {
                let mut session = session_arc.lock().await;
                session.wake_up_at = None;
                session.mark_persisted_runtime_changed();
            }

            let statuses = crate::tools::tool_task_check_agents::get_agent_statuses_for_ctx(
                &app.ctx,
                app.chat.facade.clone(),
                &task_id,
            )
            .await
            .unwrap_or_default();

            let empty_args: std::collections::HashMap<String, serde_json::Value> =
                std::collections::HashMap::new();
            let query = crate::tools::tool_task_check_agents::parse_agent_status_query(&empty_args)
                .unwrap_or_else(|_| {
                    crate::tools::tool_task_check_agents::parse_agent_status_query(
                        &std::collections::HashMap::new(),
                    )
                    .unwrap()
                });
            let status_text =
                crate::tools::tool_task_check_agents::format_agent_statuses(&statuses, &query)
                    .unwrap_or_else(|_| "(status unavailable)".to_string());

            let message = format!(
                "[AUTO WAKE] Your requested wait window expired. Current agent status:\n\n{}\n\nDecide whether to continue waiting, merge ready cards, or move on.",
                status_text
            );
            let notice = task_agent_monitor_notice(
                json!({
                    "kind": "planner_auto_wake",
                    "task_id": task_id,
                    "planner_chat_id": chat_id,
                    "status_text": status_text,
                }),
                message,
            );


            app.chat
                .facade
                .deliver_messages(
                    &chat_id,
                    PendingDelivery::new(
                        vec![notice],
                        PushMode::Append,
                        TASK_AGENT_MONITOR_SOURCE,
                        true,
                    ),
                )
                .await?;

            tracing::info!(
                "Auto-woke planner {} for task {} (wake_up_at deadline passed)",
                chat_id,
                task_id
            );
        }
    }

    Ok(())
}

/// Check all active tasks for stuck agents
pub(super) async fn check_for_stuck_agents(app: AppState) -> Result<(), String> {
    let task_metas = storage::list_tasks_for_ctx(&app.ctx).await?;

    for task_meta in task_metas {
        if task_meta.status != crate::tasks::types::TaskStatus::Active {
            continue;
        }

        let task_id = &task_meta.id;
        let board = storage::load_board_for_ctx(&app.ctx, task_id).await?;
        let sessions = app.chat.sessions.clone();

        for card in &board.cards {
            if card.column != "doing" || card.assignee.is_none() {
                continue;
            }
            if card.ab_variants.is_some() && card.agent_chat_id.is_none() {
                continue;
            }

            let last_activity_timestamp = card
                .status_updates
                .last()
                .map(|u| u.timestamp.as_str())
                .or(card.started_at.as_deref())
                .unwrap_or(&card.created_at);

            let agent_chat_id = match &card.agent_chat_id {
                Some(id) => id,
                None => {
                    if let Ok(last_time) =
                        chrono::DateTime::parse_from_rfc3339(last_activity_timestamp)
                    {
                        let elapsed =
                            Utc::now().signed_duration_since(last_time.with_timezone(&Utc));
                        if elapsed.num_seconds() as u64 > AGENT_STUCK_TIMEOUT.as_secs() {
                            tracing::warn!(
                                "Agent for card {} has no agent_chat_id but is doing, stuck for {} ago",
                                card.id,
                                humantime::format_duration(Duration::from_secs(elapsed.num_seconds() as u64))
                            );

                            mark_agent_as_failed(
app.ctx.clone(),
                                task_id,
                                &card.id,
                                card.assignee.as_deref(),
                                None,
                                &format!(
                                    "Agent appears stuck (no agent_chat_id, no activity for {})",
                                    humantime::format_duration(AGENT_STUCK_TIMEOUT)
                                ),
                                AgentFailureKind::Permanent,
                            )
                            .await?;
                        }
                    }
                    continue;
                }
            };

            let session_arc = {
                let sessions_read = sessions.read().await;
                sessions_read.get(agent_chat_id).cloned()
            };

            let Some(session_arc) = session_arc else {
                if let Ok(last_time) = chrono::DateTime::parse_from_rfc3339(last_activity_timestamp)
                {
                    let elapsed = Utc::now().signed_duration_since(last_time.with_timezone(&Utc));
                    if elapsed.num_seconds() as u64 > AGENT_STUCK_TIMEOUT.as_secs() {
                        tracing::warn!(
                            "Agent for card {} appears stuck (no session, last update {} ago)",
                            card.id,
                            humantime::format_duration(Duration::from_secs(
                                elapsed.num_seconds() as u64
                            ))
                        );

                        mark_agent_as_failed(
app.ctx.clone(),
                            task_id,
                            &card.id,
                            card.assignee.as_deref(),
                            None,
                            &format!(
                                "Agent appears stuck (no activity for {})",
                                humantime::format_duration(AGENT_STUCK_TIMEOUT)
                            ),
                            AgentFailureKind::Permanent,
                        )
                        .await?;
                    }
                }
                continue;
            };

            let session = session_arc.lock().await;

            // Check if session is in Error state
            if session.runtime.state == SessionState::Error {
                let error_msg = session
                    .runtime
                    .error
                    .as_deref()
                    .unwrap_or("Unknown error")
                    .to_string();
                let task_meta_opt = session.thread.task_meta.clone();
                let planner_chat_id = task_meta_opt
                    .as_ref()
                    .and_then(|meta| meta.planner_chat_id.clone());

                drop(session);

                tracing::warn!(
                    "Agent for card {} is in Error state: {}",
                    card.id,
                    error_msg
                );

                if let Some(task_meta) = task_meta_opt {
                    handle_agent_streaming_error(app.clone(), &task_meta, &error_msg).await;
                } else {
                    let failure_kind = AgentFailureKind::from_error(&error_msg);
                    let failure_reason = failure_kind.final_report_reason(&error_msg);
                    mark_agent_as_failed(
app.ctx.clone(),
                        task_id,
                        &card.id,
                        card.assignee.as_deref(),
                        planner_chat_id.as_deref(),
                        &failure_reason,
                        failure_kind,
                    )
                    .await?;
                }
                continue;
            }

            let elapsed = session.last_activity.elapsed();
            let planner_chat_id = session
                .thread
                .task_meta
                .as_ref()
                .and_then(|meta| meta.planner_chat_id.clone());

            let now = Utc::now();
            let stall_info: Option<(StallKind, Duration)> =
                if agent_session_idle_stall_ready(&session, task_id, card) {
                    let kind = match session.runtime.state {
                        SessionState::Completed => StallKind::Completed,
                        _ => StallKind::IdleNoFinish,
                    };
                    Some((kind, elapsed))
                } else if session.runtime.state == SessionState::Generating
                    && session
                        .last_stream_delta_at
                        .map(|t| t.elapsed() > STREAM_STALL_TIMEOUT)
                        .unwrap_or(elapsed > STREAM_STALL_TIMEOUT)
                {
                    let stall_elapsed = session
                        .last_stream_delta_at
                        .map(|t| t.elapsed())
                        .unwrap_or(elapsed);
                    Some((StallKind::GeneratingNoTokens, stall_elapsed))
                } else if session.runtime.state == SessionState::ExecutingTools
                    && session
                        .last_tool_progress_at
                        .or(session.last_tool_started_at)
                        .map(|t| t.elapsed() > TOOL_STALL_TIMEOUT)
                        .unwrap_or(false)
                {
                    let stall_elapsed = session
                        .last_tool_progress_at
                        .or(session.last_tool_started_at)
                        .map(|t| t.elapsed())
                        .unwrap_or(elapsed);
                    Some((StallKind::ExecutingToolsNoProgress, stall_elapsed))
                } else {
                    None
                };

            if let Some((kind, stall_elapsed)) = stall_info {
                let (notify_count, _) = stall_planner_notifications(card);
                drop(session);

                if notify_count >= MAX_STALL_PLANNER_NOTIFICATIONS_PER_CARD {
                    mark_agent_as_failed(
app.ctx.clone(),
                        task_id,
                        &card.id,
                        card.assignee.as_deref(),
                        planner_chat_id.as_deref(),
                        &format!(
                            "{} for {}, planner-notify retries exhausted",
                            kind.short_reason(),
                            humantime::format_duration(stall_elapsed)
                        ),
                        AgentFailureKind::TransientExhausted,
                    )
                    .await?;
                } else if stall_planner_notify_allowed(card, now) {
                    if let Err(error) = notify_agent_about_stall(
app.ctx.clone(),
                        task_id,
                        card,
                        agent_chat_id,
                        kind,
                        stall_elapsed,
                    )
                    .await
                    {
                        tracing::warn!(
                            "Failed to notify stalled agent {} on card {} in task {}: {}",
                            agent_chat_id,
                            card.id,
                            task_id,
                            error
                        );
                    }
                    let notify_result = notify_planner_about_stalled_agent(
app.ctx.clone(),
                        task_id,
                        card,
                        agent_chat_id,
                        planner_chat_id.as_deref(),
                        kind,
                        stall_elapsed,
                    )
                    .await;
                    let planner_notified = match notify_result {
                        Ok(notified) => notified,
                        Err(error) => {
                            tracing::warn!(
                                "Failed to notify planner about stalled agent for card {} in task {}: {}",
                                card.id,
                                task_id,
                                error
                            );
                            false
                        }
                    };
                    if !planner_notified
                        && stalled_agent_should_fail_after_notify_failure(stall_elapsed)
                    {
                        mark_agent_as_failed(
app.ctx.clone(),
                            task_id,
                            &card.id,
                            card.assignee.as_deref(),
                            planner_chat_id.as_deref(),
                            &format!(
                                "{} for {}, planner notification unavailable",
                                kind.short_reason(),
                                humantime::format_duration(stall_elapsed)
                            ),
                            AgentFailureKind::TransientExhausted,
                        )
                        .await?;
                    }
                }
                continue;
            }

            // If agent is idle and hasn't done anything in a long time, might be stuck
            if session.runtime.state == SessionState::Idle
                && session.command_queue.is_empty()
                && session.pending_deliveries.is_empty()
                && elapsed > AGENT_STUCK_TIMEOUT
            {
                tracing::warn!(
                    "Agent for card {} appears stuck (idle for {:?})",
                    card.id,
                    elapsed
                );

                mark_agent_as_failed(
app.ctx.clone(),
                    task_id,
                    &card.id,
                    card.assignee.as_deref(),
                    planner_chat_id.as_deref(),
                    &format!(
                        "Agent stuck (idle with no activity for {})",
                        humantime::format_duration(elapsed)
                    ),
                    AgentFailureKind::Permanent,
                )
                .await?;
            }
            drop(session);

            for member in card.team() {
                if !member.is_active() {
                    continue;
                }
                let Some(member_chat_id) = member.agent_chat_id.as_deref() else {
                    continue;
                };
                if member_chat_id == agent_chat_id {
                    continue;
                }
                let member_session_arc = {
                    let sessions_read = sessions.read().await;
                    sessions_read.get(member_chat_id).cloned()
                };
                let Some(member_session_arc) = member_session_arc else {
                    continue;
                };
                let member_session = member_session_arc.lock().await;
                let member_elapsed = member_session.last_activity.elapsed();
                let member_is_stalled = matches!(
                    member_session.runtime.state,
                    SessionState::Idle | SessionState::Completed
                ) && member_session.command_queue.is_empty()
                    && member_session.pending_deliveries.is_empty()
                    && member_elapsed > AGENT_STUCK_TIMEOUT;
                if member_is_stalled {
                    drop(member_session);
                    tracing::warn!(
                        "Room member {} ({}) on card {} appears stuck (idle for {})",
                        member.role,
                        member_chat_id,
                        card.id,
                        humantime::format_duration(member_elapsed)
                    );
                    if stall_planner_notify_allowed(card, Utc::now()) {
                        let kind = match member_session_arc.lock().await.runtime.state {
                            SessionState::Completed => StallKind::Completed,
                            _ => StallKind::IdleNoFinish,
                        };
                        let _ = notify_agent_about_stall(
app.ctx.clone(),
                            task_id,
                            card,
                            member_chat_id,
                            kind,
                            member_elapsed,
                        )
                        .await;
                        let _ = notify_planner_about_stalled_agent(
app.ctx.clone(),
                            task_id,
                            card,
                            member_chat_id,
                            planner_chat_id.as_deref(),
                            kind,
                            member_elapsed,
                        )
                        .await;
                    }
                }
            }
        }
    }

    sweep_planner_wake_ups(app).await?;

    Ok(())
}
