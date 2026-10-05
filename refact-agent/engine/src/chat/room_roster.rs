//! Fresh per-turn roster of the room a task agent shares its card with.
//!
//! Room members are isolated chat sessions: each has its own context and its own git worktree,
//! and none of them can see the others' conversations. Without a roster they guess who the
//! neighbours are from their names, which is exactly the information they need and never have.
//!
//! The roster is re-read from the board and re-rendered on **every** request build, never baked
//! into the session preamble. The preamble is only assembled when the active context has no
//! system message (`ensure_session_preamble`), so a roster baked there would freeze at spawn time:
//! whoever finished, whoever joined, would stay invisible forever.
//!
//! Replacement (rather than accumulation) is by marker, the same trick the Buddy pulse uses:
//! the message carries [`ROOM_ROSTER_MARKER`] as its `tool_call_id`, and
//! [`upsert_room_roster_message`] drops any previous block with that marker before inserting
//! the new one.

use std::sync::Arc;

use crate::call_validation::{ChatContent, ChatMessage, ContextFile};
use crate::global_context::GlobalContext;
use crate::tasks::types::{BoardCard, TeamMember};

/// Marker that makes the roster a single replaceable block instead of an append-only one.
pub const ROOM_ROSTER_MARKER: &str = "room_roster";

/// Longest mandate rendered verbatim; longer ones are cut so one verbose neighbour cannot
/// crowd the roster out of the context window.
const MAX_MANDATE_CHARS: usize = 200;

/// How many files of a finished member are listed before the list is truncated.
const MAX_FILES_LISTED: usize = 8;

/// How many tool names of a member are listed. Every name matters more than a file path, so the
/// cap is higher, but the list is still bounded: `task_agent` alone resolves to dozens of tools.
const MAX_TOOLS_LISTED: usize = 16;

/// Render the roster of `members` for the agent whose chat id is `self_chat_id`.
///
/// Returns `None` when there is nobody to show: an empty room, a room of one, or a card whose
/// only member is the reader itself. The calling agent already knows who it is, and a block that
/// merely repeats the reader teaches it nothing.
///
/// The order is fixed (by `agent_id`, falling back to chat id and role) so the block does not
/// jump around between turns and re-train the model's attention on a reshuffle that carries no
/// new information.
pub fn render_room_roster(
    members: &[TeamMember],
    self_chat_id: Option<&str>,
) -> Option<String> {
    let mut peers: Vec<&TeamMember> = members
        .iter()
        .filter(|member| {
            member
                .agent_chat_id
                .as_deref()
                .map(str::trim)
                .filter(|chat_id| !chat_id.is_empty())
                .map(|chat_id| Some(chat_id) != self_chat_id)
                .unwrap_or(false)
        })
        .collect();
    if peers.is_empty() {
        return None;
    }
    peers.sort_by(|left, right| {
        identity_key(left)
            .cmp(identity_key(right))
            .then_with(|| left.role.cmp(&right.role))
    });

    let count = peers.len();
    let mut block = format!(
        "## Room ({count} other {})\n",
        if count == 1 { "member" } else { "members" }
    );
    block.push('\n');
    for peer in peers {
        block.push_str(&render_member_entry(peer));
    }
    Some(block)
}

/// Stable sort key. `agent_id` is assigned once at spawn and never changes, so it is the only
/// field that guarantees a stable order; chat id and role are tie-breakers for hand-written
/// rosters that have no agent id at all.
fn identity_key(member: &TeamMember) -> &str {
    member
        .agent_id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .or_else(|| member.agent_chat_id.as_deref())
        .unwrap_or("")
}

fn render_member_entry(member: &TeamMember) -> String {
    let mut entry = format!("- {}", member.role);
    if let Some(mandate) = member
        .mandate
        .as_deref()
        .map(str::trim)
        .filter(|mandate| !mandate.is_empty())
    {
        entry.push_str(" — ");
        entry.push_str(&truncate(mandate, MAX_MANDATE_CHARS));
    }
    entry.push('\n');

    let files: &[String] = member
        .report
        .as_ref()
        .map(|report| report.files_changed.as_slice())
        .unwrap_or_default();

    entry.push_str("  status: ");
    entry.push_str(member.typed_status().as_str());
    if !files.is_empty() {
        entry.push_str(" · files: ");
        entry.push_str(&join_capped(&files, MAX_FILES_LISTED));
    }
    // Names only. A member's tool *descriptions* are hundreds of lines of prompt that would
    // dominate the roster and would go stale the moment the catalog is reloaded.
    entry.push_str(" · tools: ");
    entry.push_str(&join_capped(&member.tools, MAX_TOOLS_LISTED));
    entry.push('\n');

    if let Some(chat_id) = member
        .agent_chat_id
        .as_deref()
        .map(str::trim)
        .filter(|chat_id| !chat_id.is_empty())
    {
        // Mandatory: peers address each other by chat id, not by agent id — a task agent is a
        // plain chat session and has no `BackgroundAgent` record to look up.
        entry.push_str("  chat: ");
        entry.push_str(chat_id);
        entry.push('\n');
    }
    entry
}

/// Comma-separated `items`, blanks dropped, truncated to `limit` with an explicit overflow count.
/// Empty renders as `none` rather than staying silent: "no tools" is itself information.
fn join_capped(items: &[String], limit: usize) -> String {
    if items.is_empty() {
        return "none".to_string();
    }
    let kept: Vec<&str> = items
        .iter()
        .map(|item| item.trim())
        .filter(|item| !item.is_empty())
        .collect();
    if kept.is_empty() {
        return "none".to_string();
    }
    let shown = kept.len().min(limit);
    let mut joined = kept[..shown].join(", ");
    if kept.len() > shown {
        joined.push_str(&format!(", +{} more", kept.len() - shown));
    }
    joined
}

fn truncate(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max_chars.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// The roster for the card the calling agent works on, or `None` outside a room.
///
/// This is the only part that touches the board: everything else in this module is pure.
pub async fn load_room_roster(
    gcx: Arc<GlobalContext>,
    task_id: &str,
    card_id: &str,
    self_chat_id: Option<&str>,
) -> Option<String> {
    if task_id.is_empty() || card_id.is_empty() {
        return None;
    }
    let board = crate::tasks::storage::load_board(gcx, task_id)
        .await
        .ok()?;
    let card = board.get_card(card_id)?;
    roster_for_card(card, self_chat_id)
}

/// The roster for `card`, or `None` when the card is not a room.
///
/// Split out so the "is this a room at all?" rule is testable without a board on disk.
pub fn roster_for_card(card: &BoardCard, self_chat_id: Option<&str>) -> Option<String> {
    if !card.is_room() {
        return None;
    }
    render_room_roster(card.team(), self_chat_id)
}

/// Build the marker-carrying message for `text`, or `None` when there is no roster at all.
pub fn room_roster_message(text: String) -> ChatMessage {
    let line_count = text.lines().count().max(1);
    ChatMessage {
        role: "context_file".to_string(),
        content: ChatContent::ContextFiles(vec![ContextFile {
            file_name: "(room roster)".to_string(),
            file_content: text,
            line1: 1,
            line2: line_count,
            file_rev: None,
            symbols: vec![],
            gradient_type: -1,
            usefulness: 80.0,
            skip_pp: true,
        }]),
        tool_call_id: ROOM_ROSTER_MARKER.to_string(),
        ..Default::default()
    }
}

/// Insert the fresh roster, replacing any previous block carrying the same marker.
///
/// Returns the index the block landed at. A stale roster that survived in the active context is
/// worse than no roster: it tells the agent a finished peer is still running, or that a file is
/// free when it is not.
pub fn upsert_room_roster_message(messages: &mut Vec<ChatMessage>, text: String) -> usize {
    messages.retain(|message| {
        !(message.role == "context_file" && message.tool_call_id == ROOM_ROSTER_MARKER)
    });
    let insert_at = messages
        .iter()
        .position(|message| {
            message.role == "user"
                || message.role == "assistant"
                || message.role == crate::chat::internal_roles::EVENT_ROLE
        })
        .unwrap_or(messages.len());
    messages.insert(insert_at, room_roster_message(text));
    insert_at
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::rooms::new_room_member;
    use crate::tasks::types::TeamMemberReport;
    use crate::tasks::types::TeamStatus;

    fn peer(role: &str, agent_id: &str, chat_id: &str, mandate: Option<&str>) -> TeamMember {
        new_room_member(
            role,
            agent_id,
            chat_id,
            None,
            None,
            mandate.map(str::to_string),
            vec![],
        )
    }

    fn member(
        role: &str,
        agent_id: &str,
        chat_id: &str,
        mandate: Option<&str>,
        tools: &[&str],
    ) -> TeamMember {
        let mut member = peer(role, agent_id, chat_id, mandate);
        member.tools = tools.iter().map(|name| (*name).to_string()).collect();
        member
    }

    fn card_with(members: &[TeamMember]) -> BoardCard {
        BoardCard {
            id: "T-7".to_string(),
            title: "Card T-7".to_string(),
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
            team_members: members.to_vec(),
            target_files: vec![],
            scope_guard_mode: Default::default(),
        }
    }

    #[test]
    fn roster_absent_for_single_agent_card() {
        let card = card_with(&[peer("coder", "a1", "agent-T-1-a1", Some("only me"))]);

        assert!(!card.is_room());
        assert_eq!(roster_for_card(&card, Some("agent-T-1-a1")), None);
        assert_eq!(roster_for_card(&card, Some("some-other-chat")), None);
    }

    #[test]
    fn roster_absent_in_plain_chat() {
        // A plain conversation is not on a card at all, so there is no roster to render.
        assert_eq!(render_room_roster(&[], None), None);
        assert_eq!(render_room_roster(&[], Some("agent-T-1-a1")), None);
    }

    #[test]
    fn roster_lists_name_mandate_status_files_tools() {
        let mut reviewer = member(
            "reviewer",
            "a2",
            "agent-T-7-reviewer",
            Some("Reviewing: tests only"),
            &["cat", "shell"],
        );
        reviewer.member_status = Some(TeamStatus::Running);
        let mut coder = member(
            "coder",
            "a3",
            "agent-T-7-coder",
            Some("Implementing: src/foo.rs"),
            &["apply_patch", "shell"],
        );
        coder.member_status = Some(TeamStatus::Done);
        coder.report = Some(TeamMemberReport {
            files_changed: vec!["src/foo.rs".into()],
            ..Default::default()
        });

        let block = render_room_roster(&[reviewer.clone(), coder.clone()], Some("agent-T-7-me"))
            .expect("two peers must render");

        assert!(block.starts_with("## Room (2 other members)"), "{block}");
        assert!(block.contains("- reviewer — Reviewing: tests only"), "{block}");
        assert!(block.contains("status: running"), "{block}");
        assert!(block.contains("tools: cat, shell"), "{block}");
        assert!(block.contains("chat: agent-T-7-reviewer"), "{block}");
        assert!(block.contains("- coder — Implementing: src/foo.rs"), "{block}");
        assert!(block.contains("status: done"), "{block}");
        assert!(block.contains("files: src/foo.rs"), "{block}");
        assert!(block.contains("tools: apply_patch, shell"), "{block}");
        assert!(block.contains("chat: agent-T-7-coder"), "{block}");
    }

    #[test]
    fn roster_omits_self() {
        let members = vec![
            peer("architect", "a1", "agent-T-7-me", Some("plan")),
            peer("coder", "a2", "agent-T-7-coder", Some("code")),
        ];

        let block = render_room_roster(&members, Some("agent-T-7-me")).unwrap();

        assert!(!block.contains("agent-T-7-me"), "{block}");
        assert!(!block.contains("- architect"), "{block}");
        assert!(block.contains("agent-T-7-coder"), "{block}");
    }

    #[test]
    fn roster_shows_none_when_tools_unknown() {
        let members = vec![
            peer("architect", "a1", "agent-T-7-me", None),
            peer("coder", "a2", "agent-T-7-coder", None),
        ];

        let block = render_room_roster(&members, Some("agent-T-7-me")).unwrap();

        assert!(block.contains("tools: none"), "{block}");
    }

    #[test]
    fn roster_uses_mandate_not_full_prompt() {
        // The member's `prompt` is its whole system prompt; the roster shows the mandate line only.
        let mut with_prompt = peer(
            "coder",
            "a2",
            "agent-T-7-coder",
            Some("write the parser"),
        );
        with_prompt.status = Some("# Card: T-7\n## Instructions\ndo everything\n".to_string());

        let block = render_room_roster(&[with_prompt.clone()], Some("agent-T-7-me")).unwrap();

        assert!(block.contains("write the parser"), "{block}");
        assert!(!block.contains("do everything"), "{block}");
        assert!(!block.contains("## Instructions"), "{block}");
    }

    #[test]
    fn roster_omits_mandate_line_when_empty() {
        let members = vec![
            peer("architect", "a1", "agent-T-7-me", None),
            member("coder", "a2", "agent-T-7-coder", Some("   "), &["cat"]),
        ];

        let block = render_room_roster(&members, Some("agent-T-7-me")).unwrap();

        assert!(block.contains("- coder\n"), "{block}");
        assert!(
            !block.contains("- coder —"),
            "a blank mandate must not leave a dangling separator: {block}"
        );
    }

    #[test]
    fn roster_order_is_deterministic() {
        let members = vec![
            peer("reviewer", "a3", "agent-T-7-c", None),
            member("architect", "a1", "agent-T-7-a", None, &["cat"]),
            member("coder", "a2", "agent-T-7-b", None, &["shell"]),
        ];

        let first = render_room_roster(&members, Some("agent-T-7-me")).unwrap();
        let mut reversed = members.clone();
        reversed.reverse();
        let second = render_room_roster(&reversed, Some("agent-T-7-me")).unwrap();

        assert_eq!(first, second, "input order must not leak into the block");
        let positions: Vec<usize> = ["- architect", "- coder", "- reviewer"]
            .iter()
            .map(|role| first.find(role).expect("role must be listed"))
            .collect();
        assert!(
            positions.windows(2).all(|pair| pair[0] < pair[1]),
            "members must follow agent_id order: {positions:?}"
        );
    }

    #[test]
    fn roster_block_replaces_previous_block_not_appends() {
        let mut messages = vec![
            ChatMessage {
                role: "system".to_string(),
                content: ChatContent::SimpleText("system".to_string()),
                ..Default::default()
            },
            room_roster_message("## Room (1 other member)\n- coder — old\n".to_string()),
            ChatMessage {
                role: "user".to_string(),
                content: ChatContent::SimpleText("go".to_string()),
                ..Default::default()
            },
        ];

        upsert_room_roster_message(&mut messages, "## Room (2 other members)\n- fresh\n".to_string());

        let rosters: Vec<&ChatMessage> = messages
            .iter()
            .filter(|message| message.tool_call_id == ROOM_ROSTER_MARKER)
            .collect();
        assert_eq!(rosters.len(), 1, "exactly one roster block must survive");
        let text = rosters[0].content.content_text_only();
        assert!(text.contains("- fresh"), "{text}");
        assert!(!text.contains("old"), "{text}");
        assert_eq!(
            messages.iter().filter(|m| m.role == "user").count(),
            1,
            "the conversation must survive the swap"
        );
    }

    #[test]
    fn roster_omits_card_when_no_team_members() {
        // A card with an assignee but no roster entries is a single-agent card: nothing to show.
        let mut card = card_with(&[]);
        card.assignee = Some("agent-1".to_string());
        card.agent_chat_id = Some("agent-T-1-a1".to_string());

        assert!(!card.is_room());
        assert_eq!(roster_for_card(&card, Some("agent-T-1-a1")), None);
        assert_eq!(render_room_roster(&card.team(), Some("agent-T-1-a1")), None);
    }

    #[test]
    fn roster_caps_long_mandate_and_long_file_lists() {
        let mut coder = peer(
            "coder",
            "a2",
            "agent-T-7-coder",
            Some(&"x".repeat(MAX_MANDATE_CHARS + 50)),
        );
        coder.tools = vec!["cat".to_string()];
        coder.report = Some(TeamMemberReport {
            files_changed: (0..MAX_FILES_LISTED + 4)
                .map(|index| format!("src/file{index}.rs"))
                .collect(),
            ..Default::default()
        });

        let block = render_room_roster(&[coder], Some("agent-T-7-me")).unwrap();

        assert!(block.contains('…'), "a long mandate must be cut: {block}");
        assert!(block.contains("+4 more"), "{block}");
        assert!(block.chars().count() < 1_000, "{block}");
    }
}
