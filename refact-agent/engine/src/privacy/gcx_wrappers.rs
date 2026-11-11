use std::path::{Path, PathBuf};
use std::sync::Arc;

use refact_chat_api::{
    ToolEnrichment, ToolEnrichmentKind, ToolEnrichmentProvenance, ToolEnrichmentReference,
};
use refact_exec::ObservationStatus;
use refact_privacy::model_context::{PrivacyRecordScope, ShellReadDecision};
use refact_privacy::{DerivedPrivacyZones, Destination, FileRecord};

use crate::call_validation::ChatMessage;
use crate::exec::path_enrichment::{CollectedPathEnrichment, PathEnrichment};
use crate::global_context::GlobalContext;

pub fn privacy_record_scope(gcx: &Arc<GlobalContext>) -> PrivacyRecordScope {
    PrivacyRecordScope {
        privacy_policy_load: gcx.privacy_policy_load.clone(),
        privacy_settings: gcx.privacy_settings.clone(),
        privacy_observation_runtime: gcx.privacy_observation_runtime.clone(),
        file_index: gcx.file_index.clone(),
        workspace_folders: gcx.documents_state.workspace_folders.clone(),
        cache_dir: gcx.cache_dir.clone(),
    }
}

pub(crate) async fn filter_path_enrichment_for_model_context(
    gcx: Arc<GlobalContext>,
    destination: &Destination,
    derived_zones: &DerivedPrivacyZones,
    collected: CollectedPathEnrichment,
) -> PathEnrichment {
    crate::privacy::load_privacy_if_needed(gcx.clone()).await;
    let scope = privacy_record_scope(&gcx);
    refact_privacy::filter_path_enrichment_for_model_context_with_scope(
        &scope,
        destination,
        derived_zones,
        collected,
    )
    .await
}

pub(crate) fn tool_enrichment_from_path_references(enrichment: PathEnrichment) -> ToolEnrichment {
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

pub async fn apply_shell_observation(
    gcx: &Arc<GlobalContext>,
    command: &str,
    cwd: &Path,
    destination: &Destination,
    observation: ObservationStatus,
    derived_zones: &DerivedPrivacyZones,
    message: &mut ChatMessage,
) -> Result<ShellReadDecision, String> {
    if let ObservationStatus::Unavailable(reason) = &observation {
        crate::privacy::warn_observation_degraded_once(gcx.clone(), reason).await;
    }
    let scope = privacy_record_scope(gcx);
    refact_privacy::apply_shell_observation_with_scope(
        &scope,
        command,
        cwd,
        destination,
        observation,
        derived_zones,
        message,
    )
    .await
}

pub fn declared_file_records(
    gcx: &Arc<GlobalContext>,
    paths: impl IntoIterator<Item = PathBuf>,
) -> Result<Vec<FileRecord>, String> {
    refact_privacy::declared_file_records_with_scope(&privacy_record_scope(gcx), paths)
}
