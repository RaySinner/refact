#[allow(unused_imports)]
use super::*;

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
    ) -> Option<trajectories::ValidTrajectoryCandidate> {
        trajectories::find_trajectory_or_buddy_file(&self.gcx.chat_ctx(self.gcx.clone()), chat_id)
            .await
    }

    async fn load_trajectory_candidate(
        &self,
        chat_id: &str,
        candidate: trajectories::ValidTrajectoryCandidate,
    ) -> Option<trajectories::LoadedTrajectory> {
        trajectories::load_trajectory_candidate(
            &self.gcx.chat_ctx(self.gcx.clone()),
            chat_id,
            candidate,
        )
        .await
    }

    async fn persist_trajectory_snapshot_with_intent(
        &self,
        snapshot: trajectories::TrajectorySnapshot,
        intent: TrajectoryCommitIntent,
    ) -> Result<(), String> {
        trajectories::persist_trajectory_snapshot_with_intent(
            self.gcx.clone(),
            snapshot,
            intent,
        )
        .await
    }

    async fn refresh_session_from_trajectory_if_stale(
        &self,
        chat_id: &str,
        allow_replacing_messages: bool,
    ) -> bool {
        trajectories::refresh_session_from_trajectory_if_stale(
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
        trajectories::apply_mode_defaults_to_thread(
            self.gcx.clone(),
            thread,
            auto_approve_editing_present,
            auto_approve_dangerous_present,
        )
        .await
    }

    async fn persist_loaded_trajectory_repair_raw(
        &self,
        repair: &trajectories::TrajectoryRepairPatch,
    ) -> Result<String, String> {
        trajectories::persist_loaded_trajectory_repair_raw(
            &self.gcx.chat_ctx(self.gcx.clone()),
            repair,
        )
        .await
    }

    async fn maybe_save_trajectory_with_intent(
        &self,
        session_arc: Arc<AMutex<ChatSession>>,
        intent: TrajectoryCommitIntent,
    ) {
        trajectories::maybe_save_trajectory_with_intent(
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
        trajectories::try_save_trajectory_with_intent(
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
        trajectories::flush_trajectory_for_session(
            app_state_from_gcx(self.gcx.clone()).await,
            session_arc,
        )
        .await
    }

    async fn find_trajectory_path(&self, chat_id: &str) -> Option<PathBuf> {
        trajectories::find_trajectory_path(&self.gcx.chat_ctx(self.gcx.clone()), chat_id).await
    }

    async fn find_trajectory_or_buddy_path(&self, chat_id: &str) -> Option<PathBuf> {
        trajectories::find_trajectory_or_buddy_path(&self.gcx.chat_ctx(self.gcx.clone()), chat_id)
            .await
    }

    async fn find_trajectory_path_for_active_chat(
        &self,
        sessions: &crate::chat::SessionsMap,
        chat_id: &str,
    ) -> Option<PathBuf> {
        trajectories::find_trajectory_path_for_active_chat(
            &self.gcx.chat_ctx(self.gcx.clone()),
            sessions,
            chat_id,
        )
        .await
    }

    async fn read_trajectory_or_buddy_content(&self, chat_id: &str) -> Option<String> {
        trajectories::find_trajectory_or_buddy_file(&self.gcx.chat_ctx(self.gcx.clone()), chat_id)
            .await
            .map(|candidate| candidate.content)
    }

    async fn load_trajectory_by_chat_id(&self, chat_id: &str) -> Option<trajectories::LoadedTrajectory> {
        let ctx = self.gcx.chat_ctx(self.gcx.clone());
        let candidate = trajectories::find_trajectory_file(&ctx, chat_id).await?;
        trajectories::load_trajectory_candidate(&ctx, chat_id, candidate).await
    }

    async fn load_generic_trajectory_for_chat(
        &self,
        chat_id: &str,
    ) -> Option<trajectories::LoadedTrajectory> {
        trajectories::load_generic_trajectory_for_chat(&self.gcx.chat_ctx(self.gcx.clone()), chat_id)
            .await
    }

    async fn load_generic_trajectory_for_chat_matching_source(
        &self,
        chat_id: &str,
        source: &crate::chat::types::TrajectorySourceIdentity,
    ) -> Option<trajectories::LoadedTrajectory> {
        trajectories::load_generic_trajectory_for_chat_matching_source(
            &self.gcx.chat_ctx(self.gcx.clone()),
            chat_id,
            source,
        )
        .await
    }

    async fn trajectory_source_path_is_allowed(&self, path: &std::path::Path) -> bool {
        trajectories::trajectory_source_path_is_allowed(&self.gcx.chat_ctx(self.gcx.clone()), path)
            .await
    }

    async fn safe_new_task_trajectory_file(
        &self,
        task_meta: &crate::chat::types::TaskMeta,
        chat_id: &str,
    ) -> Result<PathBuf, String> {
        trajectories::safe_new_task_trajectory_file(
            &self.gcx.chat_ctx(self.gcx.clone()),
            task_meta,
            chat_id,
        )
        .await
    }

    async fn safe_new_buddy_trajectory_file(&self, chat_id: &str) -> Result<PathBuf, String> {
        trajectories::safe_new_buddy_trajectory_file(&self.gcx.chat_ctx(self.gcx.clone()), chat_id)
            .await
    }

    async fn safe_new_nested_trajectory_file(
        &self,
        chat_id: &str,
        root_chat_id: Option<&str>,
    ) -> Result<PathBuf, String> {
        trajectories::safe_new_nested_trajectory_file(
            &self.gcx.chat_ctx(self.gcx.clone()),
            chat_id,
            root_chat_id,
        )
        .await
    }

    async fn find_normal_trajectory_path(&self, chat_id: &str) -> Option<PathBuf> {
        trajectories::find_normal_trajectory_path(&self.gcx.chat_ctx(self.gcx.clone()), chat_id)
            .await
    }

    async fn resolve_trajectory_data_save_path(
        &self,
        id: &str,
        data: &trajectories::TrajectoryData,
    ) -> Result<PathBuf, String> {
        trajectories::resolve_trajectory_data_save_path(
            &self.gcx.chat_ctx(self.gcx.clone()),
            id,
            data,
        )
        .await
    }

    async fn validate_loaded_worktree_strict(
        &self,
        chat_id: &str,
        worktree: crate::worktrees::types::WorktreeMeta,
    ) -> Option<crate::worktrees::types::WorktreeMeta> {
        trajectories::validate_loaded_worktree_strict(
            &self.gcx.chat_ctx(self.gcx.clone()),
            chat_id,
            worktree,
        )
        .await
    }

    async fn index_dir_for_trajectory_file(&self, file_path: &std::path::Path) -> Option<PathBuf> {
        trajectories::index_dir_for_trajectory_file(
            &self.gcx.chat_ctx(self.gcx.clone()),
            file_path,
        )
        .await
    }

    async fn all_trajectory_dirs(&self) -> Vec<PathBuf> {
        trajectories::get_all_trajectories_dirs(&self.gcx.chat_ctx(self.gcx.clone())).await
    }

    async fn list_trajectory_dirs(&self) -> Vec<PathBuf> {
        trajectories::list_trajectory_dirs(&self.gcx.chat_ctx(self.gcx.clone())).await
    }

    async fn get_all_task_roots(&self) -> Vec<PathBuf> {
        trajectories::get_all_task_roots(&self.gcx.chat_ctx(self.gcx.clone())).await
    }

    async fn enqueue_trajectory_vectorization(&self, file_path: PathBuf) {
        trajectories::enqueue_trajectory_vectorization(
            &self.gcx.chat_ctx(self.gcx.clone()),
            file_path,
        )
        .await
    }

    async fn prune_buddy_conversations_throttled(&self) {
        trajectories::prune_buddy_conversations_throttled(&self.gcx.chat_ctx(self.gcx.clone()))
            .await
    }
}

impl TrajectoryStoreHandle for EngineTrajectoryStore {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

