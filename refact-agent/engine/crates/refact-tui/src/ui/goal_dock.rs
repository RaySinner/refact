use std::env;
#[cfg(test)]
use std::sync::{Mutex, MutexGuard, OnceLock};

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;
use serde_json::Value;

use crate::app::App;
use crate::client::GoalControlAction;
use crate::history::cells::GoalCellData;
use crate::protocol::{RuntimeGoalSnapshot, RuntimeUpdatedEvent, TranscriptMessage, TranscriptRole};
use crate::text_formatting::format_tokens_compact;
use crate::theme::ThemeRole;
use crate::vendored::line_truncation::truncate_line_with_ellipsis_if_overflow;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct GoalBudget {
    max_turns: Option<u64>,
    max_minutes: Option<u64>,
    max_tokens: Option<u64>,
    max_cost_cents: Option<u64>,
    no_progress_turns: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct GoalProgress {
    turns_used: u64,
    tokens_used: u64,
    no_progress_turns: u64,
    cost_used_cents: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GoalPresentation {
    content: String,
    status: String,
    active: bool,
    version: u64,
    resolution: Option<String>,
    execution: Option<String>,
    autonomy: Option<String>,
    owner: Option<String>,
    revision: Option<u64>,
    stop_reason: Option<String>,
    snoozed_until_ms: Option<u64>,
    quiescent: bool,
    budget: GoalBudget,
    progress: GoalProgress,
    latest_verdict: Option<String>,
    gaps: Vec<String>,
    events: Vec<String>,
    transferred_from: Option<String>,
    transferred_to: Option<String>,
}

impl GoalPresentation {
    pub(crate) fn from_messages(
        messages: &[TranscriptMessage],
        runtime: Option<&RuntimeUpdatedEvent>,
    ) -> Option<Self> {
        let goal = messages
            .iter()
            .filter(|message| message.role == TranscriptRole::Goal)
            .max_by_key(|message| goal_version(message))?;
        let metadata = goal.extra.get("goal").unwrap_or(&Value::Null);
        let mut presentation = Self {
            content: goal.content.clone(),
            status: string_field(metadata, "status").unwrap_or_else(|| "active".to_string()),
            active: bool_field(metadata, "active").unwrap_or(true),
            version: goal_version(goal).max(1),
            resolution: string_field(metadata, "resolution"),
            execution: string_field(metadata, "execution"),
            autonomy: string_field(metadata, "autonomy"),
            owner: string_field(metadata, "owner"),
            revision: metadata.get("revision").and_then(Value::as_u64),
            stop_reason: string_field(metadata, "stop_reason"),
            snoozed_until_ms: metadata.get("snoozed_until_ms").and_then(Value::as_u64),
            quiescent: metadata
                .get("events")
                .and_then(Value::as_array)
                .and_then(|events| events.last())
                .and_then(|event| string_field(event, "kind"))
                .is_some_and(|kind| matches!(kind.as_str(), "pursuit_quiescent" | "quiescent")),
            budget: GoalBudget::from_value(metadata.get("budget")),
            progress: GoalProgress::from_value(metadata.get("progress")),
            latest_verdict: None,
            gaps: Vec::new(),
            events: goal_events(metadata.get("events")),
            transferred_from: string_field(metadata, "transferred_from"),
            transferred_to: string_field(metadata, "transferred_to"),
        };
        presentation.apply_attempts(metadata.get("attempts"));
        presentation.apply_pursuit_events(messages);
        if let Some(runtime) = runtime {
            presentation.apply_runtime(&runtime.goal);
        }
        Some(presentation)
    }

    pub(crate) fn to_goal_cell_data(&self, data: GoalCellData) -> GoalCellData {
        data.with_details(self.detail_lines())
    }

    pub(crate) fn dock_line(&self) -> String {
        let mut sections = vec![
            format!("Goal {}", self.effective_execution_label()),
            turn_counter(self.progress.turns_used, self.budget.max_turns),
            token_counter(self.progress.tokens_used, self.budget.max_tokens),
            format!(
                "verdict: {}",
                self.latest_verdict
                    .as_deref()
                    .map(verdict_label)
                    .unwrap_or("PENDING")
            ),
        ];
        if let Some(event) = self.events.last() {
            sections.push(event.clone());
        }
        sections.extend(self.controls());
        sections.push("/goal".to_string());
        sections.join(" · ")
    }

    pub(crate) fn detail_lines(&self) -> Vec<String> {
        let mut lines = vec![
            format!("Goal: {}", self.content),
            format!("Status: {}", status_label(&self.status)),
            format!("Resolution: {}", self.resolution_label()),
            format!("Execution: {}", self.effective_execution_label()),
            format!(
                "Pursuit: {}",
                if self.active { "owned" } else { "not owned" }
            ),
            format!("Version: v{}", self.version),
            format!(
                "Progress: {} · {} · {}",
                turn_counter(self.progress.turns_used, self.budget.max_turns),
                token_counter(self.progress.tokens_used, self.budget.max_tokens),
                no_progress_counter(
                    self.progress.no_progress_turns,
                    self.budget.no_progress_turns
                )
            ),
            "Budget:".to_string(),
            format!("  turns: {}", limit_label(self.budget.max_turns, "turns")),
            format!("  minutes: {}", limit_label(self.budget.max_minutes, "min")),
            format!("  tokens: {}", token_limit_label(self.budget.max_tokens)),
            format!("  cost: {}", cost_limit_label(self.budget.max_cost_cents)),
            format!(
                "  no progress: {}",
                limit_label(self.budget.no_progress_turns, "turns")
            ),
            format!(
                "Latest verifier: {}",
                self.latest_verdict
                    .as_deref()
                    .map(verdict_label)
                    .unwrap_or("PENDING")
            ),
        ];
        if let Some(owner) = self.owner.as_deref() {
            lines.push(format!(
                "Owner: {} · Revision: {}",
                owner_label(owner),
                self.revision.unwrap_or(1)
            ));
        }
        if let Some(autonomy) = self.autonomy.as_deref() {
            lines.push(format!("Autonomy: {}", autonomy_label(autonomy)));
        }
        if let Some(reason) = self.stop_reason.as_deref() {
            lines.push(format!("Stop reason: {reason}"));
        }
        if let Some(until) = self.snoozed_until_ms {
            lines.push(format!("Snoozed until (epoch ms): {until}"));
        }
        if self.quiescent {
            lines.push("Pursuit is quiescent; it resumes on your next message".to_string());
        }
        if self.gaps.is_empty() {
            lines.push("Verifier gaps: none reported".to_string());
        } else {
            lines.push("Verifier gaps:".to_string());
            lines.extend(self.gaps.iter().map(|gap| format!("  - {gap}")));
        }
        if let Some(source) = self.transferred_from.as_deref() {
            lines.push(format!("Transferred from: {source}"));
        }
        if let Some(target) = self.transferred_to.as_deref() {
            lines.push(format!("Transferred to: {target}"));
        }
        if self.events.is_empty() {
            lines.push("Recent pursuit: none".to_string());
        } else {
            lines.push("Recent pursuit:".to_string());
            lines.extend(self.events.iter().map(|event| format!("  - {event}")));
        }
        let controls = self.controls();
        lines.push(if controls.is_empty() {
            "Controls: unavailable".to_string()
        } else {
            format!("Controls: {}", controls.join(" · "))
        });
        lines
    }

    pub(crate) fn allows(&self, action: GoalControlAction) -> bool {
        if self.has_modern_projection() {
            let owned = self.active && self.transferred_to.is_none();
            let active_resolution = self.resolution.as_deref() == Some("active");
            return match action {
                GoalControlAction::Pause => {
                    owned && active_resolution && self.execution.as_deref() != Some("paused")
                }
                GoalControlAction::Resume => {
                    owned && active_resolution && self.execution.as_deref() == Some("paused")
                }
                GoalControlAction::Stop | GoalControlAction::Cancel => owned && active_resolution,
                GoalControlAction::Clear => owned,
            };
        }
        match action {
            GoalControlAction::Pause | GoalControlAction::Stop => {
                self.active && self.status == "active"
            }
            GoalControlAction::Resume => matches!(self.status.as_str(), "paused" | "stopped"),
            GoalControlAction::Cancel | GoalControlAction::Clear => false,
        }
    }

    pub(crate) fn allows_replace(&self) -> bool {
        self.has_modern_projection() && self.active && self.transferred_to.is_none()
    }

    pub(crate) fn allows_autonomy(&self) -> bool {
        self.allows_replace() && self.resolution.as_deref() == Some("active")
    }

    fn has_modern_projection(&self) -> bool {
        self.resolution.is_some() && self.execution.is_some() && self.autonomy.is_some()
    }

    fn resolution_label(&self) -> &'static str {
        match self.resolution.as_deref() {
            Some("achieved") => "ACHIEVED",
            Some("failed") => "FAILED",
            Some("cancelled") => "CANCELLED",
            Some("active") => "ACTIVE",
            _ => "LEGACY",
        }
    }

    fn effective_execution_label(&self) -> String {
        if self.has_modern_projection() && self.resolution.as_deref() != Some("active") {
            return self.resolution_label().to_string();
        }
        if self.snoozed_until_ms.is_some() {
            return "SNOOZED".to_string();
        }
        if self.quiescent {
            return "QUIESCENT".to_string();
        }
        if self.status == "budget_exhausted" {
            return "PAUSED / BUDGET EXHAUSTED".to_string();
        }
        if self.status == "no_progress" {
            return "PAUSED / NO PROGRESS".to_string();
        }
        let execution = self
            .execution
            .as_deref()
            .unwrap_or_else(|| match self.status.as_str() {
                "verifying" => "verifying",
                "paused" | "budget_exhausted" | "no_progress" | "transferred" => "paused",
                _ => "idle",
            });
        if execution == "idle" && self.autonomy.as_deref() == Some("off") {
            return "IDLE / AUTONOMY OFF".to_string();
        }
        execution_label(execution).to_string()
    }

    fn controls(&self) -> Vec<String> {
        let mut controls = Vec::new();
        if self.allows(GoalControlAction::Pause) {
            controls.push("Pause (p)".to_string());
        }
        if self.allows(GoalControlAction::Resume) {
            controls.push("Resume (r)".to_string());
        }
        if self.allows(GoalControlAction::Stop) {
            controls.push("Stop (s)".to_string());
        }
        if self.allows(GoalControlAction::Cancel) {
            controls.push("Cancel (/goal cancel)".to_string());
        }
        if self.allows(GoalControlAction::Clear) {
            controls.push("Clear (/goal clear)".to_string());
        }
        if self.allows_replace() {
            controls.push("Replace (/goal replace <text>)".to_string());
        }
        if self.allows_autonomy() {
            controls.push("Autonomy (/goal autonomy on|off)".to_string());
        }
        controls
    }

    fn apply_runtime(&mut self, runtime: &RuntimeGoalSnapshot) {
        if let Some(active) = runtime.active {
            self.active = active;
        }
        if let Some(status) = runtime.status.as_deref() {
            self.status = status.to_string();
        }
        merge_string(&mut self.resolution, &runtime.resolution);
        merge_string(&mut self.execution, &runtime.execution);
        merge_string(&mut self.autonomy, &runtime.autonomy);
        merge_string(&mut self.owner, &runtime.owner);
        merge_string(&mut self.stop_reason, &runtime.stop_reason);
        if runtime.revision.is_some() {
            self.revision = runtime.revision;
        }
        if runtime.snoozed_until_ms.is_some() {
            self.snoozed_until_ms = runtime.snoozed_until_ms;
        }
        if let Some(turns) = runtime.turn_count {
            self.progress.turns_used = turns;
        }
        if let Some(tokens) = runtime.token_count {
            self.progress.tokens_used = tokens;
        }
        if let Some(no_progress) = runtime.no_progress_turn_count {
            self.progress.no_progress_turns = no_progress;
        }
        merge_limit(&mut self.budget.max_turns, runtime.budget.max_turns);
        merge_limit(&mut self.budget.max_minutes, runtime.budget.max_minutes);
        merge_limit(&mut self.budget.max_tokens, runtime.budget.max_tokens);
        merge_limit(
            &mut self.budget.max_cost_cents,
            runtime.budget.max_cost_cents,
        );
        merge_limit(
            &mut self.budget.no_progress_turns,
            runtime.budget.no_progress_turns,
        );
    }

    fn apply_attempts(&mut self, attempts: Option<&Value>) {
        let Some(attempt) = attempts
            .and_then(Value::as_array)
            .and_then(|attempts| attempts.last())
        else {
            return;
        };
        self.latest_verdict = string_field(attempt, "verdict");
        self.gaps = string_array(attempt.get("gaps"));
    }

    fn apply_pursuit_events(&mut self, messages: &[TranscriptMessage]) {
        for message in messages {
            if event_subkind(message) != Some("goal_pursuit") {
                continue;
            }
            let payload = message
                .extra
                .get("event")
                .and_then(|event| event.get("payload"))
                .unwrap_or(&Value::Null);
            let kind = string_field(payload, "kind").unwrap_or_else(|| "updated".to_string());
            self.quiescent = matches!(kind.as_str(), "pursuit_quiescent" | "quiescent");
            let label = pursuit_label(&kind);
            push_unique(&mut self.events, label);
            if matches!(kind.as_str(), "verified" | "verification_gaps") {
                self.latest_verdict = Some(if kind == "verified" {
                    "met".to_string()
                } else {
                    "unmet".to_string()
                });
            }
            let gaps = string_array(payload.get("gaps"));
            if !gaps.is_empty() {
                self.gaps = gaps;
            }
        }
        if self.events.len() > 5 {
            let keep_from = self.events.len() - 5;
            self.events.drain(..keep_from);
        }
    }
}

impl GoalBudget {
    fn from_value(value: Option<&Value>) -> Self {
        let value = value.unwrap_or(&Value::Null);
        Self {
            max_turns: positive_field(value, "max_turns"),
            max_minutes: positive_field(value, "max_minutes"),
            max_tokens: positive_field(value, "max_tokens"),
            max_cost_cents: positive_field(value, "max_cost_cents"),
            no_progress_turns: positive_field(value, "no_progress_turns"),
        }
    }
}

impl GoalProgress {
    fn from_value(value: Option<&Value>) -> Self {
        let value = value.unwrap_or(&Value::Null);
        Self {
            turns_used: positive_or_zero_field(value, "turns_used"),
            tokens_used: positive_or_zero_field(value, "tokens_used"),
            no_progress_turns: positive_or_zero_field(value, "no_progress_turns"),
            cost_used_cents: positive_or_zero_field(value, "cost_used_cents"),
        }
    }
}

pub(crate) fn surfaces_enabled() -> bool {
    env::var("REFACT_TUI_SURFACES").ok().is_some_and(|value| {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        )
    })
}

#[cfg(test)]
pub(crate) fn test_surface_lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

pub(crate) fn height(app: &App) -> u16 {
    if app.goal_surfaces_enabled() && app.goal_presentation().is_some() {
        1
    } else {
        0
    }
}

pub(crate) fn render(frame: &mut Frame<'_>, app: &App, area: Rect) {
    if area.is_empty() {
        return;
    }
    let Some(goal) = app.goal_presentation() else {
        return;
    };
    let line = truncate_line_with_ellipsis_if_overflow(
        Line::from(Span::styled(
            goal.dock_line(),
            Style::default().add_modifier(Modifier::BOLD),
        )),
        area.width as usize,
    );
    frame.render_widget(Paragraph::new(line), area);
}

pub(crate) fn render_overlay(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let Some(goal) = app.goal_presentation() else {
        return;
    };
    let width = area.width.saturating_sub(6).min(96).max(1);
    let height = area.height.saturating_sub(4).max(1);
    let popup = Rect {
        x: area.x.saturating_add(area.width.saturating_sub(width) / 2),
        y: area
            .y
            .saturating_add(area.height.saturating_sub(height) / 2),
        width,
        height,
    };
    frame.render_widget(Clear, popup);
    let lines = goal
        .detail_lines()
        .into_iter()
        .map(Line::from)
        .collect::<Vec<_>>();
    let block = Block::default()
        .title(" Goal ")
        .borders(Borders::ALL)
        .border_style(app.theme().style(ThemeRole::Muted));
    frame.render_widget(
        Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false }),
        popup,
    );
}

fn goal_version(message: &TranscriptMessage) -> u64 {
    message
        .extra
        .get("goal")
        .and_then(|goal| goal.get("version"))
        .and_then(Value::as_u64)
        .unwrap_or(0)
}

fn event_subkind(message: &TranscriptMessage) -> Option<&str> {
    message
        .extra
        .get("event")
        .and_then(|event| event.get("subkind"))
        .and_then(Value::as_str)
}

fn string_field(value: &Value, field: &str) -> Option<String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn bool_field(value: &Value, field: &str) -> Option<bool> {
    value.get(field).and_then(Value::as_bool)
}

fn positive_field(value: &Value, field: &str) -> Option<u64> {
    value
        .get(field)
        .and_then(Value::as_u64)
        .filter(|value| *value > 0)
}

fn positive_or_zero_field(value: &Value, field: &str) -> u64 {
    value.get(field).and_then(Value::as_u64).unwrap_or(0)
}

fn string_array(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn goal_events(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|events| {
            events
                .iter()
                .filter_map(|event| {
                    string_field(event, "text")
                        .or_else(|| string_field(event, "kind").map(|kind| pursuit_label(&kind)))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn push_unique(values: &mut Vec<String>, value: String) {
    if !values.iter().any(|existing| existing == &value) {
        values.push(value);
    }
}

fn merge_limit(target: &mut Option<u64>, value: Option<u64>) {
    if value.is_some() {
        *target = value;
    }
}

fn merge_string(target: &mut Option<String>, value: &Option<String>) {
    if let Some(value) = value {
        *target = Some(value.clone());
    }
}

fn status_label(status: &str) -> &'static str {
    match status {
        "active" => "ACTIVE",
        "verifying" => "VERIFYING",
        "paused" => "PAUSED",
        "completed" => "COMPLETED",
        "stopped" => "STOPPED",
        "budget_exhausted" => "BUDGET EXHAUSTED",
        "no_progress" => "NO PROGRESS",
        "transferred" => "TRANSFERRED",
        _ => "UNKNOWN",
    }
}

fn verdict_label(verdict: &str) -> &'static str {
    match verdict.trim().to_ascii_lowercase().as_str() {
        "met" | "completed" | "verified" => "MET",
        "unmet" | "needs_work" | "gaps" | "verification_gaps" => "UNMET",
        "impossible" => "IMPOSSIBLE",
        "inconclusive" => "INCONCLUSIVE",
        "blocked" | "verification_blocked" => "BLOCKED",
        _ => "PENDING",
    }
}

fn execution_label(execution: &str) -> &'static str {
    match execution {
        "idle" => "IDLE",
        "running" => "RUNNING",
        "verifying" => "VERIFYING",
        "waiting_input" => "WAITING FOR INPUT",
        "waiting_approval" => "WAITING FOR APPROVAL",
        "paused" => "PAUSED",
        "retry_scheduled" => "RETRY SCHEDULED",
        _ => "UNKNOWN",
    }
}

fn owner_label(owner: &str) -> &'static str {
    match owner {
        "delegate" => "DELEGATE",
        _ => "USER",
    }
}

fn autonomy_label(autonomy: &str) -> &'static str {
    match autonomy {
        "off" => "OFF",
        _ => "ON",
    }
}

fn pursuit_label(kind: &str) -> String {
    match kind {
        "nudge" => "Pursuit nudge".to_string(),
        "verified" => "Goal verified".to_string(),
        "verification_gaps" => "Verifier reported gaps".to_string(),
        "verification_blocked" => "Verification blocked".to_string(),
        "pursuit_quiescent" | "quiescent" => "Pursuit quiescent".to_string(),
        "budget_exhausted" => "Budget exhausted".to_string(),
        "no_progress" => "No progress limit reached".to_string(),
        "stopped" => "Goal stopped".to_string(),
        "paused" => "Goal paused".to_string(),
        "resumed" => "Goal resumed".to_string(),
        "transfer" => "Goal transferred".to_string(),
        _ => "Goal pursuit updated".to_string(),
    }
}

fn turn_counter(used: u64, limit: Option<u64>) -> String {
    match limit {
        Some(limit) => format!("{used}/{limit} turns"),
        None => format!("{used} turns"),
    }
}

fn token_counter(used: u64, limit: Option<u64>) -> String {
    match limit {
        Some(limit) => format!(
            "{}/{} tok",
            format_tokens_compact(used),
            format_tokens_compact(limit)
        ),
        None => format!("{} tok", format_tokens_compact(used)),
    }
}

fn no_progress_counter(used: u64, limit: Option<u64>) -> String {
    match limit {
        Some(limit) => format!("{used}/{limit} no-progress"),
        None => format!("{used} no-progress"),
    }
}

fn limit_label(limit: Option<u64>, unit: &str) -> String {
    limit
        .map(|limit| format!("{limit} {unit}"))
        .unwrap_or_else(|| "unlimited".to_string())
}

fn token_limit_label(limit: Option<u64>) -> String {
    limit
        .map(|limit| format!("{} tok", format_tokens_compact(limit)))
        .unwrap_or_else(|| "unlimited".to_string())
}

fn cost_limit_label(limit: Option<u64>) -> String {
    limit
        .map(|limit| format!("{limit}¢"))
        .unwrap_or_else(|| "unlimited".to_string())
}

pub(crate) fn budget_from_inputs(
    max_turns: &str,
    max_minutes: &str,
    max_tokens: &str,
    max_cost_cents: &str,
    no_progress_turns: &str,
) -> Result<crate::client::GoalBudget, String> {
    Ok(crate::client::GoalBudget {
        max_turns: parse_optional_limit(max_turns, "turn limit")?,
        max_minutes: parse_optional_limit(max_minutes, "minute limit")?,
        max_tokens: parse_optional_limit(max_tokens, "token limit")?,
        max_cost_cents: parse_optional_limit(max_cost_cents, "cost limit")?,
        cooldown_ms: None,
        no_progress_token_threshold: None,
        no_progress_turns: parse_optional_limit(no_progress_turns, "no-progress limit")?,
    })
}

fn parse_optional_limit<T>(input: &str, label: &str) -> Result<Option<T>, String>
where
    T: std::str::FromStr + PartialEq + Default,
{
    let input = input.trim();
    if input.is_empty() {
        return Ok(None);
    }
    let value = input
        .parse::<T>()
        .map_err(|_| format!("{label} must be a non-negative number"))?;
    Ok((value != T::default()).then_some(value))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn messages(goal: Value, events: Vec<Value>) -> Vec<TranscriptMessage> {
        let mut messages = vec![TranscriptMessage::from_wire(&json!({
            "role": "goal",
            "content": "Ship goal controls",
            "extra": {"goal": goal}
        }))];
        messages.extend(events.iter().map(TranscriptMessage::from_wire));
        messages
    }

    fn presentation(goal: Value) -> GoalPresentation {
        GoalPresentation::from_messages(&messages(goal, Vec::new()), None).unwrap()
    }

    #[test]
    fn dock_is_absent_without_an_installed_goal() {
        assert!(GoalPresentation::from_messages(&[], None).is_none());
    }

    #[test]
    fn unlimited_budget_uses_bare_counters_without_fabricated_ratios() {
        let dock = presentation(json!({
            "version": 1,
            "status": "active",
            "active": true,
            "budget": {"max_turns": 0, "max_tokens": 0},
            "progress": {"turns_used": 12, "tokens_used": 4200}
        }))
        .dock_line();

        assert!(dock.contains("12 turns"));
        assert!(dock.contains("4.2K tok"));
        assert!(!dock.contains("12/"));
        assert!(!dock.contains("4.2K/"));
    }

    #[test]
    fn finite_budget_uses_ratios() {
        let dock = presentation(json!({
            "version": 1,
            "budget": {"max_turns": 50, "max_tokens": 10000},
            "progress": {"turns_used": 12, "tokens_used": 4200}
        }))
        .dock_line();

        assert!(dock.contains("12/50 turns"));
        assert!(dock.contains("4.2K/10K tok"));
    }

    #[test]
    fn all_goal_statuses_have_distinct_colorless_labels() {
        let statuses = [
            "active",
            "verifying",
            "paused",
            "completed",
            "stopped",
            "budget_exhausted",
            "no_progress",
            "transferred",
        ];
        let labels = statuses
            .iter()
            .map(|status| status_label(status))
            .collect::<std::collections::BTreeSet<_>>();

        assert_eq!(labels.len(), statuses.len());
        assert_eq!(status_label("budget_exhausted"), "BUDGET EXHAUSTED");
        assert_eq!(status_label("no_progress"), "NO PROGRESS");
    }

    #[test]
    fn details_show_verifier_gaps_transfers_and_human_pursuit_labels() {
        let goal = json!({
            "version": 3,
            "status": "transferred",
            "active": false,
            "progress": {"turns_used": 2, "tokens_used": 100},
            "attempts": [{"verdict": "unmet", "gaps": ["run tests", "write docs"]}],
            "transferred_from": "chat-a",
            "transferred_to": "chat-b"
        });
        let event = json!({
            "role": "event",
            "content": "ignored raw event",
            "extra": {"event": {"subkind": "goal_pursuit", "payload": {"kind": "verification_gaps"}}}
        });
        let presentation =
            GoalPresentation::from_messages(&messages(goal, vec![event]), None).unwrap();
        let details = presentation.detail_lines().join("\n");

        assert!(details.contains("Status: TRANSFERRED"));
        assert!(details.contains("- run tests"));
        assert!(details.contains("Transferred from: chat-a"));
        assert!(details.contains("Transferred to: chat-b"));
        assert!(details.contains("Verifier reported gaps"));
        assert!(!details.contains("{\"kind\""));
    }

    #[test]
    fn controls_match_goal_status_availability() {
        let active = presentation(json!({"status": "active", "active": true}));
        assert!(active.allows(GoalControlAction::Pause));
        assert!(active.allows(GoalControlAction::Stop));
        assert!(!active.allows(GoalControlAction::Resume));

        for status in ["paused", "stopped"] {
            let goal = presentation(json!({"status": status, "active": false}));
            assert!(goal.allows(GoalControlAction::Resume));
            assert!(!goal.allows(GoalControlAction::Pause));
            assert!(!goal.allows(GoalControlAction::Stop));
        }
    }

    #[test]
    fn modern_projection_shows_effective_state_owner_revision_and_stop_reason() {
        let goal = presentation(json!({
            "version": 3,
            "revision": 7,
            "status": "active",
            "resolution": "active",
            "execution": "idle",
            "autonomy": "off",
            "owner": "delegate",
            "active": true,
            "stop_reason": "manual check",
            "budget": {"max_turns": 5},
            "progress": {"turns_used": 2},
            "attempts": [{"verdict": "inconclusive", "gaps": []}]
        }));
        let dock = goal.dock_line();
        let details = goal.detail_lines().join("\n");

        assert!(dock.contains("Goal IDLE / AUTONOMY OFF"));
        assert!(!dock.contains("Goal ACTIVE"));
        assert!(details.contains("Goal: Ship goal controls"));
        assert!(details.contains("Resolution: ACTIVE"));
        assert!(details.contains("Owner: DELEGATE · Revision: 7"));
        assert!(details.contains("Stop reason: manual check"));
        assert!(details.contains("Latest verifier: INCONCLUSIVE"));
        assert!(goal.allows(GoalControlAction::Cancel));
        assert!(goal.allows(GoalControlAction::Clear));
        assert!(goal.allows_replace());
        assert!(goal.allows_autonomy());
    }

    #[test]
    fn snoozed_quiescent_and_budget_states_are_not_reported_as_running() {
        let snoozed = presentation(json!({
            "status": "active",
            "resolution": "active",
            "execution": "retry_scheduled",
            "autonomy": "on",
            "active": true,
            "snoozed_until_ms": 12345
        }));
        assert!(snoozed.dock_line().contains("Goal SNOOZED"));

        let quiescent_event = json!({
            "role": "event",
            "content": "",
            "extra": {"event": {"subkind": "goal_pursuit", "payload": {"kind": "pursuit_quiescent"}}}
        });
        let quiescent = GoalPresentation::from_messages(
            &messages(
                json!({"status": "active", "resolution": "active", "execution": "idle", "autonomy": "on"}),
                vec![quiescent_event],
            ),
            None,
        )
        .unwrap();
        assert!(quiescent.dock_line().contains("Goal QUIESCENT"));

        let budget = presentation(json!({
            "status": "budget_exhausted",
            "resolution": "active",
            "execution": "paused",
            "autonomy": "on"
        }));
        assert!(budget.dock_line().contains("PAUSED / BUDGET EXHAUSTED"));
    }

    #[test]
    fn terminal_modern_goal_can_be_replaced_or_cleared_but_not_resumed() {
        let goal = presentation(json!({
            "status": "completed",
            "resolution": "achieved",
            "execution": "idle",
            "autonomy": "on",
            "active": true
        }));

        assert!(goal.dock_line().contains("Goal ACHIEVED"));
        assert!(!goal.allows(GoalControlAction::Resume));
        assert!(!goal.allows(GoalControlAction::Pause));
        assert!(!goal.allows(GoalControlAction::Stop));
        assert!(!goal.allows(GoalControlAction::Cancel));
        assert!(goal.allows(GoalControlAction::Clear));
        assert!(goal.allows_replace());
        assert!(!goal.allows_autonomy());
    }

    #[test]
    fn legacy_projection_keeps_legacy_controls_and_status() {
        let goal = presentation(json!({"status": "active", "active": true}));

        assert!(goal.dock_line().contains("Goal IDLE"));
        assert!(goal
            .detail_lines()
            .join("\n")
            .contains("Resolution: LEGACY"));
        assert!(!goal.allows(GoalControlAction::Cancel));
        assert!(!goal.allows(GoalControlAction::Clear));
        assert!(!goal.allows_replace());
    }

    #[test]
    fn blank_budget_inputs_omit_hard_limit_keys() {
        let budget = budget_from_inputs("", "", "", "", "").unwrap();
        let wire = serde_json::to_value(budget).unwrap();

        assert_eq!(wire, json!({}));
    }
}
