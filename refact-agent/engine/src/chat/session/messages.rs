use super::*;

impl ChatSession {
    pub fn add_message(&mut self, message: ChatMessage) {
        // Legacy event producers must not insert ahead of an uncommitted draft
        // or between an assistant tool call and its results.
        if message.role == "event"
            && (self.draft_message.is_some() || self.has_pending_tool_result_window())
        {
            self.pending_deliveries.push_back(PendingDelivery::new(
                vec![message],
                PushMode::Append,
                "chat.session",
                false,
            ));
            self.emit_queue_update();
            self.mark_persisted_runtime_changed();
            return;
        }
        self.add_message_at_boundary(message);
    }

    pub(super) fn add_message_at_boundary(&mut self, mut message: ChatMessage) {
        if message.message_id.is_empty() {
            message.message_id = Uuid::new_v4().to_string();
        }
        stamp_created_at_ms(&mut message);
        let affects_goal = message_affects_goal_projection(&message);
        if message.role == "tool"
            && self.goal.is_some()
            && tool_result_counts_as_goal_evidence(&message, &self.messages)
        {
            self.goal_turn_evidence = true;
        }
        let index = self.messages.len();
        self.messages.push(message.clone());
        if affects_goal {
            self.rebuild_goal_projection_from_messages();
        }
        self.emit(ChatEvent::MessageAdded { message, index });
        self.increment_version();
        self.touch();
    }

    pub fn queue_post_tool_side_effect(&mut self, message: ChatMessage) {
        self.post_tool_side_effects.push_back(message);
        self.touch();
    }

    pub(super) fn latest_assistant_tool_call_window(
        &self,
        tool_call_id: Option<&str>,
    ) -> Option<(usize, Vec<String>)> {
        let start = self.mutable_message_start()?;
        self.messages
            .iter()
            .enumerate()
            .skip(start)
            .rev()
            .find_map(|(idx, message)| {
                if message.role != "assistant" {
                    return None;
                }
                let tool_calls = message.tool_calls.as_ref()?;
                if tool_calls.is_empty() {
                    None
                } else if tool_call_id.map_or(true, |id| tool_calls.iter().any(|tc| tc.id == id)) {
                    Some((idx, tool_calls.iter().map(|tc| tc.id.clone()).collect()))
                } else {
                    None
                }
            })
    }

    pub(super) fn result_ids_after_assistant(&self, assistant_index: usize) -> HashSet<String> {
        let mut result_ids = HashSet::new();
        for message in self.messages.iter().skip(assistant_index + 1) {
            match message.role.as_str() {
                "tool" | "diff" if !message.tool_call_id.is_empty() => {
                    result_ids.insert(message.tool_call_id.clone());
                }
                role if role == "assistant"
                    || role == "user"
                    || role == crate::chat::internal_roles::EVENT_ROLE
                    || role == crate::chat::internal_roles::PLAN_ROLE
                    || role == crate::chat::internal_roles::GOAL_ROLE =>
                {
                    break;
                }
                _ => {}
            }
        }
        result_ids
    }

    pub(super) fn answered_tool_call_ids_for_interruption(
        &self,
        assistant_index: usize,
    ) -> HashSet<String> {
        let mut result_ids = HashSet::new();
        for message in self.messages.iter().skip(assistant_index + 1) {
            match message.role.as_str() {
                "tool" | "diff" if !message.tool_call_id.is_empty() => {
                    result_ids.insert(message.tool_call_id.clone());
                }
                role if role == crate::chat::internal_roles::EVENT_ROLE
                    || role == crate::chat::internal_roles::PLAN_ROLE
                    || role == crate::chat::internal_roles::GOAL_ROLE => {}
                _ => break,
            }
        }
        result_ids
    }

    pub(super) fn all_tool_call_ids_have_results_after(
        &self,
        assistant_index: usize,
        tool_call_ids: &[String],
    ) -> bool {
        let result_ids = self.result_ids_after_assistant(assistant_index);
        tool_call_ids.iter().all(|id| result_ids.contains(id))
    }

    pub(super) fn has_pending_tool_result_window(&self) -> bool {
        self.latest_assistant_tool_call_window(None)
            .map(|(assistant_index, tool_call_ids)| {
                !self.all_tool_call_ids_have_results_after(assistant_index, &tool_call_ids)
            })
            .unwrap_or(false)
    }

    pub(super) fn tool_result_window_closed_for_tool_call(&self, tool_call_id: &str) -> bool {
        if let Some((assistant_index, tool_call_ids)) =
            self.latest_assistant_tool_call_window(Some(tool_call_id))
        {
            self.all_tool_call_ids_have_results_after(assistant_index, &tool_call_ids)
        } else {
            !self.has_pending_tool_result_window()
        }
    }
}

impl ChatSession {
    pub fn record_ide_tool_result(
        &mut self,
        tool_call_id: String,
        content: String,
        tool_failed: bool,
    ) -> bool {
        let ok = !tool_failed;
        self.add_message(ChatMessage {
            message_id: Uuid::new_v4().to_string(),
            role: "tool".to_string(),
            content: ChatContent::SimpleText(content.clone()),
            tool_call_id: tool_call_id.clone(),
            tool_failed: Some(tool_failed),
            ..Default::default()
        });
        self.queue_post_tool_side_effect(crate::chat::internal_roles::event(
            crate::chat::internal_roles::EventSubkind::IdeCallback,
            "ide.bridge",
            json!({"tool_call_id": tool_call_id.clone(), "ok": ok, "summary": content.clone()}),
            content,
        ));
        let completed = self.tool_result_window_closed_for_tool_call(&tool_call_id);
        if completed {
            self.drain_post_tool_side_effects();
        }
        completed
    }

    pub fn accepted_control_messages(&self) -> impl Iterator<Item = ChatMessage> {
        self.accepted_control_projection().messages.into_iter()
    }

    pub fn try_accepted_control_projection(&self) -> Result<ChatSession, String> {
        let mut projection = ChatSession::new(self.chat_id.clone());
        projection.thread = self.thread.clone();
        projection.goal = self.goal.clone();
        projection.goal_ledger = self.goal_ledger.clone();
        projection.messages = refact_core::active_context::active_context(&self.messages)
            .map_err(|error| error.to_string())?
            .messages;
        projection
            .messages
            .extend(self.post_tool_side_effects.iter().cloned());
        projection.messages.extend(
            self.pending_deliveries
                .iter()
                .flat_map(|delivery| delivery.messages.iter().cloned()),
        );
        let mut seen = HashSet::new();
        projection.messages.retain(|message| {
            Self::control_kind(message).is_some()
                && (message.message_id.is_empty() || seen.insert(message.message_id.clone()))
        });
        Ok(projection)
    }

    /// Legacy callers cannot return an error; retain the invalid boundary so
    /// fallible control readers still reject it rather than treating it as empty.
    pub fn accepted_control_projection(&self) -> ChatSession {
        let mut projection = ChatSession::new(self.chat_id.clone());
        projection.thread = self.thread.clone();
        match self.try_accepted_control_projection() {
            Ok(projected) => return projected,
            Err(error) => {
                let mut boundary = ChatMessage::new("compression_report".into(), error.clone());
                boundary.extra.insert(
                    "compression_report".into(),
                    json!({"kind": "invalid_context"}),
                );
                projection.messages = vec![boundary];
                projection.runtime.state = SessionState::Error;
                projection.runtime.error = Some(error);
            }
        }
        projection
    }

    pub(super) fn control_kind(message: &ChatMessage) -> Option<&'static str> {
        match message.role.as_str() {
            "plan" => Some("plan"),
            "goal" => Some("goal"),
            "event" => match message
                .extra
                .get("event")
                .and_then(|event| event.get("subkind"))
                .and_then(serde_json::Value::as_str)
            {
                Some("plan_delta") => Some("plan"),
                Some("goal_delta" | "goal_status" | "goal_verdict" | "goal_pursuit") => {
                    Some("goal")
                }
                _ => None,
            },
            _ => None,
        }
    }
}

impl ChatSession {
    pub fn insert_message(&mut self, index: usize, mut message: ChatMessage) {
        if message.message_id.is_empty() {
            message.message_id = Uuid::new_v4().to_string();
        }
        stamp_created_at_ms(&mut message);
        let affects_goal = message_affects_goal_projection(&message);
        let insert_idx = index.min(self.messages.len());
        self.messages.insert(insert_idx, message.clone());
        if affects_goal {
            self.rebuild_goal_projection_from_messages();
        }
        self.emit(ChatEvent::MessageAdded {
            message,
            index: insert_idx,
        });
        self.increment_version();
        self.touch();
    }

    /// Insert into the active view: inside the reconstructed payload when a boundary
    /// hides the raw head, otherwise into the raw history at the same index.
    pub fn insert_active_message(
        &mut self,
        active_index: usize,
        mut message: ChatMessage,
    ) -> Result<refact_core::active_context::MessageOrigin, String> {
        use refact_core::active_context::MessageOrigin;
        if message.message_id.is_empty() {
            message.message_id = Uuid::new_v4().to_string();
        }
        stamp_created_at_ms(&mut message);
        let (messages, origin) = refact_core::active_context::insert_active_message(
            &self.messages,
            active_index,
            message.clone(),
        )
        .map_err(|error| error.to_string())?;
        let affects_goal = message_affects_goal_projection(&message);
        self.messages = messages;
        if affects_goal {
            self.rebuild_goal_projection_from_messages();
        }
        match origin {
            MessageOrigin::Stored { message_index } => {
                self.emit(ChatEvent::MessageAdded {
                    message,
                    index: message_index,
                });
            }
            MessageOrigin::ReportPayload { report_index, .. } => {
                let report = self.messages[report_index].clone();
                self.emit(ChatEvent::MessageUpdated {
                    message_id: report.message_id.clone(),
                    message: report,
                });
            }
        }
        self.increment_version();
        self.touch();
        Ok(origin)
    }

    pub(super) fn mutable_message_start(&self) -> Option<usize> {
        refact_core::active_context::active_context(&self.messages)
            .ok()
            .map(|active| active.report_index.map_or(0, |index| index + 1))
    }

    pub fn update_message(&mut self, message_id: &str, message: ChatMessage) -> Option<usize> {
        let start = self.mutable_message_start()?;
        if message.role == "compression_report"
            || message.extra.contains_key("compression_report")
            || refact_core::active_context::is_legacy_summary(&message)
        {
            return None;
        }
        if let Some(idx) = self
            .messages
            .iter()
            .position(|m| m.message_id == message_id)
        {
            if idx < start {
                return None;
            }
            let affects_goal = message_affects_goal_projection(&self.messages[idx])
                || message_affects_goal_projection(&message);
            self.messages[idx] = message.clone();
            if affects_goal {
                self.rebuild_goal_projection_from_messages();
            }
            self.thread.previous_response_id = None;
            self.reset_cache_guard_snapshot();
            self.provider_usage_stale = true;
            self.emit(ChatEvent::MessageUpdated {
                message_id: message_id.to_string(),
                message,
            });
            self.goal_reset_no_progress();
            self.increment_version();
            self.touch();
            return Some(idx);
        }
        None
    }

    pub fn remove_message(&mut self, message_id: &str) -> Option<usize> {
        let start = self.mutable_message_start()?;
        if let Some(idx) = self
            .messages
            .iter()
            .position(|m| m.message_id == message_id)
        {
            if idx < start {
                return None;
            }
            let msg = &self.messages[idx];
            let affects_goal = message_affects_goal_projection(msg);
            let role = msg.role.clone();
            let tool_call_ids: Vec<String> = msg
                .tool_calls
                .as_ref()
                .map(|tcs| tcs.iter().map(|tc| tc.id.clone()).collect())
                .unwrap_or_default();

            self.messages.remove(idx);
            if affects_goal {
                self.rebuild_goal_projection_from_messages();
            }
            self.thread.previous_response_id = None;
            self.reset_cache_guard_snapshot();
            self.provider_usage_stale = true;
            self.emit(ChatEvent::MessageRemoved {
                message_id: message_id.to_string(),
            });

            if role == "assistant" && !tool_call_ids.is_empty() {
                let tool_indices: Vec<usize> = self
                    .messages
                    .iter()
                    .enumerate()
                    .skip(start)
                    .filter(|(_, m)| m.role == "tool" && tool_call_ids.contains(&m.tool_call_id))
                    .map(|(index, _)| index)
                    .collect();

                for tool_idx in tool_indices.into_iter().rev() {
                    let message = self.messages.remove(tool_idx);
                    self.emit(ChatEvent::MessageRemoved {
                        message_id: message.message_id,
                    });
                }
            }

            self.increment_version();
            self.touch();
            return Some(idx);
        }
        None
    }

    pub fn truncate_messages(&mut self, from_index: usize) {
        if from_index < self.messages.len() {
            self.messages.truncate(from_index);
            self.rebuild_goal_projection_from_messages();
            self.thread.previous_response_id = None;
            self.reset_cache_guard_snapshot();
            self.provider_usage_stale = true;
            self.emit(ChatEvent::MessagesTruncated { from_index });
            self.increment_version();
            self.touch();
        }
    }

    pub fn perform_skill_deactivation_cleanup(&mut self) {
        let Some(pending) = self.pending_skill_deactivation.take() else {
            return;
        };

        if pending.start_index > self.messages.len() {
            warn!(
                "Skill deactivation cleanup: start_index {} is beyond messages.len() {} for skill '{}', skipping compaction",
                pending.start_index, self.messages.len(), pending.skill_name
            );
            return;
        }

        let activation_tool_message =
            pending
                .activation_tool_call_id
                .as_ref()
                .and_then(|tool_id| {
                    self.messages
                        .iter()
                        .skip(pending.start_index)
                        .find(|msg| msg.role == "tool" && msg.tool_call_id == *tool_id)
                        .cloned()
                });

        if pending.start_index > self.messages.len() {
            warn!(
                "Skill deactivation cleanup: start_index {} is beyond messages.len() {} for skill '{}', skipping compaction",
                pending.start_index, self.messages.len(), pending.skill_name
            );
            return;
        }

        info!(
            "Skill deactivation cleanup: compacting messages from index {} for skill '{}'",
            pending.start_index, pending.skill_name
        );
        self.truncate_messages(pending.start_index);

        if let Some(tool_message) = activation_tool_message {
            self.add_message(tool_message);
        }

        let report_content = format!(
            "## Skill Report: {}\n\n✅ Skill '{}' executed successfully.\n\nHere is the compactified result. The full skill conversation was compactified and removed from the thread.\n\n{}",
            pending.skill_name,
            pending.skill_name,
            pending.report
        );
        let report_message = ChatMessage {
            role: "plain_text".to_string(),
            content: ChatContent::SimpleText(report_content),
            ..Default::default()
        };
        self.add_message(report_message);
    }
}
