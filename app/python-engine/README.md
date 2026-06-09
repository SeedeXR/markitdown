# Optional Python fallback engine

The Rust binary, MCP server and desktop app are fully self-contained and cover
every default markitdown format. This folder exists for the **long tail** the
pure-Rust engine intentionally does not bundle:

| Capability | Why Rust skips it | Python engine |
|---|---|---|
| OCR for scanned PDFs / images | no pure-Rust OCR; Tesseract would break the zero-dependency, phone-class footprint | via plugins (e.g. `markitdown-ocr`) |
| Audio transcription | needs cloud APIs or ~100MB+ local models | `markitdown[audio-transcription]` |
| Azure Document Intelligence / Content Understanding | cloud-only optional extras | `markitdown[all]` |
| Python plugins | Python-only mechanism | yes |

## Trade-offs (read before building)

- The PyInstaller one-file binary is **~80–200 MB** and has a **1–3 s cold
  start** (it self-extracts). The Rust binary is ~5–10 MB with ms startup.
- Image/audio **metadata** extraction in Python markitdown shells out to the
  system `exiftool` — that is *not* bundled. (The Rust engine has its own
  pure-Rust EXIF/tag readers, so this only matters for Python-engine runs.)
- Build per-OS; no cross-compilation.

## Prebuilt binaries

The release pipeline already publishes `markitdown-py-<platform>` binaries
(Linux x86_64/aarch64, Windows x86_64, macOS Apple Silicon) on the GitHub
Releases page — download one and point `MARKITDOWN_PY_BIN` at it instead of
building. Now built for Intel macOS too (on the `macos-13` runner, best-effort). Each
asset is `markitdown-py-<platform>.{tar.gz,zip}` — extract it and keep the
folder intact (see onedir note below).

## Build

```bash
./build_binary.sh        # macOS / Linux  (uses python3.12/3.11/3.10; needs 3.10–3.13)
# or on Windows:
pwsh ./build_binary.ps1
```

Produces a **onedir** folder `dist/markitdown-py/` whose launcher is
`dist/markitdown-py/markitdown-py` (`.exe` on Windows) — point
`MARKITDOWN_PY_BIN` / the desktop Settings field at that inner executable.

> **Why onedir (the default), not a single file?** A PyInstaller *one-file*
> binary re-extracts ~100 MB to a temp dir on every launch; on macOS that also
> triggers a Gatekeeper re-scan, costing **~45s of startup per run** and wiping
> out the engine's speed advantage. onedir extracts once, so startup is ~0.5s
> after the first run (the 342 MB test PDF then converts in ~7s vs ~57s in pure
> Rust). Use `BUILD_MODE=onefile ./build_binary.sh` for a single portable file
> if you accept the slow macOS cold start.

> **Python version:** the build requires Python 3.10–3.13 (defaults to 3.12 to
> match CI). A too-new interpreter (e.g. 3.14) lacks wheels for the native deps
> and the script will stop with guidance rather than hang.

## How the engine binary is found (so it works in installed apps)

The engine is located in this order — the later steps make a *packaged* app
work without the user setting any environment variable (important because
GUI apps launched from Finder/Start menu don't inherit your shell's env):

1. An explicit path — CLI `--python-bin`, or the **desktop Settings → Python
   engine** field (with a Browse button).
2. The `MARKITDOWN_PY_BIN` environment variable.
3. **Auto-discovery**: a binary named exactly `markitdown-py` (`markitdown-py.exe`
   on Windows) sitting **next to the running executable**, or anywhere on
   `PATH`. (Only that exact name is searched — never a bare `markitdown`, which
   is this suite's own Rust CLI.)

So, to make an installed app "just work" on Linux/macOS/Windows, do any one of:
- **Bundle it next to the app**: ship `markitdown-py` in the same directory as
  the `markitdown` / desktop executable (for Tauri, add it as an
  [external binary / sidecar](https://tauri.app/develop/sidecar/) named
  `markitdown-py`). Auto-discovery finds it.
- **Install it on `PATH`**: drop `markitdown-py` in `/usr/local/bin` (macOS/
  Linux) or a `PATH` dir (Windows).
- **Point at it**: set the desktop Settings field or `MARKITDOWN_PY_BIN`.

And — crucially — if none of these are present, **nothing breaks**: the default
`auto` engine just uses pure Rust and never errors. The "missing dependency"
message only appears if you *force* `--engine python` with no binary available.

## Enable manually

```bash
export MARKITDOWN_PY_BIN="$PWD/dist/markitdown-py"
```

- **CLI / MCP server / desktop app** all default to `auto` mode: Rust converts
  everything it can; the Python engine is invoked **only** when a converter
  reports a fidelity gap (scanned PDF → OCR, DOCX comments/equations, RTF-only
  `.msg` body, audio transcription, YouTube transcript, image OCR) or rejects
  the file outright. The Python output is used only if it adds content —
  otherwise the Rust result is kept.
- `markitdown --engine python file.pdf` forces Python for full fidelity
  (e.g. PDF table reconstruction). `--python-bin PATH` overrides the env var.
- A hung engine is killed after `MARKITDOWN_PY_TIMEOUT` seconds (default 300).
- The engine is invoked with `-p` (plugins enabled), so an OCR plugin baked
  into the binary (see the commented line in `build_binary.sh`) works
  automatically.
- Inputs are handed over at maximum fidelity: **http(s) URLs as URLs** (so the
  Python YouTube-transcript/Wikipedia/Bing converters fully activate), **local
  files as paths** (zero-copy), stdin bytes only as a last resort.
- `MARKITDOWN_PY_ARGS` appends extra args to every engine call — this is how
  you reach the Azure converters through the hybrid:
  `MARKITDOWN_PY_ARGS="-d -e https://<res>.cognitiveservices.azure.com/"`
  (Document Intelligence) or `MARKITDOWN_PY_ARGS="--use-cu --cu-endpoint …"`
  (Content Understanding).
- Converting many scanned files? Build with `BUILD_MODE=onedir` — cold start
  drops from ~1–3 s (onefile self-extraction) to ~50 ms.

Without `MARKITDOWN_PY_BIN`, everything still works — the fallback is simply
never attempted and costs nothing.
