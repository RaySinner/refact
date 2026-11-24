use refact_chat_api::{
    has_displayable_assistant_content, BuddyThreadMeta, ChatSession, ClaudeCodeIdentity,
    FrozenRequestPrefix, GoalLedgerEntry, GoalSnapshot, TaskMeta, ThreadParams, WorktreeMeta,
};
use refact_core::chat_types::{ChatMessage, PendingDelivery};
use refact_perf::perf_diagnostics::{self, PerfComponent, PerfOutcome};
use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize)]
pub struct TrajectorySnapshot {
    pub chat_id: String,
    pub title: String,
    pub model: String,
    pub mode: String,
    pub tool_use: String,
    pub messages: Vec<ChatMessage>,
    pub created_at: String,
    pub boost_reasoning: bool,
    pub checkpoints_enabled: bool,
    pub context_tokens_cap: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_compression_cap: Option<usize>,
    #[serde(default)]
    pub auto_compression_cap_pending: bool,
    pub include_project_info: bool,
    pub is_title_generated: bool,
    pub auto_approve_editing_tools: bool,
    pub auto_approve_dangerous_commands: bool,
    pub autonomous_no_confirm: bool,
    pub version: u64,
    pub task_meta: Option<TaskMeta>,
    pub worktree: Option<WorktreeMeta>,
    pub parent_id: Option<String>,
    pub link_type: Option<String>,
    pub root_chat_id: Option<String>,
    pub reasoning_effort: Option<String>,
    pub thinking_budget: Option<usize>,
    pub temperature: Option<f32>,
    pub frequency_penalty: Option<f32>,
    pub max_tokens: Option<usize>,
    pub parallel_tool_calls: Option<bool>,
    pub previous_response_id: Option<String>,
    pub active_skill: Option<String>,
    pub auto_enrichment_enabled: Option<bool>,
    pub buddy_meta: Option<BuddyThreadMeta>,
    pub auto_compact_enabled: Option<bool>,
    pub frozen_request_prefix: Option<FrozenRequestPrefix>,
    pub claude_code_identity: Option<ClaudeCodeIdentity>,
    pub reactive_compact_attempts: Option<usize>,
    pub wake_up_at: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub waiting_for_card_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending_deliveries: Vec<PendingDelivery>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal: Option<GoalSnapshot>,
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "tolerant_goal_ledger"
    )]
    pub goal_ledger: Vec<GoalLedgerEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal_verification_blocked_until_ms: Option<u64>,
}

fn tolerant_goal_ledger<'de, D>(deserializer: D) -> Result<Vec<GoalLedgerEntry>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = Vec::<serde_json::Value>::deserialize(deserializer).unwrap_or_default();
    Ok(raw
        .into_iter()
        .filter_map(|entry| serde_json::from_value(entry).ok())
        .collect())
}

impl TrajectorySnapshot {
    pub fn from_thread_parts(
        chat_id: String,
        thread: &ThreadParams,
        messages: Vec<ChatMessage>,
        created_at: String,
        version: u64,
    ) -> Self {
        Self {
            chat_id,
            title: thread.title.clone(),
            model: thread.model.clone(),
            mode: thread.mode.clone(),
            tool_use: thread.tool_use.clone(),
            messages,
            created_at,
            boost_reasoning: thread.boost_reasoning.unwrap_or(false),
            checkpoints_enabled: thread.checkpoints_enabled,
            context_tokens_cap: thread.context_tokens_cap,
            auto_compression_cap: thread.auto_compression_cap,
            auto_compression_cap_pending: thread.auto_compression_cap_pending,
            include_project_info: thread.include_project_info,
            is_title_generated: thread.is_title_generated,
            auto_approve_editing_tools: thread.auto_approve_editing_tools,
            auto_approve_dangerous_commands: thread.auto_approve_dangerous_commands,
            autonomous_no_confirm: thread.autonomous_no_confirm,
            version,
            task_meta: thread.task_meta.clone(),
            worktree: thread.worktree.clone(),
            parent_id: thread.parent_id.clone(),
            link_type: thread.link_type.clone(),
            root_chat_id: thread.root_chat_id.clone(),
            reasoning_effort: thread.reasoning_effort.clone(),
            thinking_budget: thread.thinking_budget,
            temperature: thread.temperature,
            frequency_penalty: thread.frequency_penalty,
            max_tokens: thread.max_tokens,
            parallel_tool_calls: thread.parallel_tool_calls,
            previous_response_id: thread.previous_response_id.clone(),
            active_skill: thread.active_skill.clone(),
            auto_enrichment_enabled: thread.auto_enrichment_enabled,
            buddy_meta: thread.buddy_meta.clone(),
            auto_compact_enabled: thread.auto_compact_enabled,
            frozen_request_prefix: thread.frozen_request_prefix.clone(),
            claude_code_identity: thread.claude_code_identity.clone(),
            reactive_compact_attempts: thread.reactive_compact_attempts,
            wake_up_at: None,
            waiting_for_card_ids: Vec::new(),
            pending_deliveries: Vec::new(),
            goal: None,
            goal_ledger: Vec::new(),
            goal_verification_blocked_until_ms: None,
        }
    }
}

pub fn trajectory_snapshot_from_session(session: &ChatSession) -> TrajectorySnapshot {
    let span = perf_diagnostics::span(
        PerfComponent::TrajectorySnapshot,
        Some(&session.chat_id),
        None,
    );
    let messages = session
        .messages
        .iter()
        .filter(|message| message.role != "assistant" || has_displayable_assistant_content(message))
        .cloned()
        .collect();

    let mut snapshot = TrajectorySnapshot::from_thread_parts(
        session.chat_id.clone(),
        &session.thread,
        messages,
        session.created_at.clone(),
        session.trajectory_version,
    );
    snapshot.wake_up_at = session.wake_up_at;
    snapshot.waiting_for_card_ids = session.waiting_for_card_ids.clone();
    snapshot.goal = session.goal.clone();
    snapshot.goal_ledger = session.goal_ledger.clone();
    snapshot.goal_verification_blocked_until_ms = session.goal_verification_blocked_until_ms;
    snapshot.pending_deliveries = session.pending_deliveries_for_snapshot();
    span.finish(
        PerfOutcome::Success,
        None,
        Some(snapshot.messages.len() as u64),
        Some(session.trajectory_version),
        Some(session.command_queue.len() as u64),
    );
    snapshot
}

#[cfg(test)]
mod tests {
    use super::*;
    use refact_chat_api::diagnostics::{is_ui_only_message, make_ui_only_error_message};
    use refact_chat_api::{GoalBudget, GoalLedgerOp, GoalProgress, GoalStatus};
    use refact_core::chat_types::ChatContent;

    fn goal_snapshot() -> GoalSnapshot {
        let mut goal = GoalSnapshot {
            content: "ship the card".to_string(),
            version: 1,
            active: true,
            status: GoalStatus::Active,
            budget: GoalBudget::default(),
            progress: GoalProgress {
                turns_used: 2,
                tokens_used: 120,
                started_at_ms: 123,
                no_progress_turns: 0,
                last_nudge_at_ms: 456,
                cost_used_cents: 0,
            },
            attempts: Vec::new(),
            events: Vec::new(),
            criteria: Vec::new(),
            snoozed_until_ms: None,
            stop_reason: None,
            transferred_from: None,
            transferred_to: None,
            ..Default::default()
        };
        goal.refresh_derived();
        goal
    }

    fn snapshot() -> TrajectorySnapshot {
        TrajectorySnapshot::from_thread_parts(
            "chat-id".to_string(),
            &ThreadParams::default(),
            Vec::new(),
            "2026-06-18T00:00:00Z".to_string(),
            1,
        )
    }

    #[test]
    fn pending_deliveries_roundtrip_and_legacy_absence() {
        use refact_core::chat_types::PushMode;
        let mut snapshot = snapshot();
        let mut delivery = PendingDelivery::with_id(
            "stable-id",
            vec![ChatMessage::new("user".into(), "queued".into())],
            PushMode::WhenIdle,
            "test",
            true,
        );
        delivery.enqueued_at_ms = 42;
        snapshot.pending_deliveries.push(delivery.clone());
        let mut value = serde_json::to_value(&snapshot).unwrap();
        let decoded: TrajectorySnapshot = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(decoded.pending_deliveries, vec![delivery]);
        value.as_object_mut().unwrap().remove("pending_deliveries");
        value
            .as_object_mut()
            .unwrap()
            .remove("auto_compression_cap_pending");
        let legacy: TrajectorySnapshot = serde_json::from_value(value).unwrap();
        assert!(legacy.pending_deliveries.is_empty());
        assert!(serde_json::to_value(legacy)
            .unwrap()
            .get("pending_deliveries")
            .is_none());
    }

    #[test]
    fn from_thread_parts_defaults_goal_none() {
        let snapshot = snapshot();

        assert_eq!(snapshot.goal, None);
    }

    #[test]
    fn trajectory_snapshot_goal_roundtrip() {
        let mut snapshot = snapshot();
        let goal = goal_snapshot();
        snapshot.goal = Some(goal.clone());

        let encoded = serde_json::to_string(&snapshot).unwrap();
        let decoded: TrajectorySnapshot = serde_json::from_str(&encoded).unwrap();

        assert_eq!(decoded.goal, Some(goal));
    }

    #[test]
    fn message_creation_instants_roundtrip_and_legacy_absence_is_preserved() {
        let mut first = ChatMessage::new("user".to_string(), "first".to_string());
        first
            .extra
            .insert("created_at_ms".to_string(), serde_json::json!(111_111));
        let mut second = ChatMessage::new("assistant".to_string(), "second".to_string());
        second
            .extra
            .insert("created_at_ms".to_string(), serde_json::json!(222_222));
        let legacy = ChatMessage::new("event".to_string(), "legacy".to_string());
        let mut snapshot = snapshot();
        snapshot.messages = vec![first, second, legacy];

        let value = serde_json::to_value(&snapshot).unwrap();
        assert_eq!(value["messages"][0]["created_at_ms"], 111_111);
        assert_eq!(value["messages"][1]["created_at_ms"], 222_222);
        assert!(value["messages"][2].get("created_at_ms").is_none());

        let decoded: TrajectorySnapshot = serde_json::from_value(value).unwrap();
        assert_eq!(decoded.messages[0].extra["created_at_ms"], 111_111);
        assert_eq!(decoded.messages[1].extra["created_at_ms"], 222_222);
        assert!(!decoded.messages[2].extra.contains_key("created_at_ms"));
    }

    #[test]
    fn trajectory_snapshot_auto_compression_cap_roundtrip_and_legacy_absence() {
        let mut thread = ThreadParams::default();
        thread.auto_compression_cap = Some(4096);
        let snapshot = TrajectorySnapshot::from_thread_parts(
            "cap-chat".to_string(),
            &thread,
            Vec::new(),
            "2026-06-18T00:00:00Z".to_string(),
            1,
        );
        let mut value = serde_json::to_value(&snapshot).unwrap();
        let decoded: TrajectorySnapshot = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(decoded.auto_compression_cap, Some(4096));
        assert!(decoded.auto_compression_cap_pending);
        value
            .as_object_mut()
            .unwrap()
            .remove("auto_compression_cap_pending");

        value
            .as_object_mut()
            .unwrap()
            .remove("auto_compression_cap");
        let legacy: TrajectorySnapshot = serde_json::from_value(value).unwrap();
        assert_eq!(legacy.auto_compression_cap, None);
        assert!(!legacy.auto_compression_cap_pending);
    }

    #[test]
    fn obsolete_compression_cooldown_is_ignored_on_load() {
        let mut value = serde_json::to_value(snapshot()).unwrap();
        value["compression_retry_after_ms"] = serde_json::json!({"old-hash": 123_456});
        let decoded: TrajectorySnapshot = serde_json::from_value(value).unwrap();
        assert!(serde_json::to_value(decoded)
            .unwrap()
            .get("compression_retry_after_ms")
            .is_none());
    }

    #[test]
    fn tolerant_goal_ledger_skips_unknown_ops_and_keeps_known() {
        let mut snapshot = snapshot();
        snapshot.goal_ledger = vec![GoalLedgerEntry {
            seq: 1,
            at_ms: 1_000,
            op: GoalLedgerOp::NudgeRecorded,
        }];
        let mut value = serde_json::to_value(&snapshot).unwrap();
        value["goal_ledger"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({"seq": 2, "at_ms": 2_000, "op": "from_the_future", "x": 1}));

        let decoded: TrajectorySnapshot = serde_json::from_value(value).unwrap();

        assert_eq!(decoded.goal_ledger.len(), 1);
        assert_eq!(decoded.goal_ledger[0].seq, 1);
    }

    #[test]
    fn goal_verification_blocked_until_ms_roundtrips() {
        let mut snapshot = snapshot();
        snapshot.goal_verification_blocked_until_ms = Some(4_242);

        let encoded = serde_json::to_string(&snapshot).unwrap();
        let decoded: TrajectorySnapshot = serde_json::from_str(&encoded).unwrap();

        assert_eq!(decoded.goal_verification_blocked_until_ms, Some(4_242));
    }

    #[test]
    fn trajectory_snapshot_missing_goal_defaults_none() {
        let snapshot = snapshot();
        let encoded = serde_json::to_string(&snapshot).unwrap();
        let value: serde_json::Value = serde_json::from_str(&encoded).unwrap();

        assert!(value.get("goal").is_none());
        let decoded: TrajectorySnapshot = serde_json::from_value(value).unwrap();
        assert_eq!(decoded.goal, None);
    }

    #[test]
    fn trajectory_snapshot_missing_waiting_for_card_ids_defaults_empty() {
        // Trajectories written before waiting_for_card_ids existed (and current
        // files, which omit the field when empty) must still deserialize.
        let snapshot = snapshot();
        let encoded = serde_json::to_string(&snapshot).unwrap();
        let mut value: serde_json::Value = serde_json::from_str(&encoded).unwrap();
        value
            .as_object_mut()
            .unwrap()
            .remove("waiting_for_card_ids");

        let decoded: TrajectorySnapshot = serde_json::from_value(value).unwrap();
        assert!(decoded.waiting_for_card_ids.is_empty());
    }

    #[test]
    fn trajectory_snapshot_empty_waiting_for_card_ids_not_serialized() {
        let snapshot = snapshot();
        let encoded = serde_json::to_string(&snapshot).unwrap();
        let value: serde_json::Value = serde_json::from_str(&encoded).unwrap();

        assert!(value.get("waiting_for_card_ids").is_none());
    }

    #[test]
    fn trajectory_snapshot_waiting_for_card_ids_roundtrip() {
        let mut snapshot = snapshot();
        snapshot.waiting_for_card_ids = vec!["T-1".to_string(), "T-2".to_string()];

        let encoded = serde_json::to_string(&snapshot).unwrap();
        let decoded: TrajectorySnapshot = serde_json::from_str(&encoded).unwrap();

        assert_eq!(
            decoded.waiting_for_card_ids,
            vec!["T-1".to_string(), "T-2".to_string()]
        );
    }

    #[test]
    fn trajectory_snapshot_from_session_captures_wake_up_at() {
        let wake_up_at = chrono::Utc::now() + chrono::Duration::minutes(5);
        let mut session = ChatSession::new("wake-snapshot".to_string());
        session.wake_up_at = Some(wake_up_at);

        let snapshot = trajectory_snapshot_from_session(&session);

        assert_eq!(snapshot.wake_up_at, Some(wake_up_at));
    }

    #[test]
    fn trajectory_snapshot_from_session_preserves_goal_projection() {
        let mut session = ChatSession::new("goal-snapshot".to_string());
        session.install_goal("agent", "ship the goal", true, GoalBudget::default());
        session.goal.as_mut().unwrap().progress = GoalProgress {
            turns_used: 2,
            tokens_used: 345,
            started_at_ms: 123,
            no_progress_turns: 1,
            last_nudge_at_ms: 456,
            cost_used_cents: 0,
        };
        session.refresh_goal_runtime_mirror();

        let snapshot = trajectory_snapshot_from_session(&session);
        let goal = snapshot.goal.expect("goal snapshot");

        assert_eq!(goal.content, "ship the goal");
        assert_eq!(goal.status, GoalStatus::Active);
        assert_eq!(goal.progress.turns_used, 2);
        assert_eq!(goal.progress.tokens_used, 345);
        assert_eq!(goal.progress.no_progress_turns, 1);
    }

    fn trajectory_worktree_sample() -> WorktreeMeta {
        WorktreeMeta {
            id: "wt-1".to_string(),
            kind: "task_agent".to_string(),
            root: std::path::PathBuf::from("/tmp/refact-wt"),
            source_workspace_root: std::path::PathBuf::from("/tmp/refact-src"),
            repo_root: std::path::PathBuf::from("/tmp/refact-src"),
            branch: Some("refact/task/card".to_string()),
            base_branch: Some("main".to_string()),
            base_commit: Some("abc123".to_string()),
            task_id: Some("task-1".to_string()),
            card_id: Some("card-1".to_string()),
            agent_id: Some("agent-1".to_string()),
            enforce: true,
        }
    }

    #[test]
    fn trajectory_worktree_snapshot_from_session_captures_thread_worktree() {
        let worktree = trajectory_worktree_sample();
        let mut session = ChatSession::new("wt-snapshot".to_string());
        session.thread.worktree = Some(worktree.clone());
        let snapshot = trajectory_snapshot_from_session(&session);
        assert_eq!(snapshot.worktree, Some(worktree));
    }

    #[test]
    fn trajectory_snapshot_from_session_filters_empty_assistant_messages() {
        let mut session = ChatSession::new("empty-assistant-snapshot".to_string());
        session.messages.push(ChatMessage {
            role: "user".to_string(),
            content: ChatContent::SimpleText("hello".to_string()),
            ..Default::default()
        });
        session.messages.push(ChatMessage {
            role: "assistant".to_string(),
            content: ChatContent::SimpleText("   \n".to_string()),
            ..Default::default()
        });
        session.messages.push(ChatMessage {
            role: "error".to_string(),
            content: ChatContent::SimpleText("LLM error".to_string()),
            ..Default::default()
        });

        let snapshot = trajectory_snapshot_from_session(&session);

        assert_eq!(snapshot.messages.len(), 2);
        assert_eq!(snapshot.messages[0].role, "user");
        assert_eq!(snapshot.messages[1].role, "error");
    }

    #[test]
    fn trajectory_snapshot_from_session_preserves_ui_only_messages() {
        let mut session = ChatSession::new("ui-only-snapshot".to_string());
        session
            .messages
            .push(ChatMessage::new("user".to_string(), "visible".to_string()));
        session
            .messages
            .push(make_ui_only_error_message("context_length_exceeded"));

        let snapshot = trajectory_snapshot_from_session(&session);

        assert_eq!(snapshot.messages.len(), 2);
        assert_eq!(snapshot.messages[0].role, "user");
        assert_eq!(snapshot.messages[1].role, "error");
        assert!(is_ui_only_message(&snapshot.messages[1]));
    }

    #[test]
    fn trajectory_snapshot_from_session_filters_metadata_only_assistant_messages() {
        use refact_core::chat_types::ChatUsage;

        let mut session = ChatSession::new("metadata-only-assistant-snapshot".to_string());
        session.messages.push(ChatMessage {
            role: "user".to_string(),
            content: ChatContent::SimpleText("hello".to_string()),
            ..Default::default()
        });
        session.messages.push(ChatMessage {
            role: "assistant".to_string(),
            usage: Some(ChatUsage {
                prompt_tokens: 10,
                completion_tokens: 0,
                total_tokens: 10,
                cache_creation_tokens: None,
                cache_read_tokens: None,
                metering_usd: None,
            }),
            extra: serde_json::Map::from_iter([(
                "openai_response_id".to_string(),
                serde_json::json!("resp_123"),
            )]),
            ..Default::default()
        });
        session.messages.push(ChatMessage {
            role: "assistant".to_string(),
            content: ChatContent::SimpleText("visible".to_string()),
            ..Default::default()
        });

        let snapshot = trajectory_snapshot_from_session(&session);

        assert_eq!(snapshot.messages.len(), 2);
        assert_eq!(snapshot.messages[0].role, "user");
        assert_eq!(snapshot.messages[1].role, "assistant");
        assert_eq!(snapshot.messages[1].content.content_text_only(), "visible");
    }

    #[test]
    fn trajectory_snapshot_from_session_preserves_ui_only_diagnostics() {
        let mut session = ChatSession::new("ui-only-snapshot".to_string());
        let diagnostic = make_ui_only_error_message("context_length_exceeded");
        session
            .messages
            .push(ChatMessage::new("user".to_string(), "Hello".to_string()));
        session.messages.push(diagnostic.clone());

        let snapshot = trajectory_snapshot_from_session(&session);

        assert_eq!(snapshot.messages.len(), 2);
        assert!(snapshot.messages.iter().any(|message| {
            message.message_id == diagnostic.message_id
                && message.role == "error"
                && is_ui_only_message(message)
        }));
    }
}
