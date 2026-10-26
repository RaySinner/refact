use super::*;

impl App {
    pub(super) fn apply_tui_config_content(&mut self, content: &str) {
        match KeymapRegistry::from_config_file_content(Some(content)) {
            Ok(keymap) => {
                self.vim.set_enabled(keymap.vim_mode_enabled());
                for warning in keymap.warnings() {
                    self.add_notice(format!("TUI keymap warning: {warning}"));
                }
                self.keymap = keymap;
            }
            Err(error) => self.add_notice(format!("Failed to load TUI keymap config: {error}")),
        }
        let theme_home = self.tui_theme_home();
        match TuiTheme::from_config_file_content_with_custom_dir(
            Some(content),
            theme_home.as_deref(),
        ) {
            Ok(theme) => self.set_theme(theme),
            Err(error) => self.add_notice(format!("Failed to load TUI theme config: {error}")),
        }
        match NotificationConfig::from_config_file_content(Some(content)) {
            Ok(config) => self.notifications.set_config(config),
            Err(error) => {
                self.add_notice(format!("Failed to load TUI notification config: {error}"))
            }
        }
    }

    pub(super) fn set_session_state(&mut self, state: SessionState) {
        let was_working = self.session_state.shows_working_indicator();
        let is_working = state.shows_working_indicator();
        self.session_state = state;
        match (was_working, is_working) {
            (false, true) => {
                self.working_started_at_ms = Some(now_ms());
                self.working_tick = 0;
                self.working_last_tick_at_ms = None;
                self.working_detail = self.latest_tool_detail();
            }
            (true, false) => self.clear_working_indicator(),
            _ => {}
        }
    }

    pub(super) fn clear_working_indicator(&mut self) {
        self.working_started_at_ms = None;
        self.working_tick = 0;
        self.working_last_tick_at_ms = None;
        self.working_detail = None;
    }

    pub(super) fn tick_working_indicator(&mut self) {
        if !self.session_state.shows_working_indicator() {
            return;
        }
        if self.working_started_at_ms.is_none() {
            self.working_started_at_ms = Some(now_ms());
        }
        let now = now_ms();
        if self.working_last_tick_at_ms.is_some_and(|last| {
            now.saturating_sub(last) < WORKING_ANIMATION_INTERVAL.as_millis() as u64
        }) {
            return;
        }
        self.working_last_tick_at_ms = Some(now);
        self.working_tick = self.working_tick.wrapping_add(1);
    }

    pub(super) fn set_working_detail(&mut self, detail: String) {
        if self.session_state.shows_working_indicator() && !detail.is_empty() {
            self.working_detail = Some(detail);
        }
    }

    pub(super) fn clear_stream_controllers(&mut self) {
        self.stream_controller.clear();
        self.reasoning_stream_active = false;
        self.plan_stream_controller = None;
        self.plan_stream_pushed.clear();
        self.stream_chunking_policy.reset();
    }

    pub(super) fn latest_tool_detail(&self) -> Option<String> {
        self.transcript.iter().rev().find_map(|item| match item {
            TranscriptItem::Tool(card) if card.status.is_active() => Some(card.summary()),
            _ => None,
        })
    }

    pub fn pending_history_insertions(&mut self, width: u16) -> Vec<HistoryInsertion> {
        self.history
            .drain_pending_capped(width, RESIZE_REFLOW_PENDING_CELL_CAP)
    }

    pub(super) fn resize_reflow_insertions(
        &mut self,
        width: u16,
        max_rows: usize,
    ) -> Vec<HistoryInsertion> {
        self.history.inserted_tail_insertions(width, max_rows)
    }

    pub fn note_terminal_resize(&mut self) {
        if self.native_scrollback {
            self.resize_reflow.schedule_debounced();
        }
    }

    pub(super) fn resize_reflow_pending(&self) -> bool {
        self.resize_reflow.has_pending_reflow()
    }

    pub(super) fn resize_reflow_is_due(&self) -> bool {
        self.resize_reflow.pending_is_due(Instant::now())
    }

    pub(super) fn resize_reflow_delay(&self) -> Option<Duration> {
        self.resize_reflow
            .pending_until()
            .and_then(|deadline| deadline.checked_duration_since(Instant::now()))
    }

    pub(super) fn finish_resize_reflow(&mut self) {
        self.resize_reflow.clear_pending_reflow();
    }

    pub(super) fn update_stream_width_for_terminal(&mut self, width: u16) {
        let should_sync_assistant =
            self.stream_controller.has_live_tail() || self.stream_controller.stable_lines_ready();
        let should_sync_plan = self
            .plan_stream_controller
            .as_ref()
            .is_some_and(|controller| {
                controller.has_live_tail() || controller.stable_lines_ready()
            });
        let should_sync = should_sync_assistant || should_sync_plan;
        if self.session_state != SessionState::Generating && !should_sync {
            return;
        }
        self.stream_controller
            .set_width(assistant_stream_width(width));
        if let Some(controller) = &mut self.plan_stream_controller {
            controller.set_width(assistant_stream_width(width));
        }
        if should_sync {
            if should_sync_assistant && self.native_scrollback {
                self.sync_assistant_stream_tail_item();
            } else if should_sync_assistant {
                self.sync_assistant_stream_item();
            }
            if should_sync_plan {
                self.sync_plan_stream_tail_item();
            }
        }
    }

    pub fn set_native_scrollback(&mut self, enabled: bool) {
        if enabled && !self.native_scrollback {
            let old_selected = self.selected_tool_index;
            let old_items = std::mem::take(&mut self.transcript);
            self.selected_tool_index = None;
            for (old_idx, item) in old_items.into_iter().enumerate() {
                if item.keeps_live() {
                    let new_idx = self.transcript.len();
                    self.push_live_item(item);
                    if old_selected == Some(old_idx) {
                        self.selected_tool_index = Some(new_idx);
                    }
                } else {
                    self.history.enqueue(item);
                }
            }
            self.mark_rendered_state_from_messages();
        }
        self.native_scrollback = enabled;
    }

    pub fn begin_frame_render(&mut self) {
        self.rendered_message_count = 0;
    }

    pub fn note_rendered_messages(&mut self, count: usize) {
        self.rendered_message_count = self.rendered_message_count.saturating_add(count);
    }

    pub(super) fn submit_structured_prompt(&mut self, prompt: &str) -> AppAction {
        self.composer.set_text(prompt);
        self.submit_composer().unwrap_or(AppAction::None)
    }

    pub(super) fn show_current_plan(&mut self) -> AppAction {
        self.composer.clear();
        match current_plan_cell_data(self.transcript_state.messages()) {
            Some(plan) => self.push_history_item(TranscriptItem::Plan(plan)),
            None => self.add_notice("No current plan is installed for this chat"),
        }
        AppAction::None
    }

    pub(super) fn switch_to_agent_mode(&mut self) -> AppAction {
        self.composer.clear();
        self.mode = Some("agent".to_string());
        self.pending_mode = None;
        self.add_notice("Switching to Agent mode");
        AppAction::SetParams {
            patch: workflow::agent_mode_patch(),
        }
    }

    pub fn execute_command_name(&mut self, name: &str) -> AppAction {
        let (name, args) = split_command_name_and_args(name);
        let Some(command) = command_by_name(name) else {
            self.add_notice(format!("/{name} is not registered"));
            return AppAction::None;
        };
        if !self.command_available(*command, name) {
            return AppAction::None;
        }
        match command.action {
            CommandAction::BackendCommand { command } => {
                if command == "stop" && self.is_chat_active() {
                    self.cancel_queue_edit();
                    self.abort_in_flight = true;
                    self.add_notice("Cancel requested");
                    AppAction::Abort
                } else {
                    self.composer.clear();
                    self.add_notice(format!(
                        "/{command} is only available while a response is running"
                    ));
                    AppAction::None
                }
            }
            CommandAction::OpenPicker { picker } => self.execute_picker_command(picker),
            CommandAction::LocalToggle { toggle } => self.execute_local_toggle(toggle, args),
            CommandAction::ShowInfo { topic } => {
                self.show_info_topic(topic);
                AppAction::None
            }
            CommandAction::Session { command } => self.execute_session_command(command, args),
            CommandAction::Workflow { command } => {
                if command == workflow::WorkflowCommand::ShowGoal {
                    self.execute_goal_command(args, self.goal_presentation().is_none())
                } else {
                    self.execute_workflow_command(command)
                }
            }
            CommandAction::Misc { command } => self.execute_misc_command(command, args),
            CommandAction::Unavailable { reason } => {
                self.composer.clear();
                self.add_notice(format!("/{name} unavailable: {reason}"));
                AppAction::None
            }
        }
    }

    pub(super) fn execute_workflow_command(
        &mut self,
        command: workflow::WorkflowCommand,
    ) -> AppAction {
        match command {
            workflow::WorkflowCommand::ShowPlan => self.show_current_plan(),
            workflow::WorkflowCommand::ShowGoal => {
                self.execute_goal_command("", self.goal_presentation().is_none())
            }
            workflow::WorkflowCommand::AgentMode => self.switch_to_agent_mode(),
            workflow::WorkflowCommand::GitDiff => {
                self.composer.clear();
                match self.current_project_root() {
                    Some(root) => AppAction::LoadDiff { root },
                    None => {
                        self.add_notice("No active project for /diff");
                        AppAction::None
                    }
                }
            }
            workflow::WorkflowCommand::ReviewPrompt => {
                self.submit_structured_prompt(workflow::review_prompt())
            }
            workflow::WorkflowCommand::CompactPrompt => {
                self.submit_structured_prompt(workflow::compact_prompt())
            }
        }
    }

    pub(super) fn execute_local_toggle(&mut self, toggle: LocalToggle, _args: &str) -> AppAction {
        match toggle {
            LocalToggle::ClearTranscript => {
                self.composer.clear();
                self.replace_with_notice("Transcript cleared".to_string());
                AppAction::None
            }
            LocalToggle::Events => {
                self.composer.clear();
                self.events_pane.toggle();
                AppAction::RefreshWorkers
            }
            LocalToggle::Quit => self.quit_action(),
        }
    }

    pub(super) fn show_info_topic(&mut self, topic: InfoTopic) {
        self.composer.clear();
        match topic {
            InfoTopic::Help => self.help_open = true,
            InfoTopic::Status => {
                self.show_status_card();
            }
        }
    }

    pub(super) fn show_debug_config_card(&mut self) {
        let config_path = self
            .tui_config_path
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "unavailable".to_string());
        self.push_history_item(TranscriptItem::Info(vec![
            "TUI debug config".to_string(),
            format!("Config: {config_path}"),
            format!("Theme: {}", self.theme.name()),
            format!(
                "Vim: {}",
                if self.vim.enabled() {
                    self.vim.mode().label()
                } else {
                    "off"
                }
            ),
            format!("Notifications: {}", self.notification_status_label()),
            format!(
                "Commands: {} registered",
                crate::commands::command_registry().len()
            ),
        ]));
    }

    pub(super) fn show_subagents_card(&mut self) {
        let summaries = self.subagent_summaries();
        if summaries.is_empty() {
            self.push_history_item(TranscriptItem::Info(vec![
                "Subagents".to_string(),
                "No active subagents".to_string(),
            ]));
            return;
        }
        let mut lines = vec!["Subagents".to_string()];
        for summary in summaries.iter().take(MAX_SUBCHAT_DEPTH) {
            lines.push(summary.detail());
        }
        if summaries.len() > MAX_SUBCHAT_DEPTH {
            lines.push(format!(
                "… {} more subagents",
                summaries.len() - MAX_SUBCHAT_DEPTH
            ));
        }
        self.push_history_item(TranscriptItem::Info(lines));
    }

    pub(super) fn subagent_summaries(&self) -> Vec<SubagentSummary> {
        let finished_tool_ids = self
            .transcript_state
            .messages()
            .iter()
            .filter(|message| message.role.is_tool_result())
            .filter_map(|message| message.tool_call_id.clone())
            .collect::<Vec<_>>();
        let mut summaries = Vec::<SubagentSummary>::new();
        for message in self.transcript_state.messages() {
            if message.role != TranscriptRole::Assistant {
                continue;
            }
            for tool in &message.tool_calls {
                let card = ToolCard::from_tool_call(tool);
                if card.subchat_log.is_empty() && card.attached_files.is_empty() {
                    continue;
                }
                let active = card.subchat_active && !finished_tool_ids.contains(&card.id);
                summaries.push(SubagentSummary {
                    tool_call_id: card.id,
                    tool_name: card.name,
                    progress: card.subchat_log.last().cloned(),
                    attached_files: card.attached_files.len(),
                    depth: card.subchat_depth,
                    active,
                    truncated: card.subchat_truncated,
                });
            }
        }
        for item in &self.transcript {
            let TranscriptItem::Tool(card) = item else {
                continue;
            };
            if card.subchat_log.is_empty() && card.attached_files.is_empty() {
                continue;
            }
            if summaries
                .iter()
                .any(|summary| summary.tool_call_id == card.id)
            {
                continue;
            }
            summaries.push(SubagentSummary {
                tool_call_id: card.id.clone(),
                tool_name: card.name.clone(),
                progress: card.subchat_log.last().cloned(),
                attached_files: card.attached_files.len(),
                depth: card.subchat_depth,
                active: card.subchat_active && card.status.is_active(),
                truncated: card.subchat_truncated,
            });
        }
        summaries
    }

    pub(super) fn submit_composer(&mut self) -> Option<AppAction> {
        if self.current_project.is_none() {
            return None;
        }
        if self.input_queue.is_editing() {
            return self.finish_queue_edit();
        }
        let mut prompt = self.composer.submit_text()?;
        if let Some(request) = self.pending_manual_ask_questions.take() {
            if !prompt.starts_with(&format!("[QA:{}]", request.tool_call_id)) {
                prompt = request.format_manual_reply(&prompt);
            }
            self.handled_ask_questions_tool_ids
                .insert(request.tool_call_id.clone());
        }
        self.persist_history();
        if self.backtrack_pending.is_some() {
            return Some(self.start_backtrack_turn(prompt));
        }
        let (params, correlation) = self.take_submit_params(&prompt);
        if self.is_chat_active() && self.session_state != SessionState::WaitingUserInput {
            self.enqueue_input(prompt, params);
            Some(AppAction::None)
        } else {
            Some(self.start_prompt_turn(prompt, params, correlation))
        }
    }

    pub(super) fn start_backtrack_turn(&mut self, prompt: String) -> AppAction {
        let Some(target) = self.backtrack_pending.take() else {
            return AppAction::None;
        };
        self.in_flight_send = None;
        self.pending_send_retry = None;
        if !self
            .transcript_state
            .messages()
            .get(target.index)
            .is_some_and(|message| target.matches(message))
        {
            self.add_notice("Backtrack target changed; open backtrack again");
            return AppAction::None;
        }
        self.pending_backtrack_rollback = Some(BacktrackRollback {
            transcript_state: self.transcript_state.clone(),
            session_state: self.session_state,
            usage: self.usage,
            selected_backtrack_index: self.selected_backtrack_index,
            backtrack_target: self.backtrack_target.clone(),
            backtrack_pending: Some(target.clone()),
            last_escape_at: self.last_escape_at,
            prompt: prompt.clone(),
        });
        self.transcript_state.truncate_messages(target.index);
        self.transcript_state.push_user_message(prompt.clone());
        self.transcript_state.start_assistant(None);
        self.rebuild_render_transcript_from_state();
        self.set_session_state(SessionState::Generating);
        self.clear_stream_controllers();
        self.usage = None;
        self.retry_hint = None;
        self.clear_backtrack_selection();
        AppAction::RetryFromIndex {
            index: target.index,
            content: Value::String(prompt),
        }
    }

    pub(super) fn start_prompt_turn(
        &mut self,
        prompt: String,
        params: Value,
        correlation: Option<ClientMessageCorrelation>,
    ) -> AppAction {
        let correlation = correlation.unwrap_or_else(ClientMessageCorrelation::new);
        self.in_flight_send = Some(InFlightSend {
            correlation: correlation.clone(),
            accepted: false,
        });
        self.cancel_backtrack();
        self.clear_active_ask_questions();
        self.transcript_state
            .push_optimistic_user_message(prompt.clone(), correlation.client_message_id.clone());
        self.transcript_state.start_assistant(None);
        self.rebuild_render_transcript_from_state();
        self.set_session_state(SessionState::Generating);
        self.clear_stream_controllers();
        self.usage = None;
        self.retry_hint = None;
        AppAction::SendMessage {
            prompt,
            params,
            correlation,
        }
    }

    pub(super) fn take_submit_params(
        &mut self,
        prompt: &str,
    ) -> (Value, Option<ClientMessageCorrelation>) {
        if self
            .pending_send_retry
            .as_ref()
            .is_some_and(|retry| retry.prompt == prompt)
        {
            return self
                .pending_send_retry
                .take()
                .map(|retry| (retry.params, Some(retry.correlation)))
                .unwrap_or_else(|| (Value::Object(Map::new()), None));
        }
        self.pending_send_retry = None;
        (self.take_pending_params(), None)
    }

    pub(super) fn dispatch_next_queued_input(&mut self) -> AppAction {
        if !self.input_queue_matches_current_session() {
            return AppAction::None;
        }
        let Some(QueuedInput { text, params, .. }) = self.input_queue.pop_next_ready() else {
            return AppAction::None;
        };
        self.start_prompt_turn(text, params, None)
    }

    pub(super) fn enqueue_input(&mut self, prompt: String, params: Value) {
        if self.input_queue.enqueue(prompt, params) {
            self.input_queue.clear_selection();
        } else {
            self.add_notice(format!(
                "Warning: input queue is full (maximum {INPUT_QUEUE_CAPACITY} prompts); prompt was not queued"
            ));
        }
    }

    pub(super) fn submit_ask_questions_reply(&mut self, prompt: String) -> AppAction {
        let (params, correlation) = self.take_submit_params(&prompt);
        self.start_prompt_turn(prompt, params, correlation)
    }

    pub(super) fn set_params_context(&mut self, patch: &Value) -> CommandContextTag {
        match self.pending_reasoning_rollback.take() {
            Some(pending) if pending.patch == *patch => CommandContextTag::Reasoning {
                previous: pending.previous,
            },
            Some(pending) => {
                self.pending_reasoning_rollback = Some(pending);
                CommandContextTag::Other
            }
            None => CommandContextTag::Other,
        }
    }

    pub(super) fn begin_queue_edit(&mut self) -> AppAction {
        let draft = self.composer.text().to_string();
        match self.input_queue.begin_edit_selected(draft) {
            Some(text) => {
                self.composer.set_text(text);
                AppAction::None
            }
            None => AppAction::None,
        }
    }

    pub(super) fn finish_queue_edit(&mut self) -> Option<AppAction> {
        let text = self.composer.submit_text()?;
        let draft = self.input_queue.finish_edit(text).unwrap_or_default();
        self.composer.set_text(draft);
        if self.is_chat_active() {
            Some(AppAction::None)
        } else {
            Some(self.dispatch_next_queued_input())
        }
    }

    pub(super) fn cancel_queue_edit(&mut self) -> bool {
        let Some(draft) = self.input_queue.cancel_edit() else {
            return false;
        };
        self.composer.set_text(draft);
        true
    }

    pub(super) fn remove_selected_queue_item(&mut self) -> AppAction {
        self.input_queue.remove_selected();
        AppAction::None
    }

    pub(super) fn handle_idle_escape(&mut self) -> AppAction {
        if !self.composer.is_empty() {
            return AppAction::None;
        }
        if self.selected_backtrack_index.is_some() {
            self.select_previous_user_message();
            return AppAction::None;
        }
        let now = Instant::now();
        if self
            .last_escape_at
            .is_some_and(|last| now.duration_since(last) < Duration::from_millis(900))
        {
            self.select_previous_user_message();
            self.last_escape_at = None;
        } else {
            self.last_escape_at = Some(now);
        }
        AppAction::None
    }

    pub(super) fn select_previous_user_message(&mut self) {
        let indexes = self.user_message_indexes();
        if indexes.is_empty() {
            self.add_notice("No previous user message to backtrack");
            self.clear_backtrack_selection();
            return;
        }
        let next = match self.selected_backtrack_index {
            Some(current) => indexes
                .iter()
                .copied()
                .rev()
                .find(|idx| *idx < current)
                .unwrap_or(current),
            None => *indexes.last().expect("indexes checked"),
        };
        self.selected_backtrack_index = Some(next);
        self.backtrack_target = self.backtrack_target_at(next);
        self.input_queue.clear_selection();
        self.selected_tool_index = None;
    }

    pub(super) fn accept_backtrack_selection(&mut self) -> AppAction {
        let Some(index) = self.selected_backtrack_index else {
            return AppAction::None;
        };
        let Some(target) = self.backtrack_target_at(index) else {
            self.clear_backtrack_selection();
            self.add_notice("Backtrack target is no longer available");
            return AppAction::None;
        };
        self.composer.set_text(target.content.clone());
        self.backtrack_target = Some(target.clone());
        self.backtrack_pending = Some(target);
        AppAction::None
    }

    pub(super) fn user_message_indexes(&self) -> Vec<usize> {
        self.transcript_state
            .messages()
            .iter()
            .enumerate()
            .filter_map(|(idx, message)| {
                (message.role == TranscriptRole::User && !message.content.trim().is_empty())
                    .then_some(idx)
            })
            .collect()
    }

    pub(super) fn backtrack_target_at(&self, index: usize) -> Option<BacktrackTarget> {
        let message = self.transcript_state.messages().get(index)?;
        if message.role != TranscriptRole::User || message.content.trim().is_empty() {
            return None;
        }
        Some(BacktrackTarget {
            index,
            message_id: message.message_id.clone(),
            content: message.content.clone(),
        })
    }

    pub(super) fn clear_backtrack_selection(&mut self) {
        self.selected_backtrack_index = None;
        self.backtrack_target = None;
        self.last_escape_at = None;
    }

    pub(super) fn cancel_backtrack(&mut self) {
        self.clear_backtrack_selection();
        self.backtrack_pending = None;
        self.pending_backtrack_rollback = None;
    }

    pub(super) fn restore_backtrack_rollback(&mut self, rollback: BacktrackRollback) {
        self.transcript_state = rollback.transcript_state;
        self.set_session_state(rollback.session_state);
        self.usage = rollback.usage;
        self.selected_backtrack_index = rollback.selected_backtrack_index;
        self.backtrack_target = rollback.backtrack_target;
        self.backtrack_pending = rollback.backtrack_pending;
        self.last_escape_at = rollback.last_escape_at;
        self.composer.set_text(rollback.prompt);
        self.clear_stream_controllers();
        self.rebuild_render_transcript_from_state();
    }

    pub(super) fn persist_history(&mut self) {
        if let Some(path) = &self.history_path {
            self.pending_history_save = Some(HistorySaveRequest {
                path: path.clone(),
                entries: self.composer.history_entries().to_vec(),
            });
        }
    }

    pub(super) fn take_pending_history_save(&mut self) -> Option<HistorySaveRequest> {
        if self.history_save_in_flight {
            return None;
        }
        let request = self.pending_history_save.take()?;
        self.history_save_in_flight = true;
        Some(request)
    }

    pub(super) fn handle_history_save_result(&mut self, result: Result<(), String>) {
        self.history_save_in_flight = false;
        match result {
            Ok(()) => self.history_failure_notified = false,
            Err(error) if !self.history_failure_notified => {
                self.history_failure_notified = true;
                self.add_notice(error);
            }
            Err(_) => {}
        }
    }

    pub(super) fn set_workers(&mut self, workers: Vec<WorkerInfo>) {
        self.update_current_worker_from_list(&workers);
        self.events_pane.set_workers(workers);
        self.refresh_activity_surface();
    }

    pub(super) fn push_daemon_event(&mut self, event: DaemonEventRecord) {
        self.update_current_worker_from_event(&event);
        self.events_pane.push_event(event);
    }

    pub(super) fn begin_subscription_attempt(&mut self) {
        self.subscription_status = SubscriptionStatus::Waking;
        self.daemon_online = true;
    }

    pub(super) fn record_chat_resubscribe(&mut self, message: &str) {
        self.subscription_status = SubscriptionStatus::Waking;
        self.daemon_online = true;
        self.retry_hint = retry_hint_from_message(message);
    }

    pub(super) fn record_chat_disconnected(
        &mut self,
        message: &str,
        unreachable: bool,
        auth_stale: bool,
    ) {
        if auth_stale {
            self.record_subscription_offline(
                "SSE authentication is stale; refresh daemon credentials, then retry the subscription",
            );
        } else if unreachable || worker_waking_message(message) {
            self.subscription_status = SubscriptionStatus::Waking;
            self.daemon_online = true;
        } else {
            self.subscription_status = SubscriptionStatus::Offline;
            self.daemon_online = false;
        }
        if !auth_stale {
            self.retry_hint = retry_hint_from_message(message);
        }
    }

    pub(super) fn record_subscription_exhausted(&mut self) {
        self.record_subscription_offline("SSE reconnect retries exhausted; retry the subscription");
    }

    pub(super) fn record_subscription_offline(&mut self, reason: &str) {
        self.subscription_status = SubscriptionStatus::Offline;
        self.daemon_online = false;
        self.set_session_state(SessionState::Error);
        self.retry_hint = Some(reason.to_string());
    }

    pub(super) fn handle_daemon_events_disconnected(&mut self, message: String, retrying: bool) {
        if retrying {
            self.add_notice(format!(
                "Daemon events disconnected: {message}; reconnecting with bounded backoff…"
            ));
        } else {
            self.add_notice(format!(
                "Daemon events disconnected: {message}; reconnect limit reached"
            ));
        }
    }

    pub(super) fn update_current_worker_from_list(&mut self, workers: &[WorkerInfo]) {
        let Some(project) = self.current_project.as_mut() else {
            return;
        };
        if let Some(worker) = workers
            .iter()
            .find(|worker| worker.project_id == project.project_id)
            .cloned()
        {
            project.worker = Some(worker);
        }
    }

    pub(super) fn update_current_worker_from_event(&mut self, event: &DaemonEventRecord) {
        let Some(project) = self.current_project.as_mut() else {
            return;
        };
        let Some(state) = workers::update_current_worker_from_event(project, event) else {
            return;
        };
        match state {
            "starting" => self.subscription_status = SubscriptionStatus::Waking,
            "ready" if self.subscription_status == SubscriptionStatus::Waking => {
                self.subscription_status = SubscriptionStatus::Online;
            }
            _ => {}
        }
    }

    pub(super) fn quit_action(&mut self) -> AppAction {
        let abort_active = self.is_chat_active();
        if abort_active {
            self.abort_in_flight = true;
        } else {
            self.clear_active_ask_questions();
            self.should_quit = true;
        }
        AppAction::Quit { abort_active }
    }

    #[cfg(test)]
    pub fn test_set_approval(&mut self, modal: ApprovalModalState) {
        self.clear_approvals();
        self.enqueue_approval(modal);
        self.set_session_state(SessionState::Paused);
    }

    #[cfg(test)]
    pub fn test_push_tool(&mut self, card: ToolCard) {
        self.push_live_item(TranscriptItem::Tool(card));
        self.selected_tool_index = Some(self.transcript.len() - 1);
    }

    #[cfg(test)]
    pub fn test_push_history_item(&mut self, item: TranscriptItem) {
        self.push_history_item(item);
    }

    #[cfg(test)]
    pub fn test_set_history_items(&mut self, items: Vec<TranscriptItem>) {
        self.transcript = items;
    }

    #[cfg(test)]
    pub fn test_set_daemon_status(
        &mut self,
        status: DaemonStatus,
        base_url: impl Into<String>,
        source: crate::client::DaemonUrlSource,
    ) {
        self.daemon_online = true;
        self.daemon_status = Some(status);
        self.daemon_base_url = Some(base_url.into());
        self.daemon_url_source = Some(source);
    }

    #[cfg(test)]
    pub fn test_apply_tui_config_content(&mut self, content: &str) {
        self.apply_tui_config_content(content);
    }

    #[cfg(test)]
    pub fn test_take_pending_notifications(&mut self) -> Vec<Vec<u8>> {
        self.take_pending_notifications()
    }

    #[cfg(test)]
    pub fn test_set_terminal_focus(&mut self, focused: bool) {
        self.set_terminal_focus(focused);
    }

    #[cfg(test)]
    pub fn test_set_notifications_config(&mut self, config: NotificationConfig) {
        self.notifications.set_config(config);
    }

    #[cfg(test)]
    pub fn test_set_keymap(&mut self, keymap: KeymapRegistry) {
        self.vim.set_enabled(keymap.vim_mode_enabled());
        self.keymap = keymap;
    }

    #[cfg(test)]
    pub fn test_set_tui_config_path(&mut self, path: PathBuf) {
        self.tui_config_path = Some(path);
    }

    #[cfg(test)]
    pub fn test_set_composer_text(&mut self, text: &str) {
        self.composer.set_text(text);
    }

    #[cfg(test)]
    pub fn test_set_recent_sessions(&mut self, items: Vec<PickerItem>) {
        self.set_recent_sessions(items);
    }

    #[cfg(test)]
    pub fn test_insert_paste(&mut self, text: &str) {
        self.composer.insert_paste(text);
    }

    #[cfg(test)]
    pub fn test_execute_command_name(&mut self, name: &str) -> AppAction {
        self.execute_command_name(name)
    }

    #[cfg(test)]
    pub fn test_set_thread_params(&mut self, params: Value) {
        self.thread_params = params;
    }

    pub fn test_show_task_board(&mut self, data: TaskBoardViewData) {
        self.show_task_board(data);
    }

    pub fn test_set_task_context(&mut self, task_id: Option<String>, mode: Option<String>) {
        self.task_id = task_id;
        self.mode = mode;
    }

    pub fn test_goal_overlay_open(&self) -> bool {
        self.goal_overlay_open
    }

    pub fn submit_browser_context_decision(
        &mut self,
        options: BrowserContextDecisionOptions,
    ) -> AppAction {
        let Some((decision, prompt)) = self.browser_state.take_context_prompt(options) else {
            return AppAction::None;
        };
        AppAction::SendBrowserContextDecision { decision, prompt }
    }
}
