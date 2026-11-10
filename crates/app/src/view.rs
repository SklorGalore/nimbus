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
use std::time::{Duration, Instant};

use nimbus_core::wrap::{self, RowPos, RowStart, WrapCache, WrapView};
use nimbus_core::{Folds, LineEnding, Selection, Selections, Text, column, long_line};
use nimbus_render::{ColorRun, Gpu, Metrics, Style, UiFont};
use nimbus_syntax::highlight::{Highlighter, Span};

use unicode_segmentation::UnicodeSegmentation;

use crate::config::Whitespace;
use crate::document::{Carets, Document, Solo, Tail};
use crate::find::{Field, FindBar};
use crate::minimap::{self, Minimap};
use crate::sticky::Sticky;
use crate::theme::{Color, Theme, Ui};

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
    let width = ((layout.text_right() - layout.text_left) / cell)
        .ceil()
        .max(0.0) as usize;
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
    /// The theme, with its capture tables resolved (`Theme::resolve_tables`),
    /// for the look of each syntax span. `None` when the document has no
    /// highlighting, so no spans are computed.
    pub looks: Option<&'a Theme>,
    /// Where spaces and tabs show as `·` and `→` (M16).
    pub whitespace: Whitespace,
    /// Indent guides, with the columns per indentation level; `None`
    /// draws none.
    pub guides: Option<usize>,
    /// Pin the scopes around the top line above the text (M20).
    pub sticky: bool,
}

/// How long the scrollbar stays after the last scroll before it hides
/// (M16). One timer, like the cursor blink: an idle editor schedules
/// nothing once it has fired.
pub const SCROLLBAR_FADE: Duration = Duration::from_secs(1);

/// Leading whitespace is measured over at most this many bytes of a line:
/// 80 levels of 16 columns, past which guides are not drawn anyway. A
/// 100 MB line costs the same as a short one (M7's long-line rule).
const INDENT_SCAN: usize = 1280;

/// Guides stop at this many levels: deeper indentation is generated code,
/// where a fence of hairlines helps nobody.
const MAX_GUIDES: usize = 80;

/// A blank line takes its guides from the lines around it; it looks this
/// far for a line with text, in each direction.
const BLANK_LOOKAROUND: usize = 32;

/// The active guide's block is looked for this many lines either side of
/// the cursor.
const ACTIVE_SCAN: usize = 2000;

/// Columns of leading whitespace on the line at `span`; `None` when the
/// line is blank (nothing but spaces and tabs).
fn leading_cols(t: &dyn Text, span: nimbus_core::LineSpan, tab: usize) -> Option<usize> {
    let end = span.end.min(span.start + INDENT_SCAN);
    let mut col = 0;
    for pos in span.start..end {
        match t.byte_at(pos) {
            Some(b' ') => col += 1,
            Some(b'\t') => col += tab - col % tab,
            Some(_) => return Some(col),
            None => break,
        }
    }
    // Only whitespace in the scanned part: blank if that was the whole line.
    (end < span.end).then_some(col)
}

/// The indentation guides go by for the line at `span`: its own, or for a
/// blank line the smaller of the nearest lines with text above and below,
/// so a blank line inside a block keeps the block's guides.
fn guide_cols(t: &dyn Text, span: nimbus_core::LineSpan, tab: usize) -> usize {
    if let Some(c) = leading_cols(t, span, tab) {
        return c;
    }
    let mut below = None;
    let mut next = span.next;
    for _ in 0..BLANK_LOOKAROUND {
        let Some(at) = next else { break };
        let s = t.line_span(at);
        if let Some(c) = leading_cols(t, s, tab) {
            below = Some(c);
            break;
        }
        next = s.next;
    }
    let mut above = None;
    let mut start = span.start;
    for _ in 0..BLANK_LOOKAROUND {
        let Some(prev) = start.checked_sub(1) else {
            break;
        };
        let s = t.line_span(prev);
        if let Some(c) = leading_cols(t, s, tab) {
            above = Some(c);
            break;
        }
        start = s.start;
    }
    above.unwrap_or(0).min(below.unwrap_or(0))
}

/// The guide drawn brighter: the innermost level enclosing the cursor's
/// line, as `(level, bytes of the lines it spans)`. `None` at the top
/// level.
fn active_guide(
    t: &dyn Text,
    cursor: usize,
    tab: usize,
    width: usize,
) -> Option<(usize, Range<usize>)> {
    let span = t.line_span(cursor.min(t.len_bytes()));
    let level = guide_cols(t, span, tab) / width.max(1);
    if level == 0 || level > MAX_GUIDES {
        return None;
    }
    let inside = |s: nimbus_core::LineSpan| guide_cols(t, s, tab) >= level * width;
    let mut first = span;
    for _ in 0..ACTIVE_SCAN {
        let Some(prev) = first.start.checked_sub(1) else {
            break;
        };
        let s = t.line_span(prev);
        if !inside(s) {
            break;
        }
        first = s;
    }
    let mut last = span;
    for _ in 0..ACTIVE_SCAN {
        let Some(at) = last.next else { break };
        let s = t.line_span(at);
        if !inside(s) {
            break;
        }
        last = s;
    }
    Some((level - 1, first.start..last.end))
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
    /// Width of the minimap at the column's right edge (M20), 0 when
    /// none. The text stops at [`Layout::text_right`].
    pub minimap: f32,
    /// How interface text (tabs, status bar, pickers, sidebar) is
    /// measured: the `ui_font` when one is loaded, else the grid (M20).
    pub ui: UiFont,
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
            minimap: 0.0,
            ui: UiFont::grid(metrics),
        }
    }

    /// Measure interface text with `ui` (from [`Gpu::ui_font`]).
    #[must_use]
    pub fn with_ui(mut self, ui: UiFont) -> Self {
        self.ui = ui;
        self
    }

    /// Right edge of the text: the column's edge, or the minimap's left
    /// edge when one is shown.
    #[must_use]
    pub fn text_right(&self) -> f32 {
        self.width - self.minimap
    }

    /// Move the editor column right of a sidebar `left` pixels wide.
    #[must_use]
    pub fn with_sidebar(mut self, left: f32) -> Self {
        let left = left.clamp(0.0, (self.width - 10.0 * self.metrics.cell_width).max(0.0));
        self.text_left += left - self.left;
        self.left = left;
        self.fit_wrap();
        self
    }

    /// Reserve `width` pixels at the right of the column for the minimap
    /// (M20), at most half the text area; the text wraps short of it.
    #[must_use]
    pub fn with_minimap(mut self, width: f32) -> Self {
        let room = ((self.width - self.text_left) / 2.0).max(0.0);
        self.minimap = width.round().clamp(0.0, room);
        self.fit_wrap();
        self
    }

    /// Work out the soft-wrap width from the text area's edges.
    fn fit_wrap(&mut self) {
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "a small non-negative column count"
        )]
        let wrap_cols = ((self.text_right() - self.text_left - self.margin)
            / self.metrics.cell_width)
            .floor()
            .max(1.0) as usize;
        self.wrap_cols = wrap_cols;
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
    pub(crate) fn find_top(&self) -> f32 {
        self.tabs_h
    }

    /// Height of the find bar area, if any rows are reserved for it.
    pub(crate) fn bar_height(&self) -> f32 {
        (self.top - self.margin - self.tabs_h).max(0.0)
    }

    /// This layout narrowed to one column of the editor area, from `left`
    /// to `right` in physical pixels, keeping the gutter width and the
    /// vertical geometry. The compare view (M9) draws each side with one
    /// of these; everything that treats `width` as the right edge then
    /// stops at the column's edge.
    #[must_use]
    pub fn column(&self, left: f32, right: f32) -> Self {
        let mut l = *self;
        let gutter = self.text_left - self.left;
        l.left = left;
        l.width = right;
        l.text_left = left + gutter;
        l.fit_wrap();
        l
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

/// Start a frame: everything queued before is dropped. Called once per
/// frame; each group's view, or each column of a compare view, then
/// begins its own clipped layer with [`begin_column`].
pub fn begin_frame(gpu: &mut Gpu, ui: &Ui) {
    gpu.begin_frame();
    gpu.set_background(ui.background);
}

/// A layer clipped to `layout`'s text area (its column, between the bars).
fn begin_column(gpu: &mut Gpu, layout: &Layout) {
    gpu.begin_layer(Some([
        layout.left,
        layout.top,
        layout.text_right() - layout.left,
        layout.bottom - layout.top,
    ]));
}

/// One screen row of a compare view's column, for [`View::draw_rows`].
#[derive(Debug, Clone, PartialEq)]
pub struct RowPlan {
    /// The line shown, or `None` for a filler or fold row.
    pub line: Option<usize>,
    /// A fold row instead of a line: how many lines it hides.
    pub fold: Option<usize>,
    /// A background over the row's text area (a change's colour).
    pub background: Option<Color>,
    /// Bytes of the line drawn over a stronger colour: the changed words.
    pub mark: Option<Range<usize>>,
    /// Only this piece of the line is drawn (M18: a compare cut a long
    /// line into pieces, one row each), and whether it continues the
    /// piece before it, which draws `⤶` in the gutter instead of a number.
    pub piece: Option<(Range<usize>, bool)>,
}

/// What a remembered bracket pair was worked out for: the text's version,
/// the cursor, and the syntax tree's revision.
type BracketKey = (u64, usize, u64);

/// How far each way bracket matching counts when there is no syntax tree.
const BRACKET_LIMIT: usize = 64 * 1024;

/// The span to draw for a row showing `piece` of the line at `span` (M18:
/// a compare cut the line): a line of its own that ends where the piece
/// does, and whether it continues the piece before. The whole line when
/// there is no piece.
fn piece_span(
    mut span: nimbus_core::LineSpan,
    piece: Option<&(Range<usize>, bool)>,
) -> (nimbus_core::LineSpan, bool) {
    let Some((piece, continued)) = piece else {
        return (span, false);
    };
    let end = piece.end.min(span.end);
    if end < span.end {
        span.next = Some(end);
    }
    span.start = piece.start.max(span.start).min(end);
    span.end = end;
    (span, *continued)
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

/// Write `n` bytes as a short human-readable size: `512 B`, `12.0 KB`,
/// `3.4 MB`. Into a string the caller reuses, so it allocates nothing.
pub fn write_size(out: &mut String, n: usize) {
    #[expect(clippy::cast_precision_loss, reason = "a size for display")]
    let x = n as f64;
    let _ = if n < 1024 {
        write!(out, "{n} B")
    } else if n < 1024 * 1024 {
        write!(out, "{:.1} KB", x / 1024.0)
    } else if n < 1024 * 1024 * 1024 {
        write!(out, "{:.1} MB", x / (1024.0 * 1024.0))
    } else {
        write!(out, "{:.1} GB", x / (1024.0 * 1024.0 * 1024.0))
    };
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
    folds: &'a Folds,
) -> WrapView<'a> {
    let w = WrapView::new(cache, wrap.then_some(layout.wrap_cols), tab_width);
    // Rows skip folded lines (M13); with no folds there is nothing to ask.
    if folds.is_empty() {
        w
    } else {
        w.with_folds(folds)
    }
}

/// Where a fold marker goes in the gutter: right of the line numbers.
fn fold_marker_x(layout: &Layout) -> f32 {
    let cell = layout.metrics.cell_width;
    (layout.left + layout.margin + f(layout.gutter_cols) * cell + 0.4 * cell).round()
}

/// The top visible row: the row containing `scroll_top`. Snapping to the
/// containing row keeps this right after edits above the top, and after
/// wrapping changes.
fn anchor(scroll_top: usize, t: &dyn Text, w: &mut WrapView<'_>) -> RowPos {
    wrap::row_pos_of(t, w, scroll_top.min(t.len_bytes()))
}

/// A status bar item that opens choices when clicked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StatusItem {
    /// Indentation: "Spaces: 4" or "Tabs".
    #[default]
    Indent,
    /// The line ending: "LF" or "CRLF".
    LineEnding,
    /// The encoding: "UTF-8", "Windows-1252", …
    Encoding,
    /// "Follow Tail" or "Following" (M12), once the file has grown.
    Follow,
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
    /// The character columns of the items that act when clicked, within
    /// `status`: indentation ("Spaces: 4"), line ending ("CRLF"), encoding
    /// ("UTF-8") and Follow Tail (M12). Empty ranges are items not shown.
    status_items: [(StatusItem, Range<usize>); 4],
    /// How many bytes of text the last frame showed, for the scrollbar's
    /// thumb. Kept as a count rather than an end, so it stays right while
    /// a drag moves the scroll before the next frame.
    shown: usize,
    /// Where the scrollbar was drawn in the last frame, for clicks.
    scrollbar: Option<Scrollbar>,
    /// The pointer is over the scrollbar: it is drawn wider, and does not
    /// fade.
    pub scrollbar_hover: bool,
    /// The scroll position the scrollbar last saw, and when it changed:
    /// the bar shows for [`SCROLLBAR_FADE`] after that (M16).
    scroll_seen: Option<((usize, f32), Instant)>,
    /// The bracket pair at the cursor, as last worked out: for the text
    /// version, cursor and syntax revision it was worked out for. Each
    /// frame asks again only when one of those changed.
    brackets: Option<(BracketKey, Option<(usize, usize)>)>,
    /// Reused for reading around the cursor when counting brackets.
    bracket_window: Vec<u8>,
    /// Reused for the cursor and selection edges on a row, where a
    /// ligature must be drawn split.
    splits: Vec<usize>,
    /// Characters in the selections, as last counted, with the version and
    /// selections they were counted for.
    selected_chars: Option<(u64, Selections, usize)>,
    /// The folded regions in this view (M13). Each pane folds on its own;
    /// folds follow every edit to the document through its journal.
    pub folds: Folds,
    /// The document's epoch and journal end the folds last followed.
    folds_at: (u64, u64),
    /// Where each fold chip was drawn last frame, `[x0, x1, top]`, and
    /// the fold it opens, for clicks.
    chips: Vec<Chip>,
    /// The line under the pointer, by its start, when it can be folded:
    /// its gutter shows the fold marker.
    pub fold_hover: Option<usize>,
    /// The lines pinned above the text (M20's sticky scroll).
    sticky: Sticky,
    /// The minimap's blocks and where it was drawn (M20).
    minimap: Minimap,
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
    /// Bring the folds up to date with the edits made to `doc` since they
    /// last looked, from this view or another (M13). An edit that touches
    /// a fold opens it; one before it moves it. A text swapped wholesale
    /// (a reload) drops them all.
    pub fn follow(&mut self, doc: &Document) {
        let end = doc.journal().end();
        if self.folds_at.0 != doc.epoch {
            self.folds.clear();
        } else if self.folds_at.1 != end && !self.folds.is_empty() {
            match doc.journal().since(self.folds_at.1) {
                Some(shifts) => {
                    for s in shifts {
                        self.folds.apply_change(s.start, s.removed, s.inserted);
                    }
                }
                None => self.folds.clear(),
            }
        }
        self.folds_at = (doc.epoch, end);
    }

    /// The fold whose chip is at window position `(x, y)`, by its start.
    #[must_use]
    pub fn chip_at(&self, x: f32, y: f32, line_height: f32) -> Option<usize> {
        self.chips
            .iter()
            .find(|([x0, x1, top], _)| x >= *x0 && x < *x1 && y >= *top && y < top + line_height)
            .map(|&(_, start)| start)
    }

    /// Whether `x` is on the gutter's fold-marker column, right of the
    /// line numbers.
    #[must_use]
    pub fn on_fold_marker(layout: &Layout, x: f32) -> bool {
        let cell = layout.metrics.cell_width;
        x >= fold_marker_x(layout) - 0.4 * cell && x < layout.text_left
    }

    /// Pixel offset of the top row, rounded so text stays on whole pixels.
    fn px(&self) -> f32 {
        self.scroll_px.round()
    }

    fn set_anchor(&mut self, t: &dyn Text, layout: &Layout, tab_width: usize, at: RowPos) {
        let mut w = wrapper(&mut self.cache, self.wrap, layout, tab_width, &self.folds);
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
        let mut w = wrapper(&mut self.cache, self.wrap, layout, tab_width, &self.folds);
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
        let mut w = wrapper(&mut self.cache, self.wrap, layout, tab_width, &self.folds);
        let at = anchor(byte, t, &mut w);
        self.scroll_px = 0.0;
        self.set_anchor(t, layout, tab_width, at);
    }

    /// The bracket next to the primary cursor and its partner: from the
    /// syntax tree when it is up to date, otherwise by counting within
    /// [`BRACKET_LIMIT`] bytes. Only for a bare cursor. Remembered until
    /// the text, the cursor or the tree changes, so most frames only
    /// compare three numbers.
    fn bracket_pair(&mut self, doc: &Document, carets: &Carets) -> Option<(usize, usize)> {
        let sel = carets.selection(&doc.buffer);
        if !sel.is_empty() {
            return None;
        }
        let key = (doc.version, sel.head, doc.syntax.rev());
        if let Some((k, pair)) = self.brackets
            && k == key
        {
            return pair;
        }
        let t = &doc.buffer;
        let pair = doc.syntax.matching_bracket(t, sel.head).unwrap_or_else(|| {
            nimbus_core::brackets::matching_with(
                t,
                sel.head,
                BRACKET_LIMIT,
                &mut self.bracket_window,
            )
        });
        self.brackets = Some((key, pair));
        pair
    }

    /// How many characters the selections hold, counted again only when
    /// the text or the selections change.
    fn selected_chars(&mut self, doc: &Document, carets: &Carets) -> usize {
        if let Some((v, sels, n)) = &self.selected_chars
            && *v == doc.version
            && *sels == carets.selections
        {
            return *n;
        }
        let t = &doc.buffer;
        let n = carets
            .selections
            .iter()
            .map(|s| t.slice(s.range()).chars().count())
            .sum();
        self.selected_chars = Some((doc.version, carets.selections.clone(), n));
        n
    }

    /// The right side of the status bar for `doc`: the cursor's line and
    /// column (`~` for a line not counted yet in a huge file), how much is
    /// selected, the line ending, the encoding, the language and, while a
    /// huge file's lines are being counted, how far that has got. Built in
    /// a reused string, so drawing it each frame allocates nothing.
    pub fn status_info(&mut self, doc: &Document, carets: &Carets, tab_width: usize) -> &str {
        // Counting characters reads the selections; past a size where that
        // costs, bytes say as much.
        let selected = carets.selected_len();
        let chars = (selected > 0 && selected <= STATUS_COUNT_LIMIT)
            .then(|| self.selected_chars(doc, carets));
        let out = &mut self.status;
        out.clear();
        let t = &doc.buffer;
        let sel = carets.selection(t);
        let count = carets.selections.count();
        if count > 1 {
            let _ = write!(out, "{count} cursors   ");
        }
        if selected > 0 {
            if let Some(n) = chars {
                let _ = write!(out, "{n} selected   ");
            } else {
                write_size(out, selected);
                out.push_str(" selected   ");
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
        let mut items = [
            (StatusItem::Indent, 0..0),
            (StatusItem::LineEnding, 0..0),
            (StatusItem::Encoding, 0..0),
            (StatusItem::Follow, 0..0),
        ];
        let from = out.chars().count();
        out.push_str(&doc.indent.label());
        items[0].1 = from..out.chars().count();
        out.push_str("   ");
        let from = out.chars().count();
        out.push_str(match doc.line_ending {
            LineEnding::Lf => "LF",
            LineEnding::CrLf => "CRLF",
        });
        items[1].1 = from..out.chars().count();
        if let Some(encoding) = doc.encoding_label() {
            out.push_str("   ");
            let from = out.chars().count();
            out.push_str(encoding);
            items[2].1 = from..out.chars().count();
        }
        if doc.read_only.is_some() {
            out.push_str("   Read-only");
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
        // Offered once the file has grown on disk; a click toggles it.
        let follow = match doc.tail {
            Tail::Fixed => None,
            Tail::Grew => Some("Follow Tail"),
            Tail::Following => Some("Following"),
        };
        if let Some(label) = follow {
            out.push_str("   ");
            let from = out.chars().count();
            out.push_str(label);
            items[3].1 = from..out.chars().count();
        }
        self.status_items = items;
        &self.status
    }

    /// The status bar item under `x`, as last built by
    /// [`View::status_info`], which [`draw_status`] right-aligns.
    #[must_use]
    pub fn status_item_at(&self, layout: &Layout, x: f32) -> Option<StatusItem> {
        let ui = &layout.ui;
        let info_w = ui.width(&self.status);
        let info_x = (layout.width - layout.margin - info_w)
            .round()
            .max(layout.margin);
        // Where the character `c` of the status text is drawn.
        let x_of = |c: usize| {
            let byte = self
                .status
                .char_indices()
                .nth(c)
                .map_or(self.status.len(), |(i, _)| i);
            info_x + ui.width(&self.status[..byte])
        };
        self.status_items
            .iter()
            .find(|(_, r)| !r.is_empty() && x >= x_of(r.start) && x < x_of(r.end))
            .map(|(item, _)| *item)
    }

    /// Scroll horizontally by `dx` pixels. Does nothing while wrapping.
    pub fn scroll_x_by(&mut self, dx: f32) {
        self.scroll_x = (self.scroll_x + f64::from(dx)).max(0.0);
    }

    /// Scroll as little as possible to bring the cursor into view.
    pub fn ensure_cursor_visible(
        &mut self,
        doc: &Document,
        carets: &Carets,
        layout: &Layout,
        tab_width: usize,
    ) {
        let t = &doc.buffer;
        let cursor = carets.cursor(t);
        let full = layout.full_lines();
        let mut w = wrapper(&mut self.cache, self.wrap, layout, tab_width, &self.folds);
        let cursor_row = wrap::row_pos_of(t, &mut w, cursor);
        let top_row = anchor(self.scroll_top, t, &mut w);
        // Rows pinned by sticky scroll cover the top of the text: a cursor
        // under them is scrolled to just below them.
        let pinned = self.sticky.lines().len();
        let under_pinned = pinned > 0
            && cursor_row >= top_row
            && wrap::rows_between(t, &mut w, top_row, cursor_row, pinned) < pinned;
        let above = cursor_row < top_row || (cursor_row == top_row && self.scroll_px > 0.0);
        let new_anchor = if above || under_pinned {
            let back = isize::try_from(pinned).unwrap_or(0);
            Some(wrap::step_rows(t, &mut w, cursor_row, -back).0)
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
            (layout.text_right() - layout.text_left - layout.margin - layout.metrics.cell_width)
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
        wrapper(&mut self.cache, self.wrap, layout, tab_width, &self.folds)
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
        let at = {
            let mut w = wrapper(&mut self.cache, self.wrap, layout, tab_width, &self.folds);
            let anchor = anchor(self.scroll_top, t, &mut w);
            wrap::step_rows(t, &mut w, anchor, rows_down).0
        };
        self.byte_at_x(x, at, doc, layout, tab_width)
    }

    /// The byte under window x `x` on the line starting at `at.start`,
    /// wrapped row `at.row` of it. The compare view (M9) uses this with a
    /// line it chose from its row plan.
    pub fn byte_at_x(
        &mut self,
        x: f32,
        at: RowPos,
        doc: &Document,
        layout: &Layout,
        tab_width: usize,
    ) -> usize {
        let t = &doc.buffer;
        let mut w = wrapper(&mut self.cache, self.wrap, layout, tab_width, &self.folds);
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

    /// The line under window position `(x, y)` (its first byte) and the
    /// display column there, for column selection. Unlike [`View::hit`],
    /// the column is not limited to the line's length: dragging past the
    /// end of a short line still names a column further right. With soft
    /// wrap, the column counts from the start of the whole line.
    pub fn hit_column(
        &mut self,
        x: f32,
        y: f32,
        doc: &Document,
        layout: &Layout,
        tab_width: usize,
    ) -> (usize, usize) {
        let t = &doc.buffer;
        let lh = layout.metrics.line_height;
        let rows_down = ((y - layout.top + self.px()) / lh).floor().max(0.0);
        #[expect(
            clippy::cast_possible_truncation,
            reason = "a row count within the window"
        )]
        let rows_down = rows_down as isize;
        let mut w = wrapper(&mut self.cache, self.wrap, layout, tab_width, &self.folds);
        let anchor = anchor(self.scroll_top, t, &mut w);
        let (at, _) = wrap::step_rows(t, &mut w, anchor, rows_down);
        let span = t.line_span(at.start);
        let long = long_line::is_long(span);
        let wrapped = self.wrap && !long;
        let scroll = if wrapped { 0.0 } else { self.scroll_x };
        let x_text = (f64::from(x - layout.text_left) + scroll).max(0.0);
        // The nearer column boundary: a click right of a character's middle
        // is after it.
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "a non-negative column count"
        )]
        let col = (x_text / f64::from(layout.metrics.cell_width)).round() as usize;
        let row_col = if wrapped {
            let text = t.slice(span.start..span.end);
            w.rows(&text).get(at.row).map_or(0, |r| r.col)
        } else {
            0
        };
        (span.start, row_col + col)
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
        carets: &Carets,
        layout: &Layout,
        tab_width: usize,
        deco: &Decorations<'_>,
    ) {
        let t = &doc.buffer;
        let cursor = carets.cursor(t);
        let selections = carets.selections.as_slice();
        let brackets = self.bracket_pair(doc, carets);
        self.cache.next_frame();
        self.cursor_at = None;
        self.chips.clear();

        begin_column(gpu, layout);

        let px = self.px();
        let (wrapping, scroll_x) = (self.wrap, self.scroll_x);
        let mut w = wrapper(&mut self.cache, wrapping, layout, tab_width, &self.folds);
        let mut at = anchor(self.scroll_top, t, &mut w);
        let mut next_match = deco.matches.partition_point(|m| m.end <= at.start);
        // Selections and cursor heads are sorted, so each is found by
        // walking forward with the rows, as matches are.
        let first_sel = selections.partition_point(|s| s.range().end < at.start);
        let mut next_head = selections.partition_point(|s| s.head < at.start);
        // Counted once for the top line, then by one per line while known.
        let mut number = t.line_number(at.start);
        let mut shown_end = t.len_bytes();
        let spans: &[Span] = if deco.looks.is_none() {
            &[]
        } else {
            highlight_ranges(t, &mut w, at, layout, scroll_x, &mut self.hl_ranges);
            self.highlighter.spans(&doc.syntax, t, &self.hl_ranges)
        };
        // Sticky scroll (M20): asked again only when the text, its tree,
        // the top row, the wrapping or the folds changed.
        let key = (
            doc.version,
            doc.syntax.rev(),
            self.scroll_top,
            wrapping.then_some(layout.wrap_cols),
            self.folds.len(),
        );
        self.sticky
            .update(deco.sticky.then_some(key), doc, &mut w, at);
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
            splits: &mut self.splits,
            cursor_at: None,
            line_end: [0.0, 0.0],
            active_guide: deco
                .guides
                .and_then(|w| active_guide(t, cursor, tab_width, w)),
        };
        let geom = (
            scroll_col_of(layout, scroll_x),
            width_cols_of(layout, scroll_x),
            scroll_x,
        );
        while top < layout.bottom {
            let span = t.line_span(at.start);
            // The line number stands out on every line with a cursor.
            let has_cursor = has_cursor(selections, &mut next_head, span);
            let line = LineDraw {
                span,
                first_row: at.row,
                number,
                continued: false,
                has_cursor,
                mark: None,
            };
            let line_top = (line.first_row == 0).then_some(top);
            next_match = draw_line(
                &mut painter,
                t,
                &line,
                &mut w,
                &mut self.long_text,
                geom,
                &mut top,
                next_match,
            );
            let marks = (&self.folds, &mut self.chips, self.fold_hover);
            let (next, folded) = after_line(&mut painter, &w, t, span, line_top, marks);
            shown_end = next.unwrap_or(t.len_bytes());
            let Some(next) = next else {
                break;
            };
            number = if folded {
                t.line_number(next)
            } else {
                number.and_then(|n| (next <= t.indexed_bytes()).then_some(n + 1))
            };
            at = RowPos {
                start: next,
                row: 0,
            };
        }
        self.cursor_at = painter.cursor_at;
        self.shown = shown_end.saturating_sub(self.scroll_top);
        draw_sticky(&mut painter, &mut self.sticky, doc, geom);
    }

    /// The pinned line at window `y`, by its start, if `y` is on a sticky
    /// row of the last frame.
    #[must_use]
    pub fn sticky_at(&self, layout: &Layout, y: f32) -> Option<usize> {
        if y < layout.top {
            return None;
        }
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "a small non-negative row index"
        )]
        let row = ((y - layout.top) / layout.metrics.line_height).floor() as usize;
        self.sticky.lines().get(row).copied()
    }

    /// Scroll so the line starting at `start` is the top row.
    pub fn scroll_to_line(&mut self, start: usize) {
        self.scroll_top = start;
        self.scroll_px = 0.0;
    }

    /// Queue the rows of one column of a compare view: each entry of
    /// `plans` is one screen row, a line of `doc`, a filler, or a fold
    /// label. Soft wrap is off in compare views, so a line is one row.
    /// `px` is how far the first row is scrolled up out of view. The
    /// caller started the frame with [`begin_frame`]; this begins the
    /// column's own clipped layer.
    #[expect(
        clippy::too_many_arguments,
        reason = "a frame's inputs, as `draw` takes them, plus the row plan"
    )]
    pub fn draw_rows(
        &mut self,
        gpu: &mut Gpu,
        doc: &Document,
        carets: &Carets,
        layout: &Layout,
        tab_width: usize,
        deco: &Decorations<'_>,
        plans: &[RowPlan],
        px: f32,
        mark_color: Color,
    ) {
        let t = &doc.buffer;
        let lh = layout.metrics.line_height;
        let cursor = carets.cursor(t);
        let selections = carets.selections.as_slice();
        let brackets = self.bracket_pair(doc, carets);
        self.cache.next_frame();
        self.cursor_at = None;
        self.wrap = false;

        begin_column(gpu, layout);

        let scroll_x = self.scroll_x;
        let mut w = wrapper(&mut self.cache, false, layout, tab_width, &self.folds);
        planned_ranges(t, plans, &mut self.hl_ranges);
        let spans: &[Span] = if deco.looks.is_none() {
            &[]
        } else {
            self.highlighter.spans(&doc.syntax, t, &self.hl_ranges)
        };
        let first_line_start = plans
            .iter()
            .find_map(|p| p.line)
            .map_or(0, |l| t.line_to_byte(l));
        let mut next_match = deco.matches.partition_point(|m| m.end <= first_line_start);
        let first_sel = selections.partition_point(|s| s.range().end < first_line_start);
        let mut next_head = selections.partition_point(|s| s.head < first_line_start);
        let mut painter = Painter {
            spans,
            runs: &mut self.runs,
            gpu,
            layout,
            tab_width,
            deco,
            wrapping: false,
            sels: selections,
            next_sel: first_sel,
            brackets,
            cursor,
            number: &mut self.number,
            spliced: &mut self.spliced,
            splits: &mut self.splits,
            cursor_at: None,
            line_end: [0.0, 0.0],
            active_guide: None,
        };
        let geom = (
            scroll_col_of(layout, scroll_x),
            width_cols_of(layout, scroll_x),
            scroll_x,
        );
        let mut top = layout.top - px;
        let mut shown = (usize::MAX, 0);
        for plan in plans {
            if top >= layout.bottom {
                break;
            }
            if let Some(bg) = plan.background {
                painter.rect(layout.text_left, layout.text_right(), top, bg);
            }
            match (plan.line, plan.fold) {
                (Some(line), _) => {
                    let (span, continued) =
                        piece_span(t.line_span(t.line_to_byte(line)), plan.piece.as_ref());
                    shown = (shown.0.min(span.start), span.next.unwrap_or(t.len_bytes()));
                    let has_cursor = has_cursor(selections, &mut next_head, span);
                    let draw = LineDraw {
                        span,
                        first_row: 0,
                        number: Some(line),
                        continued,
                        has_cursor,
                        mark: plan.mark.clone().map(|m| (m, mark_color)),
                    };
                    next_match = draw_line(
                        &mut painter,
                        t,
                        &draw,
                        &mut w,
                        &mut self.long_text,
                        geom,
                        &mut top,
                        next_match,
                    );
                }
                (None, Some(hidden)) => {
                    painter.fold_row(hidden, top);
                    top += lh;
                }
                (None, None) => top += lh,
            }
        }
        self.cursor_at = painter.cursor_at;
        if shown.0 != usize::MAX {
            self.scroll_top = shown.0;
            self.shown = shown.1.saturating_sub(shown.0);
        }
    }

    /// Draw the minimap for the frame just drawn, when `layout` has room
    /// for one (M20).
    pub fn draw_minimap(
        &mut self,
        gpu: &mut Gpu,
        doc: &Document,
        layout: &Layout,
        (ui, looks): (&Ui, Option<&Theme>),
        tab_width: usize,
    ) {
        self.minimap.draw(
            gpu,
            doc,
            &self.folds,
            (self.scroll_top, self.shown),
            layout,
            ui,
            looks,
            tab_width,
        );
    }

    /// What a press at `(x, y)` hits on the minimap, if anything.
    #[must_use]
    pub fn minimap_press(&self, x: f32, y: f32) -> Option<minimap::Press> {
        self.minimap.press(x, y)
    }

    /// The minimap mark's height in the last frame.
    #[must_use]
    pub fn minimap_slider_height(&self) -> f32 {
        self.minimap.slider_height()
    }

    /// Drag the minimap's mark so its top is at `y`.
    pub fn drag_minimap(&mut self, y: f32, doc: &Document, layout: &Layout, tab_width: usize) {
        match self.minimap.drag(y) {
            Some(minimap::Drag::Line(start)) => self.scroll_to_line(start),
            Some(minimap::Drag::Share(share)) => {
                let len = doc.buffer.len_bytes();
                let target = share * f64_of(len.saturating_sub(self.shown));
                self.scroll_to_fraction(target / f64_of(len.max(1)), doc, layout, tab_width);
            }
            None => {}
        }
    }

    /// Scroll so the line starting at `start` is in the middle of the
    /// view: a click on the minimap.
    pub fn centre_line(&mut self, start: usize, doc: &Document, layout: &Layout, tab_width: usize) {
        let t = &doc.buffer;
        let half = isize::try_from(layout.full_lines() / 2).unwrap_or(0);
        let mut w = wrapper(&mut self.cache, self.wrap, layout, tab_width, &self.folds);
        let at = wrap::row_pos_of(t, &mut w, start);
        let (to, _) = wrap::step_rows(t, &mut w, at, -half);
        self.scroll_px = 0.0;
        self.set_anchor(t, layout, tab_width, to);
    }

    /// Draw the scrollbar for the frame just drawn, or nothing when the
    /// whole text fits on screen.
    ///
    /// The bar shows while the view scrolls and for [`SCROLLBAR_FADE`]
    /// after, and while the pointer is over it; otherwise it is hidden but
    /// still takes clicks, and hovering brings it back. A view drawn for the
    /// first time counts as scrolled, so it shows its bar at first.
    pub fn draw_scrollbar(&mut self, gpu: &mut Gpu, doc: &Document, layout: &Layout, ui: &Ui) {
        self.scrollbar = self.scrollbar_geometry(doc.buffer.len_bytes(), layout);
        let now = Instant::now();
        let pos = (self.scroll_top, self.scroll_px);
        if self.scroll_seen.is_none_or(|(p, _)| p != pos) {
            self.scroll_seen = Some((pos, now));
        }
        let Some(bar) = self.scrollbar else { return };
        if !self.scrollbar_hover && self.scrollbar_hide_at().is_none_or(|t| now >= t) {
            return;
        }
        let cell = layout.metrics.cell_width;
        let w = (cell * if self.scrollbar_hover { 0.9 } else { 0.5 })
            .round()
            .max(3.0);
        let x = layout.text_right() - w - 2.0 * layout.scale;
        gpu.begin_layer(None);
        gpu.push_rect([x, bar.thumb_top], [w, bar.thumb_h], ui.scrollbar);
    }

    /// When the scrollbar hides, if it is showing because of a recent
    /// scroll: the app wakes then to draw the frame without it.
    #[must_use]
    pub fn scrollbar_hide_at(&self) -> Option<Instant> {
        self.scroll_seen.map(|(_, at)| at + SCROLLBAR_FADE)
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
            x: layout.text_right() - cell,
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

/// Draw the pinned rows of sticky scroll over the top of the text: each
/// line's first row on the sticky background with its line number, then
/// a shadow under the last.
fn draw_sticky<'a>(
    painter: &mut Painter<'a, '_>,
    sticky: &'a mut Sticky,
    doc: &Document,
    (scroll_col, width_cols, scroll_x): (usize, usize, f64),
) {
    let t = &doc.buffer;
    let (spans, lines) = sticky.parts(doc, painter.deco.looks.is_some());
    if lines.is_empty() {
        return;
    }
    painter.spans = spans;
    painter.sels = &[];
    painter.next_sel = 0;
    let layout = painter.layout;
    // A layer of its own: rects in a layer are drawn under all of its
    // glyphs, and the pinned rows must cover the text's.
    begin_column(painter.gpu, layout);
    let lh = layout.metrics.line_height;
    let ui = painter.deco.ui;
    let right = layout.text_right();
    let (shift, first) = if painter.wrapping {
        (0.0, 0)
    } else {
        (row_shift(scroll_x), scroll_col)
    };
    let mut top = layout.top;
    for &start in lines {
        painter
            .gpu
            .push_rect([layout.left, top], [right - layout.left, lh], ui.sticky);
        painter.gutter(t.line_number(start), false, top);
        let span = t.line_span(start);
        let text = t.slice(start..span.end.min(start + crate::sticky::LINE_BYTES));
        painter.span(
            &text,
            Some(start),
            0,
            [layout.text_left - shift, top],
            (first, width_cols),
        );
        top += lh;
    }
    let h = (2.0 * layout.scale).round().max(1.0);
    painter.gpu.push_rect(
        [layout.left, top],
        [right - layout.left, h],
        ui.sticky_shadow,
    );
}

/// Draws rows for one frame; holds what every row needs.
/// The bytes to highlight for the lines in `plans`, into `out`: each line
/// capped so a monster line costs a bounded amount (the rest of such a
/// line stays plain), and near ranges merged.
fn planned_ranges(t: &dyn Text, plans: &[RowPlan], out: &mut Vec<Range<usize>>) {
    out.clear();
    for plan in plans {
        let Some(line) = plan.line else { continue };
        let span = t.line_span(t.line_to_byte(line));
        let end = span
            .next
            .unwrap_or(t.len_bytes())
            .min(span.start + WHOLE_LINES_LIMIT);
        match out.last_mut() {
            Some(last) if span.start <= last.end + MERGE_GAP => last.end = last.end.max(end),
            _ => out.push(span.start..end),
        }
    }
}

/// The horizontal scroll in pixels, for drawing a line's column 0.
#[expect(
    clippy::cast_possible_truncation,
    reason = "guides sit in the first columns; past f32's range they are off screen anyway"
)]
fn row_shift(scroll_x: f64) -> f32 {
    scroll_x as f32
}

/// The leftmost visible column of a line, from the horizontal scroll.
fn scroll_col_of(layout: &Layout, scroll_x: f64) -> usize {
    columns_on_screen(layout, scroll_x).0
}

/// How many columns fit on screen.
fn width_cols_of(layout: &Layout, scroll_x: f64) -> usize {
    columns_on_screen(layout, scroll_x).1
}

/// One logical line to draw, for [`draw_line`].
struct LineDraw {
    span: nimbus_core::LineSpan,
    /// The first wrapped row of the line to draw (rows above are scrolled
    /// off); 0 when wrap is off.
    first_row: usize,
    /// Its line number, or `None` when not known yet.
    number: Option<usize>,
    /// It continues a cut line (M18): `⤶` instead of the number.
    continued: bool,
    /// A cursor is on it: the number stands out.
    has_cursor: bool,
    /// Bytes to draw over a stronger background (a compare view's changed
    /// words), with that colour.
    mark: Option<(Range<usize>, Color)>,
}

/// A fold chip as drawn, `[x0, x1, top]`, and the start of its fold.
type Chip = ([f32; 3], usize);

/// What follows a line [`View::draw`] drew: when it is folded, the chip
/// after it and the `▸` in its gutter, and the next line is the one after
/// the fold; else `▾` if the pointer is on its gutter and it can fold.
/// Returns the next line's start and whether this one was folded.
fn after_line(
    painter: &mut Painter<'_, '_>,
    w: &WrapView<'_>,
    t: &dyn Text,
    span: nimbus_core::LineSpan,
    line_top: Option<f32>,
    (folds, chips, hover): (&Folds, &mut Vec<Chip>, Option<usize>),
) -> (Option<usize>, bool) {
    let fold = if folds.is_empty() {
        None
    } else {
        folds.on_line(span)
    };
    let Some(f) = fold else {
        if hover == Some(span.start) {
            painter.fold_marker(line_top, false);
        }
        return (span.next, false);
    };
    let hidden = t
        .line_number(f.end)
        .zip(t.line_number(f.start))
        .map(|(b, a)| b - a);
    let rect = painter.chip(hidden);
    chips.push((rect, f.start));
    painter.fold_marker(line_top, true);
    (w.next_line(t, span), true)
}

/// Draw one logical line from row `first_row` down, advancing `top` per
/// row drawn. Shared by [`View::draw`] and [`View::draw_rows`]. Returns the
/// index of the first find match not finished on the last row.
#[expect(
    clippy::too_many_arguments,
    reason = "the per-frame state `draw` threads through its loop"
)]
fn draw_line(
    painter: &mut Painter<'_, '_>,
    t: &dyn Text,
    line: &LineDraw,
    w: &mut WrapView<'_>,
    long_text: &mut String,
    (scroll_col, width_cols, scroll_x): (usize, usize, f64),
    top: &mut f32,
    mut next_match: usize,
) -> usize {
    let layout = painter.layout;
    let lh = layout.metrics.line_height;
    let tab_width = painter.tab_width;
    let wrapping = painter.wrapping;
    let span = line.span;
    let has_break = span.next.is_some();
    let guides = painter.deco.guides.map(|_| guide_cols(t, span, tab_width));
    if long_line::is_long(span) {
        let geom = (scroll_col, width_cols, scroll_x);
        let row = long_row(t, span, long_text, layout, geom, *top, tab_width);
        painter.line_gutter(line, *top);
        if let Some(cols) = guides {
            painter.guides(
                cols,
                span.start,
                row.top,
                layout.text_left - row_shift(scroll_x),
            );
        }
        if let Some((m, color)) = &line.mark {
            painter.mark(&row, m, *color);
        }
        next_match = painter.row(&row, next_match, scroll_col);
        painter.line_end = [
            row.x_of(row.end(), layout.metrics.cell_width, tab_width),
            row.top,
        ];
        *top += lh;
    } else {
        let text = t.slice(span.start..span.end);
        let rows: &[RowStart] = w.rows(&text);
        for r in line.first_row..rows.len() {
            if *top >= layout.bottom {
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
                seg_start: span.start + range.start,
                start_col,
                last_row: r + 1 == rows.len(),
                has_break,
                top: *top,
                left: layout.text_left - shift,
            };
            if r == 0 {
                painter.line_gutter(line, *top);
            }
            if let Some(cols) = guides {
                painter.guides(cols, span.start, row.top, layout.text_left - shift);
            }
            if let Some((m, color)) = &line.mark {
                painter.mark(&row, m, *color);
            }
            let first = if wrapping { start_col } else { scroll_col };
            next_match = painter.row(&row, next_match, first);
            painter.line_end = [
                row.x_of(row.end(), layout.metrics.cell_width, tab_width),
                row.top,
            ];
            *top += lh;
        }
    }
    next_match
}

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
    /// Scratch for the cursor and selection edges on a row (ligatures).
    splits: &'a mut Vec<usize>,
    cursor_at: Option<[f32; 2]>,
    /// The guide enclosing the cursor: its level and the bytes it spans.
    active_guide: Option<(usize, Range<usize>)>,
    /// Where the last row drawn ends, `[x, top]`: a fold chip goes after
    /// it.
    line_end: [f32; 2],
}

impl Painter<'_, '_> {
    /// The gutter of a line's first row: its number, or `⤶` on a piece
    /// that continues a cut line.
    fn line_gutter(&mut self, line: &LineDraw, top: f32) {
        // No gutter at all: the git window's message box (M28).
        if self.layout.gutter_cols == 0 {
            return;
        }
        if line.continued {
            self.continuation(top);
        } else {
            self.gutter(line.number, line.has_cursor, top);
        }
    }

    /// `⤶` in the gutter of a row that continues a cut line (M18), where
    /// its number would go.
    fn continuation(&mut self, top: f32) {
        let cell = self.layout.metrics.cell_width;
        let pad = self.layout.gutter_cols.saturating_sub(1);
        self.gpu.push_text_line(
            [self.layout.left + self.layout.margin + f(pad) * cell, top],
            "⤶",
            self.deco.ui.gutter,
        );
    }

    /// The line number, right-aligned in the gutter, on a line's first row;
    /// `~` where it is not known yet (a mapped file still indexing).
    fn gutter(&mut self, line: Option<usize>, current: bool, top: f32) {
        if self.layout.gutter_cols == 0 {
            return;
        }
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

    /// The chip after a folded line's last row: `…` and how many lines
    /// the fold hides (when they are counted). Returns where it is,
    /// `[x0, x1, top]`, for clicks.
    fn chip(&mut self, hidden: Option<usize>) -> [f32; 3] {
        let cell = self.layout.metrics.cell_width;
        let [end, top] = self.line_end;
        self.number.clear();
        match hidden {
            Some(1) => self.number.push_str("… 1 line"),
            Some(n) => {
                let _ = write!(self.number, "… {n} lines");
            }
            None => self.number.push('…'),
        }
        let x0 = (end + cell).round().max(self.layout.text_left);
        let x1 = x0 + f(self.number.chars().count() + 2) * cell;
        self.gpu.push_rect(
            [x0, top],
            [x1 - x0, self.layout.metrics.line_height],
            self.deco.ui.fold_chip,
        );
        self.gpu
            .push_text_line([x0 + cell, top], self.number, self.deco.ui.gutter);
        [x0, x1, top]
    }

    /// The fold marker in the gutter of the line whose first row is at
    /// `top` (`None` when scrolled out of view): `▸` when folded, `▾`
    /// when it can be.
    fn fold_marker(&mut self, top: Option<f32>, folded: bool) {
        let Some(top) = top else {
            return;
        };
        let mark = if folded { "▸" } else { "▾" };
        let x = fold_marker_x(self.layout);
        self.gpu.push_text_line([x, top], mark, self.deco.ui.gutter);
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
        let width = ((self.layout.text_right() - self.layout.text_left) / cell)
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
            self.whitespace(row, (first, width));
        }
        self.hidden_chars(row, (first, width));
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

    /// Indent guides on one row of a line indented `cols` columns, whose
    /// column 0 is at `left`: a one-pixel line at the start of each level,
    /// brighter for the level enclosing the cursor.
    fn guides(&mut self, cols: usize, line_start: usize, top: f32, left: f32) {
        let Some(width) = self.deco.guides.filter(|&w| w > 0) else {
            return;
        };
        let cell = self.layout.metrics.cell_width;
        let thin = self.layout.scale.round().max(1.0);
        let lh = self.layout.metrics.line_height;
        let levels = cols.div_ceil(width).min(MAX_GUIDES);
        for level in 0..levels {
            let x = (left + f(level * width) * cell).round();
            if x < self.layout.text_left {
                continue;
            }
            if x >= self.layout.text_right() {
                break;
            }
            let active = self
                .active_guide
                .as_ref()
                .is_some_and(|(l, r)| *l == level && r.contains(&line_start));
            let color = if active {
                self.deco.ui.guide_active
            } else {
                self.deco.ui.guide
            };
            self.gpu.push_rect([x, top], [thin, lh], color);
        }
    }

    /// Whether document byte `pos` is inside a selection.
    fn selected_byte(&self, pos: usize) -> bool {
        self.sels[self.next_sel..]
            .iter()
            .take_while(|s| s.range().start <= pos)
            .any(|s| s.range().contains(&pos))
    }

    /// `·` on each space and `→` on each tab of the row's visible part:
    /// everywhere, or only inside selections, as `render_whitespace` says.
    fn whitespace(&mut self, row: &RowGeom<'_>, (first, width): (usize, usize)) {
        let mode = self.deco.whitespace;
        if mode == Whitespace::None {
            return;
        }
        if mode == Whitespace::Selection
            && !self.sels[self.next_sel..]
                .iter()
                .take_while(|s| s.range().start <= row.end())
                .any(|s| !s.is_empty() && s.range().end > row.seg_start)
        {
            return;
        }
        let tab = self.tab_width;
        let cell = self.layout.metrics.cell_width;
        let (s, e, sc) = visible_span(row.seg, row.start_col, first, width, tab);
        let Some(text) = row.seg.get(s..e) else {
            return;
        };
        let mut col = sc;
        for (off, g) in text.grapheme_indices(true) {
            let w = column::grapheme_cols(g, col, tab);
            let mark = match g {
                " " => Some('·'),
                "\t" => Some('→'),
                _ => None,
            };
            if let Some(mark) = mark
                && (mode == Whitespace::All || self.selected_byte(row.seg_start + s + off))
            {
                let x = row.left + f(col - row.start_col) * cell;
                if x >= self.layout.text_left {
                    self.gpu
                        .push_char([x, row.top], mark, self.deco.ui.whitespace);
                }
            }
            col += w;
        }
    }

    /// A box on each bidirectional control and zero-width character in the
    /// row's visible part (M26), whatever `render_whitespace` says: these
    /// draw as nothing, and "Trojan Source" code uses them to make text
    /// read differently from what it is (`column::is_hidden_char`).
    fn hidden_chars(&mut self, row: &RowGeom<'_>, (first, width): (usize, usize)) {
        if row.seg.is_ascii() {
            return;
        }
        let tab = self.tab_width;
        let cell = self.layout.metrics.cell_width;
        let lh = self.layout.metrics.line_height;
        let (s, e, sc) = visible_span(row.seg, row.start_col, first, width, tab);
        let Some(text) = row.seg.get(s..e) else {
            return;
        };
        for (col, w) in column::hidden_cols(text, sc, tab) {
            let x = row.left + f(col - row.start_col) * cell;
            if x >= self.layout.text_left {
                self.gpu
                    .push_rect([x, row.top], [f(w) * cell, lh], self.deco.ui.hidden_char);
            }
        }
    }

    fn rect(&mut self, x0: f32, x1: f32, top: f32, color: [f32; 4]) {
        let x0 = x0.max(self.layout.text_left);
        if x1 > x0 {
            self.gpu
                .push_rect([x0, top], [x1 - x0, self.layout.metrics.line_height], color);
        }
    }

    /// A stronger background under bytes `m` of this row (changed words
    /// in a compare view), where they fall on it.
    fn mark(&mut self, row: &RowGeom<'_>, m: &Range<usize>, color: Color) {
        let (start, end) = (row.seg_start, row.end());
        if m.end <= start || m.start >= end.max(start + 1) {
            return;
        }
        let cell = self.layout.metrics.cell_width;
        let x0 = row.x_of(m.start.max(start), cell, self.tab_width);
        let x1 = row
            .x_of(m.end.min(end), cell, self.tab_width)
            .max(x0 + cell);
        self.rect(x0, x1, row.top, color);
    }

    /// A fold row: how many unchanged lines are hidden here.
    fn fold_row(&mut self, hidden: usize, top: f32) {
        self.number.clear();
        let _ = write!(self.number, "··· {hidden} unchanged lines ···");
        self.gpu.push_text_line(
            [self.layout.text_left, top],
            self.number,
            self.deco.ui.gutter,
        );
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
        self.splits.clear();
        if let Some(at) = seg_start {
            self.colour_runs(at + s..at + e);
            if self.gpu.ligatures() {
                self.edges(at + s..at + e);
            }
        }
        let x = origin[0] + f(sc - start_col) * self.layout.metrics.cell_width;
        self.gpu.push_text_runs(
            [x, origin[1]],
            &seg[s..e],
            sc,
            (self.runs, self.splits),
            self.deco.ui.text,
            self.layout.text_left,
        );
    }

    /// Fill `splits` with the cursors and selection edges inside document
    /// bytes `range`, as offsets from its start: a ligature across one is
    /// drawn as its separate characters.
    fn edges(&mut self, range: Range<usize>) {
        for sel in &self.sels[self.next_sel..] {
            let r = sel.range();
            if r.start > range.end {
                break;
            }
            for at in [r.start, r.end, sel.head] {
                if at > range.start && at < range.end {
                    self.splits.push(at - range.start);
                }
            }
        }
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
            let Some(look) = self
                .deco
                .looks
                .and_then(|t| t.look(span.table, span.capture))
            else {
                continue;
            };
            let (s, e) = (span.start.max(pos), span.end.min(range.end));
            if s > pos {
                self.runs.push(ColorRun {
                    end: s - range.start,
                    color: text,
                    style: Style::default(),
                });
            }
            self.runs.push(ColorRun {
                end: e - range.start,
                color: look.color.unwrap_or(text),
                style: look.style,
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
    let status = bar.status();
    let [field_x, _, field_w, _] = find_field_rect(layout, &status, 0);

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

/// Where the find bar's field on `row` (0 the pattern, 1 the
/// replacement) is drawn, `[x, y, width, height]`, when the bar shows
/// `status` on its right.
pub(crate) fn find_field_rect(layout: &Layout, status: &str, row: usize) -> [f32; 4] {
    let (cell, lh) = (layout.metrics.cell_width, layout.metrics.line_height);
    let label_x = layout.left + layout.margin;
    let field_x = (label_x + 9.0 * cell).round();
    // Right side: toggles and the match count.
    let right_cols = 8 + status.chars().count().max(10);
    let field_w =
        (layout.width - field_x - f(right_cols) * cell - 2.0 * layout.margin).max(10.0 * cell);
    let y = layout.find_top() + layout.margin + f(row) * (lh + layout.margin);
    [field_x, y, field_w, lh]
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
    field: &Solo,
    origin: [f32; 2],
    width: f32,
    layout: &Layout,
    cursor: bool,
) {
    let m = layout.metrics;
    let cell = m.cell_width;
    let doc = &field.doc;
    let text = doc.buffer.line(0);
    let line_end = text.len();
    let cursor_col = column::col_at(&text, field.carets.cursor(&doc.buffer).min(line_end), 4);
    // Scroll the field so the cursor stays inside it.
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a small non-negative column count"
    )]
    let visible = ((width / cell).floor() as usize).saturating_sub(1).max(1);
    let first = cursor_col.saturating_sub(visible);
    let x0 = origin[0] - f(first) * cell;
    // The field gets a layer clipped to itself, inside the caller's clip;
    // the caller's layer is restored after, so a field in the sidebar does
    // not leave the rest of the sidebar unclipped.
    let outer = gpu.clip();
    gpu.begin_layer(Some(intersect(
        [origin[0], origin[1], width, m.line_height],
        outer,
    )));
    if let Some((a, b)) = field_selection(field, &text) {
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
    gpu.begin_layer(outer);
}

/// `rect` (`[x, y, width, height]`) cut down to lie inside `clip`, or
/// `rect` itself when there is no clip. Rectangles that do not overlap
/// give an empty one, which draws nothing.
fn intersect(rect: [f32; 4], clip: Option<[f32; 4]>) -> [f32; 4] {
    let Some([cx, cy, cw, ch]) = clip else {
        return rect;
    };
    let [x, y, w, h] = rect;
    let (left, top) = (x.max(cx), y.max(cy));
    let (right, bottom) = ((x + w).min(cx + cw), (y + h).min(cy + ch));
    [left, top, (right - left).max(0.0), (bottom - top).max(0.0)]
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
            let w = (layout.ui.width(&t.name) + 5.0 * cell).round();
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

/// Draw a tab bar across the top of `layout`'s column. Only the focused
/// group's active tab carries the accent line, so it is clear which
/// group the keyboard talks to.
pub fn draw_tabs(
    gpu: &mut Gpu,
    ui: &Ui,
    layout: &Layout,
    tabs: &[TabLabel],
    active: usize,
    focused: bool,
) {
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
        }
        if is_active && focused {
            gpu.push_rect(
                [x0, layout.tabs_h - accent],
                [x1 - x0, accent],
                ui.tab_accent,
            );
        }
        let color = if is_active { ui.text } else { ui.gutter };
        gpu.push_ui_text([x0 + 2.0 * cell, text_y], &tab.name, color);
        if tab.dirty {
            let dot_x = x1 - 2.0 * cell;
            gpu.push_text_line([dot_x, text_y], "●", color);
        }
    }
}

/// Draw a line between neighbouring groups, from the top of the window
/// to the status bar. `panes` are the groups' layouts, left to right.
pub fn draw_dividers(gpu: &mut Gpu, ui: &Ui, window: &Layout, panes: &[Layout]) {
    let line = window.scale.round().max(1.0);
    gpu.begin_layer(None);
    for pane in panes.iter().skip(1) {
        gpu.push_rect([pane.left, 0.0], [line, window.bottom], ui.picker_border);
    }
}

/// What the picker overlay shows.
#[derive(Debug)]
pub struct PickerView<'a> {
    /// The query field.
    pub input: &'a Solo,
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

/// Where the picker's query text starts and how wide the field is,
/// `[x, y, width, height]`, and where its row `row` is.
pub(crate) fn picker_rects(layout: &Layout, row: usize) -> ([f32; 4], [f32; 4]) {
    let cell = layout.metrics.cell_width;
    let ([x, y, w], row_h) = picker_box(layout);
    let pad = (layout.margin / 2.0).round();
    let field = [
        x + cell,
        y + pad,
        w - 2.0 * cell,
        layout.metrics.line_height,
    ];
    let list_y = y + row_h + layout.margin;
    (field, [x, list_y + f(row) * row_h, w, row_h])
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
    let ui_font = &layout.ui;
    let status_w = ui_font.width(&view.status) + 2.0 * cell;
    let field_w = width - 2.0 * cell - status_w;
    let field_y = top + pad;
    gpu.push_rect(
        [left + cell - pad, field_y - pad / 2.0],
        [field_w + 2.0 * pad, lh + pad],
        ui.field,
    );
    if view.input.doc.buffer.len_bytes() == 0 {
        gpu.push_ui_text([left + cell, field_y], view.placeholder, ui.gutter);
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
    gpu.push_ui_text(
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
        let text_end = gpu.push_ui_text([left + cell, ty], text, ui.text) + 2.0 * cell;
        let detail_x = left + width - ui_font.width(detail) - cell;
        gpu.push_ui_text([detail_x.max(text_end).round(), ty], detail, ui.gutter);
    }
    if view.rows.is_empty() {
        gpu.push_ui_text([left + cell, list_y], "No matches", ui.gutter);
    }
}

/// Room kept at the left of the drawn title bar for macOS's close,
/// minimise and zoom buttons, in points.
const WINDOW_BUTTONS_PT: f32 = 78.0;

/// Draw the title bar Nimbus paints on macOS, in the space
/// [`Gpu::set_inset_top`] keeps above everything else (negative y):
/// the tab bar's colour, so it runs on into the sidebar and the tabs, with
/// `title` centred and dimmed while the window is in the background, as
/// the system's own title is. The system draws its window buttons on top.
/// Draws nothing when no space is kept.
pub fn draw_title_bar(gpu: &mut Gpu, ui: &Ui, layout: &Layout, title: &str, active: bool) {
    let h = gpu.inset_top();
    if h <= 0.0 {
        return;
    }
    gpu.begin_layer(None);
    gpu.push_rect([0.0, -h], [layout.width, h], ui.tab_bar);
    // Centred in the window, but never under the window buttons: a long
    // title starts after them and is cut short at the right margin.
    let left = (WINDOW_BUTTONS_PT * layout.scale).round();
    let room = (layout.width - left - layout.margin).max(0.0);
    let end = layout.ui.fit(title, room);
    let text = &title[..end];
    let w = layout.ui.width(text);
    let x = ((layout.width - w) / 2.0).max(left).round();
    let y = (-h + (h - layout.metrics.line_height) / 2.0).round();
    let color = if active { ui.text } else { ui.gutter };
    gpu.push_ui_text([x, y], text, color);
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
    let info_w = layout.ui.width(info);
    let info_x = (layout.width - layout.margin - info_w).round();
    gpu.push_ui_text([info_x.max(layout.margin), y], info, ui.gutter);
    if let Some((text, is_error)) = message {
        // Room left for the message, with two cells between it and the
        // info.
        let room = (info_x - layout.margin - 2.0 * cell).max(0.0);
        let end = layout.ui.fit(text, room);
        let color = if is_error { ui.error } else { ui.text };
        gpu.push_ui_text([layout.margin, y], &text[..end], color);
    }
}

/// The selected columns of the first line of a one-line field.
fn field_selection(field: &Solo, text: &str) -> Option<(usize, usize)> {
    let sel = field.carets.selection(&field.doc.buffer).range();
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
    use crate::document::{Editor, Solo};

    #[test]
    fn intersect_keeps_a_field_inside_its_clip() {
        let field = [10.0, 20.0, 300.0, 16.0];
        assert_eq!(intersect(field, None), field);
        // A sidebar 200 px wide cuts the field off at its edge.
        let sidebar = Some([0.0, 0.0, 200.0, 900.0]);
        assert_eq!(intersect(field, sidebar), [10.0, 20.0, 190.0, 16.0]);
        // No overlap at all: nothing to draw.
        let below = Some([0.0, 100.0, 200.0, 50.0]);
        assert_eq!(intersect(field, below)[3], 0.0);
    }

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

    fn doc(text: &str) -> Solo {
        Solo::from_text(text)
    }

    /// `NIMBUS_BENCH_FILE=$PWD/bench/out/src-10mb.rs cargo test --release -p nimbus -- --ignored --nocapture m16_typing_bench`
    ///
    /// M16 task 9: what a keystroke costs with whitespace (all), indent
    /// guides and ligatures on, against all three off: the edit, queueing
    /// the frame (`View::draw`) and drawing it on the GPU. Needs a GPU
    /// (a software one such as llvmpipe will do for the queueing number).
    #[test]
    #[ignore = "benchmark: needs NIMBUS_BENCH_FILE, a GPU and a release build"]
    fn m16_typing_bench() {
        use std::time::Instant;
        let Ok(path) = std::env::var("NIMBUS_BENCH_FILE") else {
            return;
        };
        let context = nimbus_render::GpuContext::open().unwrap();
        let font = nimbus_render::Font::bundled().unwrap();
        let mut gpu = Gpu::headless(context, font, 28.0, (1600, 1000), false);
        let mut doc = Solo::of(Document::open(std::path::Path::new(&path)).unwrap());
        let theme = Theme::default();
        let ui = theme.ui;
        let lines = doc.buffer.len_lines();
        let layout = Layout::new(gpu.metrics(), 1600, 1000, 2.0, lines);
        let modes = [
            ("off", Whitespace::None, None, false),
            ("all on", Whitespace::All, Some(4), true),
        ];
        for (label, whitespace, guides, ligatures) in modes {
            gpu.set_ligatures(ligatures);
            let mut view = View::default();
            let start = doc.buffer.line_to_byte(lines / 2);
            doc.ed().click(start, false);
            let mut queue = Vec::new();
            let mut total = Vec::new();
            for i in 0..300 {
                let t = Instant::now();
                doc.ed()
                    .insert(if i % 7 == 6 { "\n    " } else { "x" })
                    .unwrap();
                view.ensure_cursor_visible(&doc.doc, &doc.carets, &layout, 4);
                begin_frame(&mut gpu, &ui);
                let deco = Decorations {
                    cursor_visible: true,
                    preedit: None,
                    matches: &[],
                    current: None,
                    ui: &ui,
                    looks: None,
                    whitespace,
                    guides,
                    sticky: false,
                };
                view.draw(&mut gpu, &doc.doc, &doc.carets, &layout, 4, &deco);
                queue.push(t.elapsed());
                gpu.render().unwrap();
                total.push(t.elapsed());
            }
            for (what, mut v) in [("edit + queue", queue), ("edit + queue + GPU", total)] {
                v.sort();
                let pick = |p: usize| v[(v.len() - 1) * p / 100];
                println!(
                    "{label:7} {what:20} p50 {:?} p99 {:?} max {:?}",
                    pick(50),
                    pick(99),
                    v[v.len() - 1]
                );
            }
        }
    }

    /// `NIMBUS_BENCH_FILE=$PWD/bench/out/src-10mb.rs cargo test --release -p nimbus -- --ignored --nocapture m20_scroll_bench`
    ///
    /// M20 task 7: what sticky scroll and the minimap add to a scroll
    /// frame (queueing it, `View::draw` plus `draw_minimap`) on a parsed
    /// file, scrolling a line and a half per frame. Needs a GPU (a
    /// software one will do: only the queueing is timed).
    #[test]
    #[ignore = "benchmark: needs NIMBUS_BENCH_FILE, a GPU and a release build"]
    fn m20_scroll_bench() {
        use std::time::Instant;
        let Ok(path) = std::env::var("NIMBUS_BENCH_FILE") else {
            return;
        };
        let context = nimbus_render::GpuContext::open().unwrap();
        let font = nimbus_render::Font::bundled().unwrap();
        let mut gpu = Gpu::headless(context, font, 28.0, (1600, 1000), false);
        let mut doc = Document::open(std::path::Path::new(&path)).unwrap();
        doc.detect_language(&crate::languages::Languages::default());
        doc.syntax.parse_now(&doc.buffer);
        let mut theme = Theme::default();
        theme.resolve_tables();
        let ui = theme.ui;
        let carets = Carets::default();
        let lines = doc.buffer.len_lines();
        let plain = Layout::new(gpu.metrics(), 1600, 1000, 2.0, lines);
        let with_map = plain.with_minimap(200.0);
        let modes = [
            ("neither", false, false),
            ("sticky", true, false),
            ("minimap", false, true),
            ("both", true, true),
        ];
        for (label, sticky, map) in modes {
            let layout = if map { with_map } else { plain };
            let mut view = View::default();
            view.scroll_to_fraction(0.5, &doc, &layout, 4);
            let mut times = Vec::new();
            for i in 0..400 {
                let t = Instant::now();
                let dy = if i % 50 < 25 { 30.0 } else { -30.0 } * (1.0 + f32_of(i % 3));
                view.scroll_by(dy, &doc, &layout, 4);
                begin_frame(&mut gpu, &ui);
                let deco = Decorations {
                    cursor_visible: true,
                    preedit: None,
                    matches: &[],
                    current: None,
                    ui: &ui,
                    looks: Some(&theme),
                    whitespace: Whitespace::Selection,
                    guides: None,
                    sticky,
                };
                view.draw(&mut gpu, &doc, &carets, &layout, 4, &deco);
                if map {
                    view.draw_minimap(&mut gpu, &doc, &layout, (&ui, Some(&theme)), 4);
                }
                times.push(t.elapsed());
                gpu.render().unwrap();
            }
            times.sort();
            let pick = |p: usize| times[(times.len() - 1) * p / 100];
            println!(
                "{label:8} queue p50 {:?} p90 {:?} p99 {:?} max {:?}",
                pick(50),
                pick(90),
                pick(99),
                times[times.len() - 1]
            );
        }
    }

    #[test]
    fn guides_follow_indentation_and_carry_over_blank_lines() {
        use nimbus_core::RopeBuffer;
        let text = "fn a() {\n    if x {\n        y;\n\n        z;\n    }\n\t\tw\n}\n";
        let buf = RopeBuffer::from_text(text);
        let cols = |line: usize| guide_cols(&buf, buf.line_span(buf.line_to_byte(line)), 4);
        assert_eq!((cols(0), cols(1), cols(2)), (0, 4, 8));
        // The blank line inside the block keeps the block's guides.
        assert_eq!(cols(3), 8);
        // Tabs count to the tab stop.
        assert_eq!(cols(6), 8);
        assert_eq!(
            leading_cols(&buf, buf.line_span(buf.line_to_byte(3)), 4),
            None
        );
        // The cursor on `y;` (level 2): the guide at level 1 is active,
        // across lines 2 to 4 (the blank line included).
        let (level, range) = active_guide(&buf, buf.line_to_byte(2) + 9, 4, 4).unwrap();
        assert_eq!(level, 1);
        assert_eq!(range, buf.line_to_byte(2)..buf.line_to_byte(5) - 1);
        // At the top level nothing is active.
        assert!(active_guide(&buf, 0, 4, 4).is_none());
    }

    #[test]
    fn a_huge_indent_is_measured_in_bounded_time() {
        use nimbus_core::RopeBuffer;
        let text = format!("{}x\n", " ".repeat(10 * INDENT_SCAN));
        let buf = RopeBuffer::from_text(&text);
        // Only the first INDENT_SCAN bytes are looked at.
        assert_eq!(leading_cols(&buf, buf.line_span(0), 4), Some(INDENT_SCAN));
    }

    #[test]
    fn the_scrollbar_hides_a_second_after_the_last_scroll() {
        let mut v = View::default();
        assert!(v.scrollbar_hide_at().is_none());
        let before = Instant::now();
        v.scroll_seen = Some(((0, 0.0), before));
        assert_eq!(v.scrollbar_hide_at(), Some(before + SCROLLBAR_FADE));
    }

    #[test]
    fn long_lines_highlight_only_their_visible_bytes() {
        use nimbus_core::RopeBuffer;
        let mut cache = WrapCache::default();
        let none = Folds::new();
        let l = layout(3);
        let mut out = Vec::new();
        // 40 KB: under the long-line limit, so wrapped and counted exactly.
        let long = "ab".repeat(20_000);
        let buf = RopeBuffer::from_text(&format!("short\n{long}\nend\n"));
        let start = buf.line_to_byte(1);
        let at = RowPos { start, row: 0 };
        // No wrap, scrolled 1000 columns right, 50 columns wide.
        let mut w = wrapper(&mut cache, false, &l, 4, &none);
        visible_ranges(&buf, &mut w, at, 1, (1000, 50), &mut out);
        assert_eq!(out, vec![start + 1000..start + 1051]);
        // Wrapped: the rows on screen, from the top row on.
        let mut w = wrapper(&mut cache, true, &l, 4, &none);
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
            let mut w = wrapper(&mut cache, wrap, &l, 4, &none);
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
    fn top(v: &View, d: &Solo) -> (usize, usize) {
        let line = d.buffer.byte_to_line(v.scroll_top);
        (line, v.scroll_top - d.buffer.line_to_byte(line))
    }

    #[test]
    fn the_status_bar_shows_position_selection_and_file_details() {
        let mut d = doc("fn main() {\n\tlet é = 1;\n}\n");
        d.line_ending = LineEnding::CrLf;
        let mut v = View::default();
        assert_eq!(
            v.status_info(&d, &d.carets, 4),
            "Ln 1, Col 1   Spaces: 4   CRLF   UTF-8"
        );
        d.indent = nimbus_core::indent::Indent {
            width: 4,
            tabs: true,
        };
        assert_eq!(
            v.status_info(&d, &d.carets, 4),
            "Ln 1, Col 1   Tabs   CRLF   UTF-8"
        );
        // "Tabs" sits at columns 14..18 of the right-aligned info.
        let l = layout(3);
        let cell = l.metrics.cell_width;
        let info_x = (l.width - l.margin - 33.0 * cell).round();
        let at = |col: f32| v.status_item_at(&l, info_x + col * cell);
        assert_eq!(at(14.5), Some(StatusItem::Indent));
        assert_eq!(at(17.5), Some(StatusItem::Indent));
        assert_eq!(at(2.0), None);
        assert_eq!(at(18.5), None);
        // "CRLF" at 21..25, "UTF-8" at 28..33.
        assert_eq!(at(21.5), Some(StatusItem::LineEnding));
        assert_eq!(at(30.0), Some(StatusItem::Encoding));
        // Line 2, after the tab (4 columns) and "let ": column 9.
        d.ed().click(12 + 1 + 4, false);
        assert_eq!(
            v.status_info(&d, &d.carets, 4),
            "Ln 2, Col 9   Tabs   CRLF   UTF-8"
        );
        // Selecting "é = 1" counts characters, not bytes.
        d.ed().select(17..23);
        assert_eq!(
            v.status_info(&d, &d.carets, 4),
            "5 selected   Ln 2, Col 14   Tabs   CRLF   UTF-8"
        );
        d.carets.selections =
            nimbus_core::Selections::from_vec(vec![Selection::cursor(0), Selection::cursor(12)], 0);
        assert_eq!(
            v.status_info(&d, &d.carets, 4),
            "2 cursors   Ln 1, Col 1   Tabs   CRLF   UTF-8"
        );
        let size = |n| {
            let mut s = String::new();
            write_size(&mut s, n);
            s
        };
        assert_eq!(size(512), "512 B");
        assert_eq!(size(3 * 1024 * 1024 + 400 * 1024), "3.4 MB");
    }

    #[test]
    fn column_hits_go_past_the_end_of_short_lines() {
        let d = doc("ab\nabcdef\n");
        let l = layout(3);
        let mut v = View::default();
        let (cell, lh) = (l.metrics.cell_width, l.metrics.line_height);
        // Four cells in on the first line, which has two: column 4.
        let x = l.text_left + 4.0 * cell + 1.0;
        assert_eq!(v.hit_column(x, l.top + 1.0, &d, &l, 4), (0, 4));
        // The second line, nearer the boundary after column 2.
        let x = l.text_left + 2.4 * cell;
        assert_eq!(v.hit_column(x, l.top + lh + 1.0, &d, &l, 4), (3, 2));
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
        let mut d = Solo::of(Document::default());
        d.buffer = buffer.into();
        assert!(!d.buffer.is_fully_indexed());
        let l = layout(10);
        let mut v = View::default();

        // Primary+End: the cursor goes to the end and the view follows,
        // without waiting for line numbers.
        d.ed().click(text.len(), false);
        v.ensure_cursor_visible(&d, &d.carets, &l, 4);
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
    fn scrolling_and_clicks_step_over_folded_lines() {
        // 100 lines "x"; fold lines 2 to 89 behind line 1.
        let d = doc(&"x\n".repeat(99));
        let l = layout(100);
        let t = &d.buffer;
        let mut v = View::default();
        let fold = nimbus_core::fold::lines_range(t, t.line_to_byte(1), t.line_to_byte(89));
        assert!(v.folds.fold(fold.unwrap()));
        // Two rows down from the top is line 90, the next one shown.
        v.scroll_by(40.0, &d, &l, 4);
        assert_eq!(top(&v, &d).0, 90);
        v.scroll_by(-20.0, &d, &l, 4);
        assert_eq!(top(&v, &d).0, 1);
        // A click on the third row lands on line 90.
        v.scroll_by(-1e6, &d, &l, 4);
        let pos = v.hit(l.text_left, l.top + 2.0 * 20.0 + 1.0, &d, &l, 4);
        assert_eq!(t.byte_to_line(pos), 90);
        // Following an edit before the fold moves it; one inside opens it.
        let mut d = d;
        d.journal_mut().keep = true;
        v.follow(&d);
        d.ed().insert("yy").unwrap();
        v.follow(&d);
        assert_eq!(v.folds.active()[0].start, 5);
        let inside = d.buffer.line_to_byte(40);
        let mut ed = d.ed();
        ed.click(inside, false);
        ed.insert("z").unwrap();
        v.follow(&d);
        assert!(v.folds.is_empty());
    }

    /// `NIMBUS_FOLD_FILE=$PWD/bench/out/log-4gb.log cargo test --release -p nimbus -- --ignored --nocapture fold_bench`
    ///
    /// M13 task 8: on a huge file, the CPU side of what the done-when
    /// list times per frame (drawing itself needs a GPU, and costs what
    /// one unfolded screen costs): folding and unfolding one region,
    /// scrolling a screen at a time through a thousand folds, and a
    /// keystroke in one pane with a second pane on the same document
    /// following it.
    #[test]
    #[ignore = "benchmark: needs NIMBUS_FOLD_FILE and a release build"]
    fn fold_bench() {
        use std::time::{Duration, Instant};
        let Ok(path) = std::env::var("NIMBUS_FOLD_FILE") else {
            return;
        };
        let report = |what: &str, mut samples: Vec<Duration>| {
            samples.sort();
            let pick = |p: usize| samples[(samples.len() - 1) * p / 100];
            println!(
                "{what}: p50 {:?}, p99 {:?}, max {:?} ({} samples)",
                pick(50),
                pick(99),
                samples[samples.len() - 1],
                samples.len()
            );
        };
        let mut doc = Document::open(std::path::Path::new(&path)).unwrap();
        doc.journal_mut().keep = true;
        let len = doc.buffer.len_bytes();
        println!("{} MB, mapped: {}", len >> 20, doc.buffer.is_mapped());
        let screen = layout(1_000_000);
        let fold_at = |t: &dyn Text, p: usize| {
            let first = t.line_span(p);
            let mut last = first;
            for _ in 0..20 {
                last = t.line_span(last.next.unwrap_or(last.start));
            }
            nimbus_core::fold::lines_range(t, first.start, last.start)
        };
        // One region, folded and unfolded.
        let mut view = View::default();
        let mut samples = Vec::new();
        for i in 1..200 {
            let p = len / 200 * i;
            let t0 = Instant::now();
            let f = fold_at(&doc.buffer, p).unwrap();
            view.folds.fold(f.clone());
            view.folds.unfold_line(doc.buffer.line_span(f.start));
            samples.push(t0.elapsed());
        }
        report("fold + unfold one region", samples);
        // A thousand folds, then scrolling a screen at a time.
        let t0 = Instant::now();
        let regions: Vec<_> = (1..=1000)
            .filter_map(|i| fold_at(&doc.buffer, len / 1001 * i))
            .collect();
        view.folds.fold_many(regions);
        println!(
            "1000 folds made in {:?}, {} bytes",
            t0.elapsed(),
            view.folds.len() * 16
        );
        let mut samples = Vec::new();
        for i in 0..300 {
            view.scroll_top = len / 300 * i;
            let t0 = Instant::now();
            view.scroll_by(screen.bottom - screen.top, &doc, &screen, 4);
            let _ = view.hit(screen.text_left, screen.bottom - 1.0, &doc, &screen, 4);
            samples.push(t0.elapsed());
        }
        report("scroll a screen with 1000 folds (walk + hit)", samples);
        // Two panes on one document: type in one, the other follows.
        let (mut typing, mut watching) = (Carets::default(), Carets::default());
        let mut other = View::default();
        let mut samples = Vec::new();
        for i in 0..300 {
            let at = doc.buffer.line_span(len / 301 * i).start;
            Editor::new(&mut doc, &mut watching).click(at, false);
            Editor::new(&mut doc, &mut typing).click(at, false);
            let t0 = Instant::now();
            Editor::new(&mut doc, &mut typing).insert("x").unwrap();
            view.follow(&doc);
            other.follow(&doc);
            let _ = watching.fit(&doc);
            doc.journal_mut().trim();
            samples.push(t0.elapsed());
        }
        report("keystroke with a second pane following", samples);
        println!("folds left after typing: {}", view.folds.len());
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
        let at = d.buffer.line_to_byte(30);
        d.ed().click(at, false);
        v.ensure_cursor_visible(&d, &d.carets, &l, 4);
        assert_eq!(top(&v, &d).0, 21, "line 30 is the last full line");
        let at = d.buffer.line_to_byte(5);
        d.ed().click(at, false);
        v.ensure_cursor_visible(&d, &d.carets, &l, 4);
        assert_eq!(top(&v, &d).0, 5);
    }

    #[test]
    fn keep_visible_counts_wrapped_rows() {
        // Each line wraps into rows at bytes 0 and 9.
        let mut d = doc(&"aaaaaaaa bbbbbbbb\n".repeat(12));
        let l = narrow(13);
        let mut v = View::default();
        v.set_wrap(true);
        d.ed().click(6 * 18 + 9, false); // line 6, row 1
        v.ensure_cursor_visible(&d, &d.carets, &l, 4);
        assert_eq!(top(&v, &d), (2, 0));
        d.ed().click(18 + 9, false); // line 1, row 1
        v.ensure_cursor_visible(&d, &d.carets, &l, 4);
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
        d.ed().select(1..3);
        assert_eq!(field_selection(&d, "abc"), Some((1, 3)));
        d.ed().select(2..2);
        assert_eq!(field_selection(&d, "abc"), None);
    }
}
