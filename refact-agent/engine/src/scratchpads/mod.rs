pub mod multimodality;
pub mod scratchpad_utils;
pub use refact_scratchpads::code_completion_fim;

pub use crate::chat::history_limit as chat_utils_limit_history;
pub use crate::chat::prompts as chat_utils_prompts;

pub use refact_runtime_api::scratchpads_runtime::{
    create_code_completion_scratchpad, CompletionModelRecord,
};
