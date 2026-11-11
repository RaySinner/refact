use std::sync::Arc;

use axum::extract::FromRef;
use async_trait::async_trait;
use refact_agents_core::AgentHost;
use refact_agents_core::registry::BackgroundAgentRegistry;
use refact_chat_api::{DeliveryOutcome, PendingDelivery, SessionsMap};

use crate::chat_ctx::ChatCtx;
use crate::tool_ctx::ToolCtx;
use crate::global_context::SharedGlobalContext;
use crate::services::{
    BuddyServices, ChatServices, IntegrationServices, ModelServices, PathServices, RuntimeServices,
    WorkspaceServices,
};
use crate::{ActivitySink, BuddyEventSink, ChatSessionFacade, ToolRegistry};

#[derive(Clone)]
pub struct AppState {
    pub gcx: SharedGlobalContext,
    pub ctx: ChatCtx,
    pub tool: ToolCtx,
    pub runtime: RuntimeServices,
    pub paths: PathServices,
    pub model: ModelServices,
    pub workspace: WorkspaceServices,
    pub chat: ChatServices,
    pub buddy: BuddyServices,
    pub integrations: IntegrationServices,
    pub activity_sink: Arc<dyn ActivitySink>,
    pub buddy_event_sink: Arc<dyn BuddyEventSink>,
    pub tool_registry: Arc<dyn ToolRegistry>,
    pub agents: Arc<BackgroundAgentRegistry>,
}

impl AppState {
    pub fn chat_ctx(&self) -> ChatCtx {
        self.ctx.clone()
    }
}

#[async_trait]
impl AgentHost for AppState {
    fn agents(&self) -> &Arc<BackgroundAgentRegistry> {
        &self.agents
    }

    fn sessions(&self) -> &SessionsMap {
        &self.chat.sessions
    }

    async fn deliver(
        &self,
        chat_id: &str,
        delivery: PendingDelivery,
    ) -> Result<DeliveryOutcome, String> {
        self.chat.facade.deliver_messages(chat_id, delivery).await
    }
}

impl FromRef<AppState> for SharedGlobalContext {
    fn from_ref(app: &AppState) -> Self {
        app.gcx.clone()
    }
}

impl From<AppState> for SharedGlobalContext {
    fn from(app: AppState) -> Self {
        app.gcx.clone()
    }
}

impl From<&AppState> for SharedGlobalContext {
    fn from(app: &AppState) -> Self {
        app.gcx.clone()
    }
}

impl FromRef<AppState> for RuntimeServices {
    fn from_ref(app: &AppState) -> Self {
        app.runtime.clone()
    }
}

impl FromRef<AppState> for PathServices {
    fn from_ref(app: &AppState) -> Self {
        app.paths.clone()
    }
}

impl FromRef<AppState> for ModelServices {
    fn from_ref(app: &AppState) -> Self {
        app.model.clone()
    }
}

impl FromRef<AppState> for WorkspaceServices {
    fn from_ref(app: &AppState) -> Self {
        app.workspace.clone()
    }
}

impl FromRef<AppState> for ChatServices {
    fn from_ref(app: &AppState) -> Self {
        app.chat.clone()
    }
}

impl FromRef<AppState> for BuddyServices {
    fn from_ref(app: &AppState) -> Self {
        app.buddy.clone()
    }
}

impl FromRef<AppState> for IntegrationServices {
    fn from_ref(app: &AppState) -> Self {
        app.integrations.clone()
    }
}
