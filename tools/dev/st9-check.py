"""ST-9 self-check: the command-queue-starter seam exists, is wired, and no chat file moved.

The engine build guard refuses agent-side compiles, so this is the layer-A proof:
it reads the sources, the cargo manifests, and the movability census, and asserts
the seam's shape instead of trusting a transcript.

    python tools/dev/st9-check.py
"""
import os
import re
import subprocess
import sys

sys.stdout.reconfigure(encoding="utf-8")

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
ENGINE = "refact-agent/engine"
CRATE_MOD = f"{ENGINE}/crates/refact-runtime-api/src/command_queue_start.rs"
LIB = f"{ENGINE}/crates/refact-runtime-api/src/lib.rs"
CHAT_CTX = f"{ENGINE}/crates/refact-runtime-api/src/chat_ctx.rs"
CRATE_CARGO = f"{ENGINE}/crates/refact-runtime-api/Cargo.toml"
ENGINE_IMPL = f"{ENGINE}/src/app_state/queue.rs"
APP_STATE = f"{ENGINE}/src/app_state.rs"
DISPATCH = f"{ENGINE}/src/chat/queue/dispatch.rs"
DELIVERY = f"{ENGINE}/src/chat/delivery.rs"
GOAL_MONITOR = f"{ENGINE}/src/chat/goal_monitor.rs"

# Base commit of this card: comparisons must pin it, not HEAD (the card commits its own work).
CARD_BASE = "e2918b15d"

failures = []


def check(name, ok, detail=""):
    print(f"{'PASS' if ok else 'FAIL'}  {name}" + (f"  [{detail}]" if detail and not ok else ""))
    if not ok:
        failures.append(name)


def read(path):
    with open(path, encoding="utf-8-sig") as handle:
        return handle.read()


def git(*args):
    out = subprocess.run(
        ["git", *args],
        cwd=ROOT,
        capture_output=True,
        text=True,
        encoding="utf-8",
        errors="replace",
    )
    return out.stdout


# C1 — the crate module declares the trait and the free function.
crate_src = read(CRATE_MOD)
check(
    "C1 trait CommandQueueStarter declared",
    "pub trait CommandQueueStarter: Send + Sync {" in crate_src,
)
FUTURE = r"std::pin::Pin<Box<dyn std::future::Future<Output = \(\)> \+ Send>>"
check(
    "C1 trait method takes app, session_arc, processor_running",
    re.search(
        r"fn process_command_queue\(\s*&self,\s*app: AppState,\s*"
        r"session_arc: Arc<AMutex<ChatSession>>,\s*"
        rf"processor_running: Arc<AtomicBool>,\s*\)\s*->\s*{FUTURE};",
        crate_src,
    )
    is not None,
)
check(
    "C1 free fn process_command_queue dispatches through ChatCtx",
    re.search(
        r"pub fn process_command_queue\(\s*app: AppState,\s*"
        r"session_arc: Arc<AMutex<ChatSession>>,\s*"
        rf"processor_running: Arc<AtomicBool>,\s*\)\s*->\s*{FUTURE} \{{\s*"
        r"let starter = app\.ctx\.queue_processor\.clone\(\);",
        crate_src,
    )
    is not None,
)

# C2 — the module is registered and the field is on ChatCtx.
check("C2 lib.rs registers pub mod command_queue_start", "pub mod command_queue_start;" in read(LIB))
chat_ctx_src = read(CHAT_CTX)
check(
    "C2 ChatCtx carries the queue_processor field",
    "pub queue_processor: Arc<dyn crate::command_queue_start::CommandQueueStarter>," in chat_ctx_src,
)
check(
    "C2 queue_processor sits next to generation",
    re.search(
        r"pub generation: Arc<dyn crate::generation_start::GenerationStarter>,\s*"
        r"pub queue_processor: Arc<dyn crate::command_queue_start::CommandQueueStarter>,",
        chat_ctx_src,
    )
    is not None,
)

# C3 — the engine implements the trait by delegating to the byte-identical engine fn.
impl_src = read(ENGINE_IMPL)
check(
    "C3 engine implements CommandQueueStarter",
    "impl refact_runtime_api::command_queue_start::CommandQueueStarter for "
    "EngineCommandQueueStarter" in impl_src,
)
check(
    "C3 impl delegates to chat::process_command_queue",
    "crate::chat::process_command_queue(app, session_arc, processor_running)" in impl_src,
)

# C4 — the engine installs it in the one ChatCtx literal.
app_state_src = read(APP_STATE)
check("C4 app_state declares the queue module", "mod queue;" in app_state_src)
check("C4 app_state re-exports the queue module", "pub use self::queue::*;" in app_state_src)
check(
    "C4 ChatCtx literal installs EngineCommandQueueStarter",
    "queue_processor: Arc::new(EngineCommandQueueStarter)," in app_state_src,
)

# C5 — the engine function the impl delegates to is public for the shim.
check(
    "C5 dispatch::process_command_queue is pub with the seam's return type",
    re.search(
        r"pub fn process_command_queue\(\s*app: AppState,\s*"
        r"session_arc: Arc<AMutex<ChatSession>>,\s*"
        r"processor_running: Arc<AtomicBool>,\s*\)\s*->\s*"
        r"std::pin::Pin<Box<dyn std::future::Future<Output = \(\)> \+ Send>>",
        read(DISPATCH),
    )
    is not None,
)

# C6 — the three call sites bind the crate function, not the engine module.
for path, label in ((DELIVERY, "delivery"), (GOAL_MONITOR, "goal_monitor")):
    src = read(path)
    check(
        f"C6 {label} imports the crate seam",
        "use refact_runtime_api::command_queue_start::process_command_queue;" in src,
    )
    check(
        f"C6 {label} no longer imports the engine queue module",
        "use crate::chat::queue::process_command_queue;" not in src
        and "use crate::chat::process_command_queue;" not in src,
    )
    check(
        f"C6 {label} keeps tokio::spawn around the seam call",
        "tokio::spawn(process_command_queue(" in src,
    )

check(
    "C6 delivery keeps both spawn sites",
    read(DELIVERY).count("tokio::spawn(process_command_queue(") == 2,
)
check(
    "C6 goal_monitor keeps its single spawn site",
    read(GOAL_MONITOR).count("tokio::spawn(process_command_queue(") == 1,
)

# C7 — no chat file moved: under src/chat only the two call-site files changed.
chat_diff = sorted(
    line.strip()
    for line in git("diff", "--name-only", CARD_BASE, "--", f"{ENGINE}/src/chat/").splitlines()
    if line.strip()
)
check(
    "C7 only chat/delivery.rs + chat/goal_monitor.rs touched under src/chat",
    chat_diff == sorted([DELIVERY, GOAL_MONITOR]),
    ", ".join(chat_diff),
)

# C8 — no crate -> engine dependency, and no engine backrefs in the crate module.
manifest = read(CRATE_CARGO)
check("C8 refact-runtime-api does not depend on refact-lsp", "refact-lsp" not in manifest)
bad = [
    line
    for line in crate_src.splitlines()
    if re.search(r"\bcrate::(chat|subchat|tools|queue|app_state::queue)\b", line)
]
check("C8 crate module has no crate::<engine module> backrefs", not bad, "; ".join(bad)[:200])

# C9 — the census reclassifies both card files as no longer LEFT.
census = subprocess.run(
    [sys.executable, os.path.join(ROOT, "tools", "dev", "chat_movable_census.py"), "chat"],
    cwd=ROOT,
    capture_output=True,
    text=True,
    encoding="utf-8",
    errors="replace",
)
check("C9 census exits 0", census.returncode == 0, (census.stderr or "")[-300:])
report = os.path.join(ROOT, ENGINE, "target", "chat_movable_census.txt")
if os.path.isfile(report):
    text = read(report)
    movable = text.split("### BLOCKED ON ANOTHER ISLAND")[0]
    left = text.split("### LEFT (blocked on engine code)")[1].split("### ALREADY-SHIM")[0]

    def listed(section, module):
        """A census entry line starts with the line count and then the module name."""
        return re.search(rf"(?m)^\s*\d+\s+{re.escape(module)}(\s|$)", section) is not None

    for module in ("chat::delivery", "chat::goal_monitor"):
        check(f"C9 {module} is MOVABLE now", listed(movable, module))
        check(f"C9 {module} is no longer LEFT", not listed(left, module))
    check(
        "C9 chat::queue::dispatch is still LEFT on a generation edge",
        listed(left, "chat::queue::dispatch"),
    )
else:
    check("C9 census report written", False, report)

print()
if failures:
    print(f"RESULT: FAIL ({len(failures)} checks)")
    sys.exit(1)
print("RESULT: PASS")
