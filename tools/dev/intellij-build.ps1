# Build the JetBrains plugin in a reproducible JDK 17 container.
#
# Exact command (run from the repository root):
#   powershell -NoProfile -ExecutionPolicy Bypass -File tools\dev\intellij-build.ps1
#
# Required Docker version: Docker Engine 20.10 or newer (Docker Desktop with
# Engine 20.10 or newer is supported).
#
# Host Gradle cache: $env:REFACT_BUILD_ROOT\intellij\gradle-home when
# REFACT_BUILD_ROOT is set; otherwise
# %LOCALAPPDATA%\RefactBuild\intellij\gradle-home. The directory is mounted
# at /gradle-home in the container and reused on later runs.
#
# ASCII only on purpose: Windows PowerShell 5.1 reads .ps1 files as ANSI when
# there is no BOM, so non-ASCII characters here would be mangled.
[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'

$repoRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..\..'))
$pluginDir = Join-Path $repoRoot 'plugins\intellij'
$image = 'eclipse-temurin:17-jdk'
$minimumDockerVersion = [version]'20.10.0'

$buildRoot = if ($env:REFACT_BUILD_ROOT) {
    $env:REFACT_BUILD_ROOT
}
else {
    Join-Path $env:LOCALAPPDATA 'RefactBuild'
}
$gradleHome = Join-Path $buildRoot 'intellij\gradle-home'
$containerName = "refact-intellij-build-$PID-$([DateTime]::UtcNow.Ticks)"
$containerGradlew = Join-Path $buildRoot "intellij\gradlew-$PID-$([DateTime]::UtcNow.Ticks)"

foreach ($required in @($pluginDir, (Join-Path $pluginDir 'gradlew'))) {
    if (-not (Test-Path -LiteralPath $required)) {
        Write-Error "required path not found: $required"
        exit 2
    }
}

if (-not (Get-Command docker -ErrorAction SilentlyContinue)) {
    Write-Error 'Docker is required but the docker command was not found in PATH.'
    exit 2
}

$previousErrorActionPreference = $ErrorActionPreference
$ErrorActionPreference = 'Continue'
$dockerVersionOutput = & docker version --format '{{.Server.Version}}' 2>&1
$dockerVersionExitCode = $LASTEXITCODE
$ErrorActionPreference = $previousErrorActionPreference
if ($dockerVersionExitCode -ne 0) {
    Write-Error "Docker Engine is not available. `n$dockerVersionOutput"
    exit 2
}

$dockerVersionText = ($dockerVersionOutput | Out-String).Trim()
try {
    $dockerVersion = [version]$dockerVersionText
}
catch {
    Write-Error "Could not parse the Docker Engine version: $dockerVersionText"
    exit 2
}
if ($dockerVersion -lt $minimumDockerVersion) {
    Write-Error "Docker Engine $minimumDockerVersion or newer is required; found $dockerVersion."
    exit 2
}

New-Item -ItemType Directory -Force -Path $gradleHome | Out-Null

# A Windows checkout can contain CRLF in gradlew. Linux would then try to run
# "/bin/sh\r" and fail before Gradle starts. Mount a temporary LF-only copy over
# that one file in the container, without changing the tracked wrapper on disk.
$gradlewBytes = [IO.File]::ReadAllBytes((Join-Path $pluginDir 'gradlew'))
$gradlewText = [Text.Encoding]::UTF8.GetString($gradlewBytes).Replace("`r`n", "`n")
[IO.File]::WriteAllText($containerGradlew, $gradlewText, (New-Object Text.UTF8Encoding($false)))

Write-Host "Docker Engine    : $dockerVersion"
Write-Host "JDK image        : $image"
Write-Host "Repository mount : $repoRoot -> /workspace"
Write-Host "Gradle cache     : $gradleHome -> /gradle-home"

$dockerArgs = @(
    'run',
    '--rm',
    '--name', $containerName,
    '--mount', "type=bind,source=$repoRoot,target=/workspace",
    '--mount', "type=bind,source=$gradleHome,target=/gradle-home",
    '--mount', "type=bind,source=$containerGradlew,target=/workspace/plugins/intellij/gradlew",
    '--workdir', '/workspace/plugins/intellij',
    '--env', 'GRADLE_USER_HOME=/gradle-home',
    $image,
    './gradlew', 'build', '--no-daemon', '--console=plain'
)

$buildExitCode = 1
try {
    # Windows PowerShell 5.1 can turn native stderr records into terminating
    # errors when ErrorActionPreference is Stop. Gradle and the IntelliJ test
    # runtime use stderr for warnings, so judge success by Docker's exit code.
    $previousErrorActionPreference = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    # Merge Docker's stderr into stdout before PowerShell sees it. Otherwise
    # Windows PowerShell 5.1 labels native stderr as NativeCommandError even
    # when the process succeeds, and powershell.exe can then return exit 1.
    & docker @dockerArgs 2>&1 | ForEach-Object { Write-Host $_ }
    $buildExitCode = $LASTEXITCODE
    $ErrorActionPreference = $previousErrorActionPreference
}
finally {
    # --rm removes the container on a normal exit. This is a defensive cleanup
    # for an interrupted run, and intentionally ignores an already-removed name.
    $previousErrorActionPreference = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    & docker rm --force $containerName 2>$null | Out-Null
    $ErrorActionPreference = $previousErrorActionPreference
    Remove-Item -LiteralPath $containerGradlew -Force -ErrorAction SilentlyContinue
}

if ($buildExitCode -ne 0) {
    Write-Host "IntelliJ Gradle build failed (exit code $buildExitCode)." -ForegroundColor Red
    Write-Host 'Inspect the Gradle output above. For an SDK/dependency resolution failure, verify network access to services.gradle.org, plugins.gradle.org, cache-redirector.jetbrains.com, and repo1.maven.org.' -ForegroundColor Red
    exit $buildExitCode
}

Write-Host 'IntelliJ Gradle build completed successfully.' -ForegroundColor Green
exit 0
