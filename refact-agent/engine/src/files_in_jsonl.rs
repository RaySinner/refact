pub use refact_runtime_api::files_in_jsonl::*;

#[cfg(test)]
mod tests {
    use super::*;

    fn tracked_jsonl_files(gcx: &Arc<GlobalContext>) -> Vec<PathBuf> {
        gcx.documents_state.jsonl_files.lock().unwrap().clone()
    }

    #[tokio::test]
    async fn modified_jsonl_event_rereads_contents() {
        let temp = tempfile::tempdir().unwrap();
        let jsonl_path = temp.path().join("files.jsonl");
        let first_path = temp.path().join("first.rs");
        let second_path = temp.path().join("second.rs");
        let jsonl_path_str = jsonl_path.to_string_lossy().to_string();
        let mut gcx = crate::global_context::tests::make_test_gcx().await;
        Arc::get_mut(&mut gcx)
            .expect("test owns gcx")
            .cmdline
            .files_jsonl_path = jsonl_path_str.clone();

        let mut fingerprint = jsonl_fingerprint(&jsonl_path_str).await;
        assert_eq!(fingerprint, JsonlFingerprint::Missing);

        std::fs::write(&jsonl_path, "{\"path\":\"first.rs\"}\n").unwrap();
        handle_jsonl_event_if_changed(gcx.clone(), &jsonl_path_str, &mut fingerprint).await;
        assert_eq!(tracked_jsonl_files(&gcx), vec![first_path.clone()]);

        std::fs::write(&jsonl_path, "{\"path\":\"second.rs\"}\n").unwrap();
        handle_jsonl_event_if_changed(gcx.clone(), &jsonl_path_str, &mut fingerprint).await;
        assert_eq!(tracked_jsonl_files(&gcx), vec![second_path]);

        gcx.documents_state.jsonl_files.lock().unwrap().clear();
        handle_jsonl_event_if_changed(gcx.clone(), &jsonl_path_str, &mut fingerprint).await;
        assert!(tracked_jsonl_files(&gcx).is_empty());

        std::fs::remove_file(&jsonl_path).unwrap();
        handle_jsonl_event_if_changed(gcx.clone(), &jsonl_path_str, &mut fingerprint).await;
        assert_eq!(fingerprint, JsonlFingerprint::Missing);
        assert!(tracked_jsonl_files(&gcx).is_empty());
    }
}
