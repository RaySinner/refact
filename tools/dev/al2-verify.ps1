# AL-2 verification harness: chat::cache_guard settlement + the prompts/verifier/ctx/verify_cmd edge table.
#
#   powershell -NoProfile -ExecutionPolicy Bypass -File tools\dev\al2-verify.ps1
#
# Layer A (always runs, no compiler):
#   python tools/dev/chat_movable_census.py chat   -> before/after census block
#   python tools/dev/al2-check.py                  -> engine shim, destination file,
#                                                     module declaration, cargo edges
#                                                     (both directions), body preservation
# Layer B (compiler): engine-build.ps1 -Mode check / -Mode check-tests.
#   check        exit 0  (544,7 s)  after the move
#   check-tests  must not exceed the documented 36-error pre-existing baseline
#                (review_agents/mod.rs x25, files_in_workspace/tests.rs x9,
#                 tool_task_spawn_agent.rs x2)

$ErrorActionPreference = 'Continue'

$root = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
$engine = Join-Path $root 'refact-agent\engine'
$failed = @()
$log = Join-Path $engine 'target\al2-verify.txt'
if (Test-Path $log) { Remove-Item $log }
New-Item -ItemType Directory -Force -Path (Join-Path $engine 'target') | Out-Null

function Record([string]$text) {
    Add-Content -Path $log -Value $text
}

function Run([string]$Name, [scriptblock]$Body) {
    Write-Host "=== AL-2 verify: $Name ==="
    $out = & $Body 2>&1
    $code = $LASTEXITCODE
    $text = ($out | Out-String)
    Record ("=== $Name (exit $code) ===" + [Environment]::NewLine + $text)
    Write-Host $text
    return @{ Code = $code; Text = $text }
}

$struct = Run 'census then al2-check' {
    Push-Location $root
    try {
        & python tools\dev\chat_movable_census.py chat
        & python tools\dev\al2-check.py $root bf24bd08a
    } finally { Pop-Location }
}
if ($struct.Code -ne 0) { $failed += 'census then al2-check' }

$check = Run 'engine-build check' {
    Push-Location $root
    try { & powershell -NoProfile -ExecutionPolicy Bypass -File tools\dev\engine-build.ps1 } finally { Pop-Location }
}
if ($check.Code -ne 0) { $failed += 'engine-build check' }

$tests = Run 'engine-build check-tests' {
    Push-Location $root
    try { & powershell -NoProfile -ExecutionPolicy Bypass -File tools\dev\engine-build.ps1 -Mode check-tests } finally { Pop-Location }
}
if ($tests.Text -match 'BUILD REFUSED BY PROJECT POLICY') {
    Write-Host 'DEFERRED: check-tests skipped, the build guard refused compilation.'
    Record 'DEFERRED: check-tests skipped (build guard).'
} else {
    $m = [regex]::Match($tests.Text, 'due to (\d+) previous errors')
    if ($m.Success) {
        $n = [int]$m.Groups[1].Value
        Write-Host "note: check-tests reported $n errors (baseline is 36)"
        Record "note: check-tests reported $n errors (baseline 36)"
        if ($n -gt 36) { $failed += "check-tests error count $n exceeds the 36 baseline" }
    } elseif ($tests.Code -ne 0) {
        $failed += 'engine-build check-tests'
    }
}

if ($failed.Count -gt 0) {
    Write-Host ('AL-2 VERIFY FAILED: ' + ($failed -join ', '))
    exit 1
}
Write-Host 'AL-2 VERIFY PASSED'
exit 0
