pub use refact_tools::tools::tool_background_agents::*;

#[cfg(test)]
#[allow(unused_imports)]
use std::collections::HashMap;
#[cfg(test)]
#[allow(unused_imports)]
use std::sync::Arc;
#[cfg(test)]
#[allow(unused_imports)]
use std::time::Duration;
#[cfg(test)]
#[allow(unused_imports)]
use async_trait::async_trait;
#[cfg(test)]
#[allow(unused_imports)]
use chrono::{DateTime, Utc};
#[cfg(test)]
#[allow(unused_imports)]
use serde_json::{json, Value};
#[cfg(test)]
#[allow(unused_imports)]
use tokio::sync::Mutex as AMutex;
#[cfg(test)]
#[allow(unused_imports)]
use crate::agents::types::{AgentListFilter, BackgroundAgent, BgAgentKind, BgAgentStatus};
#[cfg(test)]
#[allow(unused_imports)]
use refact_runtime_api::AtCommandsContext;
#[cfg(test)]
#[allow(unused_imports)]
use refact_core::chat_types::{ChatContent, ChatMessage, ContextEnum};
#[cfg(test)]
#[allow(unused_imports)]
use refact_postprocessing::pp_command_output::OutputFilter;
#[cfg(test)]
#[allow(unused_imports)]
use crate::tools::tools_description::{Tool, ToolDesc, ToolSource, ToolSourceType};

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;

    use crate::agents::types::{AgentCompletion, CreateAgentRequest};
    use crate::app_state::AppState;
    use refact_chat_api::ChatSession;
    use crate::tools::tools_description::Tool;
    use serde_json::json;
    use tempfile::TempDir;

    const PARENT: &str = "parent-chat";
    const OTHER_PARENT: &str = "other-parent-chat";

    fn args(items: &[(&str, Value)]) -> HashMap<String, Value> {
        items
            .iter()
            .map(|(key, value)| ((*key).to_string(), value.clone()))
            .collect()
    }

    fn create_request(parent_chat_id: &str, kind: BgAgentKind, title: &str) -> CreateAgentRequest {
        CreateAgentRequest {
            parent_chat_id: parent_chat_id.to_string(),
            parent_root_chat_id: Some(parent_chat_id.to_string()),
            parent_tool_call_id: Some("tool-call".to_string()),
            kind,
            config_name: kind_label(kind).to_string(),
            title: title.to_string(),
            prompt: format!("Prompt for {title}"),
            target_files: vec!["src/frog.rs".to_string()],
            model: "test-model".to_string(),
            model_type: None,
            goal_summary: None,
            plan_present: false,
            worktree_id: None,
            worktree_branch: None,
        }
    }

    fn completion(summary: &str) -> AgentCompletion {
        AgentCompletion {
            result_summary: summary.to_string(),
            edited_files: vec!["src/frog.rs".to_string()],
            diff_summary: Some("one frog changed".to_string()),
            conflict_summary: Some("no conflicts".to_string()),
            child_chat_id: Some("subchat-frog".to_string()),
        }
    }

    async fn test_context(chat_id: &str) -> (TempDir, AppState, Arc<AMutex<AtCommandsContext>>) {
        let temp = tempfile::tempdir().unwrap();
        let config = tempfile::tempdir().unwrap();
        let gcx = crate::global_context::tests::make_test_gcx_with_dirs(
            temp.path().to_path_buf(),
            config.path().to_path_buf(),
        )
        .await;
        let app = refact_runtime_api::app_state_from_gcx(gcx).await;
        let ccx = Arc::new(AMutex::new(
            crate::at_commands::at_commands::at_commands_new_from_app(
                app.clone(),
                4096,
                20,
                false,
                vec![],
                chat_id.to_string(),
                None,
                "model".to_string(),
                None,
                None,
            )
            .await,
        ));
        (temp, app, ccx)
    }

    async fn create_agent(
        app: &AppState,
        parent_chat_id: &str,
        kind: BgAgentKind,
        title: &str,
    ) -> BackgroundAgent {
        app.agents
            .create(create_request(parent_chat_id, kind, title))
            .await
            .unwrap()
            .0
    }

    fn output_text(result: (bool, Vec<ContextEnum>)) -> String {
        match result.1.into_iter().next().unwrap() {
            ContextEnum::ChatMessage(message) => match message.content {
                ChatContent::SimpleText(text) => text,
                _ => panic!("expected text output"),
            },
            _ => panic!("expected chat message"),
        }
    }

    #[tokio::test]
    async fn agent_list_happy_path_renders_table() {
        let (_temp, app, ccx) = test_context(PARENT).await;
        let running = create_agent(&app, PARENT, BgAgentKind::Delegate, "Fix retry").await;
        app.agents
            .mark_running(&running.agent_id, "subchat-running".to_string())
            .await
            .unwrap();
        let updated = app
            .agents
            .update_progress(
                &running.agent_id,
                "searching call sites in src/".to_string(),
                5,
            )
            .await
            .unwrap();
        let completed =
            create_agent(&app, PARENT, BgAgentKind::Subagent, "List callers of X").await;
        app.agents
            .mark_completed(&completed.agent_id, completion("all callers listed"))
            .await
            .unwrap();

        let output = output_text(
            ToolAgentList {
                config_path: String::new(),
            }
            .tool_execute(ccx, &"call".to_string(), &HashMap::new())
            .await
            .unwrap(),
        );

        assert!(output.contains("# Background Agents"));
        assert!(
            output.contains("| ID | Kind | Status | Title | Tool | Tokens | Age | Last activity |")
        );
        assert!(output.contains(&running.agent_id));
        assert!(output.contains("delegate"));
        assert!(output.contains("🟢 running"));
        assert!(output.contains("Total: 2"));
        assert!(output.contains(updated.last_activity.as_deref().unwrap()));
    }

    #[tokio::test]
    async fn agent_status_happy_path_renders_status_block() {
        let (_temp, app, ccx) = test_context(PARENT).await;
        let record = create_agent(&app, PARENT, BgAgentKind::Subagent, "List callers of X").await;
        app.agents
            .mark_running(&record.agent_id, "subchat-status".to_string())
            .await
            .unwrap();
        let updated = app
            .agents
            .update_progress(
                &record.agent_id,
                "searching call sites in src/".to_string(),
                5,
            )
            .await
            .unwrap();

        let output = output_text(
            ToolAgentStatus {
                config_path: String::new(),
            }
            .tool_execute(
                ccx,
                &"call".to_string(),
                &args(&[("agent_id", json!(updated.agent_id))]),
            )
            .await
            .unwrap(),
        );

        assert!(output.contains("# Subagent: List callers of X"));
        assert!(output.contains("- Status: 🟢 running"));
        assert!(output.contains("- Step count: 5"));
        assert!(output.contains(updated.last_activity.as_deref().unwrap()));
        assert!(output.contains("- Progress: searching call sites in src/"));
        assert!(output.contains("Child trajectory: [view](refact://chat/subchat-status)"));
    }

    #[tokio::test]
    async fn agent_result_happy_path_returns_terminal_summary() {
        let (_temp, app, ccx) = test_context(PARENT).await;
        let record = create_agent(&app, PARENT, BgAgentKind::Delegate, "Fix retry").await;
        let completed = app
            .agents
            .mark_completed(&record.agent_id, completion("fixed retry logic"))
            .await
            .unwrap();

        let output = output_text(
            ToolAgentResult {
                config_path: String::new(),
            }
            .tool_execute(
                ccx,
                &"call".to_string(),
                &args(&[("agent_id", json!(completed.agent_id))]),
            )
            .await
            .unwrap(),
        );

        assert!(output.contains("# Agent Result: Fix retry"));
        assert!(output.contains("fixed retry logic"));
        assert!(output.contains("- Edited files: src/frog.rs"));
    }

    #[tokio::test]
    async fn agent_cancel_happy_path_requests_cancel() {
        let (_temp, app, ccx) = test_context(PARENT).await;
        let record = create_agent(&app, PARENT, BgAgentKind::Delegate, "Stop me").await;
        app.agents
            .mark_running(&record.agent_id, "subchat-cancel".to_string())
            .await
            .unwrap();

        let output = output_text(
            ToolAgentCancel {
                config_path: String::new(),
            }
            .tool_execute(
                ccx,
                &"call".to_string(),
                &args(&[("agent_id", json!(record.agent_id))]),
            )
            .await
            .unwrap(),
        );

        assert!(output.contains("✓ Cancel requested for: bgagent-"));
    }

    #[tokio::test]
    async fn parent_scoping_is_enforced_for_all_agent_tools() {
        let (_temp, app, ccx) = test_context(PARENT).await;
        let record = create_agent(&app, OTHER_PARENT, BgAgentKind::Delegate, "Hidden").await;

        let list_output = output_text(
            ToolAgentList {
                config_path: String::new(),
            }
            .tool_execute(ccx.clone(), &"call".to_string(), &HashMap::new())
            .await
            .unwrap(),
        );
        assert!(!list_output.contains(&record.agent_id));
        assert!(list_output.contains("Total: 0"));

        let status_err = ToolAgentStatus {
            config_path: String::new(),
        }
        .tool_execute(
            ccx.clone(),
            &"call".to_string(),
            &args(&[("agent_id", json!(record.agent_id.clone()))]),
        )
        .await
        .unwrap_err();
        assert_eq!(status_err, not_found_error(&record.agent_id));

        let wait_err = ToolAgentWait {
            config_path: String::new(),
        }
        .tool_execute(
            ccx.clone(),
            &"call".to_string(),
            &args(&[
                ("agent_id", json!(record.agent_id.clone())),
                ("timeout_ms", json!(1000)),
            ]),
        )
        .await
        .unwrap_err();
        assert_eq!(wait_err, not_found_error(&record.agent_id));

        let result_err = ToolAgentResult {
            config_path: String::new(),
        }
        .tool_execute(
            ccx.clone(),
            &"call".to_string(),
            &args(&[("agent_id", json!(record.agent_id.clone()))]),
        )
        .await
        .unwrap_err();
        assert_eq!(result_err, not_found_error(&record.agent_id));

        let cancel_err = ToolAgentCancel {
            config_path: String::new(),
        }
        .tool_execute(
            ccx,
            &"call".to_string(),
            &args(&[("agent_id", json!(record.agent_id.clone()))]),
        )
        .await
        .unwrap_err();
        assert_eq!(cancel_err, not_found_error(&record.agent_id));
    }

    #[tokio::test]
    async fn agent_list_filters_status_kind_limit_and_terminal_window() {
        let (_temp, app, ccx) = test_context(PARENT).await;
        let running_delegate = create_agent(&app, PARENT, BgAgentKind::Delegate, "Running").await;
        app.agents
            .mark_running(&running_delegate.agent_id, "subchat-running".to_string())
            .await
            .unwrap();
        let completed_delegate =
            create_agent(&app, PARENT, BgAgentKind::Delegate, "Completed").await;
        app.agents
            .mark_completed(&completed_delegate.agent_id, completion("done"))
            .await
            .unwrap();
        let subagent = create_agent(&app, PARENT, BgAgentKind::Subagent, "Queued subagent").await;

        let running_output = output_text(
            ToolAgentList {
                config_path: String::new(),
            }
            .tool_execute(
                ccx.clone(),
                &"call".to_string(),
                &args(&[("status", json!("running"))]),
            )
            .await
            .unwrap(),
        );
        assert!(running_output.contains(&running_delegate.agent_id));
        assert!(!running_output.contains(&completed_delegate.agent_id));

        let subagent_output = output_text(
            ToolAgentList {
                config_path: String::new(),
            }
            .tool_execute(
                ccx.clone(),
                &"call".to_string(),
                &args(&[("kind", json!("subagent"))]),
            )
            .await
            .unwrap(),
        );
        assert!(subagent_output.contains(&subagent.agent_id));
        assert!(!subagent_output.contains(&running_delegate.agent_id));

        let limited_output = output_text(
            ToolAgentList {
                config_path: String::new(),
            }
            .tool_execute(
                ccx.clone(),
                &"call".to_string(),
                &args(&[("limit", json!(1))]),
            )
            .await
            .unwrap(),
        );
        assert!(limited_output.contains("Total: 1"));

        let terminal_window_output = output_text(
            ToolAgentList {
                config_path: String::new(),
            }
            .tool_execute(
                ccx,
                &"call".to_string(),
                &args(&[("include_terminal_within_hours", json!(0))]),
            )
            .await
            .unwrap(),
        );
        assert!(!terminal_window_output.contains(&completed_delegate.agent_id));
        assert!(terminal_window_output.contains(&running_delegate.agent_id));
    }

    #[tokio::test]
    async fn agent_wait_timeout_returns_status_with_prefix() {
        let (_temp, app, ccx) = test_context(PARENT).await;
        let record = create_agent(&app, PARENT, BgAgentKind::Delegate, "Slow agent").await;
        app.agents
            .mark_running(&record.agent_id, "subchat-slow".to_string())
            .await
            .unwrap();

        let output = output_text(
            ToolAgentWait {
                config_path: String::new(),
            }
            .tool_execute(
                ccx,
                &"call".to_string(),
                &args(&[
                    ("agent_id", json!(record.agent_id)),
                    ("timeout_ms", json!(1000)),
                ]),
            )
            .await
            .unwrap(),
        );

        assert!(output.starts_with("⏰ Timed out after 1000ms"));
        assert!(output.contains("# Delegate: Slow agent"));
        assert!(output.contains("- Status: 🟢 running"));
    }

    #[tokio::test]
    async fn agent_wait_completion_returns_result() {
        let (_temp, app, ccx) = test_context(PARENT).await;
        let record = create_agent(&app, PARENT, BgAgentKind::Subagent, "Finish soon").await;
        app.agents
            .mark_running(&record.agent_id, "subchat-finish".to_string())
            .await
            .unwrap();
        let agent_id = record.agent_id.clone();
        let registry = app.agents.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(25)).await;
            registry
                .mark_completed(&agent_id, completion("finished from wait"))
                .await
                .unwrap();
        });

        let output = output_text(
            ToolAgentWait {
                config_path: String::new(),
            }
            .tool_execute(
                ccx,
                &"call".to_string(),
                &args(&[
                    ("agent_id", json!(record.agent_id)),
                    ("timeout_ms", json!(1000)),
                ]),
            )
            .await
            .unwrap(),
        );

        assert!(output.contains("# Agent Result: Finish soon"));
        assert!(output.contains("finished from wait"));
    }

    #[tokio::test]
    async fn agent_wait_unblocks_when_agent_completes_concurrently() {
        let (_temp, app, ccx) = test_context(PARENT).await;
        let record = create_agent(&app, PARENT, BgAgentKind::Subagent, "Concurrent finish").await;
        app.agents
            .mark_running(&record.agent_id, "subchat-concurrent".to_string())
            .await
            .unwrap();
        let agent_id = record.agent_id.clone();
        let registry = app.agents.clone();
        tokio::spawn(async move {
            registry
                .mark_completed(&agent_id, completion("done concurrently"))
                .await
                .unwrap();
        });

        let output = output_text(
            ToolAgentWait {
                config_path: String::new(),
            }
            .tool_execute(
                ccx,
                &"call".to_string(),
                &args(&[
                    ("agent_id", json!(record.agent_id)),
                    ("timeout_ms", json!(5000)),
                ]),
            )
            .await
            .unwrap(),
        );

        assert!(output.contains("# Agent Result: Concurrent finish"));
        assert!(output.contains("done concurrently"));
    }
    #[tokio::test]
    async fn agent_result_reports_not_finished_for_running_agent() {
        let (_temp, app, ccx) = test_context(PARENT).await;
        let record = create_agent(&app, PARENT, BgAgentKind::Subagent, "Still running").await;
        app.agents
            .mark_running(&record.agent_id, "subchat-running".to_string())
            .await
            .unwrap();

        let output = output_text(
            ToolAgentResult {
                config_path: String::new(),
            }
            .tool_execute(
                ccx,
                &"call".to_string(),
                &args(&[("agent_id", json!(record.agent_id))]),
            )
            .await
            .unwrap(),
        );

        assert!(output.contains("Agent has not finished yet"));
        assert!(output.contains("🟢 running"));
    }

    #[tokio::test]
    async fn agent_result_returns_full_default_output() {
        let (_temp, app, ccx) = test_context(PARENT).await;
        let record = create_agent(&app, PARENT, BgAgentKind::Delegate, "Long result").await;
        let summary = "frog".repeat(5000);
        let completed = app
            .agents
            .mark_completed(&record.agent_id, completion(&summary))
            .await
            .unwrap();

        let output = output_text(
            ToolAgentResult {
                config_path: String::new(),
            }
            .tool_execute(
                ccx.clone(),
                &"call".to_string(),
                &args(&[("agent_id", json!(completed.agent_id.clone()))]),
            )
            .await
            .unwrap(),
        );

        assert!(output.contains(&summary));
        assert!(!output.contains('…'));
        assert!(output.contains("- Diff summary: one frog changed"));

        let (_, contexts) = ToolAgentResult {
            config_path: String::new(),
        }
        .tool_execute(
            ccx,
            &"call-preserve".to_string(),
            &args(&[("agent_id", json!(completed.agent_id))]),
        )
        .await
        .unwrap();
        let ContextEnum::ChatMessage(message) = contexts.into_iter().next().unwrap() else {
            panic!("expected chat message")
        };
        assert_eq!(message.preserve, Some(true));
        assert_eq!(
            message
                .output_filter
                .as_ref()
                .map(|filter| filter.limit_chars),
            Some(usize::MAX)
        );
    }

    #[tokio::test]
    async fn agent_cancel_on_terminal_returns_no_action() {
        let (_temp, app, ccx) = test_context(PARENT).await;
        let record = create_agent(&app, PARENT, BgAgentKind::Delegate, "Already done").await;
        let completed = app
            .agents
            .mark_completed(&record.agent_id, completion("done"))
            .await
            .unwrap();

        let output = output_text(
            ToolAgentCancel {
                config_path: String::new(),
            }
            .tool_execute(
                ccx,
                &"call".to_string(),
                &args(&[("agent_id", json!(completed.agent_id.clone()))]),
            )
            .await
            .unwrap(),
        );
        let after = app.agents.get(PARENT, &completed.agent_id).await.unwrap();

        assert!(output.contains("Agent already in terminal state: ✅ completed. No action."));
        assert_eq!(after.status, BgAgentStatus::Completed);
    }

    #[tokio::test]
    async fn agent_cancel_on_running_flips_abort_flag() {
        let (_temp, app, ccx) = test_context(PARENT).await;
        let session_arc = Arc::new(AMutex::new(ChatSession::new(PARENT.to_string())));
        let mut rx = session_arc.lock().await.subscribe();
        app.chat
            .sessions
            .write()
            .await
            .insert(PARENT.to_string(), session_arc);
        let (record, abort_flag, _) = app
            .agents
            .create(create_request(PARENT, BgAgentKind::Delegate, "Abort me"))
            .await
            .unwrap();
        app.agents
            .mark_running(&record.agent_id, "subchat-abort".to_string())
            .await
            .unwrap();

        let output = output_text(
            ToolAgentCancel {
                config_path: String::new(),
            }
            .tool_execute(
                ccx,
                &"call".to_string(),
                &args(&[("agent_id", json!(record.agent_id.clone()))]),
            )
            .await
            .unwrap(),
        );
        let after = app.agents.get(PARENT, &record.agent_id).await.unwrap();

        assert!(abort_flag.load(Ordering::SeqCst));
        assert_eq!(after.status, BgAgentStatus::Cancelled);
        assert!(output.contains("Cancel requested"));
        let json = rx.try_recv().expect("background agent update");
        let envelope: Value = serde_json::from_str(&json).unwrap();
        assert_eq!(envelope["chat_id"], PARENT);
        assert_eq!(envelope["seq"], 1);
        assert_eq!(envelope["type"], "background_agent_updated");
        assert_eq!(envelope["agent"]["agentId"], record.agent_id);
        assert_eq!(
            envelope["agent"]["status"],
            BgAgentStatus::Cancelled.as_str()
        );
    }

    #[tokio::test]
    async fn agent_cancel_subtree_cancels_descendants() {
        let (_temp, app, ccx) = test_context(PARENT).await;
        let (parent, parent_abort, _) = app
            .agents
            .create(create_request(PARENT, BgAgentKind::Subagent, "Parent"))
            .await
            .unwrap();
        app.agents
            .mark_running(&parent.agent_id, "child-chat".to_string())
            .await
            .unwrap();
        let (child, child_abort, _) = app
            .agents
            .create(create_request("child-chat", BgAgentKind::Subagent, "Child"))
            .await
            .unwrap();
        app.agents
            .mark_running(&child.agent_id, "grandchild-chat".to_string())
            .await
            .unwrap();

        let output = output_text(
            ToolAgentCancel {
                config_path: String::new(),
            }
            .tool_execute(
                ccx,
                &"call".to_string(),
                &args(&[("agent_id", json!(parent.agent_id.clone()))]),
            )
            .await
            .unwrap(),
        );

        assert!(parent_abort.load(Ordering::SeqCst));
        assert!(child_abort.load(Ordering::SeqCst));
        assert_eq!(
            app.agents.get_any(&parent.agent_id).await.unwrap().status,
            BgAgentStatus::Cancelled
        );
        assert_eq!(
            app.agents.get_any(&child.agent_id).await.unwrap().status,
            BgAgentStatus::Cancelled
        );
        assert!(output.contains(&parent.agent_id));
        assert!(output.contains(&child.agent_id));
    }

    #[test]
    fn human_age_formats_short_durations() {
        let now = Utc::now();
        assert_eq!(human_age(now - chrono::Duration::minutes(2), now), "2m");
        assert_eq!(human_age(now - chrono::Duration::hours(3), now), "3h");
        assert_eq!(human_age(now - chrono::Duration::days(1), now), "1d");
    }
}
