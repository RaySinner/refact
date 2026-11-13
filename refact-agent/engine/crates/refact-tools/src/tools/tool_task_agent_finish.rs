use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, OnceLock};
use async_trait::async_trait;
use chrono::Utc;
use serde_json::{json, Value};
use tokio::sync::Mutex as AMutex;

use refact_runtime_api::AtCommandsContext;
use refact_runtime_api::GlobalContextCtx;
use refact_core::chat_types::{ChatContent, ChatMessage, ContextEnum};
use refact_chat_api::verifier::ExpectedCardState;
use refact_runtime_api::verifier::schedule_card_verifier_after_finish_for_ctx;
use refact_runtime_api::task_storage as storage;
use refact_tasks::types::{
    AbVariantFinish, AbVariantInfo, BoardCard, FinalReport, StatusUpdate, SuggestedCard,
    TeamMemberReport, TeamStatus, VerificationResult,
};
use crate::tools::tools_description::{Tool, ToolDesc, ToolSource, ToolSourceType};
use refact_worktrees::types::WorktreeMeta;

pub async fn get_task_id(ccx: &Arc<AMutex<AtCommandsContext>>) -> Result<String, String> {
    let ccx_lock = ccx.lock().await;
    ccx_lock
        .task_meta
        .as_ref()
        .map(|m| m.task_id.clone())
        .ok_or_else(|| {
            "This tool can only be used by task agents (chat not bound to a task)".to_string()
        })
}

pub async fn get_card_id(ccx: &Arc<AMutex<AtCommandsContext>>) -> Result<String, String> {
    let ccx_lock = ccx.lock().await;
    ccx_lock
        .task_meta
        .as_ref()
        .and_then(|m| m.card_id.clone())
        .ok_or_else(|| {
            "This tool can only be used by task agents (no card_id in task_meta)".to_string()
        })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedAgentWorktree {
    pub root: PathBuf,
    pub branch: Option<String>,
    pub name: Option<String>,
}

pub fn resolve_agent_worktree(
    thread_worktree: Option<WorktreeMeta>,
    card: &BoardCard,
) -> Option<ResolvedAgentWorktree> {
    if let Some(meta) = thread_worktree {
        return Some(ResolvedAgentWorktree {
            root: meta.root,
            branch: meta.branch,
            name: Some(meta.id),
        });
    }
    card.agent_worktree
        .as_ref()
        .map(|root| ResolvedAgentWorktree {
            root: PathBuf::from(root),
            branch: card.agent_branch.clone(),
            name: card.agent_worktree_name.clone(),
        })
}

pub fn resolve_ab_variant_worktree(
    thread_worktree: Option<WorktreeMeta>,
    variant: &AbVariantInfo,
) -> ResolvedAgentWorktree {
    if let Some(meta) = thread_worktree {
        return ResolvedAgentWorktree {
            root: meta.root,
            branch: meta.branch,
            name: Some(meta.id),
        };
    }
    ResolvedAgentWorktree {
        root: PathBuf::from(&variant.worktree),
        branch: variant.branch.clone(),
        name: variant.worktree_name.clone(),
    }
}

pub static FINISH_LOCKS: OnceLock<AMutex<HashMap<String, Arc<AMutex<()>>>>> = OnceLock::new();

pub fn get_finish_locks() -> &'static AMutex<HashMap<String, Arc<AMutex<()>>>> {
    FINISH_LOCKS.get_or_init(|| AMutex::new(HashMap::new()))
}

pub async fn get_finish_lock(task_id: &str, card_id: &str) -> Arc<AMutex<()>> {
    let mut locks = get_finish_locks().lock().await;
    locks
        .entry(format!("{}:{}", task_id, card_id))
        .or_insert_with(|| Arc::new(AMutex::new(())))
        .clone()
}

pub async fn ensure_lock_for<T>(
    task_id: &str,
    card_id: &str,
    finish: impl std::future::Future<Output = T>,
) -> T {
    let key = format!("{}:{}", task_id, card_id);
    let finish_lock = get_finish_lock(task_id, card_id).await;
    let result = {
        let _finish_guard = finish_lock.lock().await;
        finish.await
    };
    let mut locks = get_finish_locks().lock().await;
    if let Some(entry) = locks.get(&key) {
        if Arc::ptr_eq(entry, &finish_lock) && Arc::strong_count(&finish_lock) == 2 {
            locks.remove(&key);
        }
    }
    result
}

pub async fn refresh_finish_heartbeat_if_current(
    ctx: &refact_runtime_api::ChatCtx,
    task_id: &str,
    card_id: &str,
    finish_chat_id: &str,
    expected_agent_id: Option<&str>,
) -> Result<(), String> {
    let card_id_owned = card_id.to_string();
    let finish_chat_id_owned = finish_chat_id.to_string();
    let expected_agent_id_owned = expected_agent_id.map(str::to_string);
    let heartbeat = Utc::now().to_rfc3339();
    storage::update_board_atomic_for_ctx(ctx, task_id, move |board| {
        let card = board
            .get_card_mut(&card_id_owned)
            .ok_or_else(|| format!("Card {} not found", card_id_owned))?;
        if card.column != "doing" {
            return Err(format!(
                "Card {} is in '{}' column. Cannot finish from stale agent state.",
                card_id_owned, card.column
            ));
        }
        if let Some(variants) = card.ab_variants.as_ref() {
            let variant = if variants.a.chat_id == finish_chat_id_owned {
                &variants.a
            } else if variants.b.chat_id == finish_chat_id_owned {
                &variants.b
            } else {
                return Err(format!(
                    "Card {} has A/B variants {:?} and {:?}, not {}",
                    card_id_owned, variants.a.chat_id, variants.b.chat_id, finish_chat_id_owned
                ));
            };
            if variant.finish.is_some() {
                return Err(format!(
                    "A/B variant {} for card {} has already finished. Cannot finish twice.",
                    finish_chat_id_owned, card_id_owned
                ));
            }
            if let Some(expected_agent_id) = expected_agent_id_owned.as_deref() {
                if variant.agent_id != expected_agent_id {
                    return Err(format!(
                        "A/B variant {} for card {} belongs to agent {}, not {}",
                        finish_chat_id_owned, card_id_owned, variant.agent_id, expected_agent_id
                    ));
                }
            }
            card.last_heartbeat_at = Some(heartbeat.clone());
            return Ok(());
        }
        if !is_room_finisher(card, &finish_chat_id_owned) {
            return Err(format!(
                "Card {} is now owned by agent_chat_id={:?}, not {}",
                card_id_owned, card.agent_chat_id, finish_chat_id_owned
            ));
        }
        // The scalar owner's agent id is mirrored in `assignee`; a room member that is not the
        // owner has its identity in its own team slot (already verified above), so the assignee
        // check applies only to the owner.
        if card.agent_chat_id.as_deref() == Some(finish_chat_id_owned.as_str()) {
            if let Some(expected_agent_id) = expected_agent_id_owned.as_deref() {
                if card.assignee.as_deref() != Some(expected_agent_id) {
                    return Err(format!(
                        "Card {} is now assigned to {:?}, not {}",
                        card_id_owned, card.assignee, expected_agent_id
                    ));
                }
            }
        }
        card.last_heartbeat_at = Some(heartbeat.clone());
        Ok(())
    })
    .await
    .map(|_| ())
}

pub fn parse_success_arg(args: &HashMap<String, Value>) -> Result<bool, String> {
    match args.get("success") {
        Some(value) => refact_tool_api::coerce_bool(value).ok_or_else(|| match value {
            Value::String(_) => "Invalid 'success' string; expected true or false".to_string(),
            _ => "Missing or invalid 'success' parameter (must be boolean)".to_string(),
        }),
        _ => Err("Missing or invalid 'success' parameter (must be boolean)".to_string()),
    }
}

pub fn git_failure_details(output: &std::process::Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    match (stderr.is_empty(), stdout.is_empty()) {
        (false, false) => format!("{}\n{}", stderr, stdout),
        (false, true) => stderr,
        (true, false) => stdout,
        (true, true) => format!("exit status {}", output.status),
    }
}

pub async fn git_output_checked(
    worktree_path: &Path,
    args: &[&str],
    action: &str,
) -> Result<std::process::Output, String> {
    let mut command = tokio::process::Command::new("git");
    command.args(args).current_dir(worktree_path);
    let output = refact_worktrees::git::run_git_command(
        command,
        action,
        refact_worktrees::git::GIT_COMMAND_TIMEOUT,
    )
    .await
    .map_err(|e| format!("{} (worktree '{}')", e, worktree_path.display()))?;

    if !output.status.success() {
        return Err(format!(
            "git {} failed in worktree '{}': {}",
            action,
            worktree_path.display(),
            git_failure_details(&output)
        ));
    }

    Ok(output)
}

pub async fn validate_git_worktree(worktree_path: &Path) -> Result<(), String> {
    if !worktree_path.exists() {
        return Err(format!(
            "Assigned worktree path '{}' does not exist",
            worktree_path.display()
        ));
    }
    if !worktree_path.is_dir() {
        return Err(format!(
            "Assigned worktree path '{}' is not a directory",
            worktree_path.display()
        ));
    }

    let mut command = tokio::process::Command::new("git");
    command
        .args(["rev-parse", "--is-inside-work-tree"])
        .current_dir(worktree_path);
    let output = refact_worktrees::git::run_git_command(
        command,
        "rev-parse --is-inside-work-tree",
        refact_worktrees::git::GIT_COMMAND_TIMEOUT,
    )
    .await
    .map_err(|e| format!("Failed to validate git worktree '{}': {}", worktree_path.display(), e))?;

    if !output.status.success() {
        return Err(format!(
            "Assigned worktree path '{}' is not a git worktree/repo: {}",
            worktree_path.display(),
            git_failure_details(&output)
        ));
    }

    let inside_work_tree = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if inside_work_tree != "true" {
        return Err(format!(
            "Assigned worktree path '{}' is not inside a git worktree",
            worktree_path.display()
        ));
    }

    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParsedFinishReport {
    pub markdown: String,
    pub structured: Option<FinalReport>,
}

pub fn agent_finish_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "success": {
                "type": "boolean",
                "description": "true if the card was completed successfully, false if it failed"
            },
            "report": {
                "description": "Legacy markdown string or structured final report object",
                "anyOf": [
                    { "type": "string" },
                    final_report_schema()
                ]
            },
            "files_changed": {
                "type": "array",
                "items": { "type": "string" },
                "description": "Relative paths changed by this card"
            },
            "tests_added": {
                "type": "array",
                "items": { "type": "string" },
                "description": "Test names or paths added or updated"
            },
            "tests_added_or_updated": {
                "type": "array",
                "items": { "type": "string" },
                "description": "Test names or paths added or updated"
            },
            "verification": {
                "type": "array",
                "items": verification_schema(),
                "description": "Verification commands and results"
            },
            "followup_cards": {
                "type": "array",
                "items": suggested_card_schema(),
                "description": "Suggested follow-up task cards"
            },
            "risks": {
                "type": "array",
                "items": { "type": "string" },
                "description": "Risks or caveats"
            },
            "assumptions": {
                "type": "array",
                "items": { "type": "string" },
                "description": "Assumptions made while completing the card"
            }
        },
        "required": ["success", "report"]
    })
}

pub fn final_report_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "summary": { "type": "string" },
            "success": { "type": "boolean" },
            "files_changed": { "type": "array", "items": { "type": "string" } },
            "tests_added_or_updated": { "type": "array", "items": { "type": "string" } },
            "verification": { "type": "array", "items": verification_schema() },
            "followup_cards": { "type": "array", "items": suggested_card_schema() },
            "risks": { "type": "array", "items": { "type": "string" } },
            "assumptions": { "type": "array", "items": { "type": "string" } }
        }
    })
}

pub fn verification_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "command": { "type": "string" },
            "exit_code": { "type": ["integer", "null"] },
            "passed": { "type": "boolean" },
            "output_tail": { "type": "string" }
        }
    })
}

pub fn suggested_card_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "title": { "type": "string" },
            "instructions": { "type": "string" },
            "priority": { "type": "string" },
            "target_files": { "type": "array", "items": { "type": "string" } }
        }
    })
}

pub fn parse_finish_report(
    args: &HashMap<String, Value>,
    success: bool,
) -> Result<ParsedFinishReport, String> {
    let report_value = args
        .get("report")
        .ok_or_else(|| "Missing 'report' parameter".to_string())?;

    match report_value {
        Value::String(report) => {
            if has_structured_report_fields(args) {
                let structured = structured_report_from_args(report.clone(), success, args)?;
                let markdown = structured.to_markdown();
                Ok(ParsedFinishReport {
                    markdown,
                    structured: Some(structured),
                })
            } else {
                Ok(ParsedFinishReport {
                    markdown: report.clone(),
                    structured: None,
                })
            }
        }
        Value::Object(_) => {
            let mut structured: FinalReport = serde_json::from_value(report_value.clone())
                .map_err(|e| format!("Invalid structured 'report' parameter: {}", e))?;
            structured.success = success;
            apply_optional_structured_fields(&mut structured, args)?;
            let markdown = structured.to_markdown();
            Ok(ParsedFinishReport {
                markdown,
                structured: Some(structured),
            })
        }
        _ => Err("Invalid 'report' parameter (must be string or object)".to_string()),
    }
}

pub fn has_structured_report_fields(args: &HashMap<String, Value>) -> bool {
    [
        "files_changed",
        "tests_added",
        "tests_added_or_updated",
        "verification",
        "followup_cards",
        "risks",
        "assumptions",
    ]
    .iter()
    .any(|key| args.contains_key(*key))
}

pub fn structured_report_from_args(
    summary: String,
    success: bool,
    args: &HashMap<String, Value>,
) -> Result<FinalReport, String> {
    let mut report = FinalReport {
        summary,
        success,
        ..Default::default()
    };
    apply_optional_structured_fields(&mut report, args)?;
    Ok(report)
}

pub fn apply_optional_structured_fields(
    report: &mut FinalReport,
    args: &HashMap<String, Value>,
) -> Result<(), String> {
    if let Some(value) = args.get("files_changed") {
        report.files_changed = parse_string_vec(value, "files_changed")?;
    }
    if let Some(value) = args
        .get("tests_added_or_updated")
        .or_else(|| args.get("tests_added"))
    {
        report.tests_added_or_updated = parse_string_vec(value, "tests_added_or_updated")?;
    }
    if let Some(value) = args.get("verification") {
        report.verification = parse_json_field::<Vec<VerificationResult>>(value, "verification")?;
    }
    if let Some(value) = args.get("followup_cards") {
        report.followup_cards = parse_json_field::<Vec<SuggestedCard>>(value, "followup_cards")?;
    }
    if let Some(value) = args.get("risks") {
        report.risks = parse_string_vec(value, "risks")?;
    }
    if let Some(value) = args.get("assumptions") {
        report.assumptions = parse_string_vec(value, "assumptions")?;
    }
    Ok(())
}

pub fn parse_string_vec(value: &Value, field: &str) -> Result<Vec<String>, String> {
    parse_json_field::<Vec<String>>(value, field)
}

pub fn parse_json_field<T>(value: &Value, field: &str) -> Result<T, String>
where
    T: serde::de::DeserializeOwned,
{
    serde_json::from_value(value.clone())
        .map_err(|e| format!("Invalid '{}' parameter: {}", field, e))
}

pub fn mark_finished_card(
    card: &mut BoardCard,
    success: bool,
    report: &ParsedFinishReport,
    commit_hash: Option<&str>,
) {
    if success {
        card.final_report = Some(report.markdown.clone());
        card.final_report_structured = report.structured.clone();
        card.column = "done".to_string();
        card.completed_at = Some(Utc::now().to_rfc3339());
        if let Some(hash) = commit_hash {
            card.status_updates.push(StatusUpdate {
                timestamp: Utc::now().to_rfc3339(),
                message: format!("Auto-committed: {}", hash),
            });
        }
        card.status_updates.push(StatusUpdate {
            timestamp: Utc::now().to_rfc3339(),
            message: "Agent completed successfully".to_string(),
        });
    } else {
        card.final_report = Some(format!("FAILED: {}", report.markdown));
        card.final_report_structured = report.structured.clone();
        card.column = "failed".to_string();
        card.completed_at = Some(Utc::now().to_rfc3339());
        card.status_updates.push(StatusUpdate {
            timestamp: Utc::now().to_rfc3339(),
            message: format!("Agent failed: {}", report.markdown),
        });
    }
}

/// Whether `finish_chat_id` may finish `card`: the scalar owner or any room member.
///
/// A room is a card carrying several `team_members`; after the spawn protocol fills a slot, the
/// card's scalar `agent_chat_id` points at the primary (first writing) owner, but every member
/// must be able to call `agent_finish` for its own slot.
pub fn is_room_finisher(card: &BoardCard, finish_chat_id: &str) -> bool {
    if card.agent_chat_id.as_deref() == Some(finish_chat_id) {
        return true;
    }
    card.team_member_by_chat_id(finish_chat_id).is_some()
}

/// Record one room member's finish in its own `team_members` entry.
///
/// The member's `member_status` becomes `Done` or `Failed` and its `report` is stored, but the
/// card's column and scalar fields are left untouched: a single member finishing does not finish
/// the card. The caller finalizes the card only once every member is terminal.
pub fn mark_room_member_finished(
    card: &mut BoardCard,
    finish_chat_id: &str,
    success: bool,
    report: &ParsedFinishReport,
    completed_at: &str,
) -> Result<(), String> {
    let member = card
        .team_members
        .iter_mut()
        .find(|member| member.agent_chat_id.as_deref() == Some(finish_chat_id))
        .ok_or_else(|| {
            format!(
                "Card {} has no room member for chat {}",
                card.id, finish_chat_id
            )
        })?;
    if member.is_terminal() {
        return Err(format!(
            "Room member '{}' on card {} has already finished. Cannot finish twice.",
            member.role, card.id
        ));
    }
    member.member_status = Some(if success {
        TeamStatus::Done
    } else {
        TeamStatus::Failed
    });
    member.status = Some(if success {
        "done".to_string()
    } else {
        "failed".to_string()
    });
    member.report = Some(TeamMemberReport {
        summary: report.markdown.clone(),
        success,
        partial: false,
        files_changed: Vec::new(),
        verification: Vec::new(),
        completed_at: completed_at.to_string(),
    });
    Ok(())
}

/// Finalize the card now that every room member has reported.
///
/// The card is `done` only when every member succeeded; any failed or partial member fails the
/// card. The card-level `final_report` aggregates each member's report so the planner reads one
/// answer instead of N.
pub fn finalize_room_card(
    card: &mut BoardCard,
    reports: &refact_tasks::rooms::RoomReports,
    commit_hash: Option<&str>,
) {
    let room_success = reports.failed.is_empty();
    let now = Utc::now().to_rfc3339();
    let mut combined = format!("# Room Report: {}\n", card.title);
    for (key, summary) in &reports.reports {
        combined.push_str(&format!("\n## {key}\n{}\n", summary.trim()));
    }
    if room_success {
        card.column = "done".to_string();
    } else {
        card.column = "failed".to_string();
    }
    card.completed_at = Some(now.clone());
    card.final_report = Some(combined);
    if let Some(hash) = commit_hash {
        card.status_updates.push(StatusUpdate {
            timestamp: now.clone(),
            message: format!("Auto-committed: {}", hash),
        });
    }
    card.status_updates.push(StatusUpdate {
        timestamp: now.clone(),
        message: if room_success {
            "Room completed: all members finished successfully".to_string()
        } else {
            format!(
                "Room failed: member(s) {} did not succeed",
                reports.failed.join(", ")
            )
        },
    });
}

pub fn mark_ab_variant_finished(
    card: &mut BoardCard,
    variant_key: &str,
    finish_chat_id: &str,
    expected_agent_id: Option<&str>,
    success: bool,
    report: &ParsedFinishReport,
    commit_hash: Option<&str>,
) -> Result<bool, String> {
    if card.column != "doing" {
        return Err(format!(
            "Card {} is in '{}' column. Cannot finish from stale agent state.",
            card.id, card.column
        ));
    }
    let variant_label = variant_key.to_ascii_uppercase();
    let all_finished = {
        let variants = card
            .ab_variants
            .as_mut()
            .ok_or_else(|| format!("Card {} has no A/B variants", card.id))?;
        let variant = variants
            .variant_mut(variant_key)
            .ok_or_else(|| format!("A/B variant {} not found", variant_key))?;
        if variant.chat_id != finish_chat_id {
            return Err(format!(
                "A/B variant {} for card {} is chat {}, not {}",
                variant_label, card.id, variant.chat_id, finish_chat_id
            ));
        }
        if variant.finish.is_some() {
            return Err(format!(
                "A/B variant {} for card {} has already finished. Cannot finish twice.",
                finish_chat_id, card.id
            ));
        }
        if let Some(expected_agent_id) = expected_agent_id {
            if variant.agent_id != expected_agent_id {
                return Err(format!(
                    "A/B variant {} for card {} belongs to agent {}, not {}",
                    finish_chat_id, card.id, variant.agent_id, expected_agent_id
                ));
            }
        }
        variant.finish = Some(AbVariantFinish {
            success,
            final_report: report.markdown.clone(),
            final_report_structured: report.structured.clone(),
            completed_at: Utc::now().to_rfc3339(),
            commit_hash: commit_hash.map(str::to_string),
        });
        variants.all_finished()
    };
    if let Some(hash) = commit_hash {
        card.status_updates.push(StatusUpdate {
            timestamp: Utc::now().to_rfc3339(),
            message: format!("A/B variant {} auto-committed: {}", variant_label, hash),
        });
    }
    card.status_updates.push(StatusUpdate {
        timestamp: Utc::now().to_rfc3339(),
        message: format!(
            "A/B variant {} {}",
            variant_label,
            if success {
                "completed successfully"
            } else {
                "failed"
            }
        ),
    });
    Ok(all_finished)
}

pub fn clear_finished_agent_session(card: &mut BoardCard) {
    card.agent_chat_id = None;
    card.assignee = None;
}

pub fn clear_finished_agent_session_if_current(
    card: &mut BoardCard,
    expected_state: &ExpectedCardState,
    board_rev: u64,
) -> Result<(), String> {
    if !expected_state.matches_board_card(board_rev, card) {
        return Err("stale finish cleanup".to_string());
    }
    clear_finished_agent_session(card);
    Ok(())
}

pub fn agents_active(cards: &[BoardCard]) -> usize {
    storage::active_agent_count(cards)
}

pub fn sanitize_commit_component(text: &str, max_chars: usize) -> String {
    let mut out = String::new();
    let mut previous_space = false;

    for ch in text.chars() {
        if out.chars().count() >= max_chars {
            break;
        }
        let ch = if ch.is_control() { ' ' } else { ch };
        if ch.is_whitespace() {
            if !previous_space && !out.is_empty() {
                out.push(' ');
                previous_space = true;
            }
        } else {
            out.push(ch);
            previous_space = false;
        }
    }

    out.trim().to_string()
}

pub fn deterministic_agent_commit_message(card_id: &str, card_title: &str) -> String {
    let card_id = sanitize_commit_component(card_id, 80);
    let title = sanitize_commit_component(card_title, 160);
    let card_id = if card_id.is_empty() {
        "unknown".to_string()
    } else {
        card_id
    };

    if title.is_empty() {
        format!("Card {}", card_id)
    } else {
        format!("Card {}: {}", card_id, title)
    }
}

pub async fn auto_commit_worktree(
    worktree_path: &Path,
    card_id: &str,
    card_title: &str,
) -> Result<Option<String>, String> {
    auto_commit_worktree_with_message(worktree_path, card_id, card_title, None).await
}

pub async fn auto_commit_worktree_with_message(
    worktree_path: &Path,
    card_id: &str,
    card_title: &str,
    commit_msg_override: Option<String>,
) -> Result<Option<String>, String> {
    validate_git_worktree(worktree_path).await?;

    let status_output =
        git_output_checked(worktree_path, &["status", "--porcelain"], "status").await?;

    let status = String::from_utf8_lossy(&status_output.stdout);
    if status.trim().is_empty() {
        return Ok(None);
    }

    git_output_checked(worktree_path, &["add", "-A"], "add").await?;

    let commit_msg = commit_msg_override
        .as_deref()
        .map(|msg| sanitize_commit_component(msg, 240))
        .filter(|msg| !msg.is_empty())
        .unwrap_or_else(|| deterministic_agent_commit_message(card_id, card_title));

    let commit_output = {
        let mut command = tokio::process::Command::new("git");
        command
            .args([
                "-c",
                "user.name=Refact Agent",
                "-c",
                "user.email=agent@refact.ai",
                "commit",
                "-m",
                &commit_msg,
                "--no-gpg-sign",
            ])
            .current_dir(worktree_path);
        refact_worktrees::git::run_git_command(
            command,
            "commit",
            refact_worktrees::git::GIT_COMMAND_TIMEOUT,
        )
        .await
        .map_err(|e| {
            format!(
                "Failed to commit in worktree '{}': {}",
                worktree_path.display(),
                e
            )
        })?
    };

    if !commit_output.status.success() {
        let stderr = String::from_utf8_lossy(&commit_output.stderr);
        if stderr.contains("nothing to commit") {
            return Ok(None);
        }
        return Err(format!(
            "git commit failed in worktree '{}': {}",
            worktree_path.display(),
            git_failure_details(&commit_output)
        ));
    }

    let rev_output =
        git_output_checked(worktree_path, &["rev-parse", "HEAD"], "rev-parse HEAD").await?;

    let commit_hash = String::from_utf8_lossy(&rev_output.stdout)
        .trim()
        .to_string();
    if commit_hash.is_empty() {
        return Err(format!(
            "git rev-parse HEAD returned empty output in worktree '{}'",
            worktree_path.display()
        ));
    }
    Ok(Some(commit_hash))
}

pub struct ToolTaskAgentFinish;

impl ToolTaskAgentFinish {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for ToolTaskAgentFinish {
    fn tool_description(&self) -> ToolDesc {
        ToolDesc {
            name: "agent_finish".to_string(),
            display_name: "Task Agent Finish".to_string(),
            source: ToolSource {
                source_type: ToolSourceType::Builtin,
                config_path: String::new(),
            },
            experimental: false,
            allow_parallel: false,
            description: "Mark the current card as completed or failed. Task agents MUST call this exactly once when finished. This updates the task board and notifies the planner.".to_string(),
            input_schema: agent_finish_schema(),
            output_schema: None,
            annotations: None,
        }
    }

    async fn tool_execute(
        &mut self,
        ccx: Arc<AMutex<AtCommandsContext>>,
        tool_call_id: &String,
        args: &HashMap<String, Value>,
    ) -> Result<(bool, Vec<ContextEnum>), String> {
        let task_id = get_task_id(&ccx).await?;
        let card_id = get_card_id(&ccx).await?;
        let (planner_chat_id, finish_chat_id, finish_agent_id) = {
            let ccx_lock = ccx.lock().await;
            (
                ccx_lock
                    .task_meta
                    .as_ref()
                    .and_then(|meta| meta.planner_chat_id.clone()),
                ccx_lock.chat_id.clone(),
                ccx_lock
                    .task_meta
                    .as_ref()
                    .and_then(|meta| meta.agent_id.clone()),
            )
        };

        let success = parse_success_arg(args)?;

        let report = parse_finish_report(args, success)?;

        let app = {
            let ccx_lock = ccx.lock().await;
            ccx_lock.app.clone()
        };
        let lock_task_id = task_id.clone();
        let lock_card_id = card_id.clone();

        ensure_lock_for(&lock_task_id, &lock_card_id, async move {

        refresh_finish_heartbeat_if_current(
            &app.ctx,
            &task_id,
            &card_id,
            &finish_chat_id,
            finish_agent_id.as_deref(),
        )
        .await?;

        let board_pre = storage::load_board_for_ctx(&app.ctx, &task_id).await?;
        let card_pre = board_pre
            .get_card(&card_id)
            .ok_or(format!("Card {} not found", card_id))?;
        if card_pre.column == "done" || card_pre.column == "failed" {
            return Err(format!(
                "Card {} is already in '{}' column. Cannot finish twice.",
                card_id, card_pre.column
            ));
        }
        let thread_worktree = ccx.lock().await.execution_scope_worktree();
        let ab_variant_key = if let Some(variants) = card_pre.ab_variants.as_ref() {
            let variant_key = variants
                .variant_key_for_chat_id(&finish_chat_id)
                .ok_or_else(|| {
                    format!(
                        "Card {} has A/B variants {:?} and {:?}, not {}",
                        card_id, variants.a.chat_id, variants.b.chat_id, finish_chat_id
                    )
                })?;
            let variant = variants
                .variant(variant_key)
                .ok_or_else(|| format!("A/B variant {} not found", variant_key))?;
            if variant.finish.is_some() {
                return Err(format!(
                    "A/B variant {} for card {} has already finished. Cannot finish twice.",
                    finish_chat_id, card_id
                ));
            }
            if let Some(expected_agent_id) = finish_agent_id.as_deref() {
                if variant.agent_id != expected_agent_id {
                    return Err(format!(
                        "A/B variant {} for card {} belongs to agent {}, not {}",
                        finish_chat_id, card_id, variant.agent_id, expected_agent_id
                    ));
                }
            }
            Some(variant_key.to_string())
        } else {
            None
        };
        let resolved_worktree = if let Some(variant_key) = ab_variant_key.as_deref() {
            let variants = card_pre.ab_variants.as_ref().expect("variant key requires A/B");
            let variant = variants
                .variant(variant_key)
                .ok_or_else(|| format!("A/B variant {} not found", variant_key))?;
            Some(resolve_ab_variant_worktree(thread_worktree, variant))
        } else {
            resolve_agent_worktree(thread_worktree, card_pre)
        };
        let card_title_for_commit = card_pre.title.clone();

        let commit_result = if success {
            if let Some(ref worktree) = resolved_worktree {
                match auto_commit_worktree(&worktree.root, &card_id, &card_title_for_commit).await
                {
                    Ok(hash) => hash,
                    Err(e) => {
                        return Err(format!(
                            "Auto-commit failed in worktree '{}': {}. Please ensure your changes are committed before calling agent_finish(success=true). \
                            You can run `git add -A && git commit -m 'your message'` in the worktree, or investigate the error.",
                            worktree.root.display(),
                            e
                        ));
                    }
                }
            } else {
                None
            }
        } else {
            None
        };

        let card_id_owned = card_id.clone();
        let report_clone = report.clone();
        let success_clone = success;
        let commit_hash = commit_result.clone();

        let finish_agent_id_for_update = finish_agent_id.clone();
        let ab_variant_key_for_update = ab_variant_key.clone();
        let (board, (card_title, all_finished, verifier_expected_state, finished_ab_variant, card_success)) =
            storage::update_board_atomic_for_ctx(&app.ctx, &task_id, move |board| {
                let next_board_rev = board.rev + 1;
                let card = board
                    .get_card_mut(&card_id_owned)
                    .ok_or(format!("Card {} not found in task", card_id_owned))?;

                if let Some(variant_key) = ab_variant_key_for_update.as_deref() {
                    let card_title = card.title.clone();
                    mark_ab_variant_finished(
                        card,
                        variant_key,
                        &finish_chat_id,
                        finish_agent_id_for_update.as_deref(),
                        success_clone,
                        &report_clone,
                        commit_hash.as_deref(),
                    )?;
                    let all_finished = agents_active(&board.cards) == 0;
                    return Ok((
                        card_title,
                        all_finished,
                        None,
                        Some(variant_key.to_string()),
                        true,
                    ));
                }

                if card.column == "done" || card.column == "failed" {
                    return Err(format!(
                        "Card {} is already in '{}' column. Cannot finish twice.",
                        card_id_owned, card.column
                    ));
                }

                if !is_room_finisher(card, &finish_chat_id) {
                    return Err(format!(
                        "Card {} is now owned by agent_chat_id={:?}, not {}",
                        card_id_owned, card.agent_chat_id, finish_chat_id
                    ));
                }
                // A non-owner room member has its identity in its own slot (verified above); the
                // assignee is the primary owner's mirror, so the check applies to the owner only.
                if card.agent_chat_id.as_deref() == Some(finish_chat_id.as_str()) {
                    if let Some(expected_agent_id) = finish_agent_id_for_update.as_deref() {
                        if card.assignee.as_deref() != Some(expected_agent_id) {
                            return Err(format!(
                                "Card {} is now assigned to {:?}, not {}",
                                card_id_owned, card.assignee, expected_agent_id
                            ));
                        }
                    }
                }

                let card_title = card.title.clone();
                let completed_at = Utc::now().to_rfc3339();

                if card.is_room() {
                    mark_room_member_finished(
                        card,
                        &finish_chat_id,
                        success_clone,
                        &report_clone,
                        &completed_at,
                    )?;
                    let reports = refact_tasks::rooms::collect_room_reports(card);
                    let room_complete = reports.room_complete();
                    if room_complete {
                        finalize_room_card(card, &reports, commit_hash.as_deref());
                    }
                    let verifier_expected_state = if room_complete {
                        Some(ExpectedCardState::from_card(next_board_rev, card))
                    } else {
                        None
                    };
                    let agents_active = agents_active(&board.cards);
                    let all_finished = agents_active == 0 && room_complete;
                    let room_success = reports.failed.is_empty();
                    return Ok((
                        card_title,
                        all_finished,
                        verifier_expected_state,
                        None,
                        room_success,
                    ));
                }

                mark_finished_card(card, success_clone, &report_clone, commit_hash.as_deref());
                let verifier_expected_state = ExpectedCardState::from_card(next_board_rev, card);

                let agents_active = agents_active(&board.cards);
                let all_finished = agents_active == 0;

                Ok((
                    card_title,
                    all_finished,
                    Some(verifier_expected_state),
                    None,
                    success_clone,
                ))
            })
            .await?;

        storage::update_task_stats_for_ctx(&app.ctx, &task_id).await?;

        let result_message = if let Some(variant_key) = finished_ab_variant.as_deref() {
            let variant_label = variant_key.to_ascii_uppercase();
            if success {
                if all_finished {
                    format!(
                        "✅ **A/B Variant {} Completed: {}**\n\n**Report:**\n{}\n\nAll A/B variants have finished. Planner notified; use `pick_ab_winner` to promote the selected variant.",
                        variant_label, card_title, report.markdown
                    )
                } else {
                    format!(
                        "✅ **A/B Variant {} Completed: {}**\n\n**Report:**\n{}\n\nPlanner notified. The other A/B variant is still running.",
                        variant_label, card_title, report.markdown
                    )
                }
            } else if all_finished {
                format!(
                    "❌ **A/B Variant {} Failed: {}**\n\n**Reason:**\n{}\n\nAll A/B variants have finished. Planner notified; use `pick_ab_winner` to promote the selected variant outcome.",
                    variant_label, card_title, report.markdown
                )
            } else {
                format!(
                    "❌ **A/B Variant {} Failed: {}**\n\n**Reason:**\n{}\n\nPlanner notified. The other A/B variant is still running.",
                    variant_label, card_title, report.markdown
                )
            }
        } else if card_success {
            if all_finished {
                format!(
                    "✅ **Card Completed: {}**\n\n**Report:**\n{}\n\nAll agents have completed. Planner notified.",
                    card_title, report.markdown
                )
            } else {
                format!(
                    "✅ **Room member finished: {}**\n\n**Report:**\n{}\n\nPlanner notified. Other room members are still working.",
                    card_title, report.markdown
                )
            }
        } else {
            if all_finished {
                format!(
                    "❌ **Card Failed: {}**\n\n**Reason:**\n{}\n\nAll agents have completed. Planner notified.",
                    card_title, report.markdown
                )
            } else {
                format!(
                    "❌ **Room member failed: {}**\n\n**Reason:**\n{}\n\nPlanner notified. The card will be failed once every room member has finished.",
                    card_title, report.markdown
                )
            }
        };

        tracing::info!(
            "Agent finished card {} ({}): {}",
            card_id,
            if success { "success" } else { "failed" },
            report.markdown.chars().take(100).collect::<String>()
        );

        let notify_error = refact_chat_api::agent_notifications::notify_planner_agents_finished(
            app.chat_ctx(),
            &task_id,
            &board,
            all_finished,
            planner_chat_id.as_deref(),
        )
        .await
        .err();
        if let Some(ref error) = notify_error {
            tracing::warn!(
                "Agent finished card {}, but planner notification failed: {}",
                card_id,
                error
            );
        }

        if card_success {
            if let Some(verifier_expected_state) = verifier_expected_state {
                schedule_card_verifier_after_finish_for_ctx(
                    &app.chat_ctx(),
                    task_id.clone(),
                    card_id.clone(),
                    verifier_expected_state,
                )
                .await;
            }
        } else if let Some(expected_state) = verifier_expected_state {
            let card_id_clear = card_id.clone();
            let _ = storage::update_board_atomic_for_ctx(&app.ctx, &task_id, move |board| {
                let board_rev = board.rev;
                if let Some(c) = board.get_card_mut(&card_id_clear) {
                    clear_finished_agent_session_if_current(c, &expected_state, board_rev)?;
                }
                Ok(())
            })
            .await;
        }

        {
            let ccx_lock = ccx.lock().await;
            ccx_lock.abort_flag.store(true, Ordering::SeqCst);
        }

        let result_message = if let Some(error) = notify_error {
            format!(
                "{}\n\n⚠️ Planner notification failed: {}",
                result_message, error
            )
        } else {
            result_message
        };

        Ok((
            false,
            vec![ContextEnum::ChatMessage(ChatMessage {
                role: "tool".to_string(),
                content: ChatContent::SimpleText(result_message),
                tool_calls: None,
                tool_call_id: tool_call_id.clone(),
                ..Default::default()
            })],
        ))
        })
        .await
    }

    fn tool_depends_on(&self) -> Vec<String> {
        vec![]
    }
}
