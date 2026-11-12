use std::sync::Arc;

use axum::http::StatusCode;
use refact_caps_core::code_assistant_caps::CodeAssistantCaps;
use refact_caps_core::providers::get_latest_provider_mtime;
use refact_core::custom_error::ScratchError;
use tracing::error;

use crate::global_context::SharedGlobalContext;

const CAPS_RELOAD_BACKOFF: u64 = 60; // seconds
const CAPS_BACKGROUND_RELOAD: u64 = 3600; // seconds

pub async fn try_load_caps_quickly_if_not_present(
    gcx: SharedGlobalContext,
    max_age_seconds: u64,
) -> Result<Arc<CodeAssistantCaps>, ScratchError> {
    let cmdline = gcx.cmdline.clone();
    let (caps_state, config_dir) = { (gcx.caps_state.clone(), gcx.config_dir.clone()) };
    let caps_reading_lock = caps_state.read().await.reading_lock.clone();
    let caps_loader = gcx.context_factory.get().caps_loader();

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let caps_last_attempted_ts;
    let latest_provider_mtime = get_latest_provider_mtime(&config_dir).await.unwrap_or(0);
    let (project_defaults_root, project_defaults_mtime) =
        caps_loader.project_model_defaults_state(gcx.clone()).await;
    let latest_provider_mtime = latest_provider_mtime.max(project_defaults_mtime);

    {
        // gcx is not locked, but a specialized async mutex is, up until caps are saved
        let _caps_reading_locked = caps_reading_lock.lock().await;

        let max_age = if max_age_seconds > 0 {
            max_age_seconds
        } else {
            CAPS_BACKGROUND_RELOAD
        };
        {
            let mut caps_state = caps_state.write().await;
            if caps_state.last_attempted_ts + max_age < now
                || latest_provider_mtime >= caps_state.last_attempted_ts
                || caps_state.project_root_changed(&project_defaults_root)
            {
                caps_state.caps = None;
                caps_state.last_attempted_ts = 0;
                gcx.tool_catalog_generations.advance_capabilities();
                caps_last_attempted_ts = 0;
            } else {
                if let Some(caps_arc) = caps_state.caps.clone() {
                    return Ok(caps_arc.clone());
                }
                caps_last_attempted_ts = caps_state.last_attempted_ts;
            }
        }
        if caps_last_attempted_ts + CAPS_RELOAD_BACKOFF > now {
            let caps_state = caps_state.read().await;
            return Err(ScratchError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                caps_state.last_error.clone(),
            ));
        }

        let caps_result = caps_loader.load_caps(cmdline, gcx.clone()).await;

        {
            let mut caps_state = caps_state.write().await;
            caps_state.last_attempted_ts = now;
            match caps_result {
                Ok(caps) => {
                    caps_state.caps = Some(caps.clone());
                    caps_state.loaded_project_root = Some(project_defaults_root);
                    caps_state.last_error = "".to_string();
                    gcx.tool_catalog_generations.advance_capabilities();
                    Ok(caps)
                }
                Err(e) => {
                    error!("caps fetch failed: {:?}", e);
                    caps_state.last_error = format!("caps fetch failed: {}", e);
                    Err(ScratchError::new(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        caps_state.last_error.clone(),
                    ))
                }
            }
        }
    }
}
