"""ST-6 self-check: the generation-start seam exists, is wired, and no chat file moved."""
import re
import subprocess
import sys

sys.stdout.reconfigure(encoding="utf-8")

ENGINE = "refact-agent/engine"
CRATE_MOD = f"{ENGINE}/crates/refact-runtime-api/src/generation_start.rs"
LIB = f"{ENGINE}/crates/refact-runtime-api/src/lib.rs"
CHAT_CTX = f"{ENGINE}/crates/refact-runtime-api/src/chat_ctx.rs"
ENGINE_IMPL = f"{ENGINE}/src/app_state/generation.rs"
APP_STATE = f"{ENGINE}/src/app_state.rs"
QUEUE_MOD = f"{ENGINE}/src/chat/queue/mod.rs"
CHAT_MOD = f"{ENGINE}/src/chat/mod.rs"

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
        ["git", *args], capture_output=True, text=True, encoding="utf-8", errors="replace"
    )
    return out.stdout


# C1 — the crate module exists with the trait and both free functions.
crate_src = read(CRATE_MOD)
check("C1 trait GenerationStarter declared", "pub trait GenerationStarter: Send + Sync {" in crate_src)
check(
    "C1 trait method start_generation",
    re.search(r"async fn start_generation\(\s*&self,\s*app: AppState,", crate_src) is not None,
)
check(
    "C1 trait method prepare_session_preamble_and_knowledge",
    re.search(
        r"async fn prepare_session_preamble_and_knowledge\(\s*&self,\s*app: AppState,", crate_src
    )
    is not None,
)
check(
    "C1 free fn start_generation dispatches through ChatCtx",
    re.search(
        r"pub async fn start_generation\(app: AppState, session_arc: Arc<AMutex<ChatSession>>\) \{\s*"
        r"let starter = app\.ctx\.generation\.clone\(\);",
        crate_src,
    )
    is not None,
)
check(
    "C1 free fn prepare_session_preamble_and_knowledge dispatches through ChatCtx",
    re.search(
        r"pub async fn prepare_session_preamble_and_knowledge\(\s*app: AppState,\s*"
        r"session_arc: Arc<AMutex<ChatSession>>,\s*\) \{\s*let starter = app\.ctx\.generation\.clone\(\);",
        crate_src,
    )
    is not None,
)

# C2 — the module is registered and the field is on ChatCtx.
lib_src = read(LIB)
check("C2 lib.rs registers pub mod generation_start", "pub mod generation_start;" in lib_src)
chat_ctx_src = read(CHAT_CTX)
check(
    "C2 ChatCtx carries the generation field",
    "pub generation: Arc<dyn crate::generation_start::GenerationStarter>," in chat_ctx_src,
)

# C3 — the engine implements the trait by delegating to chat::generation.
impl_src = read(ENGINE_IMPL)
check(
    "C3 engine implements GenerationStarter",
    "impl refact_runtime_api::generation_start::GenerationStarter for EngineGenerationStarter"
    in impl_src,
)
check(
    "C3 impl delegates to chat::generation::start_generation",
    "crate::chat::generation::start_generation(app, session_arc).await" in impl_src,
)
check(
    "C3 impl delegates to chat::generation::prepare_session_preamble_and_knowledge",
    "crate::chat::generation::prepare_session_preamble_and_knowledge(app, session_arc).await"
    in impl_src,
)

# C4 — the engine installs it in the one ChatCtx literal.
app_state_src = read(APP_STATE)
check("C4 app_state declares the generation module", "mod generation;" in app_state_src)
check(
    "C4 app_state re-exports the generation module",
    "pub use self::generation::*;" in app_state_src,
)
check(
    "C4 ChatCtx literal installs EngineGenerationStarter",
    "generation: Arc::new(EngineGenerationStarter)," in app_state_src,
)

# C5 — the command loop reaches generation through the seam, not the engine module.
queue_src = read(QUEUE_MOD)
check(
    "C5 queue/mod.rs imports the seam",
    "use refact_runtime_api::generation_start::{" in queue_src
    and "prepare_session_preamble_and_knowledge, start_generation," in queue_src,
)
check(
    "C5 queue/mod.rs no longer imports generation::{start_generation,...}",
    "use crate::chat::generation::{\n    start_generation," not in queue_src,
)
check(
    "C5 queue/mod.rs keeps the engine-only trajectory helper",
    "use crate::chat::generation::maybe_save_trajectory_with_intent;" in queue_src,
)

# C6 — chat::generation is reachable crate-wide (needed by the engine impl).
chat_mod_src = read(CHAT_MOD)
check(
    "C6 chat/mod.rs widens generation to crate visibility",
    "pub(crate) mod generation;" in chat_mod_src,
)

# Base commit of this card: comparisons must pin it, not HEAD (the card commits its own work).
CARD_BASE = "f30c39661"

# C7 — no generation body changed (the engine keeps its functions unchanged).
unchanged = git("diff", CARD_BASE, "--", f"{ENGINE}/src/chat/generation/")
check("C7 chat/generation bodies untouched", unchanged.strip() == "", unchanged.strip()[:200])

# C8 — no other chat file moved: the diff under src/chat is only the two wiring files.
chat_diff = [
    line.strip()
    for line in git("diff", "--name-only", CARD_BASE, "--", f"{ENGINE}/src/chat/").splitlines()
    if line.strip()
]
expected = sorted([QUEUE_MOD, CHAT_MOD])
check(
    "C8 only chat/mod.rs + chat/queue/mod.rs touched under src/chat",
    sorted(chat_diff) == expected,
    ", ".join(chat_diff),
)

# C9 — no crate -> engine dependency.
manifest = read(f"{ENGINE}/crates/refact-runtime-api/Cargo.toml")
check("C9 refact-runtime-api does not depend on refact-lsp", "refact-lsp" not in manifest)

# C10 — the crate module never names an engine module.
bad = [
    line
    for line in crate_src.splitlines()
    if re.search(r"\bcrate::(chat|subchat|tools|generation)\b", line)
]
check("C10 crate module has no crate::<engine module> backrefs", not bad, "; ".join(bad)[:200])

print()
if failures:
    print(f"RESULT: FAIL ({len(failures)} checks)")
    sys.exit(1)
print("RESULT: PASS")
