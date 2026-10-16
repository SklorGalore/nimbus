//! The editor view: which part of the document is on screen, and drawing it.
//!
//! Scroll position is a line number plus a pixel offset into that line,
//! rather than one big pixel offset, because an `f32` loses sub-pixel
//! precision past about 16 million pixels, roughly a million lines.

use std::fmt::Write as _;

use std::ops::Range;

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

/// The part of `line` that can be visible when the view's left edge is at
/// column `first_col` and `width_cols` columns fit: returns the byte range
/// and the column where it starts. Starts at or before `first_col` so a wide
/// character straddling the edge is included; ends after the last visible
/// column. Only this span is shaped, so long lines cost what is on screen.
fn visible_span(
    line: &str,
    first_col: usize,
    width_cols: usize,
    tab_width: usize,
) -> (usize, usize, usize) {
    use unicode_segmentation::UnicodeSegmentation;
    let mut col = 0;
    let mut start = None;
    let last_col = first_col + width_cols + 1;
    for (i, g) in line.grapheme_indices(true) {
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
    let (s, sc) = start.unwrap_or((line.len(), col));
    (s, line.len(), sc)
}

/// Number of decimal digits in `n` (at least 1).
fn digits(n: usize) -> usize {
    n.checked_ilog10().map_or(1, |d| d as usize + 1)
}

/// Scroll state.
#[derive(Debug, Clone, Default)]
pub struct View {
    /// First line at the top of the window.
    pub scroll_line: usize,
    /// How far that line is scrolled up out of view, `0..line_height`.
    pub scroll_px: f32,
    /// Horizontal scroll in pixels. No soft wrap until M3, so long lines
    /// scroll sideways.
    pub scroll_x: f32,
    /// Reused for formatting line numbers without allocating per frame.
    number: String,
    /// Reused for splicing preedit text into the cursor's line.
    spliced: String,
}

impl View {
    /// Pixel offset of the top line, rounded so text stays on whole pixels.
    fn px(&self) -> f32 {
        self.scroll_px.round()
    }

    /// Top edge of line `line` on screen.
    fn line_top(&self, line: usize, layout: &Layout) -> f32 {
        let rel = if line >= self.scroll_line {
            f(line - self.scroll_line)
        } else {
            -f(self.scroll_line - line)
        };
        layout.top + rel * layout.metrics.line_height - self.px()
    }

    /// Scroll vertically by `dy` pixels (positive moves toward the end).
    pub fn scroll_by(&mut self, dy: f32, layout: &Layout, len_lines: usize) {
        let lh = layout.metrics.line_height;
        let last = len_lines.saturating_sub(1);
        let px = self.scroll_px + dy;
        let k_f = (px / lh).floor();
        #[expect(
            clippy::cast_possible_truncation,
            reason = "k is a line count from one scroll gesture"
        )]
        let k = k_f as isize;
        let mut line = self.scroll_line.saturating_add_signed(k);
        let mut rest = px - k_f * lh;
        if px < 0.0 && self.scroll_line < k.unsigned_abs() {
            (line, rest) = (0, 0.0);
        }
        if line >= last {
            (line, rest) = (last, 0.0);
        }
        self.scroll_line = line;
        self.scroll_px = rest.clamp(0.0, lh);
    }

    /// Scroll horizontally by `dx` pixels.
    pub fn scroll_x_by(&mut self, dx: f32) {
        self.scroll_x = (self.scroll_x + dx).max(0.0);
    }

    /// Scroll as little as possible to bring the cursor into view.
    pub fn ensure_cursor_visible(&mut self, doc: &Document, layout: &Layout, tab_width: usize) {
        let cursor = doc.cursor();
        let line = doc.buffer.byte_to_line(cursor);
        let full = layout.full_lines();
        if line < self.scroll_line || (line == self.scroll_line && self.scroll_px > 0.0) {
            self.scroll_line = line;
            self.scroll_px = 0.0;
        } else if line >= self.scroll_line + full {
            self.scroll_line = line + 1 - full;
            self.scroll_px = 0.0;
        }

        let cell = layout.metrics.cell_width;
        let x = f(nimbus_core::movement::column_of(
            &doc.buffer,
            cursor,
            tab_width,
        )) * cell;
        let visible = (layout.width - layout.text_left - layout.margin - cell).max(cell);
        if x < self.scroll_x {
            self.scroll_x = (x - 4.0 * cell).max(0.0);
        } else if x > self.scroll_x + visible {
            self.scroll_x = x - visible + 4.0 * cell;
        }
    }

    /// The byte offset under window position `(x, y)` in physical pixels.
    #[must_use]
    pub fn hit(&self, x: f32, y: f32, doc: &Document, layout: &Layout, tab_width: usize) -> usize {
        let lh = layout.metrics.line_height;
        let rows = ((y - layout.top + self.px()) / lh).floor().max(0.0);
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "non-negative row count within the window"
        )]
        let rows = rows as usize;
        let last = doc.buffer.len_lines().saturating_sub(1);
        let line = (self.scroll_line + rows).min(last);
        let x_text = (x - layout.text_left + self.scroll_x).max(0.0);
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "non-negative half-column count"
        )]
        let half = (x_text / (layout.metrics.cell_width / 2.0)).floor() as usize;
        let text = doc.buffer.line(line);
        doc.buffer.line_to_byte(line) + column::byte_at_half_col(&text, half, tab_width)
    }

    /// The cursor's rectangle on screen as `[x, y, width, height]`, used to
    /// draw it and to tell the IME where to put its candidate window.
    #[must_use]
    pub fn cursor_rect(
        &self,
        doc: &Document,
        layout: &Layout,
        tab_width: usize,
        preedit: Option<&Preedit>,
    ) -> [f32; 4] {
        let cursor = doc.cursor();
        let line = doc.buffer.byte_to_line(cursor);
        let col = nimbus_core::movement::column_of(&doc.buffer, cursor, tab_width);
        let cell = layout.metrics.cell_width;
        let x = layout.text_left - self.scroll_x + f(col) * cell;
        let w = preedit.map_or(cell, |p| {
            f(column::line_width(&p.text, tab_width).max(1)) * cell
        });
        [
            x.round(),
            self.line_top(line, layout),
            w,
            layout.metrics.line_height,
        ]
    }

    /// Draw the cursor's line with the composition `p` spliced in at byte
    /// `at`, underlined, with the IME's own cursor or clause marked.
    fn draw_preedit(
        &mut self,
        gpu: &mut Gpu,
        (text, at): (&str, usize),
        p: &Preedit,
        origin: [f32; 2],
        (layout, tab_width): (&Layout, usize),
        (first_col, width_cols): (usize, usize),
    ) {
        let m = layout.metrics;
        let (cell, lh, top) = (m.cell_width, m.line_height, origin[1]);
        let thin = (1.5 * layout.scale).round().max(1.0);
        self.spliced.clear();
        self.spliced.push_str(text.get(..at).unwrap_or(text));
        self.spliced.push_str(&p.text);
        self.spliced.push_str(text.get(at..).unwrap_or(""));
        let start = column::col_at(&self.spliced, at, tab_width);
        let end = column::col_at(&self.spliced, at + p.text.len(), tab_width);
        let underline_y = top + m.baseline + thin;
        gpu.push_rect(
            [origin[0] + f(start) * cell, underline_y],
            [f(end - start) * cell, thin],
            PREEDIT_COLOR,
        );
        if let Some((a, b)) = p.cursor {
            let ca = column::col_at(&self.spliced, at + a, tab_width);
            let cb = column::col_at(&self.spliced, at + b, tab_width);
            if cb > ca {
                gpu.push_rect(
                    [origin[0] + f(ca) * cell, underline_y],
                    [f(cb - ca) * cell, 2.0 * thin],
                    PREEDIT_COLOR,
                );
            } else {
                gpu.push_rect([origin[0] + f(ca) * cell, top], [thin, lh], CURSOR_COLOR);
            }
        }
        let spliced = std::mem::take(&mut self.spliced);
        draw_span(
            gpu, &spliced, origin, first_col, width_cols, tab_width, layout,
        );
        self.spliced = spliced;
    }

    /// Queue everything visible for one frame.
    pub fn draw(
        &mut self,
        gpu: &mut Gpu,
        doc: &Document,
        layout: &Layout,
        tab_width: usize,
        deco: Decorations<'_>,
    ) {
        let Decorations {
            cursor_visible,
            preedit,
            matches,
            current,
        } = deco;
        let m = layout.metrics;
        let lh = m.line_height;
        let cell = m.cell_width;
        let cursor = doc.cursor();
        let cursor_line = doc.buffer.byte_to_line(cursor);

        gpu.begin_frame();
        // The text area is its own layer, clipped so lines scrolled under a
        // bar above or below do not show through it.
        gpu.begin_layer(Some([
            0.0,
            layout.top,
            layout.width,
            layout.bottom - layout.top,
        ]));
        // Matches before the first visible line can be skipped at once.
        let first_byte = doc.buffer.line_to_byte(self.scroll_line);
        let mut next_match = matches.partition_point(|m| m.end <= first_byte);
        let mut line = self.scroll_line;
        loop {
            let top = self.line_top(line, layout);
            if top >= layout.bottom || line >= doc.buffer.len_lines() {
                break;
            }
            // Line number, right-aligned in the gutter.
            self.number.clear();
            let _ = write!(self.number, "{}", line + 1);
            let pad = layout.gutter_cols.saturating_sub(self.number.len());
            let color = if line == cursor_line {
                CURRENT_LINE_NUMBER
            } else {
                GUTTER_COLOR
            };
            gpu.push_text_line([layout.margin + f(pad) * cell, top], &self.number, color);

            let text = doc.buffer.line(line);
            let origin = [layout.text_left - self.scroll_x, top];
            if let Some((a, b)) = selected_columns(doc, line, &text, tab_width) {
                let x0 = (origin[0] + f(a) * cell).max(layout.text_left);
                let x1 = origin[0] + f(b) * cell;
                if x1 > x0 {
                    gpu.push_rect([x0, top], [x1 - x0, lh], SELECTION_COLOR);
                }
            }
            // Search matches on this line, over the selection so the current
            // match stays visible when it is selected.
            next_match = draw_matches(
                gpu,
                LineGeom {
                    text: &text,
                    start: doc.buffer.line_to_byte(line),
                    origin,
                },
                matches,
                next_match,
                current.as_ref(),
                (layout, tab_width),
            );
            #[expect(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "non-negative column counts within the window"
            )]
            let (first_col, width_cols) = (
                (self.scroll_x / cell).floor() as usize,
                ((layout.width - layout.text_left) / cell).ceil().max(0.0) as usize,
            );
            match preedit {
                Some(p) if line == cursor_line && !p.text.is_empty() => {
                    let at = cursor - doc.buffer.line_to_byte(line);
                    self.draw_preedit(
                        gpu,
                        (&text, at),
                        p,
                        origin,
                        (layout, tab_width),
                        (first_col, width_cols),
                    );
                }
                _ => draw_span(gpu, &text, origin, first_col, width_cols, tab_width, layout),
            }
            line += 1;
        }

        if cursor_visible && preedit.is_none_or(|p| p.text.is_empty()) {
            // Still inside the text-area layer, so it is clipped too.
            let [x, y, _, h] = self.cursor_rect(doc, layout, tab_width, None);
            if x >= layout.text_left {
                gpu.push_rect(
                    [x, y],
                    [(2.0 * layout.scale).round().max(1.0), h],
                    CURSOR_COLOR,
                );
            }
        }
    }
}

/// Where one logical line is drawn.
#[derive(Debug, Clone, Copy)]
struct LineGeom<'a> {
    text: &'a str,
    /// Byte offset of the line in the document.
    start: usize,
    /// Screen position of column 0 of the line.
    origin: [f32; 2],
}

/// Highlight the search matches that touch `line`, starting at index
/// `next` in `matches`. Returns the index to continue from on the next line.
fn draw_matches(
    gpu: &mut Gpu,
    line: LineGeom<'_>,
    matches: &[Range<usize>],
    mut next: usize,
    current: Option<&Range<usize>>,
    (layout, tab_width): (&Layout, usize),
) -> usize {
    let cell = layout.metrics.cell_width;
    let end = line.start + line.text.len();
    while let Some(m) = matches.get(next) {
        if m.start > end {
            break;
        }
        let a = column::col_at(line.text, m.start.max(line.start) - line.start, tab_width);
        let b = column::col_at(line.text, m.end.min(end) - line.start, tab_width);
        let color = if current == Some(m) {
            CURRENT_MATCH_COLOR
        } else {
            MATCH_COLOR
        };
        let x0 = (line.origin[0] + f(a) * cell).max(layout.text_left);
        let x1 = line.origin[0] + f(b.max(a + 1)) * cell;
        if x1 > x0 {
            gpu.push_rect(
                [x0, line.origin[1]],
                [x1 - x0, layout.metrics.line_height],
                color,
            );
        }
        if m.end > end {
            break; // continues on the next line
        }
        next += 1;
    }
    next
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
    if let Some((a, b)) = selected_columns(doc, 0, &text, 4) {
        gpu.push_rect(
            [x0 + f(a) * cell, origin[1]],
            [f(b - a) * cell, m.line_height],
            SELECTION_COLOR,
        );
    }
    let (start, end, start_col) = visible_span(&text, first, visible + 1, 4);
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

/// The selected columns of `line` (number `line_idx`, content `text`), as
/// a half-open column range, or `None` if nothing on it is selected. A
/// selection continuing past the end of the line adds one column, showing
/// that the line break is selected too.
fn selected_columns(
    doc: &Document,
    line_idx: usize,
    text: &str,
    tab_width: usize,
) -> Option<(usize, usize)> {
    let sel = doc.selection().range();
    if sel.is_empty() {
        return None;
    }
    let start = doc.buffer.line_to_byte(line_idx);
    let end = start + text.len();
    let next = doc.buffer.line_to_byte(line_idx + 1);
    if sel.end <= start || sel.start > end || (sel.start == end && sel.end <= end) {
        return None;
    }
    let a = column::col_at(text, sel.start.max(start) - start, tab_width);
    let b = column::col_at(text, sel.end.min(end) - start, tab_width);
    let past_end = sel.end > end && next > end;
    Some((a, b + usize::from(past_end)))
}

/// Draw the visible span of `line`, whose column 0 is at `origin`.
fn draw_span(
    gpu: &mut Gpu,
    line: &str,
    origin: [f32; 2],
    first_col: usize,
    width_cols: usize,
    tab_width: usize,
    layout: &Layout,
) {
    let (start, end, start_col) = visible_span(line, first_col, width_cols, tab_width);
    if start >= end {
        return;
    }
    let x = origin[0] + f(start_col) * layout.metrics.cell_width;
    gpu.push_text_span(
        [x, origin[1]],
        &line[start..end],
        start_col,
        TEXT_COLOR,
        layout.text_left,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selection_columns_per_line() {
        let mut d = doc("abc\n日x\nlast");
        d.select(1..7);
        assert_eq!(
            selected_columns(&d, 0, "abc", 4),
            Some((1, 4)),
            "with the line break"
        );
        assert_eq!(selected_columns(&d, 1, "日x", 4), Some((0, 2)));
        assert_eq!(selected_columns(&d, 2, "last", 4), None);
        d.select(3..3);
        assert_eq!(selected_columns(&d, 0, "abc", 4), None, "empty selection");
        d.select(3..4);
        assert_eq!(
            selected_columns(&d, 0, "abc", 4),
            Some((3, 4)),
            "just the line break"
        );
        assert_eq!(selected_columns(&d, 1, "日x", 4), None);
    }

    #[test]
    fn visible_span_covers_only_what_is_on_screen() {
        let line = "abcdefghij";
        assert_eq!(visible_span(line, 0, 3, 4), (0, 4, 0));
        assert_eq!(visible_span(line, 5, 2, 4), (5, 8, 5));
        assert_eq!(
            visible_span(line, 50, 2, 4),
            (10, 10, 10),
            "scrolled past the end"
        );
        // A wide character straddling the left edge is included.
        assert_eq!(visible_span("日本語", 1, 1, 4), (0, 6, 0));
        // A tab straddling the left edge is included, with its true column.
        assert_eq!(visible_span("ab\tcd", 3, 1, 4), (2, 4, 2));
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

    fn doc(text: &str) -> Document {
        Document::from_text(text)
    }

    #[test]
    fn gutter_fits_the_line_count() {
        assert_eq!(layout(9).gutter_cols, 2);
        assert_eq!(layout(1234).gutter_cols, 4);
        assert_eq!(digits(0), 1);
        assert_eq!(digits(100), 3);
    }

    #[test]
    fn full_lines_counts_whole_lines_below_the_margin() {
        assert_eq!(layout(10).full_lines(), 10);
    }

    #[test]
    fn scrolling_clamps_at_both_ends() {
        let l = layout(100);
        let mut v = View::default();
        v.scroll_by(-50.0, &l, 100);
        assert_eq!((v.scroll_line, v.scroll_px), (0, 0.0));
        v.scroll_by(45.0, &l, 100);
        assert_eq!(v.scroll_line, 2);
        assert!((v.scroll_px - 5.0).abs() < 1e-3);
        v.scroll_by(-25.0, &l, 100);
        assert_eq!((v.scroll_line, v.scroll_px), (1, 0.0));
        v.scroll_by(1e6, &l, 100);
        assert_eq!((v.scroll_line, v.scroll_px), (99, 0.0));
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
    fn clicks_map_to_lines_and_columns() {
        let d = doc("abc\n日x\nlast");
        let l = layout(3);
        let v = View::default();
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
}
