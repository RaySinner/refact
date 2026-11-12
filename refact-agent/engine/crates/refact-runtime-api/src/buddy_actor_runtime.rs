use std::path::{Path, PathBuf};

use refact_buddy_core::diagnostics::{diagnostic_id, DiagnosticContext};
use refact_buddy_core::snapshot::BuddySnapshot;
use refact_buddy_core::storage::{load_diagnostics, load_recent_diagnostics};
use refact_buddy_core::types::BuddySpeechItem;
use tracing::warn;

use crate::app_state::AppState;
use crate::files_correction::get_unscoped_project_dirs_for_ctx;

pub async fn buddy_complete_event(app: AppState, dedupe_key: &str, status: &str) {
    let buddy_arc = app.chat_ctx().buddy.buddy.clone();
    let mut lock = buddy_arc.lock().await;
    if let Some(svc) = lock.as_mut() {
        svc.complete_runtime_event(dedupe_key, status);
    }
}

pub async fn resolve_buddy_chat_model(app: AppState) -> String {
    let ctx = app.chat_ctx();
    let caps_state = ctx.model.caps.read().await;
    let Some(caps) = caps_state.caps.as_ref() else {
        return String::new();
    };
    refact_caps_core::model_records::resolve_buddy_chat_model(caps)
}

pub async fn buddy_snapshot(app: AppState) -> Option<BuddySnapshot> {
    let buddy_arc = app.chat_ctx().buddy.buddy.clone();
    let lock = buddy_arc.lock().await;
    lock.as_ref().map(|svc| svc.snapshot())
}

pub async fn buddy_record_workflow_telemetry(
    app: AppState,
    workflow_id: &str,
    llm_calls: u64,
    tokens_in: u64,
    tokens_out: u64,
    produced_output: bool,
) {
    let buddy_arc = app.chat_ctx().buddy.buddy.clone();
    let mut lock = buddy_arc.lock().await;
    if let Some(svc) = lock.as_mut() {
        svc.record_workflow_telemetry(
            workflow_id,
            llm_calls,
            tokens_in,
            tokens_out,
            produced_output,
        );
    }
}

pub async fn buddy_update_speech(app: AppState, speech: BuddySpeechItem) {
    let buddy_arc = app.chat_ctx().buddy.buddy.clone();
    let mut lock = buddy_arc.lock().await;
    if let Some(svc) = lock.as_mut() {
        svc.update_speech(speech);
    }
}

pub async fn buddy_llm_budget_exhausted(app: &AppState) -> bool {
    let buddy_arc = app.chat_ctx().buddy.buddy.clone();
    let lock = buddy_arc.lock().await;
    lock.as_ref()
        .map(|svc| svc.llm_budget_exhausted())
        .unwrap_or(false)
}

pub async fn buddy_update_speech_user(app: AppState, speech: BuddySpeechItem) {
    let buddy_arc = app.chat_ctx().buddy.buddy.clone();
    let mut lock = buddy_arc.lock().await;
    if let Some(svc) = lock.as_mut() {
        svc.update_speech_user_initiated(speech);
    }
}

pub async fn latest_project_root(app: AppState) -> Result<PathBuf, String> {
    let ctx = app.chat_ctx();
    get_unscoped_project_dirs_for_ctx(&ctx)
        .await
        .into_iter()
        .next()
        .ok_or_else(|| "no project root".to_string())
}

pub async fn load_diagnostics_for_service(project_root: &Path) -> Vec<DiagnosticContext> {
    match load_recent_diagnostics(project_root, 100).await {
        Ok(diags) => diags,
        Err(err) => {
            warn!("buddy: failed to load diagnostic history: {}", err);
            Vec::new()
        }
    }
}

pub async fn resolve_diagnostic(
    app: AppState,
    diagnostic_index: Option<usize>,
    diagnostic_id_arg: Option<&str>,
    collected_at: Option<&str>,
    fallback: Option<DiagnosticContext>,
) -> Result<DiagnosticContext, String> {
    let project_root = latest_project_root(app.clone()).await?;
    let buddy_arc = app.chat_ctx().buddy.buddy.clone();
    let lock = buddy_arc.lock().await;
    let svc = lock
        .as_ref()
        .ok_or_else(|| "buddy service not initialized".to_string())?;
    let by_id = diagnostic_id_arg.and_then(|id| svc.diagnostic_by_id(id));
    let by_time = collected_at.and_then(|ts| svc.diagnostic_by_collected_at(ts));
    let recent = svc.recent_diagnostics.clone();
    drop(lock);

    if let Some(id) = diagnostic_id_arg {
        if let Some(ctx) = by_id {
            return Ok(ctx);
        }
        let diags = load_diagnostics(&project_root).await?;
        if let Some(ctx) = diags.into_iter().find(|diag| diagnostic_id(diag) == id) {
            return Ok(ctx);
        }
        return Err("diagnostic id not found".to_string());
    }

    if let Some(ts) = collected_at {
        if let Some(ctx) = by_time {
            return Ok(ctx);
        }
        let diags = load_diagnostics(&project_root).await?;
        if let Some(ctx) = diags.into_iter().find(|diag| diag.collected_at == ts) {
            return Ok(ctx);
        }
        return Err("diagnostic timestamp not found".to_string());
    }

    if let Some(idx) = diagnostic_index {
        let diags = if recent.is_empty() {
            load_recent_diagnostics(&project_root, 100).await?
        } else {
            recent
        };
        return diags
            .get(idx)
            .cloned()
            .ok_or_else(|| "diagnostic index out of range".to_string());
    }

    fallback.ok_or_else(|| "provide diagnostic reference or error".to_string())
}
