//! Shaping: turning a line of text into positioned glyphs.
//!
//! Shaping is the step between "a string" and "pictures on screen". The
//! shaper looks up which glyph the font uses for each character, applies the
//! font's rules for combining marks and substitutions, and says how far to
//! advance after each glyph. Its output is glyph ids with x positions; turning
//! a glyph id into a bitmap is a separate step, rasterizing (see `atlas`).

use swash::shape::ShapeContext;

use crate::font::Font;

/// One positioned glyph in a shaped line.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ShapedGlyph {
    /// Glyph id in the font. 0 is the font's "missing glyph" box.
    pub id: u16,
    /// Horizontal pen position in pixels from the start of the line.
    pub x: f32,
    /// Vertical offset from the baseline in pixels, positive up. Zero for
    /// most glyphs; non-zero for marks the font positions above or below.
    pub y: f32,
    /// Byte offset of the first character this glyph came from, in the
    /// tab-expanded text that was shaped.
    pub cluster: u32,
}

/// Reusable shaping state.
///
/// Holds swash's shaping cache and scratch space so that shaping a line
/// allocates nothing once the buffers have grown to fit typical lines.
pub struct Shaper {
    ctx: ShapeContext,
    expanded: String,
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

impl Shaper {
    /// Empty shaping state.
    #[must_use]
    pub fn new() -> Self {
        Self {
            ctx: ShapeContext::new(),
            expanded: String::new(),
        }
    }

    /// Shape `line` with `font` at `size_px`, writing glyphs into `out`
    /// (cleared first).
    ///
    /// Tabs are replaced by spaces up to the next multiple of `tab_width`
    /// columns before shaping, because fonts give tab no useful advance.
    /// `line` must not contain a newline. Column counting here treats every
    /// `char` as one column, which is right for the M1 sample text; M2 moves
    /// column rules into `nimbus-core` with proper width handling.
    pub fn shape_line(
        &mut self,
        font: &Font,
        size_px: f32,
        line: &str,
        tab_width: u32,
        out: &mut Vec<ShapedGlyph>,
    ) {
        out.clear();
        let text = if line.contains('\t') {
            expand_tabs(line, tab_width, &mut self.expanded);
            self.expanded.as_str()
        } else {
            line
        };
        if text.is_empty() {
            return;
        }

        let mut shaper = self.ctx.builder(font.face()).size(size_px).build();
        shaper.add_str(text);
        let mut pen = 0.0_f32;
        shaper.shape_with(|cluster| {
            for glyph in cluster.glyphs {
                out.push(ShapedGlyph {
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

/// Write `line` into `out` with each tab replaced by enough spaces to reach
/// the next multiple of `tab_width` columns.
fn expand_tabs(line: &str, tab_width: u32, out: &mut String) {
    out.clear();
    let tab_width = tab_width.max(1) as usize;
    let mut col = 0usize;
    for ch in line.chars() {
        if ch == '\t' {
            let spaces = tab_width - col % tab_width;
            out.extend(std::iter::repeat_n(' ', spaces));
            col += spaces;
        } else {
            out.push(ch);
            col += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tabs_expand_to_the_next_stop() {
        let mut s = String::new();
        expand_tabs("\tab\tc", 4, &mut s);
        assert_eq!(s, "    ab  c");
        expand_tabs("abcd\te", 4, &mut s);
        assert_eq!(s, "abcd    e");
    }

    #[test]
    fn ascii_glyphs_sit_on_the_cell_grid() {
        let font = Font::bundled().unwrap();
        let m = font.metrics(14.0);
        let mut shaper = Shaper::new();
        let mut out = Vec::new();
        shaper.shape_line(&font, 14.0, "fn main() { let x = 1; }", 4, &mut out);
        assert_eq!(out.len(), 24);
        for (i, g) in (0u16..).zip(out.iter()) {
            let expected = m.cell_width * f32::from(i);
            assert!(
                (g.x - expected).abs() < 0.01,
                "glyph {i}: {} vs {expected}",
                g.x
            );
            assert_eq!(g.cluster, u32::from(i));
        }
    }

    #[test]
    fn tab_indented_line_starts_after_the_tab_stop() {
        let font = Font::bundled().unwrap();
        let m = font.metrics(14.0);
        let mut shaper = Shaper::new();
        let mut out = Vec::new();
        shaper.shape_line(&font, 14.0, "\tx", 4, &mut out);
        let x = out.last().unwrap();
        assert!((x.x - 4.0 * m.cell_width).abs() < 0.01);
    }

    #[test]
    fn missing_glyphs_come_back_as_id_zero() {
        let font = Font::bundled().unwrap();
        let mut shaper = Shaper::new();
        let mut out = Vec::new();
        // A Linear B syllable: no coding font covers it.
        shaper.shape_line(&font, 14.0, "\u{10000}", 4, &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].id, 0);
    }

    #[test]
    fn reshaping_reuses_the_output_buffer() {
        let font = Font::bundled().unwrap();
        let mut shaper = Shaper::new();
        let mut out = Vec::new();
        shaper.shape_line(&font, 14.0, "a long enough line of text", 4, &mut out);
        let cap = out.capacity();
        shaper.shape_line(&font, 14.0, "short", 4, &mut out);
        assert_eq!(out.len(), 5);
        assert_eq!(out.capacity(), cap);
    }
}
