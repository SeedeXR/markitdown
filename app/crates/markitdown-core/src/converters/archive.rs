//! Shared guards against zip "decompression bomb" inputs.
//!
//! OOXML (docx/pptx/xlsx) and EPUB are zip containers; a small file can declare
//! parts that inflate to many GB. These helpers cap per-entry and per-archive
//! decompression so a crafted document can't exhaust host memory.

use std::io::{Read, Seek};

/// Largest single entry we will decompress.
pub(crate) const MAX_ENTRY_BYTES: u64 = 128 * 1024 * 1024;
/// Largest total decompressed payload we will hold from one archive.
pub(crate) const MAX_TOTAL_BYTES: u64 = 512 * 1024 * 1024;
/// Most entries we will enumerate/expand from one archive.
pub(crate) const MAX_ENTRIES: usize = 4096;

/// Read a zip entry by name with a hard size cap. Returns `None` if the entry
/// is missing, unreadable, or larger than `cap` (the declared size is checked
/// first, then the actual read is `take`-bounded so a lying header can't win).
pub(crate) fn read_capped<R: Read + Seek>(
    zip: &mut zip::ZipArchive<R>,
    name: &str,
    cap: u64,
) -> Option<Vec<u8>> {
    let mut file = zip.by_name(name).ok()?;
    if file.size() > cap {
        return None;
    }
    let mut buf = Vec::new();
    (&mut file).take(cap + 1).read_to_end(&mut buf).ok()?;
    if buf.len() as u64 > cap {
        return None;
    }
    Some(buf)
}

/// Same as [`read_capped`] but for UTF-8 (lossy) text parts.
pub(crate) fn read_capped_string<R: Read + Seek>(
    zip: &mut zip::ZipArchive<R>,
    name: &str,
    cap: u64,
) -> Option<String> {
    read_capped(zip, name, cap).map(|b| String::from_utf8_lossy(&b).into_owned())
}

/// Cheap central-directory pre-check: reject an archive whose declared total
/// uncompressed size or entry count is over budget, before handing it to a
/// parser (e.g. calamine) that we can't bound from the outside.
pub(crate) fn within_budget<R: Read + Seek>(zip: &mut zip::ZipArchive<R>) -> bool {
    if zip.len() > MAX_ENTRIES {
        return false;
    }
    let mut total: u64 = 0;
    for i in 0..zip.len() {
        if let Ok(f) = zip.by_index(i) {
            total = total.saturating_add(f.size());
            if total > MAX_TOTAL_BYTES {
                return false;
            }
        }
    }
    true
}
