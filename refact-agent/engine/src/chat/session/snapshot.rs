use super::*;

impl ChatSession {
    pub fn emit(&mut self, event: ChatEvent) {
        if self.event_tx.receiver_count() == 0 {
            if perf_diagnostics::is_enabled() {
                perf_diagnostics::record(
                    PerfComponent::SseBroadcast,
                    Some(&self.chat_id),
                    PerfOutcome::Skipped,
                    0,
                    None,
                    None,
                    None,
                );
            }
            return;
        }
        let serialize_started_at = perf_diagnostics::is_enabled().then(Instant::now);
        self.event_seq += 1;
        let envelope = EventEnvelope {
            chat_id: self.chat_id.clone(),
            seq: self.event_seq,
            event,
        };
        match serde_json::to_string(&envelope) {
            Ok(json) => {
                let broadcast_started_at = serialize_started_at.map(|_| Instant::now());
                let size_bytes = serialize_started_at.map(|_| json.len() as u64);
                let receiver_count = serialize_started_at.map(|_| self.event_tx.receiver_count());
                let broadcast_result = self.event_tx.send(Arc::new(json));
                if let (
                    Some(serialize_started_at),
                    Some(broadcast_started_at),
                    Some(size_bytes),
                    Some(receiver_count),
                ) = (
                    serialize_started_at,
                    broadcast_started_at,
                    size_bytes,
                    receiver_count,
                ) {
                    let serialize_us = serialize_started_at
                        .elapsed()
                        .as_micros()
                        .try_into()
                        .unwrap_or(u64::MAX);
                    let broadcast_us = broadcast_started_at
                        .elapsed()
                        .as_micros()
                        .try_into()
                        .unwrap_or(u64::MAX);
                    self.record_sse_timing(
                        serialize_us,
                        broadcast_us,
                        size_bytes,
                        sse_broadcast_outcome(receiver_count, &broadcast_result),
                    );
                }
            }
            Err(e) => {
                if let Some(serialize_started_at) = serialize_started_at {
                    perf_diagnostics::record(
                        PerfComponent::SseSerialize,
                        Some(&self.chat_id),
                        PerfOutcome::Failure,
                        serialize_started_at
                            .elapsed()
                            .as_micros()
                            .try_into()
                            .unwrap_or(u64::MAX),
                        None,
                        None,
                        None,
                    );
                }
                tracing::error!("Failed to serialize SSE event for {}: {}", self.chat_id, e);
            }
        }
    }

    pub(super) fn record_sse_timing(
        &self,
        serialize_us: u64,
        broadcast_us: u64,
        size_bytes: u64,
        broadcast_outcome: PerfOutcome,
    ) {
        perf_diagnostics::record(
            PerfComponent::SseSerialize,
            Some(&self.chat_id),
            PerfOutcome::Success,
            serialize_us,
            Some(size_bytes),
            None,
            None,
        );
        perf_diagnostics::record(
            PerfComponent::SseBroadcast,
            Some(&self.chat_id),
            broadcast_outcome,
            broadcast_us,
            None,
            None,
            None,
        );
    }

    pub fn upsert_background_agent(&mut self, agent: BackgroundAgentSummary) {
        if should_replace_background_agent(self.background_agents.get(&agent.agent_id), &agent) {
            self.background_agents.insert(agent.agent_id.clone(), agent);
        }
    }

    pub fn upsert_background_agents<I>(&mut self, agents: I)
    where
        I: IntoIterator<Item = BackgroundAgentSummary>,
    {
        for agent in agents {
            self.upsert_background_agent(agent);
        }
    }

    pub fn snapshot(&self) -> ChatEvent {
        let mut background_agents: Vec<_> = self.background_agents.values().cloned().collect();
        background_agents.sort_by(|a, b| {
            b.change_seq
                .cmp(&a.change_seq)
                .then(a.agent_id.cmp(&b.agent_id))
        });
        self.snapshot_with_background_agents(background_agents)
    }

    pub fn snapshot_with_background_agents(
        &self,
        background_agents: Vec<BackgroundAgentSummary>,
    ) -> ChatEvent {
        let mut messages = self.messages.clone();
        if self.runtime.state == SessionState::Generating {
            if let Some(ref draft) = self.draft_message {
                if has_displayable_assistant_content(draft) {
                    messages.push(draft.clone());
                }
            }
        }
        let mut runtime = self.runtime.clone();
        apply_goal_runtime_projection(&mut runtime, self.goal.as_ref());
        runtime.is_compressing = self.is_compressing;
        runtime.compression_phase = self.compression_phase;
        runtime.compression_reason = self.compression_reason;
        runtime.compression_estimated_tokens = self.compression_estimated_tokens;
        runtime.compression_effective_cap = self.compression_effective_cap;
        runtime.compression_cap_source = self.compression_cap_source.clone();
        runtime.queued_items = self.build_queued_items();
        runtime.queue_size = runtime.queued_items.len();
        ChatEvent::Snapshot {
            goal: self.goal.clone(),
            thread: self.thread.clone(),
            runtime,
            messages,
            background_agents,
            browser: None,
        }
    }
}

impl ChatSession {
    #[cfg(test)]
    pub(crate) fn record_sse_timing_for_test(
        &self,
        serialize_us: u64,
        broadcast_us: u64,
        size_bytes: u64,
    ) {
        self.record_sse_timing(serialize_us, broadcast_us, size_bytes, PerfOutcome::Success);
    }
}

pub(super) fn sse_broadcast_outcome(
    receiver_count: usize,
    result: &Result<usize, broadcast::error::SendError<Arc<String>>>,
) -> PerfOutcome {
    if result.is_ok() {
        PerfOutcome::Success
    } else if receiver_count == 0 {
        PerfOutcome::Skipped
    } else {
        PerfOutcome::Failure
    }
}
