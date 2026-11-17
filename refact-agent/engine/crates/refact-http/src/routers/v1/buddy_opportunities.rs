use axum::extract::State;
use axum::extract::Path;
use axum::extract::Query;
use axum::response::Result;
use hyper::StatusCode;
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path as StdPath, PathBuf};
use uuid::Uuid;

use refact_buddy_core::drafts::{draft_kind_str, DraftCreateError, DraftTarget, DraftValidationError};
use refact_buddy_core::opportunities::is_terminal_status;
use refact_buddy_core::types::{
    BuddyAction, BuddyDraft, BuddyFactKind, CustomizationKind, DraftKind, InvestigationContext,
    MarketKind, OpportunityStatus,
};
use refact_buddy_core::action_policy::{action_execution, action_kind_str, ActionExecution};
use refact_buddy_core::verdicts::VerdictOutcome;
use refact_runtime_api::app_state::AppState;
use refact_core::custom_error::ScratchError;
use refact_runtime_api::ext_config_dirs::get_ext_dirs;
use refact_runtime_api::ext_marketplace::{
    install_marketplace_item, list_marketplace_items, InstallMarketplaceItemRequest,
    MarketplaceKind,
};
use refact_runtime_api::chat_ctx::ChatCtx;
use refact_runtime_api::files_correction::get_unscoped_project_dirs_for_ctx;
use refact_chat_history::trajectory_snapshot::TrajectorySnapshot;

pub use refact_buddy_core::buddy_opportunities::*;

#[derive(Debug, Deserialize)]
pub struct OpportunitiesQuery {
    pub status: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
pub struct AcceptRequest {
    #[serde(default)]
    pub action_index: usize,
}

#[derive(Debug, Deserialize, Default)]
pub struct DismissRequest {
    #[serde(default)]
    pub never: bool,
}

#[derive(Debug, Deserialize)]
pub struct UndoRequest {
    pub receipt_id: String,
}

#[derive(Debug, Deserialize)]
pub struct UnmuteRequest {
    pub rule_key: String,
}

pub struct ActionOutcome {
    pub result: serde_json::Value,
    pub status: OpportunityStatus,
}

fn draft_create_error(err: DraftCreateError) -> ScratchError {
    ScratchError::new(StatusCode::PAYLOAD_TOO_LARGE, err.to_string())
}

fn draft_validation_error(err: DraftValidationError) -> ScratchError {
    match err {
        DraftValidationError::NotFound => {
            ScratchError::new(StatusCode::NOT_FOUND, "draft_not_found".to_string())
        }
        DraftValidationError::KindMismatch { expected, actual } => ScratchError::new(
            StatusCode::CONFLICT,
            format!(
                "draft_kind_mismatch: expected {}, got {}",
                draft_kind_str(&expected),
                draft_kind_str(&actual)
            ),
        ),
        DraftValidationError::TargetMismatch { expected, actual } => ScratchError::new(
            StatusCode::CONFLICT,
            format!(
                "draft_target_mismatch: expected {}, got {}",
                expected, actual
            ),
        ),
        DraftValidationError::Parse(err) => ScratchError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("draft_parse_failed: {}", err),
        ),
    }
}

async fn validate_existing_draft(
    ctx: &ChatCtx,
    draft_id: &str,
    expected_kind: DraftKind,
    target: DraftTarget<'_>,
) -> Result<(), ScratchError> {
    let buddy_arc = ctx.buddy.buddy.clone();
    let lock = buddy_arc.lock().await;
    let svc = lock.as_ref().ok_or_else(|| {
        ScratchError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "buddy not initialized".into(),
        )
    })?;
    svc.draft_store
        .get_validated(draft_id, expected_kind, target)
        .map(|_| ())
        .map_err(draft_validation_error)
}

pub async fn handle_v1_buddy_opportunities_list(
    State(app): State<AppState>,
    Query(query): Query<OpportunitiesQuery>,
) -> Result<axum::Json<serde_json::Value>, ScratchError> {
    let buddy_arc = app.buddy.buddy.clone();
    let lock = buddy_arc.lock().await;
    let svc = lock.as_ref().ok_or_else(|| {
        ScratchError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "buddy not initialized".into(),
        )
    })?;
    let mut opps = svc.opportunity_queue.snapshot();
    if let Some(status_filter) = &query.status {
        let allowed: Vec<&str> = status_filter.split(',').map(|s| s.trim()).collect();
        opps.retain(|o| {
            let s = serde_json::to_value(&o.status)
                .ok()
                .and_then(|v| v.as_str().map(|s| s.to_string()))
                .unwrap_or_default();
            allowed.contains(&s.as_str())
        });
    }
    Ok(axum::Json(serde_json::json!({ "opportunities": opps })))
}

pub async fn handle_v1_buddy_opportunity_accept(
    State(app): State<AppState>,
    Path(id): Path<String>,
    body: Option<axum::extract::Json<AcceptRequest>>,
) -> Result<axum::Json<serde_json::Value>, ScratchError> {
    let req = body.map(|b| b.0).unwrap_or_default();

    let buddy_arc = app.buddy.buddy.clone();
    let (action, cooldown_key, opp_kind) = {
        let mut lock = buddy_arc.lock().await;
        let svc = lock.as_mut().ok_or_else(|| {
            ScratchError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "buddy not initialized".into(),
            )
        })?;
        let opp = svc.opportunity_queue.get(&id).cloned().ok_or_else(|| {
            ScratchError::new(
                StatusCode::NOT_FOUND,
                format!("opportunity not found: {}", id),
            )
        })?;
        if is_terminal_status(opp.status) {
            return Err(ScratchError::new(
                StatusCode::CONFLICT,
                format!("opportunity already resolved: {:?}", opp.status),
            ));
        }
        if svc.is_opportunity_accept_claimed(&id) {
            return Err(ScratchError::new(
                StatusCode::CONFLICT,
                format!("opportunity already in progress: {}", id),
            ));
        }
        let action = opp
            .proposed_actions
            .get(req.action_index)
            .ok_or_else(|| {
                ScratchError::new(
                    StatusCode::BAD_REQUEST,
                    format!("action_index {} out of range", req.action_index),
                )
            })?
            .clone();
        if matches!(
            action_execution(svc.settings.autonomy_level, &action),
            ActionExecution::Deny
        ) {
            return Err(ScratchError::new(
                StatusCode::FORBIDDEN,
                "action denied by autonomy level".to_string(),
            ));
        }
        if !svc.claim_opportunity_accept(&id) {
            return Err(ScratchError::new(
                StatusCode::CONFLICT,
                format!("opportunity already in progress: {}", id),
            ));
        }
        (action, opp.cooldown_key.clone(), opp.kind)
    };

    let outcome = match dispatch_action(app.clone(), &id, &action).await {
        Ok(outcome) => outcome,
        Err(err) => {
            clear_accept_claim(&app.chat_ctx(), &id).await;
            return Err(err);
        }
    };

    let buddy_arc = app.buddy.buddy.clone();
    let mut lock = buddy_arc.lock().await;
    let svc = lock.as_mut().ok_or_else(|| {
        ScratchError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "buddy not initialized".into(),
        )
    })?;
    svc.clear_opportunity_accept_claim(&id);
    if !svc.resolve_opportunity(&id, outcome.status) {
        return Err(ScratchError::new(
            StatusCode::NOT_FOUND,
            format!("opportunity not found: {}", id),
        ));
    }
    let verdict = if matches!(action, BuddyAction::Dismiss) {
        VerdictOutcome::Dismiss
    } else {
        VerdictOutcome::Accept
    };
    svc.record_verdict(&cooldown_key, opp_kind, action_kind_str(&action), verdict);
    let snap = serde_json::to_value(svc.snapshot())
        .map_err(|e| ScratchError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(axum::Json(serde_json::json!({
        "snapshot": snap,
        "action_result": outcome.result
    })))
}

pub async fn handle_v1_buddy_action_undo(
    State(app): State<AppState>,
    axum::extract::Json(req): axum::extract::Json<UndoRequest>,
) -> Result<axum::Json<serde_json::Value>, ScratchError> {
    let project_root = {
        let buddy_arc = app.buddy.buddy.clone();
        let lock = buddy_arc.lock().await;
        let svc = lock.as_ref().ok_or_else(|| {
            ScratchError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "buddy not initialized".into(),
            )
        })?;
        let action = BuddyAction::ApplyConfigPatch {
            draft_id: String::new(),
            target_path: String::new(),
        };
        if !matches!(
            action_execution(svc.settings.autonomy_level, &action),
            ActionExecution::Execute
        ) {
            return Err(ScratchError::new(
                StatusCode::FORBIDDEN,
                "action denied by autonomy level".to_string(),
            ));
        }
        svc.project_root.clone()
    };
    let receipt = refact_buddy_core::receipts::undo_receipt(&project_root, &req.receipt_id)
        .await
        .map_err(|e| ScratchError::new(StatusCode::BAD_REQUEST, e))?;
    let buddy_arc = app.buddy.buddy.clone();
    let mut lock = buddy_arc.lock().await;
    if let Some(svc) = lock.as_mut() {
        svc.record_verdict(
            &format!("action:{}", receipt.action_kind),
            refact_buddy_core::types::BuddyOpportunityKind::JobFinding,
            &receipt.action_kind,
            VerdictOutcome::Undo,
        );
    }
    Ok(axum::Json(serde_json::json!({ "receipt": receipt })))
}

pub async fn handle_v1_buddy_receipts(
    State(app): State<AppState>,
) -> Result<axum::Json<serde_json::Value>, ScratchError> {
    let project_root = buddy_project_root(&app).await?;
    let receipts = refact_buddy_core::receipts::load_receipts(&project_root).await;
    Ok(axum::Json(serde_json::json!({ "receipts": receipts })))
}

pub async fn handle_v1_buddy_rule_unmute(
    State(app): State<AppState>,
    axum::extract::Json(req): axum::extract::Json<UnmuteRequest>,
) -> Result<axum::Json<serde_json::Value>, ScratchError> {
    let buddy_arc = app.buddy.buddy.clone();
    let mut lock = buddy_arc.lock().await;
    let svc = lock.as_mut().ok_or_else(|| {
        ScratchError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "buddy not initialized".into(),
        )
    })?;
    let changed = svc.unmute_rule(&req.rule_key);
    let snap = serde_json::to_value(svc.snapshot())
        .map_err(|e| ScratchError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(axum::Json(
        serde_json::json!({ "changed": changed, "snapshot": snap }),
    ))
}

async fn buddy_project_root(app: &AppState) -> Result<PathBuf, ScratchError> {
    let buddy_arc = app.buddy.buddy.clone();
    let lock = buddy_arc.lock().await;
    let svc = lock.as_ref().ok_or_else(|| {
        ScratchError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "buddy not initialized".into(),
        )
    })?;
    Ok(svc.project_root.clone())
}

async fn clear_accept_claim(ctx: &ChatCtx, id: &str) {
    let buddy_arc = ctx.buddy.buddy.clone();
    let mut lock = buddy_arc.lock().await;
    if let Some(svc) = lock.as_mut() {
        svc.clear_opportunity_accept_claim(id);
    }
}

pub async fn dispatch_action(
    app: AppState,
    _opp_id: &str,
    action: &BuddyAction,
) -> Result<ActionOutcome, ScratchError> {
    match action {
        BuddyAction::OpenPage { page } => {
            let nav_page = serde_json::to_value(page)
                .map_err(|e| ScratchError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
            Ok(ActionOutcome {
                result: serde_json::json!({
                    "kind": "open_page",
                    "navigate_to": nav_page
                }),
                status: OpportunityStatus::Accepted,
            })
        }
        BuddyAction::LaunchInvestigationChat { preload } => {
            let mut enriched_ctx = preload.clone();
            enrich_investigation_context(&app, &mut enriched_ctx).await;
            let chat_id = create_investigation_chat(&app, &enriched_ctx).await?;
            Ok(ActionOutcome {
                result: serde_json::json!({
                    "kind": "launch_investigation_chat",
                    "chat_id": chat_id
                }),
                status: OpportunityStatus::Accepted,
            })
        }
        BuddyAction::DraftSkill { draft_id, label }
        | BuddyAction::DraftCommand { draft_id, label }
        | BuddyAction::DraftDelegate { draft_id, label }
        | BuddyAction::DraftMode { draft_id, label } => {
            let dk = match action {
                BuddyAction::DraftSkill { .. } => DraftKind::Skill,
                BuddyAction::DraftCommand { .. } => DraftKind::Command,
                BuddyAction::DraftDelegate { .. } => DraftKind::Delegate,
                _ => DraftKind::Mode,
            };
            let final_id = if draft_id.is_empty() {
                let content = match action {
                    BuddyAction::DraftSkill { .. } => format!(
                        "---\nname: {}\ndescription: Describe when to use this skill\n---\nAdd context here\n",
                        label
                    ),
                    BuddyAction::DraftCommand { .. } => {
                        "---\ndescription: Describe this command\n---\nAdd command instructions here\n"
                            .to_string()
                    }
                    BuddyAction::DraftDelegate { .. } => format!(
                        "schema_version: 1\nid: {}\ntitle: {}\nsubchat:\n  stateful: false\n",
                        label, label
                    ),
                    _ => format!(
                        "schema_version: 1\nid: {}\ntitle: {}\nprompt: Describe this mode\n",
                        label, label
                    ),
                };
                let draft =
                    synthesize_draft(&app.chat_ctx(), dk, label.to_string(), content).await?;
                draft.id.clone()
            } else {
                validate_existing_draft(&app.chat_ctx(), draft_id, dk, DraftTarget::Any).await?;
                draft_id.clone()
            };
            let kind = dk;
            Ok(ActionOutcome {
                result: serde_json::json!({
                    "kind": "draft",
                    "draft_kind": serde_json::to_value(kind).unwrap_or_default(),
                    "draft_id": final_id,
                    "label": label
                }),
                status: OpportunityStatus::Accepted,
            })
        }
        BuddyAction::DraftAgentsMdPatch { content } => {
            let draft_content = if content.is_empty() {
                "# AGENTS.md\n\nThis file provides guidance to AI agents when working with this repository.\n\n## Development Commands\n\n- **Build**: `make build`\n- **Test**: `make test`\n\n## Architecture\n\nDescribe the project architecture here.\n"
            } else {
                content.as_str()
            };
            let draft = synthesize_draft(
                &app.chat_ctx(),
                DraftKind::AgentsMd,
                "AGENTS.md".to_string(),
                draft_content.to_string(),
            )
            .await?;
            Ok(ActionOutcome {
                result: serde_json::json!({
                    "kind": "draft",
                    "draft_kind": "agents_md",
                    "draft_id": draft.id
                }),
                status: OpportunityStatus::Accepted,
            })
        }
        BuddyAction::DraftDefaultsChange {
            defaults_kind,
            patch,
        } => {
            let content = if patch != &serde_json::json!({}) {
                serde_json::to_string_pretty(patch).unwrap_or_default()
            } else {
                serde_json::to_string_pretty(&provider_defaults_patch(*defaults_kind))
                    .unwrap_or_default()
            };
            let draft = synthesize_draft(
                &app.chat_ctx(),
                DraftKind::DefaultsModel,
                "Default Models".to_string(),
                content,
            )
            .await?;
            Ok(ActionOutcome {
                result: serde_json::json!({
                    "kind": "draft",
                    "draft_kind": "defaults_model",
                    "defaults_kind": serde_json::to_value(defaults_kind).unwrap_or_default(),
                    "draft_id": draft.id
                }),
                status: OpportunityStatus::Accepted,
            })
        }
        BuddyAction::DraftCustomizationChange {
            customization_kind,
            id,
            patch,
        } => {
            let draft_kind = customization_kind_to_draft_kind(*customization_kind);
            let existing = read_existing_customization(&app, *customization_kind, id)
                .await
                .unwrap_or_else(|| default_customization_template(*customization_kind, id));
            let draft_content = merge_customization_content(*customization_kind, &existing, patch)
                .map_err(|e| ScratchError::new(StatusCode::UNPROCESSABLE_ENTITY, e))?;
            let title_kind = customization_url_kind(*customization_kind);
            let draft = synthesize_draft(
                &app.chat_ctx(),
                draft_kind,
                format!("{} {}", title_kind, id),
                draft_content,
            )
            .await?;
            Ok(ActionOutcome {
                result: serde_json::json!({
                    "kind": "draft",
                    "draft_kind": serde_json::to_value(draft_kind).unwrap_or_default(),
                    "draft_id": draft.id,
                    "label": format!("Edit {}", id)
                }),
                status: OpportunityStatus::Accepted,
            })
        }
        BuddyAction::CreatePulseReport { scope } => {
            let pulse = {
                let buddy_arc = app.buddy.buddy.clone();
                let lock = buddy_arc.lock().await;
                lock.as_ref()
                    .map(|svc| svc.pulse.clone())
                    .unwrap_or_default()
            };
            let content = render_pulse_to_markdown(&pulse, *scope);
            let draft = synthesize_draft(
                &app.chat_ctx(),
                DraftKind::PulseReport,
                format!("Pulse Report ({})", scope_label(*scope)),
                content,
            )
            .await?;
            Ok(ActionOutcome {
                result: serde_json::json!({
                    "kind": "draft",
                    "draft_kind": "pulse_report",
                    "draft_id": draft.id
                }),
                status: OpportunityStatus::Accepted,
            })
        }
        BuddyAction::OfferMarketplaceInstall {
            market_kind,
            item_id,
        } => {
            install_marketplace_action(app.clone(), *market_kind, item_id)
                .await
                .map_err(|e| {
                    ScratchError::new(
                        StatusCode::BAD_GATEWAY,
                        format!("marketplace_install_failed: {}", e),
                    )
                })?;
            Ok(ActionOutcome {
                result: serde_json::json!({
                    "kind": "marketplace_install",
                    "market_kind": serde_json::to_value(market_kind).unwrap_or_default(),
                    "item_id": item_id,
                    "success": true,
                    "error": null
                }),
                status: OpportunityStatus::Accepted,
            })
        }
        BuddyAction::ApplyMemoryBatch { batch_key, .. } => {
            let (autonomy, project_root) = {
                let buddy_arc = app.buddy.buddy.clone();
                let lock = buddy_arc.lock().await;
                let svc = lock.as_ref().ok_or_else(|| {
                    ScratchError::new(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "buddy not initialized".into(),
                    )
                })?;
                (svc.settings.autonomy_level, svc.project_root.clone())
            };
            if matches!(action_execution(autonomy, action), ActionExecution::Deny) {
                return Err(ScratchError::new(
                    StatusCode::FORBIDDEN,
                    "action denied by autonomy level".to_string(),
                ));
            }
            let (memory_ops, applied, failed, remaining) =
                refact_runtime_api::buddy_memory_ops::apply_memory_batch(
                    &project_root,
                    app.clone(),
                    batch_key,
                    500,
                )
                .await
                .map_err(|e| ScratchError::new(StatusCode::INTERNAL_SERVER_ERROR, e))?;
            {
                let buddy_arc = app.buddy.buddy.clone();
                let mut lock = buddy_arc.lock().await;
                if let Some(svc) = lock.as_mut() {
                    svc.set_memory_ops(memory_ops);
                }
            }
            Ok(ActionOutcome {
                result: serde_json::json!({
                    "kind": "memory_batch_applied",
                    "batch_key": batch_key,
                    "applied": applied,
                    "failed": failed,
                    "remaining": remaining
                }),
                status: OpportunityStatus::Completed,
            })
        }
        BuddyAction::ApplyConfigPatch {
            draft_id,
            target_path,
        } => {
            let (autonomy, project_root, draft) = {
                let buddy_arc = app.buddy.buddy.clone();
                let lock = buddy_arc.lock().await;
                let svc = lock.as_ref().ok_or_else(|| {
                    ScratchError::new(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "buddy not initialized".into(),
                    )
                })?;
                (
                    svc.settings.autonomy_level,
                    svc.project_root.clone(),
                    svc.draft_store.get(draft_id).cloned(),
                )
            };
            let draft = draft.ok_or_else(|| {
                ScratchError::new(StatusCode::NOT_FOUND, "draft_not_found".to_string())
            })?;
            match action_execution(autonomy, action) {
                ActionExecution::Execute => {
                    let receipt = refact_buddy_core::receipts::apply_config_patch(
                        &project_root,
                        target_path,
                        &draft.yaml_or_json,
                    )
                    .await
                    .map_err(|e| ScratchError::new(StatusCode::FORBIDDEN, e))?;
                    Ok(ActionOutcome {
                        result: serde_json::json!({
                            "kind": "config_patch_applied",
                            "receipt_id": receipt.id,
                            "target_path": receipt.target_path
                        }),
                        status: OpportunityStatus::Completed,
                    })
                }
                ActionExecution::Prefill => Ok(ActionOutcome {
                    result: serde_json::json!({
                        "kind": "draft",
                        "draft_kind": serde_json::to_value(draft.kind).unwrap_or_default(),
                        "draft_id": draft.id,
                        "target_path": target_path
                    }),
                    status: OpportunityStatus::Accepted,
                }),
                _ => Err(ScratchError::new(
                    StatusCode::FORBIDDEN,
                    "action denied by autonomy level".to_string(),
                )),
            }
        }
        BuddyAction::AcceptQuest { suggestion_id } => {
            let (quest, persona, identity_name, pulse) = {
                let buddy_arc = app.buddy.buddy.clone();
                let mut lock = buddy_arc.lock().await;
                let svc = lock.as_mut().ok_or_else(|| {
                    ScratchError::new(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "buddy not initialized".into(),
                    )
                })?;
                let (_, quest) = svc
                    .accept_quest_from_suggestion(suggestion_id)
                    .map_err(|e| {
                        let status = if e.contains("not a quest") {
                            StatusCode::BAD_REQUEST
                        } else {
                            StatusCode::NOT_FOUND
                        };
                        ScratchError::new(status, e)
                    })?;
                (
                    quest,
                    svc.state.personality.clone(),
                    svc.state.identity.name.clone(),
                    svc.pulse.clone(),
                )
            };
            let fallback_text = format!(
                "Quest accepted: {}. I’ll keep score from here.",
                quest.title
            );
            let mut speech = refact_runtime_api::buddy_speech_runtime::render_buddy_speech(
                app.clone(),
                persona,
                identity_name,
                pulse,
                Some(quest.quest_type.clone()),
                fallback_text.clone(),
                refact_buddy_core::voice_service::SpeechIntent::QuestAccept,
                fallback_text,
            )
            .await;
            speech.id = format!("quest-accept-{}", quest.id);
            speech.ttl_seconds = 12;
            speech.dedupe_key = Some(format!("quest_accept_{}", quest.quest_type));
            refact_runtime_api::buddy_actor_runtime::buddy_update_speech_user(app.clone(), speech).await;
            Ok(ActionOutcome {
                result: serde_json::json!({
                    "kind": "quest_accepted",
                    "quest_id": quest.id,
                    "reward_xp": quest.reward_xp
                }),
                status: OpportunityStatus::Completed,
            })
        }
        BuddyAction::OpenBuddyConversation { chat_id } => Ok(ActionOutcome {
            result: serde_json::json!({
                "kind": "open_chat",
                "chat_id": chat_id
            }),
            status: OpportunityStatus::Accepted,
        }),
        BuddyAction::Dismiss => Ok(ActionOutcome {
            result: serde_json::json!({ "kind": "dismiss" }),
            status: OpportunityStatus::Dismissed,
        }),
    }
}

async fn read_effective_ext_file<F>(app: &AppState, relative_path: F) -> Option<String>
where
    F: Fn(&StdPath) -> PathBuf,
{
    let ext_dirs = get_ext_dirs(app.clone()).await;
    let mut found = None;
    for dir in ext_dirs.all_dirs_in_order() {
        if let Ok(content) = tokio::fs::read_to_string(relative_path(dir)).await {
            found = Some(content);
        }
    }
    found
}

async fn read_effective_config_file(
    app: &AppState,
    dir_name: &str,
    file_name: &str,
) -> Option<String> {
    let config_dir = app.paths.config_dir.clone();
    let mut found = tokio::fs::read_to_string(config_dir.join(dir_name).join(file_name))
        .await
        .ok();
    for project_root in get_unscoped_project_dirs_for_ctx(&app.chat_ctx()).await {
        if let Ok(content) =
            tokio::fs::read_to_string(project_root.join(".refact").join(dir_name).join(file_name))
                .await
        {
            found = Some(content);
        }
    }
    found
}

async fn read_existing_customization(
    app: &AppState,
    kind: CustomizationKind,
    id: &str,
) -> Option<String> {
    match kind {
        CustomizationKind::Skill => {
            read_effective_ext_file(app, |dir| dir.join("skills").join(id).join("SKILL.md")).await
        }
        CustomizationKind::Command => {
            read_effective_ext_file(app, |dir| dir.join("commands").join(format!("{}.md", id)))
                .await
        }
        CustomizationKind::Delegate => {
            read_effective_config_file(app, "subagents", &format!("{}.yaml", id)).await
        }
        CustomizationKind::Mode => {
            read_effective_config_file(app, "modes", &format!("{}.yaml", id)).await
        }
        CustomizationKind::Hook => read_effective_config_file(app, "", "hooks.yaml").await,
    }
}

async fn install_marketplace_action(
    app: AppState,
    market_kind: MarketKind,
    item_id: &str,
) -> Result<Value, String> {
    match market_kind {
        MarketKind::Mcp => {
            // Buddy installs are agent-initiated with no env-var form; allow the
            // config to be written with empty required env vars so the user can
            // fill credentials in the integration form afterwards.
            let body = serde_json::to_vec(
                &serde_json::json!({ "server_id": item_id, "allow_incomplete": true }),
            )
            .map_err(|e| e.to_string())?;
            crate::routers::v1::mcp_marketplace::install_mcp_marketplace_server(
                app.gcx.clone(),
                hyper::body::Bytes::from(body),
            )
            .await
            .map(|json| json.0)
            .map_err(|e| e.to_string())
        }
        MarketKind::Skill | MarketKind::Command | MarketKind::Delegate => {
            let kind = match market_kind {
                MarketKind::Skill => MarketplaceKind::Skill,
                MarketKind::Command => MarketplaceKind::Command,
                MarketKind::Delegate => MarketplaceKind::Subagent,
                MarketKind::Mcp => unreachable!(),
            };
            let (items, _) = list_marketplace_items(app.clone(), kind).await?;
            let item = items
                .into_iter()
                .find(|item| item.id == item_id)
                .ok_or_else(|| format!("marketplace item '{}' not found", item_id))?;
            let req = InstallMarketplaceItemRequest {
                source_id: item.source_id,
                item_id: item.id,
                scope: "global".to_string(),
                overwrite: false,
                params: HashMap::new(),
            };
            install_marketplace_item(app, kind, req)
                .await
                .and_then(|response| serde_json::to_value(response).map_err(|e| e.to_string()))
        }
    }
}

async fn synthesize_draft(
    ctx: &ChatCtx,
    kind: DraftKind,
    title: String,
    content: String,
) -> Result<BuddyDraft, ScratchError> {
    let buddy_arc = ctx.buddy.buddy.clone();
    let mut lock = buddy_arc.lock().await;
    let svc = lock.as_mut().ok_or_else(|| {
        ScratchError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "buddy not initialized".into(),
        )
    })?;
    svc.create_draft(kind, title, content, String::new())
        .map_err(draft_create_error)
}

pub async fn enrich_investigation_context(app: &AppState, ctx: &mut InvestigationContext) {
    if !ctx.diagnostic_ids.is_empty() {
        let buddy_arc = app.buddy.buddy.clone();
        let lock = buddy_arc.lock().await;
        if let Some(svc) = lock.as_ref() {
            let diagnostics: Vec<_> = ctx
                .diagnostic_ids
                .iter()
                .filter_map(|id| svc.diagnostic_by_id(id))
                .collect();
            let diagnostic_lines: Vec<String> = diagnostics
                .iter()
                .map(|d| {
                    format!(
                        "- [{}] {}: {}",
                        diagnostic_severity_label(&d.severity),
                        d.error_type,
                        d.error_message
                    )
                })
                .collect();
            ctx.log_excerpt = diagnostic_lines.join("\n");
            if ctx.memory_key.is_none() {
                ctx.memory_key = ctx
                    .fact_keys
                    .iter()
                    .find(|key| key.starts_with("diag:cluster:"))
                    .cloned()
                    .or_else(|| {
                        ctx.fact_keys
                            .iter()
                            .filter_map(|key| {
                                svc.fact_store.iter().find(|fact| fact.key == key.as_str())
                            })
                            .find_map(|fact| {
                                if fact.kind != BuddyFactKind::DiagnosticCluster {
                                    return None;
                                }
                                let error_type =
                                    fact.payload.get("error_type").and_then(|v| v.as_str())?;
                                let source_bucket =
                                    fact.payload.get("source_bucket").and_then(|v| v.as_str())?;
                                Some(format!("diag:cluster:{}:{}", error_type, source_bucket))
                            })
                    })
                    .or_else(|| stable_diagnostic_memory_key(&diagnostics));
            }
        }
    }

    if ctx.memory_key.is_none() && !ctx.fact_keys.is_empty() {
        ctx.memory_key = ctx
            .fact_keys
            .iter()
            .find(|key| key.starts_with("diag:cluster:"))
            .cloned();
    }

    if ctx.memory_key.is_none() && !ctx.diagnostic_ids.is_empty() {
        let buddy_arc = app.buddy.buddy.clone();
        let lock = buddy_arc.lock().await;
        if let Some(svc) = lock.as_ref() {
            let diagnostics: Vec<_> = ctx
                .diagnostic_ids
                .iter()
                .filter_map(|id| svc.diagnostic_by_id(id))
                .collect();
            ctx.memory_key = stable_diagnostic_memory_key(&diagnostics);
        }
    }

    if let Ok(log_tail) = read_recent_log_lines(app, 50).await {
        if !log_tail.is_empty() {
            ctx.log_excerpt = if ctx.log_excerpt.is_empty() {
                log_tail
            } else {
                format!(
                    "{}\n\n--- Recent log lines ---\n{}",
                    ctx.log_excerpt, log_tail
                )
            };
        }
    }

    if let Some(config_summary) = render_caps_config_summary(app).await {
        ctx.config_summary = config_summary;
    }

    ctx.log_excerpt = cap_text_to_chars(
        &refact_buddy_core::actor::redact_sensitive(&ctx.log_excerpt),
        4000,
    );
    ctx.config_summary = cap_text_to_chars(
        &refact_buddy_core::actor::redact_sensitive(&ctx.config_summary),
        1000,
    );
}

async fn render_caps_config_summary(app: &AppState) -> Option<String> {
    let caps = app.model.caps.read().await.caps.clone()?;
    Some(format!(
        "default chat model: {}\ndefault buddy model: {}\ndefault thinking model: {}",
        caps.defaults.chat_default_model,
        caps.defaults.chat_buddy_model,
        caps.defaults.chat_thinking_model,
    ))
}

pub async fn read_recent_log_lines(
    app: &AppState,
    max_lines: usize,
) -> Result<String, String> {
    let logs_to_file = app.runtime.cmdline.logs_to_file.clone();
    let cache_dir = app.paths.cache_dir.clone();
    let log_path = if !logs_to_file.is_empty() {
        PathBuf::from(logs_to_file)
    } else {
        // Route through the shared resolver so daemon worker-*.log and classic rustbinary.* are
        // both found; the joined filename is only an anchor whose parent is the resolved dir.
        refact_tools::tools::tool_buddy_get_logs::resolve_log_dir(&cache_dir).join("refact.log")
    };
    let log_tail = read_log_content_with_notice(&log_path).await?;
    let tail: Vec<String> = log_tail
        .text
        .lines()
        .rev()
        .take(max_lines)
        .map(refact_buddy_core::actor::redact_sensitive)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    let joined = tail.join("\n");
    Ok(match log_tail.notice {
        Some(notice) => format!("{}\n{}", notice, joined),
        None => joined,
    })
}

pub fn is_log_candidate(path: &std::path::Path) -> bool {
    // Delegate to the shared candidate check so classic rustbinary.* and daemon worker-*.log
    // are recognized here too (this local picker previously missed rustbinary.* files).
    path.file_name()
        .and_then(|name| name.to_str())
        .map(refact_tools::tools::tool_buddy_get_logs::is_log_candidate)
        .unwrap_or(false)
}

pub async fn read_log_content_with_notice(
    log_path: &std::path::Path,
) -> Result<refact_tools::tools::tool_buddy_get_logs::BoundedLogTail, String> {
    if log_path.is_file() {
        return refact_tools::tools::tool_buddy_get_logs::read_bounded_log_tail_with_notice(log_path)
            .await;
    }
    let log_dir = log_path.parent().unwrap_or(log_path);
    let mut entries = tokio::fs::read_dir(log_dir)
        .await
        .map_err(|e| format!("failed to read logs dir {:?}: {}", log_dir, e))?;
    let mut files: Vec<(PathBuf, std::time::SystemTime)> = Vec::new();
    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        if !is_log_candidate(&path) {
            continue;
        }
        if let Ok(meta) = tokio::fs::metadata(&path).await {
            if meta.is_file() {
                if let Ok(modified) = meta.modified() {
                    files.push((path, modified));
                }
            }
        }
    }
    files.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let Some((newest, _)) = files.first() else {
        return Ok(refact_tools::tools::tool_buddy_get_logs::BoundedLogTail {
            text: String::new(),
            notice: None,
        });
    };
    refact_tools::tools::tool_buddy_get_logs::read_bounded_log_tail_with_notice(newest).await
}

pub async fn read_log_content(log_path: &std::path::Path) -> Result<String, String> {
    Ok(read_log_content_with_notice(log_path).await?.text)
}

async fn create_investigation_chat(
    app: &AppState,
    ctx: &InvestigationContext,
) -> Result<String, ScratchError> {
    let chat_id = Uuid::new_v4().to_string();
    let now = chrono::Utc::now().to_rfc3339();

    let model = refact_runtime_api::buddy_actor_runtime::resolve_buddy_chat_model(app.clone()).await;
    let snapshot = TrajectorySnapshot {
        pending_deliveries: Vec::new(),
        goal: None,
        goal_ledger: Vec::new(),
        goal_verification_blocked_until_ms: None,
        chat_id: chat_id.clone(),
        title: "Investigation".to_string(),
        model,
        mode: "buddy".to_string(),
        tool_use: "agent".to_string(),
        messages: vec![
            refact_core::call_validation::ChatMessage {
                role: "system".to_string(),
                content: refact_core::call_validation::ChatContent::SimpleText(
                    INVESTIGATION_SYSTEM_PROMPT.to_string(),
                ),
                ..Default::default()
            },
            refact_core::call_validation::ChatMessage {
                role: "user".to_string(),
                content: refact_core::call_validation::ChatContent::SimpleText(
                    build_investigation_data_envelope(ctx),
                ),
                ..Default::default()
            },
            refact_core::call_validation::ChatMessage {
                role: "user".to_string(),
                content: refact_core::call_validation::ChatContent::SimpleText(
                    ctx.initial_user_message.clone(),
                ),
                ..Default::default()
            },
        ],
        created_at: now,
        boost_reasoning: false,
        checkpoints_enabled: false,
        context_tokens_cap: None,
        auto_compression_cap: None,
        auto_compression_cap_pending: true,
        include_project_info: true,
        is_title_generated: false,
        auto_approve_editing_tools: false,
        auto_approve_dangerous_commands: false,
        autonomous_no_confirm: false,
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
        auto_enrichment_enabled: Some(true),
        buddy_meta: Some(refact_buddy_core::types::BuddyThreadMeta {
            is_buddy_chat: true,
            buddy_chat_kind: "investigation".to_string(),
            workflow_id: None,
        }),
        auto_compact_enabled: None,
        frozen_request_prefix: None,
        claude_code_identity: None,
        reactive_compact_attempts: None,
        wake_up_at: None,
        waiting_for_card_ids: Vec::new(),
    };

    refact_runtime_api::trajectory_runtime::save_trajectory_snapshot(app.gcx.clone(), snapshot)
        .await
        .map_err(|e| ScratchError::new(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    Ok(chat_id)
}

pub async fn handle_v1_buddy_opportunity_dismiss(
    State(app): State<AppState>,
    Path(id): Path<String>,
    body: Option<axum::extract::Json<DismissRequest>>,
) -> Result<axum::Json<serde_json::Value>, ScratchError> {
    let req = body.map(|b| b.0).unwrap_or_default();
    let buddy_arc = app.buddy.buddy.clone();
    let mut lock = buddy_arc.lock().await;
    let svc = lock.as_mut().ok_or_else(|| {
        ScratchError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "buddy not initialized".into(),
        )
    })?;
    let opp = svc.opportunity_queue.get(&id).cloned().ok_or_else(|| {
        ScratchError::new(
            StatusCode::NOT_FOUND,
            format!("opportunity not found: {}", id),
        )
    })?;
    if is_terminal_status(opp.status) {
        return Err(ScratchError::new(
            StatusCode::CONFLICT,
            format!("opportunity already resolved: {:?}", opp.status),
        ));
    }
    if svc.is_opportunity_accept_claimed(&id) {
        return Err(ScratchError::new(
            StatusCode::CONFLICT,
            format!("opportunity already in progress: {}", id),
        ));
    }
    svc.resolve_opportunity(&id, OpportunityStatus::Dismissed);
    let verdict = if req.never {
        svc.mute_rule_for_cooldown_key(&opp.cooldown_key);
        VerdictOutcome::Never
    } else {
        VerdictOutcome::Dismiss
    };
    svc.record_verdict(&opp.cooldown_key, opp.kind, "dismiss", verdict);
    let snap = serde_json::to_value(svc.snapshot())
        .map_err(|e| ScratchError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(axum::Json(serde_json::json!({ "snapshot": snap })))
}
