#!/usr/bin/env python3
"""ST-1 structural checks: `chat::stream_core` settled into `refact-runtime-api`.

Layer A of the ST-1 verification. The compiler layer is owned by the project
build guard (`~/.cargo/refact-build-guard.exe` refuses every cargo invocation
inside an agent worktree), so every check here is a structural fact about the
tree plus a body-preservation diff against the pre-move blob in `HEAD`.

Usage:  python tools/dev/st1-check.py
Exit:   0 = all checks pass, 1 = at least one check failed.
"""

import importlib.util
import os
import re
import subprocess
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
ENGINE = os.path.join(ROOT, "refact-agent", "engine")
CRATES = os.path.join(ENGINE, "crates")

ENGINE_FILE = os.path.join(ENGINE, "src", "chat", "stream_core.rs")
CRATE_FILE = os.path.join(CRATES, "refact-runtime-api", "src", "stream_core.rs")
RUNTIME_API_LIB = os.path.join(CRATES, "refact-runtime-api", "src", "lib.rs")
RUNTIME_API_CARGO = os.path.join(CRATES, "refact-runtime-api", "Cargo.toml")
LLM_CARGO = os.path.join(CRATES, "refact-llm", "Cargo.toml")
CENSUS = os.path.join(ROOT, "tools", "dev", "chat_movable_census.py")

BLOCKED_AT_BASE = {
    "chat::queue::dispatch": 1789,
    "chat::generation::stream": 1085,
    "chat::task_agent_monitor::broadcast": 941,
    "chat::generation::run_loop": 742,
    "chat::handlers": 718,
    "chat::generation::preamble": 713,
    "chat::generation::recovery": 713,
    "chat::goal_monitor": 591,
    "chat::task_agent_monitor::finish": 510,
    "chat::task_agent_monitor::monitor": 501,
    "chat::delivery": 169,
    "chat::generation::retry": 77,
}

PROD_ITEMS = [
    "stack_mb_to_bytes_clamped",
    "openai_codex_websocket_thread_stack_bytes",
    "wait_for_abort_signal",
    "abort_requested",
    "is_abort_error",
    "send_llm_http_request",
    "llm_http_response_header_timeout_message",
    "wait_before_llm_transport_retry",
    "send_llm_http_request_with_header_timeout",
    "should_commit_cache_guard_after_http_success",
    "openai_codex_instance_id",
    "claude_code_instance_id",
    "xai_oauth_instance_id",
    "is_litellm_model",
    "xai_rate_limit_headers",
    "capture_xai_oauth_rate_limit_headers",
    "claude_code_request_expires_at",
    "is_openai_codex_chatgpt_backend",
    "llm_http_header_retry_config",
    "openai_codex_websocket_auth_rejection_status",
    "set_bearer_authorization_header",
    "force_refresh_openai_codex_for_retry",
    "force_refresh_claude_code_for_retry",
    "openai_codex_websocket_endpoint",
    "build_openai_codex_websocket_message",
    "websocket_header_entries",
    "openai_codex_websocket_connection_key",
    "openai_codex_body_without_stateful_fields",
    "openai_codex_input_items",
    "openai_codex_delta_input",
    "prepare_openai_codex_websocket_body",
    "clear_openai_codex_websocket_incremental_state",
    "take_openai_codex_websocket_session",
    "store_openai_codex_websocket_session",
    "clear_unbound_openai_codex_websocket_session",
    "commit_cache_guard_snapshot_if_needed",
    "ensure_openai_codex_websocket_connected",
    "run_openai_codex_websocket_exchange",
    "close_openai_codex_websocket_connection",
    "run_llm_websocket_request",
    "refresh_openai_codex_after_websocket_auth_rejection",
    "run_llm_websocket_request_with_retry",
    "run_llm_websocket_request_on_large_stack",
    "openai_codex_websocket_error_message",
    "is_openai_codex_previous_response_missing_error",
    "run_llm_ndjson_request",
    "build_http_for_attempt",
    "run_llm_stream",
    "run_llm_stream_with_prepare_started",
    "read_error_body_bounded",
    "format_llm_error_body",
]

results = []


def check(name, ok, detail=""):
    results.append((name, bool(ok), detail))


def read(path):
    with open(path, "r", encoding="utf-8", errors="replace") as fh:
        return fh.read()


def git_show(rev_path):
    out = subprocess.run(
        ["git", "show", rev_path],
        cwd=ROOT,
        capture_output=True,
        text=True,
        encoding="utf-8",
        errors="replace",
    )
    return out.stdout if out.returncode == 0 else ""


def base_body_text(path, anchor, depth=8):
    """The most recent committed version of `path` that still carries the body.

    Once this card is committed, `HEAD` holds the shim, so the pre-move body is
    the first ancestor revision whose blob still contains `anchor`."""
    for back in range(depth):
        rev = "HEAD" if back == 0 else "HEAD~%d" % back
        text = git_show("%s:%s" % (rev, path))
        if anchor in text:
            return text, rev
    return "", ""


def prod(text):
    idx = text.find("#[cfg(test)]")
    return text if idx == -1 else text[:idx]


def prod_before_tests(text):
    """Everything before the kept test module. The base file declares
    `#[cfg(test)] use …` near the top, so cutting at the first attribute
    would return an empty body."""
    idx = text.find("\n#[cfg(test)]\nmod tests {")
    return text if idx == -1 else text[:idx]


def normalise(body):
    """Apply the move's mechanical rewrites so equal bodies compare equal."""
    pairs = [
        ("crate::chat::cache_diagnostics::", "refact_chat_api::cache_diagnostics::"),
        ("crate::chat::cache_guard::", "crate::chat_cache_guard::"),
        ("crate::chat::trajectory_ops::", "refact_chat_history::trajectory_ops::"),
        ("crate::chat::diagnostics::", "refact_chat_api::diagnostics::"),
        ("crate::chat::perf_diagnostics", "refact_perf::perf_diagnostics"),
        ("crate::chat::types::", "refact_chat_api::types::"),
        ("crate::chat::ctx::", "crate::chat_ctx::"),
        ("crate::privacy::destinations::", "refact_privacy::destinations::"),
        ("crate::providers::oauth_refresh::", "crate::providers_oauth_refresh::"),
        ("crate::providers::runtime_credential::", "refact_providers::runtime_credential::"),
        ("crate::providers::llm_http_retry::", "refact_providers::llm_http_retry::"),
        ("crate::providers::claude_code::", "refact_providers::claude_code::"),
        ("crate::providers::openai_codex::", "refact_providers::openai_codex::"),
        ("crate::providers::xai_oauth::", "refact_providers::xai_oauth::"),
        ("crate::caps::caps::", "refact_caps_core::caps_build::"),
        ("crate::llm::adapter::", "refact_llm::adapter::"),
        ("crate::llm::", "refact_llm::"),
    ]
    for old, new in pairs:
        body = body.replace(old, new)
    body = body.replace("\npub(crate) const ", "\npub const ")
    body = body.replace("\npub(crate) async fn ", "\npub async fn ")
    body = body.replace("\nfn ", "\npub fn ")
    body = body.replace("\nasync fn ", "\npub async fn ")
    body = body.replace("\nconst ", "\npub const ")
    body = body.replace(
        "\ntype OpenAICodexWebSocketThreadResult = Result<",
        "\npub type OpenAICodexWebSocketThreadResult = Result<",
    )
    return [line.rstrip() for line in body.strip().splitlines()]


def body_of(text, anchor):
    idx = text.find(anchor)
    return text[idx:] if idx != -1 else ""


print("== 1. the engine file is a pure facade ==")
engine_text = read(ENGINE_FILE)
engine_prod = prod(engine_text)
check(
    "engine src/chat/stream_core.rs starts with the crate re-export",
    engine_text.startswith("pub use refact_runtime_api::stream_core::*;"),
)
prod_decls = re.findall(
    r"(?m)^(?:pub )?(?:async )?(?:fn|struct|enum|trait|type|const|static)\s", engine_prod
)
check(
    "engine facade holds no production declarations",
    not prod_decls,
    "%d declarations" % len(prod_decls),
)
check(
    "engine facade has no engine path in production code",
    "crate::" not in engine_prod.replace("pub use refact_runtime_api::stream_core::*;", ""),
)
check("the kept tests still live in the engine file", "#[cfg(test)]\nmod tests {" in engine_text)
test_lines = engine_text.count("\n") - engine_prod.count("\n")
print("   engine facade: %d prod lines, %d kept-test lines" % (engine_prod.count("\n"), test_lines))

print()
print("== 2. the residue is in refact-runtime-api::stream_core ==")
crate_text = read(CRATE_FILE)
check("crate module exists", bool(crate_text))
check(
    "crate module is declared in lib.rs",
    "pub mod stream_core;" in read(RUNTIME_API_LIB),
)
check(
    "crate module re-exports the protocol layer it builds on",
    "pub use refact_llm::stream_core::*;" in crate_text,
)
missing = [
    name
    for name in PROD_ITEMS
    if not re.search(r"(?m)^pub (?:async )?fn " + name + r"\b|^pub type " + name + r"\b|^pub const " + name + r"\b", crate_text)
]
check(
    "every production symbol of the residue is a pub item of the crate",
    not missing,
    ", ".join(missing),
)
check(
    "ABORT_ERROR_MESSAGE is pub (cross-crate now)",
    'pub const ABORT_ERROR_MESSAGE: &str = "Aborted";' in crate_text,
)
check(
    "the crate module has no engine-local paths left",
    not re.search(
        r"crate::(?!app_state|chat_ctx|chat_cache_guard|providers_oauth_refresh)", crate_text
    ),
)
for name in (
    "app_state",
    "chat_ctx",
    "chat_cache_guard",
    "providers_oauth_refresh",
):
    check("crate seam edge crate::%s exists" % name, "pub mod %s;" % name in read(RUNTIME_API_LIB))
print("   crate module: %d production lines" % crate_text.count("\n"))

print()
print("== 3. body preservation against the pre-move blob ==")
base_text = git_show("HEAD:refact-agent/engine/src/chat/stream_core.rs")
base_text, base_rev = base_body_text(
    "refact-agent/engine/src/chat/stream_core.rs", "lazy_static::lazy_static! {"
)
check("pre-move blob is readable from history", bool(base_text), base_rev or "not found")
base_body = body_of(prod_before_tests(base_text), "lazy_static::lazy_static! {")
crate_body = body_of(crate_text, "lazy_static::lazy_static! {")
expected = normalise(base_body)
actual = normalise(crate_body)
if expected == actual:
    check("moved body is byte-identical modulo the import rewrite", True)
    print("   %d body lines preserved verbatim" % len(expected))
else:
    diffs = [
        (i, a, b)
        for i, (a, b) in enumerate(zip(expected, actual))
        if a != b
    ]
    check(
        "moved body is byte-identical modulo the import rewrite",
        False,
        "first diff at line %d: %r != %r (lengths %d vs %d)"
        % (
            (diffs[0][0] + 1, diffs[0][1], diffs[0][2], len(expected), len(actual))
            if diffs
            else (0, "", "", len(expected), len(actual))
        ),
    )

print()
print("== 4. crate edges, both directions ==")
runtime_cargo = read(RUNTIME_API_CARGO)
llm_cargo = read(LLM_CARGO)
check(
    "refact-runtime-api depends on refact-llm",
    'refact-llm = { path = "../refact-llm" }' in runtime_cargo,
)
check(
    "refact-runtime-api gained tokio-tungstenite",
    "tokio-tungstenite.workspace = true" in runtime_cargo,
)
check(
    "refact-runtime-api gained lazy_static",
    "lazy_static.workspace = true" in runtime_cargo,
)
check(
    "refact-llm does NOT depend on refact-runtime-api (no cycle)",
    "refact-runtime-api" not in llm_cargo,
)
check(
    "engine still depends on refact-runtime-api",
    "refact-runtime-api" in read(os.path.join(ENGINE, "Cargo.toml")),
)

print()
print("== 5. census: the island gates nothing any more ==")
spec = importlib.util.spec_from_file_location("st1_census", CENSUS)
census = importlib.util.module_from_spec(spec)
saved = list(sys.argv)
sys.argv = [CENSUS, "chat"]
spec.loader.exec_module(census)
sys.argv = saved

check(
    "chat::stream_core is still a declared island (unused, but not silently dropped)",
    "chat::stream_core" in census.ISLANDS,
)
check(
    "chat::stream_core classifies as already-shim",
    "chat::stream_core" in census.facade,
)
still_blocked = sorted(
    m
    for m in BLOCKED_AT_BASE
    if census.island_blocker(m) == "chat::stream_core"
)
check(
    "no file is island-blocked by chat::stream_core any more",
    not still_blocked,
    ", ".join(still_blocked),
)
blocked_class = sorted(
    census.classes["BLOCKED on another island (that island needs its own card first)"]
)
check(
    "the island-blocked class is empty",
    not blocked_class,
    "%d files: %s" % (len(blocked_class), ", ".join(blocked_class[:4])),
)
released = sorted(
    m
    for m in BLOCKED_AT_BASE
    if m not in blocked_class and not census.island_blocker(m)
)
print(
    "   %d of the 12 formerly island-blocked files are out of the island class"
    % len(released)
)
print(
    "   still held by engine code: %s"
    % ", ".join(
        "%s(%d)" % (m, BLOCKED_AT_BASE[m])
        for m in sorted(BLOCKED_AT_BASE, key=lambda m: -BLOCKED_AT_BASE[m])
        if census.info[m]["engine"]
    )
)
check(
    "the census tree total dropped below the pre-move 16 453 lines",
    sum(census.info[m]["lines"] for m in census.files) < 16453,
)

print()
print("== 6. the kept dependent modules still resolve through the facade ==")
for mod, rev in (
    ("chat::generation::stream", "refact-agent/engine/src/chat/generation/stream.rs"),
    ("chat::generation::preamble", "refact-agent/engine/src/chat/generation/preamble.rs"),
    ("chat::goal_verifier", "refact-agent/engine/src/chat/goal_verifier/mod.rs"),
    ("subchat::runner", "refact-agent/engine/src/subchat/runner.rs"),
):
    body = read(os.path.join(ROOT, rev))
    imports = [
        name
        for name in re.findall(
            r"crate::chat::stream_core::\{([^}]*)\}|crate::chat::stream_core::([A-Za-z_][A-Za-z0-9_]*)",
            body,
        )
        for name in (name[0] if isinstance(name, tuple) else name)
    ]
    flat = ", ".join(
        part.strip() for chunk in imports for part in chunk.split(",") if part.strip()
    )
    check(
        "%s imports through the facade" % mod,
        "crate::chat::stream_core::" in body,
        flat,
    )

print()
failed = [name for name, ok, _ in results if not ok]
for name, ok, detail in results:
    print(("PASS  " if ok else "FAIL  ") + name + (("  [" + detail + "]") if detail and not ok else ""))
print()
if failed:
    print("ST-1 CHECK FAILED: %d of %d" % (len(failed), len(results)))
    sys.exit(1)
print("ST-1 CHECK PASSED (%d checks)" % len(results))
