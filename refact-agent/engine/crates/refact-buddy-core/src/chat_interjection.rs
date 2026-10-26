use chrono::{DateTime, Utc};
use refact_core::string_utils::safe_truncate;
use refact_tasks::types::TaskBoard;

use crate::settings::BuddySettings;
use crate::speech_policy::{gate_speech, intent_key};
use crate::state::SpeechRotationState;
use crate::types::{BuddyFact, BuddyFactKind};
use crate::voice_service::SpeechIntent;

/// Stamped on the delivered message so the GUI and the trajectory can tell a
/// Buddy interjection apart from every other notice in an agent's transcript.
pub const INTERJECTION_SOURCE: &str = "buddy.chat_interjection";

/// An agent that has been silent for this long is worth a word, even if the
/// monitor has not formally classified a stall yet. Buddy's threshold is its
/// own judgement and acts *in addition to* the monitor, not instead of it; the
/// per-intent speech budget is what keeps the two from doubling up.
pub const INTERJECTION_MIN_IDLE: std::time::Duration = std::time::Duration::from_secs(120);

/// Ceiling on the delivered text. The agent pays a turn to read it, so it is
/// kept short; the truncation is character-safe. It has to stay above the
/// longest message `build_interjection_text` can produce (a room member with a
/// full-length card title), otherwise the last instruction is cut off and the
/// nudge loses its point.
pub const MAX_TEXT_CHARS: usize = 512;

/// Why an interjection was not delivered. Distinct reasons are what make a
/// dropped nudge debuggable instead of merely silent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InterjectionSkipReason {
    /// Buddy itself is switched off, in quiet mode, or no longer proactive.
    InterpretationDisabled,
    /// The intent is in `muted_intents`.
    IntentMuted,
    /// The target chat is in `muted_chat_ids`.
    ChatMuted,
    /// Local hour falls inside the quiet window.
    QuietHours,
    /// The per-hour / per-day budget for this intent is spent.
    IntentBudget,
    /// The daily LLM token budget is exhausted.
    LlmBudgetExhausted,
    /// The agent is mid-turn: it is generating, running tools, or waiting on an
    /// IDE. Interrupting a working agent is worse than staying quiet.
    AgentBusy,
    /// The chat is not in `app.chat.sessions` at all, so we cannot tell whether
    /// it is idle. Guessing "probably idle" is how a nudge lands inside an open
    /// tool-result window.
    AgentChatUnknown,
    /// Delivery itself failed (closed chat, unrestorable trajectory, ...).
    DeliveryFailed,
    /// The delivery was a duplicate: this interjection already landed.
    AlreadyDelivered,
}

impl InterjectionSkipReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InterpretationDisabled => "interpretation_disabled",
            Self::IntentMuted => "intent_muted",
            Self::ChatMuted => "chat_muted",
            Self::QuietHours => "quiet_hours",
            Self::IntentBudget => "intent_budget",
            Self::LlmBudgetExhausted => "llm_budget_exhausted",
            Self::AgentBusy => "agent_busy",
            Self::AgentChatUnknown => "agent_chat_unknown",
            Self::DeliveryFailed => "delivery_failed",
            Self::AlreadyDelivered => "already_delivered",
        }
    }
}

/// One candidate agent Buddy may speak to, resolved from board cards.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InterjectionCandidate {
    pub task_id: String,
    pub card_id: String,
    pub chat_id: String,
    pub card_title: String,
    pub is_room_member: bool,
}

/// One agent Buddy may speak to, resolved from a card rather than from a fact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InterjectionTarget {
    pub task_id: String,
    pub card_id: String,
    pub chat_id: String,
    /// The card title, used to make the nudge about *this* task. Never a made-up
    /// agent name: a card agent is a plain chat session, and inventing a
    /// "name" for it would be fiction the agent then has to reason about.
    pub card_title: String,
    /// How long the agent's session has been silent.
    pub idle_for: std::time::Duration,
    /// True when the card is a room and this target is a specific active member.
    pub is_room_member: bool,
}

/// The pure part of the interjection decision: everything that can be answered
/// from settings and the speech rotation alone. Split out from the async part so
/// the gates are testable without an engine, a board, or a session.
pub fn gate_interjection(
    settings: &BuddySettings,
    rotation: &SpeechRotationState,
    chat_id: &str,
    local_hour: u32,
    auto_quiet_window: Option<(u8, u8)>,
    now: DateTime<Utc>,
    llm_budget_exhausted: bool,
) -> Result<(), InterjectionSkipReason> {
    if !settings.enabled
        || settings.quiet_mode
        || !settings.proactive_enabled
        // Buddy is allowed to speak inside chats at all only when chat
        // reactions are on; that toggle is the user's "Buddy may chime in"
        // switch, and an interjection is a louder version of the same thing.
        || !settings.chat_reactions_enabled
    {
        return Err(InterjectionSkipReason::InterpretationDisabled);
    }
    if settings.muted_chat_ids.iter().any(|id| id == chat_id) {
        return Err(InterjectionSkipReason::ChatMuted);
    }
    if settings
        .muted_intents
        .iter()
        .any(|muted| muted == intent_key(SpeechIntent::AgentInterjection))
    {
        return Err(InterjectionSkipReason::IntentMuted);
    }
    if llm_budget_exhausted {
        return Err(InterjectionSkipReason::LlmBudgetExhausted);
    }
    // The shared gate. `intent_budget` is honoured here, unlike in
    // `BuddyService::gate_runtime_event_speech`.
    let decision = gate_speech(
        settings,
        rotation,
        Some(SpeechIntent::AgentInterjection),
        Some(chat_id),
        local_hour,
        auto_quiet_window,
        now,
    );
    if !decision.allowed {
        return Err(match decision.reason {
            "chat_muted" => InterjectionSkipReason::ChatMuted,
            "intent_muted" => InterjectionSkipReason::IntentMuted,
            "quiet_hours" => InterjectionSkipReason::QuietHours,
            _ => InterjectionSkipReason::IntentBudget,
        });
    }
    Ok(())
}

/// The interjection text.
///
/// This must read as Buddy, not as a second copy of the monitor. The monitor
/// says *what happened* ("your last turn ended without calling any tool, do
/// exactly one of these"). Buddy says *what to do about the task*: it has the
/// card, the mandate, and the room. `interjection_text_differs_from_monitor`
/// pins the difference so the two cannot drift into saying the same thing.
///
/// Deliberately kept under [`MAX_TEXT_CHARS`]: the agent pays a whole turn to
/// read this, and a truncated message that loses its last instruction is worse
/// than no message at all. `interjection_text_fits_the_budget` guards that.
pub fn build_interjection_text(target: &InterjectionTarget) -> String {
    let card_id = if target.card_id.is_empty() {
        "your card"
    } else {
        target.card_id.as_str()
    };
    let subject = if target.card_title.trim().is_empty() {
        format!("card `{card_id}`")
    } else {
        format!(
            "`{}` (card `{card_id}`)",
            safe_truncate(target.card_title.trim(), 120)
        )
    };

    let mut text = format!(
        "Hey, it's Buddy. {subject} has been quiet for about {}. Pick one and do it:\n\
1. Done - call `agent_finish` with what you changed.\n\
2. Not done - make a real tool call for the next step, right now.\n\
3. Blocked - call `agent_ask_planner` with `urgency=\"block\"` and say what you need.\n\
A bare text answer leaves the card hanging.",
        humantime::format_duration(target.idle_for)
    );

    if target.is_room_member {
        text.push_str("\n(One of several agents here - don't duplicate a neighbour.)");
    }

    safe_truncate(&text, MAX_TEXT_CHARS).to_string()
}

/// A stable dedupe id, so a retried pass does not deliver the same nudge twice.
///
/// Derived from the target's identity (task, card, chat, title) and never from
/// its idleness: idleness grows continuously, so an idleness-derived id would be
/// new on literally every pass and the dedupe would never fire at all. The real
/// rate limiter is the 1/hour speech budget; this id only collapses the
/// duplicate deliveries one observer pass can produce.
pub fn interjection_delivery_id(target: &InterjectionTarget) -> String {
    format!(
        "buddy-interjection:{}:{}:{}:{}",
        target.task_id,
        target.card_id,
        target.chat_id,
        safe_truncate(&target.card_title, 40).replace(' ', "_")
    )
}

/// Pure resolution of candidates from a task board.
pub fn collect_board_candidates(
    task_id: &str,
    board: Option<&TaskBoard>,
) -> Vec<InterjectionCandidate> {
    let Some(board) = board else {
        return Vec::new();
    };
    let mut candidates = Vec::new();
    for card in board.cards.iter().filter(|card| card.column == "doing") {
        let card_is_room = card.is_room();
        let mut card_candidates: Vec<(String, bool)> = Vec::new();
        if let Some(chat_id) = card
            .agent_chat_id
            .as_deref()
            .map(str::trim)
            .filter(|id| !id.is_empty())
        {
            card_candidates.push((chat_id.to_string(), false));
        }
        if card_is_room {
            for member in card.team().iter().filter(|member| member.is_active()) {
                if let Some(chat_id) = member
                    .agent_chat_id
                    .as_deref()
                    .map(str::trim)
                    .filter(|id| !id.is_empty())
                {
                    if !card_candidates
                        .iter()
                        .any(|(existing, _)| existing == chat_id)
                    {
                        card_candidates.push((chat_id.to_string(), true));
                    }
                }
            }
        }

        for (chat_id, is_room_member) in card_candidates {
            candidates.push(InterjectionCandidate {
                task_id: task_id.to_string(),
                card_id: card.id.clone(),
                chat_id,
                card_title: card.title.clone(),
                is_room_member,
            });
        }
    }
    candidates
}

/// Which task ids currently have a `TaskStuck` fact. This is the trigger; the
/// board does the targeting.
pub fn stuck_task_ids(facts: &[BuddyFact]) -> Vec<String> {
    let mut ids: Vec<String> = facts
        .iter()
        .filter(|fact| fact.kind == BuddyFactKind::TaskStuck)
        .filter_map(|fact| {
            fact.payload
                .get("task_id")
                .and_then(|value| value.as_str())
                .map(|id| id.to_string())
        })
        .collect();
    ids.sort();
    ids.dedup();
    ids
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::BuddySettings;
    use crate::speech_policy::budget_for;
    use crate::state::{IntentBudgetState, SpeechRotationState};

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-05-15T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    fn target(chat_id: &str) -> InterjectionTarget {
        InterjectionTarget {
            task_id: "task-1".to_string(),
            card_id: "T-6".to_string(),
            chat_id: chat_id.to_string(),
            card_title: "Buddy may speak into an agent chat".to_string(),
            idle_for: std::time::Duration::from_secs(600),
            is_room_member: false,
        }
    }

    fn allow_all() -> BuddySettings {
        let mut settings = BuddySettings::default();
        settings.quiet_hours_mode = crate::settings::QuietHoursMode::Off;
        settings
    }

    fn gate(
        settings: &BuddySettings,
        rotation: &SpeechRotationState,
        chat_id: &str,
        local_hour: u32,
    ) -> Result<(), InterjectionSkipReason> {
        gate_interjection(settings, rotation, chat_id, local_hour, None, now(), false)
    }

    #[test]
    fn interjection_blocked_when_budget_exhausted() {
        let settings = allow_all();
        let budget = budget_for(SpeechIntent::AgentInterjection);
        let mut rotation = SpeechRotationState::default();
        rotation.by_intent.insert(
            intent_key(SpeechIntent::AgentInterjection).to_string(),
            IntentBudgetState {
                last_emitted_at: Some(now()),
                hour_count: budget.per_hour,
                day_count: budget.per_day,
                hour_window_start: Some(now()),
                day_window_start: Some(now()),
            },
        );

        // The strict variant: unlike `gate_runtime_event_speech`, an exhausted
        // budget stops the interjection instead of being waved through.
        assert_eq!(
            gate(&settings, &rotation, "agent-1", 12),
            Err(InterjectionSkipReason::IntentBudget)
        );
    }

    #[test]
    fn interjection_blocked_during_quiet_hours() {
        let mut settings = allow_all();
        settings.quiet_hours_mode = crate::settings::QuietHoursMode::Fixed;
        settings.quiet_hours_start = 22;
        settings.quiet_hours_end = 8;

        assert_eq!(
            gate(&settings, &SpeechRotationState::default(), "agent-1", 23),
            Err(InterjectionSkipReason::QuietHours)
        );
        assert!(gate(&settings, &SpeechRotationState::default(), "agent-1", 12).is_ok());
    }

    #[test]
    fn interjection_blocked_for_muted_chat() {
        let mut settings = allow_all();
        settings.muted_chat_ids.push("agent-muted".to_string());

        assert_eq!(
            gate(
                &settings,
                &SpeechRotationState::default(),
                "agent-muted",
                12
            ),
            Err(InterjectionSkipReason::ChatMuted)
        );
    }

    #[test]
    fn interjection_blocked_for_muted_intent() {
        let mut settings = allow_all();
        settings
            .muted_intents
            .push(intent_key(SpeechIntent::AgentInterjection).to_string());

        assert_eq!(
            gate(&settings, &SpeechRotationState::default(), "agent-1", 12),
            Err(InterjectionSkipReason::IntentMuted)
        );
    }

    #[test]
    fn interjection_blocked_when_interpretation_disabled() {
        let mut settings = allow_all();
        settings.proactive_enabled = false;
        assert_eq!(
            gate(&settings, &SpeechRotationState::default(), "agent-1", 12),
            Err(InterjectionSkipReason::InterpretationDisabled)
        );

        let mut settings = allow_all();
        settings.enabled = false;
        assert_eq!(
            gate(&settings, &SpeechRotationState::default(), "agent-1", 12),
            Err(InterjectionSkipReason::InterpretationDisabled)
        );

        let mut settings = allow_all();
        settings.quiet_mode = true;
        assert_eq!(
            gate(&settings, &SpeechRotationState::default(), "agent-1", 12),
            Err(InterjectionSkipReason::InterpretationDisabled)
        );

        // "Buddy may chime in" is off, so Buddy does not speak into chats at all.
        let mut settings = allow_all();
        settings.chat_reactions_enabled = false;
        assert_eq!(
            gate(&settings, &SpeechRotationState::default(), "agent-1", 12),
            Err(InterjectionSkipReason::InterpretationDisabled)
        );
    }

    #[test]
    fn interjection_blocked_when_llm_budget_exhausted() {
        let settings = allow_all();
        assert_eq!(
            gate_interjection(
                &settings,
                &SpeechRotationState::default(),
                "agent-1",
                12,
                None,
                now(),
                true,
            ),
            Err(InterjectionSkipReason::LlmBudgetExhausted)
        );
    }

    #[test]
    fn interjection_allowed_for_idle_agent_chat() {
        assert!(gate(&allow_all(), &SpeechRotationState::default(), "agent-1", 12).is_ok());
    }

    #[test]
    fn interjection_text_differs_from_monitor_stall_text() {
        let text = build_interjection_text(&target("agent-1"));

        // Buddy speaks as itself, about the task, not as a stall classifier.
        assert!(text.contains("it's Buddy"), "{text}");
        assert!(text.contains("T-6"), "{text}");

        // The monitor's phrasing is a stall report; a Buddy interjection is not.
        // These are the specific sentences that made the two overlap before.
        let monitor_only = [
            "ended without calling any tool",
            "Nothing was recorded for card",
            "Do exactly one of these now",
            "produces no tokens for",
            "has reported no progress for",
        ];
        for phrase in monitor_only {
            assert!(
                !text.contains(phrase),
                "Buddy's interjection must not repeat the monitor's phrasing: {phrase}\n{text}"
            );
        }
    }

    #[test]
    fn interjection_names_the_agent_only_by_card_facts() {
        let mut room_target = target("agent-1");
        room_target.is_room_member = true;
        let text = build_interjection_text(&room_target);
        assert!(text.contains("several agents here"), "{text}");
        // No invented agent name: only the chat id and the card are facts.
        assert!(!text.contains("agent-1"), "{text}");

        let untitled = InterjectionTarget {
            card_title: "   ".to_string(),
            ..target("agent-1")
        };
        assert!(build_interjection_text(&untitled).contains("card `T-6`"));
    }

    #[test]
    fn interjection_text_fits_the_budget() {
        // Truncation is a safety net, not the normal path: a message that lost
        // its last instruction would be worse than silence.
        let mut room_target = target("agent-1");
        room_target.is_room_member = true;
        let longest = build_interjection_text(&room_target);
        assert!(
            longest.chars().count() <= MAX_TEXT_CHARS,
            "interjection text is {} chars, over the {MAX_TEXT_CHARS} budget: {longest}",
            longest.chars().count()
        );
        assert!(longest.contains("A bare text answer leaves the card hanging."));

        // A very long card title must not be what blows the budget.
        let verbose = InterjectionTarget {
            card_title: "x".repeat(400),
            ..target("agent-1")
        };
        let text = build_interjection_text(&verbose);
        assert!(text.chars().count() <= MAX_TEXT_CHARS);
    }

    #[test]
    fn interjection_delivery_id_is_stable_across_repeated_passes() {
        let target = target("agent-1");
        let first = interjection_delivery_id(&target);
        // Idleness keeps growing between passes; the id must not, or the dedupe
        // never fires and every observer tick delivers another nudge.
        let slightly_later = InterjectionTarget {
            idle_for: std::time::Duration::from_secs(645),
            ..target.clone()
        };
        assert_eq!(first, interjection_delivery_id(&slightly_later));

        // A different card is a different nudge.
        let other_card = InterjectionTarget {
            card_id: "T-7".to_string(),
            ..target
        };
        assert_ne!(first, interjection_delivery_id(&other_card));
    }

    #[test]
    fn stuck_task_ids_reads_task_id_from_the_fact_payload() {
        let fact = |key: &str, task_id: &str| BuddyFact {
            kind: BuddyFactKind::TaskStuck,
            key: key.to_string(),
            source: "task_health",
            payload: serde_json::json!({ "task_id": task_id }),
            seen_at: now(),
            confidence: 1.0,
        };
        let facts = vec![
            fact("a", "task-1"),
            fact("b", "task-1"),
            fact("c", "task-2"),
        ];

        assert_eq!(stuck_task_ids(&facts), vec!["task-1", "task-2"]);
    }

    #[test]
    fn interjection_intent_round_trips_through_its_key() {
        assert_eq!(
            intent_key(SpeechIntent::AgentInterjection),
            "agent_interjection"
        );
        assert_eq!(
            crate::speech_policy::parse_intent_key("agent_interjection"),
            Some(SpeechIntent::AgentInterjection)
        );
        assert!(crate::speech_policy::ALL_INTENT_KEYS.contains(&"agent_interjection"));
    }

    #[test]
    fn interjection_budget_is_tighter_than_a_chat_reaction() {
        let reaction = budget_for(SpeechIntent::ChatReaction);
        let interjection = budget_for(SpeechIntent::AgentInterjection);
        assert!(interjection.per_hour < reaction.per_hour);
        assert!(interjection.per_day < reaction.per_day);
    }

    #[test]
    fn interjection_min_idle_is_long_enough_to_not_yell() {
        assert!(INTERJECTION_MIN_IDLE >= std::time::Duration::from_secs(60));
    }
}
