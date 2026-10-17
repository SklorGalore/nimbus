//! Rasterizing: turning one glyph id into a coverage bitmap.
//!
//! A glyph in the font is an outline made of curves. Rasterizing samples that
//! outline on the pixel grid and records, for each pixel, how much of it the
//! shape covers: 0 is empty, 255 is fully inside. That one-byte-per-pixel
//! "coverage mask" is what the atlas stores and what the glyph shader later
//! uses as the alpha of the text colour.

use swash::scale::image::{Content, Image};
use swash::scale::{Render, ScaleContext, Source};
use swash::zeno::{Angle, Format, Transform, Vector};

use crate::font::Font;

/// Horizontal subpixel positions per pixel. A glyph whose pen position is
/// x = 10.3 is drawn from the bucket for 0.25 at pixel 10, so the same glyph
/// may be stored up to this many times, once per bucket.
pub const SUBPIXEL_STEPS: u8 = 4;

/// Identifies one rasterized glyph image.
///
/// Size is stored as fixed point (1/64 pixel) so the key can be hashed and
/// compared exactly, which floats cannot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GlyphKey {
    /// Glyph id in the font.
    pub glyph: u16,
    /// Pixel size times 64.
    pub size_64: u32,
    /// Horizontal subpixel bucket, `0..SUBPIXEL_STEPS`.
    pub subpx: u8,
    /// Which face: [`REGULAR`] or [`BOLD`]. Glyph ids belong to a face.
    pub face: u8,
    /// Slanted, for italic.
    pub slant: bool,
}

/// [`GlyphKey::face`] of the regular face.
pub const REGULAR: u8 = 0;

/// [`GlyphKey::face`] of the bold face.
pub const BOLD: u8 = 1;

/// How far italic text leans, in degrees: Hack's own italic leans about
/// this much.
const SLANT_DEGREES: f32 = 12.0;

impl GlyphKey {
    /// Key for `glyph` at `size_px`, drawn with its pen at fractional pixel
    /// offset `frac_x` (0.0 up to but not including 1.0).
    #[must_use]
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "sizes are small positive numbers and frac_x is in 0..1"
    )]
    pub fn new(glyph: u16, size_px: f32, frac_x: f32) -> Self {
        let steps = f32::from(SUBPIXEL_STEPS);
        let subpx = ((frac_x * steps).floor() as u8).min(SUBPIXEL_STEPS - 1);
        Self {
            glyph,
            size_64: (size_px * 64.0).round() as u32,
            subpx,
            face: REGULAR,
            slant: false,
        }
    }

    /// The same glyph from face `face`, slanted if `slant`.
    #[must_use]
    pub fn styled(self, face: u8, slant: bool) -> Self {
        Self {
            face,
            slant,
            ..self
        }
    }

    /// The pixel size this key was made for.
    #[must_use]
    #[expect(
        clippy::cast_precision_loss,
        reason = "size_64 holds small values that f32 represents exactly"
    )]
    pub fn size_px(&self) -> f32 {
        self.size_64 as f32 / 64.0
    }

    /// The subpixel offset this key's bitmap is rendered at.
    #[must_use]
    pub fn subpx_offset(&self) -> f32 {
        f32::from(self.subpx) / f32::from(SUBPIXEL_STEPS)
    }
}

/// A rasterized glyph: a coverage mask plus where to put it.
#[derive(Debug, Clone, Copy)]
pub struct GlyphBitmap<'a> {
    /// Pixels from the pen position to the bitmap's left edge.
    pub left: i32,
    /// Pixels from the baseline up to the bitmap's top edge.
    pub top: i32,
    /// Bitmap width in pixels.
    pub width: u32,
    /// Bitmap height in pixels.
    pub height: u32,
    /// `width * height` coverage bytes, row by row from the top.
    pub coverage: &'a [u8],
}

/// Reusable rasterizing state: swash's scaling cache and one image buffer.
pub struct Rasterizer {
    ctx: ScaleContext,
    image: Image,
}

impl std::fmt::Debug for Rasterizer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Rasterizer").finish_non_exhaustive()
    }
}

impl Default for Rasterizer {
    fn default() -> Self {
        Self::new()
    }
}

impl Rasterizer {
    /// Empty rasterizing state.
    #[must_use]
    pub fn new() -> Self {
        Self {
            ctx: ScaleContext::new(),
            image: Image::new(),
        }
    }

    /// Rasterize the glyph described by `key` from `fonts[key.face]` (the
    /// first font if there is no such face), slanted for italic.
    ///
    /// Returns `None` when the glyph has no outline, which is normal for a
    /// space, and also when swash cannot render it. The returned bitmap
    /// borrows this rasterizer's buffer and is valid until the next call.
    ///
    /// Hinting is off: hinted outlines snap to the pixel grid differently at
    /// each size, which looks sharper on 1x displays but makes text width and
    /// shape inconsistent. macOS does not hint either.
    pub fn rasterize(&mut self, fonts: &[Font], key: GlyphKey) -> Option<GlyphBitmap<'_>> {
        let font = fonts.get(usize::from(key.face)).or(fonts.first())?;
        let mut scaler = self
            .ctx
            .builder(font.face())
            .size(key.size_px())
            .hint(false)
            .build();
        self.image.clear();
        // Leaning the outline right: x grows with height above the baseline.
        let slant = key
            .slant
            .then(|| Transform::skew(Angle::from_degrees(SLANT_DEGREES), Angle::ZERO));
        let ok = Render::new(&[Source::Outline])
            .format(Format::Alpha)
            .transform(slant)
            .offset(Vector::new(key.subpx_offset(), 0.0))
            .render_into(&mut scaler, key.glyph, &mut self.image);
        let p = self.image.placement;
        if !ok || p.width == 0 || p.height == 0 || self.image.content != Content::Mask {
            return None;
        }
        Some(GlyphBitmap {
            left: p.left,
            top: p.top,
            width: p.width,
            height: p.height,
            coverage: &self.image.data,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[expect(clippy::float_cmp, reason = "exact values by construction")]
    fn keys_bucket_subpixel_offsets() {
        assert_eq!(GlyphKey::new(5, 14.0, 0.0).subpx, 0);
        assert_eq!(GlyphKey::new(5, 14.0, 0.24).subpx, 0);
        assert_eq!(GlyphKey::new(5, 14.0, 0.25).subpx, 1);
        assert_eq!(GlyphKey::new(5, 14.0, 0.99).subpx, 3);
        assert_eq!(GlyphKey::new(5, 14.5, 0.0).size_px(), 14.5);
    }

    #[test]
    fn letters_have_coverage_and_spaces_do_not() {
        let font = Font::bundled().unwrap();
        let mut r = Rasterizer::new();

        let m = r
            .rasterize(&[font], GlyphKey::new(font.glyph_id('M'), 28.0, 0.0))
            .unwrap();
        assert_eq!(m.coverage.len(), (m.width * m.height) as usize);
        assert!(m.coverage.contains(&255), "an M has fully covered pixels");
        assert!(m.top > 0, "M sits above the baseline");

        let space = r.rasterize(&[font], GlyphKey::new(font.glyph_id(' '), 28.0, 0.0));
        assert!(space.is_none());
    }

    #[test]
    fn subpixel_offset_changes_the_bitmap() {
        let font = Font::bundled().unwrap();
        let id = font.glyph_id('l');
        let mut r = Rasterizer::new();
        let a = r
            .rasterize(&[font], GlyphKey::new(id, 14.0, 0.0))
            .unwrap()
            .coverage
            .to_vec();
        let b = r
            .rasterize(&[font], GlyphKey::new(id, 14.0, 0.5))
            .unwrap()
            .coverage
            .to_vec();
        assert_ne!(a, b);
    }

    #[test]
    fn bold_and_slanted_glyphs_differ_from_regular() {
        let regular = Font::bundled().unwrap();
        let bold = Font::bundled_bold().unwrap();
        let faces = [regular, bold];
        let mut r = Rasterizer::new();
        let mut draw = |key: GlyphKey| r.rasterize(&faces, key).unwrap().coverage.to_vec();
        let upright = GlyphKey::new(regular.glyph_id('l'), 20.0, 0.0);
        let plain = draw(upright);
        assert_ne!(draw(upright.styled(REGULAR, true)), plain, "slanted");
        let heavy = GlyphKey::new(bold.glyph_id('l'), 20.0, 0.0).styled(BOLD, false);
        let ink = |c: &[u8]| c.iter().map(|&v| u32::from(v)).sum::<u32>();
        assert!(ink(&draw(heavy)) > ink(&plain), "bold has more ink");
    }

    #[test]
    fn missing_glyph_box_renders() {
        let font = Font::bundled().unwrap();
        let mut r = Rasterizer::new();
        assert!(r.rasterize(&[font], GlyphKey::new(0, 14.0, 0.0)).is_some());
    }
}
