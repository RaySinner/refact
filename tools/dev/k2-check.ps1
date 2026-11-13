# L-5 (K2) verification harness runner.
# Single in-repo script invocation, no shell separators.
#   powershell -NoProfile -ExecutionPolicy Bypass -File tools\dev\k2-check.ps1
$ErrorActionPreference = "Stop"
$root = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
python (Join-Path $root "tools\dev\k2-check.py")
exit $LASTEXITCODE
