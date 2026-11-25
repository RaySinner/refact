use std::collections::HashMap;

use refact_buddy_core::settings::{BuddySettings, SupervisionMode};
use refact_buddy_core::supervision_journal::{supervision_journal_path, SupervisionJournal};
use refact_chat_api::{
    arbitrate_pursuit, authorize_goal_transition, ChatCommand, ChatMessage, ChatSession, GoalActor,
    GoalAutonomy, GoalBudget, GoalProgress, GoalResolution, GoalSnapshot, GoalStatus,
    GoalTransition, PursuitArbitrationContext, PursuitDecision, PursuitRequest, PursuitSource,
    PursuitSuppressReason,
};
use refact_llm::adapter::AdapterSettings;
use refact_llm::{get_adapter, LlmRequest, WireFormat};
use refact_room::{RoomEventDraft, RoomJournalSnapshot, ROOM_EVENT_SCHEMA_VERSION};
use serde_json::{json, Value};

pub use refact_runtime_api::external_participant::*;

mod room_snapshot_export {
    pub use refact_runtime_api::room_snapshot_export::*;
}

mod chat_queue_helpers {
    pub use refact_runtime_api::chat_queue_helpers::{
        command_triggers_generation, note_user_turn_resets_goal_pursuit,
    };
}

mod supervision {
    pub use refact_buddy_core::supervision::*;
}

#[path = "../crates/refact-buddy-core/src/supervision_policy.rs"]
mod buddy_advice_scenarios;
#[path = "../crates/refact-buddy-core/src/supervision_detectors.rs"]
mod buddy_detector_scenarios;
#[path = "../crates/refact-buddy-core/src/supervision_journal.rs"]
mod buddy_journal_scenarios;
#[path = "../crates/refact-runtime-api/src/external_participant/tests.rs"]
mod external_uhp_lifecycle_scenarios;
#[path = "../crates/refact-runtime-api/src/external_participant/room_tests.rs"]
mod external_uhp_room_scenarios;
#[path = "../crates/refact-runtime-api/src/pursuit.rs"]
mod pursuit_scenarios;
#[path = "room_public_delivery_e2e.rs"]
mod room_provider_scenario;

fn openai_wire(messages: Vec<ChatMessage>) -> Value {
    let request = LlmRequest::new("gpt-4".to_string(), messages);
    let settings = AdapterSettings {
        api_key: "test-key".to_string(),
        auth_token: String::new(),
        endpoint: "https://api.openai.com/v1/chat/completions".to_string(),
        extra_headers: HashMap::new(),
        model_name: "gpt-4".to_string(),
        supports_tools: true,
        supports_reasoning: false,
        reasoning_type: None,
        supports_temperature: true,
        supports_max_completion_tokens: false,
        eof_is_done: false,
        supports_web_search: false,
        supports_cache_control: true,
    };
    get_adapter(WireFormat::OpenaiChatCompletions)
        .build_http(&refact_privacy::testing::cleared(request), &settings)
        .unwrap()
        .body
}

fn pursuit(session: &ChatSession, source: PursuitSource) -> PursuitDecision {
    arbitrate_pursuit(
        session,
        &PursuitRequest::new(source, "e2e", 1, true),
        PursuitArbitrationContext::new(1),
    )
}

#[test]
fn goal_pursuit_and_provider_wire_respect_autonomy_user_turns_and_terminal_states() {
    let ordinary = ChatSession::new("ordinary".to_string());
    assert_eq!(
        pursuit(&ordinary, PursuitSource::GoalMonitor),
        PursuitDecision::Suppress(PursuitSuppressReason::NoGoal)
    );

    let mut session = ChatSession::new("goal-wire".to_string());
    session.install_goal(
        "agent",
        "GOAL_REQUIREMENT_PROVIDER_SENTINEL",
        true,
        GoalBudget::default(),
    );
    session.goal.as_mut().unwrap().autonomy = GoalAutonomy::Off;
    session.set_goal_projection(session.goal.clone());
    assert_eq!(
        pursuit(&session, PursuitSource::InlineTurnEnd),
        PursuitDecision::Suppress(PursuitSuppressReason::AutonomyOff)
    );
    assert_eq!(
        pursuit(&session, PursuitSource::Buddy),
        PursuitDecision::VisibleOnly
    );

    let explicit = ChatCommand::UserMessage {
        content: json!("EXPLICIT_USER_TURN_PROVIDER_SENTINEL"),
        attachments: vec![],
        context_files: vec![],
        suppress_auto_enrichment: false,
        client_message_id: None,
        push: None,
    };
    assert!(chat_queue_helpers::command_triggers_generation(&explicit));
    session.add_message(ChatMessage::new(
        "user".to_string(),
        "EXPLICIT_USER_TURN_PROVIDER_SENTINEL".to_string(),
    ));
    let active_wire = openai_wire(session.messages.clone()).to_string();
    assert!(active_wire.contains("GOAL_REQUIREMENT_PROVIDER_SENTINEL"));
    assert!(active_wire.contains("EXPLICIT_USER_TURN_PROVIDER_SENTINEL"));

    session.goal_set_status(GoalStatus::Completed);
    assert_eq!(
        pursuit(&session, PursuitSource::InlineTurnEnd),
        PursuitDecision::Suppress(PursuitSuppressReason::NonActiveGoalStatus)
    );
    let completed_wire = openai_wire(session.messages.clone()).to_string();
    assert!(!completed_wire.contains("GOAL_REQUIREMENT_PROVIDER_SENTINEL"));
    assert!(completed_wire.contains("EXPLICIT_USER_TURN_PROVIDER_SENTINEL"));

    session.goal_set_status(GoalStatus::Stopped);
    session.goal_set_status(GoalStatus::Active);
    assert_eq!(
        session.goal.as_ref().unwrap().resolution,
        GoalResolution::Achieved
    );
    assert!(!session.goal.as_ref().unwrap().prompt_enabled);
    assert_eq!(
        pursuit(&session, PursuitSource::InlineTurnEnd),
        PursuitDecision::Suppress(PursuitSuppressReason::NonActiveResolution)
    );
    assert!(!openai_wire(session.messages.clone())
        .to_string()
        .contains("GOAL_REQUIREMENT_PROVIDER_SENTINEL"));

    let clear = authorize_goal_transition(
        session.goal.as_ref(),
        GoalActor::User,
        GoalTransition::Clear,
    )
    .unwrap();
    assert!(session.goal_record_transition(clear));
    session.set_goal_projection(session.goal.clone());
    assert!(!session.goal.as_ref().unwrap().active);
    assert!(authorize_goal_transition(
        session.goal.as_ref(),
        GoalActor::User,
        GoalTransition::Resume,
    )
    .is_err());
    assert_eq!(
        pursuit(&session, PursuitSource::InlineTurnEnd),
        PursuitDecision::Suppress(PursuitSuppressReason::InactiveGoal)
    );
    assert!(!openai_wire(session.messages.clone())
        .to_string()
        .contains("GOAL_REQUIREMENT_PROVIDER_SENTINEL"));
}

#[tokio::test]
async fn legacy_room_goal_and_buddy_state_load_without_destructive_migration() {
    let legacy_event: RoomEventDraft = serde_json::from_value(json!({
        "room_id": "legacy-room",
        "event_id": "legacy-event",
        "author": {"participant_id": "writer", "role": "coder", "display_name": "Writer"},
        "body": "legacy public body",
        "source_message_id": "legacy-message",
        "turn_id": "legacy-turn",
        "created_at": "2026-01-01T00:00:00Z"
    }))
    .unwrap();
    assert_eq!(legacy_event.schema_version, ROOM_EVENT_SCHEMA_VERSION);
    assert!(legacy_event.source_revision.is_none());
    assert!(legacy_event.delivery.is_none());
    legacy_event.validate().unwrap();
    let legacy_journal: RoomJournalSnapshot =
        serde_json::from_value(json!({"revision": 0, "events": []})).unwrap();
    legacy_journal.validate().unwrap();
    assert_eq!(legacy_journal.schema_version, ROOM_EVENT_SCHEMA_VERSION);

    let legacy_goal: GoalSnapshot = serde_json::from_value(json!({
        "content": "legacy completed goal",
        "version": 1,
        "active": true,
        "status": "completed",
        "budget": GoalBudget::default(),
        "progress": GoalProgress::default(),
        "attempts": [],
        "events": [],
        "transferred_from": null,
        "transferred_to": null
    }))
    .unwrap();
    assert_eq!(legacy_goal.resolution, GoalResolution::Achieved);
    assert_eq!(legacy_goal.autonomy, GoalAutonomy::On);
    assert!(!legacy_goal.can_pursue);
    assert!(!legacy_goal.prompt_enabled);
    let restored_goal: GoalSnapshot =
        serde_json::from_value(serde_json::to_value(&legacy_goal).unwrap()).unwrap();
    assert_eq!(restored_goal, legacy_goal);

    let mut legacy_buddy = serde_json::to_value(BuddySettings::default()).unwrap();
    let object = legacy_buddy.as_object_mut().unwrap();
    object.remove("supervision_mode");
    object.remove("supervision_min_confidence");
    let buddy: BuddySettings = serde_json::from_value(legacy_buddy).unwrap();
    assert_eq!(buddy.supervision_mode, SupervisionMode::Advise);
    assert_eq!(buddy.supervision_min_confidence, 80);

    let directory = tempfile::tempdir().unwrap();
    let buddy_dir = directory.path().join(".refact/buddy");
    tokio::fs::create_dir_all(&buddy_dir).await.unwrap();
    let state_path = buddy_dir.join("state.json");
    tokio::fs::write(&state_path, b"{\"legacy\":true}")
        .await
        .unwrap();
    let snapshot = SupervisionJournal::new(directory.path())
        .load()
        .await
        .unwrap();
    assert!(snapshot.records().is_empty());
    assert!(snapshot.current_candidates().is_empty());
    assert!(!supervision_journal_path(directory.path()).exists());
    assert_eq!(
        tokio::fs::read(&state_path).await.unwrap(),
        b"{\"legacy\":true}"
    );
}
