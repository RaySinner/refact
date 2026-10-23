use super::*;

impl ChatSession {
    pub fn is_duplicate_request(&mut self, request_id: &str) -> bool {
        if self.recent_request_ids_set.contains(request_id) {
            return true;
        }
        self.remember_request_id(request_id);
        false
    }

    pub fn has_seen_request(&self, request_id: &str) -> bool {
        self.recent_request_ids_set.contains(request_id)
    }

    pub fn remember_accepted_request(&mut self, request_id: &str) {
        if !self.has_seen_request(request_id) {
            self.remember_request_id(request_id);
        }
    }

    pub(super) fn remember_request_id(&mut self, request_id: &str) {
        if self.recent_request_ids.len() >= limits().recent_request_ids_capacity {
            if let Some(evicted) = self.recent_request_ids.pop_front() {
                self.recent_request_ids_set.remove(&evicted);
            }
        }
        self.recent_request_ids.push_back(request_id.to_string());
        self.recent_request_ids_set.insert(request_id.to_string());
    }

    pub(super) fn command_is_critical_for_queue(&self, command: &ChatCommand) -> bool {
        matches!(command, ChatCommand::Abort {})
            || (self.runtime.state == SessionState::Paused
                && matches!(
                    command,
                    ChatCommand::ToolDecision { .. } | ChatCommand::ToolDecisions { .. }
                ))
            || (self.runtime.state == SessionState::WaitingIde
                && matches!(command, ChatCommand::IdeToolResult { .. }))
    }

    pub(super) fn command_interrupts_active_loop(command: &ChatCommand) -> bool {
        matches!(
            command,
            ChatCommand::UserMessage { .. }
                | ChatCommand::RetryFromIndex { .. }
                | ChatCommand::Regenerate {}
                | ChatCommand::UpdateMessage {
                    regenerate: true,
                    ..
                }
                | ChatCommand::RemoveMessage {
                    regenerate: true,
                    ..
                }
                | ChatCommand::Abort {}
        )
    }

    pub fn enqueue_command(&mut self, request: CommandRequest) -> EnqueueCommandOutcome {
        if self
            .recent_request_ids_set
            .contains(&request.client_request_id)
        {
            return EnqueueCommandOutcome::Duplicate;
        }
        let request_id = request.client_request_id.clone();
        let outcome = self.enqueue_accepted_command(request);
        if outcome == EnqueueCommandOutcome::Accepted {
            self.remember_request_id(&request_id);
        }
        outcome
    }

    pub fn enqueue_accepted_command(&mut self, request: CommandRequest) -> EnqueueCommandOutcome {
        if self.command_queue.len() >= max_queue_size()
            && !self.command_is_critical_for_queue(&request.command)
        {
            return EnqueueCommandOutcome::Full;
        }

        if request.priority {
            self.enqueue_accepted_priority_command(request)
        } else {
            if perf_diagnostics::is_enabled() {
                self.command_enqueued_at
                    .insert(request.client_request_id.clone(), Instant::now());
            }
            self.command_queue.push_back(request);
            self.touch();
            self.emit_queue_update();
            self.queue_notify.notify_one();
            EnqueueCommandOutcome::Accepted
        }
    }
}

impl ChatSession {
    pub(crate) fn record_command_queue_wait(&mut self, client_request_id: &str) {
        self.record_command_queue_wait_at(client_request_id, Instant::now());
    }

    pub(crate) fn record_command_queue_wait_at(&mut self, client_request_id: &str, now: Instant) {
        let Some(enqueued_at) = self.command_enqueued_at.remove(client_request_id) else {
            return;
        };
        if !perf_diagnostics::is_enabled() {
            return;
        }
        perf_diagnostics::record(
            PerfComponent::CommandQueueWait,
            Some(&self.chat_id),
            PerfOutcome::Success,
            now.duration_since(enqueued_at)
                .as_micros()
                .try_into()
                .unwrap_or(u64::MAX),
            None,
            None,
            Some(self.command_queue.len() as u64),
        );
    }

    pub(crate) fn clear_stream_and_confirmation_timestamps(&mut self) {
        self.stream_started_at = None;
        self.confirmation_paused_at = None;
    }

    pub(crate) fn clear_discarded_queue_timestamps(&mut self) {
        self.command_enqueued_at.clear();
    }

    pub(crate) fn clear_queue_timestamp(&mut self, client_request_id: &str) {
        self.command_enqueued_at.remove(client_request_id);
    }
}

impl ChatSession {
    pub fn drain_post_tool_side_effects(&mut self) {
        if self.draft_message.is_some() || self.has_pending_tool_result_window() {
            return;
        }
        let side_effects = std::mem::take(&mut self.post_tool_side_effects);
        for message in side_effects {
            self.add_message(message);
        }
    }

    pub fn clear_post_tool_side_effects(&mut self) {
        self.post_tool_side_effects.clear();
        self.touch();
    }

    /// True when the whole turn (assistant plus the multi-step tool loop) has
    /// ended. `SessionState::Idle` alone is not enough: `tools.rs` briefly
    /// returns to `Idle` between steps of the same turn.
    pub fn turn_finished(&self) -> bool {
        self.turn_depth == 0
            && matches!(
                self.runtime.state,
                SessionState::Idle
                    | SessionState::Completed
                    | SessionState::Error
                    | SessionState::WaitingUserInput
            )
    }

    /// Wait cancellation is separate from turn cancellation: real work in the same
    /// tool window must finish, and its assistant message must remain intact.
    pub fn begin_interruptible_wait(&mut self) {
        if self.interruptible_waits == 0 {
            self.wait_interrupt_flag.store(false, Ordering::SeqCst);
        }
        self.interruptible_waits += 1;
        self.runtime.waiting_interruptible = true;
        self.emit_goal_status();
        self.interrupt_wait_for_delivery();
    }

    pub fn end_interruptible_wait(&mut self) {
        self.interruptible_waits = self.interruptible_waits.saturating_sub(1);
        self.runtime.waiting_interruptible = self.interruptible_waits > 0;
        self.emit_goal_status();
    }

    pub fn interrupt_wait_for_delivery(&mut self) {
        if self.interruptible_waits > 0
            && self
                .pending_deliveries
                .iter()
                .chain(self.runner_pending_deliveries.iter())
                .any(|delivery| {
                    delivery.push != PushMode::Preempt && delivery.after_tool_call_id.is_none()
                })
        {
            self.wait_delivery_boundary = true;
            self.wait_interrupt_flag.store(true, Ordering::SeqCst);
            self.abort_notify.notify_waiters();
        }
    }

    /// Whether a delivery with this `push` may land right now.
    ///
    /// * `Preempt` always may — the caller aborts the draft first.
    /// * `Append` needs no draft and a closed assistant + tool-result window,
    ///   so the provider-visible prefix stays append-only.
    /// * `WhenIdle` additionally needs the whole turn to be over.
    pub fn delivery_boundary_open(&self, push: PushMode) -> bool {
        match push {
            PushMode::Preempt => true,
            PushMode::Append => {
                self.draft_message.is_none() && !self.has_pending_tool_result_window()
            }
            PushMode::WhenIdle => {
                self.draft_message.is_none()
                    && !self.has_pending_tool_result_window()
                    && (self.turn_finished() || self.wait_delivery_boundary)
            }
        }
    }

    pub fn queue_post_tool_delivery(
        &mut self,
        mut delivery: PendingDelivery,
    ) -> Result<DeliveryOutcome, String> {
        delivery.after_tool_call_id = Some(
            self.latest_assistant_tool_call_window(None)
                .and_then(|(index, ids)| {
                    (!self.all_tool_call_ids_have_results_after(index, &ids))
                        .then(|| ids.last().cloned())
                        .flatten()
                })
                .unwrap_or_default(),
        );
        self.enqueue_delivery(delivery)
    }

    pub(super) fn delivery_envelope_boundary_open(&self, delivery: &PendingDelivery) -> bool {
        let kinds: HashSet<_> = delivery
            .messages
            .iter()
            .filter_map(Self::control_kind)
            .collect();
        if !kinds.is_empty()
            && self
                .pending_deliveries
                .iter()
                .take_while(|pending| pending.id != delivery.id)
                .flat_map(|pending| pending.messages.iter())
                .chain(self.post_tool_side_effects.iter())
                .any(|message| Self::control_kind(message).is_some_and(|kind| kinds.contains(kind)))
        {
            return false;
        }
        if let Some(tool_call_id) = delivery.after_tool_call_id.as_deref() {
            if (tool_call_id.is_empty()
                && matches!(
                    self.runtime.state,
                    SessionState::Generating | SessionState::ExecutingTools
                ))
                || !self.delivery_boundary_open(PushMode::Append)
                || !self.tool_result_window_closed_for_tool_call(tool_call_id)
            {
                return false;
            }
        }
        self.delivery_boundary_open(delivery.push)
    }

    pub(super) fn knows_delivery_id(&self, id: &str) -> bool {
        self.delivered_delivery_ids.contains(id)
            || self
                .pending_deliveries
                .iter()
                .any(|pending| pending.id == id)
            || self
                .runner_pending_deliveries
                .iter()
                .any(|pending| pending.id == id)
    }

    /// Append one delivery's messages, stamping delivery provenance so dedupe
    /// and the UI survive a restart without the pending queue.
    pub(super) fn append_delivery(&mut self, delivery: &PendingDelivery) {
        if let Some(patch) = &delivery.thread_patch {
            let old_mode = self.thread.mode.clone();
            let (changed, sanitized) =
                crate::chat::queue::apply_setparams_patch(&mut self.thread, patch);
            if changed {
                self.emit(ChatEvent::ThreadUpdated { params: sanitized });
                self.increment_version();
            }
            if old_mode != self.thread.mode {
                self.add_message(event(
                    EventSubkind::ModeSwitch,
                    &delivery.source,
                    json!({"from": old_mode, "to": self.thread.mode}),
                    format!(
                        "Mode changed to {} by {}.",
                        self.thread.mode, delivery.source
                    ),
                ));
            }
        }
        for message in delivery.stamp_messages() {
            self.add_message_at_boundary(message);
        }
        if delivery.wake {
            self.delivery_wake_sources.insert(
                if matches!(
                    delivery.source.as_str(),
                    "chat.goal_monitor" | "chat.goal_verifier"
                ) {
                    "goal"
                } else {
                    "external"
                },
            );
        }
        self.delivered_delivery_ids.insert(delivery.id.clone());
    }

    /// Discard the in-flight draft and close interrupted tool calls for an
    /// urgent delivery, recording at most one cancellation note per preemption.
    pub(super) fn preempt_for_delivery(&mut self, source: &str) {
        let had_draft = self.draft_message.is_some();
        let was_active = matches!(
            self.runtime.state,
            SessionState::Generating | SessionState::ExecutingTools
        );
        if !had_draft && !was_active {
            return;
        }
        self.abort_flag.store(true, Ordering::SeqCst);
        self.user_interrupt_flag.store(true, Ordering::SeqCst);
        self.abort_notify.notify_waiters();
        if let Some(draft) = self.draft_message.take() {
            self.emit(ChatEvent::StreamFinished {
                message_id: draft.message_id.clone(),
                finish_reason: Some("abort".to_string()),
            });
            self.emit(ChatEvent::MessageRemoved {
                message_id: draft.message_id,
            });
        }
        self.draft_usage = None;
        self.stream_started_at = None;
        self.clear_pending_tool_calls_for_interruption();
        self.add_message(event(
            EventSubkind::CancellationNote,
            "chat.delivery",
            json!({"reason": "preempt", "source": source}),
            format!("Answer interrupted by an urgent message from {source}."),
        ));
        self.set_runtime_state(SessionState::Idle, None);
    }

    /// Accept a delivery. Landing happens immediately when the requested
    /// boundary is already open, otherwise the delivery waits in
    /// `pending_deliveries` until `drain_pending_deliveries` sees its boundary.
    pub fn enqueue_delivery(
        &mut self,
        delivery: PendingDelivery,
    ) -> Result<DeliveryOutcome, String> {
        if self.closed {
            return Err("chat session is closed".to_string());
        }
        if delivery.messages.is_empty() {
            return Err("delivery must contain at least one message".to_string());
        }
        if delivery
            .messages
            .iter()
            .any(|message| Self::control_kind(message).is_some())
        {
            self.try_accepted_control_projection()?;
        }
        if let Some(patch) = &delivery.thread_patch {
            let Some(fields) = patch.as_object() else {
                return Err("delivery thread_patch must be an object".into());
            };
            if fields
                .iter()
                .any(|(key, value)| !matches!(key.as_str(), "mode" | "model") || !value.is_string())
            {
                return Err(
                    "delivery thread_patch supports only string mode and model fields".into(),
                );
            }
        }
        if self.knows_delivery_id(&delivery.id) {
            return Ok(DeliveryOutcome::Duplicate);
        }
        if delivery
            .messages
            .iter()
            .any(|message| Self::control_kind(message).is_some())
        {
            self.try_accepted_control_projection()?;
        }
        // Reject before any side effect rather than dropping an accepted delivery.
        if self.pending_deliveries.len() >= max_queue_size() && delivery.push != PushMode::Preempt {
            return Err("chat delivery queue is full".to_string());
        }

        if delivery.push == PushMode::Preempt && self.delivery_envelope_boundary_open(&delivery) {
            self.preempt_for_delivery(&delivery.source);
            self.append_delivery(&delivery);
            self.emit_queue_update();
            self.mark_persisted_runtime_changed();
            return Ok(DeliveryOutcome::Delivered);
        }

        if self.delivery_envelope_boundary_open(&delivery)
            && !(self.wait_delivery_boundary && !self.pending_deliveries.is_empty())
        {
            self.append_delivery(&delivery);
            self.emit_queue_update();
            self.mark_persisted_runtime_changed();
            return Ok(DeliveryOutcome::Delivered);
        }

        self.pending_deliveries.push_back(delivery);
        self.interrupt_wait_for_delivery();
        self.emit_queue_update();
        self.mark_persisted_runtime_changed();
        Ok(DeliveryOutcome::Queued)
    }

    /// Land every pending delivery whose boundary is now open, in enqueue
    /// order. A blocked `when_idle` delivery never blocks a later `append` one,
    /// and nothing lands inside an open tool-result window.
    ///
    /// Returns `(delivered_ids, wake_requested)`.
    pub fn drain_pending_deliveries(&mut self) -> (Vec<String>, bool) {
        if self.pending_deliveries.is_empty() {
            return (Vec::new(), false);
        }
        let mut delivered_ids = Vec::new();
        let mut wake = false;
        loop {
            let Some(index) = self
                .pending_deliveries
                .iter()
                .position(|pending| self.delivery_envelope_boundary_open(pending))
            else {
                break;
            };
            let Some(delivery) = self.pending_deliveries.remove(index) else {
                break;
            };
            if self.delivered_delivery_ids.contains(&delivery.id) {
                continue;
            }
            if delivery.push == PushMode::Preempt {
                self.preempt_for_delivery(&delivery.source);
            }
            self.append_delivery(&delivery);
            delivered_ids.push(delivery.id);
            wake |= delivery.wake;
        }
        if !delivered_ids.is_empty() {
            self.emit_queue_update();
            self.mark_persisted_runtime_changed();
        }
        // Runner mirrors are consumed by the runner after importing tool results.
        if self.runner_pending_deliveries.is_empty()
            && !self.has_pending_tool_result_window()
            && self.interruptible_waits == 0
        {
            self.wait_delivery_boundary = false;
        }
        (delivered_ids, wake)
    }

    /// Reprioritize or cancel a still-pending delivery. Works while a
    /// generation is running, since the pending queue is independent of the
    /// command loop. Returns whether the delivery is now ready to land.
    pub fn update_pending_delivery(
        &mut self,
        delivery_id: &str,
        push: Option<PushMode>,
        cancel: bool,
    ) -> Result<bool, String> {
        let Some(index) = self
            .pending_deliveries
            .iter()
            .position(|pending| pending.id == delivery_id)
        else {
            if self.delivered_delivery_ids.contains(delivery_id) {
                return Err(format!("delivery '{delivery_id}' already delivered"));
            }
            return Err(format!("unknown pending delivery '{delivery_id}'"));
        };

        if cancel {
            let kinds: HashSet<_> = self.pending_deliveries[index]
                .messages
                .iter()
                .filter_map(Self::control_kind)
                .collect();
            if self
                .pending_deliveries
                .iter()
                .skip(index + 1)
                .flat_map(|pending| pending.messages.iter())
                .any(|message| Self::control_kind(message).is_some_and(|kind| kinds.contains(kind)))
            {
                return Err(
                    "pending plan/goal control has dependent updates; cancel later updates first"
                        .into(),
                );
            }
            self.pending_deliveries.remove(index);
            self.emit_queue_update();
            self.mark_persisted_runtime_changed();
            return Ok(false);
        }

        let Some(push) = push else {
            return Ok(self
                .pending_deliveries
                .get(index)
                .is_some_and(|pending| self.delivery_envelope_boundary_open(pending)));
        };
        if let Some(pending) = self.pending_deliveries.get_mut(index) {
            pending.push = push;
        }
        self.interrupt_wait_for_delivery();
        self.emit_queue_update();
        self.mark_persisted_runtime_changed();
        Ok(self.delivery_envelope_boundary_open(&self.pending_deliveries[index]))
    }

    /// Replace the runner-owned delivery mirror shown in queue snapshots. The
    /// background-agent registry stays authoritative; these are never drained
    /// by this session.
    pub fn set_runner_pending_deliveries(&mut self, deliveries: Vec<PendingDelivery>) {
        if self.runner_pending_deliveries == deliveries {
            return;
        }
        self.runner_pending_deliveries = deliveries;
        self.interrupt_wait_for_delivery();
        self.emit_queue_update();
    }

    /// Deliveries to persist: still-pending ones only (delivered batches live
    /// in the message history with their `extra.delivery` stamp).
    pub fn pending_deliveries_for_snapshot(&self) -> Vec<PendingDelivery> {
        let mut seen = HashSet::new();
        self.pending_deliveries
            .iter()
            .chain(self.runner_pending_deliveries.iter())
            .filter(|delivery| seen.insert(delivery.id.clone()))
            .cloned()
            .collect()
    }

    /// Restore pending deliveries from a trajectory, skipping any whose
    /// messages are already in history, preserving enqueue timestamps.
    pub fn restore_pending_deliveries(&mut self, deliveries: Vec<PendingDelivery>) {
        self.delivered_delivery_ids.extend(
            self.messages
                .iter()
                .filter_map(delivery_id_of_message)
                .map(str::to_string),
        );
        let mut seen: HashSet<String> = self
            .pending_deliveries
            .iter()
            .chain(self.runner_pending_deliveries.iter())
            .map(|delivery| delivery.id.clone())
            .collect();
        for delivery in deliveries {
            if delivery.messages.is_empty()
                || self.delivered_delivery_ids.contains(&delivery.id)
                || !seen.insert(delivery.id.clone())
            {
                continue;
            }
            self.pending_deliveries.push_back(delivery);
        }
    }
}

impl ChatSession {
    /// Queue rows for the UI: legacy command rows first, then pending
    /// deliveries owned by this session, then the runner-owned mirror.
    pub fn build_queued_items(&self) -> Vec<QueuedItem> {
        self.command_queue
            .iter()
            .map(|r| r.to_queued_item())
            .chain(
                self.pending_deliveries
                    .iter()
                    .chain(self.runner_pending_deliveries.iter())
                    .map(QueuedItem::from_pending_delivery),
            )
            .collect()
    }

    pub fn emit_queue_update(&mut self) {
        self.runtime.queue_size = self.command_queue.len()
            + self.pending_deliveries.len()
            + self.runner_pending_deliveries.len();
        self.runtime.queued_items = self.build_queued_items();
        self.emit(ChatEvent::QueueUpdated {
            queue_size: self.runtime.queue_size,
            queued_items: self.runtime.queued_items.clone(),
        });
    }

    pub fn reprioritize_queued_command(&mut self, client_request_id: &str, priority: bool) -> bool {
        let Some(index) = self
            .command_queue
            .iter()
            .position(|request| request.client_request_id == client_request_id)
        else {
            return false;
        };
        if self.command_queue[index].priority == priority {
            return true;
        }
        let interrupts_active_loop =
            priority && Self::command_interrupts_active_loop(&self.command_queue[index].command);
        let active = matches!(
            self.runtime.state,
            SessionState::Generating | SessionState::ExecutingTools
        );
        if interrupts_active_loop && active {
            self.abort_stream();
            self.clear_pending_tool_calls_for_interruption();
        }
        let Some(mut request) = self.command_queue.remove(index) else {
            return false;
        };
        request.priority = priority;
        if priority {
            let insert_pos = self
                .command_queue
                .iter()
                .position(|queued| !queued.priority)
                .unwrap_or(self.command_queue.len());
            self.command_queue.insert(insert_pos, request);
        } else {
            self.command_queue.push_back(request);
        }
        self.touch();
        self.emit_queue_update();
        self.queue_notify.notify_one();
        true
    }

    /// Try to insert a priority command before non-priority queued work.
    ///
    /// Accepted interrupting commands abort any active generation/tool loop before
    /// insertion. Duplicate or full-queue rejections leave the active loop and
    /// side effects untouched, so callers that add messages should do so only
    /// after receiving `Accepted`.
    pub fn enqueue_priority_command(
        &mut self,
        mut request: CommandRequest,
    ) -> EnqueueCommandOutcome {
        request.priority = true;
        self.enqueue_command(request)
    }

    pub fn enqueue_accepted_priority_command(
        &mut self,
        mut request: CommandRequest,
    ) -> EnqueueCommandOutcome {
        request.priority = true;
        if matches!(request.command, ChatCommand::Regenerate {})
            && self.command_queue.iter().any(|queued| {
                queued.priority && matches!(queued.command, ChatCommand::Regenerate {})
            })
        {
            self.touch();
            self.queue_notify.notify_one();
            return EnqueueCommandOutcome::Accepted;
        }
        if self.command_queue.len() >= max_queue_size()
            && !self.command_is_critical_for_queue(&request.command)
        {
            return EnqueueCommandOutcome::Full;
        }
        let interrupts_active_loop = Self::command_interrupts_active_loop(&request.command);
        let active = matches!(
            self.runtime.state,
            SessionState::Generating | SessionState::ExecutingTools
        );
        if interrupts_active_loop && active {
            self.abort_stream();
            self.clear_pending_tool_calls_for_interruption();
        }
        let insert_pos = self
            .command_queue
            .iter()
            .position(|r| !r.priority)
            .unwrap_or(self.command_queue.len());
        if perf_diagnostics::is_enabled() {
            self.command_enqueued_at
                .insert(request.client_request_id.clone(), Instant::now());
        }
        self.command_queue.insert(insert_pos, request);
        self.touch();
        self.emit_queue_update();
        self.queue_notify.notify_one();
        EnqueueCommandOutcome::Accepted
    }

    pub fn set_paused_with_reasons_and_auto_approved(
        &mut self,
        reasons: Vec<PauseReason>,
        auto_approved_ids: Vec<String>,
        message_index: Option<usize>,
    ) {
        if perf_diagnostics::is_enabled() {
            let continues_existing_pause = self.runtime.state == SessionState::Paused
                && self.confirmation_paused_at.is_some()
                && self.runtime.pause_reasons.iter().any(|existing| {
                    reasons
                        .iter()
                        .any(|incoming| incoming.tool_call_id == existing.tool_call_id)
                });
            if continues_existing_pause {
                if let Some((_, tool_call_ids)) = self.confirmation_paused_at.as_mut() {
                    tool_call_ids.extend(reasons.iter().map(|reason| reason.tool_call_id.clone()));
                }
            } else {
                self.confirmation_paused_at = Some((
                    Instant::now(),
                    reasons
                        .iter()
                        .map(|reason| reason.tool_call_id.clone())
                        .collect(),
                ));
            }
        }
        self.runtime.pause_reasons = reasons.clone();
        self.runtime.auto_approved_tool_ids = auto_approved_ids;
        self.runtime.accepted_tool_ids.clear();
        self.runtime.paused_message_index = message_index;
        self.emit(ChatEvent::PauseRequired { reasons });
        self.set_runtime_state(SessionState::Paused, None);
    }
}
