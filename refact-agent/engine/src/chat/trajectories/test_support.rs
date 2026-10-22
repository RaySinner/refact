use super::*;
    use crate::chat::diagnostics::{is_ui_only_message, make_ui_only_error_message};
    use crate::chat::perf_diagnostics::{self, MemoryPerfSink, PerfClock, PerfRecorder};
    use crate::chat::types::{
        ActiveCommandContext, ChatEvent, CompressionPhase, CompressionReason, EventEnvelope,
    };
    use crate::vecdb::vdb_structs::{EmbeddingModelConfig, SearchResult, VecDbStatus, VecdbRecord};
    use async_trait::async_trait;
    use refact_core::vecdb_types::VecdbSearch;
    use refact_chat_api::{
        BuddyThreadMeta, ClaudeCodeIdentity, FrozenRequestPrefix, GoalBudget, GoalProgress,
        GoalStatus,
    };
    use serial_test::serial;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::sync::Mutex as StdMutex;
    pub(super) struct BlockingRecordingVecdb {
        pub(super) enqueued: Arc<StdMutex<Vec<String>>>,
        pub(super) enqueue_started: Arc<Notify>,
        pub(super) release_enqueue: Arc<Notify>,
    }

    pub(super) struct PanickingVecdb {
        pub(super) enqueue_started: Arc<Notify>,
    }

    #[async_trait]
    impl VecdbSearch for BlockingRecordingVecdb {
        async fn vecdb_search(
            &self,
            query: String,
            _top_n: usize,
            _filter_mb: Option<String>,
        ) -> Result<SearchResult, String> {
            Ok(SearchResult {
                query_text: query,
                results: vec![],
            })
        }

        async fn get_status(&self) -> Result<VecDbStatus, String> {
            Ok(VecDbStatus {
                files_unprocessed: 0,
                files_total: 0,
                requests_made_since_start: 0,
                vectors_made_since_start: 0,
                db_size: 0,
                db_cache_size: 0,
                state: "done".to_string(),
                queue_additions: false,
                vecdb_max_files_hit: false,
                vecdb_errors: Default::default(),
                cache_writes_failed: 0,
                vectors_missing_from_cache: 0,
            })
        }

        async fn remove_file(&self, _file_path: &PathBuf) -> Result<(), String> {
            Ok(())
        }

        async fn vectorizer_enqueue_files(
            &self,
            documents: &[String],
            _process_immediately: bool,
            _roots: refact_core::memory_plane::MemoryPlaneRoots,
        ) {
            self.enqueued.lock().unwrap().extend(documents.to_vec());
            self.enqueue_started.notify_one();
            self.release_enqueue.notified().await;
        }

        fn current_constants(&self) -> (EmbeddingModelConfig, usize) {
            (
                EmbeddingModelConfig {
                    model_id: String::new(),
                    endpoint: String::new(),
                    endpoint_style: String::new(),
                    embedding_endpoint_style: String::new(),
                    api_key: String::new(),
                    model_name: String::new(),
                    embedding_size: 0,
                    dimensions: None,
                    query_prefix: String::new(),
                    document_prefix: String::new(),
                    rejection_threshold: 0.0,
                    embedding_batch: 1,
                    n_ctx: 0,
                },
                0,
            )
        }

        async fn embed_query(&self, _query: &str) -> Result<Vec<f32>, String> {
            Ok(vec![])
        }

        async fn vecdb_search_with_embedding(
            &self,
            _embedding: &Vec<f32>,
            _top_n: usize,
            _filter_mb: Option<String>,
        ) -> Result<Vec<VecdbRecord>, String> {
            Ok(vec![])
        }
    }

    #[async_trait]
    impl VecdbSearch for PanickingVecdb {
        async fn vecdb_search(
            &self,
            query: String,
            _top_n: usize,
            _filter_mb: Option<String>,
        ) -> Result<SearchResult, String> {
            Ok(SearchResult {
                query_text: query,
                results: vec![],
            })
        }

        async fn get_status(&self) -> Result<VecDbStatus, String> {
            Ok(VecDbStatus {
                files_unprocessed: 0,
                files_total: 0,
                requests_made_since_start: 0,
                vectors_made_since_start: 0,
                db_size: 0,
                db_cache_size: 0,
                state: "done".to_string(),
                queue_additions: false,
                vecdb_max_files_hit: false,
                vecdb_errors: Default::default(),
                cache_writes_failed: 0,
                vectors_missing_from_cache: 0,
            })
        }

        async fn remove_file(&self, _file_path: &PathBuf) -> Result<(), String> {
            Ok(())
        }

        async fn vectorizer_enqueue_files(
            &self,
            _documents: &[String],
            _process_immediately: bool,
            _roots: refact_core::memory_plane::MemoryPlaneRoots,
        ) {
            self.enqueue_started.notify_one();
            panic!("test VecDB enqueue failure");
        }

        fn current_constants(&self) -> (EmbeddingModelConfig, usize) {
            (
                EmbeddingModelConfig {
                    model_id: String::new(),
                    endpoint: String::new(),
                    endpoint_style: String::new(),
                    embedding_endpoint_style: String::new(),
                    api_key: String::new(),
                    model_name: String::new(),
                    embedding_size: 0,
                    dimensions: None,
                    query_prefix: String::new(),
                    document_prefix: String::new(),
                    rejection_threshold: 0.0,
                    embedding_batch: 1,
                    n_ctx: 0,
                },
                0,
            )
        }

        async fn embed_query(&self, _query: &str) -> Result<Vec<f32>, String> {
            Ok(vec![])
        }

        async fn vecdb_search_with_embedding(
            &self,
            _embedding: &Vec<f32>,
            _top_n: usize,
            _filter_mb: Option<String>,
        ) -> Result<Vec<VecdbRecord>, String> {
            Ok(vec![])
        }
    }

    pub(super) fn normalized_test_path(path: &Path) -> PathBuf {
        crate::files_correction::canonicalize_normalized_path(path.to_path_buf())
    }

    pub(super) struct TestPerfClock {
        pub(super) now: AtomicU64,
    }

    impl TestPerfClock {
        fn new() -> Self {
            Self {
                now: AtomicU64::new(0),
            }
        }
    }

    impl PerfClock for TestPerfClock {
        fn now_us(&self) -> u64 {
            self.now.fetch_add(1, Ordering::SeqCst)
        }
    }

    pub(super) fn install_perf_recorder() -> (
        perf_diagnostics::TestRecorderGuard,
        Arc<MemoryPerfSink>,
        Arc<PerfRecorder>,
    ) {
        let sink = Arc::new(MemoryPerfSink::new());
        let recorder = Arc::new(PerfRecorder::with_salt(
            Arc::new(TestPerfClock::new()),
            sink.clone(),
            [5; 32],
        ));
        (
            perf_diagnostics::install_test_recorder(recorder.clone()),
            sink,
            recorder,
        )
    }

    pub(super) fn serial_test_guard() -> std::sync::MutexGuard<'static, ()> {
        perf_diagnostics::PERF_RECORDER_TEST_LOCK.lock().unwrap()
    }

    pub(super) fn assert_same_path(left: &Path, right: &Path) {
        assert_eq!(normalized_test_path(left), normalized_test_path(right));
    }

    pub(super) fn assert_same_optional_path(left: Option<PathBuf>, right: &Path) {
        assert_same_path(&left.expect("path should exist"), right);
    }

    pub(super) fn assert_same_path_str(left: &str, right: &Path) {
        assert_same_path(Path::new(left), right);
    }

    pub(super) async fn write_conversation_file(dir: &Path, id: &str, age_secs: u64) -> PathBuf {
        let path = dir.join(format!("{id}.json"));
        tokio::fs::write(
            &path,
            serde_json::json!({
                "id": id,
                "chat_id": id,
                "title": id,
                "created_at": "2026-01-01T00:00:00Z",
                "messages": [{"role": "user", "content": "hi"}]
            })
            .to_string(),
        )
        .await
        .unwrap();
        let modified = SystemTime::now() - std::time::Duration::from_secs(age_secs);
        let file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        file.set_modified(modified).unwrap();
        path
    }

    pub(super) fn run_git(cwd: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    pub(super) fn init_repo(root: &Path) {
        run_git(root, &["init"]);
        run_git(root, &["checkout", "-b", "main"]);
        run_git(root, &["config", "core.autocrlf", "false"]);
        run_git(root, &["config", "user.email", "test@example.com"]);
        run_git(root, &["config", "user.name", "Test User"]);
        std::fs::write(root.join("file.txt"), "hello\n").unwrap();
        run_git(root, &["add", "."]);
        run_git(root, &["commit", "-m", "initial"]);
    }

    pub(super) async fn make_app_with_workspace(root: &Path) -> (Arc<GlobalContext>, AppState) {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let app = AppState::from_gcx(gcx.clone()).await;
        *app.workspace
            .documents_state
            .workspace_folders
            .lock()
            .unwrap() = vec![normalized_test_path(root)];
        (gcx, app)
    }

    pub(super) fn sample_trajectory(id: &str, title: &str, updated_at: &str) -> serde_json::Value {
        json!({
            "id": id,
            "title": title,
            "model": "model",
            "mode": "agent",
            "tool_use": "agent",
            "messages": [],
            "created_at": "2024-01-01T00:00:00Z",
            "updated_at": updated_at,
            "include_project_info": true,
            "checkpoints_enabled": true
        })
    }

    pub(super) async fn write_trajectory_file(path: &Path, id: &str, title: &str, updated_at: &str) {
        tokio::fs::create_dir_all(path.parent().unwrap())
            .await
            .unwrap();
        tokio::fs::write(
            path,
            serde_json::to_string(&sample_trajectory(id, title, updated_at)).unwrap(),
        )
        .await
        .unwrap();
    }

    pub(super) async fn write_trajectory_file_with_user_message(
        path: &Path,
        id: &str,
        title: &str,
        message: &str,
    ) {
        let mut trajectory = sample_trajectory(id, title, "2024-01-01T00:00:01Z");
        trajectory["messages"] = json!([{ "role": "user", "content": message }]);
        tokio::fs::create_dir_all(path.parent().unwrap())
            .await
            .unwrap();
        tokio::fs::write(path, serde_json::to_string(&trajectory).unwrap())
            .await
            .unwrap();
    }

    pub(super) async fn write_trajectory_file_with_metadata(
        path: &Path,
        id: &str,
        title: &str,
        updated_at: &str,
        message: &str,
    ) {
        let mut trajectory = sample_trajectory(id, title, updated_at);
        trajectory["model"] = json!("fallback-model");
        trajectory["mode"] = json!("task_planner");
        trajectory["isTitleGenerated"] = json!(true);
        trajectory["parent_id"] = json!("parent-fallback");
        trajectory["link_type"] = json!("handoff");
        trajectory["root_chat_id"] = json!("root-fallback");
        trajectory["messages"] = json!([{ "role": "user", "content": message }]);
        tokio::fs::create_dir_all(path.parent().unwrap())
            .await
            .unwrap();
        tokio::fs::write(path, serde_json::to_string(&trajectory).unwrap())
            .await
            .unwrap();
    }

    pub(super) async fn write_schema_incomplete_trajectory_file(path: &Path, id: &str) {
        tokio::fs::create_dir_all(path.parent().unwrap())
            .await
            .unwrap();
        tokio::fs::write(path, serde_json::to_string(&json!({"id": id})).unwrap())
            .await
            .unwrap();
    }

    pub(super) async fn write_buddy_conversation_file(path: &Path, id: &str, title: &str) {
        tokio::fs::create_dir_all(path.parent().unwrap())
            .await
            .unwrap();
        tokio::fs::write(
            path,
            serde_json::to_string(&json!({
                "id": id,
                "chat_id": id,
                "title": title,
                "model": "model",
                "mode": "buddy",
                "tool_use": "agent",
                "created_at": "2024-01-01T00:00:00Z",
                "updated_at": "2024-01-01T00:00:00Z",
                "include_project_info": true,
                "checkpoints_enabled": false,
                "messages": [{"role":"user","content":"hello buddy"}],
                "buddy_meta": {"is_buddy_chat": true, "buddy_chat_kind": "investigation"}
            }))
            .unwrap(),
        )
        .await
        .unwrap();
    }

    pub(super) fn task_meta(
        task_id: &str,
        role: &str,
        agent_id: Option<&str>,
        card_id: Option<&str>,
        planner_chat_id: Option<&str>,
    ) -> crate::chat::types::TaskMeta {
        crate::chat::types::TaskMeta {
            task_id: task_id.to_string(),
            role: role.to_string(),
            agent_id: agent_id.map(ToString::to_string),
            card_id: card_id.map(ToString::to_string),
            planner_chat_id: planner_chat_id.map(ToString::to_string),
        }
    }

    pub(super) async fn write_task_trajectory_file_with_user_message(
        path: &Path,
        id: &str,
        title: &str,
        message: &str,
        task_meta: &crate::chat::types::TaskMeta,
    ) {
        let mut trajectory = sample_trajectory(id, title, "2024-01-01T00:00:01Z");
        trajectory["messages"] = json!([{ "role": "user", "content": message }]);
        trajectory["task_meta"] = serde_json::to_value(task_meta).unwrap();
        tokio::fs::create_dir_all(path.parent().unwrap())
            .await
            .unwrap();
        tokio::fs::write(path, serde_json::to_string(&trajectory).unwrap())
            .await
            .unwrap();
    }

    pub(super) fn buddy_thread_meta() -> BuddyThreadMeta {
        BuddyThreadMeta {
            is_buddy_chat: true,
            buddy_chat_kind: "investigation".to_string(),
            workflow_id: None,
        }
    }

    pub(super) async fn wait_for_watcher_start() {
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    }

    pub(super) fn nested_trajectory_path(
        workspace: &std::path::Path,
        folder: &str,
        chat_id: &str,
    ) -> std::path::PathBuf {
        workspace
            .join(".refact")
            .join("trajectories")
            .join(folder)
            .join(format!("{chat_id}.json"))
    }

    pub(super) fn write_temp_json(dir: &tempfile::TempDir, name: &str, contents: &str) -> PathBuf {
        let path = dir.path().join(name);
        std::fs::write(&path, contents).unwrap();
        path
    }

    pub(super) fn assert_prune_throttle_is_bounded_and_isolates_directories() {
        let mut throttle = PruneThrottle::default();
        let left = PathBuf::from("/tmp/left");
        let right = PathBuf::from("/tmp/right");

        assert!(throttle.claim(&left, 1, 3600));
        assert!(throttle.claim(&right, 1, 3600));
        assert!(!throttle.claim(&left, 1, 3600));

        for index in 0..PRUNE_THROTTLE_MAX_ENTRIES {
            assert!(throttle.claim(&PathBuf::from(format!("/tmp/prune-{index}")), 1, 3600));
        }

        assert!(throttle.entries.len() <= PRUNE_THROTTLE_MAX_ENTRIES);
        assert!(throttle.claim(&right, 3601, 3600));
    }

    pub(super) fn test_snapshot(chat_id: &str, title: &str, messages: Vec<ChatMessage>) -> TrajectorySnapshot {
        TrajectorySnapshot {
            goal: None,
            goal_ledger: Vec::new(),
            pending_deliveries: Vec::new(),
            goal_verification_blocked_until_ms: None,
            chat_id: chat_id.to_string(),
            title: title.to_string(),
            model: "model".to_string(),
            mode: "agent".to_string(),
            tool_use: "agent".to_string(),
            messages,
            created_at: "2024-01-01T00:00:00Z".to_string(),
            boost_reasoning: false,
            checkpoints_enabled: true,
            context_tokens_cap: None,
            auto_compression_cap: None,
            auto_compression_cap_pending: false,
            include_project_info: true,
            is_title_generated: true,
            auto_approve_editing_tools: false,
            auto_approve_dangerous_commands: false,
            autonomous_no_confirm: false,
            version: 1,
            task_meta: None,
            worktree: None,
            parent_id: None,
            link_type: None,
            root_chat_id: None,
            reasoning_effort: None,
            thinking_budget: None,
            temperature: None,
            frequency_penalty: None,
            max_tokens: None,
            parallel_tool_calls: None,
            previous_response_id: None,
            active_skill: None,
            auto_enrichment_enabled: None,
            buddy_meta: None,
            auto_compact_enabled: None,
            frozen_request_prefix: None,
            claude_code_identity: None,
            reactive_compact_attempts: None,
            wake_up_at: None,
            waiting_for_card_ids: Vec::new(),
        }
    }

    pub(super) async fn wait_for_trajectory_event(
        rx: &mut broadcast::Receiver<TrajectoryEvent>,
        id: &str,
    ) -> TrajectoryEvent {
        // Generous timeout: file-watcher notify events can be delayed under
        // heavy parallel test load (notify backend + tokio scheduler contention),
        // especially on macOS CI where FSEvents delivery can lag under cargo test load.
        tokio::time::timeout(std::time::Duration::from_secs(60), async {
            loop {
                match rx.recv().await {
                    Ok(event) if event.id == id => return event,
                    Ok(_) => continue,
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(err) => panic!("trajectory event channel closed: {}", err),
                }
            }
        })
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for trajectory event {id}"))
    }

    pub(super) async fn drain_trajectory_events(rx: &mut broadcast::Receiver<TrajectoryEvent>) {
        loop {
            match tokio::time::timeout(std::time::Duration::from_millis(50), rx.recv()).await {
                Ok(Ok(_)) | Ok(Err(broadcast::error::RecvError::Lagged(_))) => continue,
                Ok(Err(broadcast::error::RecvError::Closed)) | Err(_) => break,
            }
        }
    }

    pub(super) async fn assert_no_trajectory_event_for(
        rx: &mut broadcast::Receiver<TrajectoryEvent>,
        duration: std::time::Duration,
    ) {
        match tokio::time::timeout(duration, rx.recv()).await {
            Err(_) | Ok(Err(broadcast::error::RecvError::Closed)) => {}
            Ok(Ok(event)) => panic!("unexpected trajectory event: {:?}", event),
            Ok(Err(broadcast::error::RecvError::Lagged(skipped))) => {
                panic!("unexpected trajectory event lag, skipped {skipped}")
            }
        }
    }

    pub(super) async fn assert_no_chat_event_for(
        rx: &mut broadcast::Receiver<Arc<String>>,
        duration: std::time::Duration,
    ) {
        match tokio::time::timeout(duration, rx.recv()).await {
            Err(_) | Ok(Err(broadcast::error::RecvError::Closed)) => {}
            Ok(Ok(event)) => panic!("unexpected chat event: {event}"),
            Ok(Err(broadcast::error::RecvError::Lagged(skipped))) => {
                panic!("unexpected chat event lag, skipped {skipped}")
            }
        }
    }

    pub(super) async fn wait_for_file_title(path: &Path, expected_title: &str) {
        let mut last_title = None;
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if let Ok(content) = tokio::fs::read_to_string(path).await {
                    if let Ok(json) = serde_json::from_str::<serde_json::Value>(&content) {
                        last_title = json
                            .get("title")
                            .and_then(|value| value.as_str())
                            .map(ToString::to_string);
                        let generated = json
                            .get("isTitleGenerated")
                            .and_then(|value| value.as_bool())
                            .unwrap_or(false);
                        if last_title.as_deref() == Some(expected_title) && generated {
                            return;
                        }
                    }
                }
                tokio::time::sleep(std::time::Duration::from_millis(25)).await;
            }
        })
        .await
        .unwrap_or_else(|_| {
            panic!(
                "timed out waiting for title {expected_title:?}, last title was {:?}",
                last_title
            )
        });
    }

    pub(super) async fn wait_for_session_title(session_arc: &Arc<AMutex<ChatSession>>, expected_title: &str) {
        let mut last_title = None;
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                {
                    let session = session_arc.lock().await;
                    last_title = Some(session.thread.title.clone());
                    if session.thread.title == expected_title && session.thread.is_title_generated {
                        return;
                    }
                }
                tokio::time::sleep(std::time::Duration::from_millis(25)).await;
            }
        })
        .await
        .unwrap_or_else(|_| {
            panic!(
                "timed out waiting for session title {expected_title:?}, last title was {:?}",
                last_title
            )
        });
    }

    pub(super) fn trajectory_worktree_sample() -> WorktreeMeta {
        trajectory_worktree_sample_with_id("wt-1")
    }

    pub(super) fn trajectory_worktree_sample_with_id(id: &str) -> WorktreeMeta {
        WorktreeMeta {
            id: id.to_string(),
            kind: "task_agent".to_string(),
            root: std::path::PathBuf::from("/tmp/refact-wt"),
            source_workspace_root: std::path::PathBuf::from("/tmp/refact-src"),
            repo_root: std::path::PathBuf::from("/tmp/refact-src"),
            branch: Some("refact/task/card".to_string()),
            base_branch: Some("main".to_string()),
            base_commit: Some("abc123".to_string()),
            task_id: Some("task-1".to_string()),
            card_id: Some("card-1".to_string()),
            agent_id: Some("agent-1".to_string()),
            enforce: true,
        }
    }

    pub(super) fn mark_active_compression(session: &mut ChatSession, phase: CompressionPhase) {
        session.is_compressing = true;
        session.runtime.is_compressing = true;
        session.compression_phase = Some(phase);
        session.runtime.compression_phase = Some(phase);
        session.compression_reason = Some(CompressionReason::PressureLow);
        session.runtime.compression_reason = Some(CompressionReason::PressureLow);
        session.compression_attempt_generation = 9;
        session.active_compression_attempt = Some(9);
    }

    pub(super) async fn assert_external_remove_clears_active_compression(phase: CompressionPhase) {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, app) = make_app_with_workspace(dir.path()).await;
        let chat_id = format!("external-remove-{:?}", phase).to_lowercase();
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.clone())));
        let mut chat_rx = {
            let mut session = session_arc.lock().await;
            session.messages.push(ChatMessage::new(
                "user".to_string(),
                "before delete".to_string(),
            ));
            mark_active_compression(&mut session, phase);
            session.trajectory_dirty = false;
            session.subscribe()
        };
        app.chat
            .sessions
            .write()
            .await
            .insert(chat_id.clone(), session_arc.clone());

        process_trajectory_change(gcx, &chat_id, true).await;

        let session = session_arc.lock().await;
        assert!(session.messages.is_empty());
        assert!(!session.is_compressing);
        assert!(!session.runtime.is_compressing);
        assert_eq!(session.compression_phase, None);
        assert_eq!(session.runtime.compression_phase, None);
        assert_eq!(session.compression_reason, None);
        assert_eq!(session.runtime.compression_reason, None);
        assert_eq!(session.active_compression_attempt, None);
        drop(session);

        let json = chat_rx.recv().await.unwrap();
        let envelope: EventEnvelope = serde_json::from_str(&json).unwrap();
        match envelope.event {
            ChatEvent::Snapshot {
                runtime, messages, ..
            } => {
                assert!(messages.is_empty());
                assert!(!runtime.is_compressing);
                assert_eq!(runtime.compression_phase, None);
                assert_eq!(runtime.compression_reason, None);
            }
            other => panic!("expected Snapshot, got {other:?}"),
        }
    }

