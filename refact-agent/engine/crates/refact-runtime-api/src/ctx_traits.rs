use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use refact_caps_core::code_assistant_caps::CodeAssistantCaps;
use refact_core::custom_error::ScratchError;
use refact_files::indexing_settings::IndexingEverywhere;
use refact_yaml_configs::customization_types::{ModeConfig, ProjectRegistry, SubagentConfig};

#[async_trait]
pub trait FilesProvider: Send + Sync {
    async fn get_file_text_from_memory_or_disk(&self, path: &PathBuf) -> Result<String, String>;
    async fn fast_enqueue_for_edit(&self, docs: &[PathBuf]) -> Result<(), String>;
    async fn remove_memory_document_for_path(&self, path: &PathBuf) -> bool;
    async fn file_repair_candidates(&self, value: &str, top_n: usize, fuzzy: bool) -> Vec<String>;
    async fn project_dirs(&self) -> Vec<PathBuf>;
    async fn check_file_privacy_for_send(&self, path: &PathBuf) -> Result<(), String>;
    async fn check_file_privacy_for_model_context(&self, path: &PathBuf) -> Result<(), String>;
    async fn filter_privacy_allowed_files(&self, files: Vec<PathBuf>) -> Vec<PathBuf>;
    async fn filter_privacy_allowed_files_for_model_context(
        &self,
        files: Vec<PathBuf>,
    ) -> Vec<PathBuf>;
    async fn indexing_everywhere(&self) -> Arc<IndexingEverywhere>;
    async fn correct_to_nearest_dir_path(
        &self,
        correction_candidate: &str,
        fuzzy: bool,
        top_n: usize,
    ) -> Vec<String>;
    async fn correct_to_nearest_filename(
        &self,
        correction_candidate: &str,
        fuzzy: bool,
        top_n: usize,
    ) -> Vec<String>;
    async fn shortify_paths(&self, paths: &[String]) -> Vec<String>;
    async fn paths_from_anywhere(&self) -> Vec<PathBuf>;
    async fn get_file_text_from_memory_or_disk_for_model_context(
        &self,
        path: &PathBuf,
    ) -> Result<String, String>;
    async fn return_one_candidate_or_a_good_error(
        &self,
        file_path: &str,
        candidates: &[String],
        project_paths: &[PathBuf],
        dirs: bool,
    ) -> Result<String, String>;
}

#[async_trait]
pub trait ModeConfigProvider: Send + Sync {
    async fn warn_unknown_mode_tools(&self, mode_id: &str, declared: &[String], registered: &std::collections::HashSet<String>);
    async fn get_project_registry(&self) -> Option<ProjectRegistry>;
    async fn get_mode_config(&self, mode_id: &str, model_id: Option<&str>) -> Option<ModeConfig>;
    async fn get_subagent_config(
        &self,
        subagent_id: &str,
        model_id: Option<&str>,
    ) -> Option<SubagentConfig>;
}

#[async_trait]
pub trait CapsProvider: Send + Sync {
    async fn try_load_caps_quickly(
        &self,
        max_age_seconds: u64,
    ) -> Result<Arc<CodeAssistantCaps>, ScratchError>;
}

/// Handle seam over the engine buddy services bundle (BuddyService, BuddyEvent
/// and UserActivityRing remain engine types until W7-1c).
pub trait BuddyServicesHandle: Send + Sync {
    fn as_any(&self) -> &dyn std::any::Any;
}

/// Handle seam over the engine trajectory store trait object. The engine
/// `TrajectoryStore` trait carries engine-only types and moves in a later wave.
pub trait TrajectoryStoreHandle: Send + Sync {
    fn as_any(&self) -> &dyn std::any::Any;
}

/// Handle seam over the engine subchat launcher trait object. The engine
/// `SubchatLauncher` trait carries engine-only types and moves in a later wave.
pub trait SubchatLauncherHandle: Send + Sync {
    fn as_any(&self) -> &dyn std::any::Any;
}
