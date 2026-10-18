//! Proportional text for the interface (M20): the sidebar, tabs, status
//! bar, palette and pickers, in a font such as Inter or SF Pro, while the
//! editor area stays on its monospace grid.
//!
//! **Shaping with real advances.** The editor places every character on a
//! column grid, so a string's width is its column count times the cell
//! width, and a click maps to a column by division. A proportional font
//! gives each glyph its own advance: an `i` is narrow, an `M` wide. So
//! widths are *measured*, by adding up the advances the font gives, and
//! anything placed after text (a tab's dot, a picker row's detail) is
//! placed by that measure rather than by counting characters. Measuring
//! and drawing walk the text the same way, so what is measured is what is
//! drawn.
//!
//! A character the interface font lacks (the file-type icons of the
//! bundled Nerd Font, CJK in a Latin-only font) is drawn by the editor
//! font in its cells, as the editor would draw it, and measured as those
//! cells.
//!
//! Without `ui_font` in the config there is no interface font: measuring
//! counts columns and drawing is the editor's, exactly as before, and the
//! first frame looks up no fonts.

use nimbus_core::column;
use unicode_segmentation::UnicodeSegmentation;

use crate::font::{Font, Metrics};

/// How interface text is measured and placed: a proportional font, or the
/// editor's grid. Cheap to copy, so a layout can carry one and measure
/// without the GPU.
#[derive(Debug, Clone, Copy)]
pub struct UiFont {
    /// The proportional font, if one is configured and loaded.
    font: Option<Font>,
    /// The editor font's size and grid, for the grid case and for
    /// characters the interface font lacks.
    editor: Metrics,
    /// From the top of an editor line to the interface font's baseline:
    /// its glyphs are centred in the editor's line height, so rows keep
    /// their height whichever font draws them.
    baseline: f32,
}

impl UiFont {
    /// Interface text on the editor's grid: no proportional font.
    #[must_use]
    pub fn grid(editor: Metrics) -> Self {
        Self {
            font: None,
            editor,
            baseline: editor.baseline,
        }
    }

    /// Interface text in `font`, at the editor font's size.
    #[must_use]
    pub fn new(font: Font, editor: Metrics) -> Self {
        let m = font.face().metrics(&[]).scale(editor.size_px);
        let own = m.ascent + m.descent;
        let baseline = ((editor.line_height - own) / 2.0 + m.ascent).round();
        Self {
            font: Some(font),
            editor,
            baseline,
        }
    }

    /// Whether a proportional font draws the interface.
    #[must_use]
    pub fn is_proportional(&self) -> bool {
        self.font.is_some()
    }

    /// The proportional font, if any.
    #[must_use]
    pub(crate) fn font(&self) -> Option<Font> {
        self.font
    }

    /// The editor font's grid measurements.
    #[must_use]
    pub fn editor(&self) -> Metrics {
        self.editor
    }

    /// From the top of a line to the interface font's baseline.
    #[must_use]
    pub(crate) fn baseline(&self) -> f32 {
        self.baseline
    }

    /// The width of `text` in pixels as [`crate::Gpu::push_ui_text`]
    /// draws it.
    #[must_use]
    pub fn width(&self, text: &str) -> f32 {
        let mut w = 0.0;
        let mut col = 0;
        for g in text.graphemes(true) {
            let (adv, cols) = self.advance(g, col);
            w += adv;
            col += cols;
        }
        w
    }

    /// How many bytes of `text` fit in `room` pixels, ending on a
    /// character boundary: for cutting a long label short.
    #[must_use]
    pub fn fit(&self, text: &str, room: f32) -> usize {
        let mut w = 0.0;
        let mut col = 0;
        for (i, g) in text.grapheme_indices(true) {
            let (adv, cols) = self.advance(g, col);
            if w + adv > room {
                return i;
            }
            w += adv;
            col += cols;
        }
        text.len()
    }

    /// The byte of `text` under `x` pixels from its start: the nearer
    /// edge of the grapheme there.
    #[must_use]
    pub fn byte_at(&self, text: &str, x: f32) -> usize {
        let mut w = 0.0;
        let mut col = 0;
        for (i, g) in text.grapheme_indices(true) {
            let (adv, cols) = self.advance(g, col);
            if x < w + adv / 2.0 {
                return i;
            }
            w += adv;
            col += cols;
        }
        text.len()
    }

    /// One grapheme's advance in pixels and its width in columns (for tab
    /// stops after it), starting at column `col`.
    pub(crate) fn advance(&self, g: &str, col: usize) -> (f32, usize) {
        let cols = column::grapheme_cols(g, col, 4);
        match self.font {
            Some(font) if Self::has(font, g) => {
                let scaled = font.face().glyph_metrics(&[]).scale(self.editor.size_px);
                let adv = g
                    .chars()
                    .map(|c| font.glyph_id(c))
                    .filter(|&id| id != 0)
                    .map(|id| scaled.advance_width(id))
                    .sum();
                (adv, cols)
            }
            _ => (cell_count(cols) * self.editor.cell_width, cols),
        }
    }

    /// Whether `font` draws grapheme `g`: it has its first character.
    pub(crate) fn has(font: Font, g: &str) -> bool {
        g.chars()
            .next()
            .is_some_and(|c| c != '\t' && font.glyph_id(c) != 0)
    }
}

#[expect(clippy::cast_precision_loss, reason = "a few columns")]
fn cell_count(n: usize) -> f32 {
    n as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metrics() -> Metrics {
        Font::bundled().unwrap().metrics(20.0)
    }

    #[test]
    fn the_grid_measures_columns() {
        let ui = UiFont::grid(metrics());
        let cell = metrics().cell_width;
        assert!((ui.width("abc") - 3.0 * cell).abs() < 1e-3);
        assert!((ui.width("漢") - 2.0 * cell).abs() < 1e-3, "two columns");
        assert_eq!(ui.fit("abcdef", 3.5 * cell), 3);
        assert_eq!(ui.byte_at("abcdef", 1.6 * cell), 2);
        assert!(!ui.is_proportional());
    }

    #[test]
    fn a_font_measures_its_own_advances() {
        // The bundled font stands in for a proportional one: what matters
        // is that widths come from the font's advances, and characters it
        // lacks take the editor's cells.
        let font = Font::bundled().unwrap();
        let m = metrics();
        let ui = UiFont::new(font, m);
        assert!(ui.is_proportional());
        let adv = font
            .face()
            .glyph_metrics(&[])
            .scale(m.size_px)
            .advance_width(font.glyph_id('a'));
        assert!((ui.width("aa") - 2.0 * adv).abs() < 1e-3);
        let with_cjk = ui.width("a漢");
        assert!((with_cjk - (adv + 2.0 * m.cell_width)).abs() < 1e-3);
        assert_eq!(ui.fit("aaaa", 2.5 * adv), 2);
    }
}
