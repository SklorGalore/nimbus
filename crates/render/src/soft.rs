//! Compatibility mode (M23): painting frames on the CPU when no GPU can be
//! opened.
//!
//! **Software rendering.** For each frame a GPU does four things: fills
//! rectangles, strokes curve segments, blends glyph bitmaps in the text
//! colour, and shows the result. Our frames are simple enough that the CPU
//! can do the same with three loops over an array of pixels, one `u32` per
//! pixel. The frame is already built on the CPU by [`FrameBuilder`] as flat
//! lists of [`RectInstance`]s, [`SegmentInstance`]s and [`GlyphInstance`]s,
//! and the glyph bitmaps are
//! already rasterized on the CPU by swash, so nothing above this module
//! knows which painter runs.
//!
//! **Alpha blending by hand.** Every colour is turned into *premultiplied*
//! form (red, green and blue already multiplied by alpha) and laid over
//! what is there with `out = src + dst × (1 − src.alpha)`: the formula the
//! GPU's blend state uses (`PREMULTIPLIED_ALPHA_BLENDING` in
//! `pipeline.rs`). A glyph's coverage is shaped by the same gamma
//! adjustment as in `glyph.wgsl`. So a compatibility frame differs from
//! the GPU's only by rounding.
//!
//! **Which pixels a shape covers.** A GPU colours a pixel when its centre
//! (`x + 0.5`) lies inside the shape, counting a left or top edge that
//! passes exactly through a centre as inside. [`span`] does the same, so
//! rectangles with fractional edges cover the same pixels in both painters.
//!
//! **Curves.** A segment is painted the way `segment.wgsl` paints it: the
//! curve is flattened into the same number of straight pieces (once per
//! segment, into an array on the stack), and every pixel in the same grown
//! bounding box gets coverage from its centre's distance to the nearest
//! piece. The shared numbers live in `segment.rs`.
//!
//! Frames reach the window through `softbuffer`, a small crate that shows
//! a pixel array in a window on macOS, Windows, X11 and Wayland, with no
//! `unsafe` on our side.

use std::num::NonZeroU32;
use std::ops::Range;
use std::sync::{Arc, LazyLock};

use winit::window::Window;

use crate::atlas::{ATLAS_SIZE, CpuPages};
use crate::frame::FrameBuilder;
use crate::gpu::{Capture, FrameStatus};
use crate::pipeline::{GLYPH_COLOR, GlyphInstance, RectInstance, SegmentInstance};
use crate::segment;

/// Same constant as `GAMMA_ADJUST` in `glyph.wgsl`: above 1 thickens text
/// slightly, which blending in gamma space otherwise makes look thin.
const GAMMA_ADJUST: f32 = 1.4;

/// The largest frame side painted, in pixels. A window larger than this
/// (far beyond 8K) is painted at the cap, as on a GPU with a texture limit.
const MAX_SIDE: u32 = 16_384;

/// Why compatibility mode could not start: softbuffer's message. Kept as
/// text because softbuffer's error holds window handles, which cannot
/// cross threads, and `GpuError` comes back from the GPU thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompatError(String);

impl CompatError {
    fn new(e: &softbuffer::SoftBufferError) -> Self {
        Self(e.to_string())
    }
}

impl std::fmt::Display for CompatError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "could not show a pixel buffer in the window: {}", self.0)
    }
}

impl std::error::Error for CompatError {}

/// Where painted frames go.
enum Target {
    /// A window, through softbuffer. The window is shared with the app:
    /// the surface keeps its own clone so it never outlives the window.
    Window(softbuffer::Surface<Arc<Window>, Arc<Window>>),
    /// A pixel array with no window, for headless renders.
    Offscreen(Vec<u32>),
}

/// Coverage byte to shaped coverage: `(c / 255)^(1 / GAMMA_ADJUST)`,
/// worked out once instead of a `powf` per glyph pixel.
static SHAPED: LazyLock<[f32; 256]> = LazyLock::new(shaped_table);

/// The CPU painter and its frame size.
pub(crate) struct Cpu {
    target: Target,
    width: u32,
    height: u32,
}

impl Cpu {
    /// Paint into `window`.
    pub(crate) fn window(window: &Arc<Window>) -> Result<Self, CompatError> {
        let context = softbuffer::Context::new(window.clone()).map_err(|e| CompatError::new(&e))?;
        let surface =
            softbuffer::Surface::new(&context, window.clone()).map_err(|e| CompatError::new(&e))?;
        let size = window.inner_size();
        let mut cpu = Self {
            target: Target::Window(surface),
            width: 0,
            height: 0,
        };
        cpu.resize(size.width.max(1), size.height.max(1));
        Ok(cpu)
    }

    /// Paint into memory, `width × height` pixels.
    pub(crate) fn offscreen(width: u32, height: u32) -> Self {
        let mut cpu = Self {
            target: Target::Offscreen(Vec::new()),
            width: 0,
            height: 0,
        };
        cpu.resize(width.max(1), height.max(1));
        cpu
    }

    /// The frame size in pixels.
    pub(crate) fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// Change the frame size. Zero sides are ignored (a minimised window).
    pub(crate) fn resize(&mut self, width: u32, height: u32) {
        let (Some(w), Some(h)) = (
            NonZeroU32::new(width.min(MAX_SIDE)),
            NonZeroU32::new(height.min(MAX_SIDE)),
        ) else {
            return;
        };
        if (w.get(), h.get()) == (self.width, self.height) {
            return;
        }
        if width > MAX_SIDE || height > MAX_SIDE {
            log::warn!("window {width}x{height} exceeds {MAX_SIDE}; clamping");
        }
        match &mut self.target {
            Target::Window(surface) => {
                if let Err(e) = surface.resize(w, h) {
                    log::error!("compatibility mode: resize failed: {e}");
                    return;
                }
            }
            Target::Offscreen(pixels) => {
                pixels.clear();
                pixels.resize(w.get() as usize * h.get() as usize, 0);
            }
        }
        (self.width, self.height) = (w.get(), h.get());
    }

    /// Paint `frame` over `background` and show it. Returns the frame read
    /// back as RGBA too when `capture` is set.
    pub(crate) fn render(
        &mut self,
        frame: &mut FrameBuilder,
        pages: &CpuPages,
        (background, top): ([f32; 4], f32),
        capture: bool,
    ) -> (FrameStatus, Option<Capture>) {
        frame.finish();
        let (width, height) = (self.width, self.height);
        let shaped = &*SHAPED;
        match &mut self.target {
            Target::Window(surface) => {
                let mut buffer = match surface.buffer_mut() {
                    Ok(b) => b,
                    Err(e) => {
                        // Hidden, not Retry: the app backs off and gives
                        // up, instead of spinning on a buffer that will
                        // not come.
                        log::warn!("compatibility mode: no buffer this frame: {e}");
                        return (FrameStatus::Hidden, None);
                    }
                };
                let canvas = Canvas {
                    pixels: &mut buffer,
                    width,
                    height,
                    top,
                };
                let captured = paint(canvas, frame, pages, background, shaped, capture);
                if let Err(e) = buffer.present() {
                    log::warn!("compatibility mode: present failed: {e}");
                    return (FrameStatus::Hidden, captured);
                }
                (FrameStatus::Presented, captured)
            }
            Target::Offscreen(pixels) => {
                let canvas = Canvas {
                    pixels,
                    width,
                    height,
                    top,
                };
                let captured = paint(canvas, frame, pages, background, shaped, capture);
                (FrameStatus::Presented, captured)
            }
        }
    }
}

fn shaped_table() -> [f32; 256] {
    std::array::from_fn(|i| {
        let cov = f32::from(u8::try_from(i).unwrap_or(u8::MAX)) / 255.0;
        // Matches the shader, which keeps exact zero as zero.
        if cov > 0.0 {
            cov.powf(1.0 / GAMMA_ADJUST)
        } else {
            0.0
        }
    })
}

/// The pixels being painted: `width × height`, row after row from the top,
/// each `0x00RRGGBB` (the layout softbuffer wants).
pub(crate) struct Canvas<'a> {
    pub(crate) pixels: &'a mut [u32],
    pub(crate) width: u32,
    pub(crate) height: u32,
    /// How far everything moves down for a title bar the app draws
    /// (`Gpu::set_inset_top`), as the shaders' `globals.top` does. Clips
    /// already include it.
    pub(crate) top: f32,
}

/// Paint one frame and, when asked, read it back.
fn paint(
    mut canvas: Canvas<'_>,
    frame: &FrameBuilder,
    pages: &CpuPages,
    background: [f32; 4],
    shaped: &[f32; 256],
    capture: bool,
) -> Option<Capture> {
    draw(&mut canvas, frame, pages, background, shaped);
    capture.then(|| to_capture(&canvas))
}

/// Clear to `background`, then each layer's rectangles, segments and glyphs
/// within its clip, in order: the same steps as `FrameBuilder::record`.
pub(crate) fn draw(
    canvas: &mut Canvas<'_>,
    frame: &FrameBuilder,
    pages: &CpuPages,
    background: [f32; 4],
    shaped: &[f32; 256],
) {
    let [r, g, b, _] = background;
    let clear = pack(to_byte(r), to_byte(g), to_byte(b));
    let len = canvas.width as usize * canvas.height as usize;
    let Some(pixels) = canvas.pixels.get_mut(..len) else {
        log::error!("compatibility mode: buffer smaller than the frame");
        return;
    };
    pixels.fill(clear);
    for layer in frame.layers() {
        let Some(clip) = clip_box(layer.clip, canvas.width, canvas.height) else {
            continue;
        };
        for rect in layer.rects {
            fill_rect(canvas, &clip, rect);
        }
        for seg in layer.segments {
            stroke_segment(canvas, &clip, seg);
        }
        for glyph in layer.glyphs {
            blend_glyph(canvas, &clip, glyph, pages, shaped);
        }
    }
}

/// A layer's clip as pixel ranges, cut to the frame; `None` when nothing
/// of it is left (the GPU path skips those layers too).
struct ClipBox {
    x: Range<u32>,
    y: Range<u32>,
}

fn clip_box(clip: Option<[u32; 4]>, width: u32, height: u32) -> Option<ClipBox> {
    let [x, y, w, h] = clip.unwrap_or([0, 0, width, height]);
    let (x, y) = (x.min(width), y.min(height));
    let (w, h) = (w.min(width - x), h.min(height - y));
    (w > 0 && h > 0).then_some(ClipBox {
        x: x..x + w,
        y: y..y + h,
    })
}

/// The pixels from `lo` to `hi` (edges in pixels) whose centres are inside,
/// cut to `within`.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "edges are clamped to the clip, which lies within u32"
)]
fn span(lo: f32, hi: f32, within: &Range<u32>) -> Range<usize> {
    let (min, max) = (f64::from(within.start), f64::from(within.end));
    // Centre x + 0.5 inside [lo, hi): x from ceil(lo − 0.5) to ceil(hi − 0.5).
    let start = (f64::from(lo) - 0.5).ceil().clamp(min, max);
    let end = (f64::from(hi) - 0.5).ceil().clamp(min, max);
    if end <= start || start.is_nan() || end.is_nan() {
        return 0..0;
    }
    start as usize..end as usize
}

fn fill_rect(canvas: &mut Canvas<'_>, clip: &ClipBox, rect: &RectInstance) {
    let (pos, size) = (rect.pos, rect.size);
    let y = pos[1] + canvas.top;
    let xs = span(pos[0], pos[0] + size[0], &clip.x);
    let ys = span(y, y + size[1], &clip.y);
    if xs.is_empty() {
        return;
    }
    let [red, green, blue, alpha] = rect.color;
    let stride = canvas.width as usize;
    if alpha >= 1.0 {
        // Opaque: nothing shows through, so every pixel is the same.
        let color = pack(to_byte(red), to_byte(green), to_byte(blue));
        for row in ys {
            if let Some(px) = canvas
                .pixels
                .get_mut(row * stride + xs.start..row * stride + xs.end)
            {
                px.fill(color);
            }
        }
        return;
    }
    if alpha <= 0.0 {
        return;
    }
    let src = [red * alpha, green * alpha, blue * alpha, alpha];
    for row in ys {
        if let Some(px) = canvas
            .pixels
            .get_mut(row * stride + xs.start..row * stride + xs.end)
        {
            for p in px {
                *p = over(*p, src);
            }
        }
    }
}

/// Stroke one curve segment: for each pixel of its grown bounding box (the
/// GPU's quad) inside the clip, the coverage its centre gets from the
/// distance to the flattened curve, blended over what is there.
#[expect(
    clippy::cast_precision_loss,
    reason = "pixel indices are below MAX_SIDE (2^14), exact in f32"
)]
fn stroke_segment(canvas: &mut Canvas<'_>, clip: &ClipBox, seg: &SegmentInstance) {
    let [x0, y0, x1, y1] = segment::bounds(seg);
    let xs = span(x0, x1, &clip.x);
    let ys = span(y0, y1, &clip.y);
    let [red, green, blue, alpha] = seg.color;
    if xs.is_empty() || ys.is_empty() || alpha <= 0.0 {
        return;
    }
    let flat = segment::flatten(seg);
    let stride = canvas.width as usize;
    for py in ys {
        let Some(row) = canvas
            .pixels
            .get_mut(py * stride + xs.start..py * stride + xs.end)
        else {
            continue;
        };
        let cy = py as f32 + 0.5;
        for (p, px) in row.iter_mut().zip(xs.clone()) {
            let d = segment::distance(&flat, [px as f32 + 0.5, cy]);
            let cov = segment::coverage(seg.width, d);
            if cov <= 0.0 {
                continue;
            }
            let a = alpha * cov;
            *p = over(*p, [red * a, green * a, blue * a, a]);
        }
    }
}

#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "glyph positions are whole pixels and atlas texels lie within 2048"
)]
fn blend_glyph(
    canvas: &mut Canvas<'_>,
    clip: &ClipBox,
    glyph: &GlyphInstance,
    pages: &CpuPages,
    shaped: &[f32; 256],
) {
    let (pos, size) = (glyph.pos, glyph.size);
    let y = pos[1] + canvas.top;
    let xs = span(pos[0], pos[0] + size[0], &clip.x);
    let ys = span(y, y + size[1], &clip.y);
    if xs.is_empty() || ys.is_empty() {
        return;
    }
    // One texel per pixel: the texel for pixel (px, py) is the bitmap's
    // corner in the atlas plus how far the pixel is from the glyph's corner.
    let atlas_side = f64::from(ATLAS_SIZE);
    let tx0 = (f64::from(glyph.uv_min[0]) * atlas_side).round() as usize;
    let ty0 = (f64::from(glyph.uv_min[1]) * atlas_side).round() as usize;
    let x0 = f64::from(pos[0]).round() as usize;
    let y0 = f64::from(y).round() as usize;
    let stride = canvas.width as usize;
    let atlas = ATLAS_SIZE as usize;
    let [red, green, blue, alpha] = glyph.color;
    let color_page = glyph.flags & GLYPH_COLOR != 0;
    for py in ys {
        let ty = ty0 + (py - y0);
        let Some(row) = canvas
            .pixels
            .get_mut(py * stride + xs.start..py * stride + xs.end)
        else {
            continue;
        };
        for (p, px) in row.iter_mut().zip(xs.clone()) {
            let tx = tx0 + (px - x0);
            let src =
                if color_page {
                    // Already premultiplied; the text colour's alpha fades it.
                    let Some(texel) = pages.color.as_deref().and_then(|page| {
                        page.get((ty * atlas + tx) * 4..(ty * atlas + tx) * 4 + 4)
                    }) else {
                        continue;
                    };
                    let c = |i: usize| f32::from(texel[i]) / 255.0 * alpha;
                    [c(0), c(1), c(2), c(3)]
                } else {
                    let Some(&cov) = pages.coverage.get(ty * atlas + tx) else {
                        continue;
                    };
                    if cov == 0 {
                        continue;
                    }
                    let a = shaped[usize::from(cov)] * alpha;
                    [red * a, green * a, blue * a, a]
                };
            *p = over(*p, src);
        }
    }
}

/// `src` (premultiplied, 0 to 1) laid over the pixel `dst`:
/// `out = src + dst × (1 − src.alpha)`, per channel.
fn over(dst: u32, src: [f32; 4]) -> u32 {
    let [_, dr, dg, db] = dst.to_be_bytes();
    let keep = 1.0 - src[3];
    let mix = |s: f32, d: u8| to_byte(s + f32::from(d) / 255.0 * keep);
    pack(mix(src[0], dr), mix(src[1], dg), mix(src[2], db))
}

/// A channel from 0 to 1 as a byte, rounded to nearest, as a GPU stores a
/// float into an 8-bit target.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "clamped to 0..=255 first"
)]
fn to_byte(v: f32) -> u8 {
    (v.clamp(0.0, 1.0) * 255.0).round() as u8
}

fn pack(r: u8, g: u8, b: u8) -> u32 {
    u32::from_be_bytes([0, r, g, b])
}

fn to_capture(canvas: &Canvas<'_>) -> Capture {
    let len = canvas.width as usize * canvas.height as usize;
    let mut rgba = Vec::with_capacity(len * 4);
    for p in canvas.pixels.iter().take(len) {
        let [_, r, g, b] = p.to_be_bytes();
        rgba.extend_from_slice(&[r, g, b, 255]);
    }
    Capture {
        width: canvas.width,
        height: canvas.height,
        rgba,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::atlas::{Atlas, AtlasPage};

    const BLACK: [f32; 4] = [0.0, 0.0, 0.0, 1.0];

    fn canvas(pixels: &mut [u32], width: u32, height: u32) -> Canvas<'_> {
        Canvas {
            pixels,
            width,
            height,
            top: 0.0,
        }
    }

    fn paint_small(frame: &mut FrameBuilder, atlas: &Atlas, (w, h): (u32, u32)) -> Vec<u32> {
        frame.finish();
        let mut px = vec![0; (w * h) as usize];
        let pages = atlas.cpu().unwrap();
        draw(
            &mut canvas(&mut px, w, h),
            frame,
            pages,
            BLACK,
            &shaped_table(),
        );
        px
    }

    #[test]
    fn spans_follow_the_pixel_centre_rule() {
        let all = 0..100;
        assert_eq!(span(3.0, 5.0, &all), 3..5);
        // Centre 3.5 is on the left edge: inside. Centre 5.5 is past 5.4.
        assert_eq!(span(3.5, 5.4, &all), 3..5);
        assert_eq!(span(3.6, 5.6, &all), 4..6);
        assert_eq!(span(-10.0, 2.0, &all), 0..2);
        assert_eq!(span(98.0, 200.0, &all), 98..100);
        assert_eq!(span(5.0, 5.0, &all), 0..0);
        assert_eq!(span(f32::NAN, 5.0, &all), 0..0);
    }

    #[test]
    fn clips_are_cut_to_the_frame() {
        let c = clip_box(Some([8, 2, 10, 10]), 10, 6).unwrap();
        assert_eq!((c.x, c.y), (8..10, 2..6));
        assert!(clip_box(Some([20, 0, 5, 5]), 10, 6).is_none());
        let whole = clip_box(None, 10, 6).unwrap();
        assert_eq!((whole.x, whole.y), (0..10, 0..6));
    }

    #[test]
    fn blending_matches_the_premultiplied_formula() {
        // Half-transparent white over black is middle grey (0.5 × 255).
        assert_eq!(over(0, [0.5, 0.5, 0.5, 0.5]), 0x0080_8080);
        // Opaque replaces; transparent keeps.
        assert_eq!(over(0x0012_3456, [1.0, 0.0, 0.0, 1.0]), 0x00ff_0000);
        assert_eq!(over(0x0012_3456, [0.0; 4]), 0x0012_3456);
        // 25 % red over pure blue: red 0.25, blue 0.75.
        assert_eq!(over(0x0000_00ff, [0.25, 0.0, 0.0, 0.25]), 0x0040_00bf);
    }

    #[test]
    fn coverage_is_shaped_like_the_shader() {
        let t = shaped_table();
        assert!(t[0].abs() < f32::EPSILON);
        assert!((t[255] - 1.0).abs() < 1e-6);
        // Thickened: half coverage draws darker than half.
        assert!((t[128] - (128.0f32 / 255.0).powf(1.0 / 1.4)).abs() < 1e-6);
        assert!(t[128] > 0.6);
    }

    #[test]
    fn layers_draw_in_order_within_their_clips() {
        let atlas = Atlas::in_memory();
        let mut f = FrameBuilder::default();
        f.push_rect([0.0, 0.0], [4.0, 4.0], [1.0, 0.0, 0.0, 1.0]);
        f.begin_layer(Some([2, 0, 2, 4]));
        // Covers the whole frame but is clipped to the right half.
        f.push_rect([0.0, 0.0], [4.0, 4.0], [0.0, 0.0, 1.0, 1.0]);
        let px = paint_small(&mut f, &atlas, (4, 4));
        let red = 0x00ff_0000;
        let blue = 0x0000_00ff;
        for row in px.chunks(4) {
            assert_eq!(row, [red, red, blue, blue]);
        }
    }

    #[test]
    fn a_translucent_rect_blends_with_the_background() {
        let atlas = Atlas::in_memory();
        let mut f = FrameBuilder::default();
        f.push_rect([1.0, 1.0], [1.0, 1.0], [1.0, 1.0, 1.0, 0.5]);
        let px = paint_small(&mut f, &atlas, (3, 3));
        assert_eq!(px[4], 0x0080_8080);
        assert_eq!(px.iter().filter(|&&p| p == 0).count(), 8);
    }

    #[test]
    fn coverage_glyphs_tint_by_the_text_colour() {
        let mut atlas = Atlas::in_memory();
        // A 2x1 bitmap: full coverage, then none.
        let slot = atlas
            .insert(AtlasPage::Coverage, (2, 1), &[255, 0])
            .unwrap();
        let mut f = FrameBuilder::default();
        f.push_glyph([1.0, 1.0], &slot, [0.0, 1.0, 0.0, 1.0]);
        let px = paint_small(&mut f, &atlas, (4, 3));
        assert_eq!(px[4 + 1], 0x0000_ff00);
        assert_eq!(px[4 + 2], 0);
        assert_eq!(px.iter().filter(|&&p| p != 0).count(), 1);
    }

    #[test]
    fn colour_glyphs_keep_their_own_colours() {
        let mut atlas = Atlas::in_memory();
        // One premultiplied texel: half-transparent red.
        let slot = atlas
            .insert(AtlasPage::Color, (1, 1), &[128, 0, 0, 128])
            .unwrap();
        let mut f = FrameBuilder::default();
        // The text colour is ignored except for its alpha.
        f.push_glyph([0.0, 0.0], &slot, [0.0, 0.0, 1.0, 1.0]);
        let px = paint_small(&mut f, &atlas, (1, 1));
        assert_eq!(px[0], 0x0080_0000);
    }

    #[test]
    fn glyphs_read_their_own_texels_from_the_atlas() {
        let mut atlas = Atlas::in_memory();
        // Put something first so the second bitmap is not at the origin.
        atlas
            .insert(AtlasPage::Coverage, (3, 3), &[255; 9])
            .unwrap();
        let slot = atlas
            .insert(AtlasPage::Coverage, (1, 2), &[0, 255])
            .unwrap();
        let mut f = FrameBuilder::default();
        f.push_glyph([0.0, 0.0], &slot, [1.0; 4]);
        let px = paint_small(&mut f, &atlas, (1, 2));
        assert_eq!(px, [0, 0x00ff_ffff]);
    }

    #[test]
    fn glyphs_are_clipped_like_rects() {
        let mut atlas = Atlas::in_memory();
        let slot = atlas
            .insert(AtlasPage::Coverage, (4, 1), &[255; 4])
            .unwrap();
        let mut f = FrameBuilder::default();
        f.begin_layer(Some([1, 0, 2, 1]));
        f.push_glyph([0.0, 0.0], &slot, [1.0; 4]);
        let px = paint_small(&mut f, &atlas, (4, 1));
        assert_eq!(px, [0, 0x00ff_ffff, 0x00ff_ffff, 0]);
    }

    const WHITE: u32 = 0x00ff_ffff;

    fn curve(points: [[f32; 2]; 4], width: f32, color: [f32; 4]) -> SegmentInstance {
        SegmentInstance {
            p0: points[0],
            p1: points[1],
            p2: points[2],
            p3: points[3],
            color,
            width,
            pad: [0.0; 3],
        }
    }

    fn line(from: [f32; 2], to: [f32; 2], width: f32) -> SegmentInstance {
        let at = |t: f32| {
            [
                from[0] + (to[0] - from[0]) * t,
                from[1] + (to[1] - from[1]) * t,
            ]
        };
        curve([from, at(1.0 / 3.0), at(2.0 / 3.0), to], width, [1.0; 4])
    }

    fn dot(center: [f32; 2], diameter: f32, color: [f32; 4]) -> SegmentInstance {
        curve([center; 4], diameter, color)
    }

    #[test]
    fn a_horizontal_line_covers_the_pixels_under_it() {
        let atlas = Atlas::in_memory();
        let mut f = FrameBuilder::default();
        // Two pixels tall, centred on the boundary between rows 4 and 5.
        f.push_segment(line([2.0, 5.0], [18.0, 5.0], 2.0));
        let px = paint_small(&mut f, &atlas, (20, 10));
        let at = |x: usize, y: usize| px[y * 20 + x];
        for x in 2..18 {
            assert_eq!((at(x, 4), at(x, 5)), (WHITE, WHITE), "column {x}");
            // A pixel and a half away: the soft edge has ended.
            assert_eq!((at(x, 3), at(x, 6)), (0, 0), "column {x}");
        }
        // Round ends: partly covered just past the end, then nothing.
        assert!(at(1, 4) != 0 && at(1, 4) != WHITE);
        for y in (0..3).chain(7..10) {
            assert!((0..20).all(|x| at(x, y) == 0), "row {y}");
        }
        assert_eq!((at(0, 4), at(19, 5)), (0, 0));
    }

    #[test]
    fn a_dot_is_round_and_symmetric() {
        let atlas = Atlas::in_memory();
        let mut f = FrameBuilder::default();
        // Centred on pixel (5, 5)'s centre, 6 pixels across.
        f.push_segment(dot([5.5, 5.5], 6.0, [1.0; 4]));
        let px = paint_small(&mut f, &atlas, (11, 11));
        let at = |x: usize, y: usize| px[y * 11 + x];
        assert_eq!(at(5, 5), WHITE);
        for y in 0..11 {
            for x in 0..11 {
                let v = at(x, y);
                assert_eq!(v, at(10 - x, y), "mirror x at ({x}, {y})");
                assert_eq!(v, at(x, 10 - y), "mirror y at ({x}, {y})");
                assert_eq!(v, at(y, x), "diagonal at ({x}, {y})");
                // Past radius + 1 (the soft edge), untouched.
                let (dx, dy) = (x.abs_diff(5), y.abs_diff(5));
                if dx * dx + dy * dy >= 16 {
                    assert_eq!(v, 0, "outside at ({x}, {y})");
                }
            }
        }
        // Partial coverage on the rim: grey, not white.
        let [_, r, g, b] = at(5, 8).to_be_bytes();
        assert!(r == g && g == b && r > 0 && r < 255);
    }

    #[test]
    fn segments_are_clipped_to_the_layer() {
        let atlas = Atlas::in_memory();
        let mut f = FrameBuilder::default();
        f.begin_layer(Some([0, 0, 5, 4]));
        f.push_segment(line([-2.0, 2.0], [12.0, 2.0], 2.0));
        let px = paint_small(&mut f, &atlas, (10, 4));
        for row in [1, 2] {
            let r = &px[row * 10..row * 10 + 10];
            assert_eq!(r, [WHITE, WHITE, WHITE, WHITE, WHITE, 0, 0, 0, 0, 0]);
        }
    }

    #[test]
    fn segments_draw_over_rects_and_under_glyphs() {
        let mut atlas = Atlas::in_memory();
        let slot = atlas.insert(AtlasPage::Coverage, (1, 1), &[255]).unwrap();
        let mut f = FrameBuilder::default();
        // Pushed in the opposite order to the one they draw in.
        f.push_glyph([4.0, 4.0], &slot, [0.0, 1.0, 0.0, 1.0]);
        f.push_segment(line([0.0, 5.0], [10.0, 5.0], 4.0));
        f.push_rect([0.0, 0.0], [10.0, 10.0], [1.0, 0.0, 0.0, 1.0]);
        let px = paint_small(&mut f, &atlas, (10, 10));
        assert_eq!(px[4 * 10 + 4], 0x0000_ff00, "glyph over the line");
        assert_eq!(px[5 * 10 + 4], WHITE, "line over the rect");
        assert_eq!(px[0], 0x00ff_0000, "rect where the line is not");
    }

    #[test]
    fn segments_draw_in_the_order_pushed() {
        let atlas = Atlas::in_memory();
        let mut f = FrameBuilder::default();
        // A line, a white dot over it, then a smaller red dot: a ring.
        f.push_segment(curve(
            [[0.0, 7.5], [5.0, 7.5], [10.0, 7.5], [15.0, 7.5]],
            1.0,
            [0.0, 0.0, 1.0, 1.0],
        ));
        f.push_segment(dot([7.5, 7.5], 10.0, [1.0; 4]));
        f.push_segment(dot([7.5, 7.5], 4.0, [1.0, 0.0, 0.0, 1.0]));
        let px = paint_small(&mut f, &atlas, (15, 15));
        let at = |x: usize, y: usize| px[y * 15 + x];
        assert_eq!(at(7, 7), 0x00ff_0000, "inner dot on top");
        assert_eq!(at(4, 7), WHITE, "ring covers the line");
        assert_eq!(at(0, 7) & 0xff, 0xff, "line outside the ring");
    }

    #[test]
    fn a_curve_passes_through_its_endpoints() {
        let atlas = Atlas::in_memory();
        let mut f = FrameBuilder::default();
        // A lane change: down from (2.5, 2.5), across, down into (17.5, 17.5).
        f.push_segment(curve(
            [[2.5, 2.5], [2.5, 10.0], [17.5, 10.0], [17.5, 17.5]],
            2.0,
            [1.0; 4],
        ));
        let px = paint_small(&mut f, &atlas, (20, 20));
        let at = |x: usize, y: usize| px[y * 20 + x];
        assert_eq!((at(2, 2), at(17, 17)), (WHITE, WHITE));
        // It bends through the middle, not along the straight diagonal's
        // far corners.
        assert!(at(10, 10) != 0);
        assert_eq!((at(17, 2), at(2, 17)), (0, 0));
    }

    #[test]
    fn offscreen_frames_capture_as_rgba() {
        let mut cpu = Cpu::offscreen(2, 1);
        let atlas = Atlas::in_memory();
        let mut f = FrameBuilder::default();
        f.push_rect([1.0, 0.0], [1.0, 1.0], [1.0, 0.5, 0.0, 1.0]);
        let (status, cap) = cpu.render(&mut f, atlas.cpu().unwrap(), (BLACK, 0.0), true);
        assert_eq!(status, FrameStatus::Presented);
        let cap = cap.unwrap();
        assert_eq!((cap.width, cap.height), (2, 1));
        assert_eq!(cap.rgba, [0, 0, 0, 255, 255, 128, 0, 255]);
    }

    #[test]
    fn resizing_ignores_zero_and_caps_huge_sizes() {
        let mut cpu = Cpu::offscreen(10, 10);
        cpu.resize(0, 50);
        assert_eq!(cpu.size(), (10, 10));
        cpu.resize(MAX_SIDE + 1, 3);
        assert_eq!(cpu.size(), (MAX_SIDE, 3));
    }
}
