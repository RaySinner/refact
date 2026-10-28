# decision-crate-split — физическое разбиение refact-lsp (только чтение, 0 правок кода)

Дата: 2026-10-28 · Автор: фоновое исследование · Метод: PowerShell-сканы `src/` (regex `crate::([a-z_0-9]+)`, `\bgcx\b`), фасад-детектор, крейт-инвентаризация. Ни `cargo`, ни `rustc` не запускались.

## 1. Базовые числа

| Метрика | Значение |
|---|---|
| `refact-agent/engine/src/` | **631 файл, 408 113 строк** (D1 от 2026-10-24: 613 файлов / 432 902 — с тех пор ~25k строк уже уехали в крейты, напр. scheduler-core, tui) |
| Фасадные файлы в `src/` (только `pub use refact_*`) | **98 файлов, 940 строк** — след уже выполненных миграций; указывают на 26 разных крейтов |
| Реальный монолитный код в `src/` | ≈ **407 000 строк** |
| Baseline `cargo check -p refact-lsp --lib` | **12–14 мин** (текущее железо, тёплый кейс); ~25–27 мин холодный локальный компил, ~14.5 мин с sccache (см. `docs/dev/build-velocity.md`: 869.5 s на 2 compile requests) |
| Линейная модель | T(крейта) ≈ 13 мин × (строки / 408k) ≈ **1.9 c на 1000 строк** (используется ниже; допущение, не замер) |

Прецеденты предыдущих раундов (`.refact/knowledge`, D1–D3): уже вынесены `refact-scheduler-core` (ядро scheduler 2 370 L), `refact-tui` (76k), `refact-privacy` (2 469), `refact-exec` (11 024), `refact-buddy-core` (17 112), `refact-agentic`/`refact-agents-core`, `refact-at-web`, `refact-integrations` (11 267), `refact-providers` (26 014), `refact-tool-api`, `refact-chat-history`, `refact-ext`, `refact-vecdb`, `refact-files`, `refact-tasks`, `refact-worktrees`, `refact-git`, `refact-caps-core`, `refact-knowledge-graph`, `refact-perf` и др. — **44 крейта, workspace уже фрагментирован; осталось донести "хвосты" и перерезать гигантский SCC-1 (309 файлов / 291k строк, D1)**.

## 2. Стыки (трейты) — уже готовы

`src/chat/ctx.rs`: `SubchatLauncher`, `ProjectTreeProvider`, `HookRunner`, `ModeConfigProvider`, `CapsProvider`, **`ExecPolicyProvider`**, `TrajectoryStore`, `PromptAssembler`, `ChatToolExecutor` + структура `ChatCtx`.
`src/tools/ctx.rs`: **`FilesProvider`** + структура `ToolCtx`.
Реализации (`EngineExecPolicyProvider`, `EngineFilesProvider`) — в `src/app_state.rs`, которые дергают `gcx`/`files_in_workspace`. Это и есть вырезанные из W3-миграций швы: код на `ToolCtx`/`ChatCtx` не обязан знать про `GlobalContext`.

## 3. Таблица кандидатов

«реальные строки» = без фасадов-реэкспортов. «ext» = уникальные `crate::модули` вне поддерева (по всем текстовым упоминаниям, эвристика). gcx = файлы/упоминания.

| Поддерево | Реальных строк | ext-модулей | gcx (файлы/упоминания) | Что уже в крейте | Вердикт |
|---|---|---|---|---|---|
| `exec/` | 1 386 (command_policy 709 + path_enrichment 677) | **3** (call_validation, chat, global_context) | 1 / 14 | refact-exec 11 024 (spawn/pty/registry/types; 4 фасада) | **PILOT** |
| `privacy/` | 2 118 (records 1 964 + destinations 154) | 7 (call_validation, exec, file_index, files_correction, files_in_workspace, global_context, caps) | 2 / 68 | refact-privacy 2 469 (gate/matching/policy/heuristic); heuristic = фасад | **#2** |
| `files_correction.rs` | 859 (+ fuzzy_search 84 рядом) | 6 (files_in_workspace, fuzzy_search, global_context, worktrees, app_state, chat) | 1 / 67 | refact-files (correction_cache, path_utils, file_filter) | **#3** |
| `caps/` | 3 733 (caps 2 137 + providers 1 474 + models_dev 101) | **4** (custom_error, files_correction, global_context, providers) | 3 / 24 | refact-caps-core 1 439 (caps_state, model_records, provider_config) | **#4** |
| `tasks/` | 2 697 (storage 1 074, rooms 804, index 510, events 203, comments 100) | 5 (app_state, buddy, chat, files_correction, global_context) | 2 / 87 | refact-tasks 2 158 (types, events); events = фасад | **#5** |
| `git/` | 1 237 (checkpoints 813, cleanup 327, commit_info 97) | 9 (вкл. agentic, buddy, background_tasks) | 3 / 59 | refact-git 1 598 (operations) |_follow-on_ |
| `vecdb/` | 393 (vdb_highlev) | 10 | 1 / 49 | refact-vecdb 3 967 | follow-on (10 deps на 393 строк — дёшево после #2–#5) |
| `knowledge_graph/` | 325 (kg_subchat 234 + mod 91) | 11 (subchat, memories, buddy…) | 2 / 29 | refact-knowledge-graph 1 013 | STAYS (вязка на subchat/memories) |
| `scheduler/` | 4 327 (runner 3 172, delivery 585, exec_action 419) | 7 | 4 / 199 (183 — runner) | refact-scheduler-core 2 391 — ядро уже вынесено | STAYS (runner: gcx/CHAT; D3-вердикт) |
| `providers/` | 8 756 (http.rs 6 343, oauth_refresh 1 941) | 7 | 3 / 510 | refact-providers 26 014; quota/runtime_credential ещё НЕ вынесены (обещание D3 не исполнено) | follow-on: quota 282 + runtime_credential 44 EXTRACT-NOW; http.rs 6 343 — STAYS (32 axum-хендлера на gcx) |
| `ext/` | 11 968 (hooks_runner 867, skills_context 1 240, plugins 1 398, marketplace 1 781…) | 9 | 6 / 26 | refact-ext 12 545; 5 фасадов | follow-on (низкий gcx, но app_state ×7) |
| `at_commands/` | 3 196 | 15 | 7 / 59 | refact-at-web (web-часть) | follow-on после #3 |
| `integrations/` | 15 020 (mcp_common 2 094, mcp_auth 2 236, cmdline 1 253+615, browser_runtime 1 203) | 21 | 19 / 209 | refact-integrations 11 267; 8 фасадов | STAYS (mcp/browser рантайм завязаны на chat/http) |
| `buddy/` | 42 877 (memory_lifecycle 4 315, actor 2 865, jobs/…) | 22 (app_state ×126) | 65 / 748 | refact-buddy-core 17 112; 16 фасадов | STAYS (верхний слой) |
| `http/` | 44 011 (routers/v1 — 72 файла, gcx 1 102) | **45** | 62 / 1 102 | — | STAYS (D3: v1-роутеры на 44 топ-модуля) |
| `chat/` | 91 168 (generation, queue, session, trajectories…) | 38 | 47 / 1 308 | refact-chat-api 9 227, refact-chat-history 13 743; 20 фасадов | STAYS (последний этап, после обрезки всех зависимостей) |
| `tools/` | 109 085 (design_tools 2 714, scope_utils 2 346, file_edit/…) | 37 | 94 / 2 287 | refact-tool-api 6 516, refact-file-edit-core; 12 фасадов | STAYS (самый связанный) |
| `worktrees/` | **8 (все фасады)** | 0 | 0 / 0 | refact-worktrees 7 915 | ✅ уже вынесено |
| `files_in_workspace.rs` | 4 783 (один файл) | 17 | 1 / 367 | — | **STAYS — центральный хаб** (fan-out 130, D1); трейты FilesProvider уже абсорбируют потребителей |

Итого оставшийся монолит ≈ 407k строк; нижний слой («лёгкие хвосты»: exec+privacy+files_correction+caps+tasks+git+vecdb+at_commands+ext+providers-хвосты) ≈ **36k строк (~9%)**, верхний слой chat/tools/http/buddy/integrations/scheduler-runner ≈ 370k строк и связан через gигантский SCC-1.

## 4. Пилот: `exec/` → добить в `refact-exec`

Почему именно он:
1. **Минимум внешних зависимостей: 3** (лучший показатель среди всех реальных поддеревьев), при этом `path_enrichment.rs` (677 L) — **абсолютно чистый: 0 `crate::`-зависимостей, 0 gcx**.
2. **≤3000 строк: 1 386 L**.
3. **Готовый шов: `ExecPolicyProvider`** (chat/ctx.rs:101) + реализация `EngineExecPolicyProvider` в app_state.rs. `command_policy.rs` использует `crate::chat` (ChatCtx) и `gcx` всего 14 раз — это ровно тот тип употребления, который закрывается передачей `&dyn ExecPolicyProvider`/ChatCtx-срезом.
4. `refact-exec` — листовой крейт (PTY/processes/OS-наблюдатели, 0 упоминаний scheduler/chat), 4 фасада уже указывают на него; цикл невозможен.
5. Разброс риска: дифф = перенос 2 файлов + 1 сигнатура (command_policy принимает контекст параметром), 0 правок call-site'ов за пределами `exec/mod.rs` + `app_state.rs`.

## 5. Топ-5 порядок выноса

| № | Кандидат | Строк | ext-deps | Шов | Прибавка к уже вынесенному |
|---|---|---|---|---|---|
| 1 | `exec/` (command_policy + path_enrichment) | 1 386 | 3 | ExecPolicyProvider | в refact-exec |
| 2 | `privacy/records.rs` + destinations | 2 118 | 7 | refact-privacy (PolicyLoad в ChatCtx/ToolCtx) + FilesProvider для файловых проверок | в refact-privacy |
| 3 | `files_correction.rs` (+ `fuzzy_search.rs` 84 L) | 943 | 6 | refact-files (correction_cache уже там); фасад `pub use` сохранит API для 267 потребителей | в refact-files |
| 4 | `caps/` (caps.rs 2 137, providers.rs 1 474) | 3 733 | 4 | CapsProvider-трейт; refact-caps-core уже хостит state/records/provider_config | в refact-caps-core (+ refact-providers для providers.rs) |
| 5 | `tasks/` (storage, rooms, index, comments) | 2 697 | 5 | refact-tasks (types/events уже вынесены) | в refact-tasks |

Следом (не пилот, но дёшево): `providers/quota.rs` 282 L + `runtime_credential.rs` 44 L → refact-providers (незакрытые обещания D3); `git/checkpoints+cleanup` 1 140 L (требует порвать 9 deps: agentic/buddy/background_tasks — сначала traits); `ext/` хвосты (11 968 L, gcx всего 26) после tasks.

Явно НЕ выносить: `http/routers/v1` (45 модулей), `tools/` и `chat/` (37–38 модулей, 2 287 gcx), `buddy/` (app_state ×126), `scheduler/runner.rs` (183 gcx), `files_in_workspace.rs` (17 модулей, 367 gcx — хаб), `integrations/` mcp/browser рантайм, `knowledge_graph/kg_subchat` (вязка на subchat/memories).

## 6. Оценка времени check (модель: доля строк; 13 мин = 408k)

| Вынос | Строк | Доля монолита | check правки ВНУТРИ крейта (API стабилен)* | check при изменении API | check правки в остальном монолите |
|---|---|---|---|---|---|
| базлайн | — | — | 12–14 мин | 12–14 мин | 12–14 мин |
| №1 exec | 1 386 | 0.34% | **~5–30 с** | + ~12.9 мин | ~12.9 мин |
| №2 privacy | 2 118 | 0.52% | ~10–40 с | + ~12.8 мин | ~12.8 мин |
| №3 files_correction | 943 | 0.23% | ~5–30 с | + ~12.9 мин | ~12.85 мин |
| №4 caps | 3 733 | 0.91% | ~15–60 с | + ~12.8 мин | ~12.75 мин |
| №5 tasks | 2 697 | 0.66% | ~10–40 с | + ~12.9 мин | ~12.8 мин |
| топ-5 суммарно | 10 877 | **2.6%** | — | — | **~12.7 мин (−0.3)** |

\* «API стабилен» = публичные сигнатуры крейта не изменились (правки тел) — тогда зависимые крейты не пересобираются; это и есть главная точечная экономика. Плюс независимые юниты компилируются параллельно и кэшируются sccache по отдельности (исчезает «один большой всегда-промах» юнит при смене работы).

Честный вывод: **маленькие нижние выносы почти не уменьшают полный check монолита** (−0.3 мин суммарно) — их ценность в (а) ускорении итерации внутри подсистемы в 30–90 раз, (б) разблокировке следующего слоя. Крупный выигрыш приходит позже: когда верхние блоки (buddy 42.9k, http-роутеры 44k, chat 91k, tools 109k) смогут стать крейтами, правка внутри блока стоит долю от полного check: tools как крейт ≈ 13 × (109/408) ≈ **3.5 мин vs 13 мин** (−73%), chat ≈ 2.9 мин, http-роутеры ≈ 1.4 мин. Дорожная карта: донести хвосты (топ-5) → traits-изолировать app_state/files_in_workspace → постепенно отцеплять верхний слой.

## 7. Главные риски

1. **Циклические зависимости (главный).** D1: гигантский SCC-1 = 309 файлов / 291k строк / 26 подсистем — monolithic unit в терминах компилятора; любой верхний вынос требует разрыва рёбер. Жадный min-cut: `files_in_workspace→files_correction` (46), `code_intel→tool_codegraph` (32), `files_in_workspace→global_context` (28), `providers/http→global_context` (27). Лечение: перенос зависимостей вниз по слоям перед выносом, трейты как швы.
2. **`pub use` фасады.** 98 штук уже создают двойные пути (monolith facade ↔ прямой импорт refact_x); при выносе растёт риск E0364 (`pub use child::X`, где X не pub) и «leaked impl details». Готовый инструмент: `tools/dev/check_split.py` (ловит E0364/E0624/E0449) — прогонять перед каждым переносом.
3. **Ре-экспорт-API замораживает ошибки**: `caps/mod.rs: pub use caps::*` означает, что публичная поверхность caps = всё, что pub в caps.rs; при переносе в крейт придётся чистить видимость (E0624).
4. **`crate::`-упоминания в комментариях/макросах** — мои счётчики зависимостей — эвристика (±), финальную арбитражность даёт только компилятор.

## 8. Что не проверено

- Компиляция не запускалась (запрет ТЗ) — все «после»-оценки линейны по строкам, без учёта дешёвого/дорогого кода (generic-heavy chat/generation может чекаться медленнее на строку).
- Подсчёт `crate::` включает inline-ссылки и комментарии; групповые `use crate::{a, b}` учтены только по первому элементу.
- Зависимости `use refact_*` в поддеревьях не нормированы по слоям (косвенно видно через фасады).
- Тестовый код включён в строки (в check --lib он тоже тип-чекается, так что это правильно для оценки).

## 9. Артефакты измерений

Все числа получены in-place PowerShell-сканами (см. транскрипт этого чата): размеры, ext-deps, gcx, фасады, крейт-инвентарь; pre-existing анализы: `.refact/knowledge/2026-10-24_221353_*_d1-*` (граф/SCC), `..._d3-http-daemon-*`, `..._d3-scheduler-providers-*` (scheduler-core/providers-обещания), `tools/dev/check_split.py` (валидатор сплита), `docs/dev/build-velocity.md` (baseline-замеры).
