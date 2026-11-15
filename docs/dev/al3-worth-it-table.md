# AL-3 — remaining destination-crate cycles, and the worth-it table for buddy / integrations / providers

Card AL-3 had two jobs: kill any remaining `BLOCKED on a destination-crate cycle`
group, and produce the worth-it / needs-a-seam / not-worth-it judgement for the
three modules that never got one (AB-3 did it for `http` only).

Both answers are computed, not asserted. `tools/dev/al3-check.py` is the gate
(`VERIFY: PASS (22/22 checks)`, no `;` in the command) and it drives
`tools/dev/al3_edges.py`, a source-derived edge resolver written for this card
because the census loses or invents edges in five shapes (see the resolver
docstring).

## 1. The cycle: no engine change was needed, and none is possible

The census knows exactly one destination (`DEST_HINTS = {"tools":
"refact-tools"}`), so it can only ever report a cycle for `tools`:

| module | census line | before (AB-4) | after (AL-3) |
|---|---|---|---|
| `tools` | `BLOCKED on a destination-crate cycle (dest=refact-tools)` | 0 files / 0 lines | **0 files / 0 lines** |
| `buddy` | same class, `(destination crate unknown, not checked)` | 0 / 0 | 0 / 0 |
| `integrations` | idem | 0 / 0 | 0 / 0 |
| `providers` | idem | 0 / 0 | 0 / 0 |
| `http` | idem | 0 / 0 | 0 / 0 |

**There is no non-empty `BLOCKED on a destination-crate cycle` line anywhere, so
there was no cycle to break.** AB-4's fix still holds (`tools` 0/0) and every other
tree is simply "not checked" — the census cannot see a cycle it has no destination
for.

Running the census's own `cycle_blocker()` rule with a declared destination
(`al3-check.py`, section A) shows the cycles the census is blind to. The counts
cover only files that are **engine-free**: a file with engine-resident edges is
blocked by code, not by a crate cycle, and labelling it a cycle hides the real
blocker (the census does exactly that — see the fix list in section 4).

| module | `refact-buddy-core` | `refact-providers` | `refact-integrations` | `refact-http` | `refact-runtime-api` | **residual (no candidate works)** |
|---|---|---|---|---|---|---|
| `tools` | — | — | — | — | — | **0 / 0** |
| `buddy` | 3 files / 509 lines | — | — | — | 0 / 0 | **0 / 0** |
| `integrations` | — | — | 0 / 0 | — | 0 / 0 | **0 / 0** |
| `providers` | — | 3 / 1 863 | — | 0 / 0 | 2 / 728 | **0 / 0** |
| `http` | — | — | — | 0 / 0 | — | **0 / 0** |

### The exact edges

Both flagged sets have AB-4's shape, and the offending crate is always
**`refact-runtime-api`** (or `refact-http`), never an engine module:

* `buddy` — `buddy::jobs::autonomous_chats::memory_jobs` (268),
  `buddy::jobs::autonomous_chats::model_cost` (155) and
  `buddy::observers::task_health` (86) import `refact-runtime-api`, whose
  `Cargo.toml` already lists `refact-buddy-core`. All three **are** free against
  `refact-runtime-api`, so `refact-runtime-api` — not `refact-buddy-core` — is
  their real destination.
* `providers` — `providers::oauth_refresh` (1 135), `providers::http::models`
  (556) and `providers::http::quota_cache` (172) import `refact-runtime-api` and
  `refact-http`, both of which already depend on `refact-providers`.

**No edge was changed.** The edge you *would* change is
`refact-runtime-api -> refact-buddy-core` / `-> refact-providers` (and
`refact-http -> refact-tools` / `-> refact-providers`); that edge is correct as
written, because those crates own the ctx-shaped types these engine files consume.
The cycle is a **placement** decision, not a broken dependency — the AB-4
conclusion, now confirmed for these three trees.

**No new cycle was introduced.** The residual is 0 files / 0 lines for all five
trees, and the two census-destination checks (`tools`, `http`) stay at 0/0.

## 2. The worth-it / needs-a-seam / not-worth-it table

Method, so the numbers can be re-derived: a file leaves the engine when it has no
engine-resident dependency and at least one candidate destination crate that does
not already depend on its crate; a file whose blockers are all themselves movable
is **chain-only** (no new seam, it only needs the files in front of it to move).
`#[cfg(test)]` subtrees are excluded — the census marks only the direct child
`buddy::tests` as test-only, so the nine files that child declares (10 650 lines)
were being counted as production mass.

Buddy's free set differs from the census's `MOVABLE now: 3 files / 586 lines`
because the census cannot see `use super::*` / `pub mod` edges: the files that are
genuinely free are `memory_jobs`, `model_cost` and `task_health` (509 lines), plus
`local_jobs` (288) behind a shim. `buddy::observers` — which the census calls
MOVABLE — actually names its eleven observer children, so it cannot move before
they do.

### buddy — 36 production files, 33 blocked / 8 253 lines

| file | lines | verdict | why |
|---|---:|---|---|
| `buddy::jobs::autonomous_chats::memory_jobs` | 268 | **worth one card** | free; lands in `refact-runtime-api` |
| `buddy::jobs::autonomous_chats::model_cost` | 155 | **worth one card** | free; lands in `refact-runtime-api` |
| `buddy::observers::task_health` | 86 | **worth one card** | free; lands in `refact-runtime-api` |
| `buddy::jobs::autonomous_chats::local_jobs` | 288 | **worth one card** | chain-only: its only blocker is the one-line `autonomous_chats::spec` shim |
| `buddy::voice_service` | 676 | **needs a new seam** | `subchat::{config,runner}` + `autonomous_chats::runtime` |
| `buddy::jobs::autonomous_chats::runtime` | 668 | **needs a new seam** | `subchat::*`, `crate::memories::touch_memory_observation`, `crate::chat::prompts::system_prompt_add_extra_instructions`, `voice_service::VoiceIntent` |
| `buddy::interventions::stalled_turn` | 645 | **needs a new seam** | `subchat::*`, `tasks::storage` |
| `buddy::jobs::autonomous_chats::behavior` | 967 | **needs a new seam** | `chat::trajectories::paths`, `crate::memories::{normalize_preference_text_for_dedupe, preference_statement_is_safe}` |
| `buddy::pulse` | 399 | **needs a new seam** | `crate::integrations::mcp::session_mcp::SessionMCP` (the MCP cluster) |
| `buddy::observers::mcp_auth` | 121 | **needs a new seam** | `session_mcp::{SessionMCP,MCPAuthStatus}` |
| 11 `voice_service` consumers* | 1 829 | **worth one card after the subchat seam** | one shared blocker: `voice_service` |
| 13 `autonomous_chats`-bound jobs† | 2 341 | **worth one card after the subchat seam** | shared blockers: `autonomous_chats::{runtime,spec}` |
| `buddy::background` | 717 | **not worth it yet** | four-way in-tree knot: `humor::{HumorPlan,HumorService}`, `observers::build_observer_registry`, `pulse::build_pulse` |
| `buddy::scheduler` | 40 | **not worth it** | composition root — `default_buddy_jobs()` names 20 concrete job types |
| `buddy::actor` | 16 | **not worth it** | last in the chain: `background` + `speech` |
| `buddy::workflows` | 192 | **not worth it yet** | `buddy` (the tree root) + `events::BuddyEvent::StateUpdated` |

\* `chat_reactions` 264, `quest_prompt` 261, `speech` 255, `chat_phrase_bank` 254,
`humor` 184, `stats_watcher` 164, `greeting` 120, `speaker_win` 96, `tour` 89,
`speaker_insight` 81, `speaker_memory_pulse_commentary` 61.

† `buddy_test_coverage_watcher` 348, `buddy_skill_author` 252, `buddy_pr_issue_matchmaker` 195,
`buddy_onboarding` 167, `refact_compile_sniffer` 148, `buddy_daily_digest` 133,
`buddy_friday_retro` 130, `refact_self_critic` 110, `buddy_idle_suggester` 94,
`buddy_refactor_hunter` 88, 13 modules in all (one per job, plus the cluster's own members).

**Buddy ordering (each step releases the next)**

1. **797 lines free now with no new seam** — `memory_jobs` + `model_cost` +
   `task_health` (509, alternate crate) and `local_jobs` (288, behind the `spec`
   shim). One mechanical card.
2. **+1 989 lines** once the subchat crate exists and the `memories` /
   `chat::prompts` seams land: `voice_service` 676 + `autonomous_chats::runtime`
   668 + `stalled_turn` 645.
3. **+1 829 lines** then follows with no extra seam — the eleven
   `voice_service` consumers.
4. **+2 341 lines** once `autonomous_chats::runtime` has landed — the
   `autonomous_chats`-bound jobs (including `behavior`'s 967, which also needs the
   trajectories + memories seams).
5. Residue ≈ 1 500 lines (`background`, `pulse`, `observers`, `actor`,
   `workflows`, `scheduler`) is the composition layer: **not worth a card.**

### integrations — 13 production files, 13 blocked / 4 450 lines

| file | lines | verdict | why |
|---|---:|---|---|
| `integrations::mcp::integr_mcp_common` | 1 388 | **worth one card, as one unit** | the cluster hub: 15 in-tree symbols across `session_mcp`, `mcp_resources`, `mcp_sse_transport`, `tool_mcp` |
| `integrations::mcp::session_mcp` | 964 | idem | `mcp_resources::*`, `mcp_sampling::*` |
| `integrations::mcp::tool_mcp` | 624 | idem | six `session_mcp` symbols |
| `integrations::mcp::mcp_sse_transport` | 287 | idem | `integr_mcp_common::{build_auth_client_for_mcp,build_reqwest_client_for_mcp}`, `session_mcp::add_log_entry` |
| `integrations::mcp::integr_mcp_stdio` | 253 | idem | `integr_mcp_common::*`, `session_mcp::*` |
| `integrations::mcp::mcp_prompts` | 252 | idem | `session_mcp::SessionMCP` |
| `integrations::mcp::mcp_resources` | 170 | idem | `session_mcp::add_log_entry` |
| `integrations::mcp::mcp_auth` | 117 | idem | `session_mcp::{MCPAuthStatus,SessionMCP}` |
| `integrations::mcp::integr_mcp_http` | 107 | idem | `integr_mcp_common::*`, `session_mcp::*` |
| `integrations::mcp::integr_mcp_sse` | 104 | idem | `integr_mcp_common::*`, `session_mcp::*` |
| `integrations::mcp::mcp_sampling` | 97 | **worth one card after the subchat seam** | its only blocker is `subchat::runner` |
| `integrations` (mod) | 70 | **not worth it** | the host tree; blocked by `mcp` + `running_integrations` |
| `integrations::running_integrations` | 17 | **not worth it** | the `IntegrationProvider` seam (T-5) holds `integration_from_name` / `integrations_list` in the engine on purpose |

**Verdict: the whole blocked mass is one unit.** The MCP cluster is ten files /
4 353 lines that are mutually self-referential (`session_mcp` ↔
`integr_mcp_common` ↔ `tool_mcp`), with **no out-of-tree seam except
`subchat::runner` for the 97-line `mcp_sampling`**. `integrations` has **0 files /
0 lines cycle-blocked against both candidate destinations**, so it is a pure move:
**worth exactly one card, moving `integrations::mcp::*` into `refact-integrations`
behind a glob shim.**

### providers — 7 production files: 4 blocked / 1 955 lines, 3 free / 1 863 lines

| file | lines | verdict | why |
|---|---:|---|---|
| `providers::oauth_refresh` | 1 135 | **worth one card** | engine-free now; free for `refact-http` and `refact-runtime-api` |
| `providers::http::models` | 556 | **worth one card** | engine-free now; free for `refact-http` |
| `providers::http::quota_cache` | 172 | **worth one card** | engine-free now (the census already calls it MOVABLE) |
| `providers::http::oauth` | 630 | **worth one card** | chain-only: single blocker `quota_cache` |
| `providers::http::oauth_callback` | 169 | **worth one card** | chain-only: single blocker `quota_cache` |
| `providers::http::usage` | 758 | **needs a new seam, then mechanical** | `oauth_refresh::force_refresh_claude_code_for_retry` + the `quota`/`settings` shims |
| `providers::http::listing` | 398 | **needs a new seam, then mechanical** | `oauth_refresh::refresh_claude_code_instance_if_needed` + `models`/`quota`/`settings` |

**Verdict: every blocker is inside the tree — 0 lines are held by an out-of-tree
engine symbol.** The order is `oauth_refresh` + `quota_cache` + `models` (1 863
lines, move now), then `oauth` + `oauth_callback` (799, chain-only), then `usage` +
`listing` (1 156). Y-2 already established the pattern (engine side reduced to a
`pub use` shim). **Worth 1–2 cards; the whole tree can go.**

## 3. What was acted on

Nothing in the engine, deliberately:

* **no cycle existed**, so there was no edge to change (section 1);
* the one group that is both *worth one card* and *purely mechanical* —
  `providers::http::quota_cache` (172 lines) — is already reported MOVABLE by the
  census, so there is no action left inside this card;
* every other mechanical move is a crate-sized move of a hot path
  (`oauth_refresh` 1 135 lines, the MCP cluster 4 353 lines) whose only honest
  verification is `engine-build.ps1 -Mode check`, which agents cannot run in this
  task (the planner proves every build). Carding them is the correct next step;
  doing them blind is not.

## 4. Follow-up cards

1. **`providers` exit, part 1** — move `providers::oauth_refresh` (1 135),
   `providers::http::{quota_cache, models}` (728) behind glob shims; tests needing
   `make_test_gcx` stay in the engine. Releases 1 863 lines.
2. **`providers` exit, part 2** — `oauth` + `oauth_callback` (799, chain-only),
   then `usage` + `listing` (1 156). Closes the tree; no new seam needed.
3. **MCP cluster card** — move `integrations::mcp::*` (10 files / 4 353 lines) into
   `refact-integrations` as one unit; `mcp_sampling` additionally wants the subchat
   seam.
4. **subchat seam card** — the AC-1 seam already specified in the master plan.
   Unblocks 1 989 lines in `buddy`, then 1 829 more behind `voice_service` and
   2 341 behind `autonomous_chats::runtime`, plus `integrations::mcp::mcp_sampling`
   and 520 lines of `buddy` MCP observers.
5. **census fix (script-only)** —
   (a) resolve a path segment by segment through each module's own `use` / `mod`
   bindings instead of looking only the final segment up in a `pub use`-only
   table;
   (b) mark a `#[cfg(test)] mod x;` descendant test-only transitively
   (`buddy::tests::*` is 9 files / 10 650 lines of phantom production mass);
   (c) report a *latent* cycle for a tree with no `DEST_HINTS` entry, and only mark
   a file a cycle when it is engine-free (today an engine-blocked file can be
   labelled a cycle and silently drop out of `LEFT`).

## 5. Verification

```
python tools/dev/al3-check.py                  # VERIFY: PASS (22/22 checks)
python tools/dev/ab4-check.py                  # AB-4 checks: 137 run, 0 failed
python tools/dev/ab2-check.py                  # RESULT: PASS
python tools/dev/chat_movable_census.py tools  # cycle class still 0 / 0
```

No Rust file was touched, so `engine-build.ps1 -Mode check` is not required by
this card; the census and its consumers are unchanged, which the two existing
harnesses above re-prove.
