<#
.SYNOPSIS
  Builds rgit.exe and the Windows desktop app installers (NSIS and MSI).

.DESCRIPTION
  1. builds the engine CLI (rgit.exe) in release mode,
  2. copies it next to the app as the Tauri sidecar (src-tauri/binaries/rgit-<triple>.exe),
  3. installs the UI dependencies, runs the UI type check, unit tests and Vite build,
  4. runs `tauri build` to produce the NSIS (.exe) and/or MSI installers,
  5. copies the installers to <repo>\dist\windows and writes SHA256SUMS.

  Run it on Windows 10/11 from any directory. It has NOT been run on Windows by its author
  (written on a Mac); the GitHub Actions workflow .github/workflows/windows.yml is the first
  real execution. See docs/windows.md for prerequisites.

.PARAMETER Bundles
  nsis, msi or all (default).

.PARAMETER SkipTests
  Skip the UI unit tests and `cargo test` of the app shell.

.PARAMETER CertThumbprint
  SHA-1 thumbprint of a code-signing certificate in the Windows certificate store. When given,
  the sidecar rgit.exe, the app executable and the installers are signed with signtool
  (SHA-256, RFC 3161 timestamp). Without it the output is UNSIGNED.

.PARAMETER TimestampUrl
  RFC 3161 timestamp server used when signing.

.EXAMPLE
  .\scripts\windows\build.ps1
  .\scripts\windows\build.ps1 -Bundles nsis -SkipTests
  .\scripts\windows\build.ps1 -CertThumbprint 0123456789ABCDEF0123456789ABCDEF01234567
#>
[CmdletBinding()]
param(
  [ValidateSet('nsis', 'msi', 'all')][string]$Bundles = 'all',
  [switch]$SkipTests,
  [string]$CertThumbprint = '',
  [string]$TimestampUrl = 'http://timestamp.digicert.com'
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$RepoRoot = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
$AppDir = Join-Path $RepoRoot 'apps\windows'
$TauriDir = Join-Path $AppDir 'src-tauri'
$OutDir = Join-Path $RepoRoot 'dist\windows'

function Step($text) { Write-Host "`n==> $text" -ForegroundColor Cyan }

function Require($name, $hint) {
  if (-not (Get-Command $name -ErrorAction SilentlyContinue)) { throw "$name not found. $hint" }
}

function Invoke-Checked([string]$what, [scriptblock]$block) {
  & $block
  if ($LASTEXITCODE -ne 0) { throw "$what failed with exit code $LASTEXITCODE" }
}

function Find-SignTool {
  $cmd = Get-Command signtool.exe -ErrorAction SilentlyContinue
  if ($cmd) { return $cmd.Source }
  $kits = Join-Path ${env:ProgramFiles(x86)} 'Windows Kits\10\bin'
  $found = Get-ChildItem -Path $kits -Filter signtool.exe -Recurse -ErrorAction SilentlyContinue |
    Where-Object { $_.FullName -match '\\x64\\' } | Sort-Object FullName -Descending | Select-Object -First 1
  if ($found) { return $found.FullName }
  throw 'signtool.exe not found. Install the Windows SDK (signing tools) or add signtool to PATH.'
}

function Sign-File([string]$path) {
  Invoke-Checked "signtool sign $path" {
    & $script:SignTool sign /sha1 $CertThumbprint /fd SHA256 /tr $TimestampUrl /td SHA256 $path
  }
}

Step 'Checking prerequisites'
Require cargo 'Install Rust (rustup) with the MSVC toolchain: https://rustup.rs'
Require node 'Install Node.js 20 or newer.'
Require npm 'Install Node.js 20 or newer.'
$hostLine = (& rustc -vV | Select-String '^host: (.+)$')
if (-not $hostLine) { throw 'Cannot read the Rust host triple from rustc -vV' }
$Triple = $hostLine.Matches[0].Groups[1].Value
if ($Triple -notlike '*windows*') { throw "This script builds the Windows app and must run on Windows (rustc host is $Triple)." }
Write-Host "Rust host triple: $Triple"
# WebView2 runtime is needed to RUN the app (the installer bootstraps it); warn if it is missing here.
$wv2 = 'HKLM:\SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}'
if (-not (Test-Path $wv2)) { Write-Warning 'WebView2 runtime not detected on this machine (needed to run the app, not to build it).' }

if ($CertThumbprint) {
  $script:SignTool = Find-SignTool
  Write-Host "Signing enabled with certificate $CertThumbprint using $script:SignTool"
} else {
  Write-Warning 'No -CertThumbprint given: the installers will be UNSIGNED (SmartScreen will warn).'
}

Step 'Building rgit.exe (release)'
Push-Location $RepoRoot
try { Invoke-Checked 'cargo build rgit' { cargo build --release -p rustgit-cli --features iroh } } finally { Pop-Location }
$Rgit = Join-Path $RepoRoot 'target\release\rgit.exe'
if (-not (Test-Path $Rgit)) { throw "rgit.exe was not produced at $Rgit" }
$ver = & $Rgit --json version
if ($LASTEXITCODE -ne 0 -or $ver -notmatch '"json_interface":1') { throw "rgit.exe does not speak JSON interface 1: $ver" }
Write-Host $ver

if ($CertThumbprint) { Sign-File $Rgit }

Step 'Installing UI dependencies'
Push-Location $AppDir
try {
  Invoke-Checked 'npm ci' { npm ci }

  Step 'Staging the sidecar'
  Invoke-Checked 'stage-sidecar' { node scripts/stage-sidecar.mjs --target $Triple --from $Rgit }

  if (-not $SkipTests) {
    Step 'UI type check and unit tests (including the live test against the real rgit.exe)'
    $env:RGIT_BIN = $Rgit
    Invoke-Checked 'npm run typecheck' { npm run typecheck }
    Invoke-Checked 'npm test' { npm test }
    Step 'Tests of the Tauri shell'
    Push-Location $TauriDir
    try { Invoke-Checked 'cargo test (app shell)' { cargo test } } finally { Pop-Location }
  }

  Step 'Building the installers with tauri build'
  $bundleArg = if ($Bundles -eq 'all') { 'nsis,msi' } else { $Bundles }
  $tauriArgs = @('run', 'tauri', '--', 'build', '--bundles', $bundleArg)
  if ($CertThumbprint) {
    $override = Join-Path $env:TEMP 'rustgit-tauri-sign.json'
    $json = @{ bundle = @{ windows = @{ certificateThumbprint = $CertThumbprint; digestAlgorithm = 'sha256'; timestampUrl = $TimestampUrl } } } | ConvertTo-Json -Depth 5
    Set-Content -Path $override -Value $json -Encoding UTF8
    $tauriArgs += @('--config', $override)
  }
  Invoke-Checked 'tauri build' { npm @tauriArgs }
} finally { Pop-Location }

Step 'Collecting installers'
New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
$bundleRoot = Join-Path $TauriDir 'target\release\bundle'
$installers = @(Get-ChildItem -Path $bundleRoot -Recurse -Include '*.exe', '*.msi' |
  Where-Object { $_.FullName -match '\\(nsis|msi)\\' })
if ($installers.Count -eq 0) { throw "No installers found under $bundleRoot" }
foreach ($f in $installers) { Copy-Item $f.FullName -Destination $OutDir -Force }
$sums = foreach ($f in (Get-ChildItem -Path $OutDir -File | Where-Object { $_.Name -ne 'SHA256SUMS' })) {
  '{0}  {1}' -f (Get-FileHash -Algorithm SHA256 -Path $f.FullName).Hash.ToLower(), $f.Name
}
Set-Content -Path (Join-Path $OutDir 'SHA256SUMS') -Value $sums -Encoding ASCII

Step 'Done'
Get-ChildItem $OutDir | Format-Table Name, Length -AutoSize
if ($CertThumbprint) {
  foreach ($f in (Get-ChildItem $OutDir -File | Where-Object { $_.Extension -in '.exe', '.msi' })) {
    $sig = Get-AuthenticodeSignature $f.FullName
    Write-Host ("{0}: signature status {1}" -f $f.Name, $sig.Status)
  }
} else {
  Write-Warning 'Installers are UNSIGNED. They have not been installed or run on a clean Windows machine by this script.'
}
