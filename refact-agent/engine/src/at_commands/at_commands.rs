pub use refact_core::at_parse::filter_only_context_file_from_context_tool;
pub use refact_runtime_api::at_commands_impl::at_command_dict::engine_at_commands_dict;
pub use refact_runtime_api::{
    at_commands_dict, at_commands_new_from_app, at_commands_new_with_abort,
    at_commands_new_with_abort_and_execution_scope, vec_context_file_to_context_tools, AtCommand,
    AtCommandsContext, AtParam, MAX_SUBCHAT_DEPTH,
};

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn sample_worktree() -> (tempfile::TempDir, crate::worktrees::types::WorktreeMeta) {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("worktree");
        let source = temp.path().join("source");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&source).unwrap();
        let root = dunce::simplified(&fs::canonicalize(root).unwrap()).to_path_buf();
        let source = dunce::simplified(&fs::canonicalize(source).unwrap()).to_path_buf();
        (
            temp,
            crate::worktrees::types::WorktreeMeta {
                id: "wt-context".to_string(),
                kind: "chat".to_string(),
                root,
                source_workspace_root: source.clone(),
                repo_root: source,
                branch: Some("feature".to_string()),
                base_branch: Some("main".to_string()),
                base_commit: Some("base".to_string()),
                task_id: None,
                card_id: None,
                agent_id: None,
                enforce: true,
            },
        )
    }

    #[tokio::test]
    async fn subchat_worktree_at_commands_context_has_execution_scope() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let (_temp, worktree) = sample_worktree();
        let ccx = at_commands_new_from_app(
            crate::app_state::app_state_from_gcx(gcx).await,
            4096,
            20,
            false,
            vec![],
            "chat-1".to_string(),
            None,
            "model".to_string(),
            None,
            Some(worktree.clone()),
        )
        .await;

        assert!(ccx.execution_scope.is_some());
        assert_eq!(ccx.execution_scope_root(), Some(worktree.root.clone()));
        assert_eq!(ccx.effective_project_dirs(), vec![worktree.root.clone()]);
        assert_eq!(ccx.execution_scope_worktree(), Some(worktree));
    }
}
