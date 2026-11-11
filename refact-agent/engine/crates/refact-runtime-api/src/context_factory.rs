use std::sync::{Arc, RwLock};

use crate::app_state::AppState;
use crate::chat_ctx::ChatCtx;
use crate::global_context::SharedGlobalContext;
use crate::tool_ctx::ToolCtx;

/// Seam that lets `refact-runtime-api` build the composite context structs without
/// knowing about the engine's handle implementations. The engine installs the real
/// implementation right after `GlobalContext` is constructed.
pub trait ContextFactory: Send + Sync {
    fn chat_ctx(&self, gcx: SharedGlobalContext) -> ChatCtx;
    fn tool_ctx(&self, gcx: SharedGlobalContext) -> ToolCtx;
    fn app_state(&self, gcx: SharedGlobalContext) -> AppState;
}

/// Interior-mutable slot on `GlobalContext`. `GlobalContext` is built inside a struct
/// literal in two places (engine `create_global_context`, `refact-test-support`), and
/// the factory needs the finished `Arc<GlobalContext>`, so it cannot be a constructor
/// argument. Installing is idempotent-last-wins; reading before installing panics with
/// an explicit message.
pub struct ContextFactorySlot(RwLock<Option<Arc<dyn ContextFactory>>>);

impl ContextFactorySlot {
    pub fn new() -> Self {
        Self(RwLock::new(None))
    }

    pub fn install(&self, factory: Arc<dyn ContextFactory>) {
        *self
            .0
            .write()
            .unwrap_or_else(|p| p.into_inner()) = Some(factory);
    }

    pub fn get(&self) -> Arc<dyn ContextFactory> {
        self.0
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
            .expect("GlobalContext context factory is not installed")
    }
}

impl Default for ContextFactorySlot {
    fn default() -> Self {
        Self::new()
    }
}
