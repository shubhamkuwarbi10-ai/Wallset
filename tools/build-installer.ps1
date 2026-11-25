# Build dist\wallSet-Setup-<version>.exe: stage the release binaries with
# stage.ps1, then compile installer\wallSet.iss with Inno Setup 6.
#
#   .\tools\build-installer.ps1
#
# Needs Inno Setup 6 (winget install JRSoftware.InnoSetup) and
# vendor\mpv\libmpv-2.dll (see vendor\mpv\SOURCE.txt).

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot

$iscc = (Get-Command iscc -ErrorAction SilentlyContinue).Source
if (-not $iscc) {
    $iscc = @(
        "$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe",
        "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe",
        "$env:ProgramFiles\Inno Setup 6\ISCC.exe"
    ) | Where-Object { Test-Path $_ } | Select-Object -First 1
}
if (-not $iscc) { throw "Inno Setup 6 not found. Install it with: winget install JRSoftware.InnoSetup" }

# The workspace version, so the installer and the binaries agree.
$version = (Select-String -Path (Join-Path $root "Cargo.toml") -Pattern '^version\s*=\s*"([^"]+)"').Matches[0].Groups[1].Value

& (Join-Path $PSScriptRoot "stage.ps1") -Out (Join-Path $root "dist\wallSet")
& $iscc "/DAppVersion=$version" (Join-Path $root "installer\wallSet.iss")
if ($LASTEXITCODE -ne 0) { throw "Inno Setup failed (exit $LASTEXITCODE)" }
Get-Item (Join-Path $root "dist\wallSet-Setup-$version.exe") | Select-Object Name, @{n = "Size"; e = { "{0:N1} MB" -f ($_.Length / 1MB) } }
