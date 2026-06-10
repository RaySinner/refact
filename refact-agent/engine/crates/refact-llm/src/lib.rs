pub mod adapter;
pub mod adapters;
pub mod canonical;
pub mod embedding_retry;
pub mod embeddings;
pub mod logging;
pub mod openai_endpoint;
pub mod params;
pub mod provider_quirks;

pub use adapter::{get_adapter, WireFormat};
pub use canonical::{CanonicalToolChoice, ClaudeCodeIdentity, LlmRequest, LlmStreamDelta};
pub use embedding_retry::{get_embedding, get_embedding_with_retries};
pub use embeddings::get_embedding_openai_style;
pub use logging::safe_truncate;
pub use openai_endpoint::{
    forward_to_openai_style_endpoint, forward_to_openai_style_endpoint_streaming,
    try_get_compression_from_prompt,
};
pub use params::{CommonParams, ReasoningIntent};
