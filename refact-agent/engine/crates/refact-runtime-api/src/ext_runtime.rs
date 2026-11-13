use std::sync::atomic::Ordering;
use std::sync::Arc;

use refact_buddy_core::types::{BuddyControl, BuddyRuntimeEvent};
use refact_core::privacy_types::PrivacySettings;
use refact_ext::competitor_import::types::{
    Competitor, ImportIssue, ImportPrivacyFilter, ImportReport, ImportStatus, ImportSummary,
};
use refact_ext::competitor_import::{
    import_reports_for_runtime_events, log_import_summary, plural_suffix, runtime_dedupe_key,
    runtime_scope_label,
};

use crate::app_state::AppState;

pub async fn run_global_import(app: AppState) -> ImportSummary {
    let refact_config_dir = app.paths.config_dir.clone();
    let privacy_settings = app.workspace.privacy_settings.clone();
    let home_dir = home::home_dir();
    let filter = privacy_filter_from_settings(privacy_settings);
    let summary = refact_ext::competitor_import::run_global_import_with_paths_and_filter(
        &refact_config_dir,
        home_dir.as_deref(),
        &filter,
    )
    .await;
    apply_cache_invalidation(app.clone(), &summary).await;
    log_import_summary("global", &summary);
    emit_buddy_import_events(app, &summary).await;
    summary
}

pub async fn run_global_import_for_source(
    app: AppState,
    source: Option<Competitor>,
) -> ImportSummary {
    let refact_config_dir = app.paths.config_dir.clone();
    let privacy_settings = app.workspace.privacy_settings.clone();
    let home_dir = home::home_dir();
    let filter = privacy_filter_from_settings(privacy_settings);
    let summary = refact_ext::competitor_import::run_global_import_with_paths_and_filter_for_source(
        &refact_config_dir,
        home_dir.as_deref(),
        &filter,
        source,
    )
    .await;
    apply_cache_invalidation(app.clone(), &summary).await;
    log_import_summary("global", &summary);
    emit_buddy_import_events(app, &summary).await;
    summary
}

pub async fn run_project_import_for_source(
    app: AppState,
    source: Option<Competitor>,
) -> ImportSummary {
    let workspace_folders = app.workspace.documents_state.workspace_folders.clone();
    let privacy_settings = app.workspace.privacy_settings.clone();
    let workspace_roots_result = workspace_folders
        .lock()
        .map(|workspace_folders| workspace_folders.clone())
        .map_err(|err| err.to_string());
    let workspace_roots = match workspace_roots_result {
        Ok(workspace_roots) => workspace_roots,
        Err(err) => {
            let mut summary = ImportSummary::default();
            summary.add_issue(ImportIssue {
                competitor: None,
                kind: None,
                scope: None,
                path: None,
                status: ImportStatus::Error,
                message: format!("workspace folders unavailable: {err}"),
            });
            log_import_summary("project", &summary);
            emit_buddy_import_events(app, &summary).await;
            return summary;
        }
    };
    let filter = privacy_filter_from_settings(privacy_settings);
    let summary = refact_ext::competitor_import::run_project_import_with_paths_and_filter_for_source(
        &workspace_roots,
        &filter,
        source,
    )
    .await;
    apply_cache_invalidation(app.clone(), &summary).await;
    log_import_summary("project", &summary);
    emit_buddy_import_events(app, &summary).await;
    summary
}

pub async fn run_project_import(app: AppState) -> ImportSummary {
    let workspace_folders = app.workspace.documents_state.workspace_folders.clone();
    let privacy_settings = app.workspace.privacy_settings.clone();
    let workspace_roots_result = workspace_folders
        .lock()
        .map(|workspace_folders| workspace_folders.clone())
        .map_err(|err| err.to_string());
    let workspace_roots = match workspace_roots_result {
        Ok(workspace_roots) => workspace_roots,
        Err(err) => {
            let mut summary = ImportSummary::default();
            summary.add_issue(ImportIssue {
                competitor: None,
                kind: None,
                scope: None,
                path: None,
                status: ImportStatus::Error,
                message: format!("workspace folders unavailable: {err}"),
            });
            log_import_summary("project", &summary);
            emit_buddy_import_events(app, &summary).await;
            return summary;
        }
    };
    let filter = privacy_filter_from_settings(privacy_settings);
    let summary =
        refact_ext::competitor_import::run_project_import_with_paths_and_filter(
            &workspace_roots,
            &filter,
        )
        .await;
    apply_cache_invalidation(app.clone(), &summary).await;
    log_import_summary("project", &summary);
    emit_buddy_import_events(app, &summary).await;
    summary
}

fn privacy_filter_from_settings(settings: Arc<PrivacySettings>) -> ImportPrivacyFilter {
    ImportPrivacyFilter::with_check(Arc::new(move |path| {
        refact_privacy::check_file_privacy(
            settings.clone(),
            path,
            &refact_core::privacy_types::FilePrivacyLevel::AllowToSendAnywhere,
        )
    }))
}

pub async fn apply_cache_invalidation(app: AppState, summary: &ImportSummary) {
    if !summary.has_imported_changes() {
        return;
    }
    app.integrations
        .ext_cache_generation
        .fetch_add(1, Ordering::Relaxed);
    if summary.has_command_or_skill_changes() {
        crate::slash_cache::invalidate_slash_cache().await;
    }
    if summary.has_subagent_changes() {
        crate::customization_registry_gcx::invalidate_all_registry_caches(app.gcx).await;
    }
}

pub async fn emit_buddy_import_events(app: AppState, summary: &ImportSummary) {
    let reports = import_reports_for_runtime_events(summary);
    if reports.is_empty() {
        return;
    }
    let buddy_arc = app.buddy.buddy.clone();
    let mut buddy = buddy_arc.lock().await;
    let Some(service) = buddy.as_mut() else {
        return;
    };
    for report in reports {
        service.enqueue_runtime_event(buddy_runtime_event_for_import_report(&report));
    }
}

pub fn buddy_runtime_event_for_import_report(report: &ImportReport) -> BuddyRuntimeEvent {
    let created = report.status_count(&ImportStatus::Created);
    let updated = report.status_count(&ImportStatus::Updated);
    let unchanged = report.status_count(&ImportStatus::Unchanged);
    let stale = report.status_count(&ImportStatus::Stale);
    let conflicts = report.status_count(&ImportStatus::Conflict);
    let user_modified = report.status_count(&ImportStatus::UserModified);
    let unsupported = report.status_count(&ImportStatus::Unsupported);
    let errors = report.status_count(&ImportStatus::Error);
    let attention = conflicts + user_modified + errors;
    let status = if errors > 0 {
        "error"
    } else if stale + conflicts + user_modified + unsupported > 0 {
        "warning"
    } else {
        "completed"
    };
    let priority = if attention > 0 {
        "high"
    } else if created + updated > 0 || stale > 0 || unsupported > 0 {
        "normal"
    } else {
        "low"
    };
    let title = if attention > 0 {
        format!("Competitor import needs attention ({attention})")
    } else if created + updated > 0 {
        format!(
            "Competitor import added {} customization{}",
            created + updated,
            plural_suffix(created + updated)
        )
    } else if stale > 0 {
        format!(
            "Competitor import found {} stale generated customization{}",
            stale,
            plural_suffix(stale)
        )
    } else {
        "Competitor import checked customizations".to_string()
    };
    let description = format!(
        "{}: discovered {}, created {}, updated {}, unchanged {}, stale {}, conflicts {}, user-modified {}, unsupported {}, errors {}.",
        runtime_scope_label(report),
        report.discovered_candidates,
        created,
        updated,
        unchanged,
        stale,
        conflicts,
        user_modified,
        unsupported,
        errors
    );
    let mut event = refact_buddy_core::actor::make_runtime_event(
        "competitor_import",
        &title,
        "competitor_import",
        &runtime_dedupe_key(report),
        status,
        Some(priority),
    );
    event.description = Some(description);
    event.failure_category = None;
    event.failure_summary = None;
    event.persistent = attention > 0;
    event.ttl_ms = if attention > 0 {
        None
    } else if priority == "low" {
        Some(6000)
    } else {
        Some(12000)
    };
    event.speech_text = if attention > 0 { Some(title) } else { None };
    event.controls = vec![
        BuddyControl {
            id: "open-buddy".to_string(),
            label: "Open Buddy".to_string(),
            action: "open_buddy".to_string(),
            action_param: None,
            style: "primary".to_string(),
        },
        BuddyControl {
            id: "dismiss".to_string(),
            label: "Dismiss".to_string(),
            action: "dismiss".to_string(),
            action_param: None,
            style: "secondary".to_string(),
        },
    ];
    event
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    use refact_ext::competitor_import::types::{
        Competitor, ImportCandidateSummary, ImportKind, ImportOutcome, ImportScope,
    };

    fn runtime_event_summary(status: ImportStatus, scope: ImportScope) -> ImportSummary {
        let mut summary = ImportSummary::from_scopes(vec![scope.clone()]);
        let candidate = ImportCandidateSummary {
            competitor: Competitor::ClaudeCode,
            kind: ImportKind::Command,
            scope,
            source_root: PathBuf::from("/source"),
            source_path: PathBuf::from("/source/secret.md"),
            dest_name: "secret".to_string(),
            destination_path: PathBuf::from("commands/secret.md"),
            metadata: serde_json::json!({"artifact_body": "secret body"}),
        };
        summary.add_outcome(ImportOutcome {
            candidate,
            status,
            message: "generated output changed".to_string(),
        });
        summary.mark_completed();
        summary
    }

    #[test]
    fn runtime_event_for_unchanged_import_is_low_transient() {
        let summary = runtime_event_summary(ImportStatus::Unchanged, ImportScope::Global);
        let report = ImportReport::from_summary(&summary);

        let event = buddy_runtime_event_for_import_report(&report);

        assert_eq!(event.signal_type, "competitor_import");
        assert_eq!(event.source, "competitor_import");
        assert_eq!(event.status, "completed");
        assert_eq!(event.priority, "low");
        assert!(!event.persistent);
        assert_eq!(event.ttl_ms, Some(6000));
        assert_eq!(
            event.dedupe_key.as_deref(),
            Some("competitor_import:global")
        );
    }

    #[test]
    fn runtime_event_for_created_import_is_normal_completed() {
        let summary = runtime_event_summary(ImportStatus::Created, ImportScope::Global);
        let report = ImportReport::from_summary(&summary);

        let event = buddy_runtime_event_for_import_report(&report);

        assert_eq!(event.status, "completed");
        assert_eq!(event.priority, "normal");
        assert!(!event.persistent);
        assert!(event.title.contains("added 1"));
        assert_eq!(event.controls[0].action, "open_buddy");
    }

    #[test]
    fn runtime_event_for_conflicts_and_errors_is_sanitized_attention() {
        let scope = ImportScope::Project {
            root: PathBuf::from("/home/user/private-project"),
        };
        let mut summary = runtime_event_summary(ImportStatus::Conflict, scope);
        summary.add_issue(ImportIssue {
            competitor: Some(Competitor::OpenCode),
            kind: Some(ImportKind::Command),
            scope: summary.discovered_scopes.first().cloned(),
            path: Some(PathBuf::from("commands/other.md")),
            status: ImportStatus::Error,
            message: "failed without leaking body".to_string(),
        });
        summary.mark_completed();
        let report = ImportReport::from_summary(&summary);

        let event = buddy_runtime_event_for_import_report(&report);
        let event_json = serde_json::to_string(&event).unwrap();

        assert_eq!(event.status, "error");
        assert_eq!(event.priority, "high");
        assert!(event.persistent);
        assert!(event.ttl_ms.is_none());
        assert!(event.speech_text.is_some());
        assert!(!event_json.contains("secret body"));
        assert!(!event_json.contains("private-project"));
        assert!(event
            .dedupe_key
            .as_deref()
            .is_some_and(|key| key.starts_with("competitor_import:project:")));
    }

    #[test]
    fn runtime_reports_include_aggregate_unscoped_errors_with_scopes() {
        let scope = ImportScope::Project {
            root: PathBuf::from("/home/user/private-project"),
        };
        let mut summary = ImportSummary::from_scopes(vec![scope]);
        summary.add_issue(ImportIssue {
            competitor: None,
            kind: None,
            scope: None,
            path: None,
            status: ImportStatus::Error,
            message: "workspace folders unavailable".to_string(),
        });
        summary.mark_completed();

        let reports = import_reports_for_runtime_events(&summary);
        let events = reports
            .iter()
            .map(buddy_runtime_event_for_import_report)
            .collect::<Vec<_>>();

        assert_eq!(reports.len(), 2);
        assert!(events.iter().any(|event| {
            event.status == "error"
                && event.dedupe_key.as_deref() == Some("competitor_import:workspace")
        }));
    }
}
