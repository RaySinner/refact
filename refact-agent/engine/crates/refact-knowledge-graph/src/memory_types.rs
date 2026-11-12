use std::path::PathBuf;

pub use refact_core::memories_format::{normalize_memory_tag, normalize_memory_tags};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemoryWriteOutcome {
    Created(PathBuf),
    DuplicateTouched(PathBuf),
}

impl MemoryWriteOutcome {
    pub fn path(&self) -> &PathBuf {
        match self {
            MemoryWriteOutcome::Created(path) => path,
            MemoryWriteOutcome::DuplicateTouched(path) => path,
        }
    }

    pub fn created(&self) -> bool {
        matches!(self, MemoryWriteOutcome::Created(_))
    }
}

pub struct EnrichmentParams {
    pub base_tags: Vec<String>,
    pub base_filenames: Vec<String>,
    pub base_kind: String,
    pub base_title: Option<String>,
    pub source_chat_id: Option<String>,
}
