use std::path::PathBuf;

use refact_chat_api::{
    ToolEnrichment, ToolEnrichmentKind, ToolEnrichmentProvenance, ToolEnrichmentReference,
};
use refact_exec::path_enrichment::PathEnrichment;
use refact_privacy::model_context::PrivacyRecordScope;
use refact_privacy::FileRecord;

use crate::global_context::SharedGlobalContext;

pub fn privacy_record_scope(gcx: &SharedGlobalContext) -> PrivacyRecordScope {
    PrivacyRecordScope {
        privacy_policy_load: gcx.privacy_policy_load.clone(),
        privacy_settings: gcx.privacy_settings.clone(),
        privacy_observation_runtime: gcx.privacy_observation_runtime.clone(),
        file_index: gcx.file_index.clone(),
        workspace_folders: gcx.documents_state.workspace_folders.clone(),
        cache_dir: gcx.cache_dir.clone(),
    }
}

pub fn tool_enrichment_from_path_references(enrichment: PathEnrichment) -> ToolEnrichment {
    let references = enrichment
        .references
        .into_iter()
        .map(|path| {
            let mut reference = ToolEnrichmentReference::new(ToolEnrichmentKind::Path, path.path);
            reference.provenance = ToolEnrichmentProvenance::Heuristic;
            reference.line1 = path.line1.map(|line| line as usize);
            reference.line2 = path.line2.map(|line| line as usize);
            reference.column1 = path.column1.map(|column| column as usize);
            reference.column2 = path.column2.map(|column| column as usize);
            reference.source = Some(path.source);
            reference.confidence = match path.confidence.as_str() {
                "high" => Some(0.9),
                "medium" => Some(0.6),
                "low" => Some(0.3),
                _ => None,
            };
            reference
        })
        .collect();
    ToolEnrichment {
        references,
        truncated: enrichment.truncated
            || enrichment.omitted_count > 0
            || enrichment.withheld_count > 0,
        ..Default::default()
    }
}

pub fn declared_file_records(
    gcx: &SharedGlobalContext,
    paths: impl IntoIterator<Item = PathBuf>,
) -> Result<Vec<FileRecord>, String> {
    refact_privacy::declared_file_records_with_scope(&privacy_record_scope(gcx), paths)
}
