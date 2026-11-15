use std::path::PathBuf;
use std::sync::Arc;

use refact_core::ast_types::Document;
use refact_core::privacy_types::FilePrivacyLevel;
use ropey::Rope;

use crate::global_context::GlobalContext;

async fn read_file_from_disk_checked(
    gcx: Arc<GlobalContext>,
    path: &PathBuf,
) -> Result<Rope, String> {
    let privacy_settings = crate::privacy::load_privacy_if_needed(gcx).await;
    refact_privacy::check_file_privacy(
        privacy_settings,
        path,
        &FilePrivacyLevel::AllowToSendAnywhere,
    )?;
    tokio::fs::read_to_string(path)
        .await
        .map(|text| Rope::from_str(&text))
        .map_err(|e| {
            format!(
                "failed to read file {}: {}",
                refact_core::nicer_logs::last_n_chars(&path.display().to_string(), 30),
                e
            )
        })
}

pub async fn update_document_text_from_disk(
    doc: &mut Document,
    gcx: Arc<GlobalContext>,
) -> Result<(), String> {
    doc.doc_text = Some(read_file_from_disk_checked(gcx, &doc.doc_path).await?);
    Ok(())
}

pub async fn get_document_text_or_read_from_disk(
    doc: &mut Document,
    gcx: Arc<GlobalContext>,
) -> Result<String, String> {
    if let Some(text) = doc.doc_text.as_ref() {
        return Ok(text.to_string());
    }
    read_file_from_disk_checked(gcx, &doc.doc_path)
        .await
        .map(|text| text.to_string())
}
