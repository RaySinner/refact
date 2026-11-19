use std::sync::Arc;

use async_trait::async_trait;
use refact_buddy_core::actor::BuddyService;
use refact_buddy_core::events::DiagnosticBuddyEvent as BuddyEvent;
use refact_buddy_core::snapshot::BuddySnapshot;
use refact_buddy_core::types::{BuddyRuntimeEvent, BuddySuggestion};
use refact_buddy_core::user_action::UserAction;
use refact_buddy_core::user_activity::UserActivityRing;
use refact_chat_api::notification_event::NotificationEvent;
use refact_chat_api::ChatMessage;
use refact_chat_history::trajectory_event::TrajectoryEvent;
use refact_tasks::events::TaskEventEnvelope;
use tokio::sync::Mutex as AMutex;

use crate::global_context::{app_state_from_gcx, SharedGlobalContext};
use crate::{
    ActivitySink, BuddyEventSink, NotificationEventSender, TaskEventEnvelopeSender,
    TrajectoryEventSender, WorkflowFuture,
};

pub struct AppActivitySink {
    user_activity: Arc<AMutex<UserActivityRing>>,
}

impl AppActivitySink {
    pub fn new(user_activity: Arc<AMutex<UserActivityRing>>) -> Self {
        Self { user_activity }
    }
}

#[async_trait]
impl ActivitySink for AppActivitySink {
    async fn record_user_action(&self, action: UserAction) {
        if let Ok(mut ring) = self.user_activity.try_lock() {
            ring.push(action);
        }
    }
}

pub struct AppBuddyEventSink {
    gcx: SharedGlobalContext,
    buddy: Arc<AMutex<Option<BuddyService>>>,
}

impl AppBuddyEventSink {
    pub fn new(gcx: SharedGlobalContext, buddy: Arc<AMutex<Option<BuddyService>>>) -> Self {
        Self { gcx, buddy }
    }
}

#[async_trait]
impl BuddyEventSink for AppBuddyEventSink {
    async fn enqueue_event(&self, event: BuddyRuntimeEvent) {
        let buddy_arc = self.buddy.clone();
        let mut lock = buddy_arc.lock().await;
        if let Some(svc) = lock.as_mut() {
            svc.enqueue_runtime_event(event);
        }
    }

    async fn complete_event(&self, dedupe_key: &str, status: &str) {
        let buddy_arc = self.buddy.clone();
        let mut lock = buddy_arc.lock().await;
        if let Some(svc) = lock.as_mut() {
            svc.complete_runtime_event(dedupe_key, status);
        }
    }

    async fn snapshot(&self) -> Option<BuddySnapshot> {
        let buddy_arc = self.buddy.clone();
        let lock = buddy_arc.lock().await;
        lock.as_ref().map(|svc| svc.snapshot())
    }

    async fn apply_chat_completion(&self, event: BuddyRuntimeEvent, xp: u64, mood: String) {
        let buddy_arc = self.buddy.clone();
        let mut lock = buddy_arc.lock().await;
        let Some(svc) = lock.as_mut() else { return };
        svc.enqueue_runtime_event(event);
        if xp > 0 {
            svc.grant_xp(xp);
        }
        svc.state.semantic.mood = mood;
        svc.dirty = true;
        let _ = svc.events_tx.send(BuddyEvent::StateUpdated {
            state: svc.state.clone(),
        });
    }

    async fn report_error(
        &self,
        error_type: &str,
        error_msg: &str,
        source: Option<&str>,
        chat_id: Option<&str>,
    ) {
        let buddy_arc = self.buddy.clone();
        let mut lock = buddy_arc.lock().await;
        if let Some(svc) = lock.as_mut() {
            svc.report_error(error_type, error_msg, source, chat_id);
        }
    }

    async fn report_error_with_model(
        &self,
        error_type: &str,
        error_msg: &str,
        source: Option<&str>,
        chat_id: Option<&str>,
        model_id: Option<&str>,
    ) {
        let buddy_arc = self.buddy.clone();
        let mut lock = buddy_arc.lock().await;
        if let Some(svc) = lock.as_mut() {
            svc.report_error_with_model(error_type, error_msg, source, chat_id, model_id);
        }
    }

    async fn mark_chat_error(&self, event: BuddyRuntimeEvent) {
        let buddy_arc = self.buddy.clone();
        let mut lock = buddy_arc.lock().await;
        if let Some(svc) = lock.as_mut() {
            svc.enqueue_runtime_event(event);
            svc.state.semantic.mood = "worried".to_string();
            svc.dirty = true;
            let _ = svc.events_tx.send(BuddyEvent::StateUpdated {
                state: svc.state.clone(),
            });
        }
    }

    async fn maybe_add_suggestion(&self, suggestion: BuddySuggestion) {
        let buddy_arc = self.buddy.clone();
        let mut lock = buddy_arc.lock().await;
        if let Some(svc) = lock.as_mut() {
            svc.maybe_add_suggestion(suggestion);
        }
    }

    async fn run_workflow(
        &self,
        workflow_id: &str,
        icon: &str,
        xp: u64,
        summary_prefix: &str,
        summary_max_chars: usize,
        work: Box<dyn FnOnce() -> WorkflowFuture + Send + 'static>,
    ) -> Result<String, String> {
        let app = app_state_from_gcx(self.gcx.clone()).await;
        let summary_prefix = summary_prefix.to_string();
        crate::buddy_workflows::buddy_wrap_workflow(
            app,
            workflow_id,
            icon,
            xp,
            move |msg: &String| {
                let short: String = msg
                    .lines()
                    .next()
                    .unwrap_or("")
                    .chars()
                    .take(summary_max_chars)
                    .collect();
                format!("{}: {}", summary_prefix, short)
            },
            move || async move { work().await },
        )
        .await
    }

    async fn render_autonomous_report_title(
        &self,
        workflow_id: &str,
        workflow_summary: String,
        fallback_title: String,
    ) -> String {
        let app = app_state_from_gcx(self.gcx.clone()).await;
        let Some(snapshot) = self.snapshot().await else {
            return fallback_title;
        };
        crate::buddy_speech_runtime::render_buddy_activity_title(
            app,
            snapshot.state.personality,
            snapshot.state.identity.name,
            snapshot.pulse,
            Some(workflow_id.to_string()),
            workflow_summary,
            crate::buddy_voice_service::VoiceIntent::AutonomousReportSaved,
            fallback_title,
        )
        .await
    }

    async fn render_autonomous_result_summary(
        &self,
        workflow_id: &str,
        workflow_summary: String,
        fallback_title: String,
        fallback_description: String,
    ) -> Option<(String, Option<String>)> {
        let app = app_state_from_gcx(self.gcx.clone()).await;
        let snapshot = self.snapshot().await?;
        Some(
            crate::buddy_speech_runtime::render_buddy_runtime_event(
                app,
                snapshot.state.personality,
                snapshot.state.identity.name,
                snapshot.pulse,
                Some(workflow_id.to_string()),
                workflow_summary,
                "completed",
                fallback_title,
                Some(fallback_description),
            )
            .await,
        )
    }

    async fn render_runtime_event_fast(
        &self,
        workflow_id: &str,
        workflow_summary: &str,
        status: &str,
    ) -> Option<(String, Option<String>)> {
        let app = app_state_from_gcx(self.gcx.clone()).await;
        let snapshot = self.snapshot().await?;
        let pulse_one_liner = format!(
            "{} pending ops, {} recent stuck task alerts",
            snapshot.pulse.memory.pending_ops,
            snapshot.pulse.tasks.recent_stuck_alert_count_1h()
        );
        let voice_ctx = crate::buddy_voice_service::VoiceCtx {
            persona: &snapshot.state.personality,
            identity_name: snapshot.state.identity.name.as_str(),
            pulse_one_liner,
            workflow_id: Some(workflow_id),
            workflow_summary: Some(workflow_summary),
        };
        Some(
            crate::buddy_voice_service::voice_service()
                .await
                .render_runtime_event_fast(app, voice_ctx, status)
                .await,
        )
    }

    async fn build_pulse_message(&self) -> Option<ChatMessage> {
        crate::buddy_pulse_inject::build_buddy_pulse_message(
            app_state_from_gcx(self.gcx.clone()).await,
        )
        .await
    }
}

pub struct EngineTrajectoryEventSender {
    tx: tokio::sync::broadcast::Sender<TrajectoryEvent>,
}

impl EngineTrajectoryEventSender {
    pub fn new(tx: tokio::sync::broadcast::Sender<TrajectoryEvent>) -> Self {
        Self { tx }
    }
}

impl TrajectoryEventSender for EngineTrajectoryEventSender {
    fn send(&self, event: TrajectoryEvent) -> usize {
        self.tx.send(event).unwrap_or(0)
    }
}

pub struct EngineTaskEventEnvelopeSender {
    tx: tokio::sync::broadcast::Sender<TaskEventEnvelope>,
}

impl EngineTaskEventEnvelopeSender {
    pub fn new(tx: tokio::sync::broadcast::Sender<TaskEventEnvelope>) -> Self {
        Self { tx }
    }
}

impl TaskEventEnvelopeSender for EngineTaskEventEnvelopeSender {
    fn send(&self, envelope: TaskEventEnvelope) -> usize {
        self.tx.send(envelope).unwrap_or(0)
    }
}

pub struct EngineNotificationEventSender {
    tx: tokio::sync::broadcast::Sender<NotificationEvent>,
}

impl EngineNotificationEventSender {
    pub fn new(tx: tokio::sync::broadcast::Sender<NotificationEvent>) -> Self {
        Self { tx }
    }
}

impl NotificationEventSender for EngineNotificationEventSender {
    fn send(&self, event: NotificationEvent) -> usize {
        self.tx.send(event).unwrap_or(0)
    }
}
