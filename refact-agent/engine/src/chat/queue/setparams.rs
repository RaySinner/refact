use super::*;

pub fn apply_setparams_patch(
    thread: &mut ThreadParams,
    patch: &serde_json::Value,
) -> (bool, serde_json::Value) {
    let mut changed = false;
    let mut model_changed = false;
    let mut context_cap_changed = false;

    if let Some(model) = patch.get("model").and_then(|v| v.as_str()) {
        if thread.model != model {
            thread.model = model.to_string();
            // Clear provider-specific state that's invalid across models.
            // OpenAI Responses API previous_response_id is tied to a specific
            // model+endpoint; switching models makes it invalid.
            if thread.previous_response_id.is_some() {
                tracing::info!("Clearing previous_response_id on model switch");
                thread.previous_response_id = None;
            }
            changed = true;
            model_changed = true;
        }
    }
    if let Some(mode) = patch.get("mode").and_then(|v| v.as_str()) {
        let normalized_mode =
            crate::yaml_configs::customization_registry::map_legacy_mode_to_id(mode);
        if thread.mode != normalized_mode {
            thread.mode = normalized_mode.to_string();
            changed = true;
        }
    }
    if let Some(boost_val) = patch.get("boost_reasoning") {
        let new_boost = if boost_val.is_null() {
            None
        } else if let Some(boost) = boost_val.as_bool() {
            Some(boost)
        } else {
            thread.boost_reasoning
        };
        if thread.boost_reasoning != new_boost {
            thread.boost_reasoning = new_boost;
            changed = true;
        }
    }
    if let Some(effort_val) = patch.get("reasoning_effort") {
        let new_val = if effort_val.is_null() {
            None
        } else if let Some(effort) = effort_val.as_str() {
            if effort.is_empty() {
                None
            } else {
                Some(effort.to_string())
            }
        } else {
            thread.reasoning_effort.clone()
        };
        if thread.reasoning_effort != new_val {
            thread.reasoning_effort = new_val;
            changed = true;
        }
    }
    if let Some(budget_val) = patch.get("thinking_budget") {
        if budget_val.is_null() {
            if thread.thinking_budget.is_some() {
                thread.thinking_budget = None;
                changed = true;
            }
        } else if let Some(b) = budget_val.as_u64() {
            let new_val = Some(b as usize);
            if thread.thinking_budget != new_val {
                thread.thinking_budget = new_val;
                changed = true;
            }
        }
    }
    if let Some(temp_val) = patch.get("temperature") {
        if temp_val.is_null() {
            if thread.temperature.is_some() {
                thread.temperature = None;
                changed = true;
            }
        } else if let Some(t) = temp_val.as_f64() {
            let new_val = Some((t as f32).clamp(0.0, 2.0));
            if thread.temperature != new_val {
                thread.temperature = new_val;
                changed = true;
            }
        }
        // Invalid type (not null, not number) - ignore, keep current value
    }
    if let Some(freq_val) = patch.get("frequency_penalty") {
        if freq_val.is_null() {
            if thread.frequency_penalty.is_some() {
                thread.frequency_penalty = None;
                changed = true;
            }
        } else if let Some(f) = freq_val.as_f64() {
            let new_val = Some((f as f32).clamp(-2.0, 2.0));
            if thread.frequency_penalty != new_val {
                thread.frequency_penalty = new_val;
                changed = true;
            }
        }
        // Invalid type - ignore
    }
    if let Some(max_val) = patch.get("max_tokens") {
        if max_val.is_null() {
            if thread.max_tokens.is_some() {
                thread.max_tokens = None;
                changed = true;
            }
        } else if let Some(m) = max_val.as_u64() {
            let new_val = Some((m as usize).min(1_000_000));
            if thread.max_tokens != new_val {
                thread.max_tokens = new_val;
                changed = true;
            }
        }
        // Invalid type - ignore
    }
    if let Some(parallel_val) = patch.get("parallel_tool_calls") {
        if parallel_val.is_null() {
            if thread.parallel_tool_calls.is_some() {
                thread.parallel_tool_calls = None;
                changed = true;
            }
        } else if let Some(p) = parallel_val.as_bool() {
            let new_val = Some(p);
            if thread.parallel_tool_calls != new_val {
                thread.parallel_tool_calls = new_val;
                changed = true;
            }
        }
        // Invalid type - ignore
    }
    if let Some(tool_use) = patch.get("tool_use").and_then(|v| v.as_str()) {
        if thread.tool_use != tool_use {
            thread.tool_use = tool_use.to_string();
            changed = true;
        }
    }
    if let Some(cap) = patch.get("context_tokens_cap") {
        if cap.is_null() {
            if thread.context_tokens_cap.is_some() {
                thread.context_tokens_cap = None;
                changed = true;
                context_cap_changed = true;
            }
        } else if let Some(n) = cap.as_u64() {
            let new_cap = Some(n as usize);
            if thread.context_tokens_cap != new_cap {
                thread.context_tokens_cap = new_cap;
                changed = true;
                context_cap_changed = true;
            }
        }
        // Invalid type (not null, not number) - ignore, keep current value
    } else if model_changed {
        if thread.context_tokens_cap.is_some() {
            thread.context_tokens_cap = None;
            changed = true;
            context_cap_changed = true;
        }
    }
    let mut pending_auto_compression_cap_reset = false;

    if patch.get("auto_compression_cap").is_none() {
        if model_changed || context_cap_changed {
            pending_auto_compression_cap_reset = true;
        }
    }

    if let Some(cap) = patch.get("auto_compression_cap") {
        // A user-supplied value (including null/zero) settles initialization.
        if (cap.is_null() || cap.as_u64().is_some()) && thread.auto_compression_cap_pending {
            thread.auto_compression_cap_pending = false;
            changed = true;
        }
        // Null is an explicit reset; defer recomputation until the model is known.
        // Invalid patches must not change initialization provenance.
        if cap.is_null() || cap.as_u64().is_some() {
            let pending = cap.is_null();
            if thread.auto_compression_cap_pending != pending {
                thread.auto_compression_cap_pending = pending;
                changed = true;
            }
        }
        if cap.is_null() {
            if thread.auto_compression_cap.is_some() {
                thread.auto_compression_cap = None;
                changed = true;
            }
        } else if let Some(n) = cap.as_u64() {
            let new_cap = Some(n as usize);
            if thread.auto_compression_cap != new_cap {
                thread.auto_compression_cap = new_cap;
                changed = true;
            }
        }
    }
    if pending_auto_compression_cap_reset {
        thread.auto_compression_cap = None;
        thread.auto_compression_cap_pending = true;
        changed = true;
    }

    if let Some(include) = patch.get("include_project_info").and_then(|v| v.as_bool()) {
        if thread.include_project_info != include {
            thread.include_project_info = include;
            changed = true;
        }
    }
    if let Some(enabled) = patch.get("checkpoints_enabled").and_then(|v| v.as_bool()) {
        if thread.checkpoints_enabled != enabled {
            thread.checkpoints_enabled = enabled;
            changed = true;
        }
    }
    if let Some(val) = patch
        .get("auto_approve_editing_tools")
        .and_then(|v| v.as_bool())
    {
        if thread.auto_approve_editing_tools != val {
            thread.auto_approve_editing_tools = val;
            changed = true;
        }
    }
    if let Some(val) = patch
        .get("auto_approve_dangerous_commands")
        .and_then(|v| v.as_bool())
    {
        if thread.auto_approve_dangerous_commands != val {
            thread.auto_approve_dangerous_commands = val;
            changed = true;
        }
    }
    if let Some(val) = patch.get("autonomous_no_confirm").and_then(|v| v.as_bool()) {
        if thread.autonomous_no_confirm != val {
            thread.autonomous_no_confirm = val;
            changed = true;
        }
    }
    if let Some(val) = patch.get("auto_enrichment_enabled") {
        if val.is_null() {
            if thread.auto_enrichment_enabled.is_some() {
                thread.auto_enrichment_enabled = None;
                changed = true;
            }
        } else if let Some(b) = val.as_bool() {
            let new_val = Some(b);
            if thread.auto_enrichment_enabled != new_val {
                thread.auto_enrichment_enabled = new_val;
                changed = true;
            }
        }
    }
    if let Some(val) = patch.get("auto_compact_enabled") {
        if val.is_null() {
            if thread.auto_compact_enabled.is_some() {
                thread.auto_compact_enabled = None;
                changed = true;
            }
        } else if let Some(b) = val.as_bool() {
            let new_val = Some(b);
            if thread.auto_compact_enabled != new_val {
                thread.auto_compact_enabled = new_val;
                changed = true;
            }
        }
    }
    if let Some(task_meta_value) = patch.get("task_meta") {
        if !task_meta_value.is_null() {
            if let Ok(task_meta) =
                serde_json::from_value::<crate::chat::types::TaskMeta>(task_meta_value.clone())
            {
                let new_task_meta = Some(task_meta);
                if thread.task_meta != new_task_meta {
                    thread.task_meta = new_task_meta;
                    changed = true;
                }
            }
        }
    }
    if let Some(v) = patch.get("buddy_meta") {
        if let Ok(meta) =
            serde_json::from_value::<Option<refact_buddy_core::types::BuddyThreadMeta>>(v.clone())
        {
            thread.buddy_meta = meta;
            changed = true;
        }
    }
    if let Some(parent_id) = patch.get("parent_id").and_then(|v| v.as_str()) {
        let new_val = if parent_id.is_empty() {
            None
        } else {
            Some(parent_id.to_string())
        };
        if thread.parent_id != new_val {
            thread.parent_id = new_val;
            changed = true;
        }
    }
    if let Some(link_type) = patch.get("link_type").and_then(|v| v.as_str()) {
        let new_val = if link_type.is_empty() {
            None
        } else {
            Some(link_type.to_string())
        };
        if thread.link_type != new_val {
            thread.link_type = new_val;
            changed = true;
        }
    }
    if let Some(root_chat_id) = patch.get("root_chat_id").and_then(|v| v.as_str()) {
        let new_val = if root_chat_id.is_empty() {
            None
        } else {
            Some(root_chat_id.to_string())
        };
        if thread.root_chat_id != new_val {
            thread.root_chat_id = new_val;
            changed = true;
        }
    }

    if let Some(worktree_val) = patch.get("worktree") {
        if worktree_val.is_null() && thread.worktree.is_some() {
            thread.worktree = None;
            changed = true;
        }
    }

    let invalidated_cap = (model_changed || context_cap_changed)
        && patch.get("auto_compression_cap").is_none()
        && thread.auto_compression_cap.is_some();
    if invalidated_cap {
        thread.auto_compression_cap = None;
        thread.auto_compression_cap_pending = true;
        changed = true;
    }

    let mut sanitized_patch = patch.clone();
    if pending_auto_compression_cap_reset {
        if let Some(obj) = sanitized_patch.as_object_mut() {
            obj.insert("auto_compression_cap".to_string(), serde_json::Value::Null);
        }
    }
    if let Some(obj) = sanitized_patch.as_object_mut() {
        obj.remove("type");
        obj.remove("chat_id");
        obj.remove("seq");
        obj.remove("worktree_id");
        if patch.get("mode").and_then(|v| v.as_str()).is_some() {
            obj.insert("mode".to_string(), serde_json::json!(thread.mode));
        }
        if let Some(worktree_val) = patch.get("worktree") {
            if worktree_val.is_null() {
                obj.insert("worktree".to_string(), serde_json::Value::Null);
            } else {
                obj.remove("worktree");
            }
        }
        if invalidated_cap {
            obj.insert("auto_compression_cap".to_string(), serde_json::Value::Null);
        }
    }

    (changed, sanitized_patch)
}

pub(crate) const MODE_SWITCH_TOOL_NAMES_LIMIT: usize = 20;

pub(crate) fn mode_switch_bool_delta(from: bool, to: bool) -> serde_json::Value {
    serde_json::json!({
        "from": from,
        "to": to,
        "changed": from != to,
    })
}

pub(crate) fn mode_switch_tool_delta(from: &BTreeSet<String>, to: &BTreeSet<String>) -> serde_json::Value {
    let names: Vec<_> = to
        .difference(from)
        .take(MODE_SWITCH_TOOL_NAMES_LIMIT)
        .cloned()
        .collect();
    let count = to.difference(from).count();
    serde_json::json!({
        "count": count,
        "names": names,
        "truncated": count > MODE_SWITCH_TOOL_NAMES_LIMIT,
    })
}

pub(crate) fn tool_confirm_changed(from: &ModeConfig, to: &ModeConfig) -> bool {
    from.tool_confirm.rules.len() != to.tool_confirm.rules.len()
        || from
            .tool_confirm
            .rules
            .iter()
            .zip(&to.tool_confirm.rules)
            .any(|(from, to)| from.match_pattern != to.match_pattern || from.action != to.action)
}

pub(crate) fn mode_switch_diff(from: &ModeConfig, to: &ModeConfig) -> serde_json::Value {
    let from_tools: BTreeSet<_> = from.tools.iter().cloned().collect();
    let to_tools: BTreeSet<_> = to.tools.iter().cloned().collect();
    let tool_confirm_changed = tool_confirm_changed(from, to);
    serde_json::json!({
        "resolved": true,
        "tools": {
            "added": mode_switch_tool_delta(&from_tools, &to_tools),
            "removed": mode_switch_tool_delta(&to_tools, &from_tools),
        },
        "system_prompt": {
            "changed": from.prompt != to.prompt,
        },
        "permissions": {
            "allow_integrations": mode_switch_bool_delta(
                from.allow_integrations,
                to.allow_integrations,
            ),
            "allow_mcp": mode_switch_bool_delta(from.allow_mcp, to.allow_mcp),
            "allow_subagents": mode_switch_bool_delta(
                from.allow_subagents,
                to.allow_subagents,
            ),
        },
        "tool_confirm": {
            "changed": tool_confirm_changed,
            "from_rules_count": from.tool_confirm.rules.len(),
            "to_rules_count": to.tool_confirm.rules.len(),
        },
        "thread_defaults": {
            "auto_approve_editing_tools": mode_switch_bool_delta(
                from.thread_defaults.auto_approve_editing_tools.unwrap_or(false),
                to.thread_defaults.auto_approve_editing_tools.unwrap_or(false),
            ),
            "auto_approve_dangerous_commands": mode_switch_bool_delta(
                from.thread_defaults.auto_approve_dangerous_commands.unwrap_or(false),
                to.thread_defaults.auto_approve_dangerous_commands.unwrap_or(false),
            ),
        },
    })
}

pub(crate) async fn add_mode_switch_event_and_plan_if_changed(
    app: AppState,
    session: &mut ChatSession,
    old_mode: &str,
    reason: Option<&str>,
    source: &str,
) -> bool {
    let new_mode = session.thread.mode.clone();
    if new_mode == old_mode {
        return false;
    }

    let model_id = if session.thread.model.is_empty() {
        None
    } else {
        Some(session.thread.model.as_str())
    };
    let from_config = get_mode_config(app.gcx.clone(), old_mode, model_id).await;
    let to_config = get_mode_config(app.gcx.clone(), &new_mode, model_id).await;
    let diff = match (&from_config, &to_config) {
        (Some(from), Some(to)) => mode_switch_diff(from, to),
        _ => serde_json::json!({"resolved": false}),
    };
    session.add_message(mode_switch_event_with_diff(
        source, old_mode, &new_mode, reason, diff,
    ));

    if plan_role::try_current_base_plan(session).is_ok_and(|plan| plan.is_none()) {
        if let Some(mode_config) = to_config {
            let plan_template = mode_config.plan_template.trim();
            if !plan_template.is_empty() {
                let rendered = crate::chat::prompts::render_mode_plan_template(
                    app,
                    plan_template,
                    &mode_config.title,
                    session.thread.include_project_info,
                    &session.thread.task_meta,
                )
                .await;
                if !rendered.trim().is_empty() {
                    session.install_plan(&new_mode, rendered.trim());
                }
            }
        }
    }

    true
}

pub(crate) fn epoch_ms_now() -> u64 {
    Utc::now().timestamp_millis().max(0) as u64
}

pub(crate) fn goal_exists_including_pending(session: &ChatSession) -> bool {
    session.goal.is_some()
        || crate::chat::goal_role::current_base_goal(session).is_some()
        || session
            .post_tool_side_effects
            .iter()
            .any(|message| message.role == internal_roles::GOAL_ROLE)
}

pub(crate) fn goal_base_exists_including_pending(session: &ChatSession) -> bool {
    crate::chat::goal_role::current_base_goal(session).is_some()
        || session
            .post_tool_side_effects
            .iter()
            .any(|message| message.role == internal_roles::GOAL_ROLE)
}

pub(crate) fn goal_delta_count_including_pending(session: &ChatSession) -> usize {
    crate::chat::goal_role::goal_delta_events(&session.accepted_control_projection()).len()
}

pub(crate) fn update_goal_result(
    seq: usize,
    truncation: Option<internal_roles::PlanDeltaTruncation>,
) -> serde_json::Value {
    let Some(truncation) = truncation else {
        return serde_json::json!({"seq": seq, "truncated": false});
    };
    serde_json::json!({
        "seq": seq,
        "truncated": true,
        "original_chars": truncation.original_chars,
        "kept_chars": truncation.kept_chars,
    })
}

pub(crate) fn handle_set_goal_command(
    session: &mut ChatSession,
    content: String,
    budget: Option<GoalBudget>,
    criteria: Option<Vec<GoalCriterion>>,
) -> Result<serde_json::Value, String> {
    if content.trim().is_empty() {
        return Err("argument `content` must be non-empty".to_string());
    }
    if goal_exists_including_pending(session) {
        return Err("goal already exists; use update_goal".to_string());
    }
    let current_mode =
        crate::yaml_configs::customization_registry::map_legacy_mode_to_id(&session.thread.mode)
            .to_string();
    let budget = match budget {
        Some(mut budget) => {
            budget.explicit = true;
            budget
        }
        None => GoalBudget::default(),
    };
    let report = session.install_goal_with_criteria(
        &current_mode,
        &content,
        true,
        budget,
        criteria.unwrap_or_default(),
    );
    session.add_message(internal_roles::event(
        EventSubkind::SystemNotice,
        "chat.command.set_goal",
        serde_json::json!({"version": report.version}),
        format!("Goal updated to v{}", report.version),
    ));
    Ok(serde_json::json!({
        "version": report.version,
        "supersedes": report.supersedes,
    }))
}

pub(crate) fn handle_set_goal_budget_command(
    session: &mut ChatSession,
    mut budget: GoalBudget,
) -> Result<serde_json::Value, String> {
    if session.goal.is_none() {
        return Err("no goal to set budget for; call set_goal first".to_string());
    }

    let view = refact_core::active_context::active_context(&session.messages)
        .map_err(|e| e.to_string())?;
    let mut messages = view.messages.clone();
    let Some((base_index, _, _)) = messages
        .iter()
        .enumerate()
        .filter_map(|(index, message)| {
            crate::chat::goal_role::goal_version(message).map(|version| (index, version, message))
        })
        .max_by_key(|(index, version, _)| (*version, *index))
    else {
        return Err("no goal to set budget for; call set_goal first".to_string());
    };

    budget.explicit = true;
    let serialized_budget = serde_json::to_value(&budget)
        .map_err(|error| format!("failed to serialize goal budget: {error}"))?;
    let goal_meta = messages[base_index]
        .extra
        .get_mut("goal")
        .and_then(|value| value.as_object_mut())
        .ok_or_else(|| "no goal to set budget for; call set_goal first".to_string())?;
    goal_meta.insert("budget".to_string(), serialized_budget.clone());

    session.messages =
        refact_core::active_context::writeback_active_context(&session.messages, &view, &messages)
            .map_err(|e| e.to_string())?;
    session.rebuild_goal_projection_from_messages();
    session.goal_ledger_append(GoalLedgerOp::BudgetSet {
        budget: budget.clone(),
    });
    let budget_status_change = session.goal.as_ref().and_then(|goal| {
        match goal.goal_budget_exhaustion_status_at(epoch_ms_now()) {
            Some(status) if goal.active && goal.status != status => Some(status),
            None if matches!(
                goal.status,
                GoalStatus::BudgetExhausted | GoalStatus::NoProgress
            ) =>
            {
                Some(if goal.active {
                    GoalStatus::Active
                } else {
                    GoalStatus::Paused
                })
            }
            _ => None,
        }
    });
    if let Some(status) = budget_status_change {
        session.goal_set_status_reason(status, "budget_set");
    }
    session.mark_persisted_runtime_changed();
    session.emit_goal_status();

    Ok(serde_json::json!({ "budget": serialized_budget }))
}

pub(crate) fn handle_update_goal_command(
    session: &mut ChatSession,
    note: String,
) -> Result<serde_json::Value, String> {
    if note.trim().is_empty() {
        return Err("argument `note` must be non-empty".to_string());
    }
    if !goal_base_exists_including_pending(session) {
        return Err("no goal to update; call set_goal first".to_string());
    }
    let seq = goal_delta_count_including_pending(session) + 1;
    let (delta, truncation) = internal_roles::goal_delta_with_truncation(
        "chat.command.update_goal",
        serde_json::json!({"seq": seq, "at_ms": epoch_ms_now()}),
        note,
    );
    session.add_message(delta);
    session.emit_goal_status();
    Ok(update_goal_result(seq, truncation))
}

pub(crate) fn handle_goal_control_command(
    session: &mut ChatSession,
    action: String,
) -> Result<serde_json::Value, String> {
    if session.goal.is_none() {
        return Err("no goal to control; call set_goal first".to_string());
    }
    let normalized = action.trim().to_ascii_lowercase();
    let status = match normalized.as_str() {
        "pause" => GoalStatus::Paused,
        "resume" => GoalStatus::Active,
        "stop" => GoalStatus::Stopped,
        _ => return Err("goal_control action must be pause, resume, or stop".to_string()),
    };
    if normalized == "resume"
        && !matches!(
            session.goal_status,
            Some(GoalStatus::Paused | GoalStatus::Stopped)
        )
    {
        return Err("goal_control resume requires a paused or stopped goal".to_string());
    }
    session.clear_goal_stopped_by_abort_marker();
    if matches!(status, GoalStatus::Paused | GoalStatus::Stopped) {
        session.purge_goal_generated_wakes();
    }
    session.goal_set_status_reason(status, "goal_control");
    if status == GoalStatus::Stopped {
        session.add_message(internal_roles::event(
            EventSubkind::GoalPursuit,
            "chat.command.goal_control",
            serde_json::json!({"kind": "stopped", "trigger": "goal_control", "at_ms": epoch_ms_now()}),
            "Goal pursuit stopped by user.".to_string(),
        ));
    }
    Ok(serde_json::json!({
        "action": normalized,
        "status": status,
    }))
}

pub(crate) fn emit_goal_command_error(session: &mut ChatSession, error: String) {
    let event = session.runtime_update_event(
        SessionState::Error,
        Some(error),
        session.is_compressing,
        session.compression_phase,
        session.compression_reason,
    );
    session.emit(event);
}

#[derive(Clone)]
pub struct WorktreeSetParamsUpdate {
    pub worktree: Option<WorktreeMeta>,
    pub previous_worktree: Option<WorktreeMeta>,
    pub changed: bool,
    pub sse_value: serde_json::Value,
}

pub(crate) fn reference_for_thread(
    chat_id: &str,
    thread: &ThreadParams,
    worktree_kind: &str,
) -> WorktreeReference {
    let task_meta = thread.task_meta.as_ref();
    WorktreeReference {
        kind: worktree_kind.to_string(),
        chat_id: Some(chat_id.to_string()),
        task_id: task_meta.map(|meta| meta.task_id.clone()),
        card_id: task_meta.and_then(|meta| meta.card_id.clone()),
        agent_id: task_meta.and_then(|meta| meta.agent_id.clone()),
    }
}

pub(crate) async fn worktree_service_from_gcx(
    app: AppState,
    requested_source_root: Option<&std::path::Path>,
) -> Result<WorktreeService, String> {
    let cache_dir = app.paths.cache_dir.clone();
    let project_dirs = get_project_dirs(app.gcx.clone()).await;
    if project_dirs.is_empty() {
        return Err("No project root available".to_string());
    }
    let source_root = match requested_source_root {
        Some(requested) => {
            let requested = tokio::fs::canonicalize(requested).await.map_err(|e| {
                format!(
                    "Failed to resolve worktree source root '{}': {}",
                    requested.display(),
                    e
                )
            })?;
            let requested = dunce::simplified(&requested).to_path_buf();
            let mut matches = false;
            for dir in &project_dirs {
                if tokio::fs::canonicalize(dir)
                    .await
                    .map(|canonical| dunce::simplified(&canonical).to_path_buf() == requested)
                    .unwrap_or(false)
                {
                    matches = true;
                    break;
                }
            }
            if !matches {
                return Err("Worktree source root is not a current workspace directory".to_string());
            }
            requested
        }
        None => project_dirs[0].clone(),
    };
    WorktreeService::new_async(cache_dir, source_root).await
}

pub(crate) async fn remove_thread_reference(
    app: AppState,
    chat_id: &str,
    thread: &ThreadParams,
    worktree: &WorktreeMeta,
) {
    let reference = reference_for_thread(chat_id, thread, &worktree.kind);
    let Ok(service) = worktree_service_from_gcx(app, Some(&worktree.source_workspace_root)).await
    else {
        warn!(
            "Failed to resolve worktree service while detaching '{}'",
            worktree.id
        );
        return;
    };
    if let Err(e) = service.remove_reference(&worktree.id, &reference).await {
        warn!(
            "Failed to remove worktree reference '{}': {}",
            worktree.id, e
        );
    }
}

pub(crate) async fn add_thread_worktree_reference(
    app: AppState,
    chat_id: &str,
    thread: &ThreadParams,
    worktree: &WorktreeMeta,
) -> Option<WorktreeMeta> {
    let service = match worktree_service_from_gcx(app, Some(&worktree.source_workspace_root)).await
    {
        Ok(service) => service,
        Err(e) => {
            warn!(
                "Failed to resolve worktree service while preserving '{}': {}",
                worktree.id, e
            );
            return None;
        }
    };
    let reference = reference_for_thread(chat_id, thread, &worktree.kind);
    match service.add_reference(&worktree.id, reference).await {
        Ok(view) => Some(view.meta),
        Err(e) => {
            warn!(
                "Failed to add worktree reference '{}' for chat '{}': {}",
                worktree.id, chat_id, e
            );
            None
        }
    }
}

pub async fn resolve_worktree_setparams_update(
    app: AppState,
    chat_id: &str,
    thread: &ThreadParams,
    patch: &serde_json::Value,
) -> Result<Option<WorktreeSetParamsUpdate>, String> {
    if let Some(worktree_id) = patch.get("worktree_id") {
        let worktree_id = worktree_id
            .as_str()
            .filter(|id| !id.trim().is_empty())
            .ok_or_else(|| "worktree_id must be a non-empty string".to_string())?;
        if thread
            .worktree
            .as_ref()
            .map(|worktree| worktree.id.as_str())
            == Some(worktree_id)
        {
            return Ok(None);
        }
        let service = worktree_service_from_gcx(app.clone(), None).await?;
        let view = service.get_worktree(worktree_id).await?;
        let reference = reference_for_thread(chat_id, thread, &view.meta.kind);
        let view = service.add_reference(worktree_id, reference).await?;
        if let Some(old) = thread
            .worktree
            .as_ref()
            .filter(|old| old.id != view.meta.id)
        {
            remove_thread_reference(app, chat_id, thread, old).await;
        }
        let changed = thread
            .worktree
            .as_ref()
            .map(|worktree| worktree.id.as_str())
            != Some(view.meta.id.as_str());
        return Ok(Some(WorktreeSetParamsUpdate {
            worktree: Some(view.meta.clone()),
            previous_worktree: thread.worktree.clone(),
            changed,
            sse_value: serde_json::to_value(view.meta).unwrap_or(serde_json::Value::Null),
        }));
    }

    if patch.get("worktree").map_or(false, |value| value.is_null()) {
        if let Some(old) = thread.worktree.as_ref() {
            remove_thread_reference(app, chat_id, thread, old).await;
        }
        return Ok(Some(WorktreeSetParamsUpdate {
            worktree: None,
            previous_worktree: thread.worktree.clone(),
            changed: thread.worktree.is_some(),
            sse_value: serde_json::Value::Null,
        }));
    }

    Ok(None)
}
