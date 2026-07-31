The optional PyInstaller Python engine is staged here at build time and bundled
as a Tauri resource, so an installed app (DMG / MSI / deb) has OCR, audio
transcription and the Azure converters with zero configuration.

Expected layouts (either works):
  python-engine/markitdown-py                 # PyInstaller --onefile
  python-engine/markitdown-py/markitdown-py   # PyInstaller --onedir (default)

Build one with `app/python-engine/build_binary.sh` (or `.ps1`) and copy the
result here before `npm run tauri build`. On startup the app points
`MARKITDOWN_PY_BIN` at whatever it finds here (see `find_python_engine` in
`src/lib.rs`); a user-set `MARKITDOWN_PY_BIN`, or the Settings → Python engine
path field, always wins.

Bundling is OPTIONAL — the engine adds ~150–400 MB to the installer. When this
directory holds only this README the app still works: the pure-Rust engine
covers every format except OCR/transcription, and the core resolver will still
find a `markitdown-py` installed elsewhere on the machine.

The staged binaries are gitignored; only this README is committed so the
resource glob always matches and a local `tauri build` never fails.
