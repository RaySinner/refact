use axum::response::Result;
use axum::extract::State;
use hyper::{Body, Response, StatusCode};
use serde::{Deserialize, Serialize};

use refact_runtime_api::AppState;
use refact_core::custom_error::ScratchError;

#[derive(Serialize, Deserialize, Clone)]
struct VecDBPost {
    query: String,
    top_n: usize,
}

#[derive(Serialize)]
struct CodeGraphSearchResult {
    query_text: String,
    results: Vec<refact_codegraph::CodeHit>,
}

const NO_VECDB: &str = "Vector db is not running, check if you have --vecdb parameter and a vectorization model is running on server side.";
const NO_CODEGRAPH: &str =
    "Codegraph is not running, check if project indexing is enabled and finished.";

fn json_response<T: Serialize>(value: &T) -> Result<Response<Body>, ScratchError> {
    let json_string = serde_json::to_string_pretty(value).map_err(|e| {
        ScratchError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("JSON serialization problem: {}", e),
        )
    })?;
    Ok(Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "application/json")
        .body(Body::from(json_string))
        .unwrap())
}

pub async fn handle_v1_vecdb_search(
    State(app): State<AppState>,
    body_bytes: hyper::body::Bytes,
) -> Result<Response<Body>, ScratchError> {
    let gcx = app.gcx.clone();
    let post = serde_json::from_slice::<VecDBPost>(&body_bytes)
        .map_err(|e| ScratchError::new(StatusCode::BAD_REQUEST, format!("JSON problem: {}", e)))?;

    let db = match gcx.vec_db.lock().await.clone() {
        Some(db) => db,
        None => {
            return Err(ScratchError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                NO_VECDB.to_string(),
            ));
        }
    };
    let search_res = db
        .vecdb_search(post.query.to_string(), post.top_n, None)
        .await;

    match search_res {
        Ok(search_res) => json_response(&search_res),
        Err(e) => Err(ScratchError::new(StatusCode::BAD_REQUEST, e)),
    }
}

pub async fn handle_v1_codegraph_search(
    State(app): State<AppState>,
    body_bytes: hyper::body::Bytes,
) -> Result<Response<Body>, ScratchError> {
    let post = serde_json::from_slice::<VecDBPost>(&body_bytes)
        .map_err(|e| ScratchError::new(StatusCode::BAD_REQUEST, format!("JSON problem: {}", e)))?;

    let service = match app.gcx.codegraph.lock().await.clone() {
        Some(service) => service,
        None => {
            return Err(ScratchError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                NO_CODEGRAPH.to_string(),
            ));
        }
    };
    let hits = service
        .search_hybrid(&post.query, post.top_n)
        .await
        .map_err(|e| ScratchError::new(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    json_response(&CodeGraphSearchResult {
        query_text: post.query,
        results: hits,
    })
}

pub async fn handle_v1_vecdb_status(
    State(app): State<AppState>,
    _: hyper::body::Bytes,
) -> Result<Response<Body>, ScratchError> {
    let gcx = app.gcx.clone();
    let vec_db = gcx.vec_db.clone();
    let status_str = match refact_vecdb::vdb_highlev::get_status(vec_db).await {
        Ok(Some(status)) => serde_json::to_string_pretty(&status).unwrap(),
        Ok(None) => "{\"success\": 0, \"detail\": \"turned_off\"}".to_string(),
        Err(err) => {
            return Err(ScratchError::new(StatusCode::INTERNAL_SERVER_ERROR, err));
        }
    };
    Ok(Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "application/json")
        .body(Body::from(status_str))
        .unwrap())
}
