//! MHTML (MIME HTML, `.mhtml`/`.mht`) → Markdown.
//!
//! A saved-webpage MHTML is a MIME message (usually `multipart/related`) whose
//! root part is the page HTML and whose other parts are embedded resources
//! (images/CSS), commonly base64- or quoted-printable-encoded. We parse the
//! MIME structure, pick the main `text/html` part, decode it, and run it
//! through the shared HTML→Markdown pipeline — instead of dumping the raw MIME
//! (base64 and all) as plain text.

use crate::converters::html::html_to_markdown;
use crate::text::decode_text;
use crate::{ConvertError, ConvertOptions, ConvertResult, Converter, StreamInfo};
use base64::Engine as _;

pub struct MhtmlConverter;

const ACCEPTED_EXTENSIONS: &[&str] = &[".mhtml", ".mht"];
const ACCEPTED_MIMETYPES: &[&str] = &["multipart/related", "message/rfc822", "application/x-mimearchive"];

impl Converter for MhtmlConverter {
    fn name(&self) -> &'static str {
        "mhtml"
    }

    fn accepts(&self, info: &StreamInfo, data: &[u8]) -> bool {
        if info.extension_is(ACCEPTED_EXTENSIONS) || info.mimetype_is(ACCEPTED_MIMETYPES) {
            return true;
        }
        // Magic: an MHTML archive begins with MIME headers and declares a
        // multipart/related (or mhtml) content type near the top.
        let head = &data[..data.len().min(2048)];
        let head = String::from_utf8_lossy(head).to_ascii_lowercase();
        (head.contains("mime-version:") || head.starts_with("from:"))
            && head.contains("multipart/related")
    }

    fn convert(
        &self,
        data: &[u8],
        _info: &StreamInfo,
        opts: &ConvertOptions,
    ) -> Result<ConvertResult, ConvertError> {
        let text = String::from_utf8_lossy(data);
        let (headers, body) = split_headers(&text);
        let ctype = header_value(&headers, "content-type").unwrap_or_default();

        // Collect the candidate HTML body (decoded bytes + its charset).
        let (html_bytes, charset) = if let Some(boundary) = boundary_of(&ctype) {
            extract_html_part(body, &boundary)
                .ok_or_else(|| ConvertError::conversion("mhtml", "no text/html part found"))?
        } else if ctype.to_ascii_lowercase().contains("text/html") {
            // Single-part message that is itself HTML.
            (decode_part(body, &headers), charset_of(&ctype))
        } else {
            return Err(ConvertError::conversion(
                "mhtml",
                "not a multipart/related or text/html MIME message",
            ));
        };

        let mut info = StreamInfo::new();
        if let Some(cs) = charset {
            info = info.with_charset(&cs);
        }
        let html = decode_text(&html_bytes, &info);
        let (markdown, title) = html_to_markdown(&html, opts.keep_data_uris);
        let mut result = ConvertResult::new(markdown);
        if let Some(t) = title {
            result = result.with_title(t);
        }
        Ok(result)
    }
}

/// Split a MIME message into its header block and body at the first blank line.
fn split_headers(msg: &str) -> (String, &str) {
    // Unfold continuation lines (leading whitespace) into their header.
    let sep = msg.find("\r\n\r\n").map(|i| (i, 4)).or_else(|| msg.find("\n\n").map(|i| (i, 2)));
    match sep {
        Some((i, n)) => (unfold(&msg[..i]), &msg[i + n..]),
        None => (unfold(msg), ""),
    }
}

/// RFC 822 header unfolding: a line starting with space/tab continues the prior.
fn unfold(headers: &str) -> String {
    let mut out = String::new();
    for line in headers.split('\n') {
        let line = line.trim_end_matches('\r');
        if line.starts_with(' ') || line.starts_with('\t') {
            out.push(' ');
            out.push_str(line.trim());
        } else {
            out.push('\n');
            out.push_str(line);
        }
    }
    out
}

fn header_value(headers: &str, name: &str) -> Option<String> {
    let name_l = name.to_ascii_lowercase();
    headers.lines().find_map(|l| {
        let (k, v) = l.split_once(':')?;
        (k.trim().to_ascii_lowercase() == name_l).then(|| v.trim().to_string())
    })
}

/// Extract the `boundary="..."` parameter from a multipart Content-Type.
fn boundary_of(ctype: &str) -> Option<String> {
    let lower = ctype.to_ascii_lowercase();
    if !lower.contains("multipart/") {
        return None;
    }
    let idx = lower.find("boundary")?;
    let after = &ctype[idx + "boundary".len()..];
    let after = after.trim_start().strip_prefix('=')?.trim_start();
    let val = if let Some(rest) = after.strip_prefix('"') {
        rest.split('"').next().unwrap_or("")
    } else {
        after.split([';', ' ', '\t']).next().unwrap_or("")
    };
    (!val.is_empty()).then(|| val.to_string())
}

fn charset_of(ctype: &str) -> Option<String> {
    let lower = ctype.to_ascii_lowercase();
    let idx = lower.find("charset")?;
    let after = ctype[idx + "charset".len()..].trim_start().strip_prefix('=')?.trim_start();
    let val = after.trim_matches('"').split([';', ' ', '\t', '"']).next().unwrap_or("");
    (!val.is_empty()).then(|| val.to_string())
}

/// Find the first `text/html` part within a multipart body and return its
/// decoded bytes + charset.
fn extract_html_part(body: &str, boundary: &str) -> Option<(Vec<u8>, Option<String>)> {
    let delim = format!("--{boundary}");
    for part in body.split(&delim) {
        let part = part.trim_start_matches(['\r', '\n']);
        if part.is_empty() || part.starts_with("--") {
            continue;
        }
        let (headers, pbody) = split_headers(part);
        let ctype = header_value(&headers, "content-type").unwrap_or_default();
        if ctype.to_ascii_lowercase().contains("text/html") {
            return Some((decode_part(pbody, &headers), charset_of(&ctype)));
        }
    }
    None
}

/// Decode a MIME part body per its Content-Transfer-Encoding.
fn decode_part(body: &str, headers: &str) -> Vec<u8> {
    let enc = header_value(headers, "content-transfer-encoding")
        .unwrap_or_default()
        .to_ascii_lowercase();
    match enc.trim() {
        "base64" => base64::engine::general_purpose::STANDARD
            .decode(body.split_whitespace().collect::<String>())
            .unwrap_or_else(|_| body.as_bytes().to_vec()),
        "quoted-printable" => decode_quoted_printable(body),
        _ => body.as_bytes().to_vec(), // 7bit/8bit/binary/none
    }
}

/// Minimal quoted-printable decoder (soft line breaks + `=XX` hex escapes).
fn decode_quoted_printable(s: &str) -> Vec<u8> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'=' {
            // Soft line break: `=` at end of line.
            if i + 1 < bytes.len() && (bytes[i + 1] == b'\r' || bytes[i + 1] == b'\n') {
                i += if bytes[i + 1] == b'\r' && i + 2 < bytes.len() && bytes[i + 2] == b'\n' {
                    3
                } else {
                    2
                };
                continue;
            }
            // `=XX` hex.
            if i + 2 < bytes.len() {
                if let Ok(b) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                    out.push(b);
                    i += 3;
                    continue;
                }
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_mhtml() {
        let c = MhtmlConverter;
        assert!(c.accepts(&StreamInfo::new().with_extension(".mhtml"), b""));
        assert!(c.accepts(&StreamInfo::new().with_extension(".mht"), b""));
        let magic = b"From: <saved>\r\nMIME-Version: 1.0\r\nContent-Type: multipart/related; boundary=\"x\"\r\n";
        assert!(c.accepts(&StreamInfo::new(), magic));
        assert!(!c.accepts(&StreamInfo::new().with_extension(".pdf"), b""));
    }

    #[test]
    fn extracts_and_converts_html_part() {
        let mhtml = "From: <test>\r\nMIME-Version: 1.0\r\nContent-Type: multipart/related; boundary=\"BOUND\"\r\n\r\n--BOUND\r\nContent-Type: text/html; charset=utf-8\r\nContent-Transfer-Encoding: quoted-printable\r\n\r\n<h1>Hello=20World</h1><p>Body</p>\r\n--BOUND\r\nContent-Type: image/png\r\nContent-Transfer-Encoding: base64\r\n\r\naGVsbG8=\r\n--BOUND--\r\n";
        let r = MhtmlConverter
            .convert(mhtml.as_bytes(), &StreamInfo::new(), &ConvertOptions::default())
            .unwrap();
        assert!(r.markdown.contains("# Hello World"), "got: {}", r.markdown);
        assert!(r.markdown.contains("Body"));
        // The base64 image part must NOT leak into the output.
        assert!(!r.markdown.contains("aGVsbG8"));
    }

    #[test]
    fn quoted_printable_decodes() {
        assert_eq!(decode_quoted_printable("a=20b=3D"), b"a b=");
        assert_eq!(decode_quoted_printable("line=\r\nwrap"), b"linewrap");
    }
}
