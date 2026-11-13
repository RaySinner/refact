pub use refact_tools::tools::tool_knowledge::*;

#[cfg(test)]
#[allow(unused_imports)]
use std::sync::Arc;
#[cfg(test)]
#[allow(unused_imports)]
use std::collections::HashSet;
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
use async_trait::async_trait;
#[cfg(test)]
#[allow(unused_imports)]
use std::collections::HashMap;
#[cfg(test)]
#[allow(unused_imports)]
use refact_runtime_api::AtCommandsContext;
#[cfg(test)]
#[allow(unused_imports)]
use crate::tools::tools_description::{
    Tool, ToolDesc, ToolSource, ToolSourceType, json_schema_from_params,
};
#[cfg(test)]
#[allow(unused_imports)]
use refact_core::chat_types::{ChatMessage, ChatContent, ContextEnum};
#[cfg(test)]
#[allow(unused_imports)]
use refact_runtime_api::memories::memories_search;
#[cfg(test)]
#[allow(unused_imports)]
use refact_runtime_api::knowledge_graph::build_knowledge_graph;
#[cfg(test)]
#[allow(unused_imports)]
use refact_core::knowledge_index::format_related_memories_section;
#[cfg(test)]
#[allow(unused_imports)]
use refact_postprocessing::pp_command_output::OutputFilter;
#[cfg(test)]
#[allow(unused_imports)]
use crate::tools::native_enrichment::{workspace_roots, NativeReferences};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn knowledge_references_use_native_query_artifact_and_workspace_path_metadata() {
        let memory = refact_knowledge_graph::memo::MemoRecord {
            memid: "memory-1".to_string(),
            file_path: Some(crate::test_paths::abs(
                "workspace/.refact/knowledge/memory.md",
            )),
            line_range: Some((3, 5)),
            title: Some("Memory title".to_string()),
            kind: Some("decision".to_string()),
            score: Some(0.8),
            ..Default::default()
        };
        let mut references = NativeReferences::new();
        references.add_query("remember this", 1, "matched", "knowledge");
        add_knowledge_references(
            &mut references,
            &[memory],
            &[crate::test_paths::abs("workspace")],
        );
        let mut message = ChatMessage::new("tool".to_string(), "raw knowledge".to_string());
        references.attach(&mut message);

        let enrichment = refact_chat_api::tool_enrichment_from_extra(&message.extra).unwrap();
        assert_eq!(enrichment.references.len(), 3);
        assert_eq!(enrichment.references[0].target, "remember this");
        assert_eq!(enrichment.references[1].target, "artifact:memory-1");
        assert_eq!(
            enrichment.references[2].target,
            ".refact/knowledge/memory.md"
        );
        assert_eq!(enrichment.references[2].line1, Some(3));
        assert_eq!(enrichment.references[2].line2, Some(5));
        assert_eq!(message.content.content_text_only(), "raw knowledge");
    }
}
