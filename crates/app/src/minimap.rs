//! The minimap (M20): a narrow column at the right of the text showing
//! the lines around the view as coloured blocks, one short row per line,
//! with the lines on screen marked. Dragging the mark scrolls.
//!
//! It never draws the whole file. It shows a *window* of lines around the
//! view, as many as fit the column (at most [`MAX_ROWS`]), placed so that
//! the view's lines sit as far down the column as the view is through the
//! file. That needs no line count, so a 4 GB log being indexed shows its
//! neighbourhood like any file. Each line is drawn from the same syntax
//! spans the text uses, as rectangles only: no glyphs, so no atlas and no
//! shaping, which is why it is cheap. The blocks of each line are kept
//! between frames, and scrolling works out only the lines that came into
//! the window.

use std::ops::Range;

use nimbus_core::{Folds, Text};
use nimbus_render::Gpu;
use nimbus_syntax::highlight::{Highlighter, Span};

use crate::document::Document;
use crate::theme::{Color, Theme, Ui};
use crate::view::Layout;

/// The most lines the minimap shows: 4,000 rows at two pixels is a very
/// tall window. A file longer than the window shows its neighbourhood.
pub const MAX_ROWS: usize = 4000;

/// Columns of a line drawn at most: the rest is past the minimap's edge.
const MAX_COLS: usize = 400;

/// One block: a run of non-blank characters of one colour.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Block {
    /// First column.
    col: u16,
    /// Columns covered.
    len: u16,
    color: Color,
}

/// A line in the window: where it starts and its blocks.
#[derive(Debug, Clone)]
struct Line {
    start: usize,
    blocks: Range<usize>,
}

/// What the blocks were worked out for: the text's version, the syntax
/// revision, the columns drawn, the tab width and the theme's identity
/// (its text colour stands for it). Lines are reused while it holds.
type Key = (u64, u64, usize, usize, [u32; 4]);

/// Where the minimap was drawn, for clicks and drags.
#[derive(Debug, Clone, Copy)]
struct Geometry {
    /// Left edge of the column.
    x: f32,
    /// Top and height of the column.
    top: f32,
    h: f32,
    /// Height of one line's row.
    row: f32,
    /// The mark over the lines on screen.
    slider_top: f32,
    slider_h: f32,
    /// The whole text is in the window: drags map rows to lines.
    whole: bool,
    /// How far the mark can travel when the window moves with the view:
    /// a drag maps it to a share of the text.
    travel: f32,
}

/// What a press on the minimap does.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Press {
    /// On the mark, this far below its top: dragging keeps that point
    /// under the pointer.
    Slider(f32),
    /// Elsewhere: scroll so the line starting here is in the middle of
    /// the view.
    Line(usize),
}

/// One view's minimap.
#[derive(Debug, Default)]
pub struct Minimap {
    lines: Vec<Line>,
    blocks: Vec<Block>,
    key: Option<Key>,
    /// Scratch: the window's line starts, then the lines reused.
    starts: Vec<usize>,
    next_lines: Vec<Line>,
    next_blocks: Vec<Block>,
    /// The byte ranges of lines not yet worked out, to colour.
    ranges: Vec<Range<usize>>,
    highlighter: Highlighter,
    geometry: Option<Geometry>,
}

/// The lines of the window for a view whose top line starts at `top`,
/// into `out`: `rows` lines (fewer for a short text), starting `lead`
/// lines above `top`, or fewer where the text starts, and moved up where
/// it ends so the column stays full. Folded lines are skipped.
pub fn window(
    t: &dyn Text,
    folds: &Folds,
    top: usize,
    lead: usize,
    rows: usize,
    out: &mut Vec<usize>,
) {
    out.clear();
    let mut first = top;
    for _ in 0..lead {
        match folds.prev_line(t, first) {
            Some(p) => first = p,
            None => break,
        }
    }
    let mut at = Some(first);
    while let Some(s) = at {
        if out.len() >= rows {
            break;
        }
        out.push(s);
        at = folds.next_line(t, t.line_span(s));
    }
    // The end came first: fill the column from above.
    let mut above = Vec::new();
    let mut head = first;
    while out.len() + above.len() < rows {
        let Some(p) = folds.prev_line(t, head) else {
            break;
        };
        above.push(p);
        head = p;
    }
    if !above.is_empty() {
        above.reverse();
        out.splice(0..0, above);
    }
}

impl Minimap {
    /// Queue the minimap for a view whose top row is in the line holding
    /// `scroll_top`, which showed `shown` bytes in the frame just drawn.
    /// `layout.minimap` is the column's width; nothing is drawn when it
    /// is 0.
    #[expect(
        clippy::too_many_arguments,
        reason = "a frame's inputs: the view's scroll, the text, the theme"
    )]
    pub fn draw(
        &mut self,
        gpu: &mut Gpu,
        doc: &Document,
        folds: &Folds,
        (scroll_top, shown): (usize, usize),
        layout: &Layout,
        ui: &Ui,
        looks: Option<&Theme>,
        tab_width: usize,
    ) {
        self.geometry = None;
        if layout.minimap <= 0.0 {
            return;
        }
        let t = &doc.buffer;
        let row = (2.0 * layout.scale).round().max(2.0);
        let (x, top) = (layout.text_right(), layout.top);
        let h = (layout.bottom - top).max(0.0);
        let rows = rows_in(h, row);
        if rows == 0 {
            return;
        }
        let top_line = folds.visible_line(t, scroll_top).start;
        let shown_end = scroll_top + shown;
        // How many of the window's lines the view shows: counted along
        // the text (at most a screenful).
        let mut view_lines = 1;
        let mut at = top_line;
        while view_lines < rows {
            match folds.next_line(t, t.line_span(at)) {
                Some(n) if n < shown_end => {
                    view_lines += 1;
                    at = n;
                }
                _ => break,
            }
        }
        let len = t.len_bytes();
        let share = if len <= shown {
            0.0
        } else {
            (f64_of(scroll_top) / f64_of(len - shown)).clamp(0.0, 1.0)
        };
        let room = rows.saturating_sub(view_lines);
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "a row count within the column"
        )]
        let lead = (share * f64_of(room)).round() as usize;
        let mut starts = std::mem::take(&mut self.starts);
        window(t, folds, top_line, lead, rows, &mut starts);
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "a small non-negative column count"
        )]
        let cols = ((layout.minimap / layout.scale.max(1.0)).floor() as usize).min(MAX_COLS);
        let key = (
            doc.version,
            doc.syntax.rev(),
            cols,
            tab_width,
            ui.text.map(f32::to_bits),
        );
        self.refresh(doc, &starts, key, ui, looks);
        self.starts = starts;

        let col_w = layout.minimap / f_of(cols.max(1));
        gpu.begin_layer(Some([x, top, layout.minimap, h]));
        let block_h = (row * 0.75).round().max(1.0);
        let fade = |mut c: Color| {
            c[3] *= 0.7;
            c
        };
        let mut slider: Option<(usize, usize)> = None;
        for (i, line) in self.lines.iter().enumerate() {
            let y = top + f_of(i) * row;
            for b in self.blocks.get(line.blocks.clone()).unwrap_or(&[]) {
                gpu.push_rect(
                    [x + f_of(usize::from(b.col)) * col_w, y],
                    [f_of(usize::from(b.len)) * col_w, block_h],
                    fade(b.color),
                );
            }
            if line.start >= top_line && (line.start < shown_end || line.start == top_line) {
                slider = Some(slider.map_or((i, i), |(a, _)| (a, i)));
            }
        }
        let (first, last) = slider.unwrap_or((0, 0));
        let slider_top = top + f_of(first) * row;
        let slider_h = f_of(last + 1 - first) * row;
        gpu.push_rect(
            [x, slider_top],
            [layout.minimap, slider_h],
            ui.minimap_slider,
        );
        let whole = self.lines.first().is_some_and(|l| l.start == 0)
            && self
                .lines
                .last()
                .is_none_or(|l| folds.next_line(t, t.line_span(l.start)).is_none());
        self.geometry = Some(Geometry {
            x,
            top,
            h,
            row,
            slider_top,
            slider_h,
            whole,
            travel: f_of(room) * row,
        });
    }

    /// Bring the window's blocks up to date: lines already worked out for
    /// the same key are kept; the rest are coloured and measured.
    fn refresh(
        &mut self,
        doc: &Document,
        starts: &[usize],
        key: Key,
        ui: &Ui,
        looks: Option<&Theme>,
    ) {
        let t = &doc.buffer;
        let same = self.key == Some(key);
        self.key = Some(key);
        let cols = key.2;
        let tab_width = key.3.max(1);
        // The lines not in the last window, as merged byte ranges.
        self.ranges.clear();
        let mut old = 0;
        let mut missing = 0;
        // The start of the line after the last one added, when that line
        // was taken whole: a line starting there extends the same range.
        let mut joins = None;
        for &s in starts {
            let kept = same && {
                while self.lines.get(old).is_some_and(|l| l.start < s) {
                    old += 1;
                }
                self.lines.get(old).is_some_and(|l| l.start == s)
            };
            if kept {
                continue;
            }
            missing += 1;
            let span = t.line_span(s);
            let end = span.end.min(s + cols * 4);
            match self.ranges.last_mut() {
                Some(r) if joins == Some(s) => r.end = end,
                _ => self.ranges.push(s..end),
            }
            joins = if end == span.end { span.next } else { None };
        }
        if same && missing == 0 && self.lines.len() == starts.len() {
            return;
        }
        let spans: &[Span] = if looks.is_some() && !self.ranges.is_empty() {
            self.highlighter.spans(&doc.syntax, t, &self.ranges)
        } else {
            &[]
        };
        self.next_lines.clear();
        self.next_blocks.clear();
        let mut old = 0;
        let mut next_span = 0;
        for &s in starts {
            if same {
                while self.lines.get(old).is_some_and(|l| l.start < s) {
                    old += 1;
                }
                if let Some(l) = self.lines.get(old).filter(|l| l.start == s) {
                    let from = self.next_blocks.len();
                    self.next_blocks
                        .extend_from_slice(self.blocks.get(l.blocks.clone()).unwrap_or(&[]));
                    self.next_lines.push(Line {
                        start: s,
                        blocks: from..self.next_blocks.len(),
                    });
                    continue;
                }
            }
            let from = self.next_blocks.len();
            let span = t.line_span(s);
            let text = t.slice(s..span.end.min(s + cols * 4));
            next_span = measure(
                &text,
                s,
                (cols, tab_width),
                (spans, next_span),
                (ui, looks),
                &mut self.next_blocks,
            );
            self.next_lines.push(Line {
                start: s,
                blocks: from..self.next_blocks.len(),
            });
        }
        std::mem::swap(&mut self.lines, &mut self.next_lines);
        std::mem::swap(&mut self.blocks, &mut self.next_blocks);
    }

    /// What a press at `(x, y)` hits, if it is on the minimap.
    #[must_use]
    pub fn press(&self, x: f32, y: f32) -> Option<Press> {
        let g = self.geometry?;
        if x < g.x || y < g.top || y >= g.top + g.h {
            return None;
        }
        if y >= g.slider_top && y < g.slider_top + g.slider_h {
            return Some(Press::Slider(y - g.slider_top));
        }
        let i = row_at(y - g.top, g.row);
        let line = self.lines.get(i).or(self.lines.last())?;
        Some(Press::Line(line.start))
    }

    /// Where a drag that holds the mark's top at `y` should scroll: a
    /// line to put at the top when the whole text is in the window, or
    /// else a share of the text (0 at the start, 1 at the end).
    #[must_use]
    pub fn drag(&self, y: f32) -> Option<Drag> {
        let g = self.geometry?;
        if g.whole {
            let i = row_at(y - g.top, g.row);
            let line = self.lines.get(i).or(self.lines.last())?;
            return Some(Drag::Line(line.start));
        }
        let share = ((y - g.top) / g.travel.max(1.0)).clamp(0.0, 1.0);
        Some(Drag::Share(f64::from(share)))
    }

    /// The mark's height in the last frame.
    #[must_use]
    pub fn slider_height(&self) -> f32 {
        self.geometry.map_or(0.0, |g| g.slider_h)
    }
}

/// Where a drag of the minimap's mark scrolls to.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Drag {
    /// Put the line starting here at the top.
    Line(usize),
    /// Scroll this share of the way through the text.
    Share(f64),
}

/// Append the blocks of one line, `text` starting at document byte
/// `start`, to `out`: runs of non-blank characters of one colour, up to
/// `cols` columns. `spans` are sorted; the search for this line starts
/// at `next` and the index for the next line is returned.
fn measure(
    text: &str,
    start: usize,
    (cols, tab_width): (usize, usize),
    (spans, mut next): (&[Span], usize),
    (ui, looks): (&Ui, Option<&Theme>),
    out: &mut Vec<Block>,
) -> usize {
    let mut col = 0usize;
    let mut open: Option<Block> = None;
    for (i, c) in text.char_indices() {
        if col >= cols {
            break;
        }
        let at = start + i;
        let w = if c == '\t' {
            tab_width - col % tab_width
        } else {
            1
        };
        if c.is_whitespace() {
            out.extend(open.take());
            col += w;
            continue;
        }
        while spans.get(next).is_some_and(|s| s.end <= at) {
            next += 1;
        }
        let color = spans
            .get(next)
            .filter(|s| s.start <= at)
            .and_then(|s| looks.and_then(|l| l.look(s.table, s.capture)))
            .and_then(|l| l.color)
            .unwrap_or(ui.text);
        let c16 = u16::try_from(col).unwrap_or(u16::MAX);
        match &mut open {
            Some(b) if same_colour(b.color, color) => b.len = b.len.saturating_add(1),
            _ => {
                out.extend(open.take());
                open = Some(Block {
                    col: c16,
                    len: 1,
                    color,
                });
            }
        }
        col += w;
    }
    out.extend(open);
    next
}

/// Whether two colours are the same, bit for bit (they come from the
/// same theme, so equal colours have equal bits).
fn same_colour(a: Color, b: Color) -> bool {
    a.map(f32::to_bits) == b.map(f32::to_bits)
}

/// How many rows of `row` pixels fit in `h`, up to [`MAX_ROWS`].
fn rows_in(h: f32, row: f32) -> usize {
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a small non-negative row count"
    )]
    let n = (h / row).floor().max(0.0) as usize;
    n.min(MAX_ROWS)
}

/// The row at `dy` pixels below the column's top.
fn row_at(dy: f32, row: f32) -> usize {
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a small non-negative row index"
    )]
    let i = (dy / row).floor().max(0.0) as usize;
    i
}

#[expect(clippy::cast_precision_loss, reason = "row and column counts")]
fn f_of(n: usize) -> f32 {
    n as f32
}

#[expect(clippy::cast_precision_loss, reason = "byte offsets as a share")]
fn f64_of(n: usize) -> f64 {
    n as f64
}

#[cfg(test)]
mod tests {
    use super::*;
    use nimbus_core::RopeBuffer;

    fn numbered(n: usize, prefix: &str) -> String {
        let mut s = String::new();
        for i in 0..n {
            s.push_str(prefix);
            s.push_str(&i.to_string());
            s.push('\n');
        }
        s
    }

    fn lines_of(t: &RopeBuffer, starts: &[usize]) -> Vec<usize> {
        starts.iter().map(|&s| t.byte_to_line(s)).collect()
    }

    #[test]
    fn the_window_leads_the_view_and_stays_full_at_the_ends() {
        let text = numbered(100, "line ");
        let t = RopeBuffer::from_text(&text);
        let folds = Folds::new();
        let mut out = Vec::new();
        // Line 50 at the top, ten lines above it in a column of 20.
        window(&t, &folds, t.line_to_byte(50), 10, 20, &mut out);
        assert_eq!(lines_of(&t, &out), (40..60).collect::<Vec<_>>());
        // Near the start: it cannot lead by more than there is.
        window(&t, &folds, t.line_to_byte(3), 10, 20, &mut out);
        assert_eq!(lines_of(&t, &out), (0..20).collect::<Vec<_>>());
        // Near the end: moved up so the column stays full (100 lines and
        // the empty one after the last line break).
        window(&t, &folds, t.line_to_byte(95), 0, 20, &mut out);
        assert_eq!(lines_of(&t, &out), (81..101).collect::<Vec<_>>());
        // A short text is all there is.
        let short = RopeBuffer::from_text("a\nb\n");
        window(&short, &folds, 0, 0, 20, &mut out);
        assert_eq!(out.len(), 3);
    }

    #[test]
    fn folded_lines_are_skipped() {
        let text = numbered(10, "line ");
        let t = RopeBuffer::from_text(&text);
        let mut folds = Folds::new();
        // Hide lines 3 to 5.
        let from = t.line_span(t.line_to_byte(2)).end;
        let to = t.line_span(t.line_to_byte(5)).end;
        folds.fold(from..to);
        let mut out = Vec::new();
        window(&t, &folds, 0, 0, 6, &mut out);
        assert_eq!(lines_of(&t, &out), vec![0, 1, 2, 6, 7, 8]);
    }

    #[test]
    fn blocks_are_runs_of_one_colour_between_blanks() {
        let ui = Ui::default();
        let mut out = Vec::new();
        measure("ab  c\td", 0, (80, 4), (&[], 0), (&ui, None), &mut out);
        let cols: Vec<(u16, u16)> = out.iter().map(|b| (b.col, b.len)).collect();
        // `ab` at 0, `c` at 4, the tab to column 8, `d` there.
        assert_eq!(cols, vec![(0, 2), (4, 1), (8, 1)]);
        out.clear();
        measure("abcdef", 0, (3, 4), (&[], 0), (&ui, None), &mut out);
        assert_eq!(out[0].len, 3, "stops at the column limit");
    }

    #[test]
    fn a_window_of_a_huge_text_reads_only_its_lines() {
        // The window is found by walking lines from the view, never from
        // the start: a view deep in a long text costs the same.
        let text = numbered(200_000, "");
        let t = RopeBuffer::from_text(&text);
        let folds = Folds::new();
        let mut out = Vec::new();
        let started = std::time::Instant::now();
        for _ in 0..100 {
            window(&t, &folds, t.line_to_byte(150_000), 500, 1000, &mut out);
        }
        assert_eq!(out.len(), 1000);
        assert_eq!(t.byte_to_line(out[0]), 149_500);
        assert!(started.elapsed().as_secs() < 5);
    }
}
