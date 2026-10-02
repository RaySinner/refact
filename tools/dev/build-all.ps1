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
# Concurrency: the GUI and engine parts are separate powershell.exe processes
# started with Start-Process BEFORE either is waited on, so they genuinely
# overlap. Start-Job is deliberately not used: a PowerShell 5.1 job streams the
# child runspace's output over a PSRemoting channel, and the sustained npm/cargo
# output saturates that channel on this machine until it dies with
# PSRemotingTransportException (FullyQualifiedErrorId 2100,PSSessionStateBroken),
# killing the job mid-build. Start-Process has no such channel - the child writes
# straight to its own log file.
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

# Every runtime artifact lives here: the two generated part scripts, their stdout
# and stderr logs, and the GUI success sentinel. The directory is ignored by the
# root .gitignore so a build never leaves untracked files behind; the logs are
# never deleted, because on a failed 50-minute build they are the only evidence.
$logDir = Join-Path $repoRoot '.build-logs'
$guiLog = Join-Path $logDir 'gui.log'
$guiErrLog = Join-Path $logDir 'gui.log.err'
$engineLog = Join-Path $logDir 'engine.log'
$engineErrLog = Join-Path $logDir 'engine.log.err'
$guiPartScript = Join-Path $logDir 'gui-part.ps1'
$enginePartScript = Join-Path $logDir 'engine-part.ps1'

# Written by the GUI part only after `npm pack` has SUCCEEDED. The engine part
# gates on this, see the comment in engine-part.ps1.
$guiSentinel = Join-Path $logDir 'gui.ready'

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

# The generated part scripts are ASCII, but they are read by Windows PowerShell
# 5.1, which treats a BOM-less file as ANSI. Write them without a BOM so the
# bytes are identical under either interpretation.
$asciiNoBom = New-Object System.Text.UTF8Encoding($false)

if (-not (Test-Path $logDir)) {
    New-Item -ItemType Directory -Path $logDir | Out-Null
}

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

# A stale sentinel from a previous run would let the engine part start immediately.
Remove-Item $guiSentinel -Force -ErrorAction SilentlyContinue

# --------------------------------------------------------------------------------
# The two part scripts.
#
# Each is a real file because Start-Process -File needs one: passing a script
# BLOCK to a child process is not possible without the remoting channel this card
# exists to avoid. They are generated rather than committed because they only
# carry parameters, all of which are passed on the command line.
# --------------------------------------------------------------------------------

$guiPartSource = @'
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$GuiDir,
    [Parameter(Mandatory = $true)][string]$BuildEnvScript,
    [Parameter(Mandatory = $true)][string]$Sentinel
)

# npm writes deprecation and progress notices to stderr as a matter of course.
# Under 'Stop' each one is a terminating error, so a perfectly healthy `npm ci`
# would abort. Success is judged on LastExitCode instead.
$ErrorActionPreference = 'Continue'

try {
    . $BuildEnvScript
    Set-Location $GuiDir

    # `npm ci` deletes node_modules before reinstalling, so on a worktree that
    # already has it (or on a shared junction) that is pure waste and, for the
    # latter, destructive. Only install when the directory is absent.
    if (-not (Test-Path (Join-Path $GuiDir 'node_modules'))) {
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

    [IO.File]::WriteAllText($Sentinel, 'ok')
    exit 0
}
catch {
    Write-Host "[gui] FAILED: $($_.Exception.Message)" -ForegroundColor Red
    exit 1
}
'@

$enginePartSource = @'
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$EngineDir,
    [Parameter(Mandatory = $true)][string]$BuildEnvScript,
    [Parameter(Mandatory = $true)][string]$Sentinel,
    [Parameter(Mandatory = $true)][string]$SccacheBypass,
    # "1" or "0", not a [switch]: passing `-CheckMode False` to a switch parameter
    # is ambiguous in PowerShell 5.1, where the value can bind either way.
    [string]$CheckMode = '0',
    [int]$GuiProcId = 0
)

$ErrorActionPreference = 'Continue'

try {
    . $BuildEnvScript
    Set-Location $EngineDir

    # REFACT_USE_PREBUILT_GUI=1 makes build.rs skip the npm build, but it does
    # NOT skip the `gui/dist/chat` -> `engine/assets/chat/dist/chat` copy, and
    # build.rs runs before any Rust is compiled. Copying a half-written dist
    # would silently bake a stale or partial UI into the binary, so this part
    # waits for the GUI part's success sentinel before invoking cargo. The wait
    # is bounded and reports a real error instead of hanging forever.
    if ($GuiProcId -gt 0) {
        # Poll QUIETLY. The previous implementation printed a line every 10 s for
        # the whole GUI build; silence costs nothing now that output goes to a
        # file anyway.
        $deadline = (Get-Date).AddMinutes(90)
        while (-not (Test-Path $Sentinel)) {
            if ((Get-Date) -gt $deadline) {
                throw "timed out after 90 minutes waiting for the GUI build to produce $Sentinel"
            }
            # A dead GUI process shows up here as a sentinel that never appears, so
            # the wait must not assume the GUI part is healthy. Get-Process is a
            # local OS query - no remoting involved, unlike the Get-Job check the
            # Start-Job version needed.
            if ($null -eq (Get-Process -Id $GuiProcId -ErrorAction SilentlyContinue)) {
                Write-Host '[engine] the GUI process is gone; not waiting any longer'
                break
            }
            Start-Sleep -Seconds 15
        }
    }

    # Never REFACT_SKIP_GUI_BUILD=1: that skips the asset copy too, which would
    # ship a binary with no UI at all.
    $env:REFACT_USE_PREBUILT_GUI = '1'
    Remove-Item Env:\REFACT_SKIP_GUI_BUILD -ErrorAction SilentlyContinue

    $cargoArgs = @('--config', $SccacheBypass)
    if ($CheckMode -eq '1') { $cargoArgs += @('check', '--workspace', '--all-targets') }
    else { $cargoArgs += @('build', '--release', '--bin', 'refact') }

    $proc = Start-Process -FilePath 'cargo' -ArgumentList $cargoArgs `
        -WorkingDirectory $EngineDir -NoNewWindow -Wait -PassThru
    if ($proc.ExitCode -ne 0) { throw "cargo exited with $($proc.ExitCode)" }
    exit 0
}
catch {
    Write-Host "[engine] FAILED: $($_.Exception.Message)" -ForegroundColor Red
    exit 1
}
'@

[IO.File]::WriteAllText($guiPartScript, $guiPartSource, $asciiNoBom)
[IO.File]::WriteAllText($enginePartScript, $enginePartSource, $asciiNoBom)

# Launch one part as a detached powershell.exe with its own stdout and stderr
# files. The caller must not wait here: both parts are started first, so their
# runtimes actually overlap.
function Start-BuildPart {
    param(
        [string]$Script,
        [string[]]$ScriptArgs,
        [string]$StdOut,
        [string]$StdErr
    )
    foreach ($file in @($StdOut, $StdErr)) {
        if (Test-Path $file) { Remove-Item $file -Force -ErrorAction SilentlyContinue }
    }
    $arguments = @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', $Script) + $ScriptArgs
    $proc = Start-Process -FilePath 'powershell.exe' -ArgumentList $arguments `
        -RedirectStandardOutput $StdOut -RedirectStandardError $StdErr `
        -WindowStyle Hidden -PassThru
    # Touch .Handle before the child exits. A Process object obtained from
    # Start-Process -PassThru does not reliably keep a queryable handle, and
    # reading .ExitCode afterwards then throws "No process is associated with
    # this object". Caching the handle here is the documented workaround.
    $null = $proc.Handle
    return $proc
}

$guiProc = $null
$engineProc = $null

if ($SkipGui) {
    Write-Banner "GUI part SKIPPED (-SkipGui)"
}
else {
    $guiProc = Start-BuildPart -Script $guiPartScript `
        -ScriptArgs @('-GuiDir', $guiDir, '-BuildEnvScript', $buildEnvScript, '-Sentinel', $guiSentinel) `
        -StdOut $guiLog -StdErr $guiErrLog
}

if ($SkipEngine) {
    Write-Banner "engine part SKIPPED (-SkipEngine)"
}
else {
    $checkModeArg = if ($Check) { '1' } else { '0' }
    # 0 when the GUI part is skipped: with no GUI process to wait for, the
    # sentinel wait must not run at all.
    $guiProcIdArg = if ($guiProc) { $guiProc.Id } else { 0 }
    $engineProc = Start-BuildPart -Script $enginePartScript `
        -ScriptArgs @('-EngineDir', $engineDir, '-BuildEnvScript', $buildEnvScript, '-Sentinel', $guiSentinel, '-SccacheBypass', $sccacheBypass, '-CheckMode', $checkModeArg, '-GuiProcId', "$guiProcIdArg") `
        -StdOut $engineLog -StdErr $engineErrLog
}

$parts = @()
if ($guiProc) { $parts += $guiProc }
if ($engineProc) { $parts += $engineProc }

if ($parts.Count -gt 0) {
    Write-Banner 'building (GUI and engine run concurrently)'
    # WaitForExit() rather than Wait-Process: it is what makes .ExitCode readable
    # afterwards, and Wait-Job/Wait-Process on Process objects does not populate it.
    foreach ($part in $parts) { $part.WaitForExit() }
    Write-Host "gui log         : $guiLog"
    Write-Host "engine log      : $engineLog"
}

# On failure the child output only exists in the redirected log files, and a build
# script that fails silently is what makes a 50-minute run undebuggable. Print the
# tail of the log that failed plus both full paths.
function Write-PartFailure {
    param([string]$Label, [string[]]$LogPaths, [int]$Tail = 40)
    Write-Host ''
    Write-Host "$Label part FAILED (exit code $($script:lastExit[$Label]))" -ForegroundColor Red
    foreach ($path in $LogPaths) {
        Write-Host "--- last $Tail lines of $path ---" -ForegroundColor Red
        if (Test-Path $path) {
            foreach ($line in @(Get-Content -LiteralPath $path -Tail $Tail -ErrorAction SilentlyContinue)) {
                Write-Host $line
            }
        }
        else {
            Write-Host '(no output captured - the process died before writing anything)'
        }
    }
}

# Read every exit code BEFORE deciding anything, so a failure report always names
# the part that actually failed. A non-zero code fails the build before anything is
# verified, copied or packaged.
$script:lastExit = @{}
$failedParts = @()

if ($guiProc) {
    $script:lastExit['GUI'] = $guiProc.ExitCode
    if ($guiProc.ExitCode -ne 0) { $failedParts += 'GUI' }
}
if ($engineProc) {
    $script:lastExit['engine'] = $engineProc.ExitCode
    if ($engineProc.ExitCode -ne 0) { $failedParts += 'engine' }
}

if ($failedParts.Count -gt 0) {
    if ($failedParts -contains 'GUI') { Write-PartFailure -Label 'GUI' -LogPaths @($guiLog, $guiErrLog) }
    if ($failedParts -contains 'engine') { Write-PartFailure -Label 'engine' -LogPaths @($engineLog, $engineErrLog) }
    Write-Host ''
    Write-Host "full logs: $logDir" -ForegroundColor Red
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
