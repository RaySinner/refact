pub use refact_tools::tools::tool_create_knowledge::*;

#[cfg(test)]
#[allow(unused_imports)]
use std::collections::HashMap;
#[cfg(test)]
#[allow(unused_imports)]
use std::sync::Arc;
#[cfg(test)]
#[allow(unused_imports)]
use async_trait::async_trait;
#[cfg(test)]
#[allow(unused_imports)]
use serde_json::Value;
#[cfg(test)]
#[allow(unused_imports)]
use tracing::info;
#[cfg(test)]
#[allow(unused_imports)]
use tokio::sync::Mutex as AMutex;
#[cfg(test)]
#[allow(unused_imports)]
use refact_runtime_api::AtCommandsContext;
#[cfg(test)]
#[allow(unused_imports)]
use refact_core::chat_types::{ChatMessage, ChatContent, ContextEnum};
#[cfg(test)]
#[allow(unused_imports)]
use crate::tools::tools_description::{
    Tool, ToolDesc, ToolSource, ToolSourceType, json_schema_from_params,
};
#[cfg(test)]
#[allow(unused_imports)]
use refact_runtime_api::memories::memories_add_enriched;
#[cfg(test)]
#[allow(unused_imports)]
use refact_knowledge_graph::memory_types::EnrichmentParams;
#[cfg(test)]
#[allow(unused_imports)]
use refact_core::knowledge_index::format_related_memories_section;
#[cfg(test)]
#[allow(unused_imports)]
use refact_core::string_utils::redact_sensitive;


