use std::sync::Arc;

use crate::call_validation::{ChatContent, ChatMessage};
use crate::global_context::GlobalContext;
use crate::subchat::{resolve_subchat_config, run_subchat};
use crate::yaml_configs::customization_registry::get_subagent_config;

pub use refact_knowledge_graph::enrichment::{
    enrichment_identity_from_context, enrichment_query_fingerprint,
    record_enrichment_identity_on_user_message, EnrichmentItem, AUTO_ENRICHMENT_CARD_TOKEN_CAP,
    AUTO_ENRICHMENT_TOTAL_TOKEN_CAP, MAX_QUERY_LENGTH,
};
pub use refact_knowledge_graph::enrichment_cache::{
    fingerprint_bytes, fingerprint_hex, fingerprint_strings, CachedEnrichmentResult, CachedMemo,
    EnrichmentCache, EnrichmentCacheDisposition, EnrichmentCacheKey, MemoSourceFingerprint,
};
pub use refact_runtime_api::knowledge_runtime::{
    cache_result_from_memories, enrich_messages_with_knowledge, enrichment_cache_key,
    get_allowed_enrichment_dirs, revalidate_cached_memories,
};

const ENRICHMENT_SUBAGENT_ID: &str = "memory_enrichment_rewrite";

pub async fn model_gather_and_rewrite(
    gcx: Arc<GlobalContext>,
    query: &str,
) -> Result<(String, Vec<EnrichmentItem>), String> {
    let system_prompt = get_subagent_config(gcx.clone(), ENRICHMENT_SUBAGENT_ID, None)
        .await
        .and_then(|c| c.messages.system_prompt)
        .unwrap_or_else(|| {
            "Search for relevant memories using the knowledge tool, then output JSON: \
            {\"rewritten_text\": \"...\"}"
                .to_string()
        });

    let messages = vec![
        ChatMessage {
            role: "system".to_string(),
            content: ChatContent::SimpleText(system_prompt),
            ..Default::default()
        },
        ChatMessage {
            role: "user".to_string(),
            content: ChatContent::SimpleText(query.to_string()),
            ..Default::default()
        },
    ];

    let config = resolve_subchat_config(
        gcx.clone(),
        ENRICHMENT_SUBAGENT_ID,
        false,
        None,
        None,
        None,
        None,
        None,
        Some(vec!["knowledge".to_string()]),
        4,
        false,
        None,
        "agent".to_string(),
    )
    .await
    .map_err(|e| format!("config: {}", e))?;

    let result = run_subchat(gcx.clone(), messages, config)
        .await
        .map_err(|e| format!("subchat: {}", e))?;

    let last_text = result
        .messages
        .iter()
        .rev()
        .find(|m| m.role == "assistant")
        .and_then(|m| match &m.content {
            ChatContent::SimpleText(t) => Some(t.clone()),
            _ => None,
        })
        .unwrap_or_default();

    let rewritten_text = refact_knowledge_graph::enrichment::parse_rewritten_text(&last_text);

    let allowed_dirs = get_allowed_enrichment_dirs(gcx.clone()).await;
    let items =
        refact_knowledge_graph::enrichment::extract_items_from_tool_results(&result.messages, &allowed_dirs);

    Ok((rewritten_text, items))
}

#[cfg(test)]
mod tests {
    use super::*;

    use refact_files::file_filter::KNOWLEDGE_FOLDER_NAME;
    use refact_knowledge_graph::enrichment::source_fingerprint;

    #[tokio::test(flavor = "multi_thread")]
    async fn cached_memo_revalidation_rejects_changed_deleted_and_archived_sources() {
        let dir = tempfile::tempdir().unwrap();
        let gcx = crate::global_context::tests::make_test_gcx().await;
        *gcx.documents_state.workspace_folders.lock().unwrap() = vec![dir.path().to_path_buf()];
        let path = dir.path().join("memory.md");
        let frontmatter = crate::memories::create_frontmatter(
            Some("Cached memory"),
            &Vec::new(),
            &Vec::new(),
            &Vec::new(),
            "memory",
        );
        let body = "Initial cached content";
        tokio::fs::write(&path, format!("{}\n\n{}", frontmatter.to_yaml(), body))
            .await
            .unwrap();
        let memo = crate::memories::MemoRecord {
            memid: "cached".to_string(),
            content: body.to_string(),
            file_path: Some(path.clone()),
            score: Some(0.9),
            ..Default::default()
        };
        let cached = cache_result_from_memories(gcx.clone(), vec![memo]).await;
        assert!(cached.cacheable);
        assert_eq!(cached.memories.len(), 1);
        let initial_text = tokio::fs::read_to_string(&path).await.unwrap();
        assert_eq!(
            cached.memories[0].source.content_fingerprint,
            source_fingerprint(&path, &initial_text)
                .await
                .unwrap()
                .content_fingerprint
        );
        assert!(revalidate_cached_memories(gcx.clone(), &cached, 0.75, None)
            .await
            .is_some());

        tokio::fs::write(
            &path,
            format!("{}\n\nChanged content", frontmatter.to_yaml()),
        )
        .await
        .unwrap();
        assert!(revalidate_cached_memories(gcx.clone(), &cached, 0.75, None)
            .await
            .is_none());

        let mut archived = frontmatter.clone();
        archived.status = Some("archived".to_string());
        tokio::fs::write(&path, format!("{}\n\n{}", archived.to_yaml(), body))
            .await
            .unwrap();
        assert!(revalidate_cached_memories(gcx.clone(), &cached, 0.75, None)
            .await
            .is_none());

        tokio::fs::remove_file(&path).await.unwrap();
        assert!(revalidate_cached_memories(gcx, &cached, 0.75, None)
            .await
            .is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn get_allowed_enrichment_dirs_skips_symlinked_roots() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().join("workspace");
        let outside = dir.path().join("outside-knowledge");
        tokio::fs::create_dir_all(workspace.join(".refact"))
            .await
            .unwrap();
        tokio::fs::create_dir_all(&outside).await.unwrap();
        if !create_dir_symlink(&outside, &workspace.join(KNOWLEDGE_FOLDER_NAME)) {
            return;
        }

        let gcx = crate::global_context::tests::make_test_gcx().await;
        {
            *gcx.documents_state.workspace_folders.lock().unwrap() = vec![workspace];
        }

        #[cfg(unix)]
        {
            let allowed_dirs = get_allowed_enrichment_dirs(gcx).await;
            assert!(allowed_dirs.is_empty());
        }
    }

    fn create_dir_symlink(target: &std::path::Path, link: &std::path::Path) -> bool {
        #[cfg(unix)]
        {
            return std::os::unix::fs::symlink(target, link).is_ok();
        }
        #[cfg(windows)]
        {
            return std::os::windows::fs::symlink_dir(target, link).is_ok();
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = (target, link);
            false
        }
    }

}
