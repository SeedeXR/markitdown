#Requires -Version 5.1
<#
.SYNOPSIS
  Build the whole markitdown suite on Windows. (macOS/Linux: build-all.sh)

.DESCRIPTION
  Builds, in one go:
    * the CLI binary        -> target\release\markitdown.exe
    * the MCP server binary -> target\release\markitdown-mcp.exe
    * the Tauri desktop app -> desktop\src-tauri\target\release\bundle\...

.EXAMPLE
  .\scripts\build-all.ps1                 # binaries AND the desktop app
  .\scripts\build-all.ps1 -McpOnly        # just the CLI + MCP server
  .\scripts\build-all.ps1 -DesktopOnly    # just the desktop app
  .\scripts\build-all.ps1 -Pdfium         # binaries with the bundled fast PDFium backend
  .\scripts\build-all.ps1 -Debug          # debug profile
  .\scripts\build-all.ps1 -Install        # after building, register the MCP with Claude
#>
param(
  [switch]$McpOnly,
  [switch]$DesktopOnly,
  [switch]$Pdfium,
  [switch]$Debug,
  [switch]$Install
)

$ErrorActionPreference = "Stop"

function Ok($m)   { Write-Host "[ ok ] $m" -ForegroundColor Green }
function Warn($m) { Write-Host "[warn] $m" -ForegroundColor Yellow }
function Fail($m) { Write-Host "[fail] $m" -ForegroundColor Red }
function Step($m) { Write-Host ""; Write-Host "==> $m" -ForegroundColor Cyan }

$ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$AppDir = (Resolve-Path (Join-Path $ScriptDir "..")).Path
Set-Location $AppDir

if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
  Fail "cargo not found — install Rust from https://rustup.rs"; exit 1
}

$buildMcp = -not $DesktopOnly
$buildDesktop = -not $McpOnly
$profile = if ($Debug) { "debug" } else { "release" }
$profileArgs = if ($Debug) { @() } else { @("--release") }
$featureArgs = if ($Pdfium) { @("--features", "pdfium") } else { @() }

# ---- 1. CLI + MCP server binaries -----------------------------------------
if ($buildMcp) {
  Step "Building CLI + MCP server ($profile$(if ($Pdfium) {', features: pdfium'}))"
  cargo build @profileArgs @featureArgs -p markitdown-cli -p markitdown-mcp
  if ($LASTEXITCODE -ne 0) { Fail "cargo build failed"; exit 1 }
  $binDir = Join-Path $AppDir "target\$profile"
  Ok "markitdown.exe      -> $binDir\markitdown.exe"
  Ok "markitdown-mcp.exe  -> $binDir\markitdown-mcp.exe"
}

# ---- 2. Tauri desktop app -------------------------------------------------
if ($buildDesktop) {
  if (-not (Get-Command npm -ErrorAction SilentlyContinue)) {
    Fail "npm not found — install Node.js 18+ to build the desktop app"; exit 1
  }
  Step "Building the desktop app (Tauri)"
  Push-Location (Join-Path $AppDir "desktop")
  try {
    if (-not (Test-Path "node_modules")) {
      Step "Installing frontend dependencies (npm ci)"
      npm ci
    }
    if ($Debug) { npm run tauri build -- --debug } else { npm run tauri build }
    if ($LASTEXITCODE -ne 0) { Fail "tauri build failed"; exit 1 }
  } finally { Pop-Location }
  $bundle = Join-Path $AppDir "desktop\src-tauri\target\$profile\bundle"
  Ok "desktop bundles -> $bundle"
  if (Test-Path $bundle) {
    Get-ChildItem -Path $bundle -Recurse -Include *.msi, *.exe -ErrorAction SilentlyContinue |
      ForEach-Object { Write-Host "         $($_.FullName)" }
  }
}

# ---- 3. optional: register the MCP server with Claude ---------------------
if ($Install -and $buildMcp) {
  Step "Registering the MCP server with Claude (install-mcp.ps1)"
  & (Join-Path $ScriptDir "install-mcp.ps1") -Bin (Join-Path $AppDir "target\$profile\markitdown-mcp.exe")
}

Step "Done"
Ok "Build complete."
if ($buildMcp -and -not $Install) {
  Write-Host "   Connect the MCP server to Claude with:  .\scripts\install-mcp.ps1"
}
