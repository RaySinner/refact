use std::time::Duration;

use refact_core::chat_types::{ChatMessage, Checkpoint};
use crate::diagnostics::is_ui_only_message;
use crate::{TaskMeta, ThreadParams};

const DEFAULT_MAX_QUEUE_SIZE: usize = 100;
const DEFAULT_SESSION_IDLE_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const DEFAULT_SESSION_CLEANUP_INTERVAL: Duration = Duration::from_secs(5 * 60);
const DEFAULT_STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const STREAM_TOTAL_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const STREAM_HEARTBEAT: Duration = Duration::from_secs(2);

/// Prompt size (in tokens) at which idle-timeout scaling starts. Prompts at or below this
/// size keep the configured base idle timeout unchanged, so small chats behave exactly as
/// they did before scaling existed.
const STREAM_IDLE_PREFILL_SCALING_MIN_TOKENS: usize = 8_000;

/// Prefill throughput assumption used for scaling: one extra second of patience per
/// thousand prompt tokens. Prefill time grows with prompt size (a local endpoint
/// processing a large prompt legitimately emits nothing for minutes), so a flat idle
/// deadline kills healthy long-prompt requests.
const STREAM_IDLE_PREFILL_TOKENS_PER_EXTRA_SECOND: usize = 1_000;

/// Head-room kept between the scaled idle timeout and the total stream timeout, so the
/// total-timeout backstop always fires first and stays authoritative.
const STREAM_IDLE_TOTAL_TIMEOUT_MARGIN: Duration = Duration::from_secs(60);

#[derive(Clone, Copy)]
pub struct RuntimeChatTimeouts {
    pub max_queue_size: usize,
    pub session_idle: Duration,
    pub session_cleanup_interval: Duration,
    pub stream_idle: Duration,
    pub stream_total: Duration,
}

impl Default for RuntimeChatTimeouts {
    fn default() -> Self {
        Self {
            max_queue_size: DEFAULT_MAX_QUEUE_SIZE,
            session_idle: DEFAULT_SESSION_IDLE_TIMEOUT,
            session_cleanup_interval: DEFAULT_SESSION_CLEANUP_INTERVAL,
            stream_idle: DEFAULT_STREAM_IDLE_TIMEOUT,
            stream_total: STREAM_TOTAL_TIMEOUT,
        }
    }
}

static RUNTIME_TIMEOUTS: std::sync::LazyLock<std::sync::RwLock<RuntimeChatTimeouts>> =
    std::sync::LazyLock::new(|| std::sync::RwLock::new(RuntimeChatTimeouts::default()));

pub const SEGMENT_SUMMARY_KIND: &str = "llm_segment_summary";

pub fn max_queue_size() -> usize {
    RUNTIME_TIMEOUTS
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .max_queue_size
}

pub fn session_idle_timeout() -> Duration {
    RUNTIME_TIMEOUTS
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .session_idle
}

pub fn session_cleanup_interval() -> Duration {
    RUNTIME_TIMEOUTS
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .session_cleanup_interval
}

pub fn stream_idle_timeout() -> Duration {
    RUNTIME_TIMEOUTS
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .stream_idle
}

pub fn stream_total_timeout() -> Duration {
    RUNTIME_TIMEOUTS
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .stream_total
}

/// Idle deadline the stream watchdog should actually use for this request.
///
/// `stream_idle_timeout()` stays a flat value tuned for small prompts. A large prompt
/// takes proportionally longer to prefill, during which the provider emits no progress
/// events, so the flat deadline kills healthy requests. This scales the deadline with the
/// prompt size while keeping these invariants:
///
/// - unknown prompt size (`None`) returns the configured base value unchanged;
/// - a prompt at or below [`STREAM_IDLE_PREFILL_SCALING_MIN_TOKENS`] returns the configured
///   base value unchanged, so small chats behave exactly as before;
/// - the result is monotonically non-decreasing in prompt size;
/// - the result never reaches [`stream_total_timeout`], so the total-timeout backstop stays
///   authoritative.
pub fn effective_stream_idle_timeout(prompt_tokens: Option<usize>) -> Duration {
    let base = stream_idle_timeout();
    let Some(prompt_tokens) = prompt_tokens else {
        return base;
    };
    let cap = stream_total_timeout().saturating_sub(STREAM_IDLE_TOTAL_TIMEOUT_MARGIN);
    if prompt_tokens <= STREAM_IDLE_PREFILL_SCALING_MIN_TOKENS || base >= cap {
        return base;
    }
    let extra_seconds = u64::try_from(prompt_tokens / STREAM_IDLE_PREFILL_TOKENS_PER_EXTRA_SECOND)
        .unwrap_or(u64::MAX);
    base.saturating_add(Duration::from_secs(extra_seconds))
        .min(cap)
}

pub fn stream_heartbeat() -> Duration {
    STREAM_HEARTBEAT
}

pub fn install_runtime_timeouts(timeouts: RuntimeChatTimeouts) {
    *RUNTIME_TIMEOUTS
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = timeouts;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnqueueCommandOutcome {
    Accepted,
    Duplicate,
    Full,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub enum TrajectorySourceIdentity {
    #[default]
    Normal,
    Task {
        task_id: String,
        role: String,
        agent_id: Option<String>,
        card_id: Option<String>,
        planner_chat_id: Option<String>,
    },
    Buddy,
}

impl TrajectorySourceIdentity {
    pub fn task(
        task_id: String,
        role: String,
        agent_id: Option<String>,
        card_id: Option<String>,
        planner_chat_id: Option<String>,
    ) -> Self {
        Self::Task {
            task_id,
            role,
            agent_id,
            card_id,
            planner_chat_id,
        }
    }

    pub fn from_task_meta(task_meta: &TaskMeta) -> Self {
        Self::task(
            task_meta.task_id.clone(),
            task_meta.role.clone(),
            task_meta.agent_id.clone(),
            task_meta.card_id.clone(),
            task_meta.planner_chat_id.clone(),
        )
    }

    pub fn from_extra(extra: &serde_json::Map<String, serde_json::Value>) -> Result<Self, String> {
        let buddy_meta_present = extra
            .get("buddy_meta")
            .is_some_and(|value| !value.is_null());
        let task_meta_value = extra.get("task_meta").filter(|value| !value.is_null());

        if buddy_meta_present && task_meta_value.is_some() {
            return Err("trajectory cannot contain both task_meta and buddy_meta".to_string());
        }
        if buddy_meta_present {
            return Ok(Self::Buddy);
        }
        if let Some(value) = task_meta_value {
            let task_meta = serde_json::from_value::<TaskMeta>(value.clone())
                .map_err(|e| format!("invalid task_meta: {}", e))?;
            return Ok(Self::from_task_meta(&task_meta));
        }
        Ok(Self::Normal)
    }

    pub fn from_json(json: &serde_json::Value) -> Result<Self, String> {
        let Some(root) = json.as_object() else {
            return Err("trajectory JSON root must be an object".to_string());
        };
        Self::from_extra(root)
    }

    pub fn from_session_parts(thread: &ThreadParams) -> Self {
        if thread.buddy_meta.is_some() {
            Self::Buddy
        } else if let Some(task_meta) = thread.task_meta.as_ref() {
            Self::from_task_meta(task_meta)
        } else {
            Self::Normal
        }
    }

    pub fn emits_generic_event(&self) -> bool {
        !matches!(self, Self::Buddy)
    }
}

#[derive(Debug, Clone)]
pub struct PendingBrowserMessage {
    pub pending_message_id: String,
    pub content: serde_json::Value,
    pub attachments: Vec<serde_json::Value>,
    pub client_message_id: Option<String>,
    pub checkpoints: Vec<Checkpoint>,
    pub context_files: Vec<serde_json::Value>,
    pub suppress_auto_enrichment: bool,
    pub skill_activation_name: Option<String>,
    pub skill_context_msg: Option<ChatMessage>,
}

#[derive(Debug, Clone)]
pub struct PendingSkillDeactivation {
    pub start_index: usize,
    pub report: String,
    pub skill_name: String,
    pub activation_tool_call_id: Option<String>,
}

pub fn is_segment_summary(message: &ChatMessage) -> bool {
    if message.role != "assistant" || is_ui_only_message(message) {
        return false;
    }
    message
        .extra
        .get("compression")
        .and_then(|value| value.get("kind"))
        .and_then(|value| value.as_str())
        == Some(SEGMENT_SUMMARY_KIND)
}

#[cfg(test)]
mod tests {
    use super::*;
    use refact_core::chat_types::ChatContent;
    use serde_json::json;

    #[test]
    fn effective_idle_timeout_returns_base_for_unknown_prompt_size() {
        assert_eq!(effective_stream_idle_timeout(None), stream_idle_timeout());
    }

    #[test]
    fn effective_idle_timeout_returns_base_for_small_prompt() {
        assert_eq!(
            effective_stream_idle_timeout(Some(1_000)),
            stream_idle_timeout()
        );
    }

    #[test]
    fn effective_idle_timeout_grows_for_large_prompt() {
        assert!(
            effective_stream_idle_timeout(Some(500_000)) > stream_idle_timeout(),
            "a 500k-token prompt must get more patience than the flat base timeout"
        );
    }

    #[test]
    fn effective_idle_timeout_is_monotonic_in_prompt_size() {
        let mut previous = effective_stream_idle_timeout(Some(0));
        for prompt_tokens in [
            1_000,
            8_000,
            12_000,
            64_000,
            250_000,
            500_000,
            2_000_000,
            usize::MAX / 2,
        ] {
            let current = effective_stream_idle_timeout(Some(prompt_tokens));
            assert!(
                current >= previous,
                "idle timeout must not shrink: {prompt_tokens} tokens gave {current:?} after {previous:?}"
            );
            previous = current;
        }
    }

    #[test]
    fn effective_idle_timeout_stays_below_total_timeout_for_absurd_prompt() {
        let total = stream_total_timeout();
        for prompt_tokens in [500_000, usize::MAX / 2, usize::MAX] {
            assert!(
                effective_stream_idle_timeout(Some(prompt_tokens)) < total,
                "the total-timeout backstop must stay authoritative"
            );
        }
    }

    #[test]
    fn is_segment_summary_detects_assistant_compression_kind() {
        let mut extra = serde_json::Map::new();
        extra.insert(
            "compression".to_string(),
            json!({ "kind": SEGMENT_SUMMARY_KIND }),
        );
        let summary = ChatMessage {
            role: "assistant".to_string(),
            content: ChatContent::SimpleText("summary".to_string()),
            extra,
            ..Default::default()
        };

        assert!(is_segment_summary(&summary));
    }

    #[test]
    fn is_segment_summary_rejects_ui_only_messages() {
        let mut extra = serde_json::Map::new();
        extra.insert(
            "compression".to_string(),
            json!({ "kind": SEGMENT_SUMMARY_KIND }),
        );
        extra.insert("_ui_only".to_string(), json!(true));
        let summary = ChatMessage {
            role: "assistant".to_string(),
            content: ChatContent::SimpleText("summary".to_string()),
            extra,
            ..Default::default()
        };

        assert!(!is_segment_summary(&summary));
    }
}
