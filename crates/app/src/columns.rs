//! Aligned columns of text: what the compare and merge views share (M9).
//!
//! A [`Columns`] holds the aligned [`Rows`] of two or three documents, the
//! scroll position over those rows, which equal runs the user opened, and
//! the row plans drawn each frame. The compare view puts two documents in
//! it; the merge view three, with the editable result in the middle. One
//! column is *live*: the tab's own document, which takes the cursor and
//! the keyboard; the others are read-only.
//!
//! Nothing here is per line: rows come from a prefix sum over the hunks,
//! and only the rows on screen are planned and drawn.
//!
//! Rows are made of *units* (M18). Usually a unit is a line, but a compare
//! that cut long lines into pieces ([`Splits`]) has one unit per piece;
//! the columns turn units back into lines wherever a line or a byte is
//! needed, so the views outside see lines as before.

use std::collections::HashSet;
use std::ops::Range;

use nimbus_core::Text;
use nimbus_core::align::{Cell, MAX_COLUMNS, Row, Rows};
use nimbus_core::split::Splits;
use nimbus_render::Gpu;

use crate::document::{Carets, Document};
use crate::theme::{Color, Theme, Ui};
use crate::view::{Decorations, Layout, Preedit, RowPlan, ScrollbarHit, View};

/// What a click on the columns means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Click {
    /// Nothing to do.
    None,
    /// Put the live document's cursor at this byte.
    Cursor(usize),
    /// A fold row was clicked; it has been toggled.
    Toggled,
}

/// Per column, the bytes a row's cell shows: its line's content, or its
/// piece of a cut line. `None` for fillers, folds, or a text not given.
pub type Bytes = [Option<Range<usize>>; MAX_COLUMNS];

/// What the live column draws besides its text.
pub struct Live<'a> {
    /// Blink phase and focus: draw the cursor.
    pub cursor_visible: bool,
    /// An IME composition in progress.
    pub preedit: Option<&'a Preedit>,
    /// Find matches to highlight, and the current one.
    pub matches: (&'a [Range<usize>], Option<Range<usize>>),
    /// Columns per tab character.
    pub tab_width: usize,
}

/// Aligned columns over two or three documents.
pub struct Columns<T> {
    /// The aligned rows.
    pub rows: Rows<T>,
    /// How many columns are drawn side by side; 1 draws every column over
    /// the same area (the unified compare).
    count: usize,
    /// Which column (cell index) is the editable document.
    live: usize,
    scroll_row: usize,
    scroll_px: f32,
    /// Column-0 line starts of folded runs the user opened; column 0 is
    /// read-only in both views, so its numbers do not move.
    unfolded: HashSet<usize>,
    /// Row plans per column for the frame; reused.
    plans: Vec<Vec<RowPlan>>,
    /// The change navigated to last.
    pub current: Option<usize>,
    /// Per column, which lines are cut into pieces: units to lines.
    units: [Splits; MAX_COLUMNS],
}

impl<T: Copy> std::fmt::Debug for Columns<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Columns")
            .field("rows", &self.rows.len())
            .field("count", &self.count)
            .field("scroll_row", &self.scroll_row)
            .finish_non_exhaustive()
    }
}

impl<T: Copy> Columns<T> {
    /// Empty columns: `count` drawn, cell `live` editable.
    pub fn new(rows: Rows<T>, count: usize, live: usize) -> Self {
        Self {
            rows,
            count: count.clamp(1, MAX_COLUMNS),
            live: live.min(MAX_COLUMNS - 1),
            scroll_row: 0,
            scroll_px: 0.0,
            unfolded: HashSet::new(),
            plans: vec![Vec::new(); MAX_COLUMNS],
            current: None,
            units: Default::default(),
        }
    }

    /// Say which lines of each column the rows' units cut into pieces
    /// (none: units are lines).
    pub fn set_units(&mut self, units: [Splits; MAX_COLUMNS]) {
        self.units = units;
    }

    /// The line holding unit `unit` of column `c`, and its piece.
    #[must_use]
    pub fn locate(&self, c: usize, unit: usize) -> (usize, usize) {
        self.units.get(c).map_or((unit, 0), |u| u.locate(unit))
    }

    /// The bytes unit `unit` of column `c` shows in `t`: its piece of a cut
    /// line, with whether it continues the piece before, or `None` for a
    /// whole line.
    #[must_use]
    pub fn piece(&self, c: usize, unit: usize, t: &dyn Text) -> Option<(Range<usize>, bool)> {
        let u = self.units.get(c)?;
        if u.is_empty() {
            return None;
        }
        let (line, piece) = u.locate(unit);
        if line >= t.len_lines() {
            return None;
        }
        u.piece(line, piece, t.line_span(t.line_to_byte(line)))
    }

    /// The live column's first unit of `line`.
    fn live_unit(&self, line: usize) -> usize {
        self.units
            .get(self.live)
            .map_or(line, |u| u.unit_of_line(line))
    }

    /// Draw this many columns side by side (1: all over the same area).
    pub fn set_count(&mut self, count: usize) {
        self.count = count.clamp(1, MAX_COLUMNS);
    }

    /// Replace the rows (after the hunks changed), keeping the runs the
    /// user opened open and the scroll position in range.
    pub fn set_rows(&mut self, mut rows: Rows<T>) {
        for &start in &self.unfolded {
            let row = rows.row_of(0, start);
            if let Some(r) = rows.at(row)
                && matches!(r.cells[0], Cell::Fold { .. })
            {
                rows.toggle_fold(row);
            }
        }
        self.rows = rows;
        self.scroll_row = self.scroll_row.min(self.rows.len().saturating_sub(1));
    }

    /// The row plans of the last [`plan`](Columns::plan), per column.
    #[cfg(test)]
    pub fn plans(&self) -> &[Vec<RowPlan>] {
        &self.plans
    }

    /// How many rows fit on screen.
    fn visible_rows(layout: &Layout) -> usize {
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "a small non-negative row count"
        )]
        let n = ((layout.bottom - layout.top) / layout.metrics.line_height)
            .ceil()
            .max(0.0) as usize;
        n
    }

    fn max_scroll_row(&self, layout: &Layout) -> usize {
        self.rows
            .len()
            .saturating_sub(Self::visible_rows(layout).saturating_sub(1).max(1))
    }

    /// Scroll by `dy` pixels (positive is down).
    pub fn scroll_by(&mut self, dy: f32, layout: &Layout) {
        let lh = layout.metrics.line_height;
        let max_row = self.max_scroll_row(layout);
        let mut px = self.scroll_px + dy;
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "whole rows scrolled, a small number"
        )]
        let whole = (px / lh).floor().abs() as usize;
        if px >= lh {
            self.scroll_row = (self.scroll_row + whole).min(max_row);
            px -= f(whole) * lh;
        } else if px < 0.0 {
            if self.scroll_row >= whole {
                self.scroll_row -= whole;
                px += f(whole) * lh;
            } else {
                self.scroll_row = 0;
                px = 0.0;
            }
        }
        if self.scroll_row >= max_row {
            self.scroll_row = max_row;
            px = 0.0;
        }
        self.scroll_px = px.max(0.0);
    }

    /// Scroll so `row` is on screen, with a little room.
    pub fn show_row(&mut self, row: usize, layout: &Layout) {
        let visible = Self::visible_rows(layout).max(1);
        if row < self.scroll_row + 1 {
            self.scroll_row = row.saturating_sub(1);
            self.scroll_px = 0.0;
        } else if row + 2 >= self.scroll_row + visible {
            self.scroll_row = (row + 2)
                .saturating_sub(visible)
                .min(self.rows.len().saturating_sub(1));
            self.scroll_px = 0.0;
        }
    }

    /// Keep `line` of the live column on screen, opening a fold it is in.
    pub fn ensure_line_visible(&mut self, line: usize, layout: &Layout) {
        if self.rows.is_empty() {
            return;
        }
        let unit = self.live_unit(line);
        let mut row = self.rows.row_of(self.live, unit);
        if let Some(r) = self.rows.at(row)
            && let Cell::Fold { first, .. } = r.cells[0]
        {
            self.unfolded.insert(first);
            self.rows.toggle_fold(row);
            row = self.rows.row_of(self.live, unit);
        }
        self.show_row(row, layout);
    }

    /// Step to the next change after the live column's `line` (or the one
    /// after the current one), or the previous. Returns the change's first
    /// row, scrolled on screen, and records it as current.
    pub fn step_change(&mut self, forward: bool, line: usize, layout: &Layout) -> Option<usize> {
        let row = self.rows.row_of(self.live, self.live_unit(line));
        let found = if forward {
            let from = match self.current {
                Some(n) if self.rows.change_row(n).is_some_and(|r| r <= row) => {
                    self.rows.change_row(n + 1)?
                }
                _ => row + 1,
            };
            self.rows.next_change(from)
        } else {
            self.rows.prev_change(row)
        };
        let (n, start) = found?;
        self.current = Some(n);
        self.show_row(start, layout);
        Some(start)
    }

    /// The live column's line on `row`, or the nearest one below (a
    /// change may have no line on the live side); `None` past the end.
    #[must_use]
    pub fn live_line_from(&self, row: usize) -> Option<usize> {
        self.live_unit_from(row)
            .map(|u| self.locate(self.live, u).0)
    }

    /// The live column's unit on `row`, or the nearest one below.
    fn live_unit_from(&self, row: usize) -> Option<usize> {
        let mut at = row;
        loop {
            let r = self.rows.at(at)?;
            if let Cell::Line(u) = r.cells[self.live] {
                return Some(u);
            }
            at += 1;
        }
    }

    /// The row and column under `(x, y)`, if over the text area.
    fn row_at(&self, layout: &Layout, x: f32, y: f32) -> Option<(usize, usize)> {
        if y < layout.top || y >= layout.bottom || x < layout.left || x >= layout.width {
            return None;
        }
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "a non-negative row on screen"
        )]
        let r = ((y - layout.top + self.scroll_px) / layout.metrics.line_height).floor() as usize;
        let width = (layout.width - layout.left) / f(self.count);
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "a column index on screen"
        )]
        let column = (((x - layout.left) / width).floor().max(0.0) as usize).min(self.count - 1);
        Some((self.scroll_row + r, column))
    }

    /// The layout of each drawn column, left to right.
    #[must_use]
    pub fn layouts(&self, layout: &Layout) -> Vec<Layout> {
        if self.count == 1 {
            return vec![*layout];
        }
        let width = (layout.width - layout.left) / f(self.count);
        (0..self.count)
            .map(|i| {
                let left = (layout.left + f(i) * width).round();
                let right = if i + 1 == self.count {
                    layout.width
                } else {
                    (layout.left + f(i + 1) * width).round()
                };
                layout.column(left, right)
            })
            .collect()
    }

    /// A click at `(x, y)`: a fold toggles; a row puts the live document's
    /// cursor on its line (or the nearest below), at the x clicked when
    /// the click was in the live column.
    pub fn click(
        &mut self,
        doc: &Document,
        view: &mut View,
        layout: &Layout,
        tab_width: usize,
        x: f32,
        y: f32,
    ) -> Click {
        let Some((row, column)) = self.row_at(layout, x, y) else {
            return Click::None;
        };
        let Some(r) = self.rows.at(row) else {
            return Click::None;
        };
        if matches!(r.cells[column.min(MAX_COLUMNS - 1)], Cell::Fold { .. })
            && let Cell::Fold { first, .. } = r.cells[0]
        {
            // `unfolded` is keyed by the first column's line, whichever
            // column was clicked.
            if !self.unfolded.remove(&first) {
                self.unfolded.insert(first);
            }
            self.rows.toggle_fold(row);
            return Click::Toggled;
        }
        let Some(unit) = self.live_unit_from(row) else {
            return Click::None;
        };
        let line = self.locate(self.live, unit).0;
        let start = self
            .piece(self.live, unit, &doc.buffer)
            .map_or_else(|| doc.buffer.line_to_byte(line), |(r, _)| r.start);
        let live_column = if self.count == 1 {
            0
        } else {
            self.live.min(self.count - 1)
        };
        if column == live_column {
            let layouts = self.layouts(layout);
            let pos = nimbus_core::wrap::RowPos { start, row: 0 };
            Click::Cursor(view.byte_at_x(x, pos, doc, &layouts[live_column], tab_width))
        } else {
            Click::Cursor(start)
        }
    }

    /// Plan the rows on screen. `background` gives a cell's background
    /// from its row and column; `marks` the byte ranges to draw stronger
    /// (changed words) per column of a row. Returns how far the first row
    /// is scrolled up, which [`draw`](Columns::draw) also knows.
    /// `texts` are the columns' texts, for finding the pieces of cut
    /// lines; only needed when [`set_units`](Columns::set_units) cut any.
    pub fn plan(
        &mut self,
        layout: &Layout,
        texts: &[&dyn Text],
        background: impl Fn(&Row<T>, usize) -> Option<Color>,
        mut marks: impl FnMut(&Row<T>, &Bytes) -> [Option<Range<usize>>; MAX_COLUMNS],
    ) -> f32 {
        let visible = Self::visible_rows(layout) + 1;
        let columns = self.rows.columns();
        for p in &mut self.plans {
            p.clear();
        }
        for i in 0..visible {
            let Some(r) = self.rows.at(self.scroll_row + i) else {
                break;
            };
            // Each cell's line, piece and bytes, when its text was given.
            let mut lines = [None; MAX_COLUMNS];
            let mut pieces: [Option<(Range<usize>, bool)>; MAX_COLUMNS] = Default::default();
            let mut bytes: Bytes = Default::default();
            for (c, cell) in r.cells.iter().enumerate().take(columns) {
                let Cell::Line(unit) = *cell else { continue };
                let line = self.locate(c, unit).0;
                lines[c] = Some(line);
                let Some(t) = texts.get(c) else { continue };
                pieces[c] = self.piece(c, unit, *t);
                bytes[c] = Some(pieces[c].as_ref().map_or_else(
                    || {
                        let span = t.line_span(t.line_to_byte(line));
                        span.start..span.end
                    },
                    |(r, _)| r.clone(),
                ));
            }
            let marks = marks(&r, &bytes);
            for (c, (cell, mark)) in r.cells.iter().zip(marks).enumerate().take(columns) {
                let mut plan = plan_of(*cell, background(&r, c));
                plan.mark = mark;
                if lines[c].is_some() {
                    plan.line = lines[c];
                    plan.piece = pieces[c].take();
                }
                if self.count == 1 {
                    // Every column over one area: a row is drawn by the
                    // column that has its line, and the fold once.
                    if plan.line.is_none() {
                        plan.background = None;
                    }
                    if c > 0 && plan.fold.is_some() {
                        plan.fold = None;
                    }
                }
                self.plans[c].push(plan);
            }
        }
        self.scroll_px
    }

    /// Plan plain, unaligned rows: each column from its own line
    /// `scroll_row` on, for the frames before a diff is known.
    pub fn plan_unaligned(&mut self, layout: &Layout, lines: &[usize]) -> f32 {
        let visible = Self::visible_rows(layout) + 1;
        for (p, &total) in self.plans.iter_mut().zip(lines) {
            p.clear();
            for i in 0..visible {
                let row = self.scroll_row + i;
                p.push(RowPlan {
                    line: (row < total).then_some(row),
                    fold: None,
                    background: None,
                    mark: None,
                    piece: None,
                });
            }
        }
        self.scroll_px
    }

    /// Queue the columns: each document's rows from its plan, then the
    /// dividers and a scrollbar. The caller started the frame and draws
    /// the bars after. `docs` and `views` are in column order; the live
    /// column gets `live`'s decorations.
    pub fn draw(
        &mut self,
        gpu: &mut Gpu,
        layout: &Layout,
        docs: &[(&Document, &Carets)],
        views: &mut [&mut View],
        live: &Live<'_>,
        theme: &mut Theme,
    ) {
        let (px, tab_width) = (self.scroll_px, live.tab_width);
        let ui = theme.ui;
        let layouts = self.layouts(layout);
        let scroll_x = views.get(self.live).map_or(0.0, |v| v.scroll_x.max(0.0));
        for (c, (&(doc, carets), view)) in docs.iter().zip(views.iter_mut()).enumerate() {
            let col_layout = &layouts[c.min(layouts.len() - 1)];
            view.scroll_x = scroll_x;
            let looks = if doc.syntax.capture_names().is_some() {
                theme.resolve_tables();
                Some(&*theme)
            } else {
                None
            };
            let is_live = c == self.live;
            let deco = Decorations {
                cursor_visible: is_live && live.cursor_visible,
                preedit: if is_live { live.preedit } else { None },
                matches: if is_live { live.matches.0 } else { &[] },
                current: if is_live {
                    live.matches.1.clone()
                } else {
                    None
                },
                ui: &ui,
                looks,
            };
            view.draw_rows(
                gpu,
                doc,
                carets,
                col_layout,
                tab_width,
                &deco,
                &self.plans[c],
                px,
                ui.diff_word,
            );
        }
        if self.count > 1 {
            gpu.begin_layer(None);
            for l in &layouts[1..] {
                gpu.push_rect(
                    [l.left - 1.0, layout.top],
                    [layout.scale.round().max(1.0), layout.bottom - layout.top],
                    ui.bar,
                );
            }
        }
        self.draw_scrollbar(gpu, layout, &ui);
    }

    /// The scrollbar's thumb (its top and height) when the rows overflow
    /// the screen, from the same sums the ordinary view uses.
    fn thumb(&self, layout: &Layout) -> Option<(f32, f32)> {
        let total = self.rows.len().max(1);
        let visible = Self::visible_rows(layout);
        if visible >= total {
            return None;
        }
        let track = layout.bottom - layout.top;
        let (total, visible, scrolled) = (f(total), f(visible), f(self.scroll_row));
        let h = (track * visible / total)
            .max(2.0 * layout.metrics.line_height)
            .min(track);
        let y = layout.top + (track - h) * (scrolled / (total - visible).max(1.0));
        Some((y, h))
    }

    /// Where the scrollbar begins, in x: a press right of it is a press on
    /// the bar, as in the ordinary view.
    fn scrollbar_x(layout: &Layout) -> f32 {
        layout.width - (6.0 * layout.scale).round() - 2.0 * layout.scale - 4.0 * layout.scale
    }

    /// What a press at `(x, y)` on the scrollbar hits, if it is on it.
    #[must_use]
    pub fn scrollbar_hit(&self, layout: &Layout, x: f32, y: f32) -> Option<ScrollbarHit> {
        let (top, h) = self.thumb(layout)?;
        if x < Self::scrollbar_x(layout) || y < layout.top || y >= layout.bottom {
            return None;
        }
        Some(if (top..top + h).contains(&y) {
            ScrollbarHit::Thumb(y - top)
        } else {
            ScrollbarHit::Track
        })
    }

    /// The thumb's height, for centring it on a track press.
    #[must_use]
    pub fn thumb_height(&self, layout: &Layout) -> f32 {
        self.thumb(layout).map_or(0.0, |(_, h)| h)
    }

    /// Put the thumb's top at `y`: scroll to the matching row.
    pub fn drag_scrollbar(&mut self, y: f32, layout: &Layout) {
        let Some((_, h)) = self.thumb(layout) else {
            return;
        };
        let travel = (layout.bottom - layout.top - h).max(1.0);
        let frac = ((y - layout.top) / travel).clamp(0.0, 1.0);
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "a row index within the rows' count"
        )]
        let row = (frac * f(self.max_scroll_row(layout))).round() as usize;
        self.scroll_row = row.min(self.max_scroll_row(layout));
        self.scroll_px = 0.0;
    }

    /// A scrollbar over the rows, at the right edge of the area.
    fn draw_scrollbar(&self, gpu: &mut Gpu, layout: &Layout, ui: &Ui) {
        let Some((y, h)) = self.thumb(layout) else {
            return;
        };
        let w = (6.0 * layout.scale).round();
        gpu.begin_layer(None);
        gpu.push_rect(
            [layout.width - w - 2.0 * layout.scale, y],
            [w, h],
            ui.scrollbar,
        );
    }
}

/// A small count as pixels.
#[expect(
    clippy::cast_precision_loss,
    reason = "row and column counts on screen are small"
)]
fn f(n: usize) -> f32 {
    n as f32
}

fn plan_of(cell: Cell, background: Option<Color>) -> RowPlan {
    match cell {
        Cell::Line(l) => RowPlan {
            line: Some(l),
            fold: None,
            background,
            mark: None,
            piece: None,
        },
        Cell::Filler => RowPlan {
            line: None,
            fold: None,
            background,
            mark: None,
            piece: None,
        },
        Cell::Fold { hidden, .. } => RowPlan {
            line: None,
            fold: Some(hidden),
            background: None,
            mark: None,
            piece: None,
        },
    }
}
