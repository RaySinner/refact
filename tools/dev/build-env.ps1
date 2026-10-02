# Windows build environment for the Refact engine.
# Import into PowerShell before cargo/vsce builds:
#   . C:\Raid\Repos\Rust\refact-main\tools\dev\build-env.ps1

$msvcRoot = "C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools"
$msvcTools = (Get-ChildItem "$msvcRoot\VC\Tools\MSVC" -Directory -ErrorAction SilentlyContinue |
    Sort-Object Name -Descending | Select-Object -First 1).FullName
$llvmBin = "C:\Program Files\LLVM\bin"
$cargoBin = "$env:USERPROFILE\.cargo\bin"
$nodeBin = "C:\Program Files\nodejs"

$paths = @($cargoBin, $llvmBin, $nodeBin)
if ($msvcTools) { $paths += "$msvcTools\bin\Hostx64\x64" }

foreach ($p in $paths) {
    if ($p -and (Test-Path $p) -and ($env:PATH -notlike "*$p*")) {
        $env:PATH = "$p;$env:PATH"
    }
}

# sccache requires incremental off to produce reusable cache entries.
$env:CARGO_INCREMENTAL = "0"
$env:SCCACHE_CACHE_SIZE = "50G"
$env:NODE_OPTIONS = "--max-old-space-size=8192"

Write-Host "PATH prepared (cargo, LLVM, Node, MSVC=$msvcTools)"
