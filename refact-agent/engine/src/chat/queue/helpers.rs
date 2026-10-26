use super::*;

pub fn worktree_activation_message(worktree: &WorktreeMeta) -> ChatMessage {
    let branch = worktree.branch.as_deref().unwrap_or("unknown");
    let base = worktree.base_branch.as_deref().unwrap_or("unknown");
    ChatMessage {
        role: "cd_instruction".to_string(),
        content: ChatContent::SimpleText(format!(
            "💿 WORKTREE_ENABLED\n\nActive worktree scope is now ON for this chat.\n\n- Worktree id: `{}`\n- Branch: `{}`\n- Base/target branch: `{}`\n- Worktree root: `{}`\n- Source workspace root: `{}`\n\nEffects for this thread:\n- File reads, edits, shell commands, searches, and @file resolution should operate inside the worktree root unless a tool explicitly says otherwise.\n- Treat the main workspace as the merge target and do not edit it directly for this chat.\n- Use relative paths as usual; absolute paths outside the worktree may be rejected or remapped.\n- To merge completed work, call `worktree_merge` or use the Worktrees UI merge action.\n- If you need to leave the isolated scope, ask the user to detach the worktree first.",
            worktree.id,
            branch,
            base,
            worktree.root.display(),
            worktree.source_workspace_root.display()
        )),
        tool_call_id: "worktree_enabled".to_string(),
        ..Default::default()
    }
}

pub fn worktree_disabled_message(worktree: Option<&WorktreeMeta>) -> ChatMessage {
    let previous = worktree
        .map(|worktree| {
            let branch = worktree.branch.as_deref().unwrap_or("unknown");
            format!(
                "\n\nPrevious worktree scope:\n- Worktree id: `{}`\n- Branch: `{}`\n- Worktree root: `{}`",
                worktree.id,
                branch,
                worktree.root.display()
            )
        })
        .unwrap_or_default();
    ChatMessage {
        role: "cd_instruction".to_string(),
        content: ChatContent::SimpleText(format!(
            "💿 WORKTREE_DISABLED\n\nActive worktree scope is now OFF for this chat. File reads, edits, shell commands, searches, and @file resolution should use the main workspace again.{previous}"
        )),
        tool_call_id: "worktree_disabled".to_string(),
        ..Default::default()
    }
}

pub(crate) async fn push_user_activity(app: AppState, action: UserAction) {
    app.activity_sink.record_user_action(action).await;
}

pub(crate) fn apply_manual_context_files(
    session: &mut crate::chat::types::ChatSession,
    context_files: &[serde_json::Value],
) {
    const MAX_CTX_FILES: usize = 5;
    const MAX_TOTAL_CHARS: usize = 50_000;
    let mut validated: Vec<crate::call_validation::ContextFile> = Vec::new();
    let mut total_chars = 0usize;
    for v in context_files.iter().take(MAX_CTX_FILES) {
        if let Ok(file) = serde_json::from_value::<crate::call_validation::ContextFile>(v.clone()) {
            let chars = file.file_content.chars().count();
            if total_chars + chars <= MAX_TOTAL_CHARS {
                total_chars += chars;
                validated.push(file);
            } else {
                continue;
            }
        }
    }
    if !validated.is_empty() {
        let msg = ChatMessage {
            message_id: Uuid::new_v4().to_string(),
            role: "context_file".to_string(),
            content: ChatContent::ContextFiles(validated),
            tool_call_id: "manual_memory_enrichment".to_string(),
            ..Default::default()
        };
        session.add_message(msg);
    }
}

pub(crate) fn user_message_with_client_message_id(
    content: ChatContent,
    checkpoints: Vec<crate::git::checkpoints::Checkpoint>,
    client_message_id: Option<String>,
) -> ChatMessage {
    let mut message = ChatMessage {
        message_id: Uuid::new_v4().to_string(),
        role: "user".to_string(),
        content,
        checkpoints,
        ..Default::default()
    };
    if let Some(client_message_id) = client_message_id {
        message.extra.insert(
            CLIENT_MESSAGE_ID_EXTRA_KEY.to_string(),
            serde_json::Value::String(client_message_id),
        );
    }
    message
}

pub(crate) async fn aborted_before_start_generation(
    session_arc: &Arc<AMutex<crate::chat::types::ChatSession>>,
) -> bool {
    let mut session = session_arc.lock().await;
    if !session.user_interrupt_flag.load(Ordering::SeqCst) {
        return false;
    }
    session.abort_flag.store(false, Ordering::SeqCst);
    session.user_interrupt_flag.store(false, Ordering::SeqCst);
    if session.runtime.state == SessionState::Generating {
        session.set_runtime_state(SessionState::Idle, None);
    }
    session.queue_notify.notify_one();
    true
}

pub(crate) fn command_triggers_generation(cmd: &ChatCommand) -> bool {
    matches!(
        cmd,
        ChatCommand::UserMessage { .. }
            | ChatCommand::RetryFromIndex { .. }
            | ChatCommand::Regenerate {}
    )
}

pub(crate) fn note_user_turn_resets_goal_pursuit(session: &mut ChatSession) {
    session.goal_reset_no_progress();
    session.reactivate_goal_stopped_by_manual_abort();
}

pub(crate) fn drain_redundant_regenerates(
    queue: &mut VecDeque<CommandRequest>,
    priority: bool,
) -> Vec<String> {
    let mut removed = Vec::new();
    while queue.front().is_some_and(|request| {
        request.priority == priority && matches!(request.command, ChatCommand::Regenerate {})
    }) {
        if let Some(request) = queue.pop_front() {
            removed.push(request.client_request_id);
        }
    }
    removed
}

pub async fn inject_priority_messages_if_any(
    app: AppState,
    session_arc: Arc<AMutex<ChatSession>>,
) -> bool {
    let priority_requests = {
        let mut session = session_arc.lock().await;
        let requests = drain_priority_user_messages(&mut session.command_queue);
        if !requests.is_empty() {
            for request in &requests {
                session.record_command_queue_wait(&request.client_request_id);
            }
            session.emit_queue_update();
        }
        requests
    };

    if priority_requests.is_empty() {
        return false;
    }

    for request in priority_requests {
        if let ChatCommand::UserMessage {
            content,
            attachments,
            context_files,
            suppress_auto_enrichment,
            client_message_id,
            push: _,
        } = request.command
        {
            let (session_id, project_dir) = {
                let session = session_arc.lock().await;
                let sid = session.chat_id.clone();
                drop(session);
                let pd = get_project_dir_string(app.clone()).await;
                (sid, pd)
            };
            let prompt_text = match &content {
                serde_json::Value::String(s) => s.clone(),
                other => serde_json::to_string(other).unwrap_or_default(),
            };
            let hook_results = run_hooks(
                app.clone(),
                HookEvent::UserPromptSubmit,
                HookPayload {
                    hook_event_name: "UserPromptSubmit".to_string(),
                    session_id,
                    project_dir,
                    tool_name: None,
                    tool_input: None,
                    tool_output: None,
                    user_prompt: Some(prompt_text),
                    extra: std::collections::HashMap::new(),
                },
            )
            .await;
            if first_block_reason(&hook_results).is_some() {
                continue;
            }

            let (checkpoints_enabled, chat_id, latest_checkpoint, worktree) = {
                let session = session_arc.lock().await;
                (
                    session.thread.checkpoints_enabled,
                    session.chat_id.clone(),
                    find_latest_checkpoint(&session),
                    session.thread.worktree.clone(),
                )
            };

            let checkpoints = if checkpoints_enabled {
                create_checkpoint_async(
                    app.clone(),
                    latest_checkpoint.as_ref(),
                    &chat_id,
                    worktree.as_ref(),
                )
                .await
            } else {
                Vec::new()
            };

            let accepted_user_message = {
                let mut session = session_arc.lock().await;
                if !context_files.is_empty() {
                    apply_manual_context_files(&mut session, &context_files);
                }
                if suppress_auto_enrichment && context_files.is_empty() {
                    session.suppress_auto_enrichment_for_next_turn = true;
                }
                let parsed_content = parse_content_with_attachments(&content, &attachments);
                let accepted = AcceptedUserMessage {
                    chat_id: session.chat_id.clone(),
                    thread: session.thread.clone(),
                    content: parsed_content.clone(),
                };
                let user_message = user_message_with_client_message_id(
                    parsed_content,
                    checkpoints,
                    client_message_id,
                );
                session.add_message(user_message);
                note_user_turn_resets_goal_pursuit(&mut session);
                accepted
            };
            let _ = maybe_enqueue_chat_reaction(app.clone(), accepted_user_message).await;
        }
    }

    maybe_save_trajectory_background_with_intent(
        app.clone(),
        session_arc.clone(),
        TrajectoryCommitIntent::Checkpoint,
    );
    true
}

pub fn find_allowed_command_while_paused(queue: &VecDeque<CommandRequest>) -> Option<usize> {
    for (i, req) in queue.iter().enumerate() {
        match &req.command {
            ChatCommand::ToolDecision { .. }
            | ChatCommand::ToolDecisions { .. }
            | ChatCommand::Abort {} => {
                return Some(i);
            }
            _ => {}
        }
    }
    None
}

pub fn find_allowed_command_while_waiting_ide(queue: &VecDeque<CommandRequest>) -> Option<usize> {
    for (i, req) in queue.iter().enumerate() {
        match &req.command {
            ChatCommand::IdeToolResult { .. } | ChatCommand::Abort {} => {
                return Some(i);
            }
            _ => {}
        }
    }
    None
}

pub fn drain_priority_user_messages(queue: &mut VecDeque<CommandRequest>) -> Vec<CommandRequest> {
    let mut priority_messages = Vec::new();
    let mut i = 0;
    while i < queue.len() {
        if queue[i].priority && matches!(queue[i].command, ChatCommand::UserMessage { .. }) {
            if let Some(req) = queue.remove(i) {
                priority_messages.push(req);
            }
        } else {
            i += 1;
        }
    }
    priority_messages
}

pub fn drain_non_priority_user_messages(
    queue: &mut VecDeque<CommandRequest>,
) -> Vec<CommandRequest> {
    let mut messages = Vec::new();
    let mut i = 0;
    while i < queue.len() {
        if !queue[i].priority && matches!(queue[i].command, ChatCommand::UserMessage { .. }) {
            if let Some(req) = queue.remove(i) {
                messages.push(req);
            }
        } else {
            i += 1;
        }
    }
    messages
}


/// Extract the latest checkpoint from session messages (call while holding lock)
pub(crate) fn find_latest_checkpoint(session: &ChatSession) -> Option<crate::git::checkpoints::Checkpoint> {
    session
        .messages
        .iter()
        .rev()
        .find(|msg| msg.role == "user" && !msg.checkpoints.is_empty())
        .and_then(|msg| msg.checkpoints.first().cloned())
}

pub(crate) async fn create_checkpoint_async(
    app: AppState,
    latest_checkpoint: Option<&crate::git::checkpoints::Checkpoint>,
    chat_id: &str,
    worktree: Option<&crate::worktrees::types::WorktreeMeta>,
) -> Vec<crate::git::checkpoints::Checkpoint> {
    use crate::git::checkpoints::{create_workspace_checkpoint, create_workspace_checkpoint_for_root};

    let result = if let Some(worktree) = worktree {
        create_workspace_checkpoint_for_root(
            app.gcx.clone(),
            &worktree.root,
            latest_checkpoint,
            chat_id,
        )
        .await
    } else {
        create_workspace_checkpoint(app.gcx.clone(), latest_checkpoint, chat_id).await
    };

    match result {
        Ok((checkpoint, _)) => {
            tracing::info!("Checkpoint created for chat {}: {:?}", chat_id, checkpoint);
            vec![checkpoint]
        }
        Err(e) => {
            warn!("Failed to create checkpoint for chat {}: {}", chat_id, e);
            Vec::new()
        }
    }
}

