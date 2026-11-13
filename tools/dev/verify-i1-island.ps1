# Usage: powershell -NoProfile -ExecutionPolicy Bypass -File tools/dev/verify-i1-island.ps1
#
# Inspection-only verification for the I-1 `chat::context_rebuild` island kill
# (agent worktrees may not compile - the project build guard refuses cargo).
#
#   1. structural checks: the crate module exists and is wired, the engine side
#      is a mod.rs of shims plus the gcx-bound `pipeline` submodule
#   2. island checks: the four files the island blocked no longer import
#      `crate::chat::context_rebuild`
#   3. body-identity check: every moved fn/struct/enum/impl is byte-identical to
#      its original after normalising only visibility and whitespace
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

$engine = Join-Path $repo 'refact-agent\engine'
$api = Join-Path $engine 'crates\refact-chat-api'
$crateMod = Join-Path $api 'src\context_rebuild.rs'
$engineDir = Join-Path $engine 'src\chat\context_rebuild'
$modRs = Join-Path $engineDir 'mod.rs'
$pipelineRs = Join-Path $engineDir 'pipeline.rs'

$fail = 0
$checks = 0
function Check([bool]$ok, [string]$name) {
  $script:checks++
  if ($ok) { Write-Output ("PASS  " + $name) }
  else { $script:fail++; Write-Output ("FAIL  " + $name) }
}

Write-Output '--- AC: no crate -> engine dependency ---'
foreach ($c in @("$api\Cargo.toml", "$engine\crates\refact-chat-history\Cargo.toml")) {
  $txt = Get-Content $c -Raw
  Check (-not ($txt -match 'refact-lsp')) "$c has no refact-lsp dependency"
}

Write-Output '--- moved crate module exists and is wired ---'
Check (Test-Path $crateMod) 'refact-chat-api/src/context_rebuild.rs exists'
Check ((Get-Content "$api\src\lib.rs" -Raw) -match 'pub mod context_rebuild;') 'chat-api lib.rs declares context_rebuild'
Check ((Get-Content "$api\Cargo.toml" -Raw) -match 'features = \["net", "rt", "sync", "time"\]') 'chat-api tokio enables the rt feature'

Write-Output '--- engine side is a directory of shims + the gcx pipeline ---'
Check (Test-Path $modRs) 'engine src/chat/context_rebuild/mod.rs exists'
Check (Test-Path $pipelineRs) 'engine src/chat/context_rebuild/pipeline.rs exists'
Check (-not (Test-Path (Join-Path $engine 'src\chat\context_rebuild.rs'))) 'the flat engine src/chat/context_rebuild.rs is gone'

$mod = Get-Content $modRs -Raw
Check ($mod -match 'pub use refact_chat_api::context_rebuild::') 'engine mod.rs re-exports the crate module surface'
Check ($mod -match 'pub use refact_chat_api::compression_gate::') 'engine mod.rs re-exports the compression gate'
Check ($mod -match 'pub use refact_chat_api::pending_context_rebuild::') 'engine mod.rs re-exports the pending rebuild owner'
$modDefs = (Select-String -Path $modRs -Pattern '^pub fn |^pub async fn |^impl |^pub struct ')
Check ($modDefs.Count -eq 0) ('engine mod.rs defines no item of its own (found ' + $modDefs.Count + ')')
$modGcx = (Select-String -Path $modRs -Pattern '(?m)^\s*(?:pub\s+)?use\s+[^;]*GlobalContext')
Check ($modGcx.Count -eq 0) 'engine mod.rs imports no engine type: every gcx body is in the pipeline submodule'

$pipeDefs = @(Select-String -Path $pipelineRs -Pattern '^pub async fn ')
Check ($pipeDefs.Count -eq 6) ('engine pipeline.rs carries the 6 pub async wrappers (found ' + $pipeDefs.Count + ')')
Check ((Get-Content $pipelineRs -Raw) -match 'crate::chat::context_rebuild::request_rebuild') 'engine tests reach request_rebuild through the module shim'
$pipeGcx = @(Select-String -Path $pipelineRs -Pattern '(?<![\w:])gcx\b')
Check ($pipeGcx.Count -gt 0) 'engine pipeline.rs is where the gcx-bound bodies stayed'

Write-Output '--- the crate module defines the whole crate-typed surface ---'
$crateText = Get-Content $crateMod -Raw
foreach ($s in @(
  'pub struct ManualCompressionPreview',
  'pub struct ManualCompressionApplyResult',
  'pub fn pending_tools',
  'pub fn at_auto_cap',
  'pub fn idle',
  'pub fn compaction_safe',
  'pub fn status',
  'pub fn emit_compression_skipped_status',
  'pub struct AttemptGuard',
  'impl Drop for AttemptGuard',
  'pub fn prepare_explicit_idle',
  'pub fn resolve_effective_compression_cap',
  'pub enum CompactionOutcome',
  'impl CompactionOutcome'
)) {
  Check ($crateText -match ('(?m)^\s*' + [regex]::Escape($s))) "chat-api defines $s"
}
Check (-not ($crateText -match 'GlobalContext')) 'chat-api context_rebuild mentions no engine type'
Check (-not ($crateText -match 'crate::chat')) 'chat-api context_rebuild has no engine crate::chat path'

Write-Output '--- island edges removed (the four files the island blocked) ---'
$island = @(
  'src\chat\queue\dispatch.rs',
  'src\chat\queue\tests.rs',
  'src\chat\session\tests_stream.rs',
  'src\chat\goal_monitor.rs',
  'src\tools\tool_compress_chat.rs',
  'src\tools\tool_handoff_to_mode.rs'
)
foreach ($rel in $island) {
  $path = Join-Path $engine $rel
  $hits = @(Select-String -Path $path -Pattern 'crate::chat::context_rebuild::')
  Check ($hits.Count -eq 0) "$rel no longer imports through the island path (found $($hits.Count))"
}

Write-Output '--- shim consumers still resolve crate::chat::context_rebuild:: ---'
foreach ($rel in @('src\chat\generation\run_loop.rs', 'src\chat\generation\recovery.rs', 'src\http\routers\v1\trajectory_ops.rs')) {
  $path = Join-Path $engine $rel
  $hits = @(Select-String -Path $path -Pattern 'crate::chat::context_rebuild::')
  Check ($hits.Count -gt 0) "$rel still targets the engine module path through the shim"
}
foreach ($sym in @('apply_context_rebuild_with_reason', 'apply_deterministic_compaction_for_recovery', 'drain_pending', 'rebuild_session', 'preview_manual_context_rebuild', 'apply_manual_context_rebuild')) {
  $hits = @(Select-String -Path $modRs -Pattern ([regex]::Escape($sym)))
  Check ($hits.Count -gt 0) "engine mod.rs re-exports $sym"
}
Check ($crateText -match 'pub enum CompactionOutcome') 'CompactionOutcome reaches the engine through the crate glob shim'
foreach ($rel in @('src\chat\queue\dispatch.rs', 'src\chat\queue\tests.rs', 'src\chat\goal_monitor.rs', 'src\tools\tool_compress_chat.rs', 'src\tools\tool_handoff_to_mode.rs')) {
  $path = Join-Path $engine $rel
  $hits = @(Select-String -Path $path -Pattern 'refact_chat_api::compression_gate::compression_attempt_active|refact_chat_api::compression_gate::COMPRESSION_ATTEMPT_STALE_AFTER')
  Check ($hits.Count -gt 0) "$rel now reads the gate from the crate"
}

Write-Output '--- every symbol referenced through the shim is exported by mod.rs ---'
$refs = @()
Get-ChildItem "$engine\src" -Recurse -Filter *.rs -File | ForEach-Object {
  foreach ($m in [regex]::Matches((Get-Content $_.FullName -Raw), 'chat::context_rebuild::([A-Za-z_][A-Za-z0-9_]*)')) {
    $refs += $m.Groups[1].Value
  }
}
$refs = $refs | Sort-Object -Unique
Check ($refs.Count -gt 0) 'engine still resolves at least one symbol through the shim'
foreach ($sym in $refs) {
  $hits = @(Select-String -Path $modRs -Pattern ([regex]::Escape($sym)))
  Check ($hits.Count -gt 0) "engine mod.rs exports $sym (referenced as crate::chat::context_rebuild::$sym)"
}

Write-Output '--- every moved test is still present ---'
$testPattern = '#\[(tokio::)?test\]'
$origTests = 24
$crateTests = @(Select-String -Path $crateMod -Pattern $testPattern).Count
$pipeTests = @(Select-String -Path $pipelineRs -Pattern $testPattern).Count
Check (($crateTests + $pipeTests) -eq $origTests) ("24 moved tests survive (crate=$crateTests pipeline=$pipeTests)")
Check ($crateTests -eq 5) ("chat-api carries the 5 crate-local tests (found $crateTests)")

Write-Output '--- moved bodies byte-identical to the originals (normalised signatures only) ---'
$body = Join-Path $repo 'tools\dev\verify-i1-island-body.py'
$python = (Get-Command python -ErrorAction SilentlyContinue)
if ($null -eq $python) {
  Write-Output 'SKIP  python not on PATH; body-identity diff not run'
} else {
  # The original lives in the parent of the commit that removed the flat file, so
  # the check keeps working after this branch is merged and HEAD has moved on.
  $deletedBy = (& git -C $repo log --diff-filter=D --format=%H -1 -- 'refact-agent/engine/src/chat/context_rebuild.rs') | Select-Object -First 1
  if ([string]::IsNullOrWhiteSpace($deletedBy)) {
    Write-Output 'SKIP  no commit deletes the flat engine file; body-identity diff not run'
  } else {
    $origRef = $deletedBy.Trim() + '^:refact-agent/engine/src/chat/context_rebuild.rs'
    $bodyOut = & python $body $origRef $crateMod $pipelineRs 2>&1
    $bodyOut | Write-Output
    if ($LASTEXITCODE -ne 0 -or -not ($bodyOut -match '=== checks=\d+ fail=0 ===')) { $fail++ }
  }
}

Write-Output ''
Write-Output ("=== checks={0} fail={1} ===" -f $checks, $fail)
if ($fail -gt 0) { exit 1 }
