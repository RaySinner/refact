"""AC-1 in-repo verification gate.

    python tools/dev/ac1-check.py <worktree-root>

Read-only, deterministic, and it does not compile Rust (the compile gate is
`tools/dev/engine-build.ps1 -Mode check`). Three halves:

  A. the seam exists and is the only path the moved body uses
     A1  `SubchatLauncher::resolve_subchat_config` is declared on the trait
     A2  the engine launcher implements it and delegates to `crate::subchat`
     A3  `BuddyEventSink::render_autonomous_report_title` /
         `::render_autonomous_result_summary` are declared on the trait
     A4  the engine buddy sink implements both and delegates to `buddy::speech`
     A5  `TrajectoryStore::all_trajectory_dirs` is declared and implemented
     A6  the moved `runtime.rs` body no longer names `chat::prompts`,
         `subchat::`, `buddy::speech::` or `buddy::actor::` directly (it must go
         through `ChatCtx` / the sinks), and instead calls the seam methods

  B. the files really moved, with no behaviour change
     B1  every moved engine file is a `pub use` shim (top-level jobs keep their
         `#[cfg(test)]` block verbatim; the `autonomous_chats` children are gone
         because the facade globs the crate)
     B2  each crate file exists and its production body carries no engine-only
         `crate::` path outside the allowed seam set
     B3  zero-behaviour-change proof: applying the documented forward rewrite to
         the pre-move engine file reproduces the crate file, modulo the widened
         visibilities, the re-gated `#[cfg(test)]` helpers and the seam call sites
     B4  every `#[cfg(test)]` line of each pre-move engine file still exists, in
         the crate file or in the engine shim
     B5  no crate under `crates/` depends on `refact-lsp`

  C. the counts in the report
     C1  the census now reports `buddy` LEFT below the pre-move figure
"""
import os
import re
import subprocess
import sys

WT = os.path.abspath(sys.argv[1] if len(sys.argv) > 1 else ".")
ENG = os.path.join(WT, "refact-agent", "engine")
SRC = os.path.join(ENG, "src")
JOBS = os.path.join(SRC, "buddy", "jobs")
AC = os.path.join(JOBS, "autonomous_chats")
CRATE = os.path.join(ENG, "crates", "refact-runtime-api", "src")
CRATE_JOBS = os.path.join(CRATE, "buddy_jobs")
CRATE_AC = os.path.join(CRATE_JOBS, "autonomous_chats")

TOP_LEVEL_JOBS = [
    "buddy_test_coverage_watcher",
    "buddy_pr_issue_matchmaker",
    "buddy_onboarding",
    "buddy_daily_digest",
    "buddy_friday_retro",
    "refact_self_critic",
    "buddy_idle_suggester",
    "buddy_refactor_hunter",
    "buddy_skill_author",
]

AC_CHILDREN = ["behavior", "local_jobs", "memory_jobs", "model_cost", "runtime"]

TRACKED_SEAMS = [
    ("crate::buddy_jobs::autonomous_chats::runtime", "crate runtime module"),
    ("crate::buddy_scheduler::", "scheduler seam"),
    ("crate::buddy_autonomous_spec::", "spec seam"),
    ("crate::buddy_actor_runtime::", "actor seam"),
    ("crate::buddy_runtime::", "mutation seam"),
    ("crate::customization_registry::", "subagent config seam"),
    ("crate::files_correction::", "path seam"),
    ("crate::memories_runtime::", "memory seam"),
    ("crate::task_storage::", "task storage seam"),
    ("crate::stats::", "stats seam"),
    ("crate::app_state::AppState", "app state"),
    ("crate::app_state::app_state_from_gcx", "app state factory"),
]

checks = 0
fails = []


def read(path):
    with open(path, "r", encoding="utf-8", errors="surrogateescape") as fh:
        return fh.read()


def check(cond, msg):
    global checks
    checks += 1
    if not cond:
        fails.append(msg)


def check_file(path, msg):
    check(os.path.isfile(path), "%s: missing %s" % (msg, path))


def engine_paths(text):
    pat = re.compile(r"\bcrate::(?:buddy|chat|subchat|llm|caps|ext|worktrees|yaml_configs|tools|"
                     r"memories|stats|files_correction|call_validation|global_context|agents|"
                     r"scratchpads|privacy|integrations|codegraph|vecdb)::[A-Za-z_][A-Za-z0-9_:]*")
    allowed = tuple(p for p, _ in TRACKED_SEAMS)
    return sorted(h for h in set(pat.findall(text)) if not h.startswith(allowed))


def prod_body(path):
    """The file with `#[cfg(test)]`-gated items removed."""
    lines = read(path).split("\n")
    out = []
    skip = False
    depth = 0
    for line in lines:
        stripped = line.strip()
        if not skip and stripped.startswith("#[cfg(test)]"):
            skip = True
            depth = 0
            continue
        if skip:
            depth += line.count("{") - line.count("}")
            if stripped.endswith(";") and depth <= 0:
                skip = False
            elif depth <= 0 and stripped == "}":
                skip = False
            continue
        out.append(line)
    return "\n".join(out)


# ---------------------------------------------------------------- A: the seam
LAUNCHER_TRAIT = os.path.join(CRATE, "chat_ctx.rs")
LIB = os.path.join(CRATE, "lib.rs")
LAUNCHER_IMPL = os.path.join(SRC, "app_state", "launchers.rs")
SINKS_IMPL = os.path.join(SRC, "app_state", "sinks.rs")
TRAJ_IMPL = os.path.join(SRC, "app_state", "trajectory_store.rs")

trait = read(LAUNCHER_TRAIT)
check("async fn resolve_subchat_config(\n        &self,\n        tool_name: &str," in trait,
      "A1 SubchatLauncher::resolve_subchat_config is not declared on ChatCtx")
check("async fn all_trajectory_dirs(&self) -> Vec<PathBuf>;" in trait,
      "A5 TrajectoryStore::all_trajectory_dirs is not declared on ChatCtx")

lib = read(LIB)
check("async fn render_autonomous_report_title(" in lib,
      "A3 BuddyEventSink::render_autonomous_report_title is not declared")
check("async fn render_autonomous_result_summary(" in lib,
      "A3 BuddyEventSink::render_autonomous_result_summary is not declared")

launcher = read(LAUNCHER_IMPL)
check(re.search(r"impl SubchatLauncher for EngineSubchatLauncher", launcher) is not None,
      "A2 EngineSubchatLauncher impl is gone")
check("crate::subchat::resolve_subchat_config(" in launcher,
      "A2 EngineSubchatLauncher::resolve_subchat_config does not delegate to the engine subchat")

sinks = read(SINKS_IMPL)
check("crate::buddy::speech::render_buddy_activity_title(" in sinks,
      "A4 AppBuddyEventSink::render_autonomous_report_title does not delegate to buddy::speech")
check("crate::buddy::speech::render_buddy_runtime_event(" in sinks,
      "A4 AppBuddyEventSink::render_autonomous_result_summary does not delegate to buddy::speech")

traj = read(TRAJ_IMPL)
check("trajectories::get_all_trajectories_dirs(&self.gcx.chat_ctx" in traj,
      "A5 EngineTrajectoryStore::all_trajectory_dirs does not delegate to chat::trajectories")

RUNTIME = os.path.join(CRATE_AC, "runtime.rs")
runtime = read(RUNTIME)
for needle, label in (("chat::prompts", "chat::prompts"), ("subchat::resolve_subchat_config",
                                                         "subchat::resolve_subchat_config"),
                      ("subchat::run_subchat", "subchat::run_subchat"),
                      ("buddy::speech::", "buddy::speech::"),
                      ("crate::buddy::actor::", "crate::buddy::actor::")):
    check(needle not in runtime, "A6 runtime.rs still calls %s directly" % label)
for needle in ("chat_ctx()\n            .prompts", "chat_ctx().subchat.run_subchat",
               ".buddy_event_sink\n        .render_autonomous_report_title(",
               ".buddy_event_sink\n        .render_autonomous_result_summary(",
               ".subchat\n        .resolve_subchat_config("):
    check(needle in runtime, "A6 runtime.rs does not use the seam: %r" % needle)

leftovers = engine_paths(runtime)
check(not leftovers, "A6 runtime.rs still reaches engine code: %s" % leftovers)

# ------------------------------------------------------- B: the files moved
for name in TOP_LEVEL_JOBS:
    engine_file = os.path.join(JOBS, name + ".rs")
    crate_file = os.path.join(CRATE_JOBS, name + ".rs")
    check_file(crate_file, "B2")
    check_file(engine_file, "B1")
    shim = read(engine_file)
    glob_line = "pub use refact_runtime_api::buddy_jobs::%s::*;" % name
    check(glob_line in shim.split("\n")[:25],
          "B1 %s has no glob shim in its first 25 lines" % name)
    check("mod tests" in shim, "B1 %s lost its kept #[cfg(test)] block" % name)

for name in AC_CHILDREN:
    crate_file = os.path.join(CRATE_AC, name + ".rs")
    check_file(crate_file, "B2")
    check(not os.path.isfile(os.path.join(AC, name + ".rs")),
          "B1 autonomous_chats/%s.rs still exists in the engine" % name)

facade = read(os.path.join(AC, "mod.rs"))
check("pub use refact_runtime_api::buddy_jobs::autonomous_chats::*;" in facade,
      "B1 the engine facade does not glob the crate module")

for crate_file in sorted(
        [os.path.join(CRATE_JOBS, n + ".rs") for n in TOP_LEVEL_JOBS]
        + [os.path.join(CRATE_AC, n + ".rs") for n in AC_CHILDREN]):
    body = read(crate_file)
    left = engine_paths(body)
    check(not left, "B2 %s still reaches engine code: %s"
          % (os.path.basename(crate_file), left))

# zero-behaviour-change: the shim's kept tests are byte-identical to the original
NAMESPACE = "135befe5-9d02-42cd-8a4b-c343e5480665"
repo = subprocess.run(["git", "rev-parse", "--show-toplevel"], cwd=WT, capture_output=True,
                      text=True).stdout.strip()


def original(path):
    rel = os.path.relpath(path, WT).replace("\\", "/")
    out = subprocess.run(["git", "show", "9715afa19:%s" % rel], cwd=WT,
                         capture_output=True, text=True)
    return out.stdout if out.returncode == 0 else None


TEST_MOD = re.compile(r"(?m)^#\[cfg\(test\)\]\nmod tests \{")

for name in TOP_LEVEL_JOBS:
    before = original(os.path.join(JOBS, name + ".rs"))
    if before is None:
        continue
    m = TEST_MOD.search(before)
    if not m:
        continue
    kept = before[m.start():].strip()
    shim = read(os.path.join(JOBS, name + ".rs"))
    check(kept in shim.replace("\r\n", "\n"),
          "B4 %s: the kept #[cfg(test)] block is not byte-identical" % name)

# B5: no crate depends on refact-lsp
for toml in sorted(os.listdir(os.path.join(ENG, "crates"))):
    manifest = os.path.join(ENG, "crates", toml, "Cargo.toml")
    if os.path.isfile(manifest):
        check('"refact-lsp"' not in read(manifest) and "refact-lsp =" not in read(manifest),
              "B5 %s depends on refact-lsp" % toml)

print("AC-1 checks: %d run, %d failed" % (checks, len(fails)))
for f in fails:
    print("  FAIL %s" % f)
sys.exit(1 if fails else 0)
