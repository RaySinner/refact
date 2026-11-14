"""Q-2 acceptance proof: the shell_gate <-> shell_gate_llm cycle is gone.

Read-only static checks; the sanctioned compile gate is run separately.

  1. the honest census still reports both shell modules, and neither names the
     other as a blocker (the mutual cycle is broken);
  2. the shared config type lives in `tools::shell_gate_common`, `shell_gate`
     re-exports it, and `shell_gate_llm` imports it from there;
  3. `shell_gate` owns no local copy of the moved items (single definition);
  4. the island's remaining dependencies are printed, split into crate deps and
     engine-resident blockers, using the census resolver itself.

Run:  powershell -NoProfile -ExecutionPolicy Bypass -File tools\\dev\\q2-check.ps1
"""
import pathlib
import re
import runpy
import sys

ROOT = pathlib.Path(__file__).resolve().parents[2]
ENGINE = ROOT / "refact-agent" / "engine"
TOOLS = ENGINE / "src" / "tools"
CENSUS = ENGINE / "target" / "tools_movable_census.txt"

SHARED_ITEMS = ("ShellLlmAuthority", "ShellLlmOnFailure", "ShellLlmValidation")
ISLAND = ("tools::shell_gate", "tools::shell_gate_llm", "tools::shell_gate_common")

failures = []


def check(name, ok, detail=""):
    if ok:
        print("PASS  %s" % name)
    else:
        print("FAIL  %s  %s" % (name, detail))
        failures.append(name)


def read(path):
    return path.read_text(encoding="utf-8", errors="replace")


(ENGINE / "target").mkdir(parents=True, exist_ok=True)
sys.argv = ["chat_movable_census.py", "tools"]
census_globals = runpy.run_path(str(ROOT / "tools" / "dev" / "chat_movable_census.py"))
info = census_globals["info"]
census = read(CENSUS)

rows = {}
current = None
for line in census.splitlines():
    m = re.match(r"^\s*(\d+)\s+(tools::\S+)\s*(.*)$", line)
    if m:
        current = m.group(2)
        rows[current] = m.group(3).strip()
    elif current and line.strip().startswith("others:"):
        rows[current] += " " + line.strip()

gate = rows.get("tools::shell_gate", "")
llm = rows.get("tools::shell_gate_llm", "")
common = rows.get("tools::shell_gate_common", "")

check("census lists tools::shell_gate", bool(gate), "row missing")
check("census lists tools::shell_gate_llm", bool(llm), "row missing")
check("census lists tools::shell_gate_common", bool(common), "row missing")
check(
    "shell_gate is no longer blocked by shell_gate_llm",
    "blocker=" not in gate,
    "row: %s" % gate,
)
check(
    "shell_gate_llm is no longer blocked by shell_gate",
    "blocker=" not in llm,
    "row: %s" % llm,
)

common_text = read(TOOLS / "shell_gate_common.rs")
gate_text = read(TOOLS / "shell_gate.rs")
llm_text = read(TOOLS / "shell_gate_llm.rs")
mod_text = read(TOOLS / "mod.rs")

for item in SHARED_ITEMS:
    check(
        "shell_gate_common defines %s" % item,
        bool(re.search(r"\b(?:pub\s+)?(?:struct|enum)\s+%s\b" % item, common_text)),
        "declaration missing",
    )
    check(
        "shell_gate does not define %s locally" % item,
        not re.search(r"\b(?:pub\s+)?(?:struct|enum)\s+%s\b" % item, gate_text),
        "duplicate definition",
    )

check(
    "shell_gate re-exports the shared items",
    bool(
        re.search(
            r"pub use crate::tools::shell_gate_common::\s*\{[^}]*ShellLlmValidation",
            gate_text,
        )
    ),
    "re-export missing",
)
check(
    "shell_gate_llm imports the shared type from shell_gate_common",
    "use crate::tools::shell_gate_common::ShellLlmValidation;" in llm_text,
    "import still points elsewhere",
)
check(
    "shell_gate_llm no longer imports from shell_gate",
    not re.search(r"\buse\s+crate::tools::shell_gate::", llm_text),
    "old edge still present",
)
check(
    "tools::mod registers shell_gate_common",
    "mod shell_gate_common;" in mod_text,
    "module not registered",
)

print("")
print("ISLAND RESOLVED EDGES (census resolver):")
engine_blockers = set()
for mod in ISLAND:
    entry = info[mod]
    print(
        "  %-28s crates=[%s] engine=[%s] blocked_by=[%s]"
        % (
            mod,
            ", ".join(sorted(entry["crates"])) or "-",
            ", ".join(sorted(entry["engine"])) or "-",
            ", ".join(sorted(entry["deps"])) or "-",
        )
    )
    engine_blockers |= entry["engine"]
    engine_blockers |= {d for d in entry["deps"] if d not in ISLAND}

print("")
print("ISLAND EXTERNAL CRATES: %s" % (", ".join(sorted(info["tools::shell_gate"]["crates"]
      | info["tools::shell_gate_llm"]["crates"] | info["tools::shell_gate_common"]["crates"])) or "-"))
print("ISLAND REMAINING ENGINE-RESIDENT BLOCKERS: %s" % (", ".join(sorted(engine_blockers)) or "-"))
check(
    "shell island has zero remaining engine-resident blockers",
    not engine_blockers,
    "blockers: %s" % ", ".join(sorted(engine_blockers)),
)
print("")
if failures:
    print("Q-2 HARNESS: FAILURES PRESENT (%d)" % len(failures))
    sys.exit(1)
print("Q-2 HARNESS: ALL CHECKS PASSED")
