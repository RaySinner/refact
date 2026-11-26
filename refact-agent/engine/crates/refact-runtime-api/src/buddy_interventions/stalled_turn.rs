use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, OnceLock};

use chrono::{DateTime, Duration, Utc};
use serde_json::json;
use tokio::sync::{Mutex as AMutex, RwLock as ARwLock};
use uuid::Uuid;

use refact_buddy_core::settings::BuddySettings;
use refact_buddy_core::supervision::InterventionRecord;
use refact_buddy_core::supervision_detectors::stalled_turn_reason_context;
use refact_buddy_core::stalled_turn::{
    last_message_snippet, stalled_turn_fact, StalledTurnSignal, TurnContext,
};
use refact_chat_api::delivery::deliver_to_chat;
use refact_chat_api::internal_roles::{event, EventSubkind};
use refact_chat_api::types::{GoalStatus, PendingDelivery, PushMode, SessionState};
use refact_chat_api::{ChatMessage, ChatSession, SubchatParameters, ToolsPolicy};
use refact_core::chat_types::ChatModelType;
use refact_core::chat_types::CacheControl;
use refact_core::string_utils::safe_truncate;

use crate::app_state::AppState;

/// Stamped on every message Buddy delivers into an agent chat so the transcript
/// can tell a stalled-turn nudge apart from a planner steer or an inbox notice.
pub const STALLED_TURN_SOURCE: &str = "buddy.stalled_turn";

/// The agent's own message asks for something only a human can grant.
pub const ESCALATE_USER_PREFIX: &str = "ESCALATE_USER:";

/// Ceiling on the delivered nudge. The agent pays a whole turn to read it, so it
/// stays short enough to act on in one glance.
pub const MAX_REPLY_CHARS: usize = 600;

/// The only card column that still means "this task is being worked on". A card
/// that moved to done/failed/regressed is finished, not idle.
pub const CARD_DOING_COLUMN: &str = "doing";

/// The tool an agent calls to close its task, mirroring the engine's own
/// `chat::generation::SILENT_TAIL_FINISH_TOOL`, which is private to generation.
pub const AGENT_FINISH_TOOL: &str = "agent_finish";

/// Plan and goal bodies can be tens of kilobytes; Buddy only needs the shape of
/// them to say what to do next.
const MAX_PLAN_CHARS: usize = 4_000;
const MAX_GOAL_CHARS: usize = 2_000;
const MAX_NEW_TOKENS: usize = 400;
const MAX_RECENT_DIALOGUE_MESSAGES: usize = 6;
const MAX_RECENT_DIALOGUE_MESSAGE_CHARS: usize = 768;
const MAX_RECENT_DIALOGUE_CHARS: usize = 4_096;

pub const BUDDY_STALLED_TURN_SYSTEM_PROMPT: &str = "\
You are Buddy, a sleepy supervisor watching a coding agent work. \
The agent just ended its turn without calling any single tool, while its task is NOT finished. \
It is probably joking, asking the user for confirmation, or idling. \
Read its last message, its plan and its goal. \
Reply with a short (1-3 sentences) direct instruction: what to do next, concretely. \
Be firm and a little sly or funny, but never vague and never a question. \
EXCEPTION: if the agent's message asks for something ONLY the human user can decide \
(API keys, permissions, payment, approval of a destructive action), reply with text \
starting with ESCALATE_USER: followed by the question to relay to the user.";

/// Collect everything the S-1 classifier needs about a finished assistant turn.
/// Pure extraction from the session: no board loads, no LLM calls, no locks
/// held beyond the session read itself.
pub fn collect_turn_context(session: &ChatSession, tool_calls_count: usize) -> TurnContext<'_> {
    let chat_mode = {
        let mode = session.thread.mode.trim();
        (!mode.is_empty()).then_some(mode)
    };
    let goal_active = session.goal.as_ref().is_some_and(|goal| {
        goal.active
            && !matches!(
                goal.status,
                GoalStatus::Completed | GoalStatus::Stopped | GoalStatus::Transferred
            )
    });
    TurnContext {
        chat_id: &session.chat_id,
        tool_calls_count,
        has_content: last_assistant_has_visible_text(session),
        is_error: false,
        is_aborted: false,
        chat_mode,
        goal_active,
        card_doing: is_task_agent_chat(session),
        plan_has_pending: current_plan_has_unfinished_items(session),
    }
}

/// Whether the chat belongs to a task agent. This is deliberately the cheap
/// half of "card in doing": the board is loaded later, off the classifier path,
/// and a card that already left the doing column cancels the intervention there.
fn is_task_agent_chat(session: &ChatSession) -> bool {
    session
        .thread
        .task_meta
        .as_ref()
        .is_some_and(|meta| meta.role == "agents")
}

/// Whether the chat's plan still has work in it. The classifier only gets a
/// boolean, so the checklist parsing lives next to it.
fn current_plan_has_unfinished_items(session: &ChatSession) -> bool {
    refact_chat_api::session_plan_role::current_base_plan(session)
        .map(|plan| plan_has_unfinished_items(&plan.content.content_text_only()))
        .unwrap_or(false)
}

/// The assistant turn that just ended, even when an `event` notice landed after
/// it: transcripts routinely end with a runner notice, not with the reply.
fn last_assistant_message(session: &ChatSession) -> Option<&ChatMessage> {
    session
        .messages
        .iter()
        .rev()
        .find(|message| message.role == "assistant")
}

/// Whether the last assistant message carries non-empty visible text. A
/// reasoning-only turn is `false`, so the classifier sees an EmptyMessage.
fn last_assistant_has_visible_text(session: &ChatSession) -> bool {
    last_assistant_message(session)
        .is_some_and(|message| !message.content.content_text_only().trim().is_empty())
}

/// Whether a plan body still has unfinished work in it.
///
/// Plan bodies are free-form prose with an optional markdown checklist, so this
/// is a deliberate compromise: a plan without a single checkbox counts as
/// unfinished, because assuming "nothing left" would silence a real stall.
pub fn plan_has_unfinished_items(plan: &str) -> bool {
    if plan.trim().is_empty() {
        return false;
    }
    let mut saw_checkbox = false;
    for line in plan.lines() {
        let Some(checked) = checklist_state(line) else {
            continue;
        };
        saw_checkbox = true;
        if !checked {
            return true;
        }
    }
    !saw_checkbox
}

/// The state of the first markdown checkbox on a line, if it has one. Any bullet
/// marker is accepted, so mixed `-`, `*`, `+` and numbered lists all parse.
fn checklist_state(line: &str) -> Option<bool> {
    let open = line.find('[')?;
    let mut rest = line[open + 1..].chars();
    let mark = rest.next()?;
    if rest.next()? != ']' {
        return None;
    }
    match mark {
        ' ' => Some(false),
        'x' | 'X' => Some(true),
        _ => None,
    }
}

pub async fn handle_stalled_turn_candidate(app: AppState, candidate: InterventionRecord) {
    let Some(chat_id) = candidate
        .evidence
        .ids()
        .first()
        .map(|id| id.chat_id.as_str().to_string())
    else {
        return;
    };
    let Some(projection) = app.buddy.supervision.projection(&chat_id).await else {
        return;
    };
    let Some(reason_context) = stalled_turn_reason_context(&projection) else {
        return;
    };
    let signal = StalledTurnSignal {
        chat_id: chat_id.clone(),
        reason_context,
    };
    let Some(session_arc) = app.chat.sessions.read().await.get(&chat_id).cloned() else {
        return;
    };
    let (snippet, turn_index) = {
        let session = session_arc.lock().await;
        (
            last_message_snippet(&last_assistant_text(&session)),
            session
                .messages
                .iter()
                .filter(|message| message.role == "assistant")
                .count() as u64,
        )
    };
    let fact = stalled_turn_fact(&signal, turn_index, &snippet, Utc::now());
    {
        let mut buddy = app.buddy.buddy.lock().await;
        match buddy.as_mut() {
            Some(service) => service.fact_store.ingest(fact),
            None => return,
        }
    }
    handle_stalled_turn(app, &signal, &snippet, turn_index).await;
}

pub fn last_assistant_text(session: &ChatSession) -> String {
    last_assistant_message(session)
        .map(|message| message.content.content_text_only())
        .unwrap_or_default()
}

/// Per-chat nudge budget. Defaults come from `BuddySettings`; the shape is
/// separate so the pure decision below does not need a settings object.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StalledTurnBudget {
    pub min_interval_secs: u64,
    pub max_per_hour: u32,
}

impl StalledTurnBudget {
    pub fn from_settings(settings: &BuddySettings) -> Self {
        Self {
            min_interval_secs: settings.stalled_turn_min_interval_secs,
            max_per_hour: settings.stalled_turn_max_per_hour,
        }
    }
}

/// Whether one more nudge is allowed right now. `history` is the chat's own
/// timestamps, oldest first; entries older than an hour are ignored.
pub fn stalled_turn_allowed(
    history: &[DateTime<Utc>],
    now: DateTime<Utc>,
    budget: StalledTurnBudget,
) -> bool {
    let hour_ago = now - Duration::hours(1);
    let last_hour = history.iter().filter(|at| **at > hour_ago).count();
    if last_hour as u64 >= budget.max_per_hour as u64 {
        return false;
    }
    match history.iter().max() {
        Some(last) => (now - *last).num_seconds() >= budget.min_interval_secs as i64,
        None => true,
    }
}

fn stalled_turn_history() -> &'static ARwLock<HashMap<String, Vec<DateTime<Utc>>>> {
    static HISTORY: OnceLock<ARwLock<HashMap<String, Vec<DateTime<Utc>>>>> = OnceLock::new();
    HISTORY.get_or_init(|| ARwLock::new(HashMap::new()))
}

/// Reserve this chat's slot before paying for an LLM call. Returns `false` when
/// the chat is already inside its cooldown or out of hourly budget.
pub async fn reserve_stalled_turn_slot(
    chat_id: &str,
    now: DateTime<Utc>,
    budget: StalledTurnBudget,
) -> bool {
    let history = stalled_turn_history();
    let mut guard = history.write().await;
    let entries = guard.entry(chat_id.to_string()).or_default();
    let hour_ago = now - Duration::hours(1);
    entries.retain(|at| *at > hour_ago);
    if !stalled_turn_allowed(entries.as_slice(), now, budget) {
        return false;
    }
    entries.push(now);
    true
}

/// Hand the reserved slot back when the intervention never reached the chat, so
/// a failed LLM call does not burn this chat's cooldown.
pub async fn release_stalled_turn_slot(chat_id: &str, at: DateTime<Utc>) {
    let history = stalled_turn_history();
    let mut guard = history.write().await;
    let Some(entries) = guard.get_mut(chat_id) else {
        return;
    };
    if entries.last() == Some(&at) {
        entries.pop();
    }
}

pub fn intervention_enabled(settings: &BuddySettings) -> bool {
    crate::buddy_supervision_advisor::stalled_turn_enabled(settings)
}

/// The user has already written after the stalled turn: nudging the agent now
/// would talk over a human. No assistant message at all counts as "moved on".
pub fn user_spoke_after_last_assistant(messages: &[ChatMessage]) -> bool {
    let Some(last_assistant) = messages
        .iter()
        .rposition(|message| message.role == "assistant")
    else {
        return true;
    };
    messages[last_assistant + 1..]
        .iter()
        .any(|message| message.role == "user")
}

/// `true` when the chat has not produced a tool call since the last stalled-turn
/// nudge: the agent read the nudge and did nothing, so a second one is noise.
pub fn no_progress_since_last_nudge(messages: &[ChatMessage]) -> bool {
    let Some(nudge_index) = messages.iter().rposition(is_stalled_turn_nudge) else {
        return false;
    };
    !messages[nudge_index + 1..].iter().any(|message| {
        message.role == "assistant"
            && message
                .tool_calls
                .as_ref()
                .is_some_and(|calls| !calls.is_empty())
    })
}

fn is_stalled_turn_nudge(message: &ChatMessage) -> bool {
    message
        .extra
        .get("event")
        .and_then(|event| event.get("source"))
        .and_then(|source| source.as_str())
        == Some(STALLED_TURN_SOURCE)
}

/// Whether the agent already closed its task in the trailing turn window. The
/// finishing call is never the very last message (the turn stops on it), so the
/// whole window since the last user message is inspected.
pub fn agent_already_finished(messages: &[ChatMessage]) -> bool {
    messages
        .iter()
        .rev()
        .take_while(|message| message.role != "user")
        .filter(|message| message.role == "assistant")
        .any(|message| {
            message.tool_calls.as_ref().is_some_and(|calls| {
                calls
                    .iter()
                    .any(|call| call.function.name == AGENT_FINISH_TOOL)
            })
        })
}

/// The agent must be genuinely between turns. A draft, a running tool loop or a
/// non-terminal state means it already went back to work on its own.
pub fn agent_ready_for_nudge(session: &ChatSession) -> bool {
    !session.closed
        && session.draft_message.is_none()
        && session.turn_depth == 0
        && matches!(
            session.runtime.state,
            SessionState::Idle | SessionState::Completed | SessionState::WaitingUserInput
        )
}

/// What Buddy said, once the marker is stripped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BuddyReply {
    Nudge(String),
    EscalateUser(String),
}

pub fn parse_buddy_reply(reply: &str) -> Option<BuddyReply> {
    let trimmed = reply.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Some(rest) = trimmed.strip_prefix(ESCALATE_USER_PREFIX) {
        let question = rest.trim();
        return (!question.is_empty()).then(|| BuddyReply::EscalateUser(cap_reply(question)));
    }
    Some(BuddyReply::Nudge(cap_reply(trimmed)))
}

fn cap_reply(text: &str) -> String {
    safe_truncate(text, MAX_REPLY_CHARS).trim_end().to_string()
}

/// Everything Buddy needs to write one actionable nudge. Owned, so it survives
/// the session lock being released before the LLM call.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StalledTurnPrompt {
    pub chat_id: String,
    pub mode: String,
    pub reason_context: String,
    pub snippet: String,
    pub plan: Option<String>,
    pub goal: Option<String>,
    pub task_id: Option<String>,
    pub card_id: Option<String>,
    pub card_title: Option<String>,
    pub card_target_files: Vec<String>,
    pub recent_dialogue: String,
}

pub fn recent_dialogue(session: &ChatSession) -> String {
    let entries = session
        .messages
        .iter()
        .filter(|message| matches!(message.role.as_str(), "user" | "assistant"))
        .filter_map(|message| {
            let text = message.content.content_text_only();
            let text = text.trim();
            (!text.is_empty()).then(|| {
                format!(
                    "{}: {}",
                    message.role,
                    refact_buddy_core::autonomous_redaction::redact_and_cap_text(
                        text,
                        MAX_RECENT_DIALOGUE_MESSAGE_CHARS
                    )
                )
            })
        })
        .collect::<Vec<_>>();
    let mut selected = VecDeque::new();
    let mut total = 0usize;
    for entry in entries.into_iter().rev().take(MAX_RECENT_DIALOGUE_MESSAGES) {
        let cost = entry.len() + 2;
        if total + cost > MAX_RECENT_DIALOGUE_CHARS {
            break;
        }
        total += cost;
        selected.push_front(entry);
    }
    selected.into_iter().collect::<Vec<_>>().join("\n\n")
}

pub fn build_buddy_user_prompt(prompt: &StalledTurnPrompt) -> String {
    let mut parts = vec![format!("Chat: {}", prompt.chat_id)];
    if !prompt.mode.is_empty() {
        parts.push(format!("Mode: {}", prompt.mode));
    }
    parts.push(format!(
        "Why you are here: {}.",
        reason_context_hint(&prompt.reason_context)
    ));
    parts.push(format!(
        "The agent's last message (that turn called no tools):\n\"\"\"\n{}\n\"\"\"",
        prompt.snippet
    ));
    if !prompt.recent_dialogue.is_empty() {
        parts.push(format!(
            "Recent user/assistant dialogue (bounded):\n{}",
            prompt.recent_dialogue
        ));
    }
    if let Some(plan) = prompt
        .plan
        .as_deref()
        .map(str::trim)
        .filter(|p| !p.is_empty())
    {
        parts.push(format!(
            "Current plan:\n{}",
            safe_truncate(plan, MAX_PLAN_CHARS)
        ));
    }
    if let Some(goal) = prompt
        .goal
        .as_deref()
        .map(str::trim)
        .filter(|g| !g.is_empty())
    {
        parts.push(format!(
            "Current goal:\n{}",
            safe_truncate(goal, MAX_GOAL_CHARS)
        ));
    }
    if let Some(card_id) = prompt.card_id.as_deref() {
        let title = prompt.card_title.as_deref().unwrap_or("").trim();
        if title.is_empty() {
            parts.push(format!("Card: {card_id}"));
        } else {
            parts.push(format!("Card: {card_id} - {title}"));
        }
        if !prompt.card_target_files.is_empty() {
            parts.push(format!(
                "Card target files: {}",
                prompt.card_target_files.join(", ")
            ));
        }
    }
    if prompt.reason_context == "missing_goal" {
        parts.push(
            "Tell the agent to call set_goal with a short concrete goal inferred from this dialogue, then continue the work."
                .to_string(),
        );
    } else {
        parts.push("Tell the agent to finish the active objective now.".to_string());
    }
    parts.push("Write the nudge now.".to_string());
    parts.join("\n\n")
}

fn reason_context_hint(reason_context: &str) -> &'static str {
    match reason_context {
        "goal_active" => "the chat has an active goal that is not finished",
        "card_doing" => "the agent's task card is still in the doing column",
        "missing_goal" => "this task chat has no installed goal",
        "plan_pending" => "the chat has a plan that is not finished",
        _ => "the turn ended without a tool call while work is still pending",
    }
}

pub fn collect_prompt_context(
    session: &ChatSession,
    signal: &StalledTurnSignal,
    snippet: &str,
) -> StalledTurnPrompt {
    let task_meta = session.thread.task_meta.as_ref();
    StalledTurnPrompt {
        chat_id: signal.chat_id.clone(),
        mode: session.thread.mode.trim().to_string(),
        reason_context: signal.reason_context.to_string(),
        snippet: snippet.to_string(),
        plan: refact_chat_api::plan_role::synthesize_current_plan(session),
        goal: session
            .goal
            .as_ref()
            .filter(|goal| goal.active)
            .map(|goal| goal.content.clone()),
        task_id: task_meta.map(|meta| meta.task_id.clone()),
        card_id: task_meta.and_then(|meta| meta.card_id.clone()),
        card_title: None,
        card_target_files: Vec::new(),
        recent_dialogue: recent_dialogue(session),
    }
}

fn objective_is_current(session: &ChatSession, reason_context: &str) -> bool {
    match reason_context {
        "goal_active" => session.goal.as_ref().is_some_and(|goal| {
            goal.active
                && goal.can_pursue
                && matches!(goal.status, GoalStatus::Active | GoalStatus::Verifying)
        }),
        "card_doing" => is_task_agent_chat(session),
        "missing_goal" => session.thread.task_meta.is_some() && session.goal.is_none(),
        "plan_pending" => current_plan_has_unfinished_items(session),
        _ => false,
    }
}

/// Card title and target files are what make the nudge about *this* task rather
/// than about tidiness in general. Returns the card's column when the board was
/// readable. A board that cannot be read is not fatal.
async fn attach_card_context(app: &AppState, prompt: &mut StalledTurnPrompt) -> Option<String> {
    let (Some(task_id), Some(card_id)) = (prompt.task_id.clone(), prompt.card_id.clone()) else {
        return None;
    };
    match crate::task_storage::load_board(app.gcx.clone(), &task_id).await {
        Ok(board) => board.get_card(&card_id).map(|card| {
            prompt.card_title = Some(card.title.clone());
            prompt.card_target_files = card.target_files.clone();
            card.column.clone()
        }),
        Err(error) => {
            tracing::warn!(%task_id, %error, "stalled_turn: could not read the task board");
            None
        }
    }
}

/// Buddy only nags while the card is actually being worked on. An unreadable
/// board (`None`) fails open: the nudge is still worth sending.
pub fn card_work_is_active(column: Option<&str>) -> bool {
    match column {
        Some(column) => column == CARD_DOING_COLUMN,
        None => true,
    }
}

pub fn stalled_turn_delivery_id(chat_id: &str, turn_index: u64) -> String {
    format!("stalled-turn:{chat_id}:{turn_index}")
}

fn escalation_delivery_id(chat_id: &str, turn_index: u64) -> String {
    format!("stalled-turn-escalation:{chat_id}:{turn_index}")
}

/// The nudge lands as a runner-style system notice, never as a user message:
/// the human did not say this. It wakes the agent so a stalled turn actually
/// resumes. An escalation is the opposite: it is for the human, so it lands
/// quietly and does not start a turn.
pub fn stalled_turn_delivery(
    chat_id: &str,
    turn_index: u64,
    reply: &BuddyReply,
) -> PendingDelivery {
    match reply {
        BuddyReply::Nudge(text) => PendingDelivery::with_id(
            stalled_turn_delivery_id(chat_id, turn_index),
            vec![event(
                EventSubkind::SystemNotice,
                STALLED_TURN_SOURCE,
                json!({"kind": "stalled_turn_nudge", "turn_index": turn_index}),
                text.clone(),
            )],
            PushMode::Append,
            STALLED_TURN_SOURCE,
            true,
        ),
        BuddyReply::EscalateUser(question) => PendingDelivery::with_id(
            escalation_delivery_id(chat_id, turn_index),
            vec![event(
                EventSubkind::SystemNotice,
                STALLED_TURN_SOURCE,
                json!({"kind": "stalled_turn_escalation", "turn_index": turn_index}),
                question.clone(),
            )],
            PushMode::WhenIdle,
            STALLED_TURN_SOURCE,
            false,
        ),
    }
}

/// One raw reply from Buddy, turned into the delivery it should become. Split
/// out so the reply -> delivery step is testable without an LLM.
pub fn intervention_delivery(
    chat_id: &str,
    turn_index: u64,
    buddy_reply: &str,
) -> Option<PendingDelivery> {
    parse_buddy_reply(buddy_reply).map(|reply| stalled_turn_delivery(chat_id, turn_index, &reply))
}

/// The intervention itself: Buddy's own cheap model reads the stalled turn and
/// either goads the agent onward or hands a human-only question to the user.
/// Every failure is a warning line; nothing here may break the generation that
/// triggered the hook.
async fn handle_stalled_turn(
    app: AppState,
    signal: &StalledTurnSignal,
    snippet: &str,
    turn_index: u64,
) {
    if let Err(error) = run_stalled_turn_intervention(app, signal, snippet, turn_index).await {
        tracing::warn!("stalled_turn: intervention failed (ignored): {error}");
    }
}

async fn run_stalled_turn_intervention(
    app: AppState,
    signal: &StalledTurnSignal,
    snippet: &str,
    turn_index: u64,
) -> Result<(), String> {
    let settings = {
        let buddy = app.buddy.buddy.lock().await;
        match buddy.as_ref() {
            Some(service) => service.settings.clone(),
            None => return Ok(()),
        }
    };
    if !intervention_enabled(&settings) {
        return Ok(());
    }

    let Some(session_arc) = app.chat.sessions.read().await.get(&signal.chat_id).cloned() else {
        return Ok(());
    };

    let mut prompt = {
        let session = session_arc.lock().await;
        if !agent_ready_for_nudge(&session) {
            tracing::info!(chat_id = %signal.chat_id, "stalled_turn: agent already busy, staying quiet");
            return Ok(());
        }
        if user_spoke_after_last_assistant(&session.messages) {
            tracing::info!(chat_id = %signal.chat_id, "stalled_turn: user already spoke, staying quiet");
            return Ok(());
        }
        if agent_already_finished(&session.messages) {
            tracing::info!(chat_id = %signal.chat_id, "stalled_turn: agent already finished, staying quiet");
            return Ok(());
        }
        if no_progress_since_last_nudge(&session.messages) {
            tracing::info!(chat_id = %signal.chat_id, "stalled_turn: no progress since the last nudge, staying quiet");
            return Ok(());
        }
        if !objective_is_current(&session, signal.reason_context) {
            return Ok(());
        }
        collect_prompt_context(&session, signal, snippet)
    };
    let card_column = attach_card_context(&app, &mut prompt).await;
    if !card_work_is_active(card_column.as_deref()) {
        tracing::info!(
            chat_id = %signal.chat_id,
            column = card_column.as_deref().unwrap_or_default(),
            "stalled_turn: card is not in the doing column, staying quiet"
        );
        return Ok(());
    }

    let budget = StalledTurnBudget::from_settings(&settings);
    let reserved_at = Utc::now();
    if !reserve_stalled_turn_slot(&signal.chat_id, reserved_at, budget).await {
        tracing::info!(chat_id = %signal.chat_id, "stalled_turn: intervention rate limited");
        return Ok(());
    }

    let reply_text = match ask_buddy_model(&app, &prompt).await {
        Ok(reply_text) => reply_text,
        Err(error) => {
            release_stalled_turn_slot(&signal.chat_id, reserved_at).await;
            return Err(error);
        }
    };
    let Some(delivery) = intervention_delivery(&signal.chat_id, turn_index, &reply_text) else {
        release_stalled_turn_slot(&signal.chat_id, reserved_at).await;
        tracing::info!(chat_id = %signal.chat_id, "stalled_turn: buddy replied with nothing usable");
        return Ok(());
    };

    let chat_moved_on = {
        let session = session_arc.lock().await;
        !agent_ready_for_nudge(&session)
            || user_spoke_after_last_assistant(&session.messages)
            || !objective_is_current(&session, signal.reason_context)
    };
    if chat_moved_on {
        release_stalled_turn_slot(&signal.chat_id, reserved_at).await;
        tracing::info!(
            chat_id = %signal.chat_id,
            "stalled_turn: chat moved on while buddy was thinking, dropping the reply"
        );
        return Ok(());
    }

    let delivery_reference = delivery.id.clone();
    let message_id = delivery
        .messages
        .first()
        .map(|message| message.message_id.clone());
    let outcome = match deliver_to_chat(app.clone(), &signal.chat_id, delivery).await {
        Ok(outcome) => outcome,
        Err(error) => {
            release_stalled_turn_slot(&signal.chat_id, reserved_at).await;
            return Err(error);
        }
    };
    crate::buddy_supervision::collect_intervention(
        &app,
        &signal.chat_id,
        &delivery_reference,
        message_id.as_deref(),
    )
    .await;
    tracing::info!(chat_id = %signal.chat_id, ?outcome, "stalled_turn: buddy reply delivered");
    Ok(())
}

/// One-shot, tool-free call on the Buddy model slot. The user's buddy model is
/// the cheap one by definition (`chat_buddy_model`), which is exactly the
/// "weak little model" the intervention is supposed to use.
async fn ask_buddy_model(app: &AppState, prompt: &StalledTurnPrompt) -> Result<String, String> {
    let mut config = app
        .chat_ctx()
        .subchat
        .resolve_subchat_config(
            "follow_up",
            false,
            Some(format!("buddy-stalled-turn-{}", Uuid::new_v4())),
            Some("Buddy Stalled Turn".to_string()),
            Some(vec![]),
            1,
            false,
            "buddy".to_string(),
        )
        .await
        .map_err(|error| format!("subchat config: {error}"))?;

    let params = SubchatParameters {
        subchat_model_type: ChatModelType::Buddy,
        subchat_model: String::new(),
        subchat_n_ctx: config.n_ctx,
        subchat_max_new_tokens: MAX_NEW_TOKENS,
        subchat_temperature: Some(0.3),
        subchat_tokens_for_rag: 0,
        subchat_reasoning_effort: None,
        subchat_cache_control: CacheControl::Ephemeral,
    };
    let model = crate::subchat_runtime::resolve_subchat_model(&app.chat_ctx(), &params)
        .await
        .map_err(|error| format!("buddy model: {error}"))?;

    config.tool_name = "buddy_stalled_turn".to_string();
    config.tools = ToolsPolicy::None;
    config.max_steps = 1;
    config.model = model;
    config.max_new_tokens = MAX_NEW_TOKENS;
    config.temperature = Some(0.3);
    config.buddy_meta = Some(refact_core::buddy_meta::BuddyThreadMeta {
        is_buddy_chat: true,
        buddy_chat_kind: "system".to_string(),
        workflow_id: Some("buddy_stalled_turn".to_string()),
    });

    let messages = vec![
        ChatMessage::new(
            "system".to_string(),
            BUDDY_STALLED_TURN_SYSTEM_PROMPT.to_string(),
        ),
        ChatMessage::new("user".to_string(), build_buddy_user_prompt(prompt)),
    ];
    let result = app
        .chat_ctx()
        .subchat
        .run_subchat(messages, config)
        .await
        .map_err(|error| format!("subchat: {error}"))?;
    result
        .messages
        .last()
        .filter(|message| message.role == "assistant")
        .map(|message| message.content.content_text_only())
        .ok_or_else(|| "buddy returned no assistant message".to_string())
}
