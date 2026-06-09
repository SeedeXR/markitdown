#!/usr/bin/env bash
# Builds the OPTIONAL Python fallback engine: a self-contained PyInstaller
# binary of the original Python markitdown (with all optional extras).
#
# You do NOT need this for normal use — the Rust binary handles all default
# markitdown formats by itself. Build this only if you want the long tail:
# OCR for scanned documents (via plugins), audio transcription, Azure
# converters, or Python plugins.
#
# Usage:  ./build_binary.sh                    # DEFAULT: onedir (fast, ~0.5s startup)
#         BUILD_MODE=onefile ./build_binary.sh # single portable file (slow macOS start)
# Result: dist/markitdown-py/markitdown-py (onedir)  or  dist/markitdown-py (onefile)
# Point MARKITDOWN_PY_BIN (or the desktop Settings field) at the produced
# executable. For onedir, keep the whole dist/markitdown-py/ folder together.
set -euo pipefail
cd "$(dirname "$0")"

# Pick the interpreter. Honor an explicit $PYTHON; otherwise prefer a supported
# version (3.10–3.13) — and 3.12 first, to MATCH the GitHub workflow
# (.github/workflows/app-release.yml uses setup-python 3.12). Falling back to a
# bare `python3` risks a too-new build (e.g. 3.14 with no wheels), which the
# guard below then rejects with guidance.
if [ -n "${PYTHON:-}" ]; then
  PY="$PYTHON"
else
  PY="python3"
  for cand in python3.12 python3.11 python3.10 python3.13; do
    if command -v "$cand" >/dev/null 2>&1; then PY="$cand"; break; fi
  done
fi
# Default to ONEDIR. A one-file binary re-extracts ~100 MB to a temp dir on
# every launch — on macOS that triggers a Gatekeeper re-scan and ~45s startup
# *per run*, which destroys the engine's speed advantage. onedir extracts once
# (folder), so startup is ~0.5s after the first run. Set BUILD_MODE=onefile for
# a single portable file if you accept the slow macOS cold start.
MODE="${BUILD_MODE:-onedir}"

# Guard against a too-new interpreter: markitdown[all] pulls native deps
# (magika→onnxruntime, lxml, numpy) whose prebuilt wheels lag the newest
# Python by months. On an unsupported version pip tries to build from source
# and appears to hang. Require 3.10–3.13; tell the user how to pick another.
ver="$("$PY" -c 'import sys; print("%d.%d" % sys.version_info[:2])')"
major="${ver%.*}"; minor="${ver#*.}"
if [ "$major" -ne 3 ] || [ "$minor" -lt 10 ] || [ "$minor" -gt 13 ]; then
  echo "ERROR: Python $ver is not supported for this build (need 3.10–3.13)." >&2
  echo "       markitdown[all]'s native deps don't have wheels for $ver yet," >&2
  echo "       so pip would build from source and hang." >&2
  echo "       Re-run with a supported interpreter, e.g.:  PYTHON=python3.12 $0" >&2
  available="$(for v in python3.12 python3.11 python3.10 python3.13; do command -v "$v" >/dev/null 2>&1 && printf ' %s' "$v"; done)"
  [ -n "$available" ] && echo "       Found on PATH:$available" >&2
  exit 1
fi
echo "==> using $PY ($ver)"

echo "==> creating venv"
"$PY" -m venv .venv
# shellcheck disable=SC1091
source .venv/bin/activate

echo "==> installing markitdown[all] + pyinstaller"
pip install --quiet --upgrade pip
# markitdown[all] already pulls youtube-transcript-api via its
# youtube-transcription extra; install it explicitly too so the YouTube
# transcript fallback can't silently disappear if the extra is renamed.
pip install --quiet pyinstaller "markitdown[all]" youtube-transcript-api
# Optional: local OCR plugin from this repo (uncomment to include):
# pip install --quiet ../../packages/markitdown-ocr

echo "==> writing entry point"
cat > _entry.py <<'EOF'
from markitdown.__main__ import main

if __name__ == "__main__":
    main()
EOF

echo "==> building $MODE binary (this takes a few minutes)"
pyinstaller "--$MODE" --name markitdown-py \
    --collect-all magika \
    --collect-data charset_normalizer \
    --copy-metadata markitdown \
    _entry.py

echo
if [ "$MODE" = "onedir" ]; then
    BIN="$(pwd)/dist/markitdown-py/markitdown-py"
else
    BIN="$(pwd)/dist/markitdown-py"
fi
echo "Built: $BIN ($(du -sh "$(dirname "$BIN")" | cut -f1) total)"
echo
echo "Enable it for the Rust tools with:"
echo "  export MARKITDOWN_PY_BIN=$BIN"
echo "or pass --engine python / --python-bin to the markitdown CLI."
echo "(--engine auto is the default: Rust converts everything it can; the"
echo " Python engine is only invoked for fidelity gaps like OCR.)"
