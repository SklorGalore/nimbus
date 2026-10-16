//! Drawing lines of text: shaping, glyph caching, and pixel placement.
//!
//! For each glyph of a shaped line this module decides its exact pixel
//! position, finds or creates its bitmap in the atlas, and queues one quad.
//! The glyph cache maps a [`GlyphKey`] (glyph, size, subpixel bucket) to where
//! its bitmap lives, so each distinct glyph is rasterized once, not per frame.

use std::collections::HashMap;

use crate::atlas::{Atlas, AtlasError, AtlasSlot};
use crate::font::{Font, Metrics};
use crate::frame::FrameBuilder;
use crate::raster::{GlyphKey, Rasterizer};
use crate::shape::{Grid, ShapedGlyph, Shaper};

/// A colour for part of a line: from the end of the previous run (or the
/// start of the text) up to byte `end` of the text being drawn.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ColorRun {
    /// End byte, exclusive, within the text passed to the draw call.
    pub end: usize,
    /// Straight RGBA colour.
    pub color: [f32; 4],
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
        Self {
            font,
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

    /// Call at the start of each frame: performs a reset requested by a full
    /// atlas during the previous frame.
    pub(crate) fn begin_frame(&mut self, atlas: &mut Atlas, queue: &wgpu::Queue) {
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

        for g in &self.shaped {
            // Split the pen position into a whole pixel and a fraction. The
            // fraction picks the subpixel variant of the bitmap, the whole
            // part places it, so every quad lands on the pixel grid.
            let pen = origin[0] + g.x;
            if pen < style.clip_left {
                continue;
            }
            let whole = pen.floor();
            let key = GlyphKey::new(g.id, size_px, pen - whole);

            let cached = if let Some(entry) = self.cache.get(&key) {
                *entry
            } else {
                match load_glyph(&self.font, &mut self.raster, atlas, queue, key) {
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
                // Shaped glyphs come in cluster order in a monospace grid,
                // but a search keeps this right even if one does not.
                let cluster = g.cluster as usize;
                let run = style.runs.partition_point(|r| r.end <= cluster);
                let color = style.runs.get(run).map_or(style.color, |r| r.color);
                frame.push_glyph([whole + px(c.left), y], &c.slot, color);
            }
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
    font: &Font,
    raster: &mut Rasterizer,
    atlas: &mut Atlas,
    queue: &wgpu::Queue,
    key: GlyphKey,
) -> Loaded {
    let Some(bitmap) = raster.rasterize(font, key) else {
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
