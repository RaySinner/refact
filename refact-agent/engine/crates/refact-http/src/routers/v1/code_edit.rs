use refact_runtime_api::app_state::AppState;
use refact_runtime_api::generate_code_edit_runtime::generate_code_edit;
use refact_core::custom_error::ScratchError;
use axum::http::{Response, StatusCode};
use axum::extract::State;
use hyper::Body;
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
pub struct CodeEditPost {
    pub code: String,
    pub instruction: String,
    pub cursor_file: String,
    pub cursor_line: i32,
}

#[derive(Serialize)]
pub struct CodeEditResponse {
    pub edited_code: String,
}

pub async fn handle_v1_code_edit(
    State(app): State<AppState>,
    body_bytes: hyper::body::Bytes,
) -> axum::response::Result<Response<Body>, ScratchError> {
    let post = serde_json::from_slice::<CodeEditPost>(&body_bytes).map_err(|e| {
        ScratchError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("JSON problem: {}", e),
        )
    })?;

    let edited_code = generate_code_edit(
        app.chat_ctx(),
        &post.code,
        &post.instruction,
        &post.cursor_file,
        post.cursor_line,
    )
    .await
    .map_err(|e| ScratchError::new(StatusCode::UNPROCESSABLE_ENTITY, e))?;

    Ok(Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "application/json")
        .body(Body::from(
            serde_json::to_string(&CodeEditResponse { edited_code }).unwrap(),
        ))
        .unwrap())
}
