pub use refact_runtime_api::scheduler_exec_action::*;

#[cfg(test)]
use std::path::PathBuf;
#[cfg(test)]
use serde_json::json;
#[cfg(test)]
use crate::scheduler::types::CommandSpec;
#[cfg(test)]
use crate::scheduler::types::Job;

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::scheduler::types::{Action, AgentTarget, Delivery, Trigger};

    fn job() -> Job {
        Job {
            id: "command-job".to_string(),
            description: "Run command".to_string(),
            enabled: true,
            durable: false,
            created_at_ms: 1_000,
            recurring: true,
            trigger: Trigger::Interval { every_ms: 60_000 },
            action: Action::Command {
                argv: vec!["printf".to_string(), "hello".to_string()],
                target: AgentTarget::ExistingChat {
                    chat_id: "chat".to_string(),
                },
                cwd: None,
                env: None,
                timeout_secs: None,
            },
            delivery: Delivery::Chat,
            push: Default::default(),
            last_fired_at_ms: None,
            fire_count: 0,
            last_status: None,
            last_error: None,
            last_delivery_error: None,
            recent_runs: Vec::new(),
            paused_at_ms: None,
            trigger_at_ms: None,
            auto_expire_after_ms: crate::scheduler::DEFAULT_RECURRING_AUTO_EXPIRE_AFTER_MS,
            retry_attempts: 0,
        }
    }

    #[test]
    fn validate_argv_accepts_arguments_with_spaces() {
        let result = validate_argv(&[
            "printf".to_string(),
            "%s".to_string(),
            "hello world".to_string(),
        ]);

        assert_eq!(result, Ok(()));
    }

    #[test]
    fn resolve_cwd_rejects_parent_escape() {
        let root = PathBuf::from("/tmp/project");

        let error = resolve_cwd(Some(&root), Some("../outside")).unwrap_err();

        assert_eq!(error, "cwd must stay within the active project");
    }

    #[test]
    fn command_output_message_is_plain_text() {
        let result = CommandRunResult {
            status: "fired".to_string(),
            stdout: "hello".to_string(),
            stderr: String::new(),
            exit_code: Some(0),
        };
        let message = command_output_message(&job(), &result);

        assert_eq!(message.role, "plain_text");
        assert_eq!(message.content.content_text_only(), "hello");
        assert_eq!(
            message.extra["scheduler_command"]["task_id"],
            json!("command-job")
        );
    }

    #[test]
    fn command_env_uses_overrides_only() {
        let mut env = BTreeMap::new();
        env.insert("FROG".to_string(), "green".to_string());
        let cmd = CommandSpec {
            argv: vec!["env".to_string()],
            target: AgentTarget::ExistingChat {
                chat_id: "chat".to_string(),
            },
            cwd: None,
            env: Some(env),
            timeout_secs: None,
        };

        assert_eq!(command_env(&cmd).get("FROG"), Some(&"green".to_string()));
    }
}
