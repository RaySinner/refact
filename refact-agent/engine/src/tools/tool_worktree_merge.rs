pub use refact_tools::tools::tool_worktree_merge::*;

#[cfg(test)]
#[allow(unused_imports)]
use std::collections::HashMap;
#[cfg(test)]
#[allow(unused_imports)]
use std::sync::Arc;
#[cfg(test)]
#[allow(unused_imports)]
use async_trait::async_trait;
#[cfg(test)]
#[allow(unused_imports)]
use serde_json::Value;
#[cfg(test)]
#[allow(unused_imports)]
use tokio::sync::Mutex as AMutex;
#[cfg(test)]
#[allow(unused_imports)]
use refact_runtime_api::AtCommandsContext;
#[cfg(test)]
#[allow(unused_imports)]
use refact_runtime_api::GlobalContextCtx;
#[cfg(test)]
#[allow(unused_imports)]
use refact_core::chat_types::{ChatContent, ChatMessage, ContextEnum};
#[cfg(test)]
#[allow(unused_imports)]
use crate::global_context::GlobalContext;
#[cfg(test)]
#[allow(unused_imports)]
use crate::tools::tools_description::{
    json_schema_from_params, Tool, ToolDesc, ToolSource, ToolSourceType,
};
#[cfg(test)]
#[allow(unused_imports)]
use refact_worktrees::service::WorktreeService;
#[cfg(test)]
#[allow(unused_imports)]
use refact_worktrees::types::{MergeWorktreeRequest, MergeWorktreeResponse, WorktreeMergeStrategy};

#[cfg(test)]
mod tests {
    use super::*;

    fn merged_response_with_warning() -> MergeWorktreeResponse {
        MergeWorktreeResponse {
            id: "wt-1".to_string(),
            status: "merged".to_string(),
            merged: true,
            strategy: "squash".to_string(),
            source_branch: "refact/chat/test".to_string(),
            target_branch: "main".to_string(),
            committed_uncommitted: None,
            merge_commit: Some("abc123".to_string()),
            cleanup: None,
            conflict: None,
            affected_references: Vec::new(),
            affected_reference_count: 0,
            warnings: vec!["stash cleanup needs inspection".to_string()],
        }
    }

    fn merged_response_with_cleanup_warning() -> MergeWorktreeResponse {
        MergeWorktreeResponse {
            cleanup: Some(refact_worktrees::types::WorktreeRemovalResult {
                worktree_deleted: true,
                branch_deleted: false,
                registry_deleted: true,
                stale_path: false,
                warnings: vec!["branch deletion failed".to_string()],
            }),
            warnings: Vec::new(),
            ..merged_response_with_warning()
        }
    }

    #[test]
    fn merge_response_message_includes_warnings() {
        let message = merge_response_message(&merged_response_with_warning());

        assert!(message.contains("## Warnings"));
        assert!(message.contains("stash cleanup needs inspection"));
    }

    #[test]
    fn merge_response_message_includes_cleanup_warnings() {
        let message = merge_response_message(&merged_response_with_cleanup_warning());

        assert!(message.contains("## Warnings"));
        assert!(message.contains("branch deletion failed"));
    }

    #[test]
    fn strategy_defaults_to_squash() {
        assert_eq!(
            strategy_arg(&HashMap::new()).unwrap(),
            WorktreeMergeStrategy::Squash
        );
    }

    #[test]
    fn strategy_arg_rejects_non_string_values() {
        let args = HashMap::from([("strategy".to_string(), Value::Bool(true))]);

        assert!(strategy_arg(&args)
            .unwrap_err()
            .contains("must be a string"));
    }

    #[test]
    fn bool_arg_accepts_model_spellings_and_rejects_garbage() {
        let args = HashMap::from([(
            "delete_after_merge".to_string(),
            Value::String("false".to_string()),
        )]);

        assert_eq!(bool_arg(&args, "delete_after_merge", true), Ok(false));

        let args = HashMap::from([(
            "include_uncommitted".to_string(),
            Value::String("True".to_string()),
        )]);

        assert_eq!(bool_arg(&args, "include_uncommitted", false), Ok(true));

        let args = HashMap::from([(
            "include_uncommitted".to_string(),
            Value::String("maybe".to_string()),
        )]);

        assert!(bool_arg(&args, "include_uncommitted", true)
            .unwrap_err()
            .contains("must be a boolean"));
    }

    #[test]
    fn string_arg_rejects_non_string_values() {
        let args = HashMap::from([("target_branch".to_string(), Value::Bool(true))]);

        assert!(string_arg(&args, "target_branch")
            .unwrap_err()
            .contains("must be a string"));
    }
}
