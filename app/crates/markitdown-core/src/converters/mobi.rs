//! MOBI / Kindle e-book → Markdown.
//!
//! The `mobi` crate parses the Palm-database container and exposes the book's
//! HTML content, which we run through the shared HTML→Markdown pipeline (same
//! as EPUB). Pure-Rust and lightweight. NOTE: upstream Python markitdown has no
//! MOBI converter, so this is Rust-only coverage.

use crate::converters::html::html_to_markdown;
use crate::{ConvertError, ConvertOptions, ConvertResult, Converter, StreamInfo};

pub struct MobiConverter;

const ACCEPTED_EXTENSIONS: &[&str] = &[".mobi", ".azw", ".azw3", ".prc"];
const ACCEPTED_MIMETYPES: &[&str] =
    &["application/x-mobipocket-ebook", "application/vnd.amazon.ebook"];

/// MOBI files are Palm databases whose type+creator at offset 60 is `BOOKMOBI`.
fn has_mobi_magic(data: &[u8]) -> bool {
    data.len() >= 68 && &data[60..68] == b"BOOKMOBI"
}

impl Converter for MobiConverter {
    fn name(&self) -> &'static str {
        "mobi"
    }

    fn accepts(&self, info: &StreamInfo, data: &[u8]) -> bool {
        info.extension_is(ACCEPTED_EXTENSIONS)
            || info.mimetype_is(ACCEPTED_MIMETYPES)
            || has_mobi_magic(data)
    }

    fn convert(
        &self,
        data: &[u8],
        _info: &StreamInfo,
        opts: &ConvertOptions,
    ) -> Result<ConvertResult, ConvertError> {
        let book = mobi::Mobi::new(data.to_vec())
            .map_err(|e| ConvertError::conversion("mobi", format!("failed to parse MOBI: {e}")))?;

        // Lossy decode: older MOBIs use cp1252/latin1, so strict UTF-8 fails.
        let html = book.content_as_string_lossy();

        let (mut markdown, html_title) = html_to_markdown(&html, opts.keep_data_uris);

        // Prefer the container's metadata title; fall back to the HTML <title>.
        let title = book.title();
        if !title.trim().is_empty() && !markdown.contains(&format!("# {}", title.trim())) {
            markdown = format!("# {}\n\n{}", title.trim(), markdown);
        }

        let mut result = ConvertResult::new(markdown.trim().to_string());
        if !title.trim().is_empty() {
            result = result.with_title(title);
        } else if let Some(t) = html_title {
            result = result.with_title(t);
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_extension_mimetype_and_magic() {
        let c = MobiConverter;
        assert!(c.accepts(&StreamInfo::new().with_extension(".mobi"), b""));
        assert!(c.accepts(&StreamInfo::new().with_extension(".azw3"), b""));
        assert!(c.accepts(
            &StreamInfo::new().with_mimetype("application/x-mobipocket-ebook"),
            b""
        ));
        // BOOKMOBI magic at offset 60.
        let mut magic = vec![0u8; 68];
        magic[60..68].copy_from_slice(b"BOOKMOBI");
        assert!(c.accepts(&StreamInfo::new(), &magic));
        // Negatives.
        assert!(!c.accepts(&StreamInfo::new().with_extension(".pdf"), b""));
        assert!(!c.accepts(&StreamInfo::new(), b"not a mobi file at all"));
    }
}
