use crate::{BuddyThreadMeta, TaskMeta, ThreadParams};
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
            map_legacy_mode_to_id(mode);
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
                serde_json::from_value::<TaskMeta>(task_meta_value.clone())
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
            serde_json::from_value::<Option<BuddyThreadMeta>>(v.clone())
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

pub fn map_legacy_mode_to_id(mode_str: &str) -> &str {
    match mode_str.to_uppercase().as_str() {
        "NO_TOOLS" => "explore",
        "EXPLORE" => "explore",
        "AGENT" => "agent",
        "CONFIGURE" => "configurator",
        "TASK_PLANNER" => "task_planner",
        "TASK_AGENT" => "task_agent",
        "BRAINSTORM" => "brainstorming",
        "BRAINSTORMING" => "brainstorming",
        _ => {
            if mode_str
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
            {
                mode_str
            } else {
                "agent"
            }
        }
    }
}
