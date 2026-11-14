use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use chrono::{TimeDelta, Utc};

use crate::host::AgentHost;
use crate::push::{push_completion_to_parent, should_retry_completion_push};
use crate::spawn::emit_background_agent_update;
use crate::types::BgAgentStatus;

const MONITOR_INTERVAL: Duration = Duration::from_secs(60);
const STUCK_AFTER: TimeDelta = TimeDelta::minutes(20);

pub async fn run_background_agent_monitor<H: AgentHost>(app: H, shutdown: Arc<AtomicBool>) {
    tracing::info!("Starting background agent monitor");
    loop {
        if shutdown.load(Ordering::Relaxed) {
            break;
        }
        if let Err(error) = monitor_once(app.clone()).await {
            tracing::warn!("background agent monitor error: {}", error);
        }
        tokio::select! {
            _ = tokio::time::sleep(MONITOR_INTERVAL) => {}
            _ = wait_for_shutdown(shutdown.clone()) => break,
        }
    }
}

pub async fn monitor_once<H: AgentHost>(app: H) -> Result<(), String> {
    let now = Utc::now();
    for record in app.agents().list_all().await {
        if record.status == BgAgentStatus::Running
            && now.signed_duration_since(record.last_update_at) > STUCK_AFTER
        {
            let updated = app
                .agents()
                .update_progress(
                    &record.agent_id,
                    "⚠ no activity for 20+ minutes".to_string(),
                    record.step_count,
                )
                .await?;
            emit_background_agent_update(app.clone(), &updated).await;
        }

        if record.status == BgAgentStatus::Running
            && !app.agents().has_runtime(&record.agent_id).await
            && child_session_missing(&app, record.child_chat_id.as_deref()).await
        {
            let updated = app
                .agents()
                .mark_interrupted(
                    &record.agent_id,
                    "Engine restarted; child session lost".to_string(),
                )
                .await?;
            emit_background_agent_update(app.clone(), &updated).await;
            push_completion_to_parent(app.clone(), &updated).await?;
        }
    }

    for record in app
        .agents()
        .list_with_completion_message_id(&["pending", "deferred"])
        .await
    {
        if should_retry_completion_push(&record) {
            push_completion_to_parent(app.clone(), &record).await?;
        }
    }

    Ok(())
}

async fn child_session_missing<H: AgentHost>(app: &H, child_chat_id: Option<&str>) -> bool {
    let Some(child_chat_id) = child_chat_id else {
        return true;
    };
    let sessions = app.sessions().read().await;
    !sessions.contains_key(child_chat_id)
}

async fn wait_for_shutdown(shutdown: Arc<AtomicBool>) {
    while !shutdown.load(Ordering::Relaxed) {
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}
