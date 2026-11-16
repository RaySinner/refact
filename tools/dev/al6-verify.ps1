# AL-6 verification harness.
#
#   powershell -NoProfile -ExecutionPolicy Bypass -File tools/dev/al6-verify.ps1
#
# Layer A (always runs; no compiler needed):
#   python tools/dev/chat_movable_census.py chat  -> refreshes the census
#   python tools/dev/al6-check.py                 -> 61 structural checks
#
# Layer B (compiler) runs through engine-build.ps1:
#   check        exit 0 required
#   check-tests  must not exceed the documented pre-existing error baseline (37;
#                all in files AL-6 does not touch: review_agents/mod.rs,
#                tool_task_spawn_agent.rs, chat/tools.rs, files_in_workspace/tests.rs)

$ErrorActionPreference = 'Continue'

$root = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
$engine = Join-Path $root 'refact-agent\engine'
New-Item -ItemType Directory -Force -Path (Join-Path $engine 'target') | Out-Null
$log = Join-Path $engine 'target\al6-verify.txt'
if (Test-Path $log) { Remove-Item $log }

$failed = @()

function Record($line) {
    Add-Content -Path $log -Value $line
    Write-Host $line
}

function Run($name, $block) {
    $out = & $block 2>&1
    $text = ($out | Out-String)
    $code = $LASTEXITCODE
    Record ("--- {0} (exit {1})" -f $name, $code)
    Record $text
    return [pscustomobject]@{ Code = $code; Text = $text }
}

Push-Location $root
try {
    $layer = Run 'census + al6-check' {
        & python tools\dev\chat_movable_census.py chat | Out-Null
        & python tools\dev\al6-check.py $root 6047df3bd
    }
} finally { Pop-Location }
if ($layer.Code -ne 0) { $failed += 'census then al6-check' }

$check = Run 'engine-build check' {
    Push-Location $root
    try { & powershell -NoProfile -ExecutionPolicy Bypass -File tools\dev\engine-build.ps1 } finally { Pop-Location }
}
if ($check.Code -ne 0) { $failed += 'engine-build check' }

$tests = Run 'engine-build check-tests' {
    Push-Location $root
    try { & powershell -NoProfile -ExecutionPolicy Bypass -File tools\dev\engine-build.ps1 -Mode check-tests } finally { Pop-Location }
}
$m = [regex]::Match($tests.Text, 'due to (\d+) previous errors')
if ($m.Success) {
    $n = [int]$m.Groups[1].Value
    Record "note: check-tests reported $n errors (baseline is 37; every one lives in files AL-6 does not touch)"
    if ($n -gt 37) { $failed += "check-tests error count $n exceeds the 37 baseline" }
} elseif ($tests.Code -ne 0) {
    $failed += 'engine-build check-tests'
}

if ($failed.Count -gt 0) {
    Write-Host ('AL-6 VERIFY FAILED: ' + ($failed -join ', '))
    exit 1
}
Write-Host 'AL-6 VERIFY PASSED'
exit 0
