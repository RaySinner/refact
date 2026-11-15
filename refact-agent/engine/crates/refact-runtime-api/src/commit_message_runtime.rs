//! Commit-message generation orchestration: the subchat call plus the buddy
//! workflow wrapper. The pure diff/prompt/message helpers stay in
//! `refact_agentic::generate_commit_message`; everything here only needs the
//! `ChatCtx` seams (subchat launcher, mode config, buddy workflow sink).

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use hashbrown::HashMap;
use tracing::warn;

use refact_agentic::generate_commit_message::{build_messages, extract_commit_message, SUBAGENT_ID};
use refact_chat_api::TraceParent;
use refact_files::path_utils::CommandSimplifiedDirExt;
use refact_files::vcs_detect::detect_vcs_for_a_file_path;

use crate::chat_ctx::ChatCtx;
use crate::WorkflowFuture;

pub async fn generate_commit_message_for_projects(
    ctx: ChatCtx,
) -> Result<HashMap<PathBuf, String>, String> {
    let project_folders = ctx.workspace.workspace_folders.lock().unwrap().clone();
    let mut commit_messages = HashMap::new();

    for folder in project_folders {
        let Some((_, vcs_type)) = detect_vcs_for_a_file_path(&folder).await else {
            warn!("There's no recognizable version control detected for the folder {folder:?}");
            continue;
        };
        let Some(command) = refact_agentic::generate_commit_message::diff_command_for_vcs(vcs_type)
        else {
            warn!("Unrecognizable version control detected for the folder {folder:?}: {vcs_type}");
            continue;
        };

        let output = tokio::process::Command::new(command)
            .arg("diff")
            .current_dir_simplified(&folder)
            .stdin(std::process::Stdio::null())
            .output()
            .await
            .map_err(|e| format!("Failed to execute command for folder {folder:?}: {e}"))?;

        if !output.status.success() {
            warn!(
                "Command failed for folder {folder:?}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            continue;
        }

        let diff_output = String::from_utf8_lossy(&output.stdout).to_string();
        let commit_message =
            generate_commit_message_by_diff(ctx.clone(), &diff_output, &None, None).await?;
        commit_messages.insert(folder, commit_message);
    }

    Ok(commit_messages)
}

pub async fn generate_commit_message_by_diff(
    ctx: ChatCtx,
    diff: &String,
    commit_message_prompt: &Option<String>,
    parent_chat_id: Option<&str>,
) -> Result<String, String> {
    generate_commit_message_by_diff_with_abort(
        ctx,
        diff,
        commit_message_prompt,
        None,
        parent_chat_id,
    )
    .await
}

pub async fn generate_commit_message_by_diff_with_abort(
    ctx: ChatCtx,
    diff: &String,
    commit_message_prompt: &Option<String>,
    abort_flag: Option<Arc<AtomicBool>>,
    parent_chat_id: Option<&str>,
) -> Result<String, String> {
    refact_agentic::generate_commit_message::validate_diff(diff)?;
    let diff = diff.clone();
    let commit_message_prompt = commit_message_prompt.clone();
    let ctx2 = ctx.clone();
    let parent_chat_id = parent_chat_id.map(|id| id.to_string());
    let work: Box<dyn FnOnce() -> WorkflowFuture + Send + 'static> = Box::new(move || {
        Box::pin(async move {
            let subagent_config = ctx2
                .mode_config
                .get_subagent_config(SUBAGENT_ID, None)
                .await
                .ok_or_else(|| format!("subagent config '{}' not found", SUBAGENT_ID))?;

            let messages = build_messages(
                &diff,
                commit_message_prompt.as_deref(),
                subagent_config.prompts.diff_only.as_deref(),
                subagent_config.prompts.diff_with_user_text.as_deref(),
            )?;

            let result = ctx2
                .subchat
                .run_subchat_once_with_abort(
                    SUBAGENT_ID,
                    messages,
                    abort_flag,
                    TraceParent::from_parts(parent_chat_id.as_deref(), None),
                )
                .await
                .map_err(|e| format!("Error: {}", e))?;

            extract_commit_message(&result.messages)
        })
    });
    ctx.buddy_event_sink
        .run_workflow("commit_message", "📦", 5, "Commit message", 50, work)
        .await
}
