use async_trait::async_trait;

use crate::ChatCtx;

pub use refact_core::knowledge_index::KnowledgeIndex;

#[async_trait]
pub trait KnowledgeBuilder: Send + Sync {
    async fn build_knowledge_index_for_ctx(&self, ctx: &ChatCtx) -> KnowledgeIndex;
}

pub async fn build_knowledge_index_for_ctx(ctx: &ChatCtx) -> KnowledgeIndex {
    ctx.knowledge_builder
        .build_knowledge_index_for_ctx(ctx)
        .await
}
