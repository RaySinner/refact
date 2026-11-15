use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use refact_runtime_api::app_state::AppState;
use refact_runtime_api::session_runtime::get_or_create_session_with_trajectory;
use refact_chat_api::internal_roles::{event, EventSubkind};
use refact_core::custom_error::ScratchError;
use refact_daemon::daemon::auth::{bearer_token_from_headers, token_matches};
use refact_runtime_api::scheduler::active_durable_cron_store;
use refact_runtime_api::scheduler_runner::{session_cron_store, CronRunner};
use refact_scheduler_core::store::CronStore;
use refact_scheduler_core::types::{
    delivery_from_value, push_from_value, Action, AgentTarget, Delivery, Job, Trigger,
};
use refact_core::chat_types::{PendingDelivery, PushMode};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HookFireKind {
    Wake,
    Agent,
}

#[derive(Clone, Debug, Deserialize)]
pub struct HookFireRequest {
    pub kind: HookFireKind,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub hook_id: Option<String>,
    #[serde(default)]
    pub deliver: Option<Value>,
    #[serde(default)]
    pub push: Option<Value>,
}

#[derive(Debug, Serialize)]
pub struct HookFireResponse {
    pub inline: Option<InlineFireResponse>,
    pub hook_jobs: HookJobsFireResponse,
}

#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InlineFireResponse {
    Wake { chat_id: String },
    Agent { job_id: String, fired: bool },
}

#[derive(Debug, Serialize)]
pub struct HookJobsFireResponse {
    pub hook_id: Option<String>,
    pub matched: usize,
    pub fired: usize,
    pub jobs: Vec<HookJobFireResult>,
}

#[derive(Debug, Serialize)]
pub struct HookJobFireResult {
    pub id: String,
    pub fired: bool,
}

pub async fn handle_v1_hooks_fire(
    State(app): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<HookFireRequest>,
) -> Result<Json<HookFireResponse>, ScratchError> {
    authorize_worker_hook(&app, &headers)?;
    let now_ms = unix_now_ms();
    let hook_id = normalized(request.hook_id.as_deref());
    let has_inline = request_has_inline(&request);
    if !has_inline && hook_id.is_none() {
        return Err(ScratchError::new(
            StatusCode::BAD_REQUEST,
            "message or hook_id is required".to_string(),
        ));
    }
    let hook_jobs = match hook_id {
        Some(hook_id) => {
            let hook_jobs = fire_hook_jobs(&app, &hook_id, now_ms).await?;
            validate_hook_jobs_result(&hook_jobs, has_inline)?;
            hook_jobs
        }
        None => HookJobsFireResponse {
            hook_id: None,
            matched: 0,
            fired: 0,
            jobs: Vec::new(),
        },
    };
    let inline = fire_inline(&app, &request, now_ms).await?;
    Ok(Json(HookFireResponse { inline, hook_jobs }))
}

fn authorize_worker_hook(app: &AppState, headers: &HeaderMap) -> Result<(), ScratchError> {
    let Some(expected) = app.gcx.cmdline.daemon_auth_token.as_deref() else {
        return Ok(());
    };
    let actual = bearer_token_from_headers(headers);
    if actual
        .map(|actual| token_matches(actual, expected))
        .unwrap_or(false)
    {
        return Ok(());
    }
    Err(ScratchError::new(
        StatusCode::UNAUTHORIZED,
        "daemon authorization required".to_string(),
    ))
}

async fn fire_inline(
    app: &AppState,
    request: &HookFireRequest,
    now_ms: u64,
) -> Result<Option<InlineFireResponse>, ScratchError> {
    match request.kind {
        HookFireKind::Wake => {
            let Some(text) = request_text(request) else {
                return Ok(None);
            };
            let push = push_from_value(request.push.as_ref())
                .map_err(|error| ScratchError::new(StatusCode::BAD_REQUEST, error))?
                .unwrap_or_default();
            let chat_id = inject_wake(app, &text, push)
                .await
                .map_err(|error| ScratchError::new(StatusCode::INTERNAL_SERVER_ERROR, error))?;
            Ok(Some(InlineFireResponse::Wake { chat_id }))
        }
        HookFireKind::Agent => {
            let Some(message) = request_message(request) else {
                return Ok(None);
            };
            let delivery = request
                .deliver
                .as_ref()
                .map(delivery_from_value)
                .transpose()
                .map_err(|error| ScratchError::new(StatusCode::BAD_REQUEST, error))?
                .unwrap_or(Delivery::Chat);
            if !matches!(delivery, Delivery::Chat) {
                return Err(ScratchError::new(
                    StatusCode::BAD_REQUEST,
                    "agent hook delivery must be chat".to_string(),
                ));
            }
            let push = push_from_value(request.push.as_ref())
                .map_err(|error| ScratchError::new(StatusCode::BAD_REQUEST, error))?
                .unwrap_or_default();
            let mut job = inline_agent_job(
                message,
                request
                    .mode
                    .clone()
                    .and_then(|mode| normalized(Some(&mode))),
                request
                    .model
                    .clone()
                    .and_then(|model| normalized(Some(&model))),
                now_ms,
            );
            job.push = push;
            let job_id = job.id.clone();
            let fired = CronRunner::fire_manual_job(app.gcx.clone(), job, now_ms)
                .await
                .map_err(|error| ScratchError::new(StatusCode::INTERNAL_SERVER_ERROR, error))?;
            Ok(Some(InlineFireResponse::Agent { job_id, fired }))
        }
    }
}

/// A wake hook is a pure notice: route it through the shared delivery layer so a
/// busy chat queues it at the next safe boundary instead of racing the history.
async fn inject_wake(app: &AppState, text: &str, push: PushMode) -> Result<String, String> {
    let chat_id = wake_chat_id(app).await;
    let _ = get_or_create_session_with_trajectory(app.chat_ctx(), &app.chat.sessions, &chat_id).await;
    let notice = event(
        EventSubkind::SystemNotice,
        "daemon.hooks",
        json!({
            "kind": "wake",
        }),
        text.to_string(),
    );
    refact_chat_api::delivery::deliver_to_chat(
        app.clone(),
        &chat_id,
        PendingDelivery::new(vec![notice], push, "daemon.hooks".to_string(), false),
    )
    .await?;
    Ok(chat_id)
}

async fn wake_chat_id(app: &AppState) -> String {
    let sessions = app.chat.sessions.read().await;
    let mut ids = sessions.keys().cloned().collect::<Vec<_>>();
    ids.sort();
    ids.into_iter().next().unwrap_or_else(|| {
        let project_id = app.gcx.cmdline.project_id.trim();
        if project_id.is_empty() {
            "project-main".to_string()
        } else {
            format!("project-{project_id}-main")
        }
    })
}

fn inline_agent_job(
    message: String,
    mode: Option<String>,
    model: Option<String>,
    now_ms: u64,
) -> Job {
    let mut job = Job::new_cron_agent_chat(
        String::new(),
        message,
        "Inbound hook agent".to_string(),
        false,
        false,
        now_ms,
    );
    job.id = format!("hook_inline_{}", Uuid::now_v7());
    job.trigger = Trigger::Manual;
    job.action = Action::AgentTurn {
        prompt: job.prompt().unwrap_or_default().to_string(),
        target: AgentTarget::Isolated,
        mode,
        model,
        tools: None,
    };
    job.auto_expire_after_ms = 0;
    job
}

async fn fire_hook_jobs(
    app: &AppState,
    hook_id: &str,
    now_ms: u64,
) -> Result<HookJobsFireResponse, ScratchError> {
    let mut matches = Vec::<(std::sync::Arc<dyn CronStore>, Job)>::new();
    let session_store = session_cron_store();
    matches.extend(
        session_store
            .jobs_by_hook_id(hook_id)
            .await
            .into_iter()
            .map(|job| (session_store.clone(), job)),
    );
    let durable = active_durable_cron_store(app.gcx.clone())
        .await
        .map_err(|error| ScratchError::new(StatusCode::INTERNAL_SERVER_ERROR, error))?;
    if let Some(store) = durable {
        matches.extend(
            store
                .jobs_by_hook_id(hook_id)
                .await
                .into_iter()
                .map(|job| (store.clone(), job)),
        );
    }

    let matched = matches.len();
    let mut jobs = Vec::with_capacity(matched);
    for (store, job) in matches {
        let fired = CronRunner::fire_store_job_now(store, app.gcx.clone(), &job.id, now_ms)
            .await
            .map_err(|error| ScratchError::new(StatusCode::INTERNAL_SERVER_ERROR, error))?;
        jobs.push(HookJobFireResult { id: job.id, fired });
    }
    let fired = jobs.iter().filter(|job| job.fired).count();
    Ok(HookJobsFireResponse {
        hook_id: Some(hook_id.to_string()),
        matched,
        fired,
        jobs,
    })
}

fn validate_hook_jobs_result(
    hook_jobs: &HookJobsFireResponse,
    has_inline: bool,
) -> Result<(), ScratchError> {
    let hook_id = hook_jobs.hook_id.as_deref().unwrap_or_default();
    if hook_jobs.matched == 0 && !has_inline {
        return Err(ScratchError::new(
            StatusCode::NOT_FOUND,
            format!("hook not found: {hook_id}"),
        ));
    }
    if hook_jobs.matched > 0 && hook_jobs.fired == 0 {
        return Err(ScratchError::new(
            StatusCode::FAILED_DEPENDENCY,
            format!(
                "hook {hook_id} matched {} job(s) but none fired",
                hook_jobs.matched
            ),
        ));
    }
    Ok(())
}

fn request_has_inline(request: &HookFireRequest) -> bool {
    match request.kind {
        HookFireKind::Wake => request_text(request).is_some(),
        HookFireKind::Agent => request_message(request).is_some(),
    }
}

fn request_text(request: &HookFireRequest) -> Option<String> {
    normalized(request.text.as_deref()).or_else(|| normalized(request.message.as_deref()))
}

fn request_message(request: &HookFireRequest) -> Option<String> {
    normalized(request.message.as_deref()).or_else(|| normalized(request.text.as_deref()))
}

fn normalized(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

pub fn unix_now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

