use super::*;

impl ChatSession {
    pub(crate) fn runtime_update_event(
        &self,
        state: SessionState,
        error: Option<String>,
        is_compressing: bool,
        compression_phase: Option<CompressionPhase>,
        compression_reason: Option<CompressionReason>,
    ) -> ChatEvent {
        ChatEvent::RuntimeUpdated {
            waiting_interruptible: self.runtime.waiting_interruptible,
            goal_active: self.goal_active,
            goal_status: self.goal_status,
            goal_turns_used: self.goal_turns_used,
            goal_tokens_used: self.goal_tokens_used,
            goal_no_progress_turns: self.goal_no_progress_turns,
            state,
            error,
            is_compressing,
            compression_phase,
            compression_reason,
        }
    }

    pub(crate) fn emit_goal_status(&mut self) {
        self.refresh_goal_runtime_mirror();
        let event = self.runtime_update_event(
            self.runtime.state,
            self.runtime.error.clone(),
            self.is_compressing,
            self.compression_phase,
            self.compression_reason,
        );
        self.emit(event);
    }

    /// Publish the automatic-compaction gate's decision inputs as read-only
    /// diagnostics. `estimated` is `None` when the local estimate was
    /// unavailable, so an unavailable estimate never reads as a huge context.
    pub(crate) fn set_compression_gate_diagnostics(
        &mut self,
        estimated: Option<usize>,
        cap: usize,
        cap_source: &str,
    ) {
        self.compression_estimated_tokens = estimated;
        self.compression_effective_cap = Some(cap);
        self.compression_cap_source = Some(cap_source.to_string());
        self.runtime.compression_estimated_tokens = estimated;
        self.runtime.compression_effective_cap = Some(cap);
        self.runtime.compression_cap_source = Some(cap_source.to_string());
    }

    pub(crate) fn clear_compression_gate_diagnostics(&mut self) {
        self.compression_estimated_tokens = None;
        self.compression_effective_cap = None;
        self.compression_cap_source = None;
        self.runtime.compression_estimated_tokens = None;
        self.runtime.compression_effective_cap = None;
        self.runtime.compression_cap_source = None;
    }

    pub fn reset_compaction_runtime_state(&mut self) {
        self.clear_stream_and_confirmation_timestamps();
        self.release_turn_only_state();
        self.pending_max_new_tokens_boost = None;
        self.thread.reactive_compact_attempts = None;
        self.thread.previous_response_id = None;
        self.reset_cache_guard_snapshot();
        self.provider_usage_stale = true;
        self.is_compressing = false;
        self.runtime.is_compressing = false;
        self.compression_phase = None;
        self.runtime.compression_phase = None;
        self.compression_reason = None;
        self.runtime.compression_reason = None;
        self.clear_compression_gate_diagnostics();
        if let Some(abort_flag) = self.compression_abort_flag.take() {
            abort_flag.store(true, Ordering::SeqCst);
        }
        self.active_compression_attempt = None;
        self.compression_attempt_started_at_ms = None;
        self.refresh_goal_runtime_mirror();
    }

    pub fn replace_messages(&mut self, messages: Vec<ChatMessage>) {
        self.messages = messages;
        self.rebuild_goal_projection_from_messages();
        self.reset_compaction_runtime_state();
        self.increment_version();
        self.touch();
    }

    pub fn set_active_skill(&mut self, name: String) {
        self.thread.active_skill = Some(name);
        self.increment_version();
    }

    pub fn clear_active_skill(&mut self) {
        self.thread.active_skill = None;
        self.increment_version();
    }

    pub fn touch(&mut self) {
        self.last_activity = Instant::now();
    }

    pub fn reset_cache_guard_snapshot(&mut self) {
        self.cache_guard_reset_generation = self.cache_guard_reset_generation.saturating_add(1);
        self.cache_guard_force_next = true;
    }

    pub fn mark_tool_started(&mut self) {
        let now = Instant::now();
        self.last_tool_started_at = Some(now);
        self.last_tool_progress_at = None;
        self.last_activity = now;
    }

    pub fn stamp_tool_call_timing(
        &mut self,
        tool_call_id: &str,
        started_at_ms: Option<u64>,
        completed_at_ms: Option<u64>,
    ) -> bool {
        let Some(start) = self.mutable_message_start() else {
            return false;
        };
        let mut updated_message = None;
        for message in self.messages[start..].iter_mut().rev() {
            let Some(tool_call) = message
                .tool_calls
                .as_mut()
                .and_then(|tool_calls| tool_calls.iter_mut().find(|call| call.id == tool_call_id))
            else {
                continue;
            };
            let mut changed = false;
            if tool_call.started_at_ms.is_none() && started_at_ms.is_some() {
                tool_call.started_at_ms = started_at_ms;
                changed = true;
            }
            if tool_call.completed_at_ms.is_none() && completed_at_ms.is_some() {
                tool_call.completed_at_ms = completed_at_ms;
                changed = true;
            }
            if changed {
                updated_message = Some(message.clone());
            }
            break;
        }
        let Some(message) = updated_message else {
            return false;
        };
        self.increment_version();
        self.touch();
        self.emit(ChatEvent::MessageUpdated {
            message_id: message.message_id.clone(),
            message,
        });
        true
    }

    pub fn mark_tool_progress(&mut self) {
        let now = Instant::now();
        self.last_tool_progress_at = Some(now);
        self.last_activity = now;
    }

    pub(super) fn mark_stream_delta(&mut self) {
        let now = Instant::now();
        self.last_stream_delta_at = Some(now);
        self.last_activity = now;
    }

    pub(crate) fn mark_persisted_runtime_changed(&mut self) {
        self.increment_version();
        self.touch();
    }

    pub fn is_pending_wake_up(&self) -> bool {
        self.runtime.state == SessionState::WaitingUserInput
            && self
                .wake_up_at
                .as_ref()
                .is_some_and(|deadline| *deadline > chrono::Utc::now())
    }

    pub fn is_idle(&self) -> bool {
        matches!(
            self.runtime.state,
            SessionState::Idle | SessionState::WaitingUserInput
        )
    }

    pub fn is_idle_for_cleanup(&self) -> bool {
        let is_idle_like = matches!(
            self.runtime.state,
            SessionState::Idle | SessionState::Completed | SessionState::WaitingUserInput
        );
        is_idle_like
            && !self.is_pending_wake_up()
            && self.command_queue.is_empty()
            && self.last_activity.elapsed() > session_idle_timeout()
    }

    pub fn is_runner_owned_subagent_view(&self) -> bool {
        matches!(
            self.runtime.state,
            SessionState::Starting | SessionState::Generating | SessionState::ExecutingTools
        )
            && self.thread.parent_id.is_some()
            && self.thread.link_type.as_deref().is_some_and(|link_type| {
                !matches!(link_type, "handoff" | "mode_transition" | "branch")
            })
    }

    pub(crate) fn mirror_runner_messages(&mut self, mut messages: Vec<ChatMessage>) {
        for message in &mut messages {
            stamp_created_at_ms(message);
        }
        self.messages = messages;
        self.rebuild_goal_projection_from_messages();
        self.touch();
        let snapshot = self.snapshot();
        self.emit(snapshot);
    }

    pub fn close_event_channel(&mut self) {
        self.pending_context_rebuild = None;
        self.pending_mode_handoff = None;
        if let Some(flag) = &self.compression_abort_flag {
            flag.store(true, Ordering::SeqCst);
        }
        self.clear_discarded_queue_timestamps();
        self.clear_stream_and_confirmation_timestamps();
        self.release_turn_only_state();
        self.closed = true;
        self.closed_flag.store(true, Ordering::Relaxed);
        for handle in self.post_turn_task_handles.drain(..) {
            handle.abort();
        }
        let (new_tx, _) = broadcast::channel(limits().event_channel_capacity);
        self.event_tx = new_tx;
        self.queue_notify.notify_waiters();
    }

    pub fn track_post_turn_task(&mut self, handle: tokio::task::JoinHandle<()>) {
        self.post_turn_task_handles
            .retain(|task| !task.is_finished());
        self.post_turn_task_handles.push(handle);
    }

    /// Turn-scoped memory release: drops everything a completed/aborted turn owns.
    /// The per-turn investigation tracker resets here so every new turn starts with
    /// a fresh budget and an empty stagnation window.
    pub(crate) fn release_turn_only_state(&mut self) {
        self.last_prompt_messages = Vec::new();
        self.tool_catalog = None;
        self.turn_tool_pool = None;
        self.turn_investigation_tracker.reset();
        self.post_turn_task_handles
            .retain(|task| !task.is_finished());
    }

    /// The last provider usage was measured under the previous model; it must not
    /// anchor the compression gate for the new one.
    pub(crate) fn invalidate_usage_on_model_switch(&mut self) {
        self.provider_usage_stale = true;
    }
}

impl ChatSession {
    pub fn set_runtime_state(&mut self, state: SessionState, error: Option<String>) {
        let old_state = self.runtime.state;
        let old_error = self.runtime.error.clone();
        let should_clear_terminal_stream =
            is_terminal_runtime_state(state) && self.stream_started_at.is_some();
        let should_clear_terminal_compression = is_terminal_runtime_state(state)
            && (self.is_compressing
                || self.runtime.is_compressing
                || self.active_compression_attempt.is_some()
                || is_active_compression_phase(self.compression_phase)
                || is_active_compression_phase(self.runtime.compression_phase));
        if old_state == state
            && old_error == error
            && !should_clear_terminal_stream
            && !should_clear_terminal_compression
        {
            return;
        }

        let was_paused = old_state == SessionState::Paused;
        let had_pause_reasons = !self.runtime.pause_reasons.is_empty();

        if state == SessionState::ExecutingTools {
            self.mark_tool_started();
        } else if old_state == SessionState::ExecutingTools {
            self.last_tool_started_at = None;
            self.last_tool_progress_at = None;
        }
        if state == SessionState::Generating && old_state != SessionState::Generating {
            self.last_stream_delta_at = None;
        }
        if should_clear_terminal_stream {
            self.stream_started_at = None;
        }

        self.runtime.state = state;
        self.runtime.paused = state == SessionState::Paused;
        self.runtime.error = error.clone();
        self.runtime.queued_items = self.build_queued_items();
        self.runtime.queue_size = self.runtime.queued_items.len();
        if matches!(
            state,
            SessionState::Completed | SessionState::Error | SessionState::WaitingUserInput
        ) && self.post_tool_side_effects.is_empty()
        {
            self.release_turn_only_state();
        }
        if should_clear_terminal_compression {
            if let Some(abort_flag) = self.compression_abort_flag.take() {
                abort_flag.store(true, Ordering::SeqCst);
            }
            self.is_compressing = false;
            self.runtime.is_compressing = false;
            self.active_compression_attempt = None;
            if is_active_compression_phase(self.compression_phase) {
                self.compression_phase = None;
                self.compression_reason = None;
            }
            self.clear_compression_gate_diagnostics();
        }
        self.runtime.is_compressing = self.is_compressing;
        self.runtime.compression_phase = self.compression_phase;
        self.runtime.compression_reason = self.compression_reason;
        self.refresh_goal_runtime_mirror();
        self.touch();

        if state != SessionState::Paused && (was_paused || had_pause_reasons) {
            self.complete_confirmation_wait();
            self.runtime.pause_reasons.clear();
            self.runtime.auto_approved_tool_ids.clear();
            self.runtime.accepted_tool_ids.clear();
            self.runtime.paused_message_index = None;
            self.emit(ChatEvent::PauseCleared {});
        }

        if old_state == SessionState::WaitingUserInput && state != SessionState::WaitingUserInput {
            let mut changed = false;
            if self.wake_up_at.is_some() {
                self.wake_up_at = None;
                changed = true;
            }
            if !self.waiting_for_card_ids.is_empty() {
                self.waiting_for_card_ids.clear();
                changed = true;
            }
            if changed {
                self.increment_version();
            }
        }

        let event = self.runtime_update_event(
            state,
            error.clone(),
            self.is_compressing,
            self.compression_phase,
            self.compression_reason,
        );
        self.emit(event);
        self.emit_trajectory_state_change();
        self.queue_notify.notify_waiters();
    }

    pub(super) fn emit_trajectory_state_change(&self) {
        if self.thread.task_meta.is_some() {
            return;
        }
        if let Some(ref tx) = self.trajectory_events_tx {
            let state_str = match self.runtime.state {
                SessionState::Idle => "idle",
                SessionState::Starting => "starting",
                SessionState::Generating => "generating",
                SessionState::ExecutingTools => "executing_tools",
                SessionState::Paused => "paused",
                SessionState::WaitingIde => "waiting_ide",
                SessionState::WaitingUserInput => "waiting_user_input",
                SessionState::Completed => "completed",
                SessionState::Error => "error",
            };
            let effective_root = self
                .thread
                .root_chat_id
                .clone()
                .unwrap_or_else(|| self.chat_id.clone());
            let (task_id, task_role, agent_id, card_id) =
                task_context_from_task_meta(self.thread.task_meta.as_ref());
            let event = TrajectoryEvent {
                event_type: "updated".to_string(),
                id: self.chat_id.clone(),
                updated_at: None,
                title: None,
                is_title_generated: None,
                session_state: Some(state_str.to_string()),
                error: self.runtime.error.clone(),
                message_count: Some(self.messages.len()),
                parent_id: self.thread.parent_id.clone(),
                link_type: self.thread.link_type.clone(),
                root_chat_id: Some(effective_root),
                task_id,
                task_role,
                agent_id,
                card_id,
                model: Some(self.thread.model.clone()),
                mode: Some(self.thread.mode.clone()),
                worktree: self.thread.worktree.clone(),
                total_lines_added: None,
                total_lines_removed: None,
                tasks_total: None,
                tasks_done: None,
                tasks_failed: None,
                total_prompt_tokens: None,
                total_completion_tokens: None,
                total_tokens: None,
                total_cache_read_tokens: None,
                total_cache_creation_tokens: None,
                total_cost_usd: None,
            };
            let _ = tx.send(event);
        }
    }
}

impl ChatSession {
    pub fn discard_draft_for_pause(&mut self) {
        if let Some(draft) = self.draft_message.take() {
            self.emit(ChatEvent::MessageRemoved {
                message_id: draft.message_id,
            });
        }
        self.draft_usage = None;
        self.stream_started_at = None;
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Arc<String>> {
        self.event_tx.subscribe()
    }

    pub fn set_title(&mut self, title: String, is_generated: bool) {
        self.thread.title = title.clone();
        self.thread.is_title_generated = is_generated;
        self.increment_version();
        self.touch();
        if self.thread.task_meta.is_none() {
            self.emit_trajectory_title_change(title);
        }
    }

    pub fn set_title_from_trajectory_label(&mut self, title: String) {
        self.thread.title = title.clone();
        self.thread.is_title_generated = false;
        self.increment_version();
        self.touch();
        self.emit_trajectory_title_change(title);
    }

    pub(super) fn emit_trajectory_title_change(&self, title: String) {
        if let Some(ref tx) = self.trajectory_events_tx {
            let effective_root = self
                .thread
                .root_chat_id
                .clone()
                .unwrap_or_else(|| self.chat_id.clone());
            let (task_id, task_role, agent_id, card_id) =
                task_context_from_task_meta(self.thread.task_meta.as_ref());
            let event = TrajectoryEvent {
                event_type: "updated".to_string(),
                id: self.chat_id.clone(),
                updated_at: Some(chrono::Utc::now().to_rfc3339()),
                title: Some(trajectory_meta_title(&title)),
                is_title_generated: Some(self.thread.is_title_generated),
                session_state: Some(self.runtime.state.to_string()),
                error: self.runtime.error.clone(),
                message_count: Some(self.messages.len()),
                parent_id: self.thread.parent_id.clone(),
                link_type: self.thread.link_type.clone(),
                root_chat_id: Some(effective_root),
                task_id,
                task_role,
                agent_id,
                card_id,
                model: Some(self.thread.model.clone()),
                mode: Some(self.thread.mode.clone()),
                worktree: self.thread.worktree.clone(),
                total_lines_added: None,
                total_lines_removed: None,
                tasks_total: None,
                tasks_done: None,
                tasks_failed: None,
                total_prompt_tokens: None,
                total_completion_tokens: None,
                total_tokens: None,
                total_cache_read_tokens: None,
                total_cache_creation_tokens: None,
                total_cost_usd: None,
            };
            let _ = tx.send(event);
        }
    }
}

impl ChatSession {
    pub fn validate_tool_decision(&self, tool_call_id: &str) -> bool {
        self.runtime
            .pause_reasons
            .iter()
            .any(|r| r.tool_call_id == tool_call_id)
    }

    pub(in crate::chat) fn add_tool_decision_event(
        &mut self,
        decision: &str,
        tool_call_ids: Vec<String>,
        scope: &str,
    ) {
        if tool_call_ids.is_empty() {
            return;
        }
        self.queue_post_tool_side_effect(tool_decision_message(decision, tool_call_ids, scope));
    }

    pub fn process_tool_decisions(
        &mut self,
        decisions: &[ToolDecisionItem],
    ) -> ToolDecisionOutcome {
        let mut accepted_ids = Vec::new();
        let mut denied_ids = Vec::new();

        for decision in decisions {
            if !self.validate_tool_decision(&decision.tool_call_id) {
                warn!(
                    "Tool decision for unknown tool_call_id: {}",
                    decision.tool_call_id
                );
                continue;
            }
            if decision.accepted {
                accepted_ids.push(decision.tool_call_id.clone());
            } else {
                denied_ids.push(decision.tool_call_id.clone());
            }
        }

        let before_len = self.runtime.pause_reasons.len();
        self.runtime.pause_reasons.retain(|r| {
            !accepted_ids.contains(&r.tool_call_id) && !denied_ids.contains(&r.tool_call_id)
        });
        let after_len = self.runtime.pause_reasons.len();

        if before_len > after_len && after_len == 0 {
            self.complete_confirmation_wait();
        }

        for denied_id in &denied_ids {
            let has_matching_tool_call = self
                .messages
                .iter()
                .rev()
                .find(|m| m.role == "assistant")
                .and_then(|m| m.tool_calls.as_ref())
                .map_or(false, |tcs| tcs.iter().any(|tc| &tc.id == denied_id));
            if !has_matching_tool_call {
                warn!(
                    "Denied tool_call_id {} not found in last assistant message, skipping synthesis",
                    denied_id
                );
                continue;
            }
            self.add_message(ChatMessage {
                role: "tool".to_string(),
                content: ChatContent::SimpleText("Tool call denied by user.".to_string()),
                tool_call_id: denied_id.clone(),
                ..Default::default()
            });
        }

        self.add_tool_decision_event("approve", accepted_ids.clone(), "once");
        self.add_tool_decision_event("reject", denied_ids.clone(), "once");
        self.drain_post_tool_side_effects();

        if before_len != after_len {
            self.touch();
            if self.runtime.pause_reasons.is_empty() {
                // The caller knows whether a cleared pause will continue into tool execution,
                // follow-up generation, or true idle. Keep the paused runtime state intact here
                // so the caller can publish exactly one non-idle/idle transition.
            } else {
                self.emit(ChatEvent::PauseRequired {
                    reasons: self.runtime.pause_reasons.clone(),
                });
                let event = self.runtime_update_event(
                    self.runtime.state,
                    self.runtime.error.clone(),
                    self.is_compressing,
                    self.compression_phase,
                    self.compression_reason,
                );
                self.emit(event);
            }
        }

        ToolDecisionOutcome {
            accepted_ids,
            denied_ids,
        }
    }

    pub(crate) fn complete_confirmation_wait(&mut self) {
        self.complete_confirmation_wait_at(Instant::now());
    }

    pub(crate) fn complete_confirmation_wait_at(&mut self, now: Instant) {
        if !perf_diagnostics::is_enabled() {
            return;
        }
        let Some((paused_at, tool_call_ids)) = self.confirmation_paused_at.take() else {
            return;
        };
        perf_diagnostics::record(
            PerfComponent::ToolConfirmationWait,
            Some(&self.chat_id),
            PerfOutcome::Success,
            now.duration_since(paused_at)
                .as_micros()
                .try_into()
                .unwrap_or(u64::MAX),
            None,
            Some(tool_call_ids.len() as u64),
            None,
        );
    }
}
