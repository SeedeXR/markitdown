//! Regression tests for malformed PDFs.
//!
//! BACKGROUND: `pdf-extract` panics (not errors — panics) when it meets a
//! content stream it cannot parse. Real-world PDFs hit this regularly. The
//! converter catches those panics per page, so a document with one bad page
//! still yields the other pages, and a document that is bad throughout returns
//! an error instead of unwinding into the caller.
//!
//! This is the bug that killed the desktop app: its release profile had
//! `panic = "abort"`, which makes `catch_unwind` powerless, so dropping a
//! slightly-broken PDF on the window terminated the process. These tests pin
//! the recovery behaviour; `panic_strategy_is_unwind` pins the build setting it
//! depends on.
//!
//! The fixtures are built in code rather than committed as blobs, so the exact
//! breakage under test is visible and no binary needs to live in the repo.

use markitdown_core::{ConvertOptions, Engine, MarkItDown, StreamInfo};

/// Assemble a syntactically valid PDF (correct xref offsets and trailer) from
/// numbered object bodies. Object N is `objects[N - 1]`.
fn build_pdf(objects: &[Vec<u8>]) -> Vec<u8> {
    let mut pdf = Vec::from(&b"%PDF-1.4\n"[..]);
    let mut offsets = Vec::with_capacity(objects.len());
    for (i, body) in objects.iter().enumerate() {
        offsets.push(pdf.len());
        pdf.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
        pdf.extend_from_slice(body);
        pdf.extend_from_slice(b"\nendobj\n");
    }
    let xref_at = pdf.len();
    pdf.extend_from_slice(format!("xref\n0 {}\n", objects.len() + 1).as_bytes());
    pdf.extend_from_slice(b"0000000000 65535 f \n");
    for off in &offsets {
        pdf.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
    }
    pdf.extend_from_slice(
        format!(
            "trailer\n<</Size {}/Root 1 0 R>>\nstartxref\n{xref_at}\n%%EOF\n",
            objects.len() + 1
        )
        .as_bytes(),
    );
    pdf
}

/// A content-stream object holding exactly `content`.
fn stream_obj(content: &[u8]) -> Vec<u8> {
    let mut o = format!("<</Length {}>>\nstream\n", content.len()).into_bytes();
    o.extend_from_slice(content);
    o.extend_from_slice(b"\nendstream");
    o
}

fn page_obj(contents_ref: usize) -> Vec<u8> {
    format!(
        "<</Type/Page/Parent 2 0 R/MediaBox[0 0 612 792]\
         /Resources<</Font<</F1 3 0 R>>>>/Contents {contents_ref} 0 R>>"
    )
    .into_bytes()
}

/// One-page document whose single content stream is `content`.
fn one_page_pdf(content: &[u8]) -> Vec<u8> {
    build_pdf(&[
        b"<</Type/Catalog/Pages 2 0 R>>".to_vec(),
        b"<</Type/Pages/Kids[4 0 R]/Count 1>>".to_vec(),
        b"<</Type/Font/Subtype/Type1/BaseFont/Helvetica>>".to_vec(),
        page_obj(5),
        stream_obj(content),
    ])
}

/// Content streams that are structurally invalid rather than merely unusual.
fn broken_content_streams() -> Vec<&'static [u8]> {
    vec![
        // Unbalanced dictionary open — the classic InvalidContentStream.
        b"BT /F1 12 Tf << (hello) Tj ET",
        // Unterminated literal string.
        b"BT /F1 12 Tf (unterminated Tj ET",
        // Raw binary garbage where operators are expected.
        b"\xff\xfe\x00\x01\x02\x03\x80\x90 BT ET",
        // Unbalanced array.
        b"BT /F1 12 Tf [ (a) 1 (b) TJ ET",
    ]
}

fn convert(data: &[u8]) -> Result<markitdown_core::ConvertResult, markitdown_core::ConvertError> {
    // Engine::Rust so the assertion is about the Rust path, not a Python
    // fallback that may or may not be installed on the machine running tests.
    MarkItDown::with_options(ConvertOptions {
        engine: Engine::Rust,
        ..Default::default()
    })
    .convert_bytes(data, StreamInfo::new().with_extension(".pdf"))
}

#[test]
fn the_fixture_builder_produces_a_readable_pdf() {
    // Guards every other test in this file: if the builder emitted something
    // unreadable, the "survives malformed input" assertions would pass for the
    // wrong reason.
    let pdf = one_page_pdf(b"BT /F1 12 Tf 72 720 Td (HELLOFIXTURE) Tj ET");
    let out = convert(&pdf).expect("a well-formed fixture must convert");
    assert!(
        out.markdown.contains("HELLOFIXTURE"),
        "fixture text missing; got:\n{}",
        out.markdown
    );
}

#[test]
fn malformed_content_stream_is_reported_not_crashed() {
    // The contract: an unparseable document returns Err (or empty Ok) — but
    // never takes the process down. Before the fix this aborted the app.
    for content in broken_content_streams() {
        let pdf = one_page_pdf(content);
        match convert(&pdf) {
            Err(e) => {
                let msg = e.to_string();
                assert!(
                    msg.contains("pdf"),
                    "expected a PDF conversion error, got: {msg}"
                );
            }
            // A parser that recovers and yields nothing useful is fine too;
            // it must simply not contain garbage from the broken stream.
            Ok(out) => assert!(
                !out.markdown.contains("unterminated"),
                "leaked raw stream contents: {}",
                out.markdown
            ),
        }
    }
}

#[test]
fn a_bad_page_does_not_lose_the_good_pages() {
    // Page 1 is broken, page 2 is valid. Per-page recovery must still return
    // page 2 — whole-document extraction loses everything here.
    let pdf = build_pdf(&[
        b"<</Type/Catalog/Pages 2 0 R>>".to_vec(),
        b"<</Type/Pages/Kids[4 0 R 6 0 R]/Count 2>>".to_vec(),
        b"<</Type/Font/Subtype/Type1/BaseFont/Helvetica>>".to_vec(),
        page_obj(5),
        stream_obj(b"BT /F1 12 Tf << (x) Tj ET"),
        page_obj(7),
        stream_obj(b"BT /F1 12 Tf 72 720 Td (SURVIVINGPAGE) Tj ET"),
    ]);

    let result = convert(&pdf).expect("a document with one good page must convert");
    assert!(
        result.markdown.contains("SURVIVINGPAGE"),
        "the intact page's text was lost; got:\n{}",
        result.markdown
    );
}

/// The recovery above is only possible while panics unwind. If a build profile
/// ever sets `panic = "abort"` again, `catch_unwind` silently stops working and
/// every malformed PDF becomes a hard crash — so assert the strategy directly.
#[test]
fn panic_strategy_is_unwind() {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let result = std::panic::catch_unwind(|| panic!("probe"));
    std::panic::set_hook(prev);
    assert!(
        result.is_err(),
        "panics must unwind: with panic=\"abort\" this test cannot even run, and \
         the PDF converter's per-page recovery is disabled"
    );
}
