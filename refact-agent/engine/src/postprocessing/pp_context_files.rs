use std::sync::Arc;
use tokenizers::Tokenizer;

pub use refact_postprocessing::pp_context_files::{
    FileLine, PPFile, pp_color_lines, max_line_length, DEBUG, RESERVE_FOR_QUESTION_AND_FOLLOWUP,
};
use refact_core::chat_types::{ContextFile, PostprocessSettings};

use super::gcx_pp_context::ToolPPContext;
use crate::tools::ctx::ToolCtx;

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
