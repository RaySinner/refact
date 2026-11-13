pub use refact_tools::tools::tool_regex_search::*;

#[cfg(test)]
use std::collections::HashMap;
#[cfg(test)]
use std::sync::Arc;
#[cfg(test)]
use regex::Regex;
#[cfg(test)]
use serde_json::Value;
#[cfg(test)]
use tokio::sync::Mutex as AMutex;
#[cfg(test)]
use refact_runtime_api::AtCommandsContext;
#[cfg(test)]
use refact_core::chat_types::ChatMessage;
#[cfg(test)]
use refact_core::chat_types::ChatContent;
#[cfg(test)]
use refact_core::chat_types::ContextEnum;
#[cfg(test)]
use refact_runtime_api::files_in_workspace::prepare_file_read_context;
#[cfg(test)]
use crate::global_context::GlobalContext;
#[cfg(test)]
use crate::tools::tools_description::Tool;
#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::AtomicBool;

    async fn make_gcx() -> Arc<GlobalContext> {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        *gcx.privacy_settings.write().unwrap() = Arc::new(refact_core::privacy_types::PrivacySettings {
            privacy_rules: refact_core::privacy_types::FilePrivacySettings {
                only_send_to_servers_I_control: Vec::new(),
                blocked: Vec::new(),
            },
            loaded_ts: u64::MAX / 2,
        });
        gcx
    }

    async fn make_ccx(gcx: Arc<GlobalContext>) -> Arc<AMutex<AtCommandsContext>> {
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
                None,
            )
            .await,
        ))
    }

    #[tokio::test]
    async fn tool_execute_returns_successful_message_for_zero_matches() {
        let gcx = make_gcx().await;
        let temp = tempfile::Builder::new()
            .prefix("refact-regex-no-match-")
            .tempdir()
            .unwrap();
        let temp_root = dunce::simplified(&fs::canonicalize(temp.path()).unwrap()).to_path_buf();
        let file = temp_root.join("search.rs");
        fs::write(&file, "fn present() {}\n").unwrap();
        *gcx.documents_state.workspace_folders.lock().unwrap() = vec![temp_root];
        *gcx.documents_state.workspace_files.lock().unwrap() = vec![file];
        let ccx = make_ccx(gcx).await;
        let mut tool = ToolRegexSearch {
            config_path: String::new(),
        };
        let args = HashMap::from_iter([
            ("pattern".to_string(), Value::String("absent".to_string())),
            ("scope".to_string(), Value::String("workspace".to_string())),
        ]);

        let (corrections, results) = tool
            .tool_execute(ccx, &"search-call".to_string(), &args)
            .await
            .unwrap();

        assert!(!corrections);
        assert_eq!(results.len(), 1);
        let ContextEnum::ChatMessage(message) = &results[0] else {
            panic!("zero-match result must be a tool message");
        };
        let ChatContent::SimpleText(text) = &message.content else {
            panic!("zero-match result must contain text");
        };
        assert!(text.contains("No matches found for pattern 'absent' in workspace."));
        assert!(text.contains("Try broader scope"));
        assert!(message.output_filter.is_some());
        let enrichment = refact_chat_api::tool_enrichment_from_extra(&message.extra).unwrap();
        assert_eq!(enrichment.references.len(), 1);
        assert_eq!(
            enrichment.references[0].kind,
            refact_chat_api::ToolEnrichmentKind::Query
        );
        assert_eq!(enrichment.references[0].target, "absent");
        assert_eq!(
            enrichment.references[0].status.as_deref(),
            Some("no_matches")
        );
        assert_eq!(message.content.content_text_only(), *text);
    }

    #[tokio::test]
    async fn tool_execute_keeps_invalid_regex_and_path_as_errors() {
        let gcx = make_gcx().await;
        let temp = tempfile::Builder::new()
            .prefix("refact-regex-invalid-input-")
            .tempdir()
            .unwrap();
        let temp_root = dunce::simplified(&fs::canonicalize(temp.path()).unwrap()).to_path_buf();
        let file = temp_root.join("search.rs");
        fs::write(&file, "fn present() {}\n").unwrap();
        *gcx.documents_state.workspace_folders.lock().unwrap() = vec![temp_root.clone()];
        *gcx.documents_state.workspace_files.lock().unwrap() = vec![file];
        let mut tool = ToolRegexSearch {
            config_path: String::new(),
        };
        let invalid_regex_args = HashMap::from_iter([
            ("pattern".to_string(), Value::String("[".to_string())),
            ("scope".to_string(), Value::String("workspace".to_string())),
        ]);

        let regex_error = tool
            .tool_execute(
                make_ccx(gcx.clone()).await,
                &"search-call".to_string(),
                &invalid_regex_args,
            )
            .await
            .unwrap_err();
        assert!(regex_error.contains("Invalid regex"), "{regex_error}");

        let missing_path = temp_root.join("missing.rs").to_string_lossy().to_string();
        let invalid_path_args = HashMap::from_iter([
            ("pattern".to_string(), Value::String("anything".to_string())),
            ("scope".to_string(), Value::String(missing_path)),
        ]);
        assert!(
            tool.tool_execute(
                make_ccx(gcx).await,
                &"search-call".to_string(),
                &invalid_path_args,
            )
            .await
            .is_err(),
            "invalid path must remain a tool error"
        );
    }

    #[tokio::test]
    async fn search_single_file_stops_at_max_matches_per_file() {
        let gcx = make_gcx().await;
        let temp = tempfile::Builder::new()
            .prefix("refact-regex-early-stop-")
            .tempdir()
            .unwrap();
        let file = temp.path().join("many_matches.rs");
        let content = (0..100)
            .map(|i| format!("needle line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(&file, content).unwrap();

        let regex = Regex::new("needle").unwrap();
        let abort = AtomicBool::new(false);
        let read_context = prepare_file_read_context(gcx.clone()).await;
        let matches = search_single_file(
            gcx,
            file.to_string_lossy().to_string(),
            &regex,
            0,
            7,
            &abort,
            &read_context,
        )
        .await;

        assert_eq!(
            matches.len(),
            7,
            "search_single_file must stop scanning at max_matches_per_file"
        );
    }

    #[tokio::test]
    async fn search_single_file_respects_preset_abort_flag() {
        let gcx = make_gcx().await;
        let temp = tempfile::Builder::new()
            .prefix("refact-regex-abort-file-")
            .tempdir()
            .unwrap();
        let file = temp.path().join("has_matches.rs");
        fs::write(&file, "needle one\nneedle two\n").unwrap();

        let regex = Regex::new("needle").unwrap();
        let abort = AtomicBool::new(true);
        let read_context = prepare_file_read_context(gcx.clone()).await;
        let matches = search_single_file(
            gcx,
            file.to_string_lossy().to_string(),
            &regex,
            0,
            25,
            &abort,
            &read_context,
        )
        .await;

        assert!(
            matches.is_empty(),
            "aborted search must not collect matches"
        );
    }

    #[tokio::test]
    async fn search_files_with_regex_aborts_promptly() {
        let gcx = make_gcx().await;
        let temp = tempfile::Builder::new()
            .prefix("refact-regex-abort-batch-")
            .tempdir()
            .unwrap();
        let mut files = Vec::new();
        for i in 0..8 {
            let file = temp.path().join(format!("file_{i}.rs"));
            fs::write(&file, format!("needle in file {i}\n")).unwrap();
            files.push(file.to_string_lossy().to_string());
        }

        let abort = Arc::new(AtomicBool::new(true));
        let read_context = Arc::new(prepare_file_read_context(gcx.clone()).await);
        let result =
            search_files_with_regex(gcx, "needle", &files, 0, 50, 25, 200, abort, read_context)
                .await;

        assert!(result.is_err(), "aborted batch search must return an error");
        assert!(
            result.unwrap_err().contains("aborted"),
            "error should mention the abort"
        );
    }

    #[tokio::test]
    async fn search_files_with_regex_finds_matches_when_not_aborted() {
        let gcx = make_gcx().await;
        let temp = tempfile::Builder::new()
            .prefix("refact-regex-happy-")
            .tempdir()
            .unwrap();
        let file = temp.path().join("normal.rs");
        fs::write(&file, "alpha\nneedle here\nbeta\n").unwrap();
        let files = vec![file.to_string_lossy().to_string()];

        let abort = Arc::new(AtomicBool::new(false));
        let read_context = Arc::new(prepare_file_read_context(gcx.clone()).await);
        let outcome =
            search_files_with_regex(gcx, "needle", &files, 1, 50, 25, 200, abort, read_context)
                .await
                .unwrap();

        assert_eq!(outcome.matches.len(), 1);
        assert_eq!(outcome.matches[0].match_line, 2);
        assert!(!outcome.stopped_early);
    }

    #[tokio::test]
    async fn search_files_with_regex_stops_at_global_limits() {
        let gcx = make_gcx().await;
        let temp = tempfile::Builder::new()
            .prefix("refact-regex-global-limit-")
            .tempdir()
            .unwrap();
        let mut files = Vec::new();
        for index in 0..64 {
            let name = format!("file_{index:03}.rs");
            let file = temp.path().join(name);
            fs::write(&file, "needle one\nneedle two\n").unwrap();
            files.push(file.to_string_lossy().to_string());
        }

        let read_context = Arc::new(prepare_file_read_context(gcx.clone()).await);
        let outcome = search_files_with_regex(
            gcx,
            "needle",
            &files,
            0,
            1,
            25,
            200,
            Arc::new(AtomicBool::new(false)),
            read_context,
        )
        .await
        .unwrap();

        assert_eq!(outcome.files_scanned, MAX_CONCURRENT_FILE_READS);
        assert_eq!(outcome.matches.len(), 2);
        assert!(outcome
            .matches
            .iter()
            .all(|m| m.file_name.ends_with("file_000.rs")));
        assert!(outcome.stopped_early);
    }

    #[tokio::test]
    async fn search_single_file_skips_oversized_files() {
        let gcx = make_gcx().await;
        let temp = tempfile::Builder::new()
            .prefix("refact-regex-oversized-")
            .tempdir()
            .unwrap();
        let file = temp.path().join("oversized.rs");
        fs::write(&file, vec![b'a'; MAX_SEARCH_FILE_BYTES as usize + 1]).unwrap();
        let read_context = prepare_file_read_context(gcx.clone()).await;

        let matches = search_single_file(
            gcx,
            file.to_string_lossy().to_string(),
            &Regex::new("a").unwrap(),
            0,
            25,
            &AtomicBool::new(false),
            &read_context,
        )
        .await;

        assert!(matches.is_empty());
    }

    #[test]
    fn parse_usize_arg_rejects_invalid_values() {
        let invalid_string =
            HashMap::from_iter([("max_files".to_string(), Value::String("many".to_string()))]);
        let negative = HashMap::from_iter([("max_files".to_string(), Value::from(-1))]);

        assert!(parse_usize_arg(&invalid_string, "max_files").is_err());
        assert!(parse_usize_arg(&negative, "max_files").is_err());
    }
}
