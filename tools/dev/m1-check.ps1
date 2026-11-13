# M-1 verification harness: the two pre-existing test-target errors are gone.
#
#  E0277 `FollowUpResponse` doesn't implement `Debug`
#        crates/refact-agentic/src/generate_follow_up_message.rs
#        fix: add Debug to the derive list (the test's .unwrap_err() needs T: Debug)
#
#  E0061 `handle_daemon_gui_asset` takes 3 arguments but 2 were supplied
#        crates/refact-daemon/src/daemon/web.rs:1181
#        fix: pass the State((state, port)) extractor its production callers pass
#
# Usage (either location, single command, no separators/pipes/globs):
#   powershell -NoProfile -ExecutionPolicy Bypass -File refact-agent\engine\target\m1-check.ps1
#   powershell -NoProfile -ExecutionPolicy Bypass -File tools\dev\m1-check.ps1
#   ... -SkipCargo    # only the source checks, no cargo run
param([switch]$SkipCargo)
$ErrorActionPreference = "Stop"
$fail = 0
$checks = 0

function Assert-True([bool]$cond, [string]$name) {
    $script:checks++
    if ($cond) { Write-Output "OK   $name" }
    else { Write-Output "FAIL $name"; $script:fail++ }
}

# Locate <repo>/refact-agent/engine and <repo>/tools/dev by walking up from this script, so
# the harness works from refact-agent/engine/target/ and from tools/dev/ without editing.
$probe = $PSScriptRoot
$repo = $null
for ($i = 0; $i -lt 8 -and $probe; $i++) {
    if ((Test-Path (Join-Path $probe "refact-agent\engine\src\files_in_workspace\tests.rs")) -and
        (Test-Path (Join-Path $probe "tools\dev\engine-build.ps1"))) { $repo = $probe; break }
    $probe = Split-Path $probe -Parent
}
if (-not $repo) { Write-Output "FAIL cannot locate the repository root"; exit 1 }
$engine = Join-Path $repo "refact-agent\engine"
Assert-True $true "located engine at $engine"

function Read-Text([string]$relative) {
    return [IO.File]::ReadAllText((Join-Path $engine $relative))
}

# Parse cargo diagnostics: for every `error[...]:`/`error:` header, the `-->` marker within the
# next few lines names the file the error points at. Returns @{ path = count } plus the total.
function Get-ErrorLocations([string]$text) {
    $lines = $text -split "`r?`n"
    $locations = @{}
    $headers = 0
    $i = 0
    while ($i -lt $lines.Count) {
        if ($lines[$i] -match '^error(\[[^\]]*\])?:') {
            $headers++
            $path = "<none>"
            $j = $i + 1
            $stop = [Math]::Min($i + 9, $lines.Count)
            while ($j -lt $stop) {
                $marker = [regex]::Match($lines[$j], '^\s*-->\s*(\S+)')
                if ($marker.Success) { $path = $marker.Groups[1].Value; break }
                if ($lines[$j] -match '^error') { break }
                $j++
            }
            if ($locations.ContainsKey($path)) { $locations[$path]++ } else { $locations[$path] = 1 }
            $i = $j + 1
        }
        else { $i++ }
    }
    return @{ locations = $locations; headers = $headers }
}

# ---------------------------------------------------------------- R1 agentic Debug derive
$agentic = Read-Text "crates\refact-agentic\src\generate_follow_up_message.rs"
Assert-True ($agentic -match '(?m)^#\[derive\(Deserialize, Clone, Debug\)\]\r?$') "FollowUpResponse derives Debug"
Assert-True ($agentic -match '(?m)^pub struct FollowUpResponse \{\r?$') "FollowUpResponse still declared"
Assert-True ($agentic -match '(?m)^\s*pub follow_ups: Vec<String>,\r?$') "follow_ups field unchanged"
Assert-True ($agentic -match '(?m)^\s*pub topic_changed: bool,\r?$') "topic_changed field unchanged"
Assert-True ($agentic -match 'pub fn parse_follow_up_response\(text: &str\) -> Result<FollowUpResponse, String>') "parse_follow_up_response signature unchanged"
Assert-True ($agentic -match 'assert_eq!\(response\.follow_ups, vec!\["a"\.to_string\(\)\]\);') "test expectation unchanged"

# ---------------------------------------------------------------- R2 daemon State argument
$web = Read-Text "crates\refact-daemon\src\daemon\web.rs"
Assert-True ($web -match 'pub\(crate\) async fn handle_daemon_gui_asset\(') "handle_daemon_gui_asset still declared"
Assert-True ($web -match 'State\(\(state, _port\)\): State<\(Arc<DaemonState>, u16\)>,') "callee still takes the daemon State extractor"
Assert-True ($web -match 'let state = test_state\(\)\.await\.state;') "test builds the daemon state it now passes"
Assert-True ($web -match 'State\(\(state, 8488\)\),') "test passes the State extractor its production callers pass"
Assert-True (-not ($web -match 'handle_daemon_gui_asset\(AxumPath\("\.\./secret"\.to_string\(\)\), HeaderMap::new\(\)\)')) "the 2-argument call site is gone"
Assert-True ($web -match 'assert_eq!\(response\.status\(\), StatusCode::BAD_REQUEST\);') "test expectation unchanged"

# ---------------------------------------------------------------- R3 the cargo gate itself
if ($SkipCargo) {
    Write-Output "SKIP cargo check (--SkipCargo)"
}
else {
    $engineBuild = Join-Path $repo "tools\dev\engine-build.ps1"
    Assert-True (Test-Path $engineBuild) "found engine-build.ps1 at $engineBuild"
    # Capture through a file: cargo's multi-line diagnostics survive a text redirect, while a
    # pipeline of PowerShell error records would mangle them. `*> $log` keeps every stream.
    $log = Join-Path $engine "target\m1-check-tests-output.txt"
    Write-Output "RUN  powershell -NoProfile -ExecutionPolicy Bypass -File $engineBuild -Mode check-tests *> $log"

    $previous = $ErrorActionPreference
    $ErrorActionPreference = "Continue"
    try { & powershell -NoProfile -ExecutionPolicy Bypass -File $engineBuild -Mode check-tests *> $log }
    finally { $ErrorActionPreference = $previous }
    $cargoExit = $LASTEXITCODE
    $text = [IO.File]::ReadAllText($log)

    Assert-True ($text -match '=== engine-build: check-tests finished') "check-tests ran to completion"
    $parsed = Get-ErrorLocations $text
    $locations = $parsed.locations
    $agenticHits = 0
    $daemonHits = 0
    $filesWsHits = 0
    $otherHits = 0
    foreach ($key in $locations.Keys) {
        $count = $locations[$key]
        if ($key -match 'refact-agentic') { $agenticHits += $count }
        elseif ($key -match 'refact-daemon') { $daemonHits += $count }
        elseif ($key -match 'files_in_workspace[\\/]tests\.rs') { $filesWsHits += $count }
        elseif ($key -ne "<none>") { $otherHits += $count }
        Write-Output ("INFO error at {0} x{1}" -f $key, $count)
    }
    Write-Output ("INFO cargo exit {0}; error headers {1}; files_in_workspace baseline {2}; else {3}" -f $cargoExit, $parsed.headers, $filesWsHits, $otherHits)

    Assert-True ($agenticHits -eq 0) "no refact-agentic error location (found $agenticHits)"
    Assert-True ($daemonHits -eq 0) "no refact-daemon error location (found $daemonHits)"
    Assert-True (-not ($text -match 'could not compile .refact-agentic.')) "refact-agentic test target compiled"
    Assert-True (-not ($text -match 'could not compile .refact-daemon.')) "refact-daemon test target compiled"

    # The documented pre-existing red baseline must be the only thing left: a clean run in
    # which the two fixed errors are absent but nothing else broke.
    Assert-True ($filesWsHits -eq 9) "documented files_in_workspace baseline (9 errors) unchanged"
    Assert-True ($otherHits -eq 0) "no error outside the documented files_in_workspace baseline (found $otherHits)"
}

Write-Output ""
Write-Output "checks=$checks failures=$fail"
if ($fail -ne 0) { exit 1 }
exit 0
