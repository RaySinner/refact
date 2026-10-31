use refact_tasks::types::{BoardCard, VerificationOutcome};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Clone, Debug, PartialEq, Default, Serialize, Deserialize)]
pub struct ExpectedCardState {
    pub board_rev: u64,
    card_fingerprint: Value,
}

impl ExpectedCardState {
    pub fn from_card(board_rev: u64, card: &BoardCard) -> Self {
        Self {
            board_rev,
            card_fingerprint: verifier_card_fingerprint(card),
        }
    }

    pub fn matches_card(&self, card: &BoardCard) -> bool {
        self.card_fingerprint == verifier_card_fingerprint(card)
    }

    pub fn matches_board_card(&self, board_rev: u64, card: &BoardCard) -> bool {
        board_rev >= self.board_rev && self.matches_card(card)
    }
}

fn verifier_card_fingerprint(card: &BoardCard) -> Value {
    json!({
        "id": &card.id,
        "title": &card.title,
        "column": &card.column,
        "priority": &card.priority,
        "depends_on": &card.depends_on,
        "instructions": &card.instructions,
        "assignee": &card.assignee,
        "agent_chat_id": &card.agent_chat_id,
        "final_report": &card.final_report,
        "final_report_structured": &card.final_report_structured,
        "verifier_report": &card.verifier_report,
        "created_at": &card.created_at,
        "started_at": &card.started_at,
        "completed_at": &card.completed_at,
        "agent_branch": &card.agent_branch,
        "agent_worktree": &card.agent_worktree,
        "agent_worktree_name": &card.agent_worktree_name,
        "base_branch": &card.base_branch,
        "base_commit": &card.base_commit,
        "ab_variants": &card.ab_variants,
        "team_members": &card.team_members,
        "target_files": &card.target_files,
        "scope_guard_mode": &card.scope_guard_mode,
    })
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VerifyCardRequest {
    pub task_id: String,
    pub card_id: String,
    pub expected_state: Option<ExpectedCardState>,
    pub custom_command: Option<String>,
}

pub fn outcome_is_infrastructure(outcome: &VerificationOutcome) -> bool {
    matches!(
        outcome,
        VerificationOutcome::InfrastructureFailed
            | VerificationOutcome::Rejected
            | VerificationOutcome::PolicyDenied
    )
}
