"""N-2 verification harness: static proof that the K7a+K7b move into
`refact-tools` is behaviour-preserving.

Mirrors tools/dev/l7-check.py: filesystem/text assertions only, no shell
separators, no cargo (agent cards must not compile).

Run: python tools/dev/n2-check.py
"""
import pathlib
import re
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parents[2]
ENG = ROOT / "refact-agent" / "engine"
SRC = ENG / "src" / "tools"
CRATE = ENG / "crates" / "refact-tools"
DST = CRATE / "src" / "tools"

BASE = "bca526164"

MOVED = [
    "tool_agent_chat_summary",
    "tool_agent_diff",
    "tool_agent_interact",
    "tool_agent_lifecycle",
    "tool_agent_registry",
    "tool_agent_steer",
    "tool_ask_questions",
    "tool_agent_planner_qna",
    "tool_agent_pulse",
]

# tests kept verbatim in the engine shim: module -> expected #[test] count
WITH_TESTS = {
    "tool_agent_chat_summary": 4,
    "tool_agent_diff": 25,
    "tool_agent_interact": 22,
    "tool_agent_lifecycle": 12,
    "tool_agent_registry": 6,
    "tool_agent_steer": 10,
    "tool_agent_planner_qna": 15,
    "tool_agent_pulse": 8,
}

# `#[cfg(test)]` + `use` pairs lifted out of the production region
MOVED_CFG_TEST_USES = {
    "tool_agent_interact": ["use crate::agents::registry::InboxMessage;"],
    "tool_agent_lifecycle": ["use std::process::Command;"],
}

# engine path -> crate path, the only textual edits the mover is allowed to make
REWRITES = [
    ("crate::llm::params::CacheControl", "refact_core::chat_types::CacheControl"),
    ("crate::app_state::AppState", "refact_runtime_api::AppState"),
    (
        "crate::global_context::GlobalContext",
        "refact_runtime_api::global_context::GlobalContext",
    ),
    ("crate::agents::types::", "refact_agents_core::types::"),
    ("crate::agents::registry::", "refact_agents_core::registry::"),
    ("crate::agents::spawn::", "refact_agents_core::spawn::"),
    ("crate::agents::push::", "refact_agents_core::push::"),
    ("crate::agents::delivery::", "refact_agents_core::delivery::"),
    ("crate::chat::deliver_to_chat", "refact_chat_api::delivery::deliver_to_chat"),
    (
        "crate::chat::task_agent_monitor::notify_planner_blocking_question",
        "refact_chat_api::agent_notifications::notify_planner_blocking_question",
    ),
]

WIDEN = {
    "tool_agent_chat_summary": ["fn render_summary_response"],
    "tool_agent_diff": [
        "const GIT_DIFF_TIMEOUT",
        "enum AgentDiffMode",
        "struct DiffBase",
        "fn resolve_base",
        "fn run_with_timeout",
        "fn git_output_cap_notice",
        "fn run_git",
        "fn render_agent_diff",
    ],
    "tool_agent_interact": ["fn short_id"],
    "tool_agent_lifecycle": ["struct PushReport", "fn push_if_card_current_and_live"],
    "tool_agent_registry": ["fn render_registry"],
    "tool_agent_steer": ["enum AgentSteerPriority", "fn format_message"],
    "tool_agent_planner_qna": [
        "struct UnansweredQuestion",
        "fn qna_truncation_notice",
        "fn make_question_id",
        "fn collect_unanswered_questions",
        "fn format_unanswered_questions",
    ],
    "tool_agent_pulse": [],
    "tool_ask_questions": [],
}

# engine-only paths that must be gone from every moved production body
ENGINE_ONLY = [
    "crate::llm",
    "crate::app_state",
    "crate::global_context",
    "crate::agents::",
    "crate::chat::",
    "crate::subchat",
    "refact-lsp",
]

# structs whose fields the engine-side tests read
PUB_FIELD_STRUCTS = {
    "tool_agent_diff": ["DiffBase"],
    "tool_agent_lifecycle": ["PushReport"],
}

checks = []


def check(name, ok, detail=""):
    checks.append((name, ok, detail))
    print(("PASS " if ok else "FAIL ") + name + ((" -- " + detail) if detail and not ok else ""))


def read(path):
    return path.read_text(encoding="utf-8")


def base_text(mod):
    return subprocess.run(
        ["git", "-C", str(ROOT), "show", "%s:refact-agent/engine/src/tools/%s.rs" % (BASE, mod)],
        capture_output=True, check=True,
    ).stdout.decode("utf-8").replace("\r\n", "\n")


def split_test(lines):
    for i in range(len(lines) - 1):
        if lines[i].strip() == "#[cfg(test)]" and lines[i + 1].strip().startswith("mod tests"):
            return lines[:i], lines[i:]
    return lines, []


CTX_CALL = re.compile(r"\.(tool_ctx|chat_ctx|app_state)\(([^)]*)\)")


def string_literals(text):
    out = []
    for m in re.finditer(r'r#"(.*?)"#', text, re.S):
        out.append(m.group(1))
    scrubbed = re.sub(r'r#".*?"#', "", text, flags=re.S)
    for m in re.finditer(r'"((?:[^"\\\n]|\\.)*)"', scrubbed):
        out.append(m.group(1))
    return [s for s in out if len(s) >= 12]


def strip_trailing(text):
    return "\n".join(line.rstrip() for line in text.strip("\n").split("\n"))


def git(*args):
    return subprocess.run(["git", "-C", str(ROOT), *args], capture_output=True, text=True).stdout.strip()


# 1. crate files exist, carry real production code, and never mention the engine
for mod in MOVED:
    body = read(DST / f"{mod}.rs")
    check(f"crate {mod}.rs exists and is non-trivial", len(body) > 200, f"{len(body)} bytes")
    stray = sorted({m for m in re.findall(r"crate::[\w:]*", body) if not m.startswith("crate::tools::")})
    check(f"crate {mod}.rs has no engine-only crate:: refs", not stray, str(stray))
    leftovers = [p for p in ENGINE_ONLY if p in body]
    check(f"crate {mod}.rs names no engine module path", not leftovers, str(leftovers))

# 2. string literals are byte-identical between the pre-move production body and the crate body
for mod in MOVED:
    prod, _ = split_test(base_text(mod).split("\n"))
    body = read(DST / f"{mod}.rs")
    missing = [s for s in string_literals("\n".join(prod)) if s not in body]
    check(f"crate {mod}.rs keeps every production string literal", not missing,
          str(missing[:2])[:200])

# 3. the crate body is the production body verbatim: same lines, same order, and
#    the only permitted difference is the `pub ` prefix the widen step inserts
def expected_prod_lines(mod):
    prod = split_test(base_text(mod).split("\n"))[0]
    skip = set()
    for i in range(len(prod) - 1):
        if prod[i].strip() == "#[cfg(test)]" and prod[i + 1].strip() in MOVED_CFG_TEST_USES.get(mod, []):
            skip.add(i)
            skip.add(i + 1)
    kept = [line for i, line in enumerate(prod) if i not in skip]
    for i, line in enumerate(kept):
        for old, new in REWRITES:
            line = line.replace(old, new)
        indent = line[: len(line) - len(line.lstrip())]
        stripped = line.strip()
        for spec in WIDEN.get(mod, []):
            kind, name = spec.split(" ", 1)
            if re.match(r"^(?:async )?%s %s\b" % (kind, re.escape(name)), stripped):
                line = indent + "pub " + stripped
                break
        kept[i] = line
    if not any(args.strip() for _, args in CTX_CALL.findall("\n".join(kept))):
        kept = [line for line in kept if line.strip() != "use refact_runtime_api::GlobalContextCtx;"]
    for name in PUB_FIELD_STRUCTS.get(mod, []):
        start = next(
            i for i, line in enumerate(kept)
            if re.match(r"^(pub )?struct %s\b" % name, line.strip())
        )
        for j in range(start + 1, len(kept)):
            stripped = kept[j].strip()
            if stripped.startswith("}"):
                break
            if stripped and not stripped.startswith("pub ") and not stripped.startswith("//"):
                indent = kept[j][: len(kept[j]) - len(kept[j].lstrip())]
                kept[j] = indent + "pub " + stripped
    while kept and not kept[-1].strip():
        kept.pop()
    return kept


for mod in MOVED:
    want = expected_prod_lines(mod)
    got = read(DST / f"{mod}.rs").rstrip("\n").split("\n")
    check(f"crate {mod}.rs line count matches the production body",
          len(got) == len(want), f"expected {len(want)}, got {len(got)}")
    unexpected = [
        "%d: %r vs %r" % (i + 1, a[:60], b[:60])
        for i, (a, b) in enumerate(zip(want, got)) if a != b
    ]
    check(f"crate {mod}.rs is the production body with only the documented edits",
          not unexpected, str(unexpected[:3])[:220])

# 4. every private declaration the staying tests reach was widened to pub
for mod, symbols in WIDEN.items():
    body = read(DST / f"{mod}.rs")
    for symbol in symbols:
        kind, name = symbol.split(" ", 1)
        ok = re.search(r"(?m)^\s*pub (?:async )?%s %s\b" % (kind, re.escape(name)), body) is not None
        check(f"crate {mod}.rs widens {symbol}", ok)

# 4b. `GlobalContextCtx` is kept only where a builder call passes the context,
#     which is the only receiver shape the trait is implemented for
for mod in MOVED:
    body = read(DST / f"{mod}.rs")
    wants_trait = any(args.strip() for _, args in CTX_CALL.findall(body))
    has_trait = "use refact_runtime_api::GlobalContextCtx;" in body
    check(f"crate {mod}.rs keeps GlobalContextCtx exactly when a builder call needs it",
          wants_trait == has_trait, f"needs={wants_trait} has={has_trait}")

# 5. engine files are shims; the tested ones keep their tests verbatim
for mod in MOVED:
    body = read(SRC / f"{mod}.rs")
    shim = f"pub use refact_tools::tools::{mod}::*;"
    check(f"engine {mod}.rs starts with the re-export shim", body.startswith(shim),
          body.splitlines()[0] if body else "")

for mod, want in WITH_TESTS.items():
    body = read(SRC / f"{mod}.rs")
    got = len(re.findall(r"#\[(?:tokio::)?test[\(\]]", body))
    check(f"engine {mod}.rs keeps {want} tests", got == want, f"got {got}")
    check(f"engine {mod}.rs keeps its mod tests block", "#[cfg(test)]\nmod tests {" in body)

check("engine tool_ask_questions.rs is shim only",
      read(SRC / "tool_ask_questions.rs").strip() == "pub use refact_tools::tools::tool_ask_questions::*;",
      read(SRC / "tool_ask_questions.rs").strip()[:80])

# 6. the kept test module is byte-identical to the pre-move one
for mod in MOVED:
    _, test = split_test(base_text(mod).split("\n"))
    if not test:
        continue
    shim = read(SRC / f"{mod}.rs")
    marker = "#[cfg(test)]\nmod tests {"
    idx = shim.index(marker)
    kept = shim[idx:]
    check(f"engine {mod}.rs keeps the test module byte-for-byte",
          strip_trailing(kept) == strip_trailing("\n".join(test)))

# 7. the lifted cfg(test) imports are back in the shim prelude
for mod, uses in MOVED_CFG_TEST_USES.items():
    shim = read(SRC / f"{mod}.rs")
    for statement in uses:
        check(f"engine {mod}.rs re-imports lifted `{statement}`",
              ("#[cfg(test)]\n" + statement) in shim)

# 8. the shim prelude still supplies the test-only imports the tests rely on
for mod in MOVED:
    prod, test = split_test(base_text(mod).split("\n"))
    if not test:
        continue
    shim = read(SRC / f"{mod}.rs")
    prelude = shim[: shim.index("#[cfg(test)]\nmod tests {")]
    test_text = "\n".join(test)
    needed = re.findall(r"(?m)^\s*use ([^;]+);", test_text)
    already = {n.split("::")[-1].strip().strip("{}") for n in needed}
    missing = []
    for statement in re.findall(r"(?m)^use [^;]*;", "\n".join(prod)):
        body = " ".join(statement[4:-1].split())
        names = re.findall(r"[\w]+", body)
        bound = names[-1] if not body.endswith("}") else None
        if bound and bound in already:
            continue
        if bound and re.search(r"(?<![\w])%s\b" % re.escape(bound), test_text):
            if statement not in prelude:
                missing.append(statement)
    check(f"engine {mod}.rs prelude covers the test-only imports", not missing, str(missing[:3])[:200])

# 9. crate root module table
tools_rs = read(CRATE / "src" / "tools.rs")
for mod in MOVED:
    check(f"crate tools.rs declares pub mod {mod}", f"pub mod {mod};" in tools_rs)

# 10. crate manifest stays engine-free
check("crate manifest never mentions refact-lsp", "refact-lsp" not in read(CRATE / "Cargo.toml"))

# 11. engine call sites still name the moved modules, and each name is now a shim
named = set()
for path in SRC.glob("*.rs"):
    for m in re.findall(r"crate::tools::(\w+)::", read(path)):
        named.add(m)
surface = sorted(n for n in named if n in MOVED)
check("engine call sites still name all nine moved modules", len(surface) == len(MOVED), str(surface))

# 12. untouched neighbours stay untouched and still construct the same tools
dirty = git("status", "--porcelain").splitlines()
touched = {line.strip().split(None, 1)[1].replace("\\", "/") for line in dirty if line.strip().split(None, 1)[1:]}
allowed_prefixes = (
    "refact-agent/engine/src/tools/",
    "refact-agent/engine/crates/refact-tools/",
    "tools/dev/n2-move.py",
    "tools/dev/n2-check.py",
)
extra = [t for t in touched if not t.startswith(allowed_prefixes)]
check("no unrelated file touched", not extra, str(extra))
check("tools/mod.rs untouched", "refact-agent/engine/src/tools/mod.rs" not in touched)
check("engine Cargo.toml untouched", "refact-agent/engine/Cargo.toml" not in touched)
check("engine Cargo.lock untouched", "refact-agent/engine/Cargo.lock" not in touched)
check("tools_list.rs untouched", "refact-agent/engine/src/tools/tools_list.rs" not in touched)
check("task_tool_helpers.rs untouched", "refact-agent/engine/src/tools/task_tool_helpers.rs" not in touched)
check("planner_delivery.rs untouched", "refact-agent/engine/src/tools/planner_delivery.rs" not in touched)
check("crate Cargo.toml untouched", "refact-agent/engine/crates/refact-tools/Cargo.toml" not in touched)


def pristine(path):
    """Path unchanged relative to the pre-move commit."""
    return git("diff", "--name-only", BASE, "--", path) == ""


tools_list = read(SRC / "tools_list.rs")
for ctor in [
    "ToolAskQuestions {",
    "ToolAgentRegistry::new()",
    "ToolAgentDiff::new()",
    "ToolAgentPulse::new()",
    "ToolAgentChatSummary::new()",
    "ToolAgentSteer::new()",
    "ToolAgentAskPlanner::new()",
    "ToolPlannerReply::new()",
    "ToolTaskQuestionsList::new()",
    "ToolCancelAgent::new()",
    "ToolPauseAgent::new()",
    "ToolResumeAgent::new()",
    "ToolAgentsOverview {",
    "ToolAgentMessage {",
    "ToolProgressReport {",
]:
    check(f"tools_list.rs still constructs {ctor}", ctor in tools_list)

check("tools_list.rs is byte-identical to the pre-move commit",
      pristine("refact-agent/engine/src/tools/tools_list.rs"))
check("background_agents.rs (records_for_root caller) is byte-identical",
      pristine("refact-agent/engine/src/http/routers/v1/background_agents.rs"))

failed = [c for c in checks if not c[1]]
print(f"\n{len(checks) - len(failed)}/{len(checks)} checks passed")
if failed:
    print("RESULT: FAILED")
    for name, _, detail in failed:
        print("  FAIL", name, detail)
    sys.exit(1)
print("RESULT: ALL CHECKS PASSED")
