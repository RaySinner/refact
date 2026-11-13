# O-1 verification harness: static proof that moving `scratchpads::multimodality`
# into `refact-scratchpads` is behaviour-preserving, adds no crate->engine edge,
# and unblocks the `tools::design_tools` census entry.
#
# Usage (single command, no separators, no pipes, no globs):
#   powershell -NoProfile -ExecutionPolicy Bypass -File tools\dev\o1-check.ps1
#   powershell -NoProfile -ExecutionPolicy Bypass -File refact-agent\engine\target\o1-check.ps1
$ErrorActionPreference = "Stop"
# Windows PowerShell decodes a native command's stdout with the console output
# encoding, and these sources carry a BOM plus non-ASCII glyphs, so without this
# `git show` returns mojibake and every comparison below fails for reasons that
# have nothing to do with the move.
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8
$OutputEncoding = [System.Text.Encoding]::UTF8
$fail = 0
$checks = 0

function Assert-True([bool]$cond, [string]$name) {
    $script:checks++
    if ($cond) { Write-Output "OK   $name" }
    else { Write-Output "FAIL $name"; $script:fail++ }
}

# Locate <repo> by walking up from this script, so the harness works from
# tools/dev/ and from refact-agent/engine/target/ without editing.
$probe = $PSScriptRoot
$repo = $null
for ($i = 0; $i -lt 8 -and $probe; $i++) {
    if ((Test-Path (Join-Path $probe "refact-agent\engine\src\scratchpads")) -and
        (Test-Path (Join-Path $probe "tools\dev\engine-build.ps1"))) { $repo = $probe; break }
    $probe = Split-Path $probe -Parent
}
if (-not $repo) { Write-Output "FAIL cannot locate the repository root"; exit 1 }
$engine = Join-Path $repo "refact-agent\engine"
$src = Join-Path $engine "src"
$crate = Join-Path $engine "crates\refact-scratchpads"
Assert-True $true "located engine at $engine"

$BASE = "a9f66c936"
$ENGINE_MOD = Join-Path $src "scratchpads\multimodality.rs"
$CRATE_MOD = Join-Path $crate "src\multimodality.rs"
$DESIGN = Join-Path $src "tools\design_tools.rs"
$SHIM = "pub use refact_scratchpads::multimodality::*;"

function Read-Text([string]$path) {
    return [IO.File]::ReadAllText($path, [Text.Encoding]::UTF8)
}
function Clean([string]$text) {
    return ($text -replace "`r`n", "`n") -replace "\uFEFF", ""
}
function Git-Show([string]$spec) {
    $prev = $ErrorActionPreference
    $ErrorActionPreference = "Continue"
    try { $text = & git -C $repo show $spec | Out-String } finally { $ErrorActionPreference = $prev }
    return Clean $text
}
function Lines([string]$text) {
    return [string[]]((Clean $text) -split "`n")
}
function Find-CfgTest([string[]]$lines) {
    for ($i = 6; $i -lt $lines.Count; $i++) {
        if ($lines[$i].Trim() -eq "#[cfg(test)]") { return $i }
    }
    return -1
}
function NonBlank([string[]]$lines) {
    $n = 0
    foreach ($l in $lines) { if ($l.Trim() -ne "") { $n++ } }
    return $n
}
function Tokenize([string]$text) {
    return (Clean $text) -replace "\s+", ""
}

# ---------------------------------------------------------------- R1 the engine file is a re-export shim
$engineBody = Clean (Read-Text $ENGINE_MOD)
Assert-True ($engineBody.Trim() -eq $SHIM) "engine multimodality.rs body is exactly the single-line shim"
Assert-True ($engineBody.Contains($SHIM)) "engine multimodality.rs re-exports the crate module"
Assert-True (-not ($engineBody -match "(?m)^\s*pub fn new")) "engine multimodality.rs declares no pub fn new"
Assert-True (-not ($engineBody -match "(?m)^\s*pub struct")) "engine multimodality.rs declares no pub struct"
Assert-True (-not ($engineBody -match "(?m)^\s*pub enum")) "engine multimodality.rs declares no pub enum"
Assert-True (-not ($engineBody -match "(?m)^\s*pub use refact_core")) "engine multimodality.rs no longer re-exports refact-core directly"

# ---- acceptance criterion 1, run literally: no `pub fn new` / `pub struct` in the engine file
$hits1 = @(Select-String -Path $ENGINE_MOD -Pattern "pub fn new|pub struct")
Assert-True ($hits1.Count -eq 0) "acceptance grep `pub fn new|pub struct` returns 0 matches in engine multimodality.rs"
foreach ($h in $hits1) { Write-Output ("      unexpected: " + $h.Line) }
Assert-True ($hits1.Count -eq 0) "engine multimodality.rs is re-exports only"

# ---- acceptance criterion 2, run literally: design_tools has no multimodality import
$hits2 = @(Select-String -Path $DESIGN -Pattern "crate::scratchpads::multimodality")
Assert-True ($hits2.Count -eq 0) "acceptance grep `crate::scratchpads::multimodality` returns 0 matches in design_tools.rs"
foreach ($h in $hits2) { Write-Output ("      unexpected: " + $h.Line) }

# ---------------------------------------------------------------- R2 the crate module holds the moved body verbatim
$origEngineLines = Lines (Git-Show ("{0}:refact-agent/engine/src/scratchpads/multimodality.rs" -f $BASE))
$origEngineBody = Clean ($origEngineLines -join "`n")
$crateBody = Clean (Read-Text $CRATE_MOD)
Assert-True ($crateBody -eq $origEngineBody) "crate multimodality.rs is byte-identical to the pre-move engine file"
Assert-True ($crateBody.Contains("MultimodalElement")) "crate multimodality.rs exports MultimodalElement"
Assert-True ($crateBody.Contains("pub use refact_core::chat_types::")) "crate multimodality.rs re-exports from refact-core"

# the moved names resolve to real bodies in refact-core, so no code was lost
$chatTypes = Clean (Read-Text (Join-Path $engine "crates\refact-core\src\chat_types.rs"))
Assert-True ($chatTypes.Contains("pub struct MultimodalElement")) "refact-core defines pub struct MultimodalElement"
Assert-True ($chatTypes.Contains("impl MultimodalElement")) "refact-core has an inherent impl for MultimodalElement"
Assert-True ($chatTypes.Contains("pub fn new(m_type: String, m_content: String)")) "refact-core defines MultimodalElement::new"

# ---------------------------------------------------------------- R3 crate wiring
$crateLib = Read-Text (Join-Path $crate "src\lib.rs")
Assert-True ($crateLib.Contains("pub mod multimodality;")) "crate lib.rs declares pub mod multimodality"
$engineModRs = Read-Text (Join-Path $src "scratchpads\mod.rs")
Assert-True ($engineModRs.Contains("pub mod multimodality;")) "engine scratchpads/mod.rs still declares pub mod multimodality"

# ---------------------------------------------------------------- R4 design_tools uses the crate path
$designNow = Clean (Read-Text $DESIGN)
Assert-True (-not $designNow.Contains("crate::scratchpads::multimodality")) "design_tools.rs no longer imports crate::scratchpads::multimodality"
Assert-True ($designNow.Contains("use refact_scratchpads::multimodality::MultimodalElement;")) "design_tools.rs imports MultimodalElement from refact-scratchpads"
Assert-True ($designNow.Contains("MultimodalElement::new(artifact.mime.clone(), artifact.data.clone())?")) "design_tools.rs calls MultimodalElement::new through the import"
Assert-True (-not ($designNow.Contains("crate::scratchpads::"))) "design_tools.rs has no crate::scratchpads path left"

# only one occurrence of the new import (no duplicate/conflicting import)
$importHits = ([regex]::Matches($designNow, [regex]::Escape("use refact_scratchpads::multimodality::MultimodalElement;"))).Count
Assert-True ($importHits -eq 1) "design_tools.rs has exactly one refact-scratchpads import (found $importHits)"

# ---------------------------------------------------------------- R5 design_tools body is verbatim modulo the documented edits
$origDesignLines = Lines (Git-Show ("{0}:refact-agent/engine/src/tools/design_tools.rs" -f $BASE))
$cut = Find-CfgTest $origDesignLines
Assert-True ($cut -gt 0) "found #[cfg(test)] in pre-move design_tools.rs"
$nowLines = Lines (Read-Text $DESIGN)
$cutNow = Find-CfgTest $nowLines
Assert-True ($cutNow -gt 0) "found #[cfg(test)] in current design_tools.rs"

if ($cut -gt 0 -and $cutNow -gt 0) {
    $prodBase = ($origDesignLines[0..($cut - 1)]) -join "`n"
    $prodNow = ($nowLines[0..($cutNow - 1)]) -join "`n"
    function Flatten([string]$text) {
        # tokens only: drop whitespace, then the two edits this card makes are no-ops
        # (a removed trailing comma before `)` and the new `use` line)
        $t = ($text -replace "\s+", "") -replace ",\)", ")"
        return $t -replace "userefact_scratchpads::multimodality::MultimodalElement;", ""
    }
    $baseTok = Flatten ((Clean $prodBase) -replace "crate::scratchpads::multimodality::", "")
    $nowTok = Flatten (Clean $prodNow)
    Assert-True ($baseTok -eq $nowTok) "design_tools.rs production body is token-identical modulo the path rewrite and the new import"
    Assert-True ($baseTok.Contains('MultimodalElement::new(artifact.mime.clone(),artifact.data.clone())?')) "the two-element payload shape survives verbatim"
    Assert-True ($baseTok.Contains('MultimodalElement::new("text".to_string(),text)?')) "the text element payload shape survives verbatim"

    $baseTests = ($origDesignLines[$cut..($origDesignLines.Count - 1)]) -join "`n"
    $nowTests = ($nowLines[$cutNow..($nowLines.Count - 1)]) -join "`n"
    Assert-True ((Clean $baseTests) -eq (Clean $nowTests)) "design_tools.rs inline tests are byte-identical to the pre-move file"
    Assert-True ((NonBlank $origDesignLines[0..($cut - 1)])) -gt 0

    Write-Output ("design_tools.rs production lines: base={0} now={1}" -f (NonBlank $origDesignLines[0..($cut - 1)]), (NonBlank $nowLines[0..($cutNow - 1)]))
}

# ---------------------------------------------------------------- R6 every other consumer still resolves through the shim
$consumers = @(
    "src\tools\tool_chrome.rs",
    "src\integrations\mcp\tool_mcp.rs",
    "src\agentic\mode_transition.rs",
    "src\chat\tests.rs"
)
foreach ($rel in $consumers) {
    $text = Read-Text (Join-Path $engine $rel)
    Assert-True ($text.Contains("crate::scratchpads::multimodality::MultimodalElement")) "$rel still reaches the module through the engine shim"
}

# no inline fully-qualified path survives anywhere in the engine sources
$inline = @()
foreach ($file in Get-ChildItem $src -Recurse -Filter *.rs) {
    $text = Clean (Read-Text $file.FullName)
    if ($text.Contains("scratchpads::multimodality::MultimodalElement::new")) {
        $inline += $file.FullName
    }
}
Assert-True ($inline.Count -eq 0) "no fully-qualified MultimodalElement::new path left in src ($($inline -join ', '))"

# ---------------------------------------------------------------- R7 no crate->engine edge, either direction
$crateToml = Read-Text (Join-Path $crate "Cargo.toml")
Assert-True (-not ($crateToml.Contains("refact-lsp"))) "refact-scratchpads Cargo.toml never mentions refact-lsp"
Assert-True (-not ($crateToml -match 'path\s*=\s*"\.\./\.\.')) "refact-scratchpads Cargo.toml has no path dep on the engine crate"
Assert-True ($crateToml.Contains('refact-core = { path = "../refact-core" }')) "refact-scratchpads depends on refact-core"
$engineToml = Read-Text (Join-Path $engine "Cargo.toml")
Assert-True ($engineToml.Contains('refact-scratchpads = { path = "crates/refact-scratchpads" }')) "engine Cargo.toml depends on refact-scratchpads"

# ---------------------------------------------------------------- R8 the census blocker is gone
$censusOut = ""
$prev = $ErrorActionPreference
$ErrorActionPreference = "Continue"
try {
    $censusOut = (& python (Join-Path $repo "tools\dev\chat_movable_census.py") tools | Out-String)
} finally { $ErrorActionPreference = $prev }
$censusFile = Join-Path $engine "target\tools_movable_census.txt"
$censusText = ""
if (Test-Path $censusFile) { $censusText = Clean (Read-Text $censusFile) }
Assert-True ($censusText.Length -gt 0) "census output file exists"
Assert-True (-not ($censusText.Contains("blocker=scratchpads::multimodality"))) "census no longer reports the scratchpads::multimodality blocker"
Assert-True (-not ($censusText.Contains("blocker=tools::design_tools"))) "the five UI-session stubs are no longer blocked on design_tools"
$movable = ($censusText -split "### LEFT / ISLANDS")[0]
Assert-True ($movable.Contains("tools::design_tools")) "census lists tools::design_tools as MOVABLE"
foreach ($stub in @("tool_ui_probe", "tool_mark_elements", "tool_contrast_audit", "tool_image_region", "tool_visual_diff")) {
    Assert-True ($movable.Contains("tools::$stub")) "census lists tools::$stub as MOVABLE"
}
$designLine = @($movable -split "`n" | Where-Object { $_.Contains("tools::design_tools") })
if ($designLine.Count -gt 0) { Write-Output ("census: {0}" -f $designLine[0].Trim()) }

# the exact unblocked set, so the count the report quotes is machine-checked
$UNBLOCKED = @(
    "tools::design_tools",
    "tools::tool_ui_probe",
    "tools::tool_mark_elements",
    "tools::tool_contrast_audit",
    "tools::tool_image_region",
    "tools::tool_visual_diff"
)
$movableLines = @($movable -split "`n")
foreach ($mod in $UNBLOCKED) {
    $hit = @($movableLines | Where-Object { $_ -match ("^\s*\d+\s+" + [regex]::Escape($mod) + "\s") })
    Assert-True ($hit.Count -eq 1) "census has exactly one MOVABLE row for $mod"
    if ($hit.Count -eq 1) { Write-Output ("      " + $hit[0].Trim()) }
}
$stillBlocked = @($censusText -split "### LEFT / ISLANDS")[1]
foreach ($mod in $UNBLOCKED) {
    Assert-True (-not ($stillBlocked -match ("^\s*\d+\s+" + [regex]::Escape($mod) + "\s"))) "$mod left the blocked table"
}

# ---------------------------------------------------------------- R9 scope guard
$prev = $ErrorActionPreference
$ErrorActionPreference = "Continue"
try { $dirty = @(& git -C $repo status --porcelain) } finally { $ErrorActionPreference = $prev }
$touched = @()
foreach ($line in $dirty) {
    if ($line.Length -gt 3) { $touched += ($line.Substring(3).Trim() -replace "\\", "/") }
}
$allowed = @(
    "refact-agent/engine/src/scratchpads/",
    "refact-agent/engine/src/tools/design_tools.rs",
    "refact-agent/engine/crates/refact-scratchpads/",
    "tools/dev/o1-"
)
$extra = @($touched | Where-Object {
    $p = $_
    @($allowed | Where-Object { $p.StartsWith($_) }).Count -eq 0
})
Assert-True ($extra.Count -eq 0) "no unrelated file touched ($($extra -join ', '))"
foreach ($untouched in @(
    "refact-agent/engine/Cargo.toml",
    "refact-agent/engine/Cargo.lock",
    "refact-agent/engine/src/scratchpads/mod.rs",
    "refact-agent/engine/src/tools/mod.rs",
    "refact-agent/engine/src/tools/tools_list.rs",
    "refact-agent/engine/src/tools/tool_chrome.rs",
    "refact-agent/engine/src/integrations/mcp/tool_mcp.rs",
    "refact-agent/engine/src/agentic/mode_transition.rs",
    "refact-agent/engine/src/chat/tests.rs"
)) {
    Assert-True (-not ($touched -contains $untouched)) "$untouched untouched"
}

Write-Output ""
Write-Output "checks=$checks failures=$fail"
if ($fail -ne 0) { exit 1 }
exit 0
