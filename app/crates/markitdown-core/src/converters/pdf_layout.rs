//! Geometry-driven layout reconstruction for PDF pages.
//!
//! A PDF has no headings, tables or paragraphs — only positioned glyphs. Both
//! PDF backends (the pure-Rust `pdf-extract` one and the optional PDFium one)
//! normalize their output into [`Glyph`]s and hand them here, and this module
//! rebuilds Markdown structure from geometry plus font metrics:
//!
//! * **headings**   — font size relative to the document's modal ("body") size
//! * **tables**     — runs of consecutive lines whose cells overlap in columns
//! * **lists**      — bullet / numbered prefixes, indented by x offset
//! * **bold/italic** — from font flags. PDFium reports these; `pdf-extract`'s
//!   `OutputDev` never passes the font down, so that backend always sends
//!   `false` and simply gets no emphasis (everything else still works).
//! * **paragraphs** — wrapped lines are re-joined (with de-hyphenation) and
//!   split on vertical gaps
//! * **running heads/feet** — text repeating at the top/bottom of most pages is
//!   dropped, so page furniture doesn't pollute the Markdown.
//!
//! Everything here is pure computation over a `Vec<Glyph>`: no PDF parsing, no
//! I/O, no allocation-per-glyph. That keeps it cheap next to the parse itself
//! (which dominates) and makes it unit-testable without any PDF fixture.

use std::collections::HashMap;

/// One positioned glyph, as produced by either PDF backend.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Glyph {
    /// Left edge, PDF points.
    pub x: f32,
    /// Baseline, PDF points, **top-down** (0 = top of page, growing downward),
    /// so sorting by `y` walks the page in reading order.
    pub y: f32,
    /// Right edge (`x` + advance), PDF points.
    pub end_x: f32,
    /// Rendered font size, PDF points.
    pub size: f32,
    pub bold: bool,
    pub italic: bool,
    pub ch: char,
}

/// Vertical tolerance for "these glyphs share a line", as a fraction of size.
const LINE_TOL: f32 = 0.45;
/// Horizontal gap that separates two words, as a fraction of the font size.
/// Glyph extents are *advance* boxes, so letters inside a word sit end-to-end
/// (gap ≈ 0) and only a real space opens a gap. This matches the threshold
/// `pdf-extract`'s own `PlainTextOutput` has long used; raising it silently
/// glues words together in justified text, where spaces are squeezed.
const WORD_GAP: f32 = 0.1;
/// Horizontal gap that separates two table cells. Must be comfortably wider
/// than a word space (~0.25em) so ordinary prose never splits into cells.
const CELL_GAP: f32 = 1.4;
/// How much the widest gap on a line may exceed the median before the line is
/// considered to have a real break in it.
///
/// Display type is frequently tracked out past [`CELL_GAP`], which would
/// shatter a cover's "2023" into "20" / "23" and a label like "RWF" into
/// "RW" / "F". What distinguishes tracking from a cell boundary is not the size
/// of the gap but its *uniformity*: tracked text has every gap the same, while
/// a row with real cells has one gap far wider than the rest. A line whose gaps
/// are all alike is therefore never split.
const UNIFORM_TRACKING_RATIO: f32 = 1.6;
/// Widest gap (in em) that letter tracking can plausibly be. Beyond this the
/// evenly-spaced things on a line are separate cells — a row of single-character
/// columns is evenly spaced too, and merging it would concatenate the cells with
/// no separator at all.
const MAX_TRACKING_GAP: f32 = 2.5;
/// Vertical gap (in body line-heights) that starts a new paragraph.
const PARA_GAP: f32 = 1.6;
/// Longest *average* cell text a run of aligned lines may have and still be
/// read as a table. Two-column prose also produces aligned "cells", but they
/// are far longer than real table cells — this is what tells them apart.
const MAX_TABLE_CELL_LEN: usize = 55;
/// Beyond this many columns the "table" is almost certainly mis-detected text.
const MAX_TABLE_COLS: usize = 12;
/// A heading has to be short; a full paragraph in a large font is not a title.
const MAX_HEADING_CHARS: usize = 120;

// ---------------------------------------------------------------------------
// Intermediate model
// ---------------------------------------------------------------------------

/// A run of characters sharing bold/italic flags.
#[derive(Debug, Clone)]
struct Span {
    text: String,
    bold: bool,
    italic: bool,
}

/// A horizontally-separated chunk of a line — a table cell candidate.
#[derive(Debug, Clone)]
struct Cell {
    x: f32,
    end_x: f32,
    spans: Vec<Span>,
}

impl Cell {
    fn text(&self) -> String {
        collapse_ws(&self.spans.iter().map(|s| s.text.as_str()).collect::<String>())
    }
    fn is_blank(&self) -> bool {
        self.spans.iter().all(|s| s.text.trim().is_empty())
    }
    /// True when every non-space character in the cell is bold.
    fn all_bold(&self) -> bool {
        let mut saw = false;
        for s in &self.spans {
            if s.text.trim().is_empty() {
                continue;
            }
            saw = true;
            if !s.bold {
                return false;
            }
        }
        saw
    }
}

#[derive(Debug, Clone)]
struct Line {
    y: f32,
    /// Dominant font size on the line.
    size: f32,
    cells: Vec<Cell>,
}

impl Line {
    fn text(&self) -> String {
        self.cells
            .iter()
            .map(Cell::text)
            .collect::<Vec<_>>()
            .join(" ")
    }
    fn all_bold(&self) -> bool {
        !self.cells.is_empty() && self.cells.iter().all(Cell::all_bold)
    }
    fn x(&self) -> f32 {
        self.cells.first().map(|c| c.x).unwrap_or(0.0)
    }
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Reconstruct Markdown for a whole document from per-page glyphs.
///
/// Doc-wide passes (modal body size, running head/foot detection) are why this
/// takes every page at once rather than rendering page-by-page.
pub(crate) fn document_to_markdown(pages: &mut [Vec<Glyph>]) -> String {
    let body = modal_size(
        pages
            .iter()
            .flatten()
            .filter(|g| !g.ch.is_whitespace())
            .map(|g| g.size),
    );

    let mut page_lines: Vec<Vec<Line>> = pages.iter_mut().map(|g| build_lines(g)).collect();
    // Running heads are found before columns are re-ordered, while "first and
    // last line of the page" still means the top and bottom of the page.
    strip_running_heads(&mut page_lines);
    let page_lines: Vec<Vec<Line>> = page_lines
        .into_iter()
        .map(|l| order_columns(split_prose_columns(l)))
        .collect();

    let mut out = String::new();
    for lines in &page_lines {
        let md = render_lines(lines, body);
        if md.trim().is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        out.push_str(&md);
    }
    out
}

// ---------------------------------------------------------------------------
// Line assembly
// ---------------------------------------------------------------------------

/// Round a font size into a 0.5pt bucket, so near-identical sizes group.
fn bucket(size: f32) -> u32 {
    (size.max(0.0) * 2.0).round() as u32
}

/// The most common font size, in 0.5pt buckets. Ties prefer the *smaller* size,
/// because body text is what we want when a document is half prose, half
/// headings of equal glyph count.
fn modal_size(sizes: impl Iterator<Item = f32>) -> f32 {
    let mut hist: HashMap<u32, u32> = HashMap::new();
    for s in sizes {
        if s.is_finite() && s > 0.5 {
            *hist.entry(bucket(s)).or_default() += 1;
        }
    }
    hist.into_iter()
        .max_by_key(|&(b, n)| (n, std::cmp::Reverse(b)))
        .map(|(b, _)| b as f32 / 2.0)
        .unwrap_or(10.0)
}

/// Group glyphs into lines (by baseline) and lines into cells (by x gap).
fn build_lines(glyphs: &mut Vec<Glyph>) -> Vec<Line> {
    glyphs.retain(|g| {
        g.size.is_finite() && g.x.is_finite() && g.y.is_finite() && g.end_x.is_finite()
    });
    glyphs.sort_by(|a, b| a.y.total_cmp(&b.y).then(a.x.total_cmp(&b.x)));

    let mut groups: Vec<Vec<Glyph>> = Vec::new();
    for &g in glyphs.iter() {
        match groups.last_mut() {
            // Tolerance uses the *smaller* of the two sizes so a large heading
            // immediately below a body line doesn't swallow it.
            Some(cur) if g.y - cur[0].y <= LINE_TOL * cur[0].size.min(g.size) => cur.push(g),
            _ => groups.push(vec![g]),
        }
    }

    groups
        .iter_mut()
        .filter_map(|g| {
            g.sort_by(|a, b| a.x.total_cmp(&b.x));
            assemble_line(g)
        })
        .collect()
}

/// Turn one line's x-sorted glyphs into cells and spans. `None` when the line
/// holds no visible text.
fn assemble_line(glyphs: &[Glyph]) -> Option<Line> {
    let y = glyphs[0].y;
    let size = modal_size(
        glyphs
            .iter()
            .filter(|g| !g.ch.is_whitespace())
            .map(|g| g.size),
    );

    // Is this line uniformly tracked-out display type rather than a row with
    // real cell boundaries? See UNIFORM_TRACKING_RATIO.
    let uniform_tracking = {
        let mut gaps: Vec<f32> = Vec::with_capacity(glyphs.len());
        let mut prev_end = f32::NEG_INFINITY;
        for g in glyphs {
            if prev_end.is_finite() {
                gaps.push((g.x - prev_end).max(0.0));
            }
            prev_end = prev_end.max(g.end_x);
        }
        gaps.sort_by(f32::total_cmp);
        let median = gaps.get(gaps.len() / 2).copied().unwrap_or(0.0);
        // A single gap carries no evidence of uniformity — two identical gaps
        // are the minimum that can distinguish tracking from a lone break.
        let em = glyphs[0].size.max(0.1);
        gaps.len() >= 2
            && median > 0.0
            && median <= MAX_TRACKING_GAP * em
            && gaps.last().copied().unwrap_or(0.0) <= UNIFORM_TRACKING_RATIO * median
    };

    let mut cells: Vec<Cell> = Vec::new();
    let mut prev_end = f32::NEG_INFINITY;
    for g in glyphs {
        let gap = g.x - prev_end;
        if cells.is_empty() || (!uniform_tracking && gap > CELL_GAP * g.size) {
            cells.push(Cell {
                x: g.x,
                end_x: g.end_x,
                spans: Vec::new(),
            });
        } else if !uniform_tracking && gap > WORD_GAP * g.size {
            // On a uniformly tracked line every gap is letter spacing, so
            // inserting a space at each one would render "2023" as "2 0 2 3".
            push_char(cells.last_mut()?, ' ', g.bold, g.italic);
        }
        let cell = cells.last_mut()?;
        push_char(cell, g.ch, g.bold, g.italic);
        cell.end_x = cell.end_x.max(g.end_x);
        prev_end = prev_end.max(g.end_x);
    }

    cells.retain(|c| !c.is_blank());
    for c in &mut cells {
        trim_spans(&mut c.spans);
    }
    if cells.is_empty() {
        return None;
    }
    Some(Line { y, size, cells })
}

/// Append one character, extending the last span when the style matches.
/// Whitespace never opens a span of its own — emphasis must not wrap spaces or
/// the resulting `** **` fails to render.
fn push_char(cell: &mut Cell, ch: char, bold: bool, italic: bool) {
    let matches = cell
        .spans
        .last()
        .is_some_and(|s| s.bold == bold && s.italic == italic);
    if matches || (ch.is_whitespace() && !cell.spans.is_empty()) {
        cell.spans.last_mut().expect("non-empty").text.push(ch);
    } else {
        cell.spans.push(Span {
            text: ch.to_string(),
            bold,
            italic,
        });
    }
}

/// Trim leading/trailing whitespace across a cell's spans and drop the ones
/// that end up empty.
fn trim_spans(spans: &mut Vec<Span>) {
    while let Some(first) = spans.first_mut() {
        let t = first.text.trim_start().to_string();
        first.text = t;
        if first.text.is_empty() {
            spans.remove(0);
        } else {
            break;
        }
    }
    while let Some(last) = spans.last_mut() {
        let t = last.text.trim_end().to_string();
        last.text = t;
        if last.text.is_empty() {
            spans.pop();
        } else {
            break;
        }
    }
}

fn collapse_ws(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_ws = false;
    for ch in s.chars() {
        if ch.is_whitespace() {
            in_ws = true;
        } else {
            if in_ws && !out.is_empty() {
                out.push(' ');
            }
            in_ws = false;
            out.push(ch);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Running heads / feet
// ---------------------------------------------------------------------------

/// Drop the first and/or last line of every page when that same text repeats
/// across most pages — page numbers, chapter rules, report titles. Digits are
/// normalized away first so "Page 12" and "Page 13" count as the same head.
fn strip_running_heads(pages: &mut [Vec<Line>]) {
    if pages.len() < 3 {
        return;
    }
    let threshold = (pages.len() * 2 / 5).max(3);

    // `from_top` picks which end of the page to examine; running heads and
    // running feet are found the same way from opposite ends.
    for from_top in [true, false] {
        fn pick(lines: &[Line], from_top: bool) -> Option<&Line> {
            if from_top {
                lines.first()
            } else {
                lines.last()
            }
        }
        let mut counts: HashMap<String, usize> = HashMap::new();
        for lines in pages.iter() {
            if let Some(l) = pick(lines, from_top) {
                *counts.entry(normalize_running(&l.text())).or_default() += 1;
            }
        }
        let repeated: Vec<String> = counts
            .into_iter()
            .filter(|(k, n)| *n >= threshold && !k.is_empty())
            .map(|(k, _)| k)
            .collect();
        if repeated.is_empty() {
            continue;
        }
        for lines in pages.iter_mut() {
            let hit = pick(lines, from_top).is_some_and(|l| repeated.contains(&normalize_running(&l.text())));
            if hit && !lines.is_empty() {
                if from_top {
                    lines.remove(0);
                } else {
                    lines.pop();
                }
            }
        }
    }
}

/// Collapse digit runs to `#` so page numbers compare equal, and lowercase.
fn normalize_running(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_digits = false;
    for ch in text.chars() {
        if ch.is_ascii_digit() {
            if !in_digits {
                out.push('#');
            }
            in_digits = true;
        } else {
            in_digits = false;
            out.extend(ch.to_lowercase());
        }
    }
    out.trim().to_string()
}

// ---------------------------------------------------------------------------
// Column reading order
// ---------------------------------------------------------------------------

/// Number of x bins used when looking for a page's column gutter.
const GUTTER_BINS: usize = 200;
/// A gutter must be at least this fraction of the page width.
const MIN_GUTTER_FRAC: f32 = 0.04;
/// …and its centre must fall in the middle of the page, not in a margin.
const GUTTER_ZONE: (f32, f32) = (0.28, 0.72);
/// Each column needs at least this many lines before we believe in it.
const MIN_COLUMN_LINES: usize = 3;
/// Shortest average side-of-the-gutter text that reads as prose rather than as
/// table cells. A two-column table straddles a gap exactly like two-column
/// prose does; cell length is what separates them (the same signal
/// [`detect_tables`] uses, from the other direction).
const MIN_PROSE_LEN: usize = 45;

/// Split lines that straddle a gutter into one line per column, so two-column
/// prose stops reading as "left-half right-half" on every line.
///
/// Deliberately conservative: it only fires when *both* sides average more text
/// than a table cell ever would, so real tables are left intact.
fn split_prose_columns(lines: Vec<Line>) -> Vec<Line> {
    let Some(gutter) = gutter_band(&lines) else {
        return lines;
    };

    // A line is splittable when it has cells on both sides and none straddling.
    let splittable = |l: &Line| {
        !l.cells.iter().any(|c| c.x < gutter && c.end_x > gutter)
            && l.cells.iter().any(|c| c.end_x <= gutter)
            && l.cells.iter().any(|c| c.x >= gutter)
    };

    let (mut left_chars, mut right_chars, mut n) = (0usize, 0usize, 0usize);
    for l in lines.iter().filter(|l| splittable(l)) {
        let side = |keep_left: bool| {
            l.cells
                .iter()
                .filter(|c| if keep_left { c.end_x <= gutter } else { c.x >= gutter })
                .map(|c| c.text().chars().count())
                .sum::<usize>()
        };
        left_chars += side(true);
        right_chars += side(false);
        n += 1;
    }
    if n < 2 || left_chars / n <= MIN_PROSE_LEN || right_chars / n <= MIN_PROSE_LEN {
        return lines;
    }

    lines
        .into_iter()
        .flat_map(|l| {
            if !splittable(&l) {
                return vec![l];
            }
            let (y, size) = (l.y, l.size);
            let (left, right): (Vec<Cell>, Vec<Cell>) =
                l.cells.into_iter().partition(|c| c.end_x <= gutter);
            vec![
                Line { y, size, cells: left },
                Line { y, size, cells: right },
            ]
        })
        .collect()
}

/// Re-order a page's lines into column reading order.
///
/// Sorting by `y` alone interleaves a two-column page line by line, which
/// shreds the prose and glues unrelated cells into fake tables. When a clear
/// vertical gutter exists we emit the left column, then the right, flushing at
/// every line that *crosses* the gutter so banner titles keep their position.
///
/// A two-column **table** is unaffected: each of its rows has cells on both
/// sides of the gap, so every row is a crossing line and stays exactly where it
/// is. That is also why the gutter is only accepted when enough lines sit
/// wholly within one side — a table alone never satisfies it.
fn order_columns(lines: Vec<Line>) -> Vec<Line> {
    let Some(gutter) = find_gutter(&lines) else {
        return lines;
    };

    let mut out = Vec::with_capacity(lines.len());
    let (mut left, mut right) = (Vec::new(), Vec::new());
    for line in lines {
        if line.cells.iter().all(|c| c.end_x <= gutter) {
            left.push(line);
        } else if line.cells.iter().all(|c| c.x >= gutter) {
            right.push(line);
        } else {
            out.append(&mut left);
            out.append(&mut right);
            out.push(line);
        }
    }
    out.append(&mut left);
    out.append(&mut right);
    out
}

/// A vertical gutter with real content wholly on both sides of it.
fn find_gutter(lines: &[Line]) -> Option<f32> {
    let gutter = gutter_band(lines)?;
    // Both sides must hold real content, or this is just a wide indent.
    let side = |keep_left: bool| {
        lines
            .iter()
            .filter(|l| {
                l.cells
                    .iter()
                    .all(|c| if keep_left { c.end_x <= gutter } else { c.x >= gutter })
            })
            .count()
    };
    (side(true) >= MIN_COLUMN_LINES && side(false) >= MIN_COLUMN_LINES).then_some(gutter)
}

/// Locate a vertical gutter: the widest band of the page width that no cell
/// covers, centred away from the margins.
fn gutter_band(lines: &[Line]) -> Option<f32> {
    if lines.len() < MIN_COLUMN_LINES * 2 {
        return None;
    }
    let cells = || lines.iter().flat_map(|l| l.cells.iter());
    let min_x = cells().map(|c| c.x).fold(f32::INFINITY, f32::min);
    let max_x = cells().map(|c| c.end_x).fold(f32::NEG_INFINITY, f32::max);
    let width = max_x - min_x;
    if !width.is_finite() || width <= 0.0 {
        return None;
    }

    let mut covered = [false; GUTTER_BINS];
    let bins = GUTTER_BINS as f32;
    for cell in cells() {
        let lo = (((cell.x - min_x) / width) * bins).floor().clamp(0.0, bins - 1.0) as usize;
        let hi = (((cell.end_x - min_x) / width) * bins).ceil().clamp(0.0, bins) as usize;
        for slot in covered.iter_mut().take(hi.max(lo + 1).min(GUTTER_BINS)).skip(lo) {
            *slot = true;
        }
    }

    // Widest empty run whose centre sits in the middle zone.
    let mut best: Option<(usize, usize)> = None;
    let mut run_start: Option<usize> = None;
    for b in 0..=GUTTER_BINS {
        let empty = b < GUTTER_BINS && !covered[b];
        match (empty, run_start) {
            (true, None) => run_start = Some(b),
            (false, Some(s)) => {
                let len = b - s;
                let centre = (s + b) as f32 / 2.0 / bins;
                if len as f32 / bins >= MIN_GUTTER_FRAC
                    && centre >= GUTTER_ZONE.0
                    && centre <= GUTTER_ZONE.1
                    && best.is_none_or(|(_, best_len)| len > best_len)
                {
                    best = Some((s, len));
                }
                run_start = None;
            }
            _ => {}
        }
    }
    let (start, len) = best?;
    Some(min_x + width * ((start + len / 2) as f32 / bins))
}

// ---------------------------------------------------------------------------
// Table detection
// ---------------------------------------------------------------------------

/// A detected table: the half-open line range it covers plus its rows.
struct TableRun {
    start: usize,
    end: usize,
    rows: Vec<Vec<String>>,
}

/// Find runs of consecutive multi-cell lines whose cells line up in columns.
///
/// Columns are matched by horizontal **overlap**, not by start-x, so a column
/// of right-aligned numbers (whose left edges differ by the number's width)
/// still resolves to one column.
fn detect_tables(lines: &[Line]) -> Vec<TableRun> {
    let mut runs = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if lines[i].cells.len() < 2 {
            i += 1;
            continue;
        }
        let mut j = i;
        while j < lines.len() && lines[j].cells.len() >= 2 {
            j += 1;
        }
        // A run is often poisoned at its head by an unrelated multi-cell line —
        // a metadata banner ("Report ID: … Warehouse: … Date: …") sitting
        // directly above a table. Its columns don't match the real ones, so
        // seeding from it makes the very next row collide and the whole table
        // is lost. Retry from each later line before giving up.
        let mut start = i;
        while start + 1 < j {
            if let Some(mut run) = build_table(&lines[start..j], start) {
                adopt_header(&mut run, lines, i);
                runs.push(run);
                break;
            }
            start += 1;
        }
        i = j;
    }
    runs
}

/// Pull the line directly above a table in as its header row.
///
/// Head-trimming (see [`detect_tables`]) drops the lines whose columns don't
/// match, and a header often doesn't: its labels are centred over columns whose
/// data is right-aligned, so overlap assignment rejects it. But a line with
/// exactly one cell per column, immediately above the table, *is* the header —
/// and without this the first data row gets rendered as the GFM header instead.
fn adopt_header(run: &mut TableRun, lines: &[Line], run_limit: usize) {
    if run.start <= run_limit {
        return;
    }
    let candidate = &lines[run.start - 1];
    let cols = run.rows.first().map(Vec::len).unwrap_or(0);
    if candidate.cells.len() != cols || cols == 0 {
        return;
    }
    run.rows
        .insert(0, candidate.cells.iter().map(Cell::text).collect());
    run.start -= 1;
}

fn build_table(lines: &[Line], offset: usize) -> Option<TableRun> {
    if lines.len() < 2 {
        return None;
    }

    // Column intervals, grown by overlap as rows are folded in.
    let mut cols: Vec<(f32, f32)> = lines[0].cells.iter().map(|c| (c.x, c.end_x)).collect();
    let mut assignments: Vec<Vec<(usize, String)>> = Vec::with_capacity(lines.len());

    for line in lines {
        let mut row: Vec<(usize, String)> = Vec::with_capacity(line.cells.len());
        let mut used = Vec::new();
        for cell in &line.cells {
            let idx = match best_column(&cols, cell) {
                Some(k) => {
                    // Grow the column to cover this cell.
                    cols[k].0 = cols[k].0.min(cell.x);
                    cols[k].1 = cols[k].1.max(cell.end_x);
                    k
                }
                None => {
                    cols.push((cell.x, cell.end_x));
                    cols.len() - 1
                }
            };
            // Two cells landing in one column means the rows don't really
            // share a grid — not a table.
            if used.contains(&idx) {
                return None;
            }
            used.push(idx);
            row.push((idx, cell.text()));
        }
        assignments.push(row);
    }

    if cols.len() < 2 || cols.len() > MAX_TABLE_COLS {
        return None;
    }

    // Prose laid out in columns also aligns; real table cells are short.
    let (total, count) = assignments
        .iter()
        .flatten()
        .fold((0usize, 0usize), |(t, c), (_, s)| (t + s.chars().count(), c + 1));
    if count == 0 || total / count > MAX_TABLE_CELL_LEN {
        return None;
    }

    // Re-index columns left-to-right (they were discovered in row order).
    let mut order: Vec<usize> = (0..cols.len()).collect();
    order.sort_by(|&a, &b| cols[a].0.total_cmp(&cols[b].0));
    let mut rank = vec![0usize; cols.len()];
    for (pos, &c) in order.iter().enumerate() {
        rank[c] = pos;
    }

    let rows: Vec<Vec<String>> = assignments
        .iter()
        .map(|row| {
            let mut out = vec![String::new(); cols.len()];
            for (idx, text) in row {
                out[rank[*idx]] = text.clone();
            }
            out
        })
        .collect();

    Some(TableRun {
        start: offset,
        end: offset + lines.len(),
        rows,
    })
}

/// The existing column a cell overlaps most, if any.
fn best_column(cols: &[(f32, f32)], cell: &Cell) -> Option<usize> {
    let mut best: Option<(usize, f32)> = None;
    for (i, &(lo, hi)) in cols.iter().enumerate() {
        let overlap = cell.end_x.min(hi) - cell.x.max(lo);
        if overlap > 0.0 && best.is_none_or(|(_, b)| overlap > b) {
            best = Some((i, overlap));
        }
    }
    best.map(|(i, _)| i)
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

fn render_lines(lines: &[Line], body: f32) -> String {
    let tables = detect_tables(lines);
    let left_margin = lines
        .iter()
        .map(Line::x)
        .fold(f32::INFINITY, f32::min);

    let mut out = String::new();
    // Open paragraph text, flushed when a block-level element interrupts it.
    let mut para = String::new();
    let mut prev_y: Option<f32> = None;
    // True when the previous emitted block was a table, so the raw line before
    // this one belongs to that table and says nothing about this line.
    let mut after_table = false;

    let mut i = 0;
    while i < lines.len() {
        if let Some(t) = tables.iter().find(|t| t.start == i) {
            flush_para(&mut out, &mut para);
            push_block(&mut out, &render_table(t));
            prev_y = lines[t.end - 1].y.into();
            after_table = true;
            i = t.end;
            continue;
        }

        let line = &lines[i];
        let text = line.text();
        if text.trim().is_empty() {
            i += 1;
            continue;
        }

        // A wide vertical gap ends the running paragraph — as does moving
        // *upwards*, which after column re-ordering means we just jumped from
        // the foot of one column to the head of the next.
        if let Some(py) = prev_y {
            if line.y < py || line.y - py > PARA_GAP * body.max(1.0) {
                flush_para(&mut out, &mut para);
            }
        }

        let isolated_bold = line.all_bold()
            && (after_table || !lines[..i].last().is_some_and(Line::all_bold))
            && !lines[i + 1..].first().is_some_and(Line::all_bold);
        if let Some(level) = heading_level(line, body, &text, isolated_bold) {
            flush_para(&mut out, &mut para);
            push_block(
                &mut out,
                &format!("{} {}", "#".repeat(level as usize), escape_leading(&text)),
            );
        } else if let Some((marker, rest)) = list_marker(&text) {
            flush_para(&mut out, &mut para);
            let indent = (((line.x() - left_margin) / (body.max(1.0) * 1.8)) as i32).clamp(0, 3);
            push_block(
                &mut out,
                &format!(
                    "{}{marker} {}",
                    "  ".repeat(indent as usize),
                    inline_from_line(line, rest)
                ),
            );
        } else {
            append_para(&mut para, &inline_from_line(line, &text));
        }

        prev_y = Some(line.y);
        after_table = false;
        i += 1;
    }
    flush_para(&mut out, &mut para);
    out
}

/// Render a line's inline Markdown (emphasis included). `visible` is the plain
/// text actually being emitted — when it differs from the line text (a list
/// item with its marker stripped) we fall back to the plain text, because the
/// span offsets no longer line up.
fn inline_from_line(line: &Line, visible: &str) -> String {
    let full = line.text();
    if visible != full {
        return visible.to_string();
    }
    let rendered = line
        .cells
        .iter()
        .map(render_spans)
        .collect::<Vec<_>>()
        .join(" ");
    collapse_ws(&rendered)
}

/// Wrap bold/italic runs. Emphasis markers hug the text: any surrounding
/// whitespace is moved outside, since `** bold **` does not render.
fn render_spans(cell: &Cell) -> String {
    let mut out = String::new();
    for s in &cell.spans {
        let core = s.text.trim();
        if core.is_empty() {
            out.push_str(&s.text);
            continue;
        }
        let lead = &s.text[..s.text.len() - s.text.trim_start().len()];
        let tail = &s.text[s.text.trim_end().len()..];
        let marker = match (s.bold, s.italic) {
            (true, true) => "***",
            (true, false) => "**",
            (false, true) => "*",
            (false, false) => "",
        };
        out.push_str(lead);
        out.push_str(marker);
        out.push_str(core);
        out.push_str(marker);
        out.push_str(tail);
    }
    out
}

/// Classify a line as a heading. `isolated_bold` means the line is entirely
/// bold *and* its neighbours are not — the only case where boldness at body
/// size signals a heading. Without that check every line of a bold paragraph
/// (PDFs routinely set whole blocks in semibold) becomes its own heading.
fn heading_level(line: &Line, body: f32, text: &str, isolated_bold: bool) -> Option<u8> {
    let chars = text.chars().count();
    if chars > MAX_HEADING_CHARS || line.cells.len() > 1 {
        return None;
    }
    let ratio = line.size / body.max(0.1);
    let level = if ratio >= 1.8 {
        1
    } else if ratio >= 1.5 {
        2
    } else if ratio >= 1.3 {
        3
    } else if ratio >= 1.15 {
        4
    } else if isolated_bold && ratio >= 0.95 && chars <= 80 && !text.ends_with('.') {
        // Body size, but a lone fully-bold line: a run-in heading.
        5
    } else {
        return None;
    };
    Some(level)
}

const BULLETS: &[char] = &['•', '●', '◦', '▪', '▫', '‣', '·', '–', '—', '*'];

/// Recognize a list prefix, returning the Markdown marker and the remaining
/// text. Handles bullets, `1.`/`1)` and `a.`/`a)`.
fn list_marker(text: &str) -> Option<(String, &str)> {
    let t = text.trim_start();
    let mut chars = t.char_indices();
    let (_, first) = chars.next()?;

    if BULLETS.contains(&first) || first == '-' {
        let after = &t[first.len_utf8()..];
        // A bullet is separated from its text by whitespace. Without this check
        // "-15% change" and "—John Smith" become list items with the sign or
        // dash silently deleted — corruption, not just mis-formatting.
        if !after.starts_with(char::is_whitespace) {
            return None;
        }
        let rest = after.trim_start();
        return (!rest.is_empty()).then(|| ("-".to_string(), rest));
    }

    // 1. / 12) / a. / iv)
    let label: String = t
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric())
        .take(4)
        .collect();
    if label.is_empty() || label.len() > 3 {
        return None;
    }
    let after = &t[label.len()..];
    let delim = after.chars().next()?;
    if delim != '.' && delim != ')' {
        return None;
    }
    let rest = after[delim.len_utf8()..].trim_start();
    if rest.is_empty() || !after[delim.len_utf8()..].starts_with(char::is_whitespace) {
        return None;
    }
    // Only numeric labels keep their ordinal; alphabetic ones become bullets so
    // "a) foo" doesn't turn into a broken ordered list.
    if label.chars().all(|c| c.is_ascii_digit()) {
        Some((format!("{label}."), rest))
    } else {
        Some(("-".to_string(), rest))
    }
}

/// Append a wrapped line to the open paragraph, de-hyphenating a word that was
/// split across the line break. Only a hyphen preceded by a letter counts —
/// otherwise "- " bullets and ranges like "1990 -" would be glued together.
fn append_para(para: &mut String, line: &str) {
    if para.is_empty() {
        para.push_str(line);
        return;
    }
    let soft_hyphen = para.ends_with('-')
        && para.chars().rev().nth(1).is_some_and(char::is_alphabetic)
        && line.starts_with(char::is_alphabetic);
    if soft_hyphen {
        para.pop();
        para.push_str(line);
    } else {
        para.push(' ');
        para.push_str(line);
    }
}

fn flush_para(out: &mut String, para: &mut String) {
    if para.trim().is_empty() {
        para.clear();
        return;
    }
    // Escape here, not per source line: a paragraph is assembled from several
    // wrapped lines and only its FIRST character can be read as block syntax.
    push_block(out, &escape_leading(para.trim()));
    para.clear();
}

fn push_block(out: &mut String, block: &str) {
    if !out.is_empty() {
        out.push_str("\n\n");
    }
    out.push_str(block);
}

fn render_table(t: &TableRun) -> String {
    let cols = t.rows.iter().map(Vec::len).max().unwrap_or(0);
    let mut rows = t.rows.iter();
    let header = rows.next().cloned().unwrap_or_default();

    let mut out = String::new();
    out.push_str(&render_row(&header, cols));
    out.push('\n');
    out.push('|');
    for _ in 0..cols {
        out.push_str(" --- |");
    }
    for row in rows {
        out.push('\n');
        out.push_str(&render_row(row, cols));
    }
    out
}

fn render_row(row: &[String], cols: usize) -> String {
    let mut out = String::from("|");
    for i in 0..cols {
        let cell = row.get(i).map(String::as_str).unwrap_or("");
        out.push(' ');
        out.push_str(&escape_cell(cell));
        out.push_str(" |");
    }
    out
}

/// GFM cells are pipe-delimited, so a literal pipe must be escaped or the row
/// silently gains a column. Newlines become `<br>`.
fn escape_cell(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '|' => out.push_str("\\|"),
            '\n' | '\r' => out.push_str("<br>"),
            _ => out.push(ch),
        }
    }
    out
}

/// Escape a leading character that would otherwise be read as Markdown syntax
/// (a body line starting with `#`, `>` or `|`). Deliberately minimal — blanket
/// escaping makes the output noisy for no benefit.
fn escape_leading(s: &str) -> String {
    match s.chars().next() {
        Some(c @ ('#' | '>' | '|')) => format!("\\{c}{}", &s[c.len_utf8()..]),
        _ => s.to_string(),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Lay out a line of text at (x, y) with the given size/flags. Advance is
    /// 0.5em per character, which is close enough to a real proportional font
    /// for the gap heuristics.
    fn line_at(x: f32, y: f32, size: f32, bold: bool, text: &str) -> Vec<Glyph> {
        let adv = size * 0.5;
        text.chars()
            .enumerate()
            .map(|(i, ch)| Glyph {
                x: x + i as f32 * adv,
                y,
                end_x: x + (i as f32 + 1.0) * adv,
                size,
                bold,
                italic: false,
                ch,
            })
            .collect()
    }

    /// Lay out text with extra letter tracking: `gap_em` of blank space is left
    /// between consecutive glyph advances, as display/cover type does.
    fn tracked_line_at(x: f32, y: f32, size: f32, text: &str, gap_em: f32) -> Vec<Glyph> {
        let adv = size * 0.5;
        let step = adv + size * gap_em;
        text.chars()
            .enumerate()
            .map(|(i, ch)| Glyph {
                x: x + i as f32 * step,
                y,
                end_x: x + i as f32 * step + adv,
                size,
                bold: false,
                italic: false,
                ch,
            })
            .collect()
    }

    fn render(mut pages: Vec<Vec<Glyph>>) -> String {
        document_to_markdown(&mut pages)
    }

    #[test]
    fn body_text_becomes_a_paragraph() {
        let mut page = line_at(0.0, 100.0, 10.0, false, "Hello world this is body text.");
        page.extend(line_at(0.0, 112.0, 10.0, false, "It continues on a second line."));
        let md = render(vec![page]);
        // Wrapped lines re-join into one paragraph.
        assert_eq!(
            md,
            "Hello world this is body text. It continues on a second line."
        );
    }

    #[test]
    fn large_text_becomes_a_heading() {
        let mut page = line_at(0.0, 50.0, 24.0, false, "Chapter One");
        for i in 0..8 {
            page.extend(line_at(0.0, 100.0 + i as f32 * 12.0, 10.0, false, "body text here"));
        }
        let md = render(vec![page]);
        assert!(md.starts_with("# Chapter One\n\n"), "got:\n{md}");
    }

    #[test]
    fn heading_levels_scale_with_font_size() {
        let mut page = Vec::new();
        for i in 0..10 {
            page.extend(line_at(0.0, 200.0 + i as f32 * 12.0, 10.0, false, "ordinary body text"));
        }
        page.extend(line_at(0.0, 20.0, 20.0, false, "Big"));
        page.extend(line_at(0.0, 60.0, 15.0, false, "Medium"));
        page.extend(line_at(0.0, 100.0, 13.5, false, "Small"));
        let md = render(vec![page]);
        assert!(md.contains("# Big"), "{md}");
        assert!(md.contains("## Medium"), "{md}");
        assert!(md.contains("### Small"), "{md}");
    }

    #[test]
    fn bold_run_is_wrapped_in_asterisks() {
        let mut page = line_at(0.0, 100.0, 10.0, false, "normal ");
        let x = page.last().unwrap().end_x;
        page.extend(line_at(x, 100.0, 10.0, true, "bold"));
        let md = render(vec![page]);
        assert!(md.contains("normal **bold**"), "got: {md}");
    }

    #[test]
    fn bold_markers_do_not_wrap_whitespace() {
        // A trailing space inside the bold run must stay outside the markers,
        // otherwise "** bold **" renders literally.
        let mut page = line_at(0.0, 100.0, 10.0, false, "a ");
        let x = page.last().unwrap().end_x;
        page.extend(line_at(x, 100.0, 10.0, true, "b "));
        let md = render(vec![page]);
        // The bold run's trailing space must end up *outside* the markers.
        assert_eq!(md, "a **b**");
    }

    #[test]
    fn aligned_columns_become_a_gfm_table() {
        let mut page = Vec::new();
        // Three rows, two columns, separated by a wide gap.
        for (i, (a, b)) in [("Region", "Share"), ("Dar", "42%"), ("Mwanza", "17%")]
            .iter()
            .enumerate()
        {
            let y = 100.0 + i as f32 * 14.0;
            page.extend(line_at(0.0, y, 10.0, i == 0, a));
            page.extend(line_at(200.0, y, 10.0, i == 0, b));
        }
        let md = render(vec![page]);
        assert!(md.contains("| Region | Share |"), "got:\n{md}");
        assert!(md.contains("| --- | --- |"), "got:\n{md}");
        assert!(md.contains("| Dar | 42% |"), "got:\n{md}");
        assert!(md.contains("| Mwanza | 17% |"), "got:\n{md}");
    }

    #[test]
    fn right_aligned_numeric_column_stays_one_column() {
        // Right-aligned numbers have different left edges; overlap-based column
        // matching must still resolve them to a single column.
        let mut page = Vec::new();
        let rows = [("Item", "1"), ("A", "100"), ("B", "25"), ("C", "9")];
        for (i, (label, num)) in rows.iter().enumerate() {
            let y = 100.0 + i as f32 * 14.0;
            page.extend(line_at(0.0, y, 10.0, false, label));
            // right edge fixed at 250 -> left edge varies with width
            let w = num.len() as f32 * 5.0;
            page.extend(line_at(250.0 - w, y, 10.0, false, num));
        }
        let md = render(vec![page]);
        let header = md.lines().next().unwrap_or_default();
        assert_eq!(header.matches('|').count(), 3, "expected 2 columns: {md}");
    }

    #[test]
    fn two_column_prose_is_not_mistaken_for_a_table() {
        // Long text in two columns aligns just like a table — the cell-length
        // guard is what keeps it prose.
        let mut page = Vec::new();
        for i in 0..8 {
            let y = 100.0 + i as f32 * 14.0;
            page.extend(line_at(
                0.0,
                y,
                10.0,
                false,
                "this is a long sentence of running prose in the left column",
            ));
            page.extend(line_at(
                400.0,
                y,
                10.0,
                false,
                "and this is more running prose over in the right column here",
            ));
        }
        let md = render(vec![page]);
        assert!(!md.contains("| --- |"), "prose became a table:\n{md}");
    }

    #[test]
    fn bullets_become_list_items() {
        let mut page = line_at(0.0, 100.0, 10.0, false, "• first point");
        page.extend(line_at(0.0, 114.0, 10.0, false, "• second point"));
        let md = render(vec![page]);
        assert!(md.contains("- first point"), "{md}");
        assert!(md.contains("- second point"), "{md}");
    }

    #[test]
    fn numbered_lists_keep_their_ordinal() {
        let page = line_at(0.0, 100.0, 10.0, false, "1. first");
        let md = render(vec![page]);
        assert_eq!(md, "1. first");
    }

    #[test]
    fn decimal_numbers_are_not_list_items() {
        // "3." only starts a list when followed by whitespace — "3.5" is a
        // number and must survive verbatim.
        let page = line_at(0.0, 100.0, 10.0, false, "3.5 million people were surveyed");
        let md = render(vec![page]);
        assert_eq!(md, "3.5 million people were surveyed");
    }

    #[test]
    fn hyphenated_line_break_is_rejoined() {
        let mut page = line_at(0.0, 100.0, 10.0, false, "inter-");
        page.extend(line_at(0.0, 112.0, 10.0, false, "national"));
        let md = render(vec![page]);
        assert_eq!(md, "international");
    }

    #[test]
    fn running_heads_are_dropped() {
        let pages: Vec<Vec<Glyph>> = (0..6)
            .map(|p| {
                let mut page = line_at(0.0, 20.0, 9.0, false, "FinScope Tanzania 2023");
                page.extend(line_at(0.0, 300.0, 10.0, false, &format!("unique body {p}")));
                page.extend(line_at(0.0, 700.0, 9.0, false, &format!("Page {}", p + 1)));
                page
            })
            .collect();
        let md = render(pages);
        assert!(!md.contains("FinScope Tanzania 2023"), "head kept:\n{md}");
        assert!(!md.contains("Page 3"), "foot kept:\n{md}");
        assert!(md.contains("unique body 3"), "body lost:\n{md}");
    }

    #[test]
    fn two_column_prose_is_read_column_by_column() {
        // Real two-column prose shares baselines across the columns, so reading
        // by y alone emits "left-half right-half" on every line. The whole left
        // column must come out before the whole right column.
        let mut page = Vec::new();
        for i in 0..8 {
            let y = 100.0 + i as f32 * 12.0;
            page.extend(line_at(
                0.0,
                y,
                10.0,
                false,
                &format!("ALPHA{i} left column running prose that fills the measure"),
            ));
            page.extend(line_at(
                400.0,
                y,
                10.0,
                false,
                &format!("BETA{i} right column running prose that fills the measure"),
            ));
        }
        let md = render(vec![page]);
        let pos = |s: &str| md.find(s).unwrap_or_else(|| panic!("{s} missing in:\n{md}"));
        assert!(pos("ALPHA7") < pos("BETA0"), "columns interleaved:\n{md}");
        assert!(pos("ALPHA0") < pos("ALPHA7"), "left column reordered:\n{md}");
        assert!(pos("BETA0") < pos("BETA7"), "right column reordered:\n{md}");
    }

    #[test]
    fn short_aligned_pairs_stay_a_table_not_columns() {
        // The same geometry with SHORT text is a table, not prose columns —
        // splitting it would be wrong.
        let mut page = Vec::new();
        for i in 0..8 {
            let y = 100.0 + i as f32 * 12.0;
            page.extend(line_at(0.0, y, 10.0, false, &format!("Key{i}")));
            page.extend(line_at(400.0, y, 10.0, false, &format!("Val{i}")));
        }
        let md = render(vec![page]);
        assert!(md.contains("| Key1 | Val1 |"), "table was split:\n{md}");
    }

    #[test]
    fn two_column_table_survives_column_reordering() {
        // Table rows straddle the gap, so they must NOT be split into two
        // columns — this is the case the gutter logic must leave alone.
        let mut page = Vec::new();
        let rows = [("CHF", "Community Health Fund"), ("GDP", "Gross Domestic"),
                    ("KYC", "Know Your Customer"), ("MFI", "Microfinance Inst")];
        for (i, (k, v)) in rows.iter().enumerate() {
            let y = 100.0 + i as f32 * 14.0;
            page.extend(line_at(0.0, y, 10.0, false, k));
            page.extend(line_at(300.0, y, 10.0, false, v));
        }
        let md = render(vec![page]);
        assert!(md.contains("| CHF | Community Health Fund |"), "got:\n{md}");
        assert!(md.contains("| --- | --- |"), "got:\n{md}");
    }

    #[test]
    fn single_column_page_is_left_alone() {
        let mut page = Vec::new();
        for i in 0..6 {
            page.extend(line_at(
                0.0,
                100.0 + i as f32 * 12.0,
                10.0,
                false,
                &format!("line number {i} of ordinary single column prose"),
            ));
        }
        let md = render(vec![page]);
        let pos = |s: &str| md.find(s).unwrap();
        assert!(pos("number 0") < pos("number 3") && pos("number 3") < pos("number 5"), "{md}");
    }

    #[test]
    fn tracked_out_display_text_is_not_split_into_cells() {
        // Cover/heading type is often tracked well past a fixed em threshold.
        // Splitting on that turns "2023" into "20" "23" and "RWF" into "RW" "F".
        let page = tracked_line_at(0.0, 100.0, 24.0, "2023", 1.6);
        let md = render(vec![page]);
        assert!(md.contains("2023"), "tracked text was shattered: {md}");
        assert!(!md.contains('|'), "tracked text became a table: {md}");
    }

    #[test]
    fn tracked_text_split_does_not_break_real_tables() {
        // The adaptive threshold must not stop genuine table rows from
        // splitting: their letters touch, so the fixed threshold still applies.
        let mut page = Vec::new();
        for (i, (a, b)) in [("Region", "Share"), ("Dar", "42%"), ("Mwanza", "17%")]
            .iter()
            .enumerate()
        {
            let y = 100.0 + i as f32 * 14.0;
            page.extend(line_at(0.0, y, 10.0, false, a));
            page.extend(line_at(200.0, y, 10.0, false, b));
        }
        let md = render(vec![page]);
        assert!(md.contains("| Dar | 42% |"), "table stopped splitting:\n{md}");
    }

    #[test]
    fn negative_numbers_and_dashes_are_not_list_items() {
        // REGRESSION: a bullet needs whitespace after it. Without that check
        // "-15%" became "- 15%" — the minus sign silently deleted.
        assert_eq!(list_marker("-15% change year on year"), None);
        assert_eq!(list_marker("—John Smith"), None);
        assert_eq!(list_marker("*emphasis* not a bullet"), None);
        // Real bullets still work.
        assert_eq!(list_marker("- a point").unwrap().1, "a point");
        assert_eq!(list_marker("• a point").unwrap().1, "a point");
    }

    #[test]
    fn negative_number_survives_a_full_render() {
        let page = line_at(0.0, 100.0, 10.0, false, "-24 units missing from stock");
        let md = render(vec![page]);
        assert_eq!(md, "-24 units missing from stock");
    }

    #[test]
    fn widely_spaced_single_char_cells_still_split() {
        // REGRESSION: three evenly spaced single-character cells satisfied the
        // "uniform tracking" test and were concatenated with NO separator
        // ("X Y Z" -> "XYZ"). Real tracking is never this wide.
        let mut page = Vec::new();
        for row in 0..3 {
            let y = 100.0 + row as f32 * 14.0;
            for (col, ch) in ["X", "Y", "Z"].iter().enumerate() {
                page.extend(line_at(col as f32 * 60.0, y, 10.0, false, ch));
            }
        }
        let md = render(vec![page]);
        assert!(md.contains("| X | Y | Z |"), "cells were merged:\n{md}");
    }

    #[test]
    fn paragraph_escape_applies_only_at_the_start() {
        // REGRESSION: escape_leading ran per wrapped line, so a continuation
        // line beginning with "#" got a backslash in mid-sentence.
        let mut page = line_at(0.0, 100.0, 10.0, false, "the top ranked item is");
        page.extend(line_at(0.0, 112.0, 10.0, false, "#1 in the whole market today"));
        let md = render(vec![page]);
        assert_eq!(md, "the top ranked item is #1 in the whole market today");
    }

    #[test]
    fn non_finite_end_x_is_discarded() {
        // A NaN end_x never panics but poisons column overlap tests forever.
        let mut page = line_at(0.0, 100.0, 10.0, false, "ok");
        page.push(Glyph {
            x: 5.0,
            y: 100.0,
            end_x: f32::NAN,
            size: 10.0,
            bold: false,
            italic: false,
            ch: 'X',
        });
        assert_eq!(render(vec![page]), "ok");
    }

    #[test]
    fn pipes_in_cells_are_escaped() {
        let mut page = Vec::new();
        for (i, (a, b)) in [("x|y", "1"), ("p", "2")].iter().enumerate() {
            let y = 100.0 + i as f32 * 14.0;
            page.extend(line_at(0.0, y, 10.0, false, a));
            page.extend(line_at(200.0, y, 10.0, false, b));
        }
        let md = render(vec![page]);
        assert!(md.contains("x\\|y"), "unescaped pipe:\n{md}");
        // Header row still declares exactly two columns.
        assert!(md.lines().nth(1).unwrap().contains("| --- | --- |"), "{md}");
    }

    #[test]
    fn leading_hash_in_body_text_is_escaped() {
        let page = line_at(0.0, 100.0, 10.0, false, "#1 ranked provider in the market");
        let md = render(vec![page]);
        assert!(md.starts_with("\\#1"), "{md}");
    }

    #[test]
    fn empty_input_yields_empty_output() {
        assert_eq!(render(vec![]), "");
        assert_eq!(render(vec![vec![]]), "");
    }

    #[test]
    fn vertical_gap_splits_paragraphs() {
        let mut page = line_at(0.0, 100.0, 10.0, false, "First paragraph text");
        // 40pt below: a clear paragraph break at 10pt body size.
        page.extend(line_at(0.0, 140.0, 10.0, false, "Second paragraph text"));
        let md = render(vec![page]);
        assert_eq!(md, "First paragraph text\n\nSecond paragraph text");
    }

    #[test]
    fn modal_size_prefers_the_smaller_size_on_a_tie() {
        assert_eq!(modal_size([10.0, 10.0, 20.0, 20.0].into_iter()), 10.0);
        assert_eq!(modal_size([12.0, 12.0, 12.0, 30.0].into_iter()), 12.0);
        // No usable input falls back to a sane default rather than dividing by 0.
        assert_eq!(modal_size(std::iter::empty()), 10.0);
    }

    #[test]
    fn non_finite_glyphs_are_discarded() {
        let mut page = line_at(0.0, 100.0, 10.0, false, "ok");
        page.push(Glyph {
            x: f32::NAN,
            y: f32::INFINITY,
            end_x: f32::NAN,
            size: f32::NAN,
            bold: false,
            italic: false,
            ch: 'X',
        });
        let md = render(vec![page]);
        assert_eq!(md, "ok");
    }
}
