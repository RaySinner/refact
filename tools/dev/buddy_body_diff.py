"""Q-5 behaviour-preservation check for the harvested buddy modules.

Compares each base engine file (at the card's base commit) with the crate file it
became, after normalizing exactly the rewrites a move is allowed to make:

* whole `use` statements (single- or multi-line);
* the `ObserverContext`/`BuddyObserver` struct + impl block, which is the moved
  glue and now lives in `buddy_observers/mod.rs` (and, for the crate-private test
  helper, `scan_knowledge_dir_count_for_test`);
* engine module paths -> the crate module holding the same helper;
* visibility widening (`pub(crate)` / no `pub` -> `pub`), which is required
  across the crate boundary;
* `Utc::now()` vs `chrono::Utc::now()`.

Everything else must be identical, which is the card's "zero behaviour change"
requirement for prompt/observation payloads.
"""
import os
import re
import subprocess
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
ENGINE = os.path.join(ROOT, "refact-agent", "engine")
BASE = "ac177abe7"

PAIRS = [
    ("src/buddy/observers/chat_pattern.rs",
     "crates/refact-runtime-api/src/buddy_observers/chat_pattern.rs"),
    ("src/buddy/observers/customization_drift.rs",
     "crates/refact-runtime-api/src/buddy_observers/customization_drift.rs"),
    ("src/buddy/observers/diagnostic_cluster.rs",
     "crates/refact-runtime-api/src/buddy_observers/diagnostic_cluster.rs"),
    ("src/buddy/observers/memory_garden.rs",
     "crates/refact-runtime-api/src/buddy_observers/memory_garden.rs"),
    ("src/buddy/observers/provider_health.rs",
     "crates/refact-runtime-api/src/buddy_observers/provider_health.rs"),
    ("src/buddy/observers/trajectory_clutter.rs",
     "crates/refact-runtime-api/src/buddy_observers/trajectory_clutter.rs"),
    ("src/buddy/observers/worktree_hygiene.rs",
     "crates/refact-runtime-api/src/buddy_observers/worktree_hygiene.rs"),
    ("src/buddy/chat_interjection.rs",
     "crates/refact-runtime-api/src/buddy_chat_interjection.rs"),
    ("src/buddy/pulse_inject.rs",
     "crates/refact-runtime-api/src/buddy_pulse_inject.rs"),
    ("src/buddy/observers/privacy_degraded.rs",
     "crates/refact-runtime-api/src/buddy_privacy_degraded.rs"),
]

FINAL_REWRITES = [
    ("crate::yaml_configs::customization_registry::", "crate::customization_registry_gcx::"),
    ("crate::ext::config_dirs::", "crate::ext_config_dirs::"),
    ("crate::ext::skills::", "refact_ext::skills::"),
    ("crate::llm::safe_truncate", "refact_core::string_utils::safe_truncate"),
    ("crate::chat::trajectories::save_trajectory_snapshot", "crate::trajectory_runtime::save_trajectory_snapshot"),
    ("crate::buddy::observers::", "crate::"),
    ("crate::buddy::", "crate::"),
    ("crate::actor::redact_sensitive", "redact_sensitive"),
    ("crate::actor::resolve_buddy_chat_model", "resolve_buddy_chat_model"),
    ("refact_buddy_core::actor::redact_sensitive", "redact_sensitive"),
    ("refact_buddy_core::state::SpeechRotationState", "SpeechRotationState"),
    ("crate::app_state::app_state_from_gcx", "app_state_from_gcx"),
    ("crate::app_state_from_gcx", "app_state_from_gcx"),
    ("crate::trajectory_runtime::save_trajectory_snapshot", "save_trajectory_snapshot"),
    ("crate::tasks::storage::", "crate::task_storage::"),
    ("Arc<crate::global_context::GlobalContext>", "SharedGlobalContext"),
    ("Arc<GlobalContext>", "SharedGlobalContext"),
    ("chrono::Utc::now()", "Utc::now()"),
    ("pub(crate) ", ""),
    ("pub ", ""),
]

VISIBILITY_ATTRS = re.compile(
    r"^(#\[cfg_attr\(not\(test\),\s*allow\(dead_code\)\)\]|#\[allow\(dead_code\)\]|#\[cfg\(test\)\])"
)


def strip_test_block(text):
    idx = text.find("#[cfg(test)]")
    return text if idx == -1 else text[:idx]


def drop_use_statements(lines):
    out, in_use = [], False
    for line in lines:
        if in_use:
            in_use = not line.endswith(";")
            continue
        if re.match(r"^(pub\s+)?use\s|^pub\(crate\)\s+use\s", line):
            in_use = not line.endswith(";")
            continue
        out.append(line)
    return out


def drop_observer_glue(lines):
    """Drop `pub struct XObserver;`, its `impl BuddyObserver` block, and the
    crate-private test helper that moved with it."""
    out, i = [], 0
    while i < len(lines):
        line = lines[i]
        if re.match(r"^(pub )?struct \w+Observer;$", line):
            i += 1
            continue
        glue_start = re.match(r"^impl BuddyObserver for ", line) or (
            line.startswith("#[async_trait::async_trait]") and i + 1 < len(lines)
            and re.match(r"^impl BuddyObserver for ", lines[i + 1])
        )
        helper_start = re.match(
            r"^(pub )?(async )?fn scan_knowledge_dir_count_for_test\(", line
        )
        if glue_start or helper_start:
            if line.startswith("#["):
                i += 1
            depth, j = 0, i
            while j < len(lines):
                depth += lines[j].count("{") - lines[j].count("}")
                if depth == 0 and "}" in lines[j]:
                    j += 1
                    break
                j += 1
            i = j
            continue
        out.append(line)
        i += 1
    return out


def normalize(text):
    lines = [l.strip() for l in strip_test_block(text).splitlines()]
    lines = [l for l in lines if l and not l.startswith("//") and not VISIBILITY_ATTRS.match(l)]
    lines = drop_use_statements(lines)
    lines = drop_observer_glue(lines)
    blob = " ".join(lines)
    for a, b in FINAL_REWRITES:
        blob = blob.replace(a, b)
    blob = re.sub(r"\s+", "", blob)
    # rustfmt wrapping differences: a trailing comma before `)` is cosmetic.
    return re.sub(r",+\)", ")", blob)


failures = []
for src, dst in PAIRS:
    base = subprocess.run(
        ["git", "show", f"{BASE}:refact-agent/engine/{src}"],
        cwd=ROOT, capture_output=True, text=True,
    ).stdout
    with open(os.path.join(ENGINE, *dst.split("/")), encoding="utf-8") as fh:
        new = fh.read()
    a, b = normalize(base), normalize(new)
    ok = a == b
    print(f"{'OK  ' if ok else 'DIFF'} {src}  (normalized chars {len(a)} vs {len(b)})")
    if not ok:
        first = next((i for i in range(min(len(a), len(b))) if a[i] != b[i]), min(len(a), len(b)))
        print("   ...base: " + a[max(0, first - 60) : first + 60])
        print("   ... new: " + b[max(0, first - 60) : first + 60])
        failures.append(src)

print()
print("### RESULT: " + ("PASS" if not failures else "FAIL"))
sys.exit(1 if failures else 0)
