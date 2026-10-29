# W7-4 census: `GlobalContext` + `AppState` → `refact-runtime-api`

Дата: 2026-10-29. Метод: **только статическая разведка** (grep/`Select-String` + чтение кода).
Сборка не запускалась (правило №0), `cargo`/`rustc` не вызывались.

## Вердикт: СТОП (Spec п.1c)

`GlobalContext` = **57 полей** (в карточке 56; добавилось `workspace_leases`), из них
**4 поля с движковыми типами**:

| # | Поле | Тип | Почему движковый |
|---|---|---|---|
| 37 | `chat_sessions` | `crate::chat::SessionsMap` = `Arc<ARwLock<HashMap<String, Arc<AMutex<ChatSession>>>>>` | `SessionsMap` (chat/session/mod.rs:121) и `ChatSession` (chat/types.rs:287, 86 полей) определены в движке |
| 44 | `enrichment_cache` | `Arc<crate::knowledge::enrichment::EnrichmentCache>` | `EnrichmentCache` (knowledge/enrichment.rs:174) в движке и держит `MemoRecord` (memories.rs:49) |
| 50 | `buddy` | `Arc<AMutex<Option<crate::buddy::actor::BuddyService>>>` | `BuddyService` (buddy/actor.rs:209) в движке |
| 51 | `buddy_events_tx` | `Option<broadcast::Sender<crate::buddy::events::BuddyEvent>>` | `pub type BuddyEvent = CoreBuddyEvent<DiagnosticContext>` (buddy/events.rs:3) — alias объявлен в движке (оба параметра уже крейтовые) |

`4 > 3` → по Spec п.1c перенос **не выполняется**. Дополнительно: из трёх «жёстких»
блокеров ни один не закрывается трейт-швом в `refact-runtime-api` (см. §4), т.е. условие
«≤3 остатка, закрываемых трейт-швом» не выполнено даже при иной трактовке `buddy_events_tx`.

`AppState` = **14 полей**, из них **6 движковых** (`gcx`, `ctx`, `tool`, `workspace`, `chat`, `buddy`).
Итого по узлу: **10 движковых полей-типов из 71**.

## 1. Prestate (AC 4)

```
Select-String -Path refact-agent/engine/src/global_context.rs,refact-agent/engine/src/app_state.rs \
  -Pattern 'pub struct GlobalContext|pub struct AppState'
global_context.rs:123: pub struct GlobalContext {
app_state.rs:313:      pub struct AppState {
```

В `refact-agent/engine/crates/refact-runtime-api/src/*.rs` — 0 совпадений (`GlobalContext`,
`AppState`, `ChatCtx`, `ToolCtx` там отсутствуют). Каталога `crates/refact-test-support` нет.

## 2. Census: `GlobalContext` (global_context.rs:123-189, 57 полей)

`home` = где сейчас живёт тип поля. ✅ = крейт/std (переносим), ❌ = движок.

| # | Поле | Тип | home |
|---|---|---|---|
| 1 | `shutdown_flag` | `Arc<AtomicBool>` | std ✅ |
| 2 | `lsp_tcp_client_count` | `Arc<AtomicUsize>` | std ✅ |
| 3 | `exec_registry` | `Arc<ExecRegistry>` | `refact-exec` ✅ |
| 4 | `card_verifier_tasks` | `Arc<AMutex<JoinSet<()>>>` | tokio ✅ |
| 5 | `trajectory_vectorization_queue` | `Arc<AMutex<TrajectoryVectorizationQueue>>` | `refact-chat-api` ✅ (шим chat/trajectories/paths.rs:225) |
| 6 | `cmdline` | `CommandLine` | `refact-runtime-api` ✅ |
| 7 | `http_client` | `reqwest::Client` | внешний ✅ |
| 8 | `cache_dir` | `PathBuf` | std ✅ |
| 9 | `config_dir` | `PathBuf` | std ✅ |
| 10 | `caps_state` | `Arc<ARwLock<CapsState>>` | `refact-caps-core` ✅ |
| 11 | `tokenizer_state` | `Arc<StdRwLock<TokenizerState>>` | `refact-core` ✅ |
| 12 | `completions_cache` | `Arc<StdRwLock<CompletionCache>>` | `refact-scratchpads` ✅ (completion_cache.rs:1 — шим) |
| 13 | `vec_db` | `Arc<AMutex<Option<Arc<dyn VecdbSearch>>>>` | `refact-core` ✅ |
| 14 | `vec_db_error` | `Arc<StdMutex<String>>` | std ✅ |
| 15 | `codegraph` | `Arc<AMutex<Option<Arc<CodeGraphService>>>>` | `refact-codegraph` ✅ (codegraph/mod.rs:5 — шим) |
| 16 | `codegraph_error` | `Arc<StdMutex<String>>` | std ✅ |
| 17 | `ask_shutdown_sender` | `Arc<StdMutex<mpsc::Sender<String>>>` | std ✅ |
| 18 | `documents_state` | `DocumentsState` | `refact-files` ✅ (files_in_workspace.rs:613 — шим) |
| 19 | `at_commands_preview_cache` | `Arc<AMutex<AtCommandsPreviewCache>>` | `refact-core` ✅ |
| 20 | `privacy_settings` | `Arc<StdRwLock<Arc<PrivacySettings>>>` | `refact-core` ✅ (privacy.rs:18 — шим) |
| 21 | `privacy_policy_load` | `Arc<StdRwLock<PolicyLoad>>` | `refact-privacy` ✅ |
| 22 | `file_index` | `Arc<FileIndex>` | `refact-files` ✅ (file_index.rs:1 — шим) |
| 23 | `privacy_match_counts` | `Arc<StdRwLock<PrivacyMatchCounts>>` | `refact-privacy` ✅ |
| 24 | `privacy_observation_runtime` | `Arc<StdRwLock<PrivacyObservationRuntimeState>>` | `refact-privacy` ✅ |
| 25 | `indexing_everywhere` | `Arc<StdRwLock<Arc<IndexingEverywhere>>>` | `refact-files` ✅ |
| 26 | `integration_sessions` | `Arc<AMutex<HashMap<String, Arc<AMutex<Box<dyn IntegrationSession>>>>>>` | `refact-integrations` ✅ |
| 27 | `browser_runtimes` | `Arc<AMutex<HashMap<String, Arc<AMutex<BrowserRuntime>>>>>` | `refact-browser` ✅ |
| 28 | `init_shadow_repos_background_task_holder` | `BackgroundTasksHolder` | `refact-core` ✅ |
| 29 | `init_shadow_repos_lock` | `Arc<AMutex<bool>>` | std ✅ |
| 30 | `git_operations_abort_flag` | `Arc<AtomicBool>` | std ✅ |
| 31 | `app_searchable_id` | `Arc<StdMutex<String>>` | std ✅ |
| 32 | `trajectory_events_tx` | `Option<Sender<TrajectoryEvent>>` | `refact-chat-history` ✅ |
| 33 | `workspace_changed_tx` | `Option<Sender<()>>` | tokio ✅ |
| 34 | `task_events_tx` | `Option<Sender<TaskEventEnvelope>>` | `refact-tasks` ✅ (tasks/events.rs:7 — шим) |
| 35 | `task_events_seq` | `Option<Arc<AtomicU64>>` | std ✅ |
| 36 | `notification_events_tx` | `Option<Sender<NotificationEvent>>` | `refact-chat-api` ✅ (sidebar.rs:30 — шим) |
| 37 | `chat_sessions` | `SessionsMap` | **движок** ❌ |
| 38 | `trajectory_index_coordinator` | `Arc<TrajectoryIndexCoordinator>` | `refact-chat-history` ✅ |
| 39 | `performance_telemetry` | `Arc<PerformanceTelemetry>` | `refact-perf` ✅ |
| 40 | `project_registry_cache` | `Arc<StdRwLock<RegistryCacheManager>>` | `refact-yaml-configs` ✅ |
| 41 | `providers` | `Arc<ARwLock<ProviderRegistry>>` | `refact-providers` ✅ |
| 42 | `quota_cache` | `Arc<AMutex<ProviderQuotaCache>>` | `refact-providers` ✅ (providers/mod.rs:5 — шим) |
| 43 | `knowledge_index` | `Arc<AMutex<KnowledgeIndex>>` | `refact-core` ✅ (knowledge_index.rs:14 — шим) |
| 44 | `enrichment_cache` | `Arc<EnrichmentCache>` | **движок** ❌ |
| 45 | `enrichment_generation` | `Arc<AtomicU64>` | std ✅ |
| 46 | `llm_stats_sender` | `Arc<StdMutex<Option<Sender<LlmCallEvent>>>>` | `refact-stats` ✅ |
| 47 | `ext_cache_generation` | `Arc<AtomicU64>` | std ✅ |
| 48 | `tool_catalog_generations` | `Arc<ToolCatalogGenerations>` | `refact-runtime-api` ✅ |
| 49 | `tool_catalog_cache` | `Arc<ToolCatalogCache>` | `refact-runtime-api` ✅ |
| 50 | `buddy` | `Arc<AMutex<Option<BuddyService>>>` | **движок** ❌ |
| 51 | `buddy_events_tx` | `Option<Sender<BuddyEvent>>` | **движок** (alias) ❌ |
| 52 | `user_activity` | `Arc<AMutex<UserActivityRing>>` | `refact-buddy-core` ✅ |
| 53 | `agents` | `Arc<BackgroundAgentRegistry>` | `refact-agents-core` ✅ |
| 54 | `scheduler_config` | `SchedulerConfig` | `refact-scheduler-core` ✅ |
| 55 | `hooks_config` | `HooksConfig` | `refact-ext` ✅ (ext/hooks.rs:1 — шим) |
| 56 | `terminal_security_config` | `TerminalSecurityConfig` | `refact-runtime-api` ✅ |
| 57 | `workspace_leases` | `Arc<StdMutex<WorkspaceLeaseSet>>` | `refact-daemon` ✅ |

Итог: **53 ✅ / 4 ❌**.

## 3. Census: `AppState` (app_state.rs:313-327, 14 полей)

| # | Поле | Тип | home |
|---|---|---|---|
| 1 | `gcx` | `SharedGlobalContext` = `Arc<GlobalContext>` | **движок** ❌ (сам узел) |
| 2 | `ctx` | `ChatCtx` (chat/ctx.rs:285) | **движок** ❌ |
| 3 | `tool` | `ToolCtx` (tools/ctx.rs:38) | **движок**, но 100 % крейтовыразим (см. §5) |
| 4 | `runtime` | `RuntimeServices` | `refact-runtime-api` ✅ |
| 5 | `paths` | `PathServices` | `refact-runtime-api` ✅ |
| 6 | `model` | `ModelServices` | `refact-runtime-api` ✅ |
| 7 | `workspace` | `WorkspaceServices` (app_state.rs:58) | **движок-структура**, но все 8 полей крейтовые → крейтовыразима |
| 8 | `chat` | `ChatServices` (app_state.rs:70) | **движок-структура**, 7/8 полей крейтовые; блокер — `sessions: SessionsMap` |
| 9 | `buddy` | `BuddyServices` (app_state.rs:82) | **движок-структура**, 1/3 крейтовое; блокеры — `BuddyService`, `BuddyEvent` |
| 10 | `integrations` | `IntegrationServices` | `refact-runtime-api` ✅ |
| 11 | `activity_sink` | `Arc<dyn ActivitySink>` | `refact-runtime-api` ✅ |
| 12 | `buddy_event_sink` | `Arc<dyn BuddyEventSink>` | `refact-runtime-api` ✅ |
| 13 | `tool_registry` | `Arc<dyn ToolRegistry>` | `refact-runtime-api` ✅ |
| 14 | `agents` | `Arc<BackgroundAgentRegistry>` | `refact-agents-core` ✅ |

Итог: **8 ✅ / 6 ❌** (из 6: 2 крейтовыразимы без переноса — `tool`, `workspace`).

## 4. Что осталось дожать (точный список блокеров)

Порядок — от самого дешёвого к самому дорогому. Числа = совпадения/файлы в
`refact-agent/engine/src` (grep по `*.rs`).

### Б-1. `BuddyEvent` alias (1 строка) — дешёвый
`buddy/events.rs:3`: `pub type BuddyEvent = CoreBuddyEvent<super::diagnostics::DiagnosticContext>;`
Оба параметра уже крейтовые (`refact-buddy-core::events::BuddyEvent`,
`refact-buddy-core::diagnostics::DiagnosticContext`). Нужно: объявить alias в
`refact-buddy-core` (или в `refact-runtime-api`), в движке оставить `pub use`.
Разблокирует поле 51. `buddy_events_tx`: 10 совпадений / 4 файла.

### Б-2. `EnrichmentCache` + `MemoRecord` — средний
`knowledge/enrichment.rs:174` (`EnrichmentCache` → `EnrichmentCacheState` →
`CachedEnrichmentResult` → `CachedMemo { memo: crate::memories::MemoRecord }`).
`MemoRecord` — движковый (memories.rs:49), но plain-data: 9 полей, все std
(`String`/`Vec<String>`/`Option<PathBuf>`/`Option<(u64,u64)>`/`Option<f32>`).
`KnowledgeFrontmatter` уже крейтовый (`refact-knowledge-graph::kg_structs`). Нужно:
`MemoRecord` → `refact-knowledge-graph`/`refact-core`, затем `EnrichmentCache` туда же,
движок — шим. `enrichment_cache`: 24 совпадения / 3 файла.
Разблокирует поле 44.

### Б-3. `ChatSession` + `SessionsMap` — дорогой (главный)
`chat/types.rs:287` (`ChatSession`, **86 полей**, 110 строк), `chat/session/mod.rs:121`
(`SessionsMap`). Внутри `ChatSession` — движковые `ToolCallLoopGuard`
(types.rs:290), `PendingContextRebuild` (:302), `OpenAICodexWebSocketSession` (:384),
а также `TrajectoryCommitIntent` (:264); `TrajectorySourceIdentity` уже крейтовый.
Нужно: перенос `ChatSession` + `SessionsMap` + `TrajectoryCommitIntent` (+
`create_sessions_map`, `EnqueueCommandOutcome`) в `refact-chat-api`.
`chat_sessions`: **141 совпадение / 43 файла**.
Разблокирует поля 37, `ChatServices.sessions`, а через `ChatServices` — и `ChatCtx`.

### Б-4. `BuddyService` — дорогой
`buddy/actor.rs:209` (+ `BuddyState`, storage, runtime queue, autonomous workflows).
`BuddyService`: 73 совпадения / 25 файлов; `\.buddy\b`: 212 / 47.
Разблокирует поле 50 и `BuddyServices.buddy`.

### Б-5. `ChatCtx` и его трейт-пейлоады — дорогой
`chat/ctx.rs:285`. По W7-3 остаются движковыми:
`ChatServices` (Б-3), `BuddyServices` (Б-4), `SubchatLauncher` (`SubchatConfig`
subchat.rs:521, `WrapUpConfig` :477, `ExplicitSubchatSpec` :1391, `SubchatResult` :733,
`TraceParent` :484 — все крейтовыразимы), `TrajectoryStore` (`ChatSession`/`SessionsMap`),
`PromptAssembler` (`ChatMeta` call_validation.rs:90, `SkillsTrackingInfo`
ext/skills_context.rs:13), `ChatToolExecutor` (`ExecuteToolsOptions` chat/tools.rs:65),
`HookRunner` (`HookPayload`/`HookResult` ext/hooks_runner.rs:24/42).
`ChatCtx`: 382 совпадения / 62 файла.

## 5. Бесплатный выигрыш, не требующий узла: `ToolCtx`

`tools/ctx.rs:38` — **все 16 полей уже крейтовые** (`RuntimeServices`, `PathServices`,
`ModelServices`, `ToolChatScope`, `Arc<dyn BuddyServicesHandle>`, `IntegrationServices`,
`ToolWorkspaceScope`, `ToolCodeIntelScope`, `BackgroundAgentRegistry`, `Arc<dyn ToolRegistry>`,
`PolicyLoad`, `Arc<dyn FilesProvider>`, `Arc<dyn TrajectoryStoreHandle>`,
`Arc<dyn SubchatLauncherHandle>`, `Arc<dyn ModeConfigProvider>`, `Arc<dyn CapsProvider>`),
и три scope-структуры (строки 17/24/31) тоже. Т.е. `ToolCtx` можно перенести в
`refact-runtime-api` **сейчас**, независимо от `GlobalContext` (78 совпадений / 18 файлов).

## 6. Почему `make_test_gcx` → `refact-test-support` тоже заблокирован

`global_context.rs` (модуль `tests`, `make_test_gcx`/`make_test_gcx_with_dirs`) строит
`GlobalContext` и поэтому нуждается в двух движковых выражениях:
`crate::chat::create_sessions_map()` (тип `SessionsMap`) и
`crate::knowledge::enrichment::EnrichmentCache::default()`, а также в типе поля
`Arc<AMutex<Option<BuddyService>>>`. Крейт не может собрать структуру, определённую в
движке, а зависимость `refact-test-support → refact-lsp` запрещена (цикл).
`make_test_gcx`: 744 совпадения / 187 файлов (в карточке 742/193).
Крейтовыразимая часть, которую можно вынести уже сейчас: реестр временных каталогов
(`TEST_TEMP_DIRS`, `test_temp_dir_is_safe`, `register_test_temp_dir`,
`remove_test_temp_dirs_at_exit`) — она не зависит от `GlobalContext`.

## 7. Предлагаемая последовательность карточек

1. `BuddyEvent` alias → `refact-buddy-core` (Б-1) + `ToolCtx` → `refact-runtime-api` (§5) — оба дешёвые и независимые.
2. `MemoRecord` + `EnrichmentCache` → крейт (Б-2).
3. `ChatSession` + `SessionsMap` → `refact-chat-api` (Б-3).
4. `BuddyService` → `refact-buddy-core` (Б-4).
5. Пейлоады `ChatCtx` (subchat-типы, `ChatMeta`, `SkillsTrackingInfo`, `ExecuteToolsOptions`, `HookPayload`/`HookResult`) (Б-5).
6. Повтор W7-4: `GlobalContext` + `AppState` + сервисы → `refact-runtime-api`, затем `make_test_gcx` → `refact-test-support`.

## 8. Верификация этого отчёта (без сборки)

```
# prestate определений
Select-String -Path refact-agent/engine/src/global_context.rs,refact-agent/engine/src/app_state.rs `
  -Pattern 'pub struct GlobalContext|pub struct AppState'
# → global_context.rs:123, app_state.rs:313

# отсутствие узла в крейте
Select-String -Path refact-agent/engine/crates/refact-runtime-api/src/*.rs `
  -Pattern 'pub struct GlobalContext|pub struct AppState|pub struct ChatCtx|pub struct ToolCtx'
# → 0 совпадений

# счётчики блокеров
Select-String -Path (Get-ChildItem refact-agent/engine/src -Recurse -Filter *.rs).FullName -Pattern 'chat_sessions'
# → 141 / 43 файла   (enrichment_cache 24/3, buddy_events_tx 10/4, BuddyService 73/25)
```

`cargo` не запускался ни разу.
