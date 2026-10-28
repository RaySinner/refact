use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use chrono::{DateTime, Duration, Utc};
use serde_json::json;
use tokio::sync::{Mutex as AMutex, RwLock as ARwLock};
use uuid::Uuid;

use crate::app_state::AppState;
use crate::call_validation::{ChatMessage, ChatModelType, SubchatParameters};
use crate::chat::delivery::deliver_to_chat;
use crate::chat::internal_roles::{event, EventSubkind};
use crate::chat::types::{
    ChatSession, GoalStatus, PendingDelivery, PushMode, SessionState,
};
use crate::llm::params::CacheControl;
use crate::tasks::storage;
use refact_buddy_core::settings::BuddySettings;
use refact_buddy_core::stalled_turn::{
    classify_stalled_turn, last_message_snippet, stalled_turn_fact, StalledTurnSignal,
    StalledTurnVerdict, TurnContext,
};
use refact_core::string_utils::safe_truncate;

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
pub(super) fn collect_turn_context(
    session: &ChatSession,
    tool_calls_count: usize,
) -> TurnContext<'_> {
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
    crate::chat::plan_role::current_base_plan(session)
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

/// Zero-latency stall hook: called straight from the turn-finalisation site.
/// Never blocks generation, never panics: every failure is a warning line.
pub(crate) async fn maybe_detect_stalled_turn(
    app: AppState,
    session_arc: Arc<AMutex<ChatSession>>,
    tool_calls_count: usize,
    turn_index: u64,
    nudged_by_silent_tail: bool,
) {
    let result = detect_and_record_stalled_turn(
        app,
        &session_arc,
        tool_calls_count,
        turn_index,
        nudged_by_silent_tail,
    )
    .await;
    if let Err(error) = result {
        tracing::warn!("stalled_turn: hook failed (ignored): {error}");
    }
}

/// Whether the hook has anything to do at all. The engine's own silent-tail
/// nudge has already told the agent to keep going, so that turn is not Buddy's
/// business: no fact, no LLM call.
pub fn stalled_turn_hook_applies(nudged_by_silent_tail: bool, settings_enabled: bool) -> bool {
    settings_enabled && !nudged_by_silent_tail
}

async fn detect_and_record_stalled_turn(
    app: AppState,
    session_arc: &Arc<AMutex<ChatSession>>,
    tool_calls_count: usize,
    turn_index: u64,
    nudged_by_silent_tail: bool,
) -> Result<(), String> {
    let settings_enabled = {
        let buddy = app.buddy.buddy.lock().await;
        buddy
            .as_ref()
            .map(|svc| svc.settings.stalled_turn_intervention)
            .unwrap_or(false)
    };
    if !stalled_turn_hook_applies(nudged_by_silent_tail, settings_enabled) {
        return Ok(());
    }

    let snippet = {
        let session = session_arc.lock().await;
        if session.abort_flag.load(std::sync::atomic::Ordering::SeqCst) {
            return Ok(());
        }
        let ctx = collect_turn_context(&session, tool_calls_count);
        match classify_stalled_turn(&ctx) {
            StalledTurnVerdict::NotStalled(_) => return Ok(()),
            StalledTurnVerdict::Stalled(signal) => {
                let snippet = last_message_snippet(&last_assistant_text(&session));
                tracing::info!(
                    chat_id = %signal.chat_id,
                    reason_context = signal.reason_context,
                    turn_index,
                    "stalled_turn: agent turn ended without tool calls while an objective is active"
                );
                (signal, snippet)
            }
        }
    };
    let (signal, snippet) = snippet;

    let fact = stalled_turn_fact(&signal, turn_index, &snippet, Utc::now());
    {
        let mut buddy = app.buddy.buddy.lock().await;
        match buddy.as_mut() {
            Some(svc) => svc.fact_store.ingest(fact),
            None => return Err("buddy service is not running".to_string()),
        }
    }
    handle_stalled_turn(app, &signal, &snippet, turn_index).await;
    Ok(())
}

fn last_assistant_text(session: &ChatSession) -> String {
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
async fn reserve_stalled_turn_slot(
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
async fn release_stalled_turn_slot(chat_id: &str, at: DateTime<Utc>) {
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
    settings.stalled_turn_intervention
}

/// The user has already written after the stalled turn: nudging the agent now
/// would talk over a human. No assistant message at all counts as "moved on".
pub fn user_spoke_after_last_assistant(messages: &[ChatMessage]) -> bool {
    let Some(last_assistant) = messages.iter().rposition(|message| message.role == "assistant")
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
        return (!question.is_empty())
            .then(|| BuddyReply::EscalateUser(cap_reply(question)));
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
    if let Some(plan) = prompt.plan.as_deref().map(str::trim).filter(|p| !p.is_empty()) {
        parts.push(format!(
            "Current plan:\n{}",
            safe_truncate(plan, MAX_PLAN_CHARS)
        ));
    }
    if let Some(goal) = prompt.goal.as_deref().map(str::trim).filter(|g| !g.is_empty()) {
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
    parts.push("Write the nudge now.".to_string());
    parts.join("\n\n")
}

fn reason_context_hint(reason_context: &str) -> &'static str {
    match reason_context {
        "goal_active" => "the chat has an active goal that is not finished",
        "card_doing" => "the agent's task card is still in the doing column",
        "plan_pending" => "the chat has a plan that is not finished",
        _ => "the turn ended without a tool call while work is still pending",
    }
}

fn collect_prompt_context(
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
        plan: crate::chat::plan_role::synthesize_current_plan(session),
        goal: session
            .goal
            .as_ref()
            .filter(|goal| goal.active)
            .map(|goal| goal.content.clone()),
        task_id: task_meta.map(|meta| meta.task_id.clone()),
        card_id: task_meta.and_then(|meta| meta.card_id.clone()),
        card_title: None,
        card_target_files: Vec::new(),
    }
}

/// Card title and target files are what make the nudge about *this* task rather
/// than about tidiness in general. Returns the card's column when the board was
/// readable. A board that cannot be read is not fatal.
async fn attach_card_context(app: &AppState, prompt: &mut StalledTurnPrompt) -> Option<String> {
    let (Some(task_id), Some(card_id)) = (prompt.task_id.clone(), prompt.card_id.clone()) else {
        return None;
    };
    match storage::load_board(app.gcx.clone(), &task_id).await {
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
    parse_buddy_reply(buddy_reply)
        .map(|reply| stalled_turn_delivery(chat_id, turn_index, &reply))
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

    let Some(session_arc) = app
        .chat
        .sessions
        .read()
        .await
        .get(&signal.chat_id)
        .cloned()
    else {
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
        !agent_ready_for_nudge(&session) || user_spoke_after_last_assistant(&session.messages)
    };
    if chat_moved_on {
        release_stalled_turn_slot(&signal.chat_id, reserved_at).await;
        tracing::info!(
            chat_id = %signal.chat_id,
            "stalled_turn: chat moved on while buddy was thinking, dropping the reply"
        );
        return Ok(());
    }

    let outcome = match deliver_to_chat(app, &signal.chat_id, delivery).await {
        Ok(outcome) => outcome,
        Err(error) => {
            release_stalled_turn_slot(&signal.chat_id, reserved_at).await;
            return Err(error);
        }
    };
    tracing::info!(chat_id = %signal.chat_id, ?outcome, "stalled_turn: buddy reply delivered");
    Ok(())
}

/// One-shot, tool-free call on the Buddy model slot. The user's buddy model is
/// the cheap one by definition (`chat_buddy_model`), which is exactly the
/// "weak little model" the intervention is supposed to use.
async fn ask_buddy_model(app: &AppState, prompt: &StalledTurnPrompt) -> Result<String, String> {
    let mut config = crate::subchat::resolve_subchat_config(
        app.gcx.clone(),
        "follow_up",
        false,
        Some(format!("buddy-stalled-turn-{}", Uuid::new_v4())),
        Some("Buddy Stalled Turn".to_string()),
        None,
        None,
        None,
        Some(vec![]),
        1,
        false,
        None,
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
    let model = crate::subchat::resolve_subchat_model(&app.chat_ctx(), &params)
        .await
        .map_err(|error| format!("buddy model: {error}"))?;

    config.tool_name = "buddy_stalled_turn".to_string();
    config.tools = crate::subchat::ToolsPolicy::None;
    config.max_steps = 1;
    config.model = model;
    config.max_new_tokens = MAX_NEW_TOKENS;
    config.temperature = Some(0.3);
    config.buddy_meta = Some(crate::buddy::types::BuddyThreadMeta {
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
    let result = crate::subchat::run_subchat(app.gcx.clone(), messages, config)
        .await
        .map_err(|error| format!("subchat: {error}"))?;
    result
        .messages
        .last()
        .filter(|message| message.role == "assistant")
        .map(|message| message.content.content_text_only())
        .ok_or_else(|| "buddy returned no assistant message".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::call_validation::{ChatContent, ChatMessage};
    use crate::chat::types::{ChatSession, GoalSnapshot};
    use refact_buddy_core::stalled_turn::NotStalledReason;

    fn visible(role: &str, text: &str) -> ChatMessage {
        ChatMessage {
            message_id: uuid::Uuid::new_v4().to_string(),
            role: role.to_string(),
            content: ChatContent::SimpleText(text.to_string()),
            ..Default::default()
        }
    }

    fn session_with_goal() -> ChatSession {
        let mut session = ChatSession::new("chat-1".to_string());
        session.goal = Some(GoalSnapshot {
            active: true,
            status: GoalStatus::Active,
            ..Default::default()
        });
        session
    }

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-05-15T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    fn budget() -> StalledTurnBudget {
        StalledTurnBudget {
            min_interval_secs: 300,
            max_per_hour: 6,
        }
    }

    #[test]
    fn tool_free_turn_with_active_goal_classifies_as_stalled() {
        let mut session = session_with_goal();
        session
            .messages
            .push(visible("assistant", "I think that is everything for now."));
        let ctx = collect_turn_context(&session, 0);
        assert!(matches!(
            classify_stalled_turn(&ctx),
            StalledTurnVerdict::Stalled(_)
        ));
    }

    #[test]
    fn turn_with_tool_calls_is_never_a_stall() {
        let mut session = session_with_goal();
        session.messages.push(ChatMessage {
            role: "assistant".to_string(),
            content: ChatContent::SimpleText("working".to_string()),
            tool_calls: Some(vec![crate::call_validation::ChatToolCall {
                id: "call-1".to_string(),
                function: crate::call_validation::ChatToolFunction {
                    name: "search_pattern".to_string(),
                    arguments: "{}".to_string(),
                },
                tool_type: "function".to_string(),
                index: None,
                extra_content: None,
                started_at_ms: None,
                completed_at_ms: None,
            }]),
            ..Default::default()
        });
        let ctx = collect_turn_context(&session, 1);
        assert_eq!(
            classify_stalled_turn(&ctx),
            StalledTurnVerdict::NotStalled(NotStalledReason::HasToolCalls)
        );
    }

    #[test]
    fn reasoning_only_turn_is_empty_message() {
        let mut session = session_with_goal();
        session.messages.push(ChatMessage {
            role: "assistant".to_string(),
            content: ChatContent::SimpleText("   ".to_string()),
            reasoning_content: Some("I pondered deeply".to_string()),
            ..Default::default()
        });
        let ctx = collect_turn_context(&session, 0);
        assert_eq!(
            classify_stalled_turn(&ctx),
            StalledTurnVerdict::NotStalled(NotStalledReason::EmptyMessage)
        );
    }

    #[test]
    fn completed_goal_is_not_active() {
        let mut session = session_with_goal();
        session.goal.as_mut().unwrap().status = GoalStatus::Completed;
        session.messages.push(visible("assistant", "done"));
        let ctx = collect_turn_context(&session, 0);
        assert_eq!(
            classify_stalled_turn(&ctx),
            StalledTurnVerdict::NotStalled(NotStalledReason::NoActiveObjective)
        );
    }

    #[test]
    fn tool_forbidden_mode_is_not_a_stall() {
        let mut session = session_with_goal();
        session.thread.mode = "chat".to_string();
        session
            .messages
            .push(visible("assistant", "Just a plain answer."));
        let ctx = collect_turn_context(&session, 0);
        assert_eq!(
            classify_stalled_turn(&ctx),
            StalledTurnVerdict::NotStalled(NotStalledReason::ToolsForbiddenMode)
        );
    }

    #[test]
    fn intervention_is_off_when_the_setting_is_off() {
        let mut settings = BuddySettings::default();
        assert!(intervention_enabled(&settings));
        settings.stalled_turn_intervention = false;
        assert!(!intervention_enabled(&settings));
    }

    #[test]
    fn budget_defaults_are_five_minutes_and_six_per_hour() {
        let from_settings = StalledTurnBudget::from_settings(&BuddySettings::default());
        assert_eq!(from_settings.min_interval_secs, 300);
        assert_eq!(from_settings.max_per_hour, 6);
    }

    #[test]
    fn first_nudge_is_always_allowed() {
        assert!(stalled_turn_allowed(&[], now(), budget()));
    }

    #[test]
    fn second_nudge_inside_the_interval_is_suppressed() {
        let history = vec![now() - Duration::seconds(120)];
        assert!(!stalled_turn_allowed(&history, now(), budget()));

        let later = now() + Duration::seconds(181);
        assert!(stalled_turn_allowed(&history, later, budget()));
    }

    #[test]
    fn hourly_cap_stops_a_chatty_buddy() {
        let history: Vec<DateTime<Utc>> = (0..6)
            .map(|index| now() - Duration::seconds(400 + index * 400))
            .collect();
        assert!(!stalled_turn_allowed(&history, now(), budget()));

        // Entries older than the rolling hour no longer count.
        let stale: Vec<DateTime<Utc>> = (0..6)
            .map(|index| now() - Duration::hours(2) - Duration::seconds(index * 400))
            .collect();
        assert!(stalled_turn_allowed(&stale, now(), budget()));
    }

    #[test]
    fn zero_budget_means_buddy_never_speaks() {
        let zero = StalledTurnBudget {
            min_interval_secs: 300,
            max_per_hour: 0,
        };
        assert!(!stalled_turn_allowed(&[], now(), zero));
    }

    #[test]
    fn user_reply_after_the_stalled_turn_suppresses_the_nudge() {
        let messages = vec![
            visible("user", "please fix the tests"),
            visible("assistant", "All good, I think we are done."),
        ];
        assert!(!user_spoke_after_last_assistant(&messages));

        let mut with_user = messages.clone();
        with_user.push(visible("user", "no, keep going"));
        assert!(user_spoke_after_last_assistant(&with_user));
    }

    #[test]
    fn missing_assistant_message_counts_as_moved_on() {
        assert!(user_spoke_after_last_assistant(&[visible("user", "hi")]));
        assert!(user_spoke_after_last_assistant(&[]));
    }

    #[test]
    fn agent_must_be_between_turns_to_be_nudged() {
        let mut session = session_with_goal();
        session.set_runtime_state(SessionState::Idle, None);
        assert!(agent_ready_for_nudge(&session));

        session.set_runtime_state(SessionState::Generating, None);
        assert!(!agent_ready_for_nudge(&session));

        session.set_runtime_state(SessionState::Idle, None);
        session.turn_depth = 1;
        assert!(!agent_ready_for_nudge(&session));

        session.turn_depth = 0;
        session.draft_message = Some(visible("assistant", "half an answer"));
        assert!(!agent_ready_for_nudge(&session));

        session.draft_message = None;
        session.closed = true;
        assert!(!agent_ready_for_nudge(&session));
    }

    #[test]
    fn plain_reply_becomes_a_nudge() {
        assert_eq!(
            parse_buddy_reply("  Call `update_textdoc` and finish the card.  "),
            Some(BuddyReply::Nudge(
                "Call `update_textdoc` and finish the card.".to_string()
            ))
        );
    }

    #[test]
    fn escalate_marker_is_stripped_from_the_question() {
        assert_eq!(
            parse_buddy_reply("ESCALATE_USER: Which API key should the agent use?"),
            Some(BuddyReply::EscalateUser(
                "Which API key should the agent use?".to_string()
            ))
        );
        assert_eq!(
            parse_buddy_reply("ESCALATE_USER:   "),
            None,
            "an empty escalation is not a question"
        );
    }

    #[test]
    fn empty_reply_is_dropped_and_long_replies_are_capped() {
        assert_eq!(parse_buddy_reply("   \n "), None);

        let long = "x".repeat(MAX_REPLY_CHARS + 200);
        match parse_buddy_reply(&long) {
            Some(BuddyReply::Nudge(text)) => assert_eq!(text.chars().count(), MAX_REPLY_CHARS),
            other => panic!("expected a capped nudge, got {other:?}"),
        }
    }

    #[test]
    fn prompt_carries_the_turn_the_plan_the_goal_and_the_card() {
        let prompt = StalledTurnPrompt {
            chat_id: "chat-1".to_string(),
            mode: "task_agent".to_string(),
            reason_context: "card_doing".to_string(),
            snippet: "I think we are done here, right?".to_string(),
            plan: Some("1. Implement\n2. Test".to_string()),
            goal: Some("Ship the sandbox layer".to_string()),
            task_id: Some("task-1".to_string()),
            card_id: Some("S-3".to_string()),
            card_title: Some("Buddy intervention".to_string()),
            card_target_files: vec!["src/buddy/interventions/stalled_turn.rs".to_string()],
        };

        let text = build_buddy_user_prompt(&prompt);
        assert!(text.contains("chat-1"), "{text}");
        assert!(text.contains("task_agent"), "{text}");
        assert!(text.contains("doing column"), "{text}");
        assert!(text.contains("I think we are done here, right?"), "{text}");
        assert!(text.contains("1. Implement"), "{text}");
        assert!(text.contains("Ship the sandbox layer"), "{text}");
        assert!(text.contains("S-3 - Buddy intervention"), "{text}");
        assert!(text.contains("src/buddy/interventions/stalled_turn.rs"), "{text}");
        assert!(text.contains("Write the nudge now."), "{text}");
    }

    #[test]
    fn prompt_skips_absent_sections() {
        let prompt = StalledTurnPrompt {
            chat_id: "chat-1".to_string(),
            reason_context: "goal_active".to_string(),
            snippet: "nothing to do".to_string(),
            ..Default::default()
        };
        let text = build_buddy_user_prompt(&prompt);
        assert!(!text.contains("Current plan"), "{text}");
        assert!(!text.contains("Current goal"), "{text}");
        assert!(!text.contains("Card:"), "{text}");
        assert!(text.contains("active goal"), "{text}");
    }

    #[test]
    fn nudge_is_a_runner_notice_that_wakes_the_agent() {
        let delivery = stalled_turn_delivery(
            "chat-1",
            4,
            &BuddyReply::Nudge("Go finish the card.".to_string()),
        );

        assert_eq!(delivery.id, "stalled-turn:chat-1:4");
        assert_eq!(delivery.push, PushMode::Append);
        assert_eq!(delivery.source, STALLED_TURN_SOURCE);
        assert!(delivery.wake, "a stalled agent must actually resume");
        assert_eq!(delivery.messages.len(), 1);
        let message = &delivery.messages[0];
        assert_eq!(message.role, "event", "the user must not look like the author");
        assert_eq!(message.content.content_text_only(), "Go finish the card.");
        assert_eq!(message.extra["event"]["subkind"], json!("system_notice"));
        assert_eq!(message.extra["event"]["source"], json!(STALLED_TURN_SOURCE));
        assert_eq!(
            message.extra["event"]["payload"]["kind"],
            json!("stalled_turn_nudge")
        );
        assert_eq!(
            message.extra["event"]["payload"]["turn_index"],
            json!(4)
        );
    }

    #[test]
    fn escalation_goes_to_the_human_without_starting_a_turn() {
        let delivery = stalled_turn_delivery(
            "chat-1",
            4,
            &BuddyReply::EscalateUser("Which API key should I use?".to_string()),
        );

        assert_eq!(delivery.id, "stalled-turn-escalation:chat-1:4");
        assert_eq!(delivery.push, PushMode::WhenIdle);
        assert!(!delivery.wake, "the agent must not be restarted by an escalation");
        assert_eq!(delivery.messages[0].role, "event");
        assert_eq!(
            delivery.messages[0].extra["event"]["payload"]["kind"],
            json!("stalled_turn_escalation")
        );
    }

    #[test]
    fn consecutive_stalls_in_one_chat_get_distinct_delivery_ids() {
        let reply = BuddyReply::Nudge("keep going".to_string());
        assert_ne!(
            stalled_turn_delivery("chat-1", 3, &reply).id,
            stalled_turn_delivery("chat-1", 4, &reply).id
        );
        assert_ne!(
            stalled_turn_delivery("chat-1", 3, &reply).id,
            stalled_turn_delivery("chat-2", 3, &reply).id
        );
    }

    #[test]
    fn raw_buddy_reply_becomes_the_right_kind_of_delivery() {
        let nudge = intervention_delivery("chat-1", 2, "Stop idling and call `update_textdoc`.")
            .expect("a plain reply is a nudge");
        assert_eq!(nudge.push, PushMode::Append);
        assert!(nudge.wake);
        assert_eq!(
            nudge.messages[0].content.content_text_only(),
            "Stop idling and call `update_textdoc`."
        );

        let escalation =
            intervention_delivery("chat-1", 2, "ESCALATE_USER: Which API key is it?")
                .expect("an escalation marker is a question");
        assert_eq!(escalation.push, PushMode::WhenIdle);
        assert!(!escalation.wake);
        assert_eq!(
            escalation.messages[0].content.content_text_only(),
            "Which API key is it?"
        );

        assert!(intervention_delivery("chat-1", 2, "   ").is_none());
    }

    #[test]
    fn prompt_context_reads_the_session_not_the_fact_payload() {
        let mut session = session_with_goal();
        session.goal.as_mut().unwrap().content = "Finish the card".to_string();
        session.thread.mode = "task_agent".to_string();
        session.thread.task_meta = Some(refact_chat_api::TaskMeta {
            task_id: "task-1".to_string(),
            role: "agents".to_string(),
            agent_id: Some("agent-1".to_string()),
            card_id: Some("S-3".to_string()),
            planner_chat_id: None,
        });
        let signal = StalledTurnSignal {
            chat_id: "chat-1".to_string(),
            reason_context: "card_doing",
        };

        let prompt = collect_prompt_context(&session, &signal, "all done");

        assert_eq!(prompt.chat_id, "chat-1");
        assert_eq!(prompt.mode, "task_agent");
        assert_eq!(prompt.snippet, "all done");
        assert_eq!(prompt.goal.as_deref(), Some("Finish the card"));
        assert_eq!(prompt.task_id.as_deref(), Some("task-1"));
        assert_eq!(prompt.card_id.as_deref(), Some("S-3"));
        assert_eq!(prompt.card_title, None, "title comes from the board, not the thread");
    }

    fn assistant_with_tool_call(name: &str) -> ChatMessage {
        ChatMessage {
            role: "assistant".to_string(),
            content: ChatContent::SimpleText(String::new()),
            tool_calls: Some(vec![crate::call_validation::ChatToolCall {
                id: "call-1".to_string(),
                function: crate::call_validation::ChatToolFunction {
                    name: name.to_string(),
                    arguments: "{}".to_string(),
                },
                tool_type: "function".to_string(),
                index: None,
                extra_content: None,
                started_at_ms: None,
                completed_at_ms: None,
            }]),
            ..Default::default()
        }
    }

    fn stalled_nudge(text: &str) -> ChatMessage {
        event(
            EventSubkind::SystemNotice,
            STALLED_TURN_SOURCE,
            json!({"kind": "stalled_turn_nudge"}),
            text,
        )
    }

    #[test]
    fn plan_without_checkboxes_is_treated_as_unfinished() {
        assert!(plan_has_unfinished_items("1. Implement\n2. Test"));
        assert!(plan_has_unfinished_items("- implement the thing"));
    }

    #[test]
    fn empty_plan_has_nothing_pending() {
        assert!(!plan_has_unfinished_items(""));
        assert!(!plan_has_unfinished_items("   \n  "));
    }

    #[test]
    fn fully_checked_plan_has_nothing_pending() {
        assert!(!plan_has_unfinished_items(
            "- [x] wire the hook\n- [X] write tests"
        ));
    }

    #[test]
    fn partly_checked_plan_is_unfinished() {
        assert!(plan_has_unfinished_items(
            "- [x] wire the hook\n- [ ] write tests"
        ));
    }

    #[test]
    fn mixed_bullet_markers_all_count_as_checkboxes() {
        assert!(!plan_has_unfinished_items(
            "* [x] one\n+ [x] two\n1. [x] three"
        ));
        assert!(plan_has_unfinished_items(
            "* [x] one\n+ [ ] two\n1. [x] three"
        ));
    }

    #[test]
    fn no_progress_is_false_without_a_previous_nudge() {
        let messages = vec![
            visible("user", "go"),
            visible("assistant", "I think we are done."),
        ];
        assert!(!no_progress_since_last_nudge(&messages));
    }

    #[test]
    fn tool_call_after_the_nudge_is_progress() {
        let messages = vec![
            stalled_nudge("keep going"),
            assistant_with_tool_call("update_textdoc"),
        ];
        assert!(!no_progress_since_last_nudge(&messages));
    }

    #[test]
    fn plain_answer_after_the_nudge_is_no_progress() {
        let messages = vec![
            stalled_nudge("keep going"),
            visible("assistant", "I think we are done."),
        ];
        assert!(no_progress_since_last_nudge(&messages));
    }

    #[test]
    fn user_message_after_the_nudge_is_no_progress() {
        let messages = vec![
            stalled_nudge("keep going"),
            visible("user", "actually, stop"),
        ];
        assert!(no_progress_since_last_nudge(&messages));
    }

    #[test]
    fn tool_call_before_the_nudge_does_not_count_as_progress() {
        let messages = vec![
            assistant_with_tool_call("search_pattern"),
            stalled_nudge("keep going"),
            visible("assistant", "done"),
        ];
        assert!(no_progress_since_last_nudge(&messages));
    }

    #[test]
    fn trailing_event_message_does_not_hide_the_assistant_reply() {
        let mut session = session_with_goal();
        session.messages.push(visible("assistant", "готово"));
        session.messages.push(event(
            EventSubkind::ProcessCompleted,
            "process",
            json!({}),
            "done",
        ));

        let ctx = collect_turn_context(&session, 0);
        assert!(ctx.has_content);
        assert_eq!(last_assistant_text(&session), "готово");
        assert!(matches!(
            classify_stalled_turn(&ctx),
            StalledTurnVerdict::Stalled(_)
        ));
    }

    #[test]
    fn finishing_tool_call_in_the_turn_means_the_agent_is_done() {
        let messages = vec![
            visible("user", "finish the card"),
            assistant_with_tool_call(AGENT_FINISH_TOOL),
            visible("assistant", "All done."),
        ];
        assert!(agent_already_finished(&messages));
    }

    #[test]
    fn ordinary_tool_calls_do_not_mean_the_agent_is_done() {
        let messages = vec![
            visible("user", "keep going"),
            assistant_with_tool_call("update_textdoc"),
            visible("assistant", "Still working."),
        ];
        assert!(!agent_already_finished(&messages));
        assert!(!agent_already_finished(&[]));
    }

    #[test]
    fn only_a_doing_card_keeps_the_intervention_alive() {
        assert!(card_work_is_active(Some(CARD_DOING_COLUMN)));
        assert!(!card_work_is_active(Some("done")));
        assert!(!card_work_is_active(Some("failed")));
        assert!(
            card_work_is_active(None),
            "an unreadable board must not silence the nudge"
        );
    }

    #[test]
    fn silent_tail_nudge_leaves_nothing_for_the_hook_to_do() {
        assert!(!stalled_turn_hook_applies(true, true));
        assert!(stalled_turn_hook_applies(false, true));
        assert!(!stalled_turn_hook_applies(false, false));
    }

    #[tokio::test]
    async fn released_slot_does_not_block_the_next_nudge() {
        let chat_id = format!("chat-release-{}", Uuid::new_v4());
        let at = Utc::now();

        assert!(reserve_stalled_turn_slot(&chat_id, at, budget()).await);
        assert!(
            !reserve_stalled_turn_slot(&chat_id, at, budget()).await,
            "the reserved slot must hold the cooldown"
        );

        release_stalled_turn_slot(&chat_id, at).await;
        assert!(
            reserve_stalled_turn_slot(&chat_id, at + Duration::seconds(1), budget()).await,
            "a failed intervention must not burn the slot"
        );
    }
}
