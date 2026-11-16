"""AL-6 mover: settle `chat::prompts` + `chat::verifier` into `refact-tools`.

    python tools/dev/al6-move.py <worktree-root>
    python tools/dev/al6-move.py <worktree-root> --dry

Route: the two files move into the crate that is the only cycle-free host
(AL-2's crate-edge audit): `refact_tools::chat_prompts` and
`refact_tools::chat_verifier`, both at the crate root next to `chat_tools`.

What stays in the engine, and why:

  * the `#[cfg(test)] mod tests` blocks of both files (they build on
    `make_test_gcx`) - kept verbatim behind the house glob shim;
  * `chat::verify_cmd` - it IS the engine implementation of
    `CardVerifier::verification_command_policy` (it reads the engine-owned
    shell-policy file); the crate reaches it only through that seam;
  * `EngineCardVerifier` - the seam struct the engine wires into `ChatCtx`; it
    delegates the four engine-agnostic methods to the crate and keeps
    `verification_command_policy` engine-side.

Every other rewrite is a forward `crate::` -> `refact_*::` path rewrite or a
visibility widening required by the verbatim engine tests. `--dry` prints the
planned rewrites without touching the tree.
"""
import io
import os
import re
import sys

ENGINE_REL = os.path.join("refact-agent", "engine")
CRATE_REL = os.path.join(ENGINE_REL, "crates", "refact-tools", "src")

ENGINE_PROMPTS = os.path.join(ENGINE_REL, "src", "chat", "prompts.rs")
ENGINE_VERIFIER = os.path.join(ENGINE_REL, "src", "chat", "verifier.rs")
CRATE_PROMPTS = os.path.join(CRATE_REL, "chat_prompts.rs")
CRATE_VERIFIER = os.path.join(CRATE_REL, "chat_verifier.rs")
CRATE_LIB = os.path.join(CRATE_REL, "lib.rs")

TESTS_MARKER = "#[cfg(test)]\nmod tests {"

# --------------------------------------------------------------------------- io


def read(path):
    with io.open(path, "r", encoding="utf-8", errors="surrogateescape") as fh:
        return fh.read()


def write(path, text):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with io.open(path, "w", encoding="utf-8", errors="surrogateescape", newline="\n") as fh:
        fh.write(text)


# ------------------------------------------------------------------ text tools


def match_block(text, open_idx):
    """Index just past the `}` matching the `{` at open_idx."""
    depth = 0
    i = open_idx
    n = len(text)
    while i < n:
        c = text[i]
        if c == '"':
            i += 1
            while i < n:
                if text[i] == "\\":
                    i += 2
                    continue
                if text[i] == '"':
                    break
                i += 1
            i += 1
            continue
        if c == "/" and i + 1 < n and text[i + 1] == "/":
            j = text.find("\n", i)
            i = n if j < 0 else j
            continue
        if c == "{":
            depth += 1
        elif c == "}":
            depth -= 1
            if depth == 0:
                return i + 1
        i += 1
    raise SystemExit("al6-move: unbalanced block")


def cut_item(text, marker, open_at):
    """Cut the item starting at `marker` (its `{` sits at `marker`+open_at)."""
    start = text.index(marker)
    open_idx = start + len(marker) - 1 + open_at
    if text[open_idx] != "{":
        open_idx = text.index("{", start + len(marker) - 1)
    end = match_block(text, open_idx)
    head = text[:start]
    item = text[start:end]
    tail = text[end:]
    tail = tail[1:] if tail.startswith("\n") else tail
    return head, item, tail


def apply_pairs(text, pairs, label):
    for old, new in pairs:
        if old not in text:
            raise SystemExit("al6-move: %s: pattern not found: %r" % (label, old[:70]))
        text = text.replace(old, new)
    return text


def widen(text, pattern, replacement, label):
    text, n = re.subn(pattern, replacement, text)
    if not n:
        raise SystemExit("al6-move: %s: could not widen %r" % (label, pattern))
    return text


def uses_of(text):
    return re.findall(r"(?m)^use [^;]+;\n", text)


# ------------------------------------------------------------------- transforms

PROMPTS_USE_PAIRS = [
    ("use crate::app_state::AppState;",
     "use refact_runtime_api::app_state::AppState;"),
    ("use crate::call_validation;",
     "use refact_core::call_validation;"),
    ("use crate::files_correction::{\n"
     "    get_unscoped_project_dirs, normalize_path_for_unscoped_root_selection,\n"
     "    registered_worktree_path_mappings,\n"
     "};",
     "use refact_runtime_api::files_correction::get_unscoped_project_dirs;\n"
     "use refact_worktrees::path_mappings::{\n"
     "    normalize_path_for_unscoped_root_selection, registered_worktree_path_mappings,\n"
     "};"),
    ("use crate::scratchpads::scratchpad_utils::HasRagResults;",
     "use refact_scratchpads::scratchpad_utils::HasRagResults;"),
    ("use super::system_context::{",
     "use refact_runtime_api::system_context::{"),
    ("use crate::ext::skills_context::{",
     "use refact_runtime_api::ext_skills_context::{"),
    ("use crate::yaml_configs::project_information::load_project_information_config;",
     "use refact_runtime_api::project_information::load_project_information_config;"),
    ("use crate::call_validation::{ChatMessage, ChatContent, ContextFile, canonical_mode_id};",
     "use refact_core::call_validation::{ChatMessage, ChatContent, ContextFile, canonical_mode_id};"),
    ("crate::call_validation::ChatContent::SimpleText",
     "refact_core::call_validation::ChatContent::SimpleText"),
    ("call_validation::ChatMeta", "refact_chat_api::ChatMeta"),
    ("use crate::yaml_configs::customization_registry::{get_mode_config, map_legacy_mode_to_id};",
     "use refact_runtime_api::customization_registry_gcx::{get_mode_config, map_legacy_mode_to_id};"),
]
PROMPTS_BODY_PAIRS = [
    ("super::prompt_snippets::", "refact_chat_history::prompt_snippets::"),
    ("super::room_roster::load_room_roster", "refact_runtime_api::room_roster::load_room_roster"),
    ("super::room_roster::upsert_room_roster_message",
     "refact_chat_api::room_roster::upsert_room_roster_message"),
    ("crate::tasks::storage::", "refact_runtime_api::task_storage::"),
    ("crate::llm::safe_truncate(", "refact_llm::logging::safe_truncate("),
    ("crate::llm::params::CacheControl::Ephemeral", "refact_core::chat_types::CacheControl::Ephemeral"),
    ("crate::chat::types::TaskMeta", "refact_chat_api::types::TaskMeta"),
    ("crate::chat::internal_roles::EVENT_ROLE", "refact_chat_api::internal_roles::EVENT_ROLE"),
    ("crate::files_in_workspace::detect_vcs_for_a_file_path",
     "refact_files::vcs_detect::detect_vcs_for_a_file_path"),
    ("crate::global_context::try_load_caps_quickly_if_not_present",
     "refact_runtime_api::caps_loader::try_load_caps_quickly_if_not_present"),
    ("crate::caps::resolve_chat_model(", "refact_caps_core::model_resolve::resolve_chat_model("),
    ("crate::caps::ChatModelRecord", "refact_caps_core::model_records::ChatModelRecord"),
    ("crate::caps::CodeAssistantCaps", "refact_caps_core::code_assistant_caps::CodeAssistantCaps"),
    ("crate::subchat::SubchatConfig", "refact_chat_api::SubchatConfig"),
    ("crate::subchat::ToolsPolicy::None", "refact_chat_api::ToolsPolicy::None"),
    ("crate::subchat::TraceParent::unattributed()", "refact_chat_api::TraceParent::unattributed()"),
    ("crate::integrations::config_chat::mix_config_messages",
     "refact_runtime_api::integrations::config_chat::mix_config_messages"),
    ("crate::integrations::setup_chat::mix_setup_messages",
     "refact_runtime_api::integrations::setup_chat::mix_setup_messages"),
]

# Items the verbatim engine `#[cfg(test)] mod tests` reaches through `use super::*`.
# Their own `#[cfg(test)]` gates become the crate's test-util gate, which the engine
# enables on its dev/test dependency edge (the established `refact-runtime-api::spawn`
# precedent).
PROMPTS_TEST_HELPERS = [
    "clear_buddy_persona_cache_for_tests",
    "task_briefing_test_state",
    "task_briefing_test_response",
    "set_task_briefing_test_responses",
    "clear_task_briefing_test_state",
    "task_briefing_test_calls",
    "clear_task_briefing_cache_for_tests",
]

PROMPTS_WIDEN = [
    (r"(?m)^fn truncate_to_chars\b", "pub fn truncate_to_chars"),
    (r"(?m)^fn models_info_snapshot_from\b", "pub fn models_info_snapshot_from"),
    (r"(?m)^pub\(crate\) async fn models_info_snapshot\b", "pub async fn models_info_snapshot"),
    (r"(?m)^fn task_briefing_prompt\b", "pub fn task_briefing_prompt"),
    (r"(?m)^const BUDDY_PERSONALITY_MARKER\b", "pub const BUDDY_PERSONALITY_MARKER"),
    (r"(?m)^const TASK_MEMORIES_CONTEXT_MARKER\b", "pub const TASK_MEMORIES_CONTEXT_MARKER"),
    (r"(?m)^const TASK_BRIEFING_CONTEXT_MARKER\b", "pub const TASK_BRIEFING_CONTEXT_MARKER"),
    (r"(?m)^const MAX_TASK_MEMORY_CONTENT_SIZE\b", "pub const MAX_TASK_MEMORY_CONTENT_SIZE"),
]

VERIFIER_USE_PAIRS = [
    ("use crate::chat::internal_roles::{event, EventSubkind};",
     "use refact_chat_api::internal_roles::{event, EventSubkind};"),
    ("use crate::chat::ctx::ChatCtx;", "use refact_runtime_api::chat_ctx::ChatCtx;"),
    ("use crate::chat::verifier_diff::{git_changed_files_summary, resolve_verifier_diff_base};",
     "use refact_worktrees::verifier_diff::{git_changed_files_summary, resolve_verifier_diff_base};"),
    ("use crate::chat::verify_cmd::{\n"
     "    load_for_ctx as load_verify_command_policy, parse_verification_argv,\n"
     "    verification_commands, VerifyCommandPolicy,\n"
     "};",
     "pub use refact_chat_api::verify_cmd::{\n"
     "    parse_verification_argv, verification_commands, VerifyCommandPolicy,\n"
     "};"),
    ("use crate::exec::command_policy::{\n"
     "    build_exec_request_for_ctx, CommandKind, CommandPolicyInput, ExecSource,\n"
     "};",
     "use refact_exec::command_policy::{CommandKind, CommandPolicyInput, ExecSource};\n"
     "use refact_runtime_api::command_policy::build_exec_request_for_ctx;"),
    ("use crate::exec::{ExecOutputStream, ExecStatus};",
     "use refact_exec::{ExecOutputStream, ExecStatus};"),
    ("use crate::global_context::GlobalContext;",
     "use refact_runtime_api::global_context::GlobalContext;"),
    ("use crate::worktrees::service::WorktreeService;",
     "use refact_worktrees::service::WorktreeService;"),
    ("use crate::tasks::storage;", "use refact_runtime_api::task_storage as storage;"),
    ("use crate::tasks::types::{\n"
     "    BoardCard, StatusUpdate, VerificationOutcome, VerificationResult, VerifierReport,\n"
     "    VerifierReportClassification,\n"
     "};",
     "use refact_tasks::types::{\n"
     "    BoardCard, StatusUpdate, VerificationOutcome, VerificationResult, VerifierReport,\n"
     "    VerifierReportClassification,\n"
     "};"),
    ("use refact_runtime_api::verifier::CardVerifier;\n", ""),
]

VERIFIER_BODY_PAIRS = [
    ("crate::subchat::SubchatConfig", "refact_chat_api::SubchatConfig"),
    ("crate::subchat::ToolsPolicy::None", "refact_chat_api::ToolsPolicy::None"),
    ("crate::subchat::TraceParent::from_parts", "refact_chat_api::TraceParent::from_parts"),
    ("crate::llm::params::CacheControl::Ephemeral", "refact_core::chat_types::CacheControl::Ephemeral"),
    ("crate::files_correction::get_unscoped_project_dirs_for_ctx",
     "refact_runtime_api::files_correction::get_unscoped_project_dirs_for_ctx"),
    ("load_verify_command_policy(ctx).await", "ctx.card_verifier.verification_command_policy(ctx).await"),
    ("#[cfg(test)]\nasync fn run_verification_command(", "pub async fn run_verification_command("),
]

VERIFIER_WIDEN = [
    (r"(?m)^async fn load_expected_card_state_for_ctx_impl\b",
     "pub async fn load_expected_card_state_for_ctx_impl"),
    (r"(?m)^async fn verify_card_for_ctx_impl\b", "pub async fn verify_card_for_ctx_impl"),
    (r"(?m)^async fn schedule_card_verifier_after_finish_for_ctx_impl\b",
     "pub async fn schedule_card_verifier_after_finish_for_ctx_impl"),
    (r"(?m)^pub\(crate\) async fn run_verification_argv_impl\b",
     "pub async fn run_verification_argv_impl"),
    (r"(?m)^async fn run_verification_command_with_policy\b",
     "pub async fn run_verification_command_with_policy"),
    (r"(?m)^async fn run_verification_command_with_runner\b",
     "pub async fn run_verification_command_with_runner"),
    (r"(?m)^trait VerificationCommandRunner\b", "pub trait VerificationCommandRunner"),
    (r"(?m)^fn classify_verifier_report\b", "pub fn classify_verifier_report"),
    (r"(?m)^fn launch_failure_report\b", "pub fn launch_failure_report"),
    (r"(?m)^fn append_verifier_status\b", "pub fn append_verifier_status"),
    (r"(?m)^const DRAIN_TIMEOUT\b", "pub const DRAIN_TIMEOUT"),
]

VERIFIER_ENGINE_STRUCT = '''pub struct EngineCardVerifier;

#[async_trait]
impl CardVerifier for EngineCardVerifier {
    async fn load_expected_card_state(
        &self,
        ctx: &ChatCtx,
        task_id: &str,
        card_id: &str,
    ) -> Result<ExpectedCardState, String> {
        refact_tools::chat_verifier::load_expected_card_state_for_ctx_impl(ctx, task_id, card_id)
            .await
    }

    async fn verify_card(
        &self,
        ctx: &ChatCtx,
        request: VerifyCardRequest,
    ) -> Result<VerifierReport, String> {
        refact_tools::chat_verifier::verify_card_for_ctx_impl(ctx, request).await
    }

    async fn schedule_card_verifier_after_finish(
        &self,
        ctx: &ChatCtx,
        task_id: String,
        card_id: String,
        expected_state: ExpectedCardState,
    ) {
        refact_tools::chat_verifier::schedule_card_verifier_after_finish_for_ctx_impl(
            ctx,
            task_id,
            card_id,
            expected_state,
        )
        .await;
    }

    async fn run_verification_argv(
        &self,
        ctx: &ChatCtx,
        worktree: &Path,
        command: &str,
        cwd: Option<PathBuf>,
        env: Vec<(String, String)>,
        argv: Vec<String>,
        timeout: Duration,
        drain_timeout: Duration,
    ) -> VerificationResult {
        refact_tools::chat_verifier::run_verification_argv_impl(
            ctx,
            worktree,
            command,
            cwd,
            env,
            argv,
            timeout,
            drain_timeout,
        )
        .await
    }

    async fn verification_command_policy(&self, ctx: &ChatCtx) -> VerifyCommandPolicy {
        crate::chat::verify_cmd::load_for_ctx(ctx).await
    }
}
'''

VERIFIER_SHIM_HEAD = '''// Engine seam implementation of `CardVerifier`. The verification logic lives in
// `refact_tools::chat_verifier`; only the shell-policy loader (an engine-owned
// config file) stays here, so the struct delegates the rest into the crate.

use std::path::{Path, PathBuf};
use std::time::Duration;

use async_trait::async_trait;

use refact_runtime_api::verifier::CardVerifier;
use refact_tasks::types::{VerificationResult, VerifierReport};

use crate::chat::ctx::ChatCtx;

pub use refact_tools::chat_verifier::*;

#[cfg(test)]
#[allow(unused_imports)]
use std::sync::atomic::Ordering;
#[cfg(test)]
#[allow(unused_imports)]
use std::sync::Arc;
#[cfg(test)]
#[allow(unused_imports)]
use chrono::Utc;
#[cfg(test)]
#[allow(unused_imports)]
use refact_runtime_api::GlobalContextCtx;
#[cfg(test)]
#[allow(unused_imports)]
use crate::global_context::GlobalContext;
#[cfg(test)]
#[allow(unused_imports)]
use crate::tasks::storage;
#[cfg(test)]
#[allow(unused_imports)]
use crate::tasks::types::{
    BoardCard, StatusUpdate, VerificationOutcome, VerifierReportClassification,
};
#[cfg(test)]
#[allow(unused_imports)]
use crate::tools::shell_gate::{ApprovalMode, ShellGatePolicy};
#[cfg(test)]
#[allow(unused_imports)]
use crate::chat::verify_cmd::{from_shell_policy, permissive};

'''


# ----------------------------------------------------------------------- mover


def plan_prompts(root):
    text = read(os.path.join(root, ENGINE_PROMPTS))
    head, tests, tail = cut_item(text, TESTS_MARKER, 0)
    prod = head + tail
    uses = uses_of(prod)
    body = apply_pairs(prod, PROMPTS_USE_PAIRS, "prompts use")
    body = apply_pairs(body, PROMPTS_BODY_PAIRS, "prompts body")
    for pattern, repl in PROMPTS_WIDEN:
        body = widen(body, pattern, repl, "prompts widen")
    body = body.replace("#[cfg(test)]\nfn clear_buddy_persona_cache_for_tests",
                        "#[cfg(any(test, feature = \"test-util\"))]\npub fn clear_buddy_persona_cache_for_tests")
    body = body.replace("#[cfg(test)]\n#[derive(Default)]\nstruct TaskBriefingTestState",
                        "#[cfg(any(test, feature = \"test-util\"))]\n#[derive(Default)]\npub struct TaskBriefingTestState")
    body = body.replace("#[cfg(test)]\nstatic TASK_BRIEFING_TEST_STATE",
                        "#[cfg(any(test, feature = \"test-util\"))]\nstatic TASK_BRIEFING_TEST_STATE")
    body = body.replace("#[cfg(test)]\nfn task_briefing_test_state()",
                        "#[cfg(any(test, feature = \"test-util\"))]\nfn task_briefing_test_state()")
    body = body.replace("    #[cfg(test)]\n    {\n        let _ = (app, role, source);",
                        "    #[cfg(any(test, feature = \"test-util\"))]\n    {\n        let _ = (app, role, source);")
    body = body.replace("    #[cfg(not(test))]\n    {",
                        "    #[cfg(not(any(test, feature = \"test-util\")))]\n    {")
    for name in PROMPTS_TEST_HELPERS:
        if name == "task_briefing_test_state":
            continue
        body = body.replace("#[cfg(test)]\nasync fn %s(" % name,
                            "#[cfg(any(test, feature = \"test-util\"))]\npub async fn %s(" % name)
    if "pub async fn set_task_briefing_test_responses" not in body:
        raise SystemExit("al6-move: prompts: test-helper gate rewrite missed")
    if "cfg(test)" in body:
        hits = [l for l in body.split("\n") if "cfg(test)" in l]
        raise SystemExit("al6-move: prompts: cfg(test) left in the crate body: %r" % hits)
    shim = "".join("#[cfg(test)]\n#[allow(unused_imports)]\n" + use for use in uses)
    shim += "\npub use refact_tools::chat_prompts::*;\n\n"
    shim += tests.rstrip("\n") + "\n"
    return body.rstrip("\n") + "\n", shim


def plan_verifier(root):
    text = read(os.path.join(root, ENGINE_VERIFIER))
    head, tests, tail = cut_item(text, TESTS_MARKER, 0)
    prod = head + tail
    start = prod.index("pub struct EngineCardVerifier;")
    impl_at = prod.index("impl CardVerifier for EngineCardVerifier {", start)
    end = match_block(prod, prod.index("{", impl_at))
    tail_after = prod[end:]
    tail_after = tail_after[1:] if tail_after.startswith("\n") else tail_after
    prod = prod[:start].rstrip("\n") + "\n\n" + tail_after
    body = apply_pairs(prod, VERIFIER_USE_PAIRS, "verifier use")
    body = apply_pairs(body, VERIFIER_BODY_PAIRS, "verifier body")
    for pattern, repl in VERIFIER_WIDEN:
        body = widen(body, pattern, repl, "verifier widen")
    if "#[cfg(test)]" in body:
        raise SystemExit("al6-move: verifier: cfg(test) left in the crate body")
    shim = VERIFIER_SHIM_HEAD + VERIFIER_ENGINE_STRUCT + "\n" + tests.rstrip("\n") + "\n"
    return body.rstrip("\n") + "\n", shim


def move(root, dry=False):
    prompts_body, prompts_shim = plan_prompts(root)
    verifier_body, verifier_shim = plan_verifier(root)
    for label, body in (("chat_prompts", prompts_body), ("chat_verifier", verifier_body)):
        for bad in ("crate::chat::", "crate::app_state::", "crate::caps::", "crate::llm::",
                    "crate::subchat::", "crate::yaml_configs::", "crate::ext::skills_context",
                    "crate::integrations::", "crate::scratchpads::", "crate::tasks::",
                    "crate::files_correction::", "crate::files_in_workspace::",
                    "crate::global_context::", "crate::worktrees::", "crate::exec::",
                    "crate::tools::shell_gate", "super::"):
            if bad in body:
                raise SystemExit("al6-move: %s: engine path survived: %s" % (label, bad))
    if dry:
        print("prompts crate body %d lines, shim %d lines" %
              (len(prompts_body.split("\n")), len(prompts_shim.split("\n"))))
        print("verifier crate body %d lines, shim %d lines" %
              (len(verifier_body.split("\n")), len(verifier_shim.split("\n"))))
        return
    write(os.path.join(root, CRATE_PROMPTS), prompts_body)
    write(os.path.join(root, CRATE_VERIFIER), verifier_body)
    write(os.path.join(root, ENGINE_PROMPTS), prompts_shim)
    write(os.path.join(root, ENGINE_VERIFIER), verifier_shim)
    lib = read(os.path.join(root, CRATE_LIB))
    if "pub mod chat_prompts;" not in lib:
        anchor = "pub mod chat_tools;\n"
        if anchor not in lib:
            raise SystemExit("al6-move: no `pub mod chat_tools;` anchor in refact-tools lib.rs")
        lib = lib.replace(anchor, anchor + "pub mod chat_prompts;\npub mod chat_verifier;\n", 1)
        write(os.path.join(root, CRATE_LIB), lib)
    print("moved chat::prompts + chat::verifier into refact-tools")
    print("  crate chat_prompts %5d lines   engine shim %5d lines" %
          (len(prompts_body.split("\n")), len(prompts_shim.split("\n"))))
    print("  crate chat_verifier %4d lines  engine shim %5d lines" %
          (len(verifier_body.split("\n")), len(verifier_shim.split("\n"))))


def main():
    args = [a for a in sys.argv[1:] if not a.startswith("--")]
    if not args:
        raise SystemExit(__doc__)
    move(os.path.abspath(args[0]), dry="--dry" in sys.argv)


if __name__ == "__main__":
    main()
