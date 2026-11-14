pub use refact_runtime_api::buddy_privacy_degraded::*;

#[cfg(test)]
#[allow(unused_imports)]
use std::sync::atomic::AtomicBool;
#[cfg(test)]
#[allow(unused_imports)]
use std::sync::Arc;
#[cfg(test)]
#[allow(unused_imports)]
use refact_runtime_api::GlobalContextCtx;

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn privacy_degraded_warning_creates_one_buddy_chat_per_lifetime() {
        let temp = tempfile::tempdir().unwrap();
        let gcx = crate::global_context::tests::make_test_gcx().await;
        *gcx.documents_state.workspace_folders.lock().unwrap() = vec![temp.path().to_path_buf()];
        let created = Arc::new(AtomicBool::new(false));
        let attempts = (0..32)
            .map(|_| {
                let gcx = gcx.clone();
                let created = created.clone();
                tokio::spawn(async move {
                    warn_once_with_flag(gcx, "ptrace unavailable", &created).await
                })
            })
            .collect::<Vec<_>>();
        let results = futures::future::join_all(attempts).await;
        let created_chats = results
            .into_iter()
            .filter_map(|result| result.unwrap())
            .collect::<Vec<_>>();

        assert_eq!(created_chats.len(), 1);
        let first = created_chats
            .into_iter()
            .next()
            .unwrap()
            .expect("winning warning creates a chat");
        let conversations = crate::chat::trajectories::get_buddy_conversations_dir(&gcx.chat_ctx(gcx.clone()))
            .await
            .unwrap();
        let warnings = std::fs::read_dir(conversations)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| {
                let path = entry.path();
                let Ok(contents) = std::fs::read_to_string(path) else {
                    return false;
                };
                contents.contains("\"workflow_id\": \"privacy_degraded\"")
                    || contents.contains("\"workflow_id\":\"privacy_degraded\"")
            })
            .count();
        assert_eq!(warnings, 1);
        let loaded = crate::chat::trajectories::load_trajectory_for_chat(&gcx.chat_ctx(gcx.clone()), &first)
            .await
            .unwrap();
        assert_eq!(
            loaded.thread.buddy_meta.unwrap().workflow_id.as_deref(),
            Some("privacy_degraded")
        );
        let text = loaded.messages[0].content.content_text_only();
        assert!(text.contains(std::env::consts::OS));
        assert!(text.contains("ptrace unavailable"));
        assert!(text.contains("heuristic command-path attribution"));
    }
}
