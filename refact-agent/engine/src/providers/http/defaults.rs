use super::*;

pub async fn handle_v1_defaults_get(
    State(app): State<AppState>,
) -> Result<Response<Body>, ScratchError> {
    let ctx = app.chat_ctx();
    let config_dir = ctx.paths.config_dir.clone();
    let defaults = ProviderDefaults::load(&config_dir)
        .await
        .map_err(|e| ScratchError::new(StatusCode::UNPROCESSABLE_ENTITY, e))?;

    json_response(StatusCode::OK, &defaults)
}


#[derive(Deserialize)]
pub(crate) struct DefaultsUpdateRequest {
    #[serde(default)]
    pub(crate) draft_id: Option<String>,
    #[serde(flatten)]
    pub(crate) defaults: ProviderDefaults,
}


pub(crate) fn defaults_draft_validation_error(err: DraftValidationError) -> ScratchError {
    match err {
        DraftValidationError::NotFound => {
            ScratchError::new(StatusCode::NOT_FOUND, "draft_not_found".to_string())
        }
        DraftValidationError::KindMismatch { expected, actual } => ScratchError::new(
            StatusCode::CONFLICT,
            format!(
                "draft_kind_mismatch: expected {}, got {}",
                draft_kind_str(&expected),
                draft_kind_str(&actual)
            ),
        ),
        DraftValidationError::TargetMismatch { expected, actual } => ScratchError::new(
            StatusCode::CONFLICT,
            format!(
                "draft_target_mismatch: expected {}, got {}",
                expected, actual
            ),
        ),
        DraftValidationError::Parse(err) => ScratchError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("draft_parse_failed: {}", err),
        ),
    }
}


pub(crate) fn validate_defaults_draft_shape(content: &str) -> Result<(), ScratchError> {
    let value: Value = serde_json::from_str(content).map_err(|e| {
        ScratchError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("draft_parse_failed: {}", e),
        )
    })?;
    let object = value.as_object().ok_or_else(|| {
        ScratchError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "draft_parse_failed: defaults draft must be a JSON object".to_string(),
        )
    })?;
    for key in object.keys() {
        if !matches!(
            key.as_str(),
            "chat"
                | "chat_model_2"
                | "task_planner_agent_model"
                | "chat_light"
                | "chat_thinking"
                | "chat_buddy"
                | "completion_model"
                | "embedding_model"
        ) {
            return Err(ScratchError::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                format!("draft_parse_failed: unsupported defaults key {}", key),
            ));
        }
    }
    serde_json::from_value::<ProviderDefaults>(value).map_err(|e| {
        ScratchError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("draft_parse_failed: {}", e),
        )
    })?;
    Ok(())
}


pub(crate) async fn validate_defaults_draft(
    ctx: &ChatCtx,
    draft_id: &str,
) -> Result<(), ScratchError> {
    let buddy_arc = ctx.buddy.buddy.clone();
    let lock = buddy_arc.lock().await;
    let svc = lock.as_ref().ok_or_else(|| {
        ScratchError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "buddy not initialized".to_string(),
        )
    })?;
    let content = svc
        .draft_store
        .get_validated(draft_id, DraftKind::DefaultsModel, DraftTarget::Any)
        .map_err(defaults_draft_validation_error)?
        .yaml_or_json
        .clone();
    drop(lock);
    validate_defaults_draft_shape(&content)
}


pub(crate) async fn consume_defaults_draft(
    ctx: &ChatCtx,
    draft_id: &str,
) -> Result<(), ScratchError> {
    let buddy_arc = ctx.buddy.buddy.clone();
    let mut lock = buddy_arc.lock().await;
    let svc = lock.as_mut().ok_or_else(|| {
        ScratchError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "buddy not initialized".to_string(),
        )
    })?;
    svc.consume_validated_draft(draft_id, DraftKind::DefaultsModel, DraftTarget::Any)
        .map(|_| ())
        .map_err(defaults_draft_validation_error)
}


pub async fn handle_v1_defaults_update(
    State(app): State<AppState>,
    body_bytes: hyper::body::Bytes,
) -> Result<Response<Body>, ScratchError> {
    let ctx = app.chat_ctx();
    let req: DefaultsUpdateRequest = serde_json::from_slice(&body_bytes).map_err(|e| {
        ScratchError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("Invalid JSON: {}", e),
        )
    })?;

    if let Some(draft_id) = req.draft_id.as_deref() {
        validate_defaults_draft(&ctx, draft_id).await?;
    }

    let config_dir = app.paths.config_dir.clone();
    req.defaults
        .save(&config_dir)
        .await
        .map_err(|e| ScratchError::new(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    if let Some(draft_id) = req.draft_id.as_deref() {
        consume_defaults_draft(&ctx, draft_id).await?;
    }

    invalidate_caps(&ctx).await;

    json_response(StatusCode::OK, &json!({"success": true}))
}


pub(crate) async fn project_defaults_root(ctx: &ChatCtx) -> Option<std::path::PathBuf> {
    crate::files_correction::get_unscoped_project_dirs_for_ctx(ctx)
        .await
        .into_iter()
        .next()
}


pub async fn handle_v1_project_defaults_get(
    State(app): State<AppState>,
) -> Result<Response<Body>, ScratchError> {
    let ctx = app.chat_ctx();
    let Some(root) = project_defaults_root(&ctx).await else {
        return json_response(
            StatusCode::OK,
            &json!({
                "project_available": false,
                "project_root": Value::Null,
                "path": Value::Null,
                "defaults": ProjectModelDefaults::default(),
            }),
        );
    };

    let defaults = ProjectModelDefaults::load(&root)
        .await
        .map_err(|e| ScratchError::new(StatusCode::UNPROCESSABLE_ENTITY, e))?;

    json_response(
        StatusCode::OK,
        &json!({
            "project_available": true,
            "project_root": root.to_string_lossy(),
            "path": project_model_defaults_path(&root).to_string_lossy(),
            "defaults": defaults,
        }),
    )
}


pub async fn handle_v1_project_defaults_update(
    State(app): State<AppState>,
    body_bytes: hyper::body::Bytes,
) -> Result<Response<Body>, ScratchError> {
    let ctx = app.chat_ctx();
    let defaults: ProjectModelDefaults = serde_json::from_slice(&body_bytes).map_err(|e| {
        ScratchError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("Invalid JSON: {}", e),
        )
    })?;

    let Some(root) = project_defaults_root(&ctx).await else {
        return Err(ScratchError::new(
            StatusCode::BAD_REQUEST,
            "no project directory is open, cannot save project model defaults".to_string(),
        ));
    };

    defaults
        .save(&root)
        .await
        .map_err(|e| ScratchError::new(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    invalidate_caps(&ctx).await;

    json_response(StatusCode::OK, &json!({"success": true}))
}

// /v1/models endpoint - returns models grouped by type for frontend compatibility
