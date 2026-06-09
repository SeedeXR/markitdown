PDFium libraries are placed here at release time (downloaded per-OS from
https://github.com/bblanchon/pdfium-binaries) and bundled as Tauri resources:
  macOS:   libpdfium.dylib
  Linux:   libpdfium.so
  Windows: pdfium.dll
The app points MARKITDOWN_PDFIUM_LIB at the bundled file on startup. This dir's
binaries are gitignored; only this README is committed so the resource glob
always matches and local `tauri build` doesn't fail.
