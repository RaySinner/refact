use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use chrono::{DateTime, Duration, Utc};
use refact_core::chat_types::{ChatContent, ChatMessage, ContextFile};
use serde::{Deserialize, Serialize};

use crate::actor::redact_sensitive;
use crate::conversation_ledger::list_recent_buddy_conversations;
use crate::types::BuddyConversationEntry;
use crate::user_action::UserAction;
use crate::user_activity::time_of_day_pattern;

pub const BUDDY_PULSE_MARKER: &str = "buddy_project_memory_pulse";
const MAX_PULSE_MARKDOWN_CHARS: usize = 2000;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct BuddyPulsePayload {
    pub preferences: Vec<PulsePreference>,
    pub lessons: Vec<PulseLesson>,
    pub friction: PulseFriction,
    pub recent_reports: Vec<PulseReport>,
    pub user_activity: PulseActivitySection,
    pub generated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PulsePreference {
    pub statement: String,
    pub confidence: f32,
    pub last_updated: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PulseLesson {
    pub title: String,
    pub preview: String,
    pub tags: Vec<String>,
    pub updated: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PulseFriction {
    pub top_error_types: Vec<(String, u32)>,
    pub stuck_tasks: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PulseReport {
    pub workflow_id: String,
    pub title: String,
    pub preview: String,
    pub chat_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PulseActivitySection {
    pub grouped: Vec<PulseActivityGroup>,
    pub time_of_day_pattern: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PulseActivityGroup {
    pub action_type: String,
    pub count: u32,
    pub details: Vec<String>,
}

#[derive(Clone)]
struct UserPreferenceRecord {
    statement: String,
    confidence: f32,
    last_updated: String,
}

pub fn pulse_message_from_payload(payload: &BuddyPulsePayload) -> ChatMessage {
    let file_content = render_pulse_as_markdown(payload);
    let mut extra = serde_json::Map::new();
    extra.insert(
        "buddy_pulse_payload".to_string(),
        serde_json::to_value(payload).unwrap_or(serde_json::Value::Null),
    );
    ChatMessage {
        role: "context_file".to_string(),
        content: ChatContent::ContextFiles(vec![ContextFile {
            file_name: "buddy_project_memory_pulse.md".to_string(),
            file_content,
            line1: 0,
            line2: 0,
            ..Default::default()
        }]),
        tool_call_id: BUDDY_PULSE_MARKER.to_string(),
        extra,
        ..Default::default()
    }
}

pub fn render_pulse_as_markdown(payload: &BuddyPulsePayload) -> String {
    let mut sections = Vec::new();

    if !payload.preferences.is_empty() {
        let mut lines = vec!["## USER PREFERENCES".to_string()];
        for pref in payload.preferences.iter().take(3) {
            lines.push(format!(
                "- {:.2}: {} (updated {})",
                pref.confidence,
                truncate_inline(&redact(&pref.statement), 72),
                redact(&pref.last_updated)
            ));
        }
        sections.push(lines.join("\n"));
    }

    if !payload.lessons.is_empty() {
        let mut lines = vec!["## PROJECT LESSONS".to_string()];
        for lesson in payload.lessons.iter().take(3) {
            let tags = lesson
                .tags
                .iter()
                .take(4)
                .map(redact)
                .collect::<Vec<_>>()
                .join(", ");
            lines.push(format!(
                "- {}: {} [{}]",
                truncate_inline(&redact(&lesson.title), 56),
                truncate_inline(&redact(&lesson.preview), 88),
                tags
            ));
        }
        sections.push(lines.join("\n"));
    }

    if !payload.friction.top_error_types.is_empty() || payload.friction.stuck_tasks > 0 {
        let mut lines = vec!["## RECENT FRICTION".to_string()];
        if !payload.friction.top_error_types.is_empty() {
            lines.push(format!(
                "- top errors: {}",
                payload
                    .friction
                    .top_error_types
                    .iter()
                    .take(3)
                    .map(|(kind, count)| format!("{} ({})", redact(kind), count))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        if payload.friction.stuck_tasks > 0 {
            lines.push(format!(
                "- recent stuck task alerts (1h): {}",
                payload.friction.stuck_tasks
            ));
        }
        sections.push(lines.join("\n"));
    }

    if !payload.recent_reports.is_empty() {
        let mut lines = vec!["## RECENT BUDDY REPORTS".to_string()];
        for report in payload.recent_reports.iter().take(2) {
            lines.push(format!(
                "- {}: {} ({})",
                redact(&report.title),
                truncate_inline(&redact(&report.preview), 96),
                redact(&report.chat_id)
            ));
        }
        sections.push(lines.join("\n"));
    }

    if !payload.user_activity.grouped.is_empty() {
        let mut lines = vec!["## USER ACTIVITY (last 24h)".to_string()];
        lines.push(format!(
            "- pattern: {}",
            redact(&payload.user_activity.time_of_day_pattern)
        ));
        for group in payload.user_activity.grouped.iter().take(5) {
            let details = group
                .details
                .iter()
                .take(3)
                .map(redact)
                .collect::<Vec<_>>()
                .join("; ");
            if details.is_empty() {
                lines.push(format!("- {}: {}", redact(&group.action_type), group.count));
            } else {
                lines.push(format!(
                    "- {}: {} ({})",
                    redact(&group.action_type),
                    group.count,
                    truncate_inline(&details, 120)
                ));
            }
        }
        sections.push(lines.join("\n"));
    }
    let footer = format!("_Generated {}_", redact(&payload.generated_at));
    let mut out = sections;
    out.push(footer);
    cap_markdown_chars(out.join("\n\n"), MAX_PULSE_MARKDOWN_CHARS)
}

fn cap_markdown_chars(markdown: String, max_chars: usize) -> String {
    if markdown.chars().count() <= max_chars {
        return markdown;
    }
    let marker = "\n...[truncated]";
    let keep_chars = max_chars.saturating_sub(marker.chars().count());
    let mut truncated = markdown.chars().take(keep_chars).collect::<String>();
    truncated.push_str(marker);
    truncated
}

fn truncate_inline(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let marker = "...";
    let keep_chars = max_chars.saturating_sub(marker.chars().count());
    let mut truncated = text.chars().take(keep_chars).collect::<String>();
    truncated.push_str(marker);
    truncated
}

pub async fn read_preferences(project_root: &Path) -> Vec<PulsePreference> {
    let path = project_root.join(".refact/buddy/user_profile.md");
    let Ok(text) = tokio::fs::read_to_string(path).await else {
        return Vec::new();
    };
    let mut prefs = parse_user_profile(&text)
        .into_iter()
        .filter(|pref| pref.confidence >= 0.5)
        .map(|pref| PulsePreference {
            statement: redact(pref.statement),
            confidence: pref.confidence,
            last_updated: redact(pref.last_updated),
        })
        .collect::<Vec<_>>();
    prefs.sort_by(|a, b| {
        b.confidence
            .partial_cmp(&a.confidence)
            .unwrap_or(Ordering::Equal)
            .then_with(|| b.last_updated.cmp(&a.last_updated))
            .then_with(|| a.statement.cmp(&b.statement))
    });
    prefs.truncate(5);
    prefs
}

fn parse_user_profile(text: &str) -> Vec<UserPreferenceRecord> {
    let mut prefs = Vec::new();
    let mut current: Option<UserPreferenceRecord> = None;
    for line in text.lines() {
        let line = line.trim();
        if line.strip_prefix("## ").is_some() {
            if let Some(pref) = current.take() {
                if !pref.statement.is_empty() {
                    prefs.push(pref);
                }
            }
            current = Some(UserPreferenceRecord {
                statement: String::new(),
                confidence: 0.0,
                last_updated: String::new(),
            });
            continue;
        }
        let Some(pref) = current.as_mut() else {
            continue;
        };
        if let Some(value) = line.strip_prefix("- statement:") {
            pref.statement = parse_profile_value(value);
        } else if let Some(value) = line.strip_prefix("- confidence:") {
            pref.confidence = value.trim().parse::<f32>().unwrap_or(0.0);
        } else if let Some(value) = line.strip_prefix("- last_updated:") {
            pref.last_updated = parse_profile_value(value);
        }
    }
    if let Some(pref) = current {
        if !pref.statement.is_empty() {
            prefs.push(pref);
        }
    }
    prefs
}

fn parse_profile_value(value: &str) -> String {
    let value = value.trim();
    if value.len() >= 2 && value.starts_with('"') && value.ends_with('"') {
        value[1..value.len() - 1].replace("\\\"", "\"")
    } else {
        value.to_string()
    }
}

pub async fn build_recent_reports(project_root: &Path) -> Vec<PulseReport> {
    list_recent_buddy_conversations(project_root, 2)
        .await
        .into_iter()
        .map(report_from_entry)
        .collect()
}

fn report_from_entry(entry: BuddyConversationEntry) -> PulseReport {
    PulseReport {
        workflow_id: redact(entry.kind),
        title: redact(entry.title),
        preview: redact(format!(
            "{}; {} messages; updated {}",
            entry.status, entry.message_count, entry.updated_at
        )),
        chat_id: redact(entry.id),
    }
}

pub fn build_activity_section_from_actions(actions: &[UserAction]) -> PulseActivitySection {
    if actions.is_empty() {
        return PulseActivitySection {
            grouped: Vec::new(),
            time_of_day_pattern: redact(time_of_day_pattern(actions)),
        };
    }
    PulseActivitySection {
        grouped: group_activity(actions),
        time_of_day_pattern: redact(time_of_day_pattern(actions)),
    }
}

fn group_activity(actions: &[UserAction]) -> Vec<PulseActivityGroup> {
    let mut grouped: BTreeMap<String, PulseActivityGroup> = BTreeMap::new();
    for action in actions {
        let (action_type, detail) = action_summary(action);
        let group = grouped
            .entry(action_type.clone())
            .or_insert_with(|| PulseActivityGroup {
                action_type,
                count: 0,
                details: Vec::new(),
            });
        group.count = group.count.saturating_add(1);
        if let Some(detail) = detail {
            if group.details.len() < 3 && !group.details.contains(&detail) {
                group.details.push(detail);
            }
        }
    }
    let mut groups = grouped.into_values().collect::<Vec<_>>();
    groups.sort_by(|a, b| {
        b.count
            .cmp(&a.count)
            .then_with(|| a.action_type.cmp(&b.action_type))
    });
    groups
}

fn action_summary(action: &UserAction) -> (String, Option<String>) {
    match action {
        UserAction::FileOpened { path, .. } => ("file_opened".to_string(), Some(redact(path))),
        UserAction::SnippetSelected { path, lines, .. } => (
            "snippet_selected".to_string(),
            Some(redact(format!("{}:{}-{}", path, lines.0, lines.1))),
        ),
        UserAction::ToolApproved { tool_name, .. } => {
            ("tool_approved".to_string(), Some(redact(tool_name)))
        }
        UserAction::ToolRejected { tool_name, .. } => {
            ("tool_rejected".to_string(), Some(redact(tool_name)))
        }
        UserAction::CommandRun {
            command_preview, ..
        } => ("command_run".to_string(), Some(redact(command_preview))),
        UserAction::WorkspaceChanged {
            folders_added,
            folders_removed,
            ..
        } => (
            "workspace_changed".to_string(),
            Some(redact(format!(
                "+{} -{}",
                folders_added.join(","),
                folders_removed.join(",")
            ))),
        ),
        UserAction::CommitMade {
            sha,
            message_first_line,
            files,
            ..
        } => (
            "commit_made".to_string(),
            Some(redact(format!(
                "{} {} files {}",
                sha, files, message_first_line
            ))),
        ),
        UserAction::TaskFailed {
            task_id,
            reason_short,
            ..
        } => (
            "task_failed".to_string(),
            Some(redact(format!("{}: {}", task_id, reason_short))),
        ),
        UserAction::ChatStarted {
            first_user_text_preview,
            ..
        } => (
            "chat_started".to_string(),
            Some(redact(first_user_text_preview)),
        ),
    }
}

pub fn payload_is_empty(payload: &BuddyPulsePayload) -> bool {
    payload.preferences.is_empty()
        && payload.lessons.is_empty()
        && payload.recent_reports.is_empty()
        && payload.user_activity.grouped.is_empty()
        && payload.friction.top_error_types.is_empty()
        && payload.friction.stuck_tasks == 0
}

pub fn pulse_friction_from_diagnostics(
    diagnostics: &[(String, String)],
    pulse_diagnostic_types: &[String],
    stuck_tasks: u32,
    now: DateTime<Utc>,
) -> PulseFriction {
    let cutoff = now - Duration::hours(1);
    let mut counts: HashMap<String, u32> = HashMap::new();
    for (error_type, collected_at) in diagnostics {
        let recent = DateTime::parse_from_rfc3339(collected_at)
            .map(|dt| dt.with_timezone(&Utc) >= cutoff)
            .unwrap_or(true);
        if recent {
            *counts.entry(redact(error_type)).or_insert(0) += 1;
        }
    }
    for error_type in pulse_diagnostic_types {
        counts.entry(redact(error_type)).or_insert(1);
    }
    let mut top_error_types = counts.into_iter().collect::<Vec<_>>();
    top_error_types.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    top_error_types.truncate(3);
    PulseFriction {
        top_error_types,
        stuck_tasks,
    }
}

fn redact(text: impl AsRef<str>) -> String {
    redact_sensitive(text.as_ref())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, Local, TimeZone};

    fn local_ts(hour: u32) -> DateTime<Utc> {
        Local
            .with_ymd_and_hms(2024, 1, 2, hour, 0, 0)
            .single()
            .unwrap()
            .with_timezone(&Utc)
    }

    fn full_payload() -> BuddyPulsePayload {
        BuddyPulsePayload {
            preferences: (0..8)
                .map(|idx| PulsePreference {
                    statement: format!(
                        "prefers detailed direct implementation plans with no fallbacks {idx} {}",
                        "x".repeat(160)
                    ),
                    confidence: 0.95,
                    last_updated: "2026-05-14T10:00:00Z".to_string(),
                })
                .collect(),
            lessons: (0..8)
                .map(|idx| PulseLesson {
                    title: format!("Lesson {idx} {}", "y".repeat(80)),
                    preview: format!(
                        "Keep the implementation focused and verified {idx} {}",
                        "z".repeat(180)
                    ),
                    tags: vec!["lesson".to_string(), "convention".to_string()],
                    updated: "2026-05-14".to_string(),
                })
                .collect(),
            friction: PulseFriction {
                top_error_types: vec![
                    ("timeout".to_string(), 4),
                    ("compile_error".to_string(), 3),
                    ("tool_error".to_string(), 2),
                ],
                stuck_tasks: 2,
            },
            recent_reports: vec![
                PulseReport {
                    workflow_id: "buddy_report".to_string(),
                    title: "Report A".to_string(),
                    preview: "A recent report with useful context".to_string(),
                    chat_id: "chat-a".to_string(),
                },
                PulseReport {
                    workflow_id: "buddy_report".to_string(),
                    title: "Report B".to_string(),
                    preview: "Another recent report".to_string(),
                    chat_id: "chat-b".to_string(),
                },
            ],
            user_activity: PulseActivitySection {
                grouped: vec![PulseActivityGroup {
                    action_type: "file_opened".to_string(),
                    count: 6,
                    details: vec!["src/main.rs".to_string(), "src/lib.rs".to_string()],
                }],
                time_of_day_pattern: "mostly active 09–12".to_string(),
            },
            generated_at: "2026-05-14T10:00:00Z".to_string(),
        }
    }

    #[test]
    fn pulse_under_2000_chars_when_full() {
        let markdown = render_pulse_as_markdown(&full_payload());
        assert!(markdown.chars().count() <= 2000);
        assert!(markdown.contains("USER PREFERENCES"));
        assert!(markdown.contains("USER ACTIVITY"));
    }

    #[tokio::test]
    async fn preferences_exclude_low_confidence_and_keep_high() {
        let dir = tempfile::tempdir().unwrap();
        let profile = dir.path().join(".refact/buddy/user_profile.md");
        tokio::fs::create_dir_all(profile.parent().unwrap())
            .await
            .unwrap();
        tokio::fs::write(
            &profile,
            "# User Profile\n\n## low\n- statement: \"Ignore me\"\n- confidence: 0.49\n- last_updated: 2026-05-14T09:00:00Z\n\n## high\n- statement: \"Keep me\"\n- confidence: 0.90\n- last_updated: 2026-05-14T10:00:00Z\n",
        )
        .await
        .unwrap();

        let preferences = read_preferences(dir.path()).await;

        assert_eq!(preferences.len(), 1);
        assert_eq!(preferences[0].statement, "Keep me");
    }

    #[test]
    fn pulse_message_uses_correct_marker() {
        let message = pulse_message_from_payload(&full_payload());
        assert_eq!(message.role, "context_file");
        assert_eq!(message.tool_call_id, BUDDY_PULSE_MARKER);
        match message.content {
            ChatContent::ContextFiles(files) => {
                assert_eq!(files[0].file_name, "buddy_project_memory_pulse.md");
            }
            _ => panic!("expected context files"),
        }
    }

    #[test]
    fn pulse_payload_serializes_in_extra_map() {
        let message = pulse_message_from_payload(&full_payload());
        let payload_value = message.extra.get("buddy_pulse_payload").unwrap();
        assert_eq!(payload_value["recent_reports"][0]["chat_id"], "chat-a");
        assert_eq!(payload_value["friction"]["stuck_tasks"], 2);
        assert!(serde_json::to_value(&message)
            .unwrap()
            .get("buddy_pulse_payload")
            .is_some());
    }

    #[test]
    fn activity_section_groups_actions_and_reports_pattern() {
        let actions = vec![UserAction::FileOpened {
            path: "src/main.rs".to_string(),
            ts: local_ts(10),
        }];

        let section = build_activity_section_from_actions(&actions);

        assert_eq!(section.grouped[0].action_type, "file_opened");
        assert!(section.time_of_day_pattern.starts_with("mostly active"));
    }

    #[test]
    fn empty_payload_is_detected() {
        assert!(payload_is_empty(&BuddyPulsePayload::default()));
        assert!(!payload_is_empty(&full_payload()));
    }

    #[test]
    fn friction_ranks_recent_diagnostics_and_counts_pulse_types() {
        let now = Utc::now();
        let recent = now.to_rfc3339();
        let diagnostics = vec![
            ("timeout".to_string(), recent.clone()),
            ("timeout".to_string(), recent),
            (
                "compile_error".to_string(),
                "2020-01-01T00:00:00Z".to_string(),
            ),
        ];

        let friction =
            pulse_friction_from_diagnostics(&diagnostics, &["tool_error".to_string()], 2, now);

        assert_eq!(friction.top_error_types[0], ("timeout".to_string(), 2));
        assert!(friction
            .top_error_types
            .contains(&("tool_error".to_string(), 1)));
        assert_eq!(friction.stuck_tasks, 2);
    }
}
