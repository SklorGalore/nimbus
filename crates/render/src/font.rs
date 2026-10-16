//! The bundled editor font and its metrics at a given pixel size.
//!
//! The font file is compiled into the binary with `include_bytes!`, so the
//! first frame never waits on disk or on a scan of system fonts. Parsing is
//! lazy inside swash: creating a [`Font`] only checks the file's table
//! directory, which takes microseconds.
//!
//! The font is Hack Nerd Font Mono (MIT and Bitstream Vera licences, see
//! `assets/HackNerdFont-LICENSE.md`). The "Mono" build of a Nerd Font scales
//! every icon to one cell, which the editor's monospace grid relies on.

use swash::FontRef;

/// Raw bytes of the bundled font.
static BUNDLED: &[u8] = include_bytes!("../assets/HackNerdFontMono-Regular.ttf");

/// Raw bytes of its bold weight, for themes that make captures bold. Italic
/// is not bundled: Hack's italic is a slanted regular, which the
/// rasterizer makes by slanting the outlines (`raster`), at no size cost.
///
/// The bold file has the Nerd Font icons taken out (subset with fonttools'
/// `pyftsubset --unicodes="U+0000-DFFF,U+F900-FFFF"`, hinting dropped since
/// Nimbus does not hint): 185 KB instead of 2.8 MB, with every other
/// character kept. An icon in a bold run is drawn from the regular face.
static BUNDLED_BOLD: &[u8] = include_bytes!("../assets/HackNerdFontMono-Bold-NoIcons.ttf");

/// A font face the renderer can shape and rasterize with.
///
/// Cheap to copy: it is a reference into font bytes plus a cache key that
/// swash uses to reuse per-font work across calls.
#[derive(Clone, Copy)]
pub struct Font {
    face: FontRef<'static>,
}

impl std::fmt::Debug for Font {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Font")
            .field("bytes", &self.face.data.len())
            .finish_non_exhaustive()
    }
}

/// The bundled font file could not be read as a font.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FontError;

impl std::fmt::Display for FontError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "bundled font data is not a valid font")
    }
}

impl std::error::Error for FontError {}

/// Vertical and horizontal measurements for laying out a monospace grid.
///
/// All values are in physical pixels. `line_height` and `baseline` are whole
/// numbers so every line's baseline lands exactly on a pixel row, which keeps
/// text crisp; fractional baselines blur glyphs vertically.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Metrics {
    /// The size glyphs are rasterized at, in pixels per em.
    pub size_px: f32,
    /// Height of one line of text, rounded up to whole pixels.
    pub line_height: f32,
    /// Distance from the top of a line to its baseline, in whole pixels.
    pub baseline: f32,
    /// Width of one character cell: the advance of the digit zero. Not
    /// rounded, because rounding would accumulate error across a long line.
    pub cell_width: f32,
}

impl Font {
    /// The font compiled into the binary.
    ///
    /// # Errors
    ///
    /// Returns [`FontError`] if the bundled bytes are not a font, which would
    /// mean the asset file was replaced with something broken.
    pub fn bundled() -> Result<Self, FontError> {
        FontRef::from_index(BUNDLED, 0)
            .map(|face| Self { face })
            .ok_or(FontError)
    }

    /// A font from `data`, face `index` of it (a `.ttc` file holds several).
    /// For fallback fonts read from the system, whose bytes live for the
    /// rest of the session.
    #[must_use]
    pub fn from_data(data: &'static [u8], index: u32) -> Option<Self> {
        FontRef::from_index(data, usize::try_from(index).ok()?).map(|face| Self { face })
    }

    /// The bold weight of the bundled font.
    ///
    /// # Errors
    ///
    /// As for [`Font::bundled`].
    pub fn bundled_bold() -> Result<Self, FontError> {
        FontRef::from_index(BUNDLED_BOLD, 0)
            .map(|face| Self { face })
            .ok_or(FontError)
    }

    /// The swash font reference, for shaping and rasterizing.
    #[must_use]
    pub fn face(&self) -> FontRef<'static> {
        self.face
    }

    /// Glyph id for a character, or 0 (the font's "missing glyph" box) if
    /// the font has no glyph for it.
    #[must_use]
    pub fn glyph_id(&self, ch: char) -> u16 {
        self.face.charmap().map(ch)
    }

    /// Grid measurements at `size_px` pixels per em.
    ///
    /// Callers convert a logical point size to pixels by multiplying by the
    /// window's scale factor before calling this.
    #[must_use]
    pub fn metrics(&self, size_px: f32) -> Metrics {
        let m = self.face.metrics(&[]).scale(size_px);
        let line_height = (m.ascent + m.descent + m.leading).ceil();
        // Centre any leading around the glyphs, then snap to a pixel row.
        let baseline = (m.leading / 2.0 + m.ascent).round();
        let zero = self.glyph_id('0');
        let cell_width = self
            .face
            .glyph_metrics(&[])
            .scale(size_px)
            .advance_width(zero);
        Metrics {
            size_px,
            line_height,
            baseline,
            cell_width,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_font_loads_and_is_monospace() {
        let font = Font::bundled().unwrap();
        assert!(font.face().metrics(&[]).is_monospace);
    }

    #[test]
    fn metrics_are_sane_and_pixel_aligned() {
        let font = Font::bundled().unwrap();
        for size in [11.0, 13.0, 14.5, 26.0, 29.0] {
            let m = font.metrics(size);
            assert!(m.line_height >= size, "{m:?}");
            assert!(m.line_height.fract().abs() < f32::EPSILON, "{m:?}");
            assert!(m.baseline.fract().abs() < f32::EPSILON, "{m:?}");
            assert!(m.baseline > 0.0 && m.baseline < m.line_height, "{m:?}");
            assert!(m.cell_width > size * 0.4 && m.cell_width < size, "{m:?}");
        }
    }

    #[test]
    fn every_ascii_glyph_has_the_cell_width() {
        let font = Font::bundled().unwrap();
        let m = font.metrics(14.0);
        let gm = font.face().glyph_metrics(&[]).scale(14.0);
        for ch in ' '..='~' {
            let id = font.glyph_id(ch);
            assert_ne!(id, 0, "missing {ch:?}");
            assert!((gm.advance_width(id) - m.cell_width).abs() < 0.01, "{ch:?}");
        }
    }

    #[test]
    #[ignore = "diagnostic: prints which UI symbols the bundled font covers"]
    fn report_symbol_coverage() {
        let font = Font::bundled().unwrap();
        for ch in [
            '⌘', '⌥', '⌃', '⇧', '↩', '⇥', '⎋', '⌫', '⌦', '←', '→', '↑', '↓', '●', '…', '×',
        ] {
            println!(
                "{ch} U+{:04X}: {}",
                ch as u32,
                if font.glyph_id(ch) == 0 {
                    "MISSING"
                } else {
                    "ok"
                }
            );
        }
    }

    #[test]
    fn bold_has_the_same_cell_width() {
        let regular = Font::bundled().unwrap().metrics(14.0);
        let bold = Font::bundled_bold().unwrap().metrics(14.0);
        assert!((regular.cell_width - bold.cell_width).abs() < 0.01);
        assert!((regular.line_height - bold.line_height).abs() < f32::EPSILON);
    }

    #[test]
    fn nerd_font_icons_are_one_cell() {
        let font = Font::bundled().unwrap();
        let m = font.metrics(14.0);
        let gm = font.face().glyph_metrics(&[]).scale(14.0);
        // U+E0A0 is the powerline branch symbol, U+F07B a folder icon.
        for ch in ['\u{e0a0}', '\u{f07b}'] {
            let id = font.glyph_id(ch);
            assert_ne!(id, 0, "missing {ch:?}");
            assert!((gm.advance_width(id) - m.cell_width).abs() < 0.01, "{ch:?}");
        }
    }
}
