# Полная сборка Refact (GUI + Rust-движок + VS Code расширение)

> Документ для сборочного агента. Описывает полный конвейер сборки решения
> **Refact** (v8.6.4) на **Windows x64** из чистого состояния:
> 1. Графический компонент (React/TypeScript, `refact-agent/gui`)
> 2. Rust-движок (`refact-agent/engine` → `refact.exe`)
> 3. VS Code расширение (`plugins/vscode` → `.vsix`)

Документ проверен на реальной сборке 17–18.09.2026 (коммит `34e2e6bf9`).

---

## 1. Что за проект

Monorepo Refact: AI-агент для IDE.

| Компонент | Путь | Технология | Артефакт |
|---|---|---|---|
| GUI (чат-UI) | `refact-agent/gui/` | TypeScript / React 18 / Vite | `refact-chat-js-<ver>.tgz` (npm-пакет) |
| Движок (LSP/HTTP-сервер) | `refact-agent/engine/` | Rust 2021, tokio | `target/release/refact.exe` (~203 МБ) |
| VS Code расширение | `plugins/vscode/` | TypeScript | `codify-win32-x64-<ver>.vsix` (~103 МБ) |

**Ключевая связь компонентов:**
- GUI собирается в npm-пакет `refact-chat-js`, который расширение VS Code ставит как зависимость (`file:`-ссылка на `.tgz`).
- Движок **встраивает GUI внутрь себя**: `refact-agent/engine/build.rs` при `cargo build` копирует `refact-agent/gui/dist/chat` → `refact-agent/engine/assets/chat/dist/chat`, и эти ассеты попадают в бинарник. **Поэтому GUI должен быть собран ДО движка** (см. §4).
- Расширение VS Code бандлит `refact.exe` в `plugins/vscode/assets/refact.exe` — движок запускается расширением как локальный процесс.

**Порядок сборки (зависимости):**
```
GUI (npm) ──┬──► Engine (cargo, встраивает GUI-ассеты) ──► assets/refact.exe ──► VS Code .vsix
            └──────────────────────────────────────────────────────────────────────► (tgz в node_modules)
```

---

## 2. Требования к окружению (проверенные версии)

| Инструмент | Версия | Где лежит |
|---|---|---|
| Rust (rustc/cargo) | 1.98.1 (`stable-x86_64-pc-windows-msvc`) | `C:\Users\raysi\.cargo\bin` |
| LLVM (lld-link, libclang) | 18.1.8 | `C:\Program Files\LLVM\bin` |
| MSVC BuildTools (link.exe) | VS 18, MSVC 14.51.36231 | `C:\Program Files (x86)\Microsoft Visual Studio\18\BuildTools\VC\Tools\MSVC\14.51.36231\bin\Hostx64\x64` |
| Node.js | v24.13.0 | в PATH |
| npm | 11.6.2 | в PATH |
| @vscode/vsce | 3.9.2 | через `npx @vscode/vsce` (глобально ставить не обязательно) |

### 2.1. Обязательная инъекция PATH (Windows)

Rust-линковка использует `lld-link.exe` (LLVM) и MSVC-инструменты. Если они не в PATH,
сборка падает с `error: linker 'lld-link.exe' not found`. Перед `cargo build` выполнить:

```powershell
$env:PATH = "C:\Program Files\LLVM\bin;C:\Program Files (x86)\Microsoft Visual Studio\18\BuildTools\VC\Tools\MSVC\14.51.36231\bin\Hostx64\x64;" + $env:PATH
```

### 2.2. Переменные окружения сборки

| Переменная | Значение | Зачем |
|---|---|---|
| `REFACT_SKIP_GUI_BUILD` | `"1"` | Запрещает `engine/build.rs` самому запускать `npm ci`/`tsc`/`vite` внутри cargo. **Ставить только если GUI уже собран И ассеты уже скопированы в `engine/assets/chat/dist`** (иначе в бинарнике будет старый/чужой GUI). Для «чистой» полной сборки лучше НЕ ставить — пусть `build.rs` сам скопирует свежие ассеты. |
| `NODE_OPTIONS` | `--max-old-space-size=8192` (для vite/tsc) | Защита от OOM при сборке GUI. |

---

## 3. Шаг 0. Подготовка

```powershell
Set-Location C:\Raid\Repos\Rust\refact-main
git status          # убедиться, что working tree чистый (или зафиксировать изменения)
git log -n 1 --oneline   # запомнить HEAD-коммит — он попадёт в --version бинарника
```

Убедиться, что ни один запущенный `refact.exe` / VS Code с расширением не держит
`plugins/vscode/assets/refact.exe` в занятом состоянии (иначе `Copy-Item` упадёт).

---

## 4. Шаг 1. Сборка GUI (`refact-agent/gui`)

```powershell
Set-Location C:\Raid\Repos\Rust\refact-main\refact-agent\gui

# 1.1. Чистая установка зависимостей (по package-lock.json)
npm ci

# 1.2. Проверка типов (опционально, но рекомендуется)
$env:NODE_OPTIONS = "--max-old-space-size=8192"
npx tsc --noEmit

# 1.3. Сборка browser-бандла (основной чат-UI)
npx vite build

# 1.4. Сборка node-бандла
npx vite build -c vite.node.config.ts

# 1.5. Упаковка в npm-тарбол (имя: refact-chat-js-<версия>.tgz)
npm pack
```

**Результат:**
- `refact-agent/gui/dist/chat/` — собранный UI (используется движком).
- `refact-agent/gui/refact-chat-js-8.6.4.tgz` — npm-пакет (используется расширением).
  Размер ~33 МБ.

> ⚠️ `npm ci` запускает `postinstall: patch-package` — это нормально, патчи из `patches/` применяются автоматически.
> ⚠️ Не использовать `npm install` вместо `npm ci` — ломает lockfile-детерминизм.

---

## 5. Шаг 2. Сборка Rust-движка (`refact-agent/engine`)

**GUI к этому моменту должен быть собран** (шаг 1). `build.rs` сам скопирует
`gui/dist/chat` → `engine/assets/chat/dist/chat` и встроит в бинарник.

```powershell
# Инъекция PATH (см. §2.1) — ОБЯЗАТЕЛЬНО
$env:PATH = "C:\Program Files\LLVM\bin;C:\Program Files (x86)\Microsoft Visual Studio\18\BuildTools\VC\Tools\MSVC\14.51.36231\bin\Hostx64\x64;" + $env:PATH

Set-Location C:\Raid\Repos\Rust\refact-main\refact-agent\engine

# Полная release-сборка бинарника
cargo build --bin refact --release
```

**Характеристики сборки:**
- Холодная сборка: **~40–50 минут** (ThinLTO + opt-level=z + strip, ~85 крестов + 7 tree-sitter парсеров + SQLite). Инкрементальная — минуты.
- Вывод: `Finished release profile [optimized] target(s) in ...`
- Предупреждения (dead_code, unused imports) — **нормальны**, не являются ошибкой.
- Ошибки в `#[cfg(test)]`-коде (тестовые фикстуры) **не мешают** release-сборке — тестовый код в релиз не компилируется.

**Вариант с `REFACT_USE_PREBUILT_GUI=1`** (рекомендуется, если GUI только что собран в шаге 1):
```powershell
$env:REFACT_USE_PREBUILT_GUI = "1"
cargo build --bin refact --release
```
`build.rs` **не пересобирает** GUI (пропускает npm ci/tsc/vite), но **всё равно копирует** свежие `gui/dist/chat` → `engine/assets/chat/dist/chat` и встраивает в бинарник. Это быстрее полного цикла и гарантирует, что в `refact.exe` попадёт именно что только собрали. **Использовано в проверочной сборке 19.09.2026.**

**Вариант с `REFACT_SKIP_GUI_BUILD=1`** (только если GUI-ассеты уже свежие в `engine/assets/chat/dist/chat`):
```powershell
$env:REFACT_SKIP_GUI_BUILD = "1"
cargo build --bin refact --release
```
Полностью пропускает и сборку, и копирование GUI-ассетов. Без обоих флагов `build.rs` пересоберёт GUI сам (npm ci + tsc + vite ×2) — медленнее, но гарантирует свежие ассеты.

**Результат:** `refact-agent/engine/target/release/refact.exe` (~203 МБ).

### 5.1. Верификация бинарника

```powershell
& C:\Raid\Repos\Rust\refact-main\refact-agent\engine\target\release\refact.exe --version
```
Ожидаемый вывод (коммит = HEAD из шага 0):
```
refact 8.6.4
             version 8.6.4
              commit <HEAD-коммит>
            build_os windows-x86_64
        rust_version rustc 1.98.1 (...)
       cargo_version cargo 1.98.1 (...)
    daemon_version 8.6.4
```
**Проверить, что `commit` совпадает с HEAD из `git log`.** Если не совпадает — бинарник старый, пересобрать.

---

## 6. Шаг 3. Копирование бинарника в расширение

```powershell
Copy-Item C:\Raid\Repos\Rust\refact-main\refact-agent\engine\target\release\refact.exe `
          -Destination C:\Raid\Repos\Rust\refact-main\plugins\vscode\assets\refact.exe -Force

# Проверка
Get-Item C:\Raid\Repos\Rust\refact-main\plugins\vscode\assets\refact.exe | Select-Object Name, Length, LastWriteTime
```
Размер должен совпасть с исходным (~203 084 800 байт), `LastWriteTime` — свежий.

---

## 7. Шаг 4. Сборка VS Code расширения (`plugins/vscode`)

Расширение зависит от GUI-тарбола через `file:`-ссылку. `package.json` в git содержит
`"refact-chat-js": "file:../../refact-agent/gui"` — для надёжной упаковки временно
подменяем ссылку на конкретный `.tgz`, а после сборки **обязательно откатываем** `package.json`.

```powershell
Set-Location C:\Raid\Repos\Rust\refact-main\plugins\vscode

# 4.1. Установка зависимостей
npm ci

# 4.2. Установка GUI-тарбола (путь относительный от plugins/vscode)
npm install ..\..\refact-agent\gui\refact-chat-js-8.6.4.tgz --save-exact

# 4.3. Компиляция TypeScript расширения
npm run compile

# 4.4. Упаковка .vsix
npx @vscode/vsce package --target win32-x64
```

**Результат:** `plugins/vscode/codify-win32-x64-8.6.4.vsix` (~103 МБ).

### 7.1. ОБЯЗАТЕЛЬНО: откат временных изменений

`npm install <tgz> --save-exact` и `npm ci` меняют `package.json` / `package-lock.json`:

```powershell
Set-Location C:\Raid\Repos\Rust\refact-main
git checkout -- plugins/vscode/package.json plugins/vscode/package-lock.json
git status   # должен быть чистым (кроме .vsix и assets/refact.exe, если они не игнорируются)
```

> ⚠️ Если забыть откатить — в git попадёт `file:../../refact-agent/gui/refact-chat-js-8.6.4.tgz`
> вместо `file:../../refact-agent/gui`, что сломает CI-сборку.

---

## 8. Итоговая верификация

| # | Проверка | Команда | Ожидание |
|---|---|---|---|
| 1 | GUI-тарбол существует | `Get-Item refact-agent\gui\refact-chat-js-*.tgz` | 1 файл, ~33 МБ, свежая дата |
| 2 | Бинарник свежий | `refact.exe --version` | commit = HEAD |
| 3 | Бинарник в assets | `Get-Item plugins\vscode\assets\refact.exe` | размер = размер из target/release |
| 4 | .vsix собран | `Get-Item plugins\vscode\codify-win32-x64-*.vsix` | 1 файл, ~103 МБ, свежая дата |
| 5 | Git чистый | `git status` | нет изменений в `package.json`/`package-lock.json` |

---

## 9. Частые проблемы (pitfalls)

| Проблема | Причина | Решение |
|---|---|---|
| `error: linker 'lld-link.exe' not found` | LLVM/MSVC не в PATH | Инъекция PATH из §2.1 перед cargo |
| `Copy-Item: process cannot access the file` | `refact.exe` занят (VS Code / запущенный движок) | Закрыть VS Code / убить процесс `refact.exe` |
| В бинарнике старый GUI | `REFACT_SKIP_GUI_BUILD=1` при несвежих `engine/assets/chat/dist` | Собрать GUI заново и пересобрать движок БЕЗ флага (или скопировать `gui/dist/chat` → `engine/assets/chat/dist/chat` вручную) |
| `npm ci` падает в GUI | Устаревший `node_modules` | `Remove-Item -Recurse -Force node_modules` и повторить `npm ci` |
| OOM в vite/tsc | Не хватает памяти Node | `$env:NODE_OPTIONS = "--max-old-space-size=8192"` |
| `vsce` ругается на LICENSE/репозиторий | — | Не критично, `.vsix` всё равно создаётся; либо `--allow-missing-repository` |
| Тесты движка не компилируются (`retry_count` в `BoardCard`) | Сломанные `#[cfg(test)]`-фикстуры | **Не блокирует** release-сборку. Отдельная задача (P2). |
| Долгая сборка (~40–50 мин) | ThinLTO + opt-level=z | Нормально. Запускать в фоновом терминале. Опционально: sccache (`tools/dev/setup-cache.sh`) для кэша между worktree. |

---

## 10. Быстрый путь: только движок (GUI и расширение не менялись)

Если менялся **только Rust-код** (как в случае с T-28 / `task_agent_monitor.rs`):

```powershell
$env:PATH = "C:\Program Files\LLVM\bin;C:\Program Files (x86)\Microsoft Visual Studio\18\BuildTools\VC\Tools\MSVC\14.51.36231\bin\Hostx64\x64;" + $env:PATH
Set-Location C:\Raid\Repos\Rust\refact-main\refact-agent\engine
cargo build --bin refact --release
& .\target\release\refact.exe --version   # проверить commit
Copy-Item .\target\release\refact.exe ..\..\..\plugins\vscode\assets\refact.exe -Force
```
GUI и `.vsix` при этом **не пересобираются** (если не нужен новый `.vsix` — на этом всё).

---

## 11. Альтернатива: единый скрипт

В репозитории есть `scripts/build-vscode.mjs` (запуск: `node scripts/build-vscode.mjs` из корня).
Он автоматизирует весь конвейер, **но** имеет отличия от проверенного ручного процесса:
- собирает движок с `--no-default-features` (в ручном процессе используется обычный `cargo build --bin refact --release`);
- сам мутирует `plugins/vscode/package.json` и откатывает его в конце (при сбое на середине откат не гарантирован);
- не делает инъекцию PATH для LLVM/MSVC (нужно сделать вручную до запуска).

**Рекомендация:** использовать ручной конвейер из §4–§7 (проверен на сборках 17–18.09.2026).
