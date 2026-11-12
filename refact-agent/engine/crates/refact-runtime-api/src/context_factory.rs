use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use refact_caps_core::code_assistant_caps::CodeAssistantCaps;

use crate::app_state::AppState;
use crate::at_commands_ctx::AtCommand;
use crate::chat_ctx::ChatCtx;
use crate::command_line::CommandLine;
use crate::global_context::SharedGlobalContext;
use crate::tool_ctx::ToolCtx;

/// Seam over the engine caps stack (`caps::load_caps` and the project model
/// defaults mtime probe). Both need engine-only provider machinery, so they stay
/// in the engine and are reached through the installed factory.
#[async_trait]
pub trait CapsLoader: Send + Sync {
    async fn load_caps(
        &self,
        cmdline: CommandLine,
        gcx: SharedGlobalContext,
    ) -> Result<Arc<CodeAssistantCaps>, String>;
    async fn project_model_defaults_state(
        &self,
        gcx: SharedGlobalContext,
    ) -> (Option<PathBuf>, u64);
}

/// Seam that lets `refact-runtime-api` build the composite context structs without
/// knowing about the engine's handle implementations. The engine installs the real
/// implementation right after `GlobalContext` is constructed.
pub trait ContextFactory: Send + Sync {
    fn chat_ctx(&self, gcx: SharedGlobalContext) -> ChatCtx;
    fn tool_ctx(&self, gcx: SharedGlobalContext) -> ToolCtx;
    fn app_state(&self, gcx: SharedGlobalContext) -> AppState;
    fn caps_loader(&self) -> Arc<dyn CapsLoader>;
    fn at_commands(&self) -> HashMap<String, Arc<dyn AtCommand + Send>>;
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
