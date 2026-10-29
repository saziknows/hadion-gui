<#
.SYNOPSIS
  Build Hadion and package a release the updater can install.

.DESCRIPTION
  1. cargo build -p hadion (--release unless -Profile debug), skipped with -NoBuild
  2. stage dist\hadion-<version>\ : hadion.exe, README.md, LICENSE-MIT,
     LICENSE-APACHE, licenses\ (the bundled fonts' OFL texts and
     THIRD-PARTY-LICENSES.txt from `cargo run --example notices`)
  3. dist\hadion-<version>-windows-<arch>.zip  (the release asset)
  4. the installer dist\hadion-<version>-windows-<arch>-setup.exe with
     Inno Setup (ISCC) when it is installed; otherwise how to get it
  5. dist\SHA256SUMS listing the zip (and the installer)

  The version comes from [workspace.package] in Cargo.toml. The release
  contract the updater checks: a release tagged v<version> carrying
  hadion-<version>-windows-x86_64.zip and SHA256SUMS.

.EXAMPLE
  ./scripts/package.ps1
  Release build and package (what .github/workflows/release.yml runs).

.EXAMPLE
  ./scripts/package.ps1 -Profile debug -NoBuild
  Package the existing debug build, e.g. to test the packaging steps.

.EXAMPLE
  ./scripts/package.ps1 -NoBuild -Exe C:\tmp\hadion.exe -OutDir C:\tmp\dist -NoInstaller
  Package a given executable into another folder (update tests).
#>
[CmdletBinding()]
param(
    [ValidateSet('release', 'debug')]
    [string]$Profile = 'release',
    [switch]$NoBuild,
    [string]$Exe = '',
    [string]$OutDir = '',
    [switch]$NoInstaller
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$root = Split-Path -Parent $PSScriptRoot
if (-not $OutDir) { $OutDir = Join-Path $root 'dist' }
New-Item -ItemType Directory -Force $OutDir | Out-Null
$OutDir = (Resolve-Path $OutDir).Path

function Step([string]$text) { Write-Host "`n== $text" -ForegroundColor Cyan }

# --- Version and platform ---------------------------------------------------
$cargoToml = Get-Content (Join-Path $root 'Cargo.toml') -Raw
$m = [regex]::Match($cargoToml, '(?ms)^\[workspace\.package\][^\[]*?^version\s*=\s*"([^"]+)"')
if (-not $m.Success) { throw 'No version in [workspace.package] of Cargo.toml' }
$version = $m.Groups[1].Value
$arch = if ($env:PROCESSOR_ARCHITECTURE -eq 'ARM64') { 'aarch64' } else { 'x86_64' }
$platform = "windows-$arch"
$name = "hadion-$version"
Write-Host "Hadion $version for $platform ($Profile)"

# --- Build ----------------------------------------------------------------------
if (-not $NoBuild) {
    $cargoArgs = @('build', '-p', 'hadion', '--locked')
    if ($Profile -eq 'release') { $cargoArgs += '--release' }
    Step "cargo $($cargoArgs -join ' ')"
    & cargo @cargoArgs
    if ($LASTEXITCODE -ne 0) { throw "cargo build failed (exit $LASTEXITCODE)" }
}
if (-not $Exe) {
    $targetDir = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $root 'target' }
    $Exe = Join-Path $targetDir "$Profile\hadion.exe"
}
if (-not (Test-Path $Exe -PathType Leaf)) {
    throw "No executable at $Exe (build first, or pass -Exe PATH)"
}

# --- Stage ------------------------------------------------------------------------
$stage = Join-Path $OutDir $name
Step "Stage $stage"
if (Test-Path $stage) { Remove-Item $stage -Recurse -Force }
New-Item -ItemType Directory -Force (Join-Path $stage 'licenses') | Out-Null
Copy-Item $Exe (Join-Path $stage 'hadion.exe')
Copy-Item (Join-Path $root 'README.md') $stage
$licences = @()
foreach ($f in 'LICENSE', 'LICENSE.md', 'LICENSE.txt', 'LICENSE-MIT', 'LICENSE-APACHE') {
    $p = Join-Path $root $f
    if (Test-Path $p) { Copy-Item $p $stage; $licences += $f }
}
if (-not $licences) {
    Write-Warning 'The repository has no LICENSE file: the package carries only the font licences.'
}
Copy-Item (Join-Path $root 'crates\hadion\assets\fonts\*-OFL.txt') (Join-Path $stage 'licenses')
Copy-Item (Join-Path $root 'crates\hadion\assets\fonts\*-LICENSE.txt') (Join-Path $stage 'licenses')
# Every crate compiled into Hadion, with its licence files (MIT, BSD, ISC
# and Zlib ask for their notices to travel with binaries).
$notices = Join-Path $stage 'licenses\THIRD-PARTY-LICENSES.txt'
$noticeArgs = @('run', '--quiet', '--locked', '-p', 'hadion-core', '--example', 'notices')
if ($Profile -eq 'release') { $noticeArgs += '--release' }
& cargo @noticeArgs -- --out $notices
if ($LASTEXITCODE -ne 0) { throw "cargo run --example notices failed (exit $LASTEXITCODE)" }
Get-ChildItem $stage -Recurse -File | ForEach-Object {
    '  {0,12:N0}  {1}' -f $_.Length, $_.FullName.Substring($stage.Length + 1)
}

# --- Zip --------------------------------------------------------------------------
$zip = Join-Path $OutDir "$name-$platform.zip"
Step "Zip $zip"
if (Test-Path $zip) { Remove-Item $zip -Force }
Compress-Archive -Path $stage -DestinationPath $zip -CompressionLevel Optimal
$assets = @($zip)

# --- Installer (Inno Setup) ---------------------------------------------------------
if (-not $NoInstaller) {
    Step 'Installer'
    $iscc = $null
    $cmd = Get-Command 'ISCC.exe' -ErrorAction SilentlyContinue
    if ($cmd) { $iscc = $cmd.Source }
    if (-not $iscc) {
        $bases = @(${env:ProgramFiles(x86)}, $env:ProgramFiles, $env:LOCALAPPDATA) | Where-Object { $_ }
        $iscc = $bases |
            ForEach-Object { Join-Path $_ 'Inno Setup 6\ISCC.exe'; Join-Path $_ 'Programs\Inno Setup 6\ISCC.exe' } |
            Where-Object { Test-Path $_ } |
            Select-Object -First 1
    }
    if ($iscc) {
        $iss = Join-Path $root 'packaging\windows\hadion.iss'
        & $iscc "/DAppVersion=$version" "/DSourceDir=$stage" "/DOutputDir=$OutDir" "/DPlatform=$platform" $iss
        if ($LASTEXITCODE -ne 0) { throw "ISCC failed (exit $LASTEXITCODE)" }
        $setup = Join-Path $OutDir "$name-$platform-setup.exe"
        if (-not (Test-Path $setup)) { throw "ISCC did not produce $setup" }
        $assets += $setup
    }
    else {
        Write-Host 'Inno Setup is not installed, so no installer was built. To build one:'
        Write-Host '  winget install JRSoftware.InnoSetup   (or https://jrsoftware.org/isdl.php)'
        Write-Host '  then run this script again (it finds ISCC.exe on PATH or in Program Files).'
    }
}

# --- SHA256SUMS -------------------------------------------------------------------
$sums = Join-Path $OutDir 'SHA256SUMS'
Step "Checksums $sums"
$lines = foreach ($a in $assets) {
    '{0}  {1}' -f (Get-FileHash $a -Algorithm SHA256).Hash.ToLowerInvariant(), (Split-Path $a -Leaf)
}
# LF line endings, UTF-8 without BOM: what sha256sum -c and the updater expect.
[System.IO.File]::WriteAllText($sums, (($lines -join "`n") + "`n"), (New-Object System.Text.UTF8Encoding $false))
$lines | ForEach-Object { "  $_" }

Step 'Done'
foreach ($a in $assets + $sums) {
    '  {0,14:N0}  {1}' -f (Get-Item $a).Length, $a
}
Write-Host "`nRelease contract: tag v$version with $(Split-Path $zip -Leaf) and SHA256SUMS."
