# Assemble a runnable wallSet folder: release binaries + libmpv side by side.
# The binaries look for libmpv-2.dll next to themselves first, so a staged
# folder runs without the WALLSET_LIBMPV development variable.
#
#   .\tools\stage.ps1            -> .\dist\wallSet\
param([string]$Out = "dist\wallSet")

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot

# rustup installed with --no-modify-path leaves cargo off PATH; look in its
# standard location before giving up.
$cargo = (Get-Command cargo -ErrorAction SilentlyContinue).Source
if (-not $cargo) {
    $candidate = Join-Path $env:USERPROFILE ".cargo\bin\cargo.exe"
    if (Test-Path $candidate) { $cargo = $candidate }
    else { throw "cargo not found. Install Rust from https://rustup.rs, or add %USERPROFILE%\.cargo\bin to PATH." }
}

Push-Location $root
try {
    # cargo reports progress on stderr; Windows PowerShell 5.1 would turn
    # that into a terminating error when output is redirected.
    $ErrorActionPreference = "Continue"
    & $cargo build --release --workspace
    $ErrorActionPreference = "Stop"
    if ($LASTEXITCODE -ne 0) { throw "cargo build failed (exit $LASTEXITCODE)" }
    New-Item -ItemType Directory -Force $Out | Out-Null
    foreach ($exe in "wallset.exe", "wallset-daemon.exe", "wallset-render.exe", "wallset-ui.exe") {
        Copy-Item "target\release\$exe" $Out -Force
    }
    Copy-Item "vendor\mpv\libmpv-2.dll" $Out -Force
    Copy-Item "vendor\mpv\SOURCE.txt" (Join-Path $Out "LIBMPV-SOURCE.txt") -Force
    Get-ChildItem $Out | Select-Object Name, @{n = "Size"; e = { "{0:N1} MB" -f ($_.Length / 1MB) } } | Format-Table -AutoSize
    Write-Output "Staged in $Out. Start with: $Out\wallset-ui.exe"
}
finally {
    Pop-Location
}
