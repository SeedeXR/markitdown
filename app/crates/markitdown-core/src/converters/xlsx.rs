//! XLSX / XLS → Markdown converter.
//!
//! Port of `packages/markitdown/src/markitdown/converters/_xlsx_converter.py`
//! (both `XlsxConverter` and `XlsConverter`). Each worksheet is emitted as
//! `## <SheetName>` followed by a GFM table whose first row is the header,
//! mirroring pandas' `read_excel(...).to_html(index=False)` round-trip.
//!
//! Parsing is done with `calamine` instead of openpyxl/xlrd.

use std::io::Cursor;

use calamine::{open_workbook_auto_from_rs, Data, Reader};

use crate::text::rows_to_markdown_table;
use crate::{ConvertError, ConvertOptions, ConvertResult, Converter, StreamInfo};

const ACCEPTED_XLSX_MIME_TYPE_PREFIXES: &[&str] =
    &["application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"];
const ACCEPTED_XLSX_FILE_EXTENSIONS: &[&str] = &[".xlsx"];

const ACCEPTED_XLS_MIME_TYPE_PREFIXES: &[&str] =
    &["application/vnd.ms-excel", "application/excel"];
const ACCEPTED_XLS_FILE_EXTENSIONS: &[&str] = &[".xls"];

// OpenDocument Spreadsheet. calamine reads ODS natively, so we support it with
// the same code path — notably, the upstream Python markitdown does NOT.
const ACCEPTED_ODS_MIME_TYPE_PREFIXES: &[&str] =
    &["application/vnd.oasis.opendocument.spreadsheet"];
const ACCEPTED_ODS_FILE_EXTENSIONS: &[&str] = &[".ods"];

pub struct XlsxConverter;
pub struct XlsConverter;
pub struct OdsConverter;

fn mimetype_has_prefix(info: &StreamInfo, prefixes: &[&str]) -> bool {
    if let Some(mt) = &info.mimetype {
        let mt = mt.split(';').next().unwrap_or(mt).trim().to_ascii_lowercase();
        return prefixes.iter().any(|p| mt.starts_with(p));
    }
    false
}

/// Render a `calamine` cell exactly like the Display impl, but treat the
/// empty cell as an empty string (so `rows_to_markdown_table` pads correctly).
fn cell_to_string(cell: &Data) -> String {
    match cell {
        Data::Empty => String::new(),
        other => other.to_string(),
    }
}

/// Convert the spreadsheet bytes into Markdown: one `## sheet` + table per sheet.
fn convert_workbook(
    name: &'static str,
    data: &[u8],
    opts: &ConvertOptions,
) -> Result<ConvertResult, ConvertError> {
    // xlsx/ods are zip containers and can be decompression bombs. calamine
    // reads/inflates internally (we can't bound it from outside), so reject an
    // archive whose declared uncompressed size or entry count is over budget
    // up front. .xls is legacy OLE/BIFF (not a zip), so skip the check there.
    if data.starts_with(b"PK") {
        if let Ok(mut z) = zip::ZipArchive::new(Cursor::new(data)) {
            if !super::archive::within_budget(&mut z) {
                return Err(ConvertError::conversion(
                    name,
                    "spreadsheet archive exceeds the decompression budget",
                ));
            }
        }
    }

    let mut workbook = open_workbook_auto_from_rs(Cursor::new(data.to_vec()))
        .map_err(|e| ConvertError::conversion(name, e.to_string()))?;

    let sheets = workbook.sheet_names().to_vec();
    let total = sheets.len() as u64;
    let mut md = String::new();
    for (idx, sheet) in sheets.iter().enumerate() {
        let range = workbook
            .worksheet_range(sheet)
            .map_err(|e| ConvertError::conversion(name, e.to_string()))?;

        opts.report(crate::Progress::step(
            "xlsx",
            format!("sheet {}/{}: {sheet}", idx + 1, total),
            idx as u64 + 1,
            total,
        ));

        md.push_str("## ");
        md.push_str(sheet);
        md.push('\n');

        let rows: Vec<Vec<String>> = range
            .rows()
            .map(|row| row.iter().map(cell_to_string).collect())
            .collect();

        let table = rows_to_markdown_table(&rows);
        md.push_str(table.trim());
        md.push_str("\n\n");
    }

    Ok(ConvertResult::new(md.trim().to_string()))
}

impl Converter for XlsxConverter {
    fn name(&self) -> &'static str {
        "xlsx"
    }

    fn accepts(&self, info: &StreamInfo, _data: &[u8]) -> bool {
        info.extension_is(ACCEPTED_XLSX_FILE_EXTENSIONS)
            || mimetype_has_prefix(info, ACCEPTED_XLSX_MIME_TYPE_PREFIXES)
    }

    fn convert(
        &self,
        data: &[u8],
        _info: &StreamInfo,
        opts: &ConvertOptions,
    ) -> Result<ConvertResult, ConvertError> {
        convert_workbook("xlsx", data, opts)
    }
}

impl Converter for XlsConverter {
    fn name(&self) -> &'static str {
        "xls"
    }

    fn accepts(&self, info: &StreamInfo, _data: &[u8]) -> bool {
        info.extension_is(ACCEPTED_XLS_FILE_EXTENSIONS)
            || mimetype_has_prefix(info, ACCEPTED_XLS_MIME_TYPE_PREFIXES)
    }

    fn convert(
        &self,
        data: &[u8],
        _info: &StreamInfo,
        opts: &ConvertOptions,
    ) -> Result<ConvertResult, ConvertError> {
        convert_workbook("xls", data, opts)
    }
}

impl Converter for OdsConverter {
    fn name(&self) -> &'static str {
        "ods"
    }

    fn accepts(&self, info: &StreamInfo, _data: &[u8]) -> bool {
        info.extension_is(ACCEPTED_ODS_FILE_EXTENSIONS)
            || mimetype_has_prefix(info, ACCEPTED_ODS_MIME_TYPE_PREFIXES)
    }

    fn convert(
        &self,
        data: &[u8],
        _info: &StreamInfo,
        opts: &ConvertOptions,
    ) -> Result<ConvertResult, ConvertError> {
        convert_workbook("ods", data, opts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ods_converter_accepts_ods_only() {
        let c = OdsConverter;
        assert!(c.accepts(&StreamInfo::new().with_extension(".ods"), b""));
        assert!(c.accepts(
            &StreamInfo::new().with_mimetype("application/vnd.oasis.opendocument.spreadsheet"),
            b""
        ));
        assert!(!c.accepts(&StreamInfo::new().with_extension(".xlsx"), b""));
    }

    #[test]
    fn xlsx_xls_accept_their_own_extensions() {
        assert!(XlsxConverter.accepts(&StreamInfo::new().with_extension(".xlsx"), b""));
        assert!(XlsConverter.accepts(&StreamInfo::new().with_extension(".xls"), b""));
        assert!(!XlsxConverter.accepts(&StreamInfo::new().with_extension(".ods"), b""));
    }
}
