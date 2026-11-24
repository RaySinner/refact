use async_trait::async_trait;
use refact_agents_core::spawn::{SpawnHandle, SpawnRequest};
#[cfg(any(test, feature = "test-util"))]
use refact_chat_api::{ChatMessage, SubchatConfig, SubchatResult};
#[cfg(any(test, feature = "test-util"))]
use std::sync::Arc;

use crate::app_state::AppState;
#[cfg(any(test, feature = "test-util"))]
use crate::global_context::SharedGlobalContext;

/// Seam over the engine background-agent spawner. Its body needs the engine `subchat`
/// island (config resolution, stateful session install, isolated-worktree merge), so the
/// orchestration stays in the engine and lower crates reach it through `ChatCtx::spawn`.
#[async_trait]
pub trait BackgroundAgentSpawner: Send + Sync {
    async fn spawn_background_agent(
        &self,
        app: AppState,
        request: SpawnRequest,
    ) -> Result<SpawnHandle, String>;
}

pub async fn spawn_background_agent(
    app: AppState,
    request: SpawnRequest,
) -> Result<SpawnHandle, String> {
    let spawner = app.ctx.spawn.clone();
    spawner.spawn_background_agent(app, request).await
}

#[cfg(any(test, feature = "test-util"))]
pub type TestRunner = Arc<
    dyn Fn(
            SharedGlobalContext,
            Vec<ChatMessage>,
            SubchatConfig,
        )
            -> std::pin::Pin<
                Box<dyn std::future::Future<Output = Result<SubchatResult, String>> + Send>,
            >
        + Send
        + Sync,
>;

#[cfg(any(test, feature = "test-util"))]
pub type SpawnStageObserver = Arc<dyn Fn(String) + Send + Sync>;

#[cfg(any(test, feature = "test-util"))]
static TEST_RUNNER: std::sync::OnceLock<std::sync::Mutex<Option<TestRunner>>> =
    std::sync::OnceLock::new();

#[cfg(any(test, feature = "test-util"))]
static TEST_SPAWN_STAGE_OBSERVER: std::sync::OnceLock<
    std::sync::Mutex<Option<SpawnStageObserver>>,
> = std::sync::OnceLock::new();

#[cfg(any(test, feature = "test-util"))]
pub struct TestRunnerGuard;

#[cfg(any(test, feature = "test-util"))]
impl Drop for TestRunnerGuard {
    fn drop(&mut self) {
        if let Some(runner) = TEST_RUNNER.get() {
            *runner.lock().unwrap() = None;
        }
        if let Some(observer) = TEST_SPAWN_STAGE_OBSERVER.get() {
            *observer.lock().unwrap() = None;
        }
    }
}

#[cfg(any(test, feature = "test-util"))]
pub fn install_test_runner(runner: TestRunner) -> TestRunnerGuard {
    install_test_runner_with_stages(runner, None)
}

#[cfg(any(test, feature = "test-util"))]
pub fn install_test_runner_with_stages(
    runner: TestRunner,
    observer: Option<SpawnStageObserver>,
) -> TestRunnerGuard {
    *TEST_RUNNER
        .get_or_init(|| std::sync::Mutex::new(None))
        .lock()
        .unwrap() = Some(runner);
    *TEST_SPAWN_STAGE_OBSERVER
        .get_or_init(|| std::sync::Mutex::new(None))
        .lock()
        .unwrap() = observer;
    TestRunnerGuard
}

#[cfg(any(test, feature = "test-util"))]
pub fn current_test_runner() -> Option<TestRunner> {
    TEST_RUNNER
        .get_or_init(|| std::sync::Mutex::new(None))
        .lock()
        .unwrap()
        .clone()
}

#[cfg(any(test, feature = "test-util"))]
pub fn emit_test_spawn_stage(stage: String) {
    let observer = TEST_SPAWN_STAGE_OBSERVER
        .get_or_init(|| std::sync::Mutex::new(None))
        .lock()
        .unwrap()
        .clone();
    if let Some(observer) = observer {
        observer(stage);
    }
}
