pub use refact_tools::tools::tool_tree::*;

#[cfg(test)]
use std::sync::Arc;
#[cfg(test)]
use std::collections::HashMap;
#[cfg(test)]
use serde_json::Value;
#[cfg(test)]
use tokio::sync::Mutex as AMutex;
#[cfg(test)]
use crate::tools::tools_description::Tool;
#[cfg(test)]
use refact_core::chat_types::ChatMessage;
#[cfg(test)]
mod privacy_and_bounds_tests {
    use super::*;
    use refact_runtime_api::AtCommandsContext;
    use refact_core::chat_types::{ChatContent, ContextEnum};
    use refact_core::privacy_types::{FilePrivacySettings, PrivacySettings};
    use refact_worktrees::types::WorktreeMeta;
    use std::fs;
    use std::path::PathBuf;

    struct Fixture {
        _temp: tempfile::TempDir,
        worktree: WorktreeMeta,
        root: PathBuf,
        source: PathBuf,
    }

    fn make_fixture() -> Fixture {
        let temp = tempfile::Builder::new()
            .prefix("refact-tree-privacy-")
            .tempdir()
            .unwrap();
        let root = temp
            .path()
            .join(".cache")
            .join("refact")
            .join("worktrees")
            .join("wt")
            .join("proj");
        let source = temp.path().join("source");
        fs::create_dir_all(root.join("subdir")).unwrap();
        fs::create_dir_all(source.join("subdir")).unwrap();
        fs::write(root.join("subdir").join("allowed.rs"), "fn allowed() {}\n").unwrap();
        fs::write(
            root.join("subdir").join("secret.blocked"),
            "SECRET_TOKEN=abcdef\n",
        )
        .unwrap();
        let root = dunce::simplified(&fs::canonicalize(root).unwrap()).to_path_buf();
        let source = dunce::simplified(&fs::canonicalize(source).unwrap()).to_path_buf();
        let worktree = WorktreeMeta {
            id: "wt-tree-privacy".to_string(),
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
        Fixture {
            _temp: temp,
            worktree,
            root,
            source,
        }
    }

    async fn make_gcx(
        fixture: &Fixture,
        blocked: Vec<String>,
    ) -> Arc<crate::global_context::GlobalContext> {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        {
            let privacy_settings = gcx.privacy_settings.clone();
            let workspace_folders = gcx.documents_state.workspace_folders.clone();
            *privacy_settings.write().unwrap() = Arc::new(PrivacySettings {
                privacy_rules: FilePrivacySettings {
                    only_send_to_servers_I_control: vec![],
                    blocked,
                },
                loaded_ts: u64::MAX / 2,
            });
            *workspace_folders.lock().unwrap() = vec![fixture.source.clone()];
        }
        gcx
    }

    async fn make_ccx(
        gcx: Arc<crate::global_context::GlobalContext>,
        worktree: WorktreeMeta,
    ) -> Arc<AMutex<AtCommandsContext>> {
        Arc::new(AMutex::new(
            crate::at_commands::at_commands::at_commands_new_from_app(
                refact_runtime_api::app_state_from_gcx(gcx).await,
                4096,
                20,
                false,
                vec![],
                "chat".to_string(),
                None,
                "model".to_string(),
                None,
                Some(worktree),
            )
            .await,
        ))
    }

    fn tool_text(results: &[ContextEnum]) -> String {
        results
            .iter()
            .filter_map(|item| match item {
                ContextEnum::ChatMessage(message) => match &message.content {
                    ChatContent::SimpleText(text) => Some(text.clone()),
                    _ => None,
                },
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn tool_enrichment(results: &[ContextEnum]) -> refact_chat_api::ToolEnrichment {
        results
            .iter()
            .find_map(|item| match item {
                ContextEnum::ChatMessage(message) if message.role == "tool" => {
                    refact_chat_api::tool_enrichment_from_extra(&message.extra)
                }
                _ => None,
            })
            .expect("tool enrichment")
    }

    #[tokio::test]
    async fn scoped_subdir_tree_hides_privacy_blocked_files() {
        let fixture = make_fixture();
        let gcx = make_gcx(&fixture, vec!["*.blocked".to_string()]).await;
        let ccx = make_ccx(gcx, fixture.worktree.clone()).await;
        let mut tool = ToolTree {
            config_path: String::new(),
        };
        let tool_call_id = "tree-call".to_string();
        let subdir = fixture.root.join("subdir").to_string_lossy().to_string();
        let args = HashMap::from_iter([("path".to_string(), Value::String(subdir))]);

        let (_corrections, results) = tool.tool_execute(ccx, &tool_call_id, &args).await.unwrap();
        let text = tool_text(&results);

        assert!(
            text.contains("allowed.rs"),
            "allowed file must be listed: {text}"
        );
        assert!(
            !text.contains("secret.blocked"),
            "privacy-blocked file must not leak into the tree listing: {text}"
        );
        assert!(!text.contains("SECRET_TOKEN"), "{text}");
        let enrichment = tool_enrichment(&results);
        assert_eq!(enrichment.references.len(), 1);
        assert_eq!(enrichment.references[0].target, "subdir/allowed.rs");
        assert_eq!(enrichment.references[0].status.as_deref(), Some("listed"));
        assert_eq!(enrichment.references[0].source.as_deref(), Some("tree"));
    }
}
