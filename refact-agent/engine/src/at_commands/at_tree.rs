pub use refact_runtime_api::at_commands_impl::at_tree::{
    AtTree, TreeSymbols, MAX_FILES_HARD_CAP,
};
pub use refact_runtime_api::at_command_executor::tree_for_tools_ex;
pub use refact_runtime_api::at_tree::{BuildBudget, TreeNode, MAX_TREE_PATHS};

#[cfg(test)]
mod bounded_work_tests {
    use super::*;
    use refact_runtime_api::at_commands_impl::at_tree::sanitize_max_files;
    use std::path::PathBuf;
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;

    fn count_leaves(node: &TreeNode) -> usize {
        if !node.is_dir() {
            return if node.source_path.is_some() { 1 } else { 0 };
        }
        node.children.values().map(count_leaves).sum()
    }

    #[test]
    fn sanitize_max_files_clamps_into_positive_bounded_range() {
        assert_eq!(
            sanitize_max_files(0),
            1,
            "zero must become a positive value"
        );
        assert_eq!(sanitize_max_files(10), 10);
        assert_eq!(sanitize_max_files(MAX_FILES_HARD_CAP), MAX_FILES_HARD_CAP);
        assert_eq!(
            sanitize_max_files(MAX_FILES_HARD_CAP + 1),
            MAX_FILES_HARD_CAP,
            "huge requests must be capped"
        );
        assert_eq!(sanitize_max_files(usize::MAX), MAX_FILES_HARD_CAP);
    }

    #[test]
    fn build_with_budget_bounds_enumeration_and_reports_truncation() {
        let base = PathBuf::from("/tmp/refact-bounded-work-fixture");
        let paths: Vec<PathBuf> = (0..500)
            .map(|i| base.join(format!("file_{i}.rs")))
            .collect();

        let mut budget = BuildBudget::new(7, None);
        let tree = TreeNode::build_relative_with_budget(&paths, &base, &mut budget);

        assert!(
            budget.truncated,
            "budget should be marked truncated when input exceeds the cap"
        );
        let leaves = count_leaves(&tree);
        assert!(
            leaves <= 7,
            "only budgeted paths should be enumerated, got {leaves}"
        );
        assert!(leaves > 0, "some paths should still be enumerated");
    }

    #[test]
    fn build_with_budget_not_truncated_when_within_budget() {
        let base = PathBuf::from("/tmp/refact-bounded-work-small");
        let paths: Vec<PathBuf> = (0..3).map(|i| base.join(format!("file_{i}.rs"))).collect();

        let mut budget = BuildBudget::new(MAX_TREE_PATHS, None);
        let tree = TreeNode::build_relative_with_budget(&paths, &base, &mut budget);

        assert!(!budget.truncated, "small input should not truncate");
        assert_eq!(count_leaves(&tree), 3);
    }

    #[test]
    fn build_with_budget_stops_immediately_when_aborted() {
        let base = PathBuf::from("/tmp/refact-bounded-work-abort");
        let paths: Vec<PathBuf> = (0..100)
            .map(|i| base.join(format!("file_{i}.rs")))
            .collect();

        let abort = Arc::new(AtomicBool::new(true));
        let mut budget = BuildBudget::new(MAX_TREE_PATHS, Some(abort));
        let tree = TreeNode::build_relative_with_budget(&paths, &base, &mut budget);

        assert!(budget.truncated, "aborted build should report truncation");
        assert_eq!(
            count_leaves(&tree),
            0,
            "no paths should be enumerated once aborted"
        );
    }
}
