//! Privacy-observation-degraded warning: at most one Buddy chat per process,
//! created through the shared trajectory API.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use refact_chat_api::ChatMessage;
use refact_chat_history::trajectory_snapshot::TrajectorySnapshot;
use refact_core::buddy_meta::BuddyThreadMeta;
use uuid::Uuid;

use crate::buddy_actor_runtime::resolve_buddy_chat_model;
use crate::global_context::SharedGlobalContext;
use crate::trajectory_runtime::save_trajectory_snapshot;

static PRIVACY_DEGRADED_WARNING_CREATED: AtomicBool = AtomicBool::new(false);

fn warning_text(reason: &str) -> String {
    let reason = refact_buddy_core::actor::redact_sensitive(reason);
    let reason = refact_core::string_utils::safe_truncate(reason.trim(), 500);
    format!(
        "Privacy file-access observation is unavailable on {}: {}. Shell and process commands remain fail-open. Refact is using heuristic command-path attribution as a best-effort fallback, which may miss indirect file reads.",
        std::env::consts::OS,
        reason
    )
}

async fn create_warning_chat(
    gcx: SharedGlobalContext,
    reason: &str,
) -> Result<String, String> {
    let chat_id = Uuid::new_v4().to_string();
    let created_at = chrono::Utc::now().to_rfc3339();
    let model = resolve_buddy_chat_model(crate::app_state_from_gcx(gcx.clone()).await).await;
    let snapshot = TrajectorySnapshot {
        pending_deliveries: Vec::new(),
        goal: None,
        goal_ledger: Vec::new(),
        goal_verification_blocked_until_ms: None,
        chat_id: chat_id.clone(),
        title: "Privacy observation degraded".to_string(),
        model,
        mode: "buddy".to_string(),
        tool_use: "agent".to_string(),
        messages: vec![ChatMessage::new(
            "assistant".to_string(),
            warning_text(reason),
        )],
        created_at,
        boost_reasoning: false,
        checkpoints_enabled: false,
        context_tokens_cap: None,
        auto_compression_cap: None,
        auto_compression_cap_pending: true,
        include_project_info: false,
        is_title_generated: true,
        auto_approve_editing_tools: false,
        auto_approve_dangerous_commands: false,
        autonomous_no_confirm: true,
        version: 1,
        task_meta: None,
        worktree: None,
        parent_id: None,
        link_type: None,
        root_chat_id: None,
        reasoning_effort: None,
        thinking_budget: None,
        temperature: None,
        frequency_penalty: None,
        max_tokens: None,
        parallel_tool_calls: None,
        previous_response_id: None,
        active_skill: None,
        auto_enrichment_enabled: None,
        buddy_meta: Some(BuddyThreadMeta {
            is_buddy_chat: true,
            buddy_chat_kind: "system".to_string(),
            workflow_id: Some("privacy_degraded".to_string()),
        }),
        auto_compact_enabled: None,
        frozen_request_prefix: None,
        claude_code_identity: None,
        reactive_compact_attempts: None,
        wake_up_at: None,
        waiting_for_card_ids: Vec::new(),
    };
    save_trajectory_snapshot(gcx, snapshot).await?;
    Ok(chat_id)
}

pub async fn warn_once_with_flag(
    gcx: SharedGlobalContext,
    reason: &str,
    created: &AtomicBool,
) -> Option<Result<String, String>> {
    if created
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return None;
    }
    Some(create_warning_chat(gcx, reason).await)
}

pub async fn warn_once(gcx: Arc<crate::global_context::GlobalContext>, reason: &str) {
    if let Some(Err(error)) =
        warn_once_with_flag(gcx, reason, &PRIVACY_DEGRADED_WARNING_CREATED).await
    {
        tracing::warn!("failed to create privacy degraded Buddy chat: {error}");
    }
}
