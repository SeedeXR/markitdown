# Changelog — MarkItDown Rust suite (`app/`)

All notable changes to the Rust suite (core engine, CLI, MCP server, desktop
app, and the optional Python fallback). Format loosely follows
[Keep a Changelog](https://keepachangelog.com/); the suite is pre-1.0.

## [Unreleased]

### Added
- **One-command MCP installer** (`scripts/install-mcp.{sh,ps1}`), shipped inside
  every release archive. It locates (or builds) `markitdown-mcp`, runs a
  JSON-RPC smoke test (asserts all 4 tools), registers the server with **Claude
  Desktop** (per-OS config path: `~/Library/Application Support/Claude` on macOS,
  `~/.config/Claude` on Linux, `%APPDATA%\Claude` on Windows — merged, never
  clobbered, with a `.bak`), and **Claude Code** at **user scope** (every
  project) via the `claude` CLI, then installs the companion skill to
  `~/.claude/skills/markitdown/` and reports connection status. Idempotent;
  flags for a custom binary, the optional Python fallback, build-from-source,
  and skipping the skill. Windows script is PowerShell 5.1-compatible.
- **Native YouTube transcripts (no Python needed).** The YouTube converter now
  fetches the video transcript itself, the same way
  [`youtube-transcript-api`](https://github.com/jdepoix/youtube-transcript-api)
  does: it reads `captionTracks[]` out of the embedded `ytInitialPlayerResponse`,
  picks a track (preferring an English, manually-created one over auto-generated),
  GETs that track's timedtext endpoint, and parses the `<text>` lines into a
  `### Transcript` section (double-encoded entities decoded). Requires the `net`
  feature (on by default); when captions are absent or the build is offline the
  result is flagged degraded so the Python engine can try. The Python fallback
  binary now installs `youtube-transcript-api` explicitly (on top of
  `markitdown[all]`) so the transcript path can't silently disappear.
- **MHTML (saved-webpage, `.mhtml`/`.mht`) support.** Instead of dumping the raw
  MIME container (quoted-printable/base64 and all) as plain text, the converter
  parses the multipart message, decodes the root `text/html` part (QP/base64 +
  charset), and runs it through the shared HTML→Markdown pipeline — embedded
  image/CSS parts are dropped. Verified on a real saved Wikipedia article.
- **Generic structured-XML (`.xml`) support.** Non-feed XML (RSS/Atom is still
  handled first by the feed converter) now renders meaningfully: a parent with
  ≥2 repeating same-tag records → a GFM table (columns = union of field tags +
  `@attributes`); anything else → a readable nested outline (`- **tag**
  (attrs): text`). Verified on a record-style catalog (→ table) and a nested
  config (→ outline).
- **ODS (OpenDocument spreadsheet) support** via `calamine` — each sheet → GFM
  table. Notably the upstream Python markitdown can't read ODS, so the Rust
  engine is the only one that handles it.
- **MOBI / Kindle (.mobi/.azw/.azw3) support** via the `mobi` crate → HTML →
  Markdown (lossy decode for legacy cp1252 books). Also unsupported upstream.
- **Richer desktop Markdown preview.** The Rendered view's dependency-free
  renderer now handles `<!-- Slide N -->` markers (→ dividers), advisory notes
  (→ asides), in-cell `<br>`, and `~~strikethrough~~`, on top of headings,
  bold/italic, code, links, images, lists, blockquotes and GFM tables — all
  HTML-escaped for safety. Covered by `src/markdown.test.ts` (Node's built-in
  test runner via type-stripping; `npm test`, no new dependencies). Still no
  heavy Markdown library — kept ~5 KB for fast rendering.
- **Progress across all formats** (not just PDF): per-sheet (XLSX), per-slide
  (PPTX), per-file (ZIP), per-page (PDF), plus coarse `detect → convert → done`
  for everything. Still opt-in (a sink must be attached) so default conversions
  have zero overhead.
- **Formatting-fidelity regression tests** (`tests/formatting.rs`): a synthetic
  DOCX verifies `Heading→#`, `w:b→**bold**`, `w:i→*italic*`, and `w:tbl→GFM
  table`; XLSX/PPTX assert sheet/slide tables + headings on real fixtures.
- **Progress reporting for heavy conversions.** A 350 MB PDF no longer looks
  like a hung process:
  - Core: optional `ConvertOptions.progress` sink (`ProgressCallback`) emitting
    phase + percentage; the PDF converter extracts **page-by-page with real
    `page X/N (Y%)`** when a sink is set (and survives a corrupt page). Zero
    overhead and unchanged fast path when no sink is installed.
  - CLI: `-V` / `--verbose` streams phase + per-page % + timing to **stderr**
    (stdout stays clean Markdown); percent lines throttled to whole-percent
    changes. Works for the rust, python, and auto engines (shows fallback
    delegation).
  - MCP: conversion progress logged to the server's stderr via `tracing`.
  - Desktop: live `job:progress` events drive a per-job determinate progress
    bar + percentage label and Logs-panel lines, so heavy files visibly move.
- **Dev-build speed fix.** `[profile.dev.package."*"] opt-level = 3` (workspace
  + desktop) so heavy deps like `pdf-extract` run optimized under `tauri dev` /
  debug `cargo` — a large PDF that took 10+ min unoptimized now runs in ~release
  time. (The shipped release app was always optimized.)
- **PDFium PDF backend, bundled in releases.** Uses Google's PDFium for PDF text
  extraction — **~1.3s** on a 342 MB PDF vs ~21s default Rust / ~6s Python
  (≈16× / ≈5×). The release workflow downloads the matching PDFium per OS/arch
  (Linux x64/arm64, Windows x64, macOS Intel/Apple-Silicon) and ships it next to
  the CLI/MCP binaries and as a desktop Tauri resource (wired up at startup), so
  installed apps are fast with **zero setup**. Building from source is opt-in
  (`--features pdfium`); a plain build stays pure-Rust/static. The library is
  discovered via `MARKITDOWN_PDFIUM_LIB` → next to the binary → system, and
  conversion **falls back to pure-Rust** if it's missing — never a hard failure.
- **~3× faster large-PDF loading.** Re-enabled lopdf's `rayon` feature (which
  `pdf-extract` disables via `default-features=false`) through Cargo feature
  unification — PDF object parsing now runs across all cores. A 342 MB PDF went
  **57s → ~21s** end-to-end with byte-identical output and no code change.
- **Parallel PDF extraction** (rayon) on the progress path: pages render across
  all cores and a corrupt page is skipped rather than failing the document.
  Note: for image-heavy PDFs the time is dominated by the serial document
  *load* in the PDF library, so this mainly helps text-heavy, many-page files.
- **Python engine builds as onedir (fast startup) for all OSes.** A PyInstaller
  one-file binary re-extracts ~100 MB per launch (≈45s/run on macOS via
  Gatekeeper); `build_binary.{sh,ps1}` now default to **onedir** (~0.5s startup
  after first run). The release workflow builds it for Linux x86_64/aarch64,
  Windows x86_64, and macOS Apple Silicon **and Intel**, archives each onedir
  folder, and appends it to the release without blocking the main release.
- **Python build version guard.** `build_binary.{sh,ps1}` require Python
  3.10–3.13 (prefer 3.12, matching CI's `setup-python`) and stop with clear
  guidance instead of hanging on a too-new interpreter (e.g. 3.14, whose native
  deps lack wheels).
- **Python engine works in installed apps (no env needed).** `resolve_python_bin`
  now auto-discovers a `markitdown-py` binary next to the running executable
  (e.g. a bundled Tauri sidecar) or on `PATH`, in addition to `--python-bin` /
  `MARKITDOWN_PY_BIN`. Only the exact `markitdown-py` name is searched (never the
  bare `markitdown` Rust CLI). Fixes GUI "missing dependency" failures, since
  Finder/dock-launched apps don't inherit the shell environment.
- **Desktop: Settings → Python engine** path field (+ Browse + capability pill),
  passed into conversions, so users can point the GUI at the binary directly.
- **Python-engine heartbeat logging.** The Python subprocess is opaque (no
  per-page signal), so while it runs the engine now emits an elapsed-time
  heartbeat (`Python engine running… Ns elapsed`) — the path shows liveness in
  the CLI (`-V`), desktop logs, and MCP logs, like the Rust path.
- **Profiled the engines on a 342 MB / 69-page PDF** (documented in the README):
  Rust pdf-extract ≈ 57s (55.7s of it is the serial `lopdf` load); the **Python
  engine (pdfminer) ≈ 6s** and yields richer table output. For very large PDFs,
  `--engine python` (or `auto` with `MARKITDOWN_PY_BIN` set, which falls back
  for scanned PDFs) is dramatically faster.
- **LLM provider registry** (`crates/markitdown-core/src/llm_providers.rs`) —
  one customizable list of OpenAI-compatible **vision** providers: OpenAI,
  **Anthropic/Claude** (OpenAI-compatible endpoint), Ollama, LM Studio,
  OpenRouter, Groq, **Qwen-VL (Alibaba DashScope)**, **Zhipu GLM-4V**,
  **Moonshot Kimi**, and custom — each with default base URL, key requirement,
  local flag and example vision models. Shared by CLI, MCP and desktop:
  - CLI: `--llm-provider <id>` (sets the base URL) and `--list-llm-providers`.
  - Env: `MARKITDOWN_LLM_PROVIDER` (base URL preset; `MARKITDOWN_LLM_API_BASE`
    overrides it).
  - Desktop: a provider dropdown + model datalist (swap models freely).
- **LLM image captions exposed everywhere** (OpenAI-compatible, cloud **or
  local**):
  - CLI flags `--llm-api-key`, `--llm-model`, `--llm-api-base`, `--llm-prompt`
    (override the `MARKITDOWN_LLM_*` env). Local LLMs supported by pointing
    `--llm-api-base` at Ollama (`http://localhost:11434/v1`) or LM Studio
    (`http://localhost:1234/v1`).
  - MCP server honors `MARKITDOWN_LLM_*` from its launch environment.
  - Desktop app: Settings panel for key/model/base/prompt with OpenAI / Ollama
    / LM Studio presets and live capability status.
- **`markitdown --check`** ("doctor"): reports Python-fallback and LLM-caption
  availability (model + endpoint) **without printing secrets**.
- Shared `markitdown_core::capabilities()` used by the CLI `--check`, the MCP
  `list_supported_formats` tool, and the desktop status badges.
- **Optional Python fallback binaries packaged in releases**
  (`markitdown-py-<platform>` for Linux x86_64/aarch64, Windows x86_64, macOS
  Apple Silicon), built best-effort and smoke-tested; Intel macOS builds
  locally.

### Tests
- CLI: end-to-end LLM caption via a mock OpenAI server; `--check` reporting and
  no-secret-leak assertions; stub-Python-bin detection.
- MCP: LLM-via-env **simulation** (server launched with `MARKITDOWN_LLM_*` →
  image gains `# Description:`); capability reporting.
- Core: `capabilities()` unit tests incl. secret-redaction.
- Cross-platform `engine_selection.rs` (runs on Windows); the subprocess-stub
  fallback suite stays `#![cfg(unix)]`.

### Changed
- CLI default engine is `auto` (transparent Python fallback when configured).

### Security / robustness (whole-repo review pass)
- **SSRF guard + timeouts on all outbound HTTP** (new `net` module). Fetching an
  untrusted URL (a `convert_to_markdown` URI, or a YouTube caption `baseUrl`
  read from page JSON) now goes through a resolver that refuses
  private/loopback/link-local/cloud-metadata targets — enforced on **every
  redirect hop** — plus a 30s global timeout. User-configured LLM endpoints use
  a timeout-only "trusted" agent so local models (`http://localhost:…`) still
  work. Override the guard with `MARKITDOWN_ALLOW_LOCAL_URLS=1`. LLM captioning
  also caps image size before encoding.
- **No converter can crash the host.** Converter dispatch is wrapped in
  `catch_unwind`, so a malformed/malicious file that trips a panic deep in a
  third-party parser (calamine, mobi, exif, quick-xml, …) becomes a clean
  conversion error instead of aborting the CLI/MCP/desktop process.
- **Zip-bomb / decompression caps** for every zip-based format (ZIP, DOCX,
  PPTX, XLSX/ODS, EPUB): per-entry size, per-archive total, and entry-count
  limits, so a small crafted file can't exhaust memory. The recursive ZIP
  converter also switched from `by_name` (O(N²)) to `by_index`.
- **MHTML quoted-printable decoder** no longer panics on a multibyte char after
  `=` (byte-wise hex decode instead of a `&str` slice).
- **XML/RSS nesting is depth-capped** (256) so a deeply-nested document can't
  overflow the stack during recursive traversal.
- **Batch output names are unique** (CLI and MCP `convert_batch`): two inputs
  sharing a basename in different directories no longer silently overwrite each
  other; the MCP server also rejects `..` traversal in output paths.
- **Desktop concurrency fix:** the conversion semaphore is now a single
  process-wide instance with an RAII permit, so the limit holds across batches
  and a panicking conversion can't leak a permit and wedge the queue.
- **Misc hardening:** `MARKITDOWN_PY_ARGS` is split quote-aware (paths/prompts
  with spaces survive); the CLI logs to stderr via a BrokenPipe-safe macro (a
  batch worker can't panic on `… | head`); the desktop Markdown preview's
  italic/underscore regexes no longer eat a neighbouring character.

### Fixed
- **macOS downloads no longer report as "damaged".** The desktop `.app`/`.dmg`
  and the standalone `markitdown`/`markitdown-mcp` binaries (and the bundled
  PDFium dylib) are now **ad-hoc code-signed** (`codesign -s -`). On Apple
  Silicon a quarantined *unsigned* binary is rejected by Gatekeeper as
  "damaged"; an ad-hoc signature is a valid signature, so the app/binaries open
  via the normal right-click→Open / `xattr -dr com.apple.quarantine` path
  instead. Done via `bundle.macOS.signingIdentity = "-"` in `tauri.conf.json`
  (Tauri signs the app and wraps it in the dmg) plus `codesign` steps in the
  release workflow for the dylib and CLI/MCP binaries; the workflow also
  `codesign --verify`s the bundle before packaging. Still **not notarized** (no
  Apple certs in CI), so first launch still needs the one-time Gatekeeper
  confirmation.
- **Python-engine heartbeat is now emitted immediately** when the subprocess
  starts (a `Python engine running… 0s elapsed` tick) instead of only after the
  first ~2s sleep slice. This removes a thread-scheduling race where a fast
  subprocess on a loaded CI runner could finish before the heartbeat thread's
  first periodic emit, producing zero liveness events — which intermittently
  failed `python_engine_emits_heartbeat_progress` (seen on the macOS x86_64
  runner; the same code path covers aarch64). The test no longer depends on the
  stub sleep crossing the heartbeat interval.

## Earlier

- Initial Rust suite: pure-Rust engine with 18 converters; `markitdown` CLI
  (man page, parallel batch); `markitdown-mcp` server (rmcp, stdio, 4 tools);
  Tauri v2 desktop app (drag/drop, queue, progress, retry, logs, editable
  preview); hybrid Python fallback via `--engine auto` / `MARKITDOWN_PY_BIN`
  (URLs/paths handed over at full fidelity; `MARKITDOWN_PY_ARGS` for Azure).
- BrokenPipe handled gracefully (`… | head`/`grep -q` no longer panics).
- Windows-safe batch output naming.
- UPX compression of the Linux/Windows standalone binaries in releases.
- CI (`app-ci.yml`) + native multi-OS release pipeline (`app-release.yml`) with
  a shared per-format regression smoke test (`.github/scripts/smoke.sh`).
