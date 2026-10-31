use std::sync::Arc;

use axum::extract::FromRef;
use refact_agents_core::registry::BackgroundAgentRegistry;

use crate::chat_ctx::ChatCtx;
use crate::tool_ctx::ToolCtx;
use crate::global_context::SharedGlobalContext;
use crate::services::{
    BuddyServices, ChatServices, IntegrationServices, ModelServices, PathServices, RuntimeServices,
    WorkspaceServices,
};
use crate::{ActivitySink, BuddyEventSink, ToolRegistry};

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
