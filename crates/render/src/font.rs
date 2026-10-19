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
/// swash uses to reuse per-font work across calls, and, for a variable
/// font, where on its axes to draw it.
///
/// **Variable fonts (M24).** A variable font is one file holding a
/// continuous range of designs along *axes*: weight (`wght`, 100 thin to
/// 900 black), width, slant. Drawing it at weight 450 means asking the
/// shaper and the rasterizer for that point on the axis; the outlines are
/// interpolated between the masters the file stores. The point is kept as
/// *normalized coordinates* (each axis mapped to -1..1, as fixed-point
/// `i16`), which swash takes for shaping, metrics and outlines alike.
/// [`Font::with_weight`] sets it.
#[derive(Clone, Copy)]
pub struct Font {
    face: FontRef<'static>,
    /// The font has colour glyphs (bitmap strikes or layered outlines),
    /// such as an emoji font; worked out once when it is opened.
    color: bool,
    /// Normalized coordinates on the font's variation axes, the first
    /// `axes` of them in use; empty for a static font or a variable one
    /// at its default instance.
    coords: [i16; MAX_AXES],
    axes: u8,
    /// The weight asked for with [`Font::with_weight`], if the font could
    /// take it.
    weight: Option<f32>,
}

/// Variation axes kept per font. Fonts rarely have more than four (weight,
/// width, slant, optical size); one with its weight axis past this many
/// draws at its default instance.
const MAX_AXES: usize = 8;

/// The tag of the weight axis.
const WGHT: [u8; 4] = *b"wght";

/// Tables that hold colour glyphs: `CBDT` (Google's bitmaps, Noto Color
/// Emoji), `sbix` (Apple's bitmaps, Apple Color Emoji) and `COLR` (layered
/// outlines, Segoe UI Emoji).
const COLOR_TABLES: [&[u8; 4]; 3] = [b"CBDT", b"sbix", b"COLR"];

fn wrap(face: FontRef<'static>) -> Font {
    let color = COLOR_TABLES
        .iter()
        .any(|t| face.table(swash::tag_from_bytes(t)).is_some());
    Font {
        face,
        color,
        coords: [0; MAX_AXES],
        axes: 0,
        weight: None,
    }
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
        FontRef::from_index(BUNDLED, 0).map(wrap).ok_or(FontError)
    }

    /// A font from `data`, face `index` of it (a `.ttc` file holds several).
    /// For fallback fonts read from the system, whose bytes live for the
    /// rest of the session.
    #[must_use]
    pub fn from_data(data: &'static [u8], index: u32) -> Option<Self> {
        FontRef::from_index(data, usize::try_from(index).ok()?).map(wrap)
    }

    /// The bold weight of the bundled font.
    ///
    /// # Errors
    ///
    /// As for [`Font::bundled`].
    pub fn bundled_bold() -> Result<Self, FontError> {
        FontRef::from_index(BUNDLED_BOLD, 0)
            .map(wrap)
            .ok_or(FontError)
    }

    /// Whether the font has colour glyphs (an emoji font).
    #[must_use]
    pub fn has_color(&self) -> bool {
        self.color
    }

    /// The swash font reference, for shaping and rasterizing.
    #[must_use]
    pub fn face(&self) -> FontRef<'static> {
        self.face
    }

    /// Whether the font is variable along weight: one file for every
    /// weight, drawn at the one [`Font::with_weight`] sets.
    #[must_use]
    pub fn is_variable(&self) -> bool {
        self.face
            .variations()
            .find_by_tag(swash::tag_from_bytes(&WGHT))
            .is_some()
    }

    /// The weights a variable font can draw at, lowest to highest, with
    /// its default; `None` for a static font.
    #[must_use]
    pub fn weight_range(&self) -> Option<(f32, f32, f32)> {
        let axis = self
            .face
            .variations()
            .find_by_tag(swash::tag_from_bytes(&WGHT))?;
        Some((axis.min_value(), axis.default_value(), axis.max_value()))
    }

    /// The same font drawn at `weight` (100 thin, 400 regular, 700 bold,
    /// 900 black), clamped to what it can do, if it is variable along
    /// weight. A static font comes back unchanged: its weight is the one
    /// in its file.
    #[must_use]
    pub fn with_weight(mut self, weight: f32) -> Self {
        let tag = swash::tag_from_bytes(&WGHT);
        let vars = self.face.variations();
        if vars.find_by_tag(tag).is_none() || vars.count() > MAX_AXES {
            return self;
        }
        self.coords = [0; MAX_AXES];
        let mut n = 0;
        for (slot, c) in self
            .coords
            .iter_mut()
            .zip(vars.normalized_coords([(tag, weight)]))
        {
            *slot = c;
            n += 1;
        }
        self.axes = u8::try_from(n).unwrap_or(0);
        self.weight = Some(weight);
        self
    }

    /// The weight set with [`Font::with_weight`], if any.
    #[must_use]
    pub fn weight(&self) -> Option<f32> {
        self.weight
    }

    /// The normalized coordinates to shape, measure and rasterize with:
    /// empty unless [`Font::with_weight`] set a weight on a variable font.
    #[must_use]
    pub fn coords(&self) -> &[i16] {
        &self.coords[..usize::from(self.axes)]
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
        let m = self.face.metrics(self.coords()).scale(size_px);
        let line_height = (m.ascent + m.descent + m.leading).ceil();
        // Centre any leading around the glyphs, then snap to a pixel row.
        let baseline = (m.leading / 2.0 + m.ascent).round();
        let zero = self.glyph_id('0');
        let cell_width = self
            .face
            .glyph_metrics(self.coords())
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
    fn a_static_font_ignores_a_weight() {
        let font = Font::bundled().unwrap();
        assert!(!font.is_variable());
        assert_eq!(font.weight_range(), None);
        let heavy = font.with_weight(700.0);
        assert_eq!(heavy.coords(), [] as [i16; 0]);
        assert_eq!(heavy.weight(), None);
        assert_eq!(heavy.metrics(14.0), font.metrics(14.0));
    }

    /// Draws a variable font at two weights: set `NIMBUS_VARIABLE_FONT` to
    /// a font file with a weight axis (such as `JetBrainsMono[wght].ttf`)
    /// and run with `--ignored`.
    #[test]
    #[ignore = "needs a variable font named by NIMBUS_VARIABLE_FONT"]
    fn a_variable_font_takes_a_weight() {
        let path = std::env::var("NIMBUS_VARIABLE_FONT").unwrap();
        let data: &'static [u8] = Box::leak(std::fs::read(path).unwrap().into_boxed_slice());
        let font = Font::from_data(data, 0).unwrap();
        assert!(font.is_variable());
        let (min, _, max) = font.weight_range().unwrap();
        let light = font.with_weight(min);
        let heavy = font.with_weight(max);
        assert_ne!(light.coords(), [] as [i16; 0]);
        assert_ne!(light.coords(), heavy.coords());
        let mut raster = crate::raster::Rasterizer::new();
        let key = crate::raster::GlyphKey::new(light.glyph_id('W'), 40.0, 0.0);
        let ink = |f: Font, raster: &mut crate::raster::Rasterizer| {
            let b = raster.rasterize(&[f], key).unwrap();
            b.data.iter().map(|&v| u64::from(v)).sum::<u64>()
        };
        assert!(ink(heavy, &mut raster) > ink(light, &mut raster));
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
