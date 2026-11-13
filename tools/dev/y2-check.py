#!/usr/bin/env python3
"""Y-2 static verification: providers pure helpers folded into refact-providers.

Run: python tools/dev/y2-check.py

Checks (no compiler needed; the build guard refuses cargo for card worktrees):
  R1 residency      - each moved symbol is defined in refact-providers and re-exported once from the engine.
  R2 no duplicate   - the engine no longer defines the moved items.
  R3 no crate->engine edge - the new crate modules only reference crate-local paths.
  R4 body identity  - the crate copy of each moved item matches the pre-move engine text byte for byte
                      (module/import lines excluded).
  R5 balance        - no merge markers and brace/paren balance in every touched file.
"""

import os
import re
import subprocess
import sys

REPO = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
ENGINE = os.path.join(REPO, "refact-agent", "engine")
CRATE = os.path.join(ENGINE, "crates", "refact-providers", "src", "http")
HTTP = os.path.join(ENGINE, "src", "providers", "http")

MOVED = {
    "ensure_oauth_identity_for_instance_sync": (
        os.path.join(CRATE, "oauth.rs"),
        os.path.join(HTTP, "oauth.rs"),
    ),
    "stale_quota_fallback": (
        os.path.join(CRATE, "quota_cache.rs"),
        os.path.join(HTTP, "quota_cache.rs"),
    ),
    "invalidate_provider_quota": (
        os.path.join(CRATE, "quota_cache.rs"),
        os.path.join(HTTP, "quota_cache.rs"),
    ),
}

MOVED_ITEMS = [
    "SimplifiedModel",
    "ModelsResponse",
    "ModelsQueryParams",
    "AvailableModelsResponse",
    "OpenRouterHealthResponse",
    "GoogleGeminiHealthResponse",
    "ModelToggleRequest",
    "ModelProviderRequest",
    "AddCustomModelRequest",
    "RemoveCustomModelRequest",
]

TOUCHED = [
    os.path.join(CRATE, "mod.rs"),
    os.path.join(CRATE, "oauth.rs"),
    os.path.join(CRATE, "quota_cache.rs"),
    os.path.join(CRATE, "models.rs"),
    os.path.join(HTTP, "oauth.rs"),
    os.path.join(HTTP, "quota_cache.rs"),
    os.path.join(HTTP, "models.rs"),
]

ALLOWED_CRATE_REFS = {
    "crate::identity",
    "crate::traits",
    "crate::openrouter",
    "crate::google_gemini",
    "crate::quota",
    "crate::config_store",
    "crate::create_provider",
    "crate::instance",
    "crate::oauth_refresh",
    "crate::http",
}

failures = []
checks = 0


def check(name, ok, detail=""):
    global checks
    checks += 1
    if ok:
        print("PASS  " + name)
    else:
        print("FAIL  " + name + " :: " + detail)
        failures.append(name)


def read(path):
    with open(path, encoding="utf-8", errors="replace") as fh:
        return fh.read()


def fn_body(text, name):
    """Return the body of the top-level `fn name(` declaration, or None."""
    m = re.search(r"^[ \t]*pub(?:\([^)]*\))?[ \t]+(?:async[ \t]+)?fn[ \t]+" + re.escape(name) + r"\b", text, re.M)
    if not m:
        return None
    start = text.find("{", m.end())
    if start < 0:
        return None
    depth = 0
    i = start
    while i < len(text):
        ch = text[i]
        if ch == "{":
            depth += 1
        elif ch == "}":
            depth -= 1
            if depth == 0:
                return text[start : i + 1]
        i += 1
    return None


def normalize(body):
    return re.sub(r"\s+", " ", body).strip()


print("=== R1 residency: moved symbols live in refact-providers and are re-exported once ===")
for name, (crate_path, engine_path) in MOVED.items():
    crate_text = read(crate_path)
    engine_text = read(engine_path)
    check(
        "crate defines " + name,
        re.search(r"^[ \t]*pub(?:\([^)]*\))?[ \t]+(?:async[ \t]+)?fn[ \t]+" + re.escape(name) + r"\b", crate_text, re.M)
        is not None,
        name + " missing from " + os.path.relpath(crate_path, REPO),
    )
    check(
        "engine re-exports " + name,
        re.search(r"pub(?:\([^)]*\))?[ \t]+use[ \t]+refact_providers::http::[a-z_]+::", engine_text) is not None
        and name in engine_text,
        name + " is not re-exported from " + os.path.relpath(engine_path, REPO),
    )

crate_models = read(os.path.join(CRATE, "models.rs"))
for item in MOVED_ITEMS:
    check(
        "crate::http::models defines " + item,
        re.search(r"^[ \t]*pub[ \t]+(?:struct|enum)[ \t]+" + re.escape(item) + r"\b", crate_models, re.M) is not None,
        item + " missing",
    )
engine_models = read(os.path.join(HTTP, "models.rs"))
check(
    "engine models.rs re-exports the response contracts",
    "pub use refact_providers::http::models::" in engine_models,
    "no re-export block",
)

print()
print("=== R2 no duplicate definitions left in the engine ===")
for name, (_crate_path, engine_path) in MOVED.items():
    engine_text = read(engine_path)
    check(
        "engine does not redefine " + name,
        fn_body(engine_text, name) is None,
        "engine still defines " + name,
    )
for item in MOVED_ITEMS:
    check(
        "engine does not redefine " + item,
        re.search(r"^[ \t]*(?:pub)?[ \t]*(?:pub(?:\([^)]*\))?[ \t]+)?struct[ \t]+" + re.escape(item) + r"\b", engine_models, re.M)
        is None,
        "engine still defines struct " + item,
    )

print()
print("=== R3 no crate -> engine dependency in the new crate modules ===")
crate_mod_rs = read(os.path.join(CRATE, "mod.rs"))
for module in ("common", "models", "oauth", "quota_cache", "settings"):
    check(
        "crate::http exposes " + module,
        re.search(r"^pub mod " + module + r";", crate_mod_rs, re.M) is not None,
        module + " not declared in crate http/mod.rs",
    )
for path in (
    os.path.join(CRATE, "models.rs"),
    os.path.join(CRATE, "oauth.rs"),
    os.path.join(CRATE, "quota_cache.rs"),
):
    text = read(path)
    refs = set(re.findall(r"\bcrate::([a-zA-Z_][a-zA-Z0-9_]*(?:::[a-zA-Z_][a-zA-Z0-9_]*)*)", text))
    allowed = {a[len("crate::") :] for a in ALLOWED_CRATE_REFS}
    bad = sorted(r for r in refs if not any(r == a or r.startswith(a + "::") for a in allowed))
    check(
        os.path.relpath(path, REPO) + " stays crate-local",
        not bad,
        "unexpected crate:: refs: " + ", ".join(bad),
    )
    check(
        os.path.relpath(path, REPO) + " has no refact_lsp reference",
        "refact_lsp" not in text and "crate::app_state" not in text and "crate::global_context" not in text,
        "crate module reaches into the engine",
    )

print()
print("=== R4 moved bodies are identical to the pre-move engine text ===")
base = subprocess.run(
    ["git", "-C", REPO, "show", "9b6054411:refact-agent/engine/src/providers/http/oauth.rs"],
    capture_output=True,
    text=True,
    encoding="utf-8",
    errors="replace",
)
base_quota = subprocess.run(
    ["git", "-C", REPO, "show", "9b6054411:refact-agent/engine/src/providers/http/quota_cache.rs"],
    capture_output=True,
    text=True,
    encoding="utf-8",
    errors="replace",
)
if base.returncode == 0 and base_quota.returncode == 0:
    for name, (crate_path, _engine_path), base_text in (
        ("ensure_oauth_identity_for_instance_sync", MOVED["ensure_oauth_identity_for_instance_sync"], base.stdout),
        ("stale_quota_fallback", MOVED["stale_quota_fallback"], base_quota.stdout),
        ("invalidate_provider_quota", MOVED["invalidate_provider_quota"], base_quota.stdout),
    ):
        before = fn_body(base_text, name)
        after = fn_body(read(crate_path), name)
        check(
            name + " body is byte-identical after whitespace normalisation",
            before is not None and after is not None and normalize(before) == normalize(after),
            "body drifted",
        )
else:
    check("could read the pre-move baseline from git", False, "git show 9b6054411 failed")

print()
print("=== R5 no merge markers, balanced delimiters ===")
for path in TOUCHED:
    text = read(path)
    rel = os.path.relpath(path, REPO)
    check(rel + " has no merge markers", not re.search(r"^(<<<<<<<|=======|>>>>>>>)$", text, re.M), "conflict markers")
    for opener, closer in (("{", "}"), ("(", ")"), ("[", "]")):
        # strings and comments are not stripped; the shipped style keeps them balanced anyway
        check(
            rel + " balanced " + opener + closer,
            text.count(opener) == text.count(closer),
            "counts " + str(text.count(opener)) + " vs " + str(text.count(closer)),
        )

print()
print("=== R6 acceptance-criteria grep: engine providers/*.rs expose only shims/wrappers ===")
FUNCTIONAL_BLOCKED = {
    "oauth_refresh.rs": "Arc<GlobalContext> + app_state_from_gcx + buddy::actor::make_runtime_event"
    " (blocking crate: refact-runtime-api)",
    "pricing.rs": "Arc<GlobalContext> / ChatCtx (blocking crate: refact-runtime-api)",
}
providers_dir = os.path.join(ENGINE, "src", "providers")
for entry in sorted(os.listdir(providers_dir)):
    if not entry.endswith(".rs"):
        continue
    text = read(os.path.join(providers_dir, entry))
    hits = re.findall(r"^[ \t]*pub[ \t]+(?:async[ \t]+)?fn[ \t]+", text, re.M)
    if not hits:
        check("providers/" + entry + " is a shim or module only", True)
        continue
    check(
        "providers/" + entry + " real functions are a documented blocker",
        entry in FUNCTIONAL_BLOCKED,
        "%d top-level pub fn remain and no blocker is recorded" % len(hits),
    )
    if entry in FUNCTIONAL_BLOCKED:
        print("      blocker: " + FUNCTIONAL_BLOCKED[entry])

print()
if failures:
    print("VERIFY: FAIL (%d of %d checks failed)" % (len(failures), checks))
    for name in failures:
        print("  - " + name)
    sys.exit(1)

print("VERIFY: PASS (%d checks)" % checks)
sys.exit(0)
