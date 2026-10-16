//! Soft wrap: splitting long lines into visual rows that fit the window.
//!
//! A logical line (text between line breaks) is shown as one or more
//! *rows*. Rows break at word boundaries, so a word moves whole to the next
//! row; a word longer than the row is broken anywhere. Spaces and tabs
//! never cause a break: they *hang* past the right edge, as in most editors,
//! so a row never starts with the space that separated it from the previous
//! one. Tab stops are measured from the start of the logical line, so
//! toggling wrap never changes how tab-aligned text lines up.
//!
//! Nothing here sums rows over the whole document. Scrolling is anchored to
//! a logical line plus a row within it (DESIGN.md §4.3), and the walkers
//! below only touch the lines they pass. With wrap off every line is one
//! row, so the same code serves both modes.

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hasher};
use std::ops::Range;

use unicode_segmentation::UnicodeSegmentation;

use crate::column;
use crate::{LineSpan, Text};

/// Where one visual row starts within its logical line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RowStart {
    /// Byte offset in the line.
    pub byte: usize,
    /// Display column of `byte`, counted from the start of the line.
    pub col: usize,
}

const FIRST_ROW: RowStart = RowStart { byte: 0, col: 0 };
const ONE_ROW: &[RowStart] = &[FIRST_ROW];

/// Never break a row just before these: a closing bracket or punctuation at
/// the start of a row reads badly.
const NO_BREAK_BEFORE: &[char] = &[')', ']', '}', ',', '.', ';', ':', '!', '?'];

/// Split `line` into rows at most `width_cols` columns wide, writing the
/// row starts into `out` (cleared first). There is always at least one row.
pub fn wrap_line(line: &str, width_cols: usize, tab_width: usize, out: &mut Vec<RowStart>) {
    out.clear();
    out.push(FIRST_ROW);
    let width = width_cols.max(1);
    let mut row = FIRST_ROW;
    let mut col = 0;
    let mut last_break: Option<RowStart> = None;
    let mut words = line.split_word_bound_indices().map(|(i, _)| i).peekable();
    for (i, g) in line.grapheme_indices(true) {
        // Skip word boundaries that fall inside a grapheme.
        while words.next_if(|&w| w < i).is_some() {}
        if words.next_if_eq(&i).is_some() && i > row.byte && !g.starts_with(NO_BREAK_BEFORE) {
            last_break = Some(RowStart { byte: i, col });
        }
        let w = column::grapheme_cols(g, col, tab_width);
        if g != " " && g != "\t" {
            // At most twice: breaking at an earlier opportunity can still
            // leave a wide character overflowing.
            while col + w > row.col + width && col > row.col {
                let at = last_break.take().unwrap_or(RowStart { byte: i, col });
                out.push(at);
                row = at;
            }
        }
        col += w;
    }
}

/// The row containing byte offset `off`. An offset exactly at a row break
/// belongs to the row that starts there.
#[must_use]
pub fn row_of(rows: &[RowStart], off: usize) -> usize {
    rows.partition_point(|r| r.byte <= off).saturating_sub(1)
}

/// Byte range of row `r` within a line of `line_len` bytes.
#[must_use]
pub fn row_range(rows: &[RowStart], r: usize, line_len: usize) -> Range<usize> {
    let start = rows.get(r).map_or(line_len, |s| s.byte);
    let end = rows.get(r + 1).map_or(line_len, |s| s.byte);
    start.min(line_len)..end.min(line_len)
}

/// Column of byte `off` relative to the start of its row.
#[must_use]
pub fn visual_col(line: &str, rows: &[RowStart], off: usize, tab_width: usize) -> usize {
    let r = row_of(rows, off);
    let range = row_range(rows, r, line.len());
    let start = rows.get(r).copied().unwrap_or(FIRST_ROW);
    let seg = line.get(range.clone()).unwrap_or("");
    column::col_at_from(seg, off.saturating_sub(range.start), start.col, tab_width) - start.col
}

/// Byte offset in `line` for a position on row `r` given in half-columns
/// from the row's start (for clicks). On a row that continues on the next,
/// positions past its end land before its last grapheme: the row's end is
/// the next row's start, where the cursor would show on the next row.
#[must_use]
pub fn byte_at_row_half_col(
    line: &str,
    rows: &[RowStart],
    r: usize,
    half: usize,
    tab_width: usize,
) -> usize {
    let range = row_range(rows, r, line.len());
    let seg = line.get(range.clone()).unwrap_or("");
    let start_col = rows.get(r).map_or(0, |s| s.col);
    let rel = column::byte_at_half_col_from(seg, half, start_col, tab_width);
    if r + 1 < rows.len() && rel >= seg.len() {
        return range.start + seg.grapheme_indices(true).next_back().map_or(0, |(i, _)| i);
    }
    range.start + rel
}

/// Byte offset nearest to column `vcol` of row `r`, ties going left.
#[must_use]
pub fn byte_at_row_col(
    line: &str,
    rows: &[RowStart],
    r: usize,
    vcol: usize,
    tab_width: usize,
) -> usize {
    byte_at_row_half_col(line, rows, r, (2 * vcol).saturating_sub(1), tab_width)
}

/// Whether `line` certainly fits in `width` columns without wrapping. For
/// text without tabs, display width never exceeds byte length (a one-column
/// character takes at least one byte, a two-column one at least three), so
/// short lines skip the wrap cache entirely.
fn fits(line: &str, width: usize) -> bool {
    line.len() <= width && !line.contains('\t')
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct Key {
    hash: u64,
    len: usize,
    width: usize,
    tab_width: usize,
}

impl Key {
    fn new(line: &str, width: usize, tab_width: usize) -> Self {
        let mut h = DefaultHasher::new();
        h.write(line.as_bytes());
        Self {
            hash: h.finish(),
            len: line.len(),
            width,
            tab_width,
        }
    }
}

#[derive(Debug)]
struct Entry {
    rows: Vec<RowStart>,
    used: u64,
}

/// Wrapped rows of recently seen long lines.
///
/// Keyed by line content, width and tab width, so nothing needs explicit
/// invalidation: an edited line hashes differently, and a resize changes
/// the width. Entries unused for a few frames are dropped once the cache
/// grows past a limit, and their row vectors are kept for reuse, so typing
/// in a wrapped line does not allocate.
#[derive(Debug, Default)]
pub struct WrapCache {
    entries: HashMap<Key, Entry>,
    spare: Vec<Vec<RowStart>>,
    scratch: Vec<RowStart>,
    frame: u64,
}

const MAX_ENTRIES: usize = 1024;
const KEEP_FRAMES: u64 = 8;
const MAX_SPARE: usize = 256;

impl WrapCache {
    /// Mark the start of a frame; old entries are swept when the cache is
    /// over its size limit.
    pub fn next_frame(&mut self) {
        self.frame += 1;
        if self.entries.len() > MAX_ENTRIES {
            let frame = self.frame;
            let spare = &mut self.spare;
            self.entries.retain(|_, e| {
                let keep = e.used + KEEP_FRAMES >= frame;
                if !keep && spare.len() < MAX_SPARE && e.rows.capacity() <= 65_536 {
                    spare.push(std::mem::take(&mut e.rows));
                }
                keep
            });
        }
    }

    /// Forget everything, for example when wrap is turned off.
    pub fn clear(&mut self) {
        self.entries.clear();
        self.spare.clear();
    }

    /// Number of cached lines.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether nothing is cached.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    fn rows(&mut self, line: &str, width: usize, tab_width: usize) -> &[RowStart] {
        let key = Key::new(line, width, tab_width);
        let (frame, spare) = (self.frame, &mut self.spare);
        let entry = self.entries.entry(key).or_insert_with(|| {
            let mut rows = spare.pop().unwrap_or_default();
            wrap_line(line, width, tab_width, &mut rows);
            Entry { rows, used: frame }
        });
        entry.used = frame;
        &entry.rows
    }
}

/// Row lookups for one view: the cache plus the current wrap width (or
/// none, when wrap is off and every line is one row).
#[derive(Debug)]
pub struct WrapView<'a> {
    cache: &'a mut WrapCache,
    width: Option<usize>,
    tab_width: usize,
}

impl<'a> WrapView<'a> {
    /// Rows for lines wrapped at `width` columns, or unwrapped for `None`.
    pub fn new(cache: &'a mut WrapCache, width: Option<usize>, tab_width: usize) -> Self {
        Self {
            cache,
            width,
            tab_width,
        }
    }

    /// Columns per tab stop.
    #[must_use]
    pub fn tab_width(&self) -> usize {
        self.tab_width
    }

    /// Whether lines are wrapped.
    #[must_use]
    pub fn is_wrapping(&self) -> bool {
        self.width.is_some()
    }

    /// Row starts for `line`.
    pub fn rows(&mut self, line: &str) -> &[RowStart] {
        match self.width {
            Some(w) if !fits(line, w) => self.cache.rows(line, w, self.tab_width),
            _ => ONE_ROW,
        }
    }

    /// Row starts for the line `span` of `t`. Reads the line's text only
    /// when wrapping: unwrapped, every line is one row.
    pub fn rows_at(&mut self, t: &dyn Text, span: LineSpan) -> &[RowStart] {
        // Long lines are never wrapped (or read whole).
        if self.width.is_none() || crate::long_line::is_long(span) {
            return ONE_ROW;
        }
        let text = t.slice(span.start..span.end);
        self.rows(&text)
    }

    /// Number of rows the line `span` of `t` takes; like
    /// [`WrapView::row_count`], without reading the text when unwrapped.
    pub fn row_count_at(&mut self, t: &dyn Text, span: LineSpan) -> usize {
        if self.width.is_none() || crate::long_line::is_long(span) {
            return 1;
        }
        let text = t.slice(span.start..span.end);
        self.row_count(&text)
    }

    /// Number of rows `line` takes. Unlike [`WrapView::rows`], a line not
    /// in the cache is wrapped into scratch space and not stored, so walking
    /// past many lines during a fast scroll does not fill the cache.
    pub fn row_count(&mut self, line: &str) -> usize {
        match self.width {
            Some(w) if !fits(line, w) => {
                let key = Key::new(line, w, self.tab_width);
                if let Some(e) = self.cache.entries.get(&key) {
                    return e.rows.len();
                }
                wrap_line(line, w, self.tab_width, &mut self.cache.scratch);
                self.cache.scratch.len()
            }
            _ => 1,
        }
    }
}

/// A visual row in the document: a logical line, named by the byte where
/// it starts, and a row within it. Lines are named by position rather than
/// number because a mapped file may not have counted its lines that far
/// yet; ordering by `start` is document order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct RowPos {
    /// Byte where the logical line starts.
    pub start: usize,
    /// Row within the line.
    pub row: usize,
}

/// Move `n` visual rows from `at` (negative is up), stopping at the first
/// and last rows of the text. Returns the new position and how many rows
/// were actually moved. Costs one row count per line passed.
pub fn step_rows(t: &dyn Text, w: &mut WrapView<'_>, at: RowPos, n: isize) -> (RowPos, usize) {
    let mut span = t.line_span(at.start);
    let mut p = RowPos {
        start: span.start,
        row: at.row.min(w.row_count_at(t, span).saturating_sub(1)),
    };
    let mut left = n.unsigned_abs();
    if n >= 0 {
        loop {
            let rows = w.row_count_at(t, span);
            let room = rows - 1 - p.row;
            if left <= room {
                p.row += left;
                left = 0;
                break;
            }
            let Some(next) = span.next else {
                left -= room;
                p.row = rows - 1;
                break;
            };
            left -= room + 1;
            span = t.line_span(next);
            p = RowPos {
                start: span.start,
                row: 0,
            };
        }
    } else {
        loop {
            if left <= p.row {
                p.row -= left;
                left = 0;
                break;
            }
            if p.start == 0 {
                left -= p.row;
                p.row = 0;
                break;
            }
            left -= p.row + 1;
            span = t.line_span(p.start - 1);
            p = RowPos {
                start: span.start,
                row: w.row_count_at(t, span) - 1,
            };
        }
    }
    (p, n.unsigned_abs() - left)
}

/// The visual row containing byte offset `pos`.
pub fn row_pos_of(t: &dyn Text, w: &mut WrapView<'_>, pos: usize) -> RowPos {
    let span = t.line_span(pos);
    let off = pos.saturating_sub(span.start);
    RowPos {
        start: span.start,
        row: row_of(w.rows_at(t, span), off),
    }
}

/// Visual rows from `a` forward to `b`, counting at most `cap`.
pub fn rows_between(
    t: &dyn Text,
    w: &mut WrapView<'_>,
    from: RowPos,
    to: RowPos,
    cap: usize,
) -> usize {
    if to <= from {
        return 0;
    }
    let mut at = from;
    let mut count = 0;
    while at < to && count < cap {
        let (next, moved) = step_rows(t, w, at, 1);
        if moved == 0 {
            break;
        }
        at = next;
        count += 1;
    }
    count
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RopeBuffer;

    fn rows(line: &str, width: usize, tab: usize) -> Vec<(usize, usize)> {
        let mut out = Vec::new();
        wrap_line(line, width, tab, &mut out);
        out.iter().map(|r| (r.byte, r.col)).collect()
    }

    #[test]
    fn breaks_at_word_boundaries_with_hanging_spaces() {
        assert_eq!(rows("hello world", 8, 4), vec![(0, 0), (6, 6)]);
        assert_eq!(rows("abcd", 4, 4), vec![(0, 0)]);
        assert_eq!(rows("", 4, 4), vec![(0, 0)]);
        assert_eq!(rows("ab      cd", 4, 4), vec![(0, 0), (8, 8)]);
        assert_eq!(rows("abcdefgh ij", 4, 4), vec![(0, 0), (4, 4), (9, 9)]);
    }

    #[test]
    fn hard_breaks_words_longer_than_the_row() {
        assert_eq!(rows("abcdefghij", 4, 4), vec![(0, 0), (4, 4), (8, 8)]);
    }

    #[test]
    fn tabs_use_line_start_stops_and_hang() {
        assert_eq!(rows("abcdefg\tx", 4, 4), vec![(0, 0), (4, 4), (8, 8)]);
        assert_eq!(rows("abcd\tx", 4, 4), vec![(0, 0), (5, 8)]);
    }

    #[test]
    fn wide_characters_move_whole_or_overflow_an_empty_row() {
        assert_eq!(rows("ab日", 3, 4), vec![(0, 0), (2, 2)]);
        assert_eq!(rows("日日日", 3, 4), vec![(0, 0), (3, 2), (6, 4)]);
        assert_eq!(rows("日x", 1, 4), vec![(0, 0), (3, 2)]);
    }

    #[test]
    fn closing_punctuation_stays_with_its_word_and_clusters_stay_whole() {
        assert_eq!(rows("foo(bar), baz", 8, 4), vec![(0, 0), (4, 4), (10, 10)]);
        assert_eq!(rows("e\u{301}e\u{301}e\u{301}", 2, 4), vec![(0, 0), (6, 2)]);
    }

    #[test]
    fn rows_lookups() {
        let mut r = Vec::new();
        wrap_line("hello world", 8, 4, &mut r);
        assert_eq!(row_of(&r, 5), 0);
        assert_eq!(row_of(&r, 6), 1, "a break offset belongs to the next row");
        assert_eq!(row_of(&r, 99), 1);
        assert_eq!(
            byte_at_row_col("hello world", &r, 0, 99, 4),
            5,
            "before the hanging space"
        );
        assert_eq!(byte_at_row_col("hello world", &r, 1, 99, 4), 11);
        assert_eq!(visual_col("hello world", &r, 8, 4), 2);
    }

    #[test]
    fn cache_hits_and_short_lines_skip_it() {
        let mut cache = WrapCache::default();
        let mut w = WrapView::new(&mut cache, Some(8), 4);
        assert_eq!(w.rows("short").len(), 1);
        assert_eq!(w.rows("hello world").len(), 2);
        assert_eq!(w.rows("hello world").len(), 2);
        assert_eq!(cache.len(), 1);
        let mut w = WrapView::new(&mut cache, Some(20), 4);
        assert_eq!(w.rows("hello world").len(), 1, "fits at 20: no entry");
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn cache_sweeps_unused_entries() {
        let mut cache = WrapCache::default();
        for i in 0..1100 {
            let line = format!("line number {i} that is long enough to wrap");
            WrapView::new(&mut cache, Some(10), 4).rows(&line);
        }
        for _ in 0..9 {
            cache.next_frame();
        }
        assert!(cache.len() < MAX_ENTRIES);
        assert_ne!(cache.spare.len(), 0);
    }

    #[test]
    fn stepping_rows_across_wrapped_lines() {
        let t = RopeBuffer::from_text("aaaaaaaa bbbbbbbb cccccccc\nx\ny");
        let mut cache = WrapCache::default();
        let mut w = WrapView::new(&mut cache, Some(11), 4);
        // Lines start at 0, 27 and 29.
        let start = RowPos { start: 0, row: 0 };
        assert_eq!(
            step_rows(&t, &mut w, start, 2),
            (RowPos { start: 0, row: 2 }, 2)
        );
        assert_eq!(
            step_rows(&t, &mut w, start, 3),
            (RowPos { start: 27, row: 0 }, 3)
        );
        assert_eq!(
            step_rows(&t, &mut w, start, 99),
            (RowPos { start: 29, row: 0 }, 4)
        );
        let end = RowPos { start: 29, row: 0 };
        assert_eq!(
            step_rows(&t, &mut w, end, -3),
            (RowPos { start: 0, row: 1 }, 3)
        );
        assert_eq!(step_rows(&t, &mut w, end, -99), (start, 4));
        assert_eq!(rows_between(&t, &mut w, start, end, 99), 4);
        assert_eq!(rows_between(&t, &mut w, start, end, 2), 2, "capped");
        assert_eq!(row_pos_of(&t, &mut w, 20), RowPos { start: 0, row: 2 });
        assert_eq!(row_pos_of(&t, &mut w, 28), RowPos { start: 27, row: 0 });
    }

    /// Wrapping cost for the longest allowed line (1 MB). Ignored by
    /// default; run with `cargo test --release -p nimbus-core -- --ignored
    /// --nocapture wrap_one_mb`.
    #[test]
    #[ignore = "benchmark; run explicitly in release mode"]
    fn wrap_one_mb_line_benchmark() {
        let words = ["alpha", "beta", "gamma", "delta", "epsilon"];
        let mut line = String::new();
        let mut i = 0;
        while line.len() < 1_000_000 {
            line.push_str(words[i % 5]);
            line.push(' ');
            i += 1;
        }
        let mut rows = Vec::new();
        let start = std::time::Instant::now();
        wrap_line(&line, 150, 4, &mut rows);
        let took = start.elapsed();
        println!("wrap_line 1 MB at 150 cols: {took:?}, {} rows", rows.len());
        let mut cache = WrapCache::default();
        let start = std::time::Instant::now();
        WrapView::new(&mut cache, Some(150), 4).rows(&line);
        let miss = start.elapsed();
        let start = std::time::Instant::now();
        WrapView::new(&mut cache, Some(150), 4).rows(&line);
        println!("cache miss {miss:?}, cache hit {:?}", start.elapsed());
    }

    #[test]
    fn unwrapped_view_has_one_row_per_line() {
        let t = RopeBuffer::from_text("aaaaaaaa bbbbbbbb cccccccc\nx");
        let mut cache = WrapCache::default();
        let mut w = WrapView::new(&mut cache, None, 4);
        assert_eq!(
            step_rows(&t, &mut w, RowPos { start: 0, row: 0 }, 1).0,
            RowPos { start: 27, row: 0 }
        );
        assert!(cache.is_empty());
    }

    proptest::proptest! {
        #![proptest_config(proptest::test_runner::Config {
            cases: 256,
            failure_persistence: None,
            ..Default::default()
        })]

        /// Stepping n rows down and n back up returns to the same row,
        /// unless the first step stopped early at the end of the text; and
        /// every position's row steps back to itself through its line start.
        #[test]
        fn stepping_down_and_up_returns(
            pieces in proptest::collection::vec(
                proptest::sample::select(vec!["word ", "longerword ", "\n", "\r\n", "\t", "é", "日本 ", "x"]),
                0..60,
            ),
            width in proptest::option::of(3usize..14),
            from in 0usize..400,
            n in 0isize..12,
        ) {
            let text: String = pieces.concat();
            let t = RopeBuffer::from_text(&text);
            let mut cache = WrapCache::default();
            let mut w = WrapView::new(&mut cache, width, 4);
            let pos = t.line_span(from.min(text.len())).start;
            let start = row_pos_of(&t, &mut w, pos);
            let (down, moved) = step_rows(&t, &mut w, start, n);
            let moved = isize::try_from(moved).unwrap();
            let (back, moved_back) = step_rows(&t, &mut w, down, -moved);
            proptest::prop_assert_eq!(back, start, "{:?} wrapped at {:?}", text, width);
            proptest::prop_assert_eq!(isize::try_from(moved_back).unwrap(), moved);
        }
    }
}
