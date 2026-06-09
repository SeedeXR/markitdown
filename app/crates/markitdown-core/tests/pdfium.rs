//! PDFium fast-backend tests. Compiled ONLY with `--features pdfium`. The
//! actual-extraction test self-skips when no PDFium library is configured, so
//! `cargo test --features pdfium` still passes in CI without the native lib.
#![cfg(feature = "pdfium")]

use markitdown_core::MarkItDown;
use std::path::PathBuf;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../packages/markitdown/tests/test_files")
        .join(name)
}

#[test]
fn pdfium_extracts_text_when_lib_available() {
    if std::env::var_os("MARKITDOWN_PDFIUM_LIB").is_none() {
        eprintln!("skipping: set MARKITDOWN_PDFIUM_LIB to run the PDFium extraction test");
        return;
    }
    let r = MarkItDown::new().convert_path(fixture("test.pdf")).unwrap();
    assert!(
        r.markdown.contains("Introduction") || r.markdown.contains("language models"),
        "PDFium-extracted text expected, got: {:.200}",
        r.markdown
    );
}

#[test]
fn falls_back_to_pure_rust_without_lib() {
    // Point at a nonexistent lib: PDFium binding fails, conversion must still
    // succeed via the pure-Rust fallback (no hard failure).
    std::env::set_var("MARKITDOWN_PDFIUM_LIB", "/no/such/libpdfium.dylib");
    let r = MarkItDown::new().convert_path(fixture("test.pdf")).unwrap();
    std::env::remove_var("MARKITDOWN_PDFIUM_LIB");
    assert!(!r.markdown.trim().is_empty(), "fallback must still produce text");
}
