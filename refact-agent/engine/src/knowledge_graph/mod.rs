pub use refact_knowledge_graph::kg_structs;
pub use refact_knowledge_graph::kg_structs::KnowledgeFrontmatter;
pub use refact_knowledge_graph::kg_cleanup::{KgFileDeleter, KgGraphBuilder};
pub use refact_runtime_api::knowledge_graph::build_knowledge_graph;
pub use refact_runtime_api::knowledge_graph::{
    cleanup_inactive_memories_on_startup, knowledge_cleanup_background_task,
};

pub mod kg_subchat;
