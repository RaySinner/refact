use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use refact_caps_core::code_assistant_caps::CodeAssistantCaps;
use refact_caps_core::model_records::ChatModelRecord;
use refact_chat_api::{ChatCommand, ChatSession, SessionState, TaskMeta};
use refact_core::llm_types::BaseModelRecord;
use refact_lsp::app_state::{app_state_from_gcx, AppState, EngineContextFactory};
use refact_lsp::at_commands::at_commands::at_commands_new_from_app;
use refact_lsp::files_in_workspace::EngineFilesRuntime;
use refact_lsp::memories::EngineMemoryRuntime;
use refact_lsp::scheduler::EngineSchedulerRuntime;
use refact_lsp::tasks::storage::EngineTaskStorageRuntime;
use refact_runtime_api::{AtCommandsContext, ChatSessionFacade, GlobalContextCtx};
use refact_tasks::types::{BoardCard, TaskBoard, TaskMeta as StoredTaskMeta, TaskStatus};
use refact_tools::tools::tool_task_create_room::ToolTaskCreateRoom;
use refact_tools::tools::tool_task_spawn_agent::ToolTaskSpawnAgent;
use refact_tools::tools::tools_description::Tool;
use refact_worktrees::scope::ROOM_ADVISOR_WORKTREE_KIND;
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex as AMutex, Notify};
use tokio::task::JoinHandle;

const TASK_ID: &str = "task-room-e2e";
const CARD_ID: &str = "T-1";
const PLANNER_CHAT_ID: &str = "planner-task-room-e2e";
const MODEL_ID: &str = "test/room-model";
const PUBLIC_BODY: &str = "PUBLIC_WRITER_REVIEW_BODY";
const PRIVATE_REASONING: &str = "PRIVATE_REASONING_SENTINEL";
const PRIVATE_ARGUMENTS: &str = "PRIVATE_ARGUMENTS_SENTINEL";
const PRIVATE_TOOL_CALL: &str = "private-tool-call-sentinel";
const ADVISOR_INITIAL: &str = "ADVISOR_INITIAL_RESPONSE";
const QUIET_REASONING: &str = "NON_PUBLIC_QUIET_REASONING";
const UNCOMMITTED_WRITER_CONTENT: &str = "UNCOMMITTED_WRITER_CONTENT_SENTINEL";

#[derive(Clone)]
struct RecordedRequest {
    body: Value,
}

struct FakeProvider {
    endpoint: String,
    requests: Arc<StdMutex<HashMap<String, Vec<RecordedRequest>>>>,
    release_writer_final: Arc<Notify>,
    release_advisor_first: Arc<Notify>,
    writer_final_released: Arc<AtomicBool>,
    advisor_first_released: Arc<AtomicBool>,
    server: JoinHandle<()>,
}

impl FakeProvider {
    async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let requests = Arc::new(StdMutex::new(HashMap::<String, Vec<RecordedRequest>>::new()));
        let release_writer_final = Arc::new(Notify::new());
        let release_advisor_first = Arc::new(Notify::new());
        let writer_final_released = Arc::new(AtomicBool::new(false));
        let advisor_first_released = Arc::new(AtomicBool::new(false));
        let server_requests = requests.clone();
        let server_writer_final = release_writer_final.clone();
        let server_advisor_first = release_advisor_first.clone();
        let server = tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let requests = server_requests.clone();
                let writer_final = server_writer_final.clone();
                let advisor_first = server_advisor_first.clone();
                tokio::spawn(async move {
                    let body = read_request(&mut socket).await;
                    let role = request_role(&body);
                    let ordinal = {
                        let mut requests = requests.lock().unwrap();
                        let role_requests = requests.entry(role.clone()).or_default();
                        role_requests.push(RecordedRequest { body: body.clone() });
                        role_requests.len()
                    };
                    match (role.as_str(), ordinal) {
                        ("writer", 1) => {
                            let delta = json!({
                                "role": "assistant",
                                "reasoning_content": PRIVATE_REASONING,
                                "tool_calls": [{
                                    "id": PRIVATE_TOOL_CALL,
                                    "index": 0,
                                    "type": "function",
                                    "function": {
                                        "name": "set_trajectory_label",
                                        "arguments": json!({"label": PRIVATE_ARGUMENTS}).to_string()
                                    }
                                }]
                            });
                            write_complete_response(&mut socket, delta, "tool_calls").await;
                        }
                        ("writer", 2) => {
                            writer_final.notified().await;
                            let delta = json!({
                                "role": "assistant",
                                "content": PUBLIC_BODY,
                                "reasoning_content": PRIVATE_REASONING
                            });
                            write_complete_response(&mut socket, delta, "stop").await;
                        }
                        ("advisor", 1) => {
                            socket
                                .write_all(
                                    b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n",
                                )
                                .await
                                .unwrap();
                            let event = serde_json::to_string(&json!({
                                "choices": [{"delta": {"role": "assistant", "content": ADVISOR_INITIAL}}]
                            }))
                            .unwrap();
                            socket
                                .write_all(format!("data: {event}\n\n").as_bytes())
                                .await
                                .unwrap();
                            socket.flush().await.unwrap();
                            advisor_first.notified().await;
                            let event = serde_json::to_string(&json!({
                                "choices": [{"delta": {}, "finish_reason": "stop"}]
                            }))
                            .unwrap();
                            socket
                                .write_all(format!("data: {event}\n\ndata: [DONE]\n\n").as_bytes())
                                .await
                                .unwrap();
                            socket.shutdown().await.unwrap();
                        }
                        _ => {
                            write_complete_response(
                                &mut socket,
                                json!({
                                    "role": "assistant",
                                    "content": "",
                                    "reasoning_content": quiet_reasoning(&role, ordinal)
                                }),
                                "stop",
                            )
                            .await;
                        }
                    }
                });
            }
        });
        Self {
            endpoint: format!("http://{address}/v1/chat/completions"),
            requests,
            release_writer_final,
            release_advisor_first,
            writer_final_released,
            advisor_first_released,
            server,
        }
    }

    fn release_writer_final(&self) {
        self.writer_final_released.store(true, Ordering::SeqCst);
        self.release_writer_final.notify_one();
    }

    fn release_advisor_first(&self) {
        self.advisor_first_released.store(true, Ordering::SeqCst);
        self.release_advisor_first.notify_one();
    }

    fn request(&self, role: &str, ordinal: usize) -> Option<Value> {
        self.requests
            .lock()
            .unwrap()
            .get(role)
            .and_then(|requests| requests.get(ordinal - 1))
            .map(|request| request.body.clone())
    }

    fn request_count(&self, role: &str) -> usize {
        self.requests
            .lock()
            .unwrap()
            .get(role)
            .map(Vec::len)
            .unwrap_or(0)
    }

    fn release_state(&self) -> String {
        format!(
            "writer_final_released={}, advisor_first_released={}",
            self.writer_final_released.load(Ordering::SeqCst),
            self.advisor_first_released.load(Ordering::SeqCst)
        )
    }
}

impl Drop for FakeProvider {
    fn drop(&mut self) {
        self.server.abort();
    }
}

async fn read_request(socket: &mut TcpStream) -> Value {
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 8192];
    let (header_end, content_length) = loop {
        let count = socket.read(&mut chunk).await.unwrap();
        assert!(count > 0, "provider request ended before headers completed");
        bytes.extend_from_slice(&chunk[..count]);
        if let Some(position) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            let header_end = position + 4;
            let headers = String::from_utf8_lossy(&bytes[..header_end]);
            assert!(
                headers.starts_with("POST /v1/chat/completions "),
                "{headers}"
            );
            let content_length = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().unwrap())
                })
                .unwrap_or(0);
            break (header_end, content_length);
        }
    };
    while bytes.len() < header_end + content_length {
        let count = socket.read(&mut chunk).await.unwrap();
        assert!(count > 0, "provider request ended before body completed");
        bytes.extend_from_slice(&chunk[..count]);
    }
    serde_json::from_slice(&bytes[header_end..header_end + content_length]).unwrap()
}

fn request_role(body: &Value) -> String {
    let wire = body.to_string();
    if wire.contains("You are the `writer`") {
        "writer".to_string()
    } else if wire.contains("You are the `advisor`") {
        "advisor".to_string()
    } else {
        "other".to_string()
    }
}

fn quiet_reasoning(role: &str, ordinal: usize) -> String {
    format!("{QUIET_REASONING}:{role}:{ordinal}")
}

async fn write_complete_response(socket: &mut TcpStream, delta: Value, finish_reason: &str) {
    let event = serde_json::to_string(&json!({
        "choices": [{"delta": delta, "finish_reason": finish_reason}]
    }))
    .unwrap();
    let body = format!("data: {event}\n\ndata: [DONE]\n\n");
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    socket.write_all(response.as_bytes()).await.unwrap();
    socket.shutdown().await.unwrap();
}

async fn wait_until<F, Fut>(description: &str, mut predicate: F)
where
    F: FnMut() -> Fut,
    Fut: Future<Output = bool>,
{
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if predicate().await {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {description}"));
}

async fn provider_request(provider: &FakeProvider, role: &str, ordinal: usize) -> Value {
    wait_until(&format!("{role} provider request {ordinal}"), || async {
        provider.request(role, ordinal).is_some()
    })
    .await;
    provider.request(role, ordinal).unwrap()
}

async fn writer_provider_request(app: &AppState, provider: &FakeProvider) -> Value {
    let result = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if provider.request("writer", 1).is_some() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    if result.is_err() {
        panic!(
            "timed out waiting for writer provider request 1\n{}",
            writer_timeout_diagnostics(app).await
        );
    }
    provider.request("writer", 1).unwrap()
}

async fn writer_timeout_diagnostics(app: &AppState) -> String {
    let gcx = app.gcx.clone();
    let mut output = String::new();
    let writer_chat_id =
        match refact_runtime_api::task_storage::load_board(gcx.clone(), TASK_ID).await {
            Ok(board) => {
                let writer = board.get_card(CARD_ID).and_then(|card| {
                    card.team_members
                        .iter()
                        .find(|member| member.role == "writer")
                });
                output.push_str(&format!("board writer member: {writer:#?}\n"));
                writer.and_then(|member| member.agent_chat_id.clone())
            }
            Err(error) => {
                output.push_str(&format!("board load error: {error}\n"));
                None
            }
        };
    let session = match writer_chat_id.as_deref() {
        Some(writer_chat_id) => {
            output.push_str(&format!("writer chat id: {writer_chat_id}\n"));
            app.chat.sessions.read().await.get(writer_chat_id).cloned()
        }
        None => {
            output.push_str("writer chat id: absent\n");
            None
        }
    };
    output.push_str(&format!(
        "writer session existence: {}\n",
        session.is_some()
    ));
    if let Some(session) = session {
        match session.try_lock() {
            Ok(session) => {
                output.push_str(&format!("runtime.state: {:?}\n", session.runtime.state));
                output.push_str(&format!("runtime.error: {:?}\n", session.runtime.error));
                output.push_str(&format!("command_queue: {:#?}\n", session.command_queue));
                output.push_str(&format!(
                    "pending_deliveries: {:#?}\n",
                    session.pending_deliveries
                ));
                output.push_str(&format!("thread.model: {:?}\n", session.thread.model));
                output.push_str(&format!(
                    "thread.task_meta: {:#?}\n",
                    session.thread.task_meta
                ));
                let messages = session
                    .messages
                    .iter()
                    .rev()
                    .take(8)
                    .rev()
                    .cloned()
                    .collect::<Vec<_>>();
                output.push_str(&format!("last messages: {messages:#?}\n"));
            }
            Err(_) => output.push_str("writer session: locked at timeout\n"),
        }
    }
    match gcx.caps_state.try_read() {
        Ok(caps_state) => match caps_state.caps.as_ref() {
            Some(caps) => {
                output.push_str(&format!(
                    "caps defaults: task_planner={:?}, chat_default={:?}\n",
                    caps.defaults.task_planner_agent_model, caps.defaults.chat_default_model
                ));
                output.push_str(&format!(
                    "caps models: {:?}\n",
                    caps.chat_models.keys().collect::<Vec<_>>()
                ));
                output.push_str(&format!(
                    "writer model record: {:#?}\n",
                    caps.chat_models.get(MODEL_ID)
                ));
            }
            None => output.push_str("caps: absent\n"),
        },
        Err(_) => output.push_str("caps: locked at timeout\n"),
    }
    output
}

fn run_git(root: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .unwrap_or_else(|error| panic!("failed to run git {args:?}: {error}"));
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn init_repo(root: &Path) {
    std::fs::create_dir_all(root).unwrap();
    run_git(root, &["init"]);
    run_git(root, &["checkout", "-b", "main"]);
    run_git(root, &["config", "core.autocrlf", "false"]);
    run_git(root, &["config", "user.email", "room-e2e@example.com"]);
    run_git(root, &["config", "user.name", "Room E2E"]);
    std::fs::write(root.join("live.txt"), "committed base\n").unwrap();
    run_git(root, &["add", "live.txt"]);
    run_git(root, &["commit", "-m", "initial"]);
}

fn planner_task_meta() -> TaskMeta {
    TaskMeta {
        task_id: TASK_ID.to_string(),
        role: "planner".to_string(),
        agent_id: None,
        card_id: None,
        planner_chat_id: Some(PLANNER_CHAT_ID.to_string()),
    }
}

fn tool_args(value: Value) -> HashMap<String, Value> {
    value.as_object().unwrap().clone().into_iter().collect()
}

async fn spawn_member(ccx: Arc<AMutex<AtCommandsContext>>, role: &str, files_to_open: Vec<&str>) {
    ToolTaskSpawnAgent::new()
        .tool_execute(
            ccx,
            &format!("spawn-{role}"),
            &tool_args(json!({
                "card_id": CARD_ID,
                "role": role,
                "role_mandate": format!("Exercise the {role} path"),
                "files_to_open": files_to_open,
                "suggested_steps": 3
            })),
        )
        .await
        .unwrap();
}

async fn member_session(app: &AppState, chat_id: &str) -> Arc<AMutex<ChatSession>> {
    app.chat
        .sessions
        .read()
        .await
        .get(chat_id)
        .cloned()
        .unwrap_or_else(|| panic!("missing room session {chat_id}"))
}

async fn wait_for_session_idle(
    description: &str,
    session_arc: &Arc<AMutex<ChatSession>>,
    provider: &FakeProvider,
    expected_reasoning: &str,
) {
    let result = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let session = session_arc.lock().await;
            let response_completed = session
                .messages
                .iter()
                .rev()
                .find(|message| message.role == "assistant")
                .is_some_and(|message| {
                    message.reasoning_content.as_deref() == Some(expected_reasoning)
                });
            if session.runtime.state == SessionState::Idle && response_completed {
                return;
            }
            drop(session);
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    if result.is_err() {
        panic!(
            "timed out waiting for {description}\n{}",
            session_timeout_diagnostics(session_arc, provider).await
        );
    }
}

async fn session_timeout_diagnostics(
    session_arc: &Arc<AMutex<ChatSession>>,
    provider: &FakeProvider,
) -> String {
    let mut output = format!(
        "provider request counts: writer={}, advisor={}\nprovider release state: {}\n",
        provider.request_count("writer"),
        provider.request_count("advisor"),
        provider.release_state()
    );
    match session_arc.try_lock() {
        Ok(session) => {
            output.push_str(&format!("runtime.state: {:?}\n", session.runtime.state));
            output.push_str(&format!("runtime.error: {:?}\n", session.runtime.error));
            output.push_str(&format!("draft_message: {:#?}\n", session.draft_message));
            output.push_str(&format!("command_queue: {:#?}\n", session.command_queue));
            output.push_str(&format!(
                "pending_deliveries: {:#?}\n",
                session.pending_deliveries
            ));
            output.push_str(&format!(
                "queue_processor_running: {}\n",
                session.queue_processor_running.load(Ordering::SeqCst)
            ));
            output.push_str(&format!("turn_depth: {}\n", session.turn_depth));
            let messages = session
                .messages
                .iter()
                .rev()
                .take(8)
                .rev()
                .cloned()
                .collect::<Vec<_>>();
            output.push_str(&format!("last messages: {messages:#?}\n"));
        }
        Err(_) => output.push_str("session: locked at timeout\n"),
    }
    output
}

fn public_message_count(session: &ChatSession) -> usize {
    session
        .messages
        .iter()
        .filter(|message| message.content.content_text_only().contains(PUBLIC_BODY))
        .count()
}

fn assert_public_wire_request(body: &Value, writer_chat_id: &str) {
    let wire = body.to_string();
    assert_eq!(wire.matches(PUBLIC_BODY).count(), 1, "{wire}");
    assert!(
        wire.contains(&format!("[message from room writer ({writer_chat_id})]")),
        "{wire}"
    );
    for private in [PRIVATE_REASONING, PRIVATE_ARGUMENTS, PRIVATE_TOOL_CALL] {
        assert!(!wire.contains(private), "wire leaked {private}: {wire}");
    }
}

#[tokio::test]
async fn auxiliary_buddy_prompt_receives_empty_completed_response() {
    let provider = FakeProvider::start().await;
    let auxiliary = json!({
        "model": MODEL_ID,
        "messages": [
            {
                "role": "system",
                "content": "You are Buddy, a persistent coding companion for this project."
            },
            {"role": "user", "content": "Generate a concise title for this chat."}
        ],
        "stream": true
    });
    let address = provider
        .endpoint
        .strip_prefix("http://")
        .unwrap()
        .split('/')
        .next()
        .unwrap();
    let mut socket = TcpStream::connect(address).await.unwrap();
    let body = serde_json::to_vec(&auxiliary).unwrap();
    let headers = format!(
        "POST /v1/chat/completions HTTP/1.1\r\nHost: {address}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    socket.write_all(headers.as_bytes()).await.unwrap();
    socket.write_all(&body).await.unwrap();
    let mut response = Vec::new();
    socket.read_to_end(&mut response).await.unwrap();
    let response = String::from_utf8(response).unwrap();
    assert!(response.starts_with("HTTP/1.1 200 OK\r\n"), "{response}");
    assert!(response.contains("\"content\":\"\""), "{response}");
    assert!(
        response.contains("\"finish_reason\":\"stop\""),
        "{response}"
    );
    assert!(response.contains("data: [DONE]"), "{response}");
    assert_eq!(provider_request(&provider, "other", 1).await, auxiliary);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn spawn_to_provider_room_delivery_preserves_privacy_scope_and_idempotency() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    let base_commit = run_git(&repo, &["rev-parse", "HEAD"]);
    let gcx = refact_test_support::make_test_gcx_with_dirs(
        temp.path().join("cache"),
        temp.path().join("config"),
        Arc::new(EngineSchedulerRuntime),
        Arc::new(EngineMemoryRuntime),
        Arc::new(EngineTaskStorageRuntime),
        Arc::new(EngineFilesRuntime),
    )
    .await;
    gcx.context_factory.install(Arc::new(EngineContextFactory));
    let repo = repo.canonicalize().unwrap();
    *gcx.documents_state.workspace_folders.lock().unwrap() = vec![repo.clone()];

    let provider = FakeProvider::start().await;
    let mut caps = CodeAssistantCaps::default();
    caps.defaults.task_planner_agent_model = MODEL_ID.to_string();
    caps.defaults.chat_default_model = MODEL_ID.to_string();
    caps.chat_models.insert(
        MODEL_ID.to_string(),
        Arc::new(ChatModelRecord {
            base: BaseModelRecord {
                id: MODEL_ID.to_string(),
                name: "room-model".to_string(),
                n_ctx: 32_768,
                tokenizer: "fake".to_string(),
                endpoint: provider.endpoint.clone(),
                ..Default::default()
            },
            supports_tools: true,
            supports_agent: true,
            max_output_tokens: Some(2_048),
            ..Default::default()
        }),
    );
    let mut caps_state = gcx.caps_state.write().await;
    caps_state.caps = Some(Arc::new(caps));
    caps_state.last_attempted_ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    drop(caps_state);

    let task_dir = repo.join(".refact").join("tasks").join(TASK_ID);
    tokio::fs::create_dir_all(task_dir.join("trajectories").join("planner"))
        .await
        .unwrap();
    tokio::fs::create_dir_all(task_dir.join("trajectories").join("agents"))
        .await
        .unwrap();
    let now = chrono::Utc::now().to_rfc3339();
    refact_runtime_api::task_storage::save_task_meta(
        gcx.clone(),
        TASK_ID,
        &StoredTaskMeta {
            schema_version: 1,
            id: TASK_ID.to_string(),
            name: "Room provider regression".to_string(),
            status: TaskStatus::Active,
            created_at: now.clone(),
            updated_at: now,
            cards_total: 1,
            cards_done: 0,
            cards_failed: 0,
            agents_active: 0,
            base_branch: Some("main".to_string()),
            base_commit: Some(base_commit),
            default_agent_model: Some(MODEL_ID.to_string()),
            is_name_generated: false,
            last_agents_summary_at: None,
            planner_session_state: None,
        },
    )
    .await
    .unwrap();
    refact_runtime_api::task_storage::save_board(
        gcx.clone(),
        TASK_ID,
        &TaskBoard {
            cards: vec![BoardCard {
                id: CARD_ID.to_string(),
                title: "Room public delivery".to_string(),
                column: "planned".to_string(),
                priority: "P1".to_string(),
                depends_on: vec![],
                instructions: "Exercise public room delivery".to_string(),
                assignee: None,
                agent_chat_id: None,
                retry_count: 0,
                status_updates: vec![],
                comments: vec![],
                final_report: None,
                final_report_structured: None,
                verifier_report: None,
                created_at: chrono::Utc::now().to_rfc3339(),
                started_at: None,
                last_heartbeat_at: None,
                completed_at: None,
                agent_branch: None,
                agent_worktree: None,
                agent_worktree_name: None,
                base_branch: None,
                base_commit: None,
                ab_variants: None,
                team_members: vec![],
                target_files: vec![],
                scope_guard_mode: Default::default(),
            }],
            ..Default::default()
        },
    )
    .await
    .unwrap();

    let app = app_state_from_gcx(gcx.clone()).await;
    let mut planner = ChatSession::new(PLANNER_CHAT_ID.to_string());
    planner.thread.task_meta = Some(planner_task_meta());
    planner.thread.model = MODEL_ID.to_string();
    gcx.chat_sessions
        .write()
        .await
        .insert(PLANNER_CHAT_ID.to_string(), Arc::new(AMutex::new(planner)));
    let ccx = Arc::new(AMutex::new(
        at_commands_new_from_app(
            app.clone(),
            32_768,
            20,
            false,
            vec![],
            PLANNER_CHAT_ID.to_string(),
            Some(PLANNER_CHAT_ID.to_string()),
            MODEL_ID.to_string(),
            Some(planner_task_meta()),
            None,
        )
        .await,
    ));

    ToolTaskCreateRoom::new()
        .tool_execute(
            ccx.clone(),
            &"create-room".to_string(),
            &tool_args(json!({
                "card_id": CARD_ID,
                "agents": [
                    {"config_name": "code_edit", "role": "writer", "target_files": ["live.txt"]},
                    {"config_name": "design_review", "role": "advisor"}
                ]
            })),
        )
        .await
        .unwrap();

    spawn_member(ccx.clone(), "writer", vec![]).await;
    writer_provider_request(&app, &provider).await;
    let board = refact_runtime_api::task_storage::load_board(gcx.clone(), TASK_ID)
        .await
        .unwrap();
    let writer = board
        .get_card(CARD_ID)
        .unwrap()
        .team_members
        .iter()
        .find(|member| member.role == "writer")
        .unwrap()
        .clone();
    let writer_chat_id = writer.agent_chat_id.clone().unwrap();
    let writer_root = PathBuf::from(writer.agent_worktree.clone().unwrap());
    std::fs::write(writer_root.join("live.txt"), UNCOMMITTED_WRITER_CONTENT).unwrap();

    spawn_member(ccx, "advisor", vec!["live.txt"]).await;
    let first_advisor_request = provider_request(&provider, "advisor", 1).await;
    assert!(first_advisor_request
        .to_string()
        .contains(UNCOMMITTED_WRITER_CONTENT));
    assert!(!first_advisor_request.to_string().contains(PUBLIC_BODY));

    let board = refact_runtime_api::task_storage::load_board(gcx.clone(), TASK_ID)
        .await
        .unwrap();
    let card = board.get_card(CARD_ID).unwrap();
    let advisor = card
        .team_members
        .iter()
        .find(|member| member.role == "advisor")
        .unwrap()
        .clone();
    let advisor_chat_id = advisor.agent_chat_id.clone().unwrap();
    assert_eq!(advisor.agent_branch, None);
    assert_eq!(
        advisor.agent_worktree.as_deref(),
        writer.agent_worktree.as_deref()
    );
    assert_eq!(PathBuf::from(advisor.agent_worktree.unwrap()), writer_root);

    let advisor_session = member_session(&app, &advisor_chat_id).await;
    {
        let session = advisor_session.lock().await;
        let scope = session.thread.worktree.as_ref().unwrap();
        assert_eq!(scope.kind, ROOM_ADVISOR_WORKTREE_KIND);
        assert_eq!(scope.branch, None);
        assert_eq!(scope.root, writer_root);
    }
    wait_until("advisor first draft", || async {
        advisor_session
            .lock()
            .await
            .draft_message
            .as_ref()
            .is_some_and(|message| {
                message
                    .content
                    .content_text_only()
                    .contains(ADVISOR_INITIAL)
            })
    })
    .await;

    provider.release_writer_final();
    let writer_session = member_session(&app, &writer_chat_id).await;
    wait_until("writer final public body", || async {
        writer_session.lock().await.messages.iter().any(|message| {
            message.role == "assistant" && message.content.content_text_only().contains(PUBLIC_BODY)
        })
    })
    .await;
    wait_until("busy advisor pending room delivery", || async {
        advisor_session.lock().await.pending_deliveries.len() == 1
    })
    .await;
    assert_eq!(public_message_count(&*advisor_session.lock().await), 0);

    let event_id = {
        let writer = writer_session.lock().await;
        assert!(writer.messages.iter().any(|message| {
            message.role == "assistant"
                && message.tool_calls.as_ref().is_some_and(|calls| {
                    calls.iter().any(|call| {
                        call.id == PRIVATE_TOOL_CALL
                            && call.function.arguments.contains(PRIVATE_ARGUMENTS)
                    })
                })
        }));
        assert!(writer.messages.iter().any(|message| {
            message.role == "tool"
                && message
                    .content
                    .content_text_only()
                    .contains(PRIVATE_ARGUMENTS)
        }));
        let final_writer_message = writer
            .messages
            .iter()
            .find(|message| message.content.content_text_only().contains(PUBLIC_BODY))
            .unwrap();
        assert_eq!(
            final_writer_message.reasoning_content.as_deref(),
            Some(PRIVATE_REASONING)
        );
        format!("assistant:{}", final_writer_message.message_id)
    };

    provider.release_advisor_first();
    let second_advisor_request = provider_request(&provider, "advisor", 2).await;
    assert_public_wire_request(&second_advisor_request, &writer_chat_id);
    wait_until("advisor delivered public message", || async {
        public_message_count(&*advisor_session.lock().await) == 1
    })
    .await;
    wait_for_session_idle(
        "advisor idle after peer delivery",
        &advisor_session,
        &provider,
        &quiet_reasoning("advisor", 2),
    )
    .await;

    let delivered = advisor_session
        .lock()
        .await
        .messages
        .iter()
        .find(|message| message.content.content_text_only().contains(PUBLIC_BODY))
        .unwrap()
        .clone();
    let provenance = refact_chat_api::provenance_from_message(&delivered).unwrap();
    assert_eq!(provenance.chat_id, writer_chat_id);
    assert_eq!(provenance.role, "writer");
    assert_eq!(provenance.card_id.as_deref(), Some(CARD_ID));

    let tool_names = second_advisor_request["tools"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|tool| tool["function"]["name"].as_str())
        .collect::<Vec<_>>();
    for forbidden in [
        "shell",
        "process_start",
        "apply_patch",
        "create_textdoc",
        "update_textdoc",
        "rm",
        "mv",
    ] {
        assert!(
            !tool_names.contains(&forbidden),
            "advisor received write tool {forbidden}"
        );
    }

    let retry = refact_runtime_api::room_delivery::retry_room_delivery(
        app.clone(),
        TASK_ID,
        CARD_ID,
        &event_id,
        &advisor_chat_id,
        false,
    )
    .await
    .unwrap();
    assert!(retry.failed.is_empty(), "{retry:?}");
    assert_eq!(public_message_count(&*advisor_session.lock().await), 1);

    app.chat
        .facade
        .push_priority_command(&advisor_chat_id, ChatCommand::Regenerate {})
        .await
        .unwrap();
    let third_advisor_request = provider_request(&provider, "advisor", 3).await;
    assert_public_wire_request(&third_advisor_request, &writer_chat_id);
    wait_for_session_idle(
        "advisor idle after retry regeneration",
        &advisor_session,
        &provider,
        &quiet_reasoning("advisor", 3),
    )
    .await;
    assert_eq!(public_message_count(&*advisor_session.lock().await), 1);
    wait_for_session_idle(
        "writer idle after advisor publications",
        &writer_session,
        &provider,
        &quiet_reasoning("writer", 3),
    )
    .await;
    assert_eq!(provider.request_count("advisor"), 3);
    assert_eq!(provider.request_count("writer"), 3);
}
