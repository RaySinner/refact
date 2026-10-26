use super::*;

pub(super) const MEMORY_KEY_SAVE_INSTRUCTION: &str = concat!(
    "When saving findings with buddy_memory_create, ",
    "set source_id exactly to this key."
);

pub(super) const AUTONOMOUS_SUCCESS_XP: u64 = 4;
pub(super) fn build_spec(
    workflow_id: &str,
    evidence: AutonomousEvidence,
) -> AutonomousBuddyChatSpec {
    let meta = autonomous_workflow_meta(workflow_id);
    AutonomousBuddyChatSpec::new(
        workflow_id,
        meta.map(|meta| meta.title)
            .unwrap_or("Companion Autonomous Report"),
        evidence.prompt,
        evidence.evidence,
    )
    .with_display(
        meta.map(|meta| meta.icon).unwrap_or("🤖"),
        meta.map(|meta| meta.badge).unwrap_or("Companion"),
        meta.map(|meta| meta.priority).unwrap_or("normal"),
    )
}

pub(super) async fn autonomous_activity(
    gcx: AppState,
    spec: &AutonomousBuddyChatSpec,
    chat_id: &str,
    identity_name: &str,
) -> BuddyActivity {
    let fallback_title = format!("{} report saved", spec.title);
    let description = format!(
        "{} saved an autonomous {} report in chat {}.",
        identity_name, spec.badge, chat_id
    );
    let title = match crate::buddy::actor::buddy_snapshot(gcx.clone()).await {
        Some(snapshot) => {
            render_buddy_activity_title(
                gcx,
                snapshot.state.personality,
                snapshot.state.identity.name,
                snapshot.pulse,
                Some(spec.workflow_id.clone()),
                spec.title.clone(),
                VoiceIntent::AutonomousReportSaved,
                fallback_title,
            )
            .await
        }
        None => fallback_title,
    };
    BuddyActivity {
        icon: spec.icon.clone(),
        title,
        description,
        timestamp: Utc::now().to_rfc3339(),
        activity_type: spec.workflow_id.clone(),
        chat_id: Some(chat_id.to_string()),
        failure_category: None,
        failure_summary: None,
    }
}

pub(super) async fn autonomous_runtime_event(
    gcx: AppState,
    spec: &AutonomousBuddyChatSpec,
    chat_id: &str,
    identity_name: &str,
) -> BuddyRuntimeEvent {
    let fallback_title = format!("{} report ready", spec.title);
    let fallback_description = format!(
        "Open {}'s saved chat for {} details.",
        identity_name, spec.title
    );
    let (title, description) = match crate::buddy::actor::buddy_snapshot(gcx.clone()).await {
        Some(snapshot) => {
            render_buddy_runtime_event(
                gcx,
                snapshot.state.personality,
                snapshot.state.identity.name,
                snapshot.pulse,
                Some(spec.workflow_id.clone()),
                fallback_description.clone(),
                "completed",
                fallback_title,
                Some(fallback_description.clone()),
            )
            .await
        }
        None => (fallback_title, Some(fallback_description.clone())),
    };
    let mut event = make_runtime_event(
        &spec.workflow_id,
        &title,
        "buddy",
        &format!("{}:{}", spec.workflow_id, spec.signal_hash),
        "completed",
        Some(&spec.priority),
    );
    let description = description.or(Some(fallback_description));
    event.description = description.clone();
    event.ttl_ms = Some(30_000);
    if spec.workflow_id != refact_buddy_core::autonomous_workflows::BUDDY_DAILY_DIGEST_WORKFLOW_ID {
        event.speech_text = Some(description.as_deref().unwrap_or(title.as_str()).to_string());
    }
    event.scene = Some("insight".to_string());
    event.duration_hint = Some(12);
    event.chat_id = Some(chat_id.to_string());
    event.failure_category = None;
    event.failure_summary = None;
    event
}

pub(super) fn apply_autonomous_runtime_chat_notification(
    event: &mut BuddyRuntimeEvent,
    description: String,
) {
    event.description = Some(description.clone());
    event.ttl_ms = Some(30_000);
    event.speech_text = Some(description);
    event.scene = Some("insight".to_string());
    event.duration_hint = Some(12);
}

pub(super) fn called_buddy_runtime_report(messages: &[ChatMessage]) -> bool {
    messages.iter().any(|message| {
        message.tool_calls.as_ref().is_some_and(|tool_calls| {
            tool_calls.iter().any(|tool_call| {
                matches!(
                    tool_call.function.name.as_str(),
                    "buddy_runtime_event" | "buddy_log_activity"
                )
            })
        })
    })
}

pub(super) fn last_assistant_text(messages: &[ChatMessage]) -> String {
    messages
        .iter()
        .rev()
        .find(|message| message.role == "assistant")
        .map(|message| message.content.content_text_only())
        .unwrap_or_default()
}

pub(super) fn assistant_tool_call_succeeded(messages: &[ChatMessage], tool_name: &str) -> bool {
    let mut call_ids = HashSet::new();
    for message in messages {
        if message.role != "assistant" {
            continue;
        }
        if let Some(tool_calls) = &message.tool_calls {
            for tool_call in tool_calls {
                if tool_call.function.name == tool_name {
                    call_ids.insert(tool_call.id.as_str());
                }
            }
        }
    }
    messages.iter().any(|message| {
        matches!(message.role.as_str(), "tool" | "diff")
            && message.tool_failed != Some(true)
            && call_ids.contains(message.tool_call_id.as_str())
    })
}

pub(super) fn autonomous_subchat_successful(messages: &[ChatMessage]) -> bool {
    let Some(last_assistant) = messages
        .iter()
        .rev()
        .find(|message| message.role == "assistant")
    else {
        return false;
    };
    if !last_assistant.content.content_text_only().trim().is_empty() {
        return true;
    }
    [
        "buddy_runtime_event",
        "buddy_create_draft",
        "buddy_memory_create",
    ]
    .iter()
    .any(|tool_name| assistant_tool_call_succeeded(messages, tool_name))
}

pub(super) fn autonomous_failure_result(
    ctx: &BuddyJobContext,
    spec: &AutonomousBuddyChatSpec,
    error: String,
) -> BuddyJobResult {
    let category = WorkflowFailureCategory::classify(&error);
    let detail = redact_and_cap_text(&error, 1_000);
    let summary = crate::buddy::workflows::workflow_failure_summary(&category, &error);
    let previous = parse_last_autonomous_result(ctx.job_state.last_result.as_deref());
    let consecutive_failures = previous
        .as_ref()
        .filter(|last| {
            last.status.as_deref() == Some("failed")
                && last.failure_category.as_ref() == Some(&category)
        })
        .and_then(|last| last.consecutive_failures)
        .unwrap_or(0)
        .saturating_add(1);
    let last = AutonomousLastResult {
        signal_hash: spec.signal_hash.clone(),
        chat_id: format!(
            "failed:{}:{}",
            spec.workflow_id,
            fact_kind_name(BuddyFactKind::BrokenModelReference)
        ),
        completed_at: Utc::now().to_rfc3339(),
        status: Some("failed".to_string()),
        failure_category: Some(category.clone()),
        consecutive_failures: Some(consecutive_failures),
    };
    BuddyJobResult {
        workflow_failure: Some(WorkflowFailureReport {
            workflow_id: spec.workflow_id.clone(),
            category,
            summary,
            detail,
            chat_id: None,
        }),
        last_result: Some(serialize_last_autonomous_result(&last)),
        ..Default::default()
    }
}

pub(super) fn default_autonomous_activity(
    spec: &AutonomousBuddyChatSpec,
    chat_id: &str,
    report_text: &str,
) -> BuddyActivity {
    let description = crate::llm::safe_truncate(report_text.trim(), 80).to_string();
    BuddyActivity {
        icon: spec.icon.clone(),
        title: format!("Buddy ran {}", spec.workflow_id),
        description: if description.is_empty() {
            "Autonomous Buddy workflow completed without a final assistant summary.".to_string()
        } else {
            description
        },
        timestamp: Utc::now().to_rfc3339(),
        activity_type: spec.workflow_id.clone(),
        chat_id: Some(chat_id.to_string()),
        failure_category: None,
        failure_summary: None,
    }
}

pub(crate) async fn execute_autonomous_spec(
    gcx: AppState,
    ctx: &BuddyJobContext,
    spec: AutonomousBuddyChatSpec,
    cooldown_seconds: u64,
) -> BuddyJobResult {
    if same_signal_or_failed_backoff(ctx, &spec.signal_hash, cooldown_seconds) {
        return preserve_last_result(ctx);
    }
    if crate::buddy::actor::buddy_llm_budget_exhausted(&gcx).await {
        tracing::info!(
            "buddy: autonomous job {} skipped, daily LLM token budget exhausted",
            spec.workflow_id
        );
        return preserve_last_result(ctx);
    }
    let chat_id = match run_autonomous_buddy_chat(gcx.clone(), spec.clone()).await {
        Ok(chat_id) => chat_id,
        Err(err) => {
            tracing::warn!("autonomous buddy job {} failed: {}", spec.workflow_id, err);
            return autonomous_failure_result(ctx, &spec, err);
        }
    };
    autonomous_success_result(gcx, ctx, spec, chat_id).await
}

pub(super) async fn autonomous_success_result(
    gcx: AppState,
    ctx: &BuddyJobContext,
    spec: AutonomousBuddyChatSpec,
    chat_id: String,
) -> BuddyJobResult {
    let last = AutonomousLastResult::new(spec.signal_hash.clone(), chat_id.clone());
    let activity = autonomous_activity(gcx.clone(), &spec, &chat_id, &ctx.identity_name).await;
    let runtime_event = autonomous_runtime_event(gcx, &spec, &chat_id, &ctx.identity_name).await;
    BuddyJobResult {
        activity: Some(activity),
        runtime_event: Some(runtime_event),
        last_result: Some(serialize_last_autonomous_result(&last)),
        xp: AUTONOMOUS_SUCCESS_XP,
        opportunities: job_finding_opportunities(&spec, &chat_id),
        ..Default::default()
    }
}

pub(crate) fn job_finding_opportunities(
    spec: &AutonomousBuddyChatSpec,
    chat_id: &str,
) -> Vec<(crate::buddy::types::BuddyOpportunity, u64)> {
    if !refact_buddy_core::autonomous_workflows::workflow_proposes_findings(&spec.workflow_id) {
        return Vec::new();
    }
    let now = Utc::now();
    let hash_prefix: String = spec.signal_hash.chars().take(16).collect();
    let mut opp = crate::buddy::types::BuddyOpportunity {
        id: uuid::Uuid::new_v4().to_string(),
        kind: crate::buddy::types::BuddyOpportunityKind::JobFinding,
        summary: format!("{} finished — review findings", spec.title),
        priority: crate::buddy::types::BuddyPriority::Normal,
        confidence: 0.7,
        fact_keys: vec![],
        cooldown_key: format!("job_finding:{}:{}", spec.workflow_id, hash_prefix),
        cooldown_secs: 24 * 60 * 60,
        status: crate::buddy::types::OpportunityStatus::New,
        proposed_actions: vec![
            crate::buddy::types::BuddyAction::OpenBuddyConversation {
                chat_id: chat_id.to_string(),
            },
            crate::buddy::types::BuddyAction::Dismiss,
        ],
        humor: None,
        humor_allowed: false,
        related: crate::buddy::types::BuddyOpportunityLinks::default(),
        created_at: now,
        expires_at: now + chrono::Duration::hours(48),
        resolved_at: None,
    };
    opp.related.chat_ids = vec![chat_id.to_string()];
    vec![(opp, 24 * 60 * 60)]
}

pub(super) async fn autonomous_chats_enabled(gcx: AppState) -> Result<bool, String> {
    let project_root = crate::files_correction::get_project_dirs(gcx.gcx.clone())
        .await
        .into_iter()
        .next()
        .ok_or_else(|| "no project root".to_string())?;
    Ok(load_settings(&project_root).await.autonomous_chats_enabled)
}

#[cfg_attr(not(test), allow(dead_code))]
/// C1: pick the model-type mode whose configured default model actually exists in caps.
/// A stale `chat_buddy_model` (e.g. a removed model id) must not fail the whole autonomous
/// fleet; fall back to default/thinking/light. Returns "buddy" when caps are unavailable or
/// nothing resolves, so the normal error path can surface.
pub(super) async fn pick_available_buddy_mode(gcx: AppState) -> String {
    let caps_state = gcx.model.caps.read().await;
    let Some(caps) = caps_state.caps.as_ref() else {
        return "buddy".to_string();
    };
    // Use the same resolution as the real subchat path (handles provider-prefixed and
    // finetune model ids), so a valid `model:ft-...` default is not treated as missing.
    let avail = |m: &str| {
        let m = m.trim();
        !m.is_empty() && crate::caps::resolve_model(&caps.chat_models, m).is_ok()
    };
    let d = &caps.defaults;
    if avail(&d.chat_buddy_model) {
        "buddy".to_string()
    } else if avail(&d.chat_default_model) {
        tracing::warn!(
            "buddy: chat_buddy_model '{}' unavailable, falling back to chat_default_model",
            d.chat_buddy_model
        );
        "default".to_string()
    } else if avail(&d.chat_thinking_model) {
        "thinking".to_string()
    } else if avail(&d.chat_light_model) {
        "light".to_string()
    } else {
        "buddy".to_string()
    }
}

pub fn sum_llm_usage(messages: &[ChatMessage]) -> (u64, u64, u64) {
    let mut llm_calls = 0u64;
    let mut tokens_in = 0u64;
    let mut tokens_out = 0u64;
    for msg in messages {
        if msg.role != "assistant" {
            continue;
        }
        llm_calls += 1;
        if let Some(usage) = &msg.usage {
            tokens_in = tokens_in.saturating_add(usage.prompt_tokens as u64);
            tokens_out = tokens_out.saturating_add(usage.completion_tokens as u64);
        }
    }
    (llm_calls, tokens_in, tokens_out)
}

pub async fn run_autonomous_buddy_chat(
    gcx: AppState,
    spec: AutonomousBuddyChatSpec,
) -> Result<String, String> {
    if !validate_workflow_id(&spec.workflow_id) {
        return Err(format!(
            "invalid autonomous buddy workflow id: {}",
            spec.workflow_id
        ));
    }

    if !autonomous_chats_enabled(gcx.clone()).await? {
        return Err("autonomous chats disabled".to_string());
    }

    let (messages, max_steps, tools) = build_autonomous_messages(gcx.clone(), &spec).await?;

    let mut config = crate::subchat::resolve_subchat_config(
        gcx.gcx.clone(),
        &spec.workflow_id,
        true,
        Some(format!("buddy-{}-{}", spec.workflow_id, Uuid::new_v4())),
        Some(spec.title.clone()),
        None,
        None,
        None,
        Some(tools),
        max_steps,
        false,
        None,
        pick_available_buddy_mode(gcx.clone()).await,
    )
    .await?;

    config.autonomous_no_confirm = true;
    config.mode = "buddy".to_string();
    config.buddy_meta = Some(BuddyThreadMeta {
        is_buddy_chat: true,
        buddy_chat_kind: "system".to_string(),
        workflow_id: Some(spec.workflow_id.clone()),
    });

    let result = crate::subchat::run_subchat(gcx.gcx.clone(), messages, config).await?;
    let (llm_calls, tokens_in, tokens_out) = sum_llm_usage(&result.messages);
    let success = autonomous_subchat_successful(&result.messages);
    crate::buddy::actor::buddy_record_workflow_telemetry(
        gcx.clone(),
        &spec.workflow_id,
        llm_calls,
        tokens_in,
        tokens_out,
        success,
    )
    .await;
    if !success {
        return Err("autonomous buddy subchat completed without a successful report".to_string());
    }
    let chat_id = result
        .chat_id
        .clone()
        .ok_or_else(|| "autonomous buddy chat did not return a chat_id".to_string())?;
    if !called_buddy_runtime_report(&result.messages) {
        let report_text = last_assistant_text(&result.messages);
        buddy_apply(
            gcx,
            BuddyMutation {
                activity: Some(default_autonomous_activity(&spec, &chat_id, &report_text)),
                ..Default::default()
            },
        )
        .await;
    }
    Ok(chat_id)
}

#[cfg_attr(not(test), allow(dead_code))]
pub(super) async fn build_autonomous_messages(
    gcx: AppState,
    spec: &AutonomousBuddyChatSpec,
) -> Result<(Vec<ChatMessage>, usize, Vec<String>), String> {
    let subagent_config = crate::yaml_configs::customization_registry::get_subagent_config(
        gcx.gcx.clone(),
        &spec.workflow_id,
        None,
    )
    .await
    .ok_or_else(|| format!("subagent config '{}' not found", spec.workflow_id))?;
    if subagent_config.tools.is_empty() {
        tracing::warn!(workflow_id = %spec.workflow_id, "autonomous subagent config has no tools");
    }

    let mut system_prompt = subagent_config.messages.system_prompt.ok_or_else(|| {
        format!(
            "messages.system_prompt not defined for subagent '{}'",
            spec.workflow_id
        )
    })?;
    let user_template = subagent_config.messages.user_template.ok_or_else(|| {
        format!(
            "messages.user_template not defined for subagent '{}'",
            spec.workflow_id
        )
    })?;

    let max_steps = subagent_config.subchat.max_steps.unwrap_or(1).max(1);
    if system_prompt.contains("%BUDDY_PERSONALITY%") {
        system_prompt = crate::chat::prompts::system_prompt_add_extra_instructions(
            gcx,
            system_prompt,
            subagent_config
                .tools
                .iter()
                .cloned()
                .collect::<HashSet<_>>(),
            &ChatMeta::default(),
            &None,
            "buddy",
        )
        .await;
        if system_prompt.contains("%BUDDY_PERSONALITY%") {
            system_prompt = system_prompt.replace(
                "%BUDDY_PERSONALITY%",
                "You are the project buddy assistant.",
            );
        }
    }
    let user_prompt = render_autonomous_template(&user_template, spec);
    let messages = vec![
        ChatMessage::new("system".to_string(), system_prompt),
        ChatMessage::new("user".to_string(), user_prompt),
    ];
    Ok((messages, max_steps, subagent_config.tools))
}

#[cfg_attr(not(test), allow(dead_code))]
pub(super) fn render_autonomous_template(template: &str, spec: &AutonomousBuddyChatSpec) -> String {
    let prompt = redact_and_cap_prompt(&spec.prompt);
    let evidence = redact_and_cap_evidence(&spec.evidence);
    let memory_key = spec.diagnostic_memory_key().unwrap_or_default();
    let memory_instruction = if memory_key.is_empty() {
        String::new()
    } else {
        memory_key_instruction(&memory_key)
    };
    let replacements = [
        ("{{workflow_id}}", spec.workflow_id.as_str()),
        ("{{title}}", spec.title.as_str()),
        ("{{signal_hash}}", spec.signal_hash.as_str()),
        (
            "{{memory_idempotency_instruction}}",
            memory_instruction.as_str(),
        ),
        ("{{icon}}", spec.icon.as_str()),
        ("{{badge}}", spec.badge.as_str()),
        ("{{priority}}", spec.priority.as_str()),
        ("{{project_root}}", spec.project_root.as_str()),
        ("{{prompt}}", prompt.as_str()),
        ("{{evidence}}", evidence.as_str()),
    ];
    let mut rendered = template.to_string();
    for (needle, value) in replacements {
        rendered = rendered.replace(needle, value);
    }
    if spec.workflow_id == ERROR_DETECTIVE_WORKFLOW_ID
        && !rendered.contains("Memory idempotency key:")
        && !memory_instruction.is_empty()
    {
        rendered.push('\n');
        rendered.push_str(&memory_instruction);
    }
    rendered
}

pub(super) fn build_autonomous_job_spec(
    definition: AutonomousJobDefinition,
    evidence: String,
) -> AutonomousBuddyChatSpec {
    AutonomousBuddyChatSpec::new(
        definition.meta.id,
        definition.meta.title,
        definition.prompt,
        evidence,
    )
    .with_display(
        definition.meta.icon,
        definition.meta.badge,
        definition.meta.priority,
    )
}

pub(super) async fn execute_autonomous_job(
    gcx: AppState,
    ctx: &BuddyJobContext,
    definition: AutonomousJobDefinition,
    evidence: String,
) -> BuddyJobResult {
    let spec = build_autonomous_job_spec(definition, evidence);
    execute_built_autonomous_job(gcx, ctx, definition, spec).await
}

pub(super) const SIGNAL_KNOWN_FRESHNESS_DAYS: i64 = 7;

pub(super) async fn signal_already_known(
    gcx: &AppState,
    memory_key: &str,
) -> Option<std::path::PathBuf> {
    let existing = {
        let idx = gcx.gcx.knowledge_index.lock().await;
        let path = idx.first_path_for_signal_key(memory_key)?.clone();
        let card = idx.card_for_path(&path)?;
        let freshness = card
            .updated
            .as_deref()
            .or(card.created.as_deref())
            .or(card.created_at.as_deref())
            .map(|value| value.to_string());
        (path, freshness)
    };
    let (path, freshness) = existing;
    let freshness = freshness?;
    let date_part = freshness.get(..10)?;
    let observed = chrono::NaiveDate::parse_from_str(date_part, "%Y-%m-%d").ok()?;
    let age_days = (Utc::now().date_naive() - observed).num_days();
    (age_days < SIGNAL_KNOWN_FRESHNESS_DAYS).then_some(path)
}

pub(super) async fn execute_built_autonomous_job(
    gcx: AppState,
    ctx: &BuddyJobContext,
    definition: AutonomousJobDefinition,
    spec: AutonomousBuddyChatSpec,
) -> BuddyJobResult {
    if same_signal_or_failed_backoff(ctx, &spec.signal_hash, definition.cooldown_seconds) {
        return preserve_last_result(ctx);
    }
    if crate::buddy::actor::buddy_llm_budget_exhausted(&gcx).await {
        tracing::info!(
            "buddy: autonomous job {} skipped, daily LLM token budget exhausted",
            definition.meta.id
        );
        return preserve_last_result(ctx);
    }

    if let Some(memory_key) = spec.diagnostic_memory_key() {
        if let Some(existing) = signal_already_known(&gcx, &memory_key).await {
            tracing::info!(
                "buddy: autonomous job {} skipped, signal '{}' already covered by {}",
                definition.meta.id,
                memory_key,
                existing.display()
            );
            if let Err(err) =
                crate::memories::touch_memory_observation(gcx.gcx.clone(), &existing).await
            {
                tracing::debug!("buddy: failed to touch known signal memory: {}", err);
            }
            return preserve_last_result(ctx);
        }
    }

    let signal_hash = spec.signal_hash.clone();
    let chat_id = match run_autonomous_buddy_chat(gcx, spec.clone()).await {
        Ok(chat_id) => chat_id,
        Err(err) => {
            tracing::debug!(
                "buddy: autonomous job {} skipped after subchat failure: {}",
                definition.meta.id,
                err
            );
            return autonomous_failure_result(ctx, &spec, err);
        }
    };

    let activity = BuddyActivity {
        icon: definition.meta.icon.to_string(),
        title: format!(
            "{} opened {}'s check-in",
            definition.meta.title, ctx.identity_name
        ),
        description: format!(
            "{} created a {} system conversation.",
            ctx.identity_name, definition.meta.badge
        ),
        timestamp: Utc::now().to_rfc3339(),
        activity_type: definition.meta.id.to_string(),
        chat_id: Some(chat_id.clone()),
        failure_category: None,
        failure_summary: None,
    };
    let mut runtime_event = make_runtime_event(
        definition.meta.id,
        definition.meta.title,
        definition.meta.id,
        &format!("{}:{}", definition.meta.id, signal_hash),
        "completed",
        Some(definition.meta.priority),
    );
    apply_autonomous_runtime_chat_notification(
        &mut runtime_event,
        format!(
            "{} created a {} system conversation from local signals.",
            ctx.identity_name, definition.meta.badge
        ),
    );
    runtime_event.chat_id = Some(chat_id.clone());
    let opportunities = job_finding_opportunities(&spec, &chat_id);
    BuddyJobResult {
        activity: Some(activity),
        runtime_event: Some(runtime_event),
        last_result: Some(serialize_last_autonomous_result(
            &AutonomousLastResult::new(signal_hash, chat_id),
        )),
        xp: AUTONOMOUS_SUCCESS_XP,
        opportunities,
        ..Default::default()
    }
}
