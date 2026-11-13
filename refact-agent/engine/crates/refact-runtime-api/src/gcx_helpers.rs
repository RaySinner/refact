use std::path::PathBuf;
use std::sync::Arc;

use refact_chat_api::{
    ToolEnrichment, ToolEnrichmentKind, ToolEnrichmentProvenance, ToolEnrichmentReference,
};
use refact_exec::path_enrichment::PathEnrichment;
use refact_files::indexing_settings::IndexingEverywhere;
use refact_privacy::model_context::PrivacyRecordScope;
use refact_privacy::FileRecord;

use refact_chat_history::trajectory_snapshot::TrajectorySnapshot;

use crate::global_context::{GlobalContextCtx, SharedGlobalContext};

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

pub async fn filter_path_enrichment_for_model_context(
    gcx: SharedGlobalContext,
    destination: &refact_privacy::Destination,
    derived_zones: &refact_privacy::DerivedPrivacyZones,
    collected: refact_exec::path_enrichment::CollectedPathEnrichment,
) -> refact_exec::path_enrichment::PathEnrichment {
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

/// Unwraps `GlobalContext` into the explicit arguments of
/// `refact_files::indexing_settings::reload_indexing_everywhere_*` and forwards the
/// `GlobalContext` pointer as the crate cache key.
pub async fn reload_indexing_everywhere_for_gcx(
    gcx: &SharedGlobalContext,
    force: bool,
) -> Arc<IndexingEverywhere> {
    let cache_key = Arc::as_ptr(gcx) as usize;
    let config_dir = gcx.config_dir.clone();
    let indexing_yaml = gcx.cmdline.indexing_yaml.clone();
    let workspace_vcs_roots = gcx
        .documents_state
        .workspace_vcs_roots
        .lock()
        .unwrap()
        .clone();
    if force {
        refact_files::indexing_settings::reload_indexing_everywhere_now(
            &config_dir,
            &indexing_yaml,
            &workspace_vcs_roots,
            &gcx.indexing_everywhere,
            cache_key,
        )
        .await
    } else {
        refact_files::indexing_settings::reload_indexing_everywhere_if_needed(
            &config_dir,
            &indexing_yaml,
            &workspace_vcs_roots,
            &gcx.indexing_everywhere,
            cache_key,
        )
        .await
    }
}

pub async fn save_trajectory_snapshot(
    gcx: &SharedGlobalContext,
    snapshot: TrajectorySnapshot,
) -> Result<(), String> {
    let ctx = gcx.chat_ctx(gcx.clone());
    crate::trajectory_runtime::save_trajectory_snapshot_for_ctx(&ctx, snapshot).await
}
