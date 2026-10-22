use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex as AMutex;

use crate::global_context::GlobalContext;
use refact_core::image_policy::ImagePolicy;
use refact_integrations::browser_models::*;
use refact_browser::BrowserRuntime;

pub use refact_browser::controller::*;

pub async fn execute_request_with_runtime_validated(
    runtime_arc: Arc<AMutex<BrowserRuntime>>,
    request: BrowserActionRequest,
    image_policy: &ImagePolicy,
    gcx: Arc<GlobalContext>,
) -> Result<ExecutionReport, String> {
    validate_upload_paths(gcx.clone(), &request).await?;

    let plan = {
        let rt = runtime_arc.lock().await;
        rt.attached_chat_id
            .clone()
            .filter(|_| rt.owns_browser_process())
            .map(|chat_id| (chat_id, rt.profile_dir.clone(), rt.launch_options.clone()))
    };
    let Some((chat_id, profile_dir, launch_options)) = plan else {
        return execute_request_with_runtime(runtime_arc, request, image_policy).await;
    };

    let app = crate::app_state::AppState::from_gcx(gcx).await;
    let total_steps = request.steps.len();
    let transport_alive = {
        let mut rt = runtime_arc.lock().await;
        tokio::task::block_in_place(|| rt.check_connection())
    };

    let mut resume = None;
    if transport_alive {
        let outcome =
            execute_request_with_runtime(runtime_arc, request.clone(), image_policy).await;
        if !report_hit_dead_transport(&outcome) {
            return outcome;
        }
        if let Ok(attempt) = &outcome {
            resume = Some(plan_resume(attempt, total_steps));
        }
    }

    let runtime_arc = relaunch_and_resolve(app, &chat_id, profile_dir, launch_options).await?;
    let Some(resume) = resume else {
        let mut report = execute_request_with_runtime(runtime_arc, request, image_policy).await?;
        report
            .warnings
            .push(crate::integrations::browser_runtime::RELAUNCH_WARNING.to_string());
        return Ok(report);
    };
    let resume = resume.with_replay_from(&request.steps);
    if resume.resume_index > 0 && resume.replay.is_none() {
        return Ok(resume_refused_without_page_context(resume));
    }
    let mut retry_request = request;
    let tail = retry_request.steps.split_off(resume.resume_index);
    retry_request.steps = resume
        .replay
        .iter()
        .map(|(_, step)| step.clone())
        .chain(tail)
        .collect();
    let retry = execute_request_with_runtime(runtime_arc, retry_request, image_policy).await?;
    Ok(merge_resumed_report(resume, retry))
}

struct ResumePlan {
    resume_index: usize,
    total_steps: usize,
    completed_steps: Vec<StepResult>,
    warnings: Vec<String>,
    replay: Option<(usize, BrowserStep)>,
}

impl ResumePlan {
    fn with_replay_from(mut self, steps: &[BrowserStep]) -> Self {
        self.replay = steps
            .iter()
            .take(self.resume_index)
            .enumerate()
            .rev()
            .find(|(_, step)| restores_document_after_relaunch(step))
            .map(|(idx, step)| (idx, step.clone()));
        self
    }
}

fn restores_document_after_relaunch(step: &BrowserStep) -> bool {
    matches!(
        step,
        BrowserStep::Navigate { .. } | BrowserStep::SetContent { .. } | BrowserStep::OpenTab { .. }
    )
}

fn plan_resume(attempt: &ExecutionReport, total_steps: usize) -> ResumePlan {
    let step_at = |index: usize| attempt.steps.iter().find(|step| step.step_index == index);
    let resume_index = (0..total_steps)
        .find(|index| {
            step_at(*index).is_none_or(|step| step.skipped || step_hit_dead_transport(step))
        })
        .unwrap_or(total_steps);
    ResumePlan {
        resume_index,
        total_steps,
        completed_steps: (0..resume_index).filter_map(step_at).cloned().collect(),
        warnings: attempt.warnings.clone(),
        replay: None,
    }
}

fn resume_refused_without_page_context(resume: ResumePlan) -> ExecutionReport {
    let mut steps = resume.completed_steps;
    steps.push(StepResult::failure(
        resume.resume_index,
        "Resume after browser relaunch",
        "the page could not be restored after the relaunch, so this step was not run",
    ));
    mark_unexecuted_steps_skipped(&mut steps, resume.total_steps);
    let mut warnings = resume.warnings;
    warnings.push(
        crate::integrations::browser_runtime::relaunch_without_page_context_warning(
            resume.resume_index,
        ),
    );
    ExecutionReport {
        ok: false,
        steps,
        warnings,
        url: Some("about:blank".to_string()),
        ..ExecutionReport::default()
    }
}

fn merge_resumed_report(resume: ResumePlan, mut retry: ExecutionReport) -> ExecutionReport {
    let mut steps = resume.completed_steps;
    let mut retried = std::mem::take(&mut retry.steps);
    let mut warnings = resume.warnings;
    let shift = match &resume.replay {
        Some((replayed_index, _)) => {
            let replay_result = (!retried.is_empty()).then(|| retried.remove(0));
            match replay_result {
                Some(result) if result.ok => {
                    warnings.push(
                        crate::integrations::browser_runtime::relaunch_replay_warning(
                            *replayed_index,
                            resume.resume_index,
                            &result.summary,
                        ),
                    );
                    resume.resume_index - 1
                }
                other => {
                    let detail = other
                        .map(|result| result.error.unwrap_or(result.summary))
                        .unwrap_or_else(|| "no result was produced".to_string());
                    steps.push(StepResult::failure(
                        resume.resume_index,
                        "Resume after browser relaunch",
                        format!(
                            "replayed step {replayed_index} to restore the document, but it failed: {detail}"
                        ),
                    ));
                    mark_unexecuted_steps_skipped(&mut steps, resume.total_steps);
                    warnings.push(
                        crate::integrations::browser_runtime::relaunch_resume_warning(
                            resume.resume_index,
                        ),
                    );
                    warnings.append(&mut retry.warnings);
                    retry.warnings = warnings;
                    retry.steps = steps;
                    retry.ok = false;
                    return retry;
                }
            }
        }
        None => {
            warnings.push(
                crate::integrations::browser_runtime::relaunch_resume_warning(resume.resume_index),
            );
            resume.resume_index
        }
    };
    for step in &mut retried {
        step.step_index += shift;
    }
    steps.append(&mut retried);
    retry.steps = steps;
    warnings.append(&mut retry.warnings);
    retry.warnings = warnings;
    retry
}

fn step_hit_dead_transport(step: &StepResult) -> bool {
    step.error
        .as_deref()
        .is_some_and(refact_browser::is_transport_dead_error)
}

fn report_hit_dead_transport(outcome: &Result<ExecutionReport, String>) -> bool {
    match outcome {
        Err(error) => refact_browser::is_transport_dead_error(error),
        Ok(report) => !report.ok && report.steps.iter().any(step_hit_dead_transport),
    }
}

async fn relaunch_and_resolve(
    app: crate::app_state::AppState,
    chat_id: &str,
    profile_dir: PathBuf,
    launch_options: refact_browser::BrowserLaunchOptions,
) -> Result<Arc<AMutex<BrowserRuntime>>, String> {
    let window_bounds = launch_options.window_bounds.clone();
    let runtime_id = crate::integrations::browser_runtime::relaunch_runtime_for_chat(
        app.clone(),
        chat_id,
        profile_dir,
        launch_options,
        window_bounds,
    )
    .await?;
    let browser_runtimes = app.integrations.browser_runtimes.clone();
    let browser_runtimes = browser_runtimes.lock().await;
    browser_runtimes
        .get(&runtime_id)
        .cloned()
        .ok_or_else(|| format!("Relaunched BrowserRuntime {} disappeared", runtime_id))
}


async fn validate_upload_paths(
    gcx: Arc<GlobalContext>,
    request: &BrowserActionRequest,
) -> Result<(), String> {
    for (idx, step) in request.steps.iter().enumerate() {
        let (action, paths) = match step {
            BrowserStep::SetInputFiles { paths, .. } => ("set_input_files", paths.as_slice()),
            BrowserStep::ExpectFileChooser { paths } => ("expect_file_chooser", paths.as_slice()),
            BrowserStep::DropFiles { paths, .. } => ("drop_files", paths.as_slice()),
            _ => continue,
        };
        for path in paths {
            validate_upload_path(gcx.clone(), path)
                .await
                .map_err(|error| format!("step[{idx}] ({action}): {error}"))?;
        }
    }
    Ok(())
}

async fn validate_upload_path(gcx: Arc<GlobalContext>, path: &str) -> Result<(), String> {
    let path = PathBuf::from(path);
    crate::files_correction::check_if_its_inside_a_workspace_or_config(gcx.clone(), &path)
        .await
        .map_err(|error| format!("Upload path is not allowed: {error}"))?;
    let canonical = path
        .canonicalize()
        .map_err(|_| format!("Upload path does not exist: {}", path.display()))?;
    if canonical.is_file() {
        crate::files_in_workspace::check_file_privacy_for_send(gcx.clone(), &canonical)
            .await
            .map_err(|error| format!("Upload path is blocked by privacy rules: {error}"))?;
    } else if canonical.is_dir() {
        for entry in walkdir::WalkDir::new(&canonical) {
            let entry = entry.map_err(|error| {
                format!(
                    "Failed to inspect upload directory {}: {error}",
                    canonical.display()
                )
            })?;
            if entry.file_type().is_symlink() {
                return Err(format!(
                    "Upload directories cannot contain symbolic links: {}",
                    entry.path().display()
                ));
            }
            if entry.file_type().is_file() {
                crate::files_in_workspace::check_file_privacy_for_send(
                    gcx.clone(),
                    &entry.path().to_path_buf(),
                )
                .await
                .map_err(|error| format!("Upload path is blocked by privacy rules: {error}"))?;
            }
        }
    } else {
        return Err(format!(
            "Upload path is not a file or directory: {}",
            canonical.display()
        ));
    }
    Ok(())
}


#[cfg(test)]
mod tests {
    use super::*;
    use refact_browser::is_transport_dead_error;

    const DEAD_TRANSPORT: &str =
        "Unable to make method calls because underlying connection is closed";

    fn report_with_step_error(error: Option<&str>) -> ExecutionReport {
        let step = match error {
            Some(error) => StepResult::failure(0, "OpenTab", error.to_string()),
            None => StepResult::success(0, "OpenTab"),
        };
        report_with_steps(vec![step])
    }

    fn report_with_steps(steps: Vec<StepResult>) -> ExecutionReport {
        ExecutionReport {
            ok: steps.iter().all(|step| step.ok),
            steps,
            warnings: Vec::new(),
            url: None,
            title: None,
            page: None,
            stabilized: false,
            console: vec![],
            page_errors: vec![],
            network: vec![],
            network_summary: vec![],
            websockets: vec![],
            locator_handlers: vec![],
            dialogs: vec![],
            uploads: vec![],
            downloads: vec![],
            new_tabs: vec![],
            active_routes: vec![],
            intercepted_requests: vec![],
            context: None,
            screenshot: None,
        }
    }

    fn navigate_step(url: &str) -> BrowserStep {
        BrowserStep::Navigate {
            url: url.to_string(),
            timeout_ms: None,
        }
    }

    fn screenshot_step() -> BrowserStep {
        serde_json::from_value(serde_json::json!({"action": "screenshot"})).unwrap()
    }

    #[test]
    fn dead_transport_is_detected_from_step_errors_and_dispatch_errors() {
        assert!(report_hit_dead_transport(&Ok(report_with_step_error(
            Some(DEAD_TRANSPORT)
        ))));
        assert!(report_hit_dead_transport(&Err(
            "MethodCallError(ConnectionClosed)".to_string()
        )));

        assert!(!report_hit_dead_transport(&Ok(report_with_step_error(
            Some("Timed out after 5000ms")
        ))));
        assert!(!report_hit_dead_transport(&Ok(report_with_step_error(
            None
        ))));
        assert!(!report_hit_dead_transport(&Err(
            "No tab found with id=abc".to_string()
        )));
    }

    #[test]
    fn plan_resume_starts_at_the_first_unfinished_or_dead_step() {
        let all_ok = report_with_steps(vec![
            StepResult::success(0, "Navigate"),
            StepResult::success(1, "Click"),
        ]);
        assert_eq!(plan_resume(&all_ok, 2).resume_index, 2);
        assert_eq!(plan_resume(&all_ok, 2).completed_steps.len(), 2);

        let dead_first = report_with_steps(vec![StepResult::failure(0, "Click", DEAD_TRANSPORT)]);
        assert_eq!(plan_resume(&dead_first, 3).resume_index, 0);
        assert!(plan_resume(&dead_first, 3).completed_steps.is_empty());

        let dead_midway = report_with_steps(vec![
            StepResult::success(0, "Navigate"),
            StepResult::success(1, "Fill"),
            StepResult::failure(2, "Click", DEAD_TRANSPORT),
        ]);
        let resume = plan_resume(&dead_midway, 4);
        assert_eq!(resume.resume_index, 2);
        assert_eq!(
            resume
                .completed_steps
                .iter()
                .map(|step| step.step_index)
                .collect::<Vec<_>>(),
            vec![0, 1]
        );

        let soft_failure = report_with_steps(vec![
            StepResult::success(0, "Navigate"),
            StepResult::failure(1, "Expect", "Timeout 5000ms exceeded"),
            StepResult::failure(2, "Click", DEAD_TRANSPORT),
        ]);
        let resume = plan_resume(&soft_failure, 3);
        assert_eq!(resume.resume_index, 2);
        assert_eq!(resume.completed_steps.len(), 2);
        assert!(!resume.completed_steps[1].ok);

        let nothing_ran = report_with_steps(Vec::new());
        assert_eq!(plan_resume(&nothing_ran, 3).resume_index, 0);
        assert!(plan_resume(&nothing_ran, 3).completed_steps.is_empty());
    }

    #[test]
    fn merged_report_keeps_original_indices_and_both_attempt_warnings() {
        let mut attempt = report_with_steps(vec![
            StepResult::success(0, "Navigate"),
            StepResult::success(1, "Fill"),
            StepResult::failure(2, "Click", DEAD_TRANSPORT),
        ]);
        attempt.warnings.push("attempt one warning".to_string());
        let resume = plan_resume(&attempt, 4);

        let mut retry = report_with_steps(vec![
            StepResult::success(0, "Click"),
            StepResult::success(1, "Screenshot"),
        ]);
        retry.warnings.push("attempt two warning".to_string());

        let merged = merge_resumed_report(resume, retry);
        assert_eq!(
            merged
                .steps
                .iter()
                .map(|step| step.step_index)
                .collect::<Vec<_>>(),
            vec![0, 1, 2, 3]
        );
        assert_eq!(
            merged
                .steps
                .iter()
                .map(|step| step.summary.clone())
                .collect::<Vec<_>>(),
            vec!["Navigate", "Fill", "Click", "Screenshot"]
        );
        assert_eq!(
            merged.warnings,
            vec![
                "attempt one warning".to_string(),
                crate::integrations::browser_runtime::relaunch_resume_warning(2),
                "attempt two warning".to_string(),
            ]
        );
    }

    #[test]
    fn dispatch_boundary_relaunches_and_retries_once_with_a_visible_warning() {
        let attempt = report_with_steps(vec![
            StepResult::success(0, "Navigate"),
            StepResult::success(1, "Fill"),
            StepResult::failure(2, "Click", DEAD_TRANSPORT),
        ]);
        assert!(report_hit_dead_transport(&Ok(attempt.clone())));

        let resume = plan_resume(&attempt, 4);
        let replayed = resume.resume_index;
        assert_eq!(replayed, 2);
        assert!(resume.completed_steps.iter().all(|step| step.ok));

        let warning = crate::integrations::browser_runtime::relaunch_resume_warning(replayed);
        assert!(warning.contains("relaunched and resumed from step 2"));
        assert!(warning.contains("cookies/storage persist via profile"));

        let retry = report_with_steps(vec![
            StepResult::success(0, "Click"),
            StepResult::success(1, "Screenshot"),
        ]);
        let merged = merge_resumed_report(resume, retry);
        assert!(merged.warnings.contains(&warning));
        assert_eq!(merged.steps.len(), 4);
        assert_eq!(
            merged
                .steps
                .iter()
                .filter(|step| step.summary == "Click")
                .count(),
            1
        );
        assert!(crate::integrations::browser_runtime::RELAUNCH_WARNING
            .contains("relaunched and retried"));
    }

    #[test]
    fn unexecuted_steps_are_marked_skipped_instead_of_vanishing() {
        let mut results = vec![
            StepResult::success(0, "Navigate"),
            StepResult::failure(1, "Click", "Timeout 5000ms exceeded"),
        ];
        mark_unexecuted_steps_skipped(&mut results, 4);
        assert_eq!(results.len(), 4);
        assert!(results[2].skipped && results[3].skipped);
        assert!(!results[2].ok);
        assert!(results[2].summary.contains("step 1 failed earlier"));
        assert!(!results[0].skipped && !results[1].skipped);
        assert!(results[1].was_executed());
        assert!(!results[2].was_executed());

        let serialized = serde_json::to_value(&results).unwrap();
        assert_eq!(serialized[2]["skipped"], true);
        assert!(serialized[0].get("skipped").is_none());

        let mut complete = vec![StepResult::success(0, "Navigate")];
        mark_unexecuted_steps_skipped(&mut complete, 1);
        assert_eq!(complete.len(), 1);
    }

    #[test]
    fn plan_resume_treats_skipped_placeholders_as_unfinished() {
        let attempt = report_with_steps(vec![
            StepResult::success(0, "Navigate"),
            StepResult::failure(1, "Click", DEAD_TRANSPORT),
            StepResult::skipped(2, 1),
            StepResult::skipped(3, 1),
        ]);
        let resume = plan_resume(&attempt, 4);
        assert_eq!(resume.resume_index, 1);
        assert_eq!(resume.completed_steps.len(), 1);
    }

    #[test]
    fn resume_replays_the_last_navigation_before_the_dead_step() {
        let steps = vec![
            navigate_step("https://example.test/a"),
            screenshot_step(),
            navigate_step("https://example.test/b"),
            screenshot_step(),
            screenshot_step(),
        ];
        let attempt = report_with_steps(vec![
            StepResult::success(0, "Navigated to a"),
            StepResult::success(1, "Screenshot"),
            StepResult::success(2, "Navigated to b"),
            StepResult::failure(3, "Screenshot", DEAD_TRANSPORT),
            StepResult::skipped(4, 3),
        ]);
        let resume = plan_resume(&attempt, steps.len()).with_replay_from(&steps);
        assert_eq!(resume.resume_index, 3);
        let (replayed_index, replayed) = resume.replay.clone().expect("navigation to replay");
        assert_eq!(replayed_index, 2);
        assert!(matches!(replayed, BrowserStep::Navigate { ref url, .. } if url.ends_with("/b")));

        let retry = report_with_steps(vec![
            StepResult::success(0, "Navigated to b (replayed)"),
            StepResult::success(1, "Screenshot"),
            StepResult::success(2, "Screenshot"),
        ]);
        let merged = merge_resumed_report(resume, retry);
        assert!(merged.ok);
        assert_eq!(
            merged
                .steps
                .iter()
                .map(|step| step.step_index)
                .collect::<Vec<_>>(),
            vec![0, 1, 2, 3, 4]
        );
        assert_eq!(merged.steps[3].summary, "Screenshot");
        assert!(merged.steps.iter().all(|step| step.ok && !step.skipped));
        let warning = merged
            .warnings
            .iter()
            .find(|warning| warning.contains("replayed step 2"))
            .expect("replay warning");
        assert!(warning.contains("resumed from step 3"));
        assert!(!warning.contains("NOT replayed"));
    }

    #[test]
    fn resume_replay_warning_names_the_steps_that_were_not_replayed() {
        let steps = vec![
            navigate_step("https://example.test/a"),
            serde_json::from_value(serde_json::json!({
                "action": "fill",
                "locator": {"by": "css", "value": "#q"},
                "text": "hello"
            }))
            .unwrap(),
            screenshot_step(),
        ];
        let attempt = report_with_steps(vec![
            StepResult::success(0, "Navigated to a"),
            StepResult::success(1, "Filled #q"),
            StepResult::failure(2, "Screenshot", DEAD_TRANSPORT),
        ]);
        let resume = plan_resume(&attempt, steps.len()).with_replay_from(&steps);
        assert_eq!(resume.replay.as_ref().map(|(idx, _)| *idx), Some(0));
        let retry = report_with_steps(vec![
            StepResult::success(0, "Navigated to a"),
            StepResult::success(1, "Screenshot"),
        ]);
        let merged = merge_resumed_report(resume, retry);
        assert_eq!(merged.steps.len(), 3);
        assert!(merged
            .warnings
            .iter()
            .any(|warning| warning.contains("steps 1..=1 were NOT replayed")));
    }

    #[test]
    fn resume_fails_loudly_when_the_replayed_navigation_fails() {
        let steps = vec![
            navigate_step("https://example.test/a"),
            screenshot_step(),
            screenshot_step(),
        ];
        let attempt = report_with_steps(vec![
            StepResult::success(0, "Navigated to a"),
            StepResult::failure(1, "Screenshot", DEAD_TRANSPORT),
            StepResult::skipped(2, 1),
        ]);
        let resume = plan_resume(&attempt, steps.len()).with_replay_from(&steps);
        let retry = report_with_steps(vec![
            StepResult::failure(0, "Navigate", "net::ERR_CONNECTION_REFUSED"),
            StepResult::skipped(1, 0),
            StepResult::skipped(2, 0),
        ]);
        let merged = merge_resumed_report(resume, retry);
        assert!(!merged.ok);
        assert_eq!(merged.steps.len(), 3);
        assert!(merged.steps[0].ok);
        assert!(!merged.steps[1].ok && !merged.steps[1].skipped);
        assert!(merged.steps[1].error.as_deref().unwrap().contains(
            "replayed step 0 to restore the document, but it failed: net::ERR_CONNECTION_REFUSED"
        ));
        assert!(merged.steps[2].skipped);
    }

    #[test]
    fn resume_is_refused_when_no_navigation_precedes_the_dead_step() {
        let steps = vec![screenshot_step(), screenshot_step(), screenshot_step()];
        let attempt = report_with_steps(vec![
            StepResult::success(0, "Screenshot"),
            StepResult::failure(1, "Screenshot", DEAD_TRANSPORT),
            StepResult::skipped(2, 1),
        ]);
        let resume = plan_resume(&attempt, steps.len()).with_replay_from(&steps);
        assert!(resume.replay.is_none());
        assert_eq!(resume.resume_index, 1);

        let refused = resume_refused_without_page_context(resume);
        assert!(!refused.ok);
        assert_eq!(refused.url.as_deref(), Some("about:blank"));
        assert_eq!(refused.steps.len(), 3);
        assert!(refused.steps[0].ok);
        assert!(!refused.steps[1].ok && !refused.steps[1].skipped);
        assert!(refused.steps[2].skipped);
        assert!(refused.warnings.iter().any(
            |warning| warning.contains("no navigate/open_tab/set_content step precedes step 1")
        ));
    }
}
