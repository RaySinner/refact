# Usage: powershell -NoProfile -ExecutionPolicy Bypass -File tools/dev/verify-chat-harvest.ps1
#
# Inspection-only verification for the Y-1 chat harvest (agent worktrees may not compile):
#   1. structural checks: the two new crate modules exist, are wired, and the engine side is a shim
#   2. body-identity check: every moved fn/struct/enum/const is byte-identical to its original
#      after normalising only `pub(super)`/`pub(crate)` -> `pub`, `crate::runtime_settings` ->
#      `refact_core::trajectory_settings` and `feature = "bench"` -> `feature = "test-util"`
#
# Exits non-zero when any check fails.

$ErrorActionPreference = 'Stop'

$repo = $PSScriptRoot
while (-not (Test-Path (Join-Path $repo 'refact-agent\engine\Cargo.toml'))) {
    $parent = Split-Path $repo -Parent
    if ($parent -eq $repo -or [string]::IsNullOrEmpty($parent)) {
        throw "could not locate the repository root from $PSScriptRoot"
    }
    $repo = $parent
}

$api = Join-Path $repo 'refact-agent\engine\crates\refact-chat-api'
$hist = Join-Path $repo 'refact-agent\engine\crates\refact-chat-history'
$engine = Join-Path $repo 'refact-agent\engine'

$fail = 0
$checks = 0
function Check([bool]$ok, [string]$name) {
  $script:checks++
  if ($ok) { Write-Output ("PASS  " + $name) }
  else { $script:fail++; Write-Output ("FAIL  " + $name) }
}

Write-Output '--- AC: Cargo.toml does not mention refact-lsp ---'
foreach ($c in @("$api\Cargo.toml", "$hist\Cargo.toml")) {
  $txt = Get-Content $c -Raw
  Check (-not ($txt -match 'refact-lsp')) "$c has no refact-lsp dependency"
}

Write-Output '--- moved modules exist and are wired ---'
Check (Test-Path "$hist\src\trajectory_writer.rs") 'refact-chat-history/src/trajectory_writer.rs exists'
Check (Test-Path "$api\src\room_roster.rs") 'refact-chat-api/src/room_roster.rs exists'
Check ((Get-Content "$hist\src\lib.rs" -Raw) -match 'pub mod trajectory_writer;') 'chat-history lib.rs declares trajectory_writer'
Check ((Get-Content "$api\src\lib.rs" -Raw) -match 'pub mod room_roster;') 'chat-api lib.rs declares room_roster'

Write-Output '--- engine side is a shim only ---'
$w = Get-Content "$engine\src\chat\trajectories\writer.rs" -Raw
Check ($w -match 'pub use refact_chat_history::trajectory_writer::\*;') 'engine writer.rs glob-shims to the crate'
Check (-not ($w -match '(?m)^\s*(pub |pub\(super\) |pub\(crate\) )?(async )?fn ')) 'engine writer.rs defines no fn'
Check (-not ($w -match '(?m)^\s*(pub )?struct ')) 'engine writer.rs defines no struct'

$rr = Get-Content "$engine\src\chat\room_roster.rs" -Raw
Check ($rr -match 'pub use refact_chat_api::room_roster::\*;') 'engine room_roster.rs glob-shims to the crate'
Check ($rr -match 'pub async fn load_room_roster') 'engine room_roster.rs keeps gcx-bound load_room_roster'
Check (-not ($rr -match 'fn render_room_roster')) 'engine room_roster.rs no longer defines render_room_roster'
Check (-not ($rr -match 'fn upsert_room_roster_message')) 'engine room_roster.rs no longer defines upsert_room_roster_message'
Check (-not ($rr -match 'fn room_roster_message')) 'engine room_roster.rs no longer defines room_roster_message'
Check (-not ($rr -match 'fn roster_for_card')) 'engine room_roster.rs no longer defines roster_for_card'

Write-Output '--- every engine symbol of writer+room_roster is publicly defined in the target crate ---'
$writerSyms = @(
  'atomic_write_file','unique_trajectory_tmp_path','atomic_write_json_with_tmp_path','atomic_write_json',
  'TRAJECTORY_WRITER_ENV','TRAJECTORY_WATCHER_SELF_WRITE_ENV','rollout_switch_enabled',
  'trajectory_writer_rollout_enabled','trajectory_writer_rollout_enabled_for',
  'trajectory_watcher_self_write_rollout_enabled','trajectory_watcher_self_write_rollout_enabled_for',
  'TRAJECTORY_SELF_WRITE_TTL','trajectory_self_writes','register_trajectory_self_write',
  'discard_trajectory_self_write','complete_trajectory_self_write','consume_trajectory_self_write',
  'clear_trajectory_self_writes','TrajectorySelfWriteScope','TrajectorySelfWriteHandle',
  'TrajectorySelfWriteRecord','TrajectorySelfWriteRegistry','TrajectoryWriteFingerprint',
  'TrajectoryWriterRolloutGuard','clear_all_trajectory_self_writes','trajectory_writer_rollout_override'
)
$wtxt = Get-Content "$hist\src\trajectory_writer.rs" -Raw
foreach ($s in $writerSyms) {
  Check ($wtxt -match ("(?m)^\s*pub (async )?(fn|struct|const|static|type) " + [regex]::Escape($s) + '\b')) "chat-history publicly defines $s"
}
$rosterSyms = @('ROOM_ROSTER_MARKER','render_room_roster','roster_for_card','room_roster_message','upsert_room_roster_message')
$rtxt = Get-Content "$api\src\room_roster.rs" -Raw
foreach ($s in $rosterSyms) {
  Check ($rtxt -match ("(?m)^\s*pub (async )?(fn|const) " + [regex]::Escape($s) + '\b')) "chat-api defines $s"
}
foreach ($f in @('file_len','file_modified_unix_ms','content_hash','nonce','created_at','expected_content_hash','fingerprint','key','watched_roots')) {
  Check ($wtxt -match ("(?m)^\s*pub " + [regex]::Escape($f) + ':')) "chat-history keeps public field $f (used by engine tests/siblings)"
}

Write-Output '--- no duplicate definitions remain in engine ---'
foreach ($s in @('unique_trajectory_tmp_path','atomic_write_json_with_tmp_path','consume_trajectory_self_write','register_trajectory_self_write')) {
  $hits = @(Get-ChildItem "$engine\src" -Recurse -Filter *.rs -File | ForEach-Object { Select-String -Path $_.FullName -Pattern ("(?m)^\s*(pub |pub\(super\) |pub\(crate\) )?(async )?fn " + [regex]::Escape($s) + '\b') })
  Check ($hits.Count -eq 0) "engine no longer defines $s"
}

Write-Output '--- moved test bodies preserved verbatim ---'
$engineRosterTests = @(Select-String -Path "$engine\src\chat\room_roster.rs" -Pattern '#\[test\]|#\[tokio::test\]')
Check ($engineRosterTests.Count -eq 0) 'engine room_roster.rs carries no tests anymore'
$crateRosterTests = @(Select-String -Path "$api\src\room_roster.rs" -Pattern '#\[test\]')
Check ($crateRosterTests.Count -eq 11) ('chat-api room_roster.rs carries the 11 moved tests (found ' + $crateRosterTests.Count + ')')

Write-Output '--- moved crate files only use crates they declare ---'
$histCargo = Get-Content "$hist\Cargo.toml" -Raw
foreach ($dep in @('dunce','futures','md5','tokio','serde','uuid')) {
  Check ($histCargo -match ('(?m)^' + $dep + '(\s*=|\s*\.workspace)')) "chat-history declares $dep"
}
Check ($histCargo -match '(?m)^test-util = \[\]') 'chat-history declares a test-util feature'
Check ($wtxt -match 'refact_chat_api::TrajectorySourceIdentity') 'trajectory_writer imports TrajectorySourceIdentity from chat-api'
Check ($wtxt -match 'refact_core::trajectory_settings::current\(\)') 'trajectory_writer reads settings via refact-core'
Check ($wtxt -notmatch 'crate::') 'trajectory_writer has no crate:: (engine) reference at all'
Check ($rtxt -match 'use crate::internal_roles::EVENT_ROLE;') 'room_roster resolves EVENT_ROLE inside chat-api'
$badRoster = @(Select-String -Path "$api\src\room_roster.rs" -Pattern '(?<![\w:])crate::[A-Za-z_][A-Za-z0-9_:]*' -AllMatches |
  ForEach-Object { $_.Matches } | ForEach-Object { $_.Value } | Sort-Object -Unique |
  Where-Object { $_ -notlike 'crate::internal_roles*' })
Check ($badRoster.Count -eq 0) ('room_roster crate file only references chat-api modules (found ' + ($badRoster -join ',') + ')')

Write-Output '--- engine feature plumbing ---'
$engineCargo = Get-Content "$engine\Cargo.toml" -Raw
Check ($engineCargo -match 'bench = \["refact-chat-history/test-util"\]') 'engine bench feature enables chat-history/test-util'
Check ($engineCargo -match "dev-dependencies[\s\S]*refact-chat-history = \{ path = ""crates/refact-chat-history"", features = \[""test-util""\] \}") 'engine dev-deps enable chat-history/test-util'

Write-Output '--- moved bodies byte-identical to the originals (normalised signatures only) ---'
$body = Join-Path $repo 'tools\dev\verify-chat-harvest-body.py'
$python = (Get-Command python -ErrorAction SilentlyContinue)
if ($null -eq $python) {
  Write-Output 'SKIP  python not on PATH; body-identity diff not run'
} else {
  $bodyOut = & python $body 2>&1
  $bodyOut | Write-Output
  if ($LASTEXITCODE -ne 0 -or -not ($bodyOut -match '=== checks=\d+ fail=0 ===')) { $fail++ }
}

Write-Output ''
Write-Output ("=== checks={0} fail={1} ===" -f $checks, $fail)
if ($fail -gt 0) { exit 1 }
