"""P-4 verification harness: static proof the buddy observer harvest is behaviour-preserving.

No build is attempted here (the project build guard refuses compilation from agent
worktrees; the planner owns `tools/dev/engine-build.ps1`). Every assertion is a
filesystem / text check with no shell separators.

Run: python tools/dev/p4-check.py
"""
import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parents[2]
ENG = ROOT / "refact-agent" / "engine"
SRC = ENG / "src"
BUDDY = SRC / "buddy"
CRATE = ENG / "crates" / "refact-buddy-core"

checks = []


def check(name, ok, detail=""):
    checks.append((name, bool(ok), detail))


def read(path):
    return pathlib.Path(path).read_text(encoding="utf-8", errors="replace")


# --- 1. crate modules exist and own the moved pure logic ---------------------
MOVED = {
    "observers/memory_garden.rs": [
        "pub const MAX_PROPOSALS_PER_TICK",
        "pub const MAX_PROPOSALS_PER_DAY",
        "pub struct KnowledgeEntry",
        "pub struct MemoryGardenDailyCounter",
        "pub async fn scan_knowledge_dirs_from_paths(",
        "pub fn memory_garden_facts_from_entries_with_references(",
        "pub fn transition_memory_garden_facts(",
        "pub fn scan_knowledge_references(",
    ],
    "observers/git_pressure.rs": [
        "pub const MAX_UNCOMMITTED_STATUS_SCAN",
        "pub const MAX_DIFF_COMMITS",
        "pub fn count_uncommitted(",
        "pub fn count_uncommitted_in_repo(",
        "pub fn git_diff_widening(",
        "pub fn detect_git_pressure_facts(",
    ],
    "observers/worktree_hygiene.rs": ["pub fn detect_worktree_hygiene_facts("],
}

for rel, symbols in MOVED.items():
    p = CRATE / "src" / rel
    body = read(p) if p.exists() else ""
    check("crate module %s exists" % rel, p.exists(), "")
    for symbol in symbols:
        check("crate %s defines `%s`" % (rel, symbol), symbol in body)

# the crate keeps the moved inline tests verbatim
check(
    "crate observers/git_pressure.rs keeps its inline test",
    "fn uncommitted_status_scan_does_not_recurse_into_untracked_dirs()" in read(CRATE / "src/observers/git_pressure.rs"),
)
memory_garden = read(CRATE / "src/observers/memory_garden.rs")
for name in [
    "detects_untagged_use_avoid_title_conflict",
    "detects_do_not_use_before_positive_do_prefix",
    "detects_do_not_before_positive_do_prefix",
    "knowledge_candidate_collection_is_bounded_and_recent_biased",
    "rotating_chunks_cover_full_corpus_and_keep_newest",
    "knowledge_candidate_scan_stops_at_visit_budget",
    "orphan_fact_is_partial_when_based_on_bounded_subset",
    "older_frontmatter_reference_prevents_recent_orphan",
    "malformed_older_frontmatter_reference_pass_is_skipped_safely",
    "memory_garden_observer_truncates_to_per_tick_cap",
    "memory_garden_observer_enforces_per_day_cap",
    "memory_garden_transition_suppresses_same_day_and_emits_after_day_rollover",
]:
    check("crate memory_garden keeps test fn %s" % name, ("fn %s()" % name) in memory_garden)

lib_rs = read(CRATE / "src/lib.rs")
check("buddy-core lib.rs still declares `pub mod observers;`", "pub mod observers;" in lib_rs)
observers_mod = read(CRATE / "src/observers/mod.rs")
for module in ["memory_garden", "git_pressure", "worktree_hygiene"]:
    check("crate observers/mod.rs declares `pub mod %s;`" % module, ("pub mod %s;" % module) in observers_mod)

# --- 2. engine files are shims that keep the AppState-bound half -------------
engine_memory_garden = read(BUDDY / "observers/memory_garden.rs")
check(
    "engine memory_garden.rs re-exports the crate module",
    "pub use refact_buddy_core::observers::memory_garden::*;" in engine_memory_garden,
)
check(
    "engine memory_garden.rs keeps the BuddyObserver impl",
    "impl BuddyObserver for MemoryGardenObserver" in engine_memory_garden,
)
for gone in [
    "fn select_rotating_chunk",
    "fn normalized_negation_subject",
    "fn tags_hash",
    "fn hourly_rotation",
    "fn read_frontmatter_only",
    "fn memory_garden_fact_set_fingerprint",
]:
    check("engine memory_garden.rs no longer defines %s" % gone, gone not in engine_memory_garden)
check(
    "engine memory_garden.rs keeps the cfg(test) scan helper the engine tests call",
    "pub(crate) async fn scan_knowledge_dir_count_for_test(" in engine_memory_garden,
)

engine_git = read(BUDDY / "observers/git_pressure.rs")
check(
    "engine git_pressure.rs re-exports the crate module",
    "pub use refact_buddy_core::observers::git_pressure::*;" in engine_git,
)
check(
    "engine git_pressure.rs keeps detect_and_enqueue_git_memory_ops (AppState/storage bound)",
    "pub async fn detect_and_enqueue_git_memory_ops(" in engine_git,
)
check(
    "engine git_pressure.rs keeps the BuddyObserver impl",
    "impl BuddyObserver for GitPressureObserver" in engine_git,
)
for gone in ["fn count_uncommitted_in_repo", "fn git_diff_widening", "fn path_hash"]:
    check("engine git_pressure.rs no longer defines %s" % gone, gone not in engine_git)

engine_hygiene = read(BUDDY / "observers/worktree_hygiene.rs")
check(
    "engine worktree_hygiene.rs re-exports the crate module",
    "pub use refact_buddy_core::observers::worktree_hygiene::*;" in engine_hygiene,
)
check(
    "engine worktree_hygiene.rs no longer defines detect_worktree_hygiene_facts",
    "fn detect_worktree_hygiene_facts" not in engine_hygiene,
)
check(
    "engine worktree_hygiene.rs keeps the BuddyObserver impl",
    "impl BuddyObserver for WorktreeHygieneObserver" in engine_hygiene,
)

# --- 3. engine call sites still resolve through the same paths ---------------
pulse = read(BUDDY / "pulse.rs")
check(
    "buddy/pulse.rs still reads git_pressure::count_uncommitted_in_repo",
    "crate::buddy::observers::git_pressure::count_uncommitted_in_repo(&repo)" in pulse,
)
check(
    "buddy/pulse.rs still reads git_pressure::MAX_DIFF_COMMITS",
    "crate::buddy::observers::git_pressure::MAX_DIFF_COMMITS" in pulse,
)

# --- 4. no crate -> engine dependency (the hazard this card guards) ----------
cargo = read(CRATE / "Cargo.toml")
check("refact-buddy-core Cargo.toml has no `refact-lsp` dependency", "refact-lsp" not in cargo)
for crate_rel in MOVED:
    body = read(CRATE / "src" / crate_rel)
    stray = sorted(
        {m for m in re.findall(r"crate::[\w:]*", body) if not m.startswith("crate::types")}
    )
    check("crate %s has no engine `crate::` path" % crate_rel, not stray, str(stray))
    check("crate %s names no refact_lsp path" % crate_rel, "refact_lsp" not in body)

cycles = []
for crate_dir in (ENG / "crates").iterdir():
    toml_path = crate_dir / "Cargo.toml"
    if not toml_path.exists():
        continue
    if re.search(r'^\s*refact-lsp\s*=', read(toml_path), re.M):
        cycles.append(crate_dir.name)
check("no engine crate depends back on refact-lsp", not cycles, ",".join(cycles))

# --- 5. report the module masses -------------------------------------------
def prod_lines(text):
    lines = text.splitlines()
    count = 0
    index = 0
    while index < len(lines):
        line = lines[index]
        if line.startswith("#[cfg(test)]"):
            index += 1
            depth = 0
            pending = True
            while index < len(lines) and pending:
                body = lines[index]
                depth += body.count("{") - body.count("}")
                if depth > 0:
                    pending = False
                elif body.strip().endswith(";"):
                    pending = False
                index += 1
            while index < len(lines) and depth > 0:
                depth += lines[index].count("{") - lines[index].count("}")
                index += 1
            continue
        count += 1
        index += 1
    return count


print("engine shims after the move (production lines):")
for rel in ["observers/memory_garden.rs", "observers/git_pressure.rs", "observers/worktree_hygiene.rs"]:
    text = read(BUDDY / rel)
    print("  %-34s %4d prod lines, %4d total" % (rel, prod_lines(text), len(text.splitlines())))
print("crate modules carrying the moved bodies:")
for rel in MOVED:
    text = read(CRATE / "src" / rel)
    print("  %-34s %4d prod lines, %4d total" % (rel, prod_lines(text), len(text.splitlines())))

failed = [c for c in checks if not c[1]]
for name, ok, detail in checks:
    print(("PASS  " if ok else "FAIL  ") + name + (("  [%s]" % detail) if detail and not ok else ""))
print("")
print("%d/%d checks passed" % (len(checks) - len(failed), len(checks)))
sys.exit(1 if failed else 0)
