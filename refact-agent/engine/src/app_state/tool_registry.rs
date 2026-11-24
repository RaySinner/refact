#[allow(unused_imports)]
use super::*;

pub struct AppToolRegistry {
    pub(super) gcx: SharedGlobalContext,
    #[cfg(any(test, feature = "bench"))]
    fixture_tool_factory: Option<FixtureToolFactory>,
}

#[cfg(any(test, feature = "bench"))]
pub type FixtureToolFactory =
    Arc<dyn Fn() -> Vec<Box<dyn crate::tools::tools_description::Tool + Send>> + Send + Sync>;

impl AppToolRegistry {
    pub fn new(gcx: SharedGlobalContext) -> Self {
        Self {
            gcx,
            #[cfg(any(test, feature = "bench"))]
            fixture_tool_factory: None,
        }
    }

    #[cfg(any(test, feature = "bench"))]
    pub fn with_fixture_tool_factory(
        gcx: SharedGlobalContext,
        fixture_tool_factory: FixtureToolFactory,
    ) -> Self {
        Self {
            gcx,
            fixture_tool_factory: Some(fixture_tool_factory),
        }
    }

    pub(super) async fn tools_for_mode(
        &self,
        gcx: SharedGlobalContext,
        mode: &str,
        model_id: Option<&str>,
    ) -> Vec<Box<dyn crate::tools::tools_description::Tool + Send>> {
        #[cfg(any(test, feature = "bench"))]
        if let Some(fixture_tool_factory) = &self.fixture_tool_factory {
            // Exercise races around async production in pool tests; production construction has
            // many real await points before returning the vector.
            tokio::task::yield_now().await;
            return fixture_tool_factory();
        }
        crate::tools::tools_list::get_tools_for_mode(gcx, mode, model_id).await
    }

    pub(super) fn snapshot_cache_enabled() -> bool {
        tool_catalog_snapshot_rollout_enabled()
    }

    #[cfg(test)]
    pub(super) fn snapshot_cache_enabled_for(value: Option<&str>) -> bool {
        tool_catalog_snapshot_rollout_enabled_for(value)
    }

    async fn catalog_key_with_scope(
        &self,
        gcx: SharedGlobalContext,
        mode: &str,
        model_id: Option<&str>,
        execution_scope: Option<String>,
    ) -> ToolCatalogKey {
        let workspace_scope = crate::files_correction::get_active_project_path(gcx.clone())
            .await
            .unwrap_or_else(|| gcx.config_dir.clone())
            .to_string_lossy()
            .to_string();
        let generations = &gcx.tool_catalog_generations;
        ToolCatalogKey {
            workspace_scope,
            execution_scope,
            mode: mode.to_string(),
            model: model_id.map(str::to_string),
            customization_generation: generations.customization.load(Ordering::Acquire),
            integration_generation: generations.integrations.load(Ordering::Acquire),
            mcp_generation: generations.mcp.load(Ordering::Acquire),
            privacy_generation: generations.privacy.load(Ordering::Acquire),
            capability_generation: generations.capabilities.load(Ordering::Acquire),
            extension_generation: gcx.ext_cache_generation.load(Ordering::Acquire),
        }
    }

    async fn build_snapshot(
        &self,
        gcx: SharedGlobalContext,
        mode: &str,
        model_id: Option<&str>,
        execution_scope: Option<&str>,
    ) -> ToolCatalogSnapshot {
        let mut tools = crate::tools::tools_list::apply_mcp_lazy_filter(
            self.tools_for_mode(gcx, mode, model_id).await,
        );
        if execution_scope.is_some_and(refact_worktrees::scope::is_room_advisor_catalog_scope) {
            crate::tools::tools_list::retain_room_read_only_tools(&mut tools.tools);
        }
        let policy = crate::tools::tools_list::catalog_policy_for_tools(&tools.tools);
        let descriptions = tools
            .tools
            .iter()
            .map(|tool| tool.tool_description())
            .collect::<Vec<_>>();
        let names = descriptions
            .iter()
            .map(|tool| tool.name.clone())
            .collect::<Vec<_>>();
        ToolCatalogSnapshot {
            index: ToolRegistryIndex {
                tools: descriptions,
                mcp_lazy_mode: tools.mcp_lazy_mode,
                mcp_total_count: tools.mcp_total_count,
                mcp_tool_index: tools.mcp_tool_index,
            },
            policy,
            aliases: refact_tool_api::build_registry_from_names(&names),
        }
    }

    pub(super) async fn snapshot_for_mode(
        &self,
        gcx: SharedGlobalContext,
        mode: &str,
        model_id: Option<&str>,
    ) -> Arc<ToolCatalogSnapshot> {
        self.snapshot_for_mode_with_scope(gcx, mode, model_id, None)
            .await
    }

    pub(super) async fn execution_scope_from_ccx(
        ccx: &Arc<AMutex<crate::at_commands::at_commands::AtCommandsContext>>,
    ) -> Option<String> {
        ccx.lock()
            .await
            .execution_scope
            .as_ref()
            .map(|scope| scope.catalog_scope())
    }

    pub(super) async fn ensure_tool_execution_allowed(
        ccx: &Arc<AMutex<crate::at_commands::at_commands::AtCommandsContext>>,
        tool_name: &str,
    ) -> Result<(), String> {
        let scope = ccx.lock().await.execution_scope.clone();
        let resolved = crate::llm::adapters::claude_code_compat::cc_resolve_tool_name(tool_name);
        if let Some(error) =
            crate::tools::tools_list::room_read_only_tool_error(scope.as_ref(), resolved.as_str())
        {
            return Err(error);
        }
        Ok(())
    }

    pub(super) async fn fresh_mutable_tools(
        &self,
        gcx: SharedGlobalContext,
        mode: &str,
        model_id: Option<&str>,
        catalog: &ToolCatalogSnapshot,
        component: PerfComponent,
    ) -> Vec<Box<dyn crate::tools::tools_description::Tool + Send>> {
        let span = perf_diagnostics::span(component, None, None);
        let tools = crate::tools::tools_list::apply_mcp_lazy_filter(
            self.tools_for_mode(gcx, mode, model_id).await,
        )
        .tools;
        span.finish_tool(
            PerfOutcome::Success,
            1,
            catalog.index.tools.len() as u64,
            None,
        );
        tools
    }

    pub(super) async fn build_turn_tool_pool(
        &self,
        gcx: SharedGlobalContext,
        mode: &str,
        model_id: Option<&str>,
        catalog: &ToolCatalogSnapshot,
    ) -> TurnToolPool {
        let tools = self
            .fresh_mutable_tools(
                gcx,
                mode,
                model_id,
                catalog,
                PerfComponent::ToolMutableVectorBuild,
            )
            .await;
        let pool = TurnToolPool::new(AppTurnToolPool::from_tools(tools));
        pool.record_initial_vector_build();
        pool
    }

    pub(super) fn app_turn_tool_pool<'a>(
        pool: &'a TurnToolPool,
    ) -> Result<&'a AppTurnToolPool, String> {
        pool.downcast_ref::<AppTurnToolPool>()
            .ok_or_else(|| "invalid TurnToolPool passed to AppToolRegistry".to_string())
    }

    pub(super) async fn add_turn_tool_pool_fallback(
        &self,
        pool: &TurnToolPool,
        gcx: SharedGlobalContext,
        mode: &str,
        model_id: Option<&str>,
        catalog: &ToolCatalogSnapshot,
    ) -> Result<(), String> {
        let tools = self
            .fresh_mutable_tools(
                gcx,
                mode,
                model_id,
                catalog,
                PerfComponent::ToolPoolParallelExpansion,
            )
            .await;
        Self::app_turn_tool_pool(pool)?
            .add_missing_tools(tools)
            .await;
        pool.record_fallback_vector_build();
        Ok(())
    }

    pub(super) async fn take_turn_tool(
        &self,
        pool: &TurnToolPool,
        gcx: SharedGlobalContext,
        mode: &str,
        model_id: Option<&str>,
        catalog: &ToolCatalogSnapshot,
        tool_name: &str,
    ) -> Result<Option<MutableTool>, String> {
        let catalog_desc = match Self::find_catalog_descriptor(catalog, tool_name) {
            Some(desc) => desc,
            None => return Ok(None),
        };
        let pool_impl = Self::app_turn_tool_pool(pool)?;
        if let Some(tool) = pool_impl.take(catalog_desc).await {
            return Ok(Some(tool));
        }
        loop {
            // A fallback vector contains one fresh instance of every tool. Single-flight its
            // construction so concurrent misses for different tools share that vector. No lock is
            // held across construction, and each caller still removes an owned instance.
            if pool_impl
                .expansion_in_progress
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                let expansion_guard = ToolPoolExpansionGuard { pool: pool_impl };
                let result = self
                    .add_turn_tool_pool_fallback(pool, gcx.clone(), mode, model_id, catalog)
                    .await;
                let tool = match result {
                    Ok(()) => pool_impl.take(catalog_desc).await,
                    Err(error) => {
                        drop(expansion_guard);
                        return Err(error);
                    }
                };
                drop(expansion_guard);
                return Ok(tool);
            }
            let finished = pool_impl.expansion_finished.notified();
            if pool_impl.expansion_in_progress.load(Ordering::Acquire) {
                finished.await;
            }
            if let Some(tool) = pool_impl.take(catalog_desc).await {
                return Ok(Some(tool));
            }
        }
    }

    pub(super) fn find_catalog_descriptor<'a>(
        catalog: &'a ToolCatalogSnapshot,
        tool_name: &str,
    ) -> Option<&'a refact_tool_api::ToolDesc> {
        let resolved = crate::llm::adapters::claude_code_compat::cc_resolve_tool_name(tool_name);
        catalog
            .index
            .tools
            .iter()
            .find(|desc| desc.name == tool_name || desc.name == resolved.as_str())
    }

    pub(super) async fn snapshot_for_mode_with_scope(
        &self,
        gcx: SharedGlobalContext,
        mode: &str,
        model_id: Option<&str>,
        execution_scope: Option<String>,
    ) -> Arc<ToolCatalogSnapshot> {
        if !Self::snapshot_cache_enabled() {
            let span = perf_diagnostics::span(PerfComponent::ToolCatalogBuild, None, None);
            let snapshot = Arc::new(
                self.build_snapshot(gcx, mode, model_id, execution_scope.as_deref())
                    .await,
            );
            span.finish_tool(
                PerfOutcome::Success,
                1,
                snapshot.index.tools.len() as u64,
                None,
            );
            return snapshot;
        }
        let key = self
            .catalog_key_with_scope(gcx.clone(), mode, model_id, execution_scope.clone())
            .await;
        if let Some(snapshot) = gcx.tool_catalog_cache.get(&key).await {
            return snapshot;
        }
        let build_lock = gcx.tool_catalog_cache.acquire_build_lock(&key).await;
        let _build_guard = build_lock.lock().await;
        if let Some(snapshot) = gcx.tool_catalog_cache.get(&key).await {
            return snapshot;
        }
        let span = perf_diagnostics::span(PerfComponent::ToolCatalogBuild, None, None);
        let snapshot = Arc::new(
            self.build_snapshot(gcx.clone(), mode, model_id, execution_scope.as_deref())
                .await,
        );
        span.finish_tool(
            PerfOutcome::Success,
            1,
            snapshot.index.tools.len() as u64,
            None,
        );
        gcx.tool_catalog_cache
            .insert(key.clone(), snapshot.clone())
            .await;
        gcx.tool_catalog_cache.release_build_lock(&key).await;
        snapshot
    }

    #[cfg(any(test, feature = "bench"))]
    #[allow(dead_code)]
    pub(super) async fn snapshot_for_mode_for_test(
        &self,
        mode: &str,
        model_id: Option<&str>,
    ) -> Arc<ToolCatalogSnapshot> {
        self.snapshot_for_mode(self.gcx.clone(), mode, model_id)
            .await
    }

    #[cfg(any(test, feature = "bench"))]
    #[allow(dead_code)]
    pub(super) async fn snapshot_for_mode_with_scope_for_test(
        &self,
        mode: &str,
        model_id: Option<&str>,
        execution_scope: Option<String>,
    ) -> Arc<ToolCatalogSnapshot> {
        self.snapshot_for_mode_with_scope(self.gcx.clone(), mode, model_id, execution_scope)
            .await
    }
}
