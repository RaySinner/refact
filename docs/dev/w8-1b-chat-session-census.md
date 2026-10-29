# W8-1b census: `ChatSession` (96 fields) + `SessionsMap` + `ChatCtx` → `refact-chat-api`

Date: 2026-10-29. Method: **static only** (git grep / Select-String / file reads).
No `cargo`, no `rustc` (rule #0, AC "Сборка НЕ запускалась").

## Verdict: **STOP** (Spec п.1 stop-condition, plus three architectural blockers)

`ChatSession` has **96 fields** (the card says 86). By the census definition
("field type declared in the engine") **6 fields are engine-typed**, i.e. `6 > 5`
→ the card's own stop-condition fires and the body is **not** moved.

Independently of the field count, the move is impossible in one card because:

| # | Blocker | Evidence | Why it blocks |
|---|---|---|---|
| **A** | **Dependency cycle** | `refact-runtime-api/Cargo.toml:7` → `refact-chat-api`; `refact-chat-history/Cargo.toml:8` → `refact-chat-api`. ChatSession fields 65/66 are `refact_runtime_api::{ToolCatalogSnapshot, TurnToolPool}` (`crates/refact-runtime-api/src/lib.rs:93,100`), field 32 is `refact_chat_history::TrajectoryEvent` (`crates/refact-chat-history/src/trajectory_event.rs:5`). | `refact-chat-api` cannot depend on either crate. Those two type families must be pushed **down** into chat-api first (separate card). |
| **B** | **24 inherent `impl ChatSession` blocks in 13 engine files** | `app_state.rs`, `chat/session/{construct,goal,messages,queue,runtime,snapshot,stream}.rs`, `tools/tool_agent_{chat_summary,lifecycle,pulse,steer}.rs`, `tools/tool_task_broadcast.rs` | Inherent impls on a foreign type are `E0116`. The whole session method surface must become free functions (call-site churn: `ChatSession` = 801 occurrences / 109 files) or move into the crate together with everything it calls. |
| **C** | **10 `pub(crate)` fields** | `compression_estimated_tokens`, `compression_effective_cap`, `compression_cap_source`, `compression_attempt_generation`, `active_compression_attempt`, `compression_attempt_started_at_ms`, `compression_abort_flag`, `wait_interrupt_cause`, `openai_codex_websocket`, `enrichment_identities` | `pub(crate)` is engine-only; from a crate they are unreachable, so each must be widened to `pub` (visibility leak) or replaced by accessors. |
| **D** | **`ChatCtx` is not crate-expressible yet** | `chat/ctx.rs:285`, 25 fields: 4 engine structs (`ChatWorkspace` ctx.rs:38, `ChatIntegrationScope` ctx.rs:50, `ChatServices` app_state.rs:70, `BuddyServices` app_state.rs:82) + 7 engine traits (`SubchatLauncher` ctx.rs:55, `ProjectTreeProvider` :147, `HookRunner` :152, `ExecPolicyProvider` :157, `TrajectoryStore` :162, `PromptAssembler` :258, `ChatToolExecutor` :272) | `ChatServices.sessions` **is** `SessionsMap`, so ChatCtx is transitively blocked by ChatSession; the engine structs are blocked by W7-4 (AppState). |

## 1. Prestate (AC 5)

```
git grep -n "pub struct ChatSession|pub type SessionsMap" -- refact-agent/engine
refact-agent/engine/src/chat/session/mod.rs:121:pub type SessionsMap = Arc<ARwLock<HashMap<String, Arc<AMutex<ChatSession>>>>>;
refact-agent/engine/src/chat/types.rs:287:pub struct ChatSession {
```

In `refact-agent/engine/crates` there are **0** matches for `pub struct ChatSession`
/ `pub type SessionsMap` (only `ChatSessionSnapshot` / `ChatSessionUpdate` DTOs in
`refact-runtime-api/src/lib.rs:300,309`).

Sizes (engine `src/`):

| Symbol | Occurrences / files |
|---|---|
| `ChatSession` | 801 / 109 |
| `chat_sessions` | 141 / 44 |
| `impl ChatSession` | 24 blocks / 13 files |
| `ChatCtx` | `chat/ctx.rs:285`, 25 fields |

## 2. Census: `ChatSession` — 96 fields (`chat/types.rs:287-396`)

Legend for `home`: ✅ = type already lives in a workspace crate (engine path is a
`pub use` shim or a direct crate path), ❌ = type declared in the engine.

| # | Field | Declared type | home |
|---|---|---|---|
| 1 | `chat_id` | `String` | std ✅ |
| 2 | `derived_privacy_zones` | `crate::privacy::records::DerivedPrivacyZones` | **engine ❌** — alias `Arc<RwLock<HashMap<PathBuf, DerivedZone>>>` (`privacy/records.rs:39`), `DerivedZone` at `privacy/records.rs:31` |
| 3 | `tool_loop_guard` | `crate::tools::tool_call_loop_guard::ToolCallLoopGuard` | `refact-tool-api` ✅ (shim `tools/tool_call_loop_guard.rs:1`) |
| 4 | `turn_investigation_tracker` | `crate::tools::tool_call_loop_guard::TurnInvestigationTracker` | `refact-tool-api` ✅ (`tool_call_loop_guard.rs:411`) |
| 5 | `thread` | `ThreadParams` | `refact-chat-api` ✅ |
| 6 | `messages` | `Vec<ChatMessage>` | `refact-core` ✅ (shim `call_validation.rs:10`) |
| 7 | `runtime` | `RuntimeState` | `refact-chat-api` ✅ |
| 8 | `goal` | `Option<GoalSnapshot>` | `refact-chat-api` ✅ |
| 9 | `goal_active` | `bool` | std ✅ |
| 10 | `goal_status` | `Option<GoalStatus>` | `refact-chat-api` ✅ |
| 11 | `goal_turns_used` | `u32` | std ✅ |
| 12 | `goal_tokens_used` | `u64` | std ✅ |
| 13 | `goal_no_progress_turns` | `u32` | std ✅ |
| 14 | `pending_context_rebuild` | `Option<super::context_rebuild::PendingContextRebuild>` | **engine ❌** — `chat/context_rebuild.rs:22`, 2 plain fields |
| 15 | `pending_mode_handoff` | `Option<serde_json::Value>` | serde_json ✅ |
| 16 | `last_rebuild_attempt_version` | `Option<u64>` | std ✅ |
| 17 | `is_compressing` | `bool` | std ✅ |
| 18 | `compression_phase` | `Option<CompressionPhase>` | `refact-chat-api` ✅ |
| 19 | `compression_reason` | `Option<CompressionReason>` | `refact-chat-api` ✅ |
| 20 | `compression_estimated_tokens` | `Option<usize>` (pub(crate)) | std ✅ |
| 21 | `compression_effective_cap` | `Option<usize>` (pub(crate)) | std ✅ |
| 22 | `compression_cap_source` | `Option<String>` (pub(crate)) | std ✅ |
| 23 | `compression_attempt_generation` | `u64` (pub(crate)) | std ✅ |
| 24 | `active_compression_attempt` | `Option<u64>` (pub(crate)) | std ✅ |
| 25 | `compression_attempt_started_at_ms` | `Option<u64>` (pub(crate)) | std ✅ |
| 26 | `compression_abort_flag` | `Option<Arc<AtomicBool>>` (pub(crate)) | std ✅ |
| 27 | `draft_message` | `Option<ChatMessage>` | `refact-core` ✅ |
| 28 | `draft_usage` | `Option<ChatUsage>` | `refact-core` ✅ |
| 29 | `command_queue` | `VecDeque<CommandRequest>` | `refact-chat-api` ✅ |
| 30 | `event_seq` | `u64` | std ✅ |
| 31 | `event_tx` | `broadcast::Sender<Arc<String>>` | tokio ✅ |
| 32 | `trajectory_events_tx` | `Option<broadcast::Sender<super::trajectories::TrajectoryEvent>>` | `refact-chat-history` ✅ **но chat-history → chat-api (cycle A)** |
| 33 | `recent_request_ids` | `VecDeque<String>` | std ✅ |
| 34 | `recent_request_ids_set` | `HashSet<String>` | std ✅ |
| 35 | `abort_flag` | `Arc<AtomicBool>` | std ✅ |
| 36 | `abort_notify` | `Arc<Notify>` | tokio ✅ |
| 37 | `interruptible_waits` | `usize` | std ✅ |
| 38 | `wait_delivery_boundary` | `bool` | std ✅ |
| 39 | `wait_interrupt_flag` | `Arc<AtomicBool>` | std ✅ |
| 40 | `wait_interrupt_cause` | `Arc<StdMutex<WaitInterruptCause>>` (pub(crate)) | **engine ❌** — `tools/tools_description.rs:196`, 2 variants |
| 41 | `user_interrupt_flag` | `Arc<AtomicBool>` | std ✅ |
| 42 | `queue_processor_running` | `Arc<AtomicBool>` | std ✅ |
| 43 | `queue_notify` | `Arc<Notify>` | tokio ✅ |
| 44 | `queue_processor_counters` | `Arc<QueueProcessorCounters>` | **engine ❌** — `chat/types.rs:400`, 5 × `AtomicU64` (co-located plain data) |
| 45 | `last_activity` | `Instant` | std ✅ |
| 46 | `last_stream_delta_at` | `Option<Instant>` | std ✅ |
| 47 | `command_enqueued_at` | `HashMap<String, Instant>` | std ✅ |
| 48 | `stream_started_at` | `Option<Instant>` | std ✅ |
| 49 | `confirmation_paused_at` | `Option<(Instant, HashSet<String>)>` | std ✅ |
| 50 | `last_tool_started_at` | `Option<Instant>` | std ✅ |
| 51 | `last_tool_progress_at` | `Option<Instant>` | std ✅ |
| 52 | `trajectory_dirty` | `bool` | std ✅ |
| 53 | `trajectory_version` | `u64` | std ✅ |
| 54 | `trajectory_committed_version` | `u64` | std ✅ |
| 55 | `trajectory_save_in_flight` | `bool` | std ✅ |
| 56 | `trajectory_save_queued` | `bool` | std ✅ |
| 57 | `trajectory_save_mutex` | `Arc<AMutex<()>>` | tokio ✅ |
| 58 | `trajectory_commit_notify` | `Arc<Notify>` | tokio ✅ |
| 59 | `trajectory_save_error` | `Option<String>` | std ✅ |
| 60 | `created_at` | `String` | std ✅ |
| 61 | `closed` | `bool` | std ✅ |
| 62 | `closed_flag` | `Arc<AtomicBool>` | std ✅ |
| 63 | `external_reload_pending` | `Option<ExternalReloadPending>` | **engine ❌** — `chat/types.rs:248` (co-located plain data) |
| 64 | `last_prompt_messages` | `Vec<ChatMessage>` | `refact-core` ✅ |
| 65 | `tool_catalog` | `Option<Arc<ToolCatalogSnapshot>>` | `refact-runtime-api` ✅ **но runtime-api → chat-api (cycle A)** |
| 66 | `turn_tool_pool` | `Option<TurnToolPool>` | `refact-runtime-api` ✅ **но cycle A** |
| 67 | `pending_max_new_tokens_boost` | `Option<usize>` | std ✅ |
| 68 | `cache_guard_snapshot` | `Option<serde_json::Value>` | serde_json ✅ |
| 69 | `cache_guard_request_generation` | `u64` | std ✅ |
| 70 | `cache_guard_snapshot_generation` | `u64` | std ✅ |
| 71 | `cache_guard_reset_generation` | `u64` | std ✅ |
| 72 | `cache_guard_force_next` | `bool` | std ✅ |
| 73 | `provider_usage_stale` | `bool` | std ✅ |
| 74 | `task_agent_error` | `Option<String>` | std ✅ |
| 75 | `pending_browser_message` | `Option<PendingBrowserMessage>` | `refact-chat-api::chat_local_types` ✅ |
| 76 | `post_tool_side_effects` | `VecDeque<ChatMessage>` | `refact-core` ✅ |
| 77 | `pending_deliveries` | `VecDeque<PendingDelivery>` | `refact-chat-api` ✅ |
| 78 | `runner_pending_deliveries` | `Vec<PendingDelivery>` | `refact-chat-api` ✅ |
| 79 | `delivered_delivery_ids` | `HashSet<String>` | std ✅ |
| 80 | `turn_depth` | `usize` | std ✅ |
| 81 | `delivery_wake_sources` | `HashSet<&'static str>` | std ✅ |
| 82 | `active_command` | `ActiveCommandContext` | `refact-chat-api` ✅ |
| 83 | `skills_available_count` | `usize` | std ✅ |
| 84 | `skills_included` | `Vec<String>` | std ✅ |
| 85 | `pending_skill_deactivation` | `Option<PendingSkillDeactivation>` | `refact-chat-api::chat_local_types` ✅ |
| 86 | `post_turn_task_handles` | `Vec<tokio::task::JoinHandle<()>>` | tokio ✅ |
| 87 | `openai_codex_websocket` | `super::openai_codex_ws::OpenAICodexWebSocketSession` (pub(crate)) | **engine ❌** — `chat/openai_codex_ws.rs:8`, holds `tokio_tungstenite::WebSocketStream` |
| 88 | `suppress_auto_enrichment_for_next_turn` | `bool` | std ✅ |
| 89 | `enrichment_identities` | `HashSet<String>` (pub(crate)) | std ✅ |
| 90 | `wake_up_at` | `Option<chrono::DateTime<chrono::Utc>>` | chrono ✅ |
| 91 | `waiting_for_card_ids` | `Vec<String>` | std ✅ |
| 92 | `background_agents` | `HashMap<String, BackgroundAgentSummary>` | `refact-chat-api` ✅ |
| 93 | `goal_stopped_by_abort` | `bool` | std ✅ |
| 94 | `goal_ledger` | `Vec<GoalLedgerEntry>` | `refact-chat-api` ✅ |
| 95 | `goal_turn_evidence` | `bool` | std ✅ |
| 96 | `goal_verification_blocked_until_ms` | `Option<u64>` | std ✅ |

**90 ✅ / 6 ❌** (strict count by declared-in-engine type).

### The 6 engine-typed fields

| # | Field | Type | Cost to remove |
|---|---|---|---|
| 2 | `derived_privacy_zones` | `DerivedPrivacyZones` alias + `DerivedZone` | **cheap** — 2-field struct + 1 alias → `refact-privacy` (no cycle: privacy has no chat-api dep); engine keeps `pub use` |
| 14 | `pending_context_rebuild` | `PendingContextRebuild` | **cheap** — `Option<String>` + `String` → `refact-chat-api` |
| 40 | `wait_interrupt_cause` | `WaitInterruptCause` | **cheap** — 2-variant enum → `refact-chat-api` (or `refact-tool-api`) |
| 44 | `queue_processor_counters` | `QueueProcessorCounters` | **cheap** — 5 × `AtomicU64`, co-located with the struct, moves with it |
| 63 | `external_reload_pending` | `ExternalReloadPending` | **cheap** — 2 variants of `TrajectorySourceIdentity` (already chat-api), co-located, moves with the struct |
| 87 | `openai_codex_websocket` | `OpenAICodexWebSocketSession` | **expensive** — needs `tokio-tungstenite` in `refact-chat-api`; alternative: box it behind a trait/`Arc<dyn Any>` seam in the engine |

## 3. `SessionsMap` (after `ChatSession`)

`chat/session/mod.rs`:

| Item | Line | Notes |
|---|---|---|
| `pub type SessionsMap` | 121 | pure alias — trivially movable **only after** `ChatSession` |
| `pub struct EngineSessionsHandle(pub SessionsMap)` + `impl SessionsHandle` | 123 | `SessionsHandle` is in `refact-runtime-api`, which depends on chat-api → the impl cannot move to chat-api without cycle A; it can stay in the engine (trait impl on a crate alias type is legal) |
| `pub fn sessions_map_from_handle` | 138 | downcasts `dyn SessionsHandle` → `SessionsMap`; must stay where `SessionsMap` is public |
| `pub fn create_sessions_map` | 147 | trivial |

## 4. `ChatCtx` (`chat/ctx.rs:285-310`, 25 fields — card says 34)

| # | Field | Type | home |
|---|---|---|---|
| 1-3 | `runtime` / `paths` / `model` | `RuntimeServices` / `PathServices` / `ModelServices` | `refact-runtime-api` ✅ |
| 4 | `workspace` | `ChatWorkspace` | **engine ❌** (`ctx.rs:38`) |
| 5 | `chat` | `ChatServices` | **engine ❌** (`app_state.rs:70`, holds `sessions: SessionsMap`) |
| 6 | `buddy` | `BuddyServices` | **engine ❌** (`app_state.rs:82`) |
| 7 | `integrations` | `ChatIntegrationScope` | **engine ❌** (`ctx.rs:50`) |
| 8 | `agents` | `Arc<BackgroundAgentRegistry>` | `refact-agents-core` ✅ |
| 9-11 | `tool_registry` / `activity_sink` / `buddy_event_sink` | `Arc<dyn ToolRegistry>` / `Arc<dyn ActivitySink>` / `Arc<dyn BuddyEventSink>` | `refact-runtime-api` ✅ |
| 12 | `card_verifier_tasks` | `Arc<AMutex<JoinSet<()>>>` | tokio ✅ |
| 13 | `privacy_policy_load` | `Arc<StdRwLock<PolicyLoad>>` | `refact-privacy` ✅ |
| 14 | `tool_catalog_generations` | `Arc<ToolCatalogGenerations>` | `refact-runtime-api` ✅ (`tool_catalog.rs:10`) |
| 15 | `project_registry_cache` | `Arc<StdRwLock<RegistryCacheManager>>` | `refact-yaml-configs` ✅ |
| 16 | `trajectory_vectorization_queue` | `Arc<AMutex<TrajectoryVectorizationQueue>>` | `refact-chat-api` ✅ |
| 17-25 | `subchat` / `project_tree` / `hooks` / `mode_config` / `caps` / `exec_policy` / `trajectories` / `prompts` / `tool_executor` | `Arc<dyn …>` traits | mixed: `mode_config` (`ModeConfigProvider`) and `caps` (`CapsProvider`) are `refact-runtime-api` ✅ (`ctx_traits.rs:53,64`); the other 7 are **engine ❌** (`ctx.rs:55,147,152,157,162,258,272`) |

**4 engine struct fields + 7 engine traits → `ChatCtx` cannot move now.**
Its `chat`/`buddy`/`workspace` structs are exactly the W7-4 leftovers.

## 5. What remains (ordered; each step is a card)

1. **W8-1c — push `ToolCatalogSnapshot` + `TurnToolPool` down.** `ToolCatalogSnapshot`
   (`refact-runtime-api/src/lib.rs:93`) drags `ToolRegistryIndex` (:85),
   `ToolPolicyInfo` (:150), `ToolAliasRegistry` (`refact-tool-api/src/tool_name_alias.rs:43`),
   and `ToolDesc` (`refact-tool-api/src/tool_desc.rs:108`); `TurnToolPool` (:100) is
   `Arc<dyn Any + Send + Sync>` + methods. `refact-chat-api` may depend on
   `refact-tool-api` (no reverse dep today). runtime-api then re-exports.
2. **W8-1d — push `TrajectoryEvent` down** (`refact-chat-history/src/trajectory_event.rs:5`)
   → `refact-chat-api`; chat-history re-exports. Cycle-free (chat-history already
   depends on chat-api).
3. **W8-1e — 5 cheap engine field types** (`DerivedZone`/`DerivedPrivacyZones` → `refact-privacy`;
   `PendingContextRebuild`, `WaitInterruptCause` → chat-api; `QueueProcessorCounters`,
   `ExternalReloadPending` move with the struct).
4. **W8-1f — `OpenAICodexWebSocketSession` seam**: either add `tokio-tungstenite` to
   chat-api or store `Arc<dyn Any + Send + Sync>` in the field with engine-side
   downcast helpers.
5. **W8-1g — the actual `ChatSession` move**: body → `refact-chat-api`, 10 `pub(crate)`
   fields → `pub` or accessors, **24 `impl ChatSession` blocks → free functions**
   (`ChatSession` 801/109 call-sites), engine shim `pub use refact_chat_api::…` in
   `chat/types.rs`. This card **must** run `cargo check` — it is the first point where
   the move is mechanically possible and it cannot be validated statically.
6. **W8-1h — `SessionsMap` + `create_sessions_map` + `sessions_map_from_handle`** →
   chat-api (after 5); `EngineSessionsHandle` stays in the engine.
7. **W8-1i — `ChatCtx`** → chat-api: needs W7-4-retry (`ChatServices`/`BuddyServices`)
   and the 7 engine traits + `ChatWorkspace`/`ChatIntegrationScope`.

## 6. Why nothing was moved in this card

- The Spec stop-condition fires on its own terms (`6 > 5` engine-typed fields).
- Cycle A makes the stated destination (`refact-chat-api`) unreachable until steps 1-2.
- Blocker B (24 inherent impls) means even a "body-only" move breaks the engine with
  `E0116` at 24 sites, and blocker C hides 10 fields from every engine call-site.
- Build is forbidden for this card (rule #0 / AC 5), so a move of this size would ship
  unverified — the follow-up cards (5 onward) are the first that may compile.
