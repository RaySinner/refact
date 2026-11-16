#!/usr/bin/env python3
"""AL-6 structural checks: chat::prompts + chat::verifier settled into refact-tools.

Layer A of the AL-6 verification (the compiler layer runs through
`tools/dev/engine-build.ps1`; see tools/dev/al6-verify.ps1). Every check is a
structural fact about the tree, so it runs anywhere without cargo.

Usage:  python tools/dev/al6-check.py [<worktree-root>] [<base-rev>]
Exit:   0 = all checks pass, 1 = at least one check failed.
"""

import io
import os
import re
import subprocess
import sys

ROOT = os.path.abspath(sys.argv[1]) if len(sys.argv) > 1 else os.path.dirname(
    os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
)
BASE_REV = sys.argv[2] if len(sys.argv) > 2 else "6047df3bd"

ENGINE = os.path.join(ROOT, "refact-agent", "engine")
CRATES = os.path.join(ENGINE, "crates")
TOOLS = os.path.join(CRATES, "refact-tools", "src")

CRATE_PROMPTS = os.path.join(TOOLS, "chat_prompts.rs")
CRATE_VERIFIER = os.path.join(TOOLS, "chat_verifier.rs")
TOOLS_LIB = os.path.join(TOOLS, "lib.rs")
TOOLS_CARGO = os.path.join(CRATES, "refact-tools", "Cargo.toml")

ENGINE_PROMPTS = os.path.join(ENGINE, "src", "chat", "prompts.rs")
ENGINE_VERIFIER = os.path.join(ENGINE, "src", "chat", "verifier.rs")
ENGINE_VERIFY_CMD = os.path.join(ENGINE, "src", "chat", "verify_cmd.rs")
ENGINE_CARGO = os.path.join(ENGINE, "Cargo.toml")
CHAT_API_CARGO = os.path.join(CRATES, "refact-chat-api", "Cargo.toml")
RUNTIME_API_CARGO = os.path.join(CRATES, "refact-runtime-api", "Cargo.toml")
LOCK = os.path.join(ENGINE, "Cargo.lock")
CENSUS_OUT = os.path.join(ENGINE, "target", "chat_movable_census.txt")

PROMPTS_REL = "refact-agent/engine/src/chat/prompts.rs"
VERIFIER_REL = "refact-agent/engine/src/chat/verifier.rs"

CHAT_API_DEPS = {
    "refact-core", "refact-caps-core", "refact-tool-api", "refact-privacy",
    "refact-tasks", "refact-ext", "refact-perf",
}

results = []


def check(name, ok, detail=""):
    results.append((name, bool(ok), detail))


def read(path):
    with io.open(path, "r", encoding="utf-8", errors="surrogateescape") as fh:
        return fh.read()


def strip_code(text):
    text = re.sub(r'"(?:\\.|[^"\\])*"', '""', text)
    text = re.sub(r"(?m)//.*$", "", text)
    return re.sub(r"/\*.*?\*/", "", text, flags=re.S)


def prod_part(text):
    """Everything before the first top-level `#[cfg(test)]` line."""
    out = []
    for line in text.splitlines():
        if re.match(r"^\s*#\[cfg\(test\)\]\s*$", line):
            break
        out.append(line)
    return "\n".join(out)


NAME_RE = re.compile(r"(?m)^\s*(?:pub(?:\([a-z]+\))?\s+)?(?:async\s+)?(?:fn|struct|enum|trait|const|static)\s+([A-Za-z_][A-Za-z0-9_]*)")


def git_show(rel):
    out = subprocess.run(
        ["git", "show", f"{BASE_REV}:{rel}"], cwd=ROOT, capture_output=True
    )
    if out.returncode != 0:
        return None
    return out.stdout.decode("utf-8")


def block_end(text, start):
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


def cut_tests(text):
    """Split into (production text, tests text) where tests = the top-level
    `#[cfg(test)] mod tests` block only; production text keeps any code that
    follows the block (prompts.rs carries `inject_fresh_room_roster` after it)."""
    at = text.find("#[cfg(test)]\nmod tests {")
    if at < 0:
        return text, ""
    end = block_end(text, at)
    tests = text[at:end]
    prod = text[:at] + text[end:]
    return prod, tests


def normalise(text):
    text = re.sub(r"\bpub\(crate\)\s+", "", text)
    text = re.sub(r"(?m)^\s*pub\s+", "", text)
    return re.sub(r"[ \t]+", " ", "\n".join(l for l in text.splitlines() if l.strip())).strip()


# ------------------------------------------------------------------ the shims

for label, engine_path, module in (
    ("prompts", ENGINE_PROMPTS, "refact_tools::chat_prompts"),
    ("verifier", ENGINE_VERIFIER, "refact_tools::chat_verifier"),
):
    shim = read(engine_path)
    check(
        f"engine src/chat/{label}.rs globs the crate module",
        f"pub use {module}::*;" in shim,
        "missing glob re-export",
    )
    check(
        f"engine {label} shim keeps its verbatim test module",
        "#[cfg(test)]\nmod tests {" in shim and "make_test_gcx" in shim,
        "the make_test_gcx-dependent tests must stay engine-side",
    )

verifier_shim = read(ENGINE_VERIFIER)
check(
    "engine verifier shim keeps the CardVerifier seam struct",
    "pub struct EngineCardVerifier;" in verifier_shim
    and "impl CardVerifier for EngineCardVerifier" in verifier_shim,
    "the engine wires this struct into ChatCtx",
)
check(
    "engine verifier delegates verification_command_policy to chat::verify_cmd",
    "crate::chat::verify_cmd::load_for_ctx(ctx)" in verifier_shim,
    "the shell-policy loader is engine-owned and must stay",
)
check(
    "engine verify_cmd still owns the shell-policy loader",
    "pub(crate) async fn load_for_ctx" in read(ENGINE_VERIFY_CMD),
    "verify_cmd is the engine implementation of the policy seam",
)

# ------------------------------------------------------------- the crate body

prompts = read(CRATE_PROMPTS)
verifier = read(CRATE_VERIFIER)
prompts_prod = prod_part(prompts)
verifier_prod = prod_part(verifier)

for label, text, syms in (
    (
        "chat_prompts",
        prompts_prod,
        (
            "pub async fn get_mode_system_prompt(",
            "pub async fn render_mode_plan_template(",
            "pub async fn system_prompt_add_extra_instructions(",
            "pub async fn prepend_the_right_system_prompt_and_maybe_more_initial_messages(",
            "pub async fn inject_task_memories(",
            "pub async fn models_info_snapshot(",
            "pub fn clear_buddy_persona_cache_for_tests(",
            "pub const BUDDY_PERSONALITY_MARKER",
        ),
    ),
    (
        "chat_verifier",
        verifier_prod,
        (
            "pub async fn verify_card(",
            "pub async fn schedule_card_verifier(",
            "pub async fn shutdown_card_verifiers(",
            "pub async fn store_verifier_report(",
            "pub async fn verify_card_for_ctx_impl(",
            "pub async fn run_verification_argv_impl(",
            "pub fn classify_verifier_report(",
            "pub trait VerificationCommandRunner",
        ),
    ),
):
    for sym in syms:
        check(f"crate {label} exports {sym.split()[-1].split('(')[0]}", sym in text, sym)

for label, prod in (("chat_prompts", prompts_prod), ("chat_verifier", verifier_prod)):
    leftover = sorted(set(re.findall(r"\b(?:crate|super)::[A-Za-z_:]+", strip_code(prod))))
    leftover = [p for p in leftover if not p.startswith("crate::tools::tool_task_memory")]
    check(
        f"crate {label} production body carries no engine path",
        not leftover,
        "; ".join(leftover)[:300],
    )
    check(
        f"crate {label} production body carries no cfg(test) gate",
        "cfg(test)" not in prod,
        "the crate half must be gated on test-util, not cfg(test)",
    )

check(
    "crate chat_prompts keeps the tool_task_memory intra-crate path",
    "crate::tools::tool_task_memory::" in prompts_prod,
    "refact_tools::tools::tool_task_memory is the real definition site",
)
check(
    "crate chat_prompts routes the capability load through runtime-api",
    "refact_runtime_api::caps_loader::try_load_caps_quickly_if_not_present" in prompts_prod,
    "the task-briefing subchat loads caps through the crate seam",
)
check(
    "crate chat_verifier routes the policy through the CardVerifier seam",
    "ctx.card_verifier.verification_command_policy(ctx).await" in verifier_prod,
    "the crate may not read the engine shell-policy file",
)
check(
    "crate chat_verifier keeps no engine shell_gate path",
    "shell_gate" not in verifier_prod,
    "engine path survived the move",
)

lib = read(TOOLS_LIB)
check(
    "refact-tools lib declares mod chat_prompts",
    re.search(r"(?m)^pub mod chat_prompts;", lib) is not None,
    "missing module declaration",
)
check(
    "refact-tools lib declares mod chat_verifier",
    re.search(r"(?m)^pub mod chat_verifier;", lib) is not None,
    "missing module declaration",
)

# ------------------------------------------------------------- crate edges

check("edge refact-tools -> refact-lsp is absent", "refact-lsp" not in read(TOOLS_CARGO),
      "crate->engine edge")
check("edge refact-lsp -> refact-tools is present",
      re.search(r"(?m)^refact-tools\s*=", read(ENGINE_CARGO)) is not None,
      "the engine must depend on the crate it shims")
check("edge refact-runtime-api -> refact-lsp is absent",
      "refact-lsp" not in read(RUNTIME_API_CARGO), "crate->engine edge")
check("edge refact-tools -> refact-runtime-api is present",
      re.search(r"(?m)^refact-runtime-api\s*=", read(TOOLS_CARGO)) is not None,
      "the moved body calls into runtime-api")
check("edge refact-tools -> refact-chat-api is present",
      re.search(r"(?m)^refact-chat-api\s*=", read(TOOLS_CARGO)) is not None,
      "the moved body calls into chat-api")

dep_block = read(CHAT_API_CARGO).split("[dependencies]", 1)[-1].split("\n[", 1)[0]
chat_api_deps = set(re.findall(r"(?m)^(refact-[a-z0-9-]+)\s*=", dep_block))
check(
    "refact-chat-api dependency set is unchanged",
    chat_api_deps == CHAT_API_DEPS,
    f"expected {sorted(CHAT_API_DEPS)}, found {sorted(chat_api_deps)}",
)
for crate in ("refact-tools", "refact-runtime-api", "refact-files", "refact-scratchpads"):
    check(f"refact-chat-api does not depend on {crate}", crate not in chat_api_deps,
          "would create a cycle")

tools_block = ""
for block in read(LOCK).split("[[package]]"):
    if 'name = "refact-tools"' in block:
        tools_block = block
        break
check(
    "Cargo.lock records the refact-tools -> refact-chat-api edge",
    '"refact-chat-api",' in tools_block,
    "path dependency missing from the lockfile entry",
)

# ------------------------------------------------------- body preservation

for label, rel, crate_text in (
    ("prompts", PROMPTS_REL, CRATE_PROMPTS),
    ("verifier", VERIFIER_REL, CRATE_VERIFIER),
):
    base = git_show(rel)
    if base is None:
        check(f"base revision carries {rel}", False, f"git show {BASE_REV} failed")
        continue
    base_prod, base_tests = cut_tests(base)
    shim_tests = cut_tests(read(os.path.join(ROOT, rel)))[1]
    check(
        f"{label}: engine test module text is verbatim",
        base_tests.strip() == shim_tests.strip(),
        "the kept #[cfg(test)] mod tests must be byte-identical to the base",
    )
    shim_prod = cut_tests(read(os.path.join(ROOT, rel)))[0]
    allowed_defs = (
        {
            "EngineCardVerifier",
            "load_expected_card_state",
            "verify_card",
            "schedule_card_verifier_after_finish",
            "run_verification_argv",
            "verification_command_policy",
        }
        if label == "verifier"
        else set()
    )
    defined = set(NAME_RE.findall(strip_code(shim_prod)))
    check(
        f"{label}: engine shim production half carries no item definitions",
        defined <= allowed_defs,
        f"found: {sorted(defined - allowed_defs)}",
    )
    check(
        f"{label}: base production half splits cleanly",
        len(base_prod.splitlines()) > 100,
        f"base prod {len(base_prod.splitlines())} lines",
    )

base_prompts_prod, base_prompts_tests = cut_tests(git_show(PROMPTS_REL) or "")
base_verifier_prod, base_verifier_tests = cut_tests(git_show(VERIFIER_REL) or "")

# Every production item the base defined must still exist, modulo the mechanical

NAME_RE = re.compile(r"(?m)^\s*(?:pub(?:\([a-z]+\))?\s+)?(?:async\s+)?(?:fn|struct|enum|trait|const|static)\s+([A-Za-z_][A-Za-z0-9_]*)")


def names(text):
    return set(NAME_RE.findall(strip_code(text)))


PROMPTS_EXPECTED_ENGINE_MISSING = {"EngineCardVerifier", "verification_command_policy"}
VERIFIER_EXPECTED_ENGINE_MISSING = {"EngineCardVerifier"}

for label, base_text, crate_text in (
    ("prompts", base_prompts_prod, prompts_prod),
    ("verifier", base_verifier_prod, verifier_prod),
):
    missing = sorted(names(base_text) - names(crate_text))
    missing = [m for m in missing
               if not (m == "EngineCardVerifier" or m == "verification_command_policy")]
    check(
        f"{label}: no production item was dropped in the move",
        not missing,
        "missing: " + ", ".join(missing)[:280],
    )
    leftover = sorted(names(crate_text) - names(base_text))
    check(
        f"{label}: the crate adds no production item the engine did not have",
        not leftover,
        "added: " + ", ".join(leftover)[:280],
    )

check(
    "engine keeps only EngineCardVerifier + the policy seam",
    "EngineCardVerifier" in verifier_shim and "verification_command_policy" in verifier_shim
    and "impl CardVerifier for EngineCardVerifier" in verifier_shim,
    "the seam struct must stay engine-side",
)

# prompt literals: compare the set of string literals between the whole base
# file (production + tests stay together in the engine) and the crate body.
LIT_RE = re.compile(r'"(?:\\.|[^"\\])*"')
base_lits = set(LIT_RE.findall(strip_code(git_show(PROMPTS_REL) or "")))
crate_lits = set(LIT_RE.findall(strip_code(prompts)))
base_only = sorted(base_lits - crate_lits)
crate_only = sorted(crate_lits - base_lits)
crate_only = [l for l in crate_only if l != '"test-util"']
check(
    "prompts: every prompt literal survives the move",
    not base_only and not crate_only,
    f"base-only {len(base_only)}, crate-only {len(crate_only)}: "
    + "; ".join(ascii(l)[:60] for l in (base_only + crate_only)[:6]),
)

# ------------------------------------------------------------------- census

if os.path.exists(CENSUS_OUT):
    census = read(CENSUS_OUT)
    m = re.search(r"### chat: (\d+) files / (\d+) production lines", census)
    check("census covers the whole tree", m is not None, "class totals missing")
    if m:
        check(
            "census: chat shrank below the pre-move 20392 lines",
            int(m.group(2)) < 20392,
            f"current total {m.group(2)}",
        )
    for name in ("chat::prompts", "chat::verifier"):
        hit = re.search(r"(?m)^\s*(\d+)\s+%s\s" % re.escape(name), census)
        allowed = 60 if name == "chat::prompts" else 70
        check(
            f"census: {name} is released (at most {allowed} residual lines)",
            hit is not None and int(hit.group(1)) <= allowed,
            "prompts/verifier must drop to the engine test-only residue",
        )
    check(
        "census: chat::prepare is still listed",
        "chat::prepare" in census,
        "the census must still cover the un-moved modules",
    )
else:
    check("census output exists", False, CENSUS_OUT)

failed = [name for name, ok, _ in results if not ok]
for name, ok, detail in results:
    print(("PASS " if ok else "FAIL ") + name + ("" if ok else "  <- " + detail))
print(f"\nAL-6 checks: {len(results)} run, {len(failed)} failed")
sys.exit(1 if failed else 0)
