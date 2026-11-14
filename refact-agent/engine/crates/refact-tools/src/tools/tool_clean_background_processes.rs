use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::sync::Mutex as AMutex;

use refact_runtime_api::AtCommandsContext;
use refact_core::chat_types::{ChatContent, ChatMessage, ContextEnum};
use refact_exec::types::normalize_workspace_path;
use refact_exec::{ExecMode, ExecProcessFilter, ExecProcessSnapshot, ExecStatusKind};
use refact_runtime_api::files_correction::get_active_project_path_for_services;
use refact_postprocessing::pp_command_output::OutputFilter;
use crate::tools::file_edit::auxiliary::active_execution_scope;
use refact_tool_api::IntegrationConfirmation;
use crate::tools::tools_description::{Tool, ToolDesc, ToolSource, ToolSourceType};
use refact_worktrees::scope::ExecutionScope;

pub struct ToolCleanBackgroundProcesses {
    pub config_path: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum CleanScope {
    Chat,
    Workspace,
    All,
}

#[async_trait]
impl Tool for ToolCleanBackgroundProcesses {
    async fn tool_execute(
        &mut self,
        ccx: Arc<AMutex<AtCommandsContext>>,
        tool_call_id: &String,
        args: &HashMap<String, Value>,
    ) -> Result<(bool, Vec<ContextEnum>), String> {
        let scope = parse_scope(args)?;
        let include_services = parse_include_services(args)?;
        let (paths, workspace_services, exec_registry, execution_scope, chat_id, task_role) = {
            let ccx = ccx.lock().await;
            (
                ccx.app.paths.clone(),
                ccx.app.workspace.clone(),
                ccx.app.runtime.exec_registry.clone(),
                ccx.execution_scope.clone(),
                ccx.chat_id.clone(),
                ccx.task_meta.as_ref().map(|meta| meta.role.clone()),
            )
        };
        reject_unauthorized_sensitive_cleanup(scope, include_services, task_role.as_deref())?;
        let workspace = if scope == CleanScope::Workspace {
            Some(current_workspace(&paths, &workspace_services, execution_scope.as_ref()).await?)
        } else {
            None
        };
        let base_filter = scoped_filter(scope, &chat_id, tool_call_id, workspace);
        let mut killed = Vec::new();
        for mode in target_modes(include_services) {
            for status in [ExecStatusKind::Starting, ExecStatusKind::Running] {
                let mut filter = base_filter.clone();
                filter.mode = Some(mode.clone());
                filter.status = Some(status);
                killed.extend(exec_registry.remove_by_owner(filter).await?);
            }
        }
        killed.sort_by(|a, b| a.meta.process_id.as_str().cmp(b.meta.process_id.as_str()));
        let body = json!({
            "killed_count": killed.len(),
            "killed": killed.iter().map(killed_value).collect::<Vec<_>>(),
        });
        let mut extra = serde_json::Map::new();
        extra.insert("clean_background_processes".to_string(), body.clone());

        Ok((
            false,
            vec![ContextEnum::ChatMessage(ChatMessage {
                role: "tool".to_string(),
                content: ChatContent::SimpleText(body.to_string()),
                tool_calls: None,
                tool_call_id: tool_call_id.clone(),
                tool_failed: Some(false),
                output_filter: Some(OutputFilter::no_limits()),
                extra,
                ..Default::default()
            })],
        ))
    }

    fn tool_description(&self) -> ToolDesc {
        ToolDesc {
            name: "clean_background_processes".to_string(),
            display_name: "Clean Background Processes".to_string(),
            source: ToolSource {
                source_type: ToolSourceType::Builtin,
                config_path: self.config_path.clone(),
            },
            experimental: false,
            allow_parallel: false,
            description: "Kill and reap all non-terminal background processes owned by the current chat. Use to clean up after experiments. `scope=chat` (default) is available in normal chats. `scope=workspace`, `scope=all`, and `include_services=true` can execute only from planner/admin context and cannot be used by normal agents.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "scope": {
                        "type": "string",
                        "enum": ["chat", "owner", "workspace", "all"],
                        "default": "chat",
                        "description": "Which set of processes to target. `chat` (default) kills processes owned by the current chat — available in normal chats. `owner` is a compatibility alias for `chat`. `workspace` kills processes in the active workspace and can execute only from planner/admin context. `all` kills every process globally and can execute only from planner/admin context."
                    },
                    "include_services": {
                        "type": "boolean",
                        "default": false,
                        "description": "Also kill Service-mode processes. Can execute only from planner/admin context; normal agents must leave this false."
                    }
                }
            }),
            output_schema: None,
            annotations: None,
        }
    }

    fn tool_depends_on(&self) -> Vec<String> {
        vec![]
    }

    async fn command_to_match_against_confirm_deny(
        &self,
        _ccx: Arc<AMutex<AtCommandsContext>>,
        args: &HashMap<String, Value>,
    ) -> Result<String, String> {
        let scope = parse_scope(args)?;
        let include_services = parse_include_services(args)?;
        if scope == CleanScope::All || include_services {
            Ok(format!(
                "clean_background_processes scope={} include_services={}",
                scope.as_str(),
                include_services
            ))
        } else if scope == CleanScope::Workspace {
            Ok("clean_background_processes scope=workspace".to_string())
        } else {
            Ok(String::new())
        }
    }

    fn confirm_deny_rules(&self) -> Option<IntegrationConfirmation> {
        Some(IntegrationConfirmation {
            ask_user: vec!["*".to_string()],
            deny: Vec::new(),
        })
    }

    fn has_config_path(&self) -> Option<String> {
        Some(self.config_path.clone())
    }
}

fn parse_scope(args: &HashMap<String, Value>) -> Result<CleanScope, String> {
    match args.get("scope") {
        Some(Value::String(scope)) if scope.trim().is_empty() => Ok(CleanScope::Chat),
        Some(Value::String(scope)) => match scope.trim() {
            "chat" | "owner" => Ok(CleanScope::Chat),
            "workspace" => Ok(CleanScope::Workspace),
            "all" => Ok(CleanScope::All),
            other => Err(format!(
                "Invalid scope `{other}`. Must be one of: chat, owner, workspace, all"
            )),
        },
        Some(value) => Err(format!("argument `scope` is not a string: {value:?}")),
        None => Ok(CleanScope::Chat),
    }
}

fn parse_include_services(args: &HashMap<String, Value>) -> Result<bool, String> {
    match args.get("include_services") {
        Some(value) => refact_tool_api::coerce_bool(value)
            .ok_or_else(|| format!("argument `include_services` is not a boolean: {value:?}")),
        None => Ok(false),
    }
}

async fn current_workspace(
    paths: &refact_runtime_api::services::PathServices,
    workspace: &refact_runtime_api::services::WorkspaceServices,
    execution_scope: Option<&ExecutionScope>,
) -> Result<PathBuf, String> {
    if let Some(scope) = active_execution_scope(execution_scope) {
        return Ok(normalize_workspace_path(scope.effective_root()));
    }
    get_active_project_path_for_services(paths, workspace)
        .await
        .map(|path| normalize_workspace_path(&path))
        .ok_or_else(|| "No active project for background process cleanup".to_string())
}

fn scoped_filter(
    scope: CleanScope,
    chat_id: &str,
    _tool_call_id: &str,
    workspace: Option<PathBuf>,
) -> ExecProcessFilter {
    match scope {
        CleanScope::Chat => ExecProcessFilter {
            chat_id: Some(chat_id.to_string()),
            ..ExecProcessFilter::default()
        },
        CleanScope::Workspace => ExecProcessFilter {
            workspace,
            ..ExecProcessFilter::default()
        },
        CleanScope::All => ExecProcessFilter::default(),
    }
}

impl CleanScope {
    pub fn as_str(self) -> &'static str {
        match self {
            CleanScope::Chat => "chat",
            CleanScope::Workspace => "workspace",
            CleanScope::All => "all",
        }
    }
}

fn reject_unauthorized_sensitive_cleanup(
    scope: CleanScope,
    include_services: bool,
    task_role: Option<&str>,
) -> Result<(), String> {
    if is_sensitive_cleanup(scope, include_services) && !is_cleanup_admin_role(task_role) {
        return Err(
            "Workspace cleanup, global cleanup, and service cleanup require planner/admin context"
                .to_string(),
        );
    }
    Ok(())
}

fn is_sensitive_cleanup(scope: CleanScope, include_services: bool) -> bool {
    scope == CleanScope::Workspace || scope == CleanScope::All || include_services
}

fn is_cleanup_admin_role(task_role: Option<&str>) -> bool {
    matches!(task_role, Some("planner") | Some("admin"))
}

fn target_modes(include_services: bool) -> Vec<ExecMode> {
    if include_services {
        vec![ExecMode::Background, ExecMode::Service]
    } else {
        vec![ExecMode::Background]
    }
}

fn killed_value(snapshot: &ExecProcessSnapshot) -> Value {
    json!({
        "process_id": snapshot.meta.process_id.as_str(),
        "short_description": snapshot.meta.short_description.as_str(),
    })
}
