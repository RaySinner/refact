use std::sync::{Arc, RwLock as StdRwLock};

use refact_core::chat_types::CodeCompletionPost;
use refact_scratchpads::scratchpad_abstract::ScratchpadAbstract;

use crate::global_context::GlobalContext;

pub use refact_caps_core::model_records::CompletionModelRecord;

pub async fn create_code_completion_scratchpad(
    global_context: Arc<GlobalContext>,
    model_rec: &CompletionModelRecord,
    post: &CodeCompletionPost,
    cache_arc: Arc<StdRwLock<refact_scratchpads::completion_cache::CompletionCache>>,
) -> Result<Box<dyn ScratchpadAbstract>, String> {
    let tokenizer =
        crate::chat_tokens::cached_tokenizer(global_context.clone(), &model_rec.base).await?;
    let codegraph = global_context.codegraph.lock().await.clone();
    let pp_context = Arc::new(crate::pp_gcx_context::GcxPPContext(
        global_context.clone(),
    )) as Arc<dyn refact_postprocessing::pp_context_provider::PPContextTrait>;
    let project_dirs = crate::files_correction::get_project_dirs(global_context.clone()).await;
    refact_scratchpads::create_code_completion_scratchpad(
        tokenizer,
        &model_rec.scratchpad,
        &model_rec.scratchpad_patch,
        post,
        cache_arc,
        codegraph,
        pp_context,
        project_dirs,
    )
    .await
}
