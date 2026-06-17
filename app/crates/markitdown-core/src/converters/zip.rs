//! ZIP → Markdown converter.
//!
//! Port of `_zip_converter.py`. Iterates the archive entries, recursively
//! converting each via a fresh [`crate::MarkItDown`] instance, and emits the
//! results under `## File: <name>` sections. Directories are skipped; per-file
//! failures append an error note rather than aborting the whole archive.
//!
//! Recursion (zips inside zips) is bounded by a thread-local depth counter
//! (max depth 4). The `MarkItDown` engine is constructed lazily *inside*
//! `convert` to avoid constructor recursion, since the registry itself creates
//! a `ZipConverter`.

use std::cell::Cell;
use std::io::Read;

use crate::{Converter, ConvertError, ConvertOptions, ConvertResult, StreamInfo};

const ACCEPTED_EXTENSIONS: &[&str] = &[".zip"];
const ACCEPTED_MIME_PREFIXES: &[&str] = &["application/zip"];
const MAX_DEPTH: u32 = 4;
/// Zip-bomb / resource caps (per archive): the most entries we expand, the
/// largest single entry we decompress, and the total decompressed budget.
const MAX_ENTRIES: usize = 4096;
const MAX_ENTRY_BYTES: u64 = 128 * 1024 * 1024;
const MAX_TOTAL_BYTES: u64 = 512 * 1024 * 1024;

thread_local! {
    static DEPTH: Cell<u32> = const { Cell::new(0) };
}

pub struct ZipConverter;

impl Converter for ZipConverter {
    fn name(&self) -> &'static str {
        "zip"
    }

    fn accepts(&self, info: &StreamInfo, data: &[u8]) -> bool {
        if info.extension_is(ACCEPTED_EXTENSIONS) {
            return data.starts_with(b"PK");
        }
        if let Some(mt) = &info.mimetype {
            let mt = mt.split(';').next().unwrap_or(mt).trim();
            if ACCEPTED_MIME_PREFIXES.iter().any(|p| mt.starts_with(p)) {
                return data.starts_with(b"PK");
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
        let label = info
            .url
            .clone()
            .or_else(|| info.local_path.as_ref().map(|p| p.display().to_string()))
            .or_else(|| info.filename.clone())
            .unwrap_or_else(|| "archive.zip".to_string());

        let mut md = format!("Content from the zip file `{label}`:\n\n");

        let depth = DEPTH.with(|d| d.get());
        if depth >= MAX_DEPTH {
            md.push_str("> Maximum archive recursion depth reached; contents not expanded.\n");
            return Ok(ConvertResult::new(md.trim().to_string()));
        }

        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(data))
            .map_err(|e| ConvertError::conversion("zip", format!("not a valid zip: {e}")))?;

        // Build a fresh engine lazily, mirroring Python's recursive use of a
        // MarkItDown instance for the inner files.
        let engine = crate::MarkItDown::with_options(opts.clone());

        let mut degraded = false;
        let entry_count = zip.len();
        // Entry-count cap: a "zip of millions of tiny files" is a cheap DoS.
        let count = entry_count.min(MAX_ENTRIES);
        if entry_count > MAX_ENTRIES {
            md.push_str(&format!(
                "> Archive has {entry_count} entries; only the first {MAX_ENTRIES} were processed.\n\n"
            ));
            degraded = true;
        }

        // Non-directory entries in the processed range (for the progress total).
        // Iterate by index throughout — `by_name` is a linear scan (O(N²) here).
        let total = (0..count)
            .filter(|&i| {
                zip.by_index(i)
                    .map(|f| !f.is_dir() && !f.name().ends_with('/'))
                    .unwrap_or(false)
            })
            .count() as u64;
        let mut done = 0u64;
        // Cumulative decompression budget for the whole archive (zip-bomb guard).
        let mut budget = MAX_TOTAL_BYTES;

        for i in 0..count {
            let mut file = match zip.by_index(i) {
                Ok(f) => f,
                Err(_) => continue,
            };
            let name = file.name().to_string();
            if file.is_dir() || name.ends_with('/') {
                continue;
            }
            done += 1;
            opts.report(crate::Progress::step(
                "zip",
                format!("file {done}/{total}: {name}"),
                done,
                total,
            ));

            // Zip-bomb guard: reject an entry whose declared uncompressed size
            // is huge, and hard-cap the actual read at the smaller of the
            // per-entry limit and the archive's remaining budget — defending
            // against a lying header too. `take(cap + 1)` lets us detect overrun.
            if file.size() > MAX_ENTRY_BYTES {
                md.push_str(&format!(
                    "## File: {name}\n\n> Skipped: entry exceeds the per-file size limit.\n\n"
                ));
                degraded = true;
                continue;
            }
            let cap = MAX_ENTRY_BYTES.min(budget);
            let mut bytes = Vec::new();
            if (&mut file).take(cap + 1).read_to_end(&mut bytes).is_err() {
                md.push_str(&format!("## File: {name}\n\n> Failed to read entry.\n\n"));
                degraded = true;
                continue;
            }
            drop(file);
            if bytes.len() as u64 > cap {
                md.push_str(&format!(
                    "## File: {name}\n\n> Skipped: archive exceeds the {}-MiB decompression limit.\n\n",
                    MAX_TOTAL_BYTES / (1024 * 1024)
                ));
                degraded = true;
                break; // budget exhausted — stop expanding the rest
            }
            budget -= bytes.len() as u64;

            let ext = std::path::Path::new(&name)
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| format!(".{e}"));
            let basename = std::path::Path::new(&name)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or(&name);

            let mut hints = StreamInfo::new().with_filename(basename);
            if let Some(e) = &ext {
                hints = hints.with_extension(e);
            }

            DEPTH.with(|d| d.set(depth + 1));
            let result = engine.convert_bytes(&bytes, hints);
            DEPTH.with(|d| d.set(depth));

            match result {
                Ok(r) => {
                    md.push_str(&format!("## File: {name}\n\n"));
                    md.push_str(r.markdown.trim());
                    md.push_str("\n\n");
                    // Children inherit our options and may already have
                    // fallen back individually; if one is still degraded
                    // (e.g. scanned PDF, no Python engine), surface it so
                    // Engine::Auto can retry the whole archive.
                    degraded |= r.degraded;
                }
                Err(_) => {
                    // Mirror Python: unsupported / failed inner files are
                    // silently skipped (no section emitted) — but flag the
                    // archive so a configured Python engine gets a shot at
                    // formats this port can't read.
                    degraded = true;
                }
            }
        }

        let mut result = ConvertResult::new(md.trim().to_string());
        if degraded {
            result = result.with_degraded();
        }
        Ok(result)
    }
}
