# M-1 baseline capture: prove the two errors this card fixes really were errors.
#
# Temporarily restores the pre-fix source of both sites, type-checks ONLY the two crates'
# test targets, asserts the two diagnostics appear, then restores the fixed source no matter
# what happens (try/finally).
#
# `-p refact-agentic -p refact-daemon` narrows feature unification, which is why this run also
# surfaces `tokio::time::advance`/`start_paused` errors in refact-daemon's test target: those
# need tokio's `test-util` feature, which the full workspace build unifies in from another
# dev-dependency. They are an artifact of the narrowed selection, not of this card; the
# authoritative post-fix gate is the whole-workspace harness `m1-check.ps1`.
#
# Usage (either location, single command, no separators/pipes/globs):
#   powershell -NoProfile -ExecutionPolicy Bypass -File refact-agent\engine\target\m1-baseline.ps1
#   powershell -NoProfile -ExecutionPolicy Bypass -File tools\dev\m1-baseline.ps1
#   ... -Reuse   # re-analyse the existing log, no cargo run and no source rewrite
param([switch]$Reuse)
$ErrorActionPreference = "Stop"
$fail = 0
$checks = 0

function Assert-True([bool]$cond, [string]$name) {
    $script:checks++
    if ($cond) { Write-Output "OK   $name" }
    else { Write-Output "FAIL $name"; $script:fail++ }
}

$probe = $PSScriptRoot
$repo = $null
for ($i = 0; $i -lt 8 -and $probe; $i++) {
    if ((Test-Path (Join-Path $probe "refact-agent\engine\src\files_in_workspace\tests.rs")) -and
        (Test-Path (Join-Path $probe "tools\dev\build-env.ps1"))) { $repo = $probe; break }
    $probe = Split-Path $probe -Parent
}
if (-not $repo) { Write-Output "FAIL cannot locate the repository root"; exit 1 }
$engine = Join-Path $repo "refact-agent\engine"
Assert-True $true "located engine at $engine"

$agenticPath = Join-Path $engine "crates\refact-agentic\src\generate_follow_up_message.rs"
$webPath = Join-Path $engine "crates\refact-daemon\src\daemon\web.rs"
$logPath = Join-Path $engine "target\m1-baseline-output.txt"

# The two Rust files use LF; build the pre-fix text from the file's own newline style so the
# replacement cannot silently no-op through a CRLF mismatch.
function Restore-PreFix([string]$text, [string]$newline) {
    $agenticPre = $text.Replace(
        "#[derive(Deserialize, Clone, Debug)]",
        "#[derive(Deserialize, Clone)]")
    return $agenticPre
}

function Restore-PreFixWeb([string]$text, [string]$nl) {
    $old = @(
        "        let state = test_state().await.state;",
        "        let response = handle_daemon_gui_asset(",
        "            AxumPath(`"../secret`".to_string()),",
        "            State((state, 8488)),",
        "            HeaderMap::new(),",
        "        )"
    ) -join $nl
    $new = "        let response = handle_daemon_gui_asset(AxumPath(`"../secret`".to_string()), HeaderMap::new())"
    return $text.Replace($old, $new)
}

$fixedAgentic = [IO.File]::ReadAllText($agenticPath)
$fixedWeb = [IO.File]::ReadAllText($webPath)
$nl = if ($fixedWeb.Contains("`r`n")) { "`r`n" } else { "`n" }

$preAgentic = Restore-PreFix $fixedAgentic $nl
$preWeb = Restore-PreFixWeb $fixedWeb $nl

Assert-True ($preAgentic -ne $fixedAgentic) "pre-fix agentic source differs from the fixed source"
Assert-True ($preWeb -ne $fixedWeb) "pre-fix daemon source differs from the fixed source"

# cargo must run through the same wrapper policy the sanctioned engine-build.ps1 uses: the user
# config points rustc at the project build guard, which refuses builds inside a worktree.
# engine-build.ps1 passes --config with rustc-wrapper=sccache, so do exactly the same here.
$sccacheConfig = Join-Path $env:TEMP "refact-engine-build-sccache.toml"
[IO.File]::WriteAllText($sccacheConfig, "[build]`r`nrustc-wrapper = `"sccache`"`r`n")

function Invoke-CargoCheck([string]$log) {
    . (Join-Path $repo "tools\dev\build-env.ps1") | Out-Null
    $env:CARGO_INCREMENTAL = '0'
    Push-Location $engine
    $previous = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    try { & cargo --config $sccacheConfig check -p refact-agentic -p refact-daemon --profile test --all-targets *> $log }
    finally { $ErrorActionPreference = $previous; Pop-Location }
    return $LASTEXITCODE
}

if ($Reuse) {
    Assert-True (Test-Path $logPath) "reusing the existing baseline log at $logPath"
    $cargoExit = 101
    $text = [IO.File]::ReadAllText($logPath)
}
else {
    try {
        [IO.File]::WriteAllText($agenticPath, $preAgentic)
        [IO.File]::WriteAllText($webPath, $preWeb)
        Write-Output "RUN  cargo --config $sccacheConfig check -p refact-agentic -p refact-daemon --profile test --all-targets"

        $cargoExit = Invoke-CargoCheck $logPath
        $text = [IO.File]::ReadAllText($logPath)
    }
    finally {
        [IO.File]::WriteAllText($agenticPath, $fixedAgentic)
        [IO.File]::WriteAllText($webPath, $fixedWeb)
    }

    Assert-True (([IO.File]::ReadAllText($agenticPath)) -eq $fixedAgentic) "agentic source restored to the fixed version"
    Assert-True (([IO.File]::ReadAllText($webPath)) -eq $fixedWeb) "daemon source restored to the fixed version"
    Assert-True ($cargoExit -ne 0) "the pre-fix tree does not compile"
}

# Pair every `error[...]:`/`error:` header with the `-->` site it points at, exactly the way
# rustc is read by hand, and print the two sites this card is about.
function Get-ErrorRecords([string]$body) {
    $lines = $body -split "`r?`n"
    $records = @()
    $i = 0
    while ($i -lt $lines.Count) {
        if ($lines[$i] -match '^error(\[[^\]]*\])?:\s*(.*)$') {
            $code = $Matches[1]
            $message = $Matches[2]
            $site = "<none>"
            $j = $i + 1
            $stop = [Math]::Min($i + 12, $lines.Count)
            while ($j -lt $stop) {
                $marker = [regex]::Match($lines[$j], '^\s*-->\s*(\S+)')
                if ($marker.Success) { $site = $marker.Groups[1].Value; break }
                if ($lines[$j] -match '^error') { break }
                $j++
            }
            $records += [pscustomobject]@{ code = $code; message = $message; site = $site }
            $i = $j + 1
        }
        else { $i++ }
    }
    return $records
}

Write-Output "INFO cargo exit $cargoExit; log $logPath"
$records = Get-ErrorRecords $text
foreach ($record in $records) {
    Write-Output ("BEFORE {0}: {1}  --> {2}" -f ($record.code -replace '[\[\]]', ''), $record.message, $record.site)
}

$debugRecord = $records | Where-Object { $_.code -eq '[E0277]' -and $_.message -match 'FollowUpResponse' } | Select-Object -First 1
Assert-True ($null -ne $debugRecord) "E0277 FollowUpResponse does not implement Debug (before)"
if ($debugRecord) {
    Assert-True ($debugRecord.site -match 'refact-agentic\\src\\generate_follow_up_message\.rs') "the E0277 site is generate_follow_up_message.rs (got $($debugRecord.site))"
}

$arityRecord = $records | Where-Object { $_.code -eq '[E0061]' -and $_.message -match 'this function takes 3 arguments but 2 arguments were supplied' -and $_.site -match 'refact-daemon\\src\\daemon\\web\.rs' } | Select-Object -First 1
Assert-True ($null -ne $arityRecord) "E0061 handle_daemon_gui_asset arity error at daemon/web.rs (before)"
if ($arityRecord) {
    Assert-True ($text -match '(?m)^\s*1181 \|.*handle_daemon_gui_asset\(AxumPath\("\.\./secret"\.to_string\(\)\), HeaderMap::new\(\)\)') "the E0061 snippet names the 2-argument call site in the test"
    Assert-True ($text -match 'note: function defined here') "the E0061 diagnostic names the callee definition"
}

Write-Output ""
Write-Output "checks=$checks failures=$fail"
if ($fail -ne 0) { exit 1 }
exit 0
