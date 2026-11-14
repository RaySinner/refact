pub use refact_tools::tools::buddy::user_prefs::*;

#[cfg(test)]
#[allow(unused_imports)]
use std::cmp::Ordering;
#[cfg(test)]
#[allow(unused_imports)]
use std::collections::HashMap;
#[cfg(test)]
#[allow(unused_imports)]
use std::path::{Path, PathBuf};
#[cfg(test)]
#[allow(unused_imports)]
use std::sync::{Arc, OnceLock};
#[cfg(test)]
#[allow(unused_imports)]
use async_trait::async_trait;
#[cfg(test)]
#[allow(unused_imports)]
use chrono::{SecondsFormat, Utc};
#[cfg(test)]
#[allow(unused_imports)]
use serde_json::{json, Value};
#[cfg(test)]
#[allow(unused_imports)]
use tokio::sync::Mutex as AMutex;
#[cfg(test)]
#[allow(unused_imports)]
use tokio::sync::OwnedMutexGuard;
#[cfg(test)]
#[allow(unused_imports)]
use refact_runtime_api::AtCommandsContext;
#[cfg(test)]
#[allow(unused_imports)]
use refact_buddy_core::autonomous_redaction::redact_and_cap_text;
#[cfg(test)]
#[allow(unused_imports)]
use refact_core::chat_types::{ChatContent, ChatMessage, ContextEnum};
#[cfg(test)]
#[allow(unused_imports)]
use refact_runtime_api::ChatCtx;
#[cfg(test)]
#[allow(unused_imports)]
use refact_runtime_api::files_correction::get_unscoped_project_dirs_for_ctx;
#[cfg(test)]
#[allow(unused_imports)]
use crate::tools::tools_description::{Tool, ToolDesc, ToolSource, ToolSourceType};

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn buddy_user_pref_upsert_allows_empty_evidence() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("user_profile.md");
        upsert_user_pref_at(
            &path,
            "Prefers terse explanations",
            "",
            0.75,
            "2026-05-14T16:00:00Z",
        )
        .await
        .unwrap();

        let prefs = read_profile(&path).await.unwrap();
        assert_eq!(prefs.len(), 1);
        assert_eq!(prefs[0].evidence, "");
    }

    #[tokio::test]
    async fn buddy_user_pref_upsert_does_not_merge_substring_preferences() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("user_profile.md");
        upsert_user_pref_at(
            &path,
            "Prefers Rust",
            "Evidence",
            0.80,
            "2026-05-14T16:00:00Z",
        )
        .await
        .unwrap();
        upsert_user_pref_at(
            &path,
            "Prefers Rust for systems code but Python for ML pipelines and data exploration",
            "Evidence",
            0.85,
            "2026-05-14T17:00:00Z",
        )
        .await
        .unwrap();

        let prefs = read_profile(&path).await.unwrap();
        assert_eq!(prefs.len(), 2);
    }

    #[tokio::test]
    async fn upsert_user_pref_serializes_concurrent_writes() {
        let dir = tempfile::tempdir().unwrap();
        let path = Arc::new(dir.path().join("user_profile.md"));
        let mut handles = Vec::new();
        for index in 0..5 {
            let path = path.clone();
            handles.push(tokio::spawn(async move {
                upsert_user_pref_at(
                    path.as_ref(),
                    &format!("Preference {index}"),
                    "Concurrent write",
                    0.80,
                    &format!("2026-05-14T10:00:0{index}Z"),
                )
                .await
                .unwrap();
            }));
        }
        for handle in handles {
            handle.await.unwrap();
        }
        let prefs = read_profile(path.as_ref()).await.unwrap();
        assert_eq!(prefs.len(), 5);
        for index in 0..5 {
            assert!(prefs
                .iter()
                .any(|pref| pref.statement == format!("Preference {index}")));
        }
    }

    #[tokio::test]
    async fn buddy_user_pref_list_returns_top_k_by_confidence_then_recency() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("user_profile.md");
        upsert_user_pref_at(
            &path,
            "Uses concise answers",
            "Asked for brevity",
            0.70,
            "2026-05-14T10:00:00Z",
        )
        .await
        .unwrap();
        upsert_user_pref_at(
            &path,
            "Likes detailed plans",
            "Approved planning",
            0.90,
            "2026-05-14T09:00:00Z",
        )
        .await
        .unwrap();
        upsert_user_pref_at(
            &path,
            "Prefers direct fixes",
            "Rejected fallback",
            0.90,
            "2026-05-14T11:00:00Z",
        )
        .await
        .unwrap();
        let table = user_pref_table(read_profile(&path).await.unwrap(), 2);
        let direct = table.find("Prefers direct fixes").unwrap();
        let detailed = table.find("Likes detailed plans").unwrap();
        assert!(direct < detailed);
        assert!(!table.contains("Uses concise answers"));
    }

    #[tokio::test]
    async fn buddy_user_pref_remove_by_substring_works() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("user_profile.md");
        upsert_user_pref_at(
            &path,
            "Prefers Rust",
            "Evidence",
            0.8,
            "2026-05-14T10:00:00Z",
        )
        .await
        .unwrap();
        upsert_user_pref_at(
            &path,
            "Likes Python notebooks",
            "Evidence",
            0.7,
            "2026-05-14T10:00:00Z",
        )
        .await
        .unwrap();
        let removed = remove_user_prefs(&path, "rust").await.unwrap();
        let prefs = read_profile(&path).await.unwrap();
        assert_eq!(removed, 1);
        assert_eq!(prefs.len(), 1);
        assert_eq!(prefs[0].statement, "Likes Python notebooks");
    }

    #[tokio::test]
    async fn buddy_user_pref_upsert_rejects_invalid_confidence() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("user_profile.md");
        let err = upsert_user_pref_at(
            &path,
            "Prefers Rust",
            "Evidence",
            1.2,
            "2026-05-14T10:00:00Z",
        )
        .await
        .unwrap_err();
        assert!(err.contains("confidence"));
    }
}
