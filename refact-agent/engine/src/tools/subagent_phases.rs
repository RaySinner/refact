pub use refact_tools::tools::subagent_phases::*;

#[cfg(test)]
#[allow(unused_imports)]
use std::collections::HashSet;
#[cfg(test)]
#[allow(unused_imports)]
use std::path::PathBuf;
#[cfg(test)]
#[allow(unused_imports)]
use std::sync::Arc;
#[cfg(test)]
#[allow(unused_imports)]
use serde_json::json;
#[cfg(test)]
#[allow(unused_imports)]
use tokio::sync::Mutex as AMutex;
#[cfg(test)]
#[allow(unused_imports)]
use refact_runtime_api::AtCommandsContext;
#[cfg(test)]
#[allow(unused_imports)]
use refact_core::chat_types::{ChatMessage, ChatContent};
#[cfg(test)]
#[allow(unused_imports)]
use refact_chat_api::internal_roles::{event, EventSubkind};
#[cfg(test)]
#[allow(unused_imports)]
use refact_runtime_api::files_correction::correct_to_nearest_filename;
#[cfg(test)]
#[allow(unused_imports)]
use refact_chat_api::{ExplicitSubchatSpec, TraceParent};
#[cfg(test)]
#[allow(unused_imports)]
use crate::global_context::GlobalContext;
#[cfg(test)]
#[allow(unused_imports)]
use crate::tools::tool_helpers::{load_code_subagent_config, CodeSubagentConfig};
#[cfg(test)]
#[allow(unused_imports)]
use refact_worktrees::scope::ExecutionScope;

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(role: &str, text: &str) -> ChatMessage {
        ChatMessage {
            role: role.to_string(),
            content: ChatContent::SimpleText(text.to_string()),
            ..Default::default()
        }
    }

    #[test]
    fn gather_seed_keeps_users_and_converts_tools() {
        let external = vec![
            msg("system", "should be skipped"),
            msg("user", "review the compression"),
            msg("assistant", "looking"),
            msg("tool", "grep output here"),
            msg("event", "hidden event"),
        ];
        let seed = build_gather_seed_messages(&external);
        assert_eq!(seed.len(), 3);
        assert_eq!(seed[0].role, "user");
        assert_eq!(seed[1].role, "assistant");
        assert_eq!(seed[2].role, "assistant");
        assert!(seed[2]
            .content
            .content_text_only()
            .starts_with("Tool result:"));
        assert!(seed.iter().all(|m| m.tool_calls.is_none()));
        assert!(seed.iter().all(|m| m.tool_call_id.is_empty()));
    }

    #[test]
    fn gather_seed_drops_oldest_non_user_when_over_budget() {
        let big = "x".repeat(MAX_GATHER_SEED_TOOL_MSG_CHARS);
        let mut external = vec![msg("user", "task statement")];
        for _ in 0..40 {
            external.push(msg("tool", &big));
        }
        let seed = build_gather_seed_messages(&external);
        let total: usize = seed
            .iter()
            .map(|m| m.content.content_text_only().chars().count())
            .sum();
        assert!(total <= MAX_GATHER_SEED_TOTAL_CHARS + 200);
        assert!(seed
            .first()
            .unwrap()
            .content
            .content_text_only()
            .contains("omitted"));
        assert_eq!(
            seed.iter().filter(|m| m.role == "user").count(),
            1,
            "user message must survive budget pressure"
        );
    }

    #[test]
    fn gather_seed_caps_each_message() {
        let huge = "y".repeat(MAX_GATHER_SEED_USER_MSG_CHARS * 3);
        let seed = build_gather_seed_messages(&[msg("user", &huge)]);
        let text = seed[0].content.content_text_only();
        assert!(text.chars().count() <= MAX_GATHER_SEED_USER_MSG_CHARS + 50);
        assert!(text.contains("[truncated for gather seed]"));
    }

    #[test]
    fn gather_seed_carries_source_privacy_records() {
        let mut source = msg("tool", "guarded output");
        let record = refact_privacy::FileRecord {
            path: ".env".to_string(),
            zone: "secrets".to_string(),
            attribution: refact_privacy::Attribution::Declared,
        };
        refact_privacy::merge_records(&mut source, [record.clone()]);

        let seed = build_gather_seed_messages(&[source]);
        let privacy: refact_privacy::PrivacyRecord =
            serde_json::from_value(seed[0].extra["privacy"].clone()).unwrap();

        assert_eq!(privacy.files, vec![record]);
    }
}
