"""N-3 verification harness: static proof that the K9 review move is behaviour-preserving.

No build is attempted here (the project build guard refuses compilation from agent
worktrees). Every assertion is a filesystem/text check with no shell separators,
and the verbatim half recomputes the expected files from the mover's own rules.

Run: python tools/dev/n3-check.py
"""
import difflib
import importlib.util
import pathlib
import re
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parents[2]
_spec = importlib.util.spec_from_file_location(
    "n3_move", pathlib.Path(__file__).resolve().parent / "n3-move.py")
mover = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(mover)

ENG = mover.ENG
SRC = mover.SRC
DST = mover.DST
CRATE = mover.CRATE

MOVED = [rel for rel, _ in mover.MODS]
LEFT = ["review_agents/mod.rs", "review_agents/runner.rs", "tool_review.rs"]
ALREADY_SHIM = {
    "review_types.rs": "pub use refact_tool_api::review_types::*;",
    "review_merge.rs": "pub use refact_tool_api::review_merge::*;",
    "review_agents/contract.rs": "pub use refact_tool_api::review_contract::*;",
}

# prompt literals an LLM consumes: byte-identical is the whole point
PROMPT_MARKERS = [
    "You are one stage of a code review. You investigate with tools and report what you can prove.",
    "NO_FINDING is a valid, frequent and respected result.",
    "Your FINAL message must contain exactly one fenced json block",
    "Return exactly one verdict per finding id you were given",
]

# the repair prompt already lives in refact-tool-api (moved before this card)
CONTRACT_PROMPT_MARKERS = [
    "Re-emit JSON only: no prose, no explanation, no markdown outside a single fenced json block",
]

checks = []


def check(name, ok, detail=""):
    checks.append((name, ok))
    print(("PASS " if ok else "FAIL ") + name + ((" -- " + detail) if detail and not ok else ""))


def read(p):
    return p.read_text(encoding="utf-8")


def at_base(rel):
    raw = subprocess.run(
        ["git", "-C", str(ENG), "show",
         "%s:refact-agent/engine/src/tools/%s" % (mover.BASE, rel)],
        capture_output=True, check=True).stdout.decode("utf-8")
    return raw.replace("\r\n", "\n")


def git(*args):
    return subprocess.run(["git", "-C", str(ROOT), *args],
                          capture_output=True, text=True).stdout.strip()


# 1. verbatim proof, both halves, for every moved file
for rel, crate_rel in mover.MODS:
    lines = at_base(rel).split("\n")
    prod, test = mover.split_test(lines)

    expected_body = mover.crate_body(rel, prod)
    got_body = read(DST / crate_rel)
    body_ok = expected_body.rstrip("\n") == got_body.rstrip("\n")

    expected_shim = mover.shim_text(rel, prod, test)
    got_shim = read(SRC / rel)
    shim_ok = expected_shim.rstrip("\n") == got_shim.rstrip("\n")

    check("crate %s body is prod-verbatim + documented rewrites" % crate_rel, body_ok)
    check("engine %s shim is the re-export + kept tests verbatim" % rel, shim_ok)
    for label, exp, got in (("body", expected_body, got_body), ("shim", expected_shim, got_shim)):
        if exp.rstrip("\n") != got.rstrip("\n"):
            for line in list(difflib.unified_diff(
                    exp.split("\n"), got.split("\n"), "expected", "got", lineterm=""))[:24]:
                print("    %s %s" % (label, line))

# 2. no crate-internal path escaped the crate (engine-only names are gone)
for rel, crate_rel in mover.MODS:
    body = read(DST / crate_rel)
    stray = sorted({m for m in re.findall(r"crate::[\w:]*", body)
                    if not m.startswith("crate::tools::")})
    check("crate %s has no engine-only crate:: refs" % crate_rel, not stray, str(stray))
    check("crate %s has no engine global_context::tests path" % crate_rel,
          "global_context::tests" not in body)
    check("crate %s has no crate::llm path" % crate_rel, "crate::llm" not in body)

# 3. every shim re-exports a module the crate actually declares
tools_rs = read(CRATE / "src" / "tools.rs")
for rel, crate_rel in mover.MODS:
    path = rel[:-3].replace("/", "::")
    check("engine %s re-exports refact_tools::tools::%s" % (rel, path),
          ("pub use refact_tools::tools::%s::*;" % path) in read(SRC / rel))
    top = crate_rel.split("/")[0]
    top = top[:-3] if top.endswith(".rs") else top
    check("crate tools.rs declares pub mod %s" % top, "pub mod %s;" % top in tools_rs)
check("crate review_agents/mod.rs declares exactly the moved children",
      read(DST / "review_agents" / "mod.rs") == mover.CRATE_AGENT_MOD)
for child in ("config", "prompts", "stages"):
    check("crate review_agents/%s.rs exists" % child, (DST / "review_agents" / (child + ".rs")).exists())

declared = [line.strip()[len("pub mod "):-1] for line in tools_rs.split("\n")
            if line.strip().startswith("pub mod ") and line.strip().endswith(";")]
check("crate tools.rs module table is unique", len(declared) == len(set(declared)),
      str([n for n in declared if declared.count(n) > 1]))
check("crate tools.rs module table is sorted", declared == sorted(declared))
check("crate tools.rs keeps the settings_guard feature gate",
      '#[cfg(feature = "test-util")]\npub mod settings_guard;' in tools_rs)
check("engine src/tools/mod.rs still declares the review modules",
      all("pub mod %s;" % m in read(SRC / "mod.rs")
          for m in ("review_scope", "review_evidence", "review_agents")))

# 4. the already-shim trio is untouched
for rel, expected in ALREADY_SHIM.items():
    check("%s is still the refact-tool-api re-export" % rel, read(SRC / rel).strip() == expected,
          read(SRC / rel).strip())

# 5. the left-behind trio is byte-identical to BASE
for rel in LEFT:
    check("%s unchanged vs BASE" % rel, read(SRC / rel) == at_base(rel))
check("review_agents/runner.rs still imports the engine helper",
      "use crate::tools::subagent_phases::get_last_assistant_content;" in read(SRC / "review_agents" / "runner.rs"))
check("tool_review.rs still imports the engine helper",
      "use crate::tools::subagent_phases::resolve_gathered_file_path;" in read(SRC / "tool_review.rs"))

# 6. the LLM-facing prompt text is byte-identical to BASE
base_prompts = at_base("review_agents/prompts.rs")
crate_prompts = read(DST / "review_agents" / "prompts.rs")
for marker in PROMPT_MARKERS:
    check("prompt marker preserved in the crate: %s" % marker[:38], marker in crate_prompts)
check("steering header still lives in the engine-side caller tests",
      "# User steering (arrived while the review was running)" in at_base("tool_review.rs"))
contract = read(mover.ENG / "crates" / "refact-tool-api" / "src" / "review_contract.rs")
for marker in CONTRACT_PROMPT_MARKERS:
    check("contract prompt (refact-tool-api) preserved: %s" % marker[:38], marker in contract)
check("REVIEW_SYSTEM_PROMPT body is byte-identical to BASE",
      re.search(r"pub const REVIEW_SYSTEM_PROMPT: &str = r#\"(.*?)\"#;", base_prompts, re.S).group(1)
      == re.search(r"pub const REVIEW_SYSTEM_PROMPT: &str = r#\"(.*?)\"#;", crate_prompts, re.S).group(1))
for const in ("FINDINGS_CONTRACT", "VERDICTS_CONTRACT"):
    check("%s is byte-identical to BASE" % const,
          re.search(r'const %s: &str = r#"(.*?)"#;' % const, base_prompts, re.S).group(1)
          == re.search(r'const %s: &str = r#"(.*?)"#;' % const, crate_prompts, re.S).group(1))

# 7. report JSON surface unchanged (the types moved to refact-tool-api already)
for symbol in ("ReviewReport", "ReviewFinding", "StageRun", "ReviewScopeSummary", "ReviewDiffSummary"):
    check("review_types still exports %s" % symbol,
          ("pub struct %s" % symbol) in read(mover.ENG / "crates" / "refact-tool-api" / "src" / "review_types.rs"))

# 8. crate manifest never depends on the engine binary
crate_toml = read(CRATE / "Cargo.toml")
check("crate manifest never mentions refact-lsp", "refact-lsp" not in crate_toml)
check("engine [dependencies] keeps refact-tools",
      'refact-tools = { path = "crates/refact-tools" }' in read(ENG / "Cargo.toml"))
lock = read(ENG / "Cargo.lock")
tools_block = lock.split('name = "refact-tools"')[1].split("[[package]]")[0]
check("Cargo.lock refact-tools block still has no refact-lsp", "refact-lsp" not in tools_block)
for dep in ("refact-runtime-api", "refact-tool-api", "refact-worktrees", "refact-yaml-configs"):
    check("Cargo.lock refact-tools block depends on %s" % dep, ' "%s",' % dep in tools_block)

# 9. no include_str!/include_bytes! asset trap was introduced
for rel, crate_rel in mover.MODS:
    check("crate %s has no include_str!/include_bytes!" % crate_rel,
          "include_str!" not in read(DST / crate_rel) and "include_bytes!" not in read(DST / crate_rel))

# 10. constructor surface still resolves in the engine
callers = read(SRC / "tools_list.rs")
check("tools_list.rs still constructs ToolCodeReview via crate::tools::tool_review",
      "crate::tools::tool_review::ToolCodeReview {" in callers)
check("tools_list.rs still names the review tool box",
      "Box::new(crate::tools::tool_review::ToolCodeReview" in callers)

# 11. nothing that must stay was disturbed
dirty = git("status", "--porcelain").splitlines()
touched = {line.strip().split(None, 1)[1].replace("\\", "/")
           for line in dirty if line.strip().split(None, 1)[1:]}
allowed = (
    "refact-agent/engine/src/tools/review_scope.rs",
    "refact-agent/engine/src/tools/review_evidence.rs",
    "refact-agent/engine/src/tools/review_agents/config.rs",
    "refact-agent/engine/src/tools/review_agents/prompts.rs",
    "refact-agent/engine/src/tools/review_agents/stages.rs",
    "refact-agent/engine/crates/refact-tools/",
    "refact-agent/engine/Cargo.lock",
    "tools/dev/n3-check.py",
    "tools/dev/n3-move.py",
)
extra = sorted(t for t in touched if not t.startswith(allowed))
check("no unrelated file touched", not extra, str(extra))

failed = [c for c in checks if not c[1]]
print("\n%d/%d checks passed" % (len(checks) - len(failed), len(checks)))
if failed:
    print("RESULT: FAILED")
    sys.exit(1)
print("RESULT: ALL CHECKS PASSED")
