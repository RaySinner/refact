use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::io::AsyncReadExt;
use tokio::sync::Mutex as AMutex;
use refact_runtime_api::AtCommandsContext;
use refact_core::chat_types::{ChatContent, ChatMessage, ContextEnum};
use refact_chat_api::internal_roles::{self, EventSubkind};
use refact_chat_api::plan_role::PlanInstallReport;
use refact_chat_api::session_plan_role;
use refact_chat_api::types::{ChatSession, PendingDelivery, PushMode};
use crate::tools::tools_description::{ json_schema_from_params, Tool, ToolDesc, ToolSource, ToolSourceType, };
use refact_chat_api::setparams::map_legacy_mode_to_id;
pub const MAX_PLAN_FILE_BYTES: u64 = (internal_roles::MAX_PLAN_BODY_CHARS * 4 + 16 * 1024) as u64;

pub struct ToolSetPlan {
    pub config_path: String,
}

#[async_trait]
impl Tool for ToolSetPlan {
    fn tool_description(&self) -> ToolDesc {
        ToolDesc {
            name: "set_plan".to_string(),
            display_name: "Set Plan".to_string(),
            source: ToolSource {
                source_type: ToolSourceType::Builtin,
                config_path: self.config_path.clone(),
            },
            experimental: false,
            allow_parallel: false,
            description: "Install the chat's single detailed implementation plan (Markdown). Provide exactly one of `content` (full plan body) or `path` (absolute path to a `.md` report). Fails if a plan already exists — use `update_plan` to evolve it.".to_string(),
            input_schema: { let mut schema = json_schema_from_params(
                &[
                    ("content", "string", "Full Markdown plan body. Optional; provide exactly one of content or path."),
                    (
                        "path",
                        "string",
                        "Absolute path to a .md report to install as the plan",
                    ),
                    (
                        "summary",
                        "string",
                        "Short description of what changed, ≤120 chars. Optional.",
                    ),
                ],
                &[],
            ); schema["properties"]["push"] = PushMode::schema(); schema },
            output_schema: None,
            annotations: None,
        }
    }

    async fn tool_execute(
        &mut self,
        ccx: Arc<AMutex<AtCommandsContext>>,
        tool_call_id: &String,
        args: &HashMap<String, Value>,
    ) -> Result<(bool, Vec<ContextEnum>), String> {
        let push = PushMode::from_args(args)?;
        let content_arg = optional_string_arg(args, "content")?;
        let path_arg = optional_string_arg(args, "path")?;
        if content_arg.is_some() == path_arg.is_some() {
            return Err("provide exactly one of content or path".to_string());
        }
        let summary = optional_string_arg(args, "summary")?;
        if summary
            .as_ref()
            .is_some_and(|summary| summary.chars().count() > 120)
        {
            return Err("argument `summary` must be at most 120 chars".to_string());
        }

        let (gcx, chat_id) = {
            let cgcx = ccx.lock().await;
            (cgcx.app.gcx.clone(), cgcx.chat_id.clone())
        };
        let session_arc = {
            let sessions = gcx.chat_sessions.read().await;
            sessions.get(&chat_id).cloned()
        }
        .ok_or_else(|| format!("chat session `{chat_id}` not found"))?;
        {
            let session = session_arc.lock().await;
            if current_plan_including_queued(&session)?.is_some() {
                return Err("a plan already exists; use update_plan to change it".to_string());
            }
        }

        let content = match (content_arg, path_arg) {
            (Some(content), None) => content,
            (None, Some(path)) => read_plan_from_path(gcx.clone(), &path).await?,
            _ => unreachable!(),
        };
        if content.trim().is_empty() {
            return Err("argument `content` must be non-empty".to_string());
        }

        let report = {
            let mut session = session_arc.lock().await;
            if current_plan_including_queued(&session)?.is_some() {
                return Err("a plan already exists; use update_plan to change it".to_string());
            }
            let current_mode = map_legacy_mode_to_id(&session.thread.mode).to_string();
            let report = queue_plan_side_effect(
                &mut session,
                &current_mode,
                &content,
                push,
                tool_call_id,
                summary,
            )?;
            report
        };

        let result = json!({
            "version": report.version,
            "supersedes": report.supersedes,
        });

        Ok((
            false,
            vec![ContextEnum::ChatMessage(ChatMessage {
                role: "tool".to_string(),
                content: ChatContent::SimpleText(result.to_string()),
                tool_call_id: tool_call_id.clone(),
                ..Default::default()
            })],
        ))
    }
}

fn queue_plan_side_effect(
    session: &mut ChatSession,
    mode: &str,
    body: &str,
    push: PushMode,
    tool_call_id: &str,
    summary: Option<String>,
) -> Result<PlanInstallReport, String> {
    let mut delivery = PendingDelivery::new(
        vec![
            internal_roles::plan(mode, 1, body, None),
            internal_roles::event(
                EventSubkind::SystemNotice,
                "tool.set_plan",
                json!({"version": 1, "summary": summary}),
                "Plan updated to v1",
            ),
        ],
        push,
        "tool.set_plan",
        false,
    );
    delivery.after_tool_call_id = Some(tool_call_id.to_string());
    session.enqueue_delivery(delivery)?;
    Ok(PlanInstallReport {
        version: 1,
        supersedes: None,
    })
}

async fn read_plan_from_path(
    gcx: Arc<refact_runtime_api::global_context::GlobalContext>,
    path: &str,
) -> Result<String, String> {
    let requested_path = Path::new(path);
    if !requested_path.is_absolute() {
        return Err("argument `path` must be absolute".to_string());
    }
    if requested_path
        .extension()
        .and_then(|extension| extension.to_str())
        != Some("md")
    {
        return Err("argument `path` must point to a .md file".to_string());
    }

    let path = canonicalize_existing_plan_path(requested_path).await?;
    if path.extension().and_then(|extension| extension.to_str()) != Some("md") {
        return Err("argument `path` must resolve to a .md file".to_string());
    }
    let metadata = tokio::fs::metadata(&path)
        .await
        .map_err(|error| format!("failed to inspect plan file {}: {error}", path.display()))?;
    if !metadata.is_file() {
        return Err(format!(
            "argument `path` must point to a regular .md file: {}",
            path.display()
        ));
    }
    refact_runtime_api::files_in_workspace::check_file_privacy_for_send(gcx, &path).await?;

    let content = read_bounded_plan_file(&path).await?;
    if content.trim().is_empty() {
        return Err(format!("plan file {} is empty", path.display()));
    }
    Ok(content)
}

async fn canonicalize_existing_plan_path(path: &Path) -> Result<PathBuf, String> {
    tokio::fs::canonicalize(path)
        .await
        .map(|path| dunce::simplified(&path).to_path_buf())
        .map_err(|error| match error.kind() {
            std::io::ErrorKind::NotFound => format!("plan file {} does not exist", path.display()),
            _ => format!("failed to resolve plan file {}: {error}", path.display()),
        })
}

async fn read_bounded_plan_file(path: &Path) -> Result<String, String> {
    let file = tokio::fs::File::open(path)
        .await
        .map_err(|error| format!("failed to open plan file {}: {error}", path.display()))?;
    let mut reader = file.take(MAX_PLAN_FILE_BYTES + 1);
    let mut bytes = Vec::new();
    reader
        .read_to_end(&mut bytes)
        .await
        .map_err(|error| format!("failed to read plan file {}: {error}", path.display()))?;
    if bytes.len() as u64 > MAX_PLAN_FILE_BYTES {
        return Err("plan file is too large to read as a plan; use a smaller .md file".to_string());
    }
    String::from_utf8(bytes)
        .map_err(|error| format!("plan file {} is not valid UTF-8: {error}", path.display()))
}

fn current_plan_including_queued(session: &ChatSession) -> Result<Option<ChatMessage>, String> {
    session_plan_role::try_current_base_plan(session)
}

fn optional_string_arg(
    args: &HashMap<String, Value>,
    name: &str,
) -> Result<Option<String>, String> {
    match args.get(name) {
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(Value::Null) | None => Ok(None),
        Some(value) => Err(format!("argument `{name}` is not a string: {value:?}")),
    }
}
