use std::sync::Arc;

use crate::global_context::{GlobalContext, GlobalContextCtx};

pub use refact_exec::command_policy::{
    escalation_from_args, ChatMode, CommandKind, CommandPolicyInput, ExecEscalation,
    ExecEscalationMode, ExecSource,
};
pub use refact_runtime_api::command_policy::{
    build_exec_request_for_ctx, chat_mode_for_exec_for_sessions, queue_sandbox_audit_for_sessions,
    ExecPolicyError, ExecRequestPolicy, SandboxAuditRecord,
};

pub async fn build_exec_request(
    gcx: Arc<GlobalContext>,
    input: CommandPolicyInput<'_>,
) -> Result<ExecRequestPolicy, ExecPolicyError> {
    build_exec_request_for_ctx(&gcx.chat_ctx(gcx.clone()), input).await
}

pub async fn queue_sandbox_audit(
    gcx: Arc<GlobalContext>,
    chat_id: &str,
    audit: SandboxAuditRecord,
) {
    queue_sandbox_audit_for_sessions(&gcx.chat_sessions, chat_id, audit).await;
}

pub async fn chat_mode_for_exec(gcx: Arc<GlobalContext>, chat_id: &str) -> Option<ChatMode> {
    chat_mode_for_exec_for_sessions(&gcx.chat_sessions, chat_id).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::ExecEnvPolicy;
    use std::collections::HashMap;
    use std::path::PathBuf;

    #[tokio::test]
    async fn policy_sets_scrubbed_environment_and_each_audit_source() {
        let mut gcx = crate::global_context::tests::make_test_gcx().await;
        Arc::get_mut(&mut gcx)
            .unwrap()
            .terminal_security_config
            .env_passthrough = vec!["HTTP_PROXY".to_string()];
        let sources = [
            (ExecSource::ShellTool, "shell_tool"),
            (ExecSource::ProcessTool, "process_tool"),
            (ExecSource::CmdlineIntegration, "cmdline_integration"),
            (ExecSource::SchedulerJob, "scheduler_job"),
            (ExecSource::Verifier, "verifier"),
        ];

        for (source, expected) in sources {
            let argv = vec!["printf".to_string(), "ok".to_string()];
            let request = build_exec_request(
                gcx.clone(),
                CommandPolicyInput {
                    source,
                    command: CommandKind::Argv(&argv),
                    cwd: None,
                    env: HashMap::new(),
                    chat_mode: None,
                    escalation: None,
                },
            )
            .await
            .unwrap();

            assert_eq!(
                request.request.env_policy,
                ExecEnvPolicy::Scrubbed {
                    passthrough: vec!["HTTP_PROXY".to_string()]
                }
            );
            assert_eq!(request.request.audit.unwrap().source, expected);
            assert!(request.request.sandbox.is_none());
        }
    }

    #[tokio::test]
    async fn policy_preserves_argv_and_cwd() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let argv = vec!["printf".to_string(), "space value".to_string()];
        let cwd = PathBuf::from("workspace");

        let request = build_exec_request(
            gcx,
            CommandPolicyInput {
                source: ExecSource::Verifier,
                command: CommandKind::Argv(&argv),
                cwd: Some(cwd.clone()),
                env: HashMap::new(),
                chat_mode: None,
                escalation: None,
            },
        )
        .await
        .unwrap();

        assert_eq!(request.request.argv, Some(argv));
        assert_eq!(request.request.cwd, Some(cwd));
    }

    #[tokio::test]
    async fn no_op_escalation_records_no_audit() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let argv = vec!["printf".to_string(), "ok".to_string()];

        let policy = build_exec_request(
            gcx,
            CommandPolicyInput {
                source: ExecSource::ShellTool,
                command: CommandKind::Argv(&argv),
                cwd: None,
                env: HashMap::new(),
                chat_mode: Some("agent".to_string()),
                escalation: Some(ExecEscalation {
                    mode: ExecEscalationMode::WorkspaceWrite,
                    justification: "commit inside the task worktree".to_string(),
                }),
            },
        )
        .await
        .unwrap();

        assert!(policy.audit.is_none());
        assert_eq!(
            policy.request.audit.unwrap().justification.as_deref(),
            Some("commit inside the task worktree")
        );
    }
}
