#[allow(unused_imports)]
use super::*;

pub async fn verify_goal_before_completion(
    app: AppState,
    session_arc: Arc<AMutex<ChatSession>>,
    trigger: &str,
) -> GoalCompletionGateOutcome {
    let (begin, epoch, fork_trajectory_version) = {
        let mut session = session_arc.lock().await;
        let now_ms = epoch_ms_now();
        if session
            .goal_verification_blocked_until_ms
            .is_some_and(|until| now_ms < until)
            && should_verify_goal_on_done(&session)
        {
            return GoalCompletionGateOutcome::VerificationUnavailable;
        }
        let begin = begin_goal_verification_if_needed(&mut session);
        (
            begin,
            session.goal_ledger_last_seq(),
            session.trajectory_version,
        )
    };
    match begin {
        GoalVerificationBegin::Started => {}
        GoalVerificationBegin::BudgetExhausted => {
            let mut session = session_arc.lock().await;
            let status = apply_goal_budget_exhausted_terminal(&mut session, trigger)
                .unwrap_or(GoalStatus::BudgetExhausted);
            return GoalCompletionGateOutcome::BudgetExhausted(status);
        }
        GoalVerificationBegin::NoActiveGoal => return GoalCompletionGateOutcome::Passthrough,
    }
    {
        let session = session_arc.lock().await;
        if session.user_interrupt_flag.load(Ordering::SeqCst) {
            drop(session);
            abort_goal_verification(session_arc.clone()).await;
            return GoalCompletionGateOutcome::Aborted;
        }
    }

    let reply = match run_goal_verifier(app, session_arc.clone()).await {
        Ok(reply) => reply,
        Err(error) => {
            let aborted = {
                let session = session_arc.lock().await;
                session.user_interrupt_flag.load(Ordering::SeqCst) || error == ABORT_ERROR_MESSAGE
            };
            if aborted {
                abort_goal_verification(session_arc.clone()).await;
                return GoalCompletionGateOutcome::Aborted;
            }
            let mut session = session_arc.lock().await;
            return handle_verifier_failure(&mut session, trigger, &error);
        }
    };

    {
        let session = session_arc.lock().await;
        if session.user_interrupt_flag.load(Ordering::SeqCst) {
            drop(session);
            abort_goal_verification(session_arc.clone()).await;
            return GoalCompletionGateOutcome::Aborted;
        }
    }

    let mut session = session_arc.lock().await;
    match apply_goal_verdict_guarded(
        &mut session,
        trigger,
        reply,
        Some(epoch),
        fork_trajectory_version,
    ) {
        GoalVerificationApplyOutcome::Finalized => GoalCompletionGateOutcome::Finalized,
        GoalVerificationApplyOutcome::Rearmed => GoalCompletionGateOutcome::Rearmed,
        GoalVerificationApplyOutcome::Continued => GoalCompletionGateOutcome::Rearmed,
        GoalVerificationApplyOutcome::Stalled => GoalCompletionGateOutcome::VerificationStalled,
        GoalVerificationApplyOutcome::Superseded => GoalCompletionGateOutcome::Rearmed,
        GoalVerificationApplyOutcome::NoGoal => GoalCompletionGateOutcome::Passthrough,
    }
}

pub(super) struct GoalVerifierCollector;

impl StreamCollector for GoalVerifierCollector {
    fn on_delta_ops(&mut self, _choice_idx: usize, _ops: Vec<DeltaOp>) {}
    fn on_usage(&mut self, _usage: &ChatUsage) {}
    fn on_finish(&mut self, _choice_idx: usize, _finish_reason: Option<String>) {}
}

pub(super) struct GoalVerifierPrepareInputs {
    pub(super) thread: ThreadParams,
    pub(super) messages: Vec<ChatMessage>,
    pub(super) chat_id: String,
    pub(super) tools: Vec<ToolDesc>,
    pub(super) sampling_parameters: SamplingParameters,
    pub(super) options: ChatPrepareOptions,
}

pub(super) fn goal_verifier_prepare_inputs_from_parts(
    mut thread: ThreadParams,
    mut messages: Vec<ChatMessage>,
    goal_text: String,
    criteria: Vec<GoalCriterion>,
    chat_id: String,
    tools: Vec<ToolDesc>,
) -> GoalVerifierPrepareInputs {
    messages.push(goal_verification_message(&goal_text, &criteria));
    thread.max_tokens = Some(GOAL_VERIFIER_MAX_TOKENS);
    thread.temperature = Some(0.0);
    let sampling_parameters = verifier_sampling_parameters(&thread);
    let options = ChatPrepareOptions {
        prepend_system_prompt: false,
        allow_at_commands: false,
        allow_tool_prerun: false,
        supports_tools: true,
        tool_choice: Some(ToolChoice::None),
        parallel_tool_calls: Some(false),
        cache_control: CacheControl::Ephemeral,
        frozen_request_prefix: thread.frozen_request_prefix.clone(),
    };
    GoalVerifierPrepareInputs {
        thread,
        messages,
        chat_id,
        tools,
        sampling_parameters,
        options,
    }
}

#[cfg(test)]
pub(super) fn goal_verifier_prepare_inputs(
    session: &ChatSession,
    tools: Vec<ToolDesc>,
) -> Result<GoalVerifierPrepareInputs, String> {
    Ok(goal_verifier_prepare_inputs_from_parts(
        session.thread.clone(),
        refact_core::active_context::active_context(&session.messages)
            .map_err(|error| error.to_string())?
            .messages,
        goal_text_for_verifier(session)?,
        session
            .goal
            .as_ref()
            .map(|goal| goal.criteria.clone())
            .unwrap_or_default(),
        session.chat_id.clone(),
        tools,
    ))
}

pub async fn run_goal_verifier(
    app: AppState,
    session_arc: Arc<AMutex<ChatSession>>,
) -> Result<GoalVerifierReply, String> {
    let gcx = app.gcx.clone();
    let (thread, messages, goal_text, criteria, chat_id, abort_notify) = {
        let session = session_arc.lock().await;
        let goal_text = goal_text_for_verifier(&session)?;
        (
            session.thread.clone(),
            refact_core::active_context::active_context(&session.messages)
                .map_err(|error| error.to_string())?
                .messages,
            goal_text,
            session
                .goal
                .as_ref()
                .map(|goal| goal.criteria.clone())
                .unwrap_or_default(),
            session.chat_id.clone(),
            session.abort_notify.clone(),
        )
    };

    if verifier_cancel_requested(&session_arc).await {
        return Err(ABORT_ERROR_MESSAGE.to_string());
    }

    let caps = crate::global_context::try_load_caps_quickly_if_not_present(gcx.clone(), 0)
        .await
        .map_err(|e| e.message)?;
    let model_rec = crate::caps::resolve_chat_model(caps, &thread.model)?;
    let execution_scope = thread
        .worktree
        .as_ref()
        .map(|worktree| worktree.root.to_string_lossy().into_owned());
    let tools_for_gen = app
        .tool_registry
        .get_tools_index_for_mode_and_scope(
            &thread.mode,
            Some(&model_rec.base.id),
            execution_scope.as_deref(),
        )
        .await;
    let prepare_inputs = goal_verifier_prepare_inputs_from_parts(
        thread,
        messages,
        goal_text,
        criteria,
        chat_id,
        tools_for_gen.tools,
    );
    let model_n_ctx = if model_rec.base.n_ctx > 0 {
        model_rec.base.n_ctx
    } else {
        crate::chat::config::tokens().default_n_ctx
    };
    let effective_n_ctx = match prepare_inputs.thread.context_tokens_cap {
        Some(cap) if cap > 0 => cap.min(model_n_ctx),
        _ => model_n_ctx,
    };
    let tokenizer_arc = crate::tokens::cached_tokenizer(gcx.clone(), &model_rec.base).await?;
    let t = HasTokenizerAndEot::new(tokenizer_arc);
    let ccx = crate::at_commands::at_commands::at_commands_new_from_app(
        app.clone(),
        effective_n_ctx,
        CHAT_TOP_N,
        false,
        prepare_inputs.messages.clone(),
        prepare_inputs.chat_id.clone(),
        prepare_inputs.thread.root_chat_id.clone(),
        model_rec.base.id.clone(),
        prepare_inputs.thread.task_meta.clone(),
        prepare_inputs.thread.worktree.clone(),
    )
    .await;
    let meta = ChatMeta {
        chat_id: prepare_inputs.chat_id,
        chat_mode: prepare_inputs.thread.mode.clone(),
        chat_remote: false,
        current_config_file: String::new(),
        context_tokens_cap: prepare_inputs.thread.context_tokens_cap,
        include_project_info: prepare_inputs.thread.include_project_info,
        request_attempt_id: Uuid::new_v4().to_string(),
        worktree: prepare_inputs.thread.worktree.clone(),
    };
    let mut sampling_parameters = prepare_inputs.sampling_parameters;
    let mut options = prepare_inputs.options;
    options.supports_tools = model_rec.supports_tools;
    let prepared = prepare_chat_passthrough(
        gcx.chat_ctx(gcx.clone()),
        Arc::new(AMutex::new(ccx)),
        &t,
        prepare_inputs.messages,
        &prepare_inputs.thread,
        &model_rec.base.id,
        &prepare_inputs.thread.mode,
        prepare_inputs.tools,
        &meta,
        &mut sampling_parameters,
        &options,
    )
    .await?;

    if verifier_cancel_requested(&session_arc).await {
        return Err(ABORT_ERROR_MESSAGE.to_string());
    }

    let verifier_abort_flag = Arc::new(AtomicBool::new(false));
    let params = StreamRunParams {
        llm_request: prepared.llm_request,
        model_rec: model_rec.base.clone(),
        chat_id: None,
        allow_websocket: true,
        abort_flag: Some(verifier_abort_flag),
        abort_notify: Some(abort_notify),
        supports_tools: model_rec.supports_tools,
        supports_reasoning: model_rec.has_reasoning_support(),
        reasoning_type: model_rec.reasoning_type_string(),
        supports_temperature: model_rec.supports_temperature,
    };
    let mut collector = GoalVerifierCollector;
    let choices = run_llm_stream(app, params, &mut collector)
        .await
        .map_err(|error| error.message)
        .and_then(|outcome| match outcome {
            LlmStreamOutcome::Choices(choices) => Ok(choices),
            LlmStreamOutcome::PausedForCacheGuard => {
                Err("verification paused by cache guard".to_string())
            }
        })?;
    let choice = choices.into_iter().next().unwrap_or_default();
    Ok(goal_verifier_reply_from_choice(choice))
}

pub(super) fn goal_verifier_reply_from_choice(choice: ChoiceFinal) -> GoalVerifierReply {
    let tool_called = !choice.tool_calls_raw.is_empty();
    let verdict = parse_goal_verdict(&choice.content, tool_called);
    let tokens = choice
        .usage
        .as_ref()
        .map(|usage| usage.total_tokens as u64)
        .unwrap_or(0);
    GoalVerifierReply {
        verdict,
        verifier_reply: choice.content,
        tokens,
    }
}

/// Engine side of the `GoalVerifierRunner` seam. The body is the local
/// `run_goal_verifier`; the seam exists so the `validate_goal` tool can name a
/// crate path instead of an engine-internal function.
pub struct EngineGoalVerifierRunner;

#[async_trait::async_trait]
impl refact_runtime_api::goal_verifier_runtime::GoalVerifierRunner for EngineGoalVerifierRunner {
    async fn run_goal_verifier(
        &self,
        app: &AppState,
        session_arc: Arc<AMutex<ChatSession>>,
    ) -> Result<GoalVerifierReply, String> {
        run_goal_verifier(app.clone(), session_arc).await
    }
}
