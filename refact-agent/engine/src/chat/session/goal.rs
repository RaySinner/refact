use super::*;

pub(super) fn goal_event_subkind(message: &ChatMessage) -> Option<&str> {
    if message.role != crate::chat::internal_roles::EVENT_ROLE {
        return None;
    }
    message
        .extra
        .get("event")
        .and_then(|event| event.get("subkind"))
        .and_then(|subkind| subkind.as_str())
}

pub(super) fn is_goal_projection_event(message: &ChatMessage) -> bool {
    matches!(
        goal_event_subkind(message),
        Some("goal_delta" | "goal_pursuit")
    )
}

pub(super) fn message_affects_goal_projection(message: &ChatMessage) -> bool {
    message.role == GOAL_ROLE || is_goal_projection_event(message)
}

const GOAL_EVIDENCE_EXEMPT_TOOLS: &[&str] =
    &["sleep", "get_goal", "get_plan", "pause_goal", "snooze_goal"];

pub(super) fn tool_result_counts_as_goal_evidence(
    message: &ChatMessage,
    history: &[ChatMessage],
) -> bool {
    if message.tool_failed == Some(true) {
        return false;
    }
    if message.tool_call_id.is_empty() {
        return true;
    }
    let resolved_name = history.iter().rev().find_map(|prior| {
        prior
            .tool_calls
            .as_ref()?
            .iter()
            .find(|call| call.id == message.tool_call_id)
            .map(|call| call.function.name.clone())
    });
    match resolved_name {
        Some(name) => !GOAL_EVIDENCE_EXEMPT_TOOLS.contains(&name.as_str()),
        None => true,
    }
}

pub(super) fn goal_runtime_projection(
    goal: Option<&GoalSnapshot>,
) -> (bool, Option<GoalStatus>, u32, u64, u32) {
    goal.map(|goal| {
        (
            goal.active,
            Some(goal.status),
            goal.progress.turns_used,
            goal.progress.tokens_used,
            goal.progress.no_progress_turns,
        )
    })
    .unwrap_or((false, None, 0, 0, 0))
}

pub(super) fn apply_goal_runtime_projection(
    runtime: &mut RuntimeState,
    goal: Option<&GoalSnapshot>,
) {
    let (goal_active, goal_status, goal_turns_used, goal_tokens_used, goal_no_progress_turns) =
        goal_runtime_projection(goal);
    runtime.goal_active = goal_active;
    runtime.goal_status = goal_status;
    runtime.goal_turns_used = goal_turns_used;
    runtime.goal_tokens_used = goal_tokens_used;
    runtime.goal_no_progress_turns = goal_no_progress_turns;
}

pub(super) fn synthesized_goal_content(messages: &[ChatMessage], base: &ChatMessage) -> String {
    let base = base.content.content_text_only();
    let notes = messages
        .iter()
        .filter(|message| goal_event_subkind(message) == Some("goal_delta"))
        .map(|message| message.content.content_text_only())
        .collect::<Vec<_>>();
    if notes.is_empty() {
        base
    } else {
        format!("{base}\n\n---\n\n## Goal updates\n\n{}", notes.join("\n\n"))
    }
}

pub(super) fn goal_events_from_messages(messages: &[ChatMessage]) -> Vec<GoalEvent> {
    messages
        .iter()
        .filter_map(|message| {
            let subkind = goal_event_subkind(message)?;
            if !matches!(subkind, "goal_delta" | "goal_pursuit") {
                return None;
            }
            let payload = message
                .extra
                .get("event")
                .and_then(|event| event.get("payload"));
            let at_ms = payload
                .and_then(|payload| payload.get("at_ms"))
                .or_else(|| payload.and_then(|payload| payload.get("created_at_ms")))
                .and_then(|value| value.as_u64())
                .unwrap_or(0);
            Some(GoalEvent {
                at_ms,
                kind: subkind.to_string(),
                text: message.content.content_text_only(),
            })
        })
        .collect()
}

const MAX_GOAL_SNAPSHOT_EVENTS: usize = 100;

fn merge_goal_events(derived: Vec<GoalEvent>, extras: Option<Vec<GoalEvent>>) -> Vec<GoalEvent> {
    let mut merged = derived;
    for event in extras.unwrap_or_default() {
        let already_present = merged.iter().any(|existing| {
            existing.kind == event.kind
                && existing.at_ms == event.at_ms
                && existing.text == event.text
        });
        if !already_present {
            merged.push(event);
        }
    }
    if merged.len() > MAX_GOAL_SNAPSHOT_EVENTS {
        merged.drain(..merged.len() - MAX_GOAL_SNAPSHOT_EVENTS);
    }
    merged
}

pub(super) fn goal_budget_has_hard_limits(budget: &GoalBudget) -> bool {
    budget.max_turns.is_some_and(|limit| limit > 0)
        || budget.max_minutes.is_some_and(|limit| limit > 0)
        || budget.max_tokens.is_some_and(|limit| limit > 0)
        || budget.max_cost_cents.is_some_and(|limit| limit > 0)
        || budget.no_progress_turns.is_some_and(|limit| limit > 0)
}

pub(crate) fn goal_snapshot_from_messages(
    messages: &[ChatMessage],
    existing: Option<&GoalSnapshot>,
) -> Option<GoalSnapshot> {
    let active = match refact_core::active_context::active_context(messages) {
        Ok(active) => active,
        Err(_) => return existing.cloned(),
    };
    let messages = &active.messages;
    let (base_index, version, base) = messages
        .iter()
        .enumerate()
        .filter_map(|(index, message)| {
            crate::chat::goal_role::goal_version(message).map(|version| (index, version, message))
        })
        .max_by_key(|(index, version, _)| (*version, *index))?;
    let _ = base_index;
    let meta = base.extra.get("goal");
    let prior = existing.filter(|goal| goal.version == version);
    let has_prior = prior.is_some();
    let active = prior
        .map(|goal| goal.active)
        .or_else(|| {
            meta.and_then(|meta| meta.get("active"))
                .and_then(|value| value.as_bool())
        })
        .unwrap_or(true);
    let meta_status = meta
        .and_then(|meta| meta.get("status"))
        .and_then(|value| serde_json::from_value(value.clone()).ok());
    let status = prior
        .map(|goal| goal.status)
        .or(meta_status)
        .unwrap_or(if active {
            GoalStatus::Active
        } else {
            GoalStatus::Paused
        });
    let budget = meta
        .and_then(|meta| meta.get("budget"))
        .and_then(|value| serde_json::from_value(value.clone()).ok())
        .or_else(|| prior.map(|goal| goal.budget.clone()))
        .unwrap_or_default();
    let budget = budget.migrate_legacy_default_hard_limits();
    let progress = prior
        .map(|goal| goal.progress.clone())
        .or_else(|| {
            meta.and_then(|meta| meta.get("progress"))
                .and_then(|value| serde_json::from_value(value.clone()).ok())
        })
        .unwrap_or_default();
    let derived_events = goal_events_from_messages(messages);
    let extra_events: Option<Vec<GoalEvent>> =
        prior.map(|goal| goal.events.clone()).or_else(|| {
            meta.and_then(|meta| meta.get("events"))
                .and_then(|value| serde_json::from_value(value.clone()).ok())
        });
    let events = merge_goal_events(derived_events, extra_events);
    let mut snapshot = GoalSnapshot {
        content: synthesized_goal_content(messages, base),
        version,
        active,
        status,
        budget,
        progress,
        attempts: prior
            .map(|goal| goal.attempts.clone())
            .or_else(|| {
                meta.and_then(|meta| meta.get("attempts"))
                    .and_then(|value| serde_json::from_value(value.clone()).ok())
            })
            .unwrap_or_default(),
        events,
        criteria: prior
            .map(|goal| goal.criteria.clone())
            .or_else(|| {
                meta.and_then(|meta| meta.get("criteria"))
                    .and_then(|value| serde_json::from_value(value.clone()).ok())
            })
            .unwrap_or_default(),
        snoozed_until_ms: prior.and_then(|goal| goal.snoozed_until_ms),
        stop_reason: prior.and_then(|goal| goal.stop_reason.clone()),
        transferred_from: prior
            .and_then(|goal| goal.transferred_from.clone())
            .or_else(|| {
                meta.and_then(|meta| meta.get("transferred_from"))
                    .and_then(|value| value.as_str())
                    .map(str::to_string)
            }),
        transferred_to: prior
            .and_then(|goal| goal.transferred_to.clone())
            .or_else(|| {
                meta.and_then(|meta| meta.get("transferred_to"))
                    .and_then(|value| value.as_str())
                    .map(str::to_string)
            }),
    };
    if !has_prior
        && matches!(
            snapshot.status,
            GoalStatus::BudgetExhausted | GoalStatus::NoProgress
        )
        && !goal_budget_has_hard_limits(&snapshot.budget)
    {
        snapshot.status = if snapshot.active {
            GoalStatus::Active
        } else {
            GoalStatus::Paused
        };
    }
    Some(snapshot)
}

impl ChatSession {
    pub(crate) fn refresh_goal_runtime_mirror(&mut self) {
        let (goal_active, goal_status, goal_turns_used, goal_tokens_used, goal_no_progress_turns) =
            goal_runtime_projection(self.goal.as_ref());
        self.goal_active = goal_active;
        self.goal_status = goal_status;
        self.goal_turns_used = goal_turns_used;
        self.goal_tokens_used = goal_tokens_used;
        self.goal_no_progress_turns = goal_no_progress_turns;
        apply_goal_runtime_projection(&mut self.runtime, self.goal.as_ref());
    }

    pub(crate) fn set_goal_projection(&mut self, goal: Option<GoalSnapshot>) {
        self.goal = goal;
        self.refresh_goal_runtime_mirror();
    }

    pub fn goal_budget_exhausted(&self) -> bool {
        self.goal
            .as_ref()
            .is_some_and(GoalSnapshotBudgetExt::goal_budget_exhausted)
    }

    pub fn goal_can_pursue(&self) -> bool {
        self.goal
            .as_ref()
            .is_some_and(GoalSnapshotBudgetExt::goal_can_pursue)
    }

    pub fn goal_nudge_ready_at(&self, now_ms: u64) -> bool {
        self.goal
            .as_ref()
            .is_some_and(|goal| goal.goal_nudge_ready_at(now_ms))
    }

    pub fn goal_record_progress(&mut self, tokens: u64, made_progress: bool) -> bool {
        self.goal_record_progress_with_cost(tokens, made_progress, 0)
    }

    pub fn goal_record_progress_with_cost(
        &mut self,
        tokens: u64,
        made_progress: bool,
        cost_cents: u64,
    ) -> bool {
        let Some(goal) = self.goal.as_mut() else {
            return false;
        };
        goal.goal_record_progress(tokens, made_progress, cost_cents);
        self.goal_ledger_append(GoalLedgerOp::ProgressRecorded {
            tokens,
            made_progress,
            cost_cents,
        });
        self.mark_persisted_runtime_changed();
        self.emit_goal_status();
        true
    }

    pub fn goal_record_progress_from_usage(&mut self, usage: &ChatUsage) -> bool {
        let Some(goal) = self.goal.as_ref() else {
            return false;
        };
        let evidence = std::mem::take(&mut self.goal_turn_evidence);
        let made_progress =
            evidence || usage.completion_tokens as u64 >= goal.budget.no_progress_token_threshold;
        let cost_cents = usage
            .metering_usd
            .as_ref()
            .map(|metering| metering.total_usd)
            .filter(|usd| usd.is_finite() && *usd > 0.0)
            .map(|usd| (usd * 100.0).round() as u64)
            .unwrap_or(0);
        self.goal_record_progress_with_cost(usage.total_tokens as u64, made_progress, cost_cents)
    }

    pub fn goal_record_verifier_attempt(&mut self, tokens: u64) -> bool {
        let Some(goal) = self.goal.as_mut() else {
            return false;
        };
        goal.goal_record_verifier_attempt(tokens);
        self.goal_ledger_append(GoalLedgerOp::VerifierAttemptRecorded { tokens });
        self.mark_persisted_runtime_changed();
        self.emit_goal_status();
        true
    }

    pub fn goal_note_no_progress_turn(&mut self) -> bool {
        let Some(goal) = self.goal.as_mut() else {
            return false;
        };
        goal.goal_note_no_progress_turn();
        self.goal_ledger_append(GoalLedgerOp::NoProgressNoted);
        self.mark_persisted_runtime_changed();
        self.emit_goal_status();
        true
    }

    pub fn goal_record_nudge(&mut self, at_ms: u64) -> bool {
        let Some(goal) = self.goal.as_mut() else {
            return false;
        };
        goal.goal_record_nudge(at_ms);
        let tail_is_nudge = matches!(
            self.goal_ledger.last().map(|entry| &entry.op),
            Some(GoalLedgerOp::NudgeRecorded)
        );
        if tail_is_nudge {
            if let Some(entry) = self.goal_ledger.last_mut() {
                entry.at_ms = at_ms;
            }
        } else {
            self.goal_ledger_append_at(GoalLedgerOp::NudgeRecorded, at_ms);
        }
        self.mark_persisted_runtime_changed();
        self.emit_goal_status();
        true
    }

    /// Rewrites the trailing `goal_pursuit` nudge event in place instead of appending a
    /// new one, keeping the `count`/`last_at_ms` payload bounded across repeated nudges.
    ///
    /// Currently invoked only from the unit test below — no production caller exists, so
    /// repeated goal nudges are still appended one message per nudge. Reported rather
    /// than removed: wiring it up belongs with whatever owns goal-nudge emission.
    #[allow(dead_code)]
    pub(crate) fn coalesce_tail_goal_nudge_event(&mut self, at_ms: u64) -> bool {
        let is_tail_nudge = self.messages.last().is_some_and(|message| {
            goal_event_subkind(message) == Some("goal_pursuit")
                && message
                    .extra
                    .get("event")
                    .and_then(|event| event.get("payload"))
                    .and_then(|payload| payload.get("kind"))
                    .and_then(|value| value.as_str())
                    == Some("nudge")
        });
        if !is_tail_nudge {
            return false;
        }
        let message = {
            let Some(last) = self.messages.last_mut() else {
                return false;
            };
            let Some(payload) = last
                .extra
                .get_mut("event")
                .and_then(|event| event.get_mut("payload"))
                .and_then(|payload| payload.as_object_mut())
            else {
                return false;
            };
            let count = payload
                .get("count")
                .and_then(|value| value.as_u64())
                .unwrap_or(1);
            payload.insert("count".to_string(), json!(count + 1));
            payload.insert("last_at_ms".to_string(), json!(at_ms));
            last.clone()
        };
        self.emit(ChatEvent::MessageUpdated {
            message_id: message.message_id.clone(),
            message,
        });
        self.increment_version();
        self.touch();
        true
    }

    pub fn goal_reset_no_progress(&mut self) -> bool {
        let Some(goal) = self.goal.as_mut() else {
            return false;
        };
        let before = (
            goal.progress.no_progress_turns,
            goal.status,
            goal.snoozed_until_ms,
        );
        goal.goal_reset_no_progress();
        if before
            == (
                goal.progress.no_progress_turns,
                goal.status,
                goal.snoozed_until_ms,
            )
        {
            return false;
        }
        self.goal_ledger_append(GoalLedgerOp::ProgressReset {
            reason: "user_message".to_string(),
        });
        self.mark_persisted_runtime_changed();
        self.emit_goal_status();
        true
    }

    pub fn goal_set_status(&mut self, status: GoalStatus) -> bool {
        self.goal_set_status_reason(status, "")
    }

    pub fn goal_set_status_reason(&mut self, status: GoalStatus, reason: &str) -> bool {
        let Some(goal) = self.goal.as_mut() else {
            return false;
        };
        if goal.status == status {
            self.emit_goal_status();
            return true;
        }
        let from = goal.status;
        goal.status = status;
        if status == GoalStatus::Stopped {
            goal.stop_reason = (!reason.is_empty()).then(|| reason.to_string());
        } else if matches!(status, GoalStatus::Active | GoalStatus::Verifying) {
            goal.stop_reason = None;
        }
        self.goal_ledger_append(GoalLedgerOp::StatusChanged {
            from,
            to: status,
            reason: reason.to_string(),
        });
        self.mark_persisted_runtime_changed();
        self.emit_goal_status();
        true
    }

    pub fn goal_set_snooze(&mut self, until_ms: Option<u64>) -> bool {
        let Some(goal) = self.goal.as_mut() else {
            return false;
        };
        goal.snoozed_until_ms = until_ms;
        self.goal_ledger_append(GoalLedgerOp::SnoozeSet { until_ms });
        self.mark_persisted_runtime_changed();
        self.emit_goal_status();
        true
    }

    pub(crate) fn goal_ledger_append(&mut self, op: GoalLedgerOp) -> u64 {
        self.goal_ledger_append_at(op, epoch_ms_now())
    }

    pub(crate) fn goal_ledger_append_at(&mut self, op: GoalLedgerOp, at_ms: u64) -> u64 {
        let seq = self
            .goal_ledger
            .last()
            .map(|entry| entry.seq + 1)
            .unwrap_or(1);
        self.goal_ledger.push(GoalLedgerEntry { seq, at_ms, op });
        seq
    }

    pub(crate) fn goal_ledger_last_seq(&self) -> u64 {
        self.goal_ledger.last().map(|entry| entry.seq).unwrap_or(0)
    }

    pub(crate) fn goal_status_changed_since(&self, seq: u64) -> bool {
        refact_chat_api::status_changed_since(&self.goal_ledger, seq)
    }

    pub fn suppress_delivery_wakes(&mut self) {
        self.delivery_wake_sources.clear();
        for delivery in self
            .pending_deliveries
            .iter_mut()
            .chain(self.runner_pending_deliveries.iter_mut())
        {
            delivery.wake = false;
        }
        self.mark_persisted_runtime_changed();
    }

    pub fn purge_goal_generated_wakes(&mut self) -> usize {
        self.delivery_wake_sources.remove("goal");
        let mut suppressed = 0;
        for delivery in self
            .pending_deliveries
            .iter_mut()
            .chain(self.runner_pending_deliveries.iter_mut())
        {
            if delivery.wake
                && matches!(
                    delivery.source.as_str(),
                    "chat.goal_monitor" | "chat.goal_verifier"
                )
            {
                delivery.wake = false;
                suppressed += 1;
            }
        }
        let mut commands = Vec::new();
        self.command_queue.retain(|request| {
            let remove = matches!(request.command, ChatCommand::Regenerate {})
                && (request.client_request_id.starts_with("goal-nudge-")
                    || request
                        .client_request_id
                        .starts_with("goal-verifier-regenerate-"));
            if remove {
                commands.push(request.client_request_id.clone());
            }
            !remove
        });
        for id in &commands {
            self.clear_queue_timestamp(id);
        }
        self.mark_persisted_runtime_changed();
        suppressed + commands.len()
    }

    pub fn stop_goal_on_manual_abort(&mut self) -> bool {
        let should_stop = self.goal.as_ref().is_some_and(|goal| {
            goal.active
                && matches!(
                    goal.status,
                    GoalStatus::Active
                        | GoalStatus::Verifying
                        | GoalStatus::BudgetExhausted
                        | GoalStatus::NoProgress
                )
        });
        if !should_stop {
            return false;
        }
        self.goal_set_status_reason(GoalStatus::Stopped, "manual_abort");
        self.goal_stopped_by_abort = true;
        self.add_message(event(
            EventSubkind::GoalPursuit,
            "chat.session",
            json!({"kind": "stopped", "trigger": "manual_abort"}),
            "Goal pursuit stopped: chat aborted.".to_string(),
        ));
        true
    }

    pub fn reactivate_goal_stopped_by_manual_abort(&mut self) -> bool {
        if !self.goal_stopped_by_abort {
            return false;
        }
        self.goal_stopped_by_abort = false;
        let can_reactivate = self
            .goal
            .as_ref()
            .is_some_and(|goal| goal.active && goal.status == GoalStatus::Stopped);
        if !can_reactivate {
            return false;
        }
        self.goal_set_status(GoalStatus::Active);
        self.add_message(event(
            EventSubkind::GoalPursuit,
            "chat.session",
            json!({"kind": "resumed", "trigger": "manual_abort_recovery"}),
            "Goal pursuit resumed.".to_string(),
        ));
        true
    }

    pub fn clear_goal_stopped_by_abort_marker(&mut self) {
        self.goal_stopped_by_abort = false;
    }

    pub fn goal_push_attempt(&mut self, attempt: GoalAttempt) -> bool {
        let Some(goal) = self.goal.as_mut() else {
            return false;
        };
        goal.goal_push_attempt(attempt.clone());
        self.goal_ledger_append(GoalLedgerOp::AttemptPushed { attempt });
        self.mark_persisted_runtime_changed();
        self.emit_goal_status();
        true
    }

    pub fn goal_push_event(&mut self, event: GoalEvent) -> bool {
        let Some(goal) = self.goal.as_mut() else {
            return false;
        };
        goal.goal_push_event(event.clone());
        self.goal_ledger_append(GoalLedgerOp::EventPushed { event });
        self.mark_persisted_runtime_changed();
        self.emit_goal_status();
        true
    }

    pub(crate) fn rebuild_goal_projection_from_messages(&mut self) {
        let existing = self.goal.clone();
        self.goal = goal_snapshot_from_messages(&self.messages, existing.as_ref());
        self.refresh_goal_runtime_mirror();
        self.ensure_goal_ledger_installed();
    }

    pub(crate) fn ensure_goal_ledger_installed(&mut self) {
        let Some(goal) = self.goal.as_ref() else {
            return;
        };
        let version = goal.version;
        let already_installed = self.goal_ledger.iter().any(|entry| {
            matches!(&entry.op, GoalLedgerOp::Installed { version: installed, .. } if *installed == version)
        });
        if already_installed {
            return;
        }
        let (active, budget, criteria) = (goal.active, goal.budget.clone(), goal.criteria.clone());
        self.goal_ledger_append(GoalLedgerOp::Installed {
            version,
            active,
            budget,
            criteria,
        });
    }
}

impl ChatSession {
    pub fn install_plan(
        &mut self,
        mode: &str,
        body: &str,
    ) -> crate::chat::plan_role::PlanInstallReport {
        crate::chat::plan_role::install_plan(self, mode, body)
    }

    pub fn install_goal(
        &mut self,
        mode: &str,
        body: &str,
        active: bool,
        budget: GoalBudget,
    ) -> crate::chat::goal_role::GoalInstallReport {
        self.install_goal_with_criteria(mode, body, active, budget, Vec::new())
    }

    pub fn install_goal_with_criteria(
        &mut self,
        mode: &str,
        body: &str,
        active: bool,
        budget: GoalBudget,
        criteria: Vec<GoalCriterion>,
    ) -> crate::chat::goal_role::GoalInstallReport {
        if self
            .post_tool_side_effects
            .iter()
            .chain(
                self.pending_deliveries
                    .iter()
                    .flat_map(|delivery| delivery.messages.iter()),
            )
            .any(|message| message.role == GOAL_ROLE)
        {
            return crate::chat::goal_role::install_goal(self, mode, body, active, budget);
        }
        let installed_index = self.messages.len();
        let report = crate::chat::goal_role::install_goal(self, mode, body, active, budget);
        if self.messages.len() == installed_index {
            return report;
        }
        if !criteria.is_empty() {
            if let Some(message) = self
                .messages
                .iter_mut()
                .skip(installed_index)
                .rev()
                .find(|message| message.role == GOAL_ROLE)
            {
                if let Some(meta) = message
                    .extra
                    .get_mut("goal")
                    .and_then(|value| value.as_object_mut())
                {
                    meta.insert("criteria".to_string(), json!(criteria));
                }
            }
        }
        self.rebuild_goal_projection_from_messages();
        if !criteria.is_empty() {
            if let Some(goal) = self.goal.as_mut() {
                goal.criteria = criteria.clone();
            }
            self.goal_ledger_append(GoalLedgerOp::CriteriaSet { criteria });
        }
        self.goal_stopped_by_abort = false;
        self.emit_goal_status();
        report
    }
}
