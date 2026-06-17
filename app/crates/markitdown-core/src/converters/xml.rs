//! Generic structured-XML → Markdown converter.
//!
//! For arbitrary XML that is *not* an RSS/Atom feed (those are handled earlier
//! by [`super::rss::RssConverter`]) we want something better than dumping the
//! raw tags as plain text. Two shapes cover most real documents:
//!
//! * **Record-style** — a parent whose children are ≥2 repetitions of the same
//!   tag, each with leaf (text-only) fields. These render as a GFM table whose
//!   columns are the union of the records' field tags and attributes.
//! * **Everything else** — a readable nested outline, one bullet per element
//!   (`- **tag** (attr=val): text`), recursing into children.

use crate::{text::decode_text, ConvertError, ConvertOptions, ConvertResult, Converter, StreamInfo};
use quick_xml::events::Event;
use quick_xml::reader::Reader;

pub struct XmlConverter;

const ACCEPTED_EXTENSIONS: &[&str] = &[".xml"];
const ACCEPTED_MIME_PREFIXES: &[&str] = &["text/xml", "application/xml"];
/// Max element nesting. Bounds both the parsed tree depth and the recursive
/// render, so a deeply-nested document can't overflow the stack. Real-world XML
/// rarely exceeds a few dozen levels.
const MAX_DEPTH: usize = 256;

/// A minimal XML element tree carrying tag name, attributes, text and children.
#[derive(Debug, Default)]
struct Node {
    name: String,
    attrs: Vec<(String, String)>,
    text: String,
    children: Vec<Node>,
}

impl Node {
    /// An element is a *leaf* if it has no child elements — its value is text.
    fn is_leaf(&self) -> bool {
        self.children.is_empty()
    }

    fn trimmed_text(&self) -> String {
        self.text.split_whitespace().collect::<Vec<_>>().join(" ")
    }
}

impl Converter for XmlConverter {
    fn name(&self) -> &'static str {
        "xml"
    }

    fn accepts(&self, info: &StreamInfo, _data: &[u8]) -> bool {
        if info.extension_is(ACCEPTED_EXTENSIONS) {
            return true;
        }
        if let Some(mt) = &info.mimetype {
            let mt = mt.split(';').next().unwrap_or(mt).trim().to_ascii_lowercase();
            return ACCEPTED_MIME_PREFIXES.iter().any(|p| mt.starts_with(p));
        }
        false
    }

    fn convert(
        &self,
        data: &[u8],
        info: &StreamInfo,
        _opts: &ConvertOptions,
    ) -> Result<ConvertResult, ConvertError> {
        let xml = decode_text(data, info);
        let root = parse_tree(&xml).map_err(|e| ConvertError::conversion("xml", e))?;
        // The document root we care about is the single top-level element.
        let doc = root.children.into_iter().find(|n| !n.name.is_empty());
        let doc = doc.ok_or_else(|| ConvertError::conversion("xml", "no XML elements found"))?;

        let title = doc.name.clone();
        let mut md = format!("# {title}\n");
        render_element(&doc, &mut md);

        let mut result = ConvertResult::new(md);
        result = result.with_title(title);
        Ok(result)
    }
}

/// Render an element: a record-style table when its children repeat, otherwise
/// a nested outline.
fn render_element(node: &Node, md: &mut String) {
    if let Some(table) = record_table(node) {
        md.push('\n');
        md.push_str(&table);
        return;
    }
    md.push('\n');
    for child in &node.children {
        outline(child, 0, md);
    }
}

/// If `node`'s children are ≥2 elements sharing one tag and every record is
/// composed only of leaf fields (text children) and/or attributes, build a GFM
/// table. Returns `None` when the shape doesn't fit.
fn record_table(node: &Node) -> Option<String> {
    let records: Vec<&Node> = node.children.iter().collect();
    if records.len() < 2 {
        return None;
    }
    let first = &records[0].name;
    if !records.iter().all(|r| &r.name == first) {
        return None;
    }
    // Every record's children must be leaves (a flat record), and a record
    // must carry at least one field (a leaf child or an attribute).
    if !records
        .iter()
        .all(|r| r.children.iter().all(|c| c.is_leaf()) && (!r.children.is_empty() || !r.attrs.is_empty()))
    {
        return None;
    }

    // Column order: attributes (prefixed `@`) first, then child field tags, in
    // first-seen order across all records.
    let mut columns: Vec<String> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for r in &records {
        for (k, _) in &r.attrs {
            let col = format!("@{k}");
            if seen.insert(col.clone()) {
                columns.push(col);
            }
        }
        for c in &r.children {
            if seen.insert(c.name.clone()) {
                columns.push(c.name.clone());
            }
        }
    }
    if columns.is_empty() {
        return None;
    }

    let mut out = String::new();
    out.push_str("| ");
    out.push_str(&columns.iter().map(|c| escape_cell(c)).collect::<Vec<_>>().join(" | "));
    out.push_str(" |\n| ");
    out.push_str(&columns.iter().map(|_| "---").collect::<Vec<_>>().join(" | "));
    out.push_str(" |\n");
    for r in &records {
        let cells: Vec<String> = columns
            .iter()
            .map(|col| {
                if let Some(attr) = col.strip_prefix('@') {
                    r.attrs
                        .iter()
                        .find(|(k, _)| k == attr)
                        .map(|(_, v)| escape_cell(v))
                        .unwrap_or_default()
                } else {
                    r.children
                        .iter()
                        .find(|c| &c.name == col)
                        .map(|c| escape_cell(&c.trimmed_text()))
                        .unwrap_or_default()
                }
            })
            .collect();
        out.push_str("| ");
        out.push_str(&cells.join(" | "));
        out.push_str(" |\n");
    }
    Some(out)
}

/// Render an element as a nested bullet outline.
fn outline(node: &Node, depth: usize, md: &mut String) {
    let indent = "  ".repeat(depth);
    let attrs = if node.attrs.is_empty() {
        String::new()
    } else {
        let pairs: Vec<String> = node
            .attrs
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect();
        format!(" ({})", pairs.join(", "))
    };
    let text = node.trimmed_text();
    if node.is_leaf() {
        if text.is_empty() && node.attrs.is_empty() {
            md.push_str(&format!("{indent}- **{}**\n", node.name));
        } else if text.is_empty() {
            md.push_str(&format!("{indent}- **{}**{attrs}\n", node.name));
        } else {
            md.push_str(&format!("{indent}- **{}**{attrs}: {text}\n", node.name));
        }
    } else {
        if text.is_empty() {
            md.push_str(&format!("{indent}- **{}**{attrs}\n", node.name));
        } else {
            md.push_str(&format!("{indent}- **{}**{attrs}: {text}\n", node.name));
        }
        // A repeating, flat child group renders as a nested table.
        if let Some(table) = record_table(node) {
            for line in table.lines() {
                md.push_str(&format!("{indent}  {line}\n"));
            }
        } else {
            for child in &node.children {
                outline(child, depth + 1, md);
            }
        }
    }
}

/// Escape a value for inclusion in a single GFM table cell.
fn escape_cell(s: &str) -> String {
    s.replace('\\', "\\\\").replace('|', "\\|").replace('\n', " ").trim().to_string()
}

/// Parse the XML into a tree, stripping namespace prefixes from tag names.
fn parse_tree(xml: &str) -> Result<Node, String> {
    let mut reader = Reader::from_str(xml);
    let mut current = Node {
        name: String::new(),
        ..Default::default()
    };
    let mut stack: Vec<Node> = Vec::new();

    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => {
                // Bound nesting so a deeply-nested document can't overflow the
                // stack when the tree is later traversed/rendered recursively.
                if stack.len() >= MAX_DEPTH {
                    return Err(format!("XML nesting exceeds {MAX_DEPTH} levels"));
                }
                let name = local_name(e.name().as_ref());
                let attrs = read_attrs(&e);
                stack.push(std::mem::replace(
                    &mut current,
                    Node {
                        name,
                        attrs,
                        ..Default::default()
                    },
                ));
            }
            Ok(Event::End(_)) => {
                if let Some(mut parent) = stack.pop() {
                    parent.children.push(std::mem::take(&mut current));
                    current = parent;
                } else {
                    break;
                }
            }
            Ok(Event::Empty(e)) => {
                let name = local_name(e.name().as_ref());
                let attrs = read_attrs(&e);
                current.children.push(Node {
                    name,
                    attrs,
                    ..Default::default()
                });
            }
            Ok(Event::Text(e)) => {
                if let Ok(t) = e.decode() {
                    current.text.push_str(t.as_ref());
                }
            }
            Ok(Event::CData(e)) => {
                if let Ok(t) = std::str::from_utf8(e.as_ref()) {
                    current.text.push_str(t);
                }
            }
            Ok(Event::GeneralRef(e)) => {
                if let Ok(Some(ch)) = e.resolve_char_ref() {
                    current.text.push(ch);
                } else if let Ok(name) = e.decode() {
                    match name.as_ref() {
                        "amp" => current.text.push('&'),
                        "lt" => current.text.push('<'),
                        "gt" => current.text.push('>'),
                        "quot" => current.text.push('"'),
                        "apos" => current.text.push('\''),
                        _ => {}
                    }
                }
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(e) => return Err(e.to_string()),
        }
    }
    Ok(current)
}

fn read_attrs(e: &quick_xml::events::BytesStart) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for attr in e.attributes().flatten() {
        let key = local_name(attr.key.as_ref());
        let val = attr
            .normalized_value(quick_xml::XmlVersion::Implicit1_0)
            .map(|v| v.into_owned())
            .unwrap_or_else(|_| String::from_utf8_lossy(&attr.value).into_owned());
        out.push((key, val));
    }
    out
}

fn local_name(raw: &[u8]) -> String {
    let s = String::from_utf8_lossy(raw);
    match s.rsplit_once(':') {
        Some((_, local)) => local.to_string(),
        None => s.into_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn convert(xml: &str) -> String {
        XmlConverter
            .convert(xml.as_bytes(), &StreamInfo::new().with_extension(".xml"), &ConvertOptions::default())
            .unwrap()
            .markdown
    }

    #[test]
    fn accepts_xml() {
        assert!(XmlConverter.accepts(&StreamInfo::new().with_extension(".xml"), b""));
        assert!(!XmlConverter.accepts(&StreamInfo::new().with_extension(".txt"), b""));
    }

    #[test]
    fn record_style_becomes_table() {
        let xml = r#"<books>
            <book id="1"><title>A</title><author>X</author></book>
            <book id="2"><title>B</title><author>Y</author></book>
        </books>"#;
        let md = convert(xml);
        assert!(md.contains("# books"), "got: {md}");
        assert!(md.contains("| @id | title | author |"), "got: {md}");
        assert!(md.contains("| 1 | A | X |"), "got: {md}");
        assert!(md.contains("| 2 | B | Y |"), "got: {md}");
    }

    #[test]
    fn nested_becomes_outline() {
        let xml = r#"<config>
            <server><host>localhost</host><port>8080</port></server>
            <debug>true</debug>
        </config>"#;
        let md = convert(xml);
        assert!(md.contains("# config"), "got: {md}");
        assert!(md.contains("- **server**"), "got: {md}");
        assert!(md.contains("- **host**: localhost"), "got: {md}");
        assert!(md.contains("- **debug**: true"), "got: {md}");
    }

    #[test]
    fn cells_escape_pipes() {
        let xml = r#"<rows><row><a>x|y</a></row><row><a>z</a></row></rows>"#;
        let md = convert(xml);
        assert!(md.contains("x\\|y"), "got: {md}");
    }
}
