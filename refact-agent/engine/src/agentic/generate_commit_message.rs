pub use refact_agentic::generate_commit_message::{remove_fencing, SUBAGENT_ID};

use std::path::PathBuf;

use hashbrown::HashMap;
use tracing::warn;

use crate::files_correction::CommandSimplifiedDirExt;
use crate::files_in_workspace::detect_vcs_for_a_file_path;
use crate::global_context::GlobalContext;
use refact_runtime_api::GlobalContextCtx;

pub async fn generate_commit_message_by_diff(
    gcx: std::sync::Arc<GlobalContext>,
    diff: &String,
    commit_message_prompt: &Option<String>,
    parent_chat_id: Option<&str>,
) -> Result<String, String> {
    refact_runtime_api::commit_message_runtime::generate_commit_message_by_diff(
        gcx.chat_ctx(gcx.clone()),
        diff,
        commit_message_prompt,
        parent_chat_id,
    )
    .await
}

pub async fn _generate_commit_message_for_projects(
    gcx: std::sync::Arc<GlobalContext>,
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
