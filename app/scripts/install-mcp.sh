#!/usr/bin/env bash
# markitdown-mcp installer for macOS + Linux. (Windows: install-mcp.ps1)
#
# Registers the markitdown MCP server with Claude Desktop AND Claude Code,
# runs a JSON-RPC smoke test against the binary, and reports connection status.
# Idempotent — safe to re-run.
#
# Usage:
#   ./install-mcp.sh [--bin /path/to/markitdown-mcp]
#                    [--python-bin /path/to/markitdown-py]   # optional OCR/transcription fallback
#                    [--build]                               # build from source if needed
#                    [--no-skill]                            # skip installing the global skill
#
# With no --bin it looks (1) next to this script (the release archive layout),
# then (2) in the repo's target/release, then (3) builds it if cargo is present.

set -euo pipefail

SERVER=markitdown
MCP_BIN=""
PY_BIN=""
DO_BUILD=0
INSTALL_SKILL=1

# ---- pretty output --------------------------------------------------------
if [ -t 1 ]; then
  B=$'\033[1m'; G=$'\033[32m'; Y=$'\033[33m'; R=$'\033[31m'; C=$'\033[36m'; N=$'\033[0m'
else
  B=""; G=""; Y=""; R=""; C=""; N=""
fi
ok()   { printf '%s[ ok ]%s %s\n' "$G" "$N" "$*"; }
warn() { printf '%s[warn]%s %s\n' "$Y" "$N" "$*"; }
err()  { printf '%s[fail]%s %s\n' "$R" "$N" "$*" >&2; }
step() { printf '\n%s==>%s %s%s%s\n' "$C" "$N" "$B" "$*" "$N"; }

usage() {
  sed -n '2,18p' "$0" | sed 's/^#\{0,1\} \{0,1\}//'
}

while [ $# -gt 0 ]; do
  case "$1" in
    --bin)        MCP_BIN="${2:?--bin needs a path}"; shift 2 ;;
    --python-bin) PY_BIN="${2:?--python-bin needs a path}"; shift 2 ;;
    --build)      DO_BUILD=1; shift ;;
    --no-skill)   INSTALL_SKILL=0; shift ;;
    -h|--help)    usage; exit 0 ;;
    *) err "unknown argument: $1"; usage; exit 2 ;;
  esac
done

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)"
abspath() { (cd "$(dirname "$1")" && printf '%s/%s\n' "$(pwd)" "$(basename "$1")"); }

# ---- 1. locate (or build) the binary --------------------------------------
step "Locating markitdown-mcp"
if [ -z "$MCP_BIN" ]; then
  if [ -x "$SCRIPT_DIR/markitdown-mcp" ]; then
    MCP_BIN="$SCRIPT_DIR/markitdown-mcp"
  elif [ -x "$SCRIPT_DIR/../target/release/markitdown-mcp" ]; then
    MCP_BIN="$SCRIPT_DIR/../target/release/markitdown-mcp"
  fi
fi
if { [ -z "$MCP_BIN" ] || [ ! -x "$MCP_BIN" ]; } \
   && { [ "$DO_BUILD" -eq 1 ] || [ -z "$MCP_BIN" ]; } \
   && command -v cargo >/dev/null 2>&1 && [ -f "$SCRIPT_DIR/../Cargo.toml" ]; then
  step "Building markitdown-mcp (cargo build --release)"
  ( cd "$SCRIPT_DIR/.." && cargo build --release -p markitdown-mcp )
  MCP_BIN="$SCRIPT_DIR/../target/release/markitdown-mcp"
fi
if [ -z "$MCP_BIN" ] || [ ! -x "$MCP_BIN" ]; then
  err "could not find markitdown-mcp. Pass --bin /path/to/markitdown-mcp, or run with --build."
  exit 1
fi
MCP_BIN="$(abspath "$MCP_BIN")"
ok "binary: $MCP_BIN"

# ---- 2. smoke-test the binary (JSON-RPC over stdio) -----------------------
step "Smoke-testing the server"
smoke_out="$(printf '%s\n%s\n%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"installer","version":"0"}}}' \
  '{"jsonrpc":"2.0","method":"notifications/initialized","params":{}}' \
  '{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}' \
  | "$MCP_BIN" 2>/dev/null || true)"
missing=0
for tool in convert_to_markdown convert_file convert_batch list_supported_formats; do
  case "$smoke_out" in
    *"$tool"*) ;;
    *) err "tool '$tool' was not advertised by the server"; missing=1 ;;
  esac
done
[ "$missing" -eq 0 ] || { err "smoke test failed — the binary did not advertise the expected tools"; exit 1; }
ok "server starts and advertises all 4 tools"

# ---- 3. register with Claude Desktop (merge config JSON, never clobber) ----
step "Registering with Claude Desktop"
case "$(uname -s)" in
  Darwin) DESKTOP_DIR="$HOME/Library/Application Support/Claude" ;;
  Linux)  DESKTOP_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/Claude" ;;
  *)      DESKTOP_DIR="" ;;
esac
PYTHON="$(command -v python3 || command -v python || true)"
if [ -z "$DESKTOP_DIR" ]; then
  warn "unrecognized OS for Claude Desktop; skipping (Claude Code step still runs)"
elif [ -z "$PYTHON" ]; then
  warn "python3 not found — can't safely merge JSON. Add this manually to:"
  warn "  $DESKTOP_DIR/claude_desktop_config.json"
  printf '  "mcpServers": { "%s": { "command": "%s"%s } }\n' "$SERVER" "$MCP_BIN" \
    "${PY_BIN:+, \"env\": { \"MARKITDOWN_PY_BIN\": \"$PY_BIN\" }}"
else
  CONFIG="$DESKTOP_DIR/claude_desktop_config.json"
  mkdir -p "$DESKTOP_DIR"
  if [ -f "$CONFIG" ]; then cp "$CONFIG" "$CONFIG.bak" && ok "backed up existing config → $CONFIG.bak"; fi
  MARKITDOWN_BIN="$MCP_BIN" MARKITDOWN_PYBIN="$PY_BIN" "$PYTHON" - "$CONFIG" "$SERVER" <<'PYEOF'
import json, os, sys
config, server = sys.argv[1], sys.argv[2]
bin_path = os.environ["MARKITDOWN_BIN"]
py_bin = os.environ.get("MARKITDOWN_PYBIN") or ""
data = {}
if os.path.exists(config):
    try:
        with open(config, encoding="utf-8") as f:
            data = json.load(f)
        if not isinstance(data, dict):
            data = {}
    except (ValueError, OSError):
        data = {}  # invalid/unreadable — the .bak backup preserved the original
servers = data.setdefault("mcpServers", {})
entry = {"command": bin_path}
if py_bin:
    entry["env"] = {"MARKITDOWN_PY_BIN": py_bin}
servers[server] = entry
with open(config, "w", encoding="utf-8") as f:
    json.dump(data, f, indent=2)
    f.write("\n")
PYEOF
  ok "wrote $CONFIG"
  warn "restart Claude Desktop to load the new server"
fi

# ---- 4. register with Claude Code (claude CLI, user scope = all projects) --
step "Registering with Claude Code"
if command -v claude >/dev/null 2>&1; then
  claude mcp remove "$SERVER" -s user  >/dev/null 2>&1 || true
  claude mcp remove "$SERVER" -s local >/dev/null 2>&1 || true
  if [ -n "$PY_BIN" ]; then
    claude mcp add "$SERVER" -s user -e MARKITDOWN_PY_BIN="$PY_BIN" -- "$MCP_BIN" >/dev/null
  else
    claude mcp add "$SERVER" -s user -- "$MCP_BIN" >/dev/null
  fi
  ok "added to Claude Code at user scope (available in all projects)"
  if claude mcp get "$SERVER" 2>&1 | grep -qiE 'connected|✔'; then
    ok "Claude Code reports the server as Connected"
  else
    warn "added, but not reported Connected yet — Claude Code connects on first use"
  fi
else
  warn "the 'claude' CLI was not found on PATH; skipping Claude Code registration."
  warn "after installing Claude Code, run:  claude mcp add $SERVER -s user -- \"$MCP_BIN\""
fi

# ---- 5. install the companion skill globally (Claude Code) ----------------
if [ "$INSTALL_SKILL" -eq 1 ]; then
  SKILL_SRC="$SCRIPT_DIR/../skill/markitdown/SKILL.md"
  [ -f "$SKILL_SRC" ] || SKILL_SRC="$SCRIPT_DIR/SKILL.md"  # release-archive layout
  if [ -f "$SKILL_SRC" ]; then
    step "Installing the companion skill (Claude Code, all projects)"
    DEST="$HOME/.claude/skills/markitdown"
    mkdir -p "$DEST"
    cp "$SKILL_SRC" "$DEST/SKILL.md"
    ok "skill → $DEST/SKILL.md"
  fi
fi

step "Done"
ok "markitdown MCP installed. Claude Code is ready now; restart Claude Desktop to pick it up."
