use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use refact_chat_api::{
    ChatSession, SessionsMap, TaskMeta, ThreadParams, TrajectoryCommitIntent,
    TrajectorySourceIdentity,
};
use refact_chat_history::trajectory_model::{LoadedTrajectory, TrajectoryData, TrajectoryRepairPatch};
use refact_chat_history::trajectory_paths::ValidTrajectoryCandidate;
use refact_chat_history::trajectory_snapshot::TrajectorySnapshot;
use refact_core::worktree_meta::WorktreeMeta;
use tokio::sync::Mutex as AMutex;

use crate::chat_ctx::TrajectoryStore;
use crate::ctx_traits::TrajectoryStoreHandle;
use crate::global_context::{app_state_from_gcx, GlobalContextCtx, SharedGlobalContext};
use crate::trajectories::{load, paths, reload, store};

pub struct EngineTrajectoryStore {
    gcx: SharedGlobalContext,
}

impl EngineTrajectoryStore {
    pub fn new(gcx: SharedGlobalContext) -> Self {
        Self { gcx }
    }
}

#[async_trait]
impl TrajectoryStore for EngineTrajectoryStore {
    async fn find_trajectory_or_buddy_candidate(
        &self,
        chat_id: &str,
    ) -> Option<ValidTrajectoryCandidate> {
        paths::find_trajectory_or_buddy_file(&self.gcx.chat_ctx(self.gcx.clone()), chat_id).await
    }

    async fn load_trajectory_candidate(
        &self,
        chat_id: &str,
        candidate: ValidTrajectoryCandidate,
    ) -> Option<LoadedTrajectory> {
        load::load_trajectory_candidate(&self.gcx.chat_ctx(self.gcx.clone()), chat_id, candidate)
            .await
    }

    async fn persist_trajectory_snapshot_with_intent(
        &self,
        snapshot: TrajectorySnapshot,
        intent: TrajectoryCommitIntent,
    ) -> Result<(), String> {
        store::persist_trajectory_snapshot_with_intent(self.gcx.clone(), snapshot, intent).await
    }

    async fn refresh_session_from_trajectory_if_stale(
        &self,
        chat_id: &str,
        allow_replacing_messages: bool,
    ) -> bool {
        reload::refresh_session_from_trajectory_if_stale(
            &self.gcx.chat_ctx(self.gcx.clone()),
            chat_id,
            allow_replacing_messages,
        )
        .await
    }

    async fn apply_mode_defaults_to_thread(
        &self,
        thread: &mut ThreadParams,
        auto_approve_editing_present: bool,
        auto_approve_dangerous_present: bool,
    ) {
        crate::trajectory_model_runtime::apply_mode_defaults_to_thread(
            self.gcx.clone(),
            thread,
            auto_approve_editing_present,
            auto_approve_dangerous_present,
        )
        .await
    }

    async fn persist_loaded_trajectory_repair_raw(
        &self,
        repair: &TrajectoryRepairPatch,
    ) -> Result<String, String> {
        reload::persist_loaded_trajectory_repair_raw(&self.gcx.chat_ctx(self.gcx.clone()), repair)
            .await
    }

    async fn maybe_save_trajectory_with_intent(
        &self,
        session_arc: Arc<AMutex<ChatSession>>,
        intent: TrajectoryCommitIntent,
    ) {
        store::maybe_save_trajectory_with_intent(
            app_state_from_gcx(self.gcx.clone()).await,
            session_arc,
            intent,
        )
        .await
    }

    async fn try_save_trajectory_with_intent(
        &self,
        session_arc: Arc<AMutex<ChatSession>>,
        intent: TrajectoryCommitIntent,
    ) -> Result<bool, String> {
        store::try_save_trajectory_with_intent(
            app_state_from_gcx(self.gcx.clone()).await,
            session_arc,
            intent,
        )
        .await
    }

    async fn flush_trajectory_for_session(
        &self,
        session_arc: Arc<AMutex<ChatSession>>,
    ) -> Result<(), String> {
        store::flush_trajectory_for_session(app_state_from_gcx(self.gcx.clone()).await, session_arc)
            .await
    }

    async fn find_trajectory_path(&self, chat_id: &str) -> Option<PathBuf> {
        crate::trajectory_runtime::find_trajectory_path(
            &self.gcx.chat_ctx(self.gcx.clone()),
            chat_id,
        )
        .await
    }

    async fn find_trajectory_or_buddy_path(&self, chat_id: &str) -> Option<PathBuf> {
        paths::find_trajectory_or_buddy_path(&self.gcx.chat_ctx(self.gcx.clone()), chat_id).await
    }

    async fn find_trajectory_path_for_active_chat(
        &self,
        sessions: &SessionsMap,
        chat_id: &str,
    ) -> Option<PathBuf> {
        paths::find_trajectory_path_for_active_chat(
            &self.gcx.chat_ctx(self.gcx.clone()),
            sessions,
            chat_id,
        )
        .await
    }

    async fn read_trajectory_or_buddy_content(&self, chat_id: &str) -> Option<String> {
        paths::find_trajectory_or_buddy_file(&self.gcx.chat_ctx(self.gcx.clone()), chat_id)
            .await
            .map(|candidate| candidate.content)
    }

    async fn load_trajectory_by_chat_id(&self, chat_id: &str) -> Option<LoadedTrajectory> {
        let ctx = self.gcx.chat_ctx(self.gcx.clone());
        let candidate = paths::find_trajectory_file(&ctx, chat_id).await?;
        load::load_trajectory_candidate(&ctx, chat_id, candidate).await
    }

    async fn load_generic_trajectory_for_chat(&self, chat_id: &str) -> Option<LoadedTrajectory> {
        load::load_generic_trajectory_for_chat(&self.gcx.chat_ctx(self.gcx.clone()), chat_id).await
    }

    async fn load_generic_trajectory_for_chat_matching_source(
        &self,
        chat_id: &str,
        source: &TrajectorySourceIdentity,
    ) -> Option<LoadedTrajectory> {
        load::load_generic_trajectory_for_chat_matching_source(
            &self.gcx.chat_ctx(self.gcx.clone()),
            chat_id,
            source,
        )
        .await
    }

    async fn trajectory_source_path_is_allowed(&self, path: &std::path::Path) -> bool {
        load::trajectory_source_path_is_allowed(&self.gcx.chat_ctx(self.gcx.clone()), path).await
    }

    async fn safe_new_task_trajectory_file(
        &self,
        task_meta: &TaskMeta,
        chat_id: &str,
    ) -> Result<PathBuf, String> {
        load::safe_new_task_trajectory_file(
            &self.gcx.chat_ctx(self.gcx.clone()),
            task_meta,
            chat_id,
        )
        .await
    }

    async fn safe_new_buddy_trajectory_file(&self, chat_id: &str) -> Result<PathBuf, String> {
        load::safe_new_buddy_trajectory_file(&self.gcx.chat_ctx(self.gcx.clone()), chat_id).await
    }

    async fn safe_new_nested_trajectory_file(
        &self,
        chat_id: &str,
        root_chat_id: Option<&str>,
    ) -> Result<PathBuf, String> {
        load::safe_new_nested_trajectory_file(
            &self.gcx.chat_ctx(self.gcx.clone()),
            chat_id,
            root_chat_id,
        )
        .await
    }

    async fn find_normal_trajectory_path(&self, chat_id: &str) -> Option<PathBuf> {
        paths::find_normal_trajectory_path(&self.gcx.chat_ctx(self.gcx.clone()), chat_id).await
    }

    async fn resolve_trajectory_data_save_path(
        &self,
        id: &str,
        data: &TrajectoryData,
    ) -> Result<PathBuf, String> {
        load::resolve_trajectory_data_save_path(&self.gcx.chat_ctx(self.gcx.clone()), id, data)
            .await
    }

    async fn validate_loaded_worktree_strict(
        &self,
        chat_id: &str,
        worktree: WorktreeMeta,
    ) -> Option<WorktreeMeta> {
        load::validate_loaded_worktree_strict(
            &self.gcx.chat_ctx(self.gcx.clone()),
            chat_id,
            worktree,
        )
        .await
    }

    async fn index_dir_for_trajectory_file(&self, file_path: &std::path::Path) -> Option<PathBuf> {
        paths::index_dir_for_trajectory_file(&self.gcx.chat_ctx(self.gcx.clone()), file_path).await
    }

    async fn all_trajectory_dirs(&self) -> Vec<PathBuf> {
        paths::get_all_trajectories_dirs(&self.gcx.chat_ctx(self.gcx.clone())).await
    }

    async fn list_trajectory_dirs(&self) -> Vec<PathBuf> {
        paths::list_trajectory_dirs(&self.gcx.chat_ctx(self.gcx.clone())).await
    }

    async fn get_all_task_roots(&self) -> Vec<PathBuf> {
        paths::get_all_task_roots(&self.gcx.chat_ctx(self.gcx.clone())).await
    }

    async fn enqueue_trajectory_vectorization(&self, file_path: PathBuf) {
        paths::enqueue_trajectory_vectorization(&self.gcx.chat_ctx(self.gcx.clone()), file_path)
            .await
    }

    async fn prune_buddy_conversations_throttled(&self) {
        paths::prune_buddy_conversations_throttled(&self.gcx.chat_ctx(self.gcx.clone())).await
    }
}

impl TrajectoryStoreHandle for EngineTrajectoryStore {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}
