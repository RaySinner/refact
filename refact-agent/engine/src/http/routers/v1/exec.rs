use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::app_state::AppState;
use crate::custom_error::ScratchError;
use crate::exec::{ExecProcessId, ExecStatus};

#[derive(Debug, Deserialize)]
pub struct ExecStdinRequest {
    pub chars: String,
}

#[derive(Debug, Serialize)]
pub struct ExecStdinResponse {
    pub process_id: String,
    pub status: &'static str,
    pub bytes_written: usize,
    pub since_seq: u64,
    pub next_seq: u64,
    pub latest_seq: u64,
}

pub async fn handle_v1_exec_stdin(
    State(app): State<AppState>,
    Path(process_id): Path<String>,
    Json(request): Json<ExecStdinRequest>,
) -> Result<Json<ExecStdinResponse>, ScratchError> {
    let process_id = ExecProcessId(process_id);
    let result = app
        .runtime
        .exec_registry
        .write_stdin(&process_id, &request.chars, 0)
        .await
        .map_err(|message| ScratchError::new(StatusCode::BAD_REQUEST, message))?;
    let snapshot = app
        .runtime
        .exec_registry
        .get(&process_id)
        .await
        .ok_or_else(|| ScratchError::new(StatusCode::NOT_FOUND, "process not found".to_string()))?;
    Ok(Json(ExecStdinResponse {
        process_id: process_id.as_str().to_string(),
        status: status_label(&snapshot.status),
        bytes_written: result.bytes_written,
        since_seq: result.read.since_seq,
        next_seq: result.read.next_seq,
        latest_seq: result.read.latest_seq,
    }))
}

fn status_label(status: &ExecStatus) -> &'static str {
    match status {
        ExecStatus::Starting => "starting",
        ExecStatus::Running => "running",
        ExecStatus::Exited { .. } => "exited",
        ExecStatus::Failed { .. } => "failed",
        ExecStatus::Killed => "killed",
        ExecStatus::TimedOut => "timed_out",
    }
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::Request;
    use hyper::body::to_bytes;
    use serde_json::Value;
    use tower::ServiceExt;

    use crate::app_state::AppState;
    use crate::exec::ExecSpawnRequest;
    use crate::http::routers::make_refact_http_server;

    #[cfg(unix)]
    #[tokio::test]
    async fn write_stdin_to_running_tty_process_succeeds() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let app_state = AppState::from_gcx(gcx).await;
        let spawn = app_state
            .runtime
            .exec_registry
            .spawn(
                ExecSpawnRequest::background("read line; printf 'got:%s\\n' \"$line\"")
                    .with_tty(true),
            )
            .await
            .unwrap();
        let process_id = spawn.snapshot.meta.process_id.as_str().to_string();
        let router = make_refact_http_server(app_state.clone());

        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/v1/exec/{process_id}/stdin"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({ "chars": "ribbit\n" }).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), axum::http::StatusCode::OK);
        let body = to_bytes(response.into_body()).await.unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["process_id"], process_id);
        assert_eq!(json["bytes_written"], "ribbit\n".len());

        let _ = app_state
            .runtime
            .exec_registry
            .wait(&spawn.snapshot.meta.process_id)
            .await
            .unwrap();
        let read = app_state
            .runtime
            .exec_registry
            .read(&spawn.snapshot.meta.process_id, 0, None)
            .await;
        assert!(read.chunks.iter().any(|chunk| chunk.text.contains("got:")));
    }
}
