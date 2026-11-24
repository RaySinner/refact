#[allow(unused_imports)]
use super::*;

fn first_tool_text(out: Vec<crate::call_validation::ContextEnum>) -> String {
    out.into_iter()
        .find_map(|item| match item {
            crate::call_validation::ContextEnum::ChatMessage(msg) => {
                Some(msg.content.content_text_only())
            }
            _ => None,
        })
        .unwrap_or_default()
}

#[async_trait]
impl ToolRegistry for AppToolRegistry {
    async fn get_available_tool_names(&self) -> std::collections::HashSet<String> {
        use refact_runtime_api::Tool as _;
        crate::tools::tools_list::get_available_tools(self.gcx.clone())
            .await
            .into_iter()
            .map(|tool| tool.tool_description().name)
            .collect()
    }

    async fn github_issue_create_tool(&self) -> Option<String> {
        let groups = crate::tools::tools_list::get_integration_tools(self.gcx.clone()).await;
        groups
            .into_iter()
            .flat_map(|group| group.tools)
            .find_map(|tool| {
                let desc = tool.tool_description();
                let name = desc.name;
                let cfg = desc.source.config_path;
                let is_github = name.contains("github") || cfg.contains("github");
                if is_github && name.ends_with("_create_issue") {
                    Some(name)
                } else {
                    None
                }
            })
    }

    async fn run_mcp_call_bypass(
        &self,
        tool_name: String,
        args: serde_json::Value,
    ) -> Result<String, String> {
        use refact_runtime_api::Tool as _;
        let mut ccx_inner = crate::at_commands::at_commands::at_commands_new_from_app(
            refact_runtime_api::app_state_from_gcx(self.gcx.clone()).await,
            4000,
            20,
            false,
            vec![],
            String::new(),
            None,
            String::new(),
            None,
            None,
        )
        .await;
        ccx_inner.tool_access_bypass = true;
        let ccx = Arc::new(AMutex::new(ccx_inner));
        let mut tool = crate::tools::tool_mcp_call::ToolMcpCall {};
        let mut call_args = HashMap::new();
        call_args.insert("tool_name".to_string(), serde_json::json!(tool_name));
        call_args.insert("args".to_string(), args);
        let (_, out) = tool
            .tool_execute(ccx, &"buddy_mcp_issue".to_string(), &call_args)
            .await?;
        Ok(first_tool_text(out))
    }

    async fn run_buddy_tool(
        &self,
        tool_name: &str,
        args: serde_json::Value,
        error: Option<String>,
    ) -> Result<String, String> {
        let seed = error.map(|err| vec![crate::call_validation::ChatMessage::new("user".to_string(), err)]);
        let ccx = Arc::new(AMutex::new(
            crate::at_commands::at_commands::at_commands_new_from_app(
                refact_runtime_api::app_state_from_gcx(self.gcx.clone()).await,
                4000,
                20,
                false,
                seed.unwrap_or_default(),
                String::new(),
                None,
                String::new(),
                None,
                None,
            )
            .await,
        ));
        let mut call_args = HashMap::new();
        if let serde_json::Value::Object(map) = args {
            for (key, value) in map {
                call_args.insert(key, value);
            }
        }
        let (mut tool, tool_call_id): (
            Box<dyn crate::tools::tools_description::Tool + Send>,
            &str,
        ) = match tool_name {
            "buddy_get_logs" => (
                Box::new(crate::tools::tool_buddy_get_logs::ToolBuddyGetLogs {
                    config_path: String::new(),
                }),
                "buddy_logs",
            ),
            "buddy_get_context" => (
                Box::new(crate::tools::tool_buddy_get_context::ToolBuddyGetContext {
                    config_path: String::new(),
                }),
                "buddy_ctx",
            ),
            other => return Err(format!("unknown buddy tool: {other}")),
        };
        let (_, out) = tool
            .tool_execute(ccx, &tool_call_id.to_string(), &call_args)
            .await?;
        Ok(first_tool_text(out))
    }

    async fn get_tools_for_mode(
        &self,
        mode: &str,
        model_id: Option<&str>,
    ) -> Vec<refact_tool_api::ToolDesc> {
        let tools = self
            .snapshot_for_mode(self.gcx.clone(), mode, model_id)
            .await
            .index
            .tools
            .clone();
        tools
    }

    async fn get_tool_names_for_mode(
        &self,
        mode: &str,
        model_id: Option<&str>,
    ) -> HashSet<String> {
        self.tools_for_mode(self.gcx.clone(), mode, model_id)
            .await
            .into_iter()
            .map(|tool| tool.tool_description().name)
            .collect()
    }

    async fn get_tools_index_for_mode(
        &self,
        mode: &str,
        model_id: Option<&str>,
    ) -> ToolRegistryIndex {
        let index = self
            .snapshot_for_mode(self.gcx.clone(), mode, model_id)
            .await
            .index
            .clone();
        index
    }

    async fn get_tools_index_for_mode_and_scope(
        &self,
        mode: &str,
        model_id: Option<&str>,
        execution_scope: Option<&str>,
    ) -> ToolRegistryIndex {
        self.snapshot_for_mode_with_scope(
            self.gcx.clone(),
            mode,
            model_id,
            execution_scope.map(str::to_string),
        )
        .await
        .index
        .clone()
    }

    async fn acquire_tool_catalog(
        &self,
        mode: &str,
        model_id: Option<&str>,
        execution_scope: Option<&str>,
    ) -> Arc<ToolCatalogSnapshot> {
        self.snapshot_for_mode_with_scope(
            self.gcx.clone(),
            mode,
            model_id,
            execution_scope.map(str::to_string),
        )
        .await
    }

    async fn acquire_turn_tool_pool(
        &self,
        mode: &str,
        model_id: Option<&str>,
        _execution_scope: Option<&str>,
        catalog: &ToolCatalogSnapshot,
    ) -> Option<TurnToolPool> {
        if !Self::snapshot_cache_enabled() {
            return None;
        }
        Some(
            self.build_turn_tool_pool(self.gcx.clone(), mode, model_id, catalog)
                .await,
        )
    }

    async fn prepare_turn_tool_pool(
        &self,
        pool: &TurnToolPool,
        catalog: &ToolCatalogSnapshot,
        mode: &str,
        model_id: Option<&str>,
        tool_slots: &[(refact_tool_api::ToolDesc, usize)],
    ) -> Result<(), String> {
        let gcx = self.gcx.clone();
        let pool_impl = Self::app_turn_tool_pool(pool)?;
        let mut vector_builds_needed = 0;
        for (tool_desc, required_slots) in tool_slots {
            let existing = {
                let tools = pool_impl.tools.lock().await;
                tools
                    .get(&AppTurnToolPool::tool_key(tool_desc))
                    .map_or(0, VecDeque::len)
            };
            vector_builds_needed =
                vector_builds_needed.max(required_slots.saturating_sub(existing));
        }
        // Every full vector contributes one instance of every available tool, so the largest
        // per-tool deficit satisfies all requested slots. Summing deficits rebuilt the same vector
        // once for every missing slot and left large quantities of unused instances in the pool.
        for _ in 0..vector_builds_needed {
            self.add_turn_tool_pool_fallback(pool, gcx.clone(), mode, model_id, catalog)
                .await?;
        }
        Ok(())
    }

    async fn check_tool_confirmation(
        &self,
        ccx: &(dyn std::any::Any + Send + Sync),
        mode: &str,
        model_id: Option<&str>,
        tool_name: &str,
        args: serde_json::Map<String, serde_json::Value>,
    ) -> Option<Result<ToolConfirmationCheck, String>> {
        let execution_scope = match ccx
            .downcast_ref::<Arc<AMutex<crate::at_commands::at_commands::AtCommandsContext>>>()
        {
            Some(ccx) => Self::execution_scope_from_ccx(ccx).await,
            None => None,
        };
        let catalog = self
            .acquire_tool_catalog(mode, model_id, execution_scope.as_deref())
            .await;
        self.check_tool_confirmation_with_catalog(ccx, &catalog, mode, model_id, tool_name, args)
            .await
    }

    async fn check_tool_confirmation_with_catalog(
        &self,
        ccx: &(dyn std::any::Any + Send + Sync),
        catalog: &ToolCatalogSnapshot,
        mode: &str,
        model_id: Option<&str>,
        tool_name: &str,
        args: serde_json::Map<String, serde_json::Value>,
    ) -> Option<Result<ToolConfirmationCheck, String>> {
        let ccx = match ccx
            .downcast_ref::<Arc<AMutex<crate::at_commands::at_commands::AtCommandsContext>>>()
        {
            Some(ccx) => ccx.clone(),
            None => {
                return Some(Err(
                    "invalid AtCommandsContext passed to ToolRegistry".to_string()
                ))
            }
        };
        let catalog_desc = Self::find_catalog_descriptor(catalog, tool_name)?;
        let tools = self
            .fresh_mutable_tools(
                self.gcx.clone(),
                mode,
                model_id,
                catalog,
                PerfComponent::ToolMutableVectorBuild,
            )
            .await;
        let resolved = crate::llm::adapters::claude_code_compat::cc_resolve_tool_name(tool_name);
        for tool in tools {
            let desc = tool.tool_description();
            if desc.name == tool_name || desc.name == resolved.as_str() {
                let mut coerced_args: HashMap<String, serde_json::Value> =
                    args.into_iter().collect();
                refact_tool_api::coerce_hashmap_to_schema(
                    &mut coerced_args,
                    &catalog_desc.input_schema,
                );
                let integr_config_path = (!catalog_desc.source.config_path.is_empty())
                    .then(|| catalog_desc.source.config_path.clone());
                return Some(
                    tool.match_against_confirm_deny(ccx, &coerced_args)
                        .await
                        .map(|result| ToolConfirmationCheck {
                            tool_name: catalog_desc.name.clone(),
                            result,
                            integr_config_path,
                        }),
                );
            }
        }
        None
    }

    async fn check_tool_confirmation_with_catalog_and_pool(
        &self,
        ccx: &(dyn std::any::Any + Send + Sync),
        catalog: &ToolCatalogSnapshot,
        pool: Option<&TurnToolPool>,
        mode: &str,
        model_id: Option<&str>,
        tool_name: &str,
        args: serde_json::Map<String, serde_json::Value>,
    ) -> Option<Result<ToolConfirmationCheck, String>> {
        let Some(pool) = pool else {
            return self
                .check_tool_confirmation_with_catalog(ccx, catalog, mode, model_id, tool_name, args)
                .await;
        };
        let ccx = match ccx
            .downcast_ref::<Arc<AMutex<crate::at_commands::at_commands::AtCommandsContext>>>()
        {
            Some(ccx) => ccx.clone(),
            None => {
                return Some(Err(
                    "invalid AtCommandsContext passed to ToolRegistry".to_string()
                ))
            }
        };
        let catalog_desc = Self::find_catalog_descriptor(catalog, tool_name)?;
        let gcx = {
            let cgcx = ccx.lock().await;
            cgcx.app.gcx.clone()
        };
        let Some(tool) = (match self
            .take_turn_tool(pool, gcx, mode, model_id, catalog, tool_name)
            .await
        {
            Ok(tool) => tool,
            Err(error) => return Some(Err(error)),
        }) else {
            return None;
        };
        let mut coerced_args: HashMap<String, serde_json::Value> = args.into_iter().collect();
        refact_tool_api::coerce_hashmap_to_schema(&mut coerced_args, &catalog_desc.input_schema);
        let integr_config_path = (!catalog_desc.source.config_path.is_empty())
            .then(|| catalog_desc.source.config_path.clone());
        let result = tool
            .match_against_confirm_deny(ccx, &coerced_args)
            .await
            .map(|result| ToolConfirmationCheck {
                tool_name: catalog_desc.name.clone(),
                result,
                integr_config_path,
            });
        Self::app_turn_tool_pool(pool)
            .expect("pool was validated before tool lease")
            .return_tool(tool)
            .await;
        Some(result)
    }

    async fn get_tool_policy_info(
        &self,
        mode: &str,
        model_id: Option<&str>,
    ) -> Vec<ToolPolicyInfo> {
        let policy = self
            .snapshot_for_mode(self.gcx.clone(), mode, model_id)
            .await
            .policy
            .clone();
        policy
    }

    async fn execute_tool(
        &self,
        ccx: &(dyn std::any::Any + Send + Sync),
        mode: &str,
        model_id: Option<&str>,
        tool_call_id: &str,
        tool_name: &str,
        args: serde_json::Map<String, serde_json::Value>,
    ) -> Result<Option<ToolExecutionResult>, String> {
        let execution_scope = match ccx
            .downcast_ref::<Arc<AMutex<crate::at_commands::at_commands::AtCommandsContext>>>()
        {
            Some(ccx) => Self::execution_scope_from_ccx(ccx).await,
            None => None,
        };
        let catalog = self
            .acquire_tool_catalog(mode, model_id, execution_scope.as_deref())
            .await;
        self.execute_tool_with_catalog(ccx, &catalog, mode, model_id, tool_call_id, tool_name, args)
            .await
    }

    async fn execute_tool_with_catalog(
        &self,
        ccx: &(dyn std::any::Any + Send + Sync),
        catalog: &ToolCatalogSnapshot,
        mode: &str,
        model_id: Option<&str>,
        tool_call_id: &str,
        tool_name: &str,
        args: serde_json::Map<String, serde_json::Value>,
    ) -> Result<Option<ToolExecutionResult>, String> {
        let ccx = ccx
            .downcast_ref::<Arc<AMutex<crate::at_commands::at_commands::AtCommandsContext>>>()
            .ok_or_else(|| "invalid AtCommandsContext passed to ToolRegistry".to_string())?
            .clone();
        Self::ensure_tool_execution_allowed(&ccx, tool_name).await?;
        let lookup_span = perf_diagnostics::span(PerfComponent::ToolExecutionLookup, None, None);
        let catalog_desc = match Self::find_catalog_descriptor(catalog, tool_name) {
            Some(desc) => desc,
            None => {
                lookup_span.finish_tool(PerfOutcome::Failure, 1, 1, None);
                return Ok(None);
            }
        };
        let gcx = {
            let cgcx = ccx.lock().await;
            cgcx.app.gcx.clone()
        };
        let pool = self
            .acquire_turn_tool_pool(mode, model_id, None, catalog)
            .await;
        let leased = match pool.as_ref() {
            Some(pool) => {
                self.take_turn_tool(pool, gcx.clone(), mode, model_id, catalog, tool_name)
                    .await?
            }
            None => {
                let tools = self
                    .fresh_mutable_tools(
                        gcx.clone(),
                        mode,
                        model_id,
                        catalog,
                        PerfComponent::ToolMutableVectorBuild,
                    )
                    .await;
                let resolved =
                    crate::llm::adapters::claude_code_compat::cc_resolve_tool_name(tool_name);
                let mut found = None;
                for candidate in tools {
                    let name = candidate.tool_description().name;
                    if name == tool_name || name == resolved.as_str() {
                        found = Some(candidate);
                        break;
                    }
                }
                found
            }
        };
        let Some(mut tool) = leased else {
            lookup_span.finish_tool(PerfOutcome::Failure, 1, 1, None);
            return Ok(None);
        };
        let mut coerced_args: HashMap<String, serde_json::Value> = args.into_iter().collect();
        let coercion_notes = refact_tool_api::coerce_hashmap_to_schema(
            &mut coerced_args,
            &catalog_desc.input_schema,
        );
        if !coercion_notes.is_empty() {
            tracing::info!(
                "Coerced arguments for tool {}: {:?}",
                tool.tool_description().name,
                coercion_notes
            );
        }
        {
            let mut cgcx = ccx.lock().await;
            cgcx.app = app_state_from_gcx(gcx.clone()).await;
        }
        let tool_call_id = tool_call_id.to_string();
        lookup_span.finish_tool(PerfOutcome::Success, 1, 1, None);
        let runtime_span = perf_diagnostics::span(PerfComponent::ToolRuntime, None, None);
        let result = tool.tool_execute(ccx, &tool_call_id, &coerced_args).await;
        let result = match result {
            Ok(result) => {
                runtime_span.finish_tool(PerfOutcome::Success, 1, 1, None);
                result
            }
            Err(error) => {
                runtime_span.finish_tool(PerfOutcome::Failure, 1, 1, None);
                if let Some(pool) = pool.as_ref() {
                    Self::app_turn_tool_pool(pool)?.return_tool(tool).await;
                }
                return Err(error);
            }
        };
        if let Some(pool) = pool.as_ref() {
            Self::app_turn_tool_pool(pool)?.return_tool(tool).await;
        }
        let mut messages = Vec::new();
        let mut context_files = Vec::new();
        for item in result.1 {
            match item {
                crate::call_validation::ContextEnum::ChatMessage(message) => messages.push(message),
                crate::call_validation::ContextEnum::ContextFile(file) => context_files.push(file),
            }
        }
        Ok(Some(ToolExecutionResult {
            had_corrections: result.0,
            messages,
            context_files,
        }))
    }

    async fn execute_tool_with_catalog_and_pool(
        &self,
        ccx: &(dyn std::any::Any + Send + Sync),
        catalog: &ToolCatalogSnapshot,
        pool: Option<&TurnToolPool>,
        mode: &str,
        model_id: Option<&str>,
        tool_call_id: &str,
        tool_name: &str,
        args: serde_json::Map<String, serde_json::Value>,
    ) -> Result<Option<ToolExecutionResult>, String> {
        let Some(pool) = pool else {
            return self
                .execute_tool_with_catalog(
                    ccx,
                    catalog,
                    mode,
                    model_id,
                    tool_call_id,
                    tool_name,
                    args,
                )
                .await;
        };
        let ccx = ccx
            .downcast_ref::<Arc<AMutex<crate::at_commands::at_commands::AtCommandsContext>>>()
            .ok_or_else(|| "invalid AtCommandsContext passed to ToolRegistry".to_string())?
            .clone();
        Self::ensure_tool_execution_allowed(&ccx, tool_name).await?;
        let lookup_span = perf_diagnostics::span(PerfComponent::ToolExecutionLookup, None, None);
        let catalog_desc = match Self::find_catalog_descriptor(catalog, tool_name) {
            Some(desc) => desc,
            None => return Ok(None),
        };
        let gcx = {
            let cgcx = ccx.lock().await;
            cgcx.app.gcx.clone()
        };
        let Some(mut tool) = self
            .take_turn_tool(pool, gcx.clone(), mode, model_id, catalog, tool_name)
            .await?
        else {
            return Ok(None);
        };
        let mut coerced_args: HashMap<String, serde_json::Value> = args.into_iter().collect();
        let coercion_notes = refact_tool_api::coerce_hashmap_to_schema(
            &mut coerced_args,
            &catalog_desc.input_schema,
        );
        if !coercion_notes.is_empty() {
            tracing::info!(
                "Coerced arguments for tool {}: {:?}",
                tool.tool_description().name,
                coercion_notes
            );
        }
        {
            let mut cgcx = ccx.lock().await;
            cgcx.app = app_state_from_gcx(gcx).await;
        }
        lookup_span.finish_tool(PerfOutcome::Success, 1, 1, None);
        let runtime_span = perf_diagnostics::span(PerfComponent::ToolRuntime, None, None);
        let result = tool
            .tool_execute(ccx, &tool_call_id.to_string(), &coerced_args)
            .await;
        let result = match result {
            Ok(result) => {
                runtime_span.finish_tool(PerfOutcome::Success, 1, 1, None);
                result
            }
            Err(error) => {
                runtime_span.finish_tool(PerfOutcome::Failure, 1, 1, None);
                Self::app_turn_tool_pool(pool)?.return_tool(tool).await;
                return Err(error);
            }
        };
        Self::app_turn_tool_pool(pool)?.return_tool(tool).await;
        let mut messages = Vec::new();
        let mut context_files = Vec::new();
        for item in result.1 {
            match item {
                crate::call_validation::ContextEnum::ChatMessage(message) => messages.push(message),
                crate::call_validation::ContextEnum::ContextFile(file) => context_files.push(file),
            }
        }
        Ok(Some(ToolExecutionResult {
            had_corrections: result.0,
            messages,
            context_files,
        }))
    }

    async fn load_task_memories(&self, task_id: &str) -> Result<Vec<(PathBuf, String)>, String> {
        crate::tools::tool_task_memory::load_task_memories(
            &self.gcx.chat_ctx(self.gcx.clone()),
            task_id,
        )
        .await
    }
}

