#[allow(unused_imports)]
use super::*;

pub(super) type MutableTool = Box<dyn crate::tools::tools_description::Tool + Send>;

pub(super) struct AppTurnToolPool {
    pub(super) tools: AMutex<HashMap<String, VecDeque<MutableTool>>>,
    pub(super) expansion_in_progress: AtomicBool,
    pub(super) expansion_finished: Notify,
}

pub(super) struct ToolPoolExpansionGuard<'a> {
    pub(super) pool: &'a AppTurnToolPool,
}

impl Drop for ToolPoolExpansionGuard<'_> {
    fn drop(&mut self) {
        self.pool
            .expansion_in_progress
            .store(false, Ordering::Release);
        self.pool.expansion_finished.notify_waiters();
    }
}

impl AppTurnToolPool {
    pub(super) fn tool_key(desc: &refact_tool_api::ToolDesc) -> String {
        format!(
            "{}\u{1f}{:?}\u{1f}{}",
            crate::llm::adapters::claude_code_compat::cc_resolve_tool_name(&desc.name),
            desc.source.source_type,
            desc.source.config_path
        )
    }

    pub(super) fn from_tools(tools: Vec<MutableTool>) -> Self {
        let mut grouped: HashMap<String, VecDeque<MutableTool>> = HashMap::new();
        for tool in tools {
            grouped
                .entry(Self::tool_key(&tool.tool_description()))
                .or_default()
                .push_back(tool);
        }
        Self {
            tools: AMutex::new(grouped),
            expansion_in_progress: AtomicBool::new(false),
            expansion_finished: Notify::new(),
        }
    }

    pub(super) async fn take(&self, desc: &refact_tool_api::ToolDesc) -> Option<MutableTool> {
        self.tools
            .lock()
            .await
            .get_mut(&Self::tool_key(desc))
            .and_then(VecDeque::pop_front)
    }

    pub(super) async fn return_tool(&self, tool: MutableTool) {
        let name = Self::tool_key(&tool.tool_description());
        self.tools
            .lock()
            .await
            .entry(name)
            .or_default()
            .push_back(tool);
    }

    pub(super) async fn add_missing_tools(&self, tools: Vec<MutableTool>) {
        let mut grouped = self.tools.lock().await;
        for tool in tools {
            let name = Self::tool_key(&tool.tool_description());
            grouped.entry(name).or_default().push_back(tool);
        }
    }
}

