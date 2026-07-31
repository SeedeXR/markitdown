//! PDF → Markdown converter.
//!
//! Port of `packages/markitdown/src/markitdown/converters/_pdf_converter.py`.
//!
//! Unlike the Python converter (pdfminer for text + pdfplumber for tables) we
//! reconstruct structure ourselves: both backends emit positioned glyphs and
//! [`super::pdf_layout`] turns them into Markdown — headings from relative font
//! size, tables from column alignment, lists from bullet prefixes, emphasis
//! from font weight, paragraphs from vertical gaps. See that module for the
//! rules; this file only deals with *getting* the glyphs out of a PDF.
//!
//! Two backends, both funnelling into the same layout pass:
//!   * **PDFium** (feature `pdfium`, the configuration we ship) — fast, and the
//!     only one that reports font weight/style, so it is also the only one that
//!     can mark **bold**/*italic*.
//!   * **pure Rust** (`pdf-extract`/`lopdf`) — always available, no native
//!     dependency; `OutputDev` never passes the font down, so emphasis is not
//!     detectable there. Everything else works identically.
//!
//! ROBUSTNESS: `pdf-extract` panics on malformed content streams (a real,
//! common case — see `failed_pdfs/`). Every parse is wrapped in `catch_unwind`
//! *per page*, so one bad page degrades to a gap instead of failing the
//! document, and a document that cannot be loaded at all returns an error
//! rather than unwinding into the caller. This only works because every
//! profile that links this crate keeps `panic = "unwind"`.
//!
//! When extraction yields only whitespace the PDF is almost certainly scanned /
//! image-only. We then return Ok with an HTML comment noting OCR requires the
//! optional Python engine; the whitespace-only markdown trips the Auto-engine
//! fallback upstream (`markdown.trim().is_empty()`).

use super::pdf_layout::{self, Glyph};
use crate::{ConvertError, ConvertOptions, ConvertResult, Converter, StreamInfo};
use std::panic::{catch_unwind, AssertUnwindSafe};

pub struct PdfConverter;

const ACCEPTED_EXTENSIONS: &[&str] = &[".pdf"];
const ACCEPTED_MIMETYPES: &[&str] = &["application/pdf", "application/x-pdf"];

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
        let markdown = extract_markdown(data, opts)?;

        if markdown.trim().is_empty() {
            // Scanned / image-only PDF. Return whitespace-only markdown (an HTML
            // comment) so the Auto engine can fall back to the Python OCR path.
            let note = "<!-- This PDF appears to be scanned or image-only; no text \
                        layer was found. OCR requires the optional Python engine \
                        (set MARKITDOWN_PY_BIN). -->";
            return Ok(ConvertResult::new(note).with_degraded());
        }

        Ok(ConvertResult::new(markdown))
    }
}

/// Extract Markdown, preferring the PDFium backend when the `pdfium` feature is
/// compiled in AND the library is available at runtime; otherwise (or on any
/// PDFium failure) fall back to the pure-Rust path.
fn extract_markdown(data: &[u8], opts: &ConvertOptions) -> Result<String, ConvertError> {
    #[cfg(feature = "pdfium")]
    {
        match pdfium::extract(data, opts) {
            Ok(Some(mut pages)) => {
                opts.report(crate::Progress::msg("pdf", "extracted via PDFium"));
                return Ok(pdf_layout::document_to_markdown(&mut pages));
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
    let mut pages = collect_glyphs_pure_rust(data, opts)?;
    Ok(pdf_layout::document_to_markdown(&mut pages))
}

/// Collect glyphs page-by-page with the pure-Rust parser.
///
/// The document is loaded once, then pages are parsed concurrently across the
/// rayon pool (`pdf_extract::Document` is `Sync`; each page gets its own
/// collector, so there is no shared mutable state). Page order is preserved by
/// rayon's indexed `collect`. A page whose content stream is malformed panics
/// inside `pdf-extract`; that panic is caught here and the page is skipped, so
/// one broken page costs its own text and nothing more.
fn collect_glyphs_pure_rust(
    data: &[u8],
    opts: &ConvertOptions,
) -> Result<Vec<Vec<Glyph>>, ConvertError> {
    use pdf_extract::Document;
    use rayon::prelude::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    silence_pdf_parser_panics();
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

    let out: Vec<Vec<Glyph>> = pages
        .par_iter()
        .map(|&page_num| {
            let page = catch_unwind(AssertUnwindSafe(|| {
                let mut collector = GlyphCollector::default();
                pdf_extract::output_doc_page(&doc, &mut collector, page_num)?;
                Ok::<Vec<Glyph>, pdf_extract::OutputError>(collector.glyphs)
            }));
            let glyphs = match page {
                Ok(Ok(g)) => g,
                // A bad page must not sink the whole document.
                Ok(Err(_)) | Err(_) => {
                    failed.fetch_add(1, Ordering::Relaxed);
                    Vec::new()
                }
            };
            if report_pages {
                let k = done.fetch_add(1, Ordering::Relaxed) + 1;
                opts.report(crate::Progress::step(
                    "pdf",
                    format!("page {k}/{total}"),
                    k,
                    total,
                ));
            }
            glyphs
        })
        .collect();

    let failed = failed.load(Ordering::Relaxed);
    if failed > 0 {
        opts.report(crate::Progress::msg(
            "pdf",
            format!("{failed}/{total} page(s) could not be extracted and were skipped"),
        ));
    }
    // Every single page failing means the document is unreadable, not sparse.
    if total > 0 && failed == total {
        return Err(ConvertError::conversion(
            "pdf",
            "no page in the document could be parsed (malformed PDF)",
        ));
    }
    Ok(out)
}

/// Stop recovered parser panics from printing a crash report.
///
/// We already catch `pdf-extract`'s panics per page and carry on, but the
/// default panic hook still dumps "thread panicked at …" to stderr for each bad
/// page. That reads as a crash to a user watching the CLI, and it pollutes the
/// MCP server's only log channel. Installed once, this hook stays silent for
/// panics whose location is inside the PDF parser and delegates everything else
/// to the previous hook, so genuine bugs are still reported in full.
fn silence_pdf_parser_panics() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let from_parser = info.location().is_some_and(|loc| {
                let file = loc.file();
                file.contains("pdf-extract") || file.contains("lopdf")
            });
            if !from_parser {
                previous(info);
            }
        }));
    });
}

/// `pdf-extract` output device that records glyph positions instead of writing
/// text. Geometry mirrors `PlainTextOutput`: the text-rendering matrix supplies
/// the position, and the nominal font size is scaled by that matrix.
#[derive(Default)]
struct GlyphCollector {
    glyphs: Vec<Glyph>,
    /// Page height, used to flip PDF's bottom-up y into top-down reading order.
    page_height: f64,
}

impl pdf_extract::OutputDev for GlyphCollector {
    fn begin_page(
        &mut self,
        _page_num: u32,
        media_box: &pdf_extract::MediaBox,
        _art_box: Option<(f64, f64, f64, f64)>,
    ) -> Result<(), pdf_extract::OutputError> {
        self.page_height = media_box.ury - media_box.lly;
        Ok(())
    }

    fn end_page(&mut self) -> Result<(), pdf_extract::OutputError> {
        Ok(())
    }

    fn output_character(
        &mut self,
        trm: &pdf_extract::Transform,
        width: f64,
        spacing: f64,
        font_size: f64,
        char: &str,
    ) -> Result<(), pdf_extract::OutputError> {
        // Equivalent to `trm.post_transform(flip_ctm)` with
        // flip = row_major(1, 0, 0, -1, 0, page_height).
        let x = trm.m31;
        let y = self.page_height - trm.m32;
        // Per-axis scale of the text matrix, then their geometric mean. NOTE:
        // this is deliberately *not* upstream's
        // `transform_vector(vec2(fs, fs))`, which sums the two matrix rows —
        // that expression collapses to zero for text rotated ±45° (where
        // cos == sin), and a zero size zeroes every downstream threshold.
        let sx = trm.m11.hypot(trm.m12);
        let sy = trm.m21.hypot(trm.m22);
        let size = font_size * (sx * sy).abs().sqrt();
        // `spacing` is Tc (+ Tw on a literal space) in unscaled text units, so
        // it scales with the matrix but not with the font size. Upstream drops
        // it because it only ever asked "is there a space here?"; we measure
        // real geometry, and omitting it inflates every gap by exactly the
        // tracking the document applied — enough for justified prose to split
        // into cells and masquerade as a table.
        let scale = if font_size.abs() > f64::EPSILON {
            size / font_size
        } else {
            (sx * sy).abs().sqrt()
        };
        let advance = width * size + spacing * scale;

        // A single `char` callback may carry several code points (a ligature
        // expanded to "fi"); spread them across the advance so x ordering and
        // gap detection stay sane.
        let n = char.chars().count().max(1) as f64;
        for (i, ch) in char.chars().enumerate() {
            if ch.is_control() {
                continue;
            }
            let step = advance / n;
            let gx = x + step * i as f64;
            self.glyphs.push(Glyph {
                x: gx as f32,
                y: y as f32,
                end_x: (gx + step) as f32,
                size: size as f32,
                // `OutputDev` never exposes the font, so this backend cannot
                // detect emphasis. PDFium can; see the module docs.
                bold: false,
                italic: false,
                ch,
            });
        }
        Ok(())
    }

    fn begin_word(&mut self) -> Result<(), pdf_extract::OutputError> {
        Ok(())
    }
    fn end_word(&mut self) -> Result<(), pdf_extract::OutputError> {
        Ok(())
    }
    fn end_line(&mut self) -> Result<(), pdf_extract::OutputError> {
        Ok(())
    }
}

/// Fast PDFium backend (feature `pdfium`). PDFium is loaded as a dynamic
/// library at runtime, discovered via `MARKITDOWN_PDFIUM_LIB`, next to the
/// executable, or on the system. ~20x faster than pure Rust on large PDFs, and
/// the only backend that reports font weight/style.
#[cfg(feature = "pdfium")]
mod pdfium {
    use super::Glyph;
    use crate::ConvertOptions;
    use pdfium_render::prelude::*;

    /// `Ok(Some(pages))` on success, `Ok(None)` when no PDFium library is found
    /// (so the caller falls back), `Err` on a real PDFium/parse error.
    pub(super) fn extract(
        data: &[u8],
        opts: &ConvertOptions,
    ) -> Result<Option<Vec<Vec<Glyph>>>, String> {
        let Some(bindings) = bind() else {
            return Ok(None);
        };
        let pdfium = Pdfium::new(bindings);
        let doc = pdfium
            .load_pdf_from_byte_slice(data, None)
            .map_err(|e| e.to_string())?;

        let total = doc.pages().len() as u64;
        opts.report(crate::Progress::msg(
            "pdf",
            format!("extracting text from {total} page(s) via PDFium…"),
        ));
        let report_pages = opts.progress.is_some() && opts.fine_progress;

        let mut out = Vec::with_capacity(total as usize);
        for (i, page) in doc.pages().iter().enumerate() {
            let height = page.height().value;
            let mut glyphs = Vec::new();
            if let Ok(text) = page.text() {
                for ch in text.chars().iter() {
                    if let Some(g) = to_glyph(&ch, height) {
                        glyphs.push(g);
                    }
                }
            }
            out.push(glyphs);
            if report_pages {
                let k = i as u64 + 1;
                opts.report(crate::Progress::step(
                    "pdf",
                    format!("page {k}/{total}"),
                    k,
                    total,
                ));
            }
        }
        Ok(Some(out))
    }

    /// Map one PDFium character to a [`Glyph`], or `None` for control
    /// characters and PDFium's own generated line breaks (we rebuild lines from
    /// geometry, so its guesses would only add noise).
    fn to_glyph(ch: &PdfPageTextChar, page_height: f32) -> Option<Glyph> {
        let c = ch.unicode_char()?;
        if c.is_control() {
            return None;
        }
        // The BASELINE is the y to group lines by. Using the glyph's bounding
        // box bottom instead puts every descender ("p", "g", "y") a few points
        // lower than its neighbours, which splits them onto lines of their own
        // and silently drops letters out of words.
        let (origin_x, baseline) = ch.origin().ok()?;
        // loose_bounds() is the glyph's *advance* box. tight_bounds() would be
        // the ink extent, which excludes side bearings — the gap between two
        // letters inside a word would then look like a word space. Both
        // backends must measure advance width for one gap threshold to work.
        let (x, end_x) = match ch.loose_bounds().or_else(|_| ch.tight_bounds()) {
            Ok(r) => (r.left().value, r.right().value),
            Err(_) => (origin_x.value, origin_x.value),
        };
        Some(Glyph {
            x,
            // Flip PDF's bottom-up axis into top-down reading order.
            y: page_height - baseline.value,
            end_x,
            size: ch.scaled_font_size().value,
            bold: is_bold(ch),
            italic: ch.font_is_italic(),
            ch: c,
        })
    }

    /// PDFium reports the descriptor's numeric weight; 600+ is semibold/bold.
    /// `font_is_bold_reenforced` covers the ForceBold flag that some fonts set
    /// instead of a weight.
    fn is_bold(ch: &PdfPageTextChar) -> bool {
        matches!(
            ch.font_weight(),
            Some(
                PdfFontWeight::Weight600
                    | PdfFontWeight::Weight700Bold
                    | PdfFontWeight::Weight800
                    | PdfFontWeight::Weight900
            )
        ) || matches!(ch.font_weight(), Some(PdfFontWeight::Custom(w)) if w >= 600)
            || ch.font_is_bold_reenforced()
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
