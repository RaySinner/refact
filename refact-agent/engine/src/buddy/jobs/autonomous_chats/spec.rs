use super::*;

#[cfg_attr(not(test), allow(dead_code))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutonomousBuddyChatSpec {
    pub workflow_id: String,
    pub title: String,
    pub prompt: String,
    pub evidence: String,
    pub project_root: String,
    pub signal_hash: String,
    pub icon: String,
    pub badge: String,
    pub priority: String,
}

#[cfg_attr(not(test), allow(dead_code))]
impl AutonomousBuddyChatSpec {
    pub fn new(
        workflow_id: impl Into<String>,
        title: impl Into<String>,
        prompt: impl Into<String>,
        evidence: impl Into<String>,
    ) -> Self {
        let workflow_id = workflow_id.into();
        let title = title.into();
        let prompt = prompt.into();
        let evidence = evidence.into();
        let signal_hash = default_signal_hash(&workflow_id, &title, &prompt, &evidence);
        Self {
            workflow_id,
            title,
            prompt,
            evidence,
            project_root: String::new(),
            signal_hash,
            icon: String::new(),
            badge: String::new(),
            priority: "normal".to_string(),
        }
    }

    pub fn with_project_root(mut self, project_root: impl Into<String>) -> Self {
        self.project_root = project_root.into();
        self
    }

    /// Override the auto-derived signal hash with a stable one (used for diagnostic clusters
    /// so volatile timestamp/count churn does not re-trigger the same investigation).
    pub fn with_signal_hash(mut self, signal_hash: impl Into<String>) -> Self {
        self.signal_hash = signal_hash.into();
        self
    }

    pub fn diagnostic_memory_key(&self) -> Option<String> {
        if self.workflow_id == ERROR_DETECTIVE_WORKFLOW_ID && self.signal_hash.len() >= 16 {
            Some(format!("diag:signal:{}", &self.signal_hash[..16]))
        } else {
            None
        }
    }

    pub fn with_display(
        mut self,
        icon: impl Into<String>,
        badge: impl Into<String>,
        priority: impl Into<String>,
    ) -> Self {
        self.icon = icon.into();
        self.badge = badge.into();
        self.priority = priority.into();
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(not(test), allow(dead_code))]
pub struct AutonomousLastResult {
    pub signal_hash: String,
    pub chat_id: String,
    pub completed_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_category: Option<WorkflowFailureCategory>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub consecutive_failures: Option<u32>,
}

#[cfg_attr(not(test), allow(dead_code))]
impl AutonomousLastResult {
    pub fn new(signal_hash: impl Into<String>, chat_id: impl Into<String>) -> Self {
        Self {
            signal_hash: signal_hash.into(),
            chat_id: chat_id.into(),
            completed_at: Utc::now().to_rfc3339(),
            status: None,
            failure_category: None,
            consecutive_failures: None,
        }
    }
}

#[cfg_attr(not(test), allow(dead_code))]
pub fn signal_hash<I, S>(parts: I) -> String
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut hasher = Sha256::new();
    for part in parts {
        let text = part.as_ref();
        hasher.update(text.len().to_string().as_bytes());
        hasher.update(b"\0");
        hasher.update(text.as_bytes());
        hasher.update(b"\0");
    }
    hex::encode(hasher.finalize())
}

pub(super) fn memory_key_instruction(memory_key: &str) -> String {
    format!(
        "Memory idempotency key: {}\n{}",
        memory_key, MEMORY_KEY_SAVE_INSTRUCTION
    )
}

#[cfg_attr(not(test), allow(dead_code))]
pub(super) fn default_signal_hash(
    workflow_id: &str,
    title: &str,
    prompt: &str,
    evidence: &str,
) -> String {
    let prompt = redact_and_cap_prompt(prompt);
    let evidence = redact_and_cap_evidence(evidence);
    signal_hash([workflow_id, title, prompt.as_str(), evidence.as_str()])
}

#[cfg_attr(not(test), allow(dead_code))]
pub fn parse_last_autonomous_result(raw: Option<&str>) -> Option<AutonomousLastResult> {
    let raw = raw?.trim();
    if raw.is_empty() {
        return None;
    }
    let parsed = serde_json::from_str::<AutonomousLastResult>(raw).ok()?;
    if parsed.signal_hash.is_empty() || parsed.chat_id.is_empty() || parsed.completed_at.is_empty()
    {
        return None;
    }
    Some(parsed)
}

#[cfg_attr(not(test), allow(dead_code))]
pub fn serialize_last_autonomous_result(result: &AutonomousLastResult) -> String {
    serde_json::json!({
        "signal_hash": result.signal_hash,
        "chat_id": result.chat_id,
        "completed_at": result.completed_at,
        "status": result.status,
        "failure_category": result.failure_category,
        "consecutive_failures": result.consecutive_failures,
    })
    .to_string()
}

#[cfg_attr(not(test), allow(dead_code))]
pub(super) fn autonomous_failure_backoff_secs(
    cooldown_seconds: u64,
    consecutive_failures: u32,
) -> u64 {
    let exponent = consecutive_failures.saturating_sub(1).min(6);
    let multiplier = 1u64.checked_shl(exponent).unwrap_or(64).max(1);
    cooldown_seconds
        .saturating_mul(multiplier)
        .min(24 * 60 * 60)
}

#[cfg_attr(not(test), allow(dead_code))]
pub(super) fn failure_backoff_active(last: &AutonomousLastResult, cooldown_seconds: u64) -> bool {
    if last.status.as_deref() != Some("failed") {
        return false;
    }
    let consecutive = last.consecutive_failures.unwrap_or(1).max(1);
    let backoff = autonomous_failure_backoff_secs(cooldown_seconds, consecutive);
    chrono::DateTime::parse_from_rfc3339(&last.completed_at)
        .map(|t| {
            Utc::now()
                .signed_duration_since(t.with_timezone(&Utc))
                .num_seconds()
                .max(0) as u64
                <= backoff
        })
        .unwrap_or(false)
}

#[cfg_attr(not(test), allow(dead_code))]
pub fn same_signal(ctx: &BuddyJobContext, hash: &str) -> bool {
    parse_last_autonomous_result(ctx.job_state.last_result.as_deref())
        .map(|last| last.signal_hash == hash && last.status.as_deref() != Some("failed"))
        .unwrap_or(false)
}

#[cfg_attr(not(test), allow(dead_code))]
pub(super) fn same_signal_or_failed_backoff(
    ctx: &BuddyJobContext,
    hash: &str,
    cooldown_seconds: u64,
) -> bool {
    parse_last_autonomous_result(ctx.job_state.last_result.as_deref())
        .map(|last| {
            if last.status.as_deref() == Some("failed") {
                return failure_backoff_active(&last, cooldown_seconds);
            }
            last.signal_hash == hash
        })
        .unwrap_or(false)
}

pub(super) fn preserve_last_result(ctx: &BuddyJobContext) -> BuddyJobResult {
    BuddyJobResult {
        last_result: ctx.job_state.last_result.clone(),
        ..Default::default()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct AutonomousEvidence {
    pub(super) prompt: String,
    pub(super) evidence: String,
}
