use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use refact_core::ast_types::AstDefinition;
use refact_core::chat_types::{ContextFile, PostprocessSettings};
use refact_files::path_utils::canonical_path;
use refact_postprocessing::pp_context_provider::PPContextTrait;
use tokenizers::Tokenizer;

use crate::tool_ctx::ToolCtx;

pub struct ToolPPContext(pub ToolCtx);

#[async_trait]
impl PPContextTrait for ToolPPContext {
    async fn read_file(&self, path: &PathBuf) -> Result<String, String> {
        self.0.files.get_file_text_from_memory_or_disk(path).await
    }

    async fn correct_to_nearest_filename(&self, path: &str, limit: usize) -> Vec<String> {
        self.0
            .files
            .correct_to_nearest_filename(path, false, limit)
            .await
    }

    async fn shortify_paths(&self, paths: &[String]) -> Vec<String> {
        self.0.files.shortify_paths(paths).await
    }

    async fn doc_defs_for_path(&self, path: &str) -> Vec<Arc<AstDefinition>> {
        let codegraph_opt = self.0.codeintel.codegraph.lock().await.clone();
        match codegraph_opt {
            Some(service) => service.doc_defs(path).await.unwrap_or_default(),
            None => vec![],
        }
    }

    fn canonical_path(&self, path: &str) -> PathBuf {
        canonical_path(path)
    }
}

pub async fn postprocess_context_files_for_tool_ctx(
    tool: ToolCtx,
    context_file_vec: &mut Vec<ContextFile>,
    tokenizer: Option<Arc<Tokenizer>>,
    tokens_limit: usize,
    single_file_mode: bool,
    settings: &PostprocessSettings,
) -> (Vec<ContextFile>, Vec<String>) {
    refact_postprocessing::pp_context_files::postprocess_context_files(
        Arc::new(ToolPPContext(tool)),
        context_file_vec,
        tokenizer,
        tokens_limit,
        single_file_mode,
        settings,
    )
    .await
}
