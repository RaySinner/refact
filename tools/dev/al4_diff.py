#!/usr/bin/env python3
"""AL-4 body-preservation diff.

Regenerates the expected `refact-tools/src/chat_tools.rs` production body and the
expected engine shim from the *base* revision's `src/chat/tools.rs`, applying only
the documented mechanical classes:

  * path rewrites      engine module path -> published crate path
  * test relocation    `#[cfg(test)]` blocks split by make_test_gcx dependency
  * visibility widening `pub(crate)`/private items a glob shim must re-export

Then it compares the regenerated text with what is on disk, normalised for
whitespace. Any other difference is reported as a real difference.

Usage:  python tools/dev/al4_diff.py            (base = the card's start commit)
        python tools/dev/al4_diff.py <rev>
Exit:   0 = the moved bodies are byte-equivalent modulo the documented classes.
"""

import os
import re
import subprocess
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
BASE_REL = "refact-agent/engine/src/chat/tools.rs"
BASE_REV = sys.argv[1] if len(sys.argv) > 1 else "bf24bd08a"

CRATE_REL = "refact-agent/engine/crates/refact-tools/src/chat_tools.rs"
ENGINE_REL = "refact-agent/engine/src/chat/tools.rs"


def read(rel):
    with open(os.path.join(ROOT, rel), encoding="utf-8") as fh:
        return fh.read()


def git_show(rev, rel):
    out = subprocess.run(
        ["git", "show", f"{rev}:{rel}"],
        cwd=ROOT,
        capture_output=True,
        check=True,
    )
    return out.stdout.decode("utf-8")


def block_end(text, start):
    """Index just past the brace-balanced block that starts at `start`."""
    i = text.find("{", start)
    depth = 0
    while True:
        c = text[i]
        if c == "{":
            depth += 1
        elif c == "}":
            depth -= 1
            if depth == 0:
                return i + 1
        i += 1


def split_base(text):
    """(production text, engine-only test text, crate-movable test text)."""
    t1 = text.find("#[cfg(test)]\nmod tests {")
    t1_end = block_end(text, t1)
    trt = text.find("#[cfg(test)]\nasync fn tool_runtime_event_lines(")
    fb = text.find("fn fallback_tool_runtime_event_lines(")
    rw = text.find("#[cfg(test)]\nfn runtime_event_lines_with_fallback(")
    et = text.find("pub async fn execute_tools(")
    iw = text.find("#[cfg(test)]\nmod interruptible_wait_tests {")
    iw_end = block_end(text, iw)
    lg = text.find("#[cfg(test)]\nmod loop_guard_tests {")
    lg_end = block_end(text, lg)
    it = text.find("#[cfg(test)]\nmod investigation_tracker_tests {")

    prod = [
        text[:t1],
        text[t1_end:trt],
        text[fb:rw],
        text[et:iw],
        text[iw_end:lg],
        text[lg_end:it],
    ]
    engine_tests = [text[t1:t1_end], text[trt:fb], text[rw:et]]
    crate_tests = [text[lg:lg_end], text[it:]]
    return "\n".join(prod), engine_tests, crate_tests, text[it:]


REWRITES = [
    (
        "use crate::app_state::AppState;",
        "use refact_runtime_api::app_state::AppState;",
    ),
    (
        "use crate::at_commands::at_commands::AtCommandsContext;",
        "use refact_runtime_api::AtCommandsContext;",
    ),
    (
        "use crate::call_validation::{\n    ChatContent, ChatMessage, ChatToolCall, ContextFile, PostprocessSettings, SubchatParameters,\n};",
        "use refact_core::chat_types::{ChatContent, ChatMessage, ContextFile, PostprocessSettings};\nuse refact_core::call_validation::ChatToolCall;\nuse refact_chat_api::SubchatParameters;",
    ),
    ("use crate::constants::CHAT_TOP_N;", "use refact_core::constants::CHAT_TOP_N;"),
    (
        "use crate::postprocessing::pp_tool_results::{postprocess_tool_results, ToolBudget};",
        "use refact_runtime_api::pp_tool_results::{postprocess_tool_results, ToolBudget};",
    ),
    (
        "use crate::chat::perf_diagnostics::{self, PerfComponent, PerfOutcome, ToolExecutionClass};",
        "use refact_perf::perf_diagnostics::{self, PerfComponent, PerfOutcome, ToolExecutionClass};",
    ),
    (
        "use crate::yaml_configs::customization_registry::{\n"
        "    get_mode_config, map_legacy_mode_to_id, match_tool_confirm_action,\n"
        "};",
        "use refact_runtime_api::customization_registry_gcx::{\n"
        "    get_mode_config, match_tool_confirm_action,\n"
        "};\n"
        "use refact_chat_api::setparams::map_legacy_mode_to_id;",
    ),
    ("use crate::ext::hooks::HookEvent;", "use refact_ext::hooks::HookEvent;"),
    (
        "use crate::ext::hooks_runner::{HookPayload, first_block_reason, get_project_dir_string, run_hooks};",
        "use refact_ext::hooks_payload::HookPayload;\nuse refact_runtime_api::ext_hooks_runner::{first_block_reason, get_project_dir_string, run_hooks};",
    ),
    (
        "use crate::exec::command_policy::escalation_from_args;",
        "use refact_exec::command_policy::escalation_from_args;",
    ),
    ("use super::types::*;", "use refact_chat_api::types::*;"),
    (
        "super::types::ChatSession",
        "refact_chat_api::types::ChatSession",
    ),
    (
        "use super::trajectories::{",
        "use refact_runtime_api::trajectories::store::{",
    ),
    (
        "use super::goal_verifier::{\n    should_verify_goal_on_done, verify_goal_before_completion, GoalCompletionGateOutcome,\n};",
        "use refact_chat_api::goal_verifier::{should_verify_goal_on_done, GoalCompletionGateOutcome};\nuse refact_runtime_api::goal_verifier_monitor::verify_goal_before_completion;",
    ),
    ("use super::config::tokens;", "use refact_core::chat_config::tokens;"),
    (
        "crate::privacy::records::shell_ask_pending",
        "refact_privacy::model_context::shell_ask_pending",
    ),
    (
        "crate::global_context::try_load_caps_quickly_if_not_present",
        "refact_runtime_api::caps_loader::try_load_caps_quickly_if_not_present",
    ),
    (
        "crate::caps::resolve_chat_model",
        "refact_caps_core::model_resolve::resolve_chat_model",
    ),
    (
        "crate::at_commands::at_commands::at_commands_new_with_abort(",
        "refact_runtime_api::at_commands_runtime::at_commands_new_with_abort(",
    ),
    (
        "crate::at_commands::at_commands::at_commands_new_from_app(",
        "refact_runtime_api::at_commands_runtime::at_commands_new_from_app(",
    ),
    (
        "crate::llm::adapters::claude_code_compat::",
        "refact_llm::adapters::claude_code_compat::",
    ),
    (
        "crate::runtime_settings::current()",
        "refact_runtime_api::runtime_settings::current()",
    ),
    (
        "crate::tools::tool_call_loop_guard::",
        "refact_tool_api::tool_call_loop_guard::",
    ),
    ("crate::call_validation::ChatToolCall", "refact_core::call_validation::ChatToolCall"),
    (
        "crate::tools::tools_description::WaitInterrupt::from_context",
        "refact_runtime_api::WaitInterrupt::from_context",
    ),
]

WIDENED = [
    "acquire_session_turn_tool_pool",
    "build_tool_execution_context",
    "compute_final_action",
    "execute_single_tool",
    "fallback_tool_runtime_event_lines",
    "is_editing_tool",
    "is_server_executed_tool",
    "is_shell_privacy_approval",
    "is_tool_result_truncation_exempt",
    "rewrite_cc_native_stub_tool_call",
    "should_auto_approve_confirmation",
    "shell_privacy_approval_reasons",
    "SHELL_PRIVACY_APPROVAL_RULE",
    "tool_execution_messages",
    "tool_timestamp_ms",
    "InterruptibleWaitGuard",
]


def apply_rewrites(text, strict=True):
    for old, new in REWRITES:
        if old not in text:
            if strict:
                raise AssertionError(f"rewrite pattern missing from the base: {old!r}")
            continue
        text = text.replace(old, new)
    return text


TEST_PRELUDE = (
    "    use super::*;\n"
    '    use crate::tools::tool_call_loop_guard::{\n'
    "        identity_key_for, LoopVerdict, ToolCallLoopGuard, CAT_TOOL_NAME,\n"
    "    };"
)
TEST_PRELUDE_NEW = (
    "    use super::*;\n"
    "    use uuid::Uuid;\n"
    "    use refact_core::chat_types::{ChatContent, ChatMessage, ContextFile};\n"
    "    use refact_tool_api::tool_call_loop_guard::{\n"
    "        identity_key_for, LoopVerdict, ToolCallLoopGuard, CAT_TOOL_NAME,\n"
    "    };"
)


def apply_test_rewrites(text):
    if TEST_PRELUDE not in text:
        raise AssertionError("crate-test prelude pattern missing from the base")
    text = text.replace(TEST_PRELUDE, TEST_PRELUDE_NEW)
    return text.replace(
        "use crate::tools::tool_call_loop_guard::{",
        "use refact_tool_api::tool_call_loop_guard::{",
    )


def normalise(text):
    text = re.sub(r"\bpub\(crate\)\s+", "", text)
    text = re.sub(r"(?m)^pub\s+", "", text)
    text = re.sub(r"(?m)^(    )pub\s+", r"\1", text)
    return re.sub(r"[ \t]+", " ", "\n".join(l for l in text.splitlines() if l.strip())).strip()


def main():
    base = git_show(BASE_REV, BASE_REL)
    prod, _engine_tests, crate_tests, _tail = split_base(base)

    expected = apply_rewrites(prod)
    crate_test_text = "\n\n".join(t.strip() for t in crate_tests)
    expected = (
        expected.rstrip()
        + "\n\n"
        + apply_test_rewrites(crate_test_text).lstrip()
    )

    actual = read(CRATE_REL)

    # The header doc comment is an addition, not moved code.
    actual_lines = actual.splitlines()
    while actual_lines and actual_lines[0].startswith("//!"):
        actual_lines.pop(0)
    actual_body = "\n".join(actual_lines).lstrip("\n")

    problems = []
    exp_n = normalise(expected)
    act_n = normalise(actual_body)
    if exp_n != act_n:
        problems.append("crate body differs from the regenerated base body")

    check_monitor(problems)

    if re.search(r"\b(?:crate|super)::", re.sub(r"(?m)//.*$", "", prod_part_of(read(ENGINE_REL)))):
        problems.append("engine shim still contains an engine-relative path")

    for name in WIDENED:
        if not re.search(
            r"(?m)^pub\s+(?:async\s+)?(?:fn|const|static|struct)\s+" + re.escape(name) + r"\b",
            read(CRATE_REL),
        ) and f"pub async fn {name}(" not in read(CRATE_REL):
            problems.append(f"{name} was not widened to pub")

    print(f"base {BASE_REV}: {len(base)} chars")
    print(f"expected crate body: {len(expected)} chars, on disk: {len(actual_body)} chars")
    if problems:
        for p in problems:
            print("FAIL " + p)
        return 1
    print("al4_diff: 0 real differences")
    return 0


def prod_part_of(text):
    out = []
    for line in text.splitlines():
        if re.match(r"^\s*#\[cfg\(test\)\]\s*$", line):
            break
        out.append(line)
    return "\n".join(out)


MONITOR_REL = "refact-agent/engine/src/chat/goal_verifier/monitor.rs"
CRATE_MONITOR_REL = "refact-agent/engine/crates/refact-runtime-api/src/goal_verifier_monitor.rs"


def slice_fn(text, name):
    i = text.find(name)
    j = text.find("{", i)
    depth = 0
    while True:
        c = text[j]
        if c == "{":
            depth += 1
        elif c == "}":
            depth -= 1
            if depth == 0:
                return text[i : j + 1]
        j += 1


def check_monitor(problems):
    """verify_goal_before_completion moved, run_goal_verifier stayed, both verbatim."""
    base_monitor = git_show(BASE_REV, MONITOR_REL)
    crate_gate = slice_fn(read(CRATE_MONITOR_REL), "pub async fn verify_goal_before_completion(")
    engine_runner = slice_fn(read(MONITOR_REL), "pub async fn run_goal_verifier(")

    expected_gate = slice_fn(base_monitor, "pub async fn verify_goal_before_completion(")
    # `ABORT_ERROR_MESSAGE` is a pub(crate) engine constant; the crate copy spells
    # the same string, so the substitution is the whole mechanical delta.
    if expected_gate.replace("ABORT_ERROR_MESSAGE", '"Aborted"').strip() != crate_gate.strip():
        problems.append("verify_goal_before_completion body changed")

    expected_runner = slice_fn(base_monitor, "pub async fn run_goal_verifier(")
    if expected_runner.strip() != engine_runner.strip():
        problems.append("run_goal_verifier body changed")


if __name__ == "__main__":
    sys.exit(main())
