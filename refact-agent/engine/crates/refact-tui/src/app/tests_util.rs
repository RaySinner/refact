use super::*;
pub(crate) use crate::approvals::PauseReason;
pub(crate) use crate::ui::goal_dock;
pub(crate) use std::io::{Read, Write};
pub(crate) use std::net::{TcpListener, TcpStream};
pub(crate) use std::sync::{Arc, Condvar, Mutex};
pub(crate) use std::thread;

pub(crate) fn project() -> OpenProjectResponse {
    OpenProjectResponse {
        project_id: "p1".to_string(),
        slug: "demo".to_string(),
        root: PathBuf::from("/tmp/demo"),
        pinned: Some(false),
        worker: None,
        cron_pending: None,
    }
}

pub(crate) fn project_b() -> OpenProjectResponse {
    OpenProjectResponse {
        project_id: "p2".to_string(),
        slug: "other".to_string(),
        root: PathBuf::from("/tmp/other"),
        pinned: Some(false),
        worker: None,
        cron_pending: None,
    }
}

pub(crate) fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::empty())
}

pub(crate) fn assert_tool_decision_action(
    action: &AppAction,
    decisions: Vec<ToolDecision>,
    patch: Option<Value>,
) {
    match action {
        AppAction::SendToolDecisions {
            client_request_id,
            decisions: actual_decisions,
            patch: actual_patch,
            ..
        } => {
            assert!(!client_request_id.is_empty());
            assert_eq!(actual_decisions, &decisions);
            assert_eq!(actual_patch, &patch);
        }
        other => panic!("expected tool decision action, got {other:?}"),
    }
}

pub(crate) fn chat_event(app: &App, seq: u64, kind: &str, raw: Value) -> ChatEvent {
    ChatEvent {
        chat_id: Some(app.chat_id().to_string()),
        seq: Some(seq),
        kind: kind.to_string(),
        raw,
    }
}

pub(crate) fn apply_tracked(
    app: &mut App,
    tracker: &mut ChatSeqTracker,
    event: ChatEvent,
) -> ChatSeqDecision {
    let decision = tracker.observe(&event);
    if decision == ChatSeqDecision::Apply {
        app.handle_chat_event(event);
    }
    decision
}

pub(crate) fn pause_event(app: &App, tool_call_id: &str, tool_name: &str) -> ChatEvent {
    ChatEvent {
        chat_id: Some(app.chat_id().to_string()),
        seq: None,
        kind: "pause_required".to_string(),
        raw: json!({"reasons": [pause_reason_value(tool_call_id, tool_name)]}),
    }
}

pub(crate) fn pause_reason_value(tool_call_id: &str, tool_name: &str) -> Value {
    json!({"type": "confirmation", "tool_name": tool_name, "command": format!("{tool_name}({tool_call_id})"), "rule": "*", "tool_call_id": tool_call_id})
}

pub(crate) fn assistant_text(app: &App) -> String {
    app.visible_transcript()
        .iter()
        .filter_map(|item| match item {
            TranscriptItem::Assistant(text) => Some(text.as_str()),
            _ => None,
        })
        .collect::<String>()
}

pub(crate) fn assistant_cell_count(app: &App) -> usize {
    app.visible_transcript()
        .iter()
        .filter(|item| matches!(item, TranscriptItem::Assistant(_)))
        .count()
}

pub(crate) fn reasoning_text(app: &App) -> String {
    app.visible_transcript()
        .iter()
        .filter_map(|item| match item {
            TranscriptItem::Reasoning(text, _) => Some(text.as_str()),
            _ => None,
        })
        .collect::<String>()
}

pub(crate) fn reasoning_cell_count(app: &App) -> usize {
    app.visible_transcript()
        .iter()
        .filter(|item| matches!(item, TranscriptItem::Reasoning(_, _)))
        .count()
}

pub(crate) fn injected_model_text() -> &'static str {
    "lead \u{1b}[31mred \u{1b}[2Jclear\u{7} bell \u{009b}31mcsi \u{1b}]8;;http://evil\u{7}TEXT\u{1b}]8;;\u{7} tail"
}

pub(crate) fn assert_escape_inert(text: &str) {
    assert!(!text.as_bytes().contains(&0x1b), "raw ESC in {text:?}");
    assert!(!text.as_bytes().contains(&0x07), "raw BEL in {text:?}");
    assert!(!text.as_bytes().contains(&0x9b), "raw CSI byte in {text:?}");
    assert!(!text.contains('\u{009b}'), "raw CSI char in {text:?}");
    assert!(!text.contains("http://evil"), "raw OSC8 URL in {text:?}");
}

pub(crate) fn assert_model_text_survives(text: &str) {
    for fragment in ["lead", "red", "clear", "bell", "csi", "TEXT", "tail"] {
        assert!(text.contains(fragment), "missing {fragment:?} in {text:?}");
    }
}

pub(crate) fn assert_rendered_item_escape_inert(item: &TranscriptItem) {
    let lines = crate::history::cells::cell_from_transcript_item(item, false).render(80);
    for line in lines {
        for span in line.spans {
            assert_escape_inert(span.content.as_ref());
        }
    }
}

pub(crate) fn stream_tail_plain_lines(app: &App) -> Vec<String> {
    let mut stream = app.stream_controller.clone();
    stream
        .current_tail_lines()
        .iter()
        .map(|line| line_to_plain_string(&line.line))
        .collect()
}

pub(crate) fn plan_stream_text(app: &App) -> String {
    app.visible_transcript()
        .iter()
        .filter_map(|item| match item {
            TranscriptItem::PlanStream(lines) => Some(
                lines
                    .iter()
                    .map(|line| line_to_plain_string(&line.line))
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub(crate) fn tool_cards(app: &App) -> Vec<&ToolCard> {
    app.visible_transcript()
        .iter()
        .filter_map(|item| match item {
            TranscriptItem::Tool(card) => Some(card),
            _ => None,
        })
        .collect()
}

pub(crate) fn questions_cell_count(app: &App) -> usize {
    app.visible_transcript()
        .iter()
        .filter(|item| rendered_item_plain_text(item).contains("Questions"))
        .count()
}

pub(crate) fn session_cell_count(app: &App) -> usize {
    app.visible_transcript()
        .iter()
        .filter(|item| matches!(item, TranscriptItem::Session { .. }))
        .count()
}

pub(crate) fn session_text(app: &App) -> String {
    app.visible_transcript()
        .iter()
        .filter(|item| matches!(item, TranscriptItem::Session { .. }))
        .map(rendered_item_plain_text)
        .collect::<Vec<_>>()
        .join("\n")
}

pub(crate) fn rendered_item_plain_text(item: &TranscriptItem) -> String {
    crate::history::cells::cell_from_transcript_item(item, false)
        .render(80)
        .iter()
        .map(line_to_plain_string)
        .collect::<Vec<_>>()
        .join("\n")
}

pub(crate) fn assistant_added_event(app: &App, id: &str, content: &str) -> ChatEvent {
    ChatEvent {
        chat_id: Some(app.chat_id().to_string()),
        seq: None,
        kind: "message_added".to_string(),
        raw: json!({"message": {
            "message_id": id,
            "role": "assistant",
            "content": content,
            "stream_finished": true,
        }}),
    }
}

pub(crate) fn snapshot_event(app: &App, messages: Vec<Value>) -> ChatEvent {
    ChatEvent {
        chat_id: Some(app.chat_id().to_string()),
        seq: None,
        kind: "snapshot".to_string(),
        raw: json!({"runtime": {"state": "idle"}, "messages": messages}),
    }
}

pub(crate) fn tool_call_delta_event(app: &App, tool_call_id: &str) -> ChatEvent {
    ChatEvent {
        chat_id: Some(app.chat_id().to_string()),
        seq: None,
        kind: "stream_delta".to_string(),
        raw: json!({"ops": [{"op": "set_tool_calls", "tool_calls": [{
            "id": tool_call_id,
            "function": {"name": "shell", "arguments": "{\"cmd\":\"echo 1\"}"}
        }]}]}),
    }
}

pub(crate) fn tool_result_message(tool_call_id: &str, content: &str) -> Value {
    json!({
        "role": "tool",
        "tool_call_id": tool_call_id,
        "tool_failed": false,
        "content": content,
        "stream_finished": true,
    })
}

pub(crate) fn tool_result_event(app: &App, tool_call_id: &str, content: &str) -> ChatEvent {
    ChatEvent {
        chat_id: Some(app.chat_id().to_string()),
        seq: None,
        kind: "message_added".to_string(),
        raw: json!({"message": tool_result_message(tool_call_id, content)}),
    }
}

pub(crate) fn ask_questions_tool_event(
    app: &App,
    tool_call_id: &str,
    questions: Value,
) -> ChatEvent {
    ChatEvent {
        chat_id: Some(app.chat_id().to_string()),
        seq: None,
        kind: "message_added".to_string(),
        raw: json!({
            "message": {
                "role": "tool",
                "tool_call_id": tool_call_id,
                "tool_failed": false,
                "content": json!({
                    "type": "ask_questions",
                    "tool_call_id": tool_call_id,
                    "questions": questions,
                }).to_string()
            }
        }),
    }
}

pub(crate) fn waiting_user_input_event(app: &App) -> ChatEvent {
    runtime_updated_event(app, "waiting_user_input")
}

pub(crate) fn runtime_updated_event(app: &App, state: &str) -> ChatEvent {
    ChatEvent {
        chat_id: Some(app.chat_id().to_string()),
        seq: None,
        kind: "runtime_updated".to_string(),
        raw: json!({"state": state}),
    }
}

pub(crate) fn app_with_queued_input() -> App {
    let mut app = App::new(project());
    app.composer.set_text("first");
    assert!(matches!(
        app.handle_key(key(KeyCode::Enter)),
        AppAction::SendMessage { prompt, .. } if prompt == "first"
    ));
    app.composer.set_text("second");
    assert_eq!(app.handle_key(key(KeyCode::Enter)), AppAction::None);
    assert_eq!(app.input_queue().len(), 1);
    app
}

#[derive(Clone, Default)]
pub(crate) struct CommandState(Arc<(Mutex<Vec<Value>>, Condvar)>);

impl CommandState {
    pub(crate) fn push(&self, command: Value) {
        let (lock, cond) = &*self.0;
        lock.lock().unwrap().push(command);
        cond.notify_all();
    }

    pub(crate) fn wait_for(&self, command_type: &str) -> bool {
        let deadline = Instant::now() + Duration::from_secs(5);
        let (lock, cond) = &*self.0;
        let mut commands = lock.lock().unwrap();
        loop {
            if commands
                .iter()
                .any(|command| command.get("type").and_then(Value::as_str) == Some(command_type))
            {
                return true;
            }
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            let wait = deadline.saturating_duration_since(now);
            let (next_commands, timeout) = cond.wait_timeout(commands, wait).unwrap();
            commands = next_commands;
            if timeout.timed_out() {
                return false;
            }
        }
    }

    pub(crate) fn find_command(&self, command_type: &str) -> Option<Value> {
        let (lock, _) = &*self.0;
        lock.lock()
            .unwrap()
            .iter()
            .find(|command| command.get("type").and_then(Value::as_str) == Some(command_type))
            .cloned()
    }
}

pub(crate) fn spawn_command_server(state: CommandState) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            handle_command_connection(stream, state.clone());
        }
    });
    format!("http://{addr}")
}

pub(crate) fn handle_command_connection(mut stream: TcpStream, state: CommandState) {
    let mut data = Vec::new();
    let mut buf = [0u8; 1024];
    loop {
        let Ok(n) = stream.read(&mut buf) else {
            return;
        };
        if n == 0 {
            return;
        }
        data.extend_from_slice(&buf[..n]);
        if data.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
    }
    let Some(header_end) = data
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|idx| idx + 4)
    else {
        return;
    };
    let headers = String::from_utf8_lossy(&data[..header_end]);
    let content_length = headers
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length").then_some(value)
        })
        .and_then(|value| value.trim().parse::<usize>().ok())
        .unwrap_or(0);
    while data.len() < header_end + content_length {
        let Ok(n) = stream.read(&mut buf) else {
            return;
        };
        if n == 0 {
            return;
        }
        data.extend_from_slice(&buf[..n]);
    }
    if content_length > 0 {
        let body = &data[header_end..header_end + content_length];
        state.push(serde_json::from_slice(body).unwrap_or(Value::Null));
    }
    let response = b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 21\r\nConnection: close\r\n\r\n{\"status\":\"accepted\"}";
    let _ = stream.write_all(response);
}

pub(crate) fn pending_plain_lines(app: &mut App) -> Vec<String> {
    app.pending_history_insertions(80)
        .iter()
        .flat_map(|insertion| insertion.lines.iter())
        .map(|line| line_to_plain_string(&line.line))
        .collect()
}

pub(crate) fn pending_plain_text(app: &mut App) -> String {
    app.pending_history_insertions(80)
        .iter()
        .flat_map(|insertion| insertion.lines.iter())
        .map(|line| line_to_plain_string(&line.line))
        .collect::<Vec<_>>()
        .join("\n")
}
