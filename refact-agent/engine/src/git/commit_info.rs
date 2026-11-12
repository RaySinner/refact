use std::sync::Arc;
use tracing::error;

use crate::agentic::generate_commit_message::generate_commit_message_by_diff;
use crate::git::CommitInfo;
use crate::global_context::GlobalContext;

pub async fn get_commit_information_from_current_changes(
    gcx: Arc<GlobalContext>,
) -> Vec<CommitInfo> {
    let workspace_vcs_roots_arc = gcx.documents_state.workspace_vcs_roots.clone();
    let workspace_vcs_roots = workspace_vcs_roots_arc.lock().unwrap().clone();

    tracing::info!(
        "get_commit_information_from_current_changes() vcs_roots={:?}",
        workspace_vcs_roots
    );
    refact_git::commit_info::commit_information_from_vcs_roots(&workspace_vcs_roots)
}

pub async fn generate_commit_messages(
    gcx: Arc<GlobalContext>,
    commits: Vec<CommitInfo>,
) -> Vec<CommitInfo> {
    let mut commits_with_messages = Vec::new();
    for mut commit in commits {
        let project_path = commit.project_path.to_file_path().ok().unwrap_or_default();

        let diff = match refact_git::commit_info::workdir_diff_for_project_root(&project_path) {
            Ok(Some(diff)) => diff,
            Ok(None) => continue,
            Err(e) => {
                error!("{}", e);
                continue;
            }
        };

        commit.commit_message =
            match generate_commit_message_by_diff(gcx.clone(), &diff, &None, None).await {
                Ok(msg) => msg,
                Err(e) => {
                    error!("{}", e);
                    continue;
                }
            };

        commits_with_messages.push(commit);
    }

    commits_with_messages
}
