pub use refact_tools::tools::tool_process_subscribe::*;

#[cfg(test)]
#[allow(unused_imports)]
use std::collections::{HashMap, VecDeque};
#[cfg(test)]
#[allow(unused_imports)]
use std::sync::Arc;
#[cfg(test)]
#[allow(unused_imports)]
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(test)]
#[allow(unused_imports)]
use std::time::{Duration, Instant};
#[cfg(test)]
#[allow(unused_imports)]
use async_trait::async_trait;
#[cfg(test)]
#[allow(unused_imports)]
use regex::Regex;
#[cfg(test)]
#[allow(unused_imports)]
use serde_json::{json, Value};
#[cfg(test)]
#[allow(unused_imports)]
use tokio::sync::Mutex as AMutex;
#[cfg(test)]
#[allow(unused_imports)]
use tokio::task::JoinHandle;
#[cfg(test)]
#[allow(unused_imports)]
use refact_core::chat_types::{PendingDelivery, PushMode};
#[cfg(test)]
#[allow(unused_imports)]
use refact_runtime_api::AtCommandsContext;
#[cfg(test)]
#[allow(unused_imports)]
use refact_runtime_api::GlobalContextCtx;
#[cfg(test)]
#[allow(unused_imports)]
use refact_core::chat_types::{ChatContent, ChatMessage, ContextEnum};
#[cfg(test)]
#[allow(unused_imports)]
use refact_chat_api::internal_roles::{event, EventSubkind};
#[cfg(test)]
#[allow(unused_imports)]
use refact_exec::types::normalize_workspace_path;
#[cfg(test)]
#[allow(unused_imports)]
use refact_exec::{ExecOutputChunk, ExecOutputStream, ExecProcessId, ExecRegistry};
#[cfg(test)]
#[allow(unused_imports)]
use refact_runtime_api::files_correction::get_active_project_path;
#[cfg(test)]
#[allow(unused_imports)]
use crate::global_context::SharedGlobalContext;
#[cfg(test)]
#[allow(unused_imports)]
use refact_postprocessing::pp_command_output::OutputFilter;
#[cfg(test)]
#[allow(unused_imports)]
use crate::tools::file_edit::auxiliary::active_execution_scope;
#[cfg(test)]
#[allow(unused_imports)]
use crate::tools::tools_description::{Tool, ToolDesc, ToolSource, ToolSourceType};
#[cfg(test)]
#[allow(unused_imports)]
use refact_worktrees::scope::ExecutionScope;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_state::AppState;
    use refact_chat_api::internal_roles::EVENT_ROLE;
    use refact_chat_api::types::{ChatSession, SessionState};
    use refact_exec::{ExecMode, ExecOwnerMeta, ExecSpawnRequest};
    use crate::global_context::GlobalContext;
    use crate::tools::settings_guard::SettingsGuard;
    use serial_test::serial;

    async fn test_context(
        chat_id: &str,
    ) -> (
        Arc<GlobalContext>,
        Arc<AMutex<AtCommandsContext>>,
        Arc<AMutex<ChatSession>>,
    ) {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let session = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        gcx.chat_sessions
            .write()
            .await
            .insert(chat_id.to_string(), session.clone());
        let ccx = crate::at_commands::at_commands::at_commands_new_with_abort(
            refact_runtime_api::app_state_from_gcx(gcx.clone()).await,
            4096,
            20,
            false,
            Vec::new(),
            chat_id.to_string(),
            None,
            "model".to_string(),
            None,
            None,
            None,
        )
        .await;
        (gcx, Arc::new(AMutex::new(ccx)), session)
    }

    fn args(entries: Vec<(&str, Value)>) -> HashMap<String, Value> {
        entries
            .into_iter()
            .map(|(key, value)| (key.to_string(), value))
            .collect()
    }

    async fn run_tool(
        tool: &mut ToolProcessSubscribe,
        ccx: Arc<AMutex<AtCommandsContext>>,
        args: HashMap<String, Value>,
    ) -> ChatMessage {
        let (_, messages) = tool
            .tool_execute(ccx, &"tool_call".to_string(), &args)
            .await
            .unwrap();
        match messages.into_iter().next().unwrap() {
            ContextEnum::ChatMessage(message) => message,
            ContextEnum::ContextFile(_) => panic!("expected chat message"),
        }
    }

    async fn run_tool_error(
        tool: &mut ToolProcessSubscribe,
        ccx: Arc<AMutex<AtCommandsContext>>,
        args: HashMap<String, Value>,
    ) -> String {
        tool.tool_execute(ccx, &"tool_call".to_string(), &args)
            .await
            .unwrap_err()
    }

    fn owner(chat_id: &str) -> ExecOwnerMeta {
        ExecOwnerMeta {
            chat_id: Some(chat_id.to_string()),
            tool_call_id: Some("tool-call".to_string()),
            service_name: None,
            workspace: None,
        }
    }

    async fn spawn_background(
        gcx: &Arc<GlobalContext>,
        chat_id: &str,
        command: String,
    ) -> ExecProcessId {
        gcx.exec_registry
            .spawn(
                ExecSpawnRequest::new(ExecMode::Background, command)
                    .with_owner(owner(chat_id))
                    .with_short_description("test process"),
            )
            .await
            .unwrap()
            .snapshot
            .meta
            .process_id
    }

    fn ready_command() -> String {
        if cfg!(target_os = "windows") {
            "Start-Sleep -Milliseconds 100; [Console]::Out.Write('READY' + [char]10 + 'NOT_READY' + [char]10 + 'READY' + [char]10)"
                .to_string()
        } else {
            "sleep 0.1; printf 'READY\nNOT_READY\nREADY\n'".to_string()
        }
    }

    fn subscribe_duration_ms() -> u64 {
        if cfg!(target_os = "windows") {
            60_000
        } else {
            5_000
        }
    }

    fn short_sleep_command() -> String {
        if cfg!(target_os = "windows") {
            "Start-Sleep -Milliseconds 50".to_string()
        } else {
            "sleep 0.05".to_string()
        }
    }

    fn long_sleep_command() -> String {
        if cfg!(target_os = "windows") {
            "Start-Sleep -Seconds 30".to_string()
        } else {
            "sleep 30".to_string()
        }
    }

    async fn subscribe_events(session: &Arc<AMutex<ChatSession>>) -> Vec<ChatMessage> {
        let session = session.lock().await;
        session
            .messages
            .iter()
            .filter(|message| is_subscribe_event(message))
            .cloned()
            .collect()
    }

    fn is_subscribe_event(message: &ChatMessage) -> bool {
        message.role == EVENT_ROLE
            && message
                .extra
                .get("event")
                .and_then(|event| event.get("subkind"))
                .and_then(Value::as_str)
                == Some("system_notice")
            && message
                .extra
                .get("event")
                .and_then(|event| event.get("source"))
                .and_then(Value::as_str)
                == Some("exec.subscribe")
    }

    async fn wait_for_subscribe_event_count(
        session: &Arc<AMutex<ChatSession>>,
        expected_count: usize,
    ) -> Vec<ChatMessage> {
        let wait_secs = if cfg!(target_os = "windows") { 60 } else { 2 };
        let deadline = tokio::time::Instant::now() + Duration::from_secs(wait_secs);
        loop {
            let events = subscribe_events(session).await;
            if events.len() >= expected_count {
                return events;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "process subscribe events not injected"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    async fn wait_for_subscribe_event_count_for(
        session: &Arc<AMutex<ChatSession>>,
        expected_count: usize,
        timeout: Duration,
    ) -> Option<Vec<ChatMessage>> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let events = subscribe_events(session).await;
            if events.len() >= expected_count {
                return Some(events);
            }
            if tokio::time::Instant::now() >= deadline {
                return None;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    #[tokio::test]
    async fn subscribe_injects_matching_line() {
        let chat_id = "subscribe-injects-matching-line";
        let (gcx, ccx, session) = test_context(chat_id).await;
        let process_id = spawn_background(&gcx, chat_id, ready_command()).await;
        let mut tool = ToolProcessSubscribe {
            config_path: String::new(),
        };

        let message = run_tool(
            &mut tool,
            ccx,
            args(vec![
                ("process_id", json!(process_id.as_str())),
                ("regex_filter", json!("^READY$")),
                ("max_duration_ms", json!(subscribe_duration_ms())),
            ]),
        )
        .await;
        assert_eq!(
            serde_json::from_str::<Value>(&message.content.content_text_only()).unwrap(),
            json!({
                "subscribed": true,
                "max_duration_ms": subscribe_duration_ms(),
                "max_events": DEFAULT_MAX_EVENTS,
                "max_line_bytes": DEFAULT_MAX_LINE_BYTES,
                "process_id": process_id.as_str(),
                "push": "append"
            })
        );
        let _ = gcx.exec_registry.wait(&process_id).await.unwrap();

        wait_for_subscribe_event_count(&session, 2).await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        let events = subscribe_events(&session).await;
        assert_eq!(events.len(), 2);
        assert_eq!(
            events
                .iter()
                .map(|message| message.extra["event"]["payload"]["line"].as_str().unwrap())
                .collect::<Vec<_>>(),
            vec!["READY", "READY"]
        );
        assert!(events
            .iter()
            .all(|message| message.extra["event"]["payload"]["process_id"] == json!(process_id)));
    }

    #[test]
    fn push_defaults_to_append_and_validates_modes() {
        let mut input = args(vec![("process_id", json!("exec_test"))]);
        assert_eq!(parse_subscribe_args(&input).unwrap().push, PushMode::Append);
        for (value, expected) in [
            ("preempt", PushMode::Preempt),
            ("when_idle", PushMode::WhenIdle),
        ] {
            input.insert("push".into(), json!(value));
            assert_eq!(parse_subscribe_args(&input).unwrap().push, expected);
        }
        input.insert("push".into(), json!(true));
        assert!(parse_subscribe_args(&input).is_err());
        assert_eq!(
            process_subscribe_input_schema()["properties"]["push"]["default"],
            "append"
        );
    }

    #[tokio::test]
    async fn failed_batch_retains_id_and_bounded_order_then_retries() {
        let (gcx, _, session) = test_context("retry-subscribe").await;
        let workspace = tempfile::tempdir().unwrap();
        *gcx.documents_state.workspace_folders.lock().unwrap() =
            vec![workspace.path().to_path_buf()];
        let app = refact_runtime_api::app_state_from_gcx(gcx.clone()).await;
        session.lock().await.closed = true;
        let mut events = (0..25)
            .map(|n| SubscribeEvent::Line(n.to_string()))
            .collect();
        let mut pending = None;
        let abort = Arc::new(AtomicBool::new(false));
        let process = ExecProcessId("exec_retry".into());
        assert!(
            flush_events(
                &app,
                "retry-subscribe",
                &process,
                PushMode::Append,
                &mut events,
                &mut pending,
                abort.clone()
            )
            .await
        );
        let id = pending.as_ref().unwrap().id.clone();
        assert_eq!(pending.as_ref().unwrap().messages.len(), 20);
        assert_eq!(events.len(), 5);
        assert!(
            flush_events(
                &app,
                "retry-subscribe",
                &process,
                PushMode::Append,
                &mut events,
                &mut pending,
                abort.clone()
            )
            .await
        );
        assert_eq!(pending.as_ref().unwrap().id, id);
        session.lock().await.closed = false;
        assert!(
            flush_events(
                &app,
                "retry-subscribe",
                &process,
                PushMode::Append,
                &mut events,
                &mut pending,
                abort.clone()
            )
            .await
        );
        assert!(pending.is_none());
        assert_eq!(subscribe_events(&session).await.len(), 20);
        assert!(
            flush_events(
                &app,
                "retry-subscribe",
                &process,
                PushMode::Append,
                &mut events,
                &mut pending,
                abort.clone()
            )
            .await
        );
        assert_eq!(subscribe_events(&session).await.len(), 25);
        assert!(pending.is_none());
        assert!(events.is_empty());
        let saved = refact_runtime_api::trajectory_runtime::load_trajectory_for_chat(
            &gcx.chat_ctx(gcx.clone()),
            "retry-subscribe",
        )
        .await
        .expect("retried batches persisted");
        assert_eq!(
            saved
                .messages
                .iter()
                .filter(|message| is_subscribe_event(message))
                .map(|message| message.content.content_text_only())
                .collect::<Vec<_>>(),
            (0..25).map(|n| n.to_string()).collect::<Vec<_>>()
        );
        gcx.shutdown_flag.store(true, Ordering::Relaxed);
        assert!(
            !flush_events(
                &app,
                "retry-subscribe",
                &process,
                PushMode::Append,
                &mut events,
                &mut pending,
                abort
            )
            .await
        );
    }

    #[test]
    fn regex_filter_uses_substring_match() {
        let regex = Regex::new("error").unwrap();

        assert!(line_matches(Some(&regex), "fatal error: bind failed"));
    }

    #[test]
    fn pending_no_newline_buffer_is_truncated() {
        let mut state = SubscriptionState::new(None, DEFAULT_MAX_EVENTS, 32);

        state.push_text(&"x".repeat(1_000), Instant::now());

        assert_eq!(state.pending.len(), 32);
        assert!(state.pending.ends_with(TRUNCATION_MARKER));
    }

    #[test]
    fn max_events_caps_injections() {
        let mut state = SubscriptionState::new(None, 3, DEFAULT_MAX_LINE_BYTES);

        let reached_cap = state.push_text("one\ntwo\nthree\nfour\n", Instant::now());

        assert!(reached_cap);
        assert_eq!(state.event_count, 3);
        assert_eq!(state.events.len(), 3);
    }

    #[test]
    fn high_rate_matches_are_coalesced() {
        let mut state = SubscriptionState::new(None, DEFAULT_MAX_EVENTS, DEFAULT_MAX_LINE_BYTES);

        state.push_text(
            &format!(
                "{}\n",
                (0..12)
                    .map(|i| format!("line-{i}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            ),
            Instant::now(),
        );
        state.finalize_summaries();

        assert_eq!(state.events.len(), RATE_LIMIT_PER_SECOND + 1);
        assert!(matches!(
            state.events.back().unwrap(),
            SubscribeEvent::Summary { line_count: 2, .. }
        ));
    }

    #[tokio::test]
    async fn streaming_session_queues_until_boundary() {
        let chat_id = "paused-session-gets-no-injections-until-idle";
        let (gcx, ccx, session) = test_context(chat_id).await;
        {
            let mut session = session.lock().await;
            session.start_stream().unwrap();
        }
        let process_id = spawn_background(&gcx, chat_id, ready_command()).await;
        let mut tool = ToolProcessSubscribe {
            config_path: String::new(),
        };

        let _ = run_tool(
            &mut tool,
            ccx,
            args(vec![
                ("process_id", json!(process_id.as_str())),
                ("regex_filter", json!("READY")),
                ("max_duration_ms", json!(subscribe_duration_ms())),
            ]),
        )
        .await;
        let _ = gcx.exec_registry.wait(&process_id).await.unwrap();
        assert!(
            wait_for_subscribe_event_count_for(&session, 1, Duration::from_millis(200))
                .await
                .is_none()
        );

        {
            let mut session = session.lock().await;
            session.draft_message = None;
            session.set_runtime_state(SessionState::Idle, None);
            session.drain_pending_deliveries();
        }

        wait_for_subscribe_event_count(&session, 2).await;
    }

    #[tokio::test]
    async fn cross_chat_process_subscribe_denied() {
        let (gcx, _owner_ccx, _owner_session) = test_context("subscribe-owner-chat").await;
        let (_other_gcx, other_ccx, _other_session) = test_context("subscribe-other-chat").await;
        {
            let mut ccx_lock = other_ccx.lock().await;
            ccx_lock.app = refact_runtime_api::app_state_from_gcx(gcx.clone()).await;
        }
        let process_id = spawn_background(&gcx, "subscribe-owner-chat", long_sleep_command()).await;
        let mut tool = ToolProcessSubscribe {
            config_path: String::new(),
        };

        let err = run_tool_error(
            &mut tool,
            other_ccx,
            args(vec![
                ("process_id", json!(process_id.as_str())),
                ("max_duration_ms", json!(1_000)),
            ]),
        )
        .await;

        assert_eq!(err, format!("process access denied: {process_id}"));
        gcx.exec_registry.kill(&process_id).await.unwrap();
    }

    #[tokio::test]
    async fn auto_cancel_on_process_exit() {
        let chat_id = "auto-cancel-on-process-exit";
        let (gcx, _ccx, _session) = test_context(chat_id).await;
        let process_id = spawn_background(&gcx, chat_id, short_sleep_command()).await;
        let handle = spawn_process_subscription(
            refact_runtime_api::app_state_from_gcx(gcx.clone()).await,
            gcx.exec_registry.clone(),
            process_id.clone(),
            chat_id.to_string(),
            SubscriptionConfig {
                regex_filter: None,
                max_duration: Duration::from_secs(10),
                max_events: DEFAULT_MAX_EVENTS,
                max_line_bytes: DEFAULT_MAX_LINE_BYTES,
                push: PushMode::Append,
            },
            Arc::new(AtomicBool::new(false)),
        );

        let _ = gcx.exec_registry.wait(&process_id).await.unwrap();
        tokio::time::timeout(Duration::from_secs(1), handle)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn auto_cancel_on_max_duration() {
        let chat_id = "auto-cancel-on-max-duration";
        let (gcx, _ccx, _session) = test_context(chat_id).await;
        let process_id = spawn_background(&gcx, chat_id, long_sleep_command()).await;
        let handle = spawn_process_subscription(
            refact_runtime_api::app_state_from_gcx(gcx.clone()).await,
            gcx.exec_registry.clone(),
            process_id.clone(),
            chat_id.to_string(),
            SubscriptionConfig {
                regex_filter: None,
                max_duration: Duration::from_millis(50),
                max_events: DEFAULT_MAX_EVENTS,
                max_line_bytes: DEFAULT_MAX_LINE_BYTES,
                push: PushMode::Append,
            },
            Arc::new(AtomicBool::new(false)),
        );

        tokio::time::timeout(Duration::from_secs(1), handle)
            .await
            .unwrap()
            .unwrap();
        let _ = gcx.exec_registry.kill(&process_id).await;
    }

    #[test]
    fn process_subscribe_is_registered() {
        let names = crate::tools::tools_list::builtin_system_tools(String::new())
            .into_iter()
            .map(|tool| tool.tool_description().name)
            .collect::<Vec<_>>();
        assert!(names.contains(&"process_subscribe".to_string()));
    }

    #[test]
    #[serial(runtime_settings)]
    fn process_subscribe_preview_bytes_setting_changes_preview_and_is_loud() {
        let _guard = SettingsGuard::install(|settings| {
            settings.process_subscribe_preview_bytes = 20;
        });
        let mut state = SubscriptionState::new(None, 100, DEFAULT_MAX_LINE_BYTES);
        assert_eq!(state.preview_bytes, 20);

        let long_line = "z".repeat(300);
        state.add_summary_line(long_line, Instant::now());
        let summary = state.active_summary.take().expect("summary builder");

        assert!(
            summary.first_line.starts_with(&"z".repeat(20)),
            "expected the preview cut at 20 bytes, got: {}",
            summary.first_line
        );
        assert!(
            summary.first_line.contains("showing 20 of 300 line bytes")
                && summary
                    .first_line
                    .contains("process_subscribe_preview_bytes = 20"),
            "expected a quantified preview-truncation marker, got: {}",
            summary.first_line
        );
    }

    #[test]
    fn process_subscribe_preview_line_is_unmarked_when_it_fits() {
        assert_eq!(preview_line("short line", 200), "short line");
    }
}
