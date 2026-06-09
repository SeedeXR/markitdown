//! PDF → Markdown converter.
//!
//! Port of `packages/markitdown/src/markitdown/converters/_pdf_converter.py`.
//!
//! DEVIATION FROM PYTHON: the Python converter uses pdfminer for linear text
//! extraction *plus* pdfplumber word-position heuristics to reconstruct
//! borderless tables/forms into aligned Markdown tables. We deliberately SKIP
//! the table-reconstruction pass — there is no pure-Rust equivalent of
//! pdfplumber's layout analysis — and emit only linearized text via
//! `pdf_extract::extract_text_from_mem`, which mirrors pdfminer's behavior.
//!
//! `pdf-extract` can panic on malformed PDFs, so the extraction call is wrapped
//! in `catch_unwind` and any panic is mapped to a `FileConversion` error.
//!
//! When extraction yields only whitespace the PDF is almost certainly scanned /
//! image-only. We then return Ok with an HTML comment noting OCR requires the
//! optional Python engine; the empty-ish (whitespace-only) markdown trips the
//! Auto-engine fallback upstream (`markdown.trim().is_empty()`).

use crate::{ConvertError, ConvertOptions, ConvertResult, Converter, StreamInfo};
use std::panic::{catch_unwind, AssertUnwindSafe};

pub struct PdfConverter;

const ACCEPTED_EXTENSIONS: &[&str] = &[".pdf"];
const ACCEPTED_MIMETYPES: &[&str] = &["application/pdf", "application/x-pdf"];

/// Collapse 3+ consecutive newlines down to exactly 2.
fn normalize_newlines(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut newline_run = 0usize;
    for ch in text.chars() {
        if ch == '\n' {
            newline_run += 1;
            if newline_run <= 2 {
                out.push('\n');
            }
        } else if ch == '\r' {
            // normalize away bare CRs; the following \n (if any) is handled above
            continue;
        } else {
            newline_run = 0;
            out.push(ch);
        }
    }
    out
}

impl Converter for PdfConverter {
    fn name(&self) -> &'static str {
        "pdf"
    }

    fn accepts(&self, info: &StreamInfo, data: &[u8]) -> bool {
        if info.extension_is(ACCEPTED_EXTENSIONS) || info.mimetype_is(ACCEPTED_MIMETYPES) {
            return true;
        }
        // Magic-byte sanity check: PDFs start with "%PDF-".
        data.starts_with(b"%PDF-")
    }

    fn convert(
        &self,
        data: &[u8],
        _info: &StreamInfo,
        opts: &ConvertOptions,
    ) -> Result<ConvertResult, ConvertError> {
        let text = extract_text(data, opts)?;

        if text.trim().is_empty() {
            // Scanned / image-only PDF. Return whitespace-only markdown (an HTML
            // comment) so the Auto engine can fall back to the Python OCR path.
            let note = "<!-- This PDF appears to be scanned or image-only; no text \
                        layer was found. OCR requires the optional Python engine \
                        (set MARKITDOWN_PY_BIN). -->";
            return Ok(ConvertResult::new(note).with_degraded());
        }

        Ok(ConvertResult::new(normalize_newlines(&text)))
    }
}

/// Extract text, preferring the fast PDFium backend when the `pdfium` feature
/// is compiled in AND the library is available at runtime; otherwise (or on any
/// PDFium failure) fall back to the pure-Rust path. The fallback keeps the
/// default build byte-for-byte unchanged.
fn extract_text(data: &[u8], opts: &ConvertOptions) -> Result<String, ConvertError> {
    #[cfg(feature = "pdfium")]
    {
        match pdfium::extract(data) {
            Ok(Some(text)) => {
                opts.report(crate::Progress::msg("pdf", "extracted via PDFium"));
                return Ok(text);
            }
            Ok(None) => opts.report(crate::Progress::msg(
                "pdf",
                "PDFium library not found; using the pure-Rust extractor",
            )),
            Err(e) => opts.report(crate::Progress::msg(
                "pdf",
                format!("PDFium failed ({e}); using the pure-Rust extractor"),
            )),
        }
    }
    extract_pure_rust(data, opts)
}

/// The pure-Rust extraction path (pdf-extract/lopdf).
///
/// Default: the library's single-call extraction — byte-for-byte the original
/// output. When a progress sink is installed (desktop / CLI `-V`), use parallel
/// page-by-page extraction instead: it reports per-page %, survives a single
/// corrupt page, and uses all cores.
fn extract_pure_rust(data: &[u8], opts: &ConvertOptions) -> Result<String, ConvertError> {
    if opts.progress.is_some() {
        return extract_pdf_parallel(data, opts);
    }
    let extracted = catch_unwind(AssertUnwindSafe(|| pdf_extract::extract_text_from_mem(data)));
    match extracted {
        Ok(Ok(text)) => Ok(text),
        Ok(Err(e)) => Err(ConvertError::conversion(
            "pdf",
            format!("failed to extract text from PDF: {e}"),
        )),
        Err(_) => Err(ConvertError::conversion(
            "pdf",
            "pdf-extract panicked while parsing the document (malformed PDF)",
        )),
    }
}

/// Fast PDFium backend (feature `pdfium`). PDFium is loaded as a dynamic
/// library at runtime, discovered via `MARKITDOWN_PDFIUM_LIB`, next to the
/// executable, or on the system. ~20x faster than pure Rust on large PDFs.
#[cfg(feature = "pdfium")]
mod pdfium {
    use pdfium_render::prelude::*;

    /// `Ok(Some(text))` on success, `Ok(None)` when no PDFium library is found
    /// (so the caller falls back), `Err` on a real PDFium/parse error.
    pub(super) fn extract(data: &[u8]) -> Result<Option<String>, String> {
        let Some(bindings) = bind() else {
            return Ok(None);
        };
        let pdfium = Pdfium::new(bindings);
        let doc = pdfium
            .load_pdf_from_byte_slice(data, None)
            .map_err(|e| e.to_string())?;
        let mut out = String::new();
        for page in doc.pages().iter() {
            if let Ok(text) = page.text() {
                out.push_str(&text.all());
                out.push('\n');
            }
        }
        Ok(Some(out))
    }

    /// Locate and bind the PDFium library: explicit env path → next to the
    /// running executable → system library. Returns `None` if none bind.
    fn bind() -> Option<Box<dyn PdfiumLibraryBindings>> {
        if let Some(p) = std::env::var_os("MARKITDOWN_PDFIUM_LIB") {
            if let Ok(b) = Pdfium::bind_to_library(&p) {
                return Some(b);
            }
        }
        if let Ok(exe) = std::env::current_exe() {
            if let Some(dir) = exe.parent() {
                let lib = Pdfium::pdfium_platform_library_name_at_path(dir);
                if let Ok(b) = Pdfium::bind_to_library(&lib) {
                    return Some(b);
                }
            }
        }
        Pdfium::bind_to_system_library().ok()
    }
}

/// Parallel page-by-page text extraction.
///
/// Loads the document once, then renders every page concurrently across the
/// rayon thread pool (`pdf_extract::Document` is `Sync`; each page gets its own
/// `Processor`/`PlainTextOutput`, so there is no shared mutable state). Output
/// order is preserved by rayon's indexed `collect`. A single corrupt page is
/// skipped rather than failing the whole document. Per-page progress is emitted
/// only when `fine_progress` is set; otherwise just a coarse start message.
fn extract_pdf_parallel(data: &[u8], opts: &ConvertOptions) -> Result<String, ConvertError> {
    use pdf_extract::{output_doc_page, Document, PlainTextOutput};
    use rayon::prelude::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    let doc = catch_unwind(AssertUnwindSafe(|| Document::load_mem(data)))
        .map_err(|_| ConvertError::conversion("pdf", "pdf-extract panicked loading the PDF"))?
        .map_err(|e| ConvertError::conversion("pdf", format!("failed to load PDF: {e}")))?;

    // BTreeMap<page_number, ObjectId> — ordered; .len() is the page count.
    let pages: Vec<u32> = doc.get_pages().keys().copied().collect();
    let total = pages.len() as u64;
    opts.report(crate::Progress::msg(
        "pdf",
        format!("extracting text from {total} page(s) across CPU cores…"),
    ));

    let done = AtomicU64::new(0);
    let failed = AtomicU64::new(0);
    let report_pages = opts.progress.is_some() && opts.fine_progress;

    // Indexed parallel map preserves page order in the collected Vec.
    let parts: Vec<String> = pages
        .par_iter()
        .map(|&page_num| {
            let page = catch_unwind(AssertUnwindSafe(|| {
                let mut s = String::new();
                {
                    let mut out = PlainTextOutput::new(&mut s);
                    output_doc_page(&doc, &mut out, page_num)?;
                }
                Ok::<String, pdf_extract::OutputError>(s)
            }));
            let text = match page {
                Ok(Ok(s)) => s,
                // A bad page must not sink the whole document.
                Ok(Err(_)) | Err(_) => {
                    failed.fetch_add(1, Ordering::Relaxed);
                    String::new()
                }
            };
            if report_pages {
                let k = done.fetch_add(1, Ordering::Relaxed) + 1;
                opts.report(crate::Progress::step("pdf", format!("page {k}/{total}"), k, total));
            }
            text
        })
        .collect();

    let failed = failed.load(Ordering::Relaxed);
    if failed > 0 {
        opts.report(crate::Progress::msg(
            "pdf",
            format!("{failed}/{total} page(s) could not be extracted and were skipped"),
        ));
    }
    Ok(parts.concat())
}
