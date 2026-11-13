use std::collections::HashSet;
use axum::http::Response;
use axum::extract::State;
use hyper::{Body, StatusCode};
use serde::Deserialize;
use refact_runtime_api::app_state::AppState;
use refact_runtime_api::at_command_executor::{
    file_repair_candidates, return_one_candidate_or_a_good_error,
};
use refact_core::custom_error::ScratchError;
use refact_files::path_utils::preprocess_path_for_normalization;
use refact_runtime_api::files_correction::correct_to_nearest_dir_path;

#[derive(Deserialize)]
struct ResolveShortenedPathPost {
    path: String,
}

pub async fn handle_v1_fullpath(
    State(app): State<AppState>,
    body_bytes: hyper::body::Bytes,
) -> axum::response::Result<Response<Body>, ScratchError> {
    let gcx = app.gcx.clone();
    let post = serde_json::from_slice::<ResolveShortenedPathPost>(&body_bytes).map_err(|e| {
        ScratchError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("JSON problem: {}", e),
        )
    })?;

    let path = preprocess_path_for_normalization(post.path);
    let candidates_file = file_repair_candidates(&app.tool, &path, 10, false).await;
    let candidates_dir = correct_to_nearest_dir_path(gcx.clone(), &path, false, 10).await;
    let candidates = candidates_file
        .into_iter()
        .chain(candidates_dir.clone().into_iter())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();

    match return_one_candidate_or_a_good_error(&app.tool, &path, &candidates, &vec![], false).await
    {
        Ok(candidate) => {
            let is_directory = candidates_dir.contains(&candidate);
            Ok(Response::builder()
                .status(StatusCode::OK)
                .header("Content-Type", "application/json")
                .body(Body::from(
                    serde_json::to_string_pretty(&serde_json::json!({
                        "fullpath": candidate,
                        "is_directory": is_directory
                    }))
                    .unwrap(),
                ))
                .unwrap())
        }
        Err(err) => Ok(Response::builder()
            .status(StatusCode::BAD_REQUEST)
            .header("Content-Type", "application/json")
            .body(Body::from(
                serde_json::to_string_pretty(&serde_json::json!({ "detail": err })).unwrap(),
            ))
            .unwrap()),
    }
}
