pub use refact_tools::tools::tool_buddy_get_logs::*;

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
use tokio::io::{AsyncReadExt, AsyncSeekExt};
#[cfg(test)]
#[allow(unused_imports)]
use tokio::sync::Mutex as AMutex;
#[cfg(test)]
#[allow(unused_imports)]
use refact_runtime_api::AtCommandsContext;
#[cfg(test)]
#[allow(unused_imports)]
use refact_core::chat_types::{ChatContent, ChatMessage, ContextEnum};
#[cfg(test)]
#[allow(unused_imports)]
use crate::tools::tools_description::{Tool, ToolDesc, ToolSource, ToolSourceType};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::settings_guard::SettingsGuard;
    use serial_test::serial;

    #[test]
    fn test_buddy_get_logs_redaction() {
        let line = "connecting with api_key=sk-abc123defgh456ijklmn and Bearer mytoken123";
        let redacted = redact_sensitive(line);
        assert!(!redacted.contains("sk-abc123defgh456ijklmn"));
        assert!(!redacted.contains("mytoken123"));
        assert!(redacted.contains("[REDACTED]"));
    }

    #[test]
    fn test_buddy_get_logs_limit() {
        assert!(DEFAULT_LINES <= MAX_LINES);
        assert_eq!(501_usize.min(MAX_LINES), MAX_LINES);
    }

    #[tokio::test]
    async fn classic_rustbinary_layout_is_found() {
        let dir = tempfile::tempdir().unwrap();
        let logs_dir = dir.path().join("logs");
        tokio::fs::create_dir_all(&logs_dir).await.unwrap();
        tokio::fs::write(logs_dir.join("rustbinary.2026-07-01"), "classic tail")
            .await
            .unwrap();

        let resolved = resolve_log_dir(dir.path());
        let content = read_log_content(&resolved).await.unwrap();

        assert_eq!(resolved, logs_dir);
        assert!(is_log_candidate("rustbinary.2026-07-01"));
        assert!(content.contains("classic tail"));
    }

    #[tokio::test]
    async fn daemon_worker_layout_is_found_and_preferred() {
        let dir = tempfile::tempdir().unwrap();
        let classic_dir = dir.path().join("logs");
        let daemon_dir = dir.path().join("daemon").join("logs");
        tokio::fs::create_dir_all(&classic_dir).await.unwrap();
        tokio::fs::create_dir_all(&daemon_dir).await.unwrap();
        tokio::fs::write(classic_dir.join("rustbinary.2026-07-01"), "classic")
            .await
            .unwrap();
        tokio::fs::write(daemon_dir.join("worker-1.log"), "daemon tail")
            .await
            .unwrap();

        let resolved = resolve_log_dir(dir.path());
        let content = read_log_content(&resolved).await.unwrap();

        assert_eq!(resolved, daemon_dir);
        assert!(is_log_candidate("worker-1.log"));
        assert!(content.contains("daemon tail"));
    }

    #[tokio::test]
    #[serial(runtime_settings)]
    async fn get_logs_tail_bytes_setting_changes_bytes_read_and_notice() {
        let _guard = SettingsGuard::install(|settings| {
            settings.get_logs_max_tail_bytes = 4_096;
        });
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rustbinary.2026-07-01");
        let content = format!("old-start\n{}\nrecent-tail", "x".repeat(20_000));
        let total = content.len();
        tokio::fs::write(&path, content).await.unwrap();

        let tail = read_bounded_log_tail_with_notice(&path).await.unwrap();

        assert!(
            tail.text.len() <= 4_096,
            "tail {} bytes exceeds installed cap",
            tail.text.len()
        );
        assert!(!tail.text.contains("old-start"));
        assert!(tail.text.contains("recent-tail"));
        let notice = tail.notice.expect("expected a truncation notice");
        assert!(
            notice.contains(&format!("of {} log bytes", total))
                && notice.contains("get_logs_max_tail_bytes = 4096"),
            "expected a quantified log-tail notice, got: {}",
            notice
        );
    }

    #[tokio::test]
    #[serial(runtime_settings)]
    async fn get_logs_tail_emits_no_notice_when_whole_log_fits() {
        let _guard = SettingsGuard::install(|settings| {
            settings.get_logs_max_tail_bytes = 1_048_576;
        });
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rustbinary.2026-07-01");
        tokio::fs::write(&path, "line one\nline two\n")
            .await
            .unwrap();

        let tail = read_bounded_log_tail_with_notice(&path).await.unwrap();

        assert_eq!(tail.text, "line one\nline two\n");
        assert!(tail.notice.is_none());
    }

    #[tokio::test]
    #[serial(runtime_settings)]
    async fn large_file_tail_does_not_read_whole_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rustbinary.2026-07-01");
        let content = format!(
            "old-start\n{}\nrecent-tail",
            "x".repeat(refact_core::trajectory_settings::current().get_logs_max_tail_bytes + 1024)
        );
        tokio::fs::write(&path, content).await.unwrap();

        let tail = read_log_content(&path).await.unwrap();

        assert!(tail.len() <= refact_core::trajectory_settings::current().get_logs_max_tail_bytes);
        assert!(!tail.contains("old-start"));
        assert!(tail.contains("recent-tail"));
    }
}
