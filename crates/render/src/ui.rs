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

/// How a run of interface text is drawn: its size relative to the editor
/// font's, and its weight and slant. The Markdown preview (M20) uses it for
/// headings, bold and italic; the rest of the interface draws
/// [`UiStyle::PLAIN`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct UiStyle {
    /// Size as a multiple of the editor font's.
    pub scale: f32,
    /// Drawn from the bold face, when the font has one.
    pub bold: bool,
    /// Slanted.
    pub italic: bool,
}

impl UiStyle {
    /// The editor font's size, regular and upright.
    pub const PLAIN: Self = Self {
        scale: 1.0,
        bold: false,
        italic: false,
    };
}

/// How interface text is measured and placed: a proportional font, or the
/// editor's grid. Cheap to copy, so a layout can carry one and measure
/// without the GPU, or hand it to a worker thread.
#[derive(Debug, Clone, Copy)]
pub struct UiFont {
    /// The proportional font, if one is configured and loaded.
    font: Option<Font>,
    /// Its bold weight, if there is one; bold text is otherwise drawn from
    /// the regular weight.
    bold: Option<Font>,
    /// The editor font's size and grid, for the grid case and for
    /// characters the interface font lacks.
    editor: Metrics,
    /// From the top of an editor line to the interface font's baseline:
    /// its glyphs are centred in the editor's line height, so rows keep
    /// their height whichever font draws them.
    baseline: f32,
    /// The same before rounding, to scale for larger text.
    exact_baseline: f32,
}

impl UiFont {
    /// Interface text on the editor's grid: no proportional font.
    #[must_use]
    pub fn grid(editor: Metrics) -> Self {
        Self {
            font: None,
            bold: None,
            editor,
            baseline: editor.baseline,
            exact_baseline: editor.baseline,
        }
    }

    /// Interface text in `font`, at the editor font's size.
    #[must_use]
    pub fn new(font: Font, editor: Metrics) -> Self {
        let m = font.face().metrics(&[]).scale(editor.size_px);
        let own = m.ascent + m.descent;
        let exact = (editor.line_height - own) / 2.0 + m.ascent;
        Self {
            font: Some(font),
            bold: None,
            editor,
            baseline: exact.round(),
            exact_baseline: exact,
        }
    }

    /// The same, with `bold` as the bold weight.
    #[must_use]
    pub fn with_bold(self, bold: Option<Font>) -> Self {
        Self { bold, ..self }
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

    /// From the top of a line of text in `style` to its baseline.
    #[must_use]
    pub(crate) fn styled_baseline(&self, style: UiStyle) -> f32 {
        if (style.scale - 1.0).abs() < f32::EPSILON {
            self.baseline
        } else {
            (self.exact_baseline * style.scale).round()
        }
    }

    /// From the top of a line of text in `style` to where its underline
    /// goes: a little below the baseline.
    #[must_use]
    pub fn underline(&self, style: UiStyle) -> f32 {
        self.styled_baseline(style) + (self.editor.size_px * style.scale * 0.12).round().max(1.0)
    }

    /// The height of a line of text in `style`: the editor's line height,
    /// scaled.
    #[must_use]
    pub fn line_height(&self, style: UiStyle) -> f32 {
        (self.editor.line_height * style.scale).round()
    }

    /// The width of `text` in `style`, as [`crate::Gpu::push_styled_text`]
    /// draws it.
    #[must_use]
    pub fn styled_width(&self, text: &str, style: UiStyle) -> f32 {
        let mut w = 0.0;
        let mut col = 0;
        for g in text.graphemes(true) {
            let (adv, cols, _) = self.styled_advance(g, col, style);
            w += adv;
            col += cols;
        }
        w
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
        let (adv, cols, _) = self.styled_advance(g, col, UiStyle::PLAIN);
        (adv, cols)
    }

    /// As [`Self::advance`] for text in `style`, with the font that draws
    /// the grapheme and whether that is the bold weight: `None` when
    /// neither weight has it and the editor font draws it in its cells.
    pub(crate) fn styled_advance(
        &self,
        g: &str,
        col: usize,
        style: UiStyle,
    ) -> (f32, usize, Option<(Font, bool)>) {
        let cols = column::grapheme_cols(g, col, 4);
        let pick = match (self.font, self.bold) {
            (_, Some(bold)) if style.bold && Self::has(bold, g) => Some((bold, true)),
            (Some(font), _) if Self::has(font, g) => Some((font, false)),
            _ => None,
        };
        match pick {
            Some((font, is_bold)) => {
                let size = self.editor.size_px * style.scale;
                let scaled = font.face().glyph_metrics(&[]).scale(size);
                let adv = g
                    .chars()
                    .map(|c| font.glyph_id(c))
                    .filter(|&id| id != 0)
                    .map(|id| scaled.advance_width(id))
                    .sum();
                (adv, cols, Some((font, is_bold)))
            }
            None => (cell_count(cols) * self.editor.cell_width, cols, None),
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

    #[test]
    fn larger_styles_measure_wider_and_taller() {
        let ui = UiFont::new(Font::bundled().unwrap(), metrics());
        let big = UiStyle {
            scale: 2.0,
            ..UiStyle::PLAIN
        };
        let plain = ui.styled_width("abc", UiStyle::PLAIN);
        assert!((ui.styled_width("abc", big) - 2.0 * plain).abs() < 1e-2);
        assert!(ui.line_height(big) > ui.line_height(UiStyle::PLAIN));
        assert!(ui.styled_baseline(big) > ui.styled_baseline(UiStyle::PLAIN));
    }
}
