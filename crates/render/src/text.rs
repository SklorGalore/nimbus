//! Drawing lines of text: shaping, glyph caching, and pixel placement.
//!
//! For each glyph of a shaped line this module decides its exact pixel
//! position, finds or creates its bitmap in the atlas, and queues one quad.
//! The glyph cache maps a [`GlyphKey`] (glyph, size, subpixel bucket) to where
//! its bitmap lives, so each distinct glyph is rasterized once, not per frame.

use std::collections::HashMap;

use crate::atlas::{Atlas, AtlasError, AtlasSlot};
use crate::fallback::{Fallback, Notify};
use crate::font::{Font, Metrics};
use crate::frame::FrameBuilder;
use crate::raster::{BOLD, GlyphKey, REGULAR, Rasterizer};
use crate::shape::{Grid, MissingCells, ShapedGlyph, Shaper};

/// A colour and style for part of a line: from the end of the previous run
/// (or the start of the text) up to byte `end` of the text being drawn.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ColorRun {
    /// End byte, exclusive, within the text passed to the draw call.
    pub end: usize,
    /// Straight RGBA colour.
    pub color: [f32; 4],
    /// Bold, italic or both.
    pub style: Style,
}

/// Bold and italic, as a theme sets them per syntax capture.
///
/// Bold draws the glyph from the bundled bold font. Italic slants the
/// outline as it is rasterized (Hack's italic is a slanted regular anyway).
/// Neither changes a character's width, so the grid stays put.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Style {
    /// Drawn from the bold font.
    pub bold: bool,
    /// Slanted.
    pub italic: bool,
}

/// How to draw one line of text.
#[derive(Debug, Clone, Copy)]
pub(crate) struct LineStyle<'a> {
    /// Straight RGBA text colour, for bytes after the last run.
    pub(crate) color: [f32; 4],
    /// Colours by byte range, sorted by `end`; may be empty.
    pub(crate) runs: &'a [ColorRun],
    /// Glyphs whose pen position is left of this x are skipped.
    pub(crate) clip_left: f32,
    /// Column within its line where this text starts (see [`Grid`]).
    pub(crate) start_col: usize,
}

/// A glyph already in the atlas, with its offset from the pen position.
#[derive(Debug, Clone, Copy)]
struct CachedGlyph {
    slot: AtlasSlot,
    left: i32,
    top: i32,
}

/// Shaping, rasterizing and caching state for one font at one size.
pub(crate) struct TextRenderer {
    font: Font,
    /// The faces glyph keys index: regular, bold, then fallback fonts in
    /// the order they arrived.
    faces: Vec<Font>,
    /// Finds characters the bundled font lacks in system fonts.
    fallback: Fallback,
    metrics: Metrics,
    tab_width: usize,
    shaper: Shaper,
    raster: Rasterizer,
    /// `None` records glyphs with no bitmap (spaces), so they are not
    /// rasterized again every frame.
    cache: HashMap<GlyphKey, Option<CachedGlyph>>,
    shaped: Vec<ShapedGlyph>,
    /// The atlas filled up during the last frame. The next frame starts by
    /// clearing atlas and cache so every glyph can be stored again. It cannot
    /// happen mid-frame: glyphs already queued would sample overwritten texels.
    reset_pending: bool,
}

#[expect(
    clippy::cast_precision_loss,
    reason = "glyph offsets are a few hundred pixels at most, exact in f32"
)]
fn px(v: i32) -> f32 {
    v as f32
}

impl TextRenderer {
    pub(crate) fn new(font: Font, size_px: f32, tab_width: usize) -> Self {
        // A broken bold asset would be a build mistake; draw regular then.
        let bold = Font::bundled_bold().unwrap_or(font);
        Self {
            font,
            faces: vec![font, bold],
            fallback: Fallback::new(),
            metrics: font.metrics(size_px),
            tab_width,
            shaper: Shaper::new(),
            raster: Rasterizer::new(),
            cache: HashMap::new(),
            shaped: Vec::new(),
            reset_pending: false,
        }
    }

    pub(crate) fn metrics(&self) -> Metrics {
        self.metrics
    }

    pub(crate) fn grid(&self) -> Grid {
        Grid {
            cell_width: self.metrics.cell_width,
            tab_width: self.tab_width,
            start_col: 0,
        }
    }

    pub(crate) fn set_tab_width(&mut self, tab_width: usize) {
        self.tab_width = tab_width.max(1);
    }

    /// Change the pixel size, for a font-size or scale-factor change. Every
    /// cached bitmap is the wrong size now, so the cache and atlas are reset.
    pub(crate) fn set_size(&mut self, size_px: f32, atlas: &mut Atlas, queue: &wgpu::Queue) {
        if (size_px - self.metrics.size_px).abs() < f32::EPSILON {
            return;
        }
        self.metrics = self.font.metrics(size_px);
        self.reset(atlas, queue);
    }

    /// Call `notify` when a fallback font arrives, so the window draws
    /// again with it.
    pub(crate) fn set_font_notify(&mut self, notify: Notify) {
        self.fallback.set_notify(notify);
    }

    /// Take fallback fonts that have arrived. Returns whether any did.
    pub(crate) fn receive_fonts(&mut self) -> bool {
        self.fallback.receive(&mut self.faces)
    }

    /// Whether fallback fonts are still being looked for.
    pub(crate) fn fonts_pending(&self) -> bool {
        self.fallback.is_waiting()
    }

    /// Call at the start of each frame: takes fallback fonts that have
    /// arrived, and performs a reset requested by a full atlas during the
    /// previous frame.
    pub(crate) fn begin_frame(&mut self, atlas: &mut Atlas, queue: &wgpu::Queue) {
        self.receive_fonts();
        if self.reset_pending {
            log::info!("glyph atlas was full; clearing it");
            self.reset(atlas, queue);
        }
    }

    /// Whether the last frame skipped glyphs and should be drawn again.
    pub(crate) fn needs_redraw(&self) -> bool {
        self.reset_pending
    }

    fn reset(&mut self, atlas: &mut Atlas, queue: &wgpu::Queue) {
        self.cache.clear();
        atlas.clear(queue);
        self.reset_pending = false;
    }

    /// Queue the glyphs of `line` with the line's top-left corner at `origin`
    /// (physical pixels; `origin[1]` should be a whole number). Glyphs whose
    /// pen position is left of `clip_left` are skipped, so horizontally
    /// scrolled text does not draw over the gutter.
    pub(crate) fn push_line(
        &mut self,
        queue: &wgpu::Queue,
        atlas: &mut Atlas,
        frame: &mut FrameBuilder,
        origin: [f32; 2],
        line: &str,
        style: LineStyle<'_>,
    ) {
        let size_px = self.metrics.size_px;
        let baseline_y = origin[1] + self.metrics.baseline;
        if self.reset_pending {
            // The atlas is full; skip all uncached work until the reset at
            // the start of the next frame.
            return;
        }
        self.shaper.shape_line(
            &self.font,
            size_px,
            line,
            Grid {
                start_col: style.start_col,
                ..self.grid()
            },
            &mut self.shaped,
        );

        for i in 0..self.shaped.len() {
            let g = self.shaped[i];
            // Shaped glyphs come in cluster order in a monospace grid, but
            // a search keeps this right even if one does not.
            let cluster = g.cluster as usize;
            let run = style
                .runs
                .get(style.runs.partition_point(|r| r.end <= cluster));
            let (color, look) = run.map_or((style.color, Style::default()), |r| (r.color, r.style));
            // A glyph the bundled font lacks comes from a fallback font
            // when one has it: centred in the character's cells, and
            // shrunk if it is wider than them. Until then, the box.
            let fallback = g
                .missing
                .and_then(|cells| self.fallback_glyph(line, cluster, cells));
            // Split the pen position into a whole pixel and a fraction. The
            // fraction picks the subpixel variant of the bitmap, the whole
            // part places it, so every quad lands on the pixel grid.
            let pen = origin[0] + fallback.map_or(g.x, |(_, _, x, _)| x);
            if pen < style.clip_left {
                continue;
            }
            let whole = pen.floor();
            let key = match fallback {
                Some((face, id, _, size)) => {
                    GlyphKey::new(id, size, pen - whole).styled(face, look.italic)
                }
                None => self.styled_key(
                    GlyphKey::new(g.id, size_px, pen - whole),
                    line,
                    cluster,
                    look,
                ),
            };

            let cached = if let Some(entry) = self.cache.get(&key) {
                *entry
            } else {
                match load_glyph(&self.faces, &mut self.raster, atlas, queue, key) {
                    Loaded::Glyph(g) => {
                        self.cache.insert(key, Some(g));
                        Some(g)
                    }
                    Loaded::Blank => {
                        self.cache.insert(key, None);
                        None
                    }
                    Loaded::NoRoom => {
                        self.reset_pending = true;
                        return;
                    }
                }
            };
            if let Some(c) = cached {
                // Shapers may shift a glyph vertically (combining marks);
                // swash's y offset points up, screen y points down.
                let y = (baseline_y - g.y).round() - px(c.top);
                frame.push_glyph([whole + px(c.left), y], &c.slot, color);
            }
        }
    }
}

impl TextRenderer {
    /// For a character the bundled font lacks: the fallback face that has
    /// it, its glyph, its x from the start of the line, and the size to
    /// draw it at. `None` (draw the box) when the grapheme is more than one
    /// character, or no font is known to have it yet.
    fn fallback_glyph(
        &mut self,
        line: &str,
        cluster: usize,
        cells: MissingCells,
    ) -> Option<(u8, u16, f32, f32)> {
        let text = line.get(cluster..cluster + usize::from(cells.len))?;
        let mut chars = text.chars();
        let c = chars.next()?;
        if chars.next().is_some() {
            return None;
        }
        let face = self.fallback.face_for(c)?;
        let font = self.faces.get(usize::from(face))?;
        let id = font.glyph_id(c);
        let size = self.metrics.size_px;
        let advance = font.face().glyph_metrics(&[]).scale(size).advance_width(id);
        let room = f32::from(cells.cols) * self.metrics.cell_width;
        let scale = if advance > room && advance > 0.0 {
            room / advance
        } else {
            1.0
        };
        let x = cells.x + (room - advance * scale) / 2.0;
        Some((face, id, x, size * scale))
    }

    /// `key` for a glyph shaped from the regular font, drawn in `look`.
    /// Shaping uses the regular font only; a bold glyph is the bold font's
    /// glyph for the same character, which in a monospace font with no
    /// ligatures is the same shape in the same cell. A glyph that is not
    /// simply its character's (part of a cluster, a stand-in box) stays
    /// regular.
    fn styled_key(&self, key: GlyphKey, line: &str, cluster: usize, look: Style) -> GlyphKey {
        if !look.bold {
            return key.styled(REGULAR, look.italic);
        }
        let ch = line.get(cluster..).and_then(|s| s.chars().next());
        let bold = ch
            .filter(|&c| self.font.glyph_id(c) == key.glyph)
            .map(|c| self.faces[1].glyph_id(c))
            .filter(|&id| id != 0);
        match bold {
            Some(id) => GlyphKey { glyph: id, ..key }.styled(BOLD, look.italic),
            None => key.styled(REGULAR, look.italic),
        }
    }
}

/// The outcome of trying to put a glyph in the atlas.
enum Loaded {
    /// Stored; draw it.
    Glyph(CachedGlyph),
    /// Nothing to draw, such as a space, or a bitmap we cannot use.
    Blank,
    /// The atlas is full; stop and reset it before the next frame.
    NoRoom,
}

/// Rasterize `key` and copy its bitmap into the atlas.
fn load_glyph(
    fonts: &[Font],
    raster: &mut Rasterizer,
    atlas: &mut Atlas,
    queue: &wgpu::Queue,
    key: GlyphKey,
) -> Loaded {
    let Some(bitmap) = raster.rasterize(fonts, key) else {
        return Loaded::Blank;
    };
    match atlas.insert(queue, bitmap.width, bitmap.height, bitmap.coverage) {
        Ok(slot) => Loaded::Glyph(CachedGlyph {
            slot,
            left: bitmap.left,
            top: bitmap.top,
        }),
        Err(AtlasError::Full) => Loaded::NoRoom,
        Err(e) => {
            log::warn!("glyph {} not drawn: {e}", key.glyph);
            Loaded::Blank
        }
    }
}
