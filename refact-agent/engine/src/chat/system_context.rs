pub use refact_runtime_api::system_context::*;

#[cfg(test)]
#[allow(unused_imports)]
use refact_core::chat_types::ChatContent;

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_system_info_gather() {
        let info = SystemInfo::gather();
        assert!(!info.os.is_empty());
        assert!(!info.arch.is_empty());
        assert!(!info.username.is_empty());
    }

    #[test]
    fn test_glob_match() {
        assert!(glob_match("*.md", "README.md"));
        assert!(glob_match("*.md", "AGENTS.md"));
        assert!(!glob_match("*.md", "config.json"));
        assert!(glob_match("*.mdc", "rules.mdc"));
        assert!(glob_match("*", "anything"));
    }

    #[test]
    fn test_determine_tool_source() {
        assert_eq!(determine_tool_source("AGENTS.md"), "universal");
        assert_eq!(determine_tool_source("CLAUDE.md"), "claude_code");
        assert_eq!(determine_tool_source(".cursorrules"), "cursor");
    }

    #[test]
    fn test_categorize_config() {
        assert_eq!(categorize_config(".eslintrc"), "code_style");
        assert_eq!(categorize_config("jest.config.js"), "testing");
        assert_eq!(categorize_config("webpack.config.js"), "build");
        assert_eq!(categorize_config("tsconfig.json"), "typescript");
    }

    #[test]
    fn test_extract_workspace_xml() {
        let workspace_xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<project version="4">
  <component name="ChangeListManager">
    <list default="true" id="123" name="Changes" />
  </component>
  <component name="RunManager" selected="Application.Main">
    <configuration default="true" type="Application" factoryName="Application">
      <option name="MAIN_CLASS_NAME" value="" />
    </configuration>
    <configuration name="Main" type="Application" factoryName="Application">
      <option name="MAIN_CLASS_NAME" value="com.example.Main" />
      <option name="workingDirectory" value="$PROJECT_DIR$" />
    </configuration>
    <configuration name="Test" type="Application" temporary="true">
      <option name="MAIN_CLASS_NAME" value="com.example.Test" />
    </configuration>
  </component>
  <component name="ProjectId" id="abc123" />
</project>"#;

        let extracted = extract_workspace_xml_important_parts(workspace_xml);
        assert!(extracted.is_some());
        let result = extracted.unwrap();
        assert!(result.contains("configurations:"));
        assert!(result.contains("name: Main"));
        assert!(!result.contains("ChangeListManager"));
        assert!(!result.contains("ProjectId"));
        assert!(!result.contains("Test"));
    }

    #[test]
    fn test_deduplicate_instruction_files_by_content() {
        let temp_dir = TempDir::new().unwrap();
        let first_path = temp_dir.path().join("first").join("AGENTS.md");
        let second_path = temp_dir.path().join("second").join("AGENTS.md");
        let unique_path = temp_dir.path().join("third").join("AGENTS.md");
        std::fs::create_dir_all(first_path.parent().unwrap()).unwrap();
        std::fs::create_dir_all(second_path.parent().unwrap()).unwrap();
        std::fs::create_dir_all(unique_path.parent().unwrap()).unwrap();
        std::fs::write(&first_path, "# Rules\n\nUse cargo check.\n").unwrap();
        std::fs::write(&second_path, "# Rules\r\n\r\nUse cargo check.\r\n").unwrap();
        std::fs::write(&unique_path, "# Other Rules\n").unwrap();

        let files = vec![
            InstructionFile {
                file_name: "AGENTS.md".to_string(),
                file_path: first_path.to_string_lossy().to_string(),
                source_tool: "universal".to_string(),
                processed_content: None,
                importance: 0,
                max_chars: None,
            },
            InstructionFile {
                file_name: "AGENTS.md".to_string(),
                file_path: second_path.to_string_lossy().to_string(),
                source_tool: "universal".to_string(),
                processed_content: None,
                importance: 0,
                max_chars: None,
            },
            InstructionFile {
                file_name: "AGENTS.md".to_string(),
                file_path: unique_path.to_string_lossy().to_string(),
                source_tool: "universal".to_string(),
                processed_content: None,
                importance: 0,
                max_chars: None,
            },
        ];

        let deduplicated = deduplicate_instruction_files_by_content(files);

        assert_eq!(deduplicated.len(), 2);
        assert_eq!(deduplicated[0].file_path, first_path.to_string_lossy());
        assert_eq!(deduplicated[1].file_path, unique_path.to_string_lossy());
    }

    #[tokio::test]
    async fn test_create_instruction_files_message_truncates_and_keeps_all_files() {
        let temp_dir = TempDir::new().unwrap();
        let mut files = Vec::new();
        for idx in 0..20 {
            let path = temp_dir.path().join(format!("AGENTS_{}.md", idx));
            std::fs::write(&path, "x".repeat(100)).unwrap();
            files.push(InstructionFile {
                file_name: format!("AGENTS_{}.md", idx),
                file_path: path.to_string_lossy().to_string(),
                source_tool: "universal".to_string(),
                processed_content: None,
                importance: 0,
                max_chars: Some(50),
            });
        }

        let message = create_instruction_files_message(&files).await.unwrap();
        let context_files = match message.content {
            ChatContent::ContextFiles(cf) => cf,
            _ => panic!("expected context_file message"),
        };

        assert_eq!(context_files.len(), 20);
        assert!(context_files
            .iter()
            .all(|cf| cf.file_name != "(additional files - paths only)"));

        for cf in &context_files {
            assert_eq!(cf.file_content.chars().count(), 50);
            assert!(cf.file_content.starts_with(&"x".repeat(38)));
            assert!(cf.file_content.ends_with("[TRUNCATED]"));
            assert!(!cf.file_content.contains(&"x".repeat(39)));
        }
    }
}
