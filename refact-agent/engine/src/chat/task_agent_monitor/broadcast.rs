#[allow(unused_imports)]
use super::*;

pub(super) fn task_agent_monitor_notice(payload: serde_json::Value, content: String) -> ChatMessage {
    event(
        EventSubkind::SystemNotice,
        TASK_AGENT_MONITOR_SOURCE,
        payload,
        content,
    )
}

/// A legitimate planner session can come back without `task_meta` when its first
/// trajectory write raced a restart/eviction, or when the stored `task_meta` JSON
/// failed to deserialize after a schema change; both load the trajectory fine
/// otherwise. Every caller here already knows the planner chat id and task id, so
/// re-verify against durable task storage and self-heal the metadata instead of
/// silently dropping the notification. When the chat id is not a known planner
/// trajectory for the task, record a durable trace on the card (when known) so the
/// failure is visible to the planner via task_list rather than only in logs.
pub(super) async fn ensure_planner_task_meta(
    ctx: ChatCtx,
    planner_session: &Arc<AMutex<ChatSession>>,
    task_id: &str,
    planner_chat_id: &str,
    card_id: Option<&str>,
    notification_kind: &str,
) -> Result<(), String> {
    {
        let session = planner_session.lock().await;
        if session.thread.task_meta.is_some() {
            return Ok(());
        }
    }
    let known_planner = storage::list_task_trajectories_for_ctx(
        &ctx,
        task_id,
        "planner",
        None,
        crate::chat::trajectory_index::TrajectoryIndexListingCaller::TaskAgentMonitor,
    )
    .await
    .map(|trajectories| {
        trajectories
            .iter()
            .any(|trajectory| trajectory.id == planner_chat_id)
    })
    .unwrap_or(false);
    if known_planner || planner_chat_id.starts_with(&format!("planner-{}-", task_id)) {
        tracing::error!(
            "Planner session {} for task {} lost its task_meta (missing trajectory or schema drift); self-healing planner metadata before '{}' notification",
            planner_chat_id,
            task_id,
            notification_kind
        );
        let mut session = planner_session.lock().await;
        session.thread.task_meta = Some(TaskMeta {
            task_id: task_id.to_string(),
            role: "planner".to_string(),
            agent_id: None,
            card_id: None,
            planner_chat_id: Some(planner_chat_id.to_string()),
        });
        return Ok(());
    }
    let error = format!(
        "Cannot notify task planner {} for task {}: chat is not a known planner trajectory; '{}' notification dropped",
        planner_chat_id, task_id, notification_kind
    );
    tracing::error!("{}", error);
    if let Some(card_id) = card_id {
        let card_id = card_id.to_string();
        let trace = format!(
            "[NOTIFY-FAILED:{}] planner {} unreachable (no planner metadata); notification dropped",
            notification_kind, planner_chat_id
        );
        let _ = storage::update_board_atomic_for_ctx(&ctx, task_id, move |board| {
            if let Some(card) = board.get_card_mut(&card_id) {
                card.status_updates.push(StatusUpdate {
                    timestamp: Utc::now().to_rfc3339(),
                    message: trace,
                });
            }
            Ok(())
        })
        .await;
    }
    Err(error)
}

/// Wake the planner for a blocking question raised via `agent_ask_planner`.
/// The question itself is already durably recorded on the card as an `[ASK:...]`
/// status update; the chosen push mode controls when the notice lands, while
/// wake=true resumes an idle planner instead of relying on its next poll.
pub(crate) async fn notify_blocking_question(
    ctx: ChatCtx,
    task_id: &str,
    card_id: &str,
    planner_chat_id: &str,
    question_id: &str,
    question: &str,
    push: PushMode,
) -> Result<DeliveryOutcome, String> {
    let notice = task_agent_monitor_notice(
        json!({
            "kind": "blocking_question",
            "task_id": task_id,
            "card_id": card_id,
            "question_id": question_id,
        }),
        format!(
            "Task agent raised a BLOCKING question on card {} (question {}). Answer it with planner_reply(card_id=\"{}\", question_id=\"{}\", answer=\"...\").\n\nQuestion:\n{}",
            card_id, question_id, card_id, question_id, question
        ),
    );

    let sessions = ctx.chat.sessions.clone();
    let planner_session =
        get_or_create_session_with_trajectory(ctx.clone(), &sessions, planner_chat_id).await;

    ensure_planner_task_meta(
        ctx.clone(),
        &planner_session,
        task_id,
        planner_chat_id,
        Some(card_id),
        "blocking_question",
    )
    .await?;

    ctx.chat
        .facade
        .deliver_messages(
            planner_chat_id,
            PendingDelivery::with_id(
                format!("task-agent-blocking-question:{task_id}:{card_id}:{question_id}"),
                vec![notice],
                push,
                TASK_AGENT_MONITOR_SOURCE,
                true,
            ),
        )
        .await
}

pub(super) fn make_runtime_event(
    signal_type: &str,
    title: &str,
    source: &str,
    dedupe_key: &str,
    status: &str,
    priority: Option<&str>,
) -> BuddyRuntimeEvent {
    BuddyRuntimeEvent {
        id: Uuid::new_v4().to_string(),
        signal_type: signal_type.to_string(),
        title: title.to_string(),
        description: None,
        source: source.to_string(),
        status: status.to_string(),
        failure_category: None,
        failure_summary: None,
        progress: None,
        dedupe_key: Some(dedupe_key.to_string()),
        priority: priority.unwrap_or("normal").to_string(),
        created_at: Utc::now().to_rfc3339(),
        ttl_ms: None,
        bubble_policy: None,
        speech_text: None,
        scene: None,
        duration_hint: None,
        persistent: false,
        controls: Vec::new(),
        chat_id: None,
        dismissed: false,
    }
}

/// Count and timestamp the status updates recorded under one notification prefix.
/// Each family (planner escalation, agent nudge) is counted on its own prefix, so a
/// nudge aimed at the agent can never eat the planner's notification budget.
pub(super) fn stall_notifications_with_prefix(
    card: &BoardCard,
    prefix: &str,
) -> (usize, Option<chrono::DateTime<Utc>>) {
    let mut count = 0usize;
    let mut latest = None;

    for update in &card.status_updates {
        if !update.message.starts_with(prefix) {
            continue;
        }
        count += 1;
        if let Ok(parsed) = chrono::DateTime::parse_from_rfc3339(&update.timestamp) {
            let parsed = parsed.with_timezone(&Utc);
            latest = Some(match latest {
                Some(existing) if existing > parsed => existing,
                _ => parsed,
            });
        }
    }

    (count, latest)
}

pub(super) fn stall_planner_notifications(card: &BoardCard) -> (usize, Option<chrono::DateTime<Utc>>) {
    stall_notifications_with_prefix(card, STALL_PLANNER_NOTIFY_STATUS_PREFIX)
}

#[cfg(test)]
pub(super) fn stall_agent_notifications(card: &BoardCard) -> (usize, Option<chrono::DateTime<Utc>>) {
    stall_notifications_with_prefix(card, STALL_AGENT_NOTIFY_STATUS_PREFIX)
}

pub(super) fn stall_planner_notify_allowed(card: &BoardCard, now: chrono::DateTime<Utc>) -> bool {
    stall_notify_allowed(
        stall_planner_notifications(card),
        MAX_STALL_PLANNER_NOTIFICATIONS_PER_CARD,
        now,
    )
}

pub(super) fn stall_notify_allowed(
    (count, latest): (usize, Option<chrono::DateTime<Utc>>),
    max_notifications: usize,
    now: chrono::DateTime<Utc>,
) -> bool {
    if count >= max_notifications {
        return false;
    }
    if let Some(latest) = latest {
        let since = now.signed_duration_since(latest).num_seconds();
        if since < STALL_PLANNER_NOTIFY_COOLDOWN_SECONDS {
            return false;
        }
    }
    true
}

pub(super) fn stalled_agent_should_fail_after_notify_failure(stall_elapsed: Duration) -> bool {
    stall_elapsed >= AGENT_STUCK_TIMEOUT
}

pub(super) fn usage_summary(usage: Option<&ChatUsage>) -> String {
    usage
        .map(|usage| {
            let mut parts = vec![
                format!("prompt={}", usage.prompt_tokens),
                format!("completion={}", usage.completion_tokens),
                format!("total={}", usage.total_tokens),
            ];
            if let Some(cache_read) = usage.cache_read_tokens {
                parts.push(format!("cache_read={cache_read}"));
            }
            if let Some(cache_creation) = usage.cache_creation_tokens {
                parts.push(format!("cache_creation={cache_creation}"));
            }
            parts.join(", ")
        })
        .unwrap_or_else(|| "unavailable".to_string())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum StallKind {
    IdleNoFinish,
    Completed,
    GeneratingNoTokens,
    ExecutingToolsNoProgress,
}

impl StallKind {
    pub(super) fn short_reason(self) -> &'static str {
        match self {
            StallKind::IdleNoFinish => "idle, stream finished without agent_finish",
            StallKind::Completed => "Completed state but card still in doing",
            StallKind::GeneratingNoTokens => "stream appears stalled, no token activity",
            StallKind::ExecutingToolsNoProgress => "tool execution stalled, no tool progress",
        }
    }
}

/// Append one `prefix`-scoped status update to the card, guarding against the
/// card leaving `doing` and against exceeding that prefix's budget/cooldown.
pub(super) async fn record_stall_notification(
    ctx: &ChatCtx,
    task_id: &str,
    card_id: &str,
    agent_chat_id: &str,
    prefix: &str,
    max_notifications: usize,
    detail: &str,
) -> Result<bool, String> {
    let card_id_owned = card_id.to_string();
    let agent_chat_id_owned = agent_chat_id.to_string();
    let now = Utc::now();
    let timestamp = now.to_rfc3339();
    let message = format!("{} {} ({})", prefix, agent_chat_id, detail);
    let prefix = prefix.to_string();

    storage::update_board_atomic_for_ctx(ctx, task_id, move |board| {
        let card = board
            .get_card_mut(&card_id_owned)
            .ok_or_else(|| format!("Card {} not found", card_id_owned))?;
        if card.column != "doing"
            || !card_accepts_agent_chat_id(card, &agent_chat_id_owned)
            || card.assignee.is_none()
            || !stall_notify_allowed(
                stall_notifications_with_prefix(card, &prefix),
                max_notifications,
                now,
            )
        {
            return Ok(false);
        }
        card.last_heartbeat_at = Some(timestamp.clone());
        card.status_updates.push(StatusUpdate {
            timestamp: timestamp.clone(),
            message: message.clone(),
        });
        Ok(true)
    })
    .await
    .map(|(_, recorded)| recorded)
}

pub(super) async fn record_stall_planner_notification(
    ctx: &ChatCtx,
    task_id: &str,
    card_id: &str,
    agent_chat_id: &str,
    reason: &str,
) -> Result<bool, String> {
    record_stall_notification(
        ctx,
        task_id,
        card_id,
        agent_chat_id,
        STALL_PLANNER_NOTIFY_STATUS_PREFIX,
        MAX_STALL_PLANNER_NOTIFICATIONS_PER_CARD,
        reason,
    )
    .await
}

pub(super) fn build_stalled_agent_planner_message(
    card_id: &str,
    card_title: &str,
    agent_chat_id: &str,
    assignee: Option<&str>,
    kind: StallKind,
    stalled_for: Duration,
) -> String {
    let header = match kind {
        StallKind::GeneratingNoTokens => "**Task agent stream stalled.**",
        StallKind::ExecutingToolsNoProgress => "**Task agent tool execution stalled.**",
        StallKind::IdleNoFinish => "**Task agent appears stalled.**",
        StallKind::Completed => {
            "**Task agent reached Completed state but the card is still in `doing`.**"
        }
    };

    let state_line = match kind {
        StallKind::GeneratingNoTokens => format!(
            "State: Generating — no tokens received for {}.",
            humantime::format_duration(stalled_for)
        ),
        StallKind::ExecutingToolsNoProgress => format!(
            "State: ExecutingTools — no tool progress for {}.",
            humantime::format_duration(stalled_for)
        ),
        StallKind::IdleNoFinish => format!(
            "State: Idle — stream finished but `agent_finish` was not called. Idle for {}.",
            humantime::format_duration(stalled_for)
        ),
        StallKind::Completed => format!(
            "State: Completed — finish-like tool reached the agent but the card was not updated. Idle for {}.",
            humantime::format_duration(stalled_for)
        ),
    };

    let context_line = match kind {
        StallKind::GeneratingNoTokens => {
            "The model is connected but is not producing output. This often happens when a reasoning model gets stuck without progress, hits a thinking budget, or the provider stops sending data without an error."
        }
        StallKind::ExecutingToolsNoProgress => {
            "A tool started but has not reported progress for a long time. The tool process may be hung, waiting on input, or otherwise unresponsive."
        }
        StallKind::IdleNoFinish => {
            "The agent stopped producing output without calling `agent_finish`. This often happens when the model hits a token/thinking budget or returns an empty response."
        }
        StallKind::Completed => {
            "The agent emitted a finish-like tool (e.g. `agent_finish`) but the card was not moved out of `doing`. The agent's work may be effectively done already, or the transition may have failed."
        }
    };

    let assignee_line = assignee
        .map(|a| format!(" (assignee: `{}`)", a))
        .unwrap_or_default();

    format!(
        "{}\n\nCard: `{}` ({}){}\nAgent chat: `{}`\n{}\n\n{}\n\nOptions:\n\
1. Send a continue/guidance message to the agent.\n\
2. Restart the agent with `restart_agent(card_id=\"{}\", instructions=\"...\")`.\n\
3. Inspect via `agent_pulse(card_id=\"{}\")` or `board_get(card_id=\"{}\")`.\n\
4. If the task cannot proceed, move the card or mark it accordingly.",
        header,
        card_id,
        card_title,
        assignee_line,
        agent_chat_id,
        state_line,
        context_line,
        card_id,
        card_id,
        card_id,
    )
}

/// The concrete next action, so the agent does not merely learn that it stalled.
pub(super) fn build_stalled_agent_self_message(
    card: &BoardCard,
    kind: StallKind,
    stalled_for: Duration,
) -> String {
    let state = match kind {
        StallKind::IdleNoFinish => format!(
            "Your last turn ended without calling any tool, and you have been idle for {}.",
            humantime::format_duration(stalled_for)
        ),
        StallKind::Completed => format!(
            "You emitted a finish-like tool but card `{}` was never updated, and you have been idle for {}.",
            card.id,
            humantime::format_duration(stalled_for)
        ),
        StallKind::GeneratingNoTokens => format!(
            "Your stream produced no tokens for {}.",
            humantime::format_duration(stalled_for)
        ),
        StallKind::ExecutingToolsNoProgress => format!(
            "A tool you started has reported no progress for {}.",
            humantime::format_duration(stalled_for)
        ),
    };

    format!(
        "{state}\n\nNothing was recorded for card `{card_id}`, so the work looks unfinished to the planner.\n\n\
Do exactly one of these now:\n\
1. The card is done — call `agent_finish` with the report.\n\
2. Work remains — continue with a real tool call (never a bare text answer).\n\
3. You are blocked — call `agent_ask_planner` with `urgency=\"block\"` and say what you need.",
        card_id = card.id,
    )
}

/// Every chat this card owns: the primary `agent_chat_id` plus the active
/// members of a room card. Duplicates and blanks are dropped.
pub(super) fn stalled_agent_chat_targets(card: &BoardCard) -> Vec<String> {
    let mut targets: Vec<String> = Vec::new();
    let mut push = |chat_id: Option<&str>| {
        let Some(chat_id) = chat_id.map(str::trim).filter(|id| !id.is_empty()) else {
            return;
        };
        if !targets.iter().any(|existing| existing == chat_id) {
            targets.push(chat_id.to_string());
        }
    };
    push(card.agent_chat_id.as_deref());
    for member in card.team() {
        if member.is_active() {
            push(member.agent_chat_id.as_deref());
        }
    }
    targets
}

/// Wake the agent itself, not just the planner. The agent is the only party that
/// can unstick the card: the planner can restart or reassign, but it cannot make
/// the agent finish its own turn.
///
/// `PushMode::WhenIdle` (never `Append`): the notice must not land inside an open
/// assistant/tool-result window, and it must wait for the whole multi-step turn to
/// end so it cannot split a turn that is still making progress. This has a real
/// interaction with `agent_session_idle_stall_ready`: that predicate refuses a
/// session with a non-empty `pending_deliveries`, so a queued nudge suppresses the
/// next stall detection until the nudge lands and the session drains again.
pub(super) async fn notify_agent_about_stall(
    ctx: ChatCtx,
    task_id: &str,
    card: &BoardCard,
    agent_chat_id: &str,
    kind: StallKind,
    stalled_for: Duration,
) -> Result<bool, String> {
    let targets = stalled_agent_chat_targets(card);
    if targets.is_empty() {
        return Ok(false);
    }

    let message = build_stalled_agent_self_message(card, kind, stalled_for);
    let mut notified = false;

    for target in targets {
        // The budget is tracked against the primary agent chat so a room card does
        // not get one nudge per member per pass.
        if !record_stall_notification(
            &ctx,
            task_id,
            &card.id,
            agent_chat_id,
            STALL_AGENT_NOTIFY_STATUS_PREFIX,
            MAX_STALL_AGENT_NOTIFICATIONS_PER_CARD,
            kind.short_reason(),
        )
        .await?
        {
            return Ok(notified);
        }

        let notice = task_agent_monitor_notice(
            json!({
                "kind": "stalled_agent_self",
                "task_id": task_id,
                "card_id": card.id.as_str(),
                "agent_chat_id": target.as_str(),
                "stall_kind": format!("{:?}", kind),
                "reason": kind.short_reason(),
                "stalled_for_secs": stalled_for.as_secs(),
            }),
            message.clone(),
        );

        match ctx
            .chat
            .facade
            .deliver_messages(
                &target,
                PendingDelivery::new(
                    vec![notice],
                    PushMode::WhenIdle,
                    TASK_AGENT_MONITOR_SOURCE,
                    true,
                ),
            )
            .await
        {
            Ok(_) => {
                notified = true;
                tracing::info!(
                    "Notified stalled agent {} on card {} in task {} ({}): {}",
                    target,
                    card.id,
                    task_id,
                    kind.short_reason(),
                    agent_chat_id
                );
            }
            Err(error) => {
                // Recorded above, so this is already durable on the card; the log
                // names the target because a room card may have several.
                tracing::warn!(
                    "Failed to notify stalled agent {} on card {} in task {}: {}",
                    target,
                    card.id,
                    task_id,
                    error
                );
                return Ok(notified);
            }
        }
    }

    Ok(notified)
}

/// Record an escalation that had nowhere to go. Silence here is what made a
/// stalled agent look like a healthy wait: the planner was never told, and the
/// card showed nothing either.
pub(super) async fn record_stall_escalation(
    ctx: &ChatCtx,
    task_id: &str,
    card_id: &str,
    agent_chat_id: &str,
    detail: &str,
) {
    let card_id_owned = card_id.to_string();
    let message = format!(
        "{} {} ({})",
        STALL_ESCALATION_STATUS_PREFIX, agent_chat_id, detail
    );
    let _ = storage::update_board_atomic_for_ctx(ctx, task_id, move |board| {
        if let Some(card) = board.get_card_mut(&card_id_owned) {
            card.status_updates.push(StatusUpdate {
                timestamp: Utc::now().to_rfc3339(),
                message: message.clone(),
            });
        }
        Ok(())
    })
    .await;
}

pub(super) async fn notify_planner_about_stalled_agent(
    ctx: ChatCtx,
    task_id: &str,
    card: &BoardCard,
    agent_chat_id: &str,
    planner_chat_id: Option<&str>,
    kind: StallKind,
    stalled_for: Duration,
) -> Result<bool, String> {
    let card_id = card.id.as_str();
    let short_reason = kind.short_reason();

    let Some(planner_chat_id) = planner_chat_id else {
        // "Nowhere to deliver" is not "delivered": the planner was never told and
        // the card showed nothing, which is exactly the silence this path must not
        // have. Record it durably so the UI and `board_get` show the escalation.
        tracing::error!(
            "Cannot notify planner about stalled agent for card {} in task {} ({}): no planner_chat_id; escalation recorded on the card",
            card_id,
            task_id,
            short_reason
        );
        record_stall_escalation(
            &ctx,
            task_id,
            card_id,
            agent_chat_id,
            &format!(
                "{}; planner chat id unknown, escalation not delivered",
                short_reason
            ),
        )
        .await;
        return Ok(false);
    };

    let message = build_stalled_agent_planner_message(
        card_id,
        card.title.as_str(),
        agent_chat_id,
        card.assignee.as_deref(),
        kind,
        stalled_for,
    );
    let notice = task_agent_monitor_notice(
        json!({
            "kind": "stalled_agent",
            "task_id": task_id,
            "card_id": card_id,
            "card_title": card.title.as_str(),
            "agent_chat_id": agent_chat_id,
            "assignee": card.assignee.as_deref(),
            "stall_kind": format!("{:?}", kind),
            "reason": short_reason,
            "stalled_for_secs": stalled_for.as_secs(),
        }),
        message,
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
        "stalled_agent",
    )
    .await
    .is_err()
    {
        tracing::error!(
            "Planner {} for task {} is not a usable planner chat; stalled-agent escalation for card {} recorded on the card",
            planner_chat_id,
            task_id,
            card_id
        );
        record_stall_escalation(
            &ctx,
            task_id,
            card_id,
            agent_chat_id,
            &format!(
                "{}; planner {} is not a known planner chat, escalation not delivered",
                short_reason, planner_chat_id
            ),
        )
        .await;
        return Ok(false);
    }

    if !record_stall_planner_notification(
        &ctx,
        task_id,
        card_id,
        agent_chat_id,
        short_reason,
    )
    .await?
    {
        return Ok(false);
    }

    if let Err(error) = ctx
        .chat
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
        .await
    {
        // Distinct from "no planner chat": here the destination existed and the
        // transport failed, so the recorded detail carries the transport error.
        tracing::error!(
            "Failed to deliver stalled-agent escalation for card {} to planner {}: {}",
            card_id,
            planner_chat_id,
            error
        );
        record_stall_escalation(
            &ctx,
            task_id,
            card_id,
            agent_chat_id,
            &format!(
                "{}; delivery to planner {} failed: {} (the '{}' record above did not reach the planner)",
                short_reason,
                planner_chat_id,
                error,
                STALL_PLANNER_NOTIFY_STATUS_PREFIX
            ),
        )
        .await;
        return Ok(false);
    }

    tracing::info!(
        "Notified planner {} about stalled agent for card {} ({}): {}",
        planner_chat_id,
        card_id,
        agent_chat_id,
        short_reason
    );

    Ok(true)
}

/// Notify planner about newly finished agents without waiting for the full batch to end.
pub async fn notify_agents_finished(
    ctx: ChatCtx,
    task_id: &str,
    board: &crate::tasks::types::TaskBoard,
    all_finished: bool,
    planner_chat_id: Option<&str>,
) -> Result<(), String> {
    let since = match storage::load_task_meta_for_ctx(&ctx, task_id).await {
        Ok(meta) => meta
            .last_agents_summary_at
            .as_deref()
            .and_then(|ts| chrono::DateTime::parse_from_rfc3339(ts).ok())
            .map(|dt| dt.with_timezone(&Utc)),
        Err(_) => None,
    };

    let mut results = Vec::new();
    for card in &board.cards {
        if let Some(variants) = card.ab_variants.as_ref() {
            for (variant_key, variant) in [("a", &variants.a), ("b", &variants.b)] {
                let Some(finish) = variant.finish.as_ref() else {
                    continue;
                };
                if let Some(ref since_dt) = since {
                    let Some(completed_at) =
                        chrono::DateTime::parse_from_rfc3339(&finish.completed_at)
                            .ok()
                            .map(|dt| dt.with_timezone(&Utc))
                    else {
                        continue;
                    };
                    if completed_at < *since_dt {
                        continue;
                    }
                }
                let status = if finish.success {
                    "✅ done"
                } else {
                    "❌ failed"
                };
                let report_preview: String = finish.final_report.chars().take(200).collect();
                results.push(format!(
                    "**{}/{} ({} [A/B {}])**: {}\n{}",
                    card.id,
                    variant_key,
                    card.title,
                    variant_key.to_ascii_uppercase(),
                    status,
                    report_preview
                ));
            }
            if card.agent_chat_id.is_none() {
                continue;
            }
        }
        if card.agent_chat_id.is_none() {
            continue;
        }
        if let Some(ref since_dt) = since {
            let Some(completed_at) = card
                .completed_at
                .as_deref()
                .and_then(|ts| chrono::DateTime::parse_from_rfc3339(ts).ok())
                .map(|dt| dt.with_timezone(&Utc))
            else {
                continue;
            };
            if completed_at < *since_dt {
                continue;
            }
        }
        let status = if card.column == "done" {
            "✅ done"
        } else if card.column == "failed" {
            "❌ failed"
        } else {
            continue;
        };
        let report_preview: String = card
            .final_report
            .as_deref()
            .unwrap_or("")
            .chars()
            .take(200)
            .collect();
        results.push(format!(
            "**{} ({})**: {}\n{}",
            card.id, card.title, status, report_preview
        ));
    }

    let heading = if all_finished {
        "**All agents have completed.**"
    } else {
        "**Task agent finished.**"
    };
    let footer = if all_finished {
        "Run `board_get(card_id)` to see full details for any card."
    } else {
        "Other agents may still be running. Run `check_agents` to see live status or `board_get(card_id)` for full details."
    };

    let planner_message = format!(
        "{}\n\n{}\n\n{}",
        heading,
        if results.is_empty() {
            let note = since
                .as_ref()
                .map(|dt| dt.to_rfc3339())
                .unwrap_or_else(|| "(unknown)".to_string());
            format!("_(No newly-finished cards detected since {}.)_", note)
        } else {
            results.join("\n\n")
        },
        footer
    );
    let notice = task_agent_monitor_notice(
        json!({
            "kind": "agents_finished",
            "task_id": task_id,
            "all_finished": all_finished,
            "results": results,
            "since": since.as_ref().map(|dt| dt.to_rfc3339()),
        }),
        planner_message,
    );

    let sessions = ctx.chat.sessions.clone();

    let planner_chat_id = if let Some(id) = planner_chat_id {
        id.to_string()
    } else {
        let agent_session_arcs = {
            let sessions_guard = sessions.read().await;
            board
                .cards
                .iter()
                .filter(|card| card.column == "done" || card.column == "failed")
                .filter_map(|card| card.agent_chat_id.as_deref())
                .filter_map(|agent_chat_id| sessions_guard.get(agent_chat_id).cloned())
                .collect::<Vec<_>>()
        };
        let mut planner_chat_id = None;
        for session_arc in agent_session_arcs {
            let session = session_arc.lock().await;
            planner_chat_id = session
                .thread
                .task_meta
                .as_ref()
                .and_then(|meta| meta.planner_chat_id.clone());
            if planner_chat_id.is_some() {
                break;
            }
        }
        if planner_chat_id.is_none() {
            planner_chat_id = storage::list_task_trajectories_for_ctx(
                &ctx,
                task_id,
                "planner",
                None,
                crate::chat::trajectory_index::TrajectoryIndexListingCaller::TaskAgentMonitor,
            )
            .await
            .ok()
            .and_then(|trajectories| {
                let prefix = format!("planner-{}-", task_id);
                trajectories
                    .into_iter()
                    .max_by_key(|t| {
                        (
                            t.id.strip_prefix(&prefix)
                                .and_then(|s| s.parse::<u32>().ok())
                                .unwrap_or(0),
                            t.updated_at.clone(),
                        )
                    })
                    .map(|t| t.id)
            });
        }
        planner_chat_id.ok_or_else(|| {
            format!(
                "Cannot notify task planner for task {}: finished agent has no planner_chat_id",
                task_id
            )
        })?
    };
    let planner_session =
        get_or_create_session_with_trajectory(ctx.clone(), &sessions, &planner_chat_id).await;
    ensure_planner_task_meta(
        ctx.clone(),
        &planner_session,
        task_id,
        &planner_chat_id,
        None,
        "agents_finished",
    )
    .await?;

    ctx.chat
        .facade
        .deliver_messages(
            &planner_chat_id,
            PendingDelivery::new(
                vec![notice],
                PushMode::Append,
                TASK_AGENT_MONITOR_SOURCE,
                true,
            ),
        )
        .await?;

    // Best-effort: mark summary as emitted.
    if let Ok(mut meta) = storage::load_task_meta_for_ctx(&ctx, task_id).await {
        meta.last_agents_summary_at = Some(Utc::now().to_rfc3339());
        let _ = storage::save_task_meta_for_ctx(&ctx, task_id, &meta).await;
    }

    Ok(())
}

pub struct EngineChatAgentHost {
    gcx: crate::global_context::SharedGlobalContext,
}

impl EngineChatAgentHost {
    pub fn new(gcx: crate::global_context::SharedGlobalContext) -> Self {
        Self { gcx }
    }

    fn ctx(&self) -> ChatCtx {
        use crate::app_state::GlobalContextCtx;
        self.gcx.chat_ctx(self.gcx.clone())
    }
}

#[async_trait]
impl ChatAgentHost for EngineChatAgentHost {
    async fn notify_agents_finished(
        &self,
        task_id: &str,
        board: &crate::tasks::types::TaskBoard,
        all_finished: bool,
        planner_chat_id: Option<&str>,
    ) -> Result<(), String> {
        notify_agents_finished(self.ctx(), task_id, board, all_finished, planner_chat_id).await
    }

    async fn notify_blocking_question(
        &self,
        task_id: &str,
        card_id: &str,
        planner_chat_id: &str,
        question_id: &str,
        question: &str,
        push: PushMode,
    ) -> Result<DeliveryOutcome, String> {
        notify_blocking_question(
            self.ctx(),
            task_id,
            card_id,
            planner_chat_id,
            question_id,
            question,
            push,
        )
        .await
    }

    async fn deliver_pending(
        &self,
        chat_id: &str,
        delivery: PendingDelivery,
    ) -> Result<DeliveryOutcome, String> {
        let app = crate::app_state::app_state_from_gcx(self.gcx.clone()).await;
        crate::chat::delivery::deliver_pending(app, chat_id, delivery).await
    }
}

/// Return the wall-clock timestamp of the last recorded activity for the
/// given agent chat session. Returns `None` when the session no longer exists.
pub async fn get_last_agent_heartbeat(
    ctx: ChatCtx,
    agent_chat_id: &str,
) -> Option<chrono::DateTime<Utc>> {
    let sessions = ctx.chat.sessions.clone();
    let sessions_read = sessions.read().await;
    let session_arc = sessions_read.get(agent_chat_id)?.clone();
    drop(sessions_read);
    let session = session_arc.lock().await;
    let elapsed = session.last_activity.elapsed();
    drop(session);
    let heartbeat = Utc::now() - chrono::Duration::from_std(elapsed).ok()?;
    Some(heartbeat)
}
