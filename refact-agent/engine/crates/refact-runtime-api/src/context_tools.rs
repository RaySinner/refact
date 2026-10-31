use refact_core::chat_types::{ContextEnum, ContextFile};

pub fn vec_context_file_to_context_tools(x: Vec<ContextFile>) -> Vec<ContextEnum> {
    x.into_iter()
        .map(|i| ContextEnum::ContextFile(i))
        .collect::<Vec<ContextEnum>>()
}
