pub use refact_tools::tools::tool_cat::*;

#[cfg(test)]
use std::collections::HashMap;
#[cfg(test)]
use std::sync::Arc;
#[cfg(test)]
use serde_json::Value;
#[cfg(test)]
use tokio::sync::Mutex as AMutex;
#[cfg(test)]
use refact_runtime_api::AtCommandsContext;
#[cfg(test)]
use crate::tools::tools_description::Tool;
#[cfg(test)]
use refact_core::chat_types::ChatMessage;
#[cfg(test)]
use refact_core::chat_types::ChatContent;
#[cfg(test)]
use refact_core::chat_types::ContextEnum;
#[cfg(test)]
use refact_core::chat_types::ContextFile;
#[cfg(test)]
use refact_files::path_utils::canonical_path;
#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_state::AppState;
    use refact_core::privacy_types::{FilePrivacySettings, PrivacySettings};
    use crate::tools::settings_guard::SettingsGuard;
    use serial_test::serial;

    async fn ccx_for_root(root: &std::path::Path) -> Arc<AMutex<AtCommandsContext>> {
        ccx_for_root_with_blocked(root, vec![]).await
    }

    async fn ccx_for_root_with_blocked(
        root: &std::path::Path,
        blocked: Vec<String>,
    ) -> Arc<AMutex<AtCommandsContext>> {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        *gcx.documents_state.workspace_folders.lock().unwrap() =
            vec![canonical_path(root.to_string_lossy())];
        *gcx.privacy_settings.write().unwrap() = Arc::new(PrivacySettings {
            privacy_rules: FilePrivacySettings {
                only_send_to_servers_I_control: vec![],
                blocked,
            },
            loaded_ts: u64::MAX / 2,
        });
        Arc::new(AMutex::new(
            crate::at_commands::at_commands::at_commands_new_from_app(
                refact_runtime_api::app_state_from_gcx(gcx).await,
                4096,
                20,
                false,
                vec![],
                "test-chat".to_string(),
                None,
                "test-model".to_string(),
                None,
                None,
            )
            .await,
        ))
    }

    fn cat_args(paths: String) -> HashMap<String, Value> {
        HashMap::from_iter([("paths".to_string(), Value::String(paths))])
    }

    fn normalized(path: &std::path::Path) -> String {
        // Mirror the production path pipeline: canonical_path resolves platform
        // quirks (Windows 8.3 short names like RUNNER~1, symlinked /tmp on macOS)
        // exactly like tool_cat does before emitting context files.
        let canonical = refact_files::path_utils::canonical_path(path.to_string_lossy().to_string());
        refact_core::chat_types::normalize_file_name(canonical.to_string_lossy().to_string())
    }

    fn context_file_ranges(results: &[ContextEnum]) -> Vec<(String, usize, usize)> {
        results
            .iter()
            .filter_map(|item| match item {
                ContextEnum::ContextFile(file) => {
                    Some((file.file_name.clone(), file.line1, file.line2))
                }
                _ => None,
            })
            .collect()
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

    fn write_lines(path: &std::path::Path, lines: usize) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let content = (1..=lines)
            .map(|line| format!("line {line}"))
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(path, format!("{content}\n")).unwrap();
    }

    async fn run_cat(ccx: Arc<AMutex<AtCommandsContext>>, paths: String) -> Vec<ContextEnum> {
        let mut tool = ToolCat {
            config_path: String::new(),
        };
        let (_, results) = tool
            .tool_execute(ccx, &"cat-call".to_string(), &cat_args(paths))
            .await
            .unwrap();
        results
    }

    #[tokio::test]
    async fn tool_cat_duplicate_path_keeps_explicit_range_before_unbounded() {
        let temp = tempfile::Builder::new()
            .prefix("refact-tool-cat-")
            .tempdir()
            .unwrap();
        let file = temp.path().join("f.rs");
        write_lines(&file, 8);
        let ccx = ccx_for_root(temp.path()).await;

        let results = run_cat(
            ccx,
            format!("{}:2-4,{}", file.to_string_lossy(), file.to_string_lossy()),
        )
        .await;

        assert_eq!(
            context_file_ranges(&results),
            vec![(normalized(&file), 2, 4)],
            "{}",
            tool_text(&results)
        );
    }

    #[tokio::test]
    async fn tool_cat_duplicate_explicit_ranges_remain_distinct_and_ordered() {
        let temp = tempfile::Builder::new()
            .prefix("refact-tool-cat-")
            .tempdir()
            .unwrap();
        let file = temp.path().join("f.rs");
        write_lines(&file, 8);
        let ccx = ccx_for_root(temp.path()).await;

        let results = run_cat(
            ccx,
            format!(
                "{}:2-3,{}:6-7",
                file.to_string_lossy(),
                file.to_string_lossy()
            ),
        )
        .await;

        assert_eq!(
            context_file_ranges(&results),
            vec![(normalized(&file), 2, 3), (normalized(&file), 6, 7)]
        );
        let enrichment = tool_enrichment(&results);
        assert_eq!(enrichment.references.len(), 2);
        assert_eq!(enrichment.references[0].target, "f.rs");
        assert_eq!(enrichment.references[0].line1, Some(2));
        assert_eq!(enrichment.references[0].line2, Some(3));
        assert_eq!(enrichment.references[1].line1, Some(6));
        assert_eq!(enrichment.references[1].line2, Some(7));
        assert_eq!(enrichment.references[0].source.as_deref(), Some("cat"));
    }

    #[tokio::test]
    async fn tool_cat_explicit_file_range_wins_over_directory_expansion() {
        let temp = tempfile::Builder::new()
            .prefix("refact-tool-cat-")
            .tempdir()
            .unwrap();
        let dir = temp.path().join("src");
        let other = dir.join("a.rs");
        let file = dir.join("z.rs");
        write_lines(&other, 4);
        write_lines(&file, 8);
        let ccx = ccx_for_root(temp.path()).await;

        let results = run_cat(
            ccx,
            format!("{}:3-5,{}", file.to_string_lossy(), dir.to_string_lossy()),
        )
        .await;

        assert_eq!(
            context_file_ranges(&results),
            vec![(normalized(&file), 3, 5), (normalized(&other), 1, 4)]
        );
    }

    #[tokio::test]
    async fn tool_cat_later_explicit_file_range_moves_after_directory_expansion() {
        let temp = tempfile::Builder::new()
            .prefix("refact-tool-cat-")
            .tempdir()
            .unwrap();
        let dir = temp.path().join("src");
        let file = dir.join("a.rs");
        let other = dir.join("z.rs");
        write_lines(&file, 8);
        write_lines(&other, 4);
        let ccx = ccx_for_root(temp.path()).await;

        let results = run_cat(
            ccx,
            format!("{},{}:3-5", dir.to_string_lossy(), file.to_string_lossy()),
        )
        .await;

        assert_eq!(
            context_file_ranges(&results),
            vec![(normalized(&other), 1, 4), (normalized(&file), 3, 5)]
        );
    }

    #[tokio::test]
    async fn tool_cat_blocked_file_is_never_disclosed() {
        let temp = tempfile::Builder::new()
            .prefix("refact-tool-cat-")
            .tempdir()
            .unwrap();
        let dir = temp.path().join("src");
        let visible = dir.join("visible.rs");
        let secret = dir.join("secret.rs");
        write_lines(&visible, 4);
        write_lines(&secret, 4);
        let ccx = ccx_for_root_with_blocked(temp.path(), vec!["*/secret.rs".to_string()]).await;

        let results = run_cat(
            ccx,
            format!("{},{}", dir.to_string_lossy(), secret.to_string_lossy()),
        )
        .await;

        let secret_norm = normalized(&secret);
        assert!(
            context_file_ranges(&results)
                .iter()
                .all(|(name, _, _)| name != &secret_norm),
            "blocked file leaked as a context file: {}",
            tool_text(&results)
        );
        let text = tool_text(&results);
        assert!(
            !text.contains("secret.rs"),
            "blocked file path leaked in text output: {}",
            text
        );
        let visible_norm = normalized(&visible);
        assert!(
            context_file_ranges(&results)
                .iter()
                .any(|(name, _, _)| name == &visible_norm),
            "visible file was unexpectedly dropped: {}",
            text
        );
    }

    #[tokio::test]
    #[serial(runtime_settings)]
    async fn tool_cat_directory_expansion_honours_installed_setting() {
        let _guard = SettingsGuard::install(|settings| {
            settings.cat_max_expanded_files = 3;
        });
        let temp = tempfile::Builder::new()
            .prefix("refact-tool-cat-")
            .tempdir()
            .unwrap();
        let dir = temp.path().join("many");
        std::fs::create_dir_all(&dir).unwrap();
        for i in 0..8 {
            std::fs::write(dir.join(format!("f{:04}.rs", i)), "line 1\n").unwrap();
        }
        let ccx = ccx_for_root(temp.path()).await;

        let results = run_cat(ccx, dir.to_string_lossy().to_string()).await;

        let emitted = context_file_ranges(&results).len();
        assert_eq!(emitted, 3, "{}", tool_text(&results));
        let text = tool_text(&results);
        assert!(
            text.contains("directory expansion: showing 3 of 8 files")
                && text.contains("cat_max_expanded_files = 3"),
            "expected a quantified expansion-cap notice, got: {}",
            text
        );
    }

    #[tokio::test]
    #[serial(runtime_settings)]
    async fn tool_cat_large_file_cap_honours_installed_setting() {
        let _guard = SettingsGuard::install(|settings| {
            settings.cat_max_file_bytes = 4_096;
        });
        let temp = tempfile::Builder::new()
            .prefix("refact-tool-cat-")
            .tempdir()
            .unwrap();
        let big = temp.path().join("big.rs");
        std::fs::create_dir_all(big.parent().unwrap()).unwrap();
        let chunk = "x".repeat(1024);
        let mut content = String::with_capacity(16_384);
        while content.len() <= 8_192 {
            content.push_str(&chunk);
            content.push('\n');
        }
        std::fs::write(&big, content).unwrap();
        let ccx = ccx_for_root(temp.path()).await;

        let results = run_cat(ccx, big.to_string_lossy().to_string()).await;

        let big_norm = normalized(&big);
        assert!(
            context_file_ranges(&results)
                .iter()
                .all(|(name, _, _)| name != &big_norm),
            "oversized file was emitted despite the byte cap: {}",
            tool_text(&results)
        );
        let text = tool_text(&results);
        assert!(
            text.contains("exceeds the 4096 byte read limit")
                && text.contains("cat_max_file_bytes = 4096"),
            "expected an oversized-file notice naming the setting, got: {}",
            text
        );
    }

    #[tokio::test]
    #[serial(runtime_settings)]
    async fn tool_cat_max_lines_setting_changes_emitted_range_and_notice() {
        let _guard = SettingsGuard::install(|settings| {
            settings.cat_max_lines = 100;
        });
        let temp = tempfile::Builder::new()
            .prefix("refact-tool-cat-")
            .tempdir()
            .unwrap();
        let file = temp.path().join("long.rs");
        write_lines(&file, 250);
        let ccx = ccx_for_root(temp.path()).await;

        let results = run_cat(ccx, file.to_string_lossy().to_string()).await;

        assert_eq!(
            context_file_ranges(&results),
            vec![(normalized(&file), 1, 100)],
            "{}",
            tool_text(&results)
        );
        let text = tool_text(&results);
        assert!(
            text.contains("has 250 lines, showing first 100 lines")
                && text.contains("cat_max_lines = 100"),
            "expected a quantified line-cap notice, got: {}",
            text
        );
    }

    #[tokio::test]
    #[serial(runtime_settings)]
    async fn tool_cat_input_path_cap_honours_installed_setting_and_is_loud() {
        let _guard = SettingsGuard::install(|settings| {
            settings.cat_max_input_paths = 2;
        });
        let temp = tempfile::Builder::new()
            .prefix("refact-tool-cat-")
            .tempdir()
            .unwrap();
        let files = (0..5)
            .map(|i| {
                let file = temp.path().join(format!("f{i}.rs"));
                write_lines(&file, 3);
                file
            })
            .collect::<Vec<_>>();
        let ccx = ccx_for_root(temp.path()).await;

        let joined = files
            .iter()
            .map(|file| file.to_string_lossy().to_string())
            .collect::<Vec<_>>()
            .join(",");
        let results = run_cat(ccx, joined).await;

        assert_eq!(context_file_ranges(&results).len(), 2);
        let text = tool_text(&results);
        assert!(
            text.contains("showing 2 of 5 requested paths")
                && text.contains("cat_max_input_paths = 2"),
            "expected a quantified input-path cap notice, got: {}",
            text
        );
    }

    #[tokio::test]
    #[serial(runtime_settings)]
    async fn tool_cat_rejects_line_range_when_disabled() {
        let _guard = SettingsGuard::install(|settings| {
            settings.cat_line_ranges_enabled = false;
        });
        let temp = tempfile::Builder::new()
            .prefix("refact-tool-cat-")
            .tempdir()
            .unwrap();
        let file = temp.path().join("f.rs");
        write_lines(&file, 8);
        let ccx = ccx_for_root(temp.path()).await;

        let mut tool = ToolCat {
            config_path: String::new(),
        };
        let result = tool
            .tool_execute(
                ccx,
                &"cat-call".to_string(),
                &cat_args(format!("{}:1-5", file.to_string_lossy())),
            )
            .await;
        let err = result.expect_err("expected rejection");
        assert!(
            err.contains("Line range reading is disabled in settings"),
            "unexpected error: {}",
            err
        );
    }

    #[test]
    fn cat_max_range_span_follows_cat_max_lines() {
        assert_eq!(clamp_cat_line_range(Some((1, 10_000)), 100), Some((1, 100)));
        assert_eq!(
            clamp_cat_line_range(Some((5, 10_000)), 1_000),
            Some((5, 1_004))
        );
    }

    #[test]
    fn expansion_cap_notice_quantifies_shown_and_total() {
        let notice = expansion_cap_notice(512, 700, false, 512);
        assert!(notice.contains("showing 512 of 700 files"), "{}", notice);
        assert!(
            notice.contains("cat_max_expanded_files = 512"),
            "{}",
            notice
        );

        let probed = expansion_cap_notice(512, 2_049, true, 512);
        assert!(probed.contains("showing 512 of 2049+ files"), "{}", probed);
    }

    #[tokio::test]
    async fn tool_cat_attaches_declared_secrets_record() {
        let temp = tempfile::Builder::new()
            .prefix("refact-tool-cat-")
            .tempdir()
            .unwrap();
        let file = temp.path().join(".env");
        write_lines(&file, 2);
        let ccx = ccx_for_root(temp.path()).await;
        let gcx = ccx.lock().await.app.gcx.clone();
        let policy = refact_privacy::PrivacyPolicy {
            blocked: Vec::new(),
            zones: vec![
                refact_privacy::Zone {
                    name: "secrets".to_string(),
                    patterns: vec![file.to_string_lossy().to_string()],
                    send_to: vec!["*".to_string()],
                    on_shell_read: refact_privacy::ShellBehavior::Withhold,
                },
                refact_privacy::Zone {
                    name: "normal".to_string(),
                    patterns: vec!["*".to_string()],
                    send_to: vec!["*".to_string()],
                    on_shell_read: refact_privacy::ShellBehavior::Withhold,
                },
            ],
            subagents: refact_privacy::SubagentPolicy::default(),
            ..Default::default()
        };
        gcx.privacy_policy_load.write().unwrap().policy = Arc::new(policy);

        let results = run_cat(ccx, file.to_string_lossy().to_string()).await;
        let message = results
            .iter()
            .find_map(|result| match result {
                ContextEnum::ChatMessage(message) if message.role == "tool" => Some(message),
                _ => None,
            })
            .expect("cat should emit a tool message");

        assert_eq!(message.extra["privacy"]["files"][0]["zone"], "secrets");
        assert_eq!(
            message.extra["privacy"]["files"][0]["attribution"],
            "declared"
        );
    }
}
