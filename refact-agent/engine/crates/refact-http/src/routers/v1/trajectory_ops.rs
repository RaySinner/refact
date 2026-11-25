use std::sync::Arc;
use axum::extract::Path;
use axum::http::{Response, StatusCode};
use axum::extract::State;
use hyper::Body;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use refact_runtime_api::app_state::AppState;
use refact_runtime_api::GlobalContextCtx;
use refact_runtime_api::global_context::GlobalContext;
use refact_chat_history::trajectory_ops::{CompressOptions, TransformStats, compress_in_place};
use refact_agentic::mode_transition::{
    GoalTransferResult, ReconstructionRequest, insert_goal_messages_before_plan,
};
use refact_runtime_api::mode_transition_runtime::reconstruct_context;
use refact_chat_api::SessionState;
use refact_tools::tools::tool_handoff_to_mode::{
    persist_transition, transition_goal, transition_report,
};

pub struct TransitionReservation {
    session: Arc<tokio::sync::Mutex<refact_chat_api::ChatSession>>,
    marker: serde_json::Value,
    fingerprint: serde_json::Value,
    abort: Arc<std::sync::atomic::AtomicBool>,
}

use refact_tools::tools::tool_handoff_to_mode::transition_fingerprint;

impl TransitionReservation {
    pub async fn acquire(
        session: Arc<tokio::sync::Mutex<refact_chat_api::ChatSession>>,
    ) -> Result<Self, ScratchError> {
        let mut source = session.lock().await;
        if source.closed
            || !matches!(
                source.runtime.state,
                SessionState::Idle | SessionState::Error | SessionState::Completed
            )
            || compression_attempt_active(&source)
            || source.pending_mode_handoff.is_some()
            || source.pending_context_rebuild.is_some()
        {
            return Err(ScratchError::new(
                StatusCode::CONFLICT,
                "Another context operation is pending or source is busy".into(),
            ));
        }
        if source.draft_message.is_some() || source.stream_started_at.is_some() {
            return Err(ScratchError::new(
                StatusCode::CONFLICT,
                "Source stream is not finished".into(),
            ));
        }
        source
            .abort_flag
            .store(false, std::sync::atomic::Ordering::SeqCst);
        source.set_runtime_state(SessionState::Idle, None);
        let marker = serde_json::json!({"http_transition": Uuid::new_v4().to_string()});
        source.pending_mode_handoff = Some(marker.clone());
        let fingerprint = transition_fingerprint(&source);
        let abort = source.abort_flag.clone();
        drop(source);
        Ok(Self {
            session,
            marker,
            fingerprint,
            abort,
        })
    }
}

impl Drop for TransitionReservation {
    fn drop(&mut self) {
        self.abort.store(true, std::sync::atomic::Ordering::SeqCst);
        let session = self.session.clone();
        let marker = self.marker.clone();
        tokio::spawn(async move {
            let mut source = session.lock().await;
            if source.pending_mode_handoff.as_ref() == Some(&marker) {
                source.pending_mode_handoff = None;
            }
        });
    }
}

use refact_runtime_api::session_runtime::get_or_create_session_with_trajectory;
use refact_chat_api::compression_gate::compression_attempt_active;
use refact_chat_api::context_rebuild::{ManualCompressionApplyResult, ManualCompressionPreview};
use refact_runtime_api::chat_context_rebuild::{
    apply_manual_context_rebuild, preview_manual_context_rebuild,
};
use refact_chat_api::GoalSnapshot;
use refact_chat_history::trajectory_snapshot::TrajectorySnapshot;
use refact_core::custom_error::ScratchError;
use refact_chat_api::setparams::map_legacy_mode_to_id;

pub fn transition_source_goal(session: &refact_chat_api::ChatSession) -> Option<GoalSnapshot> {
    refact_tools::tools::tool_handoff_to_mode::transition_source_goal(session)
}

fn canonical_transition_mode(raw_mode: &str) -> String {
    map_legacy_mode_to_id(raw_mode.trim()).to_string()
}

pub fn reset_transition_snapshot_identity(snapshot: &mut TrajectorySnapshot) {
    snapshot.previous_response_id = None;
    snapshot.frozen_request_prefix = None;
    snapshot.claude_code_identity = None;
}

fn epoch_ms_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

pub async fn create_initial_plan_document_for_transition(
    gcx: Arc<GlobalContext>,
    task_id: &str,
    plan_text: Option<&str>,
) -> (Option<String>, Option<String>) {
    let Some(plan_text) = plan_text.map(str::trim).filter(|text| !text.is_empty()) else {
        return (None, None);
    };
    let result = async {
        let documents_dir = refact_tools::tools::tool_task_documents::documents_dir_for_task_for_ctx(
            &gcx.chat_ctx(gcx.clone()),
            task_id,
        )
        .await?;
        let slug = refact_tools::tools::tool_task_documents::next_available_slug_at(
            &documents_dir,
            "initial-plan",
        )
        .await?;
        refact_tools::tools::tool_task_documents::create_document_at(
            &documents_dir,
            &slug,
            "Initial Plan",
            "plan",
            plan_text,
            true,
            Vec::new(),
            "planner",
        )
        .await?;
        Ok::<String, String>(slug)
    }
    .await;
    match result {
        Ok(slug) => (Some(slug), None),
        Err(error) => {
            tracing::warn!(
                "failed to create initial-plan document for task {}: {}",
                task_id,
                error
            );
            (None, Some(error))
        }
    }
}

#[derive(Deserialize)]
pub struct TransformRequest {
    pub options: CompressOptions,
    #[serde(default)]
    pub expected_trajectory_version: Option<u64>,
}

#[derive(Debug, Default, Deserialize)]
pub struct LlmCompressOptions {
    #[serde(default)]
    pub summary_model: Option<String>,
    #[serde(default)]
    pub expected_trajectory_version: Option<u64>,
}

#[derive(Debug, Default, Deserialize)]
pub struct LlmCompressRequest {
    #[serde(default)]
    pub options: LlmCompressOptions,
}

#[derive(Debug, Serialize)]
pub struct LlmCompressApplyResponse {
    pub applied: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved_model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_window: Option<usize>,
    pub source_messages: usize,
    pub approximate_source_tokens: usize,
    pub stats: TransformStats,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl From<ManualCompressionApplyResult> for LlmCompressApplyResponse {
    fn from(result: ManualCompressionApplyResult) -> Self {
        Self {
            applied: result.applied,
            resolved_model: result.resolved_model,
            context_window: result.context_window,
            source_messages: result.source_messages,
            approximate_source_tokens: result.approximate_source_tokens,
            stats: TransformStats {
                before_message_count: result.before_message_count,
                after_message_count: result.after_message_count,
                before_approx_tokens: result.before_approx_tokens,
                after_approx_tokens: result.after_approx_tokens,
                context_messages_modified: 0,
                tool_messages_modified: 0,
            },
            reason: result.reason,
        }
    }
}

impl LlmCompressRequest {
    pub fn requested_model(&self) -> Option<&str> {
        self.options
            .summary_model
            .as_deref()
            .map(str::trim)
            .filter(|model| !model.is_empty())
    }
}

pub fn parse_llm_compress_request(
    body_bytes: &hyper::body::Bytes,
) -> Result<LlmCompressRequest, ScratchError> {
    if body_bytes.iter().all(u8::is_ascii_whitespace) {
        return Ok(LlmCompressRequest::default());
    }
    serde_json::from_slice(body_bytes)
        .map_err(|_| ScratchError::new(StatusCode::BAD_REQUEST, "Invalid JSON request".to_string()))
}

fn json_response<T: Serialize>(value: &T) -> Result<Response<Body>, ScratchError> {
    let body = serde_json::to_vec(value)
        .map_err(|e| ScratchError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "application/json")
        .body(Body::from(body))
        .map_err(|e| ScratchError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

pub async fn handle_llm_compress_preview(
    State(app): State<AppState>,
    Path(chat_id): Path<String>,
    body_bytes: hyper::body::Bytes,
) -> Result<Response<Body>, ScratchError> {
    let req = parse_llm_compress_request(&body_bytes)?;
    let gcx = app.gcx.clone();
    let session_arc = get_or_create_session_with_trajectory(
        refact_runtime_api::app_state_from_gcx(gcx.clone()).await.chat_ctx(),
        &gcx.chat_sessions,
        &chat_id,
    )
    .await;
    let (state, active, messages, thread, trajectory_version) = {
        let session = session_arc.lock().await;
        (
            session.runtime.state,
            compression_attempt_active(&session),
            session.messages.clone(),
            session.thread.clone(),
            session.trajectory_version,
        )
    };
    let mut preview: ManualCompressionPreview = preview_manual_context_rebuild(
        gcx,
        state,
        active,
        &messages,
        &thread,
        req.requested_model(),
    )
    .await;
    if preview.eligible {
        preview.trajectory_version = Some(trajectory_version);
    }
    json_response(&preview)
}

pub async fn handle_llm_compress_apply(
    State(app): State<AppState>,
    Path(chat_id): Path<String>,
    body_bytes: hyper::body::Bytes,
) -> Result<Response<Body>, ScratchError> {
    let req = parse_llm_compress_request(&body_bytes)?;
    let gcx = app.gcx.clone();
    let session_arc = get_or_create_session_with_trajectory(
        refact_runtime_api::app_state_from_gcx(gcx.clone()).await.chat_ctx(),
        &gcx.chat_sessions,
        &chat_id,
    )
    .await;
    let result: ManualCompressionApplyResult = apply_manual_context_rebuild(
        gcx.clone(),
        &session_arc,
        req.requested_model(),
        req.options.expected_trajectory_version,
    )
    .await;
    if result.applied {
        let _ = refact_runtime_api::trajectories::store::try_save_trajectory_with_intent(
            refact_runtime_api::app_state_from_gcx(gcx).await,
            session_arc,
            refact_chat_api::TrajectoryCommitIntent::Required,
        )
        .await;
    }
    json_response(&LlmCompressApplyResponse::from(result))
}

#[derive(Serialize)]
pub struct TransformPreviewResponse {
    pub trajectory_version: u64,
    pub stats: TransformStats,
    pub actions: Vec<String>,
}

#[derive(Serialize)]
pub struct TransformApplyResponse {
    pub stats: TransformStats,
}

#[derive(Deserialize)]
pub struct ModeTransitionApplyRequest {
    pub target_mode: String,
    #[serde(default)]
    pub target_mode_description: String,
}

#[derive(Serialize)]
pub struct ModeTransitionApplyResponse {
    pub new_chat_id: String,
    pub messages_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub root_chat_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub initial_plan_document: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub initial_plan_error: Option<String>,
}

#[derive(Deserialize)]
pub struct PlannerFromTransitionRequest {
    pub source_chat_id: String,
    #[serde(default)]
    pub target_mode_description: String,
    #[serde(default)]
    pub target_mode: Option<String>,
}

fn describe_transform_actions(opts: &CompressOptions) -> Vec<String> {
    let mut actions = Vec::new();
    if opts.drop_all_context {
        actions.push("Drop all context_file messages".to_string());
    } else if opts.dedup_and_compress_context {
        actions.push("Deduplicate and compress context files".to_string());
    }
    if opts.drop_all_memories {
        actions.push("Drop all memory/knowledge context".to_string());
    }
    if opts.drop_project_information {
        actions.push("Drop project information from system messages".to_string());
    }
    if opts.compress_non_agentic_tools {
        actions.push("Compress tool results (preserving subagent)".to_string());
    }
    if opts.strip_metering {
        actions.push("Strip metering information".to_string());
    }
    actions.push("Remove invalid tool calls and orphan results".to_string());
    actions
}

pub async fn handle_transform_preview(
    State(app): State<AppState>,
    Path(chat_id): Path<String>,
    body_bytes: hyper::body::Bytes,
) -> Result<Response<Body>, ScratchError> {
    let gcx = app.gcx.clone();
    let req: TransformRequest = serde_json::from_slice(&body_bytes)
        .map_err(|e| ScratchError::new(StatusCode::BAD_REQUEST, format!("Invalid JSON: {}", e)))?;

    let sessions = gcx.chat_sessions.clone();
    let session_arc = get_or_create_session_with_trajectory(
        refact_runtime_api::app_state_from_gcx(gcx.clone()).await.chat_ctx(),
        &sessions,
        &chat_id,
    )
    .await;

    let (mut messages, trajectory_version) = {
        let session = session_arc.lock().await;
        (session.messages.clone(), session.trajectory_version)
    };

    let stats = compress_in_place(&mut messages, &req.options)
        .map_err(|e| ScratchError::new(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    let response = TransformPreviewResponse {
        trajectory_version,
        stats,
        actions: describe_transform_actions(&req.options),
    };

    let body = serde_json::to_vec(&response)
        .map_err(|e| ScratchError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "application/json")
        .body(Body::from(body))
        .map_err(|e| ScratchError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

pub async fn handle_transform_apply(
    State(app): State<AppState>,
    Path(chat_id): Path<String>,
    body_bytes: hyper::body::Bytes,
) -> Result<Response<Body>, ScratchError> {
    let gcx = app.gcx.clone();
    let req: TransformRequest = serde_json::from_slice(&body_bytes)
        .map_err(|e| ScratchError::new(StatusCode::BAD_REQUEST, format!("Invalid JSON: {}", e)))?;

    let sessions = gcx.chat_sessions.clone();
    let session_arc = get_or_create_session_with_trajectory(
        refact_runtime_api::app_state_from_gcx(gcx.clone()).await.chat_ctx(),
        &sessions,
        &chat_id,
    )
    .await;

    let stats = {
        let mut session = session_arc.lock().await;

        if req
            .expected_trajectory_version
            .is_some_and(|v| v != session.trajectory_version)
        {
            return Err(ScratchError::new(
                StatusCode::CONFLICT,
                "Trajectory changed".to_string(),
            ));
        }

        if compression_attempt_active(&session) {
            return Err(ScratchError::new(
                StatusCode::CONFLICT,
                "Compression already in progress; retry later".to_string(),
            ));
        }

        if matches!(
            session.runtime.state,
            SessionState::Generating | SessionState::ExecutingTools
        ) {
            session.abort_stream();
            session.clear_pending_tool_calls_for_interruption();
        }

        let stats = compress_in_place(&mut session.messages, &req.options)
            .map_err(|e| ScratchError::new(StatusCode::INTERNAL_SERVER_ERROR, e))?;

        session.set_runtime_state(SessionState::Idle, None);
        session.thread.previous_response_id = None;
        session.thread.frozen_request_prefix = None;
        session.thread.claude_code_identity = None;
        session.openai_codex_websocket = Default::default();
        session.reset_cache_guard_snapshot();
        session.provider_usage_stale = true;

        session.increment_version();
        let snapshot = session.snapshot();
        session.emit(snapshot);

        stats
    };

    refact_runtime_api::trajectories::store::maybe_save_trajectory_with_intent(
        refact_runtime_api::app_state_from_gcx(gcx.clone()).await,
        session_arc,
        refact_chat_api::TrajectoryCommitIntent::Required,
    )
    .await;

    let response = TransformApplyResponse { stats };

    let body = serde_json::to_vec(&response)
        .map_err(|e| ScratchError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "application/json")
        .body(Body::from(body))
        .map_err(|e| ScratchError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

/// Hold the source epoch stable until the destination is durable, then transfer
/// ownership. A failed reconstruction/save never overwrites newer source messages.
pub async fn commit_transition_snapshot(
    gcx: Arc<GlobalContext>,
    session_arc: Arc<tokio::sync::Mutex<refact_chat_api::ChatSession>>,
    source_version: u64,
    snapshot: TrajectorySnapshot,
    transferred_goal: &GoalTransferResult,
    reservation: Option<&TransitionReservation>,
) -> Result<(), String> {
    let mut session = session_arc.lock().await;
    if session.trajectory_version != source_version
        || !matches!(
            session.runtime.state,
            SessionState::Idle | SessionState::Error
        )
        || compression_attempt_active(&session)
        || session.closed
        || session.pending_context_rebuild.is_some()
        || reservation.is_some_and(|r| {
            session.pending_mode_handoff.as_ref() != Some(&r.marker)
                || transition_fingerprint(&session) != r.fingerprint
                || session.abort_flag.load(std::sync::atomic::Ordering::SeqCst)
        })
        || (reservation.is_none() && session.pending_mode_handoff.is_some())
    {
        return Err("Source trajectory changed during reconstruction".to_string());
    }
    persist_transition(gcx, &mut session, &snapshot, transferred_goal).await
}

pub async fn handle_mode_transition_apply(
    State(app): State<AppState>,
    Path(chat_id): Path<String>,
    body_bytes: hyper::body::Bytes,
) -> Result<Response<Body>, ScratchError> {
    let gcx = app.gcx.clone();
    let req: ModeTransitionApplyRequest = serde_json::from_slice(&body_bytes)
        .map_err(|e| ScratchError::new(StatusCode::BAD_REQUEST, format!("Invalid JSON: {}", e)))?;
    let target_mode = canonical_transition_mode(&req.target_mode);
    if target_mode == "task_planner" {
        return Err(ScratchError::new(
            StatusCode::BAD_REQUEST,
            "Use /v1/tasks/:task_id/planner-chats/from-transition for task_planner transitions"
                .to_string(),
        ));
    }

    let sessions = gcx.chat_sessions.clone();
    let session_arc = get_or_create_session_with_trajectory(
        refact_runtime_api::app_state_from_gcx(gcx.clone()).await.chat_ctx(),
        &sessions,
        &chat_id,
    )
    .await;

    let reservation = TransitionReservation::acquire(session_arc.clone()).await?;
    let (messages, thread, task_meta, source_goal, session_state, source_version) = {
        let session = session_arc.lock().await;
        (
            session.messages.clone(),
            session.thread.clone(),
            session.thread.task_meta.clone(),
            transition_source_goal(&session),
            session.runtime.state.clone(),
            session.trajectory_version,
        )
    };

    // Check session state - only block when actively streaming (generating)
    if matches!(session_state, SessionState::Generating) {
        return Err(ScratchError::new(
            StatusCode::CONFLICT,
            format!("Cannot transition chat while generating, please wait or abort first"),
        ));
    }

    if messages.is_empty() {
        return Err(ScratchError::new(
            StatusCode::BAD_REQUEST,
            "Cannot transition an empty chat".to_string(),
        ));
    }

    let rebuilt = reconstruct_context(
        gcx.clone(),
        ReconstructionRequest {
            messages: &messages,
            target_mode: &target_mode,
            target_mode_description: &req.target_mode_description,
            parent_chat_id: Some(chat_id.as_str()),
            model_override: None,
            abort_flag: Some(session_arc.lock().await.abort_flag.clone()),
            hints: None,
            target_budget_symbols: None,
            preserve_goal_messages: false,
        },
    )
    .await
    .map_err(|e| ScratchError::new(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    let mut new_messages = rebuilt.messages;
    let _decisions = rebuilt.decisions;

    let new_chat_id = Uuid::new_v4().to_string();
    let transferred_goal = transition_goal(
        &messages,
        source_goal.as_ref(),
        &chat_id,
        &new_chat_id,
        &target_mode,
    )
    .map_err(|e| ScratchError::new(StatusCode::BAD_REQUEST, e))?;
    if transferred_goal.transferred() {
        insert_goal_messages_before_plan(
            &mut new_messages,
            transferred_goal.target_messages.clone(),
        );
    }
    let new_messages = transition_report(
        &new_messages,
        source_version,
        rebuilt.model,
        thread.mode.clone(),
        target_mode.clone(),
    )
    .map_err(|e| ScratchError::new(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    let now = chrono::Utc::now().to_rfc3339();

    let root_chat_id = thread
        .root_chat_id
        .clone()
        .or_else(|| Some(chat_id.clone()));

    let mut snapshot = TrajectorySnapshot {
        pending_deliveries: Vec::new(),
        goal_verification_blocked_until_ms: None,
        goal: transferred_goal.target_goal.clone(),
        goal_ledger: transferred_goal
            .target_goal
            .as_ref()
            .map(|target| {
                refact_chat_api::seed_transferred_goal_ledger(target, &chat_id, epoch_ms_now())
            })
            .unwrap_or_default(),
        chat_id: new_chat_id.clone(),
        title: String::new(),
        model: thread.model.clone(),
        mode: target_mode.clone(),
        tool_use: thread.tool_use.clone(),
        messages: new_messages.clone(),
        created_at: now,
        boost_reasoning: thread.boost_reasoning.unwrap_or(false),
        checkpoints_enabled: thread.checkpoints_enabled,
        context_tokens_cap: thread.context_tokens_cap,
        auto_compression_cap: thread.auto_compression_cap,
        auto_compression_cap_pending: thread.auto_compression_cap.is_none(),
        include_project_info: thread.include_project_info,
        is_title_generated: false,
        auto_approve_editing_tools: thread.auto_approve_editing_tools,
        auto_approve_dangerous_commands: thread.auto_approve_dangerous_commands,
        autonomous_no_confirm: thread.autonomous_no_confirm,
        version: 1,
        task_meta,
        worktree: thread.worktree.clone(),
        parent_id: Some(chat_id.clone()),
        link_type: Some("mode_transition".to_string()),
        root_chat_id: root_chat_id.clone(),
        reasoning_effort: thread.reasoning_effort.clone(),
        thinking_budget: thread.thinking_budget,
        temperature: thread.temperature,
        frequency_penalty: thread.frequency_penalty,
        max_tokens: thread.max_tokens,
        parallel_tool_calls: thread.parallel_tool_calls,
        previous_response_id: None,
        active_skill: None,
        auto_enrichment_enabled: thread.auto_enrichment_enabled,
        buddy_meta: None,
        auto_compact_enabled: thread.auto_compact_enabled,
        frozen_request_prefix: thread.frozen_request_prefix.clone(),
        claude_code_identity: thread.claude_code_identity.clone(),
        reactive_compact_attempts: None,
        wake_up_at: None,
        waiting_for_card_ids: Vec::new(),
    };
    reset_transition_snapshot_identity(&mut snapshot);

    commit_transition_snapshot(
        gcx.clone(),
        session_arc,
        source_version,
        snapshot,
        &transferred_goal,
        Some(&reservation),
    )
    .await
    .map_err(|e| ScratchError::new(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    let response = ModeTransitionApplyResponse {
        new_chat_id,
        messages_count: new_messages.len(),
        root_chat_id,
        initial_plan_document: None,
        initial_plan_error: None,
    };

    let body = serde_json::to_vec(&response)
        .map_err(|e| ScratchError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "application/json")
        .body(Body::from(body))
        .map_err(|e| ScratchError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

pub async fn handle_planner_from_transition(
    State(app): State<AppState>,
    Path(task_id): Path<String>,
    body_bytes: hyper::body::Bytes,
) -> Result<Response<Body>, ScratchError> {
    let gcx = app.gcx.clone();
    let req: PlannerFromTransitionRequest = serde_json::from_slice(&body_bytes)
        .map_err(|e| ScratchError::new(StatusCode::BAD_REQUEST, format!("Invalid JSON: {}", e)))?;

    let target_mode = req
        .target_mode
        .clone()
        .map(|m| m.trim().to_string())
        .filter(|m| !m.is_empty())
        .unwrap_or_else(|| "task_planner".to_string());
    let target_mode = canonical_transition_mode(&target_mode);
    if target_mode != "task_planner" {
        return Err(ScratchError::new(
            StatusCode::BAD_REQUEST,
            "Only task_planner chats can be created here".to_string(),
        ));
    }

    // Verify the task exists before doing any work
    refact_runtime_api::task_storage::load_task_meta(gcx.clone(), &task_id)
        .await
        .map_err(|e| ScratchError::new(StatusCode::NOT_FOUND, e))?;

    let sessions = gcx.chat_sessions.clone();
    let session_arc = get_or_create_session_with_trajectory(
        refact_runtime_api::app_state_from_gcx(gcx.clone()).await.chat_ctx(),
        &sessions,
        &req.source_chat_id,
    )
    .await;

    let reservation = TransitionReservation::acquire(session_arc.clone()).await?;
    let (messages, thread, source_goal, session_state, source_version) = {
        let session = session_arc.lock().await;
        (
            session.messages.clone(),
            session.thread.clone(),
            transition_source_goal(&session),
            session.runtime.state.clone(),
            session.trajectory_version,
        )
    };

    if matches!(session_state, SessionState::Generating) {
        return Err(ScratchError::new(
            StatusCode::CONFLICT,
            "Cannot transition chat while generating, please wait or abort first".to_string(),
        ));
    }

    if messages.is_empty() {
        return Err(ScratchError::new(
            StatusCode::BAD_REQUEST,
            "Cannot transition an empty chat".to_string(),
        ));
    }

    let rebuilt = reconstruct_context(
        gcx.clone(),
        ReconstructionRequest {
            messages: &messages,
            target_mode: &target_mode,
            target_mode_description: &req.target_mode_description,
            parent_chat_id: Some(req.source_chat_id.as_str()),
            model_override: None,
            abort_flag: Some(session_arc.lock().await.abort_flag.clone()),
            hints: None,
            target_budget_symbols: None,
            preserve_goal_messages: false,
        },
    )
    .await
    .map_err(|e| ScratchError::new(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    let mut new_messages = rebuilt.messages;
    let decisions = rebuilt.decisions;

    let new_chat_id = refact_runtime_api::task_storage::next_planner_chat_id(gcx.clone(), &task_id)
        .await
        .map_err(|e| ScratchError::new(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    let transferred_goal = transition_goal(
        &messages,
        source_goal.as_ref(),
        &req.source_chat_id,
        &new_chat_id,
        &target_mode,
    )
    .map_err(|e| ScratchError::new(StatusCode::BAD_REQUEST, e))?;
    if transferred_goal.transferred() {
        insert_goal_messages_before_plan(
            &mut new_messages,
            transferred_goal.target_messages.clone(),
        );
    }
    let new_messages = transition_report(
        &new_messages,
        source_version,
        rebuilt.model,
        thread.mode.clone(),
        target_mode.clone(),
    )
    .map_err(|e| ScratchError::new(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    let now = chrono::Utc::now().to_rfc3339();

    let task_meta = refact_chat_api::TaskMeta {
        task_id: task_id.clone(),
        role: "planner".to_string(),
        agent_id: None,
        card_id: None,
        planner_chat_id: Some(new_chat_id.clone()),
    };

    let root_chat_id = Some(new_chat_id.clone());

    let mut snapshot = TrajectorySnapshot {
        pending_deliveries: Vec::new(),
        goal_verification_blocked_until_ms: None,
        goal: transferred_goal.target_goal.clone(),
        goal_ledger: transferred_goal
            .target_goal
            .as_ref()
            .map(|target| {
                refact_chat_api::seed_transferred_goal_ledger(
                    target,
                    &req.source_chat_id,
                    epoch_ms_now(),
                )
            })
            .unwrap_or_default(),
        chat_id: new_chat_id.clone(),
        title: String::new(),
        model: thread.model.clone(),
        mode: target_mode.clone(),
        tool_use: thread.tool_use.clone(),
        messages: new_messages.clone(),
        created_at: now,
        boost_reasoning: thread.boost_reasoning.unwrap_or(false),
        checkpoints_enabled: thread.checkpoints_enabled,
        context_tokens_cap: thread.context_tokens_cap,
        auto_compression_cap: thread.auto_compression_cap,
        auto_compression_cap_pending: thread.auto_compression_cap.is_none(),
        include_project_info: thread.include_project_info,
        is_title_generated: false,
        auto_approve_editing_tools: thread.auto_approve_editing_tools,
        auto_approve_dangerous_commands: thread.auto_approve_dangerous_commands,
        autonomous_no_confirm: thread.autonomous_no_confirm,
        version: 1,
        task_meta: Some(task_meta),
        worktree: thread.worktree.clone(),
        parent_id: Some(req.source_chat_id.clone()),
        link_type: Some("mode_transition".to_string()),
        root_chat_id: root_chat_id.clone(),
        reasoning_effort: thread.reasoning_effort.clone(),
        thinking_budget: thread.thinking_budget,
        temperature: thread.temperature,
        frequency_penalty: thread.frequency_penalty,
        max_tokens: thread.max_tokens,
        parallel_tool_calls: thread.parallel_tool_calls,
        previous_response_id: None,
        active_skill: None,
        auto_enrichment_enabled: thread.auto_enrichment_enabled,
        buddy_meta: None,
        auto_compact_enabled: thread.auto_compact_enabled,
        frozen_request_prefix: thread.frozen_request_prefix.clone(),
        claude_code_identity: thread.claude_code_identity.clone(),
        reactive_compact_attempts: None,
        wake_up_at: None,
        waiting_for_card_ids: Vec::new(),
    };
    reset_transition_snapshot_identity(&mut snapshot);

    // task_meta is set, so this saves into the task's planner directory
    commit_transition_snapshot(
        gcx.clone(),
        session_arc,
        source_version,
        snapshot,
        &transferred_goal,
        Some(&reservation),
    )
    .await
    .map_err(|e| ScratchError::new(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    let (initial_plan_document, initial_plan_error) =
        if target_mode.eq_ignore_ascii_case("task_planner") {
            create_initial_plan_document_for_transition(
                gcx.clone(),
                &task_id,
                decisions.initial_plan.as_deref(),
            )
            .await
        } else {
            (None, None)
        };

    let response = ModeTransitionApplyResponse {
        new_chat_id,
        messages_count: new_messages.len(),
        root_chat_id,
        initial_plan_document,
        initial_plan_error,
    };

    let body = serde_json::to_vec(&response)
        .map_err(|e| ScratchError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "application/json")
        .body(Body::from(body))
        .map_err(|e| ScratchError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}
