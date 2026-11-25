//! Chat tool-execution pipeline moved out of the engine (process_tool_calls_once,
//! execute_tools*, check_tools_confirmation). Zero behavior change: the engine
//! reaches it through the one-line glob shim in src/chat/tools.rs.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use chrono::Utc;
use tokio::sync::Mutex as AMutex;
use tokio::sync::Notify;
use tokio::task::JoinHandle;
use tracing::info;
use uuid::Uuid;
use futures::future::join_all;

use refact_runtime_api::app_state::AppState;
use refact_runtime_api::AtCommandsContext;
use refact_buddy_core::user_action::UserAction;
use refact_core::chat_types::{ChatContent, ChatMessage, ContextFile, PostprocessSettings};
use refact_core::call_validation::ChatToolCall;
use refact_chat_api::SubchatParameters;
use refact_core::constants::CHAT_TOP_N;
use refact_runtime_api::pp_tool_results::{postprocess_tool_results, ToolBudget};
use refact_perf::perf_diagnostics::{self, PerfComponent, PerfOutcome, ToolExecutionClass};
use refact_runtime_api::customization_registry_gcx::{get_mode_config, match_tool_confirm_action};
use refact_chat_api::setparams::map_legacy_mode_to_id;
use refact_ext::hooks::HookEvent;
use refact_ext::hooks_payload::HookPayload;
use refact_runtime_api::ext_hooks_runner::{first_block_reason, get_project_dir_string, run_hooks};
use refact_runtime_api::{ToolCatalogSnapshot, TurnToolPool};
use refact_tool_api::MatchConfirmDenyResult;
use refact_exec::command_policy::escalation_from_args;

fn make_runtime_event(
    signal_type: &str,
    title: &str,
    source: &str,
    dedupe_key: &str,
    status: &str,
    priority: Option<&str>,
) -> refact_buddy_core::types::BuddyRuntimeEvent {
    refact_buddy_core::types::BuddyRuntimeEvent {
        id: Uuid::new_v4().to_string(),
        signal_type: signal_type.to_string(),
        title: title.to_string(),
        description: None,
        source: source.to_string(),
        status: status.to_string(),
        failure_category: None,
        failure_summary: None,
        progress: None,
        dedupe_key: Some(dedupe_key.to_string()),
        priority: priority.unwrap_or("normal").to_string(),
        created_at: Utc::now().to_rfc3339(),
        ttl_ms: None,
        bubble_policy: None,
        speech_text: None,
        scene: None,
        duration_hint: None,
        persistent: false,
        controls: Vec::new(),
        chat_id: None,
        dismissed: false,
    }
}

pub use refact_chat_api::ExecuteToolsOptions;

pub fn tool_execution_messages(
    messages: &[ChatMessage],
    last_prompt_messages: &[ChatMessage],
) -> Vec<ChatMessage> {
    if last_prompt_messages.is_empty() {
        messages.to_vec()
    } else {
        last_prompt_messages.to_vec()
    }
}

pub fn tool_timestamp_ms() -> Option<u64> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|duration| u64::try_from(duration.as_millis()).ok())
        .filter(|timestamp| *timestamp > 0)
}

pub const SHELL_PRIVACY_APPROVAL_RULE: &str =
    "Privacy approval required: command read guarded files";

pub fn is_shell_privacy_approval(reason: &PauseReason) -> bool {
    reason.rule == SHELL_PRIVACY_APPROVAL_RULE
}

pub fn shell_privacy_approval_reasons(
    tool_calls: &[ChatToolCall],
    tool_results: &[ChatMessage],
) -> Vec<PauseReason> {
    tool_results
        .iter()
        .filter(|message| refact_privacy::model_context::shell_ask_pending(message))
        .filter_map(|message| {
            let tool_call = tool_calls
                .iter()
                .find(|tool_call| tool_call.id == message.tool_call_id)?;
            Some(PauseReason {
                reason_type: "confirmation".to_string(),
                tool_name: tool_call.function.name.clone(),
                command: tool_call.function.name.clone(),
                rule: SHELL_PRIVACY_APPROVAL_RULE.to_string(),
                tool_call_id: tool_call.id.clone(),
                integr_config_path: None,
            })
        })
        .collect()
}

pub enum ToolStepOutcome {
    NoToolCalls,
    Paused,
    Continue,
    Stop,
}

use refact_chat_api::types::*;
use refact_runtime_api::trajectories::store::{
    maybe_save_trajectory_with_intent, maybe_save_trajectory_background_with_intent,
};
use refact_chat_api::goal_verifier::{should_verify_goal_on_done, GoalCompletionGateOutcome};
use refact_runtime_api::goal_verifier_monitor::verify_goal_before_completion;

use refact_core::chat_config::tokens;

async fn get_effective_n_ctx(app: AppState, thread: &ThreadParams) -> usize {
    let default_n_ctx = tokens().default_n_ctx;
    let model_n_ctx = match refact_runtime_api::caps_loader::try_load_caps_quickly_if_not_present(
        app.gcx.clone(),
        0,
    )
    .await
    {
        Ok(caps) => {
            match refact_caps_core::model_resolve::resolve_chat_model(caps, &thread.model) {
                Ok(model_rec) if model_rec.base.n_ctx > 0 => model_rec.base.n_ctx,
                _ => default_n_ctx,
            }
        }
        Err(_) => default_n_ctx,
    };
    match thread.context_tokens_cap {
        Some(cap) if cap > 0 => cap.min(model_n_ctx),
        _ => model_n_ctx,
    }
}

pub async fn build_tool_execution_context(
    app: AppState,
    n_ctx: usize,
    messages: &[ChatMessage],
    thread: &ThreadParams,
    abort_flag: Option<Arc<AtomicBool>>,
) -> Result<Arc<AMutex<AtCommandsContext>>, String> {
    let messages = refact_core::active_context::active_context(messages)
        .map_err(|error| error.to_string())?
        .messages;
    let ccx = refact_runtime_api::at_commands_runtime::at_commands_new_with_abort(
        app.clone(),
        n_ctx,
        CHAT_TOP_N,
        false,
        messages,
        thread.id.clone(),
        thread.root_chat_id.clone(),
        thread.model.clone(),
        thread.task_meta.clone(),
        thread.worktree.clone(),
        abort_flag,
    )
    .await;
    Ok(Arc::new(AMutex::new(ccx)))
}

fn context_error_results(tool_calls: &[ChatToolCall], error: String) -> (Vec<ChatMessage>, bool) {
    (
        tool_calls
            .iter()
            .map(|call| ChatMessage {
                message_id: Uuid::new_v4().to_string(),
                role: "tool".to_string(),
                content: ChatContent::SimpleText(format!("Invalid active context: {error}")),
                tool_call_id: call.id.clone(),
                tool_failed: Some(true),
                ..Default::default()
            })
            .collect(),
        false,
    )
}

fn skipped_tool_result(tool_call: &ChatToolCall) -> ChatMessage {
    ChatMessage {
        message_id: Uuid::new_v4().to_string(),
        role: "tool".to_string(),
        content: ChatContent::SimpleText(
            "Tool call skipped because the turn was aborted before execution.".to_string(),
        ),
        tool_call_id: tool_call.id.clone(),
        tool_failed: Some(true),
        ..Default::default()
    }
}

fn execution_scope_for_thread(thread: &ThreadParams) -> Option<String> {
    thread
        .worktree
        .as_ref()
        .map(refact_worktrees::scope::tool_catalog_scope)
}

pub async fn acquire_session_turn_tool_pool(
    app: &AppState,
    session_arc: &Arc<AMutex<ChatSession>>,
    thread: &ThreadParams,
    mode_id: &str,
    model_id: Option<&str>,
    catalog: &ToolCatalogSnapshot,
) -> Option<TurnToolPool> {
    let existing = { session_arc.lock().await.turn_tool_pool.clone() };
    if existing.is_some() {
        return existing;
    }
    let pool = app
        .tool_registry
        .acquire_turn_tool_pool(
            mode_id,
            model_id,
            execution_scope_for_thread(thread).as_deref(),
            catalog,
        )
        .await;
    if let Some(pool) = &pool {
        session_arc.lock().await.turn_tool_pool = Some(pool.clone());
    }
    pool
}

pub fn is_server_executed_tool(tool_call_id: &str) -> bool {
    tool_call_id.starts_with("srvtoolu_")
}

pub fn rewrite_cc_native_stub_tool_call(tc: &mut ChatToolCall) {
    let name = tc.function.name.as_str();
    if name != "Grep" && name != "Glob" {
        return;
    }

    let args = tc.function.parse_args().unwrap_or_default();
    let mut mapped = serde_json::Map::new();
    let canonical_name = match name {
        "Grep" => {
            let pattern = args
                .get("pattern")
                .cloned()
                .unwrap_or_else(|| serde_json::Value::String(String::new()));
            let scope = args
                .get("path")
                .and_then(|v| v.as_str())
                .filter(|s| !s.trim().is_empty())
                .unwrap_or("workspace");
            mapped.insert("pattern".to_string(), pattern);
            mapped.insert(
                "scope".to_string(),
                serde_json::Value::String(scope.to_string()),
            );
            "search_pattern"
        }
        "Glob" => {
            let pattern = args
                .get("pattern")
                .cloned()
                .unwrap_or_else(|| serde_json::Value::String(String::new()));
            mapped.insert("pattern".to_string(), pattern);
            if let Some(path) = args
                .get("path")
                .and_then(|v| v.as_str())
                .filter(|s| !s.trim().is_empty())
            {
                mapped.insert(
                    "path".to_string(),
                    serde_json::Value::String(path.to_string()),
                );
            }
            "glob"
        }
        _ => return,
    };

    tc.function.name = canonical_name.to_string();
    tc.function.arguments = serde_json::Value::Object(mapped).to_string();
}

pub fn resolve_tool_call_aliases_with_catalog(
    tool_calls: Vec<ChatToolCall>,
    catalog: &ToolCatalogSnapshot,
) -> Vec<ChatToolCall> {
    let tool_names = &catalog.index.tools;
    let registry = &catalog.aliases;
    let needs_cc = tool_calls.iter().any(|tc| {
        tc.function
            .name
            .starts_with(refact_llm::adapters::claude_code_compat::MCP_TOOL_PREFIX)
    });
    let needs_cc_native = tool_calls
        .iter()
        .any(|tc| matches!(tc.function.name.as_str(), "Grep" | "Glob"));
    let item_count = tool_calls.len() as u64;
    let span = perf_diagnostics::span(PerfComponent::ToolAliasResolution, None, None);
    if !registry.needs_aliasing() && !needs_cc && !needs_cc_native {
        span.finish_tool(PerfOutcome::Success, 1, item_count, None);
        return tool_calls;
    }
    let resolved = tool_calls
        .into_iter()
        .map(|mut tc| {
            if matches!(tc.function.name.as_str(), "Grep" | "Glob") {
                rewrite_cc_native_stub_tool_call(&mut tc);
                return tc;
            }
            if tc
                .function
                .name
                .starts_with(refact_llm::adapters::claude_code_compat::MCP_TOOL_PREFIX)
            {
                let cc_resolved = refact_llm::adapters::claude_code_compat::cc_resolve_tool_name(
                    &tc.function.name,
                );
                let lookup_name = if cc_resolved != tc.function.name {
                    &cc_resolved
                } else {
                    &tc.function.name
                };
                if let Some(internal_name) = registry.resolve_alias(lookup_name) {
                    tc.function.name = internal_name.to_string();
                } else {
                    tc.function.name = cc_resolved;
                }
            } else if let Some(internal_name) = registry.resolve_alias(&tc.function.name) {
                tc.function.name = internal_name.to_string();
            } else if needs_cc && !tool_names.iter().any(|tool| tool.name == tc.function.name) {
                let cc_resolved = refact_llm::adapters::claude_code_compat::cc_resolve_tool_name(
                    &tc.function.name,
                );
                if let Some(internal_name) = registry.resolve_alias(&cc_resolved) {
                    tc.function.name = internal_name.to_string();
                } else {
                    tc.function.name = cc_resolved;
                }
            }
            tc
        })
        .collect::<Vec<_>>();
    span.finish_tool(PerfOutcome::Success, 1, item_count, None);
    resolved
}

pub async fn resolve_tool_call_aliases(
    app: AppState,
    tool_calls: Vec<ChatToolCall>,
    mode_id: &str,
    model_id: Option<&str>,
) -> Vec<ChatToolCall> {
    let catalog = app
        .tool_registry
        .acquire_tool_catalog(mode_id, model_id, None)
        .await;
    resolve_tool_call_aliases_with_catalog(tool_calls, &catalog)
}

const EDITING_TOOLS: &[&str] = &[
    "patch",
    "text_edit",
    "create_textdoc",
    "update_textdoc",
    "replace_textdoc",
    "update_textdoc_regex",
    "update_textdoc_by_lines",
    "update_textdoc_anchored",
    "apply_patch",
    "undo_textdoc",
    "mv",
];

pub fn is_editing_tool(tool_name: &str) -> bool {
    EDITING_TOOLS.contains(&tool_name)
}

// Tools whose results are expensive, non-reproducible reports (code reviews,
// subagent/delegate outcomes). Their outputs bypass the shared per-round
// ToolBudget so running many of them at once does not chop each report to a
// fair-share fragment; overall context pressure is handled later by chat
// compression instead.
const TOOL_RESULT_TRUNCATION_EXEMPT_TOOLS: &[&str] = &[
    "review",
    "code_review",
    "subagent",
    "agent_wait",
    "agent_result",
];

pub fn is_tool_result_truncation_exempt(tool_name: &str) -> bool {
    TOOL_RESULT_TRUNCATION_EXEMPT_TOOLS.contains(&tool_name)
}

pub fn should_auto_approve_confirmation(thread: &ThreadParams, tool_name: &str) -> bool {
    if thread.autonomous_no_confirm {
        return true;
    }
    thread.auto_approve_dangerous_commands
        || (thread.auto_approve_editing_tools && is_editing_tool(tool_name))
}

async fn record_tool_activity(
    app: AppState,
    tool_calls: &[ChatToolCall],
    chat_id: &str,
    approved_ids: &std::collections::HashSet<String>,
    denied_ids: &std::collections::HashSet<String>,
) {
    if approved_ids.is_empty() && denied_ids.is_empty() {
        return;
    }
    for tc in tool_calls {
        if denied_ids.contains(&tc.id) {
            app.activity_sink
                .record_user_action(UserAction::ToolRejected {
                    tool_name: tc.function.name.clone(),
                    chat_id: chat_id.to_string(),
                    ts: Utc::now(),
                })
                .await;
        } else if approved_ids.contains(&tc.id) {
            app.activity_sink
                .record_user_action(UserAction::ToolApproved {
                    tool_name: tc.function.name.clone(),
                    chat_id: chat_id.to_string(),
                    ts: Utc::now(),
                })
                .await;
        }
    }
}

fn tool_pool_key(desc: &refact_tool_api::ToolDesc) -> String {
    format!(
        "{}\u{1f}{:?}\u{1f}{}",
        refact_llm::adapters::claude_code_compat::cc_resolve_tool_name(&desc.name),
        desc.source.source_type,
        desc.source.config_path
    )
}

fn get_context_files_from_messages(messages: &[ChatMessage]) -> Vec<String> {
    let mut paths = Vec::new();
    for msg in messages {
        if msg.role == "context_file" {
            match &msg.content {
                ChatContent::ContextFiles(files) => {
                    for file in files {
                        if !paths.contains(&file.file_name) {
                            paths.push(file.file_name.clone());
                        }
                    }
                }
                ChatContent::SimpleText(text) => {
                    if let Ok(files) = serde_json::from_str::<Vec<ContextFile>>(text) {
                        for file in files {
                            if !paths.contains(&file.file_name) {
                                paths.push(file.file_name.clone());
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }
    paths
}

struct SubchatBridge {
    cancel_flag: Arc<AtomicBool>,
    cancel_notify: Arc<Notify>,
    task: Option<JoinHandle<()>>,
}

impl SubchatBridge {
    async fn shutdown(mut self) {
        self.cancel_flag.store(true, Ordering::Relaxed);
        self.cancel_notify.notify_one();
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }
}

impl Drop for SubchatBridge {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

fn spawn_subchat_bridge(
    ccx: Arc<AMutex<AtCommandsContext>>,
    session_arc: Arc<AMutex<ChatSession>>,
) -> SubchatBridge {
    let cancel_flag = Arc::new(AtomicBool::new(false));
    let cancel_flag_clone = cancel_flag.clone();
    let cancel_notify = Arc::new(Notify::new());
    let cancel_notify_clone = cancel_notify.clone();

    let task = tokio::spawn(async move {
        let (subchat_rx, abort_flag) = {
            let cgcx = ccx.lock().await;
            (cgcx.subchat_rx.clone(), cgcx.abort_flag.clone())
        };
        info!("spawn_subchat_bridge: started listening for subchat messages");

        let mut active_tool_call_ids: Vec<String> = Vec::new();

        loop {
            let should_stop =
                cancel_flag_clone.load(Ordering::Relaxed) || abort_flag.load(Ordering::Relaxed);

            if should_stop {
                info!(
                    "spawn_subchat_bridge: cancelled, sending cleanup events for {} active tools",
                    active_tool_call_ids.len()
                );
                let mut session = session_arc.lock().await;
                for tool_call_id in active_tool_call_ids.drain(..) {
                    session.emit(ChatEvent::SubchatUpdate {
                        tool_call_id,
                        subchat_id: String::new(),
                        attached_files: vec![],
                    });
                }
                break;
            }

            let recv_result = tokio::select! {
                _ = cancel_notify_clone.notified() => {
                    cancel_flag_clone.store(true, Ordering::Relaxed);
                    continue;
                }
                recv_result = async {
                    let mut rx = subchat_rx.lock().await;
                    tokio::time::timeout(std::time::Duration::from_millis(100), rx.recv()).await
                } => recv_result,
            };

            match recv_result {
                Ok(Some(value)) => {
                    info!(
                        "spawn_subchat_bridge: received message: {}b",
                        value.to_string().len()
                    );
                    let tool_call_id = value.get("tool_call_id").and_then(|v| v.as_str());
                    let subchat_id = value.get("subchat_id").and_then(|v| v.as_str());

                    if let (Some(tool_call_id), Some(subchat_id)) = (tool_call_id, subchat_id) {
                        info!(
                            "spawn_subchat_bridge: emitting SubchatUpdate for tool_call_id={}, subchat_id={}b",
                            tool_call_id,
                            subchat_id.len()
                        );

                        if !active_tool_call_ids.contains(&tool_call_id.to_string()) {
                            active_tool_call_ids.push(tool_call_id.to_string());
                        }

                        let mut attached_files: Vec<String> = value
                            .get("attached_files")
                            .and_then(|v| v.as_array())
                            .map(|arr| {
                                arr.iter()
                                    .filter_map(|x| x.as_str().map(|s| s.to_string()))
                                    .collect()
                            })
                            .unwrap_or_default();

                        {
                            let session = session_arc.lock().await;
                            for f in get_context_files_from_messages(&session.messages) {
                                if !attached_files.contains(&f) {
                                    attached_files.push(f);
                                }
                            }
                        }

                        let mut session = session_arc.lock().await;
                        session.mark_tool_progress();
                        session.emit(ChatEvent::SubchatUpdate {
                            tool_call_id: tool_call_id.to_string(),
                            subchat_id: subchat_id.to_string(),
                            attached_files,
                        });
                    }
                }
                Ok(None) => break,
                Err(_) => {}
            }
        }
    });

    SubchatBridge {
        cancel_flag,
        cancel_notify,
        task: Some(task),
    }
}

pub async fn process_tool_calls_once(
    app: AppState,
    session_arc: Arc<AMutex<ChatSession>>,
    mode_id: &str,
    model_id: Option<&str>,
) -> ToolStepOutcome {
    let session_extraction_span =
        perf_diagnostics::span(PerfComponent::ToolSessionExtraction, None, None);
    let (
        tool_calls,
        server_tool_calls,
        messages,
        thread,
        tool_message_index,
        allowed_tools,
        source_command,
    ) = {
        let session = session_arc.lock().await;
        let msg_count = session.messages.len();
        let last_msg = session.messages.last();
        match last_msg {
            Some(m) if m.role == "assistant" && m.tool_calls.is_some() => {
                let all_calls = m.tool_calls.clone().unwrap();
                let (server_calls, client_calls): (Vec<_>, Vec<_>) = all_calls
                    .into_iter()
                    .partition(|tc| is_server_executed_tool(&tc.id));
                (
                    client_calls,
                    server_calls,
                    session.messages.clone(),
                    session.thread.clone(),
                    msg_count.saturating_sub(1),
                    session.active_command.allowed_tools.clone(),
                    session.active_command.name.clone(),
                )
            }
            _ => return ToolStepOutcome::NoToolCalls,
        }
    };
    session_extraction_span.finish_tool(
        PerfOutcome::Success,
        1,
        tool_calls.len().saturating_add(server_tool_calls.len()) as u64,
        None,
    );

    // Add synthetic tool results for server-executed tools (e.g., Anthropic's web_search).
    // These tools are executed by the LLM provider and their results are embedded in the
    // assistant's response as citations. However, the Anthropic API requires exactly one
    // tool_result per tool_use, so we add placeholder results to satisfy this requirement.
    // Without these, the LLM may regenerate similar responses because it sees tool_calls
    // without corresponding results.
    if !server_tool_calls.is_empty() {
        let mut session = session_arc.lock().await;
        for tc in &server_tool_calls {
            // Check if a tool result already exists for this tool call
            let result_exists = session
                .messages
                .iter()
                .any(|m| m.role == "tool" && m.tool_call_id == tc.id);
            if !result_exists {
                let content = if tc.function.name.starts_with("openai_") {
                    format_openai_server_tool_result(tc)
                } else {
                    format!(
                        "[Results from '{}' are included in the assistant's response above]",
                        tc.function.name
                    )
                };
                let tool_message = ChatMessage {
                    message_id: Uuid::new_v4().to_string(),
                    role: "tool".to_string(),
                    content: ChatContent::SimpleText(content),
                    tool_call_id: tc.id.clone(),
                    tool_failed: Some(false),
                    ..Default::default()
                };
                session.add_message(tool_message);
            }
        }
    }

    if tool_calls.is_empty() {
        if !server_tool_calls.is_empty() {
            let mut session = session_arc.lock().await;
            session.set_runtime_state(SessionState::Idle, None);
            session.release_turn_only_state();
        }
        return ToolStepOutcome::NoToolCalls;
    }

    let catalog_pool_span =
        perf_diagnostics::span(PerfComponent::ToolCatalogPoolAcquire, None, None);
    let session_catalog = {
        let session = session_arc.lock().await;
        session.tool_catalog.clone()
    };
    let catalog = match session_catalog {
        Some(catalog) => catalog,
        None => {
            app.tool_registry
                .acquire_tool_catalog(
                    mode_id,
                    model_id,
                    execution_scope_for_thread(&thread).as_deref(),
                )
                .await
        }
    };
    let turn_tool_pool =
        acquire_session_turn_tool_pool(&app, &session_arc, &thread, mode_id, model_id, &catalog)
            .await;
    catalog_pool_span.finish_tool(PerfOutcome::Success, 1, tool_calls.len() as u64, None);
    let tool_calls = resolve_tool_call_aliases_with_catalog(tool_calls, &catalog);

    info!(
        "process_tool_calls_once: {} tool calls to process",
        tool_calls.len()
    );

    let (confirmations, denials) = check_tools_confirmation_with_pool(
        app.clone(),
        &tool_calls,
        &messages,
        &thread,
        Some(&catalog),
        mode_id,
        model_id,
        &allowed_tools,
        &source_command,
        turn_tool_pool.as_ref(),
    )
    .await;

    let denied_ids: std::collections::HashSet<String> =
        denials.iter().map(|d| d.tool_call_id.clone()).collect();
    if !denials.is_empty() {
        let mut session = session_arc.lock().await;
        for denial in &denials {
            let tool_message = ChatMessage {
                message_id: Uuid::new_v4().to_string(),
                role: "tool".to_string(),
                content: ChatContent::SimpleText(format!("Denied by policy: {}", denial.rule)),
                tool_call_id: denial.tool_call_id.clone(),
                tool_failed: Some(true),
                ..Default::default()
            };
            session.add_message(tool_message);
        }
    }

    let mut approved_activity_ids: std::collections::HashSet<String> =
        std::collections::HashSet::new();

    if !confirmations.is_empty() {
        let (auto_approved, remaining): (Vec<_>, Vec<_>) =
            confirmations.into_iter().partition(|confirmation| {
                should_auto_approve_confirmation(&thread, &confirmation.tool_name)
            });

        for confirmation in &auto_approved {
            approved_activity_ids.insert(confirmation.tool_call_id.clone());
        }

        if !remaining.is_empty() {
            let mut auto_approved_ids: Vec<String> = auto_approved
                .iter()
                .map(|c| c.tool_call_id.clone())
                .collect();
            let paused_ids: std::collections::HashSet<&str> =
                remaining.iter().map(|r| r.tool_call_id.as_str()).collect();
            for tc in &tool_calls {
                if !denied_ids.contains(&tc.id)
                    && !paused_ids.contains(tc.id.as_str())
                    && !auto_approved_ids.contains(&tc.id)
                {
                    auto_approved_ids.push(tc.id.clone());
                }
            }

            let approved_set: std::collections::HashSet<String> =
                auto_approved_ids.iter().cloned().collect();
            record_tool_activity(
                app.clone(),
                &tool_calls,
                &thread.id,
                &approved_set,
                &denied_ids,
            )
            .await;

            let mut session = session_arc.lock().await;
            session.set_paused_with_reasons_and_auto_approved(
                remaining,
                auto_approved_ids,
                Some(tool_message_index),
            );
            return ToolStepOutcome::Paused;
        }
    }

    for tc in &tool_calls {
        if !denied_ids.contains(&tc.id) {
            approved_activity_ids.insert(tc.id.clone());
        }
    }
    record_tool_activity(
        app.clone(),
        &tool_calls,
        &thread.id,
        &approved_activity_ids,
        &denied_ids,
    )
    .await;

    let mut tools_to_execute: Vec<_> = tool_calls
        .iter()
        .filter(|tc| !denied_ids.contains(&tc.id))
        .cloned()
        .collect();

    if tools_to_execute.is_empty() {
        let mut session = session_arc.lock().await;
        session.release_turn_only_state();
        session.set_runtime_state(SessionState::Idle, None);
        drop(session);
        maybe_save_trajectory_background_with_intent(
            app.clone(),
            session_arc.clone(),
            TrajectoryCommitIntent::Checkpoint,
        );
        return ToolStepOutcome::Continue;
    }

    let (session_id, project_dir) = {
        let session = session_arc.lock().await;
        let session_id = session.chat_id.clone();
        drop(session);
        (session_id, get_project_dir_string(app.clone()).await)
    };
    let mut pre_hook_blocked_ids: std::collections::HashSet<String> =
        std::collections::HashSet::new();
    let pre_hook_item_count = tools_to_execute.len() as u64;
    let pre_hook_span = perf_diagnostics::span(PerfComponent::ToolPreHook, None, None);
    for tc in &tools_to_execute {
        let args_value: Option<serde_json::Value> = tc
            .function
            .parse_args()
            .ok()
            .map(|m| serde_json::Value::Object(m.into_iter().collect()));
        let payload = HookPayload {
            hook_event_name: "PreToolUse".to_string(),
            session_id: session_id.clone(),
            project_dir: project_dir.clone(),
            tool_name: Some(tc.function.name.clone()),
            tool_input: args_value,
            tool_output: None,
            user_prompt: None,
            extra: std::collections::HashMap::new(),
        };
        let results = run_hooks(app.clone(), HookEvent::PreToolUse, payload).await;
        if let Some(reason) = first_block_reason(&results) {
            pre_hook_blocked_ids.insert(tc.id.clone());
            let block_message = ChatMessage {
                message_id: Uuid::new_v4().to_string(),
                role: "tool".to_string(),
                content: ChatContent::SimpleText(format!("Blocked by hook: {}", reason)),
                tool_call_id: tc.id.clone(),
                tool_failed: Some(true),
                ..Default::default()
            };
            let mut session = session_arc.lock().await;
            session.add_message(block_message);
        }
    }
    pre_hook_span.finish_tool(
        if pre_hook_blocked_ids.is_empty() {
            PerfOutcome::Success
        } else {
            PerfOutcome::Failure
        },
        pre_hook_item_count,
        pre_hook_item_count,
        Some(ToolExecutionClass::Serial),
    );
    if !pre_hook_blocked_ids.is_empty() {
        tools_to_execute.retain(|tc| !pre_hook_blocked_ids.contains(&tc.id));
    }

    if tools_to_execute.is_empty() {
        let mut session = session_arc.lock().await;
        session.release_turn_only_state();
        session.set_runtime_state(SessionState::Idle, None);
        drop(session);
        maybe_save_trajectory_background_with_intent(
            app.clone(),
            session_arc.clone(),
            TrajectoryCommitIntent::Checkpoint,
        );
        return ToolStepOutcome::Continue;
    }

    {
        let mut session = session_arc.lock().await;
        session.set_runtime_state(SessionState::ExecutingTools, None);
    }

    let tool_interrupt_flag = {
        let session = session_arc.lock().await;
        session.user_interrupt_flag.clone()
    };
    let execution_wait_span = perf_diagnostics::span(PerfComponent::ToolExecutionWait, None, None);
    let tool_execution = execute_tools_with_session(
        app.clone(),
        session_arc.clone(),
        &tools_to_execute,
        &messages,
        &thread,
        mode_id,
        model_id,
        ExecuteToolsOptions {
            allowed_tools: allowed_tools.clone(),
            catalog: Some(catalog),
            turn_tool_pool,
            ..Default::default()
        },
    );
    let (tool_results, _) = tokio::select! {
        result = tool_execution => result,
        _ = wait_for_tool_abort(session_arc.clone(), tool_interrupt_flag) => (Vec::new(), false),
    };
    execution_wait_span.finish_tool(PerfOutcome::Success, 1, tools_to_execute.len() as u64, None);

    let privacy_approvals = shell_privacy_approval_reasons(&tools_to_execute, &tool_results);
    if !privacy_approvals.is_empty() {
        let session_merge_span =
            perf_diagnostics::span(PerfComponent::ToolSessionMergeEvents, None, None);
        let mut session = session_arc.lock().await;
        for result_msg in tool_results {
            session.add_message(result_msg);
        }
        session.drain_post_tool_side_effects();
        session.set_paused_with_reasons_and_auto_approved(privacy_approvals, Vec::new(), None);
        session_merge_span.finish_tool(
            PerfOutcome::Success,
            1,
            tools_to_execute.len() as u64,
            None,
        );
        return ToolStepOutcome::Paused;
    }

    // Determine tool-requested final state before checking abort, since ask_questions,
    // task_done, and agent_finish set abort_flag=true as part of their normal operation to prevent
    // further LLM generation — but they still need their state transition applied.
    // Only apply stop state if the tool actually succeeded (tool_failed != Some(true)), otherwise
    // let the loop continue so the LLM can see the error and retry with correct arguments.
    let mut final_state = SessionState::Idle;
    let mut completion_trigger: Option<String> = None;
    for tool_call in &tools_to_execute {
        let failed = tool_results
            .iter()
            .any(|r| r.tool_call_id == tool_call.id && r.tool_failed == Some(true));
        if !failed {
            let tool_name =
                refact_llm::adapters::claude_code_compat::cc_normalize_internal_tool_name(
                    &tool_call.function.name,
                );
            match tool_name.as_str() {
                "ask_questions" | "wait_agents" => final_state = SessionState::WaitingUserInput,
                "task_done" | "finish" => {
                    final_state = SessionState::Completed;
                    completion_trigger = Some(tool_name.to_string());
                }
                "agent_finish" => {
                    final_state = SessionState::Completed;
                    completion_trigger = Some(tool_name.to_string());
                }
                "handoff_to_mode" => final_state = SessionState::Completed,
                _ => {}
            }
        }
    }

    let tool_initiated_stop = matches!(
        final_state,
        SessionState::Completed | SessionState::WaitingUserInput
    );

    let was_interrupted = {
        let session = session_arc.lock().await;
        session.user_interrupt_flag.load(Ordering::Relaxed)
    };

    // Check if we were aborted during tool execution (user stop or tool-initiated).
    let was_aborted = {
        let session = session_arc.lock().await;
        session.abort_flag.load(Ordering::Relaxed)
    };

    let session_merge_span =
        perf_diagnostics::span(PerfComponent::ToolSessionMergeEvents, None, None);
    let mut verify_completion = false;
    {
        let mut session = session_arc.lock().await;
        if !was_interrupted || tool_initiated_stop {
            for result_msg in tool_results {
                session.add_message(result_msg);
            }
            session.drain_post_tool_side_effects();
        } else {
            session.drain_post_tool_side_effects();
        }
        if tool_initiated_stop {
            if final_state == SessionState::Completed
                && completion_trigger.is_some()
                && should_verify_goal_on_done(&session)
            {
                session.set_runtime_state(SessionState::ExecutingTools, None);
                verify_completion = true;
            } else {
                session.set_runtime_state(final_state, None);
            }
        } else if was_aborted {
            // User abort during regular tools: transition to Idle so UI stops animating
            session.set_runtime_state(SessionState::Idle, None);
            session.release_turn_only_state();
        } else {
            session.release_turn_only_state();
            session.set_runtime_state(SessionState::Idle, None);
        }
    }
    session_merge_span.finish_tool(PerfOutcome::Success, 1, tools_to_execute.len() as u64, None);

    if verify_completion {
        let trigger = completion_trigger.as_deref().unwrap_or("task_done");
        match verify_goal_before_completion(app.clone(), session_arc.clone(), trigger).await {
            GoalCompletionGateOutcome::Passthrough => {
                let mut session = session_arc.lock().await;
                session.set_runtime_state(SessionState::Completed, None);
            }
            GoalCompletionGateOutcome::Finalized => {}
            GoalCompletionGateOutcome::Rearmed => {
                maybe_save_trajectory_with_intent(
                    app.clone(),
                    session_arc.clone(),
                    TrajectoryCommitIntent::Required,
                )
                .await;
                return ToolStepOutcome::Stop;
            }
            GoalCompletionGateOutcome::BudgetExhausted(_) => {}
            GoalCompletionGateOutcome::VerificationUnavailable
            | GoalCompletionGateOutcome::VerificationStalled => {
                let mut session = session_arc.lock().await;
                session.set_runtime_state(SessionState::Completed, None);
            }
            GoalCompletionGateOutcome::Aborted => return ToolStepOutcome::Stop,
        }
    }

    // Perform pending skill deactivation cleanup (compacts skill-run messages into a report)
    {
        let mut session = session_arc.lock().await;
        if session.pending_skill_deactivation.is_some() {
            session.perform_skill_deactivation_cleanup();
        }
    }

    let checkpoint_span =
        perf_diagnostics::span(PerfComponent::ToolCheckpointScheduling, None, None);
    if was_aborted || tool_initiated_stop {
        maybe_save_trajectory_with_intent(
            app.clone(),
            session_arc.clone(),
            TrajectoryCommitIntent::Required,
        )
        .await;
    } else {
        maybe_save_trajectory_background_with_intent(
            app.clone(),
            session_arc.clone(),
            TrajectoryCommitIntent::Checkpoint,
        );
    }
    checkpoint_span.finish_tool(PerfOutcome::Success, 1, 1, None);

    if was_aborted || tool_initiated_stop {
        ToolStepOutcome::Stop
    } else {
        ToolStepOutcome::Continue
    }
}

fn format_openai_server_tool_result(tc: &ChatToolCall) -> String {
    let mut out = String::new();
    out.push_str("## Server tool (OpenAI Responses)\n\n");
    out.push_str(&format!("**{}**\n\n", tc.function.name));

    // Best-effort pretty JSON of the tool call arguments (usually a whole output item).
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&tc.function.arguments) {
        if let Ok(pretty) = serde_json::to_string_pretty(&v) {
            out.push_str("### Raw item\n\n```json\n");
            out.push_str(&pretty);
            out.push_str("\n```\n");
            return out;
        }
    }

    out.push_str("### Raw item\n\n```\n");
    out.push_str(&tc.function.arguments);
    out.push_str("\n```\n");
    out
}

pub fn compute_final_action(
    tool_result: &MatchConfirmDenyResult,
    mode_action: Option<&str>,
    is_auto_approved: bool,
    tool_name: &str,
) -> &'static str {
    const ALWAYS_ASK_TOOLS: &[&str] = &["compress_chat_probe", "compress_chat_apply"];
    if *tool_result == MatchConfirmDenyResult::DENY {
        return "deny";
    }
    if matches!(mode_action, Some("deny")) {
        return "deny";
    }
    if tool_name == "handoff_to_mode" {
        return "auto";
    }
    if ALWAYS_ASK_TOOLS.iter().any(|name| *name == tool_name) {
        return "ask";
    }
    match mode_action {
        Some("deny") => "deny",
        _ if is_auto_approved => "auto",
        Some("ask") => "ask",
        Some("auto") => "auto",
        _ => match tool_result {
            MatchConfirmDenyResult::CONFIRMATION => "ask",
            MatchConfirmDenyResult::PASS => "auto",
            MatchConfirmDenyResult::DENY => "deny",
        },
    }
}

pub async fn check_tools_confirmation(
    app: AppState,
    tool_calls: &[refact_core::call_validation::ChatToolCall],
    messages: &[ChatMessage],
    thread: &ThreadParams,
    catalog: Option<&ToolCatalogSnapshot>,
    mode_id: &str,
    model_id: Option<&str>,
    allowed_tools: &[String],
    _source_command: &str,
) -> (Vec<PauseReason>, Vec<PauseReason>) {
    check_tools_confirmation_with_pool(
        app,
        tool_calls,
        messages,
        thread,
        catalog,
        mode_id,
        model_id,
        allowed_tools,
        _source_command,
        None,
    )
    .await
}

async fn check_tools_confirmation_with_pool(
    app: AppState,
    tool_calls: &[refact_core::call_validation::ChatToolCall],
    messages: &[ChatMessage],
    thread: &ThreadParams,
    catalog: Option<&ToolCatalogSnapshot>,
    mode_id: &str,
    model_id: Option<&str>,
    allowed_tools: &[String],
    _source_command: &str,
    turn_tool_pool: Option<&TurnToolPool>,
) -> (Vec<PauseReason>, Vec<PauseReason>) {
    let preflight_span =
        perf_diagnostics::span(PerfComponent::ToolConfirmationPreflight, None, None);
    let item_count = tool_calls.len() as u64;
    let mut confirmations = Vec::new();
    let mut denials = Vec::new();

    let ccx = Arc::new(AMutex::new(
        refact_runtime_api::at_commands_runtime::at_commands_new_from_app(
            app.clone(),
            1000,
            1,
            false,
            messages.to_vec(),
            thread.id.clone(),
            thread.root_chat_id.clone(),
            thread.model.clone(),
            thread.task_meta.clone(),
            thread.worktree.clone(),
        )
        .await,
    ));

    let mode_id = map_legacy_mode_to_id(mode_id);
    let policy_span = perf_diagnostics::span(PerfComponent::ToolPolicyLookup, None, None);
    let mode_config = get_mode_config(app.gcx.clone(), mode_id, model_id).await;
    let tool_confirm_rules = mode_config
        .as_ref()
        .map(|m| m.tool_confirm.rules.as_slice())
        .unwrap_or(&[]);
    policy_span.finish_tool(
        PerfOutcome::Success,
        1,
        tool_confirm_rules.len() as u64,
        None,
    );

    for tool_call in tool_calls {
        if thread.autonomous_no_confirm && !allowed_tools.is_empty() {
            let resolved = refact_llm::adapters::claude_code_compat::cc_resolve_tool_name(
                &tool_call.function.name,
            );
            let in_allowlist = allowed_tools
                .iter()
                .any(|t| t == &tool_call.function.name || t == &resolved);
            if !in_allowlist {
                denials.push(PauseReason {
                    reason_type: "denial".to_string(),
                    tool_name: tool_call.function.name.clone(),
                    command: tool_call.function.name.clone(),
                    rule: format!(
                        "Tool `{}` is not allowed in this autonomous workflow",
                        tool_call.function.name
                    ),
                    tool_call_id: tool_call.id.clone(),
                    integr_config_path: None,
                });
                continue;
            }
        }

        let args = match tool_call.function.parse_args() {
            Ok(a) => serde_json::Map::from_iter(a.into_iter()),
            Err(e) => {
                denials.push(PauseReason {
                    reason_type: "denial".to_string(),
                    tool_name: tool_call.function.name.clone(),
                    command: tool_call.function.name.clone(),
                    rule: format!("Failed to parse arguments: {}", e),
                    tool_call_id: tool_call.id.clone(),
                    integr_config_path: None,
                });
                continue;
            }
        };

        if matches!(tool_call.function.name.as_str(), "shell" | "process_start") {
            if let Err(error) = escalation_from_args(&args.clone().into_iter().collect()) {
                denials.push(PauseReason {
                    reason_type: "denial".to_string(),
                    tool_name: tool_call.function.name.clone(),
                    command: tool_call.function.name.clone(),
                    rule: error,
                    tool_call_id: tool_call.id.clone(),
                    integr_config_path: None,
                });
                continue;
            }
        }

        let Some(tool_result) = (match catalog {
            Some(catalog) => {
                app.tool_registry
                    .check_tool_confirmation_with_catalog_and_pool(
                        &ccx,
                        catalog,
                        turn_tool_pool,
                        mode_id,
                        model_id,
                        &tool_call.function.name,
                        args,
                    )
                    .await
            }
            None => {
                app.tool_registry
                    .check_tool_confirmation(
                        &ccx,
                        mode_id,
                        model_id,
                        &tool_call.function.name,
                        args,
                    )
                    .await
            }
        }) else {
            info!(
                "Unknown tool: {}, skipping confirmation check",
                tool_call.function.name
            );
            continue;
        };
        let mode_action = match_tool_confirm_action(tool_confirm_rules, &tool_call.function.name);

        match tool_result {
            Ok(result) => {
                let integr_config_path = result.integr_config_path.clone();
                let resolved_tool_name = result.tool_name.clone();
                if result.result.result == MatchConfirmDenyResult::DENY {
                    denials.push(PauseReason {
                        reason_type: "denial".to_string(),
                        tool_name: tool_call.function.name.clone(),
                        command: result.result.command,
                        rule: result.result.rule,
                        tool_call_id: tool_call.id.clone(),
                        integr_config_path,
                    });
                    continue;
                }

                let is_auto_approved =
                    should_auto_approve_confirmation(thread, &tool_call.function.name)
                        || (!allowed_tools.is_empty()
                            && (allowed_tools.contains(&tool_call.function.name)
                                || allowed_tools.contains(&resolved_tool_name)));
                let final_action = compute_final_action(
                    &result.result.result,
                    mode_action.as_deref(),
                    is_auto_approved,
                    &tool_call.function.name,
                );

                let rule_text = match mode_action.as_deref() {
                    Some(action) => format!("mode policy: {}", action),
                    None => result.result.rule.clone(),
                };

                match final_action {
                    "deny" => {
                        denials.push(PauseReason {
                            reason_type: "denial".to_string(),
                            tool_name: tool_call.function.name.clone(),
                            command: result.result.command,
                            rule: rule_text,
                            tool_call_id: tool_call.id.clone(),
                            integr_config_path: integr_config_path.clone(),
                        });
                    }
                    "ask" => {
                        confirmations.push(PauseReason {
                            reason_type: "confirmation".to_string(),
                            tool_name: tool_call.function.name.clone(),
                            command: result.result.command,
                            rule: rule_text,
                            tool_call_id: tool_call.id.clone(),
                            integr_config_path: integr_config_path.clone(),
                        });
                    }
                    _ => {}
                }
            }
            Err(e) => {
                denials.push(PauseReason {
                    reason_type: "denial".to_string(),
                    tool_name: tool_call.function.name.clone(),
                    command: tool_call.function.name.clone(),
                    rule: format!("confirmation check failed: {}", e),
                    tool_call_id: tool_call.id.clone(),
                    integr_config_path: None,
                });
            }
        }
    }

    preflight_span.finish_tool(PerfOutcome::Success, item_count, item_count, None);
    (confirmations, denials)
}

pub async fn execute_tools_with_session(
    app: AppState,
    session_arc: Arc<AMutex<ChatSession>>,
    tool_calls: &[ChatToolCall],
    messages: &[ChatMessage],
    thread: &ThreadParams,
    mode_id: &str,
    model_id: Option<&str>,
    mut options: ExecuteToolsOptions,
) -> (Vec<ChatMessage>, bool) {
    if tool_calls.is_empty() {
        return (vec![], false);
    }

    let (
        prompt_messages,
        session_abort_flag,
        session_allowed_tools,
        session_catalog,
        session_turn_tool_pool,
    ) = {
        let session = session_arc.lock().await;
        let msgs = tool_execution_messages(messages, &session.last_prompt_messages);
        (
            msgs,
            session.abort_flag.clone(),
            session.active_command.allowed_tools.clone(),
            session.tool_catalog.clone(),
            session.turn_tool_pool.clone(),
        )
    };
    if options.allowed_tools.is_empty() {
        options.allowed_tools = session_allowed_tools;
    }
    let catalog = match options.catalog.clone().or(session_catalog) {
        Some(catalog) => catalog,
        None => {
            app.tool_registry
                .acquire_tool_catalog(
                    mode_id,
                    model_id,
                    execution_scope_for_thread(&thread).as_deref(),
                )
                .await
        }
    };
    if options.turn_tool_pool.is_none() {
        options.turn_tool_pool = session_turn_tool_pool;
    }
    if options.turn_tool_pool.is_none() {
        options.turn_tool_pool =
            acquire_session_turn_tool_pool(&app, &session_arc, thread, mode_id, model_id, &catalog)
                .await;
    }

    let n_ctx = get_effective_n_ctx(app.clone(), thread).await;
    let budget = match ToolBudget::try_from_n_ctx(n_ctx) {
        Ok(b) => b,
        Err(e) => {
            let error_messages: Vec<ChatMessage> = tool_calls
                .iter()
                .map(|tc| ChatMessage {
                    message_id: Uuid::new_v4().to_string(),
                    role: "tool".to_string(),
                    content: ChatContent::SimpleText(format!("Error: {}", e)),
                    tool_call_id: tc.id.clone(),
                    tool_failed: Some(true),
                    ..Default::default()
                })
                .collect();
            return (error_messages, false);
        }
    };

    let ccx = match build_tool_execution_context(
        app.clone(),
        n_ctx,
        &prompt_messages,
        thread,
        Some(session_abort_flag),
    )
    .await
    {
        Ok(context) => context,
        Err(error) => return context_error_results(tool_calls, error),
    };

    {
        let mut cgcx = ccx.lock().await;
        cgcx.tokens_for_rag = (n_ctx / 2).max(4096);
        if let Some(ref params) = options.subchat_tool_parameters {
            cgcx.subchat_tool_parameters = params.clone();
        }
    }

    let subchat_bridge = spawn_subchat_bridge(ccx.clone(), session_arc.clone());

    let turn_tool_pool = options.turn_tool_pool.clone();
    let result = execute_tools_inner(
        app.clone(),
        ccx,
        tool_calls,
        mode_id,
        model_id,
        budget,
        options,
        &prompt_messages,
        catalog,
        turn_tool_pool,
    )
    .await;

    subchat_bridge.shutdown().await;

    let context_files = get_context_files_from_messages(&result.0);
    if !context_files.is_empty() {
        let mut session = session_arc.lock().await;
        session.mark_tool_progress();
        for tc in tool_calls {
            session.emit(ChatEvent::SubchatUpdate {
                tool_call_id: tc.id.clone(),
                subchat_id: "/tool:files".to_string(),
                attached_files: context_files.clone(),
            });
        }
    }

    result
}

async fn wait_for_tool_abort(session_arc: Arc<AMutex<ChatSession>>, abort_flag: Arc<AtomicBool>) {
    loop {
        if abort_flag.load(Ordering::Relaxed) {
            return;
        }

        let notify = {
            let session = session_arc.lock().await;
            session.queue_notify.clone()
        };
        notify.notified().await;
    }
}

/// Owns the runtime wait marker even if execution errors, panics or is cancelled.
pub struct InterruptibleWaitGuard(Option<Arc<AMutex<ChatSession>>>);

impl InterruptibleWaitGuard {
    pub async fn begin(session: Arc<AMutex<ChatSession>>) -> Self {
        session.lock().await.begin_interruptible_wait();
        Self(Some(session))
    }

    pub async fn finish(&mut self) {
        if let Some(session) = &self.0 {
            session.lock().await.end_interruptible_wait();
        }
        self.0 = None;
    }
}

impl Drop for InterruptibleWaitGuard {
    fn drop(&mut self) {
        if let Some(session) = self.0.take() {
            if let Ok(mut locked) = session.try_lock() {
                locked.end_interruptible_wait();
                return;
            }
            tokio::spawn(async move {
                session.lock().await.end_interruptible_wait();
            });
        }
    }
}

type SerialToolRegistry = std::collections::HashSet<String>;

/// Decide what the loop guard says about one completed tool call, without touching
/// any session.
///
/// Pure and session-free so the whole policy can be exercised by a unit test
/// without constructing an `AppState` or a `ChatSession`. Returns `None` — meaning
/// "behave exactly as if the guard did not exist" — when:
///
/// * the guard is disabled,
/// * the call did not succeed (a failed call produced no new output, so counting it
///   would punish the agent for a transient error), or
/// * the tool is an interruptible wait tool. Polling `process_wait` is *correct*
///   behaviour and its output legitimately repeats until the process finishes, so
///   exempting it is what keeps the guard from breaking normal waiting.
fn loop_guard_next(
    guard: &mut refact_tool_api::tool_call_loop_guard::ToolCallLoopGuard,
    enabled: bool,
    canonical_name: &str,
    identity_key: &str,
    result_digest: u64,
    threshold: usize,
    success: bool,
) -> Option<refact_tool_api::tool_call_loop_guard::LoopVerdict> {
    if !enabled || !success {
        return None;
    }
    if refact_tool_api::is_interruptible_wait_tool_name(canonical_name) {
        return None;
    }
    Some(guard.observe(identity_key, result_digest, threshold))
}

/// Turn a guard verdict into the messages and context files the model will see.
///
/// * `None` and `FirstCall` return the input untouched, byte for byte. That is the
///   guarantee that ordinary tool use is completely unaffected by this guard.
/// * `Warn` keeps the real result and prepends a warning to the tool message, so
///   the agent sees both the answer and the nudge.
/// * `Block` withholds the result entirely: one `tool_failed` message explaining the
///   block, and **no** `ContextFile`. Forgetting the latter would leak the very
///   content the block is meant to stop the agent from re-reading.
fn apply_loop_guard(
    verdict: Option<refact_tool_api::tool_call_loop_guard::LoopVerdict>,
    tool_name: &str,
    threshold: usize,
    tool_call_id: &str,
    had_corrections: bool,
    msgs: Vec<ChatMessage>,
    files: Vec<ContextFile>,
) -> (bool, Vec<ChatMessage>, Vec<ContextFile>) {
    use refact_tool_api::tool_call_loop_guard::{loop_block_text, loop_warning_text, LoopVerdict};

    let mut msgs = msgs;
    match verdict {
        None | Some(LoopVerdict::FirstCall) => (had_corrections, msgs, files),
        Some(LoopVerdict::Warn { consecutive }) => {
            let warning = loop_warning_text(tool_name, consecutive, threshold);
            // Index first, then borrow: `iter_mut().find(..).or_else(|| first_mut())`
            // would be two mutable borrows of `msgs` in one expression (E0499).
            let target = msgs
                .iter()
                .position(|message| message.role == "tool")
                .or(if msgs.is_empty() { None } else { Some(0) });
            if let Some(index) = target {
                let prefix = format!("{warning}\n\n");
                match &mut msgs[index].content {
                    ChatContent::SimpleText(text) => {
                        text.insert_str(0, &prefix);
                    }
                    _ => {
                        msgs[index].content = ChatContent::SimpleText(format!(
                            "{prefix}{}",
                            msgs[index].content.content_text_only()
                        ));
                    }
                }
            } else {
                // No message to attach to: warn on its own rather than lose the nudge.
                msgs.push(ChatMessage {
                    message_id: Uuid::new_v4().to_string(),
                    role: "tool".to_string(),
                    content: ChatContent::SimpleText(warning),
                    tool_call_id: tool_call_id.to_string(),
                    ..Default::default()
                });
            }
            (had_corrections, msgs, files)
        }
        Some(LoopVerdict::Block { consecutive }) => {
            let blocked = ChatMessage {
                message_id: Uuid::new_v4().to_string(),
                role: "tool".to_string(),
                content: ChatContent::SimpleText(loop_block_text(
                    tool_name,
                    consecutive,
                    threshold,
                )),
                tool_call_id: tool_call_id.to_string(),
                tool_failed: Some(true),
                ..Default::default()
            };
            (true, vec![blocked], Vec::new())
        }
    }
}

/// Feed one completed tool result through the per-chat loop guard and rewrite the
/// result if the guard warns or blocks.
///
/// Any condition that stops the guard from having an opinion returns the input
/// untouched: the guard disabled, no session for this chat, an unknown tool name,
/// or an exempt wait tool. `execute_single_tool` is on the hot path for every tool
/// call, so those paths must stay cheap and must never fail the call.
async fn loop_guard_apply_to_result(
    app: &AppState,
    chat_id: &str,
    catalog: &ToolCatalogSnapshot,
    tool_call: &ChatToolCall,
    had_corrections: bool,
    msgs: Vec<ChatMessage>,
    files: Vec<ContextFile>,
) -> (bool, Vec<ChatMessage>, Vec<ContextFile>) {
    let settings = refact_runtime_api::runtime_settings::current();
    if !settings.tool_loop_guard_enabled {
        return (had_corrections, msgs, files);
    }

    // Canonical name: the catalog knows the real tool name, but a Claude-Code
    // compatible alias may be what the model emitted. Resolve both spellings so the
    // identity key (and therefore the `cat` override) is keyed by the real name.
    let raw_name = tool_call.function.name.as_str();
    let canonical_name = match catalog
        .index
        .tools
        .iter()
        .find(|desc| {
            desc.name == raw_name
                || desc.name
                    == refact_llm::adapters::claude_code_compat::cc_resolve_tool_name(raw_name)
        })
        .map(|desc| desc.name.clone())
    {
        Some(name) => name,
        // Unknown tool: it did not run, so there is nothing to observe.
        None => return (had_corrections, msgs, files),
    };

    if refact_tool_api::is_interruptible_wait_tool_name(&canonical_name) {
        return (had_corrections, msgs, files);
    }

    let identity_key = refact_tool_api::tool_call_loop_guard::identity_key_for(
        &canonical_name,
        &tool_call.function.arguments,
    );
    let digest = refact_tool_api::tool_call_loop_guard::result_digest(&msgs, &files);

    // Clone the Arc and drop the map guard BEFORE locking the session: holding the
    // sessions read lock across the session lock (or any IO) risks a deadlock and
    // would stall every other chat.
    let session_arc = {
        let sessions = app.chat.sessions.read().await;
        sessions.get(chat_id).cloned()
    };
    let Some(session_arc) = session_arc else {
        return (had_corrections, msgs, files);
    };

    // A result message marked `tool_failed` did not succeed, so it made no progress
    // and must not advance the counter.
    let success = !msgs.iter().any(|message| message.tool_failed == Some(true));
    let (verdict, tracker_block_reason) = {
        let mut session = session_arc.lock().await;
        let verdict = loop_guard_next(
            &mut session.tool_loop_guard,
            settings.tool_loop_guard_enabled,
            &canonical_name,
            &identity_key,
            digest,
            settings.tool_loop_guard_threshold,
            success,
        );
        if settings.tool_loop_hard_stop_on_block
            && matches!(
                verdict,
                Some(refact_tool_api::tool_call_loop_guard::LoopVerdict::Block { .. })
            )
        {
            session.turn_investigation_tracker.force_block();
        }
        let tracker_block_reason = if success {
            session.turn_investigation_tracker.record(
                &canonical_name,
                &identity_key,
                settings.max_investigation_tool_calls_per_turn,
                settings.tool_loop_stagnation_detector_enabled,
                settings.tool_loop_stagnation_threshold,
            )
        } else {
            None
        };
        (verdict, tracker_block_reason)
    };

    if let Some(reason) = tracker_block_reason {
        use refact_tool_api::tool_call_loop_guard::{
            stagnation_blocked_text, BlockReason, INVESTIGATION_BUDGET_EXHAUSTED_TEXT,
        };
        let text = match reason {
            BlockReason::BudgetExhausted => INVESTIGATION_BUDGET_EXHAUSTED_TEXT.to_string(),
            BlockReason::Stagnation { consecutive } => {
                stagnation_blocked_text(&canonical_name, consecutive as usize)
            }
        };
        let blocked = ChatMessage {
            message_id: Uuid::new_v4().to_string(),
            role: "tool".to_string(),
            content: ChatContent::SimpleText(text),
            tool_call_id: tool_call.id.to_string(),
            tool_failed: Some(true),
            ..Default::default()
        };
        return (true, vec![blocked], Vec::new());
    }

    apply_loop_guard(
        verdict,
        &canonical_name,
        settings.tool_loop_guard_threshold,
        &tool_call.id,
        had_corrections,
        msgs,
        files,
    )
}

pub async fn execute_single_tool(
    app: AppState,
    ccx: Arc<AMutex<AtCommandsContext>>,
    idx: usize,
    tool_call: ChatToolCall,
    catalog: Arc<ToolCatalogSnapshot>,
    turn_tool_pool: Option<TurnToolPool>,
    serial_registry: Arc<SerialToolRegistry>,
    allow_parallel: bool,
    batch_size: usize,
    mode_id: &str,
    model_id: Option<&str>,
) -> (usize, bool, Vec<ChatMessage>, Vec<ContextFile>) {
    let abort_flag = {
        let cgcx = ccx.lock().await;
        cgcx.abort_flag.clone()
    };
    if abort_flag.load(Ordering::Relaxed) {
        return (
            idx,
            false,
            vec![skipped_tool_result(&tool_call)],
            Vec::new(),
        );
    }

    let args: std::collections::HashMap<String, serde_json::Value> =
        match tool_call.function.parse_args() {
            Ok(a) => a,
            Err(e) => {
                return (
                    idx,
                    true, // had_corrections: parse error is a correction
                    vec![ChatMessage {
                        message_id: Uuid::new_v4().to_string(),
                        role: "tool".to_string(),
                        content: ChatContent::SimpleText(format!("Error parsing arguments: {}", e)),
                        tool_call_id: tool_call.id.clone(),
                        tool_failed: Some(true),
                        ..Default::default()
                    }],
                    vec![],
                );
            }
        };

    info!("Executing tool: {}({:?})", tool_call.function.name, args);

    let session_id = {
        let cgcx = ccx.lock().await;
        cgcx.chat_id.clone()
    };

    if !allow_parallel {
        let resolved_name = refact_llm::adapters::claude_code_compat::cc_resolve_tool_name(
            &tool_call.function.name,
        );
        if !serial_registry.contains(&tool_call.function.name)
            && !serial_registry.contains(resolved_name.as_str())
        {
            return (
                idx,
                true,
                vec![ChatMessage {
                    message_id: Uuid::new_v4().to_string(),
                    role: "tool".to_string(),
                    content: ChatContent::SimpleText(format!(
                        "Error: tool '{}' not found",
                        tool_call.function.name
                    )),
                    tool_call_id: tool_call.id.clone(),
                    tool_failed: Some(true),
                    ..Default::default()
                }],
                vec![],
            );
        }
    }

    if let Some(session_arc) = {
        let sessions_read = app.chat.sessions.read().await;
        sessions_read.get(&session_id).cloned()
    } {
        let mut session = session_arc.lock().await;
        session.mark_tool_started();
        session.stamp_tool_call_timing(&tool_call.id, tool_timestamp_ms(), None);
    }

    let interruptible = catalog
        .index
        .tools
        .iter()
        .any(|desc| desc.name == tool_call.function.name && desc.is_interruptible_wait());
    let mut wait_guard = if interruptible {
        let session = app.chat.sessions.read().await.get(&session_id).cloned();
        match session {
            Some(session) => Some(InterruptibleWaitGuard::begin(session).await),
            None => None,
        }
    } else {
        None
    };
    let started = std::time::Instant::now();
    let execution_future = app.tool_registry.execute_tool_with_catalog_and_pool(
        &ccx,
        &catalog,
        turn_tool_pool.as_ref(),
        mode_id,
        model_id,
        &tool_call.id,
        &tool_call.function.name,
        serde_json::Map::from_iter(args.into_iter()),
    );
    // Sleep must finish cooperatively so its accumulated ticks are not discarded.
    let execution = if interruptible && tool_call.function.name != "sleep" {
        let interrupt = refact_runtime_api::WaitInterrupt::from_context(&ccx).await;
        tokio::select! {
            result = execution_future => result,
            _ = interrupt.wait() => Ok(Some(refact_runtime_api::ToolExecutionResult {
                had_corrections: false,
                messages: vec![ChatMessage {
                    role: "tool".into(),
                    tool_call_id: tool_call.id.clone(),
                    tool_failed: Some(false),
                    content: ChatContent::SimpleText(serde_json::json!({
                        "interrupted": true,
                        "reason": interrupt.reason(started.elapsed().as_secs_f64()),
                    }).to_string()),
                    ..Default::default()
                }],
                context_files: vec![],
            })),
        }
    } else {
        execution_future.await
    };
    if let Some(guard) = &mut wait_guard {
        guard.finish().await;
    }

    if let Some(session_arc) = {
        let sessions_read = app.chat.sessions.read().await;
        sessions_read.get(&session_id).cloned()
    } {
        let mut session = session_arc.lock().await;
        session.stamp_tool_call_timing(&tool_call.id, None, tool_timestamp_ms());
    }

    let (idx, had_corrections, mut msgs, files) = match execution {
        Ok(Some(result)) => (
            idx,
            result.had_corrections,
            result.messages,
            result.context_files,
        ),
        Ok(None) => {
            return (
                idx,
                true,
                vec![ChatMessage {
                    message_id: Uuid::new_v4().to_string(),
                    role: "tool".to_string(),
                    content: ChatContent::SimpleText(format!(
                        "Error: tool '{}' not found",
                        tool_call.function.name
                    )),
                    tool_call_id: tool_call.id.clone(),
                    tool_failed: Some(true),
                    ..Default::default()
                }],
                vec![],
            );
        }
        Err(e) => {
            info!("Tool execution failed: {}: {}", tool_call.function.name, e);
            return (
                idx,
                true,
                vec![ChatMessage {
                    message_id: Uuid::new_v4().to_string(),
                    role: "tool".to_string(),
                    content: ChatContent::SimpleText(format!("Error: {}", e)),
                    tool_call_id: tool_call.id.clone(),
                    tool_failed: Some(true),
                    ..Default::default()
                }],
                vec![],
            );
        }
    };

    let execution_class = if allow_parallel {
        ToolExecutionClass::Parallel
    } else {
        ToolExecutionClass::Serial
    };

    for msg in &mut msgs {
        if msg.message_id.is_empty() {
            msg.message_id = Uuid::new_v4().to_string();
        }
        if msg.tool_failed.is_none() {
            msg.tool_failed = Some(false);
        }
    }

    let tool_output_text = msgs
        .iter()
        .filter(|m| m.role == "tool")
        .map(|m| m.content.content_text_only())
        .collect::<Vec<_>>()
        .join("\n");

    let project_dir = get_project_dir_string(app.clone()).await;
    let args_value: Option<serde_json::Value> = tool_call
        .function
        .parse_args()
        .ok()
        .map(|m| serde_json::Value::Object(m.into_iter().collect()));
    let post_payload = HookPayload {
        hook_event_name: "PostToolUse".to_string(),
        // Cloned: the loop guard below still needs `session_id`, and the payload owns it.
        session_id: session_id.clone(),
        project_dir,
        tool_name: Some(tool_call.function.name.clone()),
        tool_input: args_value,
        tool_output: Some(tool_output_text),
        user_prompt: None,
        extra: std::collections::HashMap::new(),
    };
    let post_hook_span = perf_diagnostics::span(PerfComponent::ToolPostHook, None, None);
    let post_results = run_hooks(app.clone(), HookEvent::PostToolUse, post_payload).await;
    if let Some(reason) = first_block_reason(&post_results) {
        post_hook_span.finish_tool(
            PerfOutcome::Failure,
            batch_size as u64,
            1,
            Some(execution_class),
        );
        return (
            idx,
            true,
            vec![ChatMessage {
                message_id: Uuid::new_v4().to_string(),
                role: "tool".to_string(),
                content: ChatContent::SimpleText(format!("Blocked by hook: {}", reason)),
                tool_call_id: tool_call.id.clone(),
                tool_failed: Some(true),
                ..Default::default()
            }],
            vec![],
        );
    }
    post_hook_span.finish_tool(
        PerfOutcome::Success,
        batch_size as u64,
        1,
        Some(execution_class),
    );

    // The single central observation point for the loop guard. Every tool — builtin,
    // integration or MCP, serial or parallel — funnels through this function, so one
    // call here covers all of them and no individual tool file needs to know the
    // guard exists. It runs after the post-tool hook so a hook-blocked result is
    // already gone from the path.
    let (had_corrections, msgs, files) = loop_guard_apply_to_result(
        &app,
        &session_id,
        &catalog,
        &tool_call,
        had_corrections,
        msgs,
        files,
    )
    .await;

    (idx, had_corrections, msgs, files)
}

async fn execute_tools_inner(
    app: AppState,
    ccx: Arc<AMutex<AtCommandsContext>>,
    tool_calls: &[ChatToolCall],
    mode_id: &str,
    model_id: Option<&str>,
    budget: ToolBudget,
    options: ExecuteToolsOptions,
    messages: &[ChatMessage],
    catalog: Arc<ToolCatalogSnapshot>,
    turn_tool_pool: Option<TurnToolPool>,
) -> (Vec<ChatMessage>, bool) {
    let policy_span = perf_diagnostics::span(PerfComponent::ToolPolicyLookup, None, None);
    let available_tools = &catalog.policy;
    policy_span.finish_tool(PerfOutcome::Success, 1, available_tools.len() as u64, None);

    let mut tool_allow_parallel: std::collections::HashMap<String, bool> =
        std::collections::HashMap::new();
    let mut serial_registry = SerialToolRegistry::new();

    for tool in available_tools {
        tool_allow_parallel.insert(tool.name.clone(), tool.effective_allow_parallel);

        // Parallel tools are instantiated per call (no shared mutex).
        // Sequential tools are cached and protected by a single mutex.
        if tool.effective_allow_parallel {
            continue;
        }
        serial_registry.insert(tool.name.clone());
    }

    let serial_registry = Arc::new(serial_registry);
    let max_parallel_tools = refact_chat_history::config::limits().max_parallel_tools;

    let supervision_chat_id = ccx.lock().await.chat_id.clone();
    let supervision_calls = tool_calls
        .iter()
        .map(|tool_call| {
            let args = tool_call
                .function
                .parse_args()
                .map(|args| serde_json::Value::Object(args.into_iter().collect()))
                .unwrap_or_else(|_| {
                    serde_json::Value::String(tool_call.function.arguments.clone())
                });
            let test_command =
                matches!(tool_call.function.name.as_str(), "shell" | "process_start")
                    && args.to_string().to_lowercase().contains("test");
            (tool_call.clone(), args, test_command)
        })
        .collect::<Vec<_>>();
    for (tool_call, args, test_command) in &supervision_calls {
        app.buddy
            .supervision
            .observe_non_fatal(
                &supervision_chat_id,
                refact_buddy_core::supervision::SupervisionKind::ToolCall {
                    tool_name: refact_buddy_core::supervision::SupervisionReference::new(
                        &tool_call.function.name,
                    ),
                    normalized_args_hash:
                        refact_buddy_core::supervision::SupervisionHash::normalized_arguments(args),
                },
            )
            .await;
        if *test_command {
            app.buddy
                .supervision
                .observe_non_fatal(
                    &supervision_chat_id,
                    refact_buddy_core::supervision::SupervisionKind::Test {
                        status: refact_buddy_core::supervision::TestStatus::Running,
                    },
                )
                .await;
        }
    }
    if !supervision_calls.is_empty() {
        app.buddy
            .supervision
            .observe_non_fatal(
                &supervision_chat_id,
                refact_buddy_core::supervision::SupervisionKind::Process {
                    facts: refact_buddy_core::supervision::ProcessFacts {
                        process_id: refact_buddy_core::supervision::SupervisionReference::new(
                            "chat-tools",
                        ),
                        state: refact_buddy_core::supervision::ProcessState::Running,
                        heartbeat_at: refact_runtime_api::buddy_supervision::epoch_secs(),
                    },
                },
            )
            .await;
    }

    let session_arc_opt: Option<Arc<AMutex<refact_chat_api::types::ChatSession>>> = {
        let cgcx = ccx.lock().await;
        let sessions = app.chat.sessions.read().await;
        sessions.get(&cgcx.chat_id).cloned()
    };

    let mut all_results: Vec<(usize, bool, Vec<ChatMessage>, Vec<ContextFile>)> = Vec::new();
    let mut current_parallel_batch: Vec<(usize, ChatToolCall)> = Vec::new();
    let mut prepared_parallel_slots: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();

    for (idx, tool_call) in tool_calls.iter().enumerate() {
        let is_aborted = {
            let cgcx = ccx.lock().await;
            cgcx.abort_flag.load(Ordering::Relaxed)
        };
        if is_aborted {
            all_results.push((
                idx,
                false,
                vec![skipped_tool_result(tool_call)],
                Vec::new(),
            ));
            continue;
        }

        let resolved_name_pre = refact_llm::adapters::claude_code_compat::cc_resolve_tool_name(
            &tool_call.function.name,
        );
        if let Some(session_arc) = session_arc_opt.as_ref() {
            let (tracker_blocked, hard_stopped) = {
                let session = session_arc.lock().await;
                (
                    session.turn_investigation_tracker.is_blocked(),
                    session.turn_investigation_tracker.is_hard_stopped(),
                )
            };
            if hard_stopped
                || (tracker_blocked
                    && refact_tool_api::tool_call_loop_guard::is_investigation_tool(
                        &resolved_name_pre,
                    ))
            {
                let text = if hard_stopped {
                    "Repetitive tool calls blocked by the loop guard. Do not repeat the same calls — synthesize what you already have and complete or change your approach now.".to_string()
                } else {
                    refact_tool_api::tool_call_loop_guard::INVESTIGATION_BUDGET_EXHAUSTED_TEXT
                        .to_string()
                };
                all_results.push((
                    idx,
                    true,
                    vec![ChatMessage {
                        message_id: Uuid::new_v4().to_string(),
                        role: "tool".to_string(),
                        content: ChatContent::SimpleText(text),
                        tool_call_id: tool_call.id.clone(),
                        tool_failed: Some(true),
                        ..Default::default()
                    }],
                    vec![],
                ));
                continue;
            }
        }

        let options_allowed_tools = &options.allowed_tools;
        if !options_allowed_tools.is_empty() {
            let resolved = refact_llm::adapters::claude_code_compat::cc_resolve_tool_name(
                &tool_call.function.name,
            );
            let in_allowlist = options_allowed_tools
                .iter()
                .any(|t| t == &tool_call.function.name || t == &resolved);
            if !in_allowlist {
                all_results.push((
                    idx,
                    false,
                    vec![ChatMessage {
                        message_id: Uuid::new_v4().to_string(),
                        role: "tool".to_string(),
                        content: ChatContent::SimpleText(format!(
                            "Tool `{}` is not allowed in this autonomous workflow. Allowed: {}",
                            tool_call.function.name,
                            options_allowed_tools.join(", ")
                        )),
                        tool_call_id: tool_call.id.clone(),
                        tool_failed: Some(true),
                        ..Default::default()
                    }],
                    vec![],
                ));
                continue;
            }
        }

        let resolved_name = refact_llm::adapters::claude_code_compat::cc_resolve_tool_name(
            &tool_call.function.name,
        );
        let allow_parallel = tool_allow_parallel
            .get(&tool_call.function.name)
            .or_else(|| tool_allow_parallel.get(resolved_name.as_str()))
            .copied()
            .unwrap_or(false);

        if allow_parallel {
            current_parallel_batch.push((idx, tool_call.clone()));
            if current_parallel_batch.len() >= max_parallel_tools {
                let batch_results = execute_parallel_batch(
                    app.clone(),
                    ccx.clone(),
                    &current_parallel_batch,
                    catalog.clone(),
                    turn_tool_pool.clone(),
                    &mut prepared_parallel_slots,
                    serial_registry.clone(),
                    mode_id,
                    model_id,
                )
                .await;
                all_results.extend(batch_results);
                current_parallel_batch.clear();
            }
        } else {
            if !current_parallel_batch.is_empty() {
                let batch_results = execute_parallel_batch(
                    app.clone(),
                    ccx.clone(),
                    &current_parallel_batch,
                    catalog.clone(),
                    turn_tool_pool.clone(),
                    &mut prepared_parallel_slots,
                    serial_registry.clone(),
                    mode_id,
                    model_id,
                )
                .await;
                all_results.extend(batch_results);
                current_parallel_batch.clear();
            }

            let result = execute_single_tool(
                app.clone(),
                ccx.clone(),
                idx,
                tool_call.clone(),
                catalog.clone(),
                turn_tool_pool.clone(),
                serial_registry.clone(),
                false,
                1,
                mode_id,
                model_id,
            )
            .await;
            all_results.push(result);
        }
    }

    if !current_parallel_batch.is_empty() {
        let batch_results = execute_parallel_batch(
            app.clone(),
            ccx.clone(),
            &current_parallel_batch,
            catalog,
            turn_tool_pool,
            &mut prepared_parallel_slots,
            serial_registry.clone(),
            mode_id,
            model_id,
        )
        .await;
        all_results.extend(batch_results);
    }

    let merge_span = perf_diagnostics::span(PerfComponent::ToolResultMerge, None, None);
    all_results.sort_by_key(|(idx, _, _, _)| *idx);

    let mut tool_messages: Vec<ChatMessage> = Vec::new();
    let mut context_files: Vec<ContextFile> = Vec::new();
    let mut any_corrections = false;
    for (_, had_corrections, msgs, files) in all_results {
        if had_corrections {
            any_corrections = true;
        }
        tool_messages.extend(msgs);
        context_files.extend(files);
    }
    let merged_item_count = tool_messages.len().saturating_add(context_files.len()) as u64;
    merge_span.finish_tool(
        PerfOutcome::Success,
        tool_calls.len() as u64,
        merged_item_count,
        None,
    );

    let pp_settings = options.postprocess_settings.unwrap_or_default();

    let truncation_exempt_ids: std::collections::HashSet<String> = tool_calls
        .iter()
        .filter(|tc| {
            let resolved =
                refact_llm::adapters::claude_code_compat::cc_resolve_tool_name(&tc.function.name);
            is_tool_result_truncation_exempt(&tc.function.name)
                || is_tool_result_truncation_exempt(resolved.as_str())
        })
        .map(|tc| tc.id.clone())
        .collect();

    let results = postprocess_tool_results(
        app.gcx.clone(),
        None,
        tool_messages,
        context_files,
        budget,
        pp_settings,
        messages,
        &truncation_exempt_ids,
    )
    .await;

    for (tool_call, _, test_command) in &supervision_calls {
        let matching = results
            .iter()
            .filter(|message| message.tool_call_id == tool_call.id)
            .collect::<Vec<_>>();
        let failed = matching
            .iter()
            .any(|message| message.tool_failed == Some(true));
        let result_payload = serde_json::json!({
            "content": matching.iter().map(|message| message.content.content_text_only()).collect::<Vec<_>>(),
            "failed": failed,
        });
        let status = if matching.is_empty() {
            refact_buddy_core::supervision::ResultStatus::Cancelled
        } else if failed {
            refact_buddy_core::supervision::ResultStatus::Failed
        } else {
            refact_buddy_core::supervision::ResultStatus::Succeeded
        };
        app.buddy
            .supervision
            .observe_non_fatal(
                &supervision_chat_id,
                refact_buddy_core::supervision::SupervisionKind::ToolResult {
                    tool_name: refact_buddy_core::supervision::SupervisionReference::new(
                        &tool_call.function.name,
                    ),
                    result_hash: refact_buddy_core::supervision::SupervisionHash::normalized_result(
                        &result_payload,
                    ),
                    status,
                    error: failed.then(|| refact_buddy_core::supervision::ErrorFingerprint {
                        category: refact_buddy_core::supervision::ErrorCategory::Tool,
                        http_status: None,
                    }),
                },
            )
            .await;
        if !failed && is_editing_tool(&tool_call.function.name) {
            app.buddy
                .supervision
                .observe_non_fatal(
                    &supervision_chat_id,
                    refact_buddy_core::supervision::SupervisionKind::Diff {
                        status: refact_buddy_core::supervision::DiffStatus::Changed,
                    },
                )
                .await;
        }
        if *test_command {
            app.buddy
                .supervision
                .observe_non_fatal(
                    &supervision_chat_id,
                    refact_buddy_core::supervision::SupervisionKind::Test {
                        status: if failed {
                            refact_buddy_core::supervision::TestStatus::Failed
                        } else {
                            refact_buddy_core::supervision::TestStatus::Passed
                        },
                    },
                )
                .await;
        }
    }
    if !supervision_calls.is_empty() {
        app.buddy
            .supervision
            .observe_non_fatal(
                &supervision_chat_id,
                refact_buddy_core::supervision::SupervisionKind::Process {
                    facts: refact_buddy_core::supervision::ProcessFacts {
                        process_id: refact_buddy_core::supervision::SupervisionReference::new(
                            "chat-tools",
                        ),
                        state: refact_buddy_core::supervision::ProcessState::Finished,
                        heartbeat_at: refact_runtime_api::buddy_supervision::epoch_secs(),
                    },
                },
            )
            .await;
    }

    (results, any_corrections)
}

async fn execute_parallel_batch(
    app: AppState,
    ccx: Arc<AMutex<AtCommandsContext>>,
    batch: &[(usize, ChatToolCall)],
    catalog: Arc<ToolCatalogSnapshot>,
    turn_tool_pool: Option<TurnToolPool>,
    prepared_parallel_slots: &mut std::collections::HashMap<String, usize>,
    serial_registry: Arc<SerialToolRegistry>,
    mode_id: &str,
    model_id: Option<&str>,
) -> Vec<(usize, bool, Vec<ChatMessage>, Vec<ContextFile>)> {
    if let Some(pool) = turn_tool_pool.as_ref() {
        let mut required_slots: std::collections::HashMap<
            String,
            (refact_tool_api::ToolDesc, usize),
        > = std::collections::HashMap::new();
        for (_, tool_call) in batch {
            let resolved_name = refact_llm::adapters::claude_code_compat::cc_resolve_tool_name(
                &tool_call.function.name,
            );
            let Some(desc) = catalog
                .index
                .tools
                .iter()
                .find(|desc| desc.name == resolved_name)
            else {
                continue;
            };
            let entry = required_slots
                .entry(tool_pool_key(desc))
                .or_insert_with(|| (desc.clone(), 0));
            entry.1 += 1;
        }
        let requested_slots = required_slots
            .into_iter()
            .filter_map(|(key, (desc, count))| {
                let required = count;
                let prepared = prepared_parallel_slots
                    .get(&key)
                    .copied()
                    .unwrap_or_default();
                (required > prepared).then(|| (key, desc, required))
            })
            .collect::<Vec<_>>();
        let descriptors = requested_slots
            .iter()
            .map(|(_, desc, count)| (desc.clone(), *count))
            .collect::<Vec<_>>();
        if let Err(error) = app
            .tool_registry
            .prepare_turn_tool_pool(pool, &catalog, mode_id, model_id, &descriptors)
            .await
        {
            return batch
                .iter()
                .map(|(idx, tool_call)| {
                    (
                        *idx,
                        true,
                        vec![ChatMessage {
                            message_id: Uuid::new_v4().to_string(),
                            role: "tool".to_string(),
                            content: ChatContent::SimpleText(format!(
                                "Error preparing tool pool: {error}"
                            )),
                            tool_call_id: tool_call.id.clone(),
                            tool_failed: Some(true),
                            ..Default::default()
                        }],
                        Vec::new(),
                    )
                })
                .collect();
        }
        for (key, _, count) in requested_slots {
            prepared_parallel_slots.insert(key, count);
        }
    }
    let batch_size = batch.len();

    let futures: Vec<_> = batch
        .iter()
        .map(|(idx, tool_call)| {
            let app = app.clone();
            let ccx = ccx.clone();
            let serial_registry = serial_registry.clone();
            let catalog = catalog.clone();
            let turn_tool_pool = turn_tool_pool.clone();
            let tool_call = tool_call.clone();
            let idx = *idx;
            let batch_size = batch_size;
            let mode_id = mode_id.to_string();
            let model_id = model_id.map(|s| s.to_string());

            async move {
                execute_single_tool(
                    app,
                    ccx,
                    idx,
                    tool_call,
                    catalog,
                    turn_tool_pool,
                    serial_registry,
                    true,
                    batch_size,
                    &mode_id,
                    model_id.as_deref(),
                )
                .await
            }
        })
        .collect();

    join_all(futures).await
}

pub fn fallback_tool_runtime_event_lines(
    tool_name: &str,
    chat_label: &str,
) -> (String, Option<String>) {
    (
        format!("Running {} in '{}'", tool_name, chat_label),
        Some(format!(
            "Using {} to help with '{}'...",
            tool_name, chat_label
        )),
    )
}

pub async fn execute_tools(
    app: AppState,
    tool_calls: &[ChatToolCall],
    messages: &[ChatMessage],
    thread: &ThreadParams,
    mode_id: &str,
    model_id: Option<&str>,
    options: ExecuteToolsOptions,
) -> (Vec<ChatMessage>, bool) {
    if tool_calls.is_empty() {
        return (vec![], false);
    }
    let catalog = match options.catalog.clone() {
        Some(catalog) => catalog,
        None => {
            app.tool_registry
                .acquire_tool_catalog(
                    mode_id,
                    model_id,
                    execution_scope_for_thread(&thread).as_deref(),
                )
                .await
        }
    };
    let turn_tool_pool = app
        .tool_registry
        .acquire_turn_tool_pool(
            mode_id,
            model_id,
            execution_scope_for_thread(thread).as_deref(),
            &catalog,
        )
        .await;

    let n_ctx = get_effective_n_ctx(app.clone(), thread).await;
    let budget = match ToolBudget::try_from_n_ctx(n_ctx) {
        Ok(b) => b,
        Err(e) => {
            let error_messages: Vec<ChatMessage> = tool_calls
                .iter()
                .map(|tc| ChatMessage {
                    message_id: Uuid::new_v4().to_string(),
                    role: "tool".to_string(),
                    content: ChatContent::SimpleText(format!("Error: {}", e)),
                    tool_call_id: tc.id.clone(),
                    tool_failed: Some(true),
                    ..Default::default()
                })
                .collect();
            return (error_messages, false);
        }
    };

    let ccx = match build_tool_execution_context(app.clone(), n_ctx, messages, thread, None).await {
        Ok(context) => context,
        Err(error) => return context_error_results(tool_calls, error),
    };

    {
        let mut cgcx = ccx.lock().await;
        cgcx.tokens_for_rag = (n_ctx / 2).max(4096);
        if let Some(params) = options.subchat_tool_parameters.clone() {
            cgcx.subchat_tool_parameters = params;
        }
    }

    let app2 = app.clone();
    let chat_id = thread.id.clone();
    let chat_label = {
        let t = thread.title.trim().to_string();
        if t.is_empty() || t == "New Chat" {
            "Untitled chat".to_string()
        } else {
            t.chars().take(60).collect()
        }
    };
    let tool_meta: Vec<(String, String)> = tool_calls
        .iter()
        .map(|tc| (tc.id.clone(), format!("tool_{}", tc.id)))
        .collect();
    for (tc, (_, dedupe_key)) in tool_calls.iter().zip(tool_meta.iter()) {
        let (title, speech_text) =
            fallback_tool_runtime_event_lines(&tc.function.name, &chat_label);
        let mut ev = make_runtime_event("tool_used", &title, "tool", dedupe_key, "started", None);
        ev.speech_text = speech_text;
        ev.scene = Some("working".to_string());
        ev.chat_id = Some(chat_id.to_string());
        app.buddy_event_sink.enqueue_event(ev).await;
    }

    let (result_msgs, had_corrections) = execute_tools_inner(
        app,
        ccx,
        tool_calls,
        mode_id,
        model_id,
        budget,
        options,
        messages,
        catalog,
        turn_tool_pool,
    )
    .await;

    for (tool_call_id, dedupe_key) in &tool_meta {
        let failed = result_msgs
            .iter()
            .any(|m| &m.tool_call_id == tool_call_id && m.tool_failed == Some(true));
        if failed {
            app2.buddy_event_sink
                .complete_event(dedupe_key, "failed")
                .await;
        } else {
            app2.buddy_event_sink
                .complete_event(dedupe_key, "completed")
                .await;
        }
    }

    (result_msgs, had_corrections)
}

/// The central loop-guard wiring: `loop_guard_next` decides, `apply_loop_guard`
/// rewrites. Both are pure so the whole policy is testable without an `AppState`.

/// Tests for the turn-investigation tracker as wired into the tool-execution path.

#[cfg(test)]
mod loop_guard_tests {
    use super::*;
    use uuid::Uuid;
    use refact_core::chat_types::{ChatContent, ChatMessage, ContextFile};
    use refact_tool_api::tool_call_loop_guard::{
        identity_key_for, LoopVerdict, ToolCallLoopGuard, CAT_TOOL_NAME,
    };

    const THRESHOLD: usize = 3;

    fn guard() -> ToolCallLoopGuard {
        ToolCallLoopGuard::new()
    }

    fn decide(
        guard: &mut ToolCallLoopGuard,
        canonical_name: &str,
        identity_key: &str,
        digest: u64,
    ) -> Option<LoopVerdict> {
        loop_guard_next(
            guard,
            true,
            canonical_name,
            identity_key,
            digest,
            THRESHOLD,
            true,
        )
    }

    fn tool_message(text: &str) -> Vec<ChatMessage> {
        vec![ChatMessage {
            message_id: Uuid::new_v4().to_string(),
            role: "tool".to_string(),
            content: ChatContent::SimpleText(text.to_string()),
            tool_call_id: "call-1".to_string(),
            tool_failed: Some(false),
            ..Default::default()
        }]
    }

    fn context_file() -> Vec<ContextFile> {
        vec![ContextFile {
            file_name: "src/main.rs".to_string(),
            file_content: "fn main() {}".to_string(),
            ..Default::default()
        }]
    }

    fn text_of(messages: &[ChatMessage]) -> String {
        messages
            .iter()
            .map(|message| message.content.content_text_only())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Drive a whole chain and return what the model would see after each call.
    fn drive(
        guard: &mut ToolCallLoopGuard,
        canonical_name: &str,
        identity_key: &str,
        repeats: usize,
        digest_of: impl Fn(usize) -> u64,
        files: bool,
    ) -> Vec<(bool, Vec<ChatMessage>, Vec<ContextFile>)> {
        (0..repeats)
            .map(|round| {
                let payload = format!("result-{round}");
                let verdict = decide(guard, canonical_name, identity_key, digest_of(round));
                apply_loop_guard(
                    verdict,
                    canonical_name,
                    THRESHOLD,
                    "call-1",
                    false,
                    tool_message(&payload),
                    if files { context_file() } else { Vec::new() },
                )
            })
            .collect()
    }

    #[test]
    fn first_call_is_byte_identical_to_no_guard() {
        let mut g = guard();
        let (corrections, messages, files) = apply_loop_guard(
            decide(&mut g, "grep", "grep\u{1}k", 1),
            "grep",
            THRESHOLD,
            "call-1",
            false,
            tool_message("the original result"),
            context_file(),
        );
        assert!(!corrections);
        assert_eq!(messages.len(), 1);
        assert_eq!(text_of(&messages), "the original result");
        assert_eq!(files.len(), 1, "a first call must not touch context files");
    }

    #[test]
    fn identical_call_warns_twice_then_blocks_with_no_leaked_content() {
        let mut g = guard();
        let outcomes = drive(
            &mut g,
            "process_list",
            "process_list\u{1}k",
            4,
            |_| 42,
            true,
        );

        // 1st call: untouched.
        assert_eq!(text_of(&outcomes[0].1), "result-0");
        // 2nd and 3rd: real result preserved AND a warning prepended.
        for round in 1..=2 {
            let (_, messages, files) = &outcomes[round];
            let text = text_of(messages);
            assert!(text.starts_with("Warning: `process_list`"), "{text}");
            assert!(text.contains(&format!("result-{round}")), "{text}");
            assert_eq!(files.len(), 1, "a warning must not withhold content");
        }
        // 4th: blocked. The result is gone, and so is the ContextFile.
        let (corrections, messages, files) = &outcomes[3];
        assert!(*corrections, "a block is a correction");
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].role, "tool");
        assert_eq!(messages[0].tool_failed, Some(true));
        let text = text_of(messages);
        assert!(text.starts_with("Blocked: `process_list`"), "{text}");
        assert!(
            !text.contains("result-3"),
            "a block must withhold the result: {text}"
        );
        assert!(
            files.is_empty(),
            "a block must not leak the withheld content as a ContextFile"
        );
    }

    #[test]
    fn changed_result_resets_the_counter_and_never_blocks() {
        let mut g = guard();
        // Same call every round, but the digest changes every round: the agent is
        // making progress (the file changed, the process advanced).
        let outcomes = drive(&mut g, "cat", "cat\u{1}k", 10, |round| round as u64, true);
        for (round, (corrections, messages, files)) in outcomes.iter().enumerate() {
            assert!(
                !text_of(messages).contains("Warning") && !text_of(messages).contains("Blocked"),
                "round {round} must not be warned or blocked"
            );
            assert!(!*corrections, "round {round} must not be a correction");
            assert_eq!(messages.len(), 1);
            assert_eq!(files.len(), 1);
        }
        assert_eq!(g.consecutive(), 1, "progress keeps resetting the chain");
    }

    #[test]
    fn different_tools_never_warn() {
        let mut g = guard();
        for name in ["shell", "grep", "cat", "process_read", "ls"] {
            let (_, messages, _) = apply_loop_guard(
                decide(&mut g, name, &format!("{name}\u{1}k"), 42),
                name,
                THRESHOLD,
                "call-1",
                false,
                tool_message("ok"),
                Vec::new(),
            );
            assert_eq!(text_of(&messages), "ok", "{name} must be untouched");
        }
        assert_eq!(g.consecutive(), 1);
    }

    #[test]
    fn exempt_wait_tools_never_warn_or_block_however_often_they_repeat() {
        for name in ["sleep", "process_wait", "agent_wait"] {
            let mut g = guard();
            for _ in 0..10 {
                let verdict = decide(&mut g, name, &format!("{name}\u{1}k"), 42);
                assert_eq!(verdict, None, "{name} must not be observed at all");
                let (_, messages, files) = apply_loop_guard(
                    verdict,
                    name,
                    THRESHOLD,
                    "call-1",
                    false,
                    tool_message("still running"),
                    Vec::new(),
                );
                assert_eq!(
                    text_of(&messages),
                    "still running",
                    "{name} must be served normally"
                );
                assert_eq!(files.len(), 0);
            }
            assert_eq!(g.consecutive(), 0, "{name} must leave the guard untouched");
        }
    }

    #[test]
    fn disabled_guard_is_completely_inert() {
        let mut g = guard();
        for _ in 0..10 {
            let verdict = loop_guard_next(
                &mut g,
                false,
                "process_list",
                "process_list\u{1}k",
                42,
                THRESHOLD,
                true,
            );
            assert_eq!(verdict, None);
            let (corrections, messages, files) = apply_loop_guard(
                verdict,
                "process_list",
                THRESHOLD,
                "call-1",
                false,
                tool_message("still running"),
                context_file(),
            );
            assert!(!corrections);
            assert_eq!(text_of(&messages), "still running");
            assert_eq!(files.len(), 1);
        }
        assert_eq!(g.consecutive(), 0, "a disabled guard must not observe");
    }

    #[test]
    fn a_failed_call_does_not_advance_the_counter() {
        let mut g = guard();
        // One real call establishes the chain.
        assert_eq!(
            decide(&mut g, "grep", "grep\u{1}k", 42),
            Some(LoopVerdict::FirstCall)
        );
        // A failing call must be ignored entirely.
        let verdict = loop_guard_next(&mut g, true, "grep", "grep\u{1}k", 42, THRESHOLD, false);
        assert_eq!(verdict, None);
        assert_eq!(
            g.consecutive(),
            1,
            "a failed call made no progress and must not count"
        );
        // The next successful identical call is therefore the 2nd, not the 5th.
        assert_eq!(
            decide(&mut g, "grep", "grep\u{1}k", 42),
            Some(LoopVerdict::Warn { consecutive: 2 })
        );
    }

    #[test]
    fn cat_of_the_same_file_by_different_line_ranges_counts_as_repeats() {
        // This is the whole reason card L-2 added a `cat` identity override: reading a
        // file line by line must look like ONE repeated call, not a fresh one each time.
        let mut g = guard();
        let ranges = [
            r#"{"paths":"src/main.rs"}"#,
            r#"{"paths":"src/main.rs:1"}"#,
            r#"{"paths":"src/main.rs:10-20"}"#,
            r#"{"paths":"src/main.rs:30-40"}"#,
            r#"{"paths":"src/main.rs"}"#,
        ];
        let mut verdicts = Vec::new();
        for range in ranges {
            let key = identity_key_for(CAT_TOOL_NAME, range);
            verdicts.push(decide(&mut g, CAT_TOOL_NAME, &key, 7));
        }
        assert_eq!(verdicts[0], Some(LoopVerdict::FirstCall));
        assert_eq!(verdicts[1], Some(LoopVerdict::Warn { consecutive: 2 }));
        assert_eq!(verdicts[2], Some(LoopVerdict::Warn { consecutive: 3 }));
        assert_eq!(verdicts[3], Some(LoopVerdict::Block { consecutive: 4 }));
        assert_eq!(verdicts[4], Some(LoopVerdict::Block { consecutive: 5 }));
    }

    #[test]
    fn warn_survives_multimodal_content_and_an_empty_result() {
        let mut g = guard();
        decide(&mut g, "grep", "grep\u{1}k", 1);
        // A non-SimpleText result must still receive the warning.
        let (_, messages, _) = apply_loop_guard(
            decide(&mut g, "grep", "grep\u{1}k", 1),
            "grep",
            THRESHOLD,
            "call-1",
            false,
            vec![ChatMessage {
                role: "tool".to_string(),
                content: ChatContent::Multimodal(vec![
                    refact_core::chat_types::MultimodalElement {
                        m_type: "text".to_string(),
                        m_content: "image result".to_string(),
                    },
                ]),
                ..Default::default()
            }],
            Vec::new(),
        );
        let text = text_of(&messages);
        assert!(text.starts_with("Warning: `grep`"), "{text}");
        assert!(text.contains("image result"), "{text}");

        // No messages at all: the warning must still reach the model.
        let (corrections, messages, files) = apply_loop_guard(
            Some(LoopVerdict::Warn { consecutive: 2 }),
            "grep",
            THRESHOLD,
            "call-9",
            false,
            Vec::new(),
            Vec::new(),
        );
        assert!(!corrections, "a warning is not a correction");
        assert_eq!(messages.len(), 1);
        assert!(text_of(&messages).starts_with("Warning:"));
        assert_eq!(messages[0].tool_call_id, "call-9");
        assert!(files.is_empty());
    }

    #[test]
    fn a_block_reports_the_tool_call_it_replaced() {
        let blocked = apply_loop_guard(
            Some(LoopVerdict::Block { consecutive: 9 }),
            "process_read",
            THRESHOLD,
            "call-42",
            false,
            tool_message("payload"),
            context_file(),
        );
        assert_eq!(blocked.0, true);
        assert_eq!(blocked.1.len(), 1);
        assert_eq!(blocked.1[0].tool_call_id, "call-42");
        assert_eq!(blocked.1[0].role, "tool");
        assert!(blocked.2.is_empty());
    }
}

#[cfg(test)]
mod investigation_tracker_tests {
    use refact_tool_api::tool_call_loop_guard::{
        is_investigation_tool, is_state_mutation_tool, BlockReason, TurnInvestigationTracker,
    };

    const BUDGET: usize = 3;
    const STAGNATION_THRESHOLD: usize = 5;

    fn tracker() -> TurnInvestigationTracker {
        TurnInvestigationTracker::new()
    }

    fn record(t: &mut TurnInvestigationTracker, name: &str, key: &str) -> Option<BlockReason> {
        t.record(name, key, BUDGET, true, STAGNATION_THRESHOLD)
    }

    #[test]
    fn investigation_tools_are_counted_and_mutation_tools_reset() {
        let mut t = tracker();
        assert!(is_investigation_tool("cat"));
        assert!(is_investigation_tool("search_pattern"));
        assert!(!is_investigation_tool("shell"));

        assert!(is_state_mutation_tool("shell"));
        assert!(is_state_mutation_tool("apply_patch"));
        assert!(!is_state_mutation_tool("cat"));

        assert_eq!(record(&mut t, "cat", "cat\u{1}a"), None);
        assert_eq!(record(&mut t, "search_pattern", "sp\u{1}k"), None);
        assert_eq!(t.consecutive_investigation(), 2);
        // A mutating tool resets the counter.
        assert_eq!(record(&mut t, "shell", "sh\u{1}cmd"), None);
        assert_eq!(t.consecutive_investigation(), 0);
    }

    #[test]
    fn budget_exhausts_after_max_consecutive_investigation_calls() {
        let mut t = tracker();
        assert_eq!(record(&mut t, "cat", "cat\u{1}a"), None);
        assert_eq!(record(&mut t, "tree", "tree\u{1}src"), None);
        assert_eq!(
            record(&mut t, "glob", "glob\u{1}*.rs"),
            Some(BlockReason::BudgetExhausted)
        );
        assert!(t.is_blocked());
    }

    #[test]
    fn blocked_tracker_stays_blocked_for_subsequent_read_calls() {
        let mut t = tracker();
        // Exhaust the budget.
        record(&mut t, "cat", "cat\u{1}a");
        record(&mut t, "tree", "tree\u{1}src");
        assert_eq!(
            record(&mut t, "glob", "glob\u{1}*.rs"),
            Some(BlockReason::BudgetExhausted)
        );
        // Every further investigation call is blocked.
        assert_eq!(
            record(&mut t, "cat", "cat\u{1}b"),
            Some(BlockReason::BudgetExhausted)
        );
        // A mutating tool does not unblock a budget-exhausted tracker within the turn.
        record(&mut t, "shell", "sh\u{1}cmd");
        assert_eq!(
            record(&mut t, "cat", "cat\u{1}c"),
            Some(BlockReason::BudgetExhausted)
        );
    }

    #[test]
    fn stagnation_fires_when_same_target_exceeds_threshold_without_mutation() {
        let mut t = TurnInvestigationTracker::new();
        // Use a smaller stagnation threshold so it fires before the budget.
        let stagnation_threshold = 2;
        let budget = 100;
        assert_eq!(
            t.record("cat", "cat\u{1}a", budget, true, stagnation_threshold),
            None
        );
        assert_eq!(
            t.record("cat", "cat\u{1}a", budget, true, stagnation_threshold),
            None
        );
        assert_eq!(
            t.record("cat", "cat\u{1}a", budget, true, stagnation_threshold),
            Some(BlockReason::Stagnation { consecutive: 3 })
        );
        assert!(t.is_blocked());
    }

    #[test]
    fn distinct_targets_do_not_trigger_stagnation_within_window() {
        let mut t = TurnInvestigationTracker::new();
        let stagnation_threshold = 2;
        let budget = 100;
        // Three distinct targets, none repeated more than twice in the window.
        assert_eq!(
            t.record("cat", "cat\u{1}a", budget, true, stagnation_threshold),
            None
        );
        assert_eq!(
            t.record("cat", "cat\u{1}b", budget, true, stagnation_threshold),
            None
        );
        assert_eq!(
            t.record("cat", "cat\u{1}c", budget, true, stagnation_threshold),
            None
        );
        assert!(!t.is_blocked());
    }

    #[test]
    fn mutation_resets_the_stagnation_window() {
        let mut t = TurnInvestigationTracker::new();
        let stagnation_threshold = 2;
        let budget = 100;
        t.record("cat", "cat\u{1}a", budget, true, stagnation_threshold);
        t.record("cat", "cat\u{1}a", budget, true, stagnation_threshold);
        // A state mutation is real progress: it clears the stagnation window, so
        // the same target is fresh again ("no state mutation" is the precondition).
        t.record(
            "apply_patch",
            "patch\u{1}x",
            budget,
            true,
            stagnation_threshold,
        );
        assert_eq!(
            t.record("cat", "cat\u{1}a", budget, true, stagnation_threshold),
            None,
            "a mutation must clear the stagnation window"
        );
        assert!(!t.is_blocked());
    }

    #[test]
    fn non_investigation_non_mutation_tools_are_ignored_by_tracker() {
        let mut t = tracker();
        assert_eq!(record(&mut t, "cat", "cat\u{1}a"), None);
        assert_eq!(t.consecutive_investigation(), 1);
        // A neutral tool leaves the count unchanged.
        assert_eq!(record(&mut t, "process_read", "pr\u{1}1"), None);
        assert_eq!(t.consecutive_investigation(), 1);
    }
}
