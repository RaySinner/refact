use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use async_trait::async_trait;
use chrono::Utc;
use serde_json::{json, Value};
use tokio::sync::Mutex as AMutex;
use uuid::Uuid;

use refact_runtime_api::AppState;
use refact_runtime_api::AtCommandsContext;
use refact_core::chat_types::{ChatMessage, ChatContent, ContextEnum, ContextFile};
use refact_runtime_api::ChatCtx;
use refact_chat_api::internal_roles::{event, EventSubkind};
use refact_runtime_api::global_context::GlobalContext;
use refact_runtime_api::task_storage as storage;
use refact_tasks::types::{BoardCard, StatusUpdate};
use crate::tools::task_tool_helpers::{wait_for_agent_abort, AGENT_ABORT_TIMEOUT};
use crate::tools::tool_task_spawn_agent::{
    build_agent_prompt, build_agent_thread_params, mark_card_agent_started, prepare_agent_worktree,
    resolve_agent_model, restore_original_card_if_current_agent,
};
use crate::tools::tools_description::{Tool, ToolDesc, ToolSource, ToolSourceType};
use refact_worktrees::service::WorktreeService;
use refact_worktrees::types::WorktreeMeta;
use refact_chat_api::ChatCommand;
use refact_runtime_api::CreateSessionRequest;

pub fn context_file_message(
    gcx: &Arc<GlobalContext>,
    context_files: Vec<ContextFile>,
) -> Result<ChatMessage, String> {
    let records = refact_runtime_api::gcx_helpers::declared_file_records(
        gcx,
        context_files
            .iter()
            .map(|file| PathBuf::from(&file.file_name)),
    )?;
    let mut message = ChatMessage {
        role: "context_file".to_string(),
        content: ChatContent::ContextFiles(context_files),
        tool_call_id: "initial_files".to_string(),
        ..Default::default()
    };
    refact_privacy::merge_records(&mut message, records);
    Ok(message)
}

async fn cleanup_old_worktree(
    ctx: &ChatCtx,
    agent_worktree_name: Option<&str>,
    agent_worktree: Option<&str>,
    agent_branch: Option<&str>,
) {
    if let Some(wt_name) = agent_worktree_name {
        let cache_dir = ctx.paths.cache_dir.clone();
        let project_dirs = refact_runtime_api::files_correction::get_unscoped_project_dirs_for_ctx(ctx).await;
        if let Some(source_root) = project_dirs.first() {
            if let Ok(service) = WorktreeService::new_async(cache_dir, source_root.clone()).await {
                if service.delete_worktree(wt_name, true, true).await.is_ok() {
                    return;
                }
            }
        }
    }

    let project_dirs = refact_runtime_api::files_correction::get_unscoped_project_dirs_for_ctx(ctx).await;
    if let Some(workspace_root) = project_dirs.first() {
        if let Some(wt) = agent_worktree {
            let _ = tokio::process::Command::new("git")
                .args(["worktree", "remove", wt, "--force"])
                .current_dir(workspace_root)
                .output()
                .await;
        }
        if let Some(branch) = agent_branch {
            let _ = tokio::process::Command::new("git")
                .args(["branch", "-D", branch])
                .current_dir(workspace_root)
                .output()
                .await;
        }
    }
}

async fn restore_card_after_restart_failure(
    ctx: &ChatCtx,
    task_id: &str,
    original_card: BoardCard,
    guard_chat_id: String,
) {
    let _ = storage::update_board_atomic_for_ctx(ctx, task_id, move |board| {
        restore_original_card_if_current_agent(board, &original_card, &guard_chat_id);
        Ok(())
    })
    .await;
    let _ = storage::update_task_stats_for_ctx(ctx, task_id).await;
}

pub fn mark_card_restarted_fresh(
    card: &mut BoardCard,
    new_agent_id: &str,
    new_agent_chat_id: &str,
    worktree_branch: Option<String>,
    worktree_path: Option<String>,
    worktree_name: Option<String>,
) {
    if let Some(prev_report) = card.final_report.take() {
        let preview: String = prev_report.chars().take(300).collect();
        card.status_updates.push(StatusUpdate {
            timestamp: Utc::now().to_rfc3339(),
            message: format!("Previous failure: {}", preview),
        });
    }
    card.final_report_structured = None;
    card.column = "doing".to_string();
    card.completed_at = None;
    mark_card_agent_started(
        card,
        new_agent_id,
        new_agent_chat_id,
        worktree_branch,
        worktree_path,
        worktree_name,
    );
    card.status_updates.last_mut().map(|u| {
        u.message = "Fresh restart: new agent started".to_string();
    });
}

pub fn mark_card_restarted_resume(card: &mut BoardCard, new_agent_id: &str, new_agent_chat_id: &str) {
    if let Some(prev_report) = card.final_report.take() {
        let preview: String = prev_report.chars().take(300).collect();
        card.status_updates.push(StatusUpdate {
            timestamp: Utc::now().to_rfc3339(),
            message: format!("Previous failure: {}", preview),
        });
    }
    card.final_report_structured = None;
    card.column = "doing".to_string();
    card.completed_at = None;
    card.assignee = Some(new_agent_id.to_string());
    card.agent_chat_id = Some(new_agent_chat_id.to_string());
    card.started_at = Some(Utc::now().to_rfc3339());
    card.status_updates.push(StatusUpdate {
        timestamp: Utc::now().to_rfc3339(),
        message: "Resume restart: new agent attached to existing worktree".to_string(),
    });
}

pub fn ensure_restart_source_current(card: &BoardCard, original: &BoardCard) -> Result<(), String> {
    if card.column == original.column
        && card.assignee == original.assignee
        && card.agent_chat_id == original.agent_chat_id
        && card.agent_branch == original.agent_branch
        && card.agent_worktree == original.agent_worktree
        && card.agent_worktree_name == original.agent_worktree_name
    {
        return Ok(());
    }
    Err(format!(
        "Card {} changed while restart was preparing; expected column={}, assignee={}, agent_chat_id={}, found column={}, assignee={}, agent_chat_id={}. Run check_agents and retry.",
        original.id,
        original.column,
        original.assignee.as_deref().unwrap_or("none"),
        original.agent_chat_id.as_deref().unwrap_or("none"),
        card.column,
        card.assignee.as_deref().unwrap_or("none"),
        card.agent_chat_id.as_deref().unwrap_or("none")
    ))
}

async fn get_worktree_meta_for_resume(
    ctx: &ChatCtx,
    card: &BoardCard,
) -> Result<WorktreeMeta, String> {
    let wt_name = card.agent_worktree_name.as_deref().ok_or_else(|| {
        format!(
            "Card {} has no retained worktree (agent_worktree_name is missing). Use mode=fresh to create a new worktree.",
            card.id
        )
    })?;
    let wt_path = card.agent_worktree.as_deref().unwrap_or("");
    if !wt_path.is_empty() && !Path::new(wt_path).exists() {
        return Err(format!(
            "Retained worktree path '{}' for card {} no longer exists. Use mode=fresh to create a new worktree.",
            wt_path, card.id
        ));
    }
    let cache_dir = ctx.paths.cache_dir.clone();
    let project_dirs = refact_runtime_api::files_correction::get_unscoped_project_dirs_for_ctx(ctx).await;
    let source_root = project_dirs
        .first()
        .cloned()
        .ok_or("No workspace folder found")?;
    let service = WorktreeService::new_async(cache_dir, source_root).await?;
    let view = service.get_worktree(wt_name).await.map_err(|e| {
        format!(
            "Retained worktree '{}' is no longer registered: {}. Use mode=fresh.",
            wt_name, e
        )
    })?;
    if !view.meta.root.exists() {
        return Err(format!(
            "Retained worktree root '{}' no longer exists on disk. Use mode=fresh.",
            view.meta.root.display()
        ));
    }
    Ok(view.meta)
}

pub struct ToolTaskRestartAgent;

impl ToolTaskRestartAgent {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for ToolTaskRestartAgent {
    fn tool_description(&self) -> ToolDesc {
        ToolDesc {
            name: "restart_agent".to_string(),
            display_name: "Task Restart Agent".to_string(),
            source: ToolSource {
                source_type: ToolSourceType::Builtin,
                config_path: String::new(),
            },
            experimental: false,
            allow_parallel: false,
            description: "Restart a failed task agent. Use mode=fresh (default) to clean up the old worktree and start fresh, or mode=resume to attach a new agent to the retained worktree and continue where the previous agent left off.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "card_id": {
                        "type": "string",
                        "description": "Card ID to restart"
                    },
                    "mode": {
                        "type": "string",
                        "enum": ["fresh", "resume"],
                        "description": "Restart mode: 'fresh' (default) deletes the old worktree and creates a new one; 'resume' reuses the retained worktree from the previous run."
                    },
                    "force": {
                        "type": "boolean",
                        "description": "If true, allow restarting even if an agent is currently running on this card (sends abort to old agent first)."
                    },
                    "suggested_steps": {
                        "type": "integer",
                        "description": "Suggested step budget for the agent (default: 30)."
                    },
                    "files_to_open": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "List of file paths to open immediately when the agent starts."
                    }
                },
                "required": ["card_id"]
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
        let ccx_lock = ccx.lock().await;
        let is_planner = ccx_lock
            .task_meta
            .as_ref()
            .map(|m| m.role == "planner")
            .unwrap_or(false);
        if !is_planner {
            return Err("restart_agent can only be called by the task planner.".to_string());
        }
        let task_id = ccx_lock
            .task_meta
            .as_ref()
            .map(|m| m.task_id.clone())
            .unwrap_or_default();
        let planner_chat_id = ccx_lock
            .task_meta
            .as_ref()
            .and_then(|m| m.planner_chat_id.clone())
            .unwrap_or_else(|| ccx_lock.chat_id.clone());
        let current_chat_id = ccx_lock.chat_id.clone();
        let current_model = ccx_lock.current_model.clone();
        let app = ccx_lock.app.clone();
        drop(ccx_lock);
        let ctx = app.chat_ctx();
        let tool = app.tool.clone();

        let card_id = args
            .get("card_id")
            .and_then(|v| v.as_str())
            .ok_or("Missing 'card_id'")?;
        let mode = args.get("mode").and_then(|v| v.as_str()).unwrap_or("fresh");
        if mode != "fresh" && mode != "resume" {
            return Err(format!(
                "Invalid mode '{}', must be 'fresh' or 'resume'",
                mode
            ));
        }
        let force = args
            .get("force")
            .and_then(refact_tool_api::coerce_bool)
            .unwrap_or(false);
        let suggested_steps: usize = match args
            .get("suggested_steps")
            .or_else(|| args.get("max_steps"))
        {
            Some(Value::String(s)) => s.parse().unwrap_or(30),
            Some(Value::Number(n)) => n.as_u64().unwrap_or(30) as usize,
            _ => 30,
        };
        let suggested_steps = suggested_steps.min(50).max(1);
        let files_to_open: Vec<String> = args
            .get("files_to_open")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();

        let task_meta = storage::load_task_meta_for_ctx(&ctx, &task_id).await?;
        let model = resolve_agent_model(&tool.caps, &current_model).await?;
        crate::tools::task_tool_helpers::preflight_agent_model(&app.chat_ctx(), &model).await?;

        let board = storage::load_board_for_ctx(&ctx, &task_id).await?;
        let card = board
            .get_card(card_id)
            .ok_or_else(|| format!("Card {} not found", card_id))?;

        match card.column.as_str() {
            "done" => {
                return Err(format!(
                    "Card {} is already done and cannot be restarted.",
                    card_id
                ))
            }
            "planned" => {
                return Err(format!(
                "Card {} is in 'planned' column. Use spawn_agent to start it for the first time.",
                card_id
            ))
            }
            "failed" => {}
            "doing" => {
                if card.agent_chat_id.is_some() && !force {
                    return Err(format!(
                        "Card {} has an active agent ({}). Use force=true to abort it and restart, or wait for it to finish.",
                        card_id,
                        card.agent_chat_id.as_ref().unwrap()
                    ));
                }
            }
            other => {
                return Err(format!(
                    "Card {} is in unexpected column '{}'",
                    card_id, other
                ))
            }
        }

        let card_title = card.title.clone();
        let card_instructions = card.instructions.clone();
        let dependency_context = board
            .get_dependency_reports(card_id)
            .into_iter()
            .map(|(title, report)| format!("### {}\n{}", title, report))
            .collect::<Vec<_>>()
            .join("\n\n");

        let result_message = if mode == "resume" {
            self.execute_resume(
                &app,
                &task_id,
                &planner_chat_id,
                &current_chat_id,
                card_id,
                &card_title,
                &card_instructions,
                &dependency_context,
                &model,
                suggested_steps,
                files_to_open,
                card,
            )
            .await?
        } else {
            self.execute_fresh(
                &app,
                &task_id,
                &planner_chat_id,
                &task_meta,
                card_id,
                &card_title,
                &card_instructions,
                &dependency_context,
                &model,
                suggested_steps,
                files_to_open,
                card,
            )
            .await?
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
    }

    fn tool_depends_on(&self) -> Vec<String> {
        vec![]
    }
}

impl ToolTaskRestartAgent {
    async fn execute_fresh(
        &self,
        app: &AppState,
        task_id: &str,
        planner_chat_id: &str,
        task_meta: &refact_tasks::types::TaskMeta,
        card_id: &str,
        card_title: &str,
        card_instructions: &str,
        dependency_context: &str,
        model: &str,
        suggested_steps: usize,
        files_to_open: Vec<String>,
        card: &BoardCard,
    ) -> Result<String, String> {
        let ctx = app.chat_ctx();
        let original_card = card.clone();

        if let Some(old_chat_id) = original_card.agent_chat_id.as_deref() {
            if let Err(e) =
                wait_for_agent_abort(&app.chat_ctx(), old_chat_id, AGENT_ABORT_TIMEOUT).await
            {
                return Err(format!(
                    "Cannot restart card {}: previous agent session '{}' did not stop in time: {}. \
                     Wait for it to settle and retry, or use cancel_agent first.",
                    card_id, old_chat_id, e
                ));
            }
        }

        let agent_id = Uuid::new_v4().to_string();
        let agent_chat_id = format!("agent-{}-{}", card_id, &agent_id[..8]);

        let mut restart_task_meta = task_meta.clone();
        if original_card.base_branch.is_some() || original_card.base_commit.is_some() {
            restart_task_meta.base_branch = original_card.base_branch.clone();
            restart_task_meta.base_commit = original_card.base_commit.clone();
        }
        let prepared_worktree = prepare_agent_worktree(
            &ctx,
            &restart_task_meta,
            task_id,
            &agent_id,
            card_id,
            &agent_chat_id,
        )
        .await?;

        let worktree_branch = prepared_worktree.branch_name();
        let worktree_path_str = Some(
            prepared_worktree
                .worktree_path()
                .to_string_lossy()
                .to_string(),
        );
        let worktree_name = Some(prepared_worktree.worktree_name());
        let worktree_meta = prepared_worktree.worktree_meta();
        let prepared_base_branch = worktree_meta.base_branch.clone();
        let prepared_base_commit = worktree_meta.base_commit.clone();
        let card_id_owned = card_id.to_string();
        let agent_id_clone = agent_id.clone();
        let agent_chat_id_clone = agent_chat_id.clone();
        let source_card = original_card.clone();

        let board_update_result =
            storage::update_board_atomic_for_ctx(&ctx, task_id, move |board| {
                let card = board
                    .get_card_mut(&card_id_owned)
                    .ok_or(format!("Card {} not found", card_id_owned))?;
                ensure_restart_source_current(card, &source_card)?;
                mark_card_restarted_fresh(
                    card,
                    &agent_id_clone,
                    &agent_chat_id_clone,
                    worktree_branch.clone(),
                    worktree_path_str.clone(),
                    worktree_name.clone(),
                );
                card.base_branch = prepared_base_branch.clone();
                card.base_commit = prepared_base_commit.clone();
                Ok(())
            })
            .await;

        if let Err(e) = board_update_result {
            prepared_worktree
                .cleanup_unlinked(app.paths.cache_dir.clone())
                .await;
            return Err(e);
        }

        let _ = storage::update_task_stats_for_ctx(&ctx, task_id).await;

        if let Err(e) = self
            .create_and_dispatch_session(
                app,
                task_id,
                planner_chat_id,
                card_id,
                card_title,
                card_instructions,
                dependency_context,
                model,
                &agent_id,
                &agent_chat_id,
                worktree_meta.clone(),
                suggested_steps,
                files_to_open,
            )
            .await
        {
            prepared_worktree
                .cleanup_unlinked(app.paths.cache_dir.clone())
                .await;
            restore_card_after_restart_failure(
                &ctx,
                task_id,
                original_card,
                agent_chat_id.clone(),
            )
            .await;
            return Err(e);
        }

        cleanup_old_worktree(
            &ctx,
            original_card.agent_worktree_name.as_deref(),
            original_card.agent_worktree.as_deref(),
            original_card.agent_branch.as_deref(),
        )
        .await;

        Ok(format!(
            "# Agent Restarted (Fresh): {}\n\n**Card:** {}\n**Agent ID:** {}\n**Model:** {}\n**Mode:** fresh\n**Status:** Running in background\n\nPrevious worktree cleaned up. New agent started from scratch.",
            card_title, card_id, agent_id, model
        ))
    }

    async fn execute_resume(
        &self,
        app: &AppState,
        task_id: &str,
        planner_chat_id: &str,
        resume_chat_id: &str,
        card_id: &str,
        card_title: &str,
        card_instructions: &str,
        dependency_context: &str,
        model: &str,
        suggested_steps: usize,
        files_to_open: Vec<String>,
        card: &BoardCard,
    ) -> Result<String, String> {
        let ctx = app.chat_ctx();
        let original_card = card.clone();

        if let Some(old_chat_id) = original_card.agent_chat_id.as_deref() {
            if let Err(e) =
                wait_for_agent_abort(&app.chat_ctx(), old_chat_id, AGENT_ABORT_TIMEOUT).await
            {
                return Err(format!(
                    "Cannot resume card {}: previous agent session '{}' did not stop in time: {}. \
                     Wait for it to settle and retry, or use cancel_agent first.",
                    card_id, old_chat_id, e
                ));
            }
        }

        let worktree_meta = get_worktree_meta_for_resume(&ctx, card).await?;

        let agent_id = Uuid::new_v4().to_string();
        let agent_chat_id = format!("agent-{}-{}", card_id, &agent_id[..8]);
        let card_id_owned = card_id.to_string();
        let agent_id_clone = agent_id.clone();
        let agent_chat_id_clone = agent_chat_id.clone();
        let _ = resume_chat_id;
        let source_card = original_card.clone();

        storage::update_board_atomic_for_ctx(&ctx, task_id, move |board| {
            let card = board
                .get_card_mut(&card_id_owned)
                .ok_or(format!("Card {} not found", card_id_owned))?;
            ensure_restart_source_current(card, &source_card)?;
            mark_card_restarted_resume(card, &agent_id_clone, &agent_chat_id_clone);
            Ok(())
        })
        .await?;

        let _ = storage::update_task_stats_for_ctx(&ctx, task_id).await;

        if let Err(e) = self
            .create_and_dispatch_session(
                app,
                task_id,
                planner_chat_id,
                card_id,
                card_title,
                card_instructions,
                dependency_context,
                model,
                &agent_id,
                &agent_chat_id,
                worktree_meta,
                suggested_steps,
                files_to_open,
            )
            .await
        {
            restore_card_after_restart_failure(
                &ctx,
                task_id,
                original_card,
                agent_chat_id.clone(),
            )
            .await;
            return Err(e);
        }

        Ok(format!(
            "# Agent Restarted (Resume): {}\n\n**Card:** {}\n**Agent ID:** {}\n**Model:** {}\n**Mode:** resume\n**Status:** Running in background\n\nAgent attached to retained worktree and continuing.",
            card_title, card_id, agent_id, model
        ))
    }

    #[allow(clippy::too_many_arguments)]
    async fn create_and_dispatch_session(
        &self,
        app: &AppState,
        task_id: &str,
        planner_chat_id: &str,
        card_id: &str,
        card_title: &str,
        card_instructions: &str,
        dependency_context: &str,
        model: &str,
        agent_id: &str,
        agent_chat_id: &str,
        worktree_meta: WorktreeMeta,
        suggested_steps: usize,
        files_to_open: Vec<String>,
    ) -> Result<(), String> {
        let thread = build_agent_thread_params(
            agent_chat_id,
            card_title,
            model,
            task_id,
            agent_id,
            card_id,
            planner_chat_id,
            worktree_meta.clone(),
        );

        let user_prompt = build_agent_prompt(
            card_title,
            card_instructions,
            dependency_context,
            suggested_steps,
        );

        let user_msg = event(
            EventSubkind::SystemNotice,
            "tool.task_restart_agent",
            json!({
                "task_id": task_id,
                "planner_chat_id": planner_chat_id,
                "card_id": card_id,
                "card_title": card_title,
                "agent_id": agent_id,
                "agent_chat_id": agent_chat_id,
                "model": model,
                "suggested_steps": suggested_steps,
                "files_to_open": &files_to_open,
                "worktree": worktree_meta.root.to_string_lossy().to_string(),
                "worktree_name": &worktree_meta.id,
                "branch": worktree_meta.branch.as_deref(),
            }),
            user_prompt,
        );
        let mut messages = vec![user_msg];

        if !files_to_open.is_empty() {
            let mut context_files: Vec<ContextFile> = Vec::new();
            let wt_root = &worktree_meta.root;
            let worktree_canonical =
                dunce::canonicalize(wt_root).unwrap_or_else(|_| wt_root.clone());
            for path_str in &files_to_open {
                let orig = std::path::Path::new(path_str);
                let source_root = &worktree_meta.source_workspace_root;
                let resolved = match orig.strip_prefix(source_root) {
                    Ok(rel) => wt_root.join(rel),
                    Err(_) => wt_root.join(path_str.trim_start_matches('/')),
                };
                let canonical_resolved = match dunce::canonicalize(&resolved) {
                    Ok(p) => p,
                    Err(_) => {
                        tracing::warn!(
                            "restart_agent: files_to_open '{}' does not exist, skipping",
                            path_str
                        );
                        continue;
                    }
                };
                if !canonical_resolved.starts_with(&worktree_canonical) {
                    tracing::warn!(
                        "restart_agent: files_to_open '{}' escapes worktree, rejecting",
                        path_str
                    );
                    continue;
                }
                if refact_runtime_api::files_in_workspace::check_file_privacy_for_model_context(
                    app.gcx.clone(),
                    &canonical_resolved,
                )
                .await
                .is_err()
                {
                    tracing::warn!(
                        "restart_agent: files_to_open '{}' blocked by privacy settings, skipping",
                        path_str
                    );
                    continue;
                }
                match tokio::fs::read_to_string(&canonical_resolved).await {
                    Ok(content) => {
                        let line_count = content.lines().count().max(1);
                        context_files.push(ContextFile {
                            file_name: canonical_resolved.to_string_lossy().to_string(),
                            file_content: content,
                            line1: 1,
                            line2: line_count,
                            ..Default::default()
                        });
                    }
                    Err(e) => {
                        tracing::warn!(
                            "restart_agent: could not read file {:?}: {}",
                            canonical_resolved,
                            e
                        );
                    }
                }
            }
            if !context_files.is_empty() {
                messages.push(context_file_message(&app.gcx, context_files)?);
            }
        }

        app.chat
            .facade
            .create_session(CreateSessionRequest {
                chat_id: agent_chat_id.to_string(),
                thread,
                messages,
            })
            .await?;
        app.chat.facade.maybe_save_session(agent_chat_id).await?;
        app.chat
            .facade
            .push_priority_command(agent_chat_id, ChatCommand::Regenerate {})
            .await?;

        Ok(())
    }
}
