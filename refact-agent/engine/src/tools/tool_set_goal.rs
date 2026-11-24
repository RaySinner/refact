pub use refact_tools::tools::tool_set_goal::*;

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
use refact_chat_api::GoalBudget;
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
use crate::chat::goal_role::{self, GoalInstallReport};
#[cfg(test)]
#[allow(unused_imports)]
use refact_chat_api::internal_roles::{self, EventSubkind};
#[cfg(test)]
#[allow(unused_imports)]
use refact_chat_api::types::{ChatSession, PendingDelivery, PushMode};
#[cfg(test)]
#[allow(unused_imports)]
use crate::tools::tools_description::{Tool, ToolDesc, ToolSource, ToolSourceType};
#[cfg(test)]
#[allow(unused_imports)]
use refact_chat_api::setparams::map_legacy_mode_to_id;

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

    use crate::app_state::AppState;
    use refact_core::chat_types::{ChatToolCall, ChatToolFunction};
    use refact_chat_api::internal_roles::{EVENT_ROLE, GOAL_ROLE};
    use crate::llm::adapter::{AdapterSettings, LlmWireAdapter};
    use crate::llm::adapters::openai_chat::OpenAiChatAdapter;
    use crate::tools::tools_list::get_tools_for_mode;

    const CHAT_ID: &str = "set-goal-chat";

    async fn ccx_for_session(
        mode: &str,
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
        let mut session = ChatSession::new(CHAT_ID.to_string());
        session.thread.mode = mode.to_string();
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
    async fn set_goal_installs_goal_side_effect() {
        let (gcx, ccx) = ccx_for_session("agent").await;
        let mut tool = ToolSetGoal {
            config_path: String::new(),
        };
        let args = HashMap::from([("content".to_string(), json!("Ship the pond"))]);

        let (_, messages) = tool
            .tool_execute(ccx, &"call".to_string(), &args)
            .await
            .unwrap();

        assert_eq!(
            tool_result_json(&messages),
            json!({"version": 1, "supersedes": null})
        );
        let session_arc = gcx
            .chat_sessions
            .read()
            .await
            .get(CHAT_ID)
            .cloned()
            .unwrap();
        let session = session_arc.lock().await;
        assert!(session
            .messages
            .iter()
            .all(|message| message.extra.contains_key("delivery")));
        assert_eq!(delivery_messages(&session).len(), 2);
        assert_eq!(delivery_messages(&session)[0].role, GOAL_ROLE);
        assert_eq!(
            content_text(&delivery_messages(&session)[0]),
            "Ship the pond"
        );
        assert_eq!(
            delivery_messages(&session)[0].extra["goal"]["active"],
            json!(true)
        );
        assert_eq!(delivery_messages(&session)[1].role, EVENT_ROLE);
        assert_eq!(
            delivery_messages(&session)[1].extra["event"],
            json!({
                "subkind": "system_notice",
                "source": "tool.set_goal",
                "payload": {"version": 1}
            })
        );
    }

    #[tokio::test]
    async fn rejects_when_goal_exists() {
        let (gcx, ccx) = ccx_for_session("agent").await;
        let session_arc = gcx
            .chat_sessions
            .read()
            .await
            .get(CHAT_ID)
            .cloned()
            .unwrap();
        {
            let mut session = session_arc.lock().await;
            crate::chat::goal_role::install_goal(
                &mut session,
                "agent",
                "existing",
                true,
                GoalBudget::default(),
            );
        }
        let mut tool = ToolSetGoal {
            config_path: String::new(),
        };
        let args = HashMap::from([("content".to_string(), json!("second"))]);

        let err = tool
            .tool_execute(ccx, &"call".to_string(), &args)
            .await
            .unwrap_err();

        assert_eq!(err, "goal already exists; use update_goal");
        let session = session_arc.lock().await;
        assert!(delivery_messages(&session).is_empty());
    }

    #[tokio::test]
    async fn empty_content_rejected() {
        let (_gcx, ccx) = ccx_for_session("agent").await;
        let mut tool = ToolSetGoal {
            config_path: String::new(),
        };
        let args = HashMap::from([("content".to_string(), json!("  \n\t"))]);

        let err = tool
            .tool_execute(ccx, &"call".to_string(), &args)
            .await
            .unwrap_err();

        assert_eq!(err, "argument `content` must be non-empty");
    }

    #[tokio::test]
    async fn model_cannot_replace_or_change_autonomy() {
        for privileged_field in ["replace", "autonomy"] {
            let (_gcx, ccx) = ccx_for_session("agent").await;
            let mut tool = ToolSetGoal {
                config_path: String::new(),
            };
            let args = HashMap::from([
                ("content".to_string(), json!("replacement")),
                (privileged_field.to_string(), json!(true)),
            ]);

            let err = tool
                .tool_execute(ccx, &"call".to_string(), &args)
                .await
                .unwrap_err();

            assert_eq!(err, "replace and autonomy require an explicit user command");
        }
    }

    #[tokio::test]
    async fn set_goal_side_effects_are_after_tool_result() {
        let (gcx, ccx) = ccx_for_session("agent").await;
        let session_arc = gcx
            .chat_sessions
            .read()
            .await
            .get(CHAT_ID)
            .cloned()
            .unwrap();
        session_arc.lock().await.add_message(assistant_tool_call(
            "call-goal",
            "set_goal",
            r#"{"content":"new"}"#,
        ));

        let mut tool = ToolSetGoal {
            config_path: String::new(),
        };
        let args = HashMap::from([("content".to_string(), json!("new"))]);
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
        assert_eq!(roles, vec!["assistant", "tool", "goal", "event"]);
        assert_openai_tool_result_not_preceded_by_hidden_role(session.messages.clone());
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
                .any(|tool| tool.tool_description().name == "set_goal");
            assert!(has_tool, "{mode} should expose set_goal");
        }
        for mode in ["NO_TOOLS", "shell", "explore"] {
            let has_tool = get_tools_for_mode(gcx.clone(), mode, None)
                .await
                .into_iter()
                .any(|tool| tool.tool_description().name == "set_goal");
            assert!(!has_tool, "{mode} should not expose set_goal");
        }
    }
}
