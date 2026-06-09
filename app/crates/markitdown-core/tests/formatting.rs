//! Formatting-fidelity regression tests: the converters must translate document
//! structure into Markdown — headings, **bold**, *italic*, and GFM tables.
//!
//! DOCX bold/italic/heading/table is exercised with a SYNTHETIC document built
//! in-test (the shipped fixtures don't contain bold/italic runs), so the
//! `w:b`→`**` / `w:i`→`*` / `pStyle Heading`→`#` / `w:tbl`→table paths are
//! genuinely covered. XLSX/PPTX assert against the real fixtures.

use markitdown_core::{MarkItDown, StreamInfo};
use std::io::Write;

/// Build a minimal but valid .docx (zip with word/document.xml) containing a
/// Heading-1 paragraph, a bold run, an italic run, and a 2x2 table.
fn synthetic_docx() -> Vec<u8> {
    let document_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
<w:body>
  <w:p><w:pPr><w:pStyle w:val="Heading1"/></w:pPr><w:r><w:t>Big Heading</w:t></w:r></w:p>
  <w:p>
    <w:r><w:rPr><w:b/></w:rPr><w:t>BoldText</w:t></w:r>
    <w:r><w:t> and </w:t></w:r>
    <w:r><w:rPr><w:i/></w:rPr><w:t>ItalicText</w:t></w:r>
  </w:p>
  <w:tbl>
    <w:tr><w:tc><w:p><w:r><w:t>H1</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>H2</w:t></w:r></w:p></w:tc></w:tr>
    <w:tr><w:tc><w:p><w:r><w:t>a</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>b</w:t></w:r></w:p></w:tc></w:tr>
  </w:tbl>
</w:body>
</w:document>"#;
    let styles_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:style w:type="paragraph" w:styleId="Heading1"><w:name w:val="heading 1"/></w:style>
</w:styles>"#;
    let mut buf = std::io::Cursor::new(Vec::new());
    {
        let mut zw = zip::ZipWriter::new(&mut buf);
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        zw.start_file("word/document.xml", opts).unwrap();
        zw.write_all(document_xml.as_bytes()).unwrap();
        zw.start_file("word/styles.xml", opts).unwrap();
        zw.write_all(styles_xml.as_bytes()).unwrap();
        zw.finish().unwrap();
    }
    buf.into_inner()
}

#[test]
fn docx_translates_heading_bold_italic_table() {
    let data = synthetic_docx();
    let md = MarkItDown::new()
        .convert_bytes(&data, StreamInfo::new().with_extension(".docx"))
        .unwrap()
        .markdown;
    assert!(md.contains("# Big Heading"), "heading -> #, got:\n{md}");
    assert!(md.contains("**BoldText**"), "w:b -> **bold**, got:\n{md}");
    assert!(md.contains("*ItalicText*"), "w:i -> *italic*, got:\n{md}");
    assert!(md.contains("| H1 | H2 |"), "table header row, got:\n{md}");
    assert!(md.contains("| --- | --- |"), "GFM table separator, got:\n{md}");
    assert!(md.contains("| a | b |"), "table body row, got:\n{md}");
}

#[test]
fn xlsx_sheets_become_markdown_tables() {
    let md = MarkItDown::new()
        .convert_path(fixture("test.xlsx"))
        .unwrap()
        .markdown;
    assert!(md.contains("## "), "sheet heading expected");
    assert!(md.contains("| --- |"), "GFM table expected");
}

#[test]
fn pptx_has_slide_markers_headings_and_table() {
    let md = MarkItDown::new()
        .convert_path(fixture("test.pptx"))
        .unwrap()
        .markdown;
    assert!(md.contains("<!-- Slide number:"), "slide markers expected");
    assert!(md.contains("# "), "slide title heading expected");
    assert!(md.contains("| --- |"), "a slide table expected");
}

fn fixture(name: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../packages/markitdown/tests/test_files")
        .join(name)
}
