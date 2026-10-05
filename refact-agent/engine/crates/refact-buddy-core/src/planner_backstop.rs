//! Buddy as the planner's backstop for a task agent's unanswered question.
//!
//! `agent_ask_planner` records a question on a card and — unless the planner is
//! awake — nothing comes back. The agent then waits forever, and nobody notices
//! because the silence looks exactly like progress. This module is the *decision*
//! half of the remedy: given the question, the clock and Buddy's settings, say
//! whether Buddy may step in, and if so what it is allowed to say.
//!
//! Two rules shape every decision here:
//!
//! 1. **Buddy never decides for a human.** A blocking question (`urgency=block`)
//!    exists precisely because a human has to choose. When the planner is silent
//!    Buddy says it is still waiting and stops there; it does not write an answer.
//! 2. **A timeout is a budget, not a licence.** Everything the user can turn off
//!    (the backstop flag, Buddy itself, quiet mode, quiet hours, muted chats and
//!    intents) is checked *before* the clock, and the question must still be
//!    unanswered at the moment of the decision.
//!
//! This module is deliberately pure: it touches no board, no session and no
//! clock of its own, so every gate below is testable without an engine.

use std::collections::HashSet;

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use crate::settings::BuddySettings;
use crate::speech_policy::gate_speech;
use crate::state::SpeechRotationState;
use crate::voice_service::SpeechIntent;

/// The marker a *reply* leaves on a card. Single source of truth: the planner QnA
/// tool parses it with [`parse_reply_marker`], and Buddy writes it with
/// [`reply_marker`], so the two cannot drift into disagreeing about whether a
/// question was answered.
pub const REPLY_PREFIX: &str = "[REPLY:";

/// Ceiling on what Buddy writes or says. A card agent pays a whole turn to read
/// it, and a truncated instruction that lost its last sentence is worse than
/// silence.
pub const MAX_BACKSTOP_TEXT_CHARS: usize = 400;

/// How long Buddy waits before stepping in, when the user has not configured it.
pub const DEFAULT_PLANNER_BACKSTOP_AFTER_SECS: u64 = 300;

/// One unanswered agent question, as the decision sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackstopQuestion {
    pub card_id: String,
    pub question_id: String,
    pub urgency: BackstopUrgency,
    pub question: String,
    pub asked_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackstopUrgency {
    Info,
    Block,
}

impl BackstopUrgency {
    pub fn parse(value: &str) -> Self {
        if value == "block" {
            BackstopUrgency::Block
        } else {
            BackstopUrgency::Info
        }
    }
}

/// Why Buddy stayed out of it. Distinct reasons are what make a dropped backstop
/// debuggable instead of merely silent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackstopSkipReason {
    /// The user has not turned the backstop on.
    Disabled,
    /// Buddy is switched off or in quiet mode.
    BuddyDisabled,
    /// The question already has an answer, so there is nothing to cover.
    AlreadyAnswered,
    /// The local hour falls inside the quiet window.
    QuietHours,
    /// The target chat is in `muted_chat_ids`.
    ChatMuted,
    /// The intent is in `muted_intents`.
    IntentMuted,
    /// The speech budget for speaking into an agent chat is spent.
    IntentBudget,
    /// Some other speech gate refused; the reason is preserved in the log.
    Gated,
    /// The question has not waited long enough yet.
    BeforeTimeout,
}

impl BackstopSkipReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::BuddyDisabled => "buddy_disabled",
            Self::AlreadyAnswered => "already_answered",
            Self::QuietHours => "quiet_hours",
            Self::ChatMuted => "chat_muted",
            Self::IntentMuted => "intent_muted",
            Self::IntentBudget => "intent_budget",
            Self::Gated => "gated",
            Self::BeforeTimeout => "before_timeout",
        }
    }
}

/// What Buddy is allowed to do about one question.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackstopPlan {
    /// Write an answer and tell the agent. Only for non-blocking questions.
    Answer { answer: String },
    /// Tell the agent the question is still waiting for a person. No answer is
    /// written, because the answer is not Buddy's to give.
    NotifyHuman { text: String },
    Skip(BackstopSkipReason),
}

fn truncate_chars(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect()
}

/// A coarse "5 minutes" style duration. Approximate on purpose: the exact figure
/// is noise in a sentence aimed at a stuck agent, and rounding to whole minutes
/// keeps the wording stable between ticks of the same wait.
fn format_waited(waited: Duration) -> String {
    let total_secs = waited.num_seconds().max(0);
    if total_secs < 60 {
        return format!("{total_secs} seconds");
    }
    let minutes = total_secs / 60;
    if minutes < 60 {
        return format!("{minutes} minutes");
    }
    format!("{} hours", minutes / 60)
}

/// The answer Buddy writes for a non-blocking question.
///
/// It is deliberately *procedural*: "if you can proceed without it, take the
/// smallest reversible step" is advice about how to work, not a decision about
/// what to build. The question is quoted so the agent can tell whether the
/// answer is even about its problem.
pub fn answer_text(question: &BackstopQuestion, waited: Duration) -> String {
    truncate_chars(
        &format!(
            "Buddy answered because the planner stayed silent for {}. \
             Nobody decided this for you: if you can move on without it, take the \
             smallest reversible step and note the assumption in your report; if you \
             cannot, call `agent_finish` naming question `{}` as still open. \
             The question was: {}",
            format_waited(waited),
            question.question_id,
            truncate_chars(question.question.trim(), 200),
        ),
        MAX_BACKSTOP_TEXT_CHARS,
    )
}

/// What Buddy says about a blocking question nobody has answered.
pub fn human_wait_text(question: &BackstopQuestion, waited: Duration) -> String {
    let text = format!(
        "Buddy here: question `{}` on card `{}` is still unanswered after {} and it is a \
         decision only a person can make, so Buddy is not answering it. Stop waiting: call \
         `agent_finish` with the open question named, or keep working on the part that does \
         not need the answer. The question was: {}",
        question.question_id,
        question.card_id,
        format_waited(waited),
        truncate_chars(question.question.trim(), 200),
    );
    truncate_chars(&text, MAX_BACKSTOP_TEXT_CHARS)
}

/// The pure decision.
///
/// `chat_id` is the chat the answer would land in (a room has several, so it may
/// be `None`; chat-level mutes then simply do not apply).
pub fn plan_backstop(
    settings: &BuddySettings,
    rotation: &SpeechRotationState,
    chat_id: Option<&str>,
    local_hour: u32,
    auto_quiet_window: Option<(u8, u8)>,
    now: DateTime<Utc>,
    question: &BackstopQuestion,
    answered: bool,
) -> BackstopPlan {
    if !settings.planner_backstop_enabled {
        return BackstopPlan::Skip(BackstopSkipReason::Disabled);
    }
    if answered {
        return BackstopPlan::Skip(BackstopSkipReason::AlreadyAnswered);
    }
    if !settings.enabled || settings.quiet_mode {
        return BackstopPlan::Skip(BackstopSkipReason::BuddyDisabled);
    }

    // The same gate an interjection passes: the answer costs the agent a turn, so
    // muted chats, muted intents, quiet hours and the speech budget all apply.
    let decision = gate_speech(
        settings,
        rotation,
        Some(SpeechIntent::AgentInterjection),
        chat_id,
        local_hour,
        auto_quiet_window,
        now,
    );
    if !decision.allowed {
        return BackstopPlan::Skip(match decision.reason {
            "chat_muted" => BackstopSkipReason::ChatMuted,
            "intent_muted" => BackstopSkipReason::IntentMuted,
            "quiet_hours" => BackstopSkipReason::QuietHours,
            "intent_budget" => BackstopSkipReason::IntentBudget,
            _ => BackstopSkipReason::Gated,
        });
    }

    let waited = now.signed_duration_since(question.asked_at);
    if waited < Duration::seconds(settings.planner_backstop_after_secs as i64) {
        return BackstopPlan::Skip(BackstopSkipReason::BeforeTimeout);
    }

    match question.urgency {
        // A blocking question is a request for a person. Buddy acknowledges and
        // stops; writing an "answer" here would be exactly the silent decision
        // the urgency flag exists to prevent.
        BackstopUrgency::Block => BackstopPlan::NotifyHuman {
            text: human_wait_text(question, waited),
        },
        BackstopUrgency::Info => BackstopPlan::Answer {
            answer: answer_text(question, waited),
        },
    }
}

/// The status update that answers a question. Same format the planner's own reply
/// writes, so `collect_unanswered_questions` drops the question either way.
pub fn reply_marker(question_id: &str, answer: &str) -> String {
    format!("{REPLY_PREFIX}{}] {}", question_id, answer)
}

/// The question id a status update answers, if it is a well-formed reply.
pub fn parse_reply_marker(message: &str) -> Option<(&str, &str)> {
    let rest = message.strip_prefix(REPLY_PREFIX)?;
    let end = rest.find(']')?;
    let id = &rest[..end];
    if !is_question_id(id) {
        return None;
    }
    Some((id, rest[end + 1..].trim_start()))
}

fn is_question_id(value: &str) -> bool {
    value.len() == 8
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// The question ids these status updates already answer.
pub fn answered_question_ids<'a>(messages: impl Iterator<Item = &'a str>) -> HashSet<String> {
    messages
        .filter_map(|message| parse_reply_marker(message).map(|(id, _)| id.to_string()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::settings::QuietHoursMode;

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-05-15T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    fn question(urgency: BackstopUrgency, waited_secs: i64) -> BackstopQuestion {
        BackstopQuestion {
            card_id: "T-12".to_string(),
            question_id: "1234abcd".to_string(),
            urgency,
            question: "Should I split the parser or patch it?".to_string(),
            asked_at: now() - Duration::seconds(waited_secs),
        }
    }

    fn allow_all() -> BuddySettings {
        let mut settings = BuddySettings::default();
        settings.quiet_hours_mode = QuietHoursMode::Off;
        settings.planner_backstop_enabled = true;
        settings
    }

    fn plan(settings: &BuddySettings, question: &BackstopQuestion) -> BackstopPlan {
        plan_backstop(
            settings,
            &SpeechRotationState::default(),
            Some("agent-T-12-code"),
            12,
            None,
            now(),
            question,
            false,
        )
    }

    #[test]
    fn buddy_backstop_disabled_by_default() {
        let settings = BuddySettings::default();
        assert!(
            !settings.planner_backstop_enabled,
            "an engine that answers for a human must opt in"
        );
        assert_eq!(
            settings.planner_backstop_after_secs,
            DEFAULT_PLANNER_BACKSTOP_AFTER_SECS
        );
        assert_eq!(
            plan(&settings, &question(BackstopUrgency::Info, 3600)),
            BackstopPlan::Skip(BackstopSkipReason::Disabled)
        );
    }

    #[test]
    fn old_settings_file_gets_the_backstop_defaults() {
        let json = r#"{"enabled": true, "auto_diagnostics": true, "auto_issue_creation": false}"#;
        let settings: BuddySettings = serde_json::from_str(json).unwrap();

        assert!(!settings.planner_backstop_enabled);
        assert_eq!(
            settings.planner_backstop_after_secs,
            DEFAULT_PLANNER_BACKSTOP_AFTER_SECS
        );
    }

    #[test]
    fn buddy_answers_after_timeout() {
        let answer = plan(&allow_all(), &question(BackstopUrgency::Info, 400));
        let BackstopPlan::Answer { answer } = answer else {
            panic!("an overdue non-blocking question must be answered")
        };
        assert!(answer.contains("1234abcd"), "{answer}");
        assert!(
            answer.contains("Should I split the parser or patch it?"),
            "the agent must be able to tell the answer is about its question: {answer}"
        );
        assert!(
            answer.chars().count() <= MAX_BACKSTOP_TEXT_CHARS,
            "backstop text is over budget: {answer}"
        );
    }

    #[test]
    fn buddy_does_not_answer_before_timeout() {
        assert_eq!(
            plan(&allow_all(), &question(BackstopUrgency::Info, 30)),
            BackstopPlan::Skip(BackstopSkipReason::BeforeTimeout)
        );
    }

    #[test]
    fn buddy_does_not_answer_when_quiet_hours_active() {
        let mut settings = allow_all();
        settings.quiet_hours_mode = QuietHoursMode::Fixed;
        settings.quiet_hours_start = 22;
        settings.quiet_hours_end = 8;
        let question = question(BackstopUrgency::Info, 3600);

        let at_night = plan_backstop(
            &settings,
            &SpeechRotationState::default(),
            Some("agent-T-12-code"),
            23,
            None,
            now(),
            &question,
            false,
        );
        assert_eq!(at_night, BackstopPlan::Skip(BackstopSkipReason::QuietHours));

        let at_noon = plan_backstop(
            &settings,
            &SpeechRotationState::default(),
            Some("agent-T-12-code"),
            12,
            None,
            now(),
            &question,
            false,
        );
        assert!(matches!(at_noon, BackstopPlan::Answer { .. }));
    }

    #[test]
    fn buddy_does_not_answer_blocking_question_without_human() {
        let plan = plan(&allow_all(), &question(BackstopUrgency::Block, 3600));
        let BackstopPlan::NotifyHuman { text } = plan else {
            panic!("a blocking question must never receive an answer from Buddy")
        };
        assert!(text.contains("only a person can make"), "{text}");
        assert!(
            !text.contains("Buddy answered"),
            "Buddy must not phrase a blocking question as answered: {text}"
        );
    }

    #[test]
    fn question_survives_until_answered() {
        let settings = allow_all();
        let question = question(BackstopUrgency::Info, 3600);

        assert!(matches!(
            plan_backstop(
                &settings,
                &SpeechRotationState::default(),
                None,
                12,
                None,
                now(),
                &question,
                false,
            ),
            BackstopPlan::Answer { .. }
        ));

        // Once the planner (or anyone) answers, Buddy has nothing to cover.
        assert_eq!(
            plan_backstop(
                &settings,
                &SpeechRotationState::default(),
                None,
                12,
                None,
                now(),
                &question,
                true,
            ),
            BackstopPlan::Skip(BackstopSkipReason::AlreadyAnswered)
        );
    }

    #[test]
    fn buddy_answer_is_written_into_question_answer() {
        let marker = reply_marker("1234abcd", "Take the small fix.");
        let answered = answered_question_ids([marker.as_str()].into_iter());

        assert!(answered.contains("1234abcd"));
        assert!(!answered.contains("deadbeef"));
        assert_eq!(
            parse_reply_marker(&marker),
            Some(("1234abcd", "Take the small fix."))
        );
        assert_eq!(parse_reply_marker("[ASK:1234abcd] a question"), None);
    }

    #[test]
    fn buddy_stays_quiet_when_it_is_switched_off() {
        let mut settings = allow_all();
        settings.enabled = false;
        assert_eq!(
            plan(&settings, &question(BackstopUrgency::Info, 3600)),
            BackstopPlan::Skip(BackstopSkipReason::BuddyDisabled)
        );

        let mut settings = allow_all();
        settings.quiet_mode = true;
        assert_eq!(
            plan(&settings, &question(BackstopUrgency::Info, 3600)),
            BackstopPlan::Skip(BackstopSkipReason::BuddyDisabled)
        );
    }

    #[test]
    fn muted_chat_and_intent_stop_the_backstop() {
        let mut settings = allow_all();
        settings.muted_chat_ids.push("agent-T-12-code".to_string());
        assert_eq!(
            plan(&settings, &question(BackstopUrgency::Info, 3600)),
            BackstopPlan::Skip(BackstopSkipReason::ChatMuted)
        );

        let mut settings = allow_all();
        settings
            .muted_intents
            .push(crate::speech_policy::intent_key(SpeechIntent::AgentInterjection).to_string());
        assert_eq!(
            plan(&settings, &question(BackstopUrgency::Info, 3600)),
            BackstopPlan::Skip(BackstopSkipReason::IntentMuted)
        );
    }
}
