//! Where a message came from: which agent produced it, in which room, wearing which accent.
//!
//! The engine cannot draw anything, but it is the only side that *knows* the truth: a room peer
//! is a plain chat session whose role lives in the card's `team_members`, and nothing else in the
//! pipeline carries that. So the engine stamps provenance onto the message and the GUI reads it
//! back, instead of the GUI guessing an author from a scalar `assignee`.
//!
//! Storage is `ChatMessage.extra` under [`PROVENANCE_EXTRA_KEY`], deliberately, for three reasons:
//!
//! * `extra` is a flattened serde map, so a message written before this field existed deserializes
//!   unchanged and one written after still round-trips — no `KNOWN_FIELDS` surgery in the core
//!   `ChatMessage` deserializer, no per-field migration.
//! * `extra` already reaches the client (it is how `tool_enrichment` reaches it), so the GUI needs
//!   no new transport, only a type.
//! * The LLM adapters map messages to provider wire formats field by field and never read `extra`,
//!   so provenance is invisible to the model — which is right: it is a presentation concern.
//!
//! Absent provenance is normal and means "no known author", not an error.

use refact_core::chat_types::ChatMessage;
use serde::{Deserialize, Serialize};
use serde_json::Map;

/// The `ChatMessage.extra` key holding a [`MessageProvenance`].
pub const PROVENANCE_EXTRA_KEY: &str = "provenance";

/// Number of distinct accent slots. Themes define one value per slot, so this is a palette size,
/// not a colour list.
const ACCENT_SLOTS: usize = 6;

/// Roles with a fixed slot, so the two most crowded ones never borrow each other's colour.
///
/// Order is also the tie-break for unknown roles: anything not listed is hashed, which is stable
/// for a given role string but may land on a named role's slot. That is a cosmetic collision on an
/// unrecognised role, and it is the correct trade for a guarantee that never re-colours a role
/// once a client has cached it.
const NAMED_ROLE_SLOTS: &[(&str, usize)] = &[
    ("architect", 0),
    ("coder", 1),
    ("reviewer", 2),
    ("researcher", 3),
    ("specialist", 4),
    ("planner", 5),
];

/// Stripe token names, indexed by slot. Themes resolve these to light/dark values.
const STRIPE_TOKENS: [&str; ACCENT_SLOTS] = [
    "--room-stripe-1",
    "--room-stripe-2",
    "--room-stripe-3",
    "--room-stripe-4",
    "--room-stripe-5",
    "--room-stripe-6",
];

/// Label token names, indexed by slot. A stripe alone cannot be told apart at a glance in a dense
/// transcript, so the name beside it gets its own themed tone.
const LABEL_TOKENS: [&str; ACCENT_SLOTS] = [
    "--room-label-1",
    "--room-label-2",
    "--room-label-3",
    "--room-label-4",
    "--room-label-5",
    "--room-label-6",
];

/// Who produced a message.
///
/// Every field is optional at the wire level so a partially-known author still deserializes
/// instead of failing a whole trajectory load.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct MessageProvenance {
    /// The chat that produced the message. For a room member this is its `agent_chat_id`.
    #[serde(default)]
    pub chat_id: String,
    /// The author's role (`coder`, `reviewer`, …), used to pick an accent and to label the author.
    #[serde(default)]
    pub role: String,
    /// Short human name for a byline.
    #[serde(default)]
    pub display_name: String,
    /// The card this author belongs to, when it is a room member.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub card_id: Option<String>,
}

impl MessageProvenance {
    /// An origin with only a chat id — honest about what is known, useful for attribution even
    /// when the role is unknown (a planner, a peer outside a card, a user-driven session).
    pub fn from_chat(chat_id: impl Into<String>) -> Self {
        Self {
            chat_id: chat_id.into(),
            ..Default::default()
        }
    }

    /// True when there is nothing worth rendering, so clients can skip the byline entirely.
    pub fn is_empty(&self) -> bool {
        self.chat_id.is_empty() && self.role.is_empty() && self.display_name.is_empty()
    }

    /// The accent slot this role wears.
    pub fn accent(&self) -> RoleAccent {
        role_accent(&self.role)
    }
}

/// A role's visual identity, as CSS custom-property *names* rather than colours.
///
/// The engine decides which slot an author wears; the GUI decides what that slot looks like in
/// light and dark. Keeping hex on the GUI side is what makes a theme swap a styling change
/// instead of a re-derivation of every message's colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RoleAccent {
    /// Token for the accent bar drawn beside the author's messages.
    pub stripe: &'static str,
    /// Token for the author's display name.
    pub label: &'static str,
}

/// The accent slot for `role`, stable for a given role string.
///
/// Deterministic rather than unique: two unrecognised roles may share a slot, but no role ever
/// changes slot between calls or across processes, which is the property a transcript rendering
/// actually depends on.
pub fn role_accent(role: &str) -> RoleAccent {
    let slot = role_accent_slot(role);
    RoleAccent {
        stripe: STRIPE_TOKENS[slot],
        label: LABEL_TOKENS[slot],
    }
}

fn role_accent_slot(role: &str) -> usize {
    let normalized = role.trim().to_ascii_lowercase();
    if let Some((_, slot)) = NAMED_ROLE_SLOTS
        .iter()
        .find(|(name, _)| *name == normalized)
    {
        return *slot;
    }
    if normalized.is_empty() {
        return 0;
    }
    // FNV-1a: no dependency, no allocation, and stable across runs and platforms. Computed in u64
    // and folded down at the end, so a 32-bit target truncates the hash instead of failing to
    // compile on a literal that does not fit its usize.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in normalized.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    (hash % ACCENT_SLOTS as u64) as usize
}

/// Stamp `provenance` onto `message`, replacing any previous stamp.
pub fn attach_provenance(message: &mut ChatMessage, provenance: MessageProvenance) {
    if provenance.is_empty() {
        message.extra.remove(PROVENANCE_EXTRA_KEY);
        return;
    }
    if let Ok(value) = serde_json::to_value(&provenance) {
        message
            .extra
            .insert(PROVENANCE_EXTRA_KEY.to_string(), value);
    }
}

/// Read provenance back off a message, ignoring an unreadable stamp rather than failing.
pub fn provenance_from_message(message: &ChatMessage) -> Option<MessageProvenance> {
    provenance_from_extra(&message.extra)
}

/// Read provenance back off an `extra` map.
pub fn provenance_from_extra(extra: &Map<String, serde_json::Value>) -> Option<MessageProvenance> {
    serde_json::from_value(extra.get(PROVENANCE_EXTRA_KEY)?.clone()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn architect() -> MessageProvenance {
        MessageProvenance {
            chat_id: "agent-T-1-arch".to_string(),
            role: "architect".to_string(),
            display_name: "architect".to_string(),
            card_id: Some("T-1".to_string()),
        }
    }

    fn plain_message() -> ChatMessage {
        ChatMessage::new("assistant".to_string(), "hello".to_string())
    }

    #[test]
    fn provenance_roundtrips_through_the_message() {
        let mut message = plain_message();
        attach_provenance(&mut message, architect());

        let restored = provenance_from_message(&message).expect("provenance must be readable");
        assert_eq!(restored, architect());
        assert_eq!(restored.accent(), role_accent("architect"));
    }

    #[test]
    fn legacy_message_without_provenance_still_parses() {
        let legacy = serde_json::json!({
            "message_id": "legacy-1",
            "role": "assistant",
            "content": "written before provenance existed",
        });

        let message: ChatMessage = serde_json::from_value(legacy).unwrap();
        assert!(provenance_from_message(&message).is_none());
        assert_eq!(
            message.content.content_text_only(),
            "written before provenance existed"
        );
    }

    #[test]
    fn provenance_survives_a_full_serialization_cycle() {
        let mut message = plain_message();
        attach_provenance(&mut message, architect());
        let serialized = serde_json::to_value(&message).unwrap();

        let restored: ChatMessage = serde_json::from_value(serialized).unwrap();
        assert_eq!(provenance_from_message(&restored), Some(architect()));
    }

    #[test]
    fn malformed_provenance_is_ignored_rather_than_fatal() {
        let mut message = plain_message();
        message.extra.insert(
            PROVENANCE_EXTRA_KEY.to_string(),
            serde_json::json!({"chat_id": 7}),
        );

        let restored: ChatMessage = serde_json::from_value(serde_json::to_value(&message).unwrap())
            .expect("a bad stamp must not poison the whole message");
        assert!(provenance_from_message(&restored).is_none());
    }

    #[test]
    fn empty_provenance_is_not_stamped() {
        let mut message = plain_message();
        attach_provenance(&mut message, MessageProvenance::default());

        assert!(
            !message.extra.contains_key(PROVENANCE_EXTRA_KEY),
            "an author with no identity at all is better shown as unattributed"
        );
    }

    #[test]
    fn restamping_replaces_rather_than_accumulates() {
        let mut message = plain_message();
        attach_provenance(&mut message, architect());
        attach_provenance(&mut message, MessageProvenance::from_chat("agent-T-1-code"));

        let restored = provenance_from_message(&message).unwrap();
        assert_eq!(restored.chat_id, "agent-T-1-code");
        assert!(restored.role.is_empty());
    }

    #[test]
    fn role_accent_is_stable_across_calls() {
        assert_eq!(role_accent("coder"), role_accent("coder"));
        assert_eq!(role_accent("  CODER "), role_accent("coder"));
        assert_eq!(role_accent("architect"), role_accent("Architect"));
    }

    #[test]
    fn named_roles_never_share_a_slot() {
        let accents = NAMED_ROLE_SLOTS
            .iter()
            .map(|(role, _)| role_accent(role))
            .collect::<Vec<_>>();
        for (index, left) in accents.iter().enumerate() {
            for right in accents.iter().skip(index + 1) {
                assert_ne!(left, right, "named roles must be told apart by colour");
            }
        }
    }

    #[test]
    fn accents_are_token_names_not_colours() {
        let accent = role_accent("coder");
        assert!(accent.stripe.starts_with("--room-stripe-"));
        assert!(accent.label.starts_with("--room-label-"));
        assert!(
            !accent.stripe.starts_with('#') && !accent.stripe.starts_with("rgb"),
            "hex in the engine would make a theme swap a logic change"
        );
    }

    #[test]
    fn unknown_roles_are_stable_too() {
        let first = role_accent("special-role");
        assert_eq!(first, role_accent("special-role"));
        assert_ne!(first, role_accent("another-special-role"));
    }
}
