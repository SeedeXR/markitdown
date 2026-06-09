#Requires -Version 5.1
<#
.SYNOPSIS
  markitdown-mcp installer for Windows. (macOS/Linux: install-mcp.sh)

.DESCRIPTION
  Registers the markitdown MCP server with Claude Desktop AND Claude Code,
  runs a JSON-RPC smoke test against the binary, and reports connection
  status. Idempotent — safe to re-run.

  With no -Bin it looks (1) next to this script (the release archive layout),
  then (2) in the repo's target\release, then (3) builds it if cargo is present.

.EXAMPLE
  .\install-mcp.ps1
  .\install-mcp.ps1 -Bin C:\tools\markitdown-mcp.exe -PythonBin C:\tools\markitdown-py\markitdown-py.exe
  .\install-mcp.ps1 -Build
#>
param(
  [string]$Bin = "",
  [string]$PythonBin = "",
  [switch]$Build,
  [switch]$NoSkill
)

$ErrorActionPreference = "Stop"
$Server = "markitdown"

function Ok($m)   { Write-Host "[ ok ] $m" -ForegroundColor Green }
function Warn($m) { Write-Host "[warn] $m" -ForegroundColor Yellow }
function Fail($m) { Write-Host "[fail] $m" -ForegroundColor Red }
function Step($m) { Write-Host ""; Write-Host "==> $m" -ForegroundColor Cyan }

$ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path

# ---- 1. locate (or build) the binary --------------------------------------
Step "Locating markitdown-mcp.exe"
if (-not $Bin) {
  $cands = @(
    (Join-Path $ScriptDir "markitdown-mcp.exe"),
    (Join-Path $ScriptDir "..\target\release\markitdown-mcp.exe")
  )
  foreach ($c in $cands) { if (Test-Path $c) { $Bin = (Resolve-Path $c).Path; break } }
}
if ((-not $Bin -or -not (Test-Path $Bin)) -and ($Build -or (Get-Command cargo -ErrorAction SilentlyContinue))) {
  if (Test-Path (Join-Path $ScriptDir "..\Cargo.toml")) {
    Step "Building markitdown-mcp (cargo build --release)"
    Push-Location (Join-Path $ScriptDir "..")
    try { cargo build --release -p markitdown-mcp } finally { Pop-Location }
    $built = Join-Path $ScriptDir "..\target\release\markitdown-mcp.exe"
    if (Test-Path $built) { $Bin = (Resolve-Path $built).Path }
  }
}
if (-not $Bin -or -not (Test-Path $Bin)) {
  Fail "could not find markitdown-mcp.exe. Pass -Bin <path>, or use -Build."
  exit 1
}
$Bin = (Resolve-Path $Bin).Path
Ok "binary: $Bin"

# ---- 2. smoke-test the binary (JSON-RPC over stdio) -----------------------
Step "Smoke-testing the server"
$msgs = @(
  '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"installer","version":"0"}}}',
  '{"jsonrpc":"2.0","method":"notifications/initialized","params":{}}',
  '{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}'
) -join "`n"
$smoke = ($msgs | & $Bin 2>$null | Out-String)
$missing = $false
foreach ($t in @("convert_to_markdown","convert_file","convert_batch","list_supported_formats")) {
  if ($smoke -notmatch [regex]::Escape($t)) { Fail "tool '$t' was not advertised by the server"; $missing = $true }
}
if ($missing) { Fail "smoke test failed — the binary did not advertise the expected tools"; exit 1 }
Ok "server starts and advertises all 4 tools"

# ---- 3. register with Claude Desktop (merge config JSON, never clobber) ----
Step "Registering with Claude Desktop"
$DesktopDir = Join-Path $env:APPDATA "Claude"
$Config = Join-Path $DesktopDir "claude_desktop_config.json"
New-Item -ItemType Directory -Force -Path $DesktopDir | Out-Null
if (Test-Path $Config) { Copy-Item $Config "$Config.bak" -Force; Ok "backed up existing config -> $Config.bak" }

$data = $null
if (Test-Path $Config) {
  $raw = Get-Content -Raw -Path $Config -ErrorAction SilentlyContinue
  if ($raw -and $raw.Trim()) {
    try { $data = $raw | ConvertFrom-Json } catch { Warn "existing config was not valid JSON; backup kept, writing fresh"; $data = $null }
  }
}
if (-not $data) { $data = [PSCustomObject]@{} }
if (-not ($data.PSObject.Properties.Name -contains "mcpServers")) {
  $data | Add-Member -NotePropertyName mcpServers -NotePropertyValue ([PSCustomObject]@{})
}
$entry = [ordered]@{ command = $Bin }
if ($PythonBin) { $entry["env"] = @{ MARKITDOWN_PY_BIN = $PythonBin } }
$entryObj = [PSCustomObject]$entry
if ($data.mcpServers.PSObject.Properties.Name -contains $Server) {
  $data.mcpServers.$Server = $entryObj
} else {
  $data.mcpServers | Add-Member -NotePropertyName $Server -NotePropertyValue $entryObj
}
# UTF-8 without BOM so Claude Desktop parses it cleanly.
[System.IO.File]::WriteAllText($Config, (($data | ConvertTo-Json -Depth 12)))
Ok "wrote $Config"
Warn "restart Claude Desktop to load the new server"

# ---- 4. register with Claude Code (claude CLI, user scope = all projects) --
Step "Registering with Claude Code"
if (Get-Command claude -ErrorAction SilentlyContinue) {
  & claude mcp remove $Server -s user  2>$null | Out-Null
  & claude mcp remove $Server -s local 2>$null | Out-Null
  if ($PythonBin) {
    & claude mcp add $Server -s user -e "MARKITDOWN_PY_BIN=$PythonBin" -- $Bin | Out-Null
  } else {
    & claude mcp add $Server -s user -- $Bin | Out-Null
  }
  Ok "added to Claude Code at user scope (available in all projects)"
  $status = (& claude mcp get $Server 2>&1 | Out-String)
  if ($status -match "(?i)connected") { Ok "Claude Code reports the server as Connected" }
  else { Warn "added, but not reported Connected yet — Claude Code connects on first use" }
} else {
  Warn "the 'claude' CLI was not found on PATH; skipping Claude Code registration."
  Warn "after installing Claude Code, run:  claude mcp add $Server -s user -- `"$Bin`""
}

# ---- 5. install the companion skill globally (Claude Code) ----------------
if (-not $NoSkill) {
  $SkillSrc = Join-Path $ScriptDir "..\skill\markitdown\SKILL.md"
  if (-not (Test-Path $SkillSrc)) { $SkillSrc = Join-Path $ScriptDir "SKILL.md" }
  if (Test-Path $SkillSrc) {
    Step "Installing the companion skill (Claude Code, all projects)"
    $Dest = Join-Path $env:USERPROFILE ".claude\skills\markitdown"
    New-Item -ItemType Directory -Force -Path $Dest | Out-Null
    Copy-Item $SkillSrc (Join-Path $Dest "SKILL.md") -Force
    Ok "skill -> $Dest\SKILL.md"
  }
}

Step "Done"
Ok "markitdown MCP installed. Claude Code is ready now; restart Claude Desktop to pick it up."
