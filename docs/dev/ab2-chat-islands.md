# AB-2 — `chat`: settle the two largest islands, and what that actually releases

Base `3af711082` · worktree `907cd153-382b-4850-9bef-0a9375155e74`.

Every number below is **measured**, by `tools/dev/chat_movable_census.py` and by
`tools/dev/ab2-check.py` (the settlement simulation added by this card). Run both:

```
python tools/dev/chat_movable_census.py chat
python tools/dev/ab2-check.py
```

---

## 1. Census, verbatim

`python tools/dev/chat_movable_census.py chat` at base:

```
### chat: 110 files / 30811 production lines
test-only modules (stay in engine): 32 files / 0 lines
declared islands (never harvested here): 19 files / 12821 lines
BLOCKED on a destination-crate cycle (destination crate unknown, not checked): 0 files / 0 lines
module hosts (destination tree roots, never move): 5 files / 241 lines
already-shim (pure crate facade): 29 files / 94 lines
MOVABLE now: 1 files / 501 lines
BLOCKED on another island (that island needs its own card first): 13 files / 9320 lines
LEFT (blocked on engine code): 11 files / 7834 lines
[classes cover 30811 of 30811 production lines]
```

After this card's settlements the same command reports:

```
### chat: 110 files / 29445 production lines
test-only modules (stay in engine): 32 files / 0 lines
declared islands (never harvested here): 19 files / 11455 lines
BLOCKED on a destination-crate cycle (destination crate unknown, not checked): 0 files / 0 lines
module hosts (destination tree roots, never move): 5 files / 241 lines
already-shim (pure crate facade): 29 files / 94 lines
MOVABLE now: 1 files / 501 lines
BLOCKED on another island (that island needs its own card first): 12 files / 9243 lines
LEFT (blocked on engine code): 12 files / 7911 lines
[classes cover 29445 of 29445 production lines]
```

1 366 production lines left `chat` in this card
(`stream_core` 1 022 + `cloud_token_count` 273 + `summarization` 83, minus the
residue the shims keep).

### MOVABLE now

```
     501  chat::task_agent_monitor::monitor                        crates=refact_tasks,refact_tools
```

### DECLARED ISLANDS (own card each, never harvested here)

```
    2970  chat::stream_core                                        blocker=chat::cache_guard
    2341  chat::tools                                              blocker=chat::goal_verifier
    1774  chat::perf_harness::runners                              blocker=chat::queue::dispatch
    1575  chat::prompts                                            blocker=files_correction
     778  chat::verifier                                           blocker=chat::verify_cmd
     643  chat::perf_harness::types                                blocker=-
     595  chat::perf_harness::validation                           blocker=-
     592  chat::perf_harness::fixtures                             blocker=background_tasks
     324  chat::perf_harness::metrics                              blocker=-
     281  chat::goal_verifier::monitor                             blocker=-
     273  chat::cloud_token_count                                  blocker=-
     222  chat::cache_guard                                        blocker=tokens
     138  chat::notifications                                      blocker=exec
     115  chat::perf_harness::turn_memory                          blocker=-
      83  chat::summarization                                      blocker=-
      75  chat::perf_harness                                       blocker=app_state
      31  chat::goal_verifier                                      blocker=chat::prepare
      10  chat::ctx                                                blocker=subchat
       1  chat::perf_diagnostics                                   blocker=-
```

Two further entries the script also tags as islands are `#[cfg(test)]` child
modules and carry no production lines: `chat::perf_harness::tests`,
`chat::goal_verifier::tests`. So the card's "19 islands / 12 821" is 19 islands
with production lines; the script's raw island set is 21.

### MODULE HOSTS (tree roots; never move)

```
      91  chat::trajectories
      68  chat
      53  chat::queue
      16  chat::generation
      13  chat::context_rebuild
```

### LEFT (blocked on engine code)

```
    1681  chat::trajectories::queries   blocker=chat::trajectories::load   others: chat::trajectories::paths, chat::trajectories::reload, chat::trajectories::watcher, subchat::from_parts
    1316  chat::trajectories::store     blocker=chat::trajectories::load   others: chat::trajectories::paths
    1054  chat::trajectories::paths     blocker=chat::trajectories::load
     986  chat::trajectories::load      blocker=chat::trajectories::paths   others: chat::trajectories::reload, chat::trajectories::watcher
     721  chat::trajectories::watcher   blocker=chat::trajectories::paths   others: chat::trajectories::reload
     691  chat::prepare                 blocker=chat::tools::default
     663  chat::trajectories::reload    blocker=chat::trajectories::watcher
     349  chat::queue::helpers          blocker=git::checkpoints
     326  chat::session_runtime         blocker=agents::AgentListFilter     others: chat::session, exec
      36  chat::session                 blocker=chat::session_runtime
      11  chat::verify_cmd              blocker=tools::shell_gate
```

---

## 2. The two largest islands

`chat::stream_core` (2 970 prod lines) and `chat::tools` (2 341 prod lines) — 5 311
of the 12 821 island lines.

Neither is blocked by the chat graph in the way the island category suggests.
The census records their *real* edge sets:

| island | crate edges (census `crates=`) | engine edges (census `engine=`) | chat-internal `deps=` |
|---|---|---|---|
| `chat::stream_core` | `refact_caps_core/refact_runtime_api`, `refact_chat_api`, `refact_chat_history`, `refact_core`, `refact_llm`, `refact_perf`, `refact_privacy`, `refact_runtime_api` | `crate::providers::*` (12 symbols: `oauth_refresh`, `OpenAIAuth*`, `XAIOAuthProvider`, `llm_http_retry`, `runtime_credential`, `claude_code`, `redact`, `resolve`, …), `crate::app_state::AppState`, `crate::call_validation::{ChatUsage,ChatToolCall,ChatToolFunction}`, `crate::chat::{ctx,diagnostics,perf_diagnostics,types,cache_diagnostics,trajectory_ops}` | `chat::cache_guard` |
| `chat::tools` | `refact_buddy_core`, `refact_chat_api`, `refact_chat_history`, `refact_core`, `refact_exec`, `refact_ext`, `refact_llm`, `refact_perf`, `refact_privacy`, `refact_runtime_api`, `refact_tool_api`, `refact_tools`, `refact_yaml_configs` | **none** | `chat::goal_verifier`, `chat::trajectories` |

So each island is really two layers glued together:

* a **crate-typed protocol layer** — pure functions and types whose whole world is
  those `refact_*` crates, and
* an **engine-coupled transport/orchestration layer** — `AppState`, the provider
  registry, `ChatCtx`, the session, subchat.

`chat::tools` has no engine edge at all: its two blockers are `chat::goal_verifier`
(31 prod lines) and `chat::trajectories` (a 4-file, 3 424-line SCC — see §4).

### What this card settled

Both islands were settled to the extent their crate-typed layer allows, by the
pattern the plan mandates: **crate body + an engine `pub use` shim**.

| island | settled into | moved | engine residue |
|---|---|---|---|
| `chat::stream_core` | `refact-llm::stream_core` (new) | 1 022 prod lines: `merge_usage`, `LlmStreamError`, `StreamRunParams`, `ChoiceFinal`, `StreamCollector`, `ReplayCollector`, `PartialOutputCollector`, `ChoiceAccumulator`, the `think`/`<function_calls>` parsers, delta-op emission, `process_stream_event_data`, NDJSON framing, `finalize_accumulators`, `merge_thinking_blocks`, `normalize_tool_call` | 4 226 lines: `run_llm_stream*`, the HTTP/WebSocket request paths, provider refresh/credential helpers, `read_error_body_bounded`, `format_llm_error_body`, abort plumbing, `ABORT_ERROR_MESSAGE`, `LlmStreamOutcome`, and the whole kept `#[cfg(test)]` module |
| `chat::tools` | *not settled by this card* | — | — |
| `chat::summarization` (83) | `refact-chat-history::summarization` | all of it — `census engine=[] deps=[]` | 1-line `pub(crate) use` |
| `chat::cloud_token_count` (273) | `refact-runtime-api::cloud_token_count` | all of it | `pub use` + the kept tests |

`chat::summarization` and `chat::cloud_token_count` were picked because both are
crate-typed right now (`engine=[] deps=[]`), i.e. they are islands only because
`ISLANDS` lists them.

`chat::tools` is deliberately **not** moved as a whole: its 2 341 lines are
`process_tool_calls_once` / `execute_tools*` / `check_tools_confirmation`, which
need `AppState`, `AtCommandsContext`, the tool registry, the hook runner and the
`refact-tools` catalog. That is a card about a **trait seam on `ChatCtx`** (the
T-17/T-22/L-4/Z-1 precedent), not a card about its crate edges — see §5.

---

## 3. The 13 island-blocked files, paired with their island

Verbatim from the census:

| file | prod lines | island the census names |
|---|---|---|
| `chat::queue::dispatch` | 1 789 | `chat::tools` |
| `chat::generation::stream` | 1 085 | `chat::cloud_token_count` |
| `chat::task_agent_monitor::broadcast` | 941 | `chat::tools` |
| `chat::context_rebuild::pipeline` | 783 | `chat::prompts` |
| `chat::generation::run_loop` | 742 | `chat::prompts` |
| `chat::handlers` | 718 | `chat::tools` |
| `chat::generation::preamble` | 713 | `chat::prompts` |
| `chat::generation::recovery` | 713 | `chat::prompts` |
| `chat::goal_monitor` | 591 | `chat::tools` |
| `chat::task_agent_monitor::finish` | 510 | `chat::tools` |
| `chat::queue::setparams` | 489 | `chat::prompts` |
| `chat::delivery` | 169 | `chat::tools` |
| `chat::generation::retry` | 77 | `chat::stream_core` |

Total 9 320 lines.

After this card's settlements the same class holds **12 files / 9 243 lines**: the
one that left is `chat::generation::retry` (77 lines), which stopped being
island-blocked because the island it named (`chat::stream_core`) is no longer an
island — it is now blocked on the host `chat::generation` instead, so the census
moves it to `LEFT` (11 → 12 files). No file became *movable*; see §4.

---

## 4. How many of the 13 became movable

**Zero, and no island settlement can change that.** (Post-settlement census: the
class is 12 files / 9 243 lines; `chat::generation::retry` moved to `LEFT` because
its named island is no longer an island. `MOVABLE now` is still the same single
file, and the tree total dropped 30 811 → 29 445 production lines.)

`tools/dev/ab2-check.py` re-runs the census closure with a settlement set `S`
*removed from the graph* — `S` out of `files`, `S` out of every other module's
`engine`/`deps` edges, `S` out of `ISLANDS` — which is what "settled to a crate +
shim" means. Results:

| simulation | islands settled | of the 13 released |
|---|---|---|
| A — the two largest | `stream_core`, `tools` | **0** |
| B — every island whose own blocker is `-` | 10 | **0** |
| C — all islands (upper bound) | 19 | **1** (`chat::queue::setparams`) |

Individually, settling *one* island releases at most one file
(`chat::prompts` → `chat::queue::setparams`); every other island releases none.

Why: `census.island_blocker()` walks `info[mod]["deps"]`, which is the
**chat-internal** edge set only. It answers "is there an island somewhere below
me in `chat`?" — it never looks at `info[mod]["engine"]`, the non-island engine
modules that must also go. Both facts are true of the same file, and the island
one is incidental. Concretely, with all 19 islands settled the 12 survivors'
next blockers are:

```
chat::context_rebuild::pipeline   next=chat::prepare
chat::delivery                    next=chat::queue::dispatch
chat::generation::preamble        next=chat
chat::generation::recovery        next=chat::context_rebuild::pipeline
chat::generation::retry           next=chat::generation
chat::generation::run_loop        next=buddy::interventions::stalled_turn
chat::generation::stream          next=chat::generation
chat::goal_monitor                next=chat::queue::dispatch
chat::goal_verifier               next=chat::prepare
chat::handlers                    next=agents::delivery
chat::perf_harness                next=app_state
chat::perf_harness::runners       next=chat::queue::dispatch
chat::queue                       next=chat::generation
chat::queue::dispatch             next=chat::delivery
chat::queue::setparams            —
chat::stream_core                 next=chat::cache_guard
chat::task_agent_monitor::broadcast  next=chat::delivery
chat::task_agent_monitor::finish  next=chat::queue::dispatch
chat::tools                       next=chat::goal_verifier
```

Note `chat::delivery` ↔ `chat::queue::dispatch` and
`chat::generation::{run_loop,stream}`, `{preamble,recovery}`: the chat tree has
**five production SCCs**, so most of the 13 can never be released one at a time:

```
- SCC (4 files / 3424 lines): chat::trajectories::{load,paths,reload,watcher}
- SCC (2 files / 1958 lines): chat::delivery, chat::queue::dispatch
- SCC (2 files / 1827 lines): chat::generation::run_loop, chat::generation::stream
- SCC (2 files / 1426 lines): chat::generation::preamble, chat::generation::recovery
- SCC (2 files /  362 lines): chat::session, chat::session_runtime
```

`chat::trajectories` is the real bottleneck for `chat::tools` too (its second
blocker, and the `blocker=` on 4 of the 11 `LEFT` files, 4 448 of 7 834 lines).
Those four files are already a crate-only unit — `refact-chat-history` exists for
exactly them — and each carries only engine-local `crate::global_context`-shaped
coupling to sort out.

---

## 5. Strategy per remaining island

Sized from the measured `engine=`/`deps=` sets, not from the census's one-word
`blocker=` column.

| island | prod lines | measured shape | strategy |
|---|---|---|---|
| `chat::stream_core` | 2 970 → residue **4 226 incl. tests** | crate-typed protocol layer now in `refact-llm`; residue is transport (`reqwest`/`tokio-tungstenite`) + `crate::providers::*` + `AppState` | **split first, then seam.** Extract the residue's provider-registry calls (see §6) behind `ChatCtx` accessors, leaving `crate::providers::*` with exactly one caller; then the HTTP/NDJSON/WebSocket transports are pure enough for `refact-llm` and only `run_llm_stream*` stays. Natural split: `transport_http`, `transport_ws`, `retry`, `run`. |
| `chat::tools` | 2 341 | **no engine edge at all**; blockers are `chat::goal_verifier` (31 lines) and `chat::trajectories` | **card 1:** settle `chat::goal_verifier` (31 lines: `pub use` + `chat::prepare`/`stream_core` seams). **card 2:** settle `chat::trajectories` (below). Then `chat::tools` moves whole into `refact-tools` (which already depends on `refact-tool-api`, `refact-ext`, `refact-exec`, `refact-yaml-configs`). Do not split it before its blockers go. |
| `chat::perf_harness::{runners,types,validation,fixtures,metrics,turn_memory}` | 4 040 | bench-only tree, `#[cfg(any(test, feature = "bench"))]`; blockers are `chat::queue::dispatch`, `background_tasks`, `app_state` | **one card, after `chat::queue::dispatch`.** `types`/`metrics`/`validation`/`turn_memory` have no engine edge — they move as-is. `runners` (1 774) needs a `ChatCtx` seam for `app_state`/`dispatch`. Move the tree into `refact-perf` (it already owns `perf_diagnostics`, `perf_telemetry` and `listing_counters`). |
| `chat::prompts` | 1 575 | blockers are `crate::files_correction` (a shim-less engine module) and `crate::subchat::{unattributed,None}` | **one card.** `files_correction` already has a crate-resident twin in `refact-runtime-api::files_correction*`; the residue is `subchat::unattributed`. Land the `subchat` accessor on `ChatCtx` and this file goes to `refact-chat-api` (it already depends on `refact-files`, `refact-tools`, `refact-scratchpads`). |
| `chat::verifier` | 778 | blockers `crate::exec` + `crate::exec::command_policy` (both shims) and `subchat::{from_parts,None}` | **one card, with `chat::verify_cmd` (11 lines).** `refact-exec` and `refact-runtime-api::command_policy` already own the exec side; only `subchat` needs a seam. Destination `refact-chat-api` (it already deps `refact-tasks`, `refact-worktrees`). |
| `chat::notifications` | 138 | single blocker `crate::exec` | **one small card.** Same `refact-exec` route as `chat::verifier`; destination `refact-chat-api`. |
| `chat::cache_guard` | 222 | single blocker `crate::tokens`; already re-exports `refact-chat-api::cache_guard` | **one small card.** `crate::tokens` is a 1-line shim over `refact-runtime-api::chat_tokens` + `refact-core::tokens_runtime`; the residue is the `AppState`-taking `is_guard_enabled`/`check_or_pause_cache_guard`/`estimate_extra_cache_miss_usd`. Put the pure parts beside the existing `refact-chat-api::cache_guard` and the three `AppState` functions in `refact-runtime-api`. |
| `chat::cloud_token_count` | 273 | **settled by this card** | done — `refact-runtime-api::cloud_token_count` + `pub use` shim. |
| `chat::goal_verifier::monitor` | 281 | `engine=[] deps=[]` — already crate-typed | **trivial card**: move to `refact-chat-api` (re-exported) or `refact-runtime-api` and leave a shim. It is an island only because `chat::goal_verifier` is. |
| `chat::goal_verifier` | 31 | deps `chat::prepare`, `chat::stream_core` | **small card**: it is a `pub use` + a 100-line `AppState`-taking entry — put the entry in `refact-runtime-api` after `chat::stream_core`'s seam lands. Unblocks `chat::tools`. |
| `chat::summarization` | 83 | **settled by this card** | done — `refact-chat-history::summarization` + shim. |
| `chat::ctx` | 10 | `engine=[subchat]` | **tiny card**: pure re-export; one `subchat` symbol (`drain_background_agent_inbox`, `install_stateful_subchat_session`) keeps it in the engine. Move with the `subchat` seam wave. |
| `chat::perf_diagnostics` | 1 | pure facade over `refact-perf` | already a shim; it is an island only because `ISLANDS` lists the name. **No work.** |
| `chat::perf_harness` (root) | 75 | `engine=[app_state]` | module host for the perf tree; moves with it. |
| `chat::perf_harness::fixtures` | 592 | blocker `background_tasks` | moves with the perf tree card. |

Ordering that actually unwedges things, by measured release count:

1. **`chat::trajectories`** (the only island that unblocks anything material):
   4 448 `LEFT` lines + `chat::tools`'s second blocker. Target crate exists.
2. **`chat::goal_verifier`** (31 lines) → releases `chat::tools`, which is
   2 341 lines with **no engine edge**.
3. **`chat::queue::dispatch` + `chat::delivery`** (SCC, 1 958 lines) →
   `chat::perf_harness::runners`, `chat::task_agent_monitor::finish`,
   `chat::goal_monitor`.
4. **`chat::generation`** (two 2-file SCCs, 3 253 lines) → `chat::queue`,
   `chat::generation::retry`.
5. **`chat::prompts`** → `chat::queue::setparams`, and the `context_rebuild` chain.

---

## 6. Follow-ups this card deliberately did not do

* `chat::stream_core`'s residue still names `crate::providers::*` (12 symbols) and
  `AppState`. Moving that needs `ChatCtx` accessors (`refresh_openai_codex`,
  `refresh_claude_code`, `rate_limit_headers`, `resolve_credential`,
  `redact_credential`, `header_retry_config`) — a seam card, not a harvest.
* `chat::tools` needs `chat::goal_verifier` + `chat::trajectories` first, and then
  it is one whole-file move (no engine edge at all). Do not split it earlier.
* The census's `island_blocker()` should report the *non-island* engine blocker
  too; today it makes 9 320 lines look island-gated when they are not.
