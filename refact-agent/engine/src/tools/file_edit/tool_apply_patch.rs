pub use refact_tools::tools::file_edit::tool_apply_patch::*;

#[cfg(test)]
mod tests {
    use super::absolute_path_inside_workspace;
    use std::path::PathBuf;

    #[test]
    fn absolute_workspace_path_keeps_disambiguating_root() {
        let project_dirs = vec![PathBuf::from("/repo/main"), PathBuf::from("/repo/worktree")];
        let path = "/repo/main/refact-agent/engine/src/subchat.rs";

        assert_eq!(
            absolute_path_inside_workspace(path, &project_dirs),
            Some(PathBuf::from(path))
        );
    }

    #[test]
    fn relative_workspace_path_is_not_resolved_as_absolute() {
        let project_dirs = vec![PathBuf::from("/repo/main")];

        assert_eq!(
            absolute_path_inside_workspace("refact-agent/engine/src/subchat.rs", &project_dirs),
            None
        );
    }

    #[test]
    fn absolute_path_outside_workspaces_is_rejected_by_helper() {
        let project_dirs = vec![PathBuf::from("/repo/main")];

        assert_eq!(
            absolute_path_inside_workspace("/tmp/subchat.rs", &project_dirs),
            None
        );
    }
}
