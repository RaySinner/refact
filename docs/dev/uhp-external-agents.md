# ADR: внешние агенты через Unified Harness Protocol

Дата: 2026-11-24
Статус: принято как проектная граница для UHP-1 и RM-5; live-совместимость Refact ещё не доказана
Область: Claude Code, OpenCode, Codex и последующие внешние harness advisors

## 1. Решение и статус доказательств

Refact подключает внешние процессы как **read-only advisors через UHP client boundary**, а не как writers и не как peer-to-peer расширение комнаты. Первый production server — self-hosted HarnessRouter Community Edition (CE), запущенный на инфраструктуре оператора; Claude Code и OpenCode подключаются через отдельные configured harnesses и независимые UHP sessions. Codex использует тот же boundary после собственной live-проверки.

Принятые решения:

1. **Refact Room Router остаётся владельцем комнаты.** UHP не получает membership, room ACL, public journal, fan-out или single-writer policy.
2. **Один participant = один configured harness = одна независимая session chain.** Нельзя делить одну UHP session между recipients: протокол запрещает concurrent tasks в одной session [S3].
3. **Только локальный Refact writer имеет mergeable worktree.** Внешний advisor получает read-only projection актуального writer state; его output никогда не применяется автоматически.
4. **Целевой профиль — UHP `2026-10-19`.** Каждый request явно несёт `UHP-Version`; server response, включая errors, проверяется на ту же версию [S3].
5. **Rust SDK не выдумывается из generic HTTP stack.** На дату исследования подходящего maintained official Rust UHP SDK не найдено. Реализуется узкий `refact-uhp` поверх уже существующих в engine `reqwest`, `serde` и `eventsource-stream`, со stable replaceable API (раздел 8).
6. **Протокол и room domain разнесены физически.** `refact-uhp` и `refact-room` — отдельные независимые crates; runtime содержит только thin adapters (раздел 3). Это обязательное review-ограничение, а не пожелание.

В документе используются разные уровни уверенности:

| Метка | Значение |
|---|---|
| **Нормативно** | Требование UHP `2026-10-19`; это контракт реализации, а не результат нашего live run. |
| **Upstream заявлено** | README/source HarnessRouter перечисляет base или capability. |
| **Upstream измерено** | Опубликованная HarnessRouter support matrix/conformance report содержит конкретный запуск; это не запуск Refact. |
| **Refact live** | Запуск нашего client + нашей Room Router + реального CE и CLI с сохранённым evidence report. Сейчас таких запусков нет. |
| **Требуется live** | Нельзя включать в production support до отдельной проверки по разделу 9. |

Наличие spec, OpenAPI, mock tests или upstream pass не повышает запись автоматически до **Refact live**. Поддержка означает конкретную комбинацию CE build, CLI version, configured harness, provider/model и capabilities; имя base само по себе недостаточно.

## 2. Scope, роли и non-goals

### 2.1. Граница и владельцы ответственности

```text
Refact tools/UI
      │ room application API
      ▼
Refact Room Router ── durable public journal, roster, privacy, writer lease
      │ Participant abstraction
      ▼
thin runtime UHP adapter
      │ refact-uhp only: HTTP + SSE
      ▼
UHP Server / HarnessRouter CE ── API auth, object scope, harness/session lifecycle
      │ implementation-defined process boundary
      ▼
configured harness (`chrn_…`: base + model + tools + environment + budgets)
      │
      ▼
external process: Claude Code / OpenCode / Codex / another base
```

| Компонент | Владеет | Не владеет |
|---|---|---|
| **Refact Room Router** | `room_id`, membership, roster, writer, durable `room_seq`, per-recipient delivery, retry identity, public/private projection, bounded replay | Запуском CLI, UHP SSE sequence, provider credentials, native CLI session state |
| **UHP Server** | Protocol versions/capabilities, configured harnesses, responses, implicit sessions, files/environments, cancellation, structured errors | Refact room membership, public journal, merge policy |
| **Configured harness** | Выбранный opaque `base`, default model, system prompt, tool restrictions, environment, budgets; адресуется server-assigned `harness_id` | Room identity; `base` не является `harness_id` [S5] |
| **External process** | Agent loop, native tools, native transcript/process state | UHP wire contract и Refact room semantics; способ запуска не протекает в client [S2] |
| **Local writer** | Единственный canonical mergeable worktree и последующий merge | Не получает права публиковать private trace advisors |

UHP описывает `Client → Server → complete Harness`; execution behind server implementation-defined [S2]. Поэтому production integration не вызывает `claude`, `opencode` или `codex` из Refact tools, не парсит их stdout и не воспроизводит native session flags. Это было бы отдельной base-specific интеграцией, а не UHP boundary.

Официальный ecosystem document относит UHP к application→harness, A2A — к agent collaboration, MCP — к agent→tools [S14]. Room Router не называется UHP/A2A и не подменяет их.

### 2.2. Scope первой интеграции

- Внешние participants выполняют review, анализ и рекомендации по актуальному writer snapshot.
- Каждый participant получает обычные UHP tasks. Refact metadata с `refact.room.*` является дополнительной correlation-информацией, не требованием к server.
- В public room journal попадает только санкционированная projection завершённого ответа (разделы 6 и 7).
- CE — первый server, но client не содержит CE-only ветвлений. Другой UHP server допускается после discovery и того же live suite.

### 2.3. Non-goals

- Не строить второй полный UHP server и не добавлять rooms/broadcast/membership в UHP.
- Не делать внешнего writer, merge agent или auto-apply патчей; не передавать writer credentials.
- Не транслировать reasoning/raw thinking, tool calls/arguments/results или приватные файлы участников.
- Не поддерживать произвольный base по одному совпадению имени и не обещать model/provider coverage без проверки.
- Не считать mocks, schema validation, upstream matrix или успешный отдельный CLI запуск доказательством сквозной Refact-интеграции.
- Не реализовывать A2A, MCP server или собственный CLI process supervisor внутри `refact-uhp`.
- Не менять цели, permissions, writer lease и plan из внешнего advisor output.

## 3. Обязательное разделение crates и dependency direction

### 3.1. Dependency direction

```text
tools/UI/jobs ──► room application facade ──► refact-room

thin runtime adapters ──► refact-room   (participant/journal abstractions)
thin runtime adapters ──► refact-uhp    (protocol API)

thin runtime adapters ──► AppState/config/SQLite/worktree services

refact-room ──X──► refact-uhp
refact-uhp  ──X──► refact-room
```

`refact-uhp` и `refact-room` **не зависят друг от друга**. Связывание происходит только снаружи: runtime adapter зависит от обоих и реализует `refact_room::Participant` через `refact_uhp::Client`. Это сохраняет возможность заменить official SDK или добавить иной participant protocol без переписывания room domain.

### 3.2. `refact-uhp`: standalone protocol library

Ответственность:

- versioned DTO для discovery, harnesses/models, responses/sessions, errors и используемых environment/file operations;
- HTTP client, bearer credential reference на входе, `UHP-Version` negotiation и validation response headers;
- SSE parsing, gap detection, terminal response handling и tolerant unknown field/event/output-item representation;
- session chain (`previous_response_id`, `metadata.session_id`), per-session serialization и client-side recovery primitives;
- обязательные idempotency keys для Refact task creation/resume, request fingerprints и conflict detection;
- response/session cancellation и polling после disconnect;
- protocol-level **public filtering**: извлечение completed non-empty assistant `output_text` без reasoning, function calls/results и streaming deltas. Room решает, публиковать ли извлечённый body; сам crate о комнате не знает.

Разрешены: `serde`/`serde_json`, `thiserror`, существующие `reqwest`, `futures`, `eventsource-stream`, минимальные `tokio` time/stream/sync primitives, `tracing` и маленькие utility crates по необходимости.

Запрещены: `AppState`/`GlobalContext`, `refact-room`, Buddy, task/card/planner types, room metadata policy, git/worktree APIs, filesystem/path ownership, SQLite, GUI, provider configuration и любые `if base == …`. Environment upload принимает bytes/stream от caller; crate не читает writer tree сам.

### 3.3. `refact-room`: standalone room domain library

Ответственность:

- versioned room journal events и ordering по `room_seq`;
- roster/membership, exactly-one-writer invariant и participant identity;
- router/fan-out, per-recipient `(event_id, recipient_id, logical_attempt)` delivery identity, bounded replay и retry decisions;
- public/private policy, attribution, dedup и safe-boundary publication;
- абстракции `Participant`, `RoomJournalStore` и clock/id generators для deterministic tests.

Разрешены: `serde`/`serde_json`, `thiserror`, `async-trait`, минимальные `tokio` sync/time, `tracing`, UUID/hash utilities.

Запрещены: `refact-uhp`, HTTP/SSE, base names, `AppState`/`GlobalContext`, Buddy, filesystem/worktree operations, SQLite и конкретные secret stores. Domain работает с domain IDs и bytes/text, а persistence и transport реализуются адаптерами.

### 3.4. Runtime connectors — только adapters

Runtime adapter может:

- отобразить room participant config на `harness_id`, model, credential reference и client options;
- преобразовать domain delivery в UHP request, а protocol status/error обратно в domain result;
- сохранить `delivery_id`, request fingerprint, `response_id`, `session_id`, `previous_response_id` и sanitized public projection;
- реализовать journal persistence, environment snapshot export и локальный participant;
- пробросить cancellation token, deadlines, trace context и redacted telemetry.

Runtime adapter не может содержать собственный retry scheduler, privacy rules, writer election, SSE state machine или второй набор UHP DTO. Эти правила принадлежат соответствующему standalone crate. Tools/UI/jobs вызывают room application API; им запрещено обходить его прямыми UHP calls.

### 3.5. Обязательный review gate

PR отклоняется как **монолитная каша**, даже если tests зелёные, при любом из признаков:

- raw `/v1/responses`, `/v1/sessions`, UHP headers или SSE parsing находятся вне `refact-uhp`;
- room journal/privacy/roster/writer logic появляется в `refact-uhp`;
- `refact-room` импортирует transport types или зависит от `refact-uhp`;
- adapter содержит business policy вместо mapping;
- runtime/tools содержат base-specific `if`/`match` для Claude Code, OpenCode, Codex; `base` должен оставаться opaque string, а различия идти из discovered capabilities/config;
- UHP DTO дублируются в room/runtime или public projection читает reasoning/tool output;
- standalone crates начинают тянуть engine state, filesystem или Buddy ради удобства.

## 4. Support matrix внешних harnesses

Снимок upstream source: HarnessRouter commit `5a7451753563d8c1cafec6b95a89a52ed0c6ff41`, проверен 2026-11-24. CLI pins относятся к этому снимку, а не ко всем существующим CE images. При rollout фиксируются фактические image digest, `/v1/uhp implementation`, CLI version и model.

| Base | Upstream заявлено и реализовано в CE source | Upstream измерено | Versions/capabilities из снимка | Refact production status |
|---|---|---|---|---|
| **Claude Code** (`claude-code`) | README и support matrix включают base; CE устанавливает npm package при первом запуске | Для нескольких Anthropic models опубликованы pass в first turn, follow-up, model switch, artifact и recycle [S11]. Это проверки HarnessRouter, не нашей интеграции | `CLAUDE_CODE_VERSION=2.1.311` по умолчанию, override поддержан [S12]. Минимальная CLI version зависит от model. Hard tool blocking имеет отдельную upstream проверку, но её нужно повторить на выбранном pin | **Требуется отдельный Refact live smoke 9.3.** Начальный candidate, ещё не совместим |
| **OpenCode** (`opencode`) | README/support matrix включают base; CE содержит отдельный installer и driver | Опубликованы pass для различных Anthropic/OpenAI/Google routes, включая пять сценариев выше; notes фиксируют provider-specific retries и необходимость корректного `/v1` base URL [S11] | `HR_OPENCODE_VERSION=1.18.23`, digest проверяется installer'ом [S12]. Hard enforcement всех mutating tools не установлен опубликованной matrix | **Требуется отдельный Refact live smoke 9.4.** Результат Claude Code сюда не наследуется |
| **Codex** (`codex`) | README/support matrix включают base; CE поддерживает app-server path | Много upstream pass, но notes сохраняют model/account-specific switch и recycle failures. Нельзя обобщать pass одной модели на весь base [S11] | `HR_CODEX_VERSION=0.154.0`; `HARNESS_CODEX_APPSERVER=1` по умолчанию [S12] | **Следующий candidate после Claude Code/OpenCode; собственный live smoke по 9.5.** Не входит в первый support claim |
| **Другие bases** | CE перечисляет Hermes, Pi, DeepSeek Harness и другие; UHP deliberately не перечисляет закрытый набор `base` | Support matrix содержит разные результаты по base×model×provider, включая FAIL и unrun | Узнаются только через live configured harness/models discovery. `base` opaque [S5] | **Unsupported по умолчанию.** Каждый добавляется отдельным ADR amendment + live evidence |

Количество bases на разных upstream страницах отражает разное время публикации и не используется как support guarantee. Production registry Refact хранит не флаг «Claude поддерживается», а evidence record: server implementation/version, image digest, protocol versions, base, CLI pin, configured harness id, provider/model, capabilities, suite report и timestamp.

### 4.1. Discovery вместо вывода capabilities из имени

Перед настройкой participant `GET /v1/uhp` должен показать `protocol: uhp`, поддержку `2026-10-19` и согласованный `conformance_class` [S3]. Отсутствующий capability key трактуется как `false`.

Для первого профиля требуются:

- Core: `streaming`, `sessions`, `cancellation`;
- `idempotency: true` — иначе production delivery не запускается;
- `session_listing: true` — для recovery/inspection;
- `environments: true` и `files_output: true` — для read-only project snapshot и контролируемых artifacts;
- `harness_management` и `harness_idempotency` — только если harness provisioned через API, а не Console;
- `harness_retirement` — читается перед delete, не предполагается;
- `resume`, `input_requests`, `input_history`, `session_branching`, `Last-Event-ID` — optional, client не строит базовый путь на них.

`GET /v1/harnesses/{id}/models` должен подтвердить выбранный `model` как `available: true`; это динамический credential/provider promise, не статическая характеристика base [S5]. Response сверяется с requested model: `metadata.model_fallback: true` запрещает считать smoke успешным для исходной model.

### 4.2. Что upstream conformance доказывает

Опубликованный measured CE report относится к CE `0.25.6`, UHP `2026-09-12`, full, 2026-09-27 [S13]. Он не доказывает conformance текущего image для `2026-10-19`, Refact client или room privacy. Официальная suite проверяет **server**; для нашего client и router нужны отдельные mock/live tests. `SKIP` не является pass, а `--only` не даёт class conformance claim [S13].

## 5. Process, environment и session lifecycle

### 5.1. Provision и start

1. Оператор выбирает CE release tag и **immutable image digest**; `latest` запрещён для evidence-bearing rollout. В журнал конфигурации попадают image digest и CE version.
2. CE запускается с persistent `/data` volume, loopback-only published port и минимальным `HR_BACKENDS=claude,opencode` для первого rollout. Добавление Codex — отдельное изменение.
3. `HR_SECRET_KEY` задаётся и хранится отдельно от volume; иначе сгенерированный ключ лежит рядом с данными и защищает только часть угроз [S10]. Перед upgrade делается consistent stopped-container backup, key остаётся тем же.
4. Console password меняется, provider credentials вводятся в CE secret store, а отдельный CE API key создаётся для Refact backend. Ни один ключ не попадает в browser code или room messages.
5. После `ready on :3000` client выполняет discovery без bearer token, затем authenticated harness/models discovery. API root CE из quickstart: `http://127.0.0.1:3000/api/harness`; protocol endpoints добавляют `/v1/...` [S9].
6. Для каждого external participant создаётся отдельный configured harness с выбранными base/model, read-only environment, budgets и минимальными tools. При API create используется отдельный stable `Idempotency-Key`; повторное создание по имени запрещено.
7. Refact сохраняет server-assigned `harness_id` (`chrn_…`), а не вычисляет его из base/name. `base` неизменен за lifetime configured harness; смена base = новый harness [S5].

Нельзя считать установку готовой, пока discovery не подтвердил нужный protocol version/capabilities, model `available: true`, а CLI install log не зафиксировал фактические versions.

### 5.2. Durable participant и delivery records

Минимальное persistent состояние, необходимое UHP-1/RM-5:

| Record | Обязательные поля |
|---|---|
| `ExternalParticipantBinding` | `participant_id`, server root, server implementation/version, image digest, `harness_id`, opaque base, CLI pin, model, credential profile ref, environment id/version, protocol version, capability snapshot |
| `UhpSessionBinding` | `participant_id`, `session_id`, `last_response_id`, last terminal status, created/updated timestamps, bounded replay cursor |
| `UhpDeliveryState` | `delivery_id`, `room_id`, `event_id`, `recipient_id`, `logical_attempt`, canonical request fingerprint, idempotency key, request state, `response_id`, `session_id`, source revision + dirty-tree digest, redacted error class, timestamps |
| `ExternalEvidenceRecord` | smoke suite id, server/CLI/model versions, checks PASS/FAIL/SKIP, generated_at, sanitized report location |

Room journal остаётся authoritative для room ordering/delivery. UHP response store authoritative для task result после disconnect [S4]. CE Console/native transcript не становится вторым room journal.

### 5.3. Environment и актуальный writer worktree

UHP Environment — построенная, версионируемая, **read-only** проектная layer; каждая session видит её активную version рядом с собственной writable private working directory. Session не может изменять environment или читать чужую working directory [S6]. Следовательно, UHP Environment не равен live host worktree.

Для внешнего advisor Refact создаёт **свежий read-only snapshot canonical writer worktree**:

1. Writer стартует первым. Snapshot включает tracked state и разрешённые uncommitted changes, необходимые для ревью; secrets, VCS internals, credentials и исключённые policy файлы не копируются.
2. До export вычисляются source revision и canonical dirty-tree digest. Те же значения сохраняются в delivery record и prompt correlation metadata.
3. Snapshot импортируется и строится как новая environment version. Symlink/path escape отклоняются; dependency/build scripts не получают writer credentials и не запускаются с большими правами, чем task.
4. Advisor читает только эту version. Его private working directory остаётся writable по UHP, но не связано с canonical worktree и не имеет merge path.
5. Перед публикацией ответа writer state проверяется повторно. Если revision/dirty digest изменился, результат помечается `stale_source` и не выдаётся за ревью текущего кода.
6. Для long-running session каждая новая room delivery привязывается к новой source version/digest. Старая conversation может продолжаться, но freshness не наследуется от предыдущего turn.

Прямой live bind-mount writer worktree не предполагается: это deployment-specific поведение вне гарантии UHP. Если будущий server его предложит, потребуется отдельный security review и write-canary proof. Snapshot freshness обязателен и для committed, и для uncommitted smoke markers.

### 5.4. Auth и credential profiles

- CE Console password, CE API bearer key и provider API key — три разных секрета [S9].
- Participant config хранит только opaque credential profile reference. `refact-uhp` получает bearer token на transport boundary; room domain видит лишь reference/identity.
- Provider keys создаются per environment с least privilege и ограниченным budget/scope. Они не включаются в `input`, `instructions`, `metadata`, idempotency key, trace logs или public errors.
- CE loopback plaintext допустим только для local development. Не-loopback требует TLS по UHP [S2], network ACL и отдельной secret rotation procedure.
- UHP object scope предполагает `404` для чужих объектов; Refact не пытается различать существование чужого `response_id`/`session_id` [S2].
- Model traffic уходит настроенному provider. Это отдельный privacy destination, требующий пользовательского разрешения; self-host CE не означает локальный inference.

### 5.5. Session creation, continuation и streaming

Первая task создаёт session неявно. Client сохраняет `response.id` и `metadata.session_id`; следующая логическая delivery использует `previous_response_id` последнего принятого response и тот же `harness_id` [S3][S7]. `session_id` не подставляется в `previous_response_id`.

Для каждой session существует одна ordered queue. Вторая task не отправляется, пока предыдущая не terminal. Если внешний actor всё же вызвал concurrent task, `409 session_busy` означает wait для terminal + retry с тем же idempotency key/body, а не новую session и не параллельный запуск [S3].

Streaming включается через `stream: true`. `sequence_number` начинается с 0 и не имеет gaps; room `room_seq` с ним не смешивается. Client игнорирует unknown event/output item/field, но сохраняет terminal response. Текст публично собирается только из финального response, не из deltas [S4]. Отсутствие streaming не отменяет server-side task; stored response — source of truth.

### 5.6. Cancellation

- Cancel одной delivery: `POST /v1/responses/{response_id}/cancel`.
- Stop всей работы participant в текущей session: `POST /v1/sessions/{session_id}/cancel`.
- Cancellation asynchronous: response acknowledgement не равен terminal. Client ждёт `status: cancelled`, сохраняет partial output приватно и оставляет session continuable [S7].
- В SSE cancelled приходит как `response.failed` с вложенным `response.status: cancelled`; authoritative поле — status, не имя event [S4].
- Cancel уже terminal task идемпотентен. Delete response/session не используется как cancel; `DELETE /v1/responses/{id}` не должен останавливать running task [S8].
- Если начальный ответ потерян и `response_id` ещё неизвестен, сначала повторяется исходный request с тем же idempotency key для восстановления id; новый request с новым key запрещён.

### 5.7. Reconnect и recovery

| Ситуация | Обязательное действие |
|---|---|
| Потерян ответ до получения `response_id` | Повторить **тот же** method/path/body и Idempotency-Key после проверки fingerprint. Не создавать новую logical attempt |
| SSE disconnect после `response.created` | Не повторять POST. Poll `GET /v1/responses/{id}` до terminal, затем продолжить обычную обработку |
| SSE sequence gap/malformed stream | Закрыть stream как untrusted incomplete transport, получить stored response. Не «чинить» пропущенный текст и не полагаться на optional `Last-Event-ID` |
| Refact process restart | Восстановить durable delivery; с known `response_id` poll, без него — idempotent retry. Идемпотентность не зависит от in-memory state |
| HTTP client timeout | Не считать task остановленной. Poll/cancel по сохранённому id; streaming использует inactivity timeout, не короткий total timeout [S15] |
| `session_busy` | Дождаться terminal in-flight response, затем retry того же request; учитывать valid `retry_after_ms`, иначе bounded backoff |
| `session_expired` | Не повторять continuation в цикле. Создать новую session с bounded public replay, новой logical attempt и видимой связью recovery→original delivery |
| `response_not_found` / `harness_mismatch` | Остановить automatic retry, проверить persisted chain/config; не перескакивать молча на новую session |
| Server restart → `failed`, `error.detail.reason: interrupted` | Прочитать `workspace` и `effects_may_repeat`. Automatic resume только при advertised `resume`, сохранённом/checkpoint workspace и `effects_may_repeat: false`; request имеет собственный stable idempotency key |
| Interrupted с `effects_may_repeat: true` или lost workspace | Не делать скрытый resume. Создать видимую новую review attempt с bounded context и отметить возможное повторение затрат/эффектов |
| Terminal `failed` | Не retry по одному transport правилу: ресурсы уже потреблены. Классифицировать provider/harness error и применить room attempt policy [S15] |

Любая новая logical attempt получает новый delivery/idempotency key и ссылку на предыдущую. Она никогда не переиспользует key с изменённым `previous_response_id`, environment, model или prompt. `store: true` обязателен для advisor tasks; иначе базовый disconnect recovery невозможен.

### 5.8. Retirement

1. Participant помечается `retiring`: новые deliveries не принимаются, replay прекращается.
2. In-flight task отменяется по явному operator/user request либо дожидается terminal согласно shutdown policy; результат/ошибка сохраняются redacted.
3. Room journal и evidence records не удаляются вместе с UHP config.
4. Перед `DELETE /v1/harnesses/{id}` читается `harness_retirement`. При `false` sessions/responses должны сохраняться; при `true` delete обязан сообщить `restorableUntil`, и эта information доводится до оператора [S5].
5. Session deletion — отдельное действие по retention policy: оно сначала отменяет in-flight work и удаляет transcript/trace/working folder [S7]. Нельзя делать его неявным побочным эффектом remove participant.
6. После drain удаляются/отзываются credential references и API keys по отдельной процедуре. Environment versions удаляются после проверки отсутствия других harness users.

## 6. External advisor security

### 6.1. Writer и merge boundary

- В room ровно один local `writes=true` participant. External UHP participant всегда `writes=false`.
- Canonical worktree, git credentials, merge operation и tool approvals принадлежат local Refact runtime. Они не экспортируются в UHP environment.
- External advisor получает только read-only environment snapshot (5.3). Его writable scratch directory не монтируется обратно и не считается branch.
- Advisor output — untrusted recommendation. Patch, command или «уже исправлено» из assistant body не исполняется и не применяется автоматически.
- `disabledTools` настраивается дополнительно, но UHP допускает instruction-only enforcement для базы [S5]. Поэтому production gate требует наблюдаемый failed write-canary и неизменный canonical digest; отказ модели писать без tool attempt считается `SKIP`, не security pass.
- Внешний participant не получает MCP servers, web tools или shell сверх необходимого read/review набора. Любое расширение capabilities — новый privacy/security review.

### 6.2. Public/private output filtering

| Данные | Room peers / public journal | Local secured diagnostics |
|---|---|---|
| Completed non-empty assistant `output_text` после freshness/policy checks | Да, с author participant id, response id и source digest | Да |
| Streaming text deltas | Нет | Только transient processing, не room history |
| Reasoning summaries/raw thinking | Нет | Не экспортируются в Buddy/peers; доступ к native diagnostics ограничен policy |
| `function_call`, arguments, `function_call_output` | Нет | Только redacted secured evidence при необходимости |
| Failed/cancelled/incomplete partial text | Не публикуется автоматически | Сохраняется для diagnosis согласно retention |
| `input_request` | Не assistant publication; преобразуется в отдельный room/user control state | Да, без secret answer |
| Artifact annotations/files | Только безопасные references после explicit policy, не bytes по умолчанию | Quarantine/private по умолчанию |
| Roster, writer identity, status, sanitized errors | Да | Да |

`refact-uhp` предоставляет protocol-level extractor completed assistant bodies; `refact-room` выполняет окончательный publication gate. Оба слоя обязательны. Пустой body не создаёт public event. Дубликат того же response/turn не публикуется повторно.

### 6.3. Artifact handling

- Первый profile запрашивает текстовое ревью с path/line citations и не скачивает artifacts автоматически.
- Если artifact явно нужен, он помещается в private quarantine: проверяются container/file ids, path confinement, regular-file type, size/count limits и отсутствие symlink/traversal. UHP conformance отдельно проверяет traversal и `nosniff`, но это не заменяет нашу content policy [S13].
- Artifact не становится executable input, не подключается как skill/plugin и не записывается поверх writer file.
- Публикация ссылки/содержимого требует secret scan, media-type validation и пользовательского действия. Любой proposed diff применяет только local writer после собственного review.

### 6.4. Prompt injection boundaries

Содержимое repository, room messages, artifacts, web pages и advisor output считается **untrusted data**, не инструкциями Refact:

1. System/harness instructions явно ограничивают роль read-only reviewer, но enforcement остаётся filesystem/tool/network policy.
2. Текст файла не может расширить tools, запросить credentials, изменить room roster/writer/goal или потребовать публикацию private trace.
3. Advisor не получает секретные environment values для обычного ревью. Если отдельный сценарий требует secret reference, используется short-lived least-privilege scoped credential и CE redaction; redaction не мешает самому agent передать секрет наружу, поэтому network egress также ограничивается [S9].
4. Room delivery содержит только bounded public context. Private reasoning/tool output соседнего participant не подставляется в prompt.
5. Advisor answer возвращается как attributed data. Наличие в нём команд, tool-like markup или роли `system/user` не меняет его фактическую роль.
6. Файл с prompt-injection fixture входит в live suite: ожидается отсутствие запрещённого tool effect и отсутствие секрета/инструкции атаки в public journal. Просто вежливый ответ без доказательства отсутствия effect не засчитывается.

### 6.5. Secrets и logs

Ни bearer keys, provider keys, secret answers, raw prompts с приватным контекстом, tool arguments/results или artifact bytes не пишутся в обычные logs/metrics/room journal. Error message от server считается безопасным по UHP [S15], но Refact всё равно пропускает его через redaction перед публикацией. В telemetry попадают IDs, hashes, status/type/code, versions и durations (раздел 10).

## 7. Refact protocol profile для UHP 2026-10-19

### 7.1. Version negotiation и compatibility

- Каждый request содержит `UHP-Version: 2026-10-19`. Server response header обязан подтвердить фактически применённую версию; mismatch — fatal protocol error, не silent downgrade [S3].
- `GET /v1/uhp` выполняется до credentials и не содержит principal-specific data. `default_version` не подменяет явный выбор client.
- `unsupported_protocol_version` завершает provisioning; `detail.supported` можно показать оператору, но нельзя автоматически выбрать другую версию без отдельного профиля/tests.
- Unknown fields, event types и output item types игнорируются согласно versioning rules [S16]; unknown error code классифицируется по `error.type`.
- Request не использует `tools` и `include`: в этой версии они reserved/ignored [S8]. Tools задаёт владелец configured harness, не room event.
- OpenAPI определяет structure, prose — behaviour; conformance определяется suite, не генерацией типов [S17].

### 7.2. Используемые endpoints

| Endpoint | Профиль Refact |
|---|---|
| `GET /v1/uhp` | Обязателен при provision/reconnect; versions, class, capabilities, implementation |
| `GET /v1/harnesses`, `GET /v1/harnesses/{id}` | Выбор и проверка configured harness; empty list обрабатывается явно |
| `GET /v1/models`, `GET /v1/harnesses/{id}/models` | Проверка default/available/fallback semantics |
| `POST /v1/harnesses` | Optional Full provisioning; stable idempotency key. Console provisioning тоже допустим |
| `PUT /v1/harnesses/{id}` | Optional config update; использовать `ETag`/`If-Match`, если server его выдаёт |
| `DELETE /v1/harnesses/{id}` | Retirement после drain и чтения `harness_retirement` |
| `POST /v1/responses` | Единственный task entry point; `metadata.harness_id`, `model`, `input`, `stream`, `previous_response_id`, `store:true`, budgets |
| `GET /v1/responses/{id}` | Source of truth после disconnect и terminal verification |
| `GET /v1/responses/{id}/input_items` | Secured diagnostics только; не public replay по умолчанию |
| `POST /v1/responses/{id}/cancel` | Cancel одной task; не путать с delete |
| `POST /v1/sessions/{id}/cancel` | Cancel active work в participant session |
| `GET /v1/sessions`, `GET /v1/sessions/{id}`, `GET /v1/sessions/{id}/turns` | Extended inspection/recovery; bounded, private |
| `DELETE /v1/sessions/{id}` | Явное retention action после cancel, не ordinary cleanup |
| Environment create/import/build/version/activate endpoints | Runtime snapshot provisioning; `refact-uhp` переносит supplied bytes, filesystem остаётся в adapter |
| `GET /v1/sessions/{id}/files`, artifact content endpoints | Optional quarantine retrieval, никогда не implicit public download |
| `DELETE /v1/responses/{id}` | Не используется для cancel; только explicit stored-response retention |

Environment закрепляется в configured harness. Per-task override, если понадобится, использует `metadata.environment` по Environments §5 и требует `environment: ready` до start [S6]. `GET /v1/harnesses/{id}/events` и SSE `Last-Event-ID` optional и не входят в обязательный recovery path [S4].

### 7.3. Используемые events

- Lifecycle: `response.created`, `response.in_progress`, `response.completed`, `response.incomplete`, `response.failed`.
- Output assembly: `response.output_item.added/done`, `response.content_part.added/done`, `response.output_text.delta/done`, annotation added.
- Reasoning/tool events (`response.reasoning_*`, `response.function_call_*`) распознаются для private diagnostics/limits, но отбрасываются public filter.
- `error` не является terminal; client обязан дождаться terminal event или восстановить final response через GET.
- Ровно один terminal event ожидается последним. Terminal содержит полный response и достаточен без промежуточных deltas [S4].
- `incomplete` с `input_required`, `max_steps` или `timeout` не ошибочно переводится в `failed`; `cancelled` определяется по response status внутри `response.failed`.

### 7.4. `refact.room.*` extensions

Refact может добавить в request `metadata` следующие informational keys:

- `refact.room.schema_version: 1`;
- `refact.room.room_id`, `refact.room.event_id`, `refact.room.room_seq`;
- `refact.room.recipient_id`, `refact.room.delivery_id`, `refact.room.logical_attempt`;
- `refact.room.author`, `refact.room.source_revision`, `refact.room.source_digest`.

Это **Refact vendor extensions**, не UHP room protocol. Client не требует от server их сохранения, echo или особой обработки: authoritative mapping хранится в durable `UhpDeliveryState`. Значения не содержат credentials или private tool trace. Room membership/ACL/writer/fan-out никогда не кодируются как будто стандартные UHP fields.

Внутренние domain errors могут использовать имена `refact.room.delivery_failed`, `refact.room.policy_blocked`, `refact.room.stale_source`; они не отправляются как стандартные UHP error codes. Внешний server видит обычные tasks, а mock-only extensions не считаются доказательством совместимости с реальным CE.

### 7.5. Idempotency mapping

Каждая room fan-out delivery — отдельный logical UHP request. Начальный ключ вычисляется детерминированно:

```text
refact-room-v1-<hex_sha256(room_id + NUL + event_id + NUL + recipient_id + NUL + decimal(logical_attempt))>
```

- `logical_attempt=0` для первой delivery. Transport retries не меняют attempt/key/body. Новая попытка после terminal failure или новой session — новый attempt с audit link.
- Перед первым send сохраняются key и SHA-256 fingerprint: normalized method/path, protocol version, participant/credential scope id и canonical request body. Секреты в fingerprint не входят.
- Retry допустим только при полном совпадении fingerprint. Изменение `previous_response_id`, environment, model, harness или prompt требует новой logical attempt.
- При потере response client повторяет тот же request. Server обязан вернуть результат первого и не запускать вторую execution; если первая ещё running, retry ждёт её [S8].
- `409 idempotency_key_reused` — fatal consistency defect/конфликт, а не повод отправить ещё раз. Проверяются persisted fingerprint и server history.
- Два recipients получают разные keys и sessions. Повтор одного `(event_id, recipient_id)` не дублирует task; сбой одного не меняет ключи другого.
- Harness create использует отдельное key namespace `refact-uhp-harness-v1-<hash(participant_id + config_revision)>`.
- Resume получает `refact-room-resume-v1-<hash(delivery_id + interrupted_response_id)>` и тоже подчиняется fingerprint rule.

UHP лишь рекомендует хранить task keys не менее 24 часов [S8]. Refact хранит собственные keys весь retention delivery, но после server retention сначала выполняет reconciliation; отсутствие гарантированной dedup не превращается в слепой новый POST.

### 7.6. Error taxonomy и решения client

Non-2xx request error имеет `{error:{type,code,message,param,detail}}`. Task failure — HTTP 200 + `status:failed` + non-null `error`; это разные каналы [S15]. Неизвестный `code` обрабатывается по `type`, человекочитаемый text не парсится для control flow.

| Класс | Примеры | Действие |
|---|---|---|
| `invalid_request_error` | `invalid_input`, `unsupported_protocol_version`, `unsupported_base`, `model_unavailable` | Не retry unchanged; config/client error, видимый sanitized status |
| Scope/not found | `harness_not_found`, `response_not_found`, `session_not_found`, `session_expired` | Reconcile persisted objects; expired session — явная новая attempt по 5.7, остальное не маскировать |
| Conflict | `session_busy`, `harness_mismatch`, `idempotency_key_reused`, `resume_not_pending` | Busy — wait+same retry; mismatch/reuse — stop и diagnose; resume state — reread session |
| `authentication_error` | `missing_credential`, `invalid_credential` | Не retry; credential rotation, no secret logging |
| `permission_error` | `insufficient_scope` | Не retry; уменьшить запрошенную операцию или изменить явно выданный scope |
| `rate_limit_error` | `rate_limited`, `quota_exhausted` | Первое — retry после `Retry-After` с тем же key; второе — ждать budget/config change, не loop |
| `server_error` | 500/502/503/504, `harness_unavailable`, `environment_unavailable` | Bounded exponential backoff+jitter с тем же key; response reconciliation после каждого ambiguous timeout |
| Task `harness_error` / `provider_error` / `timeout` | HTTP 200 failed response | Не transport retry; новая visible attempt только по room policy после учёта consumed resources |
| Cancelled | `status:cancelled` | Terminal user-intended state, не failure alert; continuation остаётся возможной |
| Unsupported optional feature | `resume_not_pending`, missing capability, `branch_unavailable` | Не эмулировать feature через другой request; использовать базовый recovery path |

Отдельно проверяются `metadata.ignored_fields` и model substitution. Если server проигнорировал нужное поле или фактическая model отличается от requested без разрешённого config, capability smoke не засчитывается. UHP допускает либо `422 model_unavailable`, либо явный fallback с `requested_model`, `model_fallback` и reason [S8].

## 8. Dependency decision: Rust client

### 8.1. Результат исследования

Официальный `protocol/IMPLEMENTATIONS.md` на проверенном commit содержит server/client examples, но **не содержит Rust SDK** [S18]. Сам HarnessRouter CE — Python, Apache-2.0 [S9]. Поиск crates.io и исходников не выявил поддерживаемого official UHP Rust client с нужными discovery/session/SSE/idempotency semantics.

Версии ниже — те, которые фактически проверены 2026-11-24; «updated» — дата публикации версии по crates.io, не оценка всего репозитория.

| Crate | Version / license / updated | Покрытие и maintenance evidence | Решение |
|---|---|---|---|
| [`uhp`](https://crates.io/crates/uhp) | `0.1.1`, MIT, 2026-08-24 | Одна версия, 3 Rust LOC, 872-byte crate, нет features/docs; repository не HarnessRouter [C1] | **Reject:** stub, нет protocol client |
| [`openuhp`](https://crates.io/crates/openuhp) | `0.1.1`, MIT, 2026-08-24 | Одна версия, 3 Rust LOC, 880-byte crate, нет features/docs; тот же unrelated owner [C2] | **Reject:** stub |
| [`harness-router`](https://crates.io/crates/harness-router) | `0.2.2`, MIT, 2026-06-09 | Binary `hr`, `has_lib:false`; переключает OAuth/API accounts для coding CLIs, не реализует UHP [C3] | **Reject:** unrelated account switcher |
| [`async-openai`](https://crates.io/crates/async-openai) | `0.43.0`, MIT, 2026-11-01 | Активно выпускается; feature `responses`, streaming, custom HTTP configuration [C4]. Но это OpenAI client без UHP discovery/harness/session model; `ResponseStreamEvent` — closed serde enum без unknown variant [C4S] | **Reject как UHP SDK/wrapper:** нарушает обязательную forward compatibility UHP events |
| [`progenitor`](https://crates.io/crates/progenitor) | `0.17.0`, MPL-2.0, 2026-11-19 | Maintained OpenAPI client generator, macro/build.rs, streams pagination; требует Rust `1.97` [C5]. Target — OpenAPI **3.0.x** [C5S], тогда как UHP schema — **3.1.0** [S17] | **Reject для текущего profile:** нет доказанной совместимости с 3.1 + высокий toolchain floor |
| [`typify`](https://crates.io/crates/typify) | `0.10.1`, Apache-2.0, 2026-11-16 | Maintained JSON Schema→Rust type generator, default macro [C6]. Не даёт HTTP/SSE/session/idempotency/cancel semantics | **Не SDK.** Возможен отдельный build-time эксперимент позже, но не dependency решение UHP-1 |
| [`eventsource-stream`](https://crates.io/crates/eventsource-stream) | `0.2.3`, MIT OR Apache-2.0, 2022-02-17 | Маленький stable SSE primitive; release старый, но уже используется engine `refact-runtime-api` и `refact-daemon` [C7] | **Reuse существующий:** только framing SSE, protocol validation остаётся нашим |
| [`reqwest-eventsource`](https://crates.io/crates/reqwest-eventsource) | `0.6.0`, MIT OR Apache-2.0, 2024-03-29 | Generic reqwest SSE helper, не UHP [C8] | **Не добавлять:** второй SSE stack без дополнительного protocol coverage |

Имена `harnessrouter`, `unified-harness`, `unified-harness-protocol` на crates.io при проверке вернули 404. Отсутствие crate сегодня не является утверждением о будущем; перед UHP-1 и перед крупным upgrade поиск повторяется.

### 8.2. Выбранный вариант: build narrow, reuse primitives

Создаётся `refact-uhp` с минимальным owned surface; никаких новых dependencies в DOC-0 не добавляется. Уже существующие engine `reqwest 0.13.x`, `serde_json` и `eventsource-stream 0.2.3` достаточны как transport primitives. Это **не называется готовым UHP SDK**: protocol semantics принадлежат нашему crate и покрываются tests.

Минимальный public API:

1. `Discovery` + `Client::discover`, version/capability validation.
2. Harness/model list/get и optional provision/update/retire methods.
3. `CreateResponse`, `Response`, `UhpError`, `SessionSummary/Turn` DTO с tolerant unknown fields.
4. `create_response` с обязательным idempotency key + fingerprint, `get_response`, response/session cancel.
5. SSE stream как protocol events с unknown variant, sequence validation и terminal guarantee; cancellation-safe drop не считается server cancel.
6. `SessionChain` для `previous_response_id`/`session_id` и one-active-task serialization.
7. Environment/file transport поверх caller-supplied bytes/streams, без filesystem dependency.
8. Typed error classification/retry advice без room scheduling.
9. Completed assistant public-body extractor без private output types.
10. Redacted diagnostics: IDs, versions, status/type/code, usage и ignored/fallback metadata.

Owned API не экспортирует `reqwest::Client`, raw SSE frames или `serde_json::Value` как основной domain contract. `Value` допустим внутри unknown extension payload. Это ограничивает замену implementation одним crate boundary.

### 8.3. Условия замены на third-party/official SDK

SDK можно принять вместо owned transport только после нового ADR review, если оно:

- maintained и явно поддерживает UHP `2026-10-19`, а не только OpenAI Responses wire shape;
- покрывает discovery, version negotiation, configured harness/models, continuation, cancellation, idempotency и нужные environment/file calls;
- игнорирует unknown fields/events/output items и классифицирует unknown errors по type;
- позволяет обязательные headers, generous/inactivity timeouts, redaction и cancellation;
- имеет совместимые license/Rust MSRV и воспроизводимые conformance/client tests.

Замена ограничивается внутренней реализацией `refact-uhp`; `refact-room`, tools и runtime mapping меняться не должны. Наличие только generic `reqwest + serde` или OpenAI Responses support не удовлетворяет этим критериям.

## 9. Live rollout и критерии приёмки

### 9.1. Stage A — pinned local CE и mock conformance

1. Зафиксировать CE image tag+digest, CLI pins, provider/model и config. Запустить loopback CE по 5.1 с отдельным test volume и fake/non-production credentials.
2. Сохранить discovery JSON, response version headers, configured harness/model JSON и install logs с фактическими CLI versions. Secrets удалить из evidence.
3. Прогнать official suite против CE для требуемого class, обычно:

   ```bash
   uhp-conformance \
     --base-url http://127.0.0.1:3000/api/harness \
     --api-key "$UHP_API_KEY" \
     --class full \
     --harness-id "$UHP_HARNESS_ID" \
     --json ce-evidence.json
   ```

   Suite выполняет реальные платные agent tasks. Сохранить `suite_version`, `generated_at`, coverage, skips и failures; `conformant_with_skips` не называть conformant [S13].
4. Прогнать `refact-uhp` tests против собственного mock server: exact version headers, discovery absent/false capabilities, non-streaming/streaming parity, forward-compatible unknowns, error envelope, sequence gap, inactivity, cancellation и stored-response recovery.
5. Прогнать mock room fan-out: один room event → два configured harnesses → две independent sessions; per-recipient idempotent retry, `session_busy`, cancel одного participant, privacy и bounded replay.
6. Отдельно проверить malformed mock responses. Mock green означает только корректность нашего client/router против ожидаемого контракта; это **не** evidence реального Claude Code/OpenCode.

Official conformance измеряет CE server. Mock conformance измеряет наш client. Для production нужны оба результата плюс live base smoke.

### 9.2. Общий формат live evidence

Каждый base получает собственный отчёт и проходит все применимые проверки независимо. Отчёт содержит:

- date/time, operator, Refact commit, CE image digest/version, CLI version, protocol version, capabilities;
- `harness_id`, base, provider connection type, requested/factual model, environment version;
- PASS/FAIL/SKIP по каждой проверке, sanitized response ids, timing и error code/type;
- canonical source revision + dirty digest до/после, write-canary result;
- public journal excerpt и secured private evidence reference с restricted retention.

`SKIP`, model refusal, fallback на другую model или отсутствие tool attempt не засчитывается как PASS. Smoke повторяется при смене CE major/minor, CLI pin, base, provider route или security profile.

Обязательный общий сценарий:

1. Fresh task streaming возвращает marker и progressive events; terminal stored response совпадает по content.
2. Follow-up через `previous_response_id` сохраняет `session_id` и помнит marker из первого turn.
3. Два одинаковых request с одним key возвращают один `response_id`; в session одна turn, фактическая execution одна.
4. Во время long task client queue не отправляет вторую task. Отдельный raw probe подтверждает `409 session_busy`; затем same-key retry проходит после terminal.
5. При disconnect после `response.created` Refact делает GET и публикует результат один раз, без нового POST.
6. Cancel достигает `cancelled`, acknowledgement и terminal различаются; session остаётся continuable.
7. Committed marker M1 и **uncommitted** marker M2 видны advisor; write attempt в environment наблюдается и завершается OS/policy failure; canonical digest не меняется.
8. Public journal содержит attributed completed body и source digest, но не reasoning/tool names/arguments/results, secret canary или private artifact bytes.
9. Изменение writer state во время review приводит к `stale_source`, а не к публикации как актуального результата.
10. Requested/factual model совпадает; `model_fallback` отсутствует. Prompt-injection fixture не создаёт forbidden effect.

### 9.3. Stage B — реальный Claude Code smoke

Цель: доказать сквозной путь **Refact Room Router → `refact-uhp` → CE → configured `claude-code` harness → реальный provider**, а не только работу CLI или CE Console.

Обязательные критерии:

1. Evidence фиксирует выбранный Claude Code pin (source default снимка `2.1.311` либо явно одобренный override) и совместимость с выбранной model. Лицензия CLI отмечена отдельно от CE Apache-2.0.
2. Все 10 общих проверок 9.2 PASS. Для follow-up дополнительно выполняется CE restart/cold restore, затем marker вспоминается в той же session без повторного initial task.
3. Streaming progressive (не один flush), sequence gapless; неизвестный event проверен отдельно mock'ом, реальный stream не подменяется записанным.
4. Claude-specific tool restriction: запрещённый shell/write tool действительно не выполняется. Evidence содержит observed attempted operation и enforcement result; «модель сказала, что не может» без проверки deny path недостаточно.
5. Environment write-canary независим от model compliance завершается failure; writer revision/dirty digest побайтно не изменился.
6. Advisor находит оба review markers и формирует ожидаемое path/line review; generated patch/command не применяется runtime.
7. Idempotent retry, busy queue, disconnect recovery и cancel подтверждаются по CE session/turn records, не только по нашим logs.
8. Public/private проверка включает отсутствие Claude reasoning summaries и native tool trace у другого participant и в public journal.
9. В отчёте requested model равна фактически served; старый CLI/model minimum mismatch или provider 4xx не маскируется под infrastructure success.

**GO для Claude Code** возможен только при отсутствии FAIL и непроверенных обязательных пунктов. После GO поддерживается именно проверенный version/model/security profile, не любой Claude Code.

### 9.4. Stage C — реальный OpenCode smoke

Цель и evidence record отдельны от Claude Code; здесь используется другой configured harness, session, CLI pin и собственный provider route.

Обязательные критерии:

1. Evidence фиксирует OpenCode `1.18.23` либо новый явно одобренный digest-checked pin и фактический CE build. Provider base URL normalization, включая требуемый `/v1` там, где он нужен, подтверждён live request [S11N].
2. Все 10 общих проверок 9.2 PASS независимо от результатов Claude Code. OpenCode не наследует session, environment freshness или security verdict другой базы.
3. Провайдерский first turn отвечает exact marker, follow-up помнит контекст, artifact/review и cold recycle после CE restart проходят на выбранной model. Один retry с content-ответом вместо marker фиксируется как reliability finding, а не скрывается.
4. Streaming, continuation, one-active-task, idempotent retry, disconnect и cancel проверяются в OpenCode session records. После cancel не остаётся background process, способного изменить snapshot или artifact store.
5. Environment write-canary выполняется и фактически блокируется read-only mount/OS boundary. Если OpenCode поддерживает лишь instruction-level `disabledTools`, production всё равно опирается на mount/no-merge/no-credentials enforcement; отсутствие hard mount denial = NO-GO.
6. Prompt-injection fixture не вызывает внешний write/network/tool effect. Public journal не содержит native reasoning/tool trace или secret canary.
7. Requested/served model совпадает; aggregator/provider substitution считается FAIL для данного smoke. Проверяется также отсутствие незадекларированного second provider route.
8. Text review соответствует текущему source digest; stale result отбрасывается; никакой OpenCode artifact не применяется автоматически.

**GO для OpenCode** выносится отдельно и только по полному report. GO Claude Code без OpenCode smoke (и наоборот) означает поддержку только одной базы.

### 9.5. Codex и последующие bases

Codex проходит тот же общий сценарий 9.2 плюс собственный app-server path, cold restore, model/account switch matrix и cancellation process check. Опубликованные upstream model-specific failures требуют выбрать и зафиксировать проверенную model/route, а не объявить весь base совместимым. До такого отчёта Codex остаётся candidate.

Любой другой base добавляется через discovery, отдельный support matrix row, security/write-canary и полный live report. «CE его перечисляет» или « Responses API совместим» недостаточно.

### 9.6. Stage D — failure injection

| Injected failure | Ожидаемый результат |
|---|---|
| Unsupported `UHP-Version` / version mismatch | Provisioning refused, zero tasks |
| Unknown response field/event/output item | Неизвестное сохраняется/игнорируется без stream failure; final known result корректен |
| SSE gap, malformed JSON, missing terminal | Public body не собирается из partial stream; GET reconciliation или sanitized protocol failure |
| Disconnect до `response.created` | Same-key/same-body retry, одна execution |
| Disconnect после `response.created` | GET до terminal, никакого второго POST |
| Refact restart в каждом delivery state | Durable recovery без duplicate session/task/publication |
| CE restart / interrupted response | Применяются `workspace`/`effects_may_repeat`; нет скрытого unsafe resume |
| 500/502/503/504 и client timeout | Bounded same-key retry + reconciliation, не новый attempt storm |
| `rate_limited` и `quota_exhausted` | Разные пути: Retry-After vs visible blocked budget без loop |
| `session_busy` | Per-recipient queue ждёт terminal; другие recipients продолжают независимо |
| `session_expired` | Новая session + bounded public replay + новый linked attempt; нет бесконечного continuation retry |
| Idempotency key reuse с другим body | Immediate consistency failure, no request mutation под старым key |
| `harness_mismatch`, missing model, model fallback | Visible config failure; smoke/support не засчитывается |
| Cancel одновременно с completion | Ровно один terminal outcome; cancel terminal idempotent; publication не дублируется |
| Writer changes during turn | `stale_source`, ответ не выдаётся за актуальный |
| Read-only write и prompt injection | Наблюдаемый denied effect, canonical state неизменен |
| Symlink/traversal/oversized artifact | Reject/quarantine, нет записи вне разрешённого store или public bytes |

### 9.7. Stage E — два реальных harness advisors

После отдельных GO Claude Code и OpenCode выполняется combined сценарий:

1. Local writer + оба advisors стартуют на одном source digest; один room event доставляется обоим в independent sessions.
2. Retry одного recipient не создаёт вторую execution и не меняет request другого. `room_seq` упорядочивает journal, SSE sequence остаётся per-stream.
3. Оба завершённых body публикуются с правильным author и source digest. Tool/reasoning/private output одного отсутствует у другого и в журнале.
4. `session_busy`, provider failure или cancel одного не отменяет другого; failure attribution точная.
5. Bounded replay позднего participant не повторяет старые tasks и не раздувает prompt сверх limit.
6. Изменение writer state делает оба результата stale согласно policy, без автоматического merge.

### 9.8. Rollout gates

| Gate | Evidence | Разрешённый следующий шаг |
|---|---|---|
| G0 | Этот ADR + crate boundaries | UHP-1/RM-5 implementation, не production claim |
| G1 | `refact-uhp` и room mock suite, code review boundary | Pinned CE setup |
| G2 | CE discovery + official conformance report без скрытых skips | Real base smoke |
| G3 | Claude Code live report | Claude-only controlled pilot |
| G4 | OpenCode live report | OpenCode controlled pilot |
| G5 | Combined fan-out/privacy/failure injection report | Limited production advisors |
| G6 | Observability/backup/retirement drill и пользовательский sign-off | Расширение rollout; external writer по-прежнему запрещён |

Mock evidence никогда не заменяет G3/G4/G5. Откат означает disable external participants и продолжение local writer workflow; room journal и canonical worktree от этого не зависят.

## 10. Observability и эксплуатационные ограничения

### 10.1. Telemetry

Метрики и structured logs как минимум покрывают:

- discovery/provision success, server implementation/version, negotiated protocol version и capability changes;
- delivery state transitions, attempts, queue depth, time-to-first-event, terminal latency, cancel acknowledgement/terminal latency;
- `session_busy`, idempotent replays, idempotency conflicts, reconnects, SSE gaps, polling recovery, stale source results;
- error `type`/code, terminal status, ignored fields, requested/factual model и fallback flag;
- environment build duration/version, source revision/digest (hash only), artifact count/bytes после limits;
- token usage, если server его действительно сообщает; `null` не превращается в fabricated zero [S8].

Labels не содержат prompt, assistant body, credentials, raw reasoning, tool arguments/results или artifact bytes. Correlation IDs: `room_id/event_id/recipient_id/delivery_id/response_id/session_id` в secured logs; метрические labels ограничены cardinality.

CE поддерживает W3C `traceparent`/`tracestate`: Refact передаёт их на create, provider calls получают child span в рамках CE. `baggage` не используется [S9]. Trace context не заменяет durable delivery ids.

### 10.2. Alerts и audit

- Page/alert: repeated idempotency conflict, version mismatch, public/private filter failure, write-canary success, canonical digest mismatch, artifact escape.
- Operational warning: growing busy queue, repeated reconnect/gaps, expired sessions, provider quota, increasing fallback/substitution, stale results, missing terminal после cancel.
- Каждый automatic new logical attempt виден с reason и previous delivery id. Невозможно молча потратить tokens через retry loop.
- Support evidence versioned и связывается с participant config. Любое изменение pin/profile делает предыдущий verdict historical, а не автоматически текущим.

### 10.3. Эксплуатационные ограничения

- CE quickstart требует Docker и около 4 GB disk; первый запуск устанавливает CLIs в volume. Повторный `docker pull` сам не обновляет running container [S9][S10].
- CE entrypoint требует root для per-session uid, без `--privileged`; не запускать с `--user`. Gateway/Console unprivileged, agent processes — per-session users [S9].
- Sessions в CE используют separate workspaces и OS users, **не отдельные containers** [S9]. Нельзя рекламировать container isolation, которого deployment не даёт.
- Environment build исполняет manifests/`setup.sh` с network/toolchain server; непроверенный project сначала проходит build policy. Read-only mount не устраняет build-time risk [S6].
- UHP требует concurrent tasks в разных sessions и запрещает их в одной; горизонтальный throughput ограничен per-participant queue и provider quotas [S3].
- Agent tasks длятся минуты и стоят tokens. Короткий HTTP timeout, automatic retries и official suite могут создать реальные расходы; применяются inactivity timeout, budgets и видимые attempts [S13][S15].
- Backup выполняется при остановленном CE для consistent SQLite+files snapshot; `HR_SECRET_KEY` и deployment config хранятся отдельно. Restore/upgrade проверяется до production rollout [S10].
- Windows host → Linux Docker CE networking, filesystem export и process cancellation входят в deployment smoke; из UHP docs не следует native Windows CLI support.

## 11. Неизвестные, которые docs не устанавливают

Следующие факты нельзя вывести из спецификации/README и потому они являются обязательными live inputs, а не скрытыми assumptions:

1. Поддерживает ли выбранный CE image `2026-10-19` и все нужные capabilities; опубликованный measured report относится к более старой версии.
2. Фактические CLI versions внутри image/volume и совместимость выбранной model/provider route с этими pins.
3. Реальная model availability, rate limits, quotas, latency, cost и served-model identity у конкретного provider.
4. Какие инструменты выбранный base блокирует аппаратно, а какие лишь instruction-level; гарантирует ли конкретный deployment write denial независимо от model compliance.
5. Поддерживает ли выбранный server `Last-Event-ID`, native resume, cold restore и какие base-specific ограничения имеет continuation/model switch.
6. Фактические session/response retention, idempotency-key retention сверх рекомендованных 24 часов, storage quotas и deletion/restore windows.
7. Request/file/environment size limits, SSE proxy buffering, keep-alive behavior и timeout topology конкретной установки.
8. Как server обрабатывает нестандартные `refact.room.*` metadata. Refact не требует их echo; любое observed persistence — vendor behavior, не contract.
9. Существует ли безопасный live read-only mount для host writer worktree. Базовый ADR его не требует и использует versioned snapshot.
10. Полные upstream license obligations для каждого auto-installed CLI/model/provider; CE Apache-2.0 не покрывает их автоматически.
11. Windows host ↔ Docker CE path/network/cancellation semantics и восстановление после host restart.
12. Поведение других UHP servers и других bases. Тест CE не устанавливает их совместимость.

Эти unknowns блокируют только соответствующий support claim. Они не меняют принятые решения: independent crates, UHP client boundary, local-only writer, explicit version negotiation, idempotent per-recipient delivery, stored-response recovery и mocks≠live evidence.

## 12. Первоисточники
Все online sources проверены 2026-11-24. Ссылки на HarnessRouter source закреплены commit `5a7451753563d8c1cafec6b95a89a52ed0c6ff41`; более поздние изменения требуют пересмотра support matrix и pins.

### Нормативные UHP sources

- [S1] — обзор endpoints UHP `2026-10-19`.
- [S2] — Architecture: roles, conformance classes, object model, TLS/auth scope.
- [S3] — Lifecycle: version headers, discovery, session states, `session_busy`.
- [S4] — Streaming: event vocabulary, sequence, terminal events, reconnect.
- [S5] — Harnesses: opaque base, configured object, models, tool restrictions, management/retirement.
- [S6] — Environments: immutable read-only versions, private working directory, builds/security.
- [S7] — Sessions: `previous_response_id`, inspection, cancellation, deletion.
- [S8] — Tasks: request/response objects, idempotency, model substitution, response delete.
- [S13] — официальная conformance suite и measured evidence rules.
- [S14] — официальная protocol ecosystem граница UHP/A2A/MCP/AG-UI.
- [S15] — error envelope, taxonomy, retry и timeout rules.
- [S16] — additive versioning и обязательная unknown tolerance.
- [S17] — нормативная OpenAPI `3.1.0` schema для `2026-10-19`.
- [S18] — official example implementations registry.

### HarnessRouter CE и support evidence

- [S9] — CE README: quickstart, API root, credentials, idempotency example, architecture и Apache-2.0.
- [S10] — self-hosting guide: image pinning, backends, secrets, upgrades/backups.
- [S11] — support matrix: first/follow-up/switch/artifact/recycle по base×model×provider.
- [S11N] — notes к matrix: tested versions, provider-specific findings и известные ограничения.
- [S12] — entrypoint с фактическими default pins Claude Code/OpenCode/Codex и process isolation.

### Rust dependencies

- [C1] `uhp 0.1.1`; [C2] `openuhp 0.1.1`; [C3] `harness-router 0.2.2` — crates.io metadata.
- [C4] `async-openai 0.43.0` metadata; [C4S] — исходник closed `ResponseStreamEvent`.
- [C5] `progenitor 0.17.0` metadata; [C5S] — заявленный OpenAPI 3.0.x target.
- [C6] `typify 0.10.1`; [C7] `eventsource-stream 0.2.3`; [C8] `reqwest-eventsource 0.6.0` — crates.io metadata.

[S1]: https://unifiedharnessprotocol.org/spec/2026-10-19
[S2]: https://unifiedharnessprotocol.org/spec/2026-10-19/architecture
[S3]: https://unifiedharnessprotocol.org/spec/2026-10-19/lifecycle
[S4]: https://unifiedharnessprotocol.org/spec/2026-10-19/streaming
[S5]: https://unifiedharnessprotocol.org/spec/2026-10-19/harnesses
[S6]: https://unifiedharnessprotocol.org/spec/2026-10-19/environments
[S7]: https://unifiedharnessprotocol.org/spec/2026-10-19/sessions
[S8]: https://unifiedharnessprotocol.org/spec/2026-10-19/tasks
[S9]: https://github.com/HarnessRouter/harnessrouter/tree/5a7451753563d8c1cafec6b95a89a52ed0c6ff41
[S10]: https://github.com/HarnessRouter/harnessrouter/blob/5a7451753563d8c1cafec6b95a89a52ed0c6ff41/docs/self-hosting-guide.md
[S11]: https://github.com/HarnessRouter/harnessrouter/blob/5a7451753563d8c1cafec6b95a89a52ed0c6ff41/docs/support-matrix.md
[S11N]: https://github.com/HarnessRouter/harnessrouter/blob/5a7451753563d8c1cafec6b95a89a52ed0c6ff41/docs/support-matrix-notes.md
[S12]: https://github.com/HarnessRouter/harnessrouter/blob/5a7451753563d8c1cafec6b95a89a52ed0c6ff41/docker/entrypoint.sh
[S13]: https://unifiedharnessprotocol.org/conformance
[S14]: https://unifiedharnessprotocol.org/ecosystem
[S15]: https://unifiedharnessprotocol.org/spec/2026-10-19/errors
[S16]: https://unifiedharnessprotocol.org/versioning
[S17]: https://github.com/HarnessRouter/harnessrouter/blob/5a7451753563d8c1cafec6b95a89a52ed0c6ff41/protocol/schema/uhp-2026-10-19.openapi.yaml
[S18]: https://github.com/HarnessRouter/harnessrouter/blob/5a7451753563d8c1cafec6b95a89a52ed0c6ff41/protocol/IMPLEMENTATIONS.md
[C1]: https://crates.io/api/v1/crates/uhp
[C2]: https://crates.io/api/v1/crates/openuhp
[C3]: https://crates.io/api/v1/crates/harness-router
[C4]: https://crates.io/api/v1/crates/async-openai/0.43.0
[C4S]: https://github.com/64bit/async-openai/blob/async-openai-v0.43.0/async-openai/src/types/responses/stream.rs
[C5]: https://crates.io/api/v1/crates/progenitor/0.17.0
[C5S]: https://github.com/oxidecomputer/progenitor/blob/v0.17.0/README.md
[C6]: https://crates.io/api/v1/crates/typify/0.10.1
[C7]: https://crates.io/api/v1/crates/eventsource-stream/0.2.3
[C8]: https://crates.io/api/v1/crates/reqwest-eventsource/0.6.0
