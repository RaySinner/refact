use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use serde::Deserialize;
use serde::Serialize;
use serde_json::{json, Value};
use tokio::sync::Mutex as AMutex;
use tracing::info;

#[cfg(not(target_os = "windows"))]
use shell_escape::escape;

use crate::at_commands_ctx::AtCommandsContext;
use crate::command_policy::build_exec_request_for_gcx as build_exec_request;
use crate::files_correction::get_project_dirs;
use crate::gcx_helpers::{
    filter_path_enrichment_for_model_context, tool_enrichment_from_path_references,
};
use crate::global_context::GlobalContext;
use crate::integr_abstract::IntegrationTrait;
use crate::integrations::setting_up_integrations::get_vars_for_replacements;
use crate::privacy::load_privacy_if_needed;
use crate::Tool;
use refact_buddy_core::user_action::UserAction;
use refact_core::chat_types::{ChatContent, ChatMessage, ContextEnum};
use refact_core::custom_error::YamlError;
use refact_exec::command_policy::{CommandKind, CommandPolicyInput, ExecSource};
use refact_exec::{
    generate_short_description, sanitize_short_description, ExecMode, ExecOutputStream,
    ExecOwnerMeta, ExecProcessSnapshot, ExecRawOutput, ExecReadResult, ExecStatus,
};
use refact_integrations::integr_abstract::IntegrationCommon;
use refact_integrations::utils::{
    deserialize_str_to_num, deserialize_str_to_opt_num, serialize_num_to_str,
    serialize_opt_num_to_str,
};
use refact_postprocessing::pp_command_output::{output_mini_postprocessing, OutputFilter};
use refact_tool_api::IntegrationConfirmation;
use refact_tool_api::{ToolDesc, ToolSource, ToolSourceType};

const CMDLINE_TRANSCRIPT_MAX_BYTES: usize = 2 * 1024 * 1024;

#[derive(Deserialize, Serialize, Clone, Default)]
pub struct CmdlineParam {
    pub name: String,
    #[serde(rename = "type", default = "CmdlineParam::default_type")]
    pub param_type: String,
    #[serde(default)]
    pub description: String,
}

impl CmdlineParam {
    fn default_type() -> String {
        "string".to_string()
    }
}

#[derive(Deserialize, Serialize, Clone, Default)]
pub struct CmdlineToolConfig {
    pub command: String,
    pub command_workdir: String,

    pub description: String,
    pub parameters: Vec<CmdlineParam>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub parameters_required: Option<Vec<String>>,

    // blocking
    #[serde(default)]
    pub timeout: String,
    #[serde(default)]
    pub output_filter: OutputFilter,

    // background
    #[serde(
        default,
        serialize_with = "serialize_opt_num_to_str",
        deserialize_with = "deserialize_str_to_opt_num"
    )]
    pub startup_wait_port: Option<u16>,
    #[serde(
        default = "_default_startup_wait",
        serialize_with = "serialize_num_to_str",
        deserialize_with = "deserialize_str_to_num"
    )]
    pub startup_wait: u64,
    #[serde(default)]
    pub startup_wait_keyword: String,
}

fn _default_startup_wait() -> u64 {
    10
}

#[derive(Default)]
pub struct ToolCmdline {
    pub common: IntegrationCommon,
    pub name: String,
    pub cfg: CmdlineToolConfig,
    pub config_path: String,
}

#[async_trait]
impl IntegrationTrait for ToolCmdline {
    async fn integr_settings_apply(
        &mut self,
        _gcx: Arc<GlobalContext>,
        config_path: String,
        value: &serde_json::Value,
    ) -> Result<(), serde_json::Error> {
        self.cfg = serde_json::from_value(value.clone())?;
        self.common = serde_json::from_value(value.clone())?;
        self.config_path = config_path;
        Ok(())
    }

    fn integr_settings_as_json(&self) -> serde_json::Value {
        serde_json::to_value(&self.cfg).unwrap()
    }

    fn integr_common(&self) -> IntegrationCommon {
        self.common.clone()
    }

    async fn integr_tools(
        &self,
        integr_name: &str,
    ) -> Vec<Box<dyn crate::Tool + Send>> {
        vec![Box::new(ToolCmdline {
            common: self.common.clone(),
            name: integr_name.to_string(),
            cfg: self.cfg.clone(),
            config_path: self.config_path.clone(),
        })]
    }

    fn integr_schema(&self) -> &str {
        CMDLINE_INTEGRATION_SCHEMA
    }
}

#[cfg(target_os = "windows")]
pub fn powershell_escape(s: &str) -> String {
    let mut needs_escape = s.is_empty();
    for ch in s.chars() {
        match ch {
            ' ' | '"' | '\'' | '$' | '`' | '[' | ']' | '{' | '}' | '(' | ')' | '@' | '&' | '#'
            | ',' | ';' | '.' | '\t' | '\n' | '|' | '<' | '>' | '\\' => {
                needs_escape = true;
                break;
            }
            _ => {}
        }
    }

    if !needs_escape {
        return s.to_string();
    }

    let mut es = String::with_capacity(s.len() + 2);
    es.push('"');

    for ch in s.chars() {
        match ch {
            '"' => es.push_str("`\""),
            '$' => es.push_str("`$"),
            '`' => es.push_str("``"),
            '\t' => es.push_str("`t"),
            '\n' => es.push_str("`n"),
            '\\' => es.push_str("\\"),
            _ => es.push(ch),
        }
    }

    es.push('"');
    es
}

pub fn replace_args(x: &str, args_str: &HashMap<String, String>) -> String {
    let mut result = x.to_string();
    for (key, value) in args_str {
        let escaped_value = if value == "" {
            // special case for an empty paramter, we want it empty as replacement, rather than escaped empty string ""
            "".to_string()
        } else {
            #[cfg(target_os = "windows")]
            let x = powershell_escape(value);
            #[cfg(not(target_os = "windows"))]
            let x = escape(std::borrow::Cow::from(value.as_str())).to_string();
            x
        };
        result = result.replace(&format!("%{}%", key), &escaped_value);
    }
    result
}

pub use refact_integrations::process_io_utils::format_output;

pub fn resolve_cmdline_workdir(command_workdir: &str, project_dirs: &[PathBuf]) -> Option<PathBuf> {
    if command_workdir.is_empty() {
        if let Some(first_project_dir) = project_dirs.first() {
            return Some(dunce::simplified(first_project_dir).to_path_buf());
        }
        tracing::warn!("no working directory, using whatever directory this binary is run :/");
        None
    } else {
        let path = PathBuf::from(command_workdir);
        Some(dunce::simplified(path.as_path()).to_path_buf())
    }
}

pub fn collect_exec_output(read: &ExecReadResult) -> (String, String) {
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

pub fn collect_foreground_output(
    read: &ExecReadResult,
    raw_output: Option<&ExecRawOutput>,
) -> (String, String) {
    raw_output
        .map(|raw| (raw.stdout.clone(), raw.stderr.clone()))
        .unwrap_or_else(|| collect_exec_output(read))
}

pub fn exec_status_label(status: &ExecStatus) -> &'static str {
    match status {
        ExecStatus::Starting => "starting",
        ExecStatus::Running => "running",
        ExecStatus::Exited { .. } => "exited",
        ExecStatus::SandboxLauncherFailed { .. } => "sandbox_launcher_failed",
        ExecStatus::Failed { .. } => "failed",
        ExecStatus::Killed => "killed",
        ExecStatus::TimedOut => "timed_out",
    }
}

pub fn exec_exit_code(status: &ExecStatus) -> Option<i32> {
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

pub fn append_status_line(
    out: &mut String,
    status: &ExecStatus,
    duration: Duration,
    timeout_secs: u64,
) {
    match status {
        ExecStatus::Exited {
            exit_code: Some(exit_code),
        } => out.push_str(&format!(
            "The command was running {:.3}s, finished with exit code {exit_code}\n",
            duration.as_secs_f64()
        )),
        ExecStatus::Exited { exit_code: None } => out.push_str(&format!(
            "The command was running {:.3}s, finished with unknown exit status\n",
            duration.as_secs_f64()
        )),
        ExecStatus::SandboxLauncherFailed { exit_code } => out.push_str(&format!(
            "⚠️ The sandbox launcher failed before the command ran (exit code {exit_code}).\n"
        )),
        ExecStatus::Killed => out.push_str(&format!(
            "⚠️ The command was interrupted by user after {:.3}s (process killed). Output above may be incomplete.\n",
            duration.as_secs_f64()
        )),
        ExecStatus::TimedOut => out.push_str(&format!(
            "⚠️ The command timed out after {} seconds (process killed). Output above may be incomplete.\n",
            timeout_secs
        )),
        ExecStatus::Failed { message } => out.push_str(&format!(
            "⚠️ The command failed after {:.3}s: {}\n",
            duration.as_secs_f64(),
            message
        )),
        ExecStatus::Starting | ExecStatus::Running => out.push_str(&format!(
            "⚠️ The command did not reach a terminal state after {:.3}s (status: {}).\n",
            duration.as_secs_f64(),
            exec_status_label(status)
        )),
    }
}

pub fn tool_failed_for_status(status: &ExecStatus) -> Option<bool> {
    match status {
        ExecStatus::Exited { exit_code: Some(0) } => Some(false),
        ExecStatus::Exited { exit_code: None }
        | ExecStatus::Exited { exit_code: Some(_) }
        | ExecStatus::SandboxLauncherFailed { .. }
        | ExecStatus::Failed { .. }
        | ExecStatus::Killed
        | ExecStatus::TimedOut => Some(true),
        ExecStatus::Starting | ExecStatus::Running => None,
    }
}

pub fn exec_extra(
    snapshot: &ExecProcessSnapshot,
    duration: Duration,
) -> serde_json::Map<String, Value> {
    let mut extra = serde_json::Map::new();
    let cwd = snapshot
        .meta
        .cwd
        .as_ref()
        .map(|path| path.to_string_lossy().to_string())
        .unwrap_or_default();
    extra.insert(
        "exec".to_string(),
        json!({
            "process_id": snapshot.meta.process_id.as_str(),
            "status": exec_status_label(&snapshot.status),
            "exit_code": exec_exit_code(&snapshot.status),
            "duration_ms": duration.as_millis() as u64,
            "short_description": snapshot.meta.short_description,
            "command": snapshot.meta.command,
            "cwd": cwd,
            "mode": snapshot.meta.mode.to_string(),
            "created_at_ms": snapshot.meta.created_at_ms,
            "started_at_ms": snapshot.meta.started_at_ms,
            "ended_at_ms": snapshot.meta.ended_at_ms,
            "service_name": snapshot.meta.owner.service_name,
        }),
    );
    extra
}

pub fn cmdline_short_description(name: &str, description: &str, command: &str) -> String {
    let raw = match (name.trim().is_empty(), description.trim().is_empty()) {
        (false, false) => format!("{}: {}", name, description),
        (false, true) => name.to_string(),
        (true, false) => description.to_string(),
        (true, true) => String::new(),
    };
    let description = sanitize_short_description(&raw);
    if description.is_empty() {
        generate_short_description(command, &ExecMode::Foreground)
    } else {
        description
    }
}

pub async fn execute_blocking_command(
    gcx: Arc<GlobalContext>,
    command: &str,
    cfg: &CmdlineToolConfig,
    command_workdir: &str,
    env_variables: &HashMap<String, String>,
    project_dirs: Vec<PathBuf>,
    exec_registry: &refact_exec::ExecRegistry,
    owner: ExecOwnerMeta,
    abort_flag: Arc<AtomicBool>,
    short_description: String,
) -> Result<(String, String, bool, ExecProcessSnapshot, Duration), String> {
    info!("EXEC workdir {:?}:\n{:?}", command_workdir, command);

    let timeout_secs = cfg.timeout.parse::<u64>().unwrap_or(10);
    let cwd = resolve_cmdline_workdir(command_workdir, &project_dirs);
    let request = build_exec_request(
        gcx,
        CommandPolicyInput {
            source: ExecSource::CmdlineIntegration,
            command: CommandKind::Shell(command),
            cwd,
            env: env_variables.clone(),
            chat_mode: None,
            escalation: None,
        },
    )
    .await
    .map_err(|error| error.message)?
    .request
    .with_timeout(Duration::from_secs(timeout_secs))
    .with_owner(owner)
    .with_transcript_limit(CMDLINE_TRANSCRIPT_MAX_BYTES)
    .with_short_description(short_description)
    .with_abort_flag(abort_flag);
    tracing::info!("command: {}", command);

    let started = tokio::time::Instant::now();
    let result = exec_registry.spawn(request).await?;
    let duration = started.elapsed();
    info!("EXEC: /finished in {:?}", duration);

    let read = exec_registry
        .read(&result.snapshot.meta.process_id, 0, None)
        .await;
    let raw_output = exec_registry
        .read_raw_capture(&result.snapshot.meta.process_id)
        .await;
    let (stdout, stderr) = collect_foreground_output(&read, raw_output.as_ref());
    let diagnostic_output =
        refact_exec::path_enrichment::bounded_diagnostic_output(&stdout, &stderr);

    let stdout = output_mini_postprocessing(&cfg.output_filter, &stdout);
    let stderr = output_mini_postprocessing(&cfg.output_filter, &stderr);

    let mut out = format_output(&stdout, &stderr);
    if read.is_truncated {
        out.push_str(&format!(
            "⚠️ Output was truncated by exec transcript limits ({} bytes kept, {} bytes dropped, {} chunks truncated).\n",
            read.current_bytes, read.dropped_bytes, read.truncated_chunks
        ));
    }
    if let Some(raw_output) = raw_output.as_ref() {
        if raw_output.is_truncated() {
            out.push_str(&format!(
                "⚠️ Raw foreground capture reached limits (stdout: {}/{} bytes kept, {} bytes elided; stderr: {}/{} bytes kept, {} bytes elided).\n",
                raw_output.stdout_captured_bytes,
                raw_output.stdout_max_bytes,
                raw_output.stdout_elided_bytes,
                raw_output.stderr_captured_bytes,
                raw_output.stderr_max_bytes,
                raw_output.stderr_elided_bytes
            ));
        }
    }
    append_status_line(&mut out, &result.snapshot.status, duration, timeout_secs);
    Ok((
        out,
        diagnostic_output.0,
        diagnostic_output.1,
        result.snapshot,
        duration,
    ))
}

pub fn _parse_command_args(
    args: &HashMap<String, serde_json::Value>,
    cfg: &CmdlineToolConfig,
) -> Result<(String, String), String> {
    let mut args_str: HashMap<String, String> = HashMap::new();
    let valid_params: Vec<String> = cfg.parameters.iter().map(|p| p.name.clone()).collect();

    for (k, v) in args.iter() {
        if !valid_params.contains(k) {
            return Err(format!("Unexpected argument `{}`", k));
        }
        match v {
            serde_json::Value::String(s) => {
                args_str.insert(k.clone(), s.clone());
            }
            _ => return Err(format!("argument `{}` is not a string: {:?}", k, v)),
        }
    }

    for param in &cfg.parameters {
        if cfg
            .parameters_required
            .as_ref()
            .map_or(false, |req| req.contains(&param.name))
            && !args_str.contains_key(&param.name)
        {
            return Err(format!("Missing required argument `{}`", param.name));
        }
    }

    let command = replace_args(cfg.command.as_str(), &args_str);
    let workdir = replace_args(cfg.command_workdir.as_str(), &args_str);
    Ok((command, workdir))
}

#[async_trait]
impl Tool for ToolCmdline {
    async fn tool_execute(
        &mut self,
        ccx: Arc<AMutex<AtCommandsContext>>,
        tool_call_id: &String,
        args: &HashMap<String, serde_json::Value>,
    ) -> Result<(bool, Vec<ContextEnum>), String> {
        let (command, workdir) = _parse_command_args(args, &self.cfg)?;

        let (gcx, exec_registry, abort_flag, chat_id, current_model, derived_privacy_zones) = {
            let cgcx = ccx.lock().await;
            (
                cgcx.app.gcx.clone(),
                cgcx.app.runtime.exec_registry.clone(),
                cgcx.abort_flag.clone(),
                cgcx.chat_id.clone(),
                cgcx.current_model.clone(),
                cgcx.derived_privacy_zones.clone(),
            )
        };
        load_privacy_if_needed(gcx.clone()).await;
        let destination = refact_privacy::provider_destination(&current_model);
        let user_activity = gcx.user_activity.clone();
        if let Ok(mut ring) = user_activity.try_lock() {
            ring.push(UserAction::CommandRun {
                command_preview: command.chars().take(80).collect(),
                chat_id: chat_id.clone(),
                ts: Utc::now(),
            });
        };
        let mut error_log = Vec::<YamlError>::new();
        let env_variables =
            get_vars_for_replacements(
                gcx.clone(),
                &mut error_log,
            )
            .await;
        let project_dirs = get_project_dirs(gcx.clone()).await;
        let enrichment_roots = project_dirs.clone();

        let owner = ExecOwnerMeta {
            chat_id: Some(chat_id),
            tool_call_id: Some(tool_call_id.clone()),
            service_name: None,
            workspace: project_dirs.first().cloned(),
        };
        let short_description =
            cmdline_short_description(&self.name, &self.cfg.description, &command);
        let (tool_output, diagnostic_output, diagnostic_truncated, snapshot, duration) =
            execute_blocking_command(
                gcx.clone(),
                &command,
                &self.cfg,
                &workdir,
                &env_variables,
                project_dirs,
                &exec_registry,
                owner,
                abort_flag,
                short_description,
            )
            .await?;

        let cwd = snapshot
            .meta
            .cwd
            .as_deref()
            .or(snapshot.meta.owner.workspace.as_deref())
            .unwrap_or_else(|| std::path::Path::new("."));
        let workspace_roots = if enrichment_roots.is_empty() {
            vec![snapshot
                .meta
                .owner
                .workspace
                .clone()
                .unwrap_or_else(|| cwd.to_path_buf())]
        } else {
            enrichment_roots
        };
        let collected = refact_exec::path_enrichment::collect_async_with_truncation(
            &command,
            cwd,
            &workspace_roots,
            diagnostic_output,
            diagnostic_truncated,
        )
        .await;
        let enrichment = filter_path_enrichment_for_model_context(
            gcx,
            &destination,
            &derived_privacy_zones,
            collected,
        )
        .await;
        let mut extra = exec_extra(&snapshot, duration);
        refact_chat_api::attach_tool_enrichment_to_extra(
            &mut extra,
            tool_enrichment_from_path_references(enrichment),
        );

        let result = vec![ContextEnum::ChatMessage(ChatMessage {
            role: "tool".to_string(),
            content: ChatContent::SimpleText(tool_output),
            tool_calls: None,
            tool_call_id: tool_call_id.clone(),
            tool_failed: tool_failed_for_status(&snapshot.status),
            output_filter: Some(OutputFilter::no_limits()),
            extra,
            ..Default::default()
        })];

        Ok((false, result))
    }

    fn tool_depends_on(&self) -> Vec<String> {
        vec![]
    }

    fn tool_description(&self) -> ToolDesc {
        let required: Vec<String> = self
            .cfg
            .parameters_required
            .clone()
            .unwrap_or_else(|| self.cfg.parameters.iter().map(|p| p.name.clone()).collect());
        let mut properties = serde_json::Map::new();
        for p in &self.cfg.parameters {
            properties.insert(
                p.name.clone(),
                json!({
                    "type": p.param_type,
                    "description": p.description
                }),
            );
        }
        let input_schema = json!({
            "type": "object",
            "properties": properties,
            "required": required
        });
        ToolDesc {
            name: self.name.clone(),
            display_name: self.name.clone(),
            source: ToolSource {
                source_type: ToolSourceType::Integration,
                config_path: self.config_path.clone(),
            },
            experimental: false,
            allow_parallel: false,
            description: self.cfg.description.clone(),
            input_schema,
            output_schema: None,
            annotations: None,
        }
    }

    async fn command_to_match_against_confirm_deny(
        &self,
        _ccx: Arc<AMutex<AtCommandsContext>>,
        args: &HashMap<String, serde_json::Value>,
    ) -> Result<String, String> {
        let (command, _workdir) = _parse_command_args(args, &self.cfg)?;
        Ok(command)
    }

    fn confirm_deny_rules(&self) -> Option<IntegrationConfirmation> {
        Some(self.integr_common().confirmation)
    }

    fn has_config_path(&self) -> Option<String> {
        Some(self.config_path.clone())
    }
}

pub const CMDLINE_INTEGRATION_SCHEMA: &str = r#"
fields:
  command:
    f_type: string_long
    f_desc: "The command to execute. To let model produce part of the command, use %param_name% notation."
    f_placeholder: "echo Hello World"
  command_workdir:
    f_type: string_long
    f_desc: "The working directory for the command. If empty then workspace directory will be used. There you can use %param_name% as well."
    f_placeholder: "/path/to/workdir"
  description:
    f_type: string_long
    f_desc: "The model will see this description, why the model should call this?"
  parameters:
    f_type: "tool_parameters"
    f_desc: "The parameters that the model should fill out. Use description to tell the model what a parameter does. The only way you can use values coming from the model is to put them into %param_name% notation in the command or the working directory."
  timeout:
    f_type: string_short
    f_desc: "The command must immediately return the results, it can't be interactive. If the command runs for too long, it will be terminated and stderr/stdout collected will be presented to the model."
    f_default: "10"
  output_filter:
    f_type: "output_filter"
    f_desc: "The output from the command can be long or even quasi-infinite. This section allows to set limits, prioritize top or bottom, or use regexp to show the model the relevant part."
    f_placeholder: "filter"
    f_extra: true
description: |
  There you can adapt any command line tool for use by AI model. You can give the model instructions why to call it, which parameters to provide,
  set a timeout and restrict the output. If you want a tool that runs in the background such as a web server, use service_* instead.
available:
  on_your_laptop_possible: true
  when_isolated_possible: true
confirmation:
  ask_user_default: ["*"]
  deny_default: ["sudo*"]
smartlinks:
  - sl_label: "Test"
    sl_chat:
      - role: "user"
        content: |
          🔧 Test the tool that corresponds to %CURRENT_CONFIG%
          If the tool isn't available or doesn't work, go through the usual plan in the system prompt. If it works express happiness, and change nothing.
    sl_enable_only_with_tool: true
  - sl_label: "Auto Configure"
    sl_chat:
      - role: "user"
        content: |
          🔧 Please write %CURRENT_CONFIG% based on what you see in the project. Follow the plan in the system prompt.
"#;

