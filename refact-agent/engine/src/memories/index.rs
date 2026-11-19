pub use refact_runtime_api::memories_index::*;

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use uuid::Uuid;

    #[tokio::test]
    async fn root_chat_memo_serves_second_resolution_without_trajectory_file() {
        let workspace = match tempfile::tempdir() {
            Ok(workspace) => workspace,
            Err(error) => panic!("failed to create temporary workspace: {error}"),
        };
        let gcx = crate::global_context::tests::make_test_gcx().await;
        if let Ok(mut folders) = gcx.documents_state.workspace_folders.lock() {
            *folders = vec![workspace.path().to_path_buf()];
        } else {
            panic!("workspace folders lock was poisoned");
        }

        let suffix = Uuid::new_v4();
        let child_id = format!("memo-child-{suffix}");
        let root_id = format!("memo-root-{suffix}");
        let trajectories_dir = workspace.path().join(".refact").join("trajectories");
        if let Err(error) = tokio::fs::create_dir_all(&trajectories_dir).await {
            panic!("failed to create trajectories directory: {error}");
        }
        let child_path = trajectories_dir.join(format!("{child_id}.json"));
        let trajectory = serde_json::json!({
            "id": child_id,
            "parent_id": root_id,
            "messages": []
        });
        if let Err(error) = tokio::fs::write(&child_path, trajectory.to_string()).await {
            panic!("failed to write child trajectory: {error}");
        }

        let mut first_call_cache = HashMap::new();
        let first = resolve_root_chat_id(gcx.clone(), &child_id, &mut first_call_cache).await;
        assert_eq!(first, root_id);
        if let Err(error) = tokio::fs::remove_file(&child_path).await {
            panic!("failed to remove child trajectory: {error}");
        }

        let mut second_call_cache = HashMap::new();
        let second = resolve_root_chat_id(gcx, &child_id, &mut second_call_cache).await;
        assert_eq!(second, root_id);
    }

    #[test]
    fn root_chat_memo_insert_is_bounded() {
        let mut memo = HashMap::new();
        for index in 0..=ROOT_CHAT_MEMO_CAPACITY {
            insert_bounded_root_chat_memo(
                &mut memo,
                format!("chat-{index}"),
                format!("root-{index}"),
            );
            assert!(memo.len() <= ROOT_CHAT_MEMO_CAPACITY);
        }
        assert_eq!(memo.len(), 1);
        assert_eq!(
            memo.get(&format!("chat-{ROOT_CHAT_MEMO_CAPACITY}")),
            Some(&format!("root-{ROOT_CHAT_MEMO_CAPACITY}"))
        );
    }
}
