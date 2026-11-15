use std::time::{Duration, SystemTime, UNIX_EPOCH};

use refact_core::chat_types::{DeliveryOutcome, PendingDelivery, PushMode};
use serde::Serialize;
use serde_json::json;

use crate::app_state::AppState;
use refact_core::call_validation::{ChatContent, ChatMessage};
use crate::session_runtime::get_or_create_session_with_trajectory;
use refact_chat_api::internal_roles::{event, EventSubkind};

use refact_scheduler_core::types::{Action, AgentTarget, Delivery, Job};

const WEBHOOK_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Serialize)]
struct WebhookPayload<'a> {
    job_id: &'a str,
    description: &'a str,
    status: &'a str,
    output: &'a str,
    ts: u64,
}

pub async fn deliver(app: &AppState, job: &Job, output: &str) -> Result<(), String> {
    match &job.delivery {
        Delivery::Chat => deliver_chat(app, job, output).await,
        Delivery::Webhook { url, token } => {
            deliver_webhook(app, job, output, url, token.as_deref()).await
        }
        Delivery::Notifier {
            integration_id,
            target,
        } => deliver_notifier(app, output, integration_id, target.as_deref()).await,
        Delivery::None => Ok(()),
    }
}

/// Command results are notices: they go through the shared delivery layer so a
/// busy chat queues the whole batch at its boundary instead of dropping it.
async fn deliver_chat(app: &AppState, job: &Job, output: &str) -> Result<(), String> {
    if output.is_empty() {
        return Ok(());
    }
    let chat_id = match job_target(job)? {
        AgentTarget::ExistingChat { chat_id } => {
            if !app.gcx.chat_sessions.read().await.contains_key(chat_id) {
                return Err(format!("Chat session {chat_id} not found"));
            }
            chat_id.clone()
        }
        AgentTarget::Isolated => {
            let chat_id = isolated_chat_id(job);
            let _ =
                get_or_create_session_with_trajectory(app.chat_ctx(), &app.chat.sessions, &chat_id)
                    .await;
            chat_id
        }
    };
    let messages = if job.last_status.as_deref() == Some("error") {
        vec![error_notice_message(job, output)]
    } else if matches!(job.action, Action::Command { .. }) {
        vec![cron_fire_message(job), output_message(job, output)]
    } else {
        vec![output_message(job, output)]
    };
    let outcome = deliver_job_messages(app, &chat_id, job, messages, false).await?;
    if outcome == DeliveryOutcome::Duplicate {
        tracing::debug!(
            "scheduled task {} chat delivery deduped for chat {}",
            job.id,
            chat_id
        );
    }
    Ok(())
}

/// Stable per-fire id so a scheduler retry of the same run cannot double-post.
pub(crate) fn delivery_id_for_fire(job_id: &str, fired_at_ms: u64, suffix: &str) -> String {
    format!("cron-{job_id}-{fired_at_ms}-{suffix}")
}

pub(crate) fn job_delivery_id(job: &Job, suffix: &str) -> String {
    delivery_id_for_fire(
        &job.id,
        job.last_fired_at_ms.unwrap_or(job.created_at_ms),
        suffix,
    )
}

pub(crate) async fn deliver_job_messages(
    app: &AppState,
    chat_id: &str,
    job: &Job,
    messages: Vec<ChatMessage>,
    wake: bool,
) -> Result<DeliveryOutcome, String> {
    let delivery = PendingDelivery::with_id(
        job_delivery_id(job, if wake { "turn" } else { "output" }),
        messages,
        job.push,
        "scheduler.cron".to_string(),
        wake,
    );
    refact_chat_api::delivery::deliver_to_chat(app.clone(), chat_id, delivery).await
}

/// Deliver a scheduled agent turn: the fire notice plus the prompt land as one
/// batch, at `job.push`, waking the chat once the batch is in the history.
pub(crate) async fn deliver_scheduled_turn(
    app: &AppState,
    chat_id: &str,
    job: &Job,
    fired_at_ms: u64,
    messages: Vec<ChatMessage>,
) -> Result<DeliveryOutcome, String> {
    // Fire count advances only after acceptance; retries (including restart
    // recovery) therefore reuse the same id independently of wall clock time.
    let mut delivery = PendingDelivery::with_id(
        format!("cron-{}-{}-turn", job.id, job.fire_count),
        messages,
        job.push,
        "scheduler.cron".to_string(),
        true,
    );
    delivery.thread_patch = refact_scheduler_core::planning::scheduled_thread_patch(job);
    let _ = fired_at_ms;
    refact_chat_api::delivery::deliver_to_chat(app.clone(), chat_id, delivery).await
}

/// Pure notices (catch-up, auto-expiry) never wake a chat.
pub(crate) async fn deliver_notice(
    app: &AppState,
    chat_id: &str,
    id: String,
    message: ChatMessage,
    push: PushMode,
) -> Result<DeliveryOutcome, String> {
    refact_chat_api::delivery::deliver_to_chat(
        app.clone(),
        chat_id,
        PendingDelivery::with_id(id, vec![message], push, "scheduler.cron".to_string(), false),
    )
    .await
}

async fn deliver_webhook(
    app: &AppState,
    job: &Job,
    output: &str,
    url: &str,
    token: Option<&str>,
) -> Result<(), String> {
    if url.trim().is_empty() {
        return Err("webhook url is required".to_string());
    }
    let payload = WebhookPayload {
        job_id: &job.id,
        description: &job.description,
        status: job.last_status.as_deref().unwrap_or("fired"),
        output,
        ts: current_timestamp_ms(),
    };
    let mut request = app
        .runtime
        .http_client
        .post(url)
        .json(&payload)
        .timeout(WEBHOOK_TIMEOUT);
    if let Some(token) = token.filter(|token| !token.trim().is_empty()) {
        request = request.bearer_auth(token);
    }
    let response = tokio::time::timeout(WEBHOOK_TIMEOUT, request.send())
        .await
        .map_err(|_| "webhook delivery timed out".to_string())?
        .map_err(|error| format!("webhook delivery failed: {error}"))?;
    if response.status().is_success() {
        Ok(())
    } else {
        Err(format!(
            "webhook delivery returned status {}",
            response.status()
        ))
    }
}

async fn deliver_notifier(
    app: &AppState,
    output: &str,
    integration_id: &str,
    target: Option<&str>,
) -> Result<(), String> {
    let notifier =
        crate::integrations::notifier::configured_notifier_backend(app.gcx.clone(), integration_id)
            .await?;
    notifier.send(target, output).await
}

fn job_target(job: &Job) -> Result<&AgentTarget, String> {
    match &job.action {
        Action::AgentTurn { target, .. } | Action::Command { target, .. } => Ok(target),
    }
}

fn isolated_chat_id(job: &Job) -> String {
    format!(
        "cron_{}_{}",
        job.id,
        job.last_fired_at_ms.unwrap_or_else(current_timestamp_ms)
    )
}

fn cron_fire_message(job: &Job) -> ChatMessage {
    let fired_at_ms = job.last_fired_at_ms.unwrap_or_else(current_timestamp_ms);
    event(
        EventSubkind::CronFire,
        "scheduler.cron",
        json!({
            "task_id": job.id,
            "cron": job.cron_expr().unwrap_or_default(),
            "recurring": job.recurring,
            "fire_count": job.fire_count.saturating_add(1),
            "final": job.recurring
                && job.auto_expire_after_ms > 0
                && fired_at_ms.saturating_sub(job.created_at_ms) > job.auto_expire_after_ms,
            "action_kind": job.action_kind(),
        }),
        job.description.clone(),
    )
}

fn output_message(job: &Job, output: &str) -> ChatMessage {
    let mut extra = serde_json::Map::new();
    extra.insert(
        "scheduler_delivery".to_string(),
        json!({
            "task_id": job.id,
            "delivery": "chat",
            "status": job.last_status.as_deref().unwrap_or("fired"),
        }),
    );
    if matches!(job.action, Action::Command { .. }) {
        extra.insert(
            "scheduler_command".to_string(),
            json!({
                "task_id": job.id,
                "status": job.last_status.as_deref().unwrap_or("fired"),
                "exit_code": null,
            }),
        );
    }
    ChatMessage {
        role: "plain_text".to_string(),
        content: ChatContent::SimpleText(output.to_string()),
        extra,
        ..Default::default()
    }
}

fn error_notice_message(job: &Job, output: &str) -> ChatMessage {
    event(
        EventSubkind::SystemNotice,
        "scheduler.cron",
        json!({
            "task_id": job.id,
            "action_kind": job.action_kind(),
            "status": job.last_status.as_deref().unwrap_or("error"),
            "exit_code": null,
        }),
        output.to_string(),
    )
}

fn current_timestamp_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64
}
