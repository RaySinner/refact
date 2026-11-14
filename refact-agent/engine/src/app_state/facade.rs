#[allow(unused_imports)]
use super::*;

pub struct EngineBuddyServicesHandle(pub BuddyServices);

impl BuddyServicesHandle for EngineBuddyServicesHandle {
    fn as_any(&self) -> &dyn std::any::Any {
        &self.0
    }
}

#[derive(Clone)]
pub struct EngineChatSessionFacade {
    gcx: SharedGlobalContext,
}

impl EngineChatSessionFacade {
    pub fn new(gcx: SharedGlobalContext) -> Self {
        Self { gcx }
    }

    async fn enqueue_command(
        &self,
        chat_id: &str,
        command: refact_chat_api::ChatCommand,
        priority: bool,
    ) -> Result<(), String> {
        let app = app_state_from_gcx(self.gcx.clone()).await;
        match command {
            refact_chat_api::ChatCommand::DeliverMessages { delivery } => {
                return chat::deliver_to_chat(app, chat_id, delivery)
                    .await
                    .map(|_| ());
            }
            refact_chat_api::ChatCommand::UpdatePendingDelivery {
                delivery_id,
                push,
                cancel,
            } => {
                return chat::update_pending_delivery_in_chat(
                    app,
                    chat_id,
                    &delivery_id,
                    push,
                    cancel,
                )
                .await;
            }
            _ => {}
        }
        let session_arc = chat::get_or_create_session_with_trajectory(
            app.chat_ctx(),
            &self.gcx.chat_sessions,
            chat_id,
        )
        .await;
        let mut session = session_arc.lock().await;
        let request = refact_chat_api::CommandRequest {
            client_request_id: uuid::Uuid::new_v4().to_string(),
            priority,
            command,
        };
        let enqueue_outcome = session.enqueue_command(request);
        if enqueue_outcome == EnqueueCommandOutcome::Full {
            return Err("chat command queue is full".to_string());
        }
        let processor_running = session.queue_processor_running.clone();
        let queue_notify = session.queue_notify.clone();
        drop(session);
        if !processor_running.swap(true, Ordering::SeqCst) {
            tokio::spawn(process_command_queue(app, session_arc, processor_running));
        } else {
            queue_notify.notify_one();
        }
        Ok(())
    }
}

#[async_trait]
impl ChatSessionFacade for EngineChatSessionFacade {
    async fn deliver_messages(
        &self,
        chat_id: &str,
        delivery: refact_core::chat_types::PendingDelivery,
    ) -> Result<refact_core::chat_types::DeliveryOutcome, String> {
        chat::deliver_to_chat(
            app_state_from_gcx(self.gcx.clone()).await,
            chat_id,
            delivery,
        )
        .await
    }

    async fn update_pending_delivery(
        &self,
        chat_id: &str,
        delivery_id: &str,
        push: Option<refact_core::chat_types::PushMode>,
        cancel: bool,
    ) -> Result<(), String> {
        chat::update_pending_delivery_in_chat(
            app_state_from_gcx(self.gcx.clone()).await,
            chat_id,
            delivery_id,
            push,
            cancel,
        )
        .await
    }

    async fn session_snapshot(&self, chat_id: &str) -> Result<ChatSessionSnapshot, String> {
        let app = app_state_from_gcx(self.gcx.clone()).await;
        let session_arc =
            chat::get_or_create_session_with_trajectory(app.chat_ctx(), &self.gcx.chat_sessions, chat_id)
                .await;
        let session = session_arc.lock().await;
        Ok(ChatSessionSnapshot {
            messages: session.messages.clone(),
            thread: session.thread.clone(),
            session_state: session.runtime.state,
            pause_reasons: session.runtime.pause_reasons.clone(),
            goal: session.goal.clone(),
        })
    }

    async fn update_session(&self, chat_id: &str, update: ChatSessionUpdate) -> Result<(), String> {
        let app = app_state_from_gcx(self.gcx.clone()).await;
        let session_arc =
            chat::get_or_create_session_with_trajectory(app.chat_ctx(), &self.gcx.chat_sessions, chat_id)
                .await;
        let background_agents: Vec<_> = self
            .gcx
            .agents
            .list_for_parent(chat_id, crate::agents::types::AgentListFilter::default())
            .await
            .iter()
            .map(crate::agents::types::BackgroundAgentSummary::from)
            .collect();
        let mut session = session_arc.lock().await;
        session.replace_messages(update.messages);
        session.thread.previous_response_id = update.previous_response_id;
        session.upsert_background_agents(background_agents);
        let snapshot = session.snapshot();
        session.emit(snapshot);
        Ok(())
    }

    async fn create_session(&self, request: CreateSessionRequest) -> Result<(), String> {
        let app = app_state_from_gcx(self.gcx.clone()).await;
        let session_arc = chat::get_or_create_session_with_trajectory(
            app.chat_ctx(),
            &self.gcx.chat_sessions,
            &request.chat_id,
        )
        .await;
        let mut session = session_arc.lock().await;
        session.thread = request.thread;
        for message in request.messages {
            session.add_message(message);
        }
        session.set_runtime_state(crate::chat::types::SessionState::Starting, None);
        session.increment_version();
        Ok(())
    }

    async fn push_command(
        &self,
        chat_id: &str,
        command: refact_chat_api::ChatCommand,
    ) -> Result<(), String> {
        self.enqueue_command(chat_id, command, false).await
    }

    async fn push_priority_command(
        &self,
        chat_id: &str,
        command: refact_chat_api::ChatCommand,
    ) -> Result<(), String> {
        self.enqueue_command(chat_id, command, true).await
    }

    async fn session_state(
        &self,
        chat_id: &str,
    ) -> Result<Option<refact_runtime_api::SessionState>, String> {
        let session_arc = {
            let sessions = self.gcx.chat_sessions.read().await;
            sessions.get(chat_id).cloned()
        };
        match session_arc {
            Some(session_arc) => Ok(Some(session_arc.lock().await.runtime.state)),
            None => Ok(None),
        }
    }

    async fn maybe_save_session(&self, chat_id: &str) -> Result<(), String> {
        let session_arc = {
            let sessions = self.gcx.chat_sessions.read().await;
            sessions.get(chat_id).cloned()
        };
        if let Some(session_arc) = session_arc {
            trajectories::maybe_save_trajectory_with_intent(
                app_state_from_gcx(self.gcx.clone()).await,
                session_arc,
                crate::chat::types::TrajectoryCommitIntent::Required,
            )
            .await;
        }
        Ok(())
    }

    async fn save_trajectory_snapshot(
        &self,
        snapshot: refact_runtime_api::RuntimeTrajectorySnapshot,
    ) -> Result<(), String> {
        trajectories::save_trajectory_snapshot(self.gcx.clone(), snapshot).await
    }
}

