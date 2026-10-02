# Build the whole product into a single VS Code .vsix.
#
#   .\tools\dev\build-all.ps1              # full build: GUI + engine in parallel, then .vsix
#   .\tools\dev\build-all.ps1 -Check       # cargo check instead of a release build (stops after the build stage)
#   .\tools\dev\build-all.ps1 -SkipEngine  # reuse an existing refact.exe
#   .\tools\dev\build-all.ps1 -SkipGui     # reuse an existing refact-chat-js-<version>.tgz
#
# Nothing needs to be passed for the normal case. A release engine build takes
# 40-50 minutes cold, so -Check is the cheap way to see whether a change compiles.
#
# ASCII only on purpose: Windows PowerShell 5.1 reads .ps1 files as ANSI when
# there is no BOM, so non-ASCII characters here would be mangled.
[CmdletBinding()]
param(
    [switch]$SkipEngine,
    [switch]$SkipGui,
    [switch]$Check
)

$ErrorActionPreference = 'Stop'

$repoRoot = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
$guiDir = Join-Path $repoRoot 'refact-agent\gui'
$engineDir = Join-Path $repoRoot 'refact-agent\engine'
$vscodeDir = Join-Path $repoRoot 'plugins\vscode'
$buildEnvScript = Join-Path $PSScriptRoot 'build-env.ps1'
$sccacheBypass = Join-Path $env:TEMP 'refact-build-all-nosccache.toml'

# Written by the GUI job only after `npm pack` has SUCCEEDED. The engine job
# gates on this, see the comment there.
$guiSentinel = Join-Path $guiDir '.build-all-gui.ready'

foreach ($required in @($guiDir, $engineDir, $vscodeDir, $buildEnvScript)) {
    if (-not (Test-Path $required)) {
        Write-Error "required path not found: $required"
        exit 2
    }
}

# PATH (cargo, LLVM, Node, MSVC), CARGO_INCREMENTAL=0 for sccache, NODE_OPTIONS.
. $buildEnvScript

# Bypass the global sccache rustc-wrapper.
#
# The user-wide ~/.cargo/config.toml sets `rustc-wrapper = "sccache"`. sccache
# re-spawns rustc with the full argv, and `refact-lsp` alone has ~150 --extern
# paths under a long worktree directory, so the command line exceeds the Windows
# 32 767-character limit. sccache then dies with "os error 206" and rustc never
# runs at all. `cargo --config 'build.rustc-wrapper=""'` cannot be used from
# PowerShell: the shell strips the inner quotes before cargo sees them and cargo
# rejects the bare key as invalid TOML. A config FILE avoids the shell entirely.
[IO.File]::WriteAllText($sccacheBypass, "[build]`r`nrustc-wrapper = `"`"`r`n")

$stopwatch = [System.Diagnostics.Stopwatch]::StartNew()

function Write-Banner {
    param([string]$Text)
    Write-Host ''
    Write-Host "=== build-all: $Text ===" -ForegroundColor Cyan
}

# The tarball name is derived from the GUI version, never hardcoded, so a version
# bump cannot make this script look for a file that no longer exists.
$guiVersion = (Get-Content (Join-Path $guiDir 'package.json') -Raw | ConvertFrom-Json).version
$tgzName = "refact-chat-js-$guiVersion.tgz"
$tgzPath = Join-Path $guiDir $tgzName
$engineExe = Join-Path $engineDir 'target\release\refact.exe'
$assetExe = Join-Path $vscodeDir 'assets\refact.exe'

Write-Banner "version $guiVersion"

# A stale sentinel from a previous run would let the engine job start immediately.
Remove-Item $guiSentinel -Force -ErrorAction SilentlyContinue

$guiJob = $null
$engineJob = $null

if ($SkipGui) {
    Write-Banner "GUI job SKIPPED (-SkipGui)"
}
else {
    $guiJob = Start-Job -Name 'gui' -ScriptBlock {
        param($guiDir, $buildEnvScript, $sentinel)
        . $buildEnvScript
        # npm writes deprecation and progress notices to stderr as a matter of
        # course. Under 'Stop' each one is a terminating error, so a perfectly
        # healthy `npm ci` would abort the job. Success is judged on LastExitCode.
        $ErrorActionPreference = 'Continue'
        # Start-Job inherits the CALLER's working directory, which is the repo
        # root, not the GUI. Without this, npm ci resolves the root package.json
        # and fails with EUSAGE.
        Set-Location $guiDir

        # `npm ci` deletes node_modules before reinstalling, so on a worktree that
        # already has it (or on a shared junction) that is pure waste and, for the
        # latter, destructive. Only install when the directory is absent.
        if (-not (Test-Path (Join-Path $guiDir 'node_modules'))) {
            & npm.cmd ci
            if ($LASTEXITCODE -ne 0) { throw "npm ci failed (exit $LASTEXITCODE)" }
        }
        else {
            Write-Host '[gui] node_modules already present, skipping npm ci'
        }

        # build:fast produces the same bundles as `npm run build` minus the eslint
        # pass, and keeps the .d.ts rollup that plugins/vscode needs to compile.
        & npm.cmd run build:fast
        if ($LASTEXITCODE -ne 0) { throw "npm run build:fast failed (exit $LASTEXITCODE)" }

        & npm.cmd pack
        if ($LASTEXITCODE -ne 0) { throw "npm pack failed (exit $LASTEXITCODE)" }

        [IO.File]::WriteAllText($sentinel, 'ok')
    } -ArgumentList $guiDir, $buildEnvScript, $guiSentinel
}

if ($SkipEngine) {
    Write-Banner "engine job SKIPPED (-SkipEngine)"
}
else {
    $engineJob = Start-Job -Name 'engine' -ScriptBlock {
        param($engineDir, $buildEnvScript, $sentinel, $sccacheBypass, $CheckMode, $WaitForGui)
        . $buildEnvScript
        $ErrorActionPreference = 'Continue'
        Set-Location $engineDir

        # REFACT_USE_PREBUILT_GUI=1 makes build.rs skip the npm build, but it does
        # NOT skip the `gui/dist/chat` -> `engine/assets/chat/dist/chat` copy, and
        # build.rs runs before any Rust is compiled. Copying a half-written dist
        # would silently bake a stale or partial UI into the binary, so the engine
        # job waits for the GUI job's success sentinel before invoking cargo. The
        # wait is bounded and reports a real error instead of hanging forever.
        if ($WaitForGui) {
            $deadline = (Get-Date).AddMinutes(90)
            while (-not (Test-Path $sentinel)) {
                if ((Get-Date) -gt $deadline) {
                    throw "timed out after 90 minutes waiting for the GUI job to produce $sentinel"
                }
                Write-Host '[engine] waiting for the GUI build to finish ...'
                Start-Sleep -Seconds 10
            }
        }

        # Never REFACT_SKIP_GUI_BUILD=1: that skips the asset copy too, which would
        # ship a binary with no UI at all.
        $env:REFACT_USE_PREBUILT_GUI = '1'
        Remove-Item Env:\REFACT_SKIP_GUI_BUILD -ErrorAction SilentlyContinue

        $cargoArgs = @('--config', $sccacheBypass)
        if ($CheckMode) { $cargoArgs += @('check', '--workspace', '--all-targets') }
        else { $cargoArgs += @('build', '--release', '--bin', 'refact') }

        $proc = Start-Process -FilePath 'cargo' -ArgumentList $cargoArgs `
            -WorkingDirectory $engineDir -NoNewWindow -Wait -PassThru
        if ($proc.ExitCode -ne 0) { throw "cargo exited with $($proc.ExitCode)" }
    } -ArgumentList $engineDir, $buildEnvScript, $guiSentinel, $sccacheBypass, [bool]$Check, (-not $SkipGui)
}

$jobs = @()
if ($guiJob) { $jobs += $guiJob }
if ($engineJob) { $jobs += $engineJob }

if ($jobs.Count -gt 0) {
    Write-Banner 'building (GUI and engine run concurrently)'
    Wait-Job -Job $jobs | Out-Null
}

# Print a finished job's output and report whether it succeeded.
#
# Receive-Job 2>&1 merges the job's stderr into ErrorRecords, and under the
# script-wide $ErrorActionPreference = 'Stop' that merge is ITSELF a terminating
# error. Without the temporary override, a build that merely printed a warning
# aborts the script at the logging line instead of reaching the failure report.
function Write-JobReport {
    param($Job, [string]$Label)
    $state = $Job.State
    $previous = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    try {
        $output = Receive-Job -Job $Job -Keep 2>&1 | Out-String
    }
    finally {
        $ErrorActionPreference = $previous
    }
    if ($output) { Write-Host $output }
    return [pscustomobject]@{ Label = $Label; State = $state; Ok = ($state -eq 'Completed') }
}

# Collect and print both logs before deciding anything, so a failure report always
# carries the output of the job that did not cause it too. The job state has to be
# read BEFORE Remove-Job, which discards it.
$reports = @()
if ($guiJob) { $reports += Write-JobReport -Job $guiJob -Label 'GUI' }
if ($engineJob) { $reports += Write-JobReport -Job $engineJob -Label 'engine' }
if ($guiJob) { Remove-Job -Job $guiJob -Force }
if ($engineJob) { Remove-Job -Job $engineJob -Force }

# Fail before anything is verified, copied or packaged.
$failed = @($reports | Where-Object { -not $_.Ok })
if ($failed.Count -gt 0) {
    Write-Host ''
    foreach ($report in $failed) {
        Write-Host "$($report.Label) job FAILED (state $($report.State))" -ForegroundColor Red
    }
    Write-Host 'build stage failed - nothing was packaged' -ForegroundColor Red
    exit 1
}

# Verify every build output before it is used. A missing file here would otherwise
# become a silently broken artifact.
Write-Banner 'verifying build outputs'
$missing = @()
if (-not $SkipGui) {
    if (-not (Test-Path (Join-Path $guiDir 'dist\chat'))) { $missing += "$guiDir\dist\chat" }
    if (-not (Test-Path $tgzPath)) { $missing += $tgzPath }
}
if (-not $SkipEngine -and -not $Check) {
    if (-not (Test-Path $engineExe)) { $missing += $engineExe }
}
if ($missing.Count -gt 0) {
    Write-Error ("expected build output missing: " + ($missing -join ', '))
    exit 1
}
Write-Host "gui dist        : $(Join-Path $guiDir 'dist\chat')"
Write-Host "gui tarball     : $tgzPath"
if (-not $SkipEngine -and -not $Check) { Write-Host "engine binary   : $engineExe" }

if ($Check) {
    $stopwatch.Stop()
    Write-Host ''
    Write-Host ("=== build-all: -Check finished in {0:N1} min - cargo check passed, packaging skipped ===" -f $stopwatch.Elapsed.TotalMinutes) -ForegroundColor Cyan
    exit 0
}

if (-not (Test-Path $tgzPath)) {
    Write-Error "GUI tarball not found: $tgzPath (drop -SkipGui, or run the GUI build first)"
    exit 1
}

# BUILD.md 3: make sure no running refact.exe / VS Code still holds the target.
$assetsDir = Join-Path $vscodeDir 'assets'
if (-not (Test-Path $assetsDir)) { New-Item -ItemType Directory -Path $assetsDir | Out-Null }

if ($SkipEngine) {
    if (-not (Test-Path $engineExe)) {
        Write-Error "-SkipEngine was given but there is no engine binary at $engineExe"
        exit 1
    }
    Write-Banner 'copying the existing engine binary into the extension assets'
}
else {
    Write-Banner 'copying refact.exe into plugins/vscode/assets'
}
Copy-Item $engineExe $assetExe -Force

# `npm install <tgz> --save-exact` rewrites the refact-chat-js dependency to a
# concrete tarball path in BOTH package.json and package-lock.json. That path is
# only valid for a local build; leaving it behind breaks CI. Snapshot both files
# up front and put them back in `finally`, so a packaging failure cannot leak it.
Write-Banner 'packaging the VS Code extension'
$snapshotDir = Join-Path $env:TEMP ("refact-build-all-snapshot-" + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $snapshotDir | Out-Null
$manifestPath = Join-Path $vscodeDir 'package.json'
$lockPath = Join-Path $vscodeDir 'package-lock.json'
Copy-Item $manifestPath $snapshotDir
Copy-Item $lockPath $snapshotDir

# npm and vsce write progress and warnings to stderr as a matter of course, so
# $ErrorActionPreference is relaxed for the whole stage and success is judged on
# $LASTEXITCODE instead - which is what actually reflects the tool's exit status.
$previousPreference = $ErrorActionPreference
$ErrorActionPreference = 'Continue'
try {
    Push-Location $vscodeDir
    try {
        # `npm install <tgz>` below rewrites node_modules/refact-chat-js anyway, so a
        # clean install is only needed when there is nothing to start from. It is
        # also destructive - npm deletes node_modules before reinstalling - so it
        # must never run against a shared or linked install.
        if (-not (Test-Path (Join-Path $vscodeDir 'node_modules'))) {
            & npm.cmd ci
            if ($LASTEXITCODE -ne 0) { throw "npm ci failed in plugins/vscode (exit $LASTEXITCODE)" }
        }
        else {
            Write-Host '[vsix] node_modules already present, skipping npm ci'
        }

        $relativeTgz = "..\..\refact-agent\gui\$tgzName"
        & npm.cmd install $relativeTgz --save-exact
        if ($LASTEXITCODE -ne 0) { throw "npm install <tgz> failed (exit $LASTEXITCODE)" }

        & npm.cmd run compile
        if ($LASTEXITCODE -ne 0) { throw "npm run compile failed (exit $LASTEXITCODE)" }

        & npx.cmd --yes @vscode/vsce package --target win32-x64
        if ($LASTEXITCODE -ne 0) { throw "vsce package failed (exit $LASTEXITCODE)" }
    }
    finally {
        Pop-Location
    }
}
finally {
    $ErrorActionPreference = $previousPreference
    Copy-Item (Join-Path $snapshotDir 'package.json') $manifestPath -Force
    Copy-Item (Join-Path $snapshotDir 'package-lock.json') $lockPath -Force
    Remove-Item $snapshotDir -Recurse -Force -ErrorAction SilentlyContinue
    Write-Banner 'restored plugins/vscode/package.json and package-lock.json'
}

$vsix = Get-ChildItem (Join-Path $vscodeDir '*.vsix') -ErrorAction SilentlyContinue |
    Sort-Object LastWriteTime -Descending |
    Select-Object -First 1

if (-not $vsix) {
    Write-Error 'vsce reported success but produced no .vsix in plugins/vscode'
    exit 1
}

$stopwatch.Stop()
Write-Banner ("done in {0:N1} min" -f $stopwatch.Elapsed.TotalMinutes)
foreach ($artifact in @($tgzPath, $engineExe, $vsix.FullName)) {
    $item = Get-Item $artifact
    Write-Host ("  {0,10:N1} MB  {1}" -f ($item.Length / 1MB), $item.FullName)
}
exit 0
