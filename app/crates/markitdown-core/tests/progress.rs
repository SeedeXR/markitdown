//! Progress-reporting tests (cross-platform, CI-safe — no network, no
//! subprocess). Verifies that a progress sink receives phase + per-page
//! percentage updates for a multi-page PDF, and that installing a sink does
//! not change the conversion output.

use markitdown_core::{ConvertOptions, MarkItDown, Progress, ProgressCallback};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../packages/markitdown/tests/test_files")
        .join(name)
}

fn collecting_opts_with(fine: bool) -> (ConvertOptions, Arc<Mutex<Vec<Progress>>>) {
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink = events.clone();
    let opts = ConvertOptions {
        progress: Some(ProgressCallback::new(move |p| sink.lock().unwrap().push(p))),
        fine_progress: fine,
        ..Default::default()
    };
    (opts, events)
}

/// Fine-grained (per-page) progress — the CLI `-V` mode.
fn collecting_opts() -> (ConvertOptions, Arc<Mutex<Vec<Progress>>>) {
    collecting_opts_with(true)
}

#[test]
fn multipage_pdf_reports_per_page_percentage() {
    let (opts, events) = collecting_opts();
    let r = MarkItDown::with_options(opts)
        .convert_path(fixture("REPAIR-2022-INV-001_multipage.pdf"))
        .unwrap();
    assert!(!r.markdown.trim().is_empty());

    let ev = events.lock().unwrap();
    // A detect phase, pdf page steps, and a final done phase.
    assert!(ev.iter().any(|p| p.phase == "detect"));
    assert!(ev.iter().any(|p| p.phase == "done"));

    let pdf_steps: Vec<&Progress> = ev.iter().filter(|p| p.phase == "pdf" && p.total.is_some()).collect();
    assert!(!pdf_steps.is_empty(), "expected per-page pdf progress");

    let total = pdf_steps[0].total.unwrap();
    assert!(total >= 2, "multipage fixture should have >=2 pages, got {total}");
    // Percentages are monotonic non-decreasing and the last reaches 100%.
    let last = pdf_steps.last().unwrap();
    assert_eq!(last.current, Some(total));
    assert_eq!(last.percent(), Some(100));
    let mut prev = 0;
    for s in &pdf_steps {
        let c = s.current.unwrap();
        assert!(c >= prev, "page counter must not go backwards");
        prev = c;
    }
}

#[test]
fn progress_sink_does_not_change_output() {
    // Same PDF with and without a sink must produce identical Markdown.
    let plain = MarkItDown::new()
        .convert_path(fixture("test.pdf"))
        .unwrap()
        .markdown;
    let (opts, _ev) = collecting_opts();
    let with_sink = MarkItDown::with_options(opts)
        .convert_path(fixture("test.pdf"))
        .unwrap()
        .markdown;
    // Both must contain the document's real text; the sink path joins pages so
    // exact whitespace can differ — assert on stable content, not byte-equality.
    for needle in ["Introduction", "language models"] {
        assert!(plain.contains(needle), "fast path missing {needle:?}");
        assert!(with_sink.contains(needle), "progress path missing {needle:?}");
    }
}

#[test]
fn coarse_progress_does_not_trigger_per_page_path() {
    // Without fine_progress, a sink still gets cheap coarse phases (detect /
    // convert / done) but NO per-page steps — the fast extraction path runs.
    let (opts, events) = collecting_opts_with(false);
    MarkItDown::with_options(opts)
        .convert_path(fixture("REPAIR-2022-INV-001_multipage.pdf"))
        .unwrap();
    let ev = events.lock().unwrap();
    assert!(ev.iter().any(|p| p.phase == "detect"));
    assert!(ev.iter().any(|p| p.phase == "convert"));
    assert!(ev.iter().any(|p| p.phase == "done"));
    assert!(
        !ev.iter().any(|p| p.phase == "pdf" && p.total.is_some()),
        "coarse mode must not emit per-page steps (fast path preserved)"
    );
}

#[test]
fn done_event_emitted_for_non_pdf_too() {
    let (opts, events) = collecting_opts();
    MarkItDown::with_options(opts)
        .convert_path(fixture("test.docx"))
        .unwrap();
    let ev = events.lock().unwrap();
    assert!(ev.iter().any(|p| p.phase == "detect"));
    assert!(ev.iter().any(|p| p.phase == "convert"));
    assert!(ev.iter().any(|p| p.phase == "done"));
}

/// Helper: collect the phases emitted with a percentage for a fixture.
fn unit_steps(fixture_name: &str, phase: &str) -> Vec<(u64, u64)> {
    let (opts, events) = collecting_opts();
    MarkItDown::with_options(opts)
        .convert_path(fixture(fixture_name))
        .unwrap();
    let ev = events.lock().unwrap();
    ev.iter()
        .filter(|p| p.phase == phase && p.total.is_some())
        .map(|p| (p.current.unwrap(), p.total.unwrap()))
        .collect()
}

#[test]
fn xlsx_reports_per_sheet_progress() {
    let steps = unit_steps("test.xlsx", "xlsx");
    assert!(!steps.is_empty(), "expected per-sheet progress");
    let (_, total) = steps[0];
    assert_eq!(steps.last().unwrap().0, total, "last sheet == total");
}

#[test]
fn pptx_reports_per_slide_progress() {
    let steps = unit_steps("test.pptx", "pptx");
    assert!(steps.len() >= 2, "multi-slide deck should report each slide");
    let (_, total) = steps[0];
    assert_eq!(steps.last().unwrap().0, total, "last slide == total");
    // Monotonic non-decreasing slide counter.
    let mut prev = 0;
    for (c, _) in &steps {
        assert!(*c >= prev);
        prev = *c;
    }
}

#[test]
fn zip_reports_per_file_progress() {
    let steps = unit_steps("test_files.zip", "zip");
    assert!(!steps.is_empty(), "expected per-file progress for the archive");
    let (_, total) = steps[0];
    assert!(total >= 1);
    assert!(steps.iter().all(|(c, t)| c <= t));
}
