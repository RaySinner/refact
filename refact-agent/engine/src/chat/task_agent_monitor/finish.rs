#[allow(unused_imports)]
use super::*;

pub(super) fn reasoning_token_limit_resume_prompt(
    card_id: &str,
    finish_reason: &str,
    usage: Option<&ChatUsage>,
) -> String {
    format!(
        "Agent `{card_id}` stopped while reasoning before producing an answer or tool call. \
Finish reason: `{finish_reason}`. Token usage: {}.\n\n\
Please inspect this agent, compact its chat if needed, then resume it with a short concrete message. \
A good recovery path is: run `agent_pulse(card_id=\"{card_id}\")`, use `agent_steer` with a concise continuation prompt, or cancel/mark the card failed if it cannot continue.",
        usage_summary(usage)
    )
}

pub(super) async fn notify_planner_about_reasoning_token_limit(
    ctx: ChatCtx,
    task_id: &str,
    card_id: &str,
    agent_chat_id: &str,
    planner_chat_id: &str,
    prompt: String,
    finish_reason: &str,
    usage: Option<&ChatUsage>,
    message_id: &str,
) -> Result<bool, String> {
    let notice = task_agent_monitor_notice(
        json!({
            "kind": "reasoning_token_limit",
            "task_id": task_id,
            "card_id": card_id,
            "agent_chat_id": agent_chat_id,
            "finish_reason": finish_reason,
            "usage": usage.map(|usage| json!({
                "prompt_tokens": usage.prompt_tokens,
                "completion_tokens": usage.completion_tokens,
                "total_tokens": usage.total_tokens,
                "cache_read_tokens": usage.cache_read_tokens,
                "cache_creation_tokens": usage.cache_creation_tokens,
            })),
            "message_id": message_id,
        }),
        prompt,
    );

    let sessions = ctx.chat.sessions.clone();
    let planner_session =
        get_or_create_session_with_trajectory(ctx.clone(), &sessions, planner_chat_id).await;

    if ensure_planner_task_meta(
        ctx.clone(),
        &planner_session,
        task_id,
        planner_chat_id,
        Some(card_id),
        "reasoning_token_limit",
    )
    .await
    .is_err()
    {
        return Ok(false);
    }

    if !record_stall_planner_notification(
        &ctx,
        task_id,
        card_id,
        agent_chat_id,
        "reasoning_token_limit",
    )
    .await?
    {
        return Ok(false);
    }

    ctx.chat
        .facade
        .deliver_messages(
            planner_chat_id,
            PendingDelivery::new(
                vec![notice],
                PushMode::Append,
                TASK_AGENT_MONITOR_SOURCE,
                true,
            ),
        )
        .await?;

    Ok(true)
}

pub async fn handle_agent_reasoning_token_limit_stop(
    ctx: ChatCtx,
    task_meta: TaskMeta,
    finish_reason: String,
    usage: Option<ChatUsage>,
    message_id: String,
    agent_chat_id: String,
) -> Result<(), String> {
    if task_meta.role != "agents" {
        return Ok(());
    }
    let Some(card_id) = task_meta.card_id.clone() else {
        return Ok(());
    };
    let Some(planner_chat_id) = task_meta.planner_chat_id.clone() else {
        return Ok(());
    };

    let prompt = reasoning_token_limit_resume_prompt(&card_id, &finish_reason, usage.as_ref());
    let planner_notified = notify_planner_about_reasoning_token_limit(
        ctx.clone(),
        &task_meta.task_id,
        &card_id,
        &agent_chat_id,
        &planner_chat_id,
        prompt,
        &finish_reason,
        usage.as_ref(),
        &message_id,
    )
    .await?;

    let notification_status = if planner_notified {
        "Planner notified for compaction/resume."
    } else {
        "Planner notification skipped because the card/session changed or a duplicate notice was already sent."
    };
    let update_message = format!(
        "Agent stopped while reasoning before producing an answer/tool call (finish_reason={finish_reason}, usage={}). {notification_status} message_id={message_id}",
        usage_summary(usage.as_ref())
    );
    let _ = storage::update_board_atomic_for_ctx(&ctx, &task_meta.task_id, move |board| {
        if let Some(card) = board.get_card_mut(&card_id) {
            card.status_updates.push(StatusUpdate {
                timestamp: Utc::now().to_rfc3339(),
                message: update_message.clone(),
            });
        }
        Ok(())
    })
    .await;

    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AgentFailureKind {
    TransientExhausted,
    ContextLimit,
    Permanent,
    Cancelled,
}

impl AgentFailureKind {
    pub(super) fn from_error(error_message: &str) -> Self {
        match classify_llm_error_for_retry(error_message) {
            RetryDecision::Retry { .. } => AgentFailureKind::TransientExhausted,
            RetryDecision::ContextLimit { .. } => AgentFailureKind::ContextLimit,
            RetryDecision::DoNotRetry { .. } => AgentFailureKind::Permanent,
            RetryDecision::UserCancelled { .. } => AgentFailureKind::Cancelled,
        }
    }

    pub(super) fn should_cleanup_worktree(self) -> bool {
        matches!(self, AgentFailureKind::Cancelled)
    }

    pub(super) fn final_report_reason(self, error_message: &str) -> String {
        let category = match self {
            AgentFailureKind::Cancelled => UserErrorCategory::Unknown,
            _ => classify_user_error(error_message),
        };
        let info = user_error_info(category);
        let retry_note = if info.is_retryable {
            "This error is usually retryable."
        } else {
            "This error is not expected to succeed by retrying unchanged."
        };
        let retention_note = match self {
            AgentFailureKind::TransientExhausted => {
                "Retries were exhausted; worktree retained for inspection or retry."
            }
            AgentFailureKind::ContextLimit => {
                "Provider context limit was reached; worktree retained so the task can be retried after compaction or with a smaller history."
            }
            AgentFailureKind::Permanent => "Worktree retained for inspection or retry via restart_agent.",
            AgentFailureKind::Cancelled => "The task was cancelled before the agent finished.",
        };

        format!(
            "{}\n\n{}\n\nSuggested action: {}\n{}\n{}\n\nRaw error: {}",
            info.title,
            info.explanation,
            format_user_error_action(info.suggested_action),
            retry_note,
            retention_note,
            error_message
        )
    }
}

pub(super) fn format_user_error_action(action: &str) -> &'static str {
    match action {
        "retry" => "Retry the task or wait for the provider/network to recover.",
        "compact" => {
            "Compact the chat, reduce attached context, or switch to a larger-context model."
        }
        "check_auth" => {
            "Check provider credentials, OAuth login, token scope, and provider configuration."
        }
        "switch_model" => "Select an available model or update the provider route configuration.",
        "check_billing" => "Check provider billing, credits, quota, and usage limits.",
        _ => "Review the raw provider error and adjust the request or task before retrying.",
    }
}

pub(super) async fn retry_agent_session(app: &AppState, session_arc: &Arc<AMutex<ChatSession>>) {
    let mut session = session_arc.lock().await;
    session.clear_stream_for_retry();
    session.task_agent_error = None;
    let request = refact_chat_api::CommandRequest {
        client_request_id: Uuid::new_v4().to_string(),
        priority: true,
        command: refact_chat_api::ChatCommand::UserMessage {
            content: json!("Continue"),
            attachments: vec![],
            context_files: vec![],
            suppress_auto_enrichment: false,
            client_message_id: None,
            push: None,
        },
    };
    session.enqueue_priority_command(request);
    let processor_running = session.queue_processor_running.clone();
    let queue_notify = session.queue_notify.clone();
    drop(session);
    if !processor_running.swap(true, std::sync::atomic::Ordering::SeqCst) {
        tokio::spawn(crate::chat::queue::process_command_queue(
            app.clone(),
            session_arc.clone(),
            processor_running,
        ));
    } else {
        queue_notify.notify_one();
    }
}

/// Detect if a session error should cause task agent failure
pub async fn handle_agent_streaming_error(
    app: AppState,
    task_meta: &TaskMeta,
    error_message: &str,
) {
    let Some(ref card_id) = task_meta.card_id else {
        tracing::warn!("Agent has no card_id in task_meta, cannot mark as failed");
        return;
    };

    tracing::error!(
        "Task agent streaming error detected for card {}: {}",
        card_id,
        error_message
    );

    let max_retries = crate::runtime_settings::current().task_agent_max_retries;

    let (should_retry, current_attempt) = {
        let card_id_owned = card_id.clone();
        let error_msg = error_message.to_string();
        storage::update_board_atomic_for_ctx(&app.ctx, &task_meta.task_id, move |board| {
            let mut info = (false, 0usize);
            if let Some(card) = board.get_card_mut(&card_id_owned) {
                if card.column == "doing" && card.retry_count < max_retries {
                    card.retry_count += 1;
                    info = (true, card.retry_count);
                    card.status_updates.push(StatusUpdate {
                        timestamp: Utc::now().to_rfc3339(),
                        message: format!(
                            "Retrying agent after streaming error (attempt {}/{}): {}",
                            card.retry_count, max_retries, error_msg
                        ),
                    });
                }
            }
            Ok(info)
        })
        .await
        .map(|(_, info)| info)
        .unwrap_or((false, 0))
    };

    if should_retry {
        tracing::warn!(
            "Retrying task agent card {} (attempt {}/{}): {}",
            card_id,
            current_attempt,
            max_retries,
            error_message
        );
        if let Ok(board) = storage::load_board_for_ctx(&app.ctx, &task_meta.task_id).await {
            if let Some(card) = board.get_card(card_id) {
                if let Some(ref agent_chat_id) = card.agent_chat_id {
                    let sessions = app.chat.sessions.clone();
                    let session_arc = {
                        let sessions_read = sessions.read().await;
                        sessions_read.get(agent_chat_id).cloned()
                    };
                    if let Some(session_arc) = session_arc {
                        retry_agent_session(&app, &session_arc).await;
                    } else {
                        tracing::warn!("Cannot retry agent {}: session not found", agent_chat_id);
                    }
                }
            }
        }
        return;
    }

    let failure_kind = AgentFailureKind::from_error(error_message);
    let failure_reason = failure_kind.final_report_reason(error_message);

    if let Err(e) = mark_agent_as_failed(
        app.ctx.clone(),
        &task_meta.task_id,
        card_id,
        task_meta.agent_id.as_deref(),
        task_meta.planner_chat_id.as_deref(),
        &failure_reason,
        failure_kind,
    )
    .await
    {
        tracing::error!("Failed to mark agent as failed: {}", e);
    }
}

/// Mark a task card as failed and notify planner
pub(super) async fn mark_agent_as_failed(
    ctx: ChatCtx,
    task_id: &str,
    card_id: &str,
    expected_agent_id: Option<&str>,
    planner_chat_id: Option<&str>,
    reason: &str,
    failure_kind: AgentFailureKind,
) -> Result<(), String> {
    let _ = update_card_heartbeat(ctx.clone(), task_id, card_id).await;

    let card_id_owned = card_id.to_string();
    let reason_clone = reason.to_string();
    let expected_agent_id_owned = expected_agent_id.map(|s| s.to_string());

    // The closure returns (card_title, actually_failed, all_finished).
    // actually_failed is true only when the card transitioned from "doing" to "failed".
    let (board, (card_title, actually_failed, all_finished)) =
        storage::update_board_atomic_for_ctx(&ctx, task_id, move |board| {
            let card = board
                .get_card_mut(&card_id_owned)
                .ok_or(format!("Card {} not found", card_id_owned))?;

            if card.column == "done" || card.column == "failed" {
                let card_title = card.title.clone();
                return Ok((card_title, false, false));
            }

            if card.column != "doing" {
                let card_title = card.title.clone();
                return Ok((card_title, false, false));
            }

            if let Some(ref expected_id) = expected_agent_id_owned {
                if card.assignee.as_ref() != Some(expected_id) {
                    tracing::warn!(
                        "Card {} assignee mismatch: expected {}, got {:?}. Skipping auto-fail.",
                        card_id_owned,
                        expected_id,
                        card.assignee
                    );
                    let card_title = card.title.clone();
                    return Ok((card_title, false, false));
                }
            }

            let card_title = card.title.clone();

            card.final_report = Some(format!("FAILED (automatic): {}", reason_clone));
            card.final_report_structured = None;
            card.column = "failed".to_string();
            card.completed_at = Some(Utc::now().to_rfc3339());
            card.status_updates.push(StatusUpdate {
                timestamp: Utc::now().to_rfc3339(),
                message: format!("Automatic failure detection: {}", reason_clone),
            });

            let _ = card;

            let agents_active_after = storage::active_agent_count(&board.cards);
            let all_finished = agents_active_after == 0;

            Ok((card_title, true, all_finished))
        })
        .await?;

    if !actually_failed {
        return Ok(());
    }

    storage::update_task_stats_for_ctx(&ctx, task_id).await?;

    tracing::info!("Marked agent for card {} as failed: {}", card_id, reason);

    {
        let ev = make_runtime_event(
            "task_failed",
            &format!("Agent failed: {}", card_title),
            "task_agent",
            &format!("task_agent_{}", card_id),
            "failed",
            Some("high"),
        );
        ctx.buddy_event_sink.enqueue_event(ev).await;
    }

    {
        let (worktree, branch, worktree_name) = if let Some(card) = board.get_card(card_id) {
            (
                card.agent_worktree.clone(),
                card.agent_branch.clone(),
                card.agent_worktree_name.clone(),
            )
        } else {
            (None, None, None)
        };
        let diff_report = if let (Some(ref wt), Some(ref br)) = (&worktree, &branch) {
            capture_failed_agent_diff(&ctx, wt, br, worktree_name.as_deref()).await
        } else {
            String::new()
        };
        let cleanup_result = if failure_kind.should_cleanup_worktree() {
            if let (Some(ref wt), Some(ref br)) = (&worktree, &branch) {
                let result = remove_agent_worktree_and_branch(
                    ctx.clone(),
                    task_id,
                    card_id,
                    wt,
                    br,
                    worktree_name.as_deref(),
                )
                .await;
                tracing::info!(
                    "Automatic cleanup for failed agent card {}: worktree_removed={}, branch_deleted={}",
                    card_id,
                    result.0,
                    result.1
                );
                Some(result)
            } else {
                Some((false, false))
            }
        } else {
            None
        };
        let retention_or_cleanup_note = match cleanup_result {
            Some((worktree_removed, branch_deleted)) => {
                let worktree_note = if worktree_removed {
                    "worktree removed"
                } else if worktree.is_some() {
                    "worktree removal not confirmed"
                } else {
                    "no worktree recorded"
                };
                let branch_note = if branch_deleted {
                    "branch deleted"
                } else if branch.is_some() {
                    "branch deletion not confirmed"
                } else {
                    "no branch recorded"
                };
                format!(
                    "\n\nAutomatic cleanup attempted: {}; {}.",
                    worktree_note, branch_note
                )
            }
            None => "\n\nWorktree and branch retained for inspection or retry via `restart_agent`."
                .to_string(),
        };
        let card_id_for_report = card_id.to_string();
        let clear_worktree_fields = cleanup_result
            .map(|(worktree_removed, _)| worktree_removed)
            .unwrap_or(false);
        let clear_branch_field = cleanup_result
            .map(|(_, branch_deleted)| branch_deleted)
            .unwrap_or(false);
        if let Err(e) = storage::update_board_atomic_for_ctx(&ctx, task_id, move |board| {
            if let Some(c) = board.get_card_mut(&card_id_for_report) {
                if let Some(ref mut report) = c.final_report {
                    if !diff_report.is_empty() {
                        report.push_str(&diff_report);
                    }
                    report.push_str(&retention_or_cleanup_note);
                }
                if clear_worktree_fields {
                    c.agent_worktree = None;
                    c.agent_worktree_name = None;
                }
                if clear_branch_field {
                    c.agent_branch = None;
                }
            } else {
                tracing::warn!(
                    "Cannot update final report for failed agent card {}: card disappeared",
                    card_id_for_report
                );
            }
            Ok(())
        })
        .await
        {
            tracing::warn!(
                "Failed to update final report for failed agent card {}: {}",
                card_id,
                e
            );
        }
    }

    let notification_board = storage::load_board_for_ctx(&ctx, task_id)
        .await
        .unwrap_or_else(|e| {
            tracing::warn!(
                "Failed to reload task board {} before planner notification: {}",
                task_id,
                e
            );
            board.clone()
        });

    if let Err(e) = notify_agents_finished(
        ctx.clone(),
        task_id,
        &notification_board,
        all_finished,
        planner_chat_id,
    )
    .await
    {
        tracing::warn!(
            "Marked agent for card {} as failed, but planner notification failed: {}",
            card_id,
            e
        );
    }

    Ok(())
}
