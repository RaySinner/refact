use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

use crate::files_in_workspace::get_file_text_from_memory_or_disk;
use crate::global_context::{GlobalContext, GlobalContextCtx};

const ROOT_CHAT_MEMO_CAPACITY: usize = 8_192;
static ROOT_CHAT_MEMO: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();

fn root_chat_memo() -> &'static Mutex<HashMap<String, String>> {
    ROOT_CHAT_MEMO.get_or_init(|| Mutex::new(HashMap::new()))
}

fn root_chat_memo_get(chat_id: &str) -> Option<String> {
    let memo = match root_chat_memo().lock() {
        Ok(memo) => memo,
        Err(poisoned) => poisoned.into_inner(),
    };
    memo.get(chat_id).cloned()
}

fn insert_bounded_root_chat_memo(
    memo: &mut HashMap<String, String>,
    chat_id: String,
    root_chat_id: String,
) {
    if memo.len() >= ROOT_CHAT_MEMO_CAPACITY && !memo.contains_key(&chat_id) {
        memo.clear();
    }
    memo.insert(chat_id, root_chat_id);
}

fn root_chat_memo_insert_many(chat_ids: impl IntoIterator<Item = String>, root_chat_id: &str) {
    let mut memo = match root_chat_memo().lock() {
        Ok(memo) => memo,
        Err(poisoned) => poisoned.into_inner(),
    };
    for chat_id in chat_ids {
        insert_bounded_root_chat_memo(&mut memo, chat_id, root_chat_id.to_string());
    }
}

pub async fn load_root_chat_id_from_trajectory(
    gcx: Arc<GlobalContext>,
    path: &PathBuf,
) -> Option<String> {
    let text = get_file_text_from_memory_or_disk(gcx, path).await.ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    v.get("root_chat_id")
        .and_then(|x| x.as_str())
        .map(|s| s.to_string())
}

pub async fn resolve_root_chat_id(
    gcx: Arc<GlobalContext>,
    start_id: &str,
    cache: &mut HashMap<String, String>,
) -> String {
    if let Some(root) = root_chat_memo_get(start_id) {
        cache.insert(start_id.to_string(), root.clone());
        return root;
    }
    if let Some(r) = cache.get(start_id) {
        return r.clone();
    }

    let mut seen = std::collections::HashSet::new();
    let mut traversed = Vec::new();
    let mut current = start_id.to_string();

    for _ in 0..50 {
        if !seen.insert(current.clone()) {
            let root = start_id.to_string();
            cache.insert(start_id.to_string(), root.clone());
            root_chat_memo_insert_many(std::iter::once(start_id.to_string()), &root);
            return root;
        }
        traversed.push(current.clone());

        if let Some(root) = root_chat_memo_get(&current).or_else(|| cache.get(&current).cloned()) {
            for chat_id in &traversed {
                cache.insert(chat_id.clone(), root.clone());
            }
            root_chat_memo_insert_many(traversed, &root);
            return root;
        }

        match crate::chat::trajectories::find_trajectory_parent_id(
            &gcx.chat_ctx(gcx.clone()),
            &current,
        )
        .await
        {
            Some(Some(parent)) => current = parent,
            Some(None) => {
                let root = current;
                for chat_id in &traversed {
                    cache.insert(chat_id.clone(), root.clone());
                }
                root_chat_memo_insert_many(traversed, &root);
                return root;
            }
            None => {
                for chat_id in &traversed {
                    cache.insert(chat_id.clone(), current.clone());
                }
                traversed.pop();
                root_chat_memo_insert_many(traversed, &current);
                return current;
            }
        }
    }

    let root = start_id.to_string();
    cache.insert(start_id.to_string(), root.clone());
    root_chat_memo_insert_many(std::iter::once(start_id.to_string()), &root);
    root
}

pub async fn enrichment_root_id(gcx: Arc<GlobalContext>, chat_id: &str) -> String {
    let mut cache = HashMap::new();
    resolve_root_chat_id(gcx, chat_id, &mut cache).await
}

pub async fn enrichment_current_root_id(
    gcx: Arc<GlobalContext>,
    current_chat_id: Option<&str>,
) -> Option<String> {
    let current_chat_id = current_chat_id?;
    Some(enrichment_root_id(gcx, current_chat_id).await)
}

#[cfg(test)]
mod tests {
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
