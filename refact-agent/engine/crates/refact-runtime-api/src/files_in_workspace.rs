use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use refact_codegraph::QueuedPath;
use refact_core::privacy_types::PrivacySettings;
use refact_worktrees::path_mappings::WorktreePathMappings;

use crate::global_context::GlobalContext;

pub struct FileReadContext {
    pub privacy_settings: Arc<PrivacySettings>,
    pub worktree_mappings: WorktreePathMappings,
    pub runtime: Arc<dyn FilesRuntime>,
}

#[async_trait]
pub trait FilesRuntime: Send + Sync {
    async fn remove_memory_document_for_path(
        &self,
        gcx: Arc<GlobalContext>,
        path: &PathBuf,
    ) -> bool;
    async fn remove_memory_documents_under_path(
        &self,
        gcx: Arc<GlobalContext>,
        path: &PathBuf,
    ) -> usize;
    async fn get_file_text_from_memory_or_disk(
        &self,
        global_context: Arc<GlobalContext>,
        file_path: &PathBuf,
    ) -> Result<String, String>;
    async fn get_file_text_from_memory_or_disk_for_model_context(
        &self,
        global_context: Arc<GlobalContext>,
        file_path: &PathBuf,
    ) -> Result<String, String>;
    async fn prepare_file_read_context(
        &self,
        global_context: Arc<GlobalContext>,
    ) -> FileReadContext;
    async fn get_file_text_from_memory_or_disk_for_model_context_with_context(
        &self,
        global_context: Arc<GlobalContext>,
        file_path: &PathBuf,
        read_context: &FileReadContext,
        max_bytes: Option<usize>,
    ) -> Result<String, String>;
    async fn check_file_privacy_for_send(
        &self,
        global_context: Arc<GlobalContext>,
        file_path: &PathBuf,
    ) -> Result<(), String>;
    async fn check_file_privacy_for_model_context(
        &self,
        global_context: Arc<GlobalContext>,
        file_path: &PathBuf,
    ) -> Result<(), String>;
    async fn filter_privacy_allowed_files(
        &self,
        global_context: Arc<GlobalContext>,
        files: Vec<PathBuf>,
    ) -> Vec<PathBuf>;
    async fn filter_privacy_allowed_files_for_model_context(
        &self,
        global_context: Arc<GlobalContext>,
        files: Vec<PathBuf>,
    ) -> Vec<PathBuf>;
    async fn enqueue_some_docs_with_read_paths(
        &self,
        gcx: Arc<GlobalContext>,
        paths: &[QueuedPath],
        force: bool,
    );
    async fn enqueue_all_files_from_workspace_folders(
        &self,
        gcx: Arc<GlobalContext>,
        wake_up_indexers: bool,
        vecdb_only: bool,
    ) -> i32;
    async fn paths_from_anywhere(&self, global_context: Arc<GlobalContext>) -> Vec<PathBuf>;
    async fn correct_to_nearest_filename(
        &self,
        gcx: Arc<GlobalContext>,
        correction_candidate: &String,
        fuzzy: bool,
        top_n: usize,
    ) -> Vec<String>;
    async fn correct_to_nearest_dir_path(
        &self,
        gcx: Arc<GlobalContext>,
        correction_candidate: &String,
        fuzzy: bool,
        top_n: usize,
    ) -> Vec<String>;
    async fn get_project_dirs(&self, gcx: Arc<GlobalContext>) -> Vec<PathBuf>;
    async fn get_unscoped_project_dirs(&self, gcx: Arc<GlobalContext>) -> Vec<PathBuf>;
    async fn get_active_project_path(&self, gcx: Arc<GlobalContext>) -> Option<PathBuf>;
    async fn get_active_project_path_for_services(
        &self,
        paths: &crate::PathServices,
        workspace: &crate::WorkspaceServices,
    ) -> Option<PathBuf>;
    async fn shortify_paths(&self, gcx: Arc<GlobalContext>, paths: &Vec<String>) -> Vec<String>;
    async fn check_if_its_inside_a_workspace_or_config(
        &self,
        gcx: Arc<GlobalContext>,
        path: &Path,
    ) -> Result<(), String>;
    fn check_file_privacy_with_context(
        &self,
        read_context: &FileReadContext,
        path: &Path,
    ) -> Result<(), String>;
    fn check_file_privacy_for_read_preparation_with_context(
        &self,
        read_context: &FileReadContext,
        path: &Path,
    ) -> Result<(), String>;
}

pub async fn remove_memory_document_for_path(gcx: Arc<GlobalContext>, path: &PathBuf) -> bool {
    gcx.files_runtime
        .clone()
        .remove_memory_document_for_path(gcx, path)
        .await
}

pub async fn remove_memory_documents_under_path(gcx: Arc<GlobalContext>, path: &PathBuf) -> usize {
    gcx.files_runtime
        .clone()
        .remove_memory_documents_under_path(gcx, path)
        .await
}

pub async fn get_file_text_from_memory_or_disk(
    global_context: Arc<GlobalContext>,
    file_path: &PathBuf,
) -> Result<String, String> {
    global_context
        .files_runtime
        .clone()
        .get_file_text_from_memory_or_disk(global_context, file_path)
        .await
}

pub async fn get_file_text_from_memory_or_disk_for_model_context(
    global_context: Arc<GlobalContext>,
    file_path: &PathBuf,
) -> Result<String, String> {
    global_context
        .files_runtime
        .clone()
        .get_file_text_from_memory_or_disk_for_model_context(global_context, file_path)
        .await
}

pub async fn prepare_file_read_context(global_context: Arc<GlobalContext>) -> FileReadContext {
    global_context
        .files_runtime
        .clone()
        .prepare_file_read_context(global_context)
        .await
}

pub async fn get_file_text_from_memory_or_disk_for_model_context_with_context(
    global_context: Arc<GlobalContext>,
    file_path: &PathBuf,
    read_context: &FileReadContext,
    max_bytes: Option<usize>,
) -> Result<String, String> {
    global_context
        .files_runtime
        .clone()
        .get_file_text_from_memory_or_disk_for_model_context_with_context(
            global_context,
            file_path,
            read_context,
            max_bytes,
        )
        .await
}

pub async fn check_file_privacy_for_send(
    global_context: Arc<GlobalContext>,
    file_path: &PathBuf,
) -> Result<(), String> {
    global_context
        .files_runtime
        .clone()
        .check_file_privacy_for_send(global_context, file_path)
        .await
}

pub async fn check_file_privacy_for_model_context(
    global_context: Arc<GlobalContext>,
    file_path: &PathBuf,
) -> Result<(), String> {
    global_context
        .files_runtime
        .clone()
        .check_file_privacy_for_model_context(global_context, file_path)
        .await
}

pub async fn filter_privacy_allowed_files(
    global_context: Arc<GlobalContext>,
    files: Vec<PathBuf>,
) -> Vec<PathBuf> {
    global_context
        .files_runtime
        .clone()
        .filter_privacy_allowed_files(global_context, files)
        .await
}

pub async fn filter_privacy_allowed_files_for_model_context(
    global_context: Arc<GlobalContext>,
    files: Vec<PathBuf>,
) -> Vec<PathBuf> {
    global_context
        .files_runtime
        .clone()
        .filter_privacy_allowed_files_for_model_context(global_context, files)
        .await
}

pub async fn enqueue_some_docs_with_read_paths(
    gcx: Arc<GlobalContext>,
    paths: &[QueuedPath],
    force: bool,
) {
    gcx.files_runtime
        .clone()
        .enqueue_some_docs_with_read_paths(gcx, paths, force)
        .await
}

pub async fn enqueue_all_files_from_workspace_folders(
    gcx: Arc<GlobalContext>,
    wake_up_indexers: bool,
    vecdb_only: bool,
) -> i32 {
    gcx.files_runtime
        .clone()
        .enqueue_all_files_from_workspace_folders(gcx, wake_up_indexers, vecdb_only)
        .await
}

pub fn check_file_privacy_with_context(
    read_context: &FileReadContext,
    path: &Path,
) -> Result<(), String> {
    read_context
        .runtime
        .check_file_privacy_with_context(read_context, path)
}

pub fn check_file_privacy_for_read_preparation_with_context(
    read_context: &FileReadContext,
    path: &Path,
) -> Result<(), String> {
    read_context
        .runtime
        .check_file_privacy_for_read_preparation_with_context(read_context, path)
}
