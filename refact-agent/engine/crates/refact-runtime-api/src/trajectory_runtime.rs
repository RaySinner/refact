use refact_chat_api::{ChatMessage, TaskMeta, ThreadParams, TrajectoryCommitIntent};
use refact_chat_history::trajectory_model::LoadedTrajectory;
use refact_chat_history::trajectory_snapshot::TrajectorySnapshot;
use refact_perf::perf_diagnostics::{self, PerfComponent, PerfOutcome};

use crate::chat_ctx::ChatCtx;

pub async fn load_trajectory_for_chat(ctx: &ChatCtx, chat_id: &str) -> Option<LoadedTrajectory> {
    ctx.trajectories.load_trajectory_for_chat(chat_id).await
}

fn task_meta_is_planner_for_task(task_meta: Option<&TaskMeta>, task_id: &str) -> bool {
    task_meta.is_some_and(|meta| meta.role == "planner" && meta.task_id == task_id)
}

pub async fn chat_id_is_planner_for_task(ctx: &ChatCtx, chat_id: &str, task_id: &str) -> bool {
    let session_arc = {
        let sessions = ctx.chat.sessions.read().await;
        sessions.get(chat_id).cloned()
    };
    if let Some(session_arc) = session_arc {
        let session = session_arc.lock().await;
        if task_meta_is_planner_for_task(session.thread.task_meta.as_ref(), task_id) {
            return true;
        }
    }

    load_trajectory_for_chat(ctx, chat_id)
        .await
        .is_some_and(|loaded| {
            task_meta_is_planner_for_task(loaded.thread.task_meta.as_ref(), task_id)
        })
}

pub async fn verified_planner_linked_root_chat_id(
    ctx: &ChatCtx,
    current_chat_id: &str,
    current_root_chat_id: Option<&str>,
    task_meta: &TaskMeta,
) -> Option<String> {
    if task_meta.role == "planner" {
        return None;
    }
    let root_chat_id =
        current_root_chat_id.filter(|id| !id.is_empty() && *id != current_chat_id)?;
    if task_meta.planner_chat_id.as_deref() == Some(root_chat_id) {
        return Some(root_chat_id.to_string());
    }
    if chat_id_is_planner_for_task(ctx, root_chat_id, &task_meta.task_id).await {
        return Some(root_chat_id.to_string());
    }
    None
}

pub async fn resolve_task_planner_controller_chat_id(
    ctx: &ChatCtx,
    current_chat_id: &str,
    current_root_chat_id: Option<&str>,
    task_meta: Option<&TaskMeta>,
) -> String {
    let Some(task_meta) = task_meta else {
        return current_chat_id.to_string();
    };
    if let Some(root_chat_id) =
        verified_planner_linked_root_chat_id(ctx, current_chat_id, current_root_chat_id, task_meta)
            .await
    {
        return root_chat_id;
    }
    if task_meta.role == "planner" {
        return task_meta
            .planner_chat_id
            .clone()
            .filter(|id| {
                !id.is_empty()
                    && current_root_chat_id.filter(|root_id| *root_id != current_chat_id)
                        != Some(id.as_str())
            })
            .unwrap_or_else(|| current_chat_id.to_string());
    }
    task_meta
        .planner_chat_id
        .clone()
        .filter(|id| !id.is_empty())
        .unwrap_or_else(|| current_chat_id.to_string())
}

pub async fn save_trajectory_as_with_intent_checked(
    ctx: &ChatCtx,
    thread: &ThreadParams,
    messages: &[ChatMessage],
    intent: TrajectoryCommitIntent,
) -> Result<(), String> {
    if !intent.persists() {
        return Ok(());
    }
    let snapshot = TrajectorySnapshot::from_thread_parts(
        thread.id.clone(),
        thread,
        messages.to_vec(),
        chrono::Utc::now().to_rfc3339(),
        1,
    );
    ctx.trajectories
        .persist_trajectory_snapshot_with_intent(snapshot, intent)
        .await
}

pub async fn save_trajectory_snapshot_for_ctx(
    ctx: &ChatCtx,
    snapshot: TrajectorySnapshot,
) -> Result<(), String> {
    let message_count = snapshot.messages.len() as u64;
    let trajectory_version = snapshot.version;
    let span = perf_diagnostics::span(
        PerfComponent::TrajectoryCommit,
        Some(&snapshot.chat_id),
        None,
    );
    let result = ctx
        .trajectories
        .persist_trajectory_snapshot_with_intent(snapshot, TrajectoryCommitIntent::Required)
        .await;
    span.finish(
        if result.is_ok() {
            PerfOutcome::Success
        } else {
            PerfOutcome::Failure
        },
        None,
        Some(message_count),
        Some(trajectory_version),
        None,
    );
    result
}
