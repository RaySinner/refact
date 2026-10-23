use super::*;

impl ChatSession {
    pub fn add_background_process_cleanup_notice(&mut self, killed_count: usize) {
        self.add_message(background_process_cleanup_notice(killed_count));
    }
}

impl ChatSession {
    pub fn new(chat_id: String) -> Self {
        let (event_tx, _) = broadcast::channel(limits().event_channel_capacity);
        Self {
            chat_id: chat_id.clone(),
            derived_privacy_zones: crate::privacy::records::new_derived_privacy_zones(),
            tool_loop_guard: crate::tools::tool_call_loop_guard::ToolCallLoopGuard::new(),
            turn_investigation_tracker:
                crate::tools::tool_call_loop_guard::TurnInvestigationTracker::new(),
            thread: ThreadParams {
                id: chat_id,
                ..Default::default()
            },
            messages: Vec::new(),
            runtime: RuntimeState::default(),
            goal: None,
            goal_active: false,
            goal_status: None,
            goal_turns_used: 0,
            goal_tokens_used: 0,
            goal_no_progress_turns: 0,
            is_compressing: false,
            compression_phase: None,
            compression_reason: None,
            compression_estimated_tokens: None,
            compression_effective_cap: None,
            compression_cap_source: None,
            pending_context_rebuild: None,
            pending_mode_handoff: None,
            last_rebuild_attempt_version: None,
            compression_attempt_generation: 0,
            active_compression_attempt: None,
            compression_attempt_started_at_ms: None,
            compression_abort_flag: None,
            draft_message: None,
            draft_usage: None,
            command_queue: VecDeque::new(),
            event_seq: 0,
            event_tx,
            trajectory_events_tx: None,
            recent_request_ids: VecDeque::with_capacity(limits().recent_request_ids_capacity),
            recent_request_ids_set: HashSet::new(),
            abort_flag: Arc::new(AtomicBool::new(false)),
            abort_notify: Arc::new(Notify::new()),
            interruptible_waits: 0,
            wait_delivery_boundary: false,
            wait_interrupt_flag: Arc::new(AtomicBool::new(false)),
            user_interrupt_flag: Arc::new(AtomicBool::new(false)),
            queue_processor_running: Arc::new(AtomicBool::new(false)),
            queue_notify: Arc::new(Notify::new()),
            queue_processor_counters: Arc::new(QueueProcessorCounters::default()),
            last_activity: Instant::now(),
            last_stream_delta_at: None,
            command_enqueued_at: HashMap::new(),
            stream_started_at: None,
            confirmation_paused_at: None,
            last_tool_started_at: None,
            last_tool_progress_at: None,
            trajectory_dirty: false,
            trajectory_version: 0,
            trajectory_committed_version: 0,
            trajectory_save_in_flight: false,
            trajectory_save_queued: false,
            trajectory_save_mutex: Arc::new(AMutex::new(())),
            trajectory_commit_notify: Arc::new(Notify::new()),
            trajectory_save_error: None,
            created_at: chrono::Utc::now().to_rfc3339(),
            closed: false,
            closed_flag: Arc::new(AtomicBool::new(false)),
            external_reload_pending: None,
            last_prompt_messages: Vec::new(),
            tool_catalog: None,
            turn_tool_pool: None,
            pending_max_new_tokens_boost: None,
            cache_guard_snapshot: None,
            cache_guard_request_generation: 0,
            cache_guard_snapshot_generation: 0,
            cache_guard_reset_generation: 0,
            cache_guard_force_next: false,
            provider_usage_stale: false,
            task_agent_error: None,
            pending_browser_message: None,
            post_tool_side_effects: VecDeque::new(),
            pending_deliveries: VecDeque::new(),
            runner_pending_deliveries: Vec::new(),
            delivered_delivery_ids: HashSet::new(),
            turn_depth: 0,
            delivery_wake_sources: Default::default(),
            active_command: ActiveCommandContext::default(),
            skills_available_count: 0,
            skills_included: Vec::new(),
            pending_skill_deactivation: None,
            post_turn_task_handles: Vec::new(),
            openai_codex_websocket: Default::default(),
            suppress_auto_enrichment_for_next_turn: false,
            enrichment_identities: HashSet::new(),
            wake_up_at: None,
            waiting_for_card_ids: Vec::new(),
            background_agents: HashMap::new(),
            goal_stopped_by_abort: false,
            goal_ledger: Vec::new(),
            goal_turn_evidence: false,
            goal_verification_blocked_until_ms: None,
        }
    }

    pub fn new_with_trajectory(
        chat_id: String,
        messages: Vec<ChatMessage>,
        mut thread: ThreadParams,
        created_at: String,
        wake_up_at: Option<chrono::DateTime<chrono::Utc>>,
        waiting_for_card_ids: Vec<String>,
        goal: Option<GoalSnapshot>,
    ) -> Self {
        // active_skill is runtime state тАФ if the server restarted mid-skill, the compaction
        // anchor (started_at_index) is lost. Clear it so the session starts cleanly rather
        // than leaving the user locked into a ghost skill that can never be deactivated.
        thread.active_skill = None;
        let goal = goal_snapshot_from_messages(&messages, goal.as_ref()).or(goal);
        let (goal_active, goal_status, goal_turns_used, goal_tokens_used, goal_no_progress_turns) =
            goal_runtime_projection(goal.as_ref());
        let mut runtime = RuntimeState::default();
        runtime.goal_active = goal_active;
        runtime.goal_status = goal_status;
        runtime.goal_turns_used = goal_turns_used;
        runtime.goal_tokens_used = goal_tokens_used;
        runtime.goal_no_progress_turns = goal_no_progress_turns;
        let (event_tx, _) = broadcast::channel(limits().event_channel_capacity);
        let enrichment_identities = messages
            .iter()
            .filter_map(|message| {
                message
                    .extra
                    .get("knowledge_enrichment")
                    .and_then(|value| value.get("identity"))
                    .and_then(|value| value.as_str())
                    .map(str::to_string)
            })
            .collect();
        Self {
            chat_id,
            derived_privacy_zones: crate::privacy::records::new_derived_privacy_zones(),
            tool_loop_guard: crate::tools::tool_call_loop_guard::ToolCallLoopGuard::new(),
            turn_investigation_tracker:
                crate::tools::tool_call_loop_guard::TurnInvestigationTracker::new(),
            thread,
            messages,
            runtime,
            goal,
            goal_active,
            goal_status,
            goal_turns_used,
            goal_tokens_used,
            goal_no_progress_turns,
            is_compressing: false,
            compression_phase: None,
            compression_reason: None,
            compression_estimated_tokens: None,
            compression_effective_cap: None,
            compression_cap_source: None,
            pending_context_rebuild: None,
            pending_mode_handoff: None,
            last_rebuild_attempt_version: None,
            compression_attempt_generation: 0,
            active_compression_attempt: None,
            compression_attempt_started_at_ms: None,
            compression_abort_flag: None,
            draft_message: None,
            draft_usage: None,
            command_queue: VecDeque::new(),
            event_seq: 0,
            event_tx,
            trajectory_events_tx: None,
            recent_request_ids: VecDeque::with_capacity(limits().recent_request_ids_capacity),
            recent_request_ids_set: HashSet::new(),
            abort_flag: Arc::new(AtomicBool::new(false)),
            abort_notify: Arc::new(Notify::new()),
            interruptible_waits: 0,
            wait_delivery_boundary: false,
            wait_interrupt_flag: Arc::new(AtomicBool::new(false)),
            user_interrupt_flag: Arc::new(AtomicBool::new(false)),
            queue_processor_running: Arc::new(AtomicBool::new(false)),
            queue_notify: Arc::new(Notify::new()),
            queue_processor_counters: Arc::new(QueueProcessorCounters::default()),
            last_activity: Instant::now(),
            last_stream_delta_at: None,
            command_enqueued_at: HashMap::new(),
            stream_started_at: None,
            confirmation_paused_at: None,
            last_tool_started_at: None,
            last_tool_progress_at: None,
            external_reload_pending: None,
            trajectory_dirty: false,
            trajectory_version: 0,
            trajectory_committed_version: 0,
            trajectory_save_in_flight: false,
            trajectory_save_queued: false,
            trajectory_save_mutex: Arc::new(AMutex::new(())),
            trajectory_commit_notify: Arc::new(Notify::new()),
            trajectory_save_error: None,
            created_at,
            closed: false,
            closed_flag: Arc::new(AtomicBool::new(false)),
            last_prompt_messages: Vec::new(),
            tool_catalog: None,
            turn_tool_pool: None,
            pending_max_new_tokens_boost: None,
            cache_guard_snapshot: None,
            cache_guard_request_generation: 0,
            cache_guard_snapshot_generation: 0,
            cache_guard_reset_generation: 0,
            cache_guard_force_next: false,
            provider_usage_stale: false,
            task_agent_error: None,
            pending_browser_message: None,
            post_tool_side_effects: VecDeque::new(),
            pending_deliveries: VecDeque::new(),
            runner_pending_deliveries: Vec::new(),
            delivered_delivery_ids: HashSet::new(),
            turn_depth: 0,
            delivery_wake_sources: Default::default(),
            active_command: ActiveCommandContext::default(),
            skills_available_count: 0,
            skills_included: Vec::new(),
            pending_skill_deactivation: None,
            post_turn_task_handles: Vec::new(),
            openai_codex_websocket: Default::default(),
            suppress_auto_enrichment_for_next_turn: false,
            enrichment_identities,
            wake_up_at,
            waiting_for_card_ids,
            background_agents: HashMap::new(),
            goal_stopped_by_abort: false,
            goal_ledger: Vec::new(),
            goal_turn_evidence: false,
            goal_verification_blocked_until_ms: None,
        }
    }

    pub fn increment_version(&mut self) {
        self.trajectory_version += 1;
        self.trajectory_dirty = true;
        if self.trajectory_save_in_flight {
            self.trajectory_save_queued = true;
        }
    }

    pub(crate) fn has_enrichment_identity(&self, identity: &str) -> bool {
        self.enrichment_identities.contains(identity)
    }

    pub(crate) fn record_enrichment_identity(&mut self, identity: String) {
        self.enrichment_identities.insert(identity);
    }

    pub(crate) fn trajectory_commit_can_write(&self, version: u64) -> bool {
        version >= self.trajectory_committed_version
    }

    pub(crate) fn complete_trajectory_commit(&mut self, version: u64) -> bool {
        if !self.trajectory_commit_can_write(version) {
            return false;
        }
        self.trajectory_committed_version = version;
        self.trajectory_dirty = self.trajectory_version != version;
        true
    }

}
