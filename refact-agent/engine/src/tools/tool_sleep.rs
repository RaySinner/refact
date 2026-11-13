pub use refact_tools::tools::tool_sleep::*;

#[cfg(test)]
use std::collections::HashMap;
#[cfg(test)]
use std::sync::Arc;
#[cfg(test)]
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(test)]
use std::sync::Mutex as StdMutex;
#[cfg(test)]
use std::time::Duration;
#[cfg(test)]
use serde_json::{json, Value};
#[cfg(test)]
use tokio::sync::Mutex as AMutex;
#[cfg(test)]
use tokio::sync::Notify;
#[cfg(test)]
use tokio::time::sleep;
#[cfg(test)]
use refact_core::chat_types::{ChatContent, ChatMessage, ContextEnum, PendingDelivery, PushMode};
#[cfg(test)]
use crate::tools::tools_description::{Tool, WaitInterrupt, WaitInterruptCause};

#[cfg(test)]
async fn sleep_with_ticks(
    duration_ms: u64,
    tick_interval_ms: Option<u64>,
    abort_flag: Arc<AtomicBool>,
    abort_notify: Option<Arc<Notify>>,
) -> SleepOutcome {
    sleep_with_ticks_interruptible(
        duration_ms,
        tick_interval_ms,
        WaitInterrupt {
            abort_flag,
            wait_flag: Arc::new(AtomicBool::new(false)),
            cause: Arc::new(StdMutex::new(WaitInterruptCause::MessageArrived)),
            notify: abort_notify,
        },
    )
    .await
}


#[cfg(test)]
mod tests {
    use super::*;
    use refact_runtime_api::AtCommandsContext;
    use refact_core::chat_types::{ChatToolCall, ChatToolFunction};
    use refact_chat_api::ChatSession;
    use crate::llm::adapter::{AdapterSettings, LlmWireAdapter};
    use crate::llm::adapters::openai_chat::OpenAiChatAdapter;
    use std::sync::atomic::AtomicBool;

    fn sleep_args(tick_interval_ms: Option<u64>, description: &str) -> HashMap<String, Value> {
        let mut args = HashMap::new();
        args.insert("duration_ms".to_string(), json!(MIN_DURATION_MS));
        args.insert("description".to_string(), json!(description));
        if let Some(tick_interval_ms) = tick_interval_ms {
            args.insert("tick_interval_ms".to_string(), json!(tick_interval_ms));
        }
        args
    }

    fn parse_error(args: HashMap<String, Value>) -> String {
        match parse_sleep_request(&args) {
            Ok(_) => panic!("expected parse_sleep_request to fail"),
            Err(error) => error,
        }
    }

    #[tokio::test(start_paused = true)]
    async fn delivery_interrupt_preserves_ticks_without_aborting_turn() {
        let abort = Arc::new(AtomicBool::new(false));
        let flag = Arc::new(AtomicBool::new(false));
        let notify = Arc::new(Notify::new());
        let interrupt = WaitInterrupt {
            abort_flag: abort.clone(),
            wait_flag: flag.clone(),
            cause: Arc::new(StdMutex::new(WaitInterruptCause::MessageArrived)),
            notify: Some(notify.clone()),
        };
        let run = tokio::spawn(sleep_with_ticks_interruptible(
            30_000,
            Some(5_000),
            interrupt,
        ));
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_millis(5_001)).await;
        tokio::task::yield_now().await;
        flag.store(true, Ordering::SeqCst);
        notify.notify_waiters();
        let outcome = run.await.unwrap();
        assert!(outcome.interrupted);
        assert_eq!(outcome.ticks.len(), 1);
        assert!(!abort.load(Ordering::SeqCst));
    }

    #[tokio::test(start_paused = true)]
    async fn pending_non_wake_delivery_does_not_stop_sleep_before_deadline() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let session_arc = Arc::new(AMutex::new(ChatSession::new(CHAT_ID.to_string())));
        gcx.chat_sessions
            .write()
            .await
            .insert(CHAT_ID.to_string(), session_arc.clone());
        {
            let mut session = session_arc.lock().await;
            session.add_message(assistant_tool_call("call-sleep"));
            session
                .enqueue_delivery(PendingDelivery::with_id(
                    "late-notice",
                    vec![ChatMessage::new("user".into(), "queued while sleeping".into())],
                    PushMode::Append,
                    "hooks",
                    false,
                ))
                .unwrap();
            session.begin_interruptible_wait();
            assert!(!session.wait_interrupt_flag.load(Ordering::SeqCst));
            assert_eq!(session.pending_deliveries.len(), 1);
        }

        let ccx = make_ccx(gcx).await;
        let mut tool = ToolSleep {
            config_path: String::new(),
        };
        let args = HashMap::from([
            ("duration_ms".to_string(), json!(30_000)),
            ("description".to_string(), json!("wait")),
        ]);
        let run = tokio::spawn(async move {
            tool.tool_execute(ccx, &"call-sleep".to_string(), &args)
                .await
                .unwrap()
        });
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_millis(30_000)).await;
        let (tool_failed, results) = run.await.unwrap();
        assert!(!tool_failed);

        let mut sleep_body = None;
        for message in results {
            let ContextEnum::ChatMessage(message) = message else {
                panic!("expected chat message")
            };
            sleep_body = Some(message.extra["sleep"].clone());
        }
        let body = sleep_body.expect("sleep tool result body");
        assert_eq!(body["slept_ms"], json!(30_000));
        assert_eq!(body["interrupted"], json!(false));
        assert!(body.get("reason").is_none());

        let mut session = session_arc.lock().await;
        session.end_interruptible_wait();
        assert!(!session.wait_interrupt_flag.load(Ordering::SeqCst));
        assert_eq!(session.pending_deliveries.len(), 1);
        assert_eq!(session.pending_deliveries[0].id, "late-notice");
    }

    #[test]
    fn sleep_push_defaults_to_append() {
        let mut args = sleep_args(None, "wait");
        assert_eq!(parse_sleep_request(&args).unwrap().push, PushMode::Append);
        args.insert("push".into(), json!("when_idle"));
        assert_eq!(parse_sleep_request(&args).unwrap().push, PushMode::WhenIdle);
        args.insert("push".into(), json!(true));
        assert!(parse_sleep_request(&args).is_err());
    }

    #[tokio::test]
    async fn ticks_all_modes_wait_for_own_result_and_c_waits_for_turn() {
        for push in [PushMode::Append, PushMode::Preempt, PushMode::WhenIdle] {
            let gcx = crate::global_context::tests::make_test_gcx().await;
            let app = refact_runtime_api::app_state_from_gcx(gcx.clone()).await;
            let session = Arc::new(AMutex::new(ChatSession::new(CHAT_ID.into())));
            gcx.chat_sessions
                .write()
                .await
                .insert(CHAT_ID.into(), session.clone());
            {
                let mut session = session.lock().await;
                session.turn_depth = 1;
                session.add_message(assistant_tool_call("own-sleep"));
            }
            queue_ticks(
                app,
                CHAT_ID.into(),
                vec![tick_event(5000, 100)],
                push,
                "own-sleep",
            )
            .await
            .unwrap();
            let mut session = session.lock().await;
            assert!(!session.abort_flag.load(Ordering::Relaxed));
            session.drain_pending_deliveries();
            assert_eq!(session.messages.len(), 1);
            session.add_message(ChatMessage {
                role: "tool".into(),
                tool_call_id: "own-sleep".into(),
                content: ChatContent::SimpleText("done".into()),
                ..Default::default()
            });
            session.drain_pending_deliveries();
            if push == PushMode::WhenIdle {
                assert_eq!(session.messages.len(), 2);
                session.turn_depth = 0;
                session.drain_pending_deliveries();
            }
            assert!(session.messages.iter().any(|m| m
                .extra
                .get("event")
                .is_some_and(|event| event["subkind"] == "tick")));
            assert_eq!(session.messages[1].role, "tool");
        }
    }

    #[test]
    fn parse_rejects_zero_tick_interval() {
        let error = parse_error(sleep_args(Some(0), "wait"));

        assert_eq!(
            error,
            format!("tick_interval_ms must be at least {MIN_TICK_INTERVAL_MS}")
        );
    }

    #[test]
    fn parse_rejects_tick_interval_below_schema_minimum() {
        let error = parse_error(sleep_args(Some(MIN_TICK_INTERVAL_MS - 1), "wait"));

        assert_eq!(
            error,
            format!("tick_interval_ms must be at least {MIN_TICK_INTERVAL_MS}")
        );
    }

    #[test]
    fn parse_rejects_empty_description() {
        let error = parse_error(sleep_args(None, ""));

        assert_eq!(error, "description must be a non-empty string");
    }

    #[test]
    fn parse_rejects_whitespace_description() {
        let error = parse_error(sleep_args(None, "   \t\n"));

        assert_eq!(error, "description must be a non-empty string");
    }

    #[test]
    fn parse_trims_description() {
        let request = parse_sleep_request(&sleep_args(None, "  wait  ")).unwrap();

        assert_eq!(request.description, "wait");
    }

    const CHAT_ID: &str = "sleep-chat";

    async fn make_ccx(
        gcx: Arc<crate::global_context::GlobalContext>,
    ) -> Arc<AMutex<AtCommandsContext>> {
        Arc::new(AMutex::new(
            crate::at_commands::at_commands::at_commands_new_from_app(
                refact_runtime_api::app_state_from_gcx(gcx).await,
                4096,
                20,
                false,
                vec![],
                CHAT_ID.to_string(),
                None,
                "model".to_string(),
                None,
                None,
            )
            .await,
        ))
    }

    fn assistant_tool_call(tool_call_id: &str) -> ChatMessage {
        ChatMessage {
            role: "assistant".to_string(),
            content: ChatContent::SimpleText(String::new()),
            tool_calls: Some(vec![ChatToolCall {
                id: tool_call_id.to_string(),
                index: Some(0),
                function: ChatToolFunction {
                    name: "sleep".to_string(),
                    arguments: r#"{"duration_ms":100,"tick_interval_ms":50,"description":"wait"}"#
                        .to_string(),
                },
                tool_type: "function".to_string(),
                extra_content: None,
                started_at_ms: None,
                completed_at_ms: None,
            }]),
            ..Default::default()
        }
    }

    fn default_settings() -> AdapterSettings {
        AdapterSettings {
            api_key: "test-key".to_string(),
            auth_token: String::new(),
            endpoint: "https://api.openai.com/v1/chat/completions".to_string(),
            extra_headers: Default::default(),
            model_name: "gpt-4.1".to_string(),
            supports_tools: true,
            supports_reasoning: true,
            reasoning_type: None,
            supports_temperature: true,
            supports_max_completion_tokens: false,
            eof_is_done: false,
            supports_web_search: false,
            supports_cache_control: false,
        }
    }

    fn assert_openai_tool_result_not_preceded_by_hidden_role(messages: Vec<ChatMessage>) {
        let req = crate::llm::canonical::LlmRequest::new("gpt-4.1".to_string(), messages);
        let body = OpenAiChatAdapter
            .build_http(&refact_privacy::testing::cleared(req), &default_settings())
            .unwrap()
            .body;
        let wire_messages = body["messages"].as_array().unwrap();
        let tool_index = wire_messages
            .iter()
            .position(|message| message["role"] == "tool")
            .expect("tool result missing from wire messages");
        let prior = &wire_messages[tool_index - 1];
        assert_eq!(prior["role"], "assistant");
        assert!(
            prior.get("tool_calls").is_some(),
            "prior message: {prior:?}"
        );
    }

    #[tokio::test]
    async fn short_sleep_returns_correct_slept_ms() {
        let outcome = sleep_with_ticks(120, None, Arc::new(AtomicBool::new(false)), None).await;

        assert!(!outcome.interrupted);
        assert!(
            (70..=170).contains(&outcome.slept_ms),
            "slept_ms was {}",
            outcome.slept_ms
        );
        assert!(outcome.ticks.is_empty());
    }

    #[tokio::test]
    async fn abort_midway_returns_interrupted() {
        let abort_flag = Arc::new(AtomicBool::new(false));
        let run = tokio::spawn({
            let abort_flag = abort_flag.clone();
            async move { sleep_with_ticks(2_000, None, abort_flag, None).await }
        });

        sleep(Duration::from_millis(120)).await;
        abort_flag.store(true, Ordering::Relaxed);
        let outcome = run.await.unwrap();

        assert!(outcome.interrupted);
        assert!(outcome.slept_ms < 500, "slept_ms was {}", outcome.slept_ms);
    }

    #[tokio::test]
    async fn abort_set_before_sleep_returns_interrupted_quickly() {
        let outcome = sleep_with_ticks(
            2_000,
            None,
            Arc::new(AtomicBool::new(true)),
            Some(Arc::new(Notify::new())),
        )
        .await;

        assert!(outcome.interrupted);
        assert!(outcome.slept_ms < 100, "slept_ms was {}", outcome.slept_ms);
    }

    #[tokio::test]
    async fn abort_polling_interrupts_without_notify_wakeup() {
        let abort_flag = Arc::new(AtomicBool::new(false));
        let abort_notify = Arc::new(Notify::new());
        let run = tokio::spawn({
            let abort_flag = abort_flag.clone();
            async move { sleep_with_ticks(2_000, None, abort_flag, Some(abort_notify)).await }
        });

        sleep(Duration::from_millis(20)).await;
        abort_flag.store(true, Ordering::Relaxed);
        let outcome = tokio::time::timeout(Duration::from_millis(500), run)
            .await
            .expect("sleep did not observe abort without notify wakeup")
            .unwrap();

        assert!(outcome.interrupted);
        assert!(outcome.slept_ms < 500, "slept_ms was {}", outcome.slept_ms);
    }

    #[tokio::test(start_paused = true)]
    async fn ticks_follow_exact_deadlines_and_exclude_end() {
        let outcome =
            sleep_with_ticks(15_000, Some(5_000), Arc::new(AtomicBool::new(false)), None).await;
        assert_eq!(outcome.slept_ms, 15_000);
        assert_eq!(outcome.ticks.len(), 2);
        assert_eq!(
            outcome.ticks[0].extra["event"]["payload"]["elapsed_ms"],
            5_000
        );
        assert_eq!(
            outcome.ticks[1].extra["event"]["payload"]["elapsed_ms"],
            10_000
        );
    }

    #[tokio::test]
    async fn tick_interval_injects_n_events() {
        let outcome =
            sleep_with_ticks(600, Some(200), Arc::new(AtomicBool::new(false)), None).await;

        assert!(!outcome.interrupted);
        assert!(
            (2..=3).contains(&outcome.ticks.len()),
            "tick count was {}",
            outcome.ticks.len()
        );
        assert!(outcome.ticks.iter().all(|message| message.role == "event"));
        assert_eq!(outcome.ticks[0].extra["event"]["subkind"], json!("tick"));
    }

    #[test]
    fn sleep_schema_documents_post_result_ticks() {
        let tool = ToolSleep {
            config_path: String::new(),
        };
        let desc = tool.tool_description();
        let tick_desc = desc.input_schema["properties"]["tick_interval_ms"]["description"]
            .as_str()
            .expect("tick_interval_ms description must be a string");
        assert!(
            tick_desc.contains("enqueued after the sleep tool result"),
            "tick_interval_ms description should mention post-result enqueueing, got: {tick_desc}"
        );
        assert!(
            !tick_desc.contains("react mid-sleep"),
            "tick_interval_ms description must not promise mid-sleep reaction, got: {tick_desc}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn sleep_tick_side_effects_are_after_tool_result() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let session_arc = Arc::new(AMutex::new(ChatSession::new(CHAT_ID.to_string())));
        gcx.chat_sessions
            .write()
            .await
            .insert(CHAT_ID.to_string(), session_arc.clone());
        session_arc
            .lock()
            .await
            .add_message(assistant_tool_call("call-sleep"));

        let ccx = make_ccx(gcx).await;
        let mut tool = ToolSleep {
            config_path: String::new(),
        };
        let args = HashMap::from([
            ("duration_ms".to_string(), json!(5_100)),
            ("tick_interval_ms".to_string(), json!(5_000)),
            ("description".to_string(), json!("wait")),
        ]);
        let run = tokio::spawn(async move {
            tool.tool_execute(ccx, &"call-sleep".to_string(), &args)
                .await
                .unwrap()
        });
        tokio::time::advance(Duration::from_millis(5_100)).await;
        let (_, results) = run.await.unwrap();

        let mut session = session_arc.lock().await;
        for message in results {
            let ContextEnum::ChatMessage(message) = message else {
                panic!("expected chat message")
            };
            session.add_message(message);
        }
        session.drain_post_tool_side_effects();
        session.drain_pending_deliveries();

        let roles: Vec<_> = session
            .messages
            .iter()
            .map(|message| message.role.as_str())
            .collect();
        assert_eq!(roles, vec!["assistant", "tool", "event"]);
        assert_eq!(session.messages[2].extra["event"]["subkind"], json!("tick"));
        assert_openai_tool_result_not_preceded_by_hidden_role(session.messages.clone());
    }
}
