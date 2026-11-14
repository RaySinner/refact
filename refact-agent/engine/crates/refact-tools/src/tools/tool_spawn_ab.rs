use std::collections::HashMap;
use std::process::Command;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use serde_json::{json, Value};
use tokio::sync::Mutex as AMutex;
use uuid::Uuid;

use refact_runtime_api::AppState;
use refact_runtime_api::AtCommandsContext;
use refact_core::chat_types::{ChatContent, ChatMessage, ContextEnum};
use refact_runtime_api::ChatCtx;
use refact_chat_api::internal_roles::{event, EventSubkind};
use refact_chat_api::verifier::ExpectedCardState;
use refact_runtime_api::verifier::schedule_card_verifier_after_finish_for_ctx;
use refact_runtime_api::task_storage as storage;
use refact_tasks::types::{AbVariantInfo, AbVariants, BoardCard, StatusUpdate};
use crate::tools::task_tool_helpers::{wait_for_agent_abort, AGENT_ABORT_TIMEOUT};
use crate::tools::tool_task_spawn_agent::{
    abandoned_worktrees_error, build_agent_prompt, build_agent_thread_params,
    find_abandoned_worktrees, prepare_agent_worktree_with_suffix, resolve_agent_model,
    resolve_invoking_planner_chat_id, PreparedWorktree,
};
use crate::tools::tools_description::{Tool, ToolDesc, ToolSource, ToolSourceType};
use refact_worktrees::service::WorktreeService;
use refact_worktrees::types::WorktreeMeta;
use refact_chat_api::ChatCommand;
use refact_runtime_api::CreateSessionRequest;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AbVariantSpec {
    pub model: Option<String>,
    pub extra_instructions: Option<String>,
    pub suggested_steps: usize,
}

#[derive(Debug)]
pub struct PreparedAbVariant {
    pub agent_id: String,
    pub chat_id: String,
    pub model: String,
    pub prepared: PreparedWorktree,
}

#[derive(Debug)]
pub struct PreparedAbVariants {
    pub a: PreparedAbVariant,
    pub b: PreparedAbVariant,
}

pub struct ToolSpawnAb;
pub struct ToolPickAbWinner;

const STALE_PICK_AB_WINNER: &str = "stale A/B winner selection";

impl ToolSpawnAb {
    pub fn new() -> Self {
        Self
    }
}

impl ToolPickAbWinner {
    pub fn new() -> Self {
        Self
    }
}

fn make_source() -> ToolSource {
    ToolSource {
        source_type: ToolSourceType::Builtin,
        config_path: String::new(),
    }
}

fn validate_id(id: &str, name: &str) -> Result<(), String> {
    if id
        .chars()
        .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
    {
        Ok(())
    } else {
        Err(format!(
            "{} '{}' contains invalid characters (only alphanumeric, '-', '_' allowed)",
            name, id
        ))
    }
}

fn parse_suggested_steps(value: Option<&Value>) -> Result<usize, String> {
    let steps = match value {
        Some(Value::String(s)) => s
            .parse::<usize>()
            .map_err(|_| "suggested_steps must be a non-negative integer".to_string())?,
        Some(Value::Number(n)) => n
            .as_u64()
            .ok_or_else(|| "suggested_steps must be a non-negative integer".to_string())?
            as usize,
        Some(Value::Null) | None => 30,
        Some(_) => return Err("suggested_steps must be a non-negative integer".to_string()),
    };
    Ok(steps.min(50).max(1))
}

fn parse_optional_string(value: Option<&Value>, key: &str) -> Result<Option<String>, String> {
    match value {
        Some(Value::String(s)) if !s.trim().is_empty() => Ok(Some(s.clone())),
        Some(Value::String(_)) | Some(Value::Null) | None => Ok(None),
        Some(_) => Err(format!("{} must be a string", key)),
    }
}

fn parse_variant(args: &HashMap<String, Value>, key: &str) -> Result<AbVariantSpec, String> {
    let value = args.get(key).ok_or_else(|| format!("Missing '{}'", key))?;
    let object = value
        .as_object()
        .ok_or_else(|| format!("{} must be an object", key))?;
    Ok(AbVariantSpec {
        model: parse_optional_string(object.get("model"), "model")?,
        extra_instructions: parse_optional_string(
            object.get("extra_instructions"),
            "extra_instructions",
        )?,
        suggested_steps: parse_suggested_steps(object.get("suggested_steps"))?,
    })
}

pub async fn planner_context(
    ccx: &Arc<AMutex<AtCommandsContext>>,
    args: &HashMap<String, Value>,
    tool_name: &str,
) -> Result<(AppState, String, String, String), String> {
    let (app, current_model, chat_id, root_chat_id, task_meta, requested_task_id) = {
        let ccx_lock = ccx.lock().await;
        let is_planner = ccx_lock
            .task_meta
            .as_ref()
            .map(|m| m.role == "planner")
            .unwrap_or(false);
        if !is_planner {
            return Err(format!(
                "{} can only be called by the task planner.",
                tool_name
            ));
        }
        (
            ccx_lock.app.clone(),
            ccx_lock.current_model.clone(),
            ccx_lock.chat_id.clone(),
            ccx_lock.root_chat_id.clone(),
            ccx_lock.task_meta.clone(),
            args.get("task_id")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(str::to_string),
        )
    };
    let task_id = task_meta
        .as_ref()
        .map(|m| m.task_id.clone())
        .or(requested_task_id)
        .or_else(|| storage::infer_task_id_from_chat_id(&chat_id))
        .ok_or_else(|| "Missing 'task_id' (and chat is not bound to a task)".to_string())?;
    let planner_chat_id =
        resolve_invoking_planner_chat_id(app.gcx.clone(), &chat_id, &root_chat_id, task_meta.as_ref())
            .await;
    Ok((app, task_id, planner_chat_id, current_model))
}

fn ab_prompt_instructions(card_instructions: &str, variant: &AbVariantSpec) -> String {
    match variant.extra_instructions.as_deref() {
        Some(extra) => format!(
            "{}\n\n## Variant Instructions\n{}",
            card_instructions, extra
        ),
        None => card_instructions.to_string(),
    }
}

pub fn variant_info(variant: &PreparedAbVariant) -> AbVariantInfo {
    AbVariantInfo {
        agent_id: variant.agent_id.clone(),
        chat_id: variant.chat_id.clone(),
        worktree: variant
            .prepared
            .worktree_path()
            .to_string_lossy()
            .to_string(),
        worktree_name: Some(variant.prepared.worktree_name()),
        branch: variant.prepared.branch_name(),
        model: Some(variant.model.clone()),
        finish: None,
    }
}

pub async fn prepare_ab_worktrees(
    app: &AppState,
    task_meta: &refact_tasks::types::TaskMeta,
    task_id: &str,
    card_id: &str,
    model_a: String,
    model_b: String,
) -> Result<PreparedAbVariants, String> {
    let ctx = app.chat_ctx();
    let agent_id_a = Uuid::new_v4().to_string();
    let agent_id_b = Uuid::new_v4().to_string();
    let chat_id_a = format!("agent-{}-{}-a", card_id, &agent_id_a[..8]);
    let chat_id_b = format!("agent-{}-{}-b", card_id, &agent_id_b[..8]);

    let prepared_a = prepare_agent_worktree_with_suffix(
        &ctx,
        task_meta,
        task_id,
        &agent_id_a,
        card_id,
        &chat_id_a,
        Some("-a"),
    )
    .await?;

    let prepared_b = match prepare_agent_worktree_with_suffix(
        &ctx,
        task_meta,
        task_id,
        &agent_id_b,
        card_id,
        &chat_id_b,
        Some("-b"),
    )
    .await
    {
        Ok(prepared) => prepared,
        Err(error) => {
            prepared_a
                .cleanup_unlinked(app.paths.cache_dir.clone())
                .await;
            return Err(error);
        }
    };

    Ok(PreparedAbVariants {
        a: PreparedAbVariant {
            agent_id: agent_id_a,
            chat_id: chat_id_a,
            model: model_a,
            prepared: prepared_a,
        },
        b: PreparedAbVariant {
            agent_id: agent_id_b,
            chat_id: chat_id_b,
            model: model_b,
            prepared: prepared_b,
        },
    })
}

async fn create_variant_session(
    app: &AppState,
    task_id: &str,
    planner_chat_id: &str,
    card_id: &str,
    card_title: &str,
    card_instructions: &str,
    dependency_context: &str,
    spec: &AbVariantSpec,
    variant: &PreparedAbVariant,
    worktree: WorktreeMeta,
) -> Result<(), String> {
    let thread = build_agent_thread_params(
        &variant.chat_id,
        card_title,
        &variant.model,
        task_id,
        &variant.agent_id,
        card_id,
        planner_chat_id,
        worktree,
    );
    let prompt_instructions = ab_prompt_instructions(card_instructions, spec);
    let user_prompt = build_agent_prompt(
        card_title,
        &prompt_instructions,
        dependency_context,
        spec.suggested_steps,
    );
    let initial_message = event(
        EventSubkind::SystemNotice,
        "tool.spawn_ab",
        json!({
            "task_id": task_id,
            "planner_chat_id": planner_chat_id,
            "card_id": card_id,
            "card_title": card_title,
            "agent_id": &variant.agent_id,
            "agent_chat_id": &variant.chat_id,
            "model": &variant.model,
            "suggested_steps": spec.suggested_steps,
            "has_extra_instructions": spec.extra_instructions.is_some(),
            "worktree": variant.prepared.worktree_path().to_string_lossy().to_string(),
            "worktree_name": variant.prepared.worktree_name(),
            "branch": variant.prepared.branch_name(),
        }),
        user_prompt,
    );
    app.chat
        .facade
        .create_session(CreateSessionRequest {
            chat_id: variant.chat_id.clone(),
            thread,
            messages: vec![initial_message],
        })
        .await?;
    app.chat.facade.maybe_save_session(&variant.chat_id).await
}

async fn start_variant_session(app: &AppState, chat_id: &str) -> Result<(), String> {
    app.chat
        .facade
        .push_priority_command(chat_id, ChatCommand::Regenerate {})
        .await
}

async fn restore_card_after_spawn_ab_failure(
    ctx: &ChatCtx,
    task_id: &str,
    original_card: BoardCard,
    chat_id_a: String,
    chat_id_b: String,
) {
    let _ = storage::update_board_atomic_for_ctx(ctx, task_id, move |board| {
        if let Some(card) = board.get_card_mut(&original_card.id) {
            let matches_ab = card
                .ab_variants
                .as_ref()
                .map(|ab| ab.a.chat_id == chat_id_a && ab.b.chat_id == chat_id_b)
                .unwrap_or(false);
            if matches_ab {
                *card = original_card.clone();
            }
        }
        Ok(())
    })
    .await;
    let _ = storage::update_task_stats_for_ctx(ctx, task_id).await;
}

fn winner_parts<'a>(
    variants: &'a AbVariants,
    winner: &str,
) -> Result<(&'a AbVariantInfo, &'a AbVariantInfo), String> {
    match winner {
        "a" => Ok((&variants.a, &variants.b)),
        "b" => Ok((&variants.b, &variants.a)),
        _ => Err(format!("Invalid winner '{}', must be 'a' or 'b'", winner)),
    }
}

pub async fn cleanup_ab_variant(app: &AppState, variant: &AbVariantInfo) -> Result<(), String> {
    cleanup_ab_variant_with_timeout(app, variant, AGENT_ABORT_TIMEOUT).await
}

pub async fn cleanup_ab_variant_with_timeout(
    app: &AppState,
    variant: &AbVariantInfo,
    abort_timeout: std::time::Duration,
) -> Result<(), String> {
    wait_for_agent_abort(&app.chat_ctx(), &variant.chat_id, abort_timeout)
        .await
        .map_err(|e| {
            format!(
                "Failed to abort A/B variant {} before cleanup: {}",
                variant.chat_id, e
            )
        })?;

    let project_dirs =
        refact_runtime_api::files_correction::get_unscoped_project_dirs_for_ctx(&app.chat_ctx()).await;
    let workspace_root = project_dirs.first().ok_or("No workspace folder found")?;

    if let Some(worktree_name) = variant.worktree_name.as_deref() {
        if let Ok(service) =
            WorktreeService::new_async(app.paths.cache_dir.clone(), workspace_root.clone()).await
        {
            if service
                .delete_worktree(worktree_name, true, true)
                .await
                .is_ok()
            {
                return Ok(());
            }
        }
    }

    fn removal_is_contended(error: &std::io::Error) -> bool {
        #[cfg(windows)]
        {
            const ERROR_SHARING_VIOLATION: i32 = 32;
            const ERROR_LOCK_VIOLATION: i32 = 33;
            const ERROR_ACCESS_DENIED: i32 = 5;
            return matches!(
                error.raw_os_error(),
                Some(ERROR_SHARING_VIOLATION)
                    | Some(ERROR_LOCK_VIOLATION)
                    | Some(ERROR_ACCESS_DENIED)
            );
        }
        #[cfg(not(windows))]
        {
            let _ = error;
            false
        }
    }

    fn remove_worktree_dir_with_retry(path: &str) -> std::io::Result<()> {
        let mut delay = std::time::Duration::from_millis(20);
        for _ in 0..6 {
            match std::fs::remove_dir_all(path) {
                Ok(()) => return Ok(()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
                Err(error) if removal_is_contended(&error) => {
                    std::thread::sleep(delay);
                    delay = (delay * 2).min(std::time::Duration::from_millis(400));
                }
                Err(error) => return Err(error),
            }
        }
        std::fs::remove_dir_all(path)
    }

    let worktree_removed = Command::new("git")
        .args(["worktree", "remove", &variant.worktree, "--force"])
        .current_dir(workspace_root)
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false);
    if !worktree_removed && std::path::Path::new(&variant.worktree).exists() {
        remove_worktree_dir_with_retry(&variant.worktree).map_err(|e| {
            format!(
                "Failed to remove loser worktree '{}': {}",
                variant.worktree, e
            )
        })?;
    }
    if let Some(branch) = variant.branch.as_deref() {
        let _ = Command::new("git")
            .args(["branch", "-D", branch])
            .current_dir(workspace_root)
            .output();
    }
    Ok(())
}

pub async fn promote_ab_winner_if_current(
    ctx: &ChatCtx,
    task_id: &str,
    card_id: &str,
    winner: &str,
    expected_state: ExpectedCardState,
    winner_variant: AbVariantInfo,
    winner_finish: refact_tasks::types::AbVariantFinish,
) -> Result<Option<ExpectedCardState>, String> {
    let card_id_owned = card_id.to_string();
    let task_id_owned = task_id.to_string();
    let winner_owned = winner.to_string();
    let winner_for_board = winner_variant;
    let winner_finish_for_board = winner_finish;
    let result = storage::update_board_atomic_for_ctx(ctx, task_id, move |board| {
        let board_rev = board.rev;
        let next_board_rev = board.rev + 1;
        let card = board
            .get_card_mut(&card_id_owned)
            .ok_or(format!("Card {} not found", card_id_owned))?;
        if !expected_state.matches_board_card(board_rev, card) {
            return Err(format!(
                "{} for task {} card {}: current card state no longer matches winner request",
                STALE_PICK_AB_WINNER, task_id_owned, card_id_owned
            ));
        }
        let variants = card
            .ab_variants
            .as_mut()
            .ok_or_else(|| format!("Card {} has no A/B variants", card_id_owned))?;
        if let Some(existing_winner) = variants.winner.as_deref() {
            return Err(format!(
                "Card {} already picked A/B winner {}",
                card_id_owned, existing_winner
            ));
        }
        let current_winner = variants
            .variant(&winner_owned)
            .ok_or_else(|| format!("Invalid winner '{}', must be 'a' or 'b'", winner_owned))?;
        if current_winner.chat_id != winner_for_board.chat_id {
            return Err(format!(
                "{} for task {} card {}: A/B winner variant changed",
                STALE_PICK_AB_WINNER, task_id_owned, card_id_owned
            ));
        }
        if current_winner.finish.as_ref() != Some(&winner_finish_for_board) {
            return Err(format!(
                "{} for task {} card {}: A/B winner finish changed",
                STALE_PICK_AB_WINNER, task_id_owned, card_id_owned
            ));
        }
        card.agent_worktree = Some(winner_for_board.worktree.clone());
        card.agent_worktree_name = winner_for_board.worktree_name.clone();
        card.agent_branch = winner_for_board.branch.clone();
        card.agent_chat_id = Some(winner_for_board.chat_id.clone());
        card.assignee = Some(winner_for_board.agent_id.clone());
        card.final_report = Some(if winner_finish_for_board.success {
            winner_finish_for_board.final_report.clone()
        } else {
            format!("FAILED: {}", winner_finish_for_board.final_report)
        });
        card.final_report_structured = winner_finish_for_board.final_report_structured.clone();
        card.column = if winner_finish_for_board.success {
            "done".to_string()
        } else {
            "failed".to_string()
        };
        card.completed_at = Some(Utc::now().to_rfc3339());
        if let Some(variants) = card.ab_variants.as_mut() {
            variants.winner = Some(winner_owned.clone());
        }
        card.status_updates.push(StatusUpdate {
            timestamp: Utc::now().to_rfc3339(),
            message: format!("A/B winner: {}; loser cleanup pending", winner_owned),
        });
        Ok(ExpectedCardState::from_card(next_board_rev, card))
    })
    .await;
    match result {
        Ok((_, expected)) => Ok(Some(expected)),
        Err(error) if error.starts_with(STALE_PICK_AB_WINNER) => {
            tracing::info!("{}", error);
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

async fn mark_ab_loser_cleaned_if_current(
    ctx: &ChatCtx,
    task_id: &str,
    card_id: &str,
    expected_state: ExpectedCardState,
    winner: &str,
    loser_key: &str,
) {
    let card_id_owned = card_id.to_string();
    let winner_owned = winner.to_string();
    let loser_key_owned = loser_key.to_string();
    let _ = storage::update_board_atomic_for_ctx(ctx, task_id, move |board| {
        let board_rev = board.rev;
        if let Some(card) = board.get_card_mut(&card_id_owned) {
            if expected_state.matches_board_card(board_rev, card) {
                card.status_updates.push(StatusUpdate {
                    timestamp: Utc::now().to_rfc3339(),
                    message: format!(
                        "A/B winner: {}, loser {} cleaned up",
                        winner_owned, loser_key_owned
                    ),
                });
            }
        }
        Ok(())
    })
    .await;
}

async fn ab_card_state_matches(
    ctx: &ChatCtx,
    task_id: &str,
    card_id: &str,
    expected_state: &ExpectedCardState,
) -> Result<bool, String> {
    let board = storage::load_board_for_ctx(ctx, task_id).await?;
    Ok(board
        .get_card(card_id)
        .is_some_and(|card| expected_state.matches_board_card(board.rev, card)))
}

pub async fn pick_ab_winner_impl(
    app: &AppState,
    task_id: &str,
    card_id: &str,
    winner: &str,
) -> Result<(String, String), String> {
    if winner != "a" && winner != "b" {
        return Err(format!("Invalid winner '{}', must be 'a' or 'b'", winner));
    }
    let ctx = app.chat_ctx();

    let board = storage::load_board_for_ctx(&ctx, task_id).await?;
    let card = board
        .get_card(card_id)
        .ok_or_else(|| format!("Card {} not found", card_id))?;
    let variants = card
        .ab_variants
        .clone()
        .ok_or_else(|| format!("Card {} has no A/B variants", card_id))?;
    if let Some(existing_winner) = variants.winner.as_deref() {
        return Err(format!(
            "Card {} already picked A/B winner {}",
            card_id, existing_winner
        ));
    }
    let (winner_variant, loser_variant) = winner_parts(&variants, winner)?;
    let winner_finish = winner_variant.finish.clone().ok_or_else(|| {
        format!(
            "A/B winner {} for card {} has not finished yet",
            winner, card_id
        )
    })?;
    let expected_state = ExpectedCardState::from_card(board.rev, card);
    let winner_variant = winner_variant.clone();
    let loser_variant = loser_variant.clone();
    let loser_key = if winner == "a" { "b" } else { "a" }.to_string();
    let success_for_board = winner_finish.success;
    let verifier_expected_state = promote_ab_winner_if_current(
        &ctx,
        task_id,
        card_id,
        winner,
        expected_state,
        winner_variant.clone(),
        winner_finish,
    )
    .await?
    .ok_or_else(|| format!("{} for card {}", STALE_PICK_AB_WINNER, card_id))?;
    storage::update_task_stats_for_ctx(&ctx, task_id).await?;
    if !ab_card_state_matches(&ctx, task_id, card_id, &verifier_expected_state).await? {
        return Err(format!("{} for card {}", STALE_PICK_AB_WINNER, card_id));
    }
    cleanup_ab_variant(app, &loser_variant).await?;
    mark_ab_loser_cleaned_if_current(
        &ctx,
        task_id,
        card_id,
        verifier_expected_state.clone(),
        winner,
        &loser_key,
    )
    .await;
    if success_for_board {
        schedule_card_verifier_after_finish_for_ctx(
            &ctx,
            task_id.to_string(),
            card_id.to_string(),
            verifier_expected_state,
        )
        .await;
    }
    Ok((winner_variant.chat_id, loser_key))
}

#[async_trait]
impl Tool for ToolSpawnAb {
    fn tool_description(&self) -> ToolDesc {
        ToolDesc {
            name: "spawn_ab".to_string(),
            display_name: "Spawn A/B Agents".to_string(),
            source: make_source(),
            experimental: false,
            allow_parallel: false,
            description: "Spawn two task agents for the same planned card in separate worktrees so the planner can pick a winner.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "task_id": { "type": "string", "description": "Task UUID (optional in planner context)" },
                    "card_id": { "type": "string", "description": "Planned card ID to race" },
                    "variant_a": {
                        "type": "object",
                        "properties": {
                            "model": { "type": "string" },
                            "extra_instructions": { "type": "string" },
                            "suggested_steps": { "type": "integer" }
                        }
                    },
                    "variant_b": {
                        "type": "object",
                        "properties": {
                            "model": { "type": "string" },
                            "extra_instructions": { "type": "string" },
                            "suggested_steps": { "type": "integer" }
                        }
                    }
                },
                "required": ["card_id", "variant_a", "variant_b"]
            }),
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
        let (app, task_id, planner_chat_id, current_model) =
            planner_context(&ccx, args, "spawn_ab").await?;
        let ctx = app.chat_ctx();
        let tool = app.tool.clone();
        let card_id = args
            .get("card_id")
            .and_then(|v| v.as_str())
            .ok_or("Missing 'card_id'")?;
        validate_id(&task_id, "task_id")?;
        validate_id(card_id, "card_id")?;
        let variant_a = parse_variant(args, "variant_a")?;
        let variant_b = parse_variant(args, "variant_b")?;
        let task_meta = storage::load_task_meta_for_ctx(&ctx, &task_id).await?;
        let model_a = match variant_a.model.as_deref() {
            Some(model) => model.to_string(),
            None => resolve_agent_model(&tool.caps, &current_model).await?,
        };
        let model_b = match variant_b.model.as_deref() {
            Some(model) => model.to_string(),
            None => resolve_agent_model(&tool.caps, &current_model).await?,
        };

        let board = storage::load_board_for_ctx(&ctx, &task_id).await?;
        let abandoned_worktrees = find_abandoned_worktrees(&board);
        if !abandoned_worktrees.is_empty() {
            return Err(abandoned_worktrees_error(
                "spawn A/B agents",
                &abandoned_worktrees,
            ));
        }
        let card = board
            .get_card(card_id)
            .ok_or_else(|| format!("Card {} not found", card_id))?;
        if card.column != "planned" {
            return Err(format!(
                "Card {} is in column '{}', expected 'planned'",
                card_id, card.column
            ));
        }
        if card.ab_variants.is_some() {
            return Err(format!("Card {} already has A/B variants", card_id));
        }
        let original_card = card.clone();
        let card_title = card.title.clone();
        let card_instructions = card.instructions.clone();
        let dependency_context = board
            .get_dependency_reports(card_id)
            .into_iter()
            .map(|(title, report)| format!("### {}\n{}", title, report))
            .collect::<Vec<_>>()
            .join("\n\n");

        let prepared =
            prepare_ab_worktrees(&app, &task_meta, &task_id, card_id, model_a, model_b).await?;
        let variants = AbVariants {
            a: variant_info(&prepared.a),
            b: variant_info(&prepared.b),
            winner: None,
        };

        let card_id_owned = card_id.to_string();
        let variants_for_board = variants.clone();
        let board_update = storage::update_board_atomic_for_ctx(&ctx, &task_id, move |board| {
            let card = board
                .get_card_mut(&card_id_owned)
                .ok_or(format!("Card {} not found", card_id_owned))?;
            if card.column != "planned" {
                return Err(format!(
                    "Card {} is in column '{}', expected 'planned'",
                    card_id_owned, card.column
                ));
            }
            if card.ab_variants.is_some() {
                return Err(format!("Card {} already has A/B variants", card_id_owned));
            }
            card.column = "doing".to_string();
            card.assignee = Some("ab".to_string());
            card.agent_chat_id = None;
            card.agent_branch = None;
            card.agent_worktree = None;
            card.agent_worktree_name = None;
            card.started_at = Some(Utc::now().to_rfc3339());
            card.ab_variants = Some(variants_for_board.clone());
            card.status_updates.push(StatusUpdate {
                timestamp: Utc::now().to_rfc3339(),
                message: format!(
                    "A/B spawned: A={}, B={}",
                    variants_for_board.a.chat_id, variants_for_board.b.chat_id
                ),
            });
            Ok(())
        })
        .await;
        if let Err(error) = board_update {
            prepared
                .a
                .prepared
                .cleanup_unlinked(app.paths.cache_dir.clone())
                .await;
            prepared
                .b
                .prepared
                .cleanup_unlinked(app.paths.cache_dir.clone())
                .await;
            return Err(error);
        }
        storage::update_task_stats_for_ctx(&ctx, &task_id).await?;

        let session_result = async {
            create_variant_session(
                &app,
                &task_id,
                &planner_chat_id,
                card_id,
                &card_title,
                &card_instructions,
                &dependency_context,
                &variant_a,
                &prepared.a,
                prepared.a.prepared.worktree_meta(),
            )
            .await?;
            create_variant_session(
                &app,
                &task_id,
                &planner_chat_id,
                card_id,
                &card_title,
                &card_instructions,
                &dependency_context,
                &variant_b,
                &prepared.b,
                prepared.b.prepared.worktree_meta(),
            )
            .await?;
            start_variant_session(&app, &prepared.a.chat_id).await?;
            start_variant_session(&app, &prepared.b.chat_id).await
        }
        .await;
        if let Err(error) = session_result {
            restore_card_after_spawn_ab_failure(
                &ctx,
                &task_id,
                original_card,
                prepared.a.chat_id.clone(),
                prepared.b.chat_id.clone(),
            )
            .await;
            prepared
                .a
                .prepared
                .cleanup_unlinked(app.paths.cache_dir.clone())
                .await;
            prepared
                .b
                .prepared
                .cleanup_unlinked(app.paths.cache_dir.clone())
                .await;
            return Err(error);
        }

        let result_message = format!(
            "# A/B Agents Spawned\n\n**Card:** {}\n**A:** {} ({})\n**B:** {} ({})\n\nUse `pick_ab_winner(card_id=\"{}\", winner=\"a\"|\"b\")` after comparing the variants.",
            card_id,
            variants.a.chat_id,
            prepared.a.model,
            variants.b.chat_id,
            prepared.b.model,
            card_id
        );
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
    }

    fn tool_depends_on(&self) -> Vec<String> {
        vec![]
    }
}

#[async_trait]
impl Tool for ToolPickAbWinner {
    fn tool_description(&self) -> ToolDesc {
        ToolDesc {
            name: "pick_ab_winner".to_string(),
            display_name: "Pick A/B Winner".to_string(),
            source: make_source(),
            experimental: false,
            allow_parallel: false,
            description: "Pick the winning A/B task-agent variant, promote its worktree to the primary card worktree, and delete the loser.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "task_id": { "type": "string", "description": "Task UUID (optional in planner context)" },
                    "card_id": { "type": "string", "description": "A/B card ID" },
                    "winner": { "type": "string", "enum": ["a", "b"], "description": "Winning variant" }
                },
                "required": ["card_id", "winner"]
            }),
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
        let (app, task_id, _, _) = planner_context(&ccx, args, "pick_ab_winner").await?;
        let card_id = args
            .get("card_id")
            .and_then(|v| v.as_str())
            .ok_or("Missing 'card_id'")?;
        let winner = args
            .get("winner")
            .and_then(|v| v.as_str())
            .ok_or("Missing 'winner'")?;
        validate_id(&task_id, "task_id")?;
        validate_id(card_id, "card_id")?;
        let (winner_chat_id, loser_key) =
            pick_ab_winner_impl(&app, &task_id, card_id, winner).await?;
        let result_message = format!(
            "# A/B Winner Picked\n\n**Card:** {}\n**Winner:** {} ({})\n**Loser:** {} cleaned up",
            card_id, winner, winner_chat_id, loser_key
        );
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
    }

    fn tool_depends_on(&self) -> Vec<String> {
        vec![]
    }
}
