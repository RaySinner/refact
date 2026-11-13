pub use refact_tools::tools::tool_search::*;

#[cfg(test)]
use serde_json::Value;
#[cfg(test)]
use std::collections::HashMap;
#[cfg(test)]
use std::path::PathBuf;
#[cfg(test)]
use std::sync::Arc;
#[cfg(test)]
use refact_runtime_api::GlobalContextCtx;
#[cfg(test)]
use refact_runtime_api::files_in_workspace::prepare_file_read_context;
#[cfg(test)]
use crate::global_context::GlobalContext;
#[cfg(test)]
use refact_core::chat_types::ChatMessage;
#[cfg(test)]
use refact_core::chat_types::ChatContent;
#[cfg(test)]
use refact_core::chat_types::ContextEnum;
#[cfg(test)]
use refact_worktrees::scope::ExecutionScope;
#[cfg(test)]
mod tests {
    use super::*;
    use refact_core::privacy_types::{FilePrivacySettings, PrivacySettings};
    use refact_worktrees::types::WorktreeMeta;
    use std::fs;
    use std::sync::atomic::AtomicBool;

    async fn make_gcx() -> Arc<GlobalContext> {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        *gcx.privacy_settings.write().unwrap() = Arc::new(PrivacySettings {
            privacy_rules: FilePrivacySettings {
                only_send_to_servers_I_control: Vec::new(),
                blocked: Vec::new(),
            },
            loaded_ts: u64::MAX / 2,
        });
        gcx
    }

    struct FallbackFixture {
        _temp: tempfile::TempDir,
        worktree: WorktreeMeta,
        root: PathBuf,
        source: PathBuf,
    }

    fn make_fallback_fixture() -> FallbackFixture {
        let temp = tempfile::Builder::new()
            .prefix("refact-semantic-fallback-")
            .tempdir()
            .unwrap();
        let root = temp
            .path()
            .join(".cache")
            .join("refact")
            .join("worktrees")
            .join("wt")
            .join("engine");
        let source = temp.path().join("source");
        fs::create_dir_all(root.join("src")).unwrap();
        fs::create_dir_all(source.join("src")).unwrap();
        let root = dunce::simplified(&fs::canonicalize(&root).unwrap()).to_path_buf();
        let source = dunce::simplified(&fs::canonicalize(&source).unwrap()).to_path_buf();
        let worktree = WorktreeMeta {
            id: "wt-semantic-fallback".to_string(),
            kind: "chat".to_string(),
            root: root.clone(),
            source_workspace_root: source.clone(),
            repo_root: source.clone(),
            branch: Some("feature".to_string()),
            base_branch: Some("main".to_string()),
            base_commit: Some("base".to_string()),
            task_id: None,
            card_id: None,
            agent_id: None,
            enforce: true,
        };
        FallbackFixture {
            _temp: temp,
            worktree,
            root,
            source,
        }
    }

    #[test]
    fn parse_usize_arg_rejects_invalid_values() {
        let invalid_string =
            HashMap::from_iter([("max_files".to_string(), Value::String("many".to_string()))]);
        let negative = HashMap::from_iter([("max_files".to_string(), Value::from(-1))]);
        let fractional = HashMap::from_iter([("context_lines".to_string(), Value::from(1.5))]);

        assert!(parse_usize_arg(&invalid_string, "max_files").is_err());
        assert!(parse_usize_arg(&negative, "max_files").is_err());
        assert!(parse_usize_arg(&fractional, "context_lines").is_err());
        assert_eq!(parse_usize_arg(&HashMap::new(), "max_files").unwrap(), None);
        assert_eq!(
            parse_usize_arg(
                &HashMap::from_iter([("max_files".to_string(), Value::from(7u64))]),
                "max_files"
            )
            .unwrap(),
            Some(7)
        );
    }

    #[test]
    fn semantic_no_match_is_a_successful_typed_tool_response() {
        let (stop, responses) = semantic_no_match_result("call-1", String::new());
        assert!(!stop);
        assert_eq!(responses.len(), 1);
        let ContextEnum::ChatMessage(message) = &responses[0] else {
            panic!("expected a tool chat message");
        };
        assert_eq!(message.role, "tool");
        assert_eq!(message.tool_call_id, "call-1");
        let ChatContent::SimpleText(content) = &message.content else {
            panic!("expected text content");
        };
        assert!(content.contains("No semantic matches found"));
        assert!(content.contains("search_pattern()"));
    }

    #[tokio::test]
    async fn fallback_skips_oversized_worktree_only_file() {
        let gcx = make_gcx().await;
        let fixture = make_fallback_fixture();
        let big = fixture.root.join("src").join("huge.rs");
        let mut content = String::from("needle marker\n");
        content.push_str(&"a".repeat(MAX_SEARCH_FILE_BYTES + 1));
        fs::write(&big, content).unwrap();

        let scope = ExecutionScope::from_worktree(&fixture.worktree);
        let read_context = prepare_file_read_context(gcx.clone()).await;
        let tool = gcx.tool_ctx(gcx.clone());
        let abort = AtomicBool::new(false);

        let outcome = direct_worktree_fallback_search(
            gcx,
            &tool,
            Some(&scope),
            &read_context,
            "workspace",
            "needle",
            0,
            50,
            &abort,
        )
        .await
        .unwrap();

        assert!(
            outcome.context_files.is_empty(),
            "oversized worktree-only file must be skipped by the capped read"
        );
        assert!(!outcome.candidate_truncated);
        assert!(!outcome.traversal_truncated);
    }

    #[tokio::test]
    async fn fallback_stops_at_candidate_budget_and_reports_truncation() {
        let gcx = make_gcx().await;
        let fixture = make_fallback_fixture();
        for idx in 0..(FALLBACK_MAX_CANDIDATE_FILES + 5) {
            let path = fixture.root.join("src").join(format!("f_{idx:05}.rs"));
            fs::write(&path, "harmless content\n").unwrap();
        }

        let scope = ExecutionScope::from_worktree(&fixture.worktree);
        let read_context = prepare_file_read_context(gcx.clone()).await;
        let tool = gcx.tool_ctx(gcx.clone());
        let abort = AtomicBool::new(false);

        let outcome = direct_worktree_fallback_search(
            gcx,
            &tool,
            Some(&scope),
            &read_context,
            "workspace",
            "needle",
            0,
            50,
            &abort,
        )
        .await
        .unwrap();

        assert!(
            outcome.candidate_truncated,
            "candidate budget must stop the fallback and flag truncation"
        );
        assert!(!outcome.traversal_truncated);
        assert!(outcome.context_files.is_empty());
    }

    #[tokio::test]
    async fn fallback_candidate_cap_is_applied_after_worktree_only_filtering() {
        let gcx = make_gcx().await;
        let fixture = make_fallback_fixture();
        for idx in 0..(FALLBACK_MAX_CANDIDATE_FILES + 5) {
            let name = format!("source_backed_{idx:05}.rs");
            fs::write(fixture.root.join("src").join(&name), "shared content\n").unwrap();
            fs::write(fixture.source.join("src").join(name), "shared content\n").unwrap();
        }
        let late = fixture.root.join("src").join("zzzzz_worktree_only.rs");
        fs::write(&late, "late worktree needle\n").unwrap();

        let scope = ExecutionScope::from_worktree(&fixture.worktree);
        let read_context = prepare_file_read_context(gcx.clone()).await;
        let tool = gcx.tool_ctx(gcx.clone());
        let abort = AtomicBool::new(false);

        let outcome = direct_worktree_fallback_search(
            gcx,
            &tool,
            Some(&scope),
            &read_context,
            "workspace",
            "needle",
            0,
            50,
            &abort,
        )
        .await
        .unwrap();

        assert_eq!(outcome.context_files.len(), 1);
        assert_eq!(PathBuf::from(&outcome.context_files[0].file_name), late);
        assert!(!outcome.candidate_truncated);
        assert!(!outcome.traversal_truncated);
    }

    #[tokio::test]
    async fn fallback_respects_preset_abort_flag() {
        let gcx = make_gcx().await;
        let fixture = make_fallback_fixture();
        let file = fixture.root.join("src").join("worktree_only.rs");
        fs::write(&file, "needle marker\n").unwrap();

        let scope = ExecutionScope::from_worktree(&fixture.worktree);
        let read_context = prepare_file_read_context(gcx.clone()).await;
        let tool = gcx.tool_ctx(gcx.clone());
        let abort = AtomicBool::new(true);

        let result = direct_worktree_fallback_search(
            gcx,
            &tool,
            Some(&scope),
            &read_context,
            "workspace",
            "needle",
            0,
            50,
            &abort,
        )
        .await;

        assert!(result.is_err(), "preset abort must return an error");
        assert!(
            result.unwrap_err().contains("aborted"),
            "error should mention the abort"
        );
    }
}
