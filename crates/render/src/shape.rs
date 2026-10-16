//! Shaping: turning a line of text into positioned glyphs.
//!
//! Shaping is the step between "a string" and "pictures on screen". The
//! shaper looks up which glyph the font uses for each character, applies the
//! font's rules for combining marks and substitutions, and says how far to
//! advance after each glyph. Its output is glyph ids with positions; turning
//! a glyph id into a bitmap is a separate step, rasterizing (see `raster`).
//!
//! The editor is a monospace grid, so after shaping, each *grapheme* (what a
//! reader sees as one character) is snapped to its column from
//! [`nimbus_core::column`], the same function the cursor uses. That keeps the
//! cursor and the text aligned even where the font disagrees: CJK characters
//! are two columns wide but this font has no CJK glyphs, and its combining
//! marks advance a full cell.

use nimbus_core::column;
use swash::shape::ShapeContext;
use unicode_segmentation::UnicodeSegmentation;

use crate::font::Font;

/// One positioned glyph in a shaped line.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ShapedGlyph {
    /// Glyph id in the font. 0 is the font's "missing glyph" box.
    pub id: u16,
    /// Horizontal position in pixels from the start of the line.
    pub x: f32,
    /// Vertical offset from the baseline in pixels, positive up. Zero for
    /// most glyphs; non-zero for marks the font positions above or below.
    pub y: f32,
    /// Byte offset in the line of the grapheme this glyph belongs to.
    pub cluster: u32,
}

/// Grid parameters for laying out one line.
#[derive(Debug, Clone, Copy)]
pub struct Grid {
    /// Width of one column in pixels.
    pub cell_width: f32,
    /// Columns per tab stop.
    pub tab_width: usize,
    /// Column at which the shaped text starts within its line. Non-zero
    /// when only the visible part of a long line is shaped; tab stops are
    /// still measured from the start of the line. Glyph positions stay
    /// relative to the start of the shaped text.
    pub start_col: usize,
}

/// A raw shaper glyph before grid placement.
#[derive(Debug, Clone, Copy)]
struct Raw {
    id: u16,
    x: f32,
    y: f32,
    cluster: u32,
}

/// Reusable shaping state.
///
/// Holds swash's shaping cache and scratch space so that shaping a line
/// allocates nothing once the buffers have grown to fit typical lines.
pub struct Shaper {
    ctx: ShapeContext,
    scratch_text: String,
    raw: Vec<Raw>,
}

impl std::fmt::Debug for Shaper {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Shaper").finish_non_exhaustive()
    }
}

impl Default for Shaper {
    fn default() -> Self {
        Self::new()
    }
}

#[expect(
    clippy::cast_precision_loss,
    reason = "column counts are far below 2^24, exact in f32"
)]
fn cols(n: usize) -> f32 {
    n as f32
}

impl Shaper {
    /// Empty shaping state.
    #[must_use]
    pub fn new() -> Self {
        Self {
            ctx: ShapeContext::new(),
            scratch_text: String::new(),
            raw: Vec::new(),
        }
    }

    /// Shape `line` with `font` at `size_px` and place each grapheme on the
    /// column grid, writing glyphs into `out` (cleared first).
    ///
    /// `line` must not contain a newline. Tabs produce no glyphs; they only
    /// move later graphemes to the next tab stop. A grapheme containing any
    /// glyph the font lacks draws as a single missing-glyph box centred in
    /// its columns.
    pub fn shape_line(
        &mut self,
        font: &Font,
        size_px: f32,
        line: &str,
        grid: Grid,
        out: &mut Vec<ShapedGlyph>,
    ) {
        out.clear();
        if line.is_empty() {
            return;
        }
        self.shape_raw(font, size_px, line);

        let cell = grid.cell_width;
        let mut col = 0usize;
        let base_col = grid.start_col;
        let mut j = 0usize;
        for (start, g) in line.grapheme_indices(true) {
            let end = start + g.len();
            let w = column::grapheme_cols(g, base_col + col, grid.tab_width);
            let first = j;
            while j < self.raw.len() && (self.raw[j].cluster as usize) < end {
                j += 1;
            }
            let glyphs = &self.raw[first..j];
            let x0 = cols(col) * cell;
            if g == "\t" || glyphs.is_empty() {
                // Nothing to draw.
            } else if glyphs.iter().any(|r| r.id == 0) {
                // The font lacks part of this grapheme: one box, centred.
                // The missing-glyph box is one cell wide in this font.
                out.push(ShapedGlyph {
                    id: 0,
                    x: x0 + (cols(w) - 1.0) * cell / 2.0,
                    y: 0.0,
                    cluster: glyphs[0].cluster,
                });
            } else {
                let base = glyphs[0].x;
                for r in glyphs {
                    out.push(ShapedGlyph {
                        id: r.id,
                        x: x0 + (r.x - base),
                        y: r.y,
                        cluster: r.cluster,
                    });
                }
            }
            col += w;
        }
    }

    /// Run swash over `line` with tabs replaced by spaces (same byte length,
    /// so cluster offsets still match `line`), filling `self.raw`.
    fn shape_raw(&mut self, font: &Font, size_px: f32, line: &str) {
        self.raw.clear();
        let text = if line.contains('\t') {
            self.scratch_text.clear();
            self.scratch_text
                .extend(line.chars().map(|c| if c == '\t' { ' ' } else { c }));
            self.scratch_text.as_str()
        } else {
            line
        };
        let mut shaper = self.ctx.builder(font.face()).size(size_px).build();
        shaper.add_str(text);
        let mut pen = 0.0_f32;
        let raw = &mut self.raw;
        shaper.shape_with(|cluster| {
            for glyph in cluster.glyphs {
                raw.push(Raw {
                    id: glyph.id,
                    x: pen + glyph.x,
                    y: glyph.y,
                    cluster: cluster.source.start,
                });
                pen += glyph.advance;
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shape(line: &str) -> (Vec<ShapedGlyph>, f32) {
        let font = Font::bundled().unwrap();
        let cell = font.metrics(14.0).cell_width;
        let mut out = Vec::new();
        Shaper::new().shape_line(
            &font,
            14.0,
            line,
            Grid {
                cell_width: cell,
                tab_width: 4,
                start_col: 0,
            },
            &mut out,
        );
        (out, cell)
    }

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 0.01
    }

    #[test]
    fn ascii_glyphs_sit_on_the_cell_grid() {
        let (out, cell) = shape("fn main() { let x = 1; }");
        assert_eq!(out.len(), 24);
        for (i, g) in (0u16..).zip(out.iter()) {
            assert!(close(g.x, cell * f32::from(i)), "glyph {i}: {}", g.x);
            assert_eq!(g.cluster, u32::from(i));
        }
    }

    #[test]
    fn tabs_draw_nothing_and_reach_the_next_stop() {
        let (out, cell) = shape("a\tb");
        assert_eq!(out.len(), 2);
        assert!(out.iter().all(|g| g.id != 0));
        assert!(close(out[1].x, 4.0 * cell));
        let (out, cell) = shape("\tx");
        assert!(close(out[0].x, 4.0 * cell));
    }

    #[test]
    fn combining_mark_does_not_push_the_next_grapheme() {
        let (out, cell) = shape("e\u{301}x");
        let x = out.last().unwrap();
        assert!(close(x.x, cell), "x at {}", x.x);
    }

    #[test]
    fn cjk_is_one_box_in_two_cells() {
        let (out, cell) = shape("日x");
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].id, 0);
        assert!(close(out[0].x, 0.5 * cell));
        assert!(close(out[1].x, 2.0 * cell));
    }

    #[test]
    fn tab_after_wide_chars_uses_display_columns() {
        let (out, cell) = shape("日日\tx");
        assert!(close(out.last().unwrap().x, 8.0 * cell));
    }

    #[test]
    fn zwj_emoji_is_one_box() {
        let (out, cell) = shape("👨‍👩‍👧x");
        assert_eq!(out.iter().filter(|g| g.id == 0).count(), 1);
        assert!(close(out.last().unwrap().x, 2.0 * cell));
    }

    #[test]
    fn tab_stops_count_from_the_line_start_when_shaping_a_span() {
        let font = Font::bundled().unwrap();
        let cell = font.metrics(14.0).cell_width;
        let mut out = Vec::new();
        // The span starts at column 2, so its tab reaches column 4: the x
        // after it is 2 columns from the start of the span.
        let grid = Grid {
            cell_width: cell,
            tab_width: 4,
            start_col: 2,
        };
        Shaper::new().shape_line(&font, 14.0, "\tx", grid, &mut out);
        assert!(close(out[0].x, 2.0 * cell));
    }

    #[test]
    fn reshaping_reuses_the_output_buffer() {
        let font = Font::bundled().unwrap();
        let grid = Grid {
            cell_width: font.metrics(14.0).cell_width,
            tab_width: 4,
            start_col: 0,
        };
        let mut shaper = Shaper::new();
        let mut out = Vec::new();
        shaper.shape_line(&font, 14.0, "a long enough line of text", grid, &mut out);
        let cap = out.capacity();
        shaper.shape_line(&font, 14.0, "short", grid, &mut out);
        assert_eq!(out.len(), 5);
        assert_eq!(out.capacity(), cap);
    }
}
