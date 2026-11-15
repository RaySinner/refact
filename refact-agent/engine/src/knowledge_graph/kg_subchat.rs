use std::sync::Arc;

use crate::global_context::GlobalContext;
use refact_knowledge_graph::kg_structs::KnowledgeDoc;
use refact_runtime_api::GlobalContextCtx;

pub use refact_runtime_api::kg_subchat_runtime::{
    DeprecationDecision, DeprecationResult, EnrichmentResult,
};

pub async fn enrich_knowledge_metadata(
    gcx: Arc<GlobalContext>,
    content: &str,
    entities: &[String],
    candidate_files: &[String],
    candidate_docs: &[(String, String)],
    parent_chat_id: Option<&str>,
) -> Result<EnrichmentResult, String> {
    let content = content.to_string();
    let entities = entities.to_vec();
    let candidate_files = candidate_files.to_vec();
    let candidate_docs = candidate_docs.to_vec();
    let parent_chat_id = parent_chat_id.map(|s| s.to_string());
    let gcx2 = gcx.clone();
    crate::buddy::workflows::buddy_wrap_workflow(
        crate::app_state::app_state_from_gcx(gcx).await,
        "kg_enrich",
        "📚",
        12,
        |_: &EnrichmentResult| "Knowledge updated".to_string(),
        move || async move {
            refact_runtime_api::kg_subchat_runtime::enrich_knowledge_metadata_once(
                gcx2.chat_ctx(gcx2.clone()),
                &content,
                &entities,
                &candidate_files,
                &candidate_docs,
                parent_chat_id.as_deref(),
            )
            .await
        },
    )
    .await
}

pub async fn check_deprecation(
    gcx: Arc<GlobalContext>,
    new_doc_title: &str,
    new_doc_tags: &[String],
    new_doc_files: &[String],
    new_doc_snippet: &str,
    candidates: &[&KnowledgeDoc],
    parent_chat_id: Option<&str>,
) -> Result<DeprecationResult, String> {
    if candidates.is_empty() {
        return Ok(DeprecationResult {
            deprecate: vec![],
            keep: vec![],
        });
    }
    let title = new_doc_title.to_string();
    let tags = new_doc_tags.to_vec();
    let files = new_doc_files.to_vec();
    let snippet = new_doc_snippet.to_string();
    let candidates: Vec<KnowledgeDoc> = candidates.iter().map(|d| (*d).clone()).collect();
    let parent_chat_id = parent_chat_id.map(|s| s.to_string());
    let gcx2 = gcx.clone();
    crate::buddy::workflows::buddy_wrap_workflow(
        crate::app_state::app_state_from_gcx(gcx).await,
        "kg_deprecate",
        "🗑",
        5,
        |_: &DeprecationResult| "Knowledge entry deprecated".to_string(),
        move || async move {
            let borrowed: Vec<&KnowledgeDoc> = candidates.iter().collect();
            refact_runtime_api::kg_subchat_runtime::check_deprecation_once(
                gcx2.chat_ctx(gcx2.clone()),
                &title,
                &tags,
                &files,
                &snippet,
                &borrowed,
                parent_chat_id.as_deref(),
            )
            .await
        },
    )
    .await
}
