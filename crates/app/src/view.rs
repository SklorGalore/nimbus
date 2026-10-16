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
use nimbus_core::{Text, column};
use nimbus_render::{Gpu, Metrics};

use crate::document::Document;
use crate::find::{Field, FindBar};

const TEXT_COLOR: [f32; 4] = [0.85, 0.87, 0.91, 1.0];
const GUTTER_COLOR: [f32; 4] = [0.42, 0.45, 0.52, 1.0];
const CURRENT_LINE_NUMBER: [f32; 4] = [0.75, 0.78, 0.84, 1.0];
const CURSOR_COLOR: [f32; 4] = [0.98, 0.75, 0.30, 1.0];
const PREEDIT_COLOR: [f32; 4] = [0.98, 0.75, 0.30, 1.0];
const SELECTION_COLOR: [f32; 4] = [0.25, 0.36, 0.55, 0.65];
const MATCH_COLOR: [f32; 4] = [0.55, 0.45, 0.15, 0.45];
const CURRENT_MATCH_COLOR: [f32; 4] = [0.85, 0.62, 0.20, 0.70];
const BAR_COLOR: [f32; 4] = [0.14, 0.15, 0.18, 1.0];
const FIELD_COLOR: [f32; 4] = [0.08, 0.09, 0.11, 1.0];
const FIELD_FOCUS_COLOR: [f32; 4] = [0.30, 0.42, 0.62, 1.0];
const TOGGLE_ON_COLOR: [f32; 4] = [0.30, 0.42, 0.62, 1.0];
const ERROR_COLOR: [f32; 4] = [0.95, 0.45, 0.42, 1.0];

#[expect(
    clippy::cast_precision_loss,
    reason = "column and line counts on screen are far below 2^24"
)]
fn f(n: usize) -> f32 {
    n as f32
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
    /// Bottom of the text area: above any message line.
    pub bottom: f32,
    /// Columns that fit in the text area: the soft-wrap width.
    pub wrap_cols: usize,
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
        }
    }

    /// Reserve `top_rows` text rows at the top (the find bar) and
    /// `bottom_rows` at the bottom (the message line).
    #[must_use]
    pub fn with_bars(mut self, top_rows: usize, bottom_rows: usize) -> Self {
        let lh = self.metrics.line_height;
        if top_rows > 0 {
            // The bar has its own padding: margin above and below its rows.
            self.top = 3.0 * self.margin + f(top_rows) * lh;
        }
        self.bottom = (self.height - f(bottom_rows) * lh).max(self.top + lh);
        self
    }

    /// Height of the find bar area, if any rows are reserved for it.
    fn bar_height(&self) -> f32 {
        (self.top - self.margin).max(0.0)
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

/// The top visible row, from the anchored scroll position.
fn anchor(scroll: (usize, usize), t: &dyn Text, w: &mut WrapView<'_>) -> RowPos {
    let line = scroll.0.min(t.len_lines().saturating_sub(1));
    let text = t.line(line);
    RowPos {
        line,
        row: wrap::row_of(w.rows(&text), scroll.1),
    }
}

/// Scroll state and per-frame scratch space.
#[derive(Debug, Default)]
pub struct View {
    /// Logical line of the top row.
    pub scroll_line: usize,
    /// Byte within `scroll_line` where the top row starts. Stored as a byte
    /// rather than a row number so the same text stays at the top when a
    /// resize rewraps the line.
    pub scroll_byte: usize,
    /// How far the top row is scrolled up out of view, `0..line_height`.
    pub scroll_px: f32,
    /// Horizontal scroll in pixels; always 0 while wrapping.
    pub scroll_x: f32,
    /// Soft wrap on.
    pub wrap: bool,
    cache: WrapCache,
    /// Where the cursor was drawn in the last frame, for the IME.
    cursor_at: Option<[f32; 2]>,
    /// Reused for formatting line numbers without allocating per frame.
    number: String,
    /// Reused for splicing preedit text into the cursor's row.
    spliced: String,
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
        let text = t.line(at.line);
        self.scroll_line = at.line;
        self.scroll_byte = w.rows(&text).get(at.row).map_or(0, |r| r.byte);
    }

    /// Turn soft wrap on or off, keeping the top line where it is.
    pub fn set_wrap(&mut self, on: bool) {
        self.wrap = on;
        self.scroll_byte = 0;
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
        let anchor = anchor((self.scroll_line, self.scroll_byte), t, &mut w);
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

    /// Scroll horizontally by `dx` pixels. Does nothing while wrapping.
    pub fn scroll_x_by(&mut self, dx: f32) {
        if !self.wrap {
            self.scroll_x = (self.scroll_x + dx).max(0.0);
        }
    }

    /// Scroll as little as possible to bring the cursor into view.
    pub fn ensure_cursor_visible(&mut self, doc: &Document, layout: &Layout, tab_width: usize) {
        let t = &doc.buffer;
        let cursor = doc.cursor();
        let full = layout.full_lines();
        let mut w = wrapper(&mut self.cache, self.wrap, layout, tab_width);
        let cursor_row = wrap::row_pos_of(t, &mut w, cursor);
        let top_row = anchor((self.scroll_line, self.scroll_byte), t, &mut w);
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
        if self.wrap {
            return;
        }
        let cell = layout.metrics.cell_width;
        let x = f(nimbus_core::movement::column_of(t, cursor, tab_width)) * cell;
        let visible = (layout.width - layout.text_left - layout.margin - cell).max(cell);
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
        let x_text = (x - layout.text_left + self.scroll_x).max(0.0);
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "non-negative half-column count"
        )]
        let half = (x_text / (layout.metrics.cell_width / 2.0)).floor() as usize;
        let mut w = wrapper(&mut self.cache, self.wrap, layout, tab_width);
        let anchor = anchor((self.scroll_line, self.scroll_byte), t, &mut w);
        let (at, _) = wrap::step_rows(t, &mut w, anchor, rows_down);
        let text = t.line(at.line);
        let rows = w.rows(&text);
        t.line_to_byte(at.line) + wrap::byte_at_row_half_col(&text, rows, at.row, half, tab_width)
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
        let cell = layout.metrics.cell_width;
        let cursor = doc.cursor();
        let cursor_line = t.byte_to_line(cursor);
        self.cache.next_frame();
        self.cursor_at = None;

        gpu.begin_frame();
        // The text area is its own layer, clipped so rows scrolled under a
        // bar above or below do not show through it.
        gpu.begin_layer(Some([
            0.0,
            layout.top,
            layout.width,
            layout.bottom - layout.top,
        ]));

        let px = self.px();
        let (wrapping, scroll_x) = (self.wrap, self.scroll_x);
        let mut w = wrapper(&mut self.cache, wrapping, layout, tab_width);
        let mut at = {
            let line = self.scroll_line.min(t.len_lines().saturating_sub(1));
            let text = t.line(line);
            RowPos {
                line,
                row: wrap::row_of(w.rows(&text), self.scroll_byte),
            }
        };
        let mut next_match = {
            let first = t.line_to_byte(at.line) + self.scroll_byte;
            deco.matches.partition_point(|m| m.end <= first)
        };
        let mut top = layout.top - px;
        let mut painter = Painter {
            gpu,
            layout,
            tab_width,
            deco,
            wrapping,
            sel: doc.selection().range(),
            cursor,
            number: &mut self.number,
            spliced: &mut self.spliced,
            cursor_at: None,
        };
        while top < layout.bottom && at.line < t.len_lines() {
            let text = t.line(at.line);
            let line_start = t.line_to_byte(at.line);
            let has_break = t.line_to_byte(at.line + 1) > line_start + text.len();
            let rows: &[RowStart] = w.rows(&text);
            for r in at.row..rows.len() {
                if top >= layout.bottom {
                    break;
                }
                let range = wrap::row_range(rows, r, text.len());
                let start_col = rows[r].col;
                let row = RowGeom {
                    seg: text.get(range.clone()).unwrap_or(""),
                    seg_start: line_start + range.start,
                    start_col,
                    last_row: r + 1 == rows.len(),
                    has_break,
                    top,
                    left: layout.text_left - if wrapping { 0.0 } else { scroll_x },
                };
                if r == 0 {
                    painter.gutter(at.line, at.line == cursor_line, top);
                }
                next_match = painter.row(&row, next_match, scroll_x / cell);
                top += lh;
            }
            at = RowPos {
                line: at.line + 1,
                row: 0,
            };
        }
        self.cursor_at = painter.cursor_at;
    }
}

/// Draws rows for one frame; holds what every row needs.
struct Painter<'a, 'g> {
    gpu: &'g mut Gpu,
    layout: &'a Layout,
    tab_width: usize,
    deco: &'a Decorations<'a>,
    /// Soft wrap on: rows never scroll sideways.
    wrapping: bool,
    sel: Range<usize>,
    cursor: usize,
    number: &'a mut String,
    spliced: &'a mut String,
    cursor_at: Option<[f32; 2]>,
}

impl Painter<'_, '_> {
    /// The line number, right-aligned in the gutter, on a line's first row.
    fn gutter(&mut self, line: usize, current: bool, top: f32) {
        let cell = self.layout.metrics.cell_width;
        self.number.clear();
        let _ = write!(self.number, "{}", line + 1);
        let pad = self.layout.gutter_cols.saturating_sub(self.number.len());
        let color = if current {
            CURRENT_LINE_NUMBER
        } else {
            GUTTER_COLOR
        };
        self.gpu.push_text_line(
            [self.layout.margin + f(pad) * cell, top],
            self.number,
            color,
        );
    }

    /// Draw one row: selection, matches, text and cursor. `first_col` is
    /// the leftmost visible column when scrolled sideways. Returns the index
    /// of the first match not finished on this row.
    fn row(&mut self, row: &RowGeom<'_>, next_match: usize, first_col: f32) -> usize {
        let (cell, lh) = (
            self.layout.metrics.cell_width,
            self.layout.metrics.line_height,
        );
        let tab = self.tab_width;
        if let Some((x0, x1)) = self.selected(row) {
            self.rect(x0, x1, row.top, SELECTION_COLOR);
        }
        let next = self.matches(row, next_match);
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "non-negative column counts within the window"
        )]
        let (first, width) = (
            row.start_col + first_col.max(0.0).floor() as usize,
            ((self.layout.width - self.layout.text_left) / cell)
                .ceil()
                .max(0.0) as usize,
        );
        let preedit = self.deco.preedit.filter(|p| !p.text.is_empty());
        if row.owns(self.cursor)
            && let Some(p) = preedit
        {
            self.preedit_row(row, p, (first, width));
        } else {
            self.span(row.seg, row.start_col, [row.left, row.top], (first, width));
        }
        if row.owns(self.cursor) {
            let x = row.x_of(self.cursor, cell, tab);
            // Keep a cursor in hanging spaces at the wrap edge visible.
            let max_x = self.layout.text_left + f(self.layout.wrap_cols) * cell;
            let x = if self.wrapping { x.min(max_x) } else { x };
            self.cursor_at = Some([x, row.top]);
            if self.deco.cursor_visible && preedit.is_none() && x >= self.layout.text_left {
                let w = (2.0 * self.layout.scale).round().max(1.0);
                self.gpu.push_rect([x, row.top], [w, lh], CURSOR_COLOR);
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
    fn selected(&self, row: &RowGeom<'_>) -> Option<(f32, f32)> {
        let sel = &self.sel;
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
                CURRENT_MATCH_COLOR
            } else {
                MATCH_COLOR
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
    /// and is drawn with that column at `origin`.
    fn span(
        &mut self,
        seg: &str,
        start_col: usize,
        origin: [f32; 2],
        (first, width): (usize, usize),
    ) {
        let (s, e, sc) = visible_span(seg, start_col, first, width, self.tab_width);
        if s >= e {
            return;
        }
        let x = origin[0] + f(sc - start_col) * self.layout.metrics.cell_width;
        self.gpu.push_text_span(
            [x, origin[1]],
            &seg[s..e],
            sc,
            TEXT_COLOR,
            self.layout.text_left,
        );
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
            .push_rect([x0, underline_y], [x1 - x0, thin], PREEDIT_COLOR);
        if let Some((a, b)) = p.cursor {
            let (ca, cb) = (x(self.spliced, at + a), x(self.spliced, at + b));
            if cb > ca {
                self.gpu
                    .push_rect([ca, underline_y], [cb - ca, 2.0 * thin], PREEDIT_COLOR);
            } else {
                self.gpu.push_rect([ca, row.top], [thin, lh], CURSOR_COLOR);
            }
        }
        let spliced = std::mem::take(self.spliced);
        self.span(&spliced, row.start_col, [row.left, row.top], view);
        *self.spliced = spliced;
    }
}

/// Draw the find bar at the top of the window, as an overlay layer.
pub fn draw_find_bar(gpu: &mut Gpu, bar: &FindBar, layout: &Layout, cursor_visible: bool) {
    let m = layout.metrics;
    let (cell, lh) = (m.cell_width, m.line_height);
    let height = layout.bar_height();
    gpu.begin_layer(None);
    gpu.push_rect([0.0, 0.0], [layout.width, height], BAR_COLOR);

    let label_x = layout.margin;
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
        let y = layout.margin + f(row) * (lh + layout.margin);
        gpu.push_text_line([label_x, y], label, GUTTER_COLOR);
        let focused = bar.focused && bar.focus == *field;
        if focused {
            let b = layout.scale.round().max(1.0);
            gpu.push_rect(
                [field_x - b, y - b],
                [field_w + 2.0 * b, lh + 2.0 * b],
                FIELD_FOCUS_COLOR,
            );
        }
        gpu.push_rect([field_x, y], [field_w, lh], FIELD_COLOR);
        let doc = match field {
            Field::Find => &bar.find,
            Field::Replace => &bar.replace,
        };
        draw_field(
            gpu,
            doc,
            [field_x, y],
            field_w,
            layout,
            focused && cursor_visible,
        );
    }

    // Toggles: case and regex, then the status.
    let y = layout.margin;
    let mut x = field_x + field_w + 2.0 * cell;
    for (on, text) in [(bar.case_sensitive, "Aa"), (bar.regex, ".*")] {
        if on {
            gpu.push_rect([x - cell / 2.0, y], [3.0 * cell, lh], TOGGLE_ON_COLOR);
        }
        gpu.push_text_line([x, y], text, TEXT_COLOR);
        x += 3.5 * cell;
    }
    let color = if bar.error.is_some() {
        ERROR_COLOR
    } else {
        GUTTER_COLOR
    };
    gpu.push_text_line([x.round(), y], &status, color);
}

/// Draw a one-line text field: its first line, selection and cursor,
/// clipped to `width` pixels from `origin`.
fn draw_field(
    gpu: &mut Gpu,
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
            SELECTION_COLOR,
        );
    }
    let (start, end, start_col) = visible_span(&text, 0, first, visible + 1, 4);
    gpu.push_text_span(
        [x0 + f(start_col) * cell, origin[1]],
        &text[start..end],
        start_col,
        TEXT_COLOR,
        origin[0],
    );
    if cursor {
        let w = (2.0 * layout.scale).round().max(1.0);
        gpu.push_rect(
            [x0 + f(cursor_col) * cell, origin[1]],
            [w, m.line_height],
            CURSOR_COLOR,
        );
    }
    gpu.begin_layer(None);
}

/// Draw a one-line message at the bottom of the window, as an overlay.
pub fn draw_message(gpu: &mut Gpu, text: &str, layout: &Layout, is_error: bool) {
    let lh = layout.metrics.line_height;
    gpu.begin_layer(None);
    gpu.push_rect(
        [0.0, layout.bottom],
        [layout.width, layout.height - layout.bottom],
        BAR_COLOR,
    );
    let color = if is_error { ERROR_COLOR } else { TEXT_COLOR };
    let y = (layout.bottom + (layout.height - layout.bottom - lh) / 2.0).round();
    gpu.push_text_line([layout.margin, y], text, color);
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

    #[test]
    fn scrolling_clamps_at_both_ends() {
        let d = doc(&"x\n".repeat(99));
        let l = layout(100);
        let mut v = View::default();
        v.scroll_by(-50.0, &d, &l, 4);
        assert_eq!((v.scroll_line, v.scroll_px), (0, 0.0));
        v.scroll_by(45.0, &d, &l, 4);
        assert_eq!(v.scroll_line, 2);
        assert!((v.scroll_px - 5.0).abs() < 1e-3);
        v.scroll_by(-25.0, &d, &l, 4);
        assert_eq!((v.scroll_line, v.scroll_px), (1, 0.0));
        v.scroll_by(1e6, &d, &l, 4);
        assert_eq!((v.scroll_line, v.scroll_px), (99, 0.0));
    }

    #[test]
    fn scrolling_walks_wrapped_rows() {
        // Line 0 wraps at 11 columns into rows starting at bytes 0, 9, 18.
        let d = doc("aaaaaaaa bbbbbbbb cccccccc\nx\ny\nz");
        let l = narrow(4);
        let mut v = View::default();
        v.set_wrap(true);
        let at = |v: &View| (v.scroll_line, v.scroll_byte, v.scroll_px.round());
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
        let mut v = View {
            scroll_line: 7,
            scroll_byte: 9,
            scroll_x: 30.0,
            wrap: true,
            ..View::default()
        };
        v.set_wrap(false);
        assert_eq!((v.scroll_line, v.scroll_byte, v.scroll_x), (7, 0, 0.0));
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
        assert_eq!(v.scroll_line, 21, "line 30 is the last full line");
        d.click(d.buffer.line_to_byte(5), false);
        v.ensure_cursor_visible(&d, &l, 4);
        assert_eq!(v.scroll_line, 5);
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
        assert_eq!((v.scroll_line, v.scroll_byte), (2, 0));
        d.click(18 + 9, false); // line 1, row 1
        v.ensure_cursor_visible(&d, &l, 4);
        assert_eq!((v.scroll_line, v.scroll_byte), (1, 9));
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
    fn field_selection_columns() {
        let mut d = doc("abc");
        d.select(1..3);
        assert_eq!(field_selection(&d, "abc"), Some((1, 3)));
        d.select(2..2);
        assert_eq!(field_selection(&d, "abc"), None);
    }
}
