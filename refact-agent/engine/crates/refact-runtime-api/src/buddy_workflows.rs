use std::future::Future;

use chrono::Utc;

use refact_buddy_core::actor::{make_runtime_event, validate_workflow_id, BuddyService};
use refact_buddy_core::events::DiagnosticBuddyEvent as BuddyEvent;
use refact_buddy_core::state::{complete_active_quest, grant_xp};
use refact_buddy_core::types::{BuddyActivity, BuddyWorkflowSummary};
use refact_buddy_core::workflows::{canonical_signal_type, workflow_failure_report, workflow_label};

use crate::app_state::AppState;
use crate::buddy_actor_runtime::buddy_update_speech;
use crate::buddy_runtime::{buddy_apply, buddy_enqueue_event};
use crate::buddy_speech_runtime::complete_quest_with_voice;
use crate::files_correction::get_project_dirs;

pub async fn buddy_wrap_workflow<T, F, Fut>(
    gcx: AppState,
    workflow_id: &str,
    icon: &str,
    xp: u64,
    summary_fn: impl Fn(&T) -> String,
    workflow_fn: F,
) -> Result<T, String>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<T, String>>,
{
    if !validate_workflow_id(workflow_id) {
        return Err(format!("invalid workflow_id: {workflow_id}"));
    }

    let label = workflow_label(workflow_id);
    let signal_type = canonical_signal_type(workflow_id);
    let dedupe_key = format!("workflow_{}", workflow_id);
    let mut started = make_runtime_event(
        signal_type,
        &format!("Running {}...", label),
        "system",
        &dedupe_key,
        "started",
        None,
    );
    started.speech_text = Some(format!("I'm working on {}...", label));
    started.scene = Some("working".to_string());
    started.persistent = true;
    buddy_enqueue_event(gcx.clone(), started).await;

    let result = workflow_fn().await;

    let (success, summary) = match &result {
        Ok(output) => (true, summary_fn(output)),
        Err(e) => (false, e.clone()),
    };

    let failure_report = result
        .as_ref()
        .err()
        .map(|error| workflow_failure_report(workflow_id, error, None));

    let buddy_arc = gcx.buddy.buddy.clone();
    let voice_gcx = gcx.clone();
    let project_dirs = get_project_dirs(gcx.gcx.clone()).await;
    let project_root = project_dirs.into_iter().next();
    let workflow_id_owned = workflow_id.to_string();
    let icon_owned = icon.to_string();

    let handle = tokio::spawn(async move {
        let activity = BuddyActivity {
            icon: icon_owned,
            title: summary.clone(),
            description: String::new(),
            timestamp: Utc::now().to_rfc3339(),
            activity_type: "workflow".to_string(),
            chat_id: None,
            failure_category: None,
            failure_summary: None,
        };

        let mut completed_quest = None;
        let mut quest_voice_state = None;
        let mut failure_write = None;
        {
            let mut buddy = buddy_arc.lock().await;
            if let Some(svc) = buddy.as_mut() {
                svc.complete_runtime_event(&dedupe_key, "completed");
                if success {
                    svc.add_activity(activity);
                    grant_xp(&mut svc.state, xp);
                    let now = Utc::now().to_rfc3339();
                    if let Some(ws) = svc
                        .state
                        .workflow_summaries
                        .iter_mut()
                        .find(|ws| ws.workflow_id == workflow_id_owned)
                    {
                        ws.run_count = ws.run_count.saturating_add(1);
                        ws.last_run = Some(now.clone());
                        ws.last_outcome = Some("success".to_string());
                        ws.failure_category = None;
                        ws.failure_summary = None;
                    } else {
                        svc.state.workflow_summaries.push(BuddyWorkflowSummary {
                            workflow_id: workflow_id_owned.clone(),
                            last_run: Some(now.clone()),
                            run_count: 1,
                            last_outcome: Some("success".to_string()),
                            failure_category: None,
                            failure_summary: None,
                            ..Default::default()
                        });
                    }
                    svc.refresh_active_quest();
                    svc.dirty = true;
                    let _ = svc.events_tx.send(BuddyEvent::StateUpdated {
                        state: svc.state.clone(),
                    });
                    let reward = svc
                        .state
                        .active_quest
                        .as_ref()
                        .filter(|quest| quest.status == "active" && quest.progress >= quest.goal)
                        .map(|quest| quest.reward_xp);
                    if let Some(reward) = reward {
                        completed_quest = complete_active_quest(&mut svc.state);
                        quest_voice_state = Some((
                            svc.state.personality.clone(),
                            svc.state.identity.name.clone(),
                            svc.pulse.clone(),
                            reward,
                        ));
                        svc.dirty = true;
                        let _ = svc.events_tx.send(BuddyEvent::StateUpdated {
                            state: svc.state.clone(),
                        });
                    }
                } else if let Some(report) = failure_report.as_ref() {
                    failure_write = svc.record_workflow_failure_report(report.clone());
                } else {
                    tracing::warn!(
                        "buddy: workflow {} failed without a failure report",
                        workflow_id_owned
                    );
                }
            }
        }

        if let Some((path, report)) = failure_write {
            BuddyService::append_workflow_failure_transcript(&path, &report).await;
        } else if let (false, Some(report), Some(root)) =
            (success, failure_report.as_ref(), project_root.as_ref())
        {
            let path = root.join(format!(
                ".refact/buddy/chats/workflows/{}.json",
                report.workflow_id
            ));
            BuddyService::append_workflow_failure_transcript(&path, report).await;
        } else if let Some(ref root) = project_root {
            BuddyService::append_workflow_transcript_to_path(
                root,
                &workflow_id_owned,
                &summary,
                success,
            )
            .await;
        }

        if let (Some(quest), Some((persona, identity_name, pulse, reward))) =
            (completed_quest, quest_voice_state)
        {
            let completed = complete_quest_with_voice(
                voice_gcx.clone(),
                quest,
                persona,
                identity_name,
                pulse,
            )
            .await;
            buddy_update_speech(voice_gcx.clone(), completed.speech).await;
            buddy_apply(voice_gcx.clone(), completed.mutation).await;
            if reward > 0 {
                let buddy_arc = voice_gcx.buddy.buddy.clone();
                let mut buddy = buddy_arc.lock().await;
                if let Some(svc) = buddy.as_mut() {
                    svc.grant_xp(reward);
                }
            }
        }
    });

    let untracked_handle = {
        let mut buddy = gcx.buddy.buddy.lock().await;
        if let Some(svc) = buddy.as_mut() {
            svc.track_background_task(handle);
            None
        } else {
            Some(handle)
        }
    };
    if let Some(handle) = untracked_handle {
        let _ = handle.await;
    }

    result
}
