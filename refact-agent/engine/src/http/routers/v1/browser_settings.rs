use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::extract::State;
use axum::Json;
use hyper::StatusCode;
use tokio::io::AsyncWriteExt;

use crate::app_state::AppState;
use crate::custom_error::ScratchError;

pub use refact_browser::settings::*;

fn bad_request(message: impl Into<String>) -> ScratchError {
    ScratchError::new(StatusCode::BAD_REQUEST, message.into())
}

fn server_error(message: impl Into<String>) -> ScratchError {
    ScratchError::new(StatusCode::INTERNAL_SERVER_ERROR, message.into())
}

async fn atomic_write(path: &Path, content: &str) -> Result<(), ScratchError> {
    let parent = path
        .parent()
        .ok_or_else(|| server_error(format!("{} has no parent directory", path.display())))?;
    tokio::fs::create_dir_all(parent)
        .await
        .map_err(|e| server_error(format!("cannot create {}: {}", parent.display(), e)))?;
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let tmp = parent.join(format!(".{BROWSER_SETTINGS_FILE}.{nonce}.tmp"));
    let mut file = tokio::fs::File::create(&tmp)
        .await
        .map_err(|e| server_error(format!("cannot create {}: {}", tmp.display(), e)))?;
    file.write_all(content.as_bytes())
        .await
        .map_err(|e| server_error(format!("cannot write {}: {}", tmp.display(), e)))?;
    file.flush()
        .await
        .map_err(|e| server_error(format!("cannot flush {}: {}", tmp.display(), e)))?;
    drop(file);
    tokio::fs::rename(&tmp, path).await.map_err(|e| {
        server_error(format!(
            "cannot move {} to {}: {}",
            tmp.display(),
            path.display(),
            e
        ))
    })
}

pub async fn handle_v1_browser_settings_get(
    State(app): State<AppState>,
) -> Result<Json<BrowserSettings>, ScratchError> {
    let config_dir = app.gcx.config_dir.clone();
    let settings = refresh_from_disk(&config_dir).await;
    Ok(Json((*settings).clone()))
}

pub async fn handle_v1_browser_settings_post(
    State(app): State<AppState>,
    body: hyper::body::Bytes,
) -> Result<Json<BrowserSettings>, ScratchError> {
    let settings = serde_json::from_slice::<BrowserSettings>(&body).map_err(|e| {
        ScratchError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("invalid browser settings payload: {}", e),
        )
    })?;
    settings.validate().map_err(bad_request)?;
    let config_dir = app.gcx.config_dir.clone();
    let path = settings_path(&config_dir);
    let yaml = serde_yaml::to_string(&settings)
        .map_err(|e| server_error(format!("cannot serialize browser settings: {}", e)))?;
    atomic_write(&path, &yaml).await?;
    publish(settings.clone());
    Ok(Json(settings))
}
