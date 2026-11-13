# O-5 verification harness wrapper.
#
# Usage (single command, no separators, no pipes, no globs):
#   powershell -NoProfile -ExecutionPolicy Bypass -File tools\dev\o5-check.ps1
$ErrorActionPreference = "Stop"

$probe = $PSScriptRoot
$repo = $null
for ($i = 0; $i -lt 8 -and $probe; $i++) {
    if ((Test-Path (Join-Path $probe "refact-agent\engine\src\chat\goal_verifier")) -and
        (Test-Path (Join-Path $probe "tools\dev\engine-build.ps1"))) { $repo = $probe; break }
    $probe = Split-Path $probe -Parent
}
if (-not $repo) { Write-Output "FAIL cannot locate the repository root"; exit 1 }

$py = Join-Path $repo "tools\dev\o5-check.py"
if (-not (Test-Path $py)) { Write-Output "FAIL missing $py"; exit 1 }

$previous = $ErrorActionPreference
$ErrorActionPreference = "Continue"
try { & python $py } finally { $ErrorActionPreference = $previous }
$code = $LASTEXITCODE
Write-Output "EXITCODE=$code"
exit $code
