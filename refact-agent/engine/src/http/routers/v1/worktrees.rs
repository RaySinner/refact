pub use refact_http::routers::v1::worktrees::*;

#[cfg(test)]
#[allow(unused_imports)]
use refact_runtime_api::GlobalContextCtx;
#[cfg(test)]
#[allow(unused_imports)]
use hyper::StatusCode;
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_for_error_maps_user_correctable_worktree_states_to_conflict() {
        for message in [
            "Worktree has uncommitted changes; set include_uncommitted=true",
            "Target workspace has uncommitted changes on a different branch",
            "Target workspace has uncommitted changes on detached HEAD",
            "Failed to auto-commit source worktree changes",
            "Branch 'refact/chat/test' already exists",
            "Worktree 'wt_1' is not a git worktree",
            "Worktree 'wt_1' path '/tmp/wt_1' is not the git worktree root",
            "Worktree 'wt_1' path '/tmp/wt_1' belongs to a different repository",
            "Worktree 'wt_1' still has 1 active reference(s) (kinds: chat); pass force_referenced=true to delete anyway.",
        ] {
            assert_eq!(status_for_error(message), StatusCode::CONFLICT, "{message}");
        }
    }

    #[test]
    fn status_for_error_maps_missing_worktree_path_to_not_found() {
        assert_eq!(
            status_for_error("Worktree 'wt_1' path '/tmp/wt_1' does not exist"),
            StatusCode::NOT_FOUND
        );
    }

    #[test]
    fn status_for_error_maps_invalid_worktree_id_to_bad_request() {
        for message in [
            "Worktree ID must contain only ASCII alphanumeric characters, hyphens, or underscores",
            "Worktree ID is too long",
        ] {
            assert_eq!(
                status_for_error(message),
                StatusCode::BAD_REQUEST,
                "{message}"
            );
        }
    }
}
