use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use indexmap::IndexMap;
use serde_json::json;
use tokenizers::Tokenizer;
use tokio::sync::Mutex as AMutex;
use tracing::info;

use refact_chat_api::{ChatMessage, TaskMeta};
use refact_core::at_parse::filter_only_context_file_from_context_tool;
use refact_core::chat_types::{ChatContent, ContextEnum, MultimodalElement, PostprocessSettings};
use refact_core::custom_error::first_n_chars;
use refact_postprocessing::pp_plain_text::postprocess_plain_text;
use refact_scratchpads::scratchpad_utils::{max_tokens_for_rag_chat, HasRagResults};
use refact_worktrees::scope::ExecutionScope;

use crate::app_state::AppState;
use crate::at_commands_ctx::{AtCommand, AtCommandMember, AtCommandsContext, AtParam};

pub const MIN_RAG_CONTEXT_LIMIT: usize = 256;

pub async fn at_commands_new_from_app(
    app: AppState,
    n_ctx: usize,
    top_n: usize,
    is_preview: bool,
    messages: Vec<ChatMessage>,
    chat_id: String,
    root_chat_id: Option<String>,
    current_model: String,
    task_meta: Option<TaskMeta>,
    worktree: Option<refact_core::worktree_meta::WorktreeMeta>,
) -> AtCommandsContext {
    at_commands_new_with_abort(
        app,
        n_ctx,
        top_n,
        is_preview,
        messages,
        chat_id,
        root_chat_id,
        current_model,
        task_meta,
        worktree,
        None,
    )
    .await
}

pub async fn at_commands_new_with_abort(
    app: AppState,
    n_ctx: usize,
    top_n: usize,
    is_preview: bool,
    messages: Vec<ChatMessage>,
    chat_id: String,
    root_chat_id: Option<String>,
    current_model: String,
    task_meta: Option<TaskMeta>,
    worktree: Option<refact_core::worktree_meta::WorktreeMeta>,
    abort_flag: Option<Arc<AtomicBool>>,
) -> AtCommandsContext {
    let execution_scope = worktree.map(|worktree| ExecutionScope::from_worktree(&worktree));
    at_commands_new_with_abort_and_execution_scope(
        app,
        n_ctx,
        top_n,
        is_preview,
        messages,
        chat_id,
        root_chat_id,
        current_model,
        task_meta,
        execution_scope,
        abort_flag,
    )
    .await
}

pub async fn at_commands_new_with_abort_and_execution_scope(
    app: AppState,
    n_ctx: usize,
    top_n: usize,
    is_preview: bool,
    messages: Vec<ChatMessage>,
    chat_id: String,
    root_chat_id: Option<String>,
    current_model: String,
    task_meta: Option<TaskMeta>,
    execution_scope: Option<ExecutionScope>,
    abort_flag: Option<Arc<AtomicBool>>,
) -> AtCommandsContext {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<serde_json::Value>();
    let effective_root = root_chat_id.unwrap_or_else(|| chat_id.clone());
    let session = app.chat.sessions.read().await.get(&chat_id).cloned();
    let derived_privacy_zones = match session {
        Some(session) => {
            let session = session.lock().await;
            session.derived_privacy_zones.clone()
        }
        None => refact_privacy::new_derived_privacy_zones(),
    };
    let background_agent_id = if !chat_id.starts_with("subchat-") {
        None
    } else {
        app.agents.find_agent_id_by_child_chat_id(&chat_id).await
    };
    AtCommandsContext {
        app: app.clone(),
        tool: app.tool.clone(),
        n_ctx,
        top_n,
        tokens_for_rag: (n_ctx / 4).max(64).min(n_ctx),
        messages,
        is_preview,
        pp_skeleton: true,
        correction_only_up_to_step: 0,
        chat_id,
        root_chat_id: effective_root,
        current_model,
        task_meta,
        execution_scope,
        derived_privacy_zones,
        subchat_depth: 0,
        tool_access_bypass: false,
        background_agent_id,
        at_commands: at_commands_dict(app).await,
        subchat_tool_parameters: IndexMap::new(),
        postprocess_parameters: PostprocessSettings::new(),
        subchat_tx: Arc::new(AMutex::new(tx)),
        subchat_rx: Arc::new(AMutex::new(rx)),
        abort_flag: abort_flag.unwrap_or_else(|| Arc::new(AtomicBool::new(false))),
        activity_stamp: None,
    }
}

pub async fn at_commands_dict(app: AppState) -> HashMap<String, Arc<dyn AtCommand + Send>> {
    let all_commands = app.gcx.context_factory.get().at_commands();

    let codegraph_on = app.tool.codeintel.codegraph.lock().await.is_some();
    let vecdb_on = app.workspace.vec_db.lock().await.is_some();
    let mut result = HashMap::new();
    for (key, value) in all_commands {
        let depends_on = value.depends_on();
        if depends_on.contains(&"codegraph".to_string()) && !codegraph_on {
            continue;
        }
        if depends_on.contains(&"vecdb".to_string()) && !vecdb_on {
            continue;
        }
        result.insert(key, value);
    }

    result
}

pub async fn correct_at_arg(
    ccx: Arc<AMutex<AtCommandsContext>>,
    param: &Box<dyn AtParam>,
    arg: &mut AtCommandMember,
) {
    if param.is_value_valid(ccx.clone(), &arg.text).await {
        return;
    }
    let completion = match param.param_completion(ccx.clone(), &arg.text).await.get(0) {
        Some(x) => x.clone(),
        None => {
            arg.ok = false;
            arg.reason = Some("incorrect argument; failed to complete".to_string());
            return;
        }
    };
    if !param.is_value_valid(ccx.clone(), &completion).await {
        arg.ok = false;
        arg.reason = Some("incorrect argument; completion did not help".to_string());
        return;
    }
    arg.text = completion;
}

pub async fn execute_at_commands_in_query(
    ccx: Arc<AMutex<AtCommandsContext>>,
    query: &mut String,
) -> (Vec<ContextEnum>, Vec<AtCommandMember>) {
    let at_commands = ccx.lock().await.at_commands.clone();
    let at_command_names = at_commands.keys().map(|x| x.clone()).collect::<Vec<_>>();
    let mut context_enums = vec![];
    let mut highlight_members = vec![];
    let mut clips: Vec<(String, usize, usize)> = vec![];

    let words = refact_core::at_parse::parse_words_from_line(query);
    for (w_idx, (word, pos1, pos2)) in words.iter().enumerate() {
        let cmd = match at_commands.get(word) {
            Some(c) => c,
            None => {
                continue;
            }
        };
        let args = words
            .iter()
            .skip(w_idx + 1)
            .map(|x| x.clone())
            .collect::<Vec<_>>();

        let mut cmd_member = AtCommandMember::new("cmd".to_string(), word.clone(), *pos1, *pos2);
        let mut arg_members = vec![];
        for (text, pos1, pos2) in args.iter().map(|x| x.clone()) {
            if at_command_names.contains(&text) {
                break;
            }
            // TODO: break if there's \n\n
            arg_members.push(AtCommandMember::new(
                "arg".to_string(),
                text.clone(),
                pos1,
                pos2,
            ));
        }

        match cmd
            .at_execute(ccx.clone(), &mut cmd_member, &mut arg_members)
            .await
        {
            Ok((res, text_on_clip)) => {
                context_enums.extend(res);
                clips.push((
                    text_on_clip,
                    cmd_member.pos1,
                    arg_members
                        .last()
                        .map(|x| x.pos2)
                        .unwrap_or(cmd_member.pos2),
                ));
            }
            Err(e) => {
                cmd_member.ok = false;
                cmd_member.reason = Some(format!("incorrect argument; failed to complete: {}", e));
                tracing::warn!("can't execute command that indicated it can execute: {}", e);
            }
        }
        highlight_members.push(cmd_member);
        highlight_members.extend(arg_members);
    }
    for (text_on_clip, pos1, pos2) in clips.iter().rev() {
        query.replace_range(*pos1..*pos2, text_on_clip);
    }
    (context_enums, highlight_members)
}

pub async fn run_at_commands_locally(
    ccx: Arc<AMutex<AtCommandsContext>>,
    tokenizer: Option<Arc<Tokenizer>>,
    maxgen: usize,
    mut original_messages: Vec<ChatMessage>,
    stream_back_to_user: &mut HasRagResults,
) -> (Vec<ChatMessage>, bool) {
    let (n_ctx, top_n, is_preview, app) = {
        let cgcx = ccx.lock().await;
        (cgcx.n_ctx, cgcx.top_n, cgcx.is_preview, cgcx.app.clone())
    };
    if !is_preview {
        let preview_cache = app.workspace.at_commands_preview_cache.clone();
        preview_cache.lock().await.clear();
    }
    let reserve_for_context = max_tokens_for_rag_chat(n_ctx, maxgen);
    info!("reserve_for_context {} tokens", reserve_for_context);

    let mut any_context_produced = false;

    let mut user_msg_starts = original_messages.len();
    let mut messages_with_at: usize = 0;
    while user_msg_starts > 0 {
        let message = original_messages.get(user_msg_starts - 1).unwrap().clone();
        if message.role == "user" {
            user_msg_starts -= 1;
            if message.content.content_text_only().contains("@") {
                messages_with_at += 1;
            }
        } else {
            break;
        }
    }

    // Token limit works like this:
    // - if there's only 1 user message at the bottom, it receives reserve_for_context tokens for context
    // - if there are N user messages, they receive reserve_for_context/N tokens each (and there's no taking from one to give to the other)
    // This is useful to give prefix and suffix of the same file precisely the position necessary for FIM-like operation of a chat model
    let messages_after_user_msg = original_messages.split_off(user_msg_starts);
    let mut new_messages = original_messages;
    for (idx, mut msg) in messages_after_user_msg.into_iter().enumerate() {
        let (mut content, original_images) = if let ChatContent::Multimodal(parts) = &msg.content {
            let text = parts
                .iter()
                .filter_map(|p| {
                    if p.m_type == "text" {
                        Some(p.m_content.as_str())
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>()
                .join("\n");
            let images = parts
                .iter()
                .filter(|p| p.m_type.starts_with("image/"))
                .cloned()
                .collect::<Vec<_>>();
            (text, Some(images))
        } else {
            (msg.content.content_text_only(), None)
        };
        let content_n_tokens = msg
            .content
            .count_tokens(tokenizer.clone(), &None)
            .unwrap_or(0) as usize;

        let mut context_limit = reserve_for_context / messages_with_at.max(1);
        context_limit = context_limit.saturating_sub(content_n_tokens);

        info!(
            "msg {} user_posted {:?} which is {} tokens, that leaves {} tokens for context of this message",
            idx + user_msg_starts,
            first_n_chars(&content, 50),
            content_n_tokens,
            context_limit
        );

        let mut messages_exec_output = vec![];
        if content.contains("@") {
            let (res, _) = execute_at_commands_in_query(ccx.clone(), &mut content).await;
            messages_exec_output.extend(res);
        }

        let mut context_file_pp = filter_only_context_file_from_context_tool(&messages_exec_output);

        let mut plain_text_messages = vec![];
        for exec_result in messages_exec_output.into_iter() {
            // at commands exec() can produce role "user" "assistant" "diff" "plain_text"
            if let ContextEnum::ChatMessage(raw_msg) = exec_result {
                // means not context_file
                if raw_msg.role != "plain_text" {
                    stream_back_to_user.push_in_json(json!(raw_msg));
                    new_messages.push(raw_msg);
                } else {
                    plain_text_messages.push(raw_msg);
                }
            }
        }

        if !plain_text_messages.is_empty() || !context_file_pp.is_empty() {
            let effective_context_limit = context_limit.max(MIN_RAG_CONTEXT_LIMIT);
            let (tokens_limit_plain, mut tokens_limit_files) = {
                if context_file_pp.is_empty() {
                    (effective_context_limit, 0)
                } else {
                    (effective_context_limit / 2, effective_context_limit / 2)
                }
            };
            info!(
                "context_limit {} tokens_limit_plain {} tokens_limit_files: {}",
                context_limit, tokens_limit_plain, tokens_limit_files
            );

            let t0 = std::time::Instant::now();

            let (pp_plain_text, non_used_plain) = postprocess_plain_text(
                plain_text_messages,
                tokenizer.clone(),
                tokens_limit_plain,
                &None,
            )
            .await;
            for m in pp_plain_text {
                // OUTPUT: plain text after all custom messages
                stream_back_to_user.push_in_json(json!(m));
                new_messages.push(m);
            }
            tokens_limit_files += non_used_plain;
            info!("tokens_limit_files {}", tokens_limit_files);
            let (tool, mut pp_settings, pp_skeleton) = {
                let cgcx = ccx.lock().await;
                (
                    cgcx.tool.clone(),
                    cgcx.postprocess_parameters.clone(),
                    cgcx.pp_skeleton,
                )
            };
            pp_settings.use_ast_based_pp = false;
            pp_settings.max_files_n = top_n;
            if pp_skeleton {
                pp_settings.take_floor = 50.0;
            }
            let post_processed = tool
                .at_command_executor
                .clone()
                .postprocess_context_files(
                    &tool,
                    &mut context_file_pp,
                    tokenizer.clone(),
                    tokens_limit_files,
                    false,
                    &pp_settings,
                )
                .await;
            let (post_processed_files, _notes) = post_processed;
            if !post_processed_files.is_empty() {
                any_context_produced = true;
                let message = ChatMessage {
                    role: "context_file".to_string(),
                    content: ChatContent::ContextFiles(post_processed_files),
                    ..Default::default()
                };
                stream_back_to_user.push_in_json(json!(message));
                new_messages.push(message);
            }
            info!(
                "postprocess_plain_text_messages + postprocess_context_files {:.3}s",
                t0.elapsed().as_secs_f32()
            );
        }

        if content.trim().len() > 0 || original_images.is_some() {
            msg.content = if let Some(mut images) = original_images {
                let mut parts = vec![];
                if !content.trim().is_empty() {
                    parts.push(MultimodalElement {
                        m_type: "text".to_string(),
                        m_content: content,
                    });
                }
                parts.append(&mut images);
                ChatContent::Multimodal(parts)
            } else {
                ChatContent::SimpleText(content)
            };
            stream_back_to_user.push_in_json(json!(msg));
            new_messages.push(msg);
        }
    }

    (new_messages, any_context_produced)
}
