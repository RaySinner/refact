use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use axum::extract::Path;
use axum::extract::Query;
use axum::http::{Response, StatusCode};
use axum::response::Json;
use axum::extract::State;
use hyper::Body;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::broadcast;
use chrono::Utc;

use refact_runtime_api::AppState;
use refact_runtime_api::GlobalContextCtx;
use refact_runtime_api::chat_ctx::ChatCtx;
use refact_core::custom_error::ScratchError;
use refact_runtime_api::files_correction::get_unscoped_project_dirs_for_ctx;
use refact_worktrees::service::WorktreeService;
use refact_runtime_api::task_comments::create_card_comment_for_ctx;
use refact_tasks::comments::CreateCardComment;
use refact_tasks::types::{
    BoardCard, StatusUpdate, TaskBoard, TaskMeta, TaskStatus, TeamMember, TrajectoryInfo,
};
use refact_chat_history::trajectory_event::TrajectoryEvent;
use refact_tasks::events::{TaskEvent, TaskEventEnvelope};
use refact_runtime_api::task_storage_runtime as storage;
use refact_tools::tools::tool_task_documents::{
    CreateDocumentRequest, TaskDocumentDetail, TaskDocumentHistoryResponse,
    TaskDocumentListResponse, append_task_document_for_api_for_ctx,
    create_task_document_for_api_for_ctx, delete_task_document_for_api_for_ctx,
    get_task_document_for_api_for_ctx, history_task_document_for_api_for_ctx,
    list_task_documents_for_api_for_ctx, pin_task_document_for_api_for_ctx,
    update_task_document_for_api_for_ctx,
};
use refact_tools::tools::tool_task_memory::{
    MemoryKind, MemoryNamespace, TaskMemoriesApiResponse, TaskMemoryArchiveApiResponse,
    TaskMemoryFacetsResponse, TaskMemoryListFilters, TaskMemoryPinApiResponse,
    TaskMemoryTriageApiResponse, archive_task_memory_for_api, list_task_memories_for_api,
    mark_task_memories_triaged_for_api, set_task_memory_pinned_for_api, task_memory_facets_for_api,
};

#[derive(Deserialize)]
pub struct CreateTaskRequest {
    pub name: String,
    #[serde(default)]
    pub target_files: Vec<String>,
}

#[derive(Deserialize)]
pub struct TaskMemoriesQuery {
    pub since: Option<String>,
    pub kind: Option<String>,
    pub namespace: Option<String>,
    pub search: Option<String>,
}

#[derive(Deserialize)]
pub struct PinTaskMemoryRequest {
    pub pinned: bool,
}

#[derive(Deserialize)]
pub struct TriageDoneRequest {
    pub cursor: Option<String>,
}

#[derive(Deserialize)]
pub struct UpdateBoardRequest {
    pub rev: u64,
    #[serde(flatten)]
    pub patch: BoardPatch,
}

#[derive(Deserialize)]
pub struct CreateCardCommentRequest {
    pub body: String,
    pub author_role: String,
    #[serde(default)]
    pub author_id: Option<String>,
    #[serde(default)]
    pub reply_to: Option<String>,
}

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum BoardPatch {
    CreateCard {
        id: String,
        title: String,
        #[serde(default)]
        priority: Option<String>,
        #[serde(default)]
        depends_on: Vec<String>,
        #[serde(default)]
        instructions: String,
        #[serde(default)]
        target_files: Vec<String>,
    },
    UpdateCard {
        id: String,
        #[serde(default)]
        title: Option<String>,
        #[serde(default)]
        priority: Option<String>,
        #[serde(default)]
        depends_on: Option<Vec<String>>,
        #[serde(default)]
        instructions: Option<String>,
        #[serde(default)]
        target_files: Option<Vec<String>>,
        #[serde(default)]
        team_members: Option<Vec<TeamMember>>,
    },
    MoveCard {
        id: String,
        column: String,
    },
    AssignAgent {
        card_id: String,
        agent_id: String,
        agent_chat_id: String,
    },
    AddStatusUpdate {
        card_id: String,
        message: String,
    },
    AddComment {
        card_id: String,
        body: String,
        author_role: String,
        author_id: Option<String>,
        reply_to: Option<String>,
    },
    SetFinalReport {
        card_id: String,
        report: String,
    },
    DeleteCard {
        card_id: String,
        #[serde(default)]
        force: Option<bool>,
    },
}

async fn enrich_task_with_session_state(ctx: &ChatCtx, task: &mut TaskMeta) {
    refact_runtime_api::task_storage::enrich_task_with_session_state_for_services(&ctx.chat, task).await;
}

pub async fn list_tasks_with_session_state(ctx: &ChatCtx) -> Result<Vec<TaskMeta>, String> {
    let mut tasks = storage::list_tasks_for_ctx(ctx).await?;
    for task in &mut tasks {
        enrich_task_with_session_state(ctx, task).await;
    }
    Ok(tasks)
}

pub async fn handle_list_tasks(
    State(app): State<AppState>,
) -> Result<Json<Vec<TaskMeta>>, (StatusCode, String)> {
    let tasks = list_tasks_with_session_state(&app.chat_ctx())
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    Ok(Json(tasks))
}

pub async fn handle_create_task(
    State(app): State<AppState>,
    Json(req): Json<CreateTaskRequest>,
) -> Result<Json<TaskMeta>, (StatusCode, String)> {
    let ctx = app.chat_ctx();
    let mut meta = storage::create_task_for_ctx(&ctx, &req.name)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    if !req.target_files.is_empty() {
        let now = Utc::now().to_rfc3339();
        let mut board = storage::load_board_for_ctx(&ctx, &meta.id)
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
        board.cards.push(BoardCard {
            id: "targets".to_string(),
            title: "Target files".to_string(),
            column: "planned".into(),
            priority: "P1".into(),
            depends_on: vec![],
            instructions: String::new(),
            assignee: None,
            agent_chat_id: None,
            retry_count: 0,
            status_updates: vec![],
            comments: vec![],
            final_report: None,
            final_report_structured: None,
            verifier_report: None,
            created_at: now,
            started_at: None,
            last_heartbeat_at: None,
            completed_at: None,
            agent_branch: None,
            agent_worktree: None,
            agent_worktree_name: None,
            base_branch: None,
            base_commit: None,
            ab_variants: None,
            target_files: req.target_files,
            scope_guard_mode: Default::default(),
            team_members: vec![],
        });
        storage::save_board_for_ctx(&ctx, &meta.id, &board)
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
        meta = storage::update_task_stats_for_ctx(&ctx, &meta.id)
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    }
    Ok(Json(meta))
}

pub async fn handle_get_task(
    State(app): State<AppState>,
    Path(task_id): Path<String>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let ctx = app.chat_ctx();
    let meta = storage::load_task_meta_for_ctx(&ctx, &task_id)
        .await
        .map_err(|e| (StatusCode::NOT_FOUND, e))?;
    let board = storage::load_board_for_ctx(&ctx, &task_id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    let ready = board.get_ready_cards();

    Ok(Json(json!({
        "meta": meta,
        "board_summary": {
            "rev": board.rev,
            "cards_count": board.cards.len(),
            "ready": ready,
        }
    })))
}

#[derive(Deserialize, Default)]
pub struct DeleteTaskQuery {
    #[serde(default)]
    pub force: bool,
}

async fn remove_task_sessions(
    sessions: &refact_chat_api::SessionsMap,
    task_id: &str,
) -> Vec<(String, Arc<tokio::sync::Mutex<refact_chat_api::types::ChatSession>>)> {
    let live_sessions = {
        let read_guard = sessions.read().await;
        read_guard
            .iter()
            .map(|(chat_id, session_arc)| (chat_id.clone(), session_arc.clone()))
            .collect::<Vec<_>>()
    };
    let mut matching = Vec::new();
    for (chat_id, session_arc) in live_sessions {
        let task_meta = session_arc.lock().await.thread.task_meta.clone();
        if task_meta.as_ref().map_or(false, |m| m.task_id == task_id) {
            matching.push((chat_id, session_arc));
        }
    }
    let mut write_guard = sessions.write().await;
    let removed = matching
        .into_iter()
        .filter(|(chat_id, _)| write_guard.remove(chat_id).is_some())
        .collect();
    removed
}

async fn remove_trajectory_dir_entries(
    coordinator: &refact_chat_history::trajectory_index::TrajectoryIndexCoordinator,
    dir: &std::path::Path,
) {
    if !dir.exists() {
        return;
    }
    if let Ok(mut entries) = tokio::fs::read_dir(dir).await {
        while let Ok(Some(entry)) = entries.next_entry().await {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            let Some(chat_id) = path.file_stem().and_then(|s| s.to_str()).map(|s| s.to_string())
            else {
                continue;
            };
            if let Err(e) =
                refact_chat_history::trajectory_index::remove_trajectory_index_entry_with_rollout(
                    coordinator, dir, &chat_id,
                )
                .await
            {
                tracing::warn!(
                    "Failed to remove trajectory index entry {} in {:?}: {}",
                    chat_id,
                    dir,
                    e
                );
            }
        }
    }
}

async fn remove_task_trajectory_index_entries(app: &AppState, task_dir: &std::path::PathBuf) {
    let coordinator = &app.chat.trajectory_index_coordinator;
    let planner_dir = storage::get_task_trajectory_dir(task_dir, "planner", None);
    remove_trajectory_dir_entries(coordinator, &planner_dir).await;
    let agents_base = storage::get_task_trajectory_dir(task_dir, "agents", None);
    if let Ok(mut agent_dirs) = tokio::fs::read_dir(&agents_base).await {
        while let Ok(Some(agent_dir)) = agent_dirs.next_entry().await {
            let is_dir = agent_dir.file_type().await.map(|t| t.is_dir()).unwrap_or(false);
            if is_dir {
                remove_trajectory_dir_entries(coordinator, &agent_dir.path()).await;
            }
        }
    }
}

async fn delete_task_worktrees(
    ctx: &ChatCtx,
    task_id: &str,
    card_worktrees: &HashSet<String>,
) {
    let project_dirs = get_unscoped_project_dirs_for_ctx(ctx).await;
    let Some(project_root) = project_dirs.into_iter().next() else {
        return;
    };
    let service = match WorktreeService::new_async(ctx.paths.cache_dir.clone(), project_root).await {
        Ok(service) => service,
        Err(e) => {
            tracing::warn!(
                "Failed to init worktree service for task deletion {}: {}",
                task_id,
                e
            );
            return;
        }
    };
    let registry = match service.load_registry().await {
        Ok(registry) => registry,
        Err(e) => {
            tracing::warn!(
                "Failed to load worktree registry for task deletion {}: {}",
                task_id,
                e
            );
            return;
        }
    };
    let matching_ids: Vec<String> = registry
        .records
        .iter()
        .filter(|record| {
            record.meta.task_id.as_deref() == Some(task_id)
                || record
                    .references
                    .iter()
                    .any(|r| r.task_id.as_deref() == Some(task_id))
                || card_worktrees.contains(&record.meta.id)
        })
        .map(|record| record.meta.id.clone())
        .collect();
    for id in matching_ids {
        if let Err(e) = service.delete_worktree(&id, true, true).await {
            tracing::warn!("Failed to delete task worktree {}: {}", id, e);
        }
    }
}

pub async fn handle_delete_task(
    State(app): State<AppState>,
    Path(task_id): Path<String>,
    Query(query): Query<DeleteTaskQuery>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let ctx = app.chat_ctx();
    let task_dir = storage::find_task_dir_for_ctx(&ctx, &task_id)
        .await
        .map_err(|e| (StatusCode::NOT_FOUND, e))?;

    let board = storage::load_board_for_ctx(&ctx, &task_id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    if !query.force {
        let active_card_ids: Vec<String> = board
            .cards
            .iter()
            .filter(|c| {
                c.column == "doing" || c.agent_worktree.is_some() || c.agent_branch.is_some()
            })
            .map(|c| c.id.clone())
            .collect();
        if !active_card_ids.is_empty() {
            return Err((
                StatusCode::CONFLICT,
                format!(
                    "task has active agent cards: {}",
                    active_card_ids.join(", ")
                ),
            ));
        }
    }

    let card_worktrees: HashSet<String> = board
        .cards
        .iter()
        .filter_map(|c| c.agent_worktree_name.clone())
        .collect();

    let removed_sessions = remove_task_sessions(&ctx.chat.sessions, &task_id).await;
    for (chat_id, session_arc) in removed_sessions {
        match tokio::time::timeout(std::time::Duration::from_millis(500), session_arc.lock()).await
        {
            Ok(mut session) => {
                session.abort_stream();
                session.close_event_channel();
                session.queue_notify.notify_waiters();
            }
            Err(_) => {
                tracing::warn!("Timed out closing deleted task session {}", chat_id);
            }
        }
    }

    delete_task_worktrees(&ctx, &task_id, &card_worktrees).await;

    remove_task_trajectory_index_entries(&app, &task_dir).await;

    storage::delete_task_for_ctx(&ctx, &task_id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    Ok(Json(json!({"deleted": true})))
}

pub async fn handle_list_task_memories(
    State(app): State<AppState>,
    Path(task_id): Path<String>,
    Query(query): Query<TaskMemoriesQuery>,
) -> Result<Json<TaskMemoriesApiResponse>, (StatusCode, String)> {
    let since = query
        .since
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .map(refact_tools::tools::tool_task_memory::parse_rfc3339_utc)
        .transpose()
        .map_err(|e| (StatusCode::BAD_REQUEST, e))?;
    let kind = query
        .kind
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .map(str::parse::<MemoryKind>)
        .transpose()
        .map_err(|e| (StatusCode::BAD_REQUEST, e))?;
    let namespace = query
        .namespace
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .map(str::parse::<MemoryNamespace>)
        .transpose()
        .map_err(|e| (StatusCode::BAD_REQUEST, e))?;
    let search = query.search.filter(|value| !value.trim().is_empty());
    let result = list_task_memories_for_api(
        &app.chat_ctx(),
        &task_id,
        TaskMemoryListFilters {
            since,
            kind,
            namespace,
            search,
        },
    )
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    Ok(Json(result))
}

pub async fn handle_pin_task_memory(
    State(app): State<AppState>,
    Path((task_id, filename)): Path<(String, String)>,
    Json(req): Json<PinTaskMemoryRequest>,
) -> Result<Json<TaskMemoryPinApiResponse>, (StatusCode, String)> {
    let result = set_task_memory_pinned_for_api(&app.chat_ctx(), &task_id, &filename, req.pinned)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    Ok(Json(result))
}

pub async fn handle_archive_task_memory(
    State(app): State<AppState>,
    Path((task_id, filename)): Path<(String, String)>,
) -> Result<Json<TaskMemoryArchiveApiResponse>, (StatusCode, String)> {
    let result = archive_task_memory_for_api(&app.chat_ctx(), &task_id, &filename)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    Ok(Json(result))
}

pub async fn handle_task_memories_triage_done(
    State(app): State<AppState>,
    Path(task_id): Path<String>,
    Json(req): Json<TriageDoneRequest>,
) -> Result<Json<TaskMemoryTriageApiResponse>, (StatusCode, String)> {
    let cursor = req
        .cursor
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .map(refact_tools::tools::tool_task_memory::parse_rfc3339_utc)
        .transpose()
        .map_err(|e| (StatusCode::BAD_REQUEST, e))?;
    let result = mark_task_memories_triaged_for_api(&app.chat_ctx(), &task_id, cursor)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    Ok(Json(result))
}

pub async fn handle_task_memory_facets(
    State(app): State<AppState>,
    Path(task_id): Path<String>,
) -> Result<Json<TaskMemoryFacetsResponse>, (StatusCode, String)> {
    let result = task_memory_facets_for_api(&app.chat_ctx(), &task_id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    Ok(Json(result))
}

pub async fn handle_get_board(
    State(app): State<AppState>,
    Path(task_id): Path<String>,
) -> Result<Json<TaskBoard>, (StatusCode, String)> {
    let board = storage::load_board_for_ctx(&app.chat_ctx(), &task_id)
        .await
        .map_err(|e| (StatusCode::NOT_FOUND, e))?;
    Ok(Json(board))
}

pub async fn handle_create_card_comment(
    State(app): State<AppState>,
    Path((task_id, card_id)): Path<(String, String)>,
    Json(req): Json<CreateCardCommentRequest>,
) -> Result<Json<TaskBoard>, (StatusCode, String)> {
    let (board, _) = create_card_comment_for_ctx(
        &app.chat_ctx(),
        &task_id,
        CreateCardComment {
            card_id,
            body: req.body,
            author_role: req.author_role,
            author_id: req.author_id,
            reply_to: req.reply_to,
        },
    )
    .await
    .map_err(map_patch_board_error)?;
    Ok(Json(board))
}

fn map_patch_board_error(error: String) -> (StatusCode, String) {
    if error.starts_with("Board rev mismatch:") || error.starts_with("active agent card:") {
        return (StatusCode::CONFLICT, error);
    }
    if error.contains(" not found") {
        return (StatusCode::NOT_FOUND, error);
    }
    if error.starts_with("Card ")
        || error.starts_with("Invalid column:")
        || error == "comment body is empty"
        || error == "invalid author_role"
        || error == "reply_to references unknown comment"
    {
        return (StatusCode::BAD_REQUEST, error);
    }
    (StatusCode::INTERNAL_SERVER_ERROR, error)
}

pub async fn handle_patch_board(
    State(app): State<AppState>,
    Path(task_id): Path<String>,
    Json(req): Json<UpdateBoardRequest>,
) -> Result<Json<TaskBoard>, (StatusCode, String)> {
    let ctx = app.chat_ctx();
    let expected_rev = req.rev;
    let patch = req.patch;
    let comment_card_id = match &patch {
        BoardPatch::AddComment { card_id, .. } => Some(card_id.clone()),
        _ => None,
    };
    let now = Utc::now().to_rfc3339();

    let update_result = storage::update_board_atomic_for_ctx(&ctx, &task_id, move |board| {
        if board.rev != expected_rev {
            let conflict = storage::BoardConflict {
                expected: expected_rev,
                actual: board.rev,
            };
            return Err(conflict.message());
        }

        match patch {
            BoardPatch::CreateCard {
                id,
                title,
                priority,
                depends_on,
                instructions,
                target_files,
            } => {
                if board.cards.iter().any(|c| c.id == id) {
                    return Err(format!("Card {} already exists", id));
                }
                board.cards.push(BoardCard {
                    id,
                    title,
                    column: "planned".into(),
                    priority: priority.unwrap_or_else(|| "P1".into()),
                    depends_on,
                    instructions,
                    assignee: None,
                    agent_chat_id: None,
                    retry_count: 0,
                    status_updates: vec![],
                    comments: vec![],
                    final_report: None,
                    final_report_structured: None,
                    verifier_report: None,
                    created_at: now.clone(),
                    started_at: None,
                    last_heartbeat_at: None,
                    completed_at: None,
                    agent_branch: None,
                    agent_worktree: None,
                    agent_worktree_name: None,
                    base_branch: None,
                    base_commit: None,
                    ab_variants: None,
                    target_files,
                    scope_guard_mode: Default::default(),
                    team_members: vec![],
                });
            }
            BoardPatch::UpdateCard {
                id,
                title,
                priority,
                depends_on,
                instructions,
                target_files,
                team_members,
            } => {
                let card = board
                    .get_card_mut(&id)
                    .ok_or_else(|| format!("Card {} not found", id))?;
                if let Some(t) = title {
                    card.title = t;
                }
                if let Some(p) = priority {
                    card.priority = p;
                }
                if let Some(d) = depends_on {
                    card.depends_on = d;
                }
                if let Some(i) = instructions {
                    card.instructions = i;
                }
                if let Some(files) = target_files {
                    card.target_files = files;
                }
                if let Some(members) = team_members {
                    card.team_members = members;
                    card.validate_team().map_err(|error| format!("Card {id}: {error}"))?;
                }
            }
            BoardPatch::MoveCard { id, column } => {
                let card = board
                    .get_card_mut(&id)
                    .ok_or_else(|| format!("Card {} not found", id))?;
                let valid_columns = ["planned", "doing", "done", "failed", "regressed"];
                if !valid_columns.contains(&column.as_str()) {
                    return Err(format!("Invalid column: {}", column));
                }
                if column == "doing" && card.started_at.is_none() {
                    card.started_at = Some(now.clone());
                }
                if (column == "done" || column == "failed" || column == "regressed")
                    && card.completed_at.is_none()
                {
                    card.completed_at = Some(now.clone());
                }
                card.column = column;
            }
            BoardPatch::AssignAgent {
                card_id,
                agent_id,
                agent_chat_id,
            } => {
                let card = board
                    .get_card_mut(&card_id)
                    .ok_or_else(|| format!("Card {} not found", card_id))?;
                card.assignee = Some(agent_id);
                card.agent_chat_id = Some(agent_chat_id);
                if card.started_at.is_none() {
                    card.started_at = Some(now.clone());
                }
            }
            BoardPatch::AddStatusUpdate { card_id, message } => {
                let card = board
                    .get_card_mut(&card_id)
                    .ok_or_else(|| format!("Card {} not found", card_id))?;
                card.status_updates.push(StatusUpdate {
                    timestamp: now.clone(),
                    message,
                });
            }
            BoardPatch::AddComment {
                card_id,
                body,
                author_role,
                author_id,
                reply_to,
            } => {
                refact_tasks::comments::add_comment_to_board(
                    board,
                    CreateCardComment {
                        card_id,
                        body,
                        author_role,
                        author_id,
                        reply_to,
                    },
                )?;
            }
            BoardPatch::SetFinalReport { card_id, report } => {
                let card = board
                    .get_card_mut(&card_id)
                    .ok_or_else(|| format!("Card {} not found", card_id))?;
                card.final_report = Some(report);
                card.final_report_structured = None;
            }
            BoardPatch::DeleteCard { card_id, force } => {
                match board.cards.iter().position(|c| c.id == card_id) {
                    None => return Err("card not found".to_string()),
                    Some(idx) => {
                        let is_active = {
                            let card = &board.cards[idx];
                            card.column == "doing"
                                || card.agent_chat_id.is_some()
                                || card.agent_worktree.is_some()
                                || card.agent_branch.is_some()
                        };
                        if is_active && !force.unwrap_or(false) {
                            return Err(format!("active agent card: {}", card_id));
                        }
                        board.cards.remove(idx);
                    }
                }
            }
        }

        Ok(())
    })
    .await;

    let (board, _) = match update_result {
        Ok(result) => result,
        Err(e) => return Err(map_patch_board_error(e)),
    };

    storage::update_task_stats_for_ctx(&ctx, &task_id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    if let Some(card_id) = comment_card_id {
        refact_tasks::events_emit::emit_task_event_for_services(
            &ctx.chat,
            TaskEvent::TaskCommentsChanged {
                task_id: task_id.clone(),
                card_id,
            },
        )
        .await;
    }

    Ok(Json(board))
}

pub async fn handle_get_planner_instructions(
    State(app): State<AppState>,
    Path(task_id): Path<String>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let content = storage::load_planner_instructions_for_ctx(&app.chat_ctx(), &task_id)
        .await
        .map_err(|e| (StatusCode::NOT_FOUND, e))?;
    Ok(Json(json!({"content": content})))
}

#[derive(Deserialize)]
pub struct SetPlannerInstructionsRequest {
    pub content: String,
}

pub async fn handle_set_planner_instructions(
    State(app): State<AppState>,
    Path(task_id): Path<String>,
    Json(req): Json<SetPlannerInstructionsRequest>,
) -> Result<Json<Value>, (StatusCode, String)> {
    storage::save_planner_instructions_for_ctx(&app.chat_ctx(), &task_id, &req.content)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    Ok(Json(json!({"saved": true})))
}

pub async fn handle_get_ready_cards(
    State(app): State<AppState>,
    Path(task_id): Path<String>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let board = storage::load_board_for_ctx(&app.chat_ctx(), &task_id)
        .await
        .map_err(|e| (StatusCode::NOT_FOUND, e))?;
    let ready = board.get_ready_cards();
    Ok(Json(json!(ready)))
}

pub async fn handle_update_task_status(
    State(app): State<AppState>,
    Path(task_id): Path<String>,
    Json(req): Json<UpdateTaskStatusRequest>,
) -> Result<Json<TaskMeta>, (StatusCode, String)> {
    let ctx = app.chat_ctx();
    let mut meta = storage::load_task_meta_for_ctx(&ctx, &task_id)
        .await
        .map_err(|e| (StatusCode::NOT_FOUND, e))?;
    let old_status = meta.status;
    meta.status = req.status;
    meta.updated_at = Utc::now().to_rfc3339();
    storage::save_task_meta_for_ctx(&ctx, &task_id, &meta)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    refact_runtime_api::task_storage::emit_task_updated_via(
        &ctx.chat.sessions,
        &Some(ctx.chat.task_events_tx.clone()),
        &Some(ctx.chat.task_events_seq.clone()),
        task_id.clone(),
        meta.clone(),
    )
    .await;
    if old_status != meta.status {
        match meta.status {
            TaskStatus::Completed => {
                refact_runtime_api::buddy_runtime::buddy_apply(
                    app.clone(),
                    refact_buddy_core::actor::BuddyMutation {
                        runtime_event: Some(refact_buddy_core::actor::make_runtime_event(
                            "task_completed",
                            &format!("Task done: {}", meta.name),
                            "task",
                            &format!("task_{}", task_id),
                            "completed",
                            None,
                        )),
                        xp: 30,
                        activity: Some(refact_buddy_core::types::BuddyActivity {
                            icon: "вњ…".to_string(),
                            title: format!("Task completed: {}", meta.name),
                            description: format!("Task {} finished successfully", task_id),
                            timestamp: Utc::now().to_rfc3339(),
                            activity_type: "task_completed".to_string(),
                            chat_id: None,
                            failure_category: None,
                            failure_summary: None,
                        }),
                        mood: Some("excited".to_string()),
                    },
                )
                .await;
            }
            TaskStatus::Abandoned => {
                refact_runtime_api::buddy_runtime::buddy_apply(
                    app.clone(),
                    refact_buddy_core::actor::BuddyMutation {
                        runtime_event: Some(refact_buddy_core::actor::make_runtime_event(
                            "task_abandoned",
                            &format!("Task abandoned: {}", meta.name),
                            "task",
                            &format!("task_{}", task_id),
                            "failed",
                            Some("high"),
                        )),
                        activity: Some(refact_buddy_core::types::BuddyActivity {
                            icon: "рџ—‘пёЏ".to_string(),
                            title: format!("Task abandoned: {}", meta.name),
                            description: format!("Task {} was abandoned", task_id),
                            timestamp: Utc::now().to_rfc3339(),
                            activity_type: "task_abandoned".to_string(),
                            chat_id: None,
                            failure_category: None,
                            failure_summary: None,
                        }),
                        mood: Some("worried".to_string()),
                        ..Default::default()
                    },
                )
                .await;
            }
            _ => {}
        }
    }
    enrich_task_with_session_state(&ctx, &mut meta).await;
    Ok(Json(meta))
}

#[derive(Deserialize)]
pub struct UpdateTaskStatusRequest {
    pub status: TaskStatus,
}

#[derive(Deserialize)]
pub struct UpdateTaskMetaRequest {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub base_branch: Option<String>,
    #[serde(default)]
    pub base_commit: Option<String>,
    #[serde(default)]
    pub default_agent_model: Option<String>,
}

pub async fn handle_update_task_meta(
    State(app): State<AppState>,
    Path(task_id): Path<String>,
    Json(req): Json<UpdateTaskMetaRequest>,
) -> Result<Json<TaskMeta>, (StatusCode, String)> {
    let ctx = app.chat_ctx();
    let mut meta = storage::load_task_meta_for_ctx(&ctx, &task_id)
        .await
        .map_err(|e| (StatusCode::NOT_FOUND, e))?;
    if let Some(name) = req.name {
        meta.name = name;
    }
    if let Some(branch) = req.base_branch {
        meta.base_branch = Some(branch);
    }
    if let Some(commit) = req.base_commit {
        meta.base_commit = Some(commit);
    }
    if let Some(model) = req.default_agent_model {
        meta.default_agent_model = Some(model);
    }
    meta.updated_at = Utc::now().to_rfc3339();
    storage::save_task_meta_for_ctx(&ctx, &task_id, &meta)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    refact_runtime_api::task_storage::emit_task_updated_via(
        &ctx.chat.sessions,
        &Some(ctx.chat.task_events_tx.clone()),
        &Some(ctx.chat.task_events_seq.clone()),
        task_id,
        meta.clone(),
    )
    .await;
    enrich_task_with_session_state(&ctx, &mut meta).await;
    Ok(Json(meta))
}

pub async fn handle_list_task_trajectories(
    State(app): State<AppState>,
    Path((task_id, role)): Path<(String, String)>,
) -> Result<Json<Vec<TrajectoryInfo>>, (StatusCode, String)> {
    let trajectories = storage::list_task_trajectories_for_ctx(
        &app.chat_ctx(),
        &task_id,
        &role,
        None,
        refact_chat_history::trajectory_index::TrajectoryIndexListingCaller::TaskTrajectoryApi,
    )
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    Ok(Json(trajectories))
}

#[derive(Deserialize, Default)]
pub struct CreatePlannerChatRequest {
    #[serde(default)]
    pub mode: Option<String>,
}

pub async fn handle_create_planner_chat(
    State(app): State<AppState>,
    Path(task_id): Path<String>,
    body_bytes: hyper::body::Bytes,
) -> Result<Json<Value>, (StatusCode, String)> {
    let ctx = app.chat_ctx();
    let req: CreatePlannerChatRequest = if body_bytes.is_empty() {
        CreatePlannerChatRequest::default()
    } else {
        serde_json::from_slice(&body_bytes)
            .map_err(|e| (StatusCode::BAD_REQUEST, format!("Invalid JSON: {}", e)))?
    };
    let mode = req
        .mode
        .map(|m| m.trim().to_string())
        .filter(|m| !m.is_empty())
        .unwrap_or_else(|| "task_planner".to_string());
    if mode.eq_ignore_ascii_case("task_agent") {
        return Err((
            StatusCode::BAD_REQUEST,
            "task_agent chats cannot be created here".to_string(),
        ));
    }
    storage::load_task_meta_for_ctx(&ctx, &task_id)
        .await
        .map_err(|e| (StatusCode::NOT_FOUND, e))?;

    let chat_id = storage::next_planner_chat_id_for_ctx(&ctx, &task_id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    refact_runtime_api::trajectory_runtime::save_initial_task_chat_trajectory(
        &ctx,
        &task_id,
        &chat_id,
        &mode,
    )
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    Ok(Json(json!({"chat_id": chat_id, "mode": mode})))
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct AgentReference {
    chat_id: String,
    card_id: Option<String>,
    source: &'static str,
}

#[derive(Deserialize, Default)]
struct PersistedTaskMetaBlock {
    #[serde(default)]
    task_id: String,
    #[serde(default)]
    card_id: Option<String>,
    #[serde(default)]
    planner_chat_id: Option<String>,
}

#[derive(Deserialize, Default)]
struct PersistedAgentTrajectoryBlock {
    #[serde(default)]
    chat_id: String,
    #[serde(default)]
    task_meta: Option<PersistedTaskMetaBlock>,
}

#[derive(Deserialize, Default)]
pub struct DeletePlannerChatQuery {
    #[serde(default)]
    pub force: bool,
}

fn delete_error(status: StatusCode, error: impl Into<String>) -> (StatusCode, Json<Value>) {
    (status, Json(json!({ "error": error.into() })))
}

async fn scan_persisted_agent_trajectory(
    path: &std::path::Path,
    task_id: &str,
    planner_chat_id: &str,
) -> Result<Option<AgentReference>, String> {
    let content = tokio::fs::read_to_string(path).await.map_err(|e| {
        format!(
            "Failed to read persisted agent trajectory {:?}: {}",
            path, e
        )
    })?;
    let trajectory: PersistedAgentTrajectoryBlock =
        serde_json::from_str(&content).map_err(|e| {
            format!(
                "Failed to parse persisted agent trajectory {:?}: {}",
                path, e
            )
        })?;
    let Some(task_meta) = trajectory.task_meta else {
        return Ok(None);
    };
    if task_meta.task_id != task_id || task_meta.planner_chat_id.as_deref() != Some(planner_chat_id)
    {
        return Ok(None);
    }
    let chat_id = if trajectory.chat_id.is_empty() {
        path.file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or_default()
            .to_string()
    } else {
        trajectory.chat_id
    };
    if chat_id.is_empty() {
        return Ok(None);
    }
    Ok(Some(AgentReference {
        chat_id,
        card_id: task_meta.card_id,
        source: "persisted",
    }))
}

async fn find_planner_referencing_agents(
    app: &AppState,
    task_id: &str,
    planner_chat_id: &str,
) -> Result<Vec<AgentReference>, String> {
    let mut refs = Vec::new();
    let mut seen = HashSet::new();
    let live_sessions = {
        let sessions = app.chat.sessions.read().await;
        sessions
            .iter()
            .map(|(chat_id, session)| (chat_id.clone(), session.clone()))
            .collect::<Vec<_>>()
    };
    for (chat_id, session_arc) in live_sessions {
        if chat_id == planner_chat_id {
            continue;
        }
        let task_meta = {
            let session = session_arc.lock().await;
            session.thread.task_meta.clone()
        };
        let Some(task_meta) = task_meta else {
            continue;
        };
        if task_meta.role == "agents"
            && task_meta.task_id == task_id
            && task_meta.planner_chat_id.as_deref() == Some(planner_chat_id)
        {
            seen.insert((chat_id.clone(), "live"));
            refs.push(AgentReference {
                chat_id,
                card_id: task_meta.card_id,
                source: "live",
            });
        }
    }

    let task_dir = storage::find_task_dir_for_ctx(&app.chat_ctx(), task_id).await?;
    let agents_dir = storage::get_task_trajectory_dir(&task_dir, "agents", None);
    if !agents_dir.exists() {
        return Ok(refs);
    }
    let mut agent_dirs = tokio::fs::read_dir(&agents_dir).await.map_err(|e| {
        format!(
            "Failed to read persisted agent trajectories {:?}: {}",
            agents_dir, e
        )
    })?;
    while let Some(agent_dir) = agent_dirs.next_entry().await.map_err(|e| e.to_string())? {
        let file_type = agent_dir.file_type().await.map_err(|e| e.to_string())?;
        if !file_type.is_dir() {
            continue;
        }
        let mut trajectories = tokio::fs::read_dir(agent_dir.path()).await.map_err(|e| {
            format!(
                "Failed to read persisted agent directory {:?}: {}",
                agent_dir.path(),
                e
            )
        })?;
        while let Some(entry) = trajectories.next_entry().await.map_err(|e| e.to_string())? {
            let file_type = entry.file_type().await.map_err(|e| e.to_string())?;
            if !file_type.is_file()
                || entry.path().extension().and_then(|ext| ext.to_str()) != Some("json")
            {
                continue;
            }
            let Some(agent_ref) =
                scan_persisted_agent_trajectory(&entry.path(), task_id, planner_chat_id).await?
            else {
                continue;
            };
            if seen.insert((agent_ref.chat_id.clone(), agent_ref.source)) {
                refs.push(agent_ref);
            }
        }
    }
    Ok(refs)
}

async fn add_planner_deleted_status_updates(
    ctx: &ChatCtx,
    task_id: &str,
    planner_chat_id: &str,
    agent_refs: &[AgentReference],
) -> Result<(), String> {
    let card_ids = agent_refs
        .iter()
        .filter_map(|agent_ref| agent_ref.card_id.clone())
        .collect::<HashSet<_>>();
    if card_ids.is_empty() {
        return Ok(());
    }
    let timestamp = Utc::now().to_rfc3339();
    let message = format!(
        "Planner chat {} deleted while this agent still referenced it; planner_chat_id link is now dangling.",
        planner_chat_id
    );
    storage::update_board_atomic_for_ctx(ctx, task_id, move |board| {
        for card in &mut board.cards {
            if card_ids.contains(&card.id) {
                card.status_updates.push(StatusUpdate {
                    timestamp: timestamp.clone(),
                    message: message.clone(),
                });
            }
        }
        Ok(())
    })
    .await
    .map(|_| ())
}

pub async fn handle_delete_planner_chat(
    State(app): State<AppState>,
    Path((task_id, chat_id)): Path<(String, String)>,
    Query(query): Query<DeletePlannerChatQuery>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let ctx = app.chat_ctx();
    refact_chat_history::trajectory_paths::validate_trajectory_id(&chat_id)
        .map_err(|e| delete_error(e.status_code, e.message))?;
    storage::load_task_meta_for_ctx(&ctx, &task_id)
        .await
        .map_err(|e| delete_error(StatusCode::NOT_FOUND, e))?;
    let task_dir = storage::find_task_dir_for_ctx(&ctx, &task_id)
        .await
        .map_err(|e| delete_error(StatusCode::NOT_FOUND, e))?;
    let planner_dir = storage::get_task_trajectory_dir(&task_dir, "planner", None);
    let expected_file_path = planner_dir.join(format!("{}.json", chat_id));
    let file_path = if expected_file_path.exists() {
        expected_file_path
    } else {
        match refact_runtime_api::trajectory_runtime::find_trajectory_path(&ctx, &chat_id).await {
            Some(found_path) if found_path.exists() => found_path,
            _ => {
                return Err(delete_error(
                    StatusCode::NOT_FOUND,
                    "Planner chat not found",
                ))
            }
        }
    };
    let canon_dir = tokio::fs::canonicalize(&planner_dir)
        .await
        .map_err(|e| delete_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let canon_file = tokio::fs::canonicalize(&file_path)
        .await
        .map_err(|e| delete_error(StatusCode::NOT_FOUND, e.to_string()))?;

    if !canon_file.starts_with(&canon_dir) || canon_file.parent() != Some(canon_dir.as_path()) {
        return Err(delete_error(
            StatusCode::FORBIDDEN,
            "Planner chat does not belong to this task",
        ));
    }

    let agent_refs = find_planner_referencing_agents(&app, &task_id, &chat_id)
        .await
        .map_err(|e| delete_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    if !agent_refs.is_empty() && !query.force {
        return Err((
            StatusCode::CONFLICT,
            Json(json!({
                "error": "planner chat has active or persisted agent references",
                "agent_refs": agent_refs,
                "hint": "Pass ?force=true to delete anyway."
            })),
        ));
    }

    if !agent_refs.is_empty() {
        tracing::warn!(
            ?agent_refs,
            "Force deleting planner chat {} while task agents still reference it",
            chat_id
        );
        add_planner_deleted_status_updates(&ctx, &task_id, &chat_id, &agent_refs)
            .await
            .map_err(|e| delete_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    }

    tokio::fs::remove_file(&file_path)
        .await
        .map_err(|e| delete_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    if let Some(dir) = file_path.parent() {
        if let Err(e) = refact_chat_history::trajectory_index::remove_trajectory_index_entry_with_rollout(
            &app.chat.trajectory_index_coordinator,
            dir,
            &chat_id,
        )
        .await
        {
            tracing::warn!(
                "Failed to remove planner trajectory {} from index {:?}: {}",
                chat_id,
                dir,
                e
            );
        }
    }

    let removed_session = {
        let sessions = ctx.chat.sessions.clone();
        let mut sessions_guard = sessions.write().await;
        sessions_guard.remove(&chat_id)
    };
    if let Some(session_arc) = removed_session {
        match tokio::time::timeout(std::time::Duration::from_millis(500), session_arc.lock()).await
        {
            Ok(mut session) => {
                session.abort_stream();
                session.close_event_channel();
                session.queue_notify.notify_waiters();
            }
            Err(_) => {
                tracing::warn!("Timed out closing deleted planner chat session {}", chat_id);
            }
        }
    }

    let _ = ctx.chat.trajectory_events_tx.send(TrajectoryEvent {
        event_type: "deleted".to_string(),
        id: chat_id,
        updated_at: None,
        title: None,
        is_title_generated: None,
        session_state: None,
        error: None,
        message_count: None,
        parent_id: None,
        link_type: None,
        root_chat_id: None,
        task_id: None,
        task_role: None,
        agent_id: None,
        card_id: None,
        model: None,
        mode: None,
        worktree: None,
        total_lines_added: None,
        total_lines_removed: None,
        tasks_total: None,
        tasks_done: None,
        tasks_failed: None,
        total_prompt_tokens: None,
        total_completion_tokens: None,
        total_tokens: None,
        total_cache_read_tokens: None,
        total_cache_creation_tokens: None,
        total_cost_usd: None,
    });

    Ok(Json(json!({"deleted": true})))
}

pub async fn handle_tasks_subscribe(
    State(app): State<AppState>,
) -> Result<Response<Body>, ScratchError> {
    let ctx = app.chat_ctx();
    let (tx, rx, seq_counter, snapshot_seq, tasks) = {
        let tx = ctx.chat.task_events_tx.clone();
        let seq_counter = ctx.chat.task_events_seq.clone();
        let snapshot_seq = seq_counter.fetch_add(1, Ordering::SeqCst);
        let rx = tx.subscribe();
        let tasks = list_tasks_with_session_state(&ctx)
            .await
            .map_err(|e| ScratchError::new(StatusCode::INTERNAL_SERVER_ERROR, e))?;
        (tx, rx, seq_counter, snapshot_seq, tasks)
    };

    let stream = async_stream::stream! {
        let envelope = TaskEventEnvelope { seq: snapshot_seq, event: TaskEvent::Snapshot { tasks } };
        let json = serde_json::to_string(&envelope).unwrap_or_default();
        yield Ok::<_, std::convert::Infallible>(format!("data: {}\n\n", json));

        let mut rx = rx;
        let mut heartbeat = tokio::time::interval_at(
            tokio::time::Instant::now() + std::time::Duration::from_secs(10),
            std::time::Duration::from_secs(10),
        );
        heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                result = rx.recv() => {
                    match result {
                        Ok(envelope) => {
                            let json = serde_json::to_string(&envelope).unwrap_or_default();
                            yield Ok::<_, std::convert::Infallible>(format!("data: {}\n\n", json));
                        }
                        Err(broadcast::error::RecvError::Lagged(_)) => {
                            let seq = seq_counter.fetch_add(1, Ordering::SeqCst);
                            rx = tx.subscribe();
                            match list_tasks_with_session_state(&ctx).await {
                                Ok(tasks) => {
                                    let envelope = TaskEventEnvelope { seq, event: TaskEvent::Snapshot { tasks } };
                                    let json = serde_json::to_string(&envelope).unwrap_or_default();
                                    yield Ok::<_, std::convert::Infallible>(format!("data: {}\n\n", json));
                                }
                                Err(error) => {
                                    let payload = serde_json::json!({
                                        "type": "error",
                                        "retryable": true,
                                        "error": error,
                                    });
                                    yield Ok::<_, std::convert::Infallible>(format!("event: error\ndata: {}\n\n", payload));
                                }
                            }
                        }
                        Err(broadcast::error::RecvError::Closed) => break,
                    }
                }
                _ = heartbeat.tick() => {
                    let ts = Utc::now().to_rfc3339();
                    let seq = seq_counter.fetch_add(1, Ordering::SeqCst);
                    let envelope = TaskEventEnvelope { seq, event: TaskEvent::Heartbeat { ts } };
                    let json = serde_json::to_string(&envelope).unwrap_or_default();
                    yield Ok::<_, std::convert::Infallible>(format!("data: {}\n\n", json));
                }

                _ = async {
                    let shutdown_flag = ctx.runtime.shutdown_flag.clone();
                    while !shutdown_flag.load(std::sync::atomic::Ordering::SeqCst) {
                        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                    }
                } => {
                    break;
                }
            }
        }
    };

    Ok(Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "text/event-stream")
        .header("Cache-Control", "no-cache")
        .header("Connection", "keep-alive")
        .body(Body::wrap_stream(stream))
        .unwrap())
}

#[derive(Deserialize)]
pub struct GetDocumentQuery {
    pub version: Option<u64>,
}

#[derive(Deserialize)]
pub struct UpdateTaskDocumentRequest {
    pub content: String,
    pub pinned: Option<bool>,
}

#[derive(Deserialize)]
pub struct AppendTaskDocumentRequest {
    pub section: String,
}

#[derive(Deserialize)]
pub struct PinTaskDocumentRequest {
    pub pinned: bool,
}

fn map_doc_error(e: String) -> (StatusCode, String) {
    if e.contains("already exists")
        || e.contains("invalid kind")
        || e.contains("slug must")
        || e.contains("slug cannot")
    {
        (StatusCode::BAD_REQUEST, e)
    } else if e.contains("does not exist") || e.contains("not found") {
        (StatusCode::NOT_FOUND, e)
    } else {
        (StatusCode::INTERNAL_SERVER_ERROR, e)
    }
}

pub async fn handle_list_task_documents(
    State(app): State<AppState>,
    Path(task_id): Path<String>,
) -> Result<Json<TaskDocumentListResponse>, (StatusCode, String)> {
    let result = list_task_documents_for_api_for_ctx(&app.chat_ctx(), &task_id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    Ok(Json(result))
}

pub async fn handle_get_task_document(
    State(app): State<AppState>,
    Path((task_id, slug)): Path<(String, String)>,
    Query(query): Query<GetDocumentQuery>,
) -> Result<Json<TaskDocumentDetail>, (StatusCode, String)> {
    let result = get_task_document_for_api_for_ctx(&app.chat_ctx(), &task_id, &slug, query.version)
        .await
        .map_err(map_doc_error)?;
    Ok(Json(result))
}

pub async fn handle_create_task_document(
    State(app): State<AppState>,
    Path(task_id): Path<String>,
    Json(req): Json<CreateDocumentRequest>,
) -> Result<Json<TaskDocumentDetail>, (StatusCode, String)> {
    let result = create_task_document_for_api_for_ctx(&app.chat_ctx(), &task_id, req)
        .await
        .map_err(map_doc_error)?;
    Ok(Json(result))
}

pub async fn handle_update_task_document(
    State(app): State<AppState>,
    Path((task_id, slug)): Path<(String, String)>,
    Json(req): Json<UpdateTaskDocumentRequest>,
) -> Result<Json<TaskDocumentDetail>, (StatusCode, String)> {
    let result =
        update_task_document_for_api_for_ctx(
            &app.chat_ctx(),
            &task_id,
            &slug,
            req.content,
            req.pinned,
        )
        .await
        .map_err(map_doc_error)?;
    Ok(Json(result))
}

pub async fn handle_append_task_document(
    State(app): State<AppState>,
    Path((task_id, slug)): Path<(String, String)>,
    Json(req): Json<AppendTaskDocumentRequest>,
) -> Result<Json<TaskDocumentDetail>, (StatusCode, String)> {
    let result = append_task_document_for_api_for_ctx(&app.chat_ctx(), &task_id, &slug, req.section)
        .await
        .map_err(map_doc_error)?;
    Ok(Json(result))
}

pub async fn handle_delete_task_document(
    State(app): State<AppState>,
    Path((task_id, slug)): Path<(String, String)>,
) -> Result<StatusCode, (StatusCode, String)> {
    delete_task_document_for_api_for_ctx(&app.chat_ctx(), &task_id, &slug)
        .await
        .map_err(map_doc_error)?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn handle_pin_task_document(
    State(app): State<AppState>,
    Path((task_id, slug)): Path<(String, String)>,
    Json(req): Json<PinTaskDocumentRequest>,
) -> Result<Json<TaskDocumentDetail>, (StatusCode, String)> {
    let result = pin_task_document_for_api_for_ctx(&app.chat_ctx(), &task_id, &slug, req.pinned)
        .await
        .map_err(map_doc_error)?;
    Ok(Json(result))
}

pub async fn handle_history_task_document(
    State(app): State<AppState>,
    Path((task_id, slug)): Path<(String, String)>,
) -> Result<Json<TaskDocumentHistoryResponse>, (StatusCode, String)> {
    let result = history_task_document_for_api_for_ctx(&app.chat_ctx(), &task_id, &slug)
        .await
        .map_err(map_doc_error)?;
    Ok(Json(result))
}
