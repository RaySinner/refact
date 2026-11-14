# Q-3 verification harness for the agents::spawn seam.
#
# Usage (single command, no pipes, no globs):
#   powershell -NoProfile -ExecutionPolicy Bypass -File tools\dev\q3-check.ps1
#
# Asserts the card's acceptance criteria that are checkable without a full build:
#   AC1  no `agents::spawn` reference remains under engine `src/tools`
#   AC2  the crate sides hold no engine edge (refact-agents-core / refact-runtime-api
#        must not list the engine package `refact-lsp`)
# then runs the engine build guard. `check` must be exit 0. `check-tests` is
# allowed to be the DOCUMENTED pre-existing red baseline (36 errors at base
# ac177abe7: 2 `json!` in tool_task_spawn_agent.rs, 25 StageJob/StageProduct/now_ms
# in review_agents/mod.rs, 9 IndexingEverywhere/is_valid_file in
# files_in_workspace/tests.rs) -- in exactly those 3 files, nothing else.
# `check-all` is not run: the `--all-targets` pass reproduces the same baseline
# errors and costs another full cycle for no extra evidence.
$ErrorActionPreference = "Stop"

[Console]::OutputEncoding = [System.Text.Encoding]::UTF8
$OutputEncoding = [System.Text.Encoding]::UTF8

$repoRoot = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
Set-Location $repoRoot

Write-Output "=== AC1: no agents::spawn reference under engine src/tools ==="
$previous = $ErrorActionPreference
$ErrorActionPreference = "Continue"
$matches = & git grep -n "agents::spawn" -- "refact-agent/engine/src/tools" 2>$null
$grepExit = $LASTEXITCODE
$ErrorActionPreference = $previous
if ($grepExit -eq 0) {
    Write-Output "FAIL: agents::spawn still referenced:"
    $matches | ForEach-Object { Write-Output $_ }
    exit 1
}
Write-Output "OK: grep found nothing (git grep exit $grepExit)"

Write-Output ""
Write-Output "=== AC2: crate edges checked (must contain no engine package reference) ==="
$edges = @(
    "refact-agent/engine/crates/refact-agents-core/Cargo.toml",
    "refact-agent/engine/crates/refact-runtime-api/Cargo.toml"
)
foreach ($edge in $edges) {
    Write-Output "checked: $edge"
    $text = Get-Content -Raw $edge
    if ($text -match "refact-lsp") {
        Write-Output "FAIL: $edge references the engine package refact-lsp"
        exit 1
    }
}
Write-Output "OK: no crate -> engine edge in either manifest"

Write-Output ""
Write-Output "=== build guard: engine-build.ps1 -Mode check (must be exit 0) ==="
& "$PSScriptRoot\engine-build.ps1" -Mode check
if ($LASTEXITCODE -ne 0) {
    Write-Output "FAIL: check mode exited $LASTEXITCODE"
    exit $LASTEXITCODE
}

Write-Output ""
Write-Output "=== build guard: engine-build.ps1 -Mode check-tests (documented baseline allowed) ==="
$log = Join-Path $env:TEMP "q3-check-tests.log"
& "$PSScriptRoot\engine-build.ps1" -Mode check-tests *>&1 | Tee-Object -FilePath $log
$testsExit = $LASTEXITCODE
if ($testsExit -eq 0) {
    Write-Output "OK: check-tests exit 0"
    Write-Output ""
    Write-Output "Q3 CHECK PASSED"
    exit 0
}

# Only `error...` blocks count. A `-->` line belongs to the error block that
# precedes it, so walk the log and remember the last header seen.
$lines = Get-Content $log
$errorCount = 0
$sources = New-Object System.Collections.Generic.List[string]
$inError = $false
foreach ($line in $lines) {
    if ($line -match "^error(\[|:)") {
        if ($line -notmatch "^error: could not compile") {
            $errorCount++
        }
        $inError = $true
        continue
    }
    if ($inError -and $line -match "^\s+-->\s+(.+)$") {
        $path = $Matches[1] -replace ":\d+.*$", ""
        if (-not $sources.Contains($path)) { $sources.Add($path) }
        $inError = $false
    }
}

Write-Output "check-tests exit $testsExit with $errorCount error lines"
Write-Output "error sources:"
$sources | ForEach-Object { Write-Output "  $_" }

$allowed = @(
    "src\tools\tool_task_spawn_agent.rs",
    "src\tools\review_agents\mod.rs",
    "src\files_in_workspace\tests.rs"
)
$unexpected = @($sources | Where-Object { $allowed -notcontains $_ })
if ($unexpected.Count -gt 0) {
    Write-Output "FAIL: errors outside the documented baseline:"
    $unexpected | ForEach-Object { Write-Output "  $_" }
    exit 1
}
if ($errorCount -ne 36) {
    Write-Output "FAIL: expected the documented 36-error baseline, saw $errorCount"
    exit 1
}
Write-Output "OK: exactly the documented 36-error baseline, in the 3 known files"

Write-Output ""
Write-Output "Q3 CHECK PASSED"
