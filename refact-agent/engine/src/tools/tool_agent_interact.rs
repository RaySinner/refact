pub use refact_tools::tools::tool_agent_interact::*;

#[cfg(test)]
use crate::agents::registry::InboxMessage;
#[cfg(test)]
use std::collections::HashMap;
#[cfg(test)]
use std::sync::Arc;
#[cfg(test)]
use chrono::Duration;
#[cfg(test)]
use chrono::Utc;
#[cfg(test)]
use serde_json::json;
#[cfg(test)]
use serde_json::Value;
#[cfg(test)]
use tokio::sync::Mutex as AMutex;
#[cfg(test)]
use refact_runtime_api::GlobalContextCtx;
#[cfg(test)]
use refact_core::chat_types::PushMode;
#[cfg(test)]
use crate::agents::types::BackgroundAgent;
#[cfg(test)]
use crate::app_state::AppState;
#[cfg(test)]
use refact_runtime_api::AtCommandsContext;
#[cfg(test)]
use refact_core::chat_types::ChatContent;
#[cfg(test)]
use refact_core::chat_types::ChatMessage;
#[cfg(test)]
use refact_core::chat_types::ContextEnum;
#[cfg(test)]
use crate::tools::tools_description::Tool;

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::atomic::Ordering;

    use crate::agents::types::{BgAgentKind, CreateAgentRequest};
    use refact_chat_api::ChatSession;
    use crate::tools::tools_description::Tool;

    fn request(parent_chat_id: &str, title: &str) -> CreateAgentRequest {
        CreateAgentRequest {
            parent_chat_id: parent_chat_id.to_string(),
            parent_root_chat_id: Some(parent_chat_id.to_string()),
            parent_tool_call_id: None,
            kind: BgAgentKind::Subagent,
            config_name: "subagent".to_string(),
            title: title.to_string(),
            prompt: title.to_string(),
            target_files: vec![format!("src/{title}.rs")],
            model: "test/model".to_string(),
            model_type: Some("light".to_string()),
            goal_summary: Some("ship it".to_string()),
            plan_present: true,
            worktree_id: None,
            worktree_branch: Some(format!("branch/{title}")),
        }
    }

    async fn test_context(
        chat_id: &str,
        root_chat_id: &str,
        background_agent_id: Option<String>,
    ) -> (AppState, Arc<AMutex<AtCommandsContext>>) {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let app = refact_runtime_api::app_state_from_gcx(gcx).await;
        let ccx = Arc::new(AMutex::new(
            crate::at_commands::at_commands::at_commands_new_from_app(
                app.clone(),
                4096,
                20,
                false,
                vec![],
                chat_id.to_string(),
                Some(root_chat_id.to_string()),
                "test/model".to_string(),
                None,
                None,
            )
            .await,
        ));
        ccx.lock().await.background_agent_id = background_agent_id;
        (app, ccx)
    }

    async fn create(app: &AppState, parent_chat_id: &str, title: &str) -> BackgroundAgent {
        app.agents
            .create(request(parent_chat_id, title))
            .await
            .unwrap()
            .0
    }

    async fn parent_delivery_fixture(
        app: &AppState,
        chat_id: &str,
    ) -> (tempfile::TempDir, Arc<AMutex<ChatSession>>) {
        let workspace = tempfile::tempdir().unwrap();
        *app.gcx.documents_state.workspace_folders.lock().unwrap() =
            vec![workspace.path().to_path_buf()];
        let session = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        session
            .lock()
            .await
            .queue_processor_running
            .store(true, Ordering::SeqCst);
        app.chat
            .sessions
            .write()
            .await
            .insert(chat_id.to_string(), session.clone());
        (workspace, session)
    }

    async fn assert_parent_notice_persisted(app: &AppState, chat_id: &str, text: &str) {
        let saved = refact_runtime_api::trajectory_runtime::load_trajectory_for_chat(
            &app.chat_ctx(),
            chat_id,
        )
            .await
            .expect("parent notice trajectory persisted");
        assert!(saved
            .messages
            .iter()
            .any(|message| message.content.content_text_only().contains(text)));
    }

    fn args(items: &[(&str, Value)]) -> HashMap<String, Value> {
        items
            .iter()
            .map(|(key, value)| ((*key).to_string(), value.clone()))
            .collect()
    }

    fn text(result: (bool, Vec<ContextEnum>)) -> String {
        let ContextEnum::ChatMessage(message) = result.1.into_iter().next().unwrap() else {
            panic!("expected tool message")
        };
        let ChatContent::SimpleText(text) = message.content else {
            panic!("expected text")
        };
        text
    }

    #[tokio::test]
    async fn overview_renders_recursive_tree_and_caller_identity() {
        let (app, root_ccx) = test_context("root", "root", None).await;
        let child = create(&app, "root", "child").await;
        app.agents
            .mark_running(&child.agent_id, "child-chat".to_string())
            .await
            .unwrap();
        let grandchild = create(&app, "child-chat", "grandchild").await;
        app.agents
            .update_activity(
                &grandchild.agent_id,
                Some("checking files".to_string()),
                Some(2),
                Some(Some("cat".to_string())),
            )
            .await
            .unwrap();
        let child_ccx = Arc::new(AMutex::new(
            crate::at_commands::at_commands::at_commands_new_from_app(
                app.clone(),
                4096,
                20,
                false,
                vec![],
                "child-chat".to_string(),
                Some("root".to_string()),
                "test/model".to_string(),
                None,
                None,
            )
            .await,
        ));
        child_ccx.lock().await.background_agent_id = Some(child.agent_id.clone());

        let mut tool = ToolAgentsOverview {
            config_path: String::new(),
        };
        let root_output = text(
            tool.tool_execute(root_ccx, &"call".to_string(), &HashMap::new())
                .await
                .unwrap(),
        );
        assert!(root_output.contains(short_id(&child.agent_id)));
        assert!(root_output.contains(short_id(&grandchild.agent_id)));
        assert!(root_output.contains("tool: cat"));

        let child_output = text(
            tool.tool_execute(child_ccx, &"call".to_string(), &HashMap::new())
                .await
                .unwrap(),
        );
        assert!(child_output.contains(&format!("You are agent {}", child.agent_id)));
    }

    #[tokio::test]
    async fn child_messages_preserve_all_push_modes() {
        let (app, ccx) = test_context("root", "root", None).await;
        let child = create(&app, "root", "child").await;
        let mut tool = ToolAgentMessage {
            config_path: String::new(),
        };
        for mode in [PushMode::Append, PushMode::Preempt, PushMode::WhenIdle] {
            tool.tool_execute(
                ccx.clone(),
                &"call".to_string(),
                &args(&[
                    ("to", json!(child.agent_id)),
                    ("text", json!("hello")),
                    ("push", json!(mode)),
                ]),
            )
            .await
            .unwrap();
        }
        let record = app.agents.get_any(&child.agent_id).await.unwrap();
        assert_eq!(
            record
                .pending_deliveries
                .iter()
                .map(|d| d.push)
                .collect::<Vec<_>>(),
            vec![PushMode::Append, PushMode::Preempt, PushMode::WhenIdle]
        );
    }

    #[tokio::test]
    async fn parent_message_rejects_agent_outside_its_tree() {
        let (app, ccx) = test_context("root", "root", None).await;
        let outsider = create(&app, "other", "outsider").await;
        let mut tool = ToolAgentMessage {
            config_path: String::new(),
        };
        let error = tool
            .tool_execute(
                ccx,
                &"call".to_string(),
                &args(&[("to", json!(outsider.agent_id)), ("text", json!("hello"))]),
            )
            .await
            .unwrap_err();
        assert!(error.contains("direct children or descendants"));
    }

    #[tokio::test]
    async fn child_question_records_question_and_queues_parent_notice() {
        let (app, ccx) = test_context("child-chat", "root", None).await;
        let child = create(&app, "root", "child").await;
        ccx.lock().await.background_agent_id = Some(child.agent_id.clone());
        let (_workspace, session) = parent_delivery_fixture(&app, "root").await;

        let mut tool = ToolAgentMessage {
            config_path: String::new(),
        };
        let output = text(
            tool.tool_execute(
                ccx,
                &"call".to_string(),
                &args(&[
                    ("to", json!("parent")),
                    ("text", json!("May I edit frogs?")),
                    ("expects_reply", json!(true)),
                ]),
            )
            .await
            .unwrap(),
        );
        let after = app.agents.get_any(&child.agent_id).await.unwrap();
        assert_eq!(after.questions.len(), 1);
        assert!(output.contains(&after.questions[0].id));
        assert!(session.lock().await.messages.iter().any(|message| {
            message
                .content
                .content_text_only()
                .contains("subagent question")
        }));
        assert_parent_notice_persisted(&app, "root", "May I edit frogs?").await;
    }

    #[tokio::test]
    async fn reply_answers_question_and_delivers_inbox() {
        let (app, ccx) = test_context("root", "root", None).await;
        let child = create(&app, "root", "child").await;
        let (_, question_id) = app
            .agents
            .add_question(&child.agent_id, "Can I edit frogs?".to_string())
            .await
            .unwrap();
        let mut tool = ToolAgentMessage {
            config_path: String::new(),
        };
        tool.tool_execute(
            ccx,
            &"call".to_string(),
            &args(&[
                ("to", json!(child.agent_id)),
                ("reply_to", json!(question_id)),
                ("text", json!("Yes")),
            ]),
        )
        .await
        .unwrap();
        let after = app.agents.get_any(&child.agent_id).await.unwrap();
        assert_eq!(after.questions[0].answer.as_deref(), Some("Yes"));
        let inbox = app
            .agents
            .get_any(&child.agent_id)
            .await
            .unwrap()
            .pending_deliveries;
        assert_eq!(inbox[0].push, PushMode::Append);
        assert!(inbox[0].messages[0]
            .content
            .content_text_only()
            .contains(&format!("Answer to {question_id}: Yes")));
    }

    #[tokio::test]
    async fn child_note_delivers_to_parent_without_changing_progress() {
        let (app, ccx) = test_context("child-chat", "root", None).await;
        let (_workspace, _session) = parent_delivery_fixture(&app, "root").await;
        let child = create(&app, "root", "child").await;
        ccx.lock().await.background_agent_id = Some(child.agent_id.clone());
        let mut tool = ToolAgentMessage {
            config_path: String::new(),
        };
        tool.tool_execute(
            ccx,
            &"call".to_string(),
            &args(&[("to", json!("parent")), ("text", json!("I found a clue"))]),
        )
        .await
        .unwrap();
        let after = app.agents.get_any(&child.agent_id).await.unwrap();
        assert_eq!(after.progress, None);
        assert_parent_notice_persisted(&app, "root", "I found a clue").await;
    }

    #[tokio::test]
    async fn progress_report_requires_child_identity() {
        let (app, parent_ccx) = test_context("root", "root", None).await;
        let child = create(&app, "root", "child").await;
        let child_ccx = Arc::new(AMutex::new(
            crate::at_commands::at_commands::at_commands_new_from_app(
                app.clone(),
                4096,
                20,
                false,
                vec![],
                "child-chat".to_string(),
                Some("root".to_string()),
                "test/model".to_string(),
                None,
                None,
            )
            .await,
        ));
        child_ccx.lock().await.background_agent_id = Some(child.agent_id.clone());
        let mut tool = ToolProgressReport {
            config_path: String::new(),
        };
        assert!(tool
            .tool_execute(
                parent_ccx,
                &"call".to_string(),
                &args(&[("status_line", json!("working"))]),
            )
            .await
            .unwrap_err()
            .contains("not a subagent"));
        tool.tool_execute(
            child_ccx,
            &"call".to_string(),
            &args(&[("status_line", json!("working"))]),
        )
        .await
        .unwrap();
        assert_eq!(
            app.agents
                .get_any(&child.agent_id)
                .await
                .unwrap()
                .progress
                .as_deref(),
            Some("working")
        );
    }

    #[tokio::test]
    async fn inbox_drain_appends_system_notice() {
        let (app, ccx) = test_context("child-chat", "root", None).await;
        let child = create(&app, "root", "child").await;
        ccx.lock().await.background_agent_id = Some(child.agent_id.clone());
        app.agents
            .push_inbox(
                &child.agent_id,
                InboxMessage {
                    from: "user".to_string(),
                    text: "Please verify tests".to_string(),
                    queued_at: Utc::now(),
                },
            )
            .await
            .unwrap();
        let mut messages = Vec::new();
        crate::subchat::drain_background_agent_inbox(&ccx, &mut messages).await;
        assert_eq!(messages.len(), 1);
        assert!(messages[0]
            .content
            .content_text_only()
            .contains("[message from user]\nPlease verify tests"));
    }

    #[tokio::test]
    async fn unloaded_subchat_context_restores_background_agent_identity() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let app = refact_runtime_api::app_state_from_gcx(gcx).await;
        let child = create(&app, "parent", "child").await;
        app.agents
            .mark_running(&child.agent_id, "subchat-unloaded-child".to_string())
            .await
            .unwrap();

        let ccx = crate::at_commands::at_commands::at_commands_new_from_app(
            app,
            4096,
            20,
            false,
            vec![],
            "subchat-unloaded-child".to_string(),
            Some("parent".to_string()),
            "test/model".to_string(),
            None,
            None,
        )
        .await;

        assert_eq!(
            ccx.background_agent_id.as_deref(),
            Some(child.agent_id.as_str())
        );
    }

    #[tokio::test]
    async fn stateful_child_session_restores_identity_for_parent_tools() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let app = refact_runtime_api::app_state_from_gcx(gcx.clone()).await;
        let (_workspace, _parent_session) = parent_delivery_fixture(&app, "parent").await;
        let child = create(&app, "parent", "child").await;
        app.agents
            .mark_running(&child.agent_id, "subchat-child-chat".to_string())
            .await
            .unwrap();
        let config = refact_chat_api::SubchatConfig {
            tool_name: "subagent".to_string(),
            stateful: true,
            autonomous_no_confirm: false,
            auto_approve_editing_tools: false,
            auto_approve_dangerous_commands: false,
            chat_id: Some("subchat-child-chat".to_string()),
            title: Some("child".to_string()),
            parent_id: Some("parent".to_string()),
            link_type: Some("subagent".to_string()),
            root_chat_id: Some("parent".to_string()),
            tools: refact_chat_api::ToolsPolicy::All,
            max_steps: 1,
            prepend_system_prompt: false,
            wrap_up: None,
            task_meta: None,
            worktree: None,
            model: "test/model".to_string(),
            mode: "agent".to_string(),
            n_ctx: 4096,
            max_new_tokens: 512,
            temperature: None,
            reasoning_effort: None,
            cache_control: crate::llm::params::CacheControl::Ephemeral,
            parent_tool_call_id: None,
            parent_subchat_tx: None,
            abort_flag: None,
            soft_abort: false,
            activity_stamp: None,
            background_agent_id: Some(child.agent_id.clone()),
            subchat_depth: 1,
            final_step_force_answer: false,
            buddy_meta: None,
            step_progress: None,
            trace_parent: refact_chat_api::TraceParent::rooted("parent", "parent"),
        };
        crate::subchat::install_stateful_subchat_session(&app, "subchat-child-chat", &config, &[])
            .await;
        let thread = app
            .chat
            .sessions
            .read()
            .await
            .get("subchat-child-chat")
            .cloned()
            .unwrap()
            .lock()
            .await
            .thread
            .clone();
        refact_runtime_api::trajectory_runtime::save_trajectory_as_with_intent(
            &gcx.chat_ctx(gcx.clone()),
            &thread,
            &[],
            refact_chat_api::TrajectoryCommitIntent::Required,
        )
        .await;
        tokio::time::timeout(std::time::Duration::from_secs(30), async {
            loop {
                if refact_runtime_api::trajectory_runtime::load_trajectory_for_chat(
                    &gcx.chat_ctx(gcx.clone()),
                    "subchat-child-chat",
                )
                .await
                .is_some()
                {
                    return;
                }
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
        })
        .await
        .expect("child trajectory should persist");
        app.chat.sessions.write().await.remove("subchat-child-chat");

        crate::chat::get_or_create_session_with_trajectory(
            app.chat_ctx(),
            &app.chat.sessions,
            "subchat-child-chat",
        )
        .await;
        app.chat
            .sessions
            .read()
            .await
            .get("subchat-child-chat")
            .cloned()
            .unwrap()
            .lock()
            .await
            .thread
            .parent_id = None;

        let ccx = Arc::new(AMutex::new(
            crate::at_commands::at_commands::at_commands_new_from_app(
                app.clone(),
                4096,
                20,
                false,
                vec![],
                "subchat-child-chat".to_string(),
                Some("parent".to_string()),
                "test/model".to_string(),
                None,
                None,
            )
            .await,
        ));
        assert_eq!(
            ccx.lock().await.background_agent_id.as_deref(),
            Some(child.agent_id.as_str())
        );
        let mut progress = ToolProgressReport {
            config_path: String::new(),
        };
        progress
            .tool_execute(
                ccx.clone(),
                &"progress".to_string(),
                &args(&[("status_line", json!("stateful child running"))]),
            )
            .await
            .unwrap();
        let mut message = ToolAgentMessage {
            config_path: String::new(),
        };
        message
            .tool_execute(
                ccx,
                &"message".to_string(),
                &args(&[("to", json!("parent")), ("text", json!("hello parent"))]),
            )
            .await
            .unwrap();
        let record = app.agents.get_any(&child.agent_id).await.unwrap();
        assert_eq!(
            record.progress.as_deref(),
            Some("stateful child running"),
            "agent_message delivery is independent of progress activity"
        );
        assert_parent_notice_persisted(&app, "parent", "hello parent").await;
    }

#[tokio::test]
    async fn room_members_can_message_each_other() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let app = refact_runtime_api::app_state_from_gcx(gcx.clone()).await;
        let workspace = room_workspace();
        *gcx.documents_state.workspace_folders.lock().unwrap() = vec![workspace.path().to_path_buf()];

        let board_card = room_board_card(&["agent-T-1-arch", "agent-T-1-code"]);
        refact_runtime_api::task_storage::save_board(
            gcx.clone(),
            "task-room",
            &refact_tasks::types::TaskBoard {
                cards: vec![board_card],
                ..Default::default()
            },
        )
        .await
        .unwrap();

        let (session, _) = room_session(&app, "agent-T-1-code").await;
        let ccx = room_context(&app, "agent-T-1-arch", "task-room", "T-1").await;

        let mut tool = ToolAgentMessage {
            config_path: String::new(),
        };
        let delivered = text(
            tool.tool_execute(
                ccx,
                &"call".to_string(),
                &args(&[
                    ("to", json!("agent-T-1-code")),
                    ("text", json!("parser is done, review it")),
                ]),
            )
            .await
            .unwrap(),
        );

        assert!(delivered.contains("agent-T-1-code"), "{delivered}");
        assert!(
            session
                .lock()
                .await
                .messages
                .iter()
                .any(|message| message
                    .content
                    .content_text_only()
                    .contains("parser is done, review it")),
            "peer message must land in the peer's chat"
        );
    }

    #[tokio::test]
    async fn agents_on_different_cards_still_cannot_message() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let app = refact_runtime_api::app_state_from_gcx(gcx.clone()).await;
        let workspace = room_workspace();
        *gcx.documents_state.workspace_folders.lock().unwrap() = vec![workspace.path().to_path_buf()];

        refact_runtime_api::task_storage::save_board(
            gcx.clone(),
            "task-room",
            &refact_tasks::types::TaskBoard {
                cards: vec![
                    room_board_card(&["agent-T-1-arch", "agent-T-1-code"]),
                    room_board_card_for("T-2", &["agent-T-2-arch", "agent-T-2-code"]),
                ],
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let (_session, _peer) = room_session(&app, "agent-T-2-code").await;
        let ccx = room_context(&app, "agent-T-1-arch", "task-room", "T-1").await;

        let mut tool = ToolAgentMessage {
            config_path: String::new(),
        };
        let error = tool
            .tool_execute(
                ccx,
                &"call".to_string(),
                &args(&[
                    ("to", json!("agent-T-2-code")),
                    ("text", json!("crossing rooms")),
                ]),
            )
            .await
            .unwrap_err();

        assert!(error.contains("direct children or descendants"), "{error}");
    }

    #[tokio::test]
    async fn room_message_carries_author_chat_id() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let app = refact_runtime_api::app_state_from_gcx(gcx.clone()).await;
        let workspace = room_workspace();
        *gcx.documents_state.workspace_folders.lock().unwrap() =
            vec![workspace.path().to_path_buf()];

        refact_runtime_api::task_storage::save_board(
            gcx.clone(),
            "task-room",
            &refact_tasks::types::TaskBoard {
                cards: vec![room_board_card(&["agent-T-1-arch", "agent-T-1-code"])],
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let (session, _) = room_session(&app, "agent-T-1-code").await;
        let ccx = room_context(&app, "agent-T-1-arch", "task-room", "T-1").await;

        let mut tool = ToolAgentMessage {
            config_path: String::new(),
        };
        tool.tool_execute(
            ccx,
            &"call".to_string(),
            &args(&[
                ("to", json!("agent-T-1-code")),
                ("text", json!("parser is done")),
            ]),
        )
        .await
        .unwrap();

        let provenance = delivered_provenance(&session, "parser is done").await;
        assert_eq!(provenance.chat_id, "agent-T-1-arch");
        assert_eq!(provenance.card_id.as_deref(), Some("T-1"));
    }

    #[tokio::test]
    async fn room_message_carries_role_for_accent() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let app = refact_runtime_api::app_state_from_gcx(gcx.clone()).await;
        let workspace = room_workspace();
        *gcx.documents_state.workspace_folders.lock().unwrap() =
            vec![workspace.path().to_path_buf()];

        refact_runtime_api::task_storage::save_board(
            gcx.clone(),
            "task-room",
            &refact_tasks::types::TaskBoard {
                cards: vec![room_board_card(&["agent-T-1-arch", "agent-T-1-code"])],
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let (session, _) = room_session(&app, "agent-T-1-code").await;
        let ccx = room_context(&app, "agent-T-1-arch", "task-room", "T-1").await;

        let mut tool = ToolAgentMessage {
            config_path: String::new(),
        };
        tool.tool_execute(
            ccx,
            &"call".to_string(),
            &args(&[
                ("to", json!("agent-T-1-code")),
                ("text", json!("split the work first")),
            ]),
        )
        .await
        .unwrap();

        let provenance = delivered_provenance(&session, "split the work first").await;
        assert_eq!(provenance.role, "architect");
        assert_eq!(provenance.display_name, "architect");
        assert_eq!(
            provenance.accent(),
            refact_chat_api::role_accent("architect"),
            "the accent a GUI draws must be the one the engine assigned"
        );
    }

    #[tokio::test]
    async fn provenance_survives_trajectory_roundtrip() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let app = refact_runtime_api::app_state_from_gcx(gcx.clone()).await;
        let workspace = room_workspace();
        *gcx.documents_state.workspace_folders.lock().unwrap() =
            vec![workspace.path().to_path_buf()];

        refact_runtime_api::task_storage::save_board(
            gcx.clone(),
            "task-room",
            &refact_tasks::types::TaskBoard {
                cards: vec![room_board_card(&["agent-T-1-arch", "agent-T-1-code"])],
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let (session, _) = room_session(&app, "agent-T-1-code").await;
        let ccx = room_context(&app, "agent-T-1-arch", "task-room", "T-1").await;

        let mut tool = ToolAgentMessage {
            config_path: String::new(),
        };
        tool.tool_execute(
            ccx,
            &"call".to_string(),
            &args(&[
                ("to", json!("agent-T-1-code")),
                ("text", json!("survives a save")),
            ]),
        )
        .await
        .unwrap();

        // Force the same serialization a trajectory save performs, then read the provenance back.
        let message = session
            .lock()
            .await
            .messages
            .iter()
            .find(|message| {
                message
                    .content
                    .content_text_only()
                    .contains("survives a save")
            })
            .cloned()
            .expect("room message delivered");
        let restored: ChatMessage =
            serde_json::from_value(serde_json::to_value(&message).unwrap()).unwrap();

        let provenance = refact_chat_api::provenance_from_message(&restored).unwrap();
        assert_eq!(provenance.chat_id, "agent-T-1-arch");
        assert_eq!(provenance.role, "architect");
    }

    #[tokio::test]
    async fn room_message_defaults_to_all_members() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let app = refact_runtime_api::app_state_from_gcx(gcx.clone()).await;
        let workspace = room_workspace();
        *gcx.documents_state.workspace_folders.lock().unwrap() = vec![workspace.path().to_path_buf()];

        refact_runtime_api::task_storage::save_board(
            gcx.clone(),
            "task-room",
            &refact_tasks::types::TaskBoard {
                cards: vec![room_board_card(&[
                    "agent-T-1-arch",
                    "agent-T-1-code",
                    "agent-T-1-rev",
                ])],
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let (code, _) = room_session(&app, "agent-T-1-code").await;
        let (review, _) = room_session(&app, "agent-T-1-rev").await;
        let (architect, _) = room_session(&app, "agent-T-1-arch").await;
        let ccx = room_context(&app, "agent-T-1-arch", "task-room", "T-1").await;

        let mut tool = ToolAgentMessage {
            config_path: String::new(),
        };
        // No `to` at all: a room member addressing the room.
        let delivered = text(
            tool.tool_execute(ccx, &"call".to_string(), &args(&[("text", json!("plan agreed, go"))]))
                .await
                .unwrap(),
        );

        assert!(delivered.contains("agent-T-1-code"), "{delivered}");
        assert!(delivered.contains("agent-T-1-rev"), "{delivered}");
        for session in [&code, &review] {
            assert!(
                session
                    .lock()
                    .await
                    .messages
                    .iter()
                    .any(|message| message
                        .content
                        .content_text_only()
                        .contains("plan agreed, go")),
                "a broadcast must reach every other member"
            );
        }
        assert!(
            !architect
                .lock()
                .await
                .messages
                .iter()
                .any(|message| message
                    .content
                    .content_text_only()
                    .contains("plan agreed, go")),
            "the sender must not receive its own message"
        );
    }

    #[tokio::test]
    async fn room_message_can_target_single_member() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let app = refact_runtime_api::app_state_from_gcx(gcx.clone()).await;
        let workspace = room_workspace();
        *gcx.documents_state.workspace_folders.lock().unwrap() = vec![workspace.path().to_path_buf()];

        refact_runtime_api::task_storage::save_board(
            gcx.clone(),
            "task-room",
            &refact_tasks::types::TaskBoard {
                cards: vec![room_board_card(&[
                    "agent-T-1-arch",
                    "agent-T-1-code",
                    "agent-T-1-rev",
                ])],
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let (code, _) = room_session(&app, "agent-T-1-code").await;
        let (review, _) = room_session(&app, "agent-T-1-rev").await;
        let ccx = room_context(&app, "agent-T-1-arch", "task-room", "T-1").await;

        let mut tool = ToolAgentMessage {
            config_path: String::new(),
        };
        tool.tool_execute(
            ccx,
            &"call".to_string(),
            &args(&[
                ("to", json!("agent-T-1-code")),
                ("text", json!("only you")),
            ]),
        )
        .await
        .unwrap();

        assert!(
            code.lock()
                .await
                .messages
                .iter()
                .any(|message| message.content.content_text_only().contains("only you")),
            "the addressed member must get the message"
        );
        assert!(
            !review
                .lock()
                .await
                .messages
                .iter()
                .any(|message| message.content.content_text_only().contains("only you")),
            "an addressed message must not leak to the rest of the room"
        );
    }

    #[tokio::test]
    async fn room_member_with_agent_id_can_message_peer() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let app = refact_runtime_api::app_state_from_gcx(gcx.clone()).await;
        let workspace = room_workspace();
        *gcx.documents_state.workspace_folders.lock().unwrap() = vec![workspace.path().to_path_buf()];

        let board_card = room_board_card(&["agent-T-1-arch", "agent-T-1-code"]);
        refact_runtime_api::task_storage::save_board(
            gcx.clone(),
            "task-room",
            &refact_tasks::types::TaskBoard {
                cards: vec![board_card],
                ..Default::default()
            },
        )
        .await
        .unwrap();

        let (session, _) = room_session(&app, "agent-T-1-code").await;
        let ccx = room_context(&app, "agent-T-1-arch", "task-room", "T-1").await;
        ccx.lock().await.background_agent_id = Some("subagent-id-12345".to_string());

        let mut tool = ToolAgentMessage {
            config_path: String::new(),
        };
        let delivered = text(
            tool.tool_execute(
                ccx,
                &"call".to_string(),
                &args(&[
                    ("to", json!("agent-T-1-code")),
                    ("text", json!("parser is ready, please check")),
                ]),
            )
            .await
            .unwrap(),
        );

        assert!(delivered.contains("agent-T-1-code"), "{delivered}");
        assert!(
            session
                .lock()
                .await
                .messages
                .iter()
                .any(|message| message
                    .content
                    .content_text_only()
                    .contains("parser is ready, please check")),
            "peer message must land in the peer's chat even when caller has background_agent_id"
        );
    }

    #[tokio::test]
    async fn room_member_can_message_peer_by_role() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let app = refact_runtime_api::app_state_from_gcx(gcx.clone()).await;
        let workspace = room_workspace();
        *gcx.documents_state.workspace_folders.lock().unwrap() = vec![workspace.path().to_path_buf()];

        let board_card = room_board_card(&["agent-T-1-arch", "agent-T-1-code"]);
        refact_runtime_api::task_storage::save_board(
            gcx.clone(),
            "task-room",
            &refact_tasks::types::TaskBoard {
                cards: vec![board_card],
                ..Default::default()
            },
        )
        .await
        .unwrap();

        let (session, _) = room_session(&app, "agent-T-1-code").await;
        let ccx = room_context(&app, "agent-T-1-arch", "task-room", "T-1").await;
        ccx.lock().await.background_agent_id = Some("agent-id-0".to_string());

        let mut tool = ToolAgentMessage {
            config_path: String::new(),
        };
        let delivered = text(
            tool.tool_execute(
                ccx,
                &"call".to_string(),
                &args(&[
                    ("to", json!("coder")),
                    ("text", json!("message sent by role")),
                ]),
            )
            .await
            .unwrap(),
        );

        assert!(delivered.contains("agent-T-1-code"), "{delivered}");
        assert!(
            session
                .lock()
                .await
                .messages
                .iter()
                .any(|message| message
                    .content
                    .content_text_only()
                    .contains("message sent by role")),
            "peer message addressed by role must land in the peer's chat"
        );
    }

    #[tokio::test]
    async fn room_broadcast_works_when_caller_has_agent_id() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let app = refact_runtime_api::app_state_from_gcx(gcx.clone()).await;
        let workspace = room_workspace();
        *gcx.documents_state.workspace_folders.lock().unwrap() = vec![workspace.path().to_path_buf()];

        let board_card = room_board_card(&["agent-T-1-arch", "agent-T-1-code"]);
        refact_runtime_api::task_storage::save_board(
            gcx.clone(),
            "task-room",
            &refact_tasks::types::TaskBoard {
                cards: vec![board_card],
                ..Default::default()
            },
        )
        .await
        .unwrap();

        let (session, _) = room_session(&app, "agent-T-1-code").await;
        let ccx = room_context(&app, "agent-T-1-arch", "task-room", "T-1").await;
        ccx.lock().await.background_agent_id = Some("agent-id-0".to_string());

        let mut tool = ToolAgentMessage {
            config_path: String::new(),
        };
        let delivered = text(
            tool.tool_execute(
                ccx,
                &"call".to_string(),
                &args(&[
                    ("to", json!("all")),
                    ("text", json!("broadcast with agent id")),
                ]),
            )
            .await
            .unwrap(),
        );

        assert!(delivered.contains("agent-T-1-code"), "{delivered}");
        assert!(
            session
                .lock()
                .await
                .messages
                .iter()
                .any(|message| message
                    .content
                    .content_text_only()
                    .contains("broadcast with agent id")),
            "broadcast message must land in the peer's chat even when caller has agent id"
        );
    }

    #[tokio::test]
    async fn foreign_card_members_cannot_receive_message() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let app = refact_runtime_api::app_state_from_gcx(gcx.clone()).await;
        let workspace = room_workspace();
        *gcx.documents_state.workspace_folders.lock().unwrap() = vec![workspace.path().to_path_buf()];

        refact_runtime_api::task_storage::save_board(
            gcx.clone(),
            "task-room",
            &refact_tasks::types::TaskBoard {
                cards: vec![
                    room_board_card(&["agent-T-1-arch", "agent-T-1-code"]),
                    room_board_card_for("T-2", &["agent-T-2-arch", "agent-T-2-code"]),
                ],
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let (other_room, _) = room_session(&app, "agent-T-2-code").await;
        let ccx = room_context(&app, "agent-T-1-arch", "task-room", "T-1").await;

        let mut tool = ToolAgentMessage {
            config_path: String::new(),
        };
        let error = tool
            .tool_execute(
                ccx,
                &"call".to_string(),
                &args(&[
                    ("to", json!("agent-T-2-code")),
                    ("text", json!("crossing rooms")),
                ]),
            )
            .await
            .unwrap_err();

        assert!(error.contains("direct children or descendants"), "{error}");
        assert!(
            !other_room
                .lock()
                .await
                .messages
                .iter()
                .any(|message| message
                    .content
                    .content_text_only()
                    .contains("crossing rooms")),
            "a rejected delivery must not land anywhere"
        );
    }

    #[tokio::test]
    async fn neighbour_tool_output_never_enters_my_context() {
        // The invariant behind "room members talk, not through each other's tool
        // output": each member is its own `ChatSession`, so a message delivered to a
        // neighbour is simply not in this session. If this test ever fails because
        // the two sessions were merged, the whole context-isolation story is gone.
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let app = refact_runtime_api::app_state_from_gcx(gcx.clone()).await;
        let workspace = room_workspace();
        *gcx.documents_state.workspace_folders.lock().unwrap() = vec![workspace.path().to_path_buf()];

        refact_runtime_api::task_storage::save_board(
            gcx.clone(),
            "task-room",
            &refact_tasks::types::TaskBoard {
                cards: vec![room_board_card(&["agent-T-1-arch", "agent-T-1-code"])],
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let (code, _) = room_session(&app, "agent-T-1-code").await;
        let (architect, _) = room_session(&app, "agent-T-1-arch").await;
        let ccx = room_context(&app, "agent-T-1-arch", "task-room", "T-1").await;

        let mut tool = ToolAgentMessage {
            config_path: String::new(),
        };
        tool.tool_execute(
            ccx,
            &"call".to_string(),
            &args(&[("text", json!("for the code agent only"))]),
        )
        .await
        .unwrap();

        let neighbour_got_it = code
            .lock()
            .await
            .messages
            .iter()
            .any(|message| message
                .content
                .content_text_only()
                .contains("for the code agent only"));
        let author_saw_it = architect
            .lock()
            .await
            .messages
            .iter()
            .any(|message| message
                .content
                .content_text_only()
                .contains("for the code agent only"));

        assert!(neighbour_got_it, "the message is the only channel between them");
        assert!(
            !author_saw_it,
            "a delivered room message must not loop back into the sender's transcript"
        );
    }

    #[tokio::test]
    async fn completed_assistant_body_is_published_to_peer_without_private_traces() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let app = refact_runtime_api::app_state_from_gcx(gcx.clone()).await;
        let workspace = room_workspace();
        *gcx.documents_state.workspace_folders.lock().unwrap() =
            vec![workspace.path().to_path_buf()];
        refact_runtime_api::task_storage::save_board(
            gcx.clone(),
            "task-room",
            &refact_tasks::types::TaskBoard {
                cards: vec![room_board_card(&["agent-T-1-arch", "agent-T-1-code"])],
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let architect = room_session_only(&app, "agent-T-1-arch").await;
        let code = room_session_only(&app, "agent-T-1-code").await;
        {
            let mut session = architect.lock().await;
            session.thread.task_meta = Some(refact_chat_api::TaskMeta {
                task_id: "task-room".to_string(),
                role: "agents".to_string(),
                agent_id: Some("agent-id-0".to_string()),
                card_id: Some("T-1".to_string()),
                planner_chat_id: Some("planner-room".to_string()),
            });
            let mut message = ChatMessage::new(
                "assistant".to_string(),
                "The public review result is ready.".to_string(),
            );
            message.message_id = "assistant-public-1".to_string();
            message.reasoning_content = Some("PRIVATE_REASONING_SENTINEL".to_string());
            message.tool_calls = Some(vec![refact_core::chat_types::ChatToolCall {
                id: "private-call".to_string(),
                index: None,
                function: refact_core::chat_types::ChatToolFunction {
                    name: "private_tool".to_string(),
                    arguments: "PRIVATE_ARGUMENTS_SENTINEL".to_string(),
                },
                tool_type: "function".to_string(),
                extra_content: None,
                started_at_ms: None,
                completed_at_ms: None,
            }]);
            session.add_message(message);
        }

        refact_runtime_api::room_delivery::publish_latest_assistant_message(
            app.clone(),
            architect.clone(),
        )
        .await
        .unwrap()
        .expect("room body published");

        let peer_messages = code.lock().await.messages.clone();
        let delivered = peer_messages
            .iter()
            .find(|message| {
                message
                    .content
                    .content_text_only()
                    .contains("The public review result is ready.")
            })
            .expect("public body reaches the peer");
        let serialized = serde_json::to_string(delivered).unwrap();
        assert!(!serialized.contains("PRIVATE_REASONING_SENTINEL"));
        assert!(!serialized.contains("PRIVATE_ARGUMENTS_SENTINEL"));
        assert!(!serialized.contains("private_tool"));
        let provenance = refact_chat_api::provenance_from_message(delivered).unwrap();
        assert_eq!(provenance.chat_id, "agent-T-1-arch");
        assert_eq!(provenance.role, "architect");
        assert_eq!(provenance.card_id.as_deref(), Some("T-1"));
        assert!(refact_core::active_context::active_context(&peer_messages)
            .unwrap()
            .messages
            .iter()
            .any(|message| message.message_id == delivered.message_id));
        assert!(
            !architect.lock().await.messages.iter().any(|message| {
                message.role == "event"
                    && message.content.content_text_only().contains(
                        "The public review result is ready.",
                    )
            }),
            "the author must not receive its own room body"
        );
    }

    #[tokio::test]
    async fn repeated_publication_is_deduplicated_per_recipient() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let app = refact_runtime_api::app_state_from_gcx(gcx.clone()).await;
        let workspace = room_workspace();
        *gcx.documents_state.workspace_folders.lock().unwrap() =
            vec![workspace.path().to_path_buf()];
        refact_runtime_api::task_storage::save_board(
            gcx.clone(),
            "task-room",
            &refact_tasks::types::TaskBoard {
                cards: vec![room_board_card(&["agent-T-1-arch", "agent-T-1-code"])],
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let architect = room_session_only(&app, "agent-T-1-arch").await;
        let code = room_session_only(&app, "agent-T-1-code").await;
        {
            let mut session = architect.lock().await;
            session.thread.task_meta = Some(refact_chat_api::TaskMeta {
                task_id: "task-room".to_string(),
                role: "agents".to_string(),
                agent_id: Some("agent-id-0".to_string()),
                card_id: Some("T-1".to_string()),
                planner_chat_id: Some("planner-room".to_string()),
            });
            let mut message =
                ChatMessage::new("assistant".to_string(), "One public body only.".to_string());
            message.message_id = "assistant-public-dedup".to_string();
            session.add_message(message);
        }

        for _ in 0..2 {
            refact_runtime_api::room_delivery::publish_latest_assistant_message(
                app.clone(),
                architect.clone(),
            )
            .await
            .unwrap()
            .expect("room body published");
        }

        let matching = code
            .lock()
            .await
            .messages
            .iter()
            .filter(|message| {
                message
                    .content
                    .content_text_only()
                    .contains("One public body only.")
            })
            .count();
        assert_eq!(matching, 1, "the same body must reach a peer once");
    }

    #[tokio::test]
    async fn partial_recipient_failure_preserves_busy_peer_delivery() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let app = refact_runtime_api::app_state_from_gcx(gcx.clone()).await;
        let workspace = room_workspace();
        *gcx.documents_state.workspace_folders.lock().unwrap() =
            vec![workspace.path().to_path_buf()];
        refact_runtime_api::task_storage::save_board(
            gcx.clone(),
            "task-room",
            &refact_tasks::types::TaskBoard {
                cards: vec![room_board_card(&[
                    "agent-T-1-arch",
                    "agent-T-1-code",
                    "agent-T-1-missing",
                ])],
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let architect = room_session_only(&app, "agent-T-1-arch").await;
        let code = room_session_only(&app, "agent-T-1-code").await;
        {
            let mut session = architect.lock().await;
            session.thread.task_meta = Some(refact_chat_api::TaskMeta {
                task_id: "task-room".to_string(),
                role: "agents".to_string(),
                agent_id: Some("agent-id-0".to_string()),
                card_id: Some("T-1".to_string()),
                planner_chat_id: Some("planner-room".to_string()),
            });
            let mut message = ChatMessage::new(
                "assistant".to_string(),
                "Deliver despite the missing peer.".to_string(),
            );
            message.message_id = "assistant-partial-failure".to_string();
            session.add_message(message);
        }
        code.lock().await.runtime.state = refact_chat_api::SessionState::Generating;

        let report = refact_runtime_api::room_delivery::publish_latest_assistant_message(
            app.clone(),
            architect,
        )
        .await
        .unwrap()
        .expect("room body published");

        assert_eq!(report.delivered, vec!["agent-T-1-code".to_string()]);
        assert_eq!(report.failed.len(), 1);
        assert_eq!(report.failed[0].0, "agent-T-1-missing");
        let session = code.lock().await;
        assert_eq!(session.pending_deliveries.len(), 1);
        assert!(session.pending_deliveries[0]
            .preview_text()
            .contains("Deliver despite the missing peer."));
        assert!(session.messages.is_empty());
    }

    /// Provenance of the message carrying `needle` in the peer's chat.
    async fn delivered_provenance(
        session: &Arc<AMutex<ChatSession>>,
        needle: &str,
    ) -> refact_chat_api::MessageProvenance {
        let message = session
            .lock()
            .await
            .messages
            .iter()
            .find(|message| message.content.content_text_only().contains(needle))
            .cloned()
            .unwrap_or_else(|| panic!("room message {needle:?} must reach the peer"));
        refact_chat_api::provenance_from_message(&message)
            .unwrap_or_else(|| panic!("room message {needle:?} must carry provenance"))
    }

    fn room_board_card(chats: &[&str]) -> refact_tasks::types::BoardCard {
        room_board_card_for("T-1", chats)
    }

    fn room_board_card_for(
        card_id: &str,
        chats: &[&str],
    ) -> refact_tasks::types::BoardCard {
        let roles = ["architect", "coder"];
        let members = chats
            .iter()
            .enumerate()
            .map(|(index, chat)| {
                let mut member = refact_tasks::rooms::new_room_member(
                    roles[index.min(roles.len() - 1)],
                    &format!("agent-id-{index}"),
                    chat,
                    None,
                    None,
                    None,
                    vec![],
                );
                member.member_status = Some(refact_tasks::types::TeamStatus::Running);
                member
            })
            .collect();
        refact_tasks::types::BoardCard {
            id: card_id.to_string(),
            title: format!("Card {card_id}"),
            column: "doing".to_string(),
            priority: "P1".to_string(),
            depends_on: vec![],
            instructions: String::new(),
            assignee: None,
            agent_chat_id: None,
            retry_count: 0,
            status_updates: vec![],
            comments: vec![],
            final_report: None,
            final_report_structured: None,
            verifier_report: None,
            created_at: "2026-01-01T00:00:00Z".to_string(),
            started_at: None,
            last_heartbeat_at: None,
            completed_at: None,
            agent_branch: None,
            agent_worktree: None,
            agent_worktree_name: None,
            base_branch: None,
            base_commit: None,
            ab_variants: None,
            team_members: members,
            target_files: vec![],
            scope_guard_mode: Default::default(),
        }
    }

    /// A workspace that already holds the `task-room` task directory the board
    /// fixtures write into: `save_board` refuses to invent a task.
    fn room_workspace() -> tempfile::TempDir {
        let workspace = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(
            workspace
                .path()
                .join(".refact")
                .join("tasks")
                .join("task-room"),
        )
        .unwrap();
        workspace
    }

    /// A member session. The workspace belongs to the caller: installing a
    /// fresh tempdir here would discard the board the test has just saved.
    async fn room_session(app: &AppState, chat_id: &str) -> (Arc<AMutex<ChatSession>>, tempfile::TempDir) {
        (
            room_session_only(app, chat_id).await,
            tempfile::tempdir().unwrap(),
        )
    }

    /// A second (or third) member's session, *without* touching the workspace.
    ///
    /// `room_session` installs a fresh tempdir, and the previous one is deleted on drop — so a test
    /// that needs two peers must keep the workspace and add only the sessions, or the board written
    /// into the first tempdir disappears and the roster read starts failing for the wrong reason.
    async fn room_session_only(app: &AppState, chat_id: &str) -> Arc<AMutex<ChatSession>> {
        let session = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        session
            .lock()
            .await
            .queue_processor_running
            .store(true, Ordering::SeqCst);
        app.chat
            .sessions
            .write()
            .await
            .insert(chat_id.to_string(), session.clone());
        session
    }

    async fn room_context(
        app: &AppState,
        chat_id: &str,
        task_id: &str,
        card_id: &str,
    ) -> Arc<AMutex<AtCommandsContext>> {
        let task_meta = refact_chat_api::TaskMeta {
            task_id: task_id.to_string(),
            role: "agents".to_string(),
            agent_id: Some("agent-id-0".to_string()),
            card_id: Some(card_id.to_string()),
            planner_chat_id: Some("planner-room".to_string()),
        };
        let session = room_session_only(app, chat_id).await;
        session.lock().await.thread.task_meta = Some(task_meta.clone());
        let ccx = Arc::new(AMutex::new(
            crate::at_commands::at_commands::at_commands_new_from_app(
                app.clone(),
                4096,
                20,
                false,
                vec![],
                chat_id.to_string(),
                Some("planner-room".to_string()),
                "test/model".to_string(),
                None,
                None,
            )
            .await,
        ));
        ccx.lock().await.task_meta = Some(task_meta);
        ccx
    }

}
