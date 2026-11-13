"""N-4 path audit: every `refact_*::` path used by the moved crate files resolves.

Checks that each `refact_xxx::a::b` in the moved crate files maps to an existing
crate and that the module file exists (or is re-exported from lib.rs).
"""
import pathlib
import re

ROOT = pathlib.Path(__file__).resolve().parents[2]
ENG = ROOT / "refact-agent" / "engine"
CRATES = ENG / "crates"
DST = CRATES / "refact-tools" / "src" / "tools"

MOVED = [
    "tool_task_documents", "tool_task_memory", "subagent_phases",
    "tool_add_workspace_folder", "tool_create_knowledge", "tool_knowledge",
    "tool_activate_skill", "tool_config_subagent", "tool_cron_list", "tool_cron_delete",
]

tools_toml = (CRATES / "refact-tools" / "Cargo.toml").read_text(encoding="utf-8")
declared = set(re.findall(r"(?m)^(refact-[\w-]+) = ", tools_toml))
print("declared refact-* deps: %d" % len(declared))

problems = []
paths = {}
for mod in MOVED:
    text = (DST / f"{mod}.rs").read_text(encoding="utf-8")
    for m in re.finditer(r"\brefact_([a-z_]+)::([\w:]+)", text):
        crate = "refact-" + m.group(1).replace("_", "-")
        paths.setdefault(crate, set()).add(m.group(2))

for crate, subs in sorted(paths.items()):
    if crate not in declared:
        problems.append("%s NOT DECLARED in refact-tools/Cargo.toml" % crate)
        continue
    src = CRATES / crate / "src"
    if not src.is_dir():
        problems.append("%s has no src/ dir" % crate)
        continue
    lib = (src / "lib.rs").read_text(encoding="utf-8")
    for sub in sorted(subs):
        first = sub.split("::")[0]
        if re.search(r"(?m)^pub mod %s;" % re.escape(first), lib):
            continue
        if f"pub use" in lib and re.search(r"pub use [\w:]*\b%s\b" % re.escape(first), lib):
            continue
        if (src / (first + ".rs")).exists() or (src / first / "mod.rs").exists():
            continue
        if re.search(r"(?m)^\s*%s[,}]" % re.escape(first), lib):
            continue
        problems.append("%s::%s -> no `pub mod %s` in %s/src/lib.rs"
                        % (crate, sub, first, crate))

# crate-local crate::tools::* targets
for mod in MOVED:
    text = (DST / f"{mod}.rs").read_text(encoding="utf-8")
    for m in re.finditer(r"crate::tools::(\w+)", text):
        target = m.group(1)
        if not (DST / (target + ".rs")).exists() and not (DST / target).is_dir():
            problems.append("%s: crate::tools::%s does not exist in the crate" % (mod, target))

print("\nresolved crate roots: %d" % len(paths))
for crate in sorted(paths):
    print("  %-26s %s" % (crate, ", ".join(sorted(paths[crate]))))

print("\nPROBLEMS: %d" % len(problems))
for p in problems:
    print("  " + p)
