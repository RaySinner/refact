# Y-2 — Close out providers

## Summary

`providers` is **not** "14 of 15 files movable, 1 file / 66 lines blocked". The census figure is an artifact of
`refact-agent/engine/target/census_module.py`, which is wrong twice over for this module:

1. `prod_part()` truncates a file at the **first** `#[cfg(test)]` anywhere in it, so
   `providers/oauth_refresh.rs` (1 941 lines, 1 486 of them production) is measured as **34 lines** — the head up to
   `#[cfg(test)] fn oauth_failed_instance_count_for_test` on line 35. Nothing after that is visible to the census.
2. `providers/http/tests.rs` has no inner `#[cfg(test)]` (it is pulled in by `#[cfg(test)] mod tests;` in
   `http/mod.rs`), so its **1 596 test lines count as production**. 1 722 of the 5 537 "movable" lines are test code.
3. Name-level resolution cannot see `use super::*;`, so every `providers/http/*.rs` handler module — which takes
   `AppState`, `ChatCtx`, `ScratchError`, `json_response` and the DTOs from `http/mod.rs`'s prelude — scores as
   dependency-free.

Re-measured (test modules excluded, `crate::` paths resolved name-level):

| module `providers` | files | production lines |
|---|---|---|
| total | 15 | 7 565 |
| census-movable | 9 | 2 449 |
| blocked | 6 | 5 116 |

and of the 9 "movable" files, **three are already shims or module trees** (`providers/mod.rs`, `http/mod.rs`,
`http/quota.rs`), so the moveable remainder is smaller still.

## What actually moved in Y-2

The genuinely pure, dependency-free remainder, following the X-2 pattern (path-mirroring
`refact_providers::http::*`, engine side reduced to a re-export):

| item | from | to |
|---|---|---|
| `ensure_oauth_identity_for_instance_sync` | `providers/http/oauth.rs` | `refact_providers::http::oauth` |
| `stale_quota_fallback` | `providers/http/quota_cache.rs` | `refact_providers::http::quota_cache` |
| `invalidate_provider_quota` | `providers/http/quota_cache.rs` | `refact_providers::http::quota_cache` |
| `SimplifiedModel`, `ModelsResponse`, `ModelsQueryParams`, `AvailableModelsResponse`, `OpenRouterHealthResponse`, `GoogleGeminiHealthResponse`, `ModelToggleRequest`, `ModelProviderRequest`, `AddCustomModelRequest`, `RemoveCustomModelRequest`, `impl From<&ProviderModel> for SimplifiedModel` | `providers/http/models.rs` | `refact_providers::http::models` |

The engine keeps the axum/`ChatCtx`/`AppState` layer and re-exports the rest, so every existing path — including the
`use super::*` prelude consumed by `http/tests.rs` — resolves unchanged. Behaviour is untouched: identical HTTP request
shapes, identical SSE frames, identical error strings; moved bodies are byte-identical modulo whitespace, asserted by
`tools/dev/y2-check.py` against the `9b6054411` baseline.

## The last blocking symbol (the 66-line file)

The census's single "blocked 1 file / 66 lines" is `providers::http` — that is **`providers/http/mod.rs`**, a 68-line
module tree plus prelude, not movable code. Its only real engine import is the one the census named:

> **`crate::caps::model_caps::get_model_caps`** (`refact-agent/engine/src/caps/model_caps.rs:13`)

Its signature is `async fn get_model_caps(gcx: Arc<GlobalContext>, force_refresh: bool)`, so its home crate is
**`refact-runtime-api`** (the crate that owns `GlobalContext`/`ChatCtx`/`AppState`), not `refact-providers`.
`refact-runtime-api` already depends on `refact-providers`; the reverse edge is a cycle, which is why no engine file
with a `ChatCtx`/`GlobalContext`/`AppState` parameter can move into `refact-providers` at all.

## Real blocker inventory (what a `providers` exit still needs)

| file | prod lines | blocker | destination crate |
|---|---|---|---|
| `providers/oauth_refresh.rs` | 1 486 | `Arc<GlobalContext>`, `app_state_from_gcx`, `buddy::actor::{make_runtime_event, buddy_enqueue_event}`; tests need the engine `make_test_gcx` (it installs `EngineContextFactory`) | `refact-runtime-api` |
| `providers/http/tests.rs` | 1 596 (test-only) | `crate::global_context::tests::make_test_gcx` | follows its subjects |
| `providers/http/usage.rs` | 759 | island → `oauth_refresh::force_refresh_claude_code_for_retry` | `refact-runtime-api` |
| `providers/http/models.rs` | 653 | `State<AppState>`, `get_model_caps(Arc<GlobalContext>)` | `refact-http` |
| `providers/http/oauth.rs` | 666 | `State<AppState>` | `refact-http` |
| `providers/http/listing.rs` | 398 | island → `oauth_refresh::refresh_claude_code_instance_if_needed` | `refact-http` |
| `providers/http/defaults.rs` | 204 | `State<AppState>` + `ctx.buddy` drafts | `refact-http` |
| `providers/http/oauth_callback.rs` | 169 | `State<AppState>` | `refact-http` |
| `providers/http/quota_cache.rs` | 188 | `ChatCtx` + `Arc<GlobalContext>` | `refact-runtime-api` |
| `providers/pricing.rs` | 100 | `Arc<GlobalContext>` / `ChatCtx` (`ctx.caps.try_load_caps_quickly`) | `refact-runtime-api` |

Naming, not line count, is the gate: the dominant module blocker is `crate::app_state::AppState`, whose home is
`refact-runtime-api`, so the handler files belong in `refact-http` (the U-8 precedent — it already deps
`refact-runtime-api`) and the ctx-bound logic in `refact-runtime-api`. **Y-2 could not have closed the module inside
this card and no card scoped to `refact-providers` ever can.**

## Verification

```
python tools/dev/y2-check.py                                  # VERIFY: PASS (75 checks)
powershell -NoProfile -ExecutionPolicy Bypass -File tools/dev/y2-grep.ps1   # AC1 grep, no ';'
tools/dev/engine-build.ps1 -Mode check                        # exit 0, 693 s
tools/dev/engine-build.ps1 -Mode check-tests                  # exit 101 (9 PRE-EXISTING errors)
git diff --check                                              # exit 0
```

`-Mode check` (the lib target) is green with the same 54 warnings as the `9b6054411` baseline.

`-Mode check-tests` fails with **exactly the 9 pre-existing errors** in `src/files_in_workspace/tests.rs`
(`IndexingEverywhere` ×8, `is_valid_file` ×1) recorded as a known base red — no new error mentions `providers`,
`refact-providers`, `models.rs`, `oauth.rs` or `quota_cache.rs`. Those two imports are still the fix for that separate
card.
