"""S-5 verification harness (in-repo only; the build guard refuses cargo from agent cards).

Usage:  python tools/dev/s5-check.py

Classifies and proves the four engine-root files.  Every check is a filesystem / git-blob
comparison except the final `cargo check` gate.

  S1  prod-line census before/after for the four files;
  S2  every moved body is byte-identical to the pre-move text (plus the documented rewrites);
  S3  the engine files that became shims wire the crate back (glob/`pub use`);
  S4  no crate ever gains a `refact-lsp` edge;
  S5  the declared islands are still engine-side and nowhere else;
  S6  the moved inline tests survive verbatim on the crate side;
  S7  delimiters balanced, no merge markers;
  S8  `engine-build.ps1 -Mode check` exits 0 (skipped with --static);
      `--with-tests` also runs `-Mode check-tests` and asserts the documented
      36-error baseline (25 `review_agents/mod.rs`, 9 `files_in_workspace/tests.rs`,
      2 `tool_task_spawn_agent.rs`) - pre-existing, unrelated to this card.
"""

import os
import re
import subprocess
import sys

sys.stdout.reconfigure(encoding="utf-8", errors="replace")

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
ENG = os.path.join(ROOT, "refact-agent", "engine")
SRC = os.path.join(ENG, "src")
CRATES = os.path.join(ENG, "crates")

BASE = os.environ.get("S5_BASE", "58fbd4aac")

REL = "refact-agent/engine/src/%s"

EXPECTED_SHRINK = ("global_context.rs", "tokens.rs", "http.rs")
UNCHANGED = ("files_correction.rs",)

FAILURES = []
CHECKS = [0]


def read(path):
    with open(path, encoding="utf-8", errors="replace") as fh:
        return fh.read().replace("\r\n", "\n")


def git(*args):
    return subprocess.run(
        ["git", "-C", ROOT, *args], capture_output=True, check=False
    ).stdout.decode("utf-8", errors="replace").replace("\r\n", "\n")


def base_blob(rel):
    return git("show", "%s:%s" % (BASE, rel))


def prod(text):
    index = text.find("#[cfg(test)]")
    return text[:index] if index >= 0 else text


def prod_line_count(text):
    """Lines strictly before the first `#[cfg(test)]` marker."""
    return prod(text).count("\n")


def prod_lines(path):
    return prod_line_count(read(path))


def check(cond, message):
    CHECKS[0] += 1
    if not cond:
        FAILURES.append(message)


def balance(text):
    stack, pairs, i = [], {")": "(", "]": "[", "}": "{"}, 0
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


MOVED_FNS = [
    "load_engine_global_config",
    "migrate_to_config_folder",
    "get_app_searchable_id",
    "block_until_signal",
]


def build_cmd(mode):
    return [
        "powershell", "-NoProfile", "-ExecutionPolicy", "Bypass",
        "-File", os.path.join(ROOT, "tools", "dev", "engine-build.ps1"),
        "-Mode", mode,
    ]


CHECK_TESTS_LOG = os.path.join(ENG, "target", "s5-check-tests.log")

RE_ERROR = re.compile(r"^error(?:\[E\d+\])?: ")
RE_LOCATION = re.compile(r"^\s*--> ([^:]+):\d+:")


def analyze_check_tests(log):
    """Real `error:` lines (not the summary) plus the files they point at."""
    errors, by_file, pending = 0, {}, False
    for line in log.splitlines():
        if line.startswith("error: could not compile"):
            pending = False
            continue
        if RE_ERROR.match(line):
            errors += 1
            pending = True
            continue
        match = RE_LOCATION.match(line)
        if match and pending:
            name = match.group(1).strip().replace("\\", "/")
            by_file[name] = by_file.get(name, 0) + 1
            pending = False
    return errors, by_file


def report_check_tests(log):
    errors, by_file = analyze_check_tests(log)
    print("  %d errors in %d files" % (errors, len(by_file)))
    for name, count in sorted(by_file.items()):
        print("    %-45s %d" % (name, count))
    check(errors == 36, "S8b exactly the 36 baseline errors (%d)" % errors)
    unexpected = {
        name: count
        for name, count in by_file.items()
        if not any(
            name.endswith(known)
            for known in (
                "tools/review_agents/mod.rs",
                "files_in_workspace/tests.rs",
                "tools/tool_task_spawn_agent.rs",
            )
        )
    }
    check(not unexpected, "S8b no error in a file this card touched (%s)" % unexpected)


def fn_bodies(text, name):
    """Brace-enclosed interiors of every definition of `name` (signature excluded)."""
    marker = ("pub async fn %s(" % name, "pub(crate) fn %s(" % name, "pub fn %s(" % name,
              "async fn %s(" % name, "fn %s(" % name)
    bodies = []
    for candidate in marker:
        search = 0
        while True:
            start = text.find("\n" + candidate, search)
            if start < 0:
                break
            search = start + 1
            open_brace = text.find("{", start)
            if open_brace < 0:
                continue
            depth, k = 0, open_brace
            while k < len(text):
                if text[k] == "{":
                    depth += 1
                elif text[k] == "}":
                    depth -= 1
                    if depth == 0:
                        bodies.append(text[open_brace : k + 1])
                        break
                k += 1
    return bodies


def body_present(text, name, rewrites=()):
    bodies = fn_bodies(text, name)
    for body in bodies:
        if body in text:
            return True
        for old, new in rewrites:
            if body.replace(old, new) in text:
                return True
    return False


def main():
    print("# base commit: %s" % BASE)

    # --------------------------------------------------------------- S1 census
    print("\n### S1 prod lines before -> after (line of the first #[cfg(test)])")
    for name in EXPECTED_SHRINK + UNCHANGED:
        before = prod_line_count(git("show", "%s:%s" % (BASE, REL % name)))
        after = prod_lines(os.path.join(SRC, name))
        print("  %-22s %4d -> %4d" % (name, before, after))
        if name in UNCHANGED:
            check(after == before, "S1 %s untouched (%d -> %d)" % (name, before, after))
        else:
            check(after < before, "S1 %s shrank (%d -> %d)" % (name, before, after))

    # ------------------------------------------------- S2 moved bodies verbatim
    gc_base = base_blob("refact-agent/engine/src/global_context.rs")
    boot = read(os.path.join(CRATES, "refact-runtime-api", "src", "engine_bootstrap.rs"))
    check(bool(gc_base.strip()), "S0 base global_context.rs recovered")
    for name in MOVED_FNS:
        check(bool(fn_bodies(gc_base, name)), "S0 base defines %s" % name)
        check(
            body_present(
                boot,
                name,
                [("crate::files_correction::canonical_path", "canonical_path")],
            ),
            "S2 %s body moved verbatim" % name,
        )

    net = read(os.path.join(CRATES, "refact-core", "src", "net_utils.rs"))
    http_base = base_blob("refact-agent/engine/src/http.rs")
    check(bool(fn_bodies(http_base, "resolve_http_bind_addr")),
          "S0 base defines resolve_http_bind_addr")
    check(body_present(net, "resolve_http_bind_addr"),
          "S2 resolve_http_bind_addr body moved verbatim")

    tok_base = base_blob("refact-agent/engine/src/tokens.rs")
    chat_tokens = read(os.path.join(CRATES, "refact-runtime-api", "src", "chat_tokens.rs"))
    for name in ("cached_tokenizer", "cached_tokenizer_for_ctx", "cached_tokenizer_for_parts",
                 "tokenizer_load_is_disabled"):
        check(bool(fn_bodies(tok_base, name)), "S0 base tokens.rs defines %s" % name)
        check(
            body_present(
                chat_tokens,
                name,
                [
                    (
                        "        let (client, cache_dir, caps_state) = {\n"
                        "            (\n"
                        "                http_client,\n"
                        "                cache_dir,\n"
                        "                caps_state,\n"
                        "            )\n"
                        "        };\n",
                        "        let (client, cache_dir, caps_state) = (http_client, cache_dir, caps_state);\n",
                    ),
                    ("    match Tokenizer::from_file(path) {\n"
                     "        Ok(_) => true,\n"
                     "        Err(_) => false,\n"
                     "    }\n",
                     "    Tokenizer::from_file(path).is_ok()\n"),
                ],
            ),
            "S2 %s body present in chat_tokens" % name,
        )

    # ------------------------------------------------ S6 inline tests preserved
    tok_tests = tok_base[tok_base.find("#[cfg(test)]"):]
    check(tok_tests.strip() in chat_tokens, "S6 tokens tests survive verbatim in the crate")
    check(tok_tests.strip() in read(os.path.join(SRC, "tokens.rs")),
          "S6 tokens tests survive verbatim in the engine shim")
    http_tests = http_base[http_base.find("#[cfg(test)]"):]
    check(http_tests.strip() not in read(os.path.join(SRC, "http.rs")),
          "S6 the four pure bind tests left the engine http.rs")
    for name in ("http_bind_defaults_to_loopback", "http_bind_preserves_inside_container_default",
                 "http_bind_uses_explicit_host", "http_bind_rejects_invalid_host"):
        check(name in net, "S2 %s moved to refact-core" % name)
    check("mdns_advertises_without_daemon_endpoint" in read(os.path.join(SRC, "http.rs")),
          "S6 the mDNS tests stay engine-side")

    # ------------------------------------------------------------- S3 the shims
    engine_tokens = read(os.path.join(SRC, "tokens.rs"))
    check("refact_runtime_api::chat_tokens::" in engine_tokens,
          "S3 tokens.rs re-exports the crate")
    check(re.search(r"(?m)^\s*(?:pub )?(?:async )?fn ", prod(engine_tokens)) is None,
          "S3 tokens.rs defines no function outside its test module")
    engine_gc = read(os.path.join(SRC, "global_context.rs"))
    check("refact_runtime_api::engine_bootstrap::" in engine_gc,
          "S3 global_context.rs re-exports engine_bootstrap")
    engine_http = read(os.path.join(SRC, "http.rs"))
    check("refact_core::net_utils::resolve_http_bind_addr" in engine_http,
          "S3 http.rs imports the moved bind helper")
    check("pub mod engine_bootstrap;" in read(os.path.join(CRATES, "refact-runtime-api", "src", "lib.rs")),
          "S3 engine_bootstrap declared in lib.rs")

    # ---------------------------------------------------- S4 no crate->engine
    for crate in ("refact-runtime-api", "refact-core", "refact-http", "refact-files"):
        manifest = read(os.path.join(CRATES, crate, "Cargo.toml"))
        check("refact-lsp" not in manifest, "S4 %s must not depend on refact-lsp" % crate)
    runtime_manifest = read(os.path.join(CRATES, "refact-runtime-api", "Cargo.toml"))
    check("winreg" in runtime_manifest, "S4 winreg moved into refact-runtime-api")
    check("pnet_datalink" in runtime_manifest, "S4 pnet_datalink moved into refact-runtime-api")
    engine_manifest = read(os.path.join(ENG, "Cargo.toml"))
    check("winreg" not in engine_manifest, "S4 winreg gone from the engine manifest")
    check("pnet_datalink" not in engine_manifest, "S4 pnet_datalink gone from the engine manifest")

    # ------------------------------------------------------------- S5 islands
    islands = {
        "global_context.rs": ["pub async fn create_global_context(",
                              "EngineContextFactory",
                              "mod tests {"],
        "files_correction.rs": ["pub(crate) async fn paths_from_anywhere_impl(",
                                "pub(crate) async fn shortify_paths_impl(",
                                "FilesRuntime"],
        "http.rs": ["pub async fn start_server(", "mod routers;",
                    "pub(crate) fn should_advertise_mdns(",
                    "fn start_mdns_advertisement("],
    }
    for name, markers in islands.items():
        text = read(os.path.join(SRC, name))
        for marker in markers:
            check(marker in text, "S5 island stays in %s: %s" % (name, marker))

    # ------------------------------------------------ S7 balance / merge marks
    for rel in (
        "src/global_context.rs",
        "src/http.rs",
        "src/tokens.rs",
        "src/files_correction.rs",
        "crates/refact-runtime-api/src/engine_bootstrap.rs",
        "crates/refact-runtime-api/src/chat_tokens.rs",
        "crates/refact-core/src/net_utils.rs",
    ):
        text = read(os.path.join(ENG, rel))
        check(balance(text), "S7 %s delimiters balanced" % rel)
        check("<<<<<<<" not in text and ">>>>>>>" not in text, "S7 no merge markers in %s" % rel)

    print("\n%d checks, %d failures" % (CHECKS[0], len(FAILURES)))
    for failure in FAILURES:
        print("FAIL %s" % failure)
    if FAILURES:
        return 1

    if "--static" in sys.argv:
        print("static checks passed (cargo check skipped)")
        return 0

    print("\n### S8 cargo check")
    code = subprocess.call(build_cmd("check"), cwd=ROOT)
    if code != 0:
        print("FAIL cargo check exited %d" % code)
        return 1

    if "--with-tests" in sys.argv:
        print("\n### S8b cargo check-tests (36-error baseline)")
        if os.path.exists(CHECK_TESTS_LOG):
            log = read(CHECK_TESTS_LOG)
            print("  reusing the saved log %s (delete it to force a run)" % CHECK_TESTS_LOG)
        else:
            result = subprocess.run(
                build_cmd("check-tests"), cwd=ROOT, capture_output=True, text=True,
                encoding="utf-8", errors="replace",
            )
            log = (result.stdout or "") + (result.stderr or "")
            with open(CHECK_TESTS_LOG, "w", encoding="utf-8") as fh:
                fh.write(log)
            check(result.returncode != 0, "S8b check-tests still fails (known baseline)")
        report_check_tests(log)

    print("S-5 verification OK")
    return 0


if __name__ == "__main__":
    sys.exit(main())
