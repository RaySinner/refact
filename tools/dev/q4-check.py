"""Q-4 verification harness: 3 `http/routers/v1` modules harvested into `refact-http`.

Usage: python tools/dev/q4-check.py

The build guard refuses cargo from agent cards, so this proves the move
statically:

  R1  each engine file is a glob shim into `refact-http` (plus its inline tests);
  R2  each module is declared on both sides (crate `routers/v1/mod.rs`, engine `v1.rs`);
  R3  the moved body contains no `crate::` engine path (the only intra-crate
      reference is the documented `crate::routers::v1::git_read::…` retarget);
  R4  every external crate root the moved code uses is a declared `refact-http`
      dependency and is listed in the `refact-http` Cargo.lock entry;
  R5  no public handler disappeared: every `pub fn`/`pub async fn` of the original
      prod region still exists in the crate file;
  R6  the moved file keeps the original's `#[cfg(test)]` item count (tests not dropped);
  R7  no route path, SSE event name or JSON payload changed: no other engine file is
      touched beyond the shims and the census/tracker scripts;
  R8  balanced delimiters / no merge markers;
  R9  `refact-http/Cargo.toml` never mentions `refact-lsp` (no crate -> engine edge);
  R10 the crate file body is byte-identical to the pre-move body except for the
      documented rewrite table and the one test-facing widening;
  R11 the honesty census re-run reports the new movable/blocked numbers.
"""
import os
import re
import subprocess
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
ENGINE = "refact-agent/engine"
V1 = ENGINE + "/src/http/routers/v1"
CRATE_V1 = ENGINE + "/crates/refact-http/src/routers/v1"

MOVED = ["code_intel", "background_agents", "file_edit_tools"]

BASE = os.environ.get("Q4_BASE", "ac177abe7")

REWRITES = {
    "code_intel": [
        ("use crate::app_state::AppState;", "use refact_runtime_api::AppState;"),
        ("use crate::codegraph::code_intel_api::*;", "use refact_code_intel_api::*;"),
        ("use crate::custom_error::ScratchError;", "use refact_core::custom_error::ScratchError;"),
        (
            "crate::files_in_workspace::get_file_text_from_memory_or_disk",
            "refact_runtime_api::files_in_workspace::get_file_text_from_memory_or_disk",
        ),
        ("crate::tools::tool_codegraph::", "refact_tools::tools::tool_codegraph::"),
        (
            "super::git_read::resolve_requested_root",
            "crate::routers::v1::git_read::resolve_requested_root",
        ),
    ],
    "background_agents": [
        (
            "use crate::agents::types::{BackgroundAgent, BackgroundAgentSummary};",
            "use refact_agents_core::types::{BackgroundAgent, BackgroundAgentSummary};",
        ),
        ("use crate::app_state::AppState;", "use refact_runtime_api::AppState;"),
        (
            "crate::agents::delivery::deliver_to_agent",
            "refact_agents_core::delivery::deliver_to_agent",
        ),
        (
            "crate::agents::spawn::emit_background_agent_update",
            "refact_agents_core::spawn::emit_background_agent_update",
        ),
        ("crate::chat::internal_roles::", "refact_chat_api::internal_roles::"),
        (
            "crate::tools::tool_agent_interact::records_for_root",
            "refact_tools::tools::tool_agent_interact::records_for_root",
        ),
    ],
    "file_edit_tools": [
        ("use crate::app_state::AppState;", "use refact_runtime_api::AppState;"),
        ("use crate::call_validation::DiffChunk;", "use refact_core::call_validation::DiffChunk;"),
        ("use crate::custom_error::ScratchError;", "use refact_core::custom_error::ScratchError;"),
        ("crate::tools::file_edit::", "refact_tools::tools::file_edit::"),
    ],
}

WIDEN = {"code_intel": ["fn select_graph_node_ids("], "background_agents": [], "file_edit_tools": []}

ALLOWED_NEW_DEPS = [
    "refact-agents-core",
    "refact-chat-api",
    "refact-code-intel-api",
    "refact-codehealth",
    "refact-git-intel",
    "refact-tools",
]

STD = {"std", "core", "alloc", "crate", "self", "super", "test", "tests", "main"}

# engine files allowed to change beyond the shims
ALLOWED = (
    {f"{V1}/{n}.rs" for n in MOVED}
    | {f"{CRATE_V1}/{n}.rs" for n in MOVED}
    | {
        f"{CRATE_V1}/mod.rs",
        ENGINE + "/Cargo.lock",
        ENGINE + "/crates/refact-http/Cargo.toml",
        "tools/dev/q4-move.py",
        "tools/dev/q4-check.py",
    }
)

FAILURES = []
CHECKS = [0]


def read(path):
    with open(path, encoding="utf-8-sig", errors="replace") as fh:
        return fh.read()


def git(*args):
    return subprocess.run(
        ["git", "-C", ROOT, *args], capture_output=True, check=False
    ).stdout.decode("utf-8", errors="replace")


def check(cond, message):
    CHECKS[0] += 1
    if not cond:
        FAILURES.append(message)


def balance(text):
    stack = []
    pairs = {")": "(", "]": "[", "}": "{"}
    i = 0
    while i < len(text):
        if text[i : i + 2] == "//":
            i = text.find("\n", i)
            if i == -1:
                break
            continue
        ch = text[i]
        if ch == '"':
            i += 1
            while i < len(text) and text[i] != '"':
                i += 2 if text[i] == "\\" else 1
            i += 1
            continue
        if ch == "'":
            m = re.match(r"'(\\.|[^\\'])'", text[i:])
            if m:
                i += m.end()
                continue
        if ch in "([{":
            stack.append(ch)
        elif ch in ")]}":
            if not stack or stack.pop() != pairs[ch]:
                return False
        i += 1
    return not stack


def declared_deps():
    deps = set()
    for line in read(ENGINE + "/crates/refact-http/Cargo.toml").splitlines():
        m = re.match(r"^([A-Za-z0-9_-]+)\s*=", line)
        if m:
            deps.add(m.group(1).replace("-", "_"))
    return deps


STD_CRATES = {
    "str", "usize", "isize", "u8", "u16", "u32", "u64", "i8", "i16", "i32", "i64",
    "bool", "char", "f32", "f64", "Option", "Some", "None", "Result", "Ok", "Err",
}


def used_roots(text):
    roots, alias = set(), {}
    for m in re.finditer(
        r"(?m)^[ \t]*(?:pub(?:\([^)]*\))?[ \t]+)?use[ \t]+([a-z_][a-z0-9_]*)::([^;]+);", text, re.S
    ):
        root, rest = m.group(1), re.sub(r"\s+", " ", m.group(2))
        roots.add(root)
        head = rest.split("{")[0].rstrip(" :")
        if " as " in head:
            head = head.split(" as ")[-1].strip()
        elif head:
            head = head.split("::")[0].strip()
        if head and re.match(r"^[a-z_][a-z0-9_]*$", head):
            alias[head] = root
    body = re.sub(r"(?m)^[ \t]*(?:pub(?:\([^)]*\))?[ \t]+)?use[ \t]+[^;]+;", "", text)
    for name in re.findall(r"(?<![:\w])([a-z_][a-z0-9_]*)::(?=[A-Za-z_])", body):
        if name in alias:
            roots.add(alias[name])
    return {f for f in roots - STD if f != "refact_http"}


def prod_region(text):
    m = re.search(r"^[ \t]*#\[cfg\(test\)\]", text, re.M)
    return text[: m.start()] if m else text


def public_fns(text):
    return set(
        re.findall(
            r"(?m)^[ \t]*pub(?:\([^)]*\))?[ \t]+(?:async[ \t]+)?(?:unsafe[ \t]+)?"
            r"fn[ \t]+([A-Za-z_][A-Za-z0-9_]*)",
            text,
        )
    )


def normalise(text):
    return text.replace("\r\n", "\n")


def main():
    deps = declared_deps()
    manifest = read(ENGINE + "/crates/refact-http/Cargo.toml")
    check("refact-lsp" not in manifest, "R9 refact-http/Cargo.toml must not depend on refact-lsp")
    for dep in ALLOWED_NEW_DEPS:
        check(
            dep.replace("-", "_") in deps,
            "R4 %s declared in refact-http Cargo.toml" % dep,
        )

    lock = read(ENGINE + "/Cargo.lock")
    start = lock.find('name = "refact-http"')
    lock_block = lock[start : lock.find("[[package]]", start)]
    lock_names = {
        re.sub(r" .*$", "", entry).replace("-", "_")
        for entry in re.findall(r'^ "([^"]+)",$', lock_block, re.M)
    }
    for dep in ALLOWED_NEW_DEPS:
        check(
            dep.replace("-", "_") in lock_names,
            "R4 %s in the refact-http lock entry" % dep,
        )

    crate_v1_mod = read(CRATE_V1 + "/mod.rs")
    engine_v1 = read(V1 + ".rs")

    print("# base commit: %s" % BASE)

    for name in MOVED:
        engine_text = normalise(read(f"{V1}/{name}.rs"))
        crate_text = normalise(read(f"{CRATE_V1}/{name}.rs"))
        original = normalise(git("show", f"{BASE}:{V1}/{name}.rs"))
        check(bool(original.strip()), f"R0 {name}: base source recovered")
        if not original.strip():
            continue

        check(
            engine_text.startswith(f"pub use refact_http::routers::v1::{name}::*;"),
            f"R1 {name}: engine file starts with the glob shim",
        )
        check(f"pub mod {name};" in crate_v1_mod, f"R2 {name}: declared in crate v1/mod.rs")
        check(
            re.search(rf"(?m)^(?:pub )?mod {name};", engine_v1) is not None,
            f"R2 {name}: still declared in engine v1.rs",
        )

        stray = sorted(
            {
                m
                for m in re.findall(r"crate::[\w:]*", crate_text)
                if not m.startswith("crate::routers::v1::git_read::")
            }
        )
        check(not stray, f"R3 {name}: no crate:: engine reference left ({stray})")

        for root in sorted(used_roots(crate_text)):
            check(root in deps, f"R4 {name}: `{root}` declared in refact-http Cargo.toml")
            check(root in lock_names, f"R4 {name}: `{root}` in the lock entry")

        missing = sorted(public_fns(prod_region(original)) - public_fns(crate_text))
        check(not missing, f"R5 {name}: public fns preserved (missing {missing})")

        check(crate_text.count("#[cfg(test)]") == 0, f"R6 {name}: no cfg(test) moved to the crate")
        check(
            engine_text.count("#[cfg(test)]") >= original.count("#[cfg(test)]"),
            f"R6 {name}: every cfg(test) item kept ("
            f"{engine_text.count('#[cfg(test)]')} vs {original.count('#[cfg(test)]')})",
        )

        # R10: crate body == pre-move body after the documented rewrites and widening.
        expected = prod_region(original)
        for pattern in WIDEN[name]:
            expected = re.sub(
                r"(?m)^[ \t]*" + re.escape(pattern), "pub " + pattern.replace("\\", "\\\\"),
                expected,
                count=1,
            )
        for old, new in REWRITES[name]:
            expected = expected.replace(old, new)
        check(
            expected.rstrip("\n") == crate_text.rstrip("\n"),
            f"R10 {name}: crate body is the pre-move body plus the documented rewrites",
        )

        # the tests stay on the engine side, byte-identical (the shim prepends a
        # `#[cfg(test)]` import prelude, so compare from the test module header)
        m = re.search(r"(?m)^[ \t]*mod tests \{", original)
        if m:
            check(
                original[m.start() :].rstrip("\n") in engine_text,
                f"R6 {name}: inline tests are byte-identical in the engine shim",
            )

        check(balance(crate_text), f"R8 {name}: crate file delimiters balanced")
        check(balance(engine_text), f"R8 {name}: engine file delimiters balanced")
        for text, side in ((crate_text, "crate"), (engine_text, "engine")):
            check(
                "<<<<<<<" not in text and ">>>>>>>" not in text,
                f"R8 {name}: no merge markers in the {side} file",
            )

    dirty = set(git("diff", "--name-only", BASE, "--", ENGINE).split())
    untracked = set(
        line[3:].strip()
        for line in git("status", "--porcelain", "--untracked-files=all").splitlines()
        if line[:2] == "??"
    )
    unexpected = sorted((dirty | untracked) - ALLOWED)
    check(not unexpected, f"R7 no unrelated engine change (got {unexpected})")

    # R11: honesty census.
    target_dir = os.path.join(ROOT, ENGINE, "target")
    os.makedirs(target_dir, exist_ok=True)
    census = subprocess.run(
        [sys.executable, os.path.join(ROOT, "tools", "dev", "chat_movable_census.py"), "http"],
        cwd=ROOT,
        capture_output=True,
        text=True,
        encoding="utf-8",
    )
    print(census.stdout.strip())
    report_path = os.path.join(target_dir, "http_movable_census.txt")
    report = read(report_path) if os.path.exists(report_path) else ""
    for name in MOVED:
        check(
            re.search(rf"(?m)^\s*\d+\s+http::routers::v1::{name}\b", report) is None,
            f"R11 {name} is no longer a movable candidate (it is a shim now)",
        )
    check(
        "MOVABLE now: " in report,
        "R11 census report present (%s)" % report_path,
    )

    print(f"{CHECKS[0]} checks, {len(FAILURES)} failures")
    for failure in FAILURES:
        print(f"FAIL {failure}")
    return 1 if FAILURES else 0


if __name__ == "__main__":
    sys.exit(main())
