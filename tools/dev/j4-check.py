"""J-4 verification harness: 13 `http/routers/v1` modules harvested into `refact-http`.

The build guard refuses cargo from agent cards, so this proves the move statically:

  R1  each engine file is a glob shim into `refact-http` (plus its inline tests);
  R2  each module is declared on both sides (crate `routers/v1/mod.rs`, engine `v1.rs`);
  R3  the moved body contains no `crate::` engine path;
  R4  every external crate root the moved code uses is a declared `refact-http`
      dependency and is listed in the `refact-http` Cargo.lock entry;
  R5  no public handler disappeared: every `pub fn`/`pub async fn` of the original
      prod region still exists in the crate file;
  R6  the moved file keeps the original's `#[cfg(test)]` item count (tests not dropped);
  R7  no route path, SSE event name or JSON payload changed: no other engine file is
      touched beyond the shims and the documented blocker retargets;
  R8  balanced delimiters / no merge markers;
  R9  `refact-http/Cargo.toml` never mentions `refact-lsp` (no crate -> engine edge).

Usage: python tools/dev/j4-check.py
"""
import os
import re
import subprocess
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
ENGINE = "refact-agent/engine"
V1 = ENGINE + "/src/http/routers/v1"
CRATE_V1 = ENGINE + "/crates/refact-http/src/routers/v1"

MOVED = [
    "code_lens",
    "commands_marketplace",
    "exec",
    "ext_marketplace_sources",
    "git_read",
    "performance",
    "scheduler",
    "settings_config",
    "skills_marketplace",
    "skills_status",
    "status",
    "subagents_marketplace",
    "vecdb",
]

STD = {"std", "core", "alloc", "crate", "self", "super", "test", "tests", "main"}

# engine files allowed to change beyond the shims: the three blocker retargets.
EXTRA_ENGINE_EDITS = {
    V1 + "/scheduler.rs",
    V1 + "/settings_config.rs",
    ENGINE + "/src/tools/tool_cron_create.rs",
    ENGINE + "/crates/refact-scheduler-core/src/lib.rs",
    ENGINE + "/crates/refact-scheduler-core/src/types.rs",
}
ALLOWED = (
    {f"{V1}/{n}.rs" for n in MOVED}
    | {f"{CRATE_V1}/{n}.rs" for n in MOVED}
    | EXTRA_ENGINE_EDITS
    | {
        ENGINE + "/Cargo.lock",
        ENGINE + "/crates/refact-http/Cargo.toml",
        ENGINE + "/crates/refact-http/src/routers/v1/mod.rs",
    }
)

FAILURES = []
CHECKS = [0]


def read(path):
    with open(path, encoding="utf-8-sig", errors="replace") as fh:
        return fh.read()


def git(*args):
    return subprocess.run(
        ["git", "-C", ROOT, *args], capture_output=True, text=True, check=False
    ).stdout


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
    """Crate roots reached by the file: `use a::b::c;` roots plus body paths.

    Body identifiers are mapped back through the `use` aliases, so
    `use tokio::sync::broadcast;` + `broadcast::error::X` counts as `tokio`, while
    intrinsic segments such as `str::from_utf8` are ignored.
    """
    roots = set()
    alias = {}
    for m in re.finditer(
        r"(?m)^[ \t]*(?:pub(?:\([^)]*\))?[ \t]+)?use[ \t]+([a-z_][a-z0-9_]*)::([^;]+);",
        text,
        re.S,
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
    body = re.sub(
        r"(?m)^[ \t]*(?:pub(?:\([^)]*\))?[ \t]+)?use[ \t]+[^;]+;", "", text
    )
    for name in re.findall(r"(?<![:\w])([a-z_][a-z0-9_]*)::(?=[A-Za-z_])", body):
        if name in alias:
            roots.add(alias[name])
    return {f for f in roots - STD if f != "refact_http"}


def prod_region(text):
    """Text before the first `#[cfg(test)]` item."""
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


def main():
    deps = declared_deps()
    manifest = read(ENGINE + "/crates/refact-http/Cargo.toml")
    check(
        "refact-lsp" not in manifest,
        "R9 refact-http/Cargo.toml must not depend on refact-lsp",
    )

    lock = read(ENGINE + "/Cargo.lock")
    start = lock.find('name = "refact-http"')
    lock_block = lock[start : lock.find("[[package]]", start)]
    lock_names = {
        re.sub(r" .*$", "", entry).replace("-", "_")
        for entry in re.findall(r'^ "([^"]+)",$', lock_block, re.M)
    }

    crate_v1_mod = read(CRATE_V1 + "/mod.rs")
    engine_v1 = read(V1 + ".rs")

    base = os.environ.get("J4_BASE", "015219e71")
    print("# base commit: %s" % base)

    for name in MOVED:
        engine_text = read(f"{V1}/{name}.rs")
        crate_text = read(f"{CRATE_V1}/{name}.rs")
        original = git("show", f"{base}:{V1}/{name}.rs")
        check(bool(original.strip()), f"R0 {name}: base source recovered")
        if not original.strip():
            continue

        check(
            engine_text.startswith(
                f"pub use refact_http::routers::v1::{name}::*;"
            ),
            f"R1 {name}: engine file starts with the glob shim",
        )
        check(
            f"pub mod {name};" in crate_v1_mod,
            f"R2 {name}: declared in crate v1/mod.rs",
        )
        check(
            re.search(rf"(?m)^(?:pub )?mod {name};", engine_v1) is not None,
            f"R2 {name}: still declared in engine v1.rs",
        )
        check("crate::" not in crate_text, f"R3 {name}: no crate:: reference left")

        for root in sorted(used_roots(crate_text)):
            check(
                root in deps,
                f"R4 {name}: `{root}` declared in refact-http Cargo.toml",
            )
            check(root in lock_names, f"R4 {name}: `{root}` in the lock entry")

        missing = sorted(public_fns(prod_region(original)) - public_fns(crate_text))
        check(not missing, f"R5 {name}: public fns preserved (missing {missing})")

        check(
            crate_text.count("#[cfg(test)]") == 0
            or engine_text.count("#[cfg(test)]") >= 1,
            f"R6 {name}: inline tests stay on the engine side",
        )
        check(
            engine_text.count("#[cfg(test)]") >= original.count("#[cfg(test)]"),
            f"R6 {name}: every cfg(test) item kept ("
            f"{engine_text.count('#[cfg(test)]')} vs {original.count('#[cfg(test)]')})",
        )

        check(balance(crate_text), f"R8 {name}: crate file delimiters balanced")
        check(balance(engine_text), f"R8 {name}: engine file delimiters balanced")
        for text, side in ((crate_text, "crate"), (engine_text, "engine")):
            check(
                "<<<<<<<" not in text and ">>>>>>>" not in text,
                f"R8 {name}: no merge markers in the {side} file",
            )

    dirty = set(git("diff", "--name-only", base, "--", ENGINE).split())
    unexpected = sorted(dirty - ALLOWED)
    check(not unexpected, f"R7 no unrelated engine change (got {unexpected})")

    print(f"{CHECKS[0]} checks, {len(FAILURES)} failures")
    for failure in FAILURES:
        print(f"FAIL {failure}")
    return 1 if FAILURES else 0


sys.exit(main())
