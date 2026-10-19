# Builds installer\output\PsychoWaluigiHD-Setup.exe.
# Compiles the proxy (i686), the viewer, cleanedge and hd_setup in release mode, stages them in
# installer\stage\ with the README and licences, then runs Inno Setup's ISCC on
# psycho_waluigi_hd.iss.
# Usage: powershell -File installer\build_installer.ps1 [-Version 1.0] [-Prebuilt]
#   -Prebuilt  take mmfs2.dll and hd_view.exe from hd_runtime\ instead of building them
#              (the binaries already tested there); cleanedge and hd_setup are still built.
param([string]$Version = '1.0', [switch]$Prebuilt)
$ErrorActionPreference = 'Stop'

$root = Split-Path -Parent $PSScriptRoot
$stage = Join-Path $PSScriptRoot 'stage'

function Build([string]$dir) {
    Write-Host "building $dir"
    Push-Location (Join-Path $root $dir)
    try {
        cargo build --release
        if ($LASTEXITCODE -ne 0) { throw "cargo build failed in $dir" }
    } finally { Pop-Location }
}

$iscc = @("$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe",
          "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe",
          "$env:ProgramFiles\Inno Setup 6\ISCC.exe") | Where-Object { Test-Path $_ } | Select-Object -First 1
if (-not $iscc) { throw 'Inno Setup 6 not found (winget install JRSoftware.InnoSetup)' }

if ($Prebuilt) {
    $proxy = Join-Path $root 'hd_runtime\mmfs2.dll'
    $viewer = Join-Path $root 'hd_runtime\hd_view.exe'
} else {
    Build 'proxy\mmfs2_proxy'
    Build 'viewer\hd_view'
    $proxy = Join-Path $root 'proxy\mmfs2_proxy\target\i686-pc-windows-msvc\release\mmfs2.dll'
    $viewer = Join-Path $root 'viewer\hd_view\target\release\hd_view.exe'
}
Build 'tools\cleanedge'
Build 'tools\hd_setup'

if (Test-Path $stage) { Remove-Item $stage -Recurse -Force }
New-Item -ItemType Directory -Force (Join-Path $stage 'licenses') | Out-Null
Copy-Item $proxy, $viewer $stage
Copy-Item (Join-Path $root 'tools\cleanedge\target\release\cleanedge.exe') $stage
Copy-Item (Join-Path $root 'tools\hd_setup\target\release\hd_setup.exe') $stage
Copy-Item (Join-Path $PSScriptRoot 'README.txt') $stage
Copy-Item (Join-Path $PSScriptRoot 'licenses\*') (Join-Path $stage 'licenses')

& $iscc "/DAppVersion=$Version" (Join-Path $PSScriptRoot 'psycho_waluigi_hd.iss')
if ($LASTEXITCODE -ne 0) { throw 'ISCC failed' }
Get-Item (Join-Path $PSScriptRoot 'output\PsychoWaluigiHD-Setup.exe') | Select-Object FullName, Length
