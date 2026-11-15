use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::json;

use crate::app_state::AppState;
use refact_core::call_validation::{ChatContent, ChatMessage};
use refact_chat_api::internal_roles::{event, EventSubkind};
use crate::command_policy::build_exec_request_for_gcx as build_exec_request;
use refact_exec::command_policy::{CommandKind, CommandPolicyInput, ExecSource};
use refact_exec::{
    sanitize_short_description, ExecOutputStream, ExecOwnerMeta, ExecRawOutput, ExecReadResult,
    ExecStatus,
};
use crate::files_correction::get_active_project_path;
use refact_scheduler_core::types::{CommandSpec, Job};

pub use refact_scheduler_core::command_result::*;

const DEFAULT_COMMAND_TIMEOUT_SECS: u64 = 300;
const MAX_COMMAND_TIMEOUT_SECS: u64 = 3600;
const COMMAND_TRANSCRIPT_MAX_BYTES: usize = 2 * 1024 * 1024;

pub async fn run_command(app: &AppState, job: &Job, cmd: &CommandSpec) -> CommandRunResult {
    match run_command_inner(app, job, cmd).await {
        Ok(result) => result,
        Err(message) => CommandRunResult {
            status: "error".to_string(),
            stdout: String::new(),
            stderr: message,
            exit_code: None,
        },
    }
}

async fn run_command_inner(
    app: &AppState,
    job: &Job,
    cmd: &CommandSpec,
) -> Result<CommandRunResult, String> {
    validate_argv(&cmd.argv)?;
    let project_root = get_active_project_path(app.gcx.clone()).await;
    let cwd = resolve_cwd(project_root.as_deref(), cmd.cwd.as_deref())?;
    let owner = ExecOwnerMeta {
        chat_id: job.chat_id().map(str::to_string),
        tool_call_id: Some(format!("cron:{}", job.id)),
        service_name: None,
        workspace: project_root.clone(),
    };
    let timeout_secs = cmd
        .timeout_secs
        .unwrap_or(DEFAULT_COMMAND_TIMEOUT_SECS)
        .min(MAX_COMMAND_TIMEOUT_SECS);
    let request = build_exec_request(
        app.gcx.clone(),
        CommandPolicyInput {
            source: ExecSource::SchedulerJob,
            command: CommandKind::Argv(&cmd.argv),
            cwd,
            env: command_env(cmd),
            chat_mode: None,
            escalation: None,
        },
    )
    .await
    .map_err(|error| error.message)?
    .request
    .with_timeout(Duration::from_secs(timeout_secs))
    .with_owner(owner)
    .with_transcript_limit(COMMAND_TRANSCRIPT_MAX_BYTES)
    .with_short_description(sanitize_short_description(&job.description))
    .with_tty(false);
    let result = app.runtime.exec_registry.spawn(request).await?;
    let process_id = result.snapshot.meta.process_id.clone();
    let read = app.runtime.exec_registry.read(&process_id, 0, None).await;
    let raw_output = app
        .runtime
        .exec_registry
        .read_raw_capture(&process_id)
        .await;
    let (mut stdout, mut stderr) = collect_foreground_output(&read, raw_output.as_ref());
    if read.is_truncated {
        stderr.push_str(&format!(
            "Output was truncated by exec transcript limits ({} bytes kept, {} bytes dropped, {} chunks truncated).\n",
            read.current_bytes, read.dropped_bytes, read.truncated_chunks
        ));
    }
    if let Some(raw_output) = raw_output.as_ref() {
        if raw_output.is_truncated() {
            stderr.push_str(&format!(
                "Raw foreground capture reached limits (stdout: {}/{} bytes kept, {} bytes elided; stderr: {}/{} bytes kept, {} bytes elided).\n",
                raw_output.stdout_captured_bytes,
                raw_output.stdout_max_bytes,
                raw_output.stdout_elided_bytes,
                raw_output.stderr_captured_bytes,
                raw_output.stderr_max_bytes,
                raw_output.stderr_elided_bytes
            ));
        }
    }
    let exit_code = exec_exit_code(&result.snapshot.status);
    let status = if command_status_is_success(&result.snapshot.status) {
        "fired"
    } else {
        append_status_summary(&mut stderr, &result.snapshot.status, timeout_secs);
        "error"
    };
    Ok(CommandRunResult {
        status: status.to_string(),
        stdout: std::mem::take(&mut stdout),
        stderr,
        exit_code,
    })
}

pub fn command_output_message(task: &Job, result: &CommandRunResult) -> ChatMessage {
    let mut extra = serde_json::Map::new();
    extra.insert(
        "scheduler_command".to_string(),
        json!({
            "task_id": task.id,
            "status": result.status,
            "exit_code": result.exit_code,
        }),
    );
    ChatMessage {
        role: "plain_text".to_string(),
        content: ChatContent::SimpleText(result.stdout.clone()),
        extra,
        ..Default::default()
    }
}

pub fn command_error_notice_message(task: &Job, result: &CommandRunResult) -> ChatMessage {
    event(
        EventSubkind::SystemNotice,
        "scheduler.cron",
        json!({
            "task_id": task.id,
            "action_kind": "command",
            "status": result.status,
            "exit_code": result.exit_code,
        }),
        command_error_summary(result),
    )
}

pub fn validate_argv(argv: &[String]) -> Result<(), String> {
    if argv.is_empty() {
        return Err("command argv is empty".to_string());
    }
    if argv.iter().any(|part| part.trim().is_empty()) {
        return Err("command argv contains an empty argument".to_string());
    }
    Ok(())
}

pub fn command_env(cmd: &CommandSpec) -> HashMap<String, String> {
    cmd.env
        .clone()
        .unwrap_or_default()
        .into_iter()
        .collect::<HashMap<_, _>>()
}

pub fn resolve_cwd(project_root: Option<&Path>, cwd: Option<&str>) -> Result<Option<PathBuf>, String> {
    let Some(cwd) = cwd.map(str::trim).filter(|cwd| !cwd.is_empty()) else {
        return Ok(project_root.map(Path::to_path_buf));
    };
    let root =
        project_root.ok_or_else(|| "No active project root available for cwd".to_string())?;
    let candidate = PathBuf::from(cwd);
    let joined = if candidate.is_absolute() {
        candidate
    } else {
        root.join(candidate)
    };
    let normalized_root = normalize_existing_or_lexical(root);
    let normalized_joined = normalize_existing_or_lexical(&joined);
    if !normalized_joined.starts_with(&normalized_root) {
        return Err("cwd must stay within the active project".to_string());
    }
    Ok(Some(normalized_joined))
}

fn normalize_existing_or_lexical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path)
        .map(|path| dunce::simplified(&path).to_path_buf())
        .unwrap_or_else(|_| dunce::simplified(&lexical_normalize(path)).to_path_buf())
}

fn lexical_normalize(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                let last = normalized.components().next_back().map(|component| {
                    (
                        matches!(component, std::path::Component::Normal(_)),
                        matches!(component, std::path::Component::ParentDir),
                    )
                });
                match last {
                    Some((true, _)) => {
                        normalized.pop();
                    }
                    Some((_, true)) | None => normalized.push(".."),
                    Some((false, false)) => {}
                }
            }
            std::path::Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            std::path::Component::RootDir => normalized.push(component.as_os_str()),
            std::path::Component::Normal(part) => normalized.push(part),
        }
    }
    if normalized.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        normalized
    }
}

fn collect_exec_output(read: &ExecReadResult) -> (String, String) {
    let mut stdout = String::new();
    let mut stderr = String::new();
    for chunk in &read.chunks {
        match chunk.stream {
            ExecOutputStream::Stdout | ExecOutputStream::Combined => stdout.push_str(&chunk.text),
            ExecOutputStream::Stderr => stderr.push_str(&chunk.text),
        }
    }
    (stdout, stderr)
}

fn collect_foreground_output(
    read: &ExecReadResult,
    raw_output: Option<&ExecRawOutput>,
) -> (String, String) {
    raw_output
        .map(|raw| (raw.stdout.clone(), raw.stderr.clone()))
        .unwrap_or_else(|| collect_exec_output(read))
}

fn exec_exit_code(status: &ExecStatus) -> Option<i32> {
    match status {
        ExecStatus::Exited { exit_code } => *exit_code,
        ExecStatus::SandboxLauncherFailed { exit_code } => Some(*exit_code),
        ExecStatus::Starting
        | ExecStatus::Running
        | ExecStatus::Failed { .. }
        | ExecStatus::Killed
        | ExecStatus::TimedOut => None,
    }
}

fn command_status_is_success(status: &ExecStatus) -> bool {
    match status {
        ExecStatus::Exited { exit_code } => exit_code.unwrap_or_default() == 0,
        ExecStatus::Starting | ExecStatus::Running => true,
        ExecStatus::SandboxLauncherFailed { .. }
        | ExecStatus::Failed { .. }
        | ExecStatus::Killed
        | ExecStatus::TimedOut => false,
    }
}

fn append_status_summary(stderr: &mut String, status: &ExecStatus, timeout_secs: u64) {
    let summary = match status {
        ExecStatus::Exited { exit_code } => {
            format!("command exited with code {}", exit_code.unwrap_or_default())
        }
        ExecStatus::SandboxLauncherFailed { exit_code } => {
            format!("sandbox launcher failed with code {exit_code}")
        }
        ExecStatus::Failed { message } => format!("command failed: {message}"),
        ExecStatus::Killed => "command was killed".to_string(),
        ExecStatus::TimedOut => format!("command timed out after {timeout_secs} seconds"),
        ExecStatus::Starting | ExecStatus::Running => {
            format!("command ended with status {status:?}")
        }
    };
    if !stderr.is_empty() && !stderr.ends_with('\n') {
        stderr.push('\n');
    }
    stderr.push_str(&summary);
    stderr.push('\n');
}
