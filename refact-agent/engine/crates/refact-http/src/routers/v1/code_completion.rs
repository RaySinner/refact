use std::sync::Arc;
use tokio::sync::Mutex as AMutex;

use axum::extract::State;
use axum::response::Result;
use hyper::{Body, Response, StatusCode};
use tracing::info;
use refact_core::call_validation::{code_completion_post_validate, CodeCompletionPost};
use refact_runtime_api::caps_load_runtime::{resolve_completion_model, CompletionModelRecord};
use refact_scratchpads::completion_cache;
use refact_runtime_api::app_state::AppState;
use refact_core::custom_error::ScratchError;
use refact_runtime_api::privacy::load_privacy_if_needed;
use refact_files::path_utils::canonical_path;
use refact_runtime_api::global_context::GlobalContext;
use refact_runtime_api::scratchpads_runtime;
use refact_scratchpads::scratchpad_abstract::ScratchpadPromptInput;

const CODE_COMPLETION_TOP_N: usize = 5;

async fn resolve_completion_model_for_path(
    gcx: Arc<GlobalContext>,
    requested_model: &str,
    cursor_file: &str,
) -> Result<Arc<CompletionModelRecord>, ScratchError> {
    let caps =
        refact_runtime_api::caps_loader::try_load_caps_quickly_if_not_present(gcx.clone(), 0)
            .await?;
    let model_rec = resolve_completion_model(caps, requested_model)
        .map_err(|e| ScratchError::new(StatusCode::UNPROCESSABLE_ENTITY, e.to_string()))?;
    load_privacy_if_needed(gcx.clone()).await;
    let policy = gcx.privacy_policy_load.read().unwrap().policy.clone();
    let workspace_roots = gcx
        .documents_state
        .workspace_folders
        .lock()
        .unwrap()
        .clone();
    authorize_completion_path(&policy, cursor_file, &model_rec.base.id, &workspace_roots)?;
    Ok(model_rec)
}

fn authorize_completion_path(
    policy: &refact_privacy::PrivacyPolicy,
    cursor_file: &str,
    model_id: &str,
    workspace_roots: &[std::path::PathBuf],
) -> Result<(), ScratchError> {
    let destination = refact_privacy::Destination {
        id: refact_privacy::DestinationId(
            model_id.split('/').next().unwrap_or(model_id).to_string(),
        ),
        kind: refact_privacy::DestinationKind::Completion,
        display_name: model_id.to_string(),
    };
    let cpath = canonical_path(cursor_file);
    let compiled = policy.compile().map_err(|e| {
        ScratchError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("privacy policy cannot authorize completion: {e}"),
        )
    })?;
    let zone = compiled.zone_for_path_with_roots(&cpath, workspace_roots);
    if !destination.matches_send_to(&zone.send_to) {
        return Err(ScratchError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!(
                "completion provider {} cannot receive path {}",
                destination.id.0,
                cpath.display()
            ),
        ));
    }
    Ok(())
}

pub async fn handle_v1_code_completion(
    app: AppState,
    code_completion_post: &mut CodeCompletionPost,
) -> Result<Response<Body>, ScratchError> {
    let gcx = app.gcx.clone();
    code_completion_post_validate(code_completion_post)?;

    let model_rec = resolve_completion_model_for_path(
        gcx.clone(),
        &code_completion_post.model,
        &code_completion_post.inputs.cursor.file,
    )
    .await?;
    if code_completion_post.parameters.max_new_tokens == 0 {
        code_completion_post.parameters.max_new_tokens = 50;
    }
    code_completion_post.model = model_rec.base.id.clone();
    info!(
        "chosen completion model: {}, scratchpad: {}",
        code_completion_post.model, model_rec.scratchpad
    );
    code_completion_post.parameters.temperature =
        Some(code_completion_post.parameters.temperature.unwrap_or(0.2));
    let cache_arc = { gcx.completions_cache.clone() };
    if !code_completion_post.no_cache {
        let cache_key = completion_cache::cache_key_from_post(&code_completion_post);
        let cached_maybe = completion_cache::cache_get(cache_arc.clone(), cache_key.clone());
        if let Some(cached_json_value) = cached_maybe {
            // info!("cache hit for key {:?}", cache_key.clone());
            if !code_completion_post.stream {
                return refact_runtime_api::restream::cached_not_stream(cached_json_value.as_ref())
                    .await;
            } else {
                return refact_runtime_api::restream::cached_stream(cached_json_value.as_ref())
                    .await;
            }
        }
    }

    let mut scratchpad = scratchpads_runtime::create_code_completion_scratchpad(
        gcx.clone(),
        &model_rec,
        &code_completion_post.clone(),
        cache_arc.clone(),
    )
    .await
    .map_err(|e| ScratchError::new(StatusCode::BAD_REQUEST, e))?;
    let ccx = Arc::new(AMutex::new(
        refact_runtime_api::at_commands_new_from_app(
            app,
            model_rec.base.n_ctx,
            CODE_COMPLETION_TOP_N,
            true,
            vec![],
            "".to_string(),
            None,
            model_rec.base.id.clone(),
            None,
            None,
        )
        .await,
    ));
    if !code_completion_post.stream {
        refact_runtime_api::restream::scratchpad_interaction_not_stream(
            ccx.clone(),
            &mut scratchpad,
            "completion".to_string(),
            &model_rec.base,
            &mut code_completion_post.parameters,
            false,
        )
        .await
    } else {
        refact_runtime_api::restream::scratchpad_interaction_stream(
            ccx.clone(),
            scratchpad,
            "completion-stream".to_string(),
            model_rec.base.clone(),
            code_completion_post.parameters.clone(),
            false,
            None,
        )
        .await
    }
}

pub async fn handle_v1_code_completion_web(
    State(app): State<AppState>,
    body_bytes: hyper::body::Bytes,
) -> Result<Response<Body>, ScratchError> {
    let mut code_completion_post = serde_json::from_slice::<CodeCompletionPost>(&body_bytes)
        .map_err(|e| ScratchError::new(StatusCode::BAD_REQUEST, format!("JSON problem: {}", e)))?;
    handle_v1_code_completion(app, &mut code_completion_post).await
}

pub async fn handle_v1_code_completion_prompt(
    State(app): State<AppState>,
    body_bytes: hyper::body::Bytes,
) -> Result<Response<Body>, ScratchError> {
    let gcx = app.gcx.clone();
    // Almost the same function, but only returns the prompt (good for generating data)
    let mut post = serde_json::from_slice::<CodeCompletionPost>(&body_bytes)
        .map_err(|e| ScratchError::new(StatusCode::BAD_REQUEST, format!("JSON problem: {}", e)))?;
    code_completion_post_validate(&post)?;

    let model_rec =
        resolve_completion_model_for_path(gcx.clone(), &post.model, &post.inputs.cursor.file)
            .await?;

    // don't need cache, but go along
    let cache_arc = { gcx.completions_cache.clone() };

    let mut scratchpad = scratchpads_runtime::create_code_completion_scratchpad(
        gcx.clone(),
        &model_rec,
        &post,
        cache_arc.clone(),
    )
    .await
    .map_err(|e| ScratchError::new(StatusCode::BAD_REQUEST, e))?;

    let ccx = Arc::new(AMutex::new(
        refact_runtime_api::at_commands_new_from_app(
            app,
            model_rec.base.n_ctx,
            CODE_COMPLETION_TOP_N,
            true,
            vec![],
            "".to_string(),
            None,
            model_rec.base.id.clone(),
            None,
            None,
        )
        .await,
    ));
    let prompt_input = {
        let cgcx = ccx.lock().await;
        ScratchpadPromptInput {
            n_ctx: cgcx.n_ctx,
            postprocess_parameters: cgcx.postprocess_parameters.clone(),
        }
    };
    let prompt = scratchpad
        .prompt(prompt_input, &mut post.parameters)
        .await
        .map_err(|e| {
            ScratchError::new(StatusCode::INTERNAL_SERVER_ERROR, format!("Prompt: {}", e))
        })?;

    let body = serde_json::json!({"prompt": prompt}).to_string();
    let response = Response::builder()
        .status(200)
        .header("Content-Type", "application/json")
        .body(Body::from(body))
        .unwrap();
    return Ok(response);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completion_path_policy_denies_excluded_zone_and_allows_normal_zone() {
        let policy = refact_privacy::PrivacyPolicy {
            blocked: Vec::new(),
            zones: vec![
                refact_privacy::Zone {
                    name: "excluded".to_string(),
                    patterns: vec!["*denied.rs".to_string()],
                    send_to: vec!["other".to_string()],
                    on_shell_read: refact_privacy::ShellBehavior::Withhold,
                },
                refact_privacy::Zone {
                    name: "normal".to_string(),
                    patterns: vec!["*".to_string()],
                    send_to: vec!["trusted".to_string()],
                    on_shell_read: refact_privacy::ShellBehavior::Withhold,
                },
            ],
            subagents: refact_privacy::SubagentPolicy::default(),
            ..Default::default()
        };

        let denied = authorize_completion_path(&policy, "denied.rs", "trusted/model", &[]);
        let allowed = authorize_completion_path(&policy, "allowed.rs", "trusted/model", &[]);

        assert_eq!(
            denied.unwrap_err().status_code,
            StatusCode::UNPROCESSABLE_ENTITY
        );
        assert!(allowed.is_ok());
    }
}
