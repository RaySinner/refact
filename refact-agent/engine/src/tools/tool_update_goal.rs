pub use refact_tools::tools::tool_update_goal::*;

#[cfg(test)]
#[allow(unused_imports)]
use std::collections::HashMap;
#[cfg(test)]
#[allow(unused_imports)]
use std::sync::Arc;
#[cfg(test)]
#[allow(unused_imports)]
use async_trait::async_trait;
#[cfg(test)]
#[allow(unused_imports)]
use serde_json::{json, Value};
#[cfg(test)]
#[allow(unused_imports)]
use tokio::sync::Mutex as AMutex;
#[cfg(test)]
#[allow(unused_imports)]
use refact_runtime_api::AtCommandsContext;
#[cfg(test)]
#[allow(unused_imports)]
use refact_core::chat_types::{ChatContent, ChatMessage, ContextEnum};
#[cfg(test)]
#[allow(unused_imports)]
use crate::chat::goal_role;
#[cfg(test)]
#[allow(unused_imports)]
use refact_chat_api::internal_roles;
#[cfg(test)]
#[allow(unused_imports)]
use refact_chat_api::types::{ChatSession, PendingDelivery, PushMode};
#[cfg(test)]
#[allow(unused_imports)]
use crate::tools::tools_description::{
    json_schema_from_params, Tool, ToolDesc, ToolSource, ToolSourceType,
};

#[cfg(test)]
mod tests {
    use super::*;

    fn delivery_messages(session: &ChatSession) -> Vec<ChatMessage> {
        session
            .messages
            .iter()
            .filter(|message| message.extra.contains_key("delivery"))
            .chain(
                session
                    .pending_deliveries
                    .iter()
                    .flat_map(|delivery| delivery.messages.iter()),
            )
            .cloned()
            .collect()
    }

    use refact_chat_api::GoalBudget;
    use crate::app_state::AppState;
    use refact_core::chat_types::{ChatToolCall, ChatToolFunction};
    use refact_chat_api::internal_roles::{EVENT_ROLE, GOAL_ROLE};
    use crate::llm::adapter::{AdapterSettings, LlmWireAdapter};
    use crate::llm::adapters::openai_chat::OpenAiChatAdapter;
    use crate::tools::tools_list::get_tools_for_mode;

    const CHAT_ID: &str = "update-goal-chat";

    async fn ccx_for_session(
        session: ChatSession,
    ) -> (
        Arc<crate::global_context::GlobalContext>,
        Arc<AMutex<AtCommandsContext>>,
    ) {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        refact_yaml_configs::project_configs_bootstrap::global_configs_try_create_all(
            &gcx.config_dir,
        )
        .await
        .unwrap();
        gcx.chat_sessions
            .write()
            .await
            .insert(CHAT_ID.to_string(), Arc::new(AMutex::new(session)));
        (gcx.clone(), make_ccx(gcx).await)
    }

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

    fn content_text(message: &ChatMessage) -> &str {
        match &message.content {
            ChatContent::SimpleText(text) => text,
            ChatContent::Multimodal(_) | ChatContent::ContextFiles(_) => {
                panic!("expected text message")
            }
        }
    }

    fn tool_result_json(messages: &[ContextEnum]) -> Value {
        match &messages[0] {
            ContextEnum::ChatMessage(message) => {
                serde_json::from_str(content_text(message)).unwrap()
            }
            ContextEnum::ContextFile(_) => panic!("expected tool chat message"),
        }
    }

    fn goal_delta_payload(message: &ChatMessage) -> &Value {
        &message.extra["event"]["payload"]
    }

    fn assistant_tool_call(tool_call_id: &str, name: &str, arguments: &str) -> ChatMessage {
        ChatMessage {
            role: "assistant".to_string(),
            content: ChatContent::SimpleText(String::new()),
            tool_calls: Some(vec![ChatToolCall {
                id: tool_call_id.to_string(),
                index: Some(0),
                function: ChatToolFunction {
                    name: name.to_string(),
                    arguments: arguments.to_string(),
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
    async fn appends_goal_delta_event() {
        let mut session = ChatSession::new(CHAT_ID.to_string());
        crate::chat::goal_role::install_goal(
            &mut session,
            "agent",
            "base goal",
            true,
            GoalBudget::default(),
        );
        session.add_message(internal_roles::goal_delta(
            "tool.update_goal",
            json!({"seq": 1}),
            "first note",
        ));
        let (gcx, ccx) = ccx_for_session(session).await;
        let mut tool = ToolUpdateGoal {
            config_path: String::new(),
        };
        let args = HashMap::from([("note".to_string(), json!("second note"))]);

        let (_, messages) = tool
            .tool_execute(ccx.clone(), &"call".to_string(), &args)
            .await
            .unwrap();

        assert_eq!(
            tool_result_json(&messages),
            json!({"seq": 2, "truncated": false})
        );
        let session_arc = gcx
            .chat_sessions
            .read()
            .await
            .get(CHAT_ID)
            .cloned()
            .unwrap();
        let session = session_arc.lock().await;
        assert_eq!(delivery_messages(&session).len(), 1);
        assert_eq!(delivery_messages(&session)[0].role, EVENT_ROLE);
        assert_eq!(content_text(&delivery_messages(&session)[0]), "second note");
        assert_eq!(
            delivery_messages(&session)[0].extra["event"],
            json!({
                "subkind": "goal_delta",
                "source": "tool.update_goal",
                "payload": {
                    "seq": 2,
                    "kind": "progress",
                    "author": "model",
                    "revision": 1
                }
            })
        );
        assert_eq!(session.goal.as_ref().unwrap().revision, 1);
    }

    #[tokio::test]
    async fn oversized_note_is_truncated_with_metadata() {
        let mut session = ChatSession::new(CHAT_ID.to_string());
        crate::chat::goal_role::install_goal(
            &mut session,
            "agent",
            "base goal",
            true,
            GoalBudget::default(),
        );
        let (gcx, ccx) = ccx_for_session(session).await;
        let mut tool = ToolUpdateGoal {
            config_path: String::new(),
        };
        let note = "a".repeat(internal_roles::MAX_GOAL_DELTA_CHARS + 100);
        let original_chars = note.chars().count();
        let args = HashMap::from([("note".to_string(), json!(note))]);

        let (_, messages) = tool
            .tool_execute(ccx, &"call".to_string(), &args)
            .await
            .unwrap();

        let result = tool_result_json(&messages);
        assert_eq!(result["seq"], json!(1));
        assert_eq!(result["truncated"], json!(true));
        assert_eq!(result["original_chars"], json!(original_chars));
        let session_arc = gcx
            .chat_sessions
            .read()
            .await
            .get(CHAT_ID)
            .cloned()
            .unwrap();
        let session = session_arc.lock().await;
        let deliveries = delivery_messages(&session);
        let delta = &deliveries[0];
        let content = content_text(delta);
        assert!(content.chars().count() <= internal_roles::MAX_GOAL_DELTA_CHARS);
        assert!(content.contains("[truncated:"));
        assert_eq!(goal_delta_payload(delta)["truncated"], json!(true));
        let kept_chars = goal_delta_payload(delta)["kept_chars"].as_u64().unwrap() as usize;
        assert_eq!(result["kept_chars"], json!(kept_chars));
    }

    #[tokio::test]
    async fn rejects_when_no_goal() {
        let session = ChatSession::new(CHAT_ID.to_string());
        let (_gcx, ccx) = ccx_for_session(session).await;
        let mut tool = ToolUpdateGoal {
            config_path: String::new(),
        };
        let args = HashMap::from([("note".to_string(), json!("new direction"))]);

        let err = tool
            .tool_execute(ccx, &"call".to_string(), &args)
            .await
            .unwrap_err();

        assert_eq!(err, "no goal to update; call set_goal first");
    }

    #[tokio::test]
    async fn rejects_terminal_and_transferred_goals_without_delivery() {
        for transferred in [false, true] {
            let mut session = ChatSession::new(CHAT_ID.to_string());
            crate::chat::goal_role::install_goal(
                &mut session,
                "agent",
                "base goal",
                true,
                GoalBudget::default(),
            );
            if transferred {
                let goal = session.goal.as_mut().unwrap();
                goal.active = false;
                goal.transferred_to = Some("new-owner".to_string());
            } else {
                session.goal_set_status(refact_chat_api::GoalStatus::Completed);
            }
            let (gcx, ccx) = ccx_for_session(session).await;
            let mut tool = ToolUpdateGoal {
                config_path: String::new(),
            };
            let args = HashMap::from([("note".to_string(), json!("progress"))]);

            assert!(tool
                .tool_execute(ccx, &"call".to_string(), &args)
                .await
                .is_err());

            let session_arc = gcx
                .chat_sessions
                .read()
                .await
                .get(CHAT_ID)
                .cloned()
                .unwrap();
            let session = session_arc.lock().await;
            assert!(delivery_messages(&session).is_empty());
        }
    }

    #[tokio::test]
    async fn missing_note_errors() {
        let session = ChatSession::new(CHAT_ID.to_string());
        let (_gcx, ccx) = ccx_for_session(session).await;
        let mut tool = ToolUpdateGoal {
            config_path: String::new(),
        };

        let err = tool
            .tool_execute(ccx, &"call".to_string(), &HashMap::new())
            .await
            .unwrap_err();

        assert_eq!(err, "argument `note` is missing");
    }

    #[tokio::test]
    async fn whitespace_only_note_errors() {
        let session = ChatSession::new(CHAT_ID.to_string());
        let (_gcx, ccx) = ccx_for_session(session).await;
        let mut tool = ToolUpdateGoal {
            config_path: String::new(),
        };
        let args = HashMap::from([("note".to_string(), json!("  \n\t"))]);

        let err = tool
            .tool_execute(ccx, &"call".to_string(), &args)
            .await
            .unwrap_err();

        assert_eq!(err, "argument `note` must be non-empty");
    }

    #[tokio::test]
    async fn update_goal_side_effects_are_after_tool_result() {
        let mut session = ChatSession::new(CHAT_ID.to_string());
        crate::chat::goal_role::install_goal(
            &mut session,
            "agent",
            "base goal",
            true,
            GoalBudget::default(),
        );
        session.add_message(assistant_tool_call(
            "call-goal",
            "update_goal",
            r#"{"note":"new"}"#,
        ));
        let (gcx, ccx) = ccx_for_session(session).await;
        let session_arc = gcx
            .chat_sessions
            .read()
            .await
            .get(CHAT_ID)
            .cloned()
            .unwrap();

        let mut tool = ToolUpdateGoal {
            config_path: String::new(),
        };
        let args = HashMap::from([("note".to_string(), json!("new"))]);
        let (_, results) = tool
            .tool_execute(ccx, &"call-goal".to_string(), &args)
            .await
            .unwrap();

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
        assert_eq!(roles, vec!["goal", "assistant", "tool", "event"]);
        assert_eq!(
            session.messages[3].extra["event"]["subkind"],
            json!("goal_delta")
        );
        assert_openai_tool_result_not_preceded_by_hidden_role(session.messages.clone());
    }

    #[test]
    fn queued_base_goal_allows_update() {
        let mut session = ChatSession::new(CHAT_ID.to_string());
        session.queue_post_tool_side_effect(internal_roles::goal(
            "agent",
            1,
            "base",
            None,
            true,
            GoalBudget::default(),
        ));

        assert!(has_base_goal_including_queued(&session).unwrap());
        assert_eq!(session.post_tool_side_effects[0].role, GOAL_ROLE);
    }

    #[tokio::test]
    async fn available_in_plan_supporting_modes() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        refact_yaml_configs::project_configs_bootstrap::global_configs_try_create_all(
            &gcx.config_dir,
        )
        .await
        .unwrap();

        for mode in ["agent", "task_agent", "task_planner"] {
            let has_tool = get_tools_for_mode(gcx.clone(), mode, None)
                .await
                .into_iter()
                .any(|tool| tool.tool_description().name == "update_goal");
            assert!(has_tool, "{mode} should expose update_goal");
        }
        for mode in ["NO_TOOLS", "shell", "explore"] {
            let has_tool = get_tools_for_mode(gcx.clone(), mode, None)
                .await
                .into_iter()
                .any(|tool| tool.tool_description().name == "update_goal");
            assert!(!has_tool, "{mode} should not expose update_goal");
        }
    }
}
