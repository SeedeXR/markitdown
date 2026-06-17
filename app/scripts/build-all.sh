#!/usr/bin/env bash
# Build the whole markitdown suite for macOS / Linux. (Windows: build-all.ps1)
#
# Builds, in one go:
#   * the CLI binary        -> target/release/markitdown
#   * the MCP server binary -> target/release/markitdown-mcp
#   * the Tauri desktop app -> desktop/src-tauri/target/release/bundle/...
#
# Usage:
#   ./scripts/build-all.sh                # build binaries AND the desktop app
#   ./scripts/build-all.sh --mcp-only     # just the CLI + MCP server binaries
#   ./scripts/build-all.sh --desktop-only # just the desktop app
#   ./scripts/build-all.sh --pdfium       # build binaries with the bundled fast PDFium backend
#   ./scripts/build-all.sh --debug        # debug profile (faster compile, slower runtime)
#   ./scripts/build-all.sh --install      # after building, register the MCP with Claude (install-mcp.sh)
#
# Flags combine, e.g.  ./scripts/build-all.sh --pdfium --install

set -euo pipefail

BUILD_MCP=1
BUILD_DESKTOP=1
FEATURES=""          # comma-separated cargo features (avoid arrays for bash 3.2)
PROFILE="release"
PROFILE_FLAG="--release"
DO_INSTALL=0

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

usage() { sed -n '2,18p' "$0" | sed 's/^#\{0,1\} \{0,1\}//'; }

while [ $# -gt 0 ]; do
  case "$1" in
    --mcp-only)     BUILD_DESKTOP=0; shift ;;
    --desktop-only) BUILD_MCP=0; shift ;;
    --pdfium)       FEATURES="${FEATURES:+$FEATURES,}pdfium"; shift ;;
    --debug)        PROFILE="debug"; PROFILE_FLAG=""; shift ;;
    --install)      DO_INSTALL=1; shift ;;
    -h|--help)      usage; exit 0 ;;
    *) err "unknown argument: $1"; usage; exit 2 ;;
  esac
done

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)"
APP_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$APP_DIR"

command -v cargo >/dev/null 2>&1 || { err "cargo not found — install Rust from https://rustup.rs"; exit 1; }

EXE=""
case "$(uname -s)" in MINGW*|MSYS*|CYGWIN*) EXE=".exe" ;; esac

# ---- 1. CLI + MCP server binaries -----------------------------------------
if [ "$BUILD_MCP" -eq 1 ]; then
  step "Building CLI + MCP server ($PROFILE${FEATURES:+, features: $FEATURES})"
  # $PROFILE_FLAG is empty for --debug; an unquoted empty var vanishes. Pass
  # --features only when set, to avoid a stray empty argument.
  if [ -n "$FEATURES" ]; then
    cargo build $PROFILE_FLAG --features "$FEATURES" -p markitdown-cli -p markitdown-mcp
  else
    cargo build $PROFILE_FLAG -p markitdown-cli -p markitdown-mcp
  fi
  BIN_DIR="$APP_DIR/target/$PROFILE"
  ok "markitdown      -> $BIN_DIR/markitdown$EXE"
  ok "markitdown-mcp  -> $BIN_DIR/markitdown-mcp$EXE"
fi

# ---- 2. Tauri desktop app -------------------------------------------------
if [ "$BUILD_DESKTOP" -eq 1 ]; then
  command -v npm >/dev/null 2>&1 || { err "npm not found — install Node.js 18+ to build the desktop app"; exit 1; }
  step "Building the desktop app (Tauri)"
  cd "$APP_DIR/desktop"
  if [ ! -d node_modules ]; then
    step "Installing frontend dependencies (npm ci)"
    npm ci
  fi
  if [ "$PROFILE" = "debug" ]; then
    npm run tauri build -- --debug
  else
    npm run tauri build
  fi
  cd "$APP_DIR"
  BUNDLE="$APP_DIR/desktop/src-tauri/target/$PROFILE/bundle"
  ok "desktop bundles -> $BUNDLE"
  if [ -d "$BUNDLE" ]; then
    find "$BUNDLE" -maxdepth 2 \( -name '*.dmg' -o -name '*.app' -o -name '*.AppImage' \
      -o -name '*.deb' -o -name '*.rpm' -o -name '*.msi' -o -name '*.exe' \) 2>/dev/null \
      | sed 's/^/         /'
  fi
fi

# ---- 3. optional: register the MCP server with Claude ---------------------
if [ "$DO_INSTALL" -eq 1 ] && [ "$BUILD_MCP" -eq 1 ]; then
  step "Registering the MCP server with Claude (install-mcp.sh)"
  "$SCRIPT_DIR/install-mcp.sh" --bin "$APP_DIR/target/$PROFILE/markitdown-mcp$EXE"
fi

step "Done"
ok "Build complete."
if [ "$BUILD_MCP" -eq 1 ] && [ "$DO_INSTALL" -eq 0 ]; then
  echo "   Connect the MCP server to Claude with:  ./scripts/install-mcp.sh"
fi
