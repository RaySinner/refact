"""S-6 verification harness: static proof that the engine-root tail move is behaviour-preserving.

No build is attempted here (the project build guard refuses compilation from agent
worktrees). Every assertion is a filesystem/text check with no shell separators.

Run: python tools/dev/s6-check.py
"""
import pathlib
import re
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parents[2]
ENG = ROOT / "refact-agent" / "engine"
SRC = ENG / "src"
CRATES = ENG / "crates"

# Parent commit of the S-6 card work (batch R head).
BASE = "58fbd4aac"

# engine file -> (destination crate dir, destination crate-relative file)
MOVED = [
    ("fuzzy_search.rs", "refact-files", "fuzzy_search.rs"),
    ("cache_maintenance.rs", "refact-daemon", "cache_maintenance.rs"),
    ("nicer_logs.rs", "refact-daemon", "nicer_logs.rs"),
    ("parent_stdin.rs", "refact-core", "parent_stdin.rs"),
    ("indexing_utils.rs", "refact-http", "indexing_utils.rs"),
    ("privacy.rs", "refact-runtime-api", "privacy.rs"),
]

# functions that must exist, prod-verbatim, in the destination crate file
VERBATIM_FNS = {
    "fuzzy_search.rs": ["fuzzy_search"],
    "cache_maintenance.rs": ["sweep_logs_dir", "log_file_date_age", "sweep_logs_dir_blocking"],
    "parent_stdin.rs": ["watch_parent_stdin_for_eof"],
    "indexing_utils.rs": ["wait_for_indexing_if_needed"],
    "privacy.rs": [
        "global_privacy_path",
        "global_only_policy",
        "save_privacy_policy",
        "backup_corrupt_privacy_file",
        "write_privacy_file",
    ],
}

# engine modules that must stay islands (never become shims)
ISLANDS = ["files_in_jsonl.rs"]

SHIMS = {
    "fuzzy_search.rs": "pub use refact_files::fuzzy_search::fuzzy_search;\n",
    "cache_maintenance.rs": "pub use refact_daemon::cache_maintenance::*;\n",
    "indexing_utils.rs": "pub use refact_http::indexing_utils::wait_for_indexing_if_needed;\n",
}

# tests that must have travelled with their module
MOVED_TESTS = {
    "refact-daemon/cache_maintenance.rs": ["sweep_only_removes_eligible_log_files"],
    "refact-core/parent_stdin.rs": [
        "eof_triggers_on_eof_callback",
        "open_pipe_does_not_trigger_eof",
        "read_error_triggers_eof_callback",
    ],
}

checks = []


def check(name, ok, detail=""):
    checks.append((name, ok))
    print(("PASS " if ok else "FAIL ") + name + ((" -- " + detail) if detail and not ok else ""))


def read(p):
    return p.read_text(encoding="utf-8")


def at_base(rel):
    raw = subprocess.run(
        ["git", "-C", str(ENG), "show", "%s:refact-agent/engine/%s" % (BASE, rel)],
        capture_output=True, check=True).stdout.decode("utf-8")
    return raw.replace("\r\n", "\n")


def git(*args):
    return subprocess.run(["git", "-C", str(ROOT), *args],
                          capture_output=True, text=True, check=True).stdout.strip()


def fn_body(text, name):
    lines = text.split("\n")
    start = None
    for i, line in enumerate(lines):
        if re.match(r"^(pub )?(async )?fn %s\b" % re.escape(name), line):
            start = i
            break
    if start is None:
        return None
    for j in range(start, len(lines)):
        if lines[j] == "}":
            return "\n".join(lines[start:j + 1])
    return None


def prod_lines(text):
    lines = text.split("\n")
    for i, line in enumerate(lines):
        if line.strip() == "#[cfg(test)]":
            return lines[:i]
    return lines


def count_prod(text):
    return len(prod_lines(text))


# 1. every moved function body is prod-verbatim in the destination crate
for eng_rel, crate, crate_rel in MOVED:
    dest = CRATES / crate / "src" / crate_rel
    if not dest.exists():
        check("crate %s/src/%s exists" % (crate, crate_rel), False)
        continue
    check("crate %s/src/%s exists" % (crate, crate_rel), True)
    base = at_base("src/" + eng_rel)
    body = read(dest)
    for name in VERBATIM_FNS.get(eng_rel, []):
        expected = fn_body(base, name)
        got = fn_body(body, name)
        check("%s::%s body is prod-verbatim" % (crate_rel, name),
              expected is not None and expected == got,
              "expected %r got %r" % (
                  (expected or "")[:120], (got or "")[:120]))

# nicer_logs has no top-level fn of its own besides impls: compare the whole
# prod body with `use` lines and the refact-core re-export stripped
base = at_base("src/nicer_logs.rs")
dest = CRATES / "refact-daemon" / "src" / "nicer_logs.rs"


def strip_imports(text):
    keep = [line for line in text.split("\n")
            if not line.startswith("use ") and not line.startswith("pub use ")]
    return "\n".join(line for line in keep if line.strip())


check("refact-daemon nicer_logs prod body is base-verbatim modulo imports",
      strip_imports("\n".join(prod_lines(base))) == strip_imports(read(dest)))

# 2. the engine files are pure re-export shims (except the islands)
for eng_rel, crate, crate_rel in MOVED:
    if eng_rel in ISLANDS:
        continue
    shim = read(SRC / eng_rel)
    if eng_rel == "nicer_logs.rs":
        expected_prefix = "pub use refact_core::nicer_logs::{first_n_chars, human_readable_bytes, last_n_chars};\n"
        check("engine nicer_logs.rs re-exports the refact-core helpers",
              shim.startswith(expected_prefix), shim.split("\n")[0])
        check("engine nicer_logs.rs re-exports CustomLayer from refact-daemon",
              "pub use refact_daemon::nicer_logs::CustomLayer;" in shim)
    elif eng_rel == "privacy.rs":
        for item in ("pub use refact_runtime_api::privacy::{",
                     "global_only_policy",
                     "save_privacy_policy",
                     "load_privacy_if_needed",
                     "load_privacy_if_needed_for_policy"):
            check("engine privacy.rs re-exports %s" % item, item in shim)
        check("engine privacy.rs keeps the warn_observation_degraded_once island",
              "pub async fn warn_observation_degraded_once" in shim)
        check("engine privacy.rs keeps its submodule table",
              "mod gcx_wrappers;" in shim and "pub mod records;" in shim
              and "pub mod destinations;" in shim and "pub mod heuristic;" in shim)
    elif eng_rel == "parent_stdin.rs":
        check("engine parent_stdin.rs re-exports watch_parent_stdin_for_eof from refact-core",
              "pub use refact_core::parent_stdin::watch_parent_stdin_for_eof;" in shim)
        check("engine parent_stdin.rs keeps run_parent_stdin_watch (island)",
              "pub async fn run_parent_stdin_watch" in shim)
        check("engine parent_stdin.rs keeps spawn_parent_stdin_watch_if_requested (island)",
              "pub fn spawn_parent_stdin_watch_if_requested" in shim)
        check("engine parent_stdin.rs kept no moved test module",
              "#[cfg(test)]" not in shim)
    else:
        expected = SHIMS[eng_rel]
        check("engine %s is exactly the re-export shim" % eng_rel, shim == expected, repr(shim))

# 3. the islands are byte-identical to BASE
for rel in ISLANDS:
    check("engine %s is unchanged vs BASE (permanent island)" % rel,
          read(SRC / rel) == at_base("src/" + rel))

# 4. destination crate modules are declared in their lib.rs
for eng_rel, crate, crate_rel in MOVED:
    lib = read(CRATES / crate / "src" / "lib.rs")
    mod_name = crate_rel[:-3]
    if crate == "refact-runtime-api":
        check("refact-runtime-api declares privacy (pre-existing module keeps the additions)",
              "pub mod privacy;" in lib)
    else:
        check("%s declares pub mod %s" % (crate, mod_name), "pub mod %s;" % mod_name in lib)

# 5. engine lib.rs still declares every engine module
lib = read(SRC / "lib.rs")
for eng_rel, _, _ in MOVED:
    mod_name = eng_rel[:-3]
    check("engine lib.rs still declares %s" % mod_name, "pub mod %s;" % mod_name in lib)

# 6. tests travelled with their module and left the engine shim
for key, names in MOVED_TESTS.items():
    crate, crate_rel = key.split("/")
    body = read(CRATES / crate / "src" / crate_rel)
    check("%s/src/%s has a #[cfg(test)] module" % (crate, crate_rel), "#[cfg(test)]" in body)
    for name in names:
        check("%s::%s test travelled" % (crate_rel, name), name in body)

# 7. no engine-only path escaped into a destination crate, and each crate:: ref resolves
for eng_rel, crate, crate_rel in MOVED:
    body = read(CRATES / crate / "src" / crate_rel)
    crate_root = CRATES / crate / "src"
    imported = set(re.findall(r"use crate::(\w+)", body))
    lib_text = read(crate_root / "lib.rs")
    for reference in sorted(set(re.findall(r"crate::[\w:]+", body))):
        parts = reference.split("::")[1:]
        if parts[0] in imported and len(parts) == 1:
            check("%s::%s is a crate-root import used by %s" % (crate, reference, crate_rel), True)
            continue
        if re.search(r"pub use [\w:]*\b%s\b" % re.escape(parts[0]), lib_text):
            check("%s::%s is a crate-root re-export of %s" % (crate, reference, crate), True)
            continue
        resolved = False
        while parts:
            head = crate_root / (parts[0] + ".rs")
            deep = crate_root.joinpath(*parts)
            if head.exists() or deep.exists() or (crate_root / parts[0]).is_dir():
                resolved = True
                break
            parts = parts[:-1]
        check("%s::%s resolves inside %s" % (crate, reference, crate),
              resolved, str(crate_root))
    check("%s/src/%s has no refact_lsp / engine global_context::tests ref" % (crate, crate_rel),
          "refact_lsp" not in body and "global_context::tests" not in body)
    check("%s/src/%s introduced no include_str!/include_bytes!" % (crate, crate_rel),
          "include_str!" not in body and "include_bytes!" not in body)

# 8. manifests: no crate -> engine dependency, engine -> crate dependency intact
engine_toml = read(ENG / "Cargo.toml")
for eng_rel, crate, crate_rel in MOVED:
    manifest = read(CRATES / crate / "Cargo.toml")
    check("%s manifest never depends on the engine binary" % crate, "refact-lsp" not in manifest)
    check("engine [dependencies] keeps %s" % crate,
          ('%s = { path = "crates/%s"' % (crate, crate)) in engine_toml)

# 9. only the files this card owns were touched
expected_touched = {
    "refact-agent/engine/src/fuzzy_search.rs",
    "refact-agent/engine/src/cache_maintenance.rs",
    "refact-agent/engine/src/nicer_logs.rs",
    "refact-agent/engine/src/parent_stdin.rs",
    "refact-agent/engine/src/indexing_utils.rs",
    "refact-agent/engine/src/privacy.rs",
    "refact-agent/engine/src/files_in_jsonl.rs",
    "refact-agent/engine/crates/refact-core/src/lib.rs",
    "refact-agent/engine/crates/refact-core/src/parent_stdin.rs",
    "refact-agent/engine/crates/refact-daemon/src/lib.rs",
    "refact-agent/engine/crates/refact-daemon/src/cache_maintenance.rs",
    "refact-agent/engine/crates/refact-daemon/src/nicer_logs.rs",
    "refact-agent/engine/crates/refact-files/src/lib.rs",
    "refact-agent/engine/crates/refact-files/src/fuzzy_search.rs",
    "refact-agent/engine/crates/refact-http/src/lib.rs",
    "refact-agent/engine/crates/refact-http/src/indexing_utils.rs",
    "refact-agent/engine/crates/refact-runtime-api/src/privacy.rs",
    "tools/dev/s6-check.py",
}
touched = {line for line in git("diff", "--name-only", BASE, "HEAD").split("\n") if line}
extra = sorted(touched - expected_touched)
check("no unrelated file touched", not extra, str(extra))

# 10. prod line counts before/after, printed for the report
print("\nprod line counts (BASE -> worktree):")
for eng_rel, crate, crate_rel in MOVED:
    before = count_prod(at_base("src/" + eng_rel))
    after = count_prod(read(SRC / eng_rel))
    dest = CRATES / crate / "src" / crate_rel
    dest_prod = count_prod(read(dest)) if dest.exists() else 0
    print("  %-24s engine %4d -> %3d   %s/src/%s %4d" % (eng_rel, before, after, crate, crate_rel, dest_prod))

failed = [c for c in checks if not c[1]]
print("\n%d/%d checks passed" % (len(checks) - len(failed), len(checks)))
if failed:
    print("RESULT: FAILED")
    sys.exit(1)
print("RESULT: ALL CHECKS PASSED")
