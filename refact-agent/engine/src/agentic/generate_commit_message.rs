pub use refact_agentic::generate_commit_message::{remove_fencing, SUBAGENT_ID};

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use hashbrown::HashMap;
use tracing::warn;

use crate::files_correction::CommandSimplifiedDirExt;
use crate::files_in_workspace::detect_vcs_for_a_file_path;
use crate::global_context::GlobalContext;
use crate::subchat::{run_subchat_once_with_abort, TraceParent};
use crate::yaml_configs::customization_registry::get_subagent_config;

pub async fn generate_commit_message_by_diff(
    gcx: Arc<GlobalContext>,
    diff: &String,
    commit_message_prompt: &Option<String>,
    parent_chat_id: Option<&str>,
) -> Result<String, String> {
    generate_commit_message_by_diff_with_abort(
        gcx,
        diff,
        commit_message_prompt,
        None,
        parent_chat_id,
    )
    .await
}

pub async fn generate_commit_message_by_diff_with_abort(
    gcx: Arc<GlobalContext>,
    diff: &String,
    commit_message_prompt: &Option<String>,
    abort_flag: Option<Arc<AtomicBool>>,
    parent_chat_id: Option<&str>,
) -> Result<String, String> {
    refact_agentic::generate_commit_message::validate_diff(diff)?;
    let diff = diff.clone();
    let commit_message_prompt = commit_message_prompt.clone();
    let gcx2 = gcx.clone();
    let parent_chat_id = parent_chat_id.map(|id| id.to_string());
    crate::buddy::workflows::buddy_wrap_workflow(
        crate::app_state::app_state_from_gcx(gcx).await,
        "commit_message",
        "📦",
        5,
        |msg: &String| {
            let short: String = msg.lines().next().unwrap_or("").chars().take(50).collect();
            format!("Commit message: {}", short)
        },
        move || async move {
            let subagent_config = get_subagent_config(gcx2.clone(), SUBAGENT_ID, None)
                .await
                .ok_or_else(|| format!("subagent config '{}' not found", SUBAGENT_ID))?;

            let messages = refact_agentic::generate_commit_message::build_messages(
                &diff,
                commit_message_prompt.as_deref(),
                subagent_config.prompts.diff_only.as_deref(),
                subagent_config.prompts.diff_with_user_text.as_deref(),
            )?;

            let result = run_subchat_once_with_abort(
                gcx2,
                SUBAGENT_ID,
                messages,
                abort_flag,
                TraceParent::from_parts(parent_chat_id.as_deref(), None),
            )
            .await
            .map_err(|e| format!("Error: {}", e))?;

            refact_agentic::generate_commit_message::extract_commit_message(&result.messages)
        },
    )
    .await
}

pub async fn _generate_commit_message_for_projects(
    gcx: Arc<GlobalContext>,
) -> Result<HashMap<PathBuf, String>, String> {
    let project_folders = gcx
        .documents_state
        .workspace_folders
        .lock()
        .unwrap()
        .clone();
    let mut commit_messages = HashMap::new();

    for folder in project_folders {
        let Some((_, vcs_type)) = detect_vcs_for_a_file_path(&folder).await else {
            warn!("There's no recognizable version control detected for the folder {folder:?}");
            continue;
        };
        let Some(command) =
            refact_agentic::generate_commit_message::diff_command_for_vcs(vcs_type)
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
            generate_commit_message_by_diff(gcx.clone(), &diff_output, &None, None).await?;
        commit_messages.insert(folder, commit_message);
    }

    Ok(commit_messages)
}

