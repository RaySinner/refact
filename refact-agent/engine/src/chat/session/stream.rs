use super::*;

/// Why `start_stream` refused to open a new assistant turn.
///
/// Every variant carries its own `error_info` rather than being classified from
/// prose. That is deliberate: `make_ui_only_error_message` runs
/// `classify_user_error` over the text, and a message like "waiting for a mode
/// switch" matches no known provider fingerprint, so it would land in
/// `UserErrorCategory::Unknown` and render as the generic "Unknown error" card,
/// hiding the very cause we are trying to surface. The GUI renders
/// `error_info.title` and `error_info.explanation`, so those carry the meaning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum StreamStartRefusal {
    /// A context rebuild owns the session right now. Transient: the user's
    /// message is intact and runs once compaction finishes.
    Compression,
    /// A rebuild was requested and has not run yet.
    PendingContextRebuild,
    /// A mode handoff is queued and still owns the conversation.
    PendingModeHandoff,
    /// The active context cannot be projected; generation cannot build a prompt.
    InvalidActiveContext(String),
    /// Tool calls from a previous step are still executing.
    ExecutingTools,
    /// A draft assistant response is still open.
    DraftOpen,
}

impl StreamStartRefusal {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Compression => "a context rebuild is in progress",
            Self::PendingContextRebuild => "a context rebuild is pending",
            Self::PendingModeHandoff => "a mode handoff is pending",
            Self::InvalidActiveContext(_) => "the active context is invalid",
            Self::ExecutingTools => "tool calls are still executing",
            Self::DraftOpen => "a draft response is still open",
        }
    }

    pub(super) fn user_message(&self) -> ChatMessage {
        // Built before the match so the borrow outlives the tuple below. The detail is
        // interpolated rather than matched on: `classify_user_error` recognises none of the
        // real `ActiveContextError` prose, so routing this arm through the classifier-based
        // `make_ui_only_error_message` renders it as "Unknown error" — the exact swallow this
        // enum exists to prevent.
        let invalid_context_detail = match self {
            Self::InvalidActiveContext(detail) => Some(format!(
                "The stored history for this chat could not be projected into a request \
                 context ({detail}), so this message has not been processed. An explicit \
                 context rebuild is required before generation can continue."
            )),
            _ => None,
        };

        let (category, title, explanation, suggested_action, is_retryable) = match self {
            Self::Compression => (
                "ProviderTransient",
                "Waiting for context compaction",
                "Context compaction is still running, so this message has not been processed \
yet. It stays in the chat and runs as soon as compaction finishes. If compaction does \
not finish, this chat reports that compaction stopped instead of staying silent.",
                "none",
                true,
            ),
            Self::PendingContextRebuild => (
                "InvalidRequest",
                "Waiting for a context rebuild",
                "A context rebuild is queued for this chat and has not run yet, so this message \
has not been processed. The rebuild runs before the next request, then this message is \
picked up.",
                "none",
                false,
            ),
            Self::PendingModeHandoff => (
                "InvalidRequest",
                "Waiting for a mode switch",
                "A mode switch is still being applied to this chat, so this message has not been \
processed yet. It is picked up once the switch completes.",
                "none",
                false,
            ),
            Self::InvalidActiveContext(_) => (
                "InvalidRequest",
                "This chat needs an explicit context rebuild",
                invalid_context_detail.as_deref().unwrap_or_default(),
                "compact",
                false,
            ),
            Self::ExecutingTools => (
                "InvalidRequest",
                "Previous tool calls are still running",
                "Tool calls from the previous step have not finished, so this message has not \
been processed yet. It is picked up as soon as those tool calls return.",
                "none",
                false,
            ),
            Self::DraftOpen => (
                "InvalidRequest",
                "Previous response is still open",
                "An assistant response is still in progress, so this message has not been \
processed yet. It is picked up once that response finishes.",
                "none",
                false,
            ),
        };

        let cause = self.as_str();
        let mut message = make_ui_only_error_message_with_info(
            cause,
            category,
            title,
            explanation,
            suggested_action,
            is_retryable,
        );
        message.extra.insert(
            "refusal".to_string(),
            serde_json::json!({
                "source": "chat.session.start_stream",
                "cause": cause,
            }),
        );
        message
    }
}

impl ChatSession {
    pub fn start_stream(&mut self) -> Option<(String, Arc<AtomicBool>)> {
        if let Some(refusal) = self.stream_start_refusal() {
            warn!(
                "Attempted to start stream while blocked: {}",
                refusal.as_str()
            );
            return None;
        }
        self.wait_delivery_boundary = false;
        self.abort_flag.store(false, Ordering::SeqCst);
        self.user_interrupt_flag.store(false, Ordering::SeqCst);
        let message_id = Uuid::new_v4().to_string();
        self.draft_message = Some(ChatMessage {
            message_id: message_id.clone(),
            role: "assistant".to_string(),
            ..Default::default()
        });
        self.draft_usage = None;
        self.stream_started_at = perf_diagnostics::is_enabled().then(Instant::now);
        self.set_runtime_state(SessionState::Generating, None);
        self.emit(ChatEvent::StreamStarted {
            message_id: message_id.clone(),
        });
        self.touch();
        Some((message_id, self.abort_flag.clone()))
    }

    /// The first blocker that would make `start_stream` refuse, in the order the
    /// refusal check applies them.
    ///
    /// Compression is checked first because it is the wait a user can actually
    /// observe (the "Compacting older context…" spinner), so naming it beats
    /// naming whatever incidental state happens to be set alongside it.
    pub(crate) fn stream_start_refusal(&self) -> Option<StreamStartRefusal> {
        if crate::chat::context_rebuild::compression_attempt_active(self) {
            return Some(StreamStartRefusal::Compression);
        }
        if self.pending_context_rebuild.is_some() {
            return Some(StreamStartRefusal::PendingContextRebuild);
        }
        if self.pending_mode_handoff.is_some() {
            return Some(StreamStartRefusal::PendingModeHandoff);
        }
        if let Err(error) = refact_core::active_context::active_context(&self.messages) {
            return Some(StreamStartRefusal::InvalidActiveContext(error.to_string()));
        }
        if self.runtime.state == SessionState::ExecutingTools {
            return Some(StreamStartRefusal::ExecutingTools);
        }
        if self.draft_message.is_some() {
            return Some(StreamStartRefusal::DraftOpen);
        }
        None
    }

    /// Tell the user why nothing happened, instead of only logging it.
    ///
    /// Idempotent by construction: `append_error_message_deduped_content`
    /// collapses repeats of the same text, so a session that keeps waking up
    /// while blocked bumps `repeat_count` on one card rather than appending a
    /// new one on every wake.
    pub(crate) fn append_stream_start_refusal(&mut self, refusal: &StreamStartRefusal) {
        self.append_error_message_deduped_content(refusal.user_message());
    }

    pub fn emit_stream_delta(&mut self, ops: Vec<DeltaOp>) {
        self.emit_stream_delta_after_emit(ops, None);
    }

    #[cfg(test)]
    pub(crate) fn emit_stream_delta_at(&mut self, ops: Vec<DeltaOp>, now: Instant) {
        self.emit_stream_delta_after_emit(ops, Some(now));
    }

    pub(super) fn emit_stream_delta_after_emit(&mut self, ops: Vec<DeltaOp>, now: Option<Instant>) {
        let (message_id, applied) = match &mut self.draft_message {
            Some(draft) => {
                let mut applied = false;
                for op in &ops {
                    match op {
                        DeltaOp::AppendContent { text } => match &mut draft.content {
                            ChatContent::SimpleText(s) => {
                                if !text.is_empty() {
                                    s.push_str(text);
                                    applied = true;
                                }
                            }
                            _ => {
                                if !text.is_empty() {
                                    draft.content = ChatContent::SimpleText(text.clone());
                                    applied = true;
                                }
                            }
                        },
                        DeltaOp::AppendReasoning { text } => {
                            if !text.is_empty() {
                                let reasoning =
                                    draft.reasoning_content.get_or_insert_with(String::new);
                                reasoning.push_str(text);
                                applied = true;
                            }
                        }
                        DeltaOp::SetReasoning { text } => {
                            if !text.is_empty() && draft.reasoning_content.as_deref() != Some(text)
                            {
                                draft.reasoning_content = Some(text.clone());
                                applied = true;
                            }
                        }
                        DeltaOp::SetToolCalls { tool_calls } => {
                            let had_tool_calls = draft
                                .tool_calls
                                .as_ref()
                                .map_or(false, |calls| !calls.is_empty());
                            if !tool_calls.is_empty() || had_tool_calls {
                                if let Ok(parsed) = serde_json::from_value(json!(tool_calls)) {
                                    draft.tool_calls = Some(parsed);
                                    applied = true;
                                }
                            }
                        }
                        DeltaOp::SetThinkingBlocks { blocks } => {
                            let had_blocks = draft
                                .thinking_blocks
                                .as_ref()
                                .map_or(false, |current| !current.is_empty());
                            if !blocks.is_empty() || had_blocks {
                                draft.thinking_blocks = Some(blocks.clone());
                                applied = true;
                            }
                        }
                        DeltaOp::AddCitation { citation } => {
                            if !citation.is_null() {
                                draft.citations.push(citation.clone());
                                applied = true;
                            }
                        }
                        DeltaOp::AddServerContentBlock { block } => {
                            if !block.is_null() {
                                draft.server_content_blocks.push(block.clone());
                                applied = true;
                            }
                        }
                        DeltaOp::SetUsage { usage } => {
                            if let Ok(u) = serde_json::from_value(usage.clone()) {
                                draft.usage = Some(u);
                                applied = true;
                            }
                        }
                        DeltaOp::MergeExtra { extra } => {
                            if !extra.is_empty() {
                                draft.extra.extend(extra.clone());
                                applied = true;
                            }
                        }
                    }
                }
                (draft.message_id.clone(), applied)
            }
            None => return,
        };
        self.emit(ChatEvent::StreamDelta { message_id, ops });
        if applied {
            if perf_diagnostics::is_enabled() {
                if let Some(started_at) = self.stream_started_at.take() {
                    let emitted_at = now.unwrap_or_else(Instant::now);
                    perf_diagnostics::record(
                        PerfComponent::StreamFirstDelta,
                        Some(&self.chat_id),
                        PerfOutcome::Success,
                        emitted_at
                            .duration_since(started_at)
                            .as_micros()
                            .try_into()
                            .unwrap_or(u64::MAX),
                        None,
                        None,
                        None,
                    );
                }
            }
            self.mark_stream_delta();
        }
    }

    pub fn finish_stream(&mut self, finish_reason: Option<String>) {
        self.finish_stream_with_next_state(finish_reason, SessionState::Idle);
    }

    pub fn finish_stream_with_next_state(
        &mut self,
        finish_reason: Option<String>,
        next_state: SessionState,
    ) {
        self.stream_started_at = None;
        if let Some(mut draft) = self.draft_message.take() {
            let should_keep_draft = has_displayable_assistant_content(&draft);

            self.emit(ChatEvent::StreamFinished {
                message_id: draft.message_id.clone(),
                finish_reason: finish_reason.clone(),
            });

            if should_keep_draft {
                draft.finish_reason = finish_reason;
                if let Some(usage) = self.draft_usage.take() {
                    draft.usage = Some(usage);
                }
                self.add_message(draft);
            } else {
                tracing::warn!("Discarding empty assistant message");
                self.emit(ChatEvent::MessageRemoved {
                    message_id: draft.message_id,
                });
            }
        }
        self.draft_usage = None;
        self.set_runtime_state(next_state, None);
        if next_state != SessionState::ExecutingTools {
            self.release_turn_only_state();
        }
        self.touch();
    }

    /// Preserve accepted text before rebuilding, but never retain incomplete tool calls.
    pub fn finish_stream_for_rebuild(&mut self) {
        if let Some(mut draft) = self.draft_message.take() {
            self.emit(ChatEvent::StreamFinished {
                message_id: draft.message_id.clone(),
                finish_reason: Some("context_length".into()),
            });
            if !draft.content.content_text_only().trim().is_empty() {
                draft.tool_calls = None;
                draft.finish_reason = Some("context_length".into());
                draft.usage = self.draft_usage.take();
                self.add_message(draft);
            } else {
                self.emit(ChatEvent::MessageRemoved {
                    message_id: draft.message_id,
                });
            }
        }
        self.draft_usage = None;
        self.stream_started_at = None;
        self.set_runtime_state(SessionState::Idle, None);
    }

    pub fn clear_stream_for_retry(&mut self) {
        if let Some(draft) = self.draft_message.take() {
            self.emit(ChatEvent::MessageRemoved {
                message_id: draft.message_id,
            });
        }
        self.draft_usage = None;
        self.stream_started_at = None;
        self.task_agent_error = None;
        self.runtime.error = None;
        self.set_runtime_state(SessionState::Idle, None);
        self.touch();
    }

    /// Append a ui-only error message, collapsing repeats: if the same error
    /// text was already the most recent error (allowing a few hidden `event`
    /// messages in between, e.g. goal nudges), bump its `repeat_count` and
    /// emit `MessageUpdated` instead of appending another copy.
    pub fn append_error_message_deduped(&mut self, error: &str) {
        self.append_error_message_deduped_content(make_ui_only_error_message(error));
    }

    /// Same collapsing rule as `append_error_message_deduped`, for callers that
    /// build the message themselves (e.g. a caller-supplied `error_info`).
    pub(crate) fn append_error_message_deduped_content(&mut self, fresh: ChatMessage) {
        const LOOKBACK_PAST_EVENTS: usize = 4;
        let fresh_text = fresh.content.content_text_only();

        let mut candidate_idx: Option<usize> = None;
        let mut events_skipped = 0usize;
        for (idx, message) in self.messages.iter().enumerate().rev() {
            match message.role.as_str() {
                "event" if events_skipped < LOOKBACK_PAST_EVENTS => {
                    events_skipped += 1;
                }
                "error" => {
                    if message.content.content_text_only() == fresh_text {
                        candidate_idx = Some(idx);
                    }
                    break;
                }
                _ => break,
            }
        }

        if let Some(idx) = candidate_idx {
            let updated = {
                let message = &mut self.messages[idx];
                let count = message
                    .extra
                    .get("repeat_count")
                    .and_then(|value| value.as_u64())
                    .unwrap_or(1);
                message
                    .extra
                    .insert("repeat_count".to_string(), serde_json::json!(count + 1));
                message.clone()
            };
            self.emit(ChatEvent::MessageUpdated {
                message_id: updated.message_id.clone(),
                message: updated,
            });
            self.increment_version();
            self.touch();
            return;
        }

        self.add_message(fresh);
    }

    pub fn finish_stream_with_error(&mut self, error: String) {
        if let Some(mut draft) = self.draft_message.take() {
            if has_displayable_assistant_content(&draft) {
                self.emit(ChatEvent::StreamFinished {
                    message_id: draft.message_id.clone(),
                    finish_reason: Some("error".to_string()),
                });
                draft.finish_reason = Some("error".to_string());
                if let Some(usage) = self.draft_usage.take() {
                    draft.usage = Some(usage);
                }
                self.add_message(draft);
            } else {
                self.emit(ChatEvent::MessageRemoved {
                    message_id: draft.message_id,
                });
            }
        }
        self.stream_started_at = None;
        self.append_error_message_deduped(&error);
        self.set_runtime_state(SessionState::Error, Some(error.clone()));
        self.touch();

        // Store task_meta for async notification (need to clone before async)
        self.task_agent_error = Some(error);
    }

    pub fn abort_stream(&mut self) {
        self.pending_context_rebuild = None;
        self.pending_mode_handoff = None;
        if let Some(flag) = &self.compression_abort_flag {
            flag.store(true, Ordering::SeqCst);
        }
        self.abort_flag.store(true, Ordering::SeqCst);
        self.user_interrupt_flag.store(true, Ordering::SeqCst);
        self.abort_notify.notify_waiters();
        self.refresh_goal_runtime_mirror();
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
        self.confirmation_paused_at = None;
        self.set_runtime_state(SessionState::Idle, None);
        self.release_turn_only_state();
        self.touch();
        self.queue_notify.notify_one();
    }

    pub fn clear_pending_tool_calls_for_interruption(&mut self) {
        let latest_window = self.latest_assistant_tool_call_window(None);
        let mut updated_message = None;
        if let Some((assistant_index, _)) = latest_window {
            let answered_ids = self.answered_tool_call_ids_for_interruption(assistant_index);
            let Some(message) = self.messages.get_mut(assistant_index) else {
                return;
            };
            let Some(tool_calls) = message.tool_calls.as_ref() else {
                return;
            };
            let retained_tool_calls: Vec<_> = tool_calls
                .iter()
                .filter(|tool_call| answered_ids.contains(&tool_call.id))
                .cloned()
                .collect();

            if retained_tool_calls.len() != tool_calls.len() {
                message.tool_calls = if retained_tool_calls.is_empty() {
                    None
                } else {
                    Some(retained_tool_calls)
                };
                updated_message = Some(message.clone());
            }
        }

        if let Some(message) = updated_message {
            self.increment_version();
            self.emit(ChatEvent::MessageUpdated {
                message_id: message.message_id.clone(),
                message,
            });
        }
    }
}
