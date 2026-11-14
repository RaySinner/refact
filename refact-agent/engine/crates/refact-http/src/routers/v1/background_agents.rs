use axum::extract::{Path, Query, State};
use axum::response::Json;
use hyper::StatusCode;
use serde::Deserialize;
use serde_json::{json, Value};

use refact_core::chat_types::{PendingDelivery, PushMode};
use refact_agents_core::types::{BackgroundAgent, BackgroundAgentSummary};
use refact_runtime_api::AppState;

type ApiResult<T> = Result<Json<T>, (StatusCode, Json<Value>)>;

#[derive(Debug, Deserialize)]
pub struct BackgroundAgentsQuery {
    pub chat_id: String,
}

#[derive(Debug, Deserialize)]
pub struct CancelRequest {
    #[serde(default)]
    pub chat_id: Option<String>,
    #[serde(default = "default_subtree")]
    pub subtree: bool,
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct MessageRequest {
    #[serde(default)]
    pub chat_id: Option<String>,
    pub text: String,
    #[serde(default)]
    pub push: PushMode,
}

fn default_subtree() -> bool {
    true
}

fn api_error(status: StatusCode, error: impl Into<String>) -> (StatusCode, Json<Value>) {
    (
        status,
        Json(json!({
            "error": error.into(),
            "code": match status {
                StatusCode::NOT_FOUND => "not_found",
                StatusCode::CONFLICT => "conflict",
                StatusCode::BAD_REQUEST => "bad_request",
                _ => "background_agent_error",
            },
        })),
    )
}

fn belongs_to_chat(record: &BackgroundAgent, chat_id: &str) -> bool {
    record.parent_chat_id == chat_id || record.parent_root_chat_id.as_deref() == Some(chat_id)
}

fn verify_optional_chat_ownership(
    record: &BackgroundAgent,
    chat_id: Option<&str>,
) -> Result<(), (StatusCode, Json<Value>)> {
    let Some(chat_id) = chat_id.filter(|chat_id| !chat_id.trim().is_empty()) else {
        // TODO: require chat_id after GUI T-15 sends it for every request.
        tracing::warn!(agent_id = %record.agent_id, "background agent request omitted chat_id");
        return Ok(());
    };
    if belongs_to_chat(record, chat_id) {
        Ok(())
    } else {
        Err(api_error(
            StatusCode::FORBIDDEN,
            "Background agent does not belong to this chat",
        ))
    }
}

async fn root_chat_id(app: &AppState, chat_id: &str) -> String {
    app.agents
        .list_all()
        .await
        .into_iter()
        .find(|record| record.child_chat_id.as_deref() == Some(chat_id))
        .and_then(|record| record.parent_root_chat_id)
        .unwrap_or_else(|| chat_id.to_string())
}

fn summary(record: &BackgroundAgent) -> BackgroundAgentSummary {
    BackgroundAgentSummary::from(record)
}

pub async fn handle_v1_background_agents_get(
    State(app): State<AppState>,
    Query(query): Query<BackgroundAgentsQuery>,
) -> Json<Vec<BackgroundAgentSummary>> {
    let root_chat_id = root_chat_id(&app, &query.chat_id).await;
    let records = refact_tools::tools::tool_agent_interact::records_for_root(&app, &root_chat_id).await;
    Json(
        records
            .into_iter()
            .map(|(record, _)| summary(&record))
            .collect(),
    )
}

pub async fn handle_v1_background_agents_cancel(
    State(app): State<AppState>,
    Path(agent_id): Path<String>,
    Json(request): Json<CancelRequest>,
) -> ApiResult<Value> {
    let record = app
        .agents
        .get_any(&agent_id)
        .await
        .map_err(|_| api_error(StatusCode::NOT_FOUND, "Background agent not found"))?;
    verify_optional_chat_ownership(&record, request.chat_id.as_deref())?;
    let cancelled = app
        .agents
        .cancel_subtree(
            &record.parent_chat_id,
            &agent_id,
            request.subtree,
            request.reason,
        )
        .await
        .map_err(|error| api_error(StatusCode::CONFLICT, error))?;
    for record in &cancelled {
        refact_agents_core::spawn::emit_background_agent_update(app.clone(), record).await;
    }
    let cancelled_agent_ids = cancelled
        .iter()
        .map(|record| record.agent_id.clone())
        .collect::<Vec<_>>();
    let agents = cancelled.iter().map(summary).collect::<Vec<_>>();
    Ok(Json(
        json!({ "cancelled_agent_ids": cancelled_agent_ids, "agents": agents }),
    ))
}

pub async fn handle_v1_background_agents_message(
    State(app): State<AppState>,
    Path(agent_id): Path<String>,
    Json(request): Json<MessageRequest>,
) -> ApiResult<Value> {
    let text = request.text.trim();
    if text.is_empty() {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "Message text must not be empty",
        ));
    }
    let record = app
        .agents
        .get_any(&agent_id)
        .await
        .map_err(|_| api_error(StatusCode::NOT_FOUND, "Background agent not found"))?;
    verify_optional_chat_ownership(&record, request.chat_id.as_deref())?;
    if record.status.is_terminal() {
        return Err(api_error(
            StatusCode::CONFLICT,
            "Background agent is not running",
        ));
    }
    let outcome = refact_agents_core::delivery::deliver_to_agent(
        app.clone(),
        &agent_id,
        PendingDelivery::new(
            vec![refact_chat_api::internal_roles::event(
                refact_chat_api::internal_roles::EventSubkind::SystemNotice,
                "agents.message",
                json!({"from": "user"}),
                format!("[message from user]\n{text}"),
            )],
            request.push,
            "agents.message".to_string(),
            true,
        ),
    )
    .await
    .map_err(|error| api_error(StatusCode::CONFLICT, error))?;
    refact_agents_core::spawn::emit_background_agent_update(app, &record).await;
    Ok(Json(
        json!({ "agent": summary(&record), "queued": true, "outcome": outcome }),
    ))
}
