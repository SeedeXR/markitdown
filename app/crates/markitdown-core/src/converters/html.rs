//! HTML → Markdown converter. Port of `_html_converter.py` / `_markdownify.py`.
use crate::{text::decode_text, Converter, ConvertError, ConvertOptions, ConvertResult, StreamInfo};
use htmd::HtmlToMarkdown;
use scraper::{Html, Selector};

const ACCEPTED_MIME_PREFIXES: &[&str] = &["text/html", "application/xhtml"];
const ACCEPTED_EXTENSIONS: &[&str] = &[".html", ".htm", ".xhtml"];

pub struct HtmlConverter;

impl Converter for HtmlConverter {
    fn name(&self) -> &'static str {
        "html"
    }

    fn accepts(&self, info: &StreamInfo, _data: &[u8]) -> bool {
        if info.extension_is(ACCEPTED_EXTENSIONS) {
            return true;
        }
        if let Some(mt) = &info.mimetype {
            let mt = mt.split(';').next().unwrap_or(mt).trim().to_ascii_lowercase();
            if ACCEPTED_MIME_PREFIXES.iter().any(|p| mt.starts_with(p)) {
                return true;
            }
        }
        false
    }

    fn convert(
        &self,
        data: &[u8],
        info: &StreamInfo,
        opts: &ConvertOptions,
    ) -> Result<ConvertResult, ConvertError> {
        // A served page often omits `charset` from its Content-Type and
        // declares it in the document instead. Without this a Windows-1252 or
        // Shift-JIS page falls through to statistical detection and comes out
        // mojibake.
        let html = match info.charset {
            Some(_) => decode_text(data, info),
            None => match sniff_meta_charset(data) {
                Some(cs) => decode_text(data, &info.clone().with_charset(&cs)),
                None => decode_text(data, info),
            },
        };
        let (markdown, title) = html_to_markdown(&html, opts.keep_data_uris);
        let mut result = ConvertResult::new(markdown);
        if let Some(t) = title {
            result = result.with_title(t);
        }
        Ok(result)
    }
}

/// Convert an HTML document (or fragment) to Markdown, returning the Markdown
/// body and the `<title>` text when present.
///
/// Mirrors the Python `_CustomMarkdownify` pipeline: strip `script`/`style`,
/// render the `<body>` (or the whole document when there is no body) to
/// Markdown, and—unless `keep_data_uris`—truncate long `data:` URIs.
///
/// Shared by the Wikipedia / Bing / YouTube / RSS converters (and, later, EPUB).
pub(crate) fn html_to_markdown(html: &str, keep_data_uris: bool) -> (String, Option<String>) {
    let doc = Html::parse_document(html);
    let title = extract_title_doc(&doc);
    // Narrow to the article body when the page marks one up, so a real website
    // yields its content instead of its navigation.
    let markdown = match main_content(&doc) {
        Some(inner) => render_fragment(&inner, keep_data_uris),
        None => render_fragment(html, keep_data_uris),
    };
    (markdown, title)
}

/// Selectors for a page's main content, most specific first. These are the
/// standard semantic containers; nothing is guessed from class names, so a page
/// without them simply falls back to the whole document.
const MAIN_CONTENT_SELECTORS: &[&str] = &["main", "[role=\"main\"]", "article"];

/// Total visible text length of a document, used to judge whether a candidate
/// container actually holds the page's content.
fn text_len(doc: &Html) -> usize {
    let Ok(sel) = Selector::parse("body") else {
        return 0;
    };
    doc.select(&sel)
        .next()
        .map(|b| b.text().map(str::len).sum())
        .unwrap_or(0)
}

/// Inner HTML of the page's main-content container, when one exists and holds
/// enough of the page to be believable.
///
/// The size check matters: plenty of sites wrap a teaser or a comment widget in
/// `<article>`, and swapping the whole page for that would silently drop the
/// content we were asked to convert. Falling back to the full document is
/// always safe — just noisier.
fn main_content(doc: &Html) -> Option<String> {
    let total = text_len(doc);
    for pattern in MAIN_CONTENT_SELECTORS {
        let Ok(sel) = Selector::parse(pattern) else {
            continue;
        };
        // Only trust an unambiguous container; several <article>s on a page is
        // an index, not an article.
        let mut matches = doc.select(&sel);
        let (Some(el), None) = (matches.next(), matches.next()) else {
            continue;
        };
        let len: usize = el.text().map(str::len).sum();
        if len >= 200 && (total == 0 || len * 4 >= total) {
            return Some(el.inner_html());
        }
    }
    None
}

/// Target of a `<meta http-equiv="refresh" content="0; url=…">` redirect.
///
/// These are invisible to HTTP redirect handling but common on documentation
/// sites that have moved a page, and without following them a URL a browser
/// resolves to an article converts to "Click here to be redirected."
///
/// Only an immediate redirect (delay 0 or 1) counts — a longer delay is a
/// "you'll be moved shortly" notice on a page that has its own content. The
/// scheme is checked by the caller.
pub(crate) fn meta_refresh_target(html: &str) -> Option<String> {
    // Redirect stubs put the `<meta refresh>` inside `<noscript>` — that is
    // exactly who it is for. The HTML parser assumes scripting is enabled and
    // so treats `<noscript>` content as raw *text*, meaning the meta never
    // becomes an element and a selector can never see it. Dropping just the
    // wrapper tags puts it back in the DOM.
    let unwrapped = strip_noscript_tags(html);
    let doc = Html::parse_document(&unwrapped);
    let sel = Selector::parse("meta[http-equiv]").ok()?;
    // NOTE: every rejection below must `continue`, never `?`. A page can carry
    // several refresh metas and the unusable one is often first; bailing out of
    // the whole function on it loses the real redirect.
    for el in doc.select(&sel) {
        let Some(equiv) = el.value().attr("http-equiv") else {
            continue;
        };
        if !equiv.eq_ignore_ascii_case("refresh") {
            continue;
        }
        let Some(content) = el.value().attr("content") else {
            continue;
        };
        // "0; url=x", "0;url=x", or a bare "url=x" with no delay at all —
        // browsers accept all three.
        let rest = match content.split_once(';') {
            Some((delay, rest)) => {
                if !matches!(delay.trim().parse::<f32>(), Ok(d) if d <= 1.0) {
                    continue;
                }
                rest
            }
            None => content,
        };
        let rest = rest.trim();
        let Some(eq) = rest.find('=') else {
            continue;
        };
        if !rest[..eq].trim().eq_ignore_ascii_case("url") {
            continue;
        }
        let url = rest[eq + 1..].trim().trim_matches(['"', '\'']).trim();
        if !url.is_empty() {
            return Some(url.to_string());
        }
    }
    None
}

/// Remove `<noscript>` / `</noscript>` tags (not their contents), so markup
/// inside them is parsed as markup. `to_ascii_lowercase` preserves byte
/// offsets, so the lowercased copy can index the original safely.
fn strip_noscript_tags(html: &str) -> String {
    let lower = html.to_ascii_lowercase();
    let mut out = String::with_capacity(html.len());
    let mut i = 0;
    while i < html.len() {
        let open = lower[i..].find("<noscript").map(|p| i + p);
        let close = lower[i..].find("</noscript").map(|p| i + p);
        let Some(start) = open.into_iter().chain(close).min() else {
            out.push_str(&html[i..]);
            break;
        };
        out.push_str(&html[i..start]);
        match tag_end(&html[start..]) {
            Some(end) => i = start + end + 1,
            // Unterminated tag: nothing further is parseable.
            None => break,
        }
    }
    out
}

/// Byte offset of the `>` that closes the tag starting at `s[0]`, ignoring any
/// `>` inside a quoted attribute value (`<noscript data-x="1>2">`).
fn tag_end(s: &str) -> Option<usize> {
    let mut quote: Option<char> = None;
    for (i, c) in s.char_indices() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None => match c {
                '"' | '\'' => quote = Some(c),
                '>' => return Some(i),
                _ => {}
            },
        }
    }
    None
}

/// Extract a `<meta charset>` / `<meta http-equiv="content-type">` declaration
/// from the head of a document. Only the first 4 KiB is scanned, which is where
/// the HTML spec requires the declaration to appear.
fn sniff_meta_charset(data: &[u8]) -> Option<String> {
    let head = &data[..data.len().min(4096)];
    let text = String::from_utf8_lossy(head).to_ascii_lowercase();
    // Every occurrence, not just the first: the word "charset" routinely shows
    // up earlier in a script name or variable, and stopping there means the
    // real declaration is never seen.
    for (i, _) in text.match_indices("charset") {
        let rest = &text[i + "charset".len()..];
        let Some(rest) = rest.trim_start().strip_prefix('=') else {
            continue;
        };
        let rest = rest.trim_start().trim_start_matches(['"', '\'']);
        let label: String = rest
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
            .collect();
        // Reject junk: only labels encoding_rs actually knows are useful.
        if encoding_rs::Encoding::for_label(label.as_bytes()).is_some() {
            return Some(label);
        }
    }
    None
}

/// Render an arbitrary HTML fragment to Markdown (no `<title>` extraction).
/// Used by converters that pass in an already-selected DOM subtree's inner HTML.
pub(crate) fn fragment_to_markdown(html: &str, keep_data_uris: bool) -> String {
    render_fragment(html, keep_data_uris)
}

/// Elements that never carry document content.
///
/// This list is shared with the EPUB/MOBI/MHTML converters, so it holds only
/// tags that are non-content *everywhere*. Deliberately absent:
/// `header`/`footer` (an article's byline and date live there), and
/// `aside`/`form` — in an ebook an `<aside>` is a footnote or pull-quote and a
/// `<form>` sometimes wraps a real table, so skipping them would delete book
/// content to tidy up web pages.
const SKIP_TAGS: &[&str] = &[
    "script", "style", "noscript", "template", "svg", "canvas", "iframe", "button", "select",
    "nav", "dialog",
];

fn render_fragment(html: &str, keep_data_uris: bool) -> String {
    let converter = HtmlToMarkdown::builder()
        .skip_tags(SKIP_TAGS.to_vec())
        .build();
    let md = converter.convert(html).unwrap_or_default();
    let md = md.trim().to_string();
    if keep_data_uris {
        md
    } else {
        truncate_data_uris(&md)
    }
}

/// Extract the `<title>` text from an already-parsed document.
pub(crate) fn extract_title_doc(doc: &Html) -> Option<String> {
    let sel = Selector::parse("title").ok()?;
    let el = doc.select(&sel).next()?;
    let text: String = el.text().collect();
    let text = text.trim();
    if text.is_empty() {
        None
    } else {
        Some(text.to_string())
    }
}

/// Replicate Python's `src.split(",")[0] + "..."` truncation for `data:` URIs.
///
/// We scan the rendered Markdown for `data:` substrings and, when one is
/// followed by a comma, drop everything from the comma onward, replacing it
/// with `...`. This keeps the mime/encoding prefix (e.g. `data:image/png;base64`)
/// and appends `...` exactly as the Python converter does.
fn truncate_data_uris(md: &str) -> String {
    let bytes = md.as_bytes();
    let mut out = String::with_capacity(md.len());
    let mut i = 0;
    while i < bytes.len() {
        if md[i..].starts_with("data:") {
            // Find the end of the data URI payload. In Markdown the URI is
            // bounded by `)`, whitespace, or a quote (for titles).
            let mut j = i;
            let mut comma: Option<usize> = None;
            while j < bytes.len() {
                let c = bytes[j];
                if c == b')' || c == b'"' || c == b'\'' || c.is_ascii_whitespace() {
                    break;
                }
                if c == b',' && comma.is_none() {
                    comma = Some(j);
                }
                j += 1;
            }
            match comma {
                Some(cidx) => {
                    out.push_str(&md[i..cidx]);
                    out.push_str("...");
                }
                None => out.push_str(&md[i..j]),
            }
            i = j;
        } else {
            // Advance one UTF-8 char.
            let ch = md[i..].chars().next().unwrap();
            out.push(ch);
            i += ch.len_utf8();
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncates_data_uri_when_not_kept() {
        let html = r#"<html><body><img src="data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAAC" alt="x"></body></html>"#;
        let (md, _) = html_to_markdown(html, false);
        assert!(md.contains("data:image/png;base64..."), "got: {md}");
        assert!(!md.contains("iVBORw0KGgo"), "payload should be gone: {md}");
    }

    #[test]
    fn keeps_data_uri_when_requested() {
        let html = r#"<html><body><img src="data:image/png;base64,iVBORw0KGgoAAAANSU" alt="x"></body></html>"#;
        let (md, _) = html_to_markdown(html, true);
        assert!(md.contains("data:image/png;base64,iVBORw0KGgoAAAANSU"), "got: {md}");
    }

    /// Body text long enough to clear `main_content`'s 200-char threshold.
    fn filler(tag: &str) -> String {
        format!(
            "<{tag}>{}</{tag}>",
            "This is a paragraph of real article content that a reader came for. ".repeat(5)
        )
    }

    #[test]
    fn main_container_wins_over_page_furniture() {
        let html = format!(
            "<html><body><nav><a href='/x'>NAVLINK</a></nav>\
             <main>{}</main>\
             <aside>SIDEBARJUNK</aside></body></html>",
            filler("p")
        );
        let (md, _) = html_to_markdown(&html, false);
        assert!(md.contains("real article content"), "content lost: {md}");
        assert!(!md.contains("NAVLINK"), "nav leaked: {md}");
        assert!(!md.contains("SIDEBARJUNK"), "aside leaked: {md}");
    }

    #[test]
    fn small_article_teaser_does_not_replace_the_page() {
        // A short <article> next to a much larger body is a teaser, not the
        // content — swapping the page for it would silently drop everything.
        let html = format!(
            "<html><body><article>tiny teaser</article><div>{}</div></body></html>",
            "Long body text that is clearly the real content of this page. ".repeat(20)
        );
        let (md, _) = html_to_markdown(&html, false);
        assert!(md.contains("real content of this page"), "body dropped: {md}");
    }

    #[test]
    fn multiple_articles_are_treated_as_an_index() {
        // An index page lists many <article>s; picking the first would drop all
        // the rest.
        let html = format!(
            "<html><body>{}{}</body></html>",
            filler("article"),
            filler("article")
        );
        let (md, _) = html_to_markdown(&html, false);
        // Both survive because we fell back to the whole document.
        assert!(md.matches("real article content").count() >= 2, "got: {md}");
    }

    #[test]
    fn pages_without_semantic_containers_still_convert() {
        let html = "<html><body><div><h1>Title</h1><p>Some body text.</p></div></body></html>";
        let (md, _) = html_to_markdown(html, false);
        assert!(md.contains("# Title"), "{md}");
        assert!(md.contains("Some body text."), "{md}");
    }

    #[test]
    fn scripts_and_styles_never_leak_into_the_markdown() {
        let html = "<html><body><script>var SECRET=1;</script>\
                    <style>.a{color:red}</style><p>visible</p></body></html>";
        let (md, _) = html_to_markdown(html, false);
        assert!(md.contains("visible"));
        assert!(!md.contains("SECRET"), "script leaked: {md}");
        assert!(!md.contains("color:red"), "style leaked: {md}");
    }

    #[test]
    fn meta_charset_is_detected() {
        assert_eq!(
            sniff_meta_charset(br#"<html><head><meta charset="windows-1252">"#).as_deref(),
            Some("windows-1252")
        );
        assert_eq!(
            sniff_meta_charset(
                br#"<meta http-equiv="Content-Type" content="text/html; charset=Shift_JIS">"#
            )
            .as_deref(),
            Some("shift_jis")
        );
        // Unknown or absent labels must not produce a bogus hint.
        assert_eq!(sniff_meta_charset(b"<html><head><title>x</title>"), None);
        assert_eq!(sniff_meta_charset(br#"<meta charset="not-a-charset">"#), None);
    }

    #[test]
    fn meta_charset_decodes_a_non_utf8_page() {
        // "café" in windows-1252 (0xE9 is not valid UTF-8), declared in-document
        // with no HTTP charset header — the case that used to come out mojibake.
        let mut html = br#"<html><head><meta charset="windows-1252"></head><body><p>caf"#.to_vec();
        html.push(0xE9);
        html.extend_from_slice(b"</p></body></html>");
        let out = HtmlConverter
            .convert(
                &html,
                &StreamInfo::new().with_extension(".html"),
                &ConvertOptions::default(),
            )
            .unwrap();
        assert!(out.markdown.contains("café"), "got: {}", out.markdown);
    }

    #[test]
    fn meta_refresh_target_is_followed_only_when_immediate() {
        assert_eq!(
            meta_refresh_target(r#"<meta http-equiv="refresh" content="0; url=/new/page">"#)
                .as_deref(),
            Some("/new/page")
        );
        // Case and quoting variations seen in the wild.
        assert_eq!(
            meta_refresh_target(r#"<meta HTTP-EQUIV="REFRESH" content="0;URL='/x'">"#).as_deref(),
            Some("/x")
        );
        // A long delay is a notice on a real page, not a redirect.
        assert_eq!(
            meta_refresh_target(r#"<meta http-equiv="refresh" content="30; url=/x">"#),
            None
        );
        // Not a refresh at all.
        assert_eq!(
            meta_refresh_target(r#"<meta http-equiv="content-type" content="text/html">"#),
            None
        );
        assert_eq!(meta_refresh_target("<html><body>hi</body></html>"), None);
    }

    #[test]
    fn a_bad_refresh_meta_does_not_hide_a_good_one() {
        // REGRESSION: `?` on a malformed meta returned None for the WHOLE
        // function, so a valid redirect after a broken one was never seen.
        let html = r#"<meta http-equiv="refresh" content="600">
                      <meta http-equiv="refresh">
                      <meta http-equiv="refresh" content="0; url=/real">"#;
        assert_eq!(meta_refresh_target(html).as_deref(), Some("/real"));
        // A bare "url=x" with no delay is honoured by browsers too.
        assert_eq!(
            meta_refresh_target(r#"<meta http-equiv="refresh" content="url=/x">"#).as_deref(),
            Some("/x")
        );
        // Spaces around the "=" are tolerated.
        assert_eq!(
            meta_refresh_target(r#"<meta http-equiv="refresh" content="0; url = /y">"#).as_deref(),
            Some("/y")
        );
    }

    #[test]
    fn charset_is_found_after_an_earlier_false_match() {
        // REGRESSION: only the FIRST literal "charset" was examined, so the
        // word appearing in a script name or variable killed detection and the
        // page came out mojibake — the exact failure this feature prevents.
        assert_eq!(
            sniff_meta_charset(
                br#"<script>var charset_x=1</script><meta charset="shift_jis">"#
            )
            .as_deref(),
            Some("shift_jis")
        );
        assert_eq!(
            sniff_meta_charset(
                br#"<script src="/js/charset-detect.js"></script><meta charset="windows-1252">"#
            )
            .as_deref(),
            Some("windows-1252")
        );
    }

    #[test]
    fn tag_end_ignores_a_bracket_inside_a_quoted_attribute() {
        assert_eq!(
            strip_noscript_tags(r#"a<noscript data-x="1>2">b</noscript>c"#),
            "abc"
        );
    }

    #[test]
    fn ebook_content_tags_are_not_skipped() {
        // SKIP_TAGS is shared with EPUB/MOBI/MHTML, where <aside> is a footnote
        // and <form> can wrap a real table — dropping them deletes book content.
        let html = "<body><aside>footnote text</aside><form><p>tabular</p></form></body>";
        let md = fragment_to_markdown(html, false);
        assert!(md.contains("footnote text"), "aside dropped: {md}");
        assert!(md.contains("tabular"), "form dropped: {md}");
    }

    #[test]
    fn meta_refresh_inside_noscript_is_found() {
        // Redirect stubs put the meta in <noscript> for exactly the non-JS
        // clients we are. The HTML parser treats <noscript> content as raw text,
        // so without unwrapping it the redirect is invisible and the URL
        // converts to "Click here to be redirected."
        let html = r#"<!doctype html><title>Redirect</title>
            <script>window.location.replace("https://x/")</script>
            <noscript>
              <meta http-equiv="refresh" content="0; url=https://example.com/real/">
            </noscript>
            <p><a href="https://example.com/real/">Click here</a></p>"#;
        assert_eq!(
            meta_refresh_target(html).as_deref(),
            Some("https://example.com/real/")
        );
    }

    #[test]
    fn strip_noscript_keeps_content_and_handles_case() {
        assert_eq!(strip_noscript_tags("a<NOSCRIPT>b</NoScript>c"), "abc");
        assert_eq!(strip_noscript_tags("<noscript foo=1>x</noscript>"), "x");
        // No noscript at all is returned unchanged.
        assert_eq!(strip_noscript_tags("<p>hi</p>"), "<p>hi</p>");
        // An unterminated tag must not panic or loop.
        assert_eq!(strip_noscript_tags("ok<noscript"), "ok");
    }

    #[test]
    fn extracts_title() {
        let html = "<html><head><title>Hello World</title></head><body><p>hi</p></body></html>";
        let (_, title) = html_to_markdown(html, false);
        assert_eq!(title.as_deref(), Some("Hello World"));
    }
}
