use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use refact_buddy_core::supervision::{
    GoalFacts, MessageBody, MessageRole, PlanFacts, ProcessFacts, ProcessState, ProviderState,
    StreamFacts, StreamState, SupervisionEvent, SupervisionKind, SupervisionProjection,
    SupervisionReference,
};
use refact_buddy_core::supervision_detectors::{
    detect_supervision_candidates, SupervisionDetectorConfig,
};
use refact_buddy_core::supervision_journal::SupervisionJournal;
use refact_chat_api::diagnostics::safe_provider_error_diagnostic;
use refact_chat_api::types::{ChatMessage, ChatSession, SessionState};
use refact_chat_api::{GoalAutonomy, GoalSnapshot};
use refact_core::retry_policy::{classify_user_error, UserErrorCategory};
use tokio::sync::Mutex as AMutex;

const MAX_SUPERVISION_CHATS: usize = 64;
const MAX_MESSAGE_IDS: usize = 128;

#[derive(Clone)]
pub struct BuddySupervisionCollector {
    states: Arc<AMutex<HashMap<String, ChatSupervisionState>>>,
    journal: SupervisionJournal,
    config: SupervisionDetectorConfig,
}

struct ChatSupervisionState {
    projection: SupervisionProjection,
    next_seq: u64,
    message_ids: VecDeque<String>,
    goal: Option<GoalFacts>,
    plan: Option<PlanFacts>,
    task: Option<SupervisionReference>,
    runtime: Option<SupervisionKind>,
    candidate_keys: VecDeque<String>,
}

impl ChatSupervisionState {
    fn new(chat_id: &str) -> Self {
        Self {
            projection: SupervisionProjection::new(chat_id),
            next_seq: 1,
            message_ids: VecDeque::new(),
            goal: None,
            plan: None,
            task: None,
            runtime: None,
            candidate_keys: VecDeque::new(),
        }
    }

    fn remember_message(&mut self, message_id: &str) -> bool {
        if self.message_ids.iter().any(|id| id == message_id) {
            return false;
        }
        self.message_ids.push_back(message_id.to_string());
        if self.message_ids.len() > MAX_MESSAGE_IDS {
            self.message_ids.pop_front();
        }
        true
    }

    fn remember_candidate(
        &mut self,
        record: &refact_buddy_core::supervision::InterventionRecord,
    ) -> bool {
        let key = format!(
            "{:?}:{}:{:?}",
            record.candidate,
            String::from(record.evidence.hash().clone()),
            record.verdict
        );
        if self.candidate_keys.iter().any(|existing| existing == &key) {
            return false;
        }
        self.candidate_keys.push_back(key);
        if self.candidate_keys.len() > MAX_MESSAGE_IDS {
            self.candidate_keys.pop_front();
        }
        true
    }
}

fn candidates_for_state(
    state: &mut ChatSupervisionState,
    config: SupervisionDetectorConfig,
    now: u64,
) -> Vec<refact_buddy_core::supervision::InterventionRecord> {
    detect_supervision_candidates(&state.projection, config, now)
        .into_iter()
        .filter(|candidate| state.remember_candidate(candidate))
        .collect()
}

#[derive(Clone)]
pub struct SessionSupervisionSnapshot {
    pub chat_id: String,
    pub messages: Vec<ChatMessage>,
    pub goal: Option<GoalSnapshot>,
    pub plan: Option<PlanFacts>,
    pub task: Option<SupervisionReference>,
    pub runtime: Option<SupervisionKind>,
}

pub fn capture_session_supervision(session: &ChatSession, now: u64) -> SessionSupervisionSnapshot {
    let goal = session.goal.clone();
    let plan =
        refact_chat_api::session_plan_role::current_base_plan(session).map(|plan| PlanFacts {
            reference: SupervisionReference::new(&plan.message_id),
            version: plan.extra["plan"]["version"].as_u64().unwrap_or(0),
            has_pending: plan
                .content
                .content_text_only()
                .lines()
                .any(|line| line.trim_start().starts_with("- [ ]")),
        });
    let task =
        session.thread.task_meta.as_ref().map(|meta| {
            SupervisionReference::new(meta.card_id.as_deref().unwrap_or(&meta.task_id))
        });
    let runtime = match session.runtime.state {
        SessionState::Generating => Some(SupervisionKind::Stream {
            facts: StreamFacts {
                state: StreamState::Streaming,
                provider: ProviderState::Ready,
            },
            error: None,
        }),
        SessionState::ExecutingTools => Some(SupervisionKind::Process {
            facts: ProcessFacts {
                process_id: SupervisionReference::new("chat-tools"),
                state: ProcessState::Running,
                heartbeat_at: now,
            },
        }),
        SessionState::Paused => Some(SupervisionKind::Process {
            facts: ProcessFacts {
                process_id: SupervisionReference::new("chat-runtime"),
                state: ProcessState::WaitingApproval,
                heartbeat_at: now,
            },
        }),
        SessionState::WaitingUserInput | SessionState::WaitingIde => {
            Some(SupervisionKind::Process {
                facts: ProcessFacts {
                    process_id: SupervisionReference::new("chat-runtime"),
                    state: ProcessState::WaitingInput,
                    heartbeat_at: now,
                },
            })
        }
        SessionState::Idle | SessionState::Starting | SessionState::Completed => {
            Some(SupervisionKind::Process {
                facts: ProcessFacts {
                    process_id: SupervisionReference::new("chat-runtime"),
                    state: ProcessState::Finished,
                    heartbeat_at: now,
                },
            })
        }
        SessionState::Error => None,
    };
    SessionSupervisionSnapshot {
        chat_id: session.chat_id.clone(),
        messages: session.messages.clone(),
        goal,
        plan,
        task,
        runtime,
    }
}

impl BuddySupervisionCollector {
    pub fn new(project_root: impl Into<PathBuf>) -> Self {
        Self {
            states: Arc::new(AMutex::new(HashMap::new())),
            journal: SupervisionJournal::new(project_root),
            config: SupervisionDetectorConfig::default(),
        }
    }

    pub fn project_root(project_root: &Path, fallback: &Path) -> PathBuf {
        if project_root.as_os_str().is_empty() {
            fallback.to_path_buf()
        } else {
            project_root.to_path_buf()
        }
    }

    pub async fn observe(&self, chat_id: &str, kind: SupervisionKind) -> Result<(), String> {
        self.observe_at(chat_id, kind, epoch_secs()).await
    }

    pub async fn observe_at(
        &self,
        chat_id: &str,
        kind: SupervisionKind,
        now: u64,
    ) -> Result<(), String> {
        let candidates = {
            let mut states = self.states.lock().await;
            if !states.contains_key(chat_id) && states.len() >= MAX_SUPERVISION_CHATS {
                if let Some(oldest) = states.keys().next().cloned() {
                    states.remove(&oldest);
                }
            }
            let state = states
                .entry(chat_id.to_string())
                .or_insert_with(|| ChatSupervisionState::new(chat_id));
            let seq = state.next_seq;
            let event = SupervisionEvent::new(seq, chat_id, now, kind);
            state
                .projection
                .append(event)
                .map_err(|error| error.to_string())?;
            state.next_seq = state.next_seq.saturating_add(1);
            candidates_for_state(state, self.config, now)
        };
        for candidate in candidates {
            self.journal.append(candidate).await?;
        }
        Ok(())
    }

    pub async fn observe_non_fatal(&self, chat_id: &str, kind: SupervisionKind) {
        if let Err(error) = self.observe(chat_id, kind).await {
            tracing::warn!("buddy supervision collector ignored failure: {error}");
        }
    }

    pub async fn observe_session(
        &self,
        snapshot: SessionSupervisionSnapshot,
    ) -> Result<(), String> {
        self.observe_session_at(snapshot, epoch_secs()).await
    }

    pub async fn observe_session_at(
        &self,
        snapshot: SessionSupervisionSnapshot,
        now: u64,
    ) -> Result<(), String> {
        let mut pending = Vec::new();
        {
            let mut states = self.states.lock().await;
            if !states.contains_key(&snapshot.chat_id) && states.len() >= MAX_SUPERVISION_CHATS {
                if let Some(oldest) = states.keys().next().cloned() {
                    states.remove(&oldest);
                }
            }
            let state = states
                .entry(snapshot.chat_id.clone())
                .or_insert_with(|| ChatSupervisionState::new(&snapshot.chat_id));
            let message_start = snapshot.messages.len().saturating_sub(MAX_MESSAGE_IDS);
            for message in &snapshot.messages[message_start..] {
                if !state.remember_message(&message.message_id) {
                    continue;
                }
                let Some(kind) = message_supervision_kind(message) else {
                    continue;
                };
                pending.push(kind);
            }
            let goal = snapshot.goal.as_ref().map(goal_facts);
            if state.goal != goal {
                state.goal = goal.clone();
                pending.push(SupervisionKind::Goal { facts: goal });
            }
            if state.plan != snapshot.plan {
                state.plan = snapshot.plan.clone();
                pending.push(SupervisionKind::Plan {
                    facts: snapshot.plan.clone(),
                });
            }
            if state.task != snapshot.task {
                state.task = snapshot.task.clone();
                pending.push(SupervisionKind::Task {
                    reference: snapshot.task.clone(),
                });
            }
            if state.runtime != snapshot.runtime {
                if let Some(runtime) = snapshot.runtime.clone() {
                    state.runtime = Some(runtime.clone());
                    pending.push(runtime);
                }
            }
        }
        for kind in pending {
            self.observe_at(&snapshot.chat_id, kind, now).await?;
        }
        Ok(())
    }

    pub async fn observe_session_non_fatal(&self, snapshot: SessionSupervisionSnapshot) {
        if let Err(error) = self.observe_session(snapshot).await {
            tracing::warn!("buddy supervision session collector ignored failure: {error}");
        }
    }

    pub async fn projection(&self, chat_id: &str) -> Option<SupervisionProjection> {
        self.states
            .lock()
            .await
            .get(chat_id)
            .map(|state| state.projection.clone())
    }

    pub async fn evaluate_all_at(&self, now: u64) -> Result<(), String> {
        let candidates = {
            let mut states = self.states.lock().await;
            states
                .values_mut()
                .flat_map(|state| candidates_for_state(state, self.config, now))
                .collect::<Vec<_>>()
        };
        for candidate in candidates {
            self.journal.append(candidate).await?;
        }
        Ok(())
    }

    pub async fn evaluate_all_non_fatal(&self) {
        if let Err(error) = self.evaluate_all_at(epoch_secs()).await {
            tracing::warn!("buddy supervision evaluation ignored failure: {error}");
        }
    }
}

fn message_supervision_kind(message: &ChatMessage) -> Option<SupervisionKind> {
    if let Some(event) = message.extra.get("event") {
        let source = event
            .get("source")
            .and_then(|source| source.as_str())
            .unwrap_or("");
        if source.starts_with("buddy") {
            return Some(SupervisionKind::Intervention {
                delivery_reference: SupervisionReference::new(&message.message_id),
                message_id: Some(SupervisionReference::new(&message.message_id)),
            });
        }
    }
    let role = match message.role.as_str() {
        "user" => MessageRole::User,
        "assistant" => MessageRole::Assistant,
        "event" => MessageRole::SystemNotice,
        _ => return None,
    };
    let body = message.content.content_text_only();
    if body.trim().is_empty() {
        return None;
    }
    Some(SupervisionKind::Message {
        role,
        message_id: SupervisionReference::new(&message.message_id),
        body: Some(MessageBody::new(&body)),
    })
}

fn goal_facts(goal: &GoalSnapshot) -> GoalFacts {
    GoalFacts {
        revision: goal.revision,
        active: goal.active,
        autonomy: goal.autonomy == GoalAutonomy::On,
        prompt_enabled: goal.prompt_enabled,
        can_pursue: goal.can_pursue,
    }
}

pub fn provider_error_fingerprint(error: &str) -> refact_buddy_core::supervision::ErrorFingerprint {
    let redacted = safe_provider_error_diagnostic(error);
    let category = match classify_user_error(&redacted) {
        UserErrorCategory::NetworkFailure => {
            refact_buddy_core::supervision::ErrorCategory::Transport
        }
        UserErrorCategory::StreamCorrupted => {
            refact_buddy_core::supervision::ErrorCategory::StreamCorrupted
        }
        UserErrorCategory::ProviderRateLimit => {
            refact_buddy_core::supervision::ErrorCategory::RateLimited
        }
        UserErrorCategory::AuthenticationFailed => {
            refact_buddy_core::supervision::ErrorCategory::Authentication
        }
        UserErrorCategory::InvalidRequest => {
            refact_buddy_core::supervision::ErrorCategory::InvalidRequest
        }
        UserErrorCategory::ModelUnavailable => {
            refact_buddy_core::supervision::ErrorCategory::Unavailable
        }
        UserErrorCategory::ToolSchemaInvalid
        | UserErrorCategory::ContextTooLarge
        | UserErrorCategory::BillingQuota
        | UserErrorCategory::ContentPolicy => refact_buddy_core::supervision::ErrorCategory::Other,
        UserErrorCategory::ProviderTransient => {
            if redacted.to_lowercase().contains("timeout") {
                refact_buddy_core::supervision::ErrorCategory::Timeout
            } else {
                refact_buddy_core::supervision::ErrorCategory::Unavailable
            }
        }
        UserErrorCategory::Unknown => refact_buddy_core::supervision::ErrorCategory::Other,
    };
    refact_buddy_core::supervision::ErrorFingerprint {
        category,
        http_status: None,
    }
}

pub fn provider_stream_kind(error: &str, retry_scheduled: bool) -> SupervisionKind {
    let fingerprint = provider_error_fingerprint(error);
    let disconnected = matches!(
        fingerprint.category,
        refact_buddy_core::supervision::ErrorCategory::Transport
            | refact_buddy_core::supervision::ErrorCategory::StreamCorrupted
    );
    SupervisionKind::Stream {
        facts: StreamFacts {
            state: if retry_scheduled {
                StreamState::Idle
            } else {
                StreamState::Failed
            },
            provider: if retry_scheduled {
                ProviderState::RetryScheduled
            } else if disconnected {
                ProviderState::Disconnected
            } else {
                ProviderState::Failed
            },
        },
        error: Some(fingerprint),
    }
}

pub async fn collect_session_boundary(
    app: &crate::app_state::AppState,
    session_arc: &Arc<AMutex<ChatSession>>,
) {
    let snapshot = {
        let session = session_arc.lock().await;
        capture_session_supervision(&session, epoch_secs())
    };
    app.buddy
        .supervision
        .observe_session_non_fatal(snapshot)
        .await;
}

pub async fn collect_provider_error(
    app: &crate::app_state::AppState,
    chat_id: &str,
    error: &str,
    retry_scheduled: bool,
) {
    app.buddy
        .supervision
        .observe_non_fatal(chat_id, provider_stream_kind(error, retry_scheduled))
        .await;
}

pub async fn collect_intervention(
    app: &crate::app_state::AppState,
    chat_id: &str,
    delivery_reference: &str,
    message_id: Option<&str>,
) {
    app.buddy
        .supervision
        .observe_non_fatal(
            chat_id,
            SupervisionKind::Intervention {
                delivery_reference: SupervisionReference::new(delivery_reference),
                message_id: message_id.map(SupervisionReference::new),
            },
        )
        .await;
}

pub fn epoch_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use refact_buddy_core::supervision::{EventOrigin, InterventionCandidate, InterventionDelivery};
    use refact_buddy_core::supervision_journal::SupervisionJournal;

    fn message(id: &str, role: &str, body: &str) -> ChatMessage {
        ChatMessage {
            message_id: id.to_string(),
            role: role.to_string(),
            content: refact_core::chat_types::ChatContent::SimpleText(body.to_string()),
            ..Default::default()
        }
    }

    fn snapshot(chat_id: &str, messages: Vec<ChatMessage>) -> SessionSupervisionSnapshot {
        SessionSupervisionSnapshot {
            chat_id: chat_id.to_string(),
            messages,
            goal: None,
            plan: None,
            task: None,
            runtime: None,
        }
    }

    #[tokio::test]
    async fn buddy_supervision_collector_appends_real_messages_and_shadow_candidates() {
        let dir = tempfile::tempdir().unwrap();
        let collector = BuddySupervisionCollector::new(dir.path());
        collector
            .observe_session_at(
                snapshot("chat-1", vec![message("m1", "user", "inspect")]),
                1,
            )
            .await
            .unwrap();
        let goal = GoalSnapshot {
            active: true,
            revision: 2,
            prompt_enabled: true,
            ..Default::default()
        };
        let mut completed = snapshot(
            "chat-1",
            vec![
                message("m1", "user", "inspect"),
                message("m2", "assistant", "working"),
            ],
        );
        completed.goal = Some(goal);
        collector
            .observe_session_at(completed.clone(), 2)
            .await
            .unwrap();
        collector.observe_session_at(completed, 3).await.unwrap();
        let projection = collector.projection("chat-1").await.unwrap();
        assert_eq!(
            projection
                .detection_events()
                .filter(|event| matches!(event.data, SupervisionKind::Message { .. }))
                .count(),
            2
        );
        let journal = SupervisionJournal::new(dir.path()).load().await.unwrap();
        assert!(journal
            .records()
            .iter()
            .any(|record| record.candidate == InterventionCandidate::OffGoal));
        assert!(journal
            .records()
            .iter()
            .all(|record| !record.evidence.ids().is_empty()));
        assert!(journal
            .records()
            .iter()
            .all(|record| matches!(record.delivery, InterventionDelivery::Suppressed { .. })));
    }

    #[tokio::test]
    async fn buddy_supervision_stagnation_evaluation_needs_no_new_event() {
        let dir = tempfile::tempdir().unwrap();
        let collector = BuddySupervisionCollector::new(dir.path());
        collector
            .observe_session_at(
                snapshot("chat-1", vec![message("m1", "assistant", "last update")]),
                1,
            )
            .await
            .unwrap();
        collector.evaluate_all_at(1_000).await.unwrap();
        let journal = SupervisionJournal::new(dir.path()).load().await.unwrap();
        assert!(journal.records().iter().any(|record| {
            record.candidate == InterventionCandidate::Stagnation
                && record.evidence.ids()[0].seq == 1
        }));
        collector.evaluate_all_at(1_001).await.unwrap();
        assert_eq!(
            SupervisionJournal::new(dir.path())
                .load()
                .await
                .unwrap()
                .records()
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn buddy_supervision_collector_failure_is_non_fatal_and_projection_survives() {
        let dir = tempfile::tempdir().unwrap();
        let blocked = dir.path().join("blocked");
        tokio::fs::write(&blocked, b"not-a-directory")
            .await
            .unwrap();
        let collector = BuddySupervisionCollector::new(&blocked);
        let terminal = provider_stream_kind("connection closed by remote host", false);
        collector
            .observe_non_fatal("chat-1", terminal.clone())
            .await;
        collector.observe_non_fatal("chat-1", terminal).await;
        assert_eq!(
            collector
                .projection("chat-1")
                .await
                .unwrap()
                .events()
                .count(),
            2
        );
        collector
            .observe_session_non_fatal(snapshot(
                "chat-1",
                vec![message("secret", "assistant", "token=private-canary")],
            ))
            .await;
        let serialized = collector
            .projection("chat-1")
            .await
            .unwrap()
            .snapshot()
            .unwrap();
        assert!(!serialized.contains("private-canary"));
    }

    #[test]
    fn buddy_supervision_provider_classification_uses_redacted_terminal_categories() {
        let retry = provider_stream_kind("503 provider unavailable token=secret", true);
        assert!(matches!(
            retry,
            SupervisionKind::Stream {
                facts: StreamFacts {
                    provider: ProviderState::RetryScheduled,
                    ..
                },
                ..
            }
        ));
        let terminal = provider_stream_kind("connection closed by remote host token=secret", false);
        let serialized = serde_json::to_string(&terminal).unwrap();
        assert!(!serialized.contains("secret"));
        assert!(matches!(
            terminal,
            SupervisionKind::Stream {
                facts: StreamFacts {
                    provider: ProviderState::Disconnected,
                    ..
                },
                ..
            }
        ));
        assert_eq!(
            provider_error_fingerprint("context limit exceeded").category,
            refact_buddy_core::supervision::ErrorCategory::Other
        );
    }

    #[test]
    fn buddy_supervision_own_intervention_is_marked() {
        let mut own = message("notice-1", "user", "keep going");
        own.extra.insert(
            "event".to_string(),
            serde_json::json!({"source": "buddy.stalled_turn"}),
        );
        assert!(matches!(
            message_supervision_kind(&own),
            Some(SupervisionKind::Intervention { .. })
        ));
        let event = SupervisionEvent::new(1, "chat-1", 1, message_supervision_kind(&own).unwrap());
        assert_eq!(event.origin, EventOrigin::Buddy);
    }
}
