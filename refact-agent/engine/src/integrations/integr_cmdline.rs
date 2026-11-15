pub use refact_runtime_api::integrations::cmdline::*;

#[cfg(test)]
use std::collections::HashMap;
#[cfg(test)]
use std::sync::atomic::AtomicBool;
#[cfg(test)]
use std::sync::Arc;
#[cfg(test)]
use std::time::Duration;

#[cfg(test)]
use serde_json::{json, Value};
#[cfg(test)]
use tokio::sync::Mutex as AMutex;

#[cfg(test)]
use crate::at_commands::at_commands::AtCommandsContext;
#[cfg(test)]
use crate::call_validation::{ChatContent, ChatMessage, ContextEnum};
#[cfg(test)]
use crate::exec::{ExecMode, ExecProcessSnapshot, ExecStatus};
#[cfg(test)]
use crate::global_context::GlobalContext;
#[cfg(test)]
use crate::integrations::integr_abstract::{IntegrationCommon, IntegrationConfirmation};
#[cfg(test)]
use crate::postprocessing::pp_command_output::OutputFilter;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_state::AppState;
    use refact_tool_api::MatchConfirmDenyResult;
    use refact_runtime_api::Tool;

    fn args(entries: Vec<(&str, Value)>) -> HashMap<String, Value> {
        entries
            .into_iter()
            .map(|(key, value)| (key.to_string(), value))
            .collect()
    }

    async fn test_ccx() -> (Arc<GlobalContext>, Arc<AMutex<AtCommandsContext>>) {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let ccx = crate::at_commands::at_commands::at_commands_new_with_abort(
            crate::app_state::app_state_from_gcx(gcx.clone()).await,
            4096,
            20,
            false,
            Vec::new(),
            "chat".to_string(),
            None,
            "model".to_string(),
            None,
            None,
            None,
        )
        .await;
        (gcx, Arc::new(AMutex::new(ccx)))
    }

    fn only_message(messages: Vec<ContextEnum>) -> ChatMessage {
        match messages.into_iter().next().unwrap() {
            ContextEnum::ChatMessage(message) => message,
            ContextEnum::ContextFile(_) => panic!("expected chat message"),
        }
    }

    fn text(message: &ChatMessage) -> String {
        match &message.content {
            ChatContent::SimpleText(text) => text.clone(),
            _ => String::new(),
        }
    }

    fn exec(message: &ChatMessage) -> &Value {
        message.extra.get("exec").unwrap()
    }

    fn success_command() -> String {
        if cfg!(target_os = "windows") {
            "[Console]::Out.Write('hello')".to_string()
        } else {
            "printf hello".to_string()
        }
    }

    fn delayed_success_command() -> String {
        if cfg!(target_os = "windows") {
            "Start-Sleep -Milliseconds 20; [Console]::Out.Write('hello')".to_string()
        } else {
            "sleep 0.02; printf hello".to_string()
        }
    }

    fn platform_timeout_secs() -> String {
        if cfg!(target_os = "windows") {
            "60".to_string()
        } else {
            "10".to_string()
        }
    }

    fn failure_command() -> String {
        if cfg!(target_os = "windows") {
            "[Console]::Out.Write('bad'); exit 7".to_string()
        } else {
            "printf bad; exit 7".to_string()
        }
    }

    fn slow_command() -> String {
        if cfg!(target_os = "windows") {
            "[Console]::Out.WriteLine('start'); Start-Sleep -Seconds 5".to_string()
        } else {
            "printf start; sleep 5".to_string()
        }
    }

    fn assert_partial_start_output(body: &str) {
        if cfg!(target_os = "windows") {
            return;
        }
        assert!(body.contains("start"));
    }

    fn late_marker_command(marker: &str) -> String {
        if cfg!(target_os = "windows") {
            format!(
                "$chunk = ('f' * 1024) + \"`n\"; [Console]::Out.Write($chunk * 1024); [Console]::Out.Write('{marker}`n'); $tail = ('t' * 1024) + \"`n\"; [Console]::Out.Write($tail * 3072)"
            )
        } else {
            format!(
                "chunk=$(printf '%01024d\\n' 0 | tr '0' f); i=0; while [ $i -lt 1024 ]; do printf '%s\\n' \"$chunk\"; i=$((i + 1)); done; printf '%s\\n' '{marker}'; chunk=$(printf '%01024d\\n' 0 | tr '0' t); i=0; while [ $i -lt 3072 ]; do printf '%s\\n' \"$chunk\"; i=$((i + 1)); done"
            )
        }
    }

    async fn run_tool(mut tool: ToolCmdline, args: HashMap<String, Value>) -> ChatMessage {
        let (_, ccx) = test_ccx().await;
        let (_, messages) = tool
            .tool_execute(ccx, &"tool_call".to_string(), &args)
            .await
            .unwrap();
        only_message(messages)
    }

    #[test]
    fn replace_args_keeps_shell_escaping_and_empty_replacement() {
        let replaced = replace_args(
            "echo %target% %empty%",
            &HashMap::from([
                ("target".to_string(), "hello world".to_string()),
                ("empty".to_string(), String::new()),
            ]),
        );

        #[cfg(target_os = "windows")]
        assert_eq!(replaced, "echo \"hello world\" ");
        #[cfg(not(target_os = "windows"))]
        assert_eq!(replaced, "echo 'hello world' ");
    }

    #[tokio::test]
    async fn configured_cmdline_success_uses_exec_runtime_and_metadata() {
        let (gcx, ccx) = test_ccx().await;
        let mut tool = ToolCmdline {
            name: "cmdline_hello".to_string(),
            cfg: CmdlineToolConfig {
                command: success_command(),
                description: "Say hello".to_string(),
                parameters: Vec::new(),
                ..CmdlineToolConfig::default()
            },
            ..ToolCmdline::default()
        };

        let (_, messages) = tool
            .tool_execute(ccx, &"tool_call".to_string(), &args(vec![]))
            .await
            .unwrap();
        let message = only_message(messages);
        let body = text(&message);
        let exec = exec(&message);

        assert!(body.contains("hello"));
        assert!(body.contains("exit code 0"));
        assert_eq!(exec["status"], "exited");
        assert_eq!(exec["exit_code"], 0);
        assert_eq!(exec["short_description"], "cmdline_hello: Say hello");
        let process_id = exec["process_id"].as_str().unwrap();
        assert!(process_id.starts_with("exec_"));
        assert!(gcx
            .exec_registry
            .list(Default::default())
            .await
            .iter()
            .any(|snapshot| snapshot.meta.process_id.as_str() == process_id));
        assert_eq!(message.tool_failed, Some(false));
    }

    #[tokio::test]
    async fn cmdline_extra_exec_full_shape() {
        let command = delayed_success_command();
        let temp_dir = std::env::temp_dir();
        let expected_cwd = dunce::simplified(temp_dir.as_path())
            .to_string_lossy()
            .to_string();
        let message = run_tool(
            ToolCmdline {
                name: "cmdline_duration".to_string(),
                cfg: CmdlineToolConfig {
                    command: command.clone(),
                    description: "Measure duration".to_string(),
                    parameters: Vec::new(),
                    command_workdir: expected_cwd.clone(),
                    timeout: platform_timeout_secs(),
                    ..CmdlineToolConfig::default()
                },
                ..ToolCmdline::default()
            },
            args(vec![]),
        )
        .await;
        let exec = exec(&message);
        let object = exec.as_object().unwrap();
        let keys = [
            "process_id",
            "status",
            "exit_code",
            "duration_ms",
            "short_description",
            "command",
            "cwd",
            "mode",
            "created_at_ms",
            "started_at_ms",
            "ended_at_ms",
            "service_name",
        ];

        for key in keys {
            assert!(object.contains_key(key), "missing exec.{key}");
        }
        assert!(exec["process_id"].as_str().unwrap().starts_with("exec_"));
        assert_eq!(exec["status"], "exited");
        assert_eq!(exec["exit_code"], 0);
        assert!(exec["duration_ms"].as_u64().unwrap() > 0);
        assert_eq!(
            exec["short_description"],
            "cmdline_duration: Measure duration"
        );
        assert_eq!(exec["command"], command);
        assert_eq!(exec["cwd"], expected_cwd);
        assert_eq!(exec["mode"], "foreground");
        assert!(exec["created_at_ms"].is_u64());
        assert!(exec["started_at_ms"].is_u64());
        assert!(exec["ended_at_ms"].is_u64());
        assert!(exec["service_name"].is_null());

        let pending = ExecProcessSnapshot::new(crate::exec::ExecProcessMeta::new(
            ExecMode::Foreground,
            "pending".to_string(),
        ));
        let pending_extra = exec_extra(&pending, Duration::from_millis(0));
        let pending_exec = pending_extra.get("exec").unwrap();
        assert!(pending_exec["started_at_ms"].is_null());
        assert!(pending_exec["ended_at_ms"].is_null());
        assert!(pending_exec["service_name"].is_null());
    }

    #[tokio::test]
    async fn cmdline_extra_exec_no_legacy_duration_field() {
        let message = run_tool(
            ToolCmdline {
                name: "cmdline_duration".to_string(),
                cfg: CmdlineToolConfig {
                    command: delayed_success_command(),
                    description: "Measure duration".to_string(),
                    parameters: Vec::new(),
                    ..CmdlineToolConfig::default()
                },
                ..ToolCmdline::default()
            },
            args(vec![]),
        )
        .await;

        assert!(exec(&message)
            .get(&["duration", "secs"].join("_"))
            .is_none());
    }

    #[tokio::test]
    async fn configured_cmdline_failure_reports_exit_code_in_metadata() {
        let message = run_tool(
            ToolCmdline {
                name: "cmdline_fail".to_string(),
                cfg: CmdlineToolConfig {
                    command: failure_command(),
                    description: "Fail nicely".to_string(),
                    parameters: Vec::new(),
                    ..CmdlineToolConfig::default()
                },
                ..ToolCmdline::default()
            },
            args(vec![]),
        )
        .await;
        let body = text(&message);
        let exec = exec(&message);

        assert!(body.contains("bad"));
        assert!(body.contains("exit code 7"));
        assert_eq!(exec["status"], "exited");
        assert_eq!(exec["exit_code"], 7);
        assert_eq!(message.tool_failed, Some(true));
    }

    #[test]
    fn cmdline_exit_status_rendering_metadata_and_failure_are_consistent() {
        let duration = Duration::from_millis(250);
        let cases = [
            (
                ExecStatus::Exited { exit_code: Some(0) },
                "finished with exit code 0",
                json!(0),
                Some(false),
            ),
            (
                ExecStatus::Exited { exit_code: Some(7) },
                "finished with exit code 7",
                json!(7),
                Some(true),
            ),
            (
                ExecStatus::Exited { exit_code: None },
                "finished with unknown exit status",
                Value::Null,
                Some(true),
            ),
        ];

        for (status, expected_output, expected_exit_code, expected_failed) in cases {
            let mut output = String::new();
            append_status_line(&mut output, &status, duration, 10);
            let snapshot = ExecProcessSnapshot::new(crate::exec::ExecProcessMeta::new(
                ExecMode::Foreground,
                "status".to_string(),
            ))
            .with_status(status.clone());
            let extra = exec_extra(&snapshot, duration);
            let exec = extra.get("exec").unwrap();

            assert!(output.contains(expected_output));
            assert_eq!(exec["status"], "exited");
            assert_eq!(exec["exit_code"], expected_exit_code);
            assert_eq!(tool_failed_for_status(&status), expected_failed);
        }
    }

    #[test]
    fn cmdline_terminal_failure_statuses_remain_failed() {
        for status in [
            ExecStatus::SandboxLauncherFailed { exit_code: 1 },
            ExecStatus::Killed,
            ExecStatus::TimedOut,
        ] {
            assert_eq!(tool_failed_for_status(&status), Some(true));
        }
    }

    #[tokio::test]
    async fn configured_cmdline_timeout_returns_partial_output_and_metadata() {
        let message = run_tool(
            ToolCmdline {
                name: "cmdline_slow".to_string(),
                cfg: CmdlineToolConfig {
                    command: slow_command(),
                    description: "Slow command".to_string(),
                    parameters: Vec::new(),
                    timeout: "1".to_string(),
                    ..CmdlineToolConfig::default()
                },
                ..ToolCmdline::default()
            },
            args(vec![]),
        )
        .await;
        let body = text(&message);
        let exec = exec(&message);

        assert_partial_start_output(&body);
        assert!(body.contains("timed out"));
        assert_eq!(exec["status"], "timed_out");
        assert!(exec["exit_code"].is_null());
        assert_eq!(message.tool_failed, Some(true));
    }

    #[tokio::test]
    async fn configured_cmdline_abort_returns_partial_output_and_metadata() {
        let (gcx, ccx) = test_ccx().await;
        let abort_flag = Arc::new(AtomicBool::new(false));
        {
            let mut ccx_lock = ccx.lock().await;
            ccx_lock.abort_flag = abort_flag.clone();
        }
        let mut tool = ToolCmdline {
            name: "cmdline_abort".to_string(),
            cfg: CmdlineToolConfig {
                command: slow_command(),
                description: "Abort command".to_string(),
                parameters: Vec::new(),
                timeout: "10".to_string(),
                ..CmdlineToolConfig::default()
            },
            ..ToolCmdline::default()
        };
        let run = tokio::spawn({
            let ccx = ccx.clone();
            async move {
                tool.tool_execute(ccx, &"tool_call".to_string(), &args(vec![]))
                    .await
                    .unwrap()
            }
        });
        tokio::time::sleep(if cfg!(windows) {
            Duration::from_secs(2)
        } else {
            Duration::from_millis(200)
        })
        .await;
        abort_flag.store(true, std::sync::atomic::Ordering::Relaxed);
        let (_, messages) = run.await.unwrap();
        let message = only_message(messages);
        let body = text(&message);
        let exec = exec(&message);

        assert_partial_start_output(&body);
        assert!(body.contains("interrupted by user"));
        assert_eq!(exec["status"], "killed");
        assert_eq!(message.tool_failed, Some(true));
        let process_id = exec["process_id"].as_str().unwrap();
        let snapshot = gcx
            .exec_registry
            .get(&crate::exec::ExecProcessId(process_id.to_string()))
            .await
            .unwrap();
        assert_eq!(snapshot.status, ExecStatus::Killed);
    }

    #[tokio::test]
    async fn configured_cmdline_output_filter_finds_late_match() {
        let marker = "CMDLINE_FOREGROUND_LATE_MATCH";
        let message = run_tool(
            ToolCmdline {
                name: "cmdline_filter".to_string(),
                cfg: CmdlineToolConfig {
                    command: late_marker_command(marker),
                    description: "Find marker".to_string(),
                    parameters: Vec::new(),
                    timeout: "20".to_string(),
                    output_filter: OutputFilter {
                        grep: marker.to_string(),
                        limit_lines: 8,
                        limit_chars: 2000,
                        ..OutputFilter::default()
                    },
                    ..CmdlineToolConfig::default()
                },
                ..ToolCmdline::default()
            },
            args(vec![]),
        )
        .await;
        let body = text(&message);

        assert!(body.contains(marker));
        assert!(body.contains("filtered"));
        assert_eq!(exec(&message)["status"], "exited");
    }

    #[tokio::test]
    async fn configured_cmdline_confirmation_matches_configured_command_text() {
        let (_, ccx) = test_ccx().await;
        let tool = ToolCmdline {
            common: IntegrationCommon {
                confirmation: IntegrationConfirmation {
                    ask_user: vec!["echo*".to_string()],
                    deny: vec!["sudo*".to_string()],
                },
                ..IntegrationCommon::default()
            },
            cfg: CmdlineToolConfig {
                command: "echo %target%".to_string(),
                parameters: vec![CmdlineParam {
                    name: "target".to_string(),
                    param_type: "string".to_string(),
                    description: String::new(),
                }],
                parameters_required: Some(vec!["target".to_string()]),
                ..CmdlineToolConfig::default()
            },
            ..ToolCmdline::default()
        };

        let matched = tool
            .match_against_confirm_deny(ccx, &args(vec![("target", json!("hello world"))]))
            .await
            .unwrap();

        assert_eq!(matched.result, MatchConfirmDenyResult::CONFIRMATION);
        #[cfg(target_os = "windows")]
        assert_eq!(matched.command, "echo \"hello world\"");
        #[cfg(not(target_os = "windows"))]
        assert_eq!(matched.command, "echo 'hello world'");
        assert_eq!(matched.rule, "echo*");
    }

    #[test]
    fn configured_cmdline_schema_keeps_required_fields() {
        let schema: serde_yaml::Value = serde_yaml::from_str(CMDLINE_INTEGRATION_SCHEMA).unwrap();
        let fields = schema.get("fields").unwrap();

        for field in [
            "command",
            "command_workdir",
            "description",
            "parameters",
            "timeout",
            "output_filter",
        ] {
            assert!(fields.get(field).is_some(), "missing field {field}");
        }
        assert_eq!(
            schema["confirmation"]["ask_user_default"][0].as_str(),
            Some("*")
        );
        assert_eq!(
            schema["confirmation"]["deny_default"][0].as_str(),
            Some("sudo*")
        );
    }

    #[tokio::test]
    async fn configured_cmdline_required_param_validation_is_unchanged() {
        let tool = ToolCmdline {
            cfg: CmdlineToolConfig {
                command: "echo %target%".to_string(),
                parameters: vec![CmdlineParam {
                    name: "target".to_string(),
                    param_type: "string".to_string(),
                    description: String::new(),
                }],
                parameters_required: Some(vec!["target".to_string()]),
                ..CmdlineToolConfig::default()
            },
            ..ToolCmdline::default()
        };

        let err = tool
            .command_to_match_against_confirm_deny(test_ccx().await.1, &args(vec![]))
            .await
            .unwrap_err();

        assert_eq!(err, "Missing required argument `target`");
    }
}
