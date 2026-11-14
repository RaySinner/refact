//! Engine-side shell-policy loading for verification commands. Command parsing and the
//! deny-rule check live in `refact_chat_api::verify_cmd` so the post-merge workflow can use
//! them from a crate; only the engine knows how to read its shell policy file.

use crate::chat::ctx::ChatCtx;
use crate::tools::shell_gate::ShellGatePolicy;

pub(crate) use refact_chat_api::verify_cmd::{
    parse_verification_argv, verification_commands, VerifyCommandPolicy,
};

pub(crate) async fn load_for_ctx(ctx: &ChatCtx) -> VerifyCommandPolicy {
    from_shell_policy(&crate::tools::shell_gate::load_policy_for_ctx(ctx).await)
}

pub(crate) fn from_shell_policy(policy: &ShellGatePolicy) -> VerifyCommandPolicy {
    VerifyCommandPolicy::from_deny_rules(policy.deny.clone(), format!("{:?}", policy.mode))
}

#[cfg(test)]
pub(crate) fn permissive() -> VerifyCommandPolicy {
    VerifyCommandPolicy::from_deny_rules(Vec::new(), "Yolo".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::shell_gate::ApprovalMode;

    #[test]
    fn default_deny_rules_block_sudo_but_allow_normal_commands() {
        let policy = from_shell_policy(&ShellGatePolicy::default());
        let denied = policy.check("sudo rm -rf /").unwrap_err();
        assert!(denied.contains("denied by shell policy rule"), "{denied}");
        assert!(policy.check("dotnet test").is_ok());
        assert!(policy.check("go test ./...").is_ok());
        assert!(policy.check("rm -rf /").is_ok());
    }

    #[test]
    fn yolo_mode_allows_arbitrary_binaries_but_still_denies() {
        let policy = from_shell_policy(&ShellGatePolicy {
            mode: ApprovalMode::Yolo,
            ..ShellGatePolicy::default()
        });
        for command in ["mycompany-build --release", "gradlew build", "zig build"] {
            assert!(parse_verification_argv(command).is_ok(), "{command}");
            assert!(policy.check(command).is_ok(), "{command}");
        }
        assert!(policy.check("sudo id").is_err());
        assert!(permissive().check("sudo id").is_ok());
    }
}
