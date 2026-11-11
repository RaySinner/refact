use std::path::Path;

use crate::skills::load_skill_linked_file;

#[derive(Debug, Default, Clone)]
pub struct SkillsTrackingInfo {
    pub available_count: usize,
    pub included_names: Vec<String>,
}

pub const MAX_INCLUDES: usize = 5;

pub async fn expand_skill_includes(body: &str, skill_dir: &Path) -> String {
    let mut new_lines = Vec::new();
    let mut include_count = 0;
    for line in body.lines() {
        let trimmed = line.trim();
        if let Some(path) = trimmed.strip_prefix("@include ") {
            if include_count < MAX_INCLUDES {
                if let Some(content) = load_skill_linked_file(skill_dir, path.trim()).await {
                    new_lines.push(content);
                    include_count += 1;
                    continue;
                } else {
                    tracing::warn!("Failed to load @include file: {}", path.trim());
                }
            } else {
                tracing::warn!("Skipping @include (max {} includes reached)", MAX_INCLUDES);
            }
        }
        new_lines.push(line.to_string());
    }
    new_lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn expand_skill_includes_resolves_included_file() {
        let tmp = tempfile::tempdir().unwrap();
        tokio::fs::write(tmp.path().join("context.md"), "Included file content")
            .await
            .unwrap();

        let expanded = expand_skill_includes("Before\n@include context.md\nAfter", tmp.path()).await;

        assert_eq!(expanded, "Before\nIncluded file content\nAfter");
    }

    #[tokio::test]
    async fn expand_skill_includes_keeps_directive_when_file_is_missing() {
        let tmp = tempfile::tempdir().unwrap();

        let expanded = expand_skill_includes("Before\n@include missing.md\nAfter", tmp.path()).await;

        assert_eq!(expanded, "Before\n@include missing.md\nAfter");
    }
}
