pub use refact_tools::tools::tool_handoff_to_mode::*;

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
use uuid::Uuid;
#[cfg(test)]
#[allow(unused_imports)]
use refact_agentic::mode_transition::{
    insert_goal_messages_before_plan, transfer_goal_ownership, ParsedDecisions,
};
#[cfg(test)]
#[allow(unused_imports)]
use refact_runtime_api::GlobalContextCtx;
#[cfg(test)]
#[allow(unused_imports)]
use refact_runtime_api::AtCommandsContext;
#[cfg(test)]
#[allow(unused_imports)]
use refact_runtime_api::trajectory_runtime::{
    chat_id_is_planner_for_task, resolve_task_planner_controller_chat_id,
    verified_planner_linked_root_chat_id,
};
#[cfg(test)]
#[allow(unused_imports)]
use refact_core::chat_types::{ChatContent, ChatMessage, ContextEnum};
#[cfg(test)]
#[allow(unused_imports)]
use refact_postprocessing::pp_command_output::OutputFilter;
#[cfg(test)]
#[allow(unused_imports)]
use refact_runtime_api::task_storage as storage;
#[cfg(test)]
#[allow(unused_imports)]
use crate::tools::tool_task_documents::{
    create_document_at, documents_dir_for_task_for_ctx, next_available_slug_at,
};
#[cfg(test)]
#[allow(unused_imports)]
use refact_chat_history::trajectory_ops::sanitize_messages_for_new_thread;
#[cfg(test)]
#[allow(unused_imports)]
use refact_chat_history::trajectory_snapshot::TrajectorySnapshot;
#[cfg(test)]
#[allow(unused_imports)]
use refact_runtime_api::SessionState;
#[cfg(test)]
#[allow(unused_imports)]
use crate::tools::tools_description::{
    MatchConfirmDeny, MatchConfirmDenyResult, Tool, ToolDesc, ToolSource, ToolSourceType,
};
#[cfg(test)]
#[allow(unused_imports)]
use refact_runtime_api::customization_registry::get_mode_config;
#[cfg(test)]
#[allow(unused_imports)]
use refact_chat_api::setparams::map_legacy_mode_to_id;

#[cfg(test)]
mod deferred_tests {
    use super::*;

    #[test]
    fn report_transfer_preserves_archive_and_ledger_stop_wins() {
        let mut session = refact_chat_api::ChatSession::new("source".into());
        session.install_goal("agent", "Ship safely", true, Default::default());
        let active = session.messages.clone();
        let mut raw = vec![ChatMessage::new("user".into(), "immutable archive".into())];
        raw.extend(
            transition_report(&active, 1, "model".into(), "agent".into(), "agent".into()).unwrap(),
        );
        let archive = serde_json::to_vec(&raw).unwrap();
        let transfer =
            transition_goal(&raw, session.goal.as_ref(), "source", "target", "agent").unwrap();
        assert!(transfer.transferred());
        assert_eq!(
            serde_json::to_vec(&transfer.source_messages).unwrap(),
            archive
        );
        let report = transition_report(
            &transfer.target_messages,
            1,
            "model".into(),
            "agent".into(),
            "agent".into(),
        )
        .unwrap();
        assert_eq!(report.len(), 1);
        assert_eq!(report[0].role, "compression_report");
        assert!(refact_core::active_context::active_context(&report)
            .unwrap()
            .messages
            .iter()
            .any(|m| m.role == "goal"));
        session.goal_ledger_append(refact_chat_api::GoalLedgerOp::StatusChanged {
            from: refact_chat_api::GoalStatus::Active,
            to: refact_chat_api::GoalStatus::Stopped,
            reason: "user stop".into(),
        });
        let goal = transition_source_goal(&session);
        assert!(
            !transition_goal(&raw, goal.as_ref(), "source", "target", "agent")
                .unwrap()
                .transferred()
        );
    }

    #[test]
    fn completed_handoff_updates_actual_ack_and_emits_navigation_event() {
        let mut session = refact_chat_api::ChatSession::new("source".into());
        session.add_message(ChatMessage {
            role: "tool".into(),
            tool_call_id: "handoff".into(),
            content: ChatContent::SimpleText("queued".into()),
            ..Default::default()
        });
        let id = session.messages[0].message_id.clone();
        let mut events = session.subscribe();
        let (updated, message) = handoff_completion_messages(
            &session.messages,
            "handoff",
            &json!({
                "type": "handoff_to_mode", "status": "completed", "new_chat_id": "target"
            }),
        )
        .unwrap();
        publish_handoff_completion(&mut session, updated, message);
        assert_eq!(session.messages[0].message_id, id);
        let mut found = false;
        while let Ok(event) = events.try_recv() {
            let event: Value = serde_json::from_str(&event).unwrap();
            if event["type"] == "message_updated" {
                assert_eq!(event["message_id"], id);
                assert!(event["message"]["content"]
                    .as_str()
                    .unwrap()
                    .contains("completed"));
                found = true;
            }
        }
        assert!(
            found,
            "completion must emit MessageUpdated for navigation middleware"
        );
    }

    #[test]
    fn completed_handoff_emits_message_updated_for_existing_ack() {
        let mut session = refact_chat_api::ChatSession::new("source".into());
        session.add_message(ChatMessage {
            role: "tool".into(),
            tool_call_id: "handoff".into(),
            content: ChatContent::SimpleText("queued".into()),
            ..Default::default()
        });
        let id = session.messages[0].message_id.clone();
        let mut events = session.subscribe();
        let (messages, message) = handoff_completion_messages(
            &session.messages,
            "handoff",
            &json!({
                "type": "handoff_to_mode", "status": "completed", "new_chat_id": "target"
            }),
        )
        .unwrap();
        publish_handoff_completion(&mut session, messages, message);
        assert_eq!(session.messages[0].message_id, id);
        let mut found = false;
        while let Ok(event) = events.try_recv() {
            let event: Value = serde_json::from_str(&event).unwrap();
            if event["type"] == "message_updated" {
                assert_eq!(event["message_id"], id);
                assert!(event["message"]["content"]
                    .as_str()
                    .unwrap()
                    .contains("completed"));
                found = true;
            }
        }
        assert!(found, "completion must notify navigation middleware");
    }

    #[tokio::test]
    async fn handoff_queue_keeps_sibling_calls_and_allows_same_mode_restart() {
        use refact_core::chat_types::{ChatToolCall, ChatToolFunction};
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let app = refact_runtime_api::app_state_from_gcx(gcx.clone()).await;
        let mut session = refact_chat_api::ChatSession::new("queue-source".into());
        session.thread.mode = "agent".into();
        session.add_message(ChatMessage::new("user".into(), "Restart".into()));
        session.add_message(ChatMessage {
            role: "assistant".into(),
            tool_calls: Some(
                [("handoff", "handoff_to_mode"), ("sibling", "cat")]
                    .into_iter()
                    .map(|(id, name)| ChatToolCall {
                        id: id.into(),
                        index: None,
                        function: ChatToolFunction {
                            name: name.into(),
                            arguments: "{}".into(),
                        },
                        tool_type: "function".into(),
                        extra_content: None,
                        started_at_ms: None,
                        completed_at_ms: None,
                    })
                    .collect(),
            ),
            ..Default::default()
        });
        session.set_runtime_state(SessionState::ExecutingTools, None);
        let before = serde_json::to_value(&session.messages).unwrap();
        let session = Arc::new(AMutex::new(session));
        gcx.chat_sessions
            .write()
            .await
            .insert("queue-source".into(), session.clone());
        let ccx = Arc::new(AMutex::new(
            crate::at_commands::at_commands::at_commands_new_from_app(
                app,
                4096,
                20,
                false,
                vec![],
                "queue-source".into(),
                None,
                "model".into(),
                None,
                None,
            )
            .await,
        ));
        let mut tool = ToolHandoffToMode {
            config_path: String::new(),
        };
        let args = HashMap::from([("target_mode".into(), json!("agent"))]);
        let (_, messages) = tool
            .tool_execute(ccx.clone(), &"handoff".into(), &args)
            .await
            .unwrap();
        assert_eq!(
            serde_json::to_value(&session.lock().await.messages).unwrap(),
            before
        );
        let payload = session.lock().await.pending_mode_handoff.clone().unwrap();
        assert_eq!(payload["tool_call_id"], "handoff");
        let ContextEnum::ChatMessage(result) = &messages[0] else {
            panic!("expected acknowledgement")
        };
        let result: Value = serde_json::from_str(&result.content.content_text_only()).unwrap();
        assert_eq!(result["queued"], true);
        assert!(result.get("new_chat_id").is_none());
        assert!(tool
            .tool_execute(ccx, &"handoff".into(), &args)
            .await
            .is_err());
    }
}
