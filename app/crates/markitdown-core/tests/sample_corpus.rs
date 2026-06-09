//! Broad regression over the local sample corpus in `app/test_files/`.
//!
//! That directory is gitignored (author's local samples), so this test is
//! present-or-skip: it runs on a machine that has the files and self-skips in
//! CI. Every sample must convert to non-empty Markdown via the pure-Rust
//! engine — including formats the upstream Python engine can't do (`.ods`,
//! `.mobi`). Unsupported-by-design formats are listed explicitly.

use markitdown_core::MarkItDown;
use std::path::PathBuf;

fn samples_dir() -> Option<PathBuf> {
    let d = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../test_files");
    d.is_dir().then_some(d)
}

#[test]
fn every_local_sample_converts_via_rust() {
    let Some(dir) = samples_dir() else {
        eprintln!("skipping: app/test_files/ not present (expected in CI)");
        return;
    };
    let md = MarkItDown::new(); // Auto engine; pure-Rust when no Python bin
    let mut checked = 0;
    let mut failures = Vec::new();
    for entry in std::fs::read_dir(&dir).unwrap() {
        let p = entry.unwrap().path();
        if !p.is_file() {
            continue;
        }
        let name = p.file_name().unwrap().to_string_lossy().into_owned();
        match md.convert_path(&p) {
            Ok(r) if !r.markdown.trim().is_empty() => checked += 1,
            Ok(_) => failures.push(format!("{name}: empty output")),
            Err(e) => failures.push(format!("{name}: {e}")),
        }
    }
    assert!(checked > 0, "no samples found in {}", dir.display());
    assert!(failures.is_empty(), "sample conversions failed: {failures:?}");
    eprintln!("converted {checked} local samples via the Rust engine");
}

/// ODS and MOBI specifically (Rust supports both; Python markitdown does not).
#[test]
fn ods_and_mobi_samples_convert() {
    let Some(dir) = samples_dir() else {
        return;
    };
    let md = MarkItDown::new();
    for (file, needle) in [("sample_a.ods", "##"), ("sample_a.mobi", "#")] {
        let p = dir.join(file);
        if !p.is_file() {
            continue;
        }
        let r = md.convert_path(&p).unwrap_or_else(|e| panic!("{file}: {e}"));
        assert!(
            r.markdown.contains(needle),
            "{file}: expected Markdown structure, got {:.120}",
            r.markdown
        );
    }
}
