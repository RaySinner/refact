[CmdletBinding()]
param(
    [ValidateSet("All", "ClaudeCode", "OpenCode", "Combined")]
    [string]$Suite = "All",
    [switch]$RequireLive,
    [string]$ComposeFile = "",
    [string]$EvidenceDirectory = ""
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$Root = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot "..\.."))
if ([string]::IsNullOrWhiteSpace($ComposeFile)) {
    $ComposeFile = Join-Path $Root "packaging\docker\uhp-advisors.yaml"
}
if ([string]::IsNullOrWhiteSpace($EvidenceDirectory)) {
    $EvidenceDirectory = Join-Path ([IO.Path]::GetTempPath()) "refact-uhp-live-evidence"
}
$EngineDirectory = Join-Path $Root "refact-agent\engine"
$LiveTestFile = Join-Path $EngineDirectory "tests\uhp_live_smoke.rs"
$ExpectedDigest = "sha256:374709e16d6426ea58a1e238df40476bb56e53c9b1b20c3210bc3a934b79c53f"

$CommonGates = @(
    "discovery_conformance",
    "first_streaming",
    "follow_up",
    "model_identity",
    "artifact_review",
    "cold_recycle",
    "same_key_idempotency",
    "session_busy",
    "disconnect_recovery",
    "cancel_descendants",
    "restart_resume",
    "read_only_write_canary",
    "committed_uncommitted_markers",
    "stale_source",
    "credential_canaries",
    "prompt_injection",
    "budgets",
    "public_privacy",
    "router_delivery"
)

function Get-GatesForSuite {
    param([string]$Name)
    switch ($Name) {
        "ClaudeCode" { return $CommonGates }
        "OpenCode" { return $CommonGates + @("provider_url_normalization", "no_second_provider_route") }
        "Combined" {
            return @(
                "combined_fanout",
                "combined_idempotency",
                "combined_privacy",
                "combined_independence",
                "combined_bounded_replay",
                "combined_stale_source"
            )
        }
        default { throw "Unknown suite name" }
    }
}

function Get-BaseForSuite {
    param([string]$Name)
    switch ($Name) {
        "ClaudeCode" { return "claude-code" }
        "OpenCode" { return "opencode" }
        "Combined" { return "combined" }
        default { throw "Unknown suite name" }
    }
}

function Get-CliPinForSuite {
    param([string]$Name)
    switch ($Name) {
        "ClaudeCode" { return "2.1.311" }
        "OpenCode" { return "1.18.23" }
        "Combined" { return "not-applicable" }
        default { throw "Unknown suite name" }
    }
}

function Write-SanitizedJson {
    param(
        [string]$Path,
        [object]$Value
    )
    $json = $Value | ConvertTo-Json -Depth 12
    [IO.File]::WriteAllText($Path, $json + [Environment]::NewLine, [Text.UTF8Encoding]::new($false))
}

function Write-StaticReport {
    param([string]$Name)
    $checks = foreach ($gate in (Get-GatesForSuite -Name $Name)) {
        [ordered]@{
            name = $gate
            status = "SKIP"
            error_code = "live_not_requested"
            duration_ms = 0
            response_ids = @()
        }
    }
    $report = [ordered]@{
        schema_version = 1
        suite = $Name
        base = Get-BaseForSuite -Name $Name
        mode = "static"
        generated_at = [DateTimeOffset]::UtcNow.ToString("o")
        ce_version = "0.41.25"
        ce_image_digest = $ExpectedDigest
        cli_version = Get-CliPinForSuite -Name $Name
        protocol_version = "2026-10-19"
        checks = @($checks)
        verdict = "BLOCKED_NO_GO"
        production_support = $false
        detail = "Static configuration only; no CE task, provider call, or live support claim was made."
    }
    $path = Join-Path $EvidenceDirectory (($Name.ToLowerInvariant()) + ".static.json")
    Write-SanitizedJson -Path $path -Value $report
    Write-Output ("STATIC {0}: BLOCKED_NO_GO; report={1}" -f $Name, $path)
}

function Assert-StaticConfiguration {
    foreach ($path in @($ComposeFile, $LiveTestFile)) {
        if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
            throw "Required LIVE-1 file is missing: $path"
        }
    }
    $compose = [IO.File]::ReadAllText($ComposeFile)
    $requiredFragments = @(
        "harnessrouter/harnessrouter:0.41.25@$ExpectedDigest",
        '"127.0.0.1:3000:3000"',
        'HR_SESSION_LEASE: "enforce"',
        'CLAUDE_CODE_VERSION: "2.1.311"',
        'HR_OPENCODE_VERSION: "1.18.23"',
        'HR_BACKENDS: "claude,opencode"',
        'refact-uhp-live-data:/data',
        '${HR_SECRET_KEY:?',
        '${HR_AUTH_PASSWORD:?'
    )
    foreach ($fragment in $requiredFragments) {
        if (-not $compose.Contains($fragment)) {
            throw "Pinned compose configuration is missing a required safety setting"
        }
    }
    if ($compose -match "(?i)latest|codex|sk-[A-Za-z0-9]|Bearer\s+[A-Za-z0-9]") {
        throw "Compose configuration contains a forbidden image, base, or credential literal"
    }
    $driver = [IO.File]::ReadAllText($LiveTestFile)
    foreach ($suite in @("ClaudeCode", "OpenCode", "Combined")) {
        foreach ($gate in (Get-GatesForSuite -Name $suite)) {
            if (-not $driver.Contains('"' + $gate + '"')) {
                throw "Live driver does not declare required gate $gate"
            }
        }
    }
    $docker = Get-Command docker -ErrorAction SilentlyContinue
    if ($null -ne $docker) {
        & docker compose -f $ComposeFile config --no-interpolate | Out-Null
        if ($LASTEXITCODE -ne 0) {
            throw "Docker Compose rejected the staging file"
        }
    }
}

function Get-RequiredEnvironment {
    param([string]$Name)
    $value = [Environment]::GetEnvironmentVariable($Name)
    if ([string]::IsNullOrWhiteSpace($value)) {
        throw "Live prerequisite $Name is not set; no live task was started"
    }
    return $value
}

function Assert-LivePrerequisites {
    if ([Environment]::GetEnvironmentVariable("REFACT_UHP_LIVE") -ne "1") {
        throw "-RequireLive also requires REFACT_UHP_LIVE=1; no external task or paid provider call was started"
    }
    foreach ($name in @(
        "UHP_API_KEY",
        "UHP_OPERATOR",
        "UHP_CREDENTIAL_CANARY",
        "UHP_CLAUDE_CONFORMANCE_REPORT",
        "UHP_OPENCODE_CONFORMANCE_REPORT",
        "UHP_OBSERVATION_FILE",
        "UHP_CE_IMAGE_DIGEST",
        "UHP_CLAUDE_HARNESS_ID",
        "UHP_CLAUDE_ENVIRONMENT_ID",
        "UHP_CLAUDE_MODEL",
        "UHP_CLAUDE_PROVIDER_CREDENTIAL_REF",
        "UHP_CLAUDE_CLI_VERSION",
        "UHP_OPENCODE_HARNESS_ID",
        "UHP_OPENCODE_ENVIRONMENT_ID",
        "UHP_OPENCODE_MODEL",
        "UHP_OPENCODE_PROVIDER_CREDENTIAL_REF",
        "UHP_OPENCODE_CLI_VERSION"
    )) {
        $null = Get-RequiredEnvironment -Name $name
    }
    if ([Environment]::GetEnvironmentVariable("UHP_CE_IMAGE_DIGEST") -ne $ExpectedDigest) {
        throw "Running CE image digest does not match the pinned staging image"
    }
    if ([Environment]::GetEnvironmentVariable("UHP_CLAUDE_CLI_VERSION") -ne "2.1.311") {
        throw "Observed Claude Code CLI version does not match the approved pin"
    }
    if ([Environment]::GetEnvironmentVariable("UHP_OPENCODE_CLI_VERSION") -ne "1.18.23") {
        throw "Observed OpenCode CLI version does not match the approved pin"
    }
    $serverRoot = [Environment]::GetEnvironmentVariable("UHP_SERVER_ROOT")
    if ([string]::IsNullOrWhiteSpace($serverRoot)) {
        $serverRoot = "http://127.0.0.1:3000/api/harness"
        [Environment]::SetEnvironmentVariable("UHP_SERVER_ROOT", $serverRoot)
    }
    $uri = [Uri]$serverRoot
    if ($uri.Scheme -notin @("http", "https") -or $uri.Host -notin @("127.0.0.1", "localhost", "::1")) {
        throw "Live staging must use a loopback UHP server root"
    }
    foreach ($name in @("UHP_CLAUDE_CONFORMANCE_REPORT", "UHP_OPENCODE_CONFORMANCE_REPORT", "UHP_OBSERVATION_FILE")) {
        $path = [Environment]::GetEnvironmentVariable($name)
        if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
            throw "Live prerequisite $name does not identify a readable sanitized evidence file"
        }
    }
}

function Assert-ReportsContainNoSecrets {
    $secretNames = @(
        "UHP_API_KEY",
        "HR_SECRET_KEY",
        "HR_AUTH_PASSWORD",
        "ANTHROPIC_API_KEY",
        "OPENAI_API_KEY"
        "UHP_CREDENTIAL_CANARY"
    )
    $secrets = foreach ($name in $secretNames) {
        $value = [Environment]::GetEnvironmentVariable($name)
        if (-not [string]::IsNullOrWhiteSpace($value) -and $value.Length -ge 4) {
            $value
        }
    }
    foreach ($report in Get-ChildItem -LiteralPath $EvidenceDirectory -Filter "*.json" -File) {
        $text = [IO.File]::ReadAllText($report.FullName)
        foreach ($secret in $secrets) {
            if ($text.Contains($secret)) {
                throw "A sanitized evidence report contains a credential value; report path withheld"
            }
        }
    }
}

New-Item -ItemType Directory -Path $EvidenceDirectory -Force | Out-Null
$EvidenceDirectory = [IO.Path]::GetFullPath($EvidenceDirectory)
Assert-StaticConfiguration

if (-not $RequireLive) {
    $suites = if ($Suite -eq "All") { @("ClaudeCode", "OpenCode", "Combined") } else { @($Suite) }
    foreach ($name in $suites) {
        Write-StaticReport -Name $name
    }
    Write-Output "Static validation passed. This is not live evidence and not production GO."
    exit 0
}

Assert-LivePrerequisites
[Environment]::SetEnvironmentVariable("UHP_SUITE", $Suite)
[Environment]::SetEnvironmentVariable("UHP_EVIDENCE_DIRECTORY", $EvidenceDirectory)
[Environment]::SetEnvironmentVariable("UHP_EXPECTED_IMAGE_DIGEST", $ExpectedDigest)
$refactCommit = (& git -C $Root rev-parse HEAD).Trim()
if ($LASTEXITCODE -ne 0 -or [string]::IsNullOrWhiteSpace($refactCommit)) {
    throw "Unable to bind live evidence to a Refact commit"
}
[Environment]::SetEnvironmentVariable("UHP_REFACT_COMMIT", $refactCommit)

Push-Location $EngineDirectory
try {
    & cargo test --test uhp_live_smoke live_evidence_suites -- --ignored --nocapture --test-threads=1
    $liveExit = $LASTEXITCODE
}
finally {
    Pop-Location
}
Assert-ReportsContainNoSecrets
if ($liveExit -ne 0) {
    throw "Live evidence gates failed or were skipped; inspect sanitized reports in $EvidenceDirectory. Production remains NO-GO."
}
Write-Output "Live driver completed. Production GO is valid only for reports whose verdict is GO and whose pins and model identities match."
