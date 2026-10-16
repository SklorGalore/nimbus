//! The editor view: which part of the document is on screen, and drawing it.
//!
//! The screen shows *visual rows*. With soft wrap off each line is one row;
//! with it on, long lines take several (see `nimbus_core::wrap`). The scroll
//! position is anchored: a logical line, the byte where the top row starts
//! within it, and a pixel offset into that row. Nothing ever counts rows
//! over the whole document, and an `f32` never has to hold a huge pixel
//! offset (it loses sub-pixel precision past about 16 million pixels).

use std::fmt::Write as _;

use std::ops::Range;

use nimbus_core::wrap::{self, RowPos, RowStart, WrapCache, WrapView};
use nimbus_core::{LineEnding, Selection, Text, column, long_line};
use nimbus_render::{ColorRun, Gpu, Metrics};
use nimbus_syntax::highlight::{Highlighter, Span};

use crate::document::Document;
use crate::find::{Field, FindBar};
use crate::theme::{Color, Ui};

#[expect(
    clippy::cast_precision_loss,
    reason = "column and line counts on screen are far below 2^24"
)]
fn f(n: usize) -> f32 {
    n as f32
}

/// A column count as `f64`, for horizontal positions on long lines, which
/// can be far past where `f32` keeps whole pixels.
#[expect(clippy::cast_precision_loss, reason = "columns stay far below 2^53")]
fn f64_of(n: usize) -> f64 {
    n as f64
}

/// The first column shown when scrolled `scroll_x` pixels sideways, and how
/// many columns the text area is wide.
fn columns_on_screen(layout: &Layout, scroll_x: f64) -> (usize, usize) {
    let cell = layout.metrics.cell_width;
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a small non-negative column count"
    )]
    let width = ((layout.width - layout.text_left) / cell).ceil().max(0.0) as usize;
    (first_col(scroll_x, cell), width)
}

/// The first column shown when scrolled `scroll_x` pixels sideways.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "a non-negative column count"
)]
fn first_col(scroll_x: f64, cell: f32) -> usize {
    (scroll_x / f64::from(cell)).max(0.0).floor() as usize
}

/// Text being composed with an input method, not yet in the document.
#[derive(Debug, Clone, Default)]
pub struct Preedit {
    /// The composition so far.
    pub text: String,
    /// The IME's cursor or highlighted clause within `text`, as a byte range.
    pub cursor: Option<(usize, usize)>,
}

/// What to show besides the text: cursor, composition and search matches.
#[derive(Debug, Clone)]
pub struct Decorations<'a> {
    /// Draw the editor cursor (blink phase, and the editor has focus).
    pub cursor_visible: bool,
    /// Text being composed with an input method.
    pub preedit: Option<&'a Preedit>,
    /// Search matches to highlight, sorted.
    pub matches: &'a [Range<usize>],
    /// The current search match, highlighted more strongly.
    pub current: Option<Range<usize>>,
    /// Interface colours.
    pub ui: &'a Ui,
    /// Syntax colour per capture index of the document's language; empty
    /// when it has none.
    pub captures: &'a [Option<Color>],
}

/// Pixel geometry for one frame, from the window size and font metrics.
#[derive(Debug, Clone, Copy)]
pub struct Layout {
    /// Window height in physical pixels.
    pub height: f32,
    /// Window width in physical pixels.
    pub width: f32,
    /// Space around the text, in pixels.
    pub margin: f32,
    /// Font metrics at the current size.
    pub metrics: Metrics,
    /// Width of the line-number gutter in columns.
    pub gutter_cols: usize,
    /// Left edge of the text area.
    pub text_left: f32,
    /// Scale factor, for sizes that should look the same on any display.
    pub scale: f32,
    /// Top of the text area: below the margin and any bar at the top.
    pub top: f32,
    /// Bottom of the text area: above the status bar.
    pub bottom: f32,
    /// Columns that fit in the text area: the soft-wrap width.
    pub wrap_cols: usize,
    /// Height of the tab bar at the top (0 when hidden).
    pub tabs_h: f32,
    /// Left edge of the editor column: 0, or the sidebar's width.
    pub left: f32,
}

impl Layout {
    /// Geometry for a window of `width × height` physical pixels showing a
    /// document of `len_lines` lines.
    #[must_use]
    pub fn new(metrics: Metrics, width: u32, height: u32, scale: f32, len_lines: usize) -> Self {
        let margin = (8.0 * scale).round();
        let gutter_cols = digits(len_lines).max(2);
        let text_left = (margin + f(gutter_cols + 2) * metrics.cell_width).round();
        #[expect(clippy::cast_precision_loss, reason = "window sizes are exact in f32")]
        let (width, height) = (width as f32, height as f32);
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "a small non-negative column count"
        )]
        let wrap_cols = ((width - text_left - margin) / metrics.cell_width)
            .floor()
            .max(1.0) as usize;
        Self {
            height,
            width,
            margin,
            metrics,
            gutter_cols,
            text_left,
            scale,
            top: margin,
            bottom: height,
            wrap_cols,
            tabs_h: 0.0,
            left: 0.0,
        }
    }

    /// Move the editor column right of a sidebar `left` pixels wide.
    #[must_use]
    pub fn with_sidebar(mut self, left: f32) -> Self {
        let left = left.clamp(0.0, (self.width - 10.0 * self.metrics.cell_width).max(0.0));
        self.text_left += left - self.left;
        self.left = left;
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "a small non-negative column count"
        )]
        let wrap_cols = ((self.width - self.text_left - self.margin) / self.metrics.cell_width)
            .floor()
            .max(1.0) as usize;
        self.wrap_cols = wrap_cols;
        self
    }

    /// Reserve room for the window chrome: a tab bar, `find_rows` rows of
    /// find bar below it, and a status bar at the bottom.
    #[must_use]
    pub fn with_chrome(mut self, tab_bar: bool, find_rows: usize, status: bool) -> Self {
        let lh = self.metrics.line_height;
        let m = self.margin;
        self.tabs_h = if tab_bar { (lh + 1.5 * m).round() } else { 0.0 };
        // The find bar has its own padding: a margin above, between and
        // below its rows.
        let find_h = if find_rows > 0 {
            2.0 * m + f(find_rows) * lh + f(find_rows - 1) * m
        } else {
            0.0
        };
        self.top = self.tabs_h + find_h + m;
        self.bottom = (self.height - if status { lh } else { 0.0 }).max(self.top + lh);
        self
    }

    /// Top of the find bar area, just below the tab bar.
    fn find_top(&self) -> f32 {
        self.tabs_h
    }

    /// Height of the find bar area, if any rows are reserved for it.
    fn bar_height(&self) -> f32 {
        (self.top - self.margin - self.tabs_h).max(0.0)
    }

    /// Lines that fit entirely in the window: one "page" for Page Up/Down.
    #[must_use]
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a small non-negative line count"
    )]
    pub fn full_lines(&self) -> usize {
        (((self.bottom - self.top) / self.metrics.line_height).floor() as usize).max(1)
    }
}

/// The part of `seg` that can be visible: `seg` starts at column
/// `start_col` of its line, the view's left edge is at column `first_col`,
/// and `width_cols` columns fit. Returns the byte range within `seg` and the
/// column where it starts. Starts at or before `first_col`, so a wide
/// character straddling the edge is included, and ends after the last
/// visible column. Only this span is shaped, so long lines cost what is on
/// screen.
fn visible_span(
    seg: &str,
    start_col: usize,
    first_col: usize,
    width_cols: usize,
    tab_width: usize,
) -> (usize, usize, usize) {
    use unicode_segmentation::UnicodeSegmentation;
    let mut col = start_col;
    let mut start = None;
    let last_col = first_col + width_cols + 1;
    for (i, g) in seg.grapheme_indices(true) {
        let w = column::grapheme_cols(g, col, tab_width);
        if start.is_none() && col + w > first_col {
            start = Some((i, col));
        }
        if col >= last_col {
            let (s, sc) = start.unwrap_or((i, col));
            return (s, i, sc);
        }
        col += w;
    }
    let (s, sc) = start.unwrap_or((seg.len(), col));
    (s, seg.len(), sc)
}

/// The bytes to highlight for a frame whose top row is `at`, into `out`
/// (sorted, not overlapping): the lines that can be on screen (at most one
/// per row) when that is little text, or else just the visible part of each
/// row, so a long line costs what is on screen, not its length.
fn highlight_ranges(
    t: &dyn Text,
    w: &mut WrapView<'_>,
    at: RowPos,
    layout: &Layout,
    scroll_x: f64,
    out: &mut Vec<Range<usize>>,
) {
    out.clear();
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a small non-negative row count"
    )]
    let rows = ((layout.bottom - layout.top) / layout.metrics.line_height)
        .ceil()
        .max(0.0) as usize
        + 1;
    let first = at.start;
    // The start of the line `rows` lines down, or the end of the text.
    let mut end = first;
    for _ in 0..rows {
        let Some(next) = t.line_span(end).next else {
            end = t.len_bytes();
            break;
        };
        end = next;
    }
    if end - first <= WHOLE_LINES_LIMIT {
        out.push(first..end);
    } else {
        visible_ranges(t, w, at, rows, columns_on_screen(layout, scroll_x), out);
    }
}

/// Above this many bytes in the visible lines, highlighting works out the
/// visible bytes of each row instead of taking whole lines.
const WHOLE_LINES_LIMIT: usize = 64 * 1024;

/// Visible ranges closer than this are highlighted as one.
const MERGE_GAP: usize = 4096;

#[expect(clippy::cast_precision_loss, reason = "scrollbar geometry")]
fn f32_of(n: usize) -> f32 {
    n as f32
}

/// Start a frame with the text area as its own layer, clipped so rows
/// scrolled under a bar above or below do not show through it.
fn begin_text(gpu: &mut Gpu, layout: &Layout, ui: &Ui) {
    gpu.begin_frame();
    gpu.set_background(ui.background);
    gpu.begin_layer(Some([
        layout.left,
        layout.top,
        layout.width - layout.left,
        layout.bottom - layout.top,
    ]));
}

/// How far each way bracket matching counts when there is no syntax tree.
const BRACKET_LIMIT: usize = 64 * 1024;

/// The bracket next to the primary cursor and its partner: from the syntax
/// tree when it is up to date, otherwise by counting within
/// [`BRACKET_LIMIT`] bytes. Only for a bare cursor.
fn bracket_pair(doc: &Document) -> Option<(usize, usize)> {
    let sel = doc.selection();
    if !sel.is_empty() {
        return None;
    }
    let t = &doc.buffer;
    doc.syntax
        .matching_bracket(t, sel.head)
        .unwrap_or_else(|| nimbus_core::brackets::matching(t, sel.head, BRACKET_LIMIT))
}

/// Whether a cursor is on the line `span` (its number then stands out).
/// `next` walks forward through the sorted `selections` from line to line,
/// so the whole screen costs one pass.
fn has_cursor(selections: &[Selection], next: &mut usize, span: nimbus_core::LineSpan) -> bool {
    while selections.get(*next).is_some_and(|s| s.head < span.start) {
        *next += 1;
    }
    selections.get(*next).is_some_and(|s| s.head <= span.end)
}

/// Selections up to this many bytes show their length in characters in
/// the status bar; longer ones in bytes, which needs no reading.
const STATUS_COUNT_LIMIT: usize = 1024 * 1024;

/// `n` bytes as a short human-readable size: `512 B`, `12.0 KB`, `3.4 MB`.
fn human_bytes(n: usize) -> String {
    #[expect(clippy::cast_precision_loss, reason = "a size for display")]
    let x = n as f64;
    if n < 1024 {
        format!("{n} B")
    } else if n < 1024 * 1024 {
        format!("{:.1} KB", x / 1024.0)
    } else if n < 1024 * 1024 * 1024 {
        format!("{:.1} MB", x / (1024.0 * 1024.0))
    } else {
        format!("{:.1} GB", x / (1024.0 * 1024.0 * 1024.0))
    }
}

/// Add `r` to the sorted ranges in `out`, joining it to the last one if
/// they are close.
fn push_merged(out: &mut Vec<Range<usize>>, r: Range<usize>) {
    match out.last_mut() {
        Some(last) if r.start <= last.end + MERGE_GAP => last.end = last.end.max(r.end),
        _ => out.push(r),
    }
}

/// The visible bytes of each of `rows` rows from `at`, into `out`.
/// `(scroll_cols, width)` is the horizontal scroll and window width in
/// columns, as each row is drawn.
fn visible_ranges(
    t: &dyn Text,
    w: &mut WrapView<'_>,
    mut at: RowPos,
    rows: usize,
    (scroll_cols, width): (usize, usize),
    out: &mut Vec<Range<usize>>,
) {
    let tab_width = w.tab_width();
    let mut left = rows;
    while left > 0 {
        let span = t.line_span(at.start);
        if long_line::is_long(span) {
            // One row, and only its window is on screen.
            push_merged(out, long_line::window(t, span, scroll_cols, width));
            left -= 1;
        } else {
            let text = t.slice(span.start..span.end);
            let starts: &[RowStart] = w.rows(&text);
            for r in at.row..starts.len() {
                if left == 0 {
                    break;
                }
                left -= 1;
                let range = wrap::row_range(starts, r, text.len());
                let seg = text.get(range.clone()).unwrap_or("");
                let col = starts[r].col;
                let (s, e, _) = visible_span(seg, col, col + scroll_cols, width, tab_width);
                let base = span.start + range.start;
                push_merged(out, base + s..base + e);
            }
        }
        let Some(next) = span.next else {
            break;
        };
        at = RowPos {
            start: next,
            row: 0,
        };
    }
}

/// The single row of a long line: only the bytes around the visible
/// columns are read, into `buf`. `geom` is the first visible column, the
/// width in columns, and the horizontal scroll in pixels.
fn long_row<'a>(
    t: &dyn Text,
    span: nimbus_core::LineSpan,
    buf: &'a mut String,
    layout: &Layout,
    (scroll_col, width_cols, scroll_x): (usize, usize, f64),
    top: f32,
    tab_width: usize,
) -> RowGeom<'a> {
    let win = long_line::window(t, span, scroll_col, width_cols);
    buf.clear();
    t.slice_into(win.clone(), buf);
    let start_col = long_line::col_at(t, span, win.start, tab_width);
    // Worked out in f64, so only a small offset reaches f32.
    #[expect(
        clippy::cast_possible_truncation,
        reason = "the offset is within the window"
    )]
    let left = (f64::from(layout.text_left)
        + f64_of(start_col) * f64::from(layout.metrics.cell_width)
        - scroll_x) as f32;
    RowGeom {
        seg: buf,
        seg_start: win.start,
        start_col,
        last_row: win.end == span.end,
        has_break: span.next.is_some(),
        top,
        left,
    }
}

/// Number of decimal digits in `n` (at least 1).
fn digits(n: usize) -> usize {
    n.checked_ilog10().map_or(1, |d| d as usize + 1)
}

/// Row lookups for `layout`: wrapped at its width when `wrap` is on.
fn wrapper<'a>(
    cache: &'a mut WrapCache,
    wrap: bool,
    layout: &Layout,
    tab_width: usize,
) -> WrapView<'a> {
    WrapView::new(cache, wrap.then_some(layout.wrap_cols), tab_width)
}

/// The top visible row: the row containing `scroll_top`. Snapping to the
/// containing row keeps this right after edits above the top, and after
/// wrapping changes.
fn anchor(scroll_top: usize, t: &dyn Text, w: &mut WrapView<'_>) -> RowPos {
    wrap::row_pos_of(t, w, scroll_top.min(t.len_bytes()))
}

/// Scroll state and per-frame scratch space.
#[derive(Debug, Default)]
pub struct View {
    /// Byte where the top row starts. A byte rather than a line and row:
    /// the same text stays at the top when a resize rewraps the line, and
    /// it needs no line number, which a mapped file may not know yet.
    pub scroll_top: usize,
    /// How far the top row is scrolled up out of view, `0..line_height`.
    pub scroll_px: f32,
    /// Horizontal scroll in pixels. While wrapping it moves only long
    /// lines, which are never wrapped. `f64`: a 100 MB line is a billion
    /// pixels wide, where `f32` no longer holds whole pixels.
    pub scroll_x: f64,
    /// Soft wrap on.
    pub wrap: bool,
    cache: WrapCache,
    /// Where the cursor was drawn in the last frame, for the IME.
    cursor_at: Option<[f32; 2]>,
    /// Reused for formatting line numbers without allocating per frame.
    number: String,
    /// Reused for splicing preedit text into the cursor's row.
    spliced: String,
    /// Reused for the visible window of a long line.
    long_text: String,
    /// Reused for the byte ranges to highlight.
    hl_ranges: Vec<Range<usize>>,
    /// Syntax spans for the visible lines, cached between frames.
    highlighter: Highlighter,
    /// Reused for each row's colour runs.
    runs: Vec<ColorRun>,
    /// Reused for the right side of the status bar.
    status: String,
    /// How many bytes of text the last frame showed, for the scrollbar's
    /// thumb. Kept as a count rather than an end, so it stays right while
    /// a drag moves the scroll before the next frame.
    shown: usize,
    /// Where the scrollbar was drawn in the last frame, for clicks.
    scrollbar: Option<Scrollbar>,
    /// The pointer is over the scrollbar: it is drawn wider.
    pub scrollbar_hover: bool,
}

/// The scrollbar as drawn: a track down the right edge of the text and a
/// thumb showing which part of the text is on screen. Its position is by
/// byte, not line, so it needs no line count and works on a 4 GB file
/// before its lines are counted.
#[derive(Debug, Clone, Copy)]
struct Scrollbar {
    /// Left edge of the area that takes clicks (wider than the bar).
    x: f32,
    top: f32,
    h: f32,
    thumb_top: f32,
    thumb_h: f32,
}

/// What a press on the scrollbar hit.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ScrollbarHit {
    /// The thumb, this far below its top: dragging keeps that point under
    /// the pointer.
    Thumb(f32),
    /// The track: jump so the thumb is centred there.
    Track,
}

/// Everything needed to draw one visual row.
#[derive(Debug, Clone, Copy)]
struct RowGeom<'a> {
    /// The row's text.
    seg: &'a str,
    /// Document byte offset where `seg` starts.
    seg_start: usize,
    /// Column of `seg` within its logical line.
    start_col: usize,
    /// The last row of its line (the line break, if any, follows it).
    last_row: bool,
    /// A line break follows this line (it is not the end of the text).
    has_break: bool,
    /// Top edge on screen.
    top: f32,
    /// Screen x of column `start_col`.
    left: f32,
}

impl RowGeom<'_> {
    fn end(&self) -> usize {
        self.seg_start + self.seg.len()
    }

    /// Whether document offset `pos` is shown on this row. A row's end is
    /// the next row's start, so only the last row of a line owns its end.
    fn owns(&self, pos: usize) -> bool {
        pos >= self.seg_start && (pos < self.end() || (self.last_row && pos == self.end()))
    }

    /// Screen x of document offset `pos`, which must be on this row.
    fn x_of(&self, pos: usize, cell: f32, tab_width: usize) -> f32 {
        let col = column::col_at_from(self.seg, pos - self.seg_start, self.start_col, tab_width);
        self.left + f(col - self.start_col) * cell
    }
}

impl View {
    /// Pixel offset of the top row, rounded so text stays on whole pixels.
    fn px(&self) -> f32 {
        self.scroll_px.round()
    }

    fn set_anchor(&mut self, t: &dyn Text, layout: &Layout, tab_width: usize, at: RowPos) {
        let mut w = wrapper(&mut self.cache, self.wrap, layout, tab_width);
        let span = t.line_span(at.start);
        self.scroll_top = span.start + w.rows_at(t, span).get(at.row).map_or(0, |r| r.byte);
    }

    /// Turn soft wrap on or off, keeping the top line where it is.
    pub fn set_wrap(&mut self, on: bool) {
        // The top stays in the same line: drawing snaps `scroll_top` to the
        // row containing it.
        self.wrap = on;
        self.scroll_x = 0.0;
        if !on {
            self.cache.clear();
        }
    }

    /// Scroll vertically by `dy` pixels (positive moves toward the end).
    pub fn scroll_by(&mut self, dy: f32, doc: &Document, layout: &Layout, tab_width: usize) {
        let t = &doc.buffer;
        let lh = layout.metrics.line_height;
        let px = self.scroll_px + dy;
        let k_f = (px / lh).floor();
        #[expect(
            clippy::cast_possible_truncation,
            reason = "k is a row count from one scroll gesture"
        )]
        let k = k_f as isize;
        let mut w = wrapper(&mut self.cache, self.wrap, layout, tab_width);
        let anchor = anchor(self.scroll_top, t, &mut w);
        let (to, moved) = wrap::step_rows(t, &mut w, anchor, k);
        let (end, _) = wrap::step_rows(t, &mut w, to, 1);
        let mut rest = px - k_f * lh;
        // At either end of the text, stop exactly on a row.
        if moved < k.unsigned_abs() || end == to {
            rest = 0.0;
        }
        self.scroll_px = rest.clamp(0.0, lh);
        self.set_anchor(t, layout, tab_width, to);
    }

    /// Scroll so the row holding the byte `fraction` of the way through the
    /// text is at the top: what dragging a scrollbar does (its drawing is
    /// M8). Works by byte, so it needs no line numbers and goes anywhere
    /// in a huge file before its lines are counted. A scrollbar by bytes
    /// moves unevenly over lines of very different lengths, which is the
    /// price of not counting them.
    pub fn scroll_to_fraction(
        &mut self,
        fraction: f64,
        doc: &Document,
        layout: &Layout,
        tab_width: usize,
    ) {
        let t = &doc.buffer;
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "a byte offset from a clamped fraction of the length"
        )]
        let byte = (fraction.clamp(0.0, 1.0) * f64_of(t.len_bytes())) as usize;
        let mut w = wrapper(&mut self.cache, self.wrap, layout, tab_width);
        let at = anchor(byte, t, &mut w);
        self.scroll_px = 0.0;
        self.set_anchor(t, layout, tab_width, at);
    }

    /// The right side of the status bar for `doc`: the cursor's line and
    /// column (`~` for a line not counted yet in a huge file), how much is
    /// selected, the line ending, the encoding, the language and, while a
    /// huge file's lines are being counted, how far that has got. Built in
    /// a reused string, so drawing it each frame allocates nothing.
    pub fn status_info(&mut self, doc: &Document, tab_width: usize) -> &str {
        let out = &mut self.status;
        out.clear();
        let t = &doc.buffer;
        let sel = doc.selection();
        let count = doc.selections.count();
        if count > 1 {
            let _ = write!(out, "{count} cursors   ");
        }
        let selected = doc.selected_len();
        if selected > 0 {
            // Counting characters reads the selections; past a size where
            // that costs, bytes say as much.
            if selected <= STATUS_COUNT_LIMIT {
                let n: usize = doc
                    .selections
                    .iter()
                    .map(|s| t.slice(s.range()).chars().count())
                    .sum();
                let _ = write!(out, "{n} selected   ");
            } else {
                let _ = write!(out, "{} selected   ", human_bytes(selected));
            }
        }
        let head = sel.head;
        let col = nimbus_core::movement::column_of(t, head, tab_width) + 1;
        match t.line_number(head) {
            Some(line) => {
                let _ = write!(out, "Ln {}, Col {col}", line + 1);
            }
            None => {
                let _ = write!(out, "Ln ~, Col {col}");
            }
        }
        out.push_str("   ");
        out.push_str(match doc.line_ending {
            LineEnding::Lf => "LF",
            LineEnding::CrLf => "CRLF",
        });
        if let Some(encoding) = doc.encoding_label() {
            out.push_str("   ");
            out.push_str(encoding);
        }
        if let Some(name) = doc.language_name() {
            out.push_str("   ");
            out.push_str(name);
        }
        if doc.counting_lines() {
            #[expect(clippy::cast_precision_loss, reason = "a percentage for display")]
            let pct = t.indexed_bytes() as f64 * 100.0 / t.len_bytes().max(1) as f64;
            let _ = write!(out, "   Counting lines {pct:.0}%");
        }
        &self.status
    }

    /// Scroll horizontally by `dx` pixels. Does nothing while wrapping.
    pub fn scroll_x_by(&mut self, dx: f32) {
        self.scroll_x = (self.scroll_x + f64::from(dx)).max(0.0);
    }

    /// Scroll as little as possible to bring the cursor into view.
    pub fn ensure_cursor_visible(&mut self, doc: &Document, layout: &Layout, tab_width: usize) {
        let t = &doc.buffer;
        let cursor = doc.cursor();
        let full = layout.full_lines();
        let mut w = wrapper(&mut self.cache, self.wrap, layout, tab_width);
        let cursor_row = wrap::row_pos_of(t, &mut w, cursor);
        let top_row = anchor(self.scroll_top, t, &mut w);
        let new_anchor = if cursor_row < top_row || (cursor_row == top_row && self.scroll_px > 0.0)
        {
            Some(cursor_row)
        } else if wrap::rows_between(t, &mut w, top_row, cursor_row, full) >= full {
            let back = isize::try_from(full - 1).unwrap_or(0);
            Some(wrap::step_rows(t, &mut w, cursor_row, -back).0)
        } else {
            None
        };
        if let Some(at) = new_anchor {
            self.scroll_px = 0.0;
            self.set_anchor(t, layout, tab_width, at);
        }
        // Wrapped lines never scroll sideways; long lines always can.
        if self.wrap && !long_line::is_long(t.line_span(cursor)) {
            return;
        }
        let cell = f64::from(layout.metrics.cell_width);
        let x = f64_of(nimbus_core::movement::column_of(t, cursor, tab_width)) * cell;
        let visible = f64::from(
            (layout.width - layout.text_left - layout.margin - layout.metrics.cell_width)
                .max(layout.metrics.cell_width),
        );
        if x < self.scroll_x {
            self.scroll_x = (x - 4.0 * cell).max(0.0);
        } else if x > self.scroll_x + visible {
            self.scroll_x = x - visible + 4.0 * cell;
        }
    }

    /// Lookups for the current wrap setting, for cursor movement by row.
    pub fn wrap_view(&mut self, layout: &Layout, tab_width: usize) -> WrapView<'_> {
        wrapper(&mut self.cache, self.wrap, layout, tab_width)
    }

    /// The byte offset under window position `(x, y)` in physical pixels.
    pub fn hit(
        &mut self,
        x: f32,
        y: f32,
        doc: &Document,
        layout: &Layout,
        tab_width: usize,
    ) -> usize {
        let t = &doc.buffer;
        let lh = layout.metrics.line_height;
        let rows_down = ((y - layout.top + self.px()) / lh).floor().max(0.0);
        #[expect(
            clippy::cast_possible_truncation,
            reason = "a row count within the window"
        )]
        let rows_down = rows_down as isize;
        let mut w = wrapper(&mut self.cache, self.wrap, layout, tab_width);
        let anchor = anchor(self.scroll_top, t, &mut w);
        let (at, _) = wrap::step_rows(t, &mut w, anchor, rows_down);
        let span = t.line_span(at.start);
        let long = long_line::is_long(span);
        // Wrapped rows do not scroll sideways; long lines and unwrapped
        // ones do.
        let scroll = if self.wrap && !long {
            0.0
        } else {
            self.scroll_x
        };
        let x_text = (f64::from(x - layout.text_left) + scroll).max(0.0);
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "non-negative half-column count"
        )]
        let half = (x_text / f64::from(layout.metrics.cell_width / 2.0)).floor() as usize;
        if long {
            return long_line::byte_at_half_col(t, span, half, tab_width);
        }
        let text = t.slice(span.start..span.end);
        let rows = w.rows(&text);
        span.start + wrap::byte_at_row_half_col(&text, rows, at.row, half, tab_width)
    }

    /// The cursor's rectangle on screen as `[x, y, width, height]` from the
    /// last frame drawn, for the IME's candidate window. `preedit_cols` is
    /// the width of any composition in progress.
    #[must_use]
    pub fn cursor_rect(&self, layout: &Layout, preedit_cols: usize) -> [f32; 4] {
        let m = layout.metrics;
        let [x, y] = self.cursor_at.unwrap_or([layout.text_left, layout.bottom]);
        [
            x.round(),
            y,
            f(preedit_cols.max(1)) * m.cell_width,
            m.line_height,
        ]
    }

    /// Queue everything visible for one frame.
    pub fn draw(
        &mut self,
        gpu: &mut Gpu,
        doc: &Document,
        layout: &Layout,
        tab_width: usize,
        deco: &Decorations<'_>,
    ) {
        let t = &doc.buffer;
        let lh = layout.metrics.line_height;
        let cursor = doc.cursor();
        let selections = doc.selections.as_slice();
        let brackets = bracket_pair(doc);
        self.cache.next_frame();
        self.cursor_at = None;

        begin_text(gpu, layout, deco.ui);

        let px = self.px();
        let (wrapping, scroll_x) = (self.wrap, self.scroll_x);
        let mut w = wrapper(&mut self.cache, wrapping, layout, tab_width);
        let mut at = anchor(self.scroll_top, t, &mut w);
        let mut next_match = deco.matches.partition_point(|m| m.end <= at.start);
        // Selections and cursor heads are sorted, so each is found by
        // walking forward with the rows, as matches are.
        let first_sel = selections.partition_point(|s| s.range().end < at.start);
        let mut next_head = selections.partition_point(|s| s.head < at.start);
        // Counted once for the top line, then by one per line while known.
        let mut number = t.line_number(at.start);
        let mut shown_end = t.len_bytes();
        let spans: &[Span] = if deco.captures.is_empty() {
            &[]
        } else {
            highlight_ranges(t, &mut w, at, layout, scroll_x, &mut self.hl_ranges);
            self.highlighter.spans(&doc.syntax, t, &self.hl_ranges)
        };
        let mut top = layout.top - px;
        let mut painter = Painter {
            spans,
            runs: &mut self.runs,
            gpu,
            layout,
            tab_width,
            deco,
            wrapping,
            sels: selections,
            next_sel: first_sel,
            brackets,
            cursor,
            number: &mut self.number,
            spliced: &mut self.spliced,
            cursor_at: None,
        };
        let (scroll_col, width_cols) = columns_on_screen(layout, scroll_x);
        while top < layout.bottom {
            let span = t.line_span(at.start);
            let line_start = span.start;
            let has_break = span.next.is_some();
            // The line number stands out on every line with a cursor.
            let has_cursor = has_cursor(selections, &mut next_head, span);
            if long_line::is_long(span) {
                let geom = (scroll_col, width_cols, scroll_x);
                let row = long_row(t, span, &mut self.long_text, layout, geom, top, tab_width);
                painter.gutter(number, has_cursor, top);
                next_match = painter.row(&row, next_match, scroll_col);
                top += lh;
            } else {
                let text = t.slice(span.start..span.end);
                let rows: &[RowStart] = w.rows(&text);
                for r in at.row..rows.len() {
                    if top >= layout.bottom {
                        break;
                    }
                    let range = wrap::row_range(rows, r, text.len());
                    let start_col = rows[r].col;
                    #[expect(
                        clippy::cast_possible_truncation,
                        reason = "a normal line is under 64 KB of columns"
                    )]
                    let shift = if wrapping { 0.0 } else { scroll_x as f32 };
                    let row = RowGeom {
                        seg: text.get(range.clone()).unwrap_or(""),
                        seg_start: line_start + range.start,
                        start_col,
                        last_row: r + 1 == rows.len(),
                        has_break,
                        top,
                        left: layout.text_left - shift,
                    };
                    if r == 0 {
                        painter.gutter(number, has_cursor, top);
                    }
                    let first = if wrapping { start_col } else { scroll_col };
                    next_match = painter.row(&row, next_match, first);
                    top += lh;
                }
            }
            shown_end = span.next.unwrap_or(t.len_bytes());
            let Some(next) = span.next else {
                break;
            };
            number = number.and_then(|n| (next <= t.indexed_bytes()).then_some(n + 1));
            at = RowPos {
                start: next,
                row: 0,
            };
        }
        self.cursor_at = painter.cursor_at;
        self.shown = shown_end.saturating_sub(self.scroll_top);
    }

    /// Draw the scrollbar for the frame just drawn, or nothing when the
    /// whole text fits on screen.
    pub fn draw_scrollbar(&mut self, gpu: &mut Gpu, doc: &Document, layout: &Layout, ui: &Ui) {
        self.scrollbar = self.scrollbar_geometry(doc.buffer.len_bytes(), layout);
        let Some(bar) = self.scrollbar else { return };
        let cell = layout.metrics.cell_width;
        let w = (cell * if self.scrollbar_hover { 0.9 } else { 0.5 })
            .round()
            .max(3.0);
        let x = layout.width - w - 2.0 * layout.scale;
        gpu.begin_layer(None);
        gpu.push_rect([x, bar.thumb_top], [w, bar.thumb_h], ui.scrollbar);
    }

    /// Where the scrollbar goes for a text of `len` bytes, as last drawn;
    /// `None` when it all fits on screen. The thumb's height is the share
    /// of the text on screen (at least three cells), and its place the
    /// share scrolled past, both by byte.
    fn scrollbar_geometry(&self, len: usize, layout: &Layout) -> Option<Scrollbar> {
        let shown = self.shown;
        if len == 0 || (self.scroll_top == 0 && shown >= len) {
            return None;
        }
        let cell = layout.metrics.cell_width;
        let (top, h) = (layout.top, layout.bottom - layout.top);
        let thumb_h = (h * f32_of(shown) / f32_of(len)).clamp((3.0 * cell).min(h), h);
        let frac = (f32_of(self.scroll_top) / f32_of(len.saturating_sub(shown).max(1))).min(1.0);
        Some(Scrollbar {
            x: layout.width - cell,
            top,
            h,
            thumb_top: top + (h - thumb_h) * frac,
            thumb_h,
        })
    }

    /// The scrollbar thumb's height in the last frame (0 when none).
    #[must_use]
    pub fn thumb_height(&self) -> f32 {
        self.scrollbar.map_or(0.0, |b| b.thumb_h)
    }

    /// What a press at `(x, y)` hits on the scrollbar, if anything.
    #[must_use]
    pub fn scrollbar_hit(&self, x: f32, y: f32) -> Option<ScrollbarHit> {
        let bar = self.scrollbar?;
        if x < bar.x || y < bar.top || y > bar.top + bar.h {
            return None;
        }
        Some(
            if (bar.thumb_top..=bar.thumb_top + bar.thumb_h).contains(&y) {
                ScrollbarHit::Thumb(y - bar.thumb_top)
            } else {
                ScrollbarHit::Track
            },
        )
    }

    /// Drag the thumb so that its top is at `y`: scroll by the same
    /// fraction of the text ([`View::scroll_to_fraction`]).
    pub fn drag_scrollbar(&mut self, y: f32, doc: &Document, layout: &Layout, tab_width: usize) {
        let Some(bar) = self.scrollbar else { return };
        let travel = (bar.h - bar.thumb_h).max(1.0);
        let frac = f64::from(((y - bar.top) / travel).clamp(0.0, 1.0));
        let len = doc.buffer.len_bytes();
        let shown = self.shown;
        let target = frac * f64_of(len.saturating_sub(shown));
        self.scroll_to_fraction(target / f64_of(len.max(1)), doc, layout, tab_width);
    }
}

/// Draws rows for one frame; holds what every row needs.
struct Painter<'a, 'g> {
    gpu: &'g mut Gpu,
    /// Syntax spans covering the visible lines, sorted.
    spans: &'a [Span],
    runs: &'a mut Vec<ColorRun>,
    layout: &'a Layout,
    tab_width: usize,
    deco: &'a Decorations<'a>,
    /// Soft wrap on: rows never scroll sideways.
    wrapping: bool,
    /// Every selection, sorted.
    sels: &'a [Selection],
    /// The first selection that may reach the row being drawn.
    next_sel: usize,
    /// The bracket at the cursor and its partner, to box.
    brackets: Option<(usize, usize)>,
    /// The primary cursor: the input method's composition and the
    /// candidate window go there.
    cursor: usize,
    number: &'a mut String,
    spliced: &'a mut String,
    cursor_at: Option<[f32; 2]>,
}

impl Painter<'_, '_> {
    /// The line number, right-aligned in the gutter, on a line's first row;
    /// `~` where it is not known yet (a mapped file still indexing).
    fn gutter(&mut self, line: Option<usize>, current: bool, top: f32) {
        let cell = self.layout.metrics.cell_width;
        self.number.clear();
        match line {
            Some(n) => {
                let _ = write!(self.number, "{}", n + 1);
            }
            None => self.number.push('~'),
        }
        let pad = self.layout.gutter_cols.saturating_sub(self.number.len());
        let color = if current {
            self.deco.ui.current_line_number
        } else {
            self.deco.ui.gutter
        };
        self.gpu.push_text_line(
            [self.layout.left + self.layout.margin + f(pad) * cell, top],
            self.number,
            color,
        );
    }

    /// Draw one row: selection, matches, text and cursor. `first` is the
    /// leftmost visible column of the line. Returns the index of the first
    /// match not finished on this row.
    fn row(&mut self, row: &RowGeom<'_>, next_match: usize, first: usize) -> usize {
        let (cell, lh) = (
            self.layout.metrics.cell_width,
            self.layout.metrics.line_height,
        );
        let tab = self.tab_width;
        while self
            .sels
            .get(self.next_sel)
            .is_some_and(|s| s.range().end < row.seg_start)
        {
            self.next_sel += 1;
        }
        for i in self.next_sel..self.sels.len() {
            let sel = self.sels[i].range();
            if sel.start > row.end() {
                break;
            }
            if let Some((x0, x1)) = self.selected(row, &sel) {
                self.rect(x0, x1, row.top, self.deco.ui.selection);
            }
        }
        if let Some((a, b)) = self.brackets {
            for at in [a, b] {
                if row.owns(at) && at < row.end() {
                    let x0 = row.x_of(at, cell, tab);
                    let x1 = row.x_of(at + 1, cell, tab);
                    self.rect(x0, x1, row.top, self.deco.ui.bracket);
                }
            }
        }
        let next = self.matches(row, next_match);
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "non-negative column counts within the window"
        )]
        let width = ((self.layout.width - self.layout.text_left) / cell)
            .ceil()
            .max(0.0) as usize;
        let preedit = self.deco.preedit.filter(|p| !p.text.is_empty());
        if row.owns(self.cursor)
            && let Some(p) = preedit
        {
            self.preedit_row(row, p, (first, width));
        } else {
            let origin = [row.left, row.top];
            self.span(
                row.seg,
                Some(row.seg_start),
                row.start_col,
                origin,
                (first, width),
            );
        }
        for i in self.next_sel..self.sels.len() {
            let head = self.sels[i].head;
            if self.sels[i].range().start > row.end() {
                break;
            }
            if !row.owns(head) {
                continue;
            }
            let x = row.x_of(head, cell, tab);
            // Keep a cursor in hanging spaces at the wrap edge visible.
            let max_x = self.layout.text_left + f(self.layout.wrap_cols) * cell;
            let x = if self.wrapping { x.min(max_x) } else { x };
            let primary = head == self.cursor;
            if primary {
                self.cursor_at = Some([x, row.top]);
            }
            let composing = primary && preedit.is_some();
            if self.deco.cursor_visible && !composing && x >= self.layout.text_left {
                let w = (2.0 * self.layout.scale).round().max(1.0);
                self.gpu
                    .push_rect([x, row.top], [w, lh], self.deco.ui.cursor);
            }
        }
        next
    }

    fn rect(&mut self, x0: f32, x1: f32, top: f32, color: [f32; 4]) {
        let x0 = x0.max(self.layout.text_left);
        if x1 > x0 {
            self.gpu
                .push_rect([x0, top], [x1 - x0, self.layout.metrics.line_height], color);
        }
    }

    /// Screen span of the selection on this row. A selection continuing
    /// past the end of a line adds one cell, showing the line break.
    fn selected(&self, row: &RowGeom<'_>, sel: &Range<usize>) -> Option<(f32, f32)> {
        let (start, end) = (row.seg_start, row.end());
        let outside = sel.is_empty()
            || sel.end <= start
            || (row.last_row && (sel.start > end || (sel.start == end && sel.end <= end)))
            || (!row.last_row && sel.start >= end);
        if outside {
            return None;
        }
        let cell = self.layout.metrics.cell_width;
        let x0 = row.x_of(sel.start.max(start), cell, self.tab_width);
        let x1 = row.x_of(sel.end.min(end), cell, self.tab_width);
        let past_end = row.last_row && row.has_break && sel.end > end;
        Some((x0, x1 + if past_end { cell } else { 0.0 }))
    }

    /// Highlight matches on this row; returns where the next row continues.
    fn matches(&mut self, row: &RowGeom<'_>, mut next: usize) -> usize {
        let cell = self.layout.metrics.cell_width;
        let (start, end) = (row.seg_start, row.end());
        while let Some(m) = self.deco.matches.get(next) {
            if m.start > end || (!row.last_row && m.start >= end) {
                break;
            }
            if m.end <= start && m.start < start {
                // Ended with the previous line's break: nothing on this row.
                next += 1;
                continue;
            }
            let x0 = row.x_of(m.start.max(start), cell, self.tab_width);
            // A match including the line break shows one cell for it, as a
            // selection does.
            let past_end = row.last_row && row.has_break && m.end > end;
            let x1 =
                row.x_of(m.end.min(end), cell, self.tab_width) + if past_end { cell } else { 0.0 };
            let x1 = x1.max(x0 + cell);
            let color = if self.deco.current.as_ref() == Some(m) {
                self.deco.ui.current_match
            } else {
                self.deco.ui.find_match
            };
            self.rect(x0, x1, row.top, color);
            if m.end > end {
                break; // continues on the next row
            }
            next += 1;
        }
        next
    }

    /// Draw the visible part of `seg`, which starts at column `start_col`
    /// and is drawn with that column at `origin`. `seg_start` is where
    /// `seg` is in the document, to colour it; `None` draws it plain.
    fn span(
        &mut self,
        seg: &str,
        seg_start: Option<usize>,
        start_col: usize,
        origin: [f32; 2],
        (first, width): (usize, usize),
    ) {
        let (s, e, sc) = visible_span(seg, start_col, first, width, self.tab_width);
        if s >= e {
            return;
        }
        self.runs.clear();
        if let Some(at) = seg_start {
            self.colour_runs(at + s..at + e);
        }
        let x = origin[0] + f(sc - start_col) * self.layout.metrics.cell_width;
        self.gpu.push_text_runs(
            [x, origin[1]],
            &seg[s..e],
            sc,
            self.runs,
            self.deco.ui.text,
            self.layout.text_left,
        );
    }

    /// Fill `runs` with syntax colours for document bytes `range`, as
    /// offsets from its start.
    fn colour_runs(&mut self, range: Range<usize>) {
        let text = self.deco.ui.text;
        let mut pos = range.start;
        let first = self.spans.partition_point(|s| s.end <= range.start);
        for span in self.spans.get(first..).unwrap_or(&[]) {
            if span.start >= range.end {
                break;
            }
            let Some(color) = self
                .deco
                .captures
                .get(span.capture as usize)
                .copied()
                .flatten()
            else {
                continue;
            };
            let (s, e) = (span.start.max(pos), span.end.min(range.end));
            if s > pos {
                self.runs.push(ColorRun {
                    end: s - range.start,
                    color: text,
                });
            }
            self.runs.push(ColorRun {
                end: e - range.start,
                color,
            });
            pos = e;
        }
    }

    /// Draw the cursor's row with the composition `p` spliced in at the
    /// cursor, underlined, with the IME's own cursor or clause marked.
    fn preedit_row(&mut self, row: &RowGeom<'_>, p: &Preedit, view: (usize, usize)) {
        let m = self.layout.metrics;
        let (cell, lh, tab) = (m.cell_width, m.line_height, self.tab_width);
        let thin = (1.5 * self.layout.scale).round().max(1.0);
        let at = self.cursor - row.seg_start;
        self.spliced.clear();
        self.spliced.push_str(row.seg.get(..at).unwrap_or(row.seg));
        self.spliced.push_str(&p.text);
        self.spliced.push_str(row.seg.get(at..).unwrap_or(""));
        let x = |s: &str, off: usize| {
            row.left + f(column::col_at_from(s, off, row.start_col, tab) - row.start_col) * cell
        };
        let (x0, x1) = (x(self.spliced, at), x(self.spliced, at + p.text.len()));
        let underline_y = row.top + m.baseline + thin;
        self.gpu
            .push_rect([x0, underline_y], [x1 - x0, thin], self.deco.ui.preedit);
        if let Some((a, b)) = p.cursor {
            let (ca, cb) = (x(self.spliced, at + a), x(self.spliced, at + b));
            if cb > ca {
                self.gpu.push_rect(
                    [ca, underline_y],
                    [cb - ca, 2.0 * thin],
                    self.deco.ui.preedit,
                );
            } else {
                self.gpu
                    .push_rect([ca, row.top], [thin, lh], self.deco.ui.cursor);
            }
        }
        let spliced = std::mem::take(self.spliced);
        // Composed text is not in the document, so it is drawn uncoloured.
        self.span(&spliced, None, row.start_col, [row.left, row.top], view);
        *self.spliced = spliced;
    }
}

/// Draw the find bar at the top of the window, as an overlay layer.
pub fn draw_find_bar(gpu: &mut Gpu, ui: &Ui, bar: &FindBar, layout: &Layout, cursor_visible: bool) {
    let m = layout.metrics;
    let (cell, lh) = (m.cell_width, m.line_height);
    let height = layout.bar_height();
    let y0 = layout.find_top();
    gpu.begin_layer(None);
    gpu.push_rect(
        [layout.left, y0],
        [layout.width - layout.left, height],
        ui.bar,
    );

    let label_x = layout.left + layout.margin;
    let field_x = (label_x + 9.0 * cell).round();
    // Right side: toggles and the match count.
    let status = bar.status();
    let right_cols = 8 + status.chars().count().max(10);
    let field_w =
        (layout.width - field_x - f(right_cols) * cell - 2.0 * layout.margin).max(10.0 * cell);

    let rows: &[(Field, &str)] = if bar.replace_mode {
        &[(Field::Find, "Find"), (Field::Replace, "Replace")]
    } else {
        &[(Field::Find, "Find")]
    };
    for (row, (field, label)) in rows.iter().enumerate() {
        let y = y0 + layout.margin + f(row) * (lh + layout.margin);
        gpu.push_text_line([label_x, y], label, ui.gutter);
        let focused = bar.focused && bar.focus == *field;
        if focused {
            let b = layout.scale.round().max(1.0);
            gpu.push_rect(
                [field_x - b, y - b],
                [field_w + 2.0 * b, lh + 2.0 * b],
                ui.field_focus,
            );
        }
        gpu.push_rect([field_x, y], [field_w, lh], ui.field);
        let doc = match field {
            Field::Find => &bar.find,
            Field::Replace => &bar.replace,
        };
        draw_field(
            gpu,
            ui,
            doc,
            [field_x, y],
            field_w,
            layout,
            focused && cursor_visible,
        );
    }

    // Toggles: case and regex, then the status.
    let y = y0 + layout.margin;
    let mut x = field_x + field_w + 2.0 * cell;
    for (on, text) in [(bar.case_sensitive, "Aa"), (bar.regex, ".*")] {
        if on {
            gpu.push_rect([x - cell / 2.0, y], [3.0 * cell, lh], ui.toggle_on);
        }
        gpu.push_text_line([x, y], text, ui.text);
        x += 3.5 * cell;
    }
    let color = if bar.error.is_some() {
        ui.error
    } else {
        ui.gutter
    };
    gpu.push_text_line([x.round(), y], &status, color);
}

/// Which row of the find bar is at window height `y`: 0 for the query,
/// 1 for the replacement, `None` outside the bar.
#[must_use]
pub fn find_bar_row(layout: &Layout, y: f32, rows: usize) -> Option<usize> {
    let y0 = layout.find_top();
    if rows == 0 || y < y0 || y >= layout.top - layout.margin {
        return None;
    }
    let row_h = layout.metrics.line_height + layout.margin;
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a small row index"
    )]
    let row = ((y - y0 - layout.margin / 2.0) / row_h).floor().max(0.0) as usize;
    Some(row.min(rows - 1))
}

/// Draw a one-line text field: its first line, selection and cursor,
/// clipped to `width` pixels from `origin`.
pub(crate) fn draw_field(
    gpu: &mut Gpu,
    ui: &Ui,
    doc: &Document,
    origin: [f32; 2],
    width: f32,
    layout: &Layout,
    cursor: bool,
) {
    let m = layout.metrics;
    let cell = m.cell_width;
    let text = doc.buffer.line(0);
    let line_end = text.len();
    let cursor_col = column::col_at(&text, doc.cursor().min(line_end), 4);
    // Scroll the field so the cursor stays inside it.
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a small non-negative column count"
    )]
    let visible = ((width / cell).floor() as usize).saturating_sub(1).max(1);
    let first = cursor_col.saturating_sub(visible);
    let x0 = origin[0] - f(first) * cell;
    gpu.begin_layer(Some([origin[0], origin[1], width, m.line_height]));
    if let Some((a, b)) = field_selection(doc, &text) {
        gpu.push_rect(
            [x0 + f(a) * cell, origin[1]],
            [f(b - a) * cell, m.line_height],
            ui.selection,
        );
    }
    let (start, end, start_col) = visible_span(&text, 0, first, visible + 1, 4);
    gpu.push_text_span(
        [x0 + f(start_col) * cell, origin[1]],
        &text[start..end],
        start_col,
        ui.text,
        origin[0],
    );
    if cursor {
        let w = (2.0 * layout.scale).round().max(1.0);
        gpu.push_rect(
            [x0 + f(cursor_col) * cell, origin[1]],
            [w, m.line_height],
            ui.cursor,
        );
    }
    gpu.begin_layer(None);
}

/// One tab's label, and whether it has unsaved changes.
#[derive(Debug, Clone)]
pub struct TabLabel {
    /// File name, or "untitled".
    pub name: String,
    /// Unsaved changes.
    pub dirty: bool,
}

/// Horizontal extent `[x0, x1)` of each tab in the tab bar, scrolled so the
/// active tab is visible when they do not all fit.
#[must_use]
pub fn tab_extents(layout: &Layout, tabs: &[TabLabel], active: usize) -> Vec<(f32, f32)> {
    let cell = layout.metrics.cell_width;
    let mut x = layout.left + layout.margin;
    let mut out: Vec<(f32, f32)> = tabs
        .iter()
        .map(|t| {
            // Room for the name, a dirty dot and padding on both sides.
            let w = f(t.name.chars().count() + 5) * cell;
            let ext = (x, x + w);
            x += w + 1.0;
            ext
        })
        .collect();
    if let Some(&(_, end)) = out.get(active) {
        let shift = (end - (layout.width - layout.margin)).max(0.0);
        for e in &mut out {
            e.0 -= shift;
            e.1 -= shift;
        }
    }
    out
}

/// The tab under window position `(x, y)`, if any.
#[must_use]
pub fn tab_at(layout: &Layout, tabs: &[TabLabel], active: usize, x: f32, y: f32) -> Option<usize> {
    if y >= layout.tabs_h {
        return None;
    }
    tab_extents(layout, tabs, active)
        .iter()
        .position(|&(a, b)| x >= a && x < b)
}

/// Draw the tab bar across the top of the window.
pub fn draw_tabs(gpu: &mut Gpu, ui: &Ui, layout: &Layout, tabs: &[TabLabel], active: usize) {
    if layout.tabs_h <= 0.0 {
        return;
    }
    let (cell, lh) = (layout.metrics.cell_width, layout.metrics.line_height);
    let w = layout.width - layout.left;
    gpu.begin_layer(Some([layout.left, 0.0, w, layout.tabs_h]));
    gpu.push_rect([layout.left, 0.0], [w, layout.tabs_h], ui.tab_bar);
    let text_y = ((layout.tabs_h - lh) / 2.0).round();
    let accent = (2.0 * layout.scale).round().max(1.0);
    for (i, ((x0, x1), tab)) in tab_extents(layout, tabs, active)
        .into_iter()
        .zip(tabs)
        .enumerate()
    {
        let is_active = i == active;
        if is_active {
            gpu.push_rect([x0, 0.0], [x1 - x0, layout.tabs_h], ui.tab_active);
            gpu.push_rect(
                [x0, layout.tabs_h - accent],
                [x1 - x0, accent],
                ui.tab_accent,
            );
        }
        let color = if is_active { ui.text } else { ui.gutter };
        gpu.push_text_line([x0 + 2.0 * cell, text_y], &tab.name, color);
        if tab.dirty {
            let dot_x = x1 - 2.0 * cell;
            gpu.push_text_line([dot_x, text_y], "●", color);
        }
    }
}

/// What the picker overlay shows.
#[derive(Debug)]
pub struct PickerView<'a> {
    /// The query field.
    pub input: &'a Document,
    /// Hint shown while the query is empty.
    pub placeholder: &'a str,
    /// Visible rows: main text and detail.
    pub rows: &'a [(String, String)],
    /// Index in `rows` of the highlighted one.
    pub selected: Option<usize>,
    /// Summary such as "42 files" or "1,204 files…".
    pub status: String,
}

/// Geometry of the picker box: `[x, y, width]` and row height.
fn picker_box(layout: &Layout) -> ([f32; 3], f32) {
    let cell = layout.metrics.cell_width;
    let w = (layout.width - 4.0 * layout.margin)
        .min(90.0 * cell)
        .max(20.0 * cell);
    let x = ((layout.width - w) / 2.0).round();
    let y = layout.tabs_h + layout.margin;
    (
        [x, y, w],
        layout.metrics.line_height + (layout.margin / 2.0).round(),
    )
}

/// The picker row under window position `(x, y)`, if any.
#[must_use]
pub fn picker_row_at(layout: &Layout, rows: usize, x: f32, y: f32) -> Option<usize> {
    let ([bx, by, bw], row_h) = picker_box(layout);
    let list_y = by + row_h + layout.margin;
    if x < bx || x >= bx + bw || y < list_y {
        return None;
    }
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a small row index"
    )]
    let r = ((y - list_y) / row_h).floor() as usize;
    (r < rows).then_some(r)
}

/// Whether window position `(x, y)` is inside the picker box showing
/// `rows` rows.
#[must_use]
pub fn picker_contains(layout: &Layout, rows: usize, x: f32, y: f32) -> bool {
    let ([bx, by, bw], row_h) = picker_box(layout);
    let h = row_h + layout.margin + f(rows.max(1)) * row_h + (layout.margin / 2.0).round();
    x >= bx && x < bx + bw && y >= by && y < by + h
}

/// Draw the command palette or file opener as an overlay.
pub fn draw_picker(
    gpu: &mut Gpu,
    ui: &Ui,
    layout: &Layout,
    view: &PickerView<'_>,
    cursor_visible: bool,
) {
    let (cell, lh) = (layout.metrics.cell_width, layout.metrics.line_height);
    let ([left, top, width], row_h) = picker_box(layout);
    let pad = (layout.margin / 2.0).round();
    let height = row_h + layout.margin + f(view.rows.len().max(1)) * row_h + pad;
    gpu.begin_layer(None);
    let b = layout.scale.round().max(1.0);
    gpu.push_rect(
        [left - b, top - b],
        [width + 2.0 * b, height + 2.0 * b],
        ui.picker_border,
    );
    gpu.push_rect([left, top], [width, height], ui.picker);
    // The query field, with the status on its right.
    let status_w = f(view.status.chars().count() + 2) * cell;
    let field_w = width - 2.0 * cell - status_w;
    let field_y = top + pad;
    gpu.push_rect(
        [left + cell - pad, field_y - pad / 2.0],
        [field_w + 2.0 * pad, lh + pad],
        ui.field,
    );
    if view.input.buffer.len_bytes() == 0 {
        gpu.push_text_line([left + cell, field_y], view.placeholder, ui.gutter);
    }
    draw_field(
        gpu,
        ui,
        view.input,
        [left + cell, field_y],
        field_w,
        layout,
        cursor_visible,
    );
    gpu.begin_layer(None);
    gpu.push_text_line(
        [left + width - status_w + cell, field_y],
        &view.status,
        ui.gutter,
    );
    // The rows, clipped to the box.
    let list_y = top + row_h + layout.margin;
    gpu.begin_layer(Some([left, list_y, width, height - (list_y - top)]));
    for (i, (text, detail)) in view.rows.iter().enumerate() {
        let ry = list_y + f(i) * row_h;
        if Some(i) == view.selected {
            gpu.push_rect([left, ry], [width, row_h], ui.picker_selected);
        }
        let ty = (ry + (row_h - lh) / 2.0).round();
        gpu.push_text_line([left + cell, ty], text, ui.text);
        let detail_x = left + width - f(detail.chars().count() + 1) * cell;
        let text_end = left + cell + f(text.chars().count() + 2) * cell;
        gpu.push_text_line([detail_x.max(text_end), ty], detail, ui.gutter);
    }
    if view.rows.is_empty() {
        gpu.push_text_line([left + cell, list_y], "No matches", ui.gutter);
    }
}

/// Draw the status bar along the bottom of the window: `message` (and
/// whether it is an error) on the left, `info` (cursor position and the
/// document's details, from [`View::status_info`]) on the right. A long
/// message is cut short rather than drawn over the details.
pub fn draw_status(
    gpu: &mut Gpu,
    ui: &Ui,
    layout: &Layout,
    message: Option<(&str, bool)>,
    info: &str,
) {
    let lh = layout.metrics.line_height;
    let cell = layout.metrics.cell_width;
    gpu.begin_layer(None);
    gpu.push_rect(
        [0.0, layout.bottom],
        [layout.width, layout.height - layout.bottom],
        ui.bar,
    );
    let y = (layout.bottom + (layout.height - layout.bottom - lh) / 2.0).round();
    let info_w = f(info.chars().count()) * cell;
    let info_x = (layout.width - layout.margin - info_w).round();
    gpu.push_text_line([info_x.max(layout.margin), y], info, ui.gutter);
    if let Some((text, is_error)) = message {
        // Columns left for the message, with two between it and the info.
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "a small non-negative column count"
        )]
        let room = ((info_x - layout.margin) / cell - 2.0).max(0.0) as usize;
        let end = text.char_indices().nth(room).map_or(text.len(), |(i, _)| i);
        let color = if is_error { ui.error } else { ui.text };
        gpu.push_text_line([layout.margin, y], &text[..end], color);
    }
}

/// The selected columns of the first line of a one-line field.
fn field_selection(doc: &Document, text: &str) -> Option<(usize, usize)> {
    let sel = doc.selection().range();
    if sel.is_empty() || sel.start > text.len() {
        return None;
    }
    let a = column::col_at(text, sel.start, 4);
    let b = column::col_at(text, sel.end.min(text.len()), 4);
    Some((a, b))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout(len_lines: usize) -> Layout {
        let metrics = Metrics {
            size_px: 14.0,
            line_height: 20.0,
            baseline: 15.0,
            cell_width: 10.0,
        };
        Layout::new(metrics, 800, 208, 1.0, len_lines)
    }

    /// Narrow: text from x 48 to 168 minus the margin, so 11 columns.
    fn narrow(len_lines: usize) -> Layout {
        let metrics = Metrics {
            size_px: 14.0,
            line_height: 20.0,
            baseline: 15.0,
            cell_width: 10.0,
        };
        Layout::new(metrics, 168, 208, 1.0, len_lines)
    }

    fn doc(text: &str) -> Document {
        Document::from_text(text)
    }

    #[test]
    fn long_lines_highlight_only_their_visible_bytes() {
        use nimbus_core::RopeBuffer;
        let mut cache = WrapCache::default();
        let l = layout(3);
        let mut out = Vec::new();
        // 40 KB: under the long-line limit, so wrapped and counted exactly.
        let long = "ab".repeat(20_000);
        let buf = RopeBuffer::from_text(&format!("short\n{long}\nend\n"));
        let start = buf.line_to_byte(1);
        let at = RowPos { start, row: 0 };
        // No wrap, scrolled 1000 columns right, 50 columns wide.
        let mut w = wrapper(&mut cache, false, &l, 4);
        visible_ranges(&buf, &mut w, at, 1, (1000, 50), &mut out);
        assert_eq!(out, vec![start + 1000..start + 1051]);
        // Wrapped: the rows on screen, from the top row on.
        let mut w = wrapper(&mut cache, true, &l, 4);
        let at = RowPos { start, row: 5 };
        out.clear();
        visible_ranges(&buf, &mut w, at, 3, (0, 50), &mut out);
        let cols = l.wrap_cols;
        assert_eq!(out.len(), 1, "adjacent rows merge");
        assert_eq!(out[0].start, start + 5 * cols);
        assert!(out[0].end <= start + 8 * cols + 1, "{out:?}");

        // 200 KB followed by more lines: the long line's window, and the
        // next lines separately, never the bytes between.
        let long = "ab".repeat(100_000);
        let buf = RopeBuffer::from_text(&format!("short\n{long}\nend\n"));
        let at = RowPos { start, row: 0 };
        for wrap in [false, true] {
            let mut w = wrapper(&mut cache, wrap, &l, 4);
            out.clear();
            visible_ranges(&buf, &mut w, at, 3, (100_000, 50), &mut out);
            let total: usize = out.iter().map(std::ops::Range::len).sum();
            assert!(total <= 4 * (long_line::BLOCK + 50) + 10, "{out:?}");
            assert!(out[0].start <= start + 100_000 && start + 100_050 <= out[0].end);
        }
    }

    #[test]
    fn visible_span_covers_only_what_is_on_screen() {
        let line = "abcdefghij";
        assert_eq!(visible_span(line, 0, 0, 3, 4), (0, 4, 0));
        assert_eq!(visible_span(line, 0, 5, 2, 4), (5, 8, 5));
        assert_eq!(
            visible_span(line, 0, 50, 2, 4),
            (10, 10, 10),
            "scrolled past the end"
        );
        assert_eq!(
            visible_span("日本語", 0, 1, 1, 4),
            (0, 6, 0),
            "straddling wide char"
        );
        assert_eq!(
            visible_span("ab\tcd", 0, 3, 1, 4),
            (2, 4, 2),
            "straddling tab"
        );
        // A wrapped row starting at column 6 of its line.
        assert_eq!(visible_span("world", 6, 6, 2, 4), (0, 3, 6));
    }

    #[test]
    fn layout_measures() {
        assert_eq!(layout(9).gutter_cols, 2);
        assert_eq!(layout(1234).gutter_cols, 4);
        assert_eq!(digits(0), 1);
        assert_eq!(digits(100), 3);
        assert_eq!(layout(10).full_lines(), 10);
        assert_eq!(narrow(4).wrap_cols, 11);
    }

    /// The top row as (line, byte within the line).
    fn top(v: &View, d: &Document) -> (usize, usize) {
        let line = d.buffer.byte_to_line(v.scroll_top);
        (line, v.scroll_top - d.buffer.line_to_byte(line))
    }

    #[test]
    fn the_status_bar_shows_position_selection_and_file_details() {
        let mut d = doc("fn main() {\n\tlet é = 1;\n}\n");
        d.line_ending = LineEnding::CrLf;
        let mut v = View::default();
        assert_eq!(v.status_info(&d, 4), "Ln 1, Col 1   CRLF   UTF-8");
        // Line 2, after the tab (4 columns) and "let ": column 9.
        d.click(12 + 1 + 4, false);
        assert_eq!(v.status_info(&d, 4), "Ln 2, Col 9   CRLF   UTF-8");
        // Selecting "é = 1" counts characters, not bytes.
        d.select(17..23);
        assert_eq!(
            v.status_info(&d, 4),
            "5 selected   Ln 2, Col 14   CRLF   UTF-8"
        );
        d.selections =
            nimbus_core::Selections::from_vec(vec![Selection::cursor(0), Selection::cursor(12)], 0);
        assert_eq!(
            v.status_info(&d, 4),
            "2 cursors   Ln 1, Col 1   CRLF   UTF-8"
        );
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(3 * 1024 * 1024 + 400 * 1024), "3.4 MB");
    }

    #[test]
    fn scrolling_clamps_at_both_ends() {
        let d = doc(&"x\n".repeat(99));
        let l = layout(100);
        let mut v = View::default();
        v.scroll_by(-50.0, &d, &l, 4);
        assert_eq!((top(&v, &d).0, v.scroll_px), (0, 0.0));
        v.scroll_by(45.0, &d, &l, 4);
        assert_eq!(top(&v, &d).0, 2);
        assert!((v.scroll_px - 5.0).abs() < 1e-3);
        v.scroll_by(-25.0, &d, &l, 4);
        assert_eq!((top(&v, &d).0, v.scroll_px), (1, 0.0));
        v.scroll_by(1e6, &d, &l, 4);
        assert_eq!((top(&v, &d).0, v.scroll_px), (99, 0.0));
    }

    #[test]
    fn the_end_and_any_fraction_are_reachable_before_lines_are_counted() {
        use nimbus_core::line_index::{LineIndex, Scanner, Spacing};
        use nimbus_core::mapped::Limits;
        use nimbus_core::{Buffer as _, MappedBuffer, Source, VecSource};
        let text = "0123456789\n".repeat(10_000);
        let source: std::sync::Arc<dyn Source> =
            std::sync::Arc::new(VecSource::from(text.as_str()));
        // Only the first 100 bytes' lines are counted.
        let mut scanner = Scanner::new(Spacing::new(2, 5));
        scanner.feed(&text.as_bytes()[..100]);
        let mut index = LineIndex::new();
        index.merge(scanner.take_chunk()).unwrap();
        let buffer = MappedBuffer::with_index(source, index, Limits::default()).unwrap();
        let mut d = Document::default();
        d.buffer = buffer.into();
        assert!(!d.buffer.is_fully_indexed());
        let l = layout(10);
        let mut v = View::default();

        // Primary+End: the cursor goes to the end and the view follows,
        // without waiting for line numbers.
        d.click(text.len(), false);
        v.ensure_cursor_visible(&d, &l, 4);
        let shown = l.full_lines() * 11;
        assert!(v.scroll_top + shown >= text.len(), "{}", v.scroll_top);
        assert_eq!(d.buffer.line_number(v.scroll_top), None, "shown as ~");

        // Halfway: the top row is the line holding the middle byte.
        v.scroll_to_fraction(0.5, &d, &l, 4);
        assert_eq!(v.scroll_top, 55_000);
        v.scroll_to_fraction(2.0, &d, &l, 4);
        assert_eq!(v.scroll_top, text.len(), "clamped to the last line");
        v.scroll_to_fraction(0.0, &d, &l, 4);
        assert_eq!(v.scroll_top, 0);
    }

    #[test]
    fn the_scrollbar_thumb_follows_the_scroll_and_drags_it() {
        // 100 lines of 10 bytes; pretend lines 0..10 are on screen.
        let d = doc(&"123456789\n".repeat(100));
        let l = layout(10);
        let mut v = View {
            shown: 100,
            ..View::default()
        };
        let bar = v.scrollbar_geometry(1000, &l).unwrap();
        assert!((bar.thumb_top - l.top).abs() < 1e-3, "at the top");
        // A tenth of the text shown: a tenth of the track, at least.
        let h = l.bottom - l.top;
        assert!(bar.thumb_h >= (h / 10.0).min(30.0) - 1e-3);
        v.scrollbar = Some(bar);
        assert_eq!(
            v.scrollbar_hit(l.width - 2.0, l.top + 1.0),
            Some(ScrollbarHit::Thumb(1.0))
        );
        assert_eq!(
            v.scrollbar_hit(l.width - 2.0, l.bottom - 1.0),
            Some(ScrollbarHit::Track)
        );
        assert_eq!(v.scrollbar_hit(l.width / 2.0, l.top + 1.0), None);

        // Dragging the thumb to the bottom shows the end; halfway, the middle.
        v.drag_scrollbar(l.bottom, &d, &l, 4);
        assert_eq!(v.scroll_top, 900);
        v.drag_scrollbar(l.top + (h - bar.thumb_h) / 2.0, &d, &l, 4);
        assert_eq!(v.scroll_top, 450);

        // Everything on screen: no scrollbar.
        let v = View {
            shown: 1000,
            ..View::default()
        };
        assert!(v.scrollbar_geometry(1000, &l).is_none());
    }

    #[test]
    fn scrolling_walks_wrapped_rows() {
        // Line 0 wraps at 11 columns into rows starting at bytes 0, 9, 18.
        let d = doc("aaaaaaaa bbbbbbbb cccccccc\nx\ny\nz");
        let l = narrow(4);
        let mut v = View::default();
        v.set_wrap(true);
        let at = |v: &View| {
            let (line, byte) = top(v, &d);
            (line, byte, v.scroll_px.round())
        };
        v.scroll_by(25.0, &d, &l, 4);
        assert_eq!(at(&v), (0, 9, 5.0));
        v.scroll_by(40.0, &d, &l, 4);
        assert_eq!(at(&v), (1, 0, 5.0));
        v.scroll_by(-30.0, &d, &l, 4);
        assert_eq!(at(&v), (0, 9, 15.0));
        v.scroll_by(1e6, &d, &l, 4);
        assert_eq!(at(&v), (3, 0, 0.0));
        v.scroll_by(-1e6, &d, &l, 4);
        assert_eq!(at(&v), (0, 0, 0.0));
    }

    #[test]
    fn toggling_wrap_keeps_the_top_line() {
        // The top is mid-line (row 1 of a wrapped line); unwrapped, the row
        // containing it is the whole line, so the line stays at the top.
        let d = doc("aaaaaaaa bbbbbbbb cccccccc\nx");
        let l = narrow(4);
        let mut v = View {
            scroll_top: 9,
            scroll_x: 30.0,
            wrap: true,
            ..View::default()
        };
        v.set_wrap(false);
        assert_eq!(v.scroll_x, 0.0);
        let scroll_top = v.scroll_top;
        let mut w = v.wrap_view(&l, 4);
        assert_eq!(
            anchor(scroll_top, &d.buffer, &mut w),
            RowPos { start: 0, row: 0 }
        );
    }

    #[test]
    fn cursor_below_the_view_scrolls_it_into_view() {
        let text = (0..50).fold(String::new(), |mut s, i| {
            let _ = writeln!(s, "{i}");
            s
        });
        let mut d = doc(&text);
        let l = layout(51);
        let mut v = View::default();
        d.click(d.buffer.line_to_byte(30), false);
        v.ensure_cursor_visible(&d, &l, 4);
        assert_eq!(top(&v, &d).0, 21, "line 30 is the last full line");
        d.click(d.buffer.line_to_byte(5), false);
        v.ensure_cursor_visible(&d, &l, 4);
        assert_eq!(top(&v, &d).0, 5);
    }

    #[test]
    fn keep_visible_counts_wrapped_rows() {
        // Each line wraps into rows at bytes 0 and 9.
        let mut d = doc(&"aaaaaaaa bbbbbbbb\n".repeat(12));
        let l = narrow(13);
        let mut v = View::default();
        v.set_wrap(true);
        d.click(6 * 18 + 9, false); // line 6, row 1
        v.ensure_cursor_visible(&d, &l, 4);
        assert_eq!(top(&v, &d), (2, 0));
        d.click(18 + 9, false); // line 1, row 1
        v.ensure_cursor_visible(&d, &l, 4);
        assert_eq!(top(&v, &d), (1, 9));
    }

    #[test]
    fn clicks_map_to_lines_and_columns() {
        let d = doc("abc\n日x\nlast");
        let l = layout(3);
        let mut v = View::default();
        let left = l.text_left;
        assert_eq!(
            v.hit(left + 16.0, 8.0 + 5.0, &d, &l, 4),
            2,
            "right half of b"
        );
        assert_eq!(
            v.hit(left + 12.0, 8.0 + 25.0, &d, &l, 4),
            7,
            "right half of 日"
        );
        assert_eq!(
            v.hit(left + 500.0, 8.0 + 5.0, &d, &l, 4),
            3,
            "past the end of the line"
        );
        assert_eq!(v.hit(left, 2000.0, &d, &l, 4), 9, "below the last line");
        assert_eq!(v.hit(0.0, 8.0 + 5.0, &d, &l, 4), 0, "in the gutter");
    }

    #[test]
    fn clicks_on_wrapped_rows() {
        let d = doc("aaaaaaaa bbbbbbbb cccccccc\nx\ny\nz");
        let l = narrow(4);
        let mut v = View::default();
        v.set_wrap(true);
        let left = l.text_left;
        assert_eq!(
            v.hit(left + 22.0, 8.0 + 25.0, &d, &l, 4),
            11,
            "row 1, column 2"
        );
        assert_eq!(
            v.hit(left + 500.0, 8.0 + 5.0, &d, &l, 4),
            8,
            "past row 0: before its hanging space"
        );
        assert_eq!(
            v.hit(left + 500.0, 8.0 + 45.0, &d, &l, 4),
            26,
            "past the last row: line end"
        );
    }

    #[test]
    fn row_geometry_ownership() {
        let row = RowGeom {
            seg: "hello ",
            seg_start: 10,
            start_col: 0,
            last_row: false,
            has_break: true,
            top: 0.0,
            left: 0.0,
        };
        assert!(row.owns(10) && row.owns(15));
        assert!(!row.owns(16), "a row's end belongs to the next row");
        let last = RowGeom {
            last_row: true,
            ..row
        };
        assert!(last.owns(16));
    }

    #[test]
    fn chrome_reserves_rows_top_and_bottom() {
        let l = layout(10).with_chrome(true, 2, true);
        assert!(l.tabs_h > 0.0);
        assert!(l.top > l.tabs_h + 2.0 * 20.0);
        assert!((l.bottom - (208.0 - 20.0)).abs() < 1e-3);
        assert_eq!(find_bar_row(&l, l.tabs_h + 9.0, 2), Some(0));
        assert_eq!(find_bar_row(&l, l.tabs_h + 40.0, 2), Some(1));
        assert_eq!(find_bar_row(&l, 1.0, 2), None, "in the tab bar");
    }

    #[test]
    fn tabs_scroll_to_keep_the_active_one_visible() {
        let l = layout(10).with_chrome(true, 0, false);
        let tabs: Vec<TabLabel> = (0..20)
            .map(|i| TabLabel {
                name: format!("file{i}.rs"),
                dirty: false,
            })
            .collect();
        let ext = tab_extents(&l, &tabs, 19);
        assert!(ext[19].1 <= l.width - l.margin + 1e-3);
        assert_eq!(
            tab_at(&l, &tabs, 0, ext[0].0 + 1.0, 5.0),
            None,
            "scrolled only when active is far"
        );
        let ext0 = tab_extents(&l, &tabs, 0);
        assert_eq!(tab_at(&l, &tabs, 0, ext0[1].0 + 1.0, 5.0), Some(1));
        assert_eq!(tab_at(&l, &tabs, 0, ext0[1].0 + 1.0, l.tabs_h + 1.0), None);
    }

    #[test]
    fn field_selection_columns() {
        let mut d = doc("abc");
        d.select(1..3);
        assert_eq!(field_selection(&d, "abc"), Some((1, 3)));
        d.select(2..2);
        assert_eq!(field_selection(&d, "abc"), None);
    }
}
