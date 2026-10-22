use super::*;
use std::sync::Arc;
use crate::app_state::AppState;
use chrono::Duration;
use tokio::sync::broadcast;
use crate::tasks::types::{BoardCard, TaskBoard, TaskMeta, TaskStatus};
use super::actor::BuddyService;
use super::diagnostics::{classify_error, DiagnosticContext, DiagnosticSeverity};
use super::issues::{
    check_issue_gate, check_manual_issue_gate, detect_repo_from_git, issue_dedupe_text,
    issue_title_and_body, mcp_issue_args, parse_remote_url, prepare_issue_content,
    record_issue_success, redact_diagnostic_text, sanitize_body, sanitize_title,
    validate_issue_binary, IssueGate, IssueProvider, RepoHost,
};
use super::scheduler::BuddyJobContext;
use super::settings::{AutonomyLevel, BuddySettings, HumorLevel, MAX_PALETTE_INDEX};
use super::state::{
    add_activity, apply_care_action, apply_pet_tick, default_buddy_state, grant_xp,
    reroll_personality,
};
use super::types::{
    BuddyAction, BuddyActivity, BuddyCareAction, BuddyFact, BuddyFactKind, BuddyJobState,
    BuddyOnboarding, BuddyOpportunity, BuddyOpportunityKind, BuddyOpportunityLinks, BuddyPage,
    BuddyPriority, BuddyPulse, BuddySuggestion, BuddyState, CustomizationKind, DefaultsKind,
    DraftKind, InvestigationContext, MarketKind, OpportunityStatus, PulseScope,
    BuddyWorkflowSummary,
};

pub(super) fn make_service_with_events(
) -> (BuddyService, broadcast::Receiver<super::events::BuddyEvent>) {
    let (tx, rx) = broadcast::channel(16);
    let svc = BuddyService::new(
        std::env::temp_dir().join(format!("buddy-test-{}", uuid::Uuid::new_v4())),
        default_buddy_state(),
        BuddySettings::default(),
        Vec::new(),
        super::runtime_queue::RuntimeQueue::new(),
        tx,
        None,
    );
    (svc, rx)
}

pub(super) fn make_service() -> BuddyService {
    make_service_with_events().0
}

// =============================================================================
// FactStore tests
// =============================================================================
pub(super) fn make_fact(
    key: &str,
    kind: BuddyFactKind,
    seen_at: chrono::DateTime<chrono::Utc>,
) -> BuddyFact {
    BuddyFact {
        kind,
        key: key.to_string(),
        source: "test",
        payload: serde_json::json!({"k": key}),
        seen_at,
        confidence: 1.0,
    }
}

pub(super) fn make_opportunity(id: &str, cooldown_key: &str) -> BuddyOpportunity {
    let now = chrono::Utc::now();
    BuddyOpportunity {
        id: id.to_string(),
        kind: BuddyOpportunityKind::TaskHealth,
        summary: "test".to_string(),
        priority: BuddyPriority::Normal,
        confidence: 0.9,
        fact_keys: vec![],
        cooldown_key: cooldown_key.to_string(),
        cooldown_secs: 1800,
        status: OpportunityStatus::New,
        proposed_actions: vec![],
        humor: None,
        humor_allowed: false,
        related: BuddyOpportunityLinks::default(),
        created_at: now,
        expires_at: now + Duration::hours(1),
        resolved_at: None,
    }
}

pub(super) fn push_opportunity(
    queue: &mut super::opportunities::OpportunityQueue,
    opp: BuddyOpportunity,
) {
    queue.push_with_cooldown(
        opp,
        super::opportunities::DEFAULT_COOLDOWN.num_seconds() as u64,
    );
}

pub(super) async fn apply_humor_plan(
    service: &mut super::humor::HumorService,
    opp: &mut BuddyOpportunity,
    kind: BuddyFactKind,
    pulse: &BuddyPulse,
    gcx: AppState,
) {
    match service.plan_humor(kind, pulse) {
        super::humor::HumorPlan::Ready(line) => opp.humor = Some(line),
        super::humor::HumorPlan::Generate(reservation) => {
            let lines = reservation.generate(gcx).await;
            if let Some(line) = service.complete_humor(reservation, lines) {
                opp.humor = Some(line);
            }
        }
        super::humor::HumorPlan::Skip => {}
    }
}

pub(super) fn make_task_meta(
    id: &str,
    name: &str,
    status: TaskStatus,
    created_at: &str,
) -> TaskMeta {
    TaskMeta {
        schema_version: 1,
        id: id.to_string(),
        name: name.to_string(),
        status,
        created_at: created_at.to_string(),
        updated_at: created_at.to_string(),
        cards_total: 0,
        cards_done: 0,
        cards_failed: 0,
        agents_active: 0,
        base_branch: None,
        base_commit: None,
        default_agent_model: None,
        is_name_generated: false,
        last_agents_summary_at: None,
        planner_session_state: None,
    }
}

pub(super) fn make_board_card(
    id: &str,
    column: &str,
    assignee: Option<&str>,
    started_at: Option<&str>,
) -> BoardCard {
    BoardCard {
        id: id.to_string(),
        title: "T".to_string(),
        column: column.to_string(),
        priority: "P1".to_string(),
        depends_on: vec![],
        instructions: String::new(),
        assignee: assignee.map(|s| s.to_string()),
        agent_chat_id: None,
        retry_count: 0,
        status_updates: vec![],
        comments: vec![],
        final_report: None,
        final_report_structured: None,
        verifier_report: None,
        created_at: chrono::Utc::now().to_rfc3339(),
        started_at: started_at.map(|s| s.to_string()),
        last_heartbeat_at: None,
        completed_at: None,
        agent_branch: None,
        agent_worktree: None,
        agent_worktree_name: None,
        base_branch: None,
        base_commit: None,
        ab_variants: None,
        team_members: vec![],
        target_files: vec![],
        scope_guard_mode: Default::default(),
    }
}

pub(super) fn chat_msg(role: &str, content: &str) -> crate::call_validation::ChatMessage {
    crate::call_validation::ChatMessage {
        role: role.to_string(),
        content: crate::call_validation::ChatContent::SimpleText(content.to_string()),
        ..Default::default()
    }
}

pub(super) async fn make_gcx_with_buddy() -> AppState {
    let gcx = crate::global_context::tests::make_test_gcx().await;
    let app = crate::app_state::AppState::from_gcx(gcx).await;
    let buddy_arc = app.buddy.buddy.clone();
    *buddy_arc.lock().await = Some(make_service());
    app
}

pub(super) async fn wait_for_runtime_event(
    app: &AppState,
    signal_type: &str,
) -> super::types::BuddyRuntimeEvent {
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            {
                let lock = app.buddy.buddy.lock().await;
                if let Some(event) = lock.as_ref().and_then(|svc| {
                    svc.runtime_queue
                        .items
                        .iter()
                        .find(|event| event.signal_type == signal_type)
                        .cloned()
                }) {
                    return event;
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("runtime event {signal_type} did not arrive"))
}

pub(super) async fn wait_for_chat_reaction_debug_attempt(
    app: &AppState,
    result: &str,
) -> super::snapshot::ChatReactionDebug {
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            {
                let lock = app.buddy.buddy.lock().await;
                if let Some(debug) = lock.as_ref().map(|svc| svc.chat_reaction_debug.snapshot()) {
                    if debug
                        .recent_attempts
                        .iter()
                        .any(|attempt| attempt.result == result)
                    {
                        return debug;
                    }
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("chat reaction debug attempt {result} did not arrive"))
}

pub(super) fn make_suggestion(id: &str, stype: &str, created_at: &str) -> BuddySuggestion {
    BuddySuggestion {
        id: id.to_string(),
        suggestion_type: stype.to_string(),
        title: "t".to_string(),
        description: "d".to_string(),
        created_at: created_at.to_string(),
        dismissed: false,
        controls: vec![],
        quest: None,
    }
}

mod chat_reactions_and_pulse;
mod issues_diagnostics_events;
mod opportunity_resolution;
mod policy_humor_observers;
mod runtime_queue_persistence;
mod stores_and_pulse;
mod task_health_and_investigations;
mod tools_routes_and_privacy;
mod workflows_and_state;
