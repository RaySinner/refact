use super::*;
use crate::exec::types::DEFAULT_EXEC_OUTPUT_LIMIT_BYTES;
use crate::exec::{ExecMode, ExecOwnerMeta, ExecProcessId, ExecProcessMeta};
use crate::yaml_configs::customization_types::{ModeOverride, ModeThreadDefaults, ProjectRegistry};
use serde_json::json;
use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

pub(crate) fn test_mode_config(id: &str) -> ModeConfig {
    ModeConfig {
        schema_version: 1,
        id: id.to_string(),
        title: id.to_string(),
        description: String::new(),
        specific: false,
        prompt: String::new(),
        plan_template: String::new(),
        tools: Vec::new(),
        allow_integrations: false,
        allow_mcp: false,
        allow_subagents: false,
        model_defaults: Default::default(),
        tool_confirm: Default::default(),
        thread_defaults: Default::default(),
        ui: Default::default(),
        base: None,
        match_models: None,
        override_config: None,
    }
}

pub(crate) fn make_request(cmd: ChatCommand) -> CommandRequest {
    CommandRequest {
        client_request_id: "req-1".into(),
        priority: false,
        command: cmd,
    }
}

pub(crate) fn abort_request(request_id: &str) -> CommandRequest {
    CommandRequest {
        client_request_id: request_id.to_string(),
        priority: false,
        command: ChatCommand::Abort {},
    }
}

pub(crate) async fn wait_for_empty_lock(session_arc: &Arc<AMutex<ChatSession>>) {
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let waiting = session_arc
                .lock()
                .await
                .queue_processor_counters
                .empty_locks
                .load(Ordering::Relaxed);
            if waiting > 0 {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("queue processor did not wait for notification");
}

pub(crate) async fn wait_for_queue_to_drain(session_arc: &Arc<AMutex<ChatSession>>) {
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if session_arc.lock().await.command_queue.is_empty() {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("queue processor did not drain queued command");
}

pub(crate) async fn close_processor(
    session_arc: &Arc<AMutex<ChatSession>>,
    handle: tokio::task::JoinHandle<()>,
) {
    session_arc.lock().await.close_event_channel();
    tokio::time::timeout(std::time::Duration::from_secs(2), handle)
        .await
        .expect("queue processor did not exit after session close")
        .expect("queue processor panicked");
}

pub(crate) fn user_message_command(content: &str, client_message_id: Option<&str>) -> ChatCommand {
    ChatCommand::UserMessage {
        content: json!(content),
        attachments: vec![],
        context_files: vec![],
        suppress_auto_enrichment: false,
        client_message_id: client_message_id.map(str::to_string),
        push: None,
    }
}

pub(crate) fn sample_worktree(id: &str) -> WorktreeMeta {
    WorktreeMeta {
        id: id.to_string(),
        kind: "chat".to_string(),
        root: std::path::PathBuf::from(format!("/tmp/{id}-worktree")),
        source_workspace_root: std::path::PathBuf::from("/tmp/source"),
        repo_root: std::path::PathBuf::from("/tmp/source"),
        branch: Some(format!("refact/chat/{id}")),
        base_branch: Some("main".to_string()),
        base_commit: None,
        task_id: None,
        card_id: None,
        agent_id: None,
        enforce: true,
    }
}

pub(crate) fn run_git(cwd: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
}

pub(crate) fn init_repo(root: &Path) {
    run_git(root, &["init"]);
    run_git(root, &["checkout", "-b", "main"]);
    run_git(root, &["config", "core.autocrlf", "false"]);
    run_git(root, &["config", "user.email", "test@example.com"]);
    run_git(root, &["config", "user.name", "Test User"]);
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/lib.rs"), "pub fn source() {}\n").unwrap();
    run_git(root, &["add", "."]);
    run_git(root, &["commit", "-m", "initial"]);
}

pub(crate) async fn register_running_process(
    gcx: &crate::global_context::GlobalContext,
    process_id: &str,
    mode: ExecMode,
    chat_id: &str,
) -> ExecProcessId {
    let snapshot = gcx
        .exec_registry
        .register(
            ExecProcessMeta::new(mode, "test command".to_string())
                .with_process_id(ExecProcessId(process_id.to_string()))
                .with_owner(ExecOwnerMeta {
                    chat_id: Some(chat_id.to_string()),
                    ..ExecOwnerMeta::default()
                }),
            DEFAULT_EXEC_OUTPUT_LIMIT_BYTES,
        )
        .await;
    gcx.exec_registry
        .mark_started(&snapshot.meta.process_id)
        .await
        .unwrap();
    snapshot.meta.process_id
}

pub(crate) async fn wait_for_cleanup_notice(
    rx: &mut tokio::sync::broadcast::Receiver<Arc<String>>,
) -> ChatMessage {
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let json = rx.recv().await.unwrap();
            let envelope: EventEnvelope = serde_json::from_str(&json).unwrap();
            if let ChatEvent::MessageAdded { message, .. } = envelope.event {
                if message.role == "event"
                    && message.extra.get("event").is_some_and(|event| {
                        event["subkind"] == json!("system_notice")
                            && event["source"] == json!("chat.session")
                    })
                {
                    return message;
                }
            }
        }
    })
    .await
    .unwrap()
}

pub(crate) fn make_plan_message(content: &str) -> ChatMessage {
    let mut extra = serde_json::Map::new();
    extra.insert(
        "plan".to_string(),
        json!({
            "mode": "agent",
            "version": 1,
            "created_at_ms": 123,
            "supersedes": null,
        }),
    );
    ChatMessage {
        message_id: Uuid::new_v4().to_string(),
        role: "plan".to_string(),
        content: ChatContent::SimpleText(content.to_string()),
        preserve: Some(true),
        extra,
        ..Default::default()
    }
}

pub(crate) fn make_plan_delta_event(content: &str, seq: u32) -> ChatMessage {
    let mut extra = serde_json::Map::new();
    extra.insert(
        "event".to_string(),
        json!({
            "subkind": "plan_delta",
            "source": "tool.set_plan",
            "payload": {"seq": seq},
        }),
    );
    ChatMessage {
        message_id: Uuid::new_v4().to_string(),
        role: "event".to_string(),
        content: ChatContent::SimpleText(content.to_string()),
        extra,
        ..Default::default()
    }
}

pub(crate) fn make_goal_message(content: &str) -> ChatMessage {
    crate::chat::internal_roles::goal("agent", 1, content, None, true, GoalBudget::default())
}

pub(crate) fn make_goal_event(content: &str, subkind: &str) -> ChatMessage {
    let mut extra = serde_json::Map::new();
    extra.insert(
        "event".to_string(),
        json!({
            "subkind": subkind,
            "source": "goal.test",
            "payload": {"seq": 1},
        }),
    );
    ChatMessage {
        message_id: Uuid::new_v4().to_string(),
        role: "event".to_string(),
        content: ChatContent::SimpleText(content.to_string()),
        extra,
        ..Default::default()
    }
}

pub(crate) fn drain_events(rx: &mut tokio::sync::broadcast::Receiver<Arc<String>>) -> Vec<ChatEvent> {
    let mut events = Vec::new();
    while let Ok(json) = rx.try_recv() {
        let envelope: EventEnvelope = serde_json::from_str(&json).unwrap();
        events.push(envelope.event);
    }
    events
}
