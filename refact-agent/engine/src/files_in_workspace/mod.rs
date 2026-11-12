use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Weak};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use indexmap::IndexSet;
use notify::{Event, EventKind};
use notify::event::{AccessKind, AccessMode, CreateKind, ModifyKind, RemoveKind};
use ropey::Rope;
use tokio::io::AsyncReadExt;
use tokio::sync::{RwLock as ARwLock, Mutex as AMutex};
use walkdir::WalkDir;
use which::which;
use tracing::info;
use chrono::Utc;
use refact_codegraph::QueuedPath;

use refact_buddy_core::user_action::UserAction;
use crate::files_correction::{
    canonical_path, resolve_codegraph_queue_path, CommandSimplifiedDirExt,
};
use crate::global_context::GlobalContextCtx;
use crate::git::operations::git_ls_files;
use crate::global_context::{get_app_searchable_id, GlobalContext};
use crate::chat::ctx::ChatCtx;
use crate::integrations::running_integrations::load_integrations;
use crate::file_filter::SOURCE_FILE_EXTENSIONS;
use crate::privacy::{
    check_file_privacy, check_file_privacy_for_read_preparation, load_privacy_if_needed,
    FilePrivacyLevel, PrivacySettings,
};
use crate::files_blocklist::is_blocklisted;
use crate::files_in_jsonl::enqueue_all_docs_from_jsonl_but_read_first;
pub use refact_files::correction_cache::CacheCorrection;

pub use refact_files::path_classification::{
    path_for_blocklist, path_has_allowed_hidden_component, path_has_hidden_component,
    path_is_refact_import_internal, path_is_refact_internal, path_is_refact_runtime_state,
    path_triggers_registry_reload, path_triggers_tool_catalog_reload,
    refact_runtime_entry_matches, REFACT_RUNTIME_STATE_ENTRIES,
    REFACT_RUNTIME_STATE_TOP_LEVEL_ENTRIES,
};
pub use refact_files::watcher_plan::{
    build_watch_plan, build_watch_plan_for_root, classify_watch_subtree,
    path_is_excluded_from_watch, WatchRegistration, WatchRegistrationMode, WatchSubtreeClass,
};

pub use refact_runtime_api::files_in_workspace::{
    check_file_privacy_for_model_context, check_file_privacy_for_send,
    enqueue_all_files_from_workspace_folders, enqueue_some_docs_with_read_paths,
    filter_privacy_allowed_files, filter_privacy_allowed_files_for_model_context,
    get_file_text_from_memory_or_disk, get_file_text_from_memory_or_disk_for_model_context,
    remove_memory_document_for_path, remove_memory_documents_under_path,
};
pub(crate) use refact_runtime_api::files_in_workspace::{
    check_file_privacy_for_read_preparation_with_context, check_file_privacy_with_context,
    get_file_text_from_memory_or_disk_for_model_context_with_context, prepare_file_read_context,
    FileReadContext,
};

// How this works
// --------------
//
// IDE Window communicates workspace folders via LSP:
//    workspace_folder1:
//       some_dir/
//          vcs_root1/
//       vcs_root2/
//    workspace_folder2:
//       dir_without_version/
//          maybe_because_its_new/
//
// We use version control (git, hg, svn) to list files, whenever we can find it.
// If we can't, just use built-in blocklist and recursive directory walk.
// When a file event arrives (such as file created, file modified) we just add the file into index, because it
// might be new (not yet in version control), but apply blocklists to avoid indexing all kinds of junk
// files.
// So blocklist is mainly useful to deal with file events.
// You can customize blocklist using:
//   ~/.config/refact/indexing.yaml
//   ~/path/to/your/project/.refact/indexing.yaml

mod compat;
mod index;
mod refresh;
mod watcher;

pub use compat::*;
pub use index::*;
pub use refresh::*;
pub use watcher::*;

#[cfg(test)]
mod tests;

pub use refact_files::documents_state::*;

pub use refact_files::workspace::{_ls_files, ls_files, ls_files_limited, LsFilesLimited};

pub struct EngineFilesRuntime;

#[async_trait::async_trait]
impl refact_runtime_api::files_in_workspace::FilesRuntime for EngineFilesRuntime {
    async fn remove_memory_document_for_path(
        &self,
        gcx: Arc<GlobalContext>,
        path: &PathBuf,
    ) -> bool {
        crate::files_in_workspace::remove_memory_document_for_path_impl(gcx, path).await
    }

    async fn remove_memory_documents_under_path(
        &self,
        gcx: Arc<GlobalContext>,
        path: &PathBuf,
    ) -> usize {
        crate::files_in_workspace::remove_memory_documents_under_path_impl(gcx, path).await
    }

    async fn get_file_text_from_memory_or_disk(
        &self,
        global_context: Arc<GlobalContext>,
        file_path: &PathBuf,
    ) -> Result<String, String> {
        crate::files_in_workspace::get_file_text_from_memory_or_disk_impl(global_context, file_path)
            .await
    }

    async fn get_file_text_from_memory_or_disk_for_model_context(
        &self,
        global_context: Arc<GlobalContext>,
        file_path: &PathBuf,
    ) -> Result<String, String> {
        crate::files_in_workspace::get_file_text_from_memory_or_disk_for_model_context_impl(
            global_context,
            file_path,
        )
        .await
    }

    async fn prepare_file_read_context(
        &self,
        global_context: Arc<GlobalContext>,
    ) -> FileReadContext {
        crate::files_in_workspace::prepare_file_read_context_impl(global_context).await
    }

    async fn get_file_text_from_memory_or_disk_for_model_context_with_context(
        &self,
        global_context: Arc<GlobalContext>,
        file_path: &PathBuf,
        read_context: &FileReadContext,
        max_bytes: Option<usize>,
    ) -> Result<String, String> {
        crate::files_in_workspace::get_file_text_from_memory_or_disk_for_model_context_with_context_impl(global_context, file_path, read_context, max_bytes).await
    }

    async fn check_file_privacy_for_send(
        &self,
        global_context: Arc<GlobalContext>,
        file_path: &PathBuf,
    ) -> Result<(), String> {
        crate::files_in_workspace::check_file_privacy_for_send_impl(global_context, file_path).await
    }

    async fn check_file_privacy_for_model_context(
        &self,
        global_context: Arc<GlobalContext>,
        file_path: &PathBuf,
    ) -> Result<(), String> {
        crate::files_in_workspace::check_file_privacy_for_model_context_impl(
            global_context,
            file_path,
        )
        .await
    }

    async fn filter_privacy_allowed_files(
        &self,
        global_context: Arc<GlobalContext>,
        files: Vec<PathBuf>,
    ) -> Vec<PathBuf> {
        crate::files_in_workspace::filter_privacy_allowed_files_impl(global_context, files).await
    }

    async fn filter_privacy_allowed_files_for_model_context(
        &self,
        global_context: Arc<GlobalContext>,
        files: Vec<PathBuf>,
    ) -> Vec<PathBuf> {
        crate::files_in_workspace::filter_privacy_allowed_files_for_model_context_impl(
            global_context,
            files,
        )
        .await
    }

    async fn enqueue_some_docs_with_read_paths(
        &self,
        gcx: Arc<GlobalContext>,
        paths: &[QueuedPath],
        force: bool,
    ) {
        crate::files_in_workspace::enqueue_some_docs_with_read_paths_impl(gcx, paths, force).await
    }

    async fn enqueue_all_files_from_workspace_folders(
        &self,
        gcx: Arc<GlobalContext>,
        wake_up_indexers: bool,
        vecdb_only: bool,
    ) -> i32 {
        crate::files_in_workspace::enqueue_all_files_from_workspace_folders_impl(
            gcx,
            wake_up_indexers,
            vecdb_only,
        )
        .await
    }

    async fn paths_from_anywhere(&self, global_context: Arc<GlobalContext>) -> Vec<PathBuf> {
        crate::files_correction::paths_from_anywhere_impl(global_context).await
    }

    async fn correct_to_nearest_filename(
        &self,
        gcx: Arc<GlobalContext>,
        correction_candidate: &String,
        fuzzy: bool,
        top_n: usize,
    ) -> Vec<String> {
        crate::files_correction::correct_to_nearest_filename_impl(
            gcx,
            correction_candidate,
            fuzzy,
            top_n,
        )
        .await
    }

    async fn correct_to_nearest_dir_path(
        &self,
        gcx: Arc<GlobalContext>,
        correction_candidate: &String,
        fuzzy: bool,
        top_n: usize,
    ) -> Vec<String> {
        crate::files_correction::correct_to_nearest_dir_path_impl(
            gcx,
            correction_candidate,
            fuzzy,
            top_n,
        )
        .await
    }

    async fn get_project_dirs(&self, gcx: Arc<GlobalContext>) -> Vec<PathBuf> {
        crate::files_correction::get_project_dirs_impl(gcx).await
    }

    async fn get_unscoped_project_dirs(&self, gcx: Arc<GlobalContext>) -> Vec<PathBuf> {
        crate::files_correction::get_unscoped_project_dirs_impl(gcx).await
    }

    async fn get_active_project_path(&self, gcx: Arc<GlobalContext>) -> Option<PathBuf> {
        crate::files_correction::get_active_project_path_impl(gcx).await
    }

    async fn get_active_project_path_for_services(
        &self,
        paths: &crate::app_state::PathServices,
        workspace: &crate::app_state::WorkspaceServices,
    ) -> Option<PathBuf> {
        crate::files_correction::get_active_project_path_for_services_impl(paths, workspace).await
    }

    async fn shortify_paths(&self, gcx: Arc<GlobalContext>, paths: &Vec<String>) -> Vec<String> {
        crate::files_correction::shortify_paths_impl(gcx, paths).await
    }

    async fn check_if_its_inside_a_workspace_or_config(
        &self,
        gcx: Arc<GlobalContext>,
        path: &Path,
    ) -> Result<(), String> {
        crate::files_correction::check_if_its_inside_a_workspace_or_config_impl(gcx, path).await
    }

    fn check_file_privacy_with_context(
        &self,
        read_context: &FileReadContext,
        path: &Path,
    ) -> Result<(), String> {
        crate::files_in_workspace::check_file_privacy_with_context_impl(read_context, path)
    }

    fn check_file_privacy_for_read_preparation_with_context(
        &self,
        read_context: &FileReadContext,
        path: &Path,
    ) -> Result<(), String> {
        crate::files_in_workspace::check_file_privacy_for_read_preparation_with_context_impl(
            read_context,
            path,
        )
    }
}
