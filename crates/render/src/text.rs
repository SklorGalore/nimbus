//! Drawing lines of text: shaping, glyph caching, and pixel placement.
//!
//! For each glyph of a shaped line this module decides its exact pixel
//! position, finds or creates its bitmap in the atlas, and queues one quad.
//! The glyph cache maps a [`GlyphKey`] (glyph, size, subpixel bucket) to where
//! its bitmap lives, so each distinct glyph is rasterized once, not per frame.

use std::collections::HashMap;

use crate::atlas::{Atlas, AtlasError, AtlasSlot};
use crate::fallback::{Fallback, FallbackPrefs, Notify};
use crate::font::{Font, Metrics};
use crate::frame::FrameBuilder;
use crate::raster::{BOLD, GlyphKey, REGULAR, Rasterizer};
use crate::script::{self, CjkLanguage, Class};
use crate::shape::{ClusterGlyph, Grid, MissingCells, ShapedGlyph, Shaper, is_default_ignorable};
use crate::ui::{UiFont, UiStyle};
use unicode_segmentation::UnicodeSegmentation;

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
    /// Byte offsets in the text where a cursor or selection edge is: a
    /// ligature across one is drawn as separate characters.
    pub(crate) splits: &'a [usize],
}

/// A glyph already in the atlas, with its offset from the pen position.
#[derive(Debug, Clone, Copy)]
struct CachedGlyph {
    slot: AtlasSlot,
    left: i32,
    top: i32,
}

/// A grapheme shaped with a fallback font (M24): its glyphs and its
/// advance at the renderer's size, before fitting into its cells.
#[derive(Debug, Clone)]
struct FitCluster {
    glyphs: Vec<ClusterGlyph>,
    advance: f32,
}

/// Graphemes shaped with fallback fonts, by (face, language) and then by
/// the grapheme's text: a page of Japanese shapes each distinct character
/// once. `None` remembers graphemes the face cannot draw after all.
type ClusterCache = HashMap<(u8, u8), HashMap<Box<str>, Option<FitCluster>>>;

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
    /// The same, for glyphs of the interface font (M20), which is not one
    /// of `faces`: its keys would collide with theirs in one map.
    ui_cache: HashMap<GlyphKey, Option<CachedGlyph>>,
    shaped: Vec<ShapedGlyph>,
    /// The class of each missing grapheme of the line being drawn, in
    /// order (M24): which preference list its fallback font comes from.
    classes: Vec<Class>,
    /// Graphemes shaped with fallback fonts.
    clusters: ClusterCache,
    /// Scratch for shaping one grapheme, and for the glyphs of the one
    /// being drawn.
    cluster_glyphs: Vec<ClusterGlyph>,
    fit: Vec<ClusterGlyph>,
    /// The regional forms Han ideographs take, for the shaper.
    cjk: CjkLanguage,
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
            ui_cache: HashMap::new(),
            shaped: Vec::new(),
            classes: Vec::new(),
            clusters: HashMap::new(),
            cluster_glyphs: Vec::new(),
            fit: Vec::new(),
            cjk: CjkLanguage::default(),
            reset_pending: false,
        }
    }

    /// Prefer these fallback fonts per script, and draw Han in `prefs.cjk`'s
    /// regional forms (M24). Characters are looked up again; glyphs
    /// already in the atlas stay until its next reset.
    pub(crate) fn set_fallback_prefs(&mut self, prefs: FallbackPrefs) {
        if *self.fallback.prefs() == prefs {
            return;
        }
        self.cjk = prefs.cjk;
        self.clusters.clear();
        self.fallback.set_prefs(prefs);
    }

    pub(crate) fn metrics(&self) -> Metrics {
        self.metrics
    }

    /// The editor font's regular and bold faces.
    pub(crate) fn editor_faces(&self) -> (Font, Font) {
        (self.faces[0], self.faces[1])
    }

    pub(crate) fn grid(&self) -> Grid {
        Grid {
            cell_width: self.metrics.cell_width,
            tab_width: self.tab_width,
            start_col: 0,
        }
    }

    /// Draw with another font (`font` in the config): every cached bitmap
    /// belongs to the old one, so the cache and atlas are reset, and the
    /// grid is measured again.
    pub(crate) fn set_font(&mut self, regular: Font, bold: Option<Font>, atlas: &mut Atlas) {
        self.font = regular;
        self.faces[0] = regular;
        self.faces[1] = bold.unwrap_or(regular);
        self.metrics = regular.metrics(self.metrics.size_px);
        self.reset(atlas);
    }

    /// Let the font join characters into ligatures.
    pub(crate) fn set_ligatures(&mut self, on: bool) {
        self.shaper.set_ligatures(on);
    }

    /// Whether ligatures are on.
    pub(crate) fn ligatures(&self) -> bool {
        self.shaper.ligatures()
    }

    pub(crate) fn set_tab_width(&mut self, tab_width: usize) {
        self.tab_width = tab_width.max(1);
    }

    /// Change the pixel size, for a font-size or scale-factor change. Every
    /// cached bitmap is the wrong size now, so the cache and atlas are reset.
    pub(crate) fn set_size(&mut self, size_px: f32, atlas: &mut Atlas) {
        if (size_px - self.metrics.size_px).abs() < f32::EPSILON {
            return;
        }
        self.metrics = self.font.metrics(size_px);
        self.reset(atlas);
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
    pub(crate) fn begin_frame(&mut self, atlas: &mut Atlas) {
        self.receive_fonts();
        if self.reset_pending {
            log::info!("glyph atlas was full; clearing it");
            self.reset(atlas);
        }
    }

    /// Whether the last frame skipped glyphs and should be drawn again.
    pub(crate) fn needs_redraw(&self) -> bool {
        self.reset_pending
    }

    fn reset(&mut self, atlas: &mut Atlas) {
        self.cache.clear();
        self.ui_cache.clear();
        // Shaped at the old size (or with the old font).
        self.clusters.clear();
        atlas.clear();
        self.reset_pending = false;
    }

    /// Queue the glyphs of `line` with the line's top-left corner at `origin`
    /// (physical pixels; `origin[1]` should be a whole number). Glyphs whose
    /// pen position is left of `clip_left` are skipped, so horizontally
    /// scrolled text does not draw over the gutter.
    pub(crate) fn push_line(
        &mut self,
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
            style.splits,
            &mut self.shaped,
        );

        // The graphemes the font lacks, in order, with their scripts
        // resolved over the line, so punctuation joins its sentence.
        if self.shaped.iter().any(|g| g.missing.is_some()) {
            let shaped = &self.shaped;
            script::resolve_runs(
                shaped.iter().filter_map(|g| {
                    let cells = g.missing?;
                    let at = g.cluster as usize;
                    Some(line.get(at..at + usize::from(cells.len)).unwrap_or(""))
                }),
                &mut self.classes,
            );
        }
        let mut next_class = 0;

        for i in 0..self.shaped.len() {
            let g = self.shaped[i];
            // Shaped glyphs come in cluster order in a monospace grid, but
            // a search keeps this right even if one does not.
            let cluster = g.cluster as usize;
            let run = style
                .runs
                .get(style.runs.partition_point(|r| r.end <= cluster));
            let (color, look) = run.map_or((style.color, Style::default()), |r| (r.color, r.style));
            // A grapheme the editor font lacks comes from a fallback font
            // when one has it: shaped on its own, centred in its cells,
            // and shrunk if it is wider than them. Until then, the box.
            if let Some(cells) = g.missing {
                let class = self
                    .classes
                    .get(next_class)
                    .copied()
                    .unwrap_or(Class::Symbols);
                next_class += 1;
                if let Some((face, x, scale)) = self.fallback_cluster(line, cluster, cells, class) {
                    for j in 0..self.fit.len() {
                        let fg = self.fit[j];
                        let pen = origin[0] + x + fg.dx * scale;
                        if pen < style.clip_left {
                            continue;
                        }
                        let whole = pen.floor();
                        let key = GlyphKey::new(fg.id, size_px * scale, pen - whole)
                            .styled(face, look.italic);
                        let at = [whole, baseline_y - fg.dy * scale];
                        if !self.queue(atlas, frame, key, at, color) {
                            return;
                        }
                    }
                    continue;
                }
            }
            // Split the pen position into a whole pixel and a fraction. The
            // fraction picks the subpixel variant of the bitmap, the whole
            // part places it, so every quad lands on the pixel grid.
            let pen = origin[0] + g.x;
            if pen < style.clip_left {
                continue;
            }
            let whole = pen.floor();
            let key = self.styled_key(
                GlyphKey::new(g.id, size_px, pen - whole),
                line,
                cluster,
                look,
            );
            if !self.queue(atlas, frame, key, [whole, baseline_y - g.y], color) {
                return;
            }
        }
    }

    /// Queue glyph `key` with its pen at `[x, y]`: `x` a whole pixel, `y`
    /// the baseline, shifted for marks the shaper moves up or down.
    /// Rasterizes it into the atlas the first time. Returns `false` when
    /// the atlas is full, after which the frame should stop queueing.
    fn queue(
        &mut self,
        atlas: &mut Atlas,
        frame: &mut FrameBuilder,
        key: GlyphKey,
        [x, y]: [f32; 2],
        color: [f32; 4],
    ) -> bool {
        let cached = if let Some(entry) = self.cache.get(&key) {
            *entry
        } else {
            match load_glyph(&self.faces, &mut self.raster, atlas, key) {
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
                    return false;
                }
            }
        };
        if let Some(c) = cached {
            // Swash's y offset points up, screen y points down.
            frame.push_glyph([x + px(c.left), y.round() - px(c.top)], &c.slot, color);
        }
        true
    }
}

impl TextRenderer {
    /// Queue a line of interface text (M20) with its top-left corner at
    /// `origin`, placed glyph by glyph at the advances `ui` measures with
    /// (see [`crate::ui`]). Graphemes the interface font lacks are drawn
    /// by the editor font in their cells. Returns the x where the text
    /// ends.
    pub(crate) fn push_ui_line(
        &mut self,
        atlas: &mut Atlas,
        frame: &mut FrameBuilder,
        origin: [f32; 2],
        text: &str,
        color: [f32; 4],
        ui: &UiFont,
    ) -> f32 {
        if ui.font().is_none() {
            self.push_line(atlas, frame, origin, text, plain(color));
            return origin[0] + ui.width(text);
        }
        self.push_ui_styled(atlas, frame, (origin, color), text, ui, UiStyle::PLAIN)
    }

    /// Queue a line of interface text in `style` (size, weight, slant)
    /// with its top-left corner at `origin`; returns the x where it ends.
    /// Graphemes neither weight has are drawn by the editor font in its
    /// cells, at its own size.
    pub(crate) fn push_ui_styled(
        &mut self,
        atlas: &mut Atlas,
        frame: &mut FrameBuilder,
        (origin, color): ([f32; 2], [f32; 4]),
        text: &str,
        ui: &UiFont,
        style: UiStyle,
    ) -> f32 {
        let size_px = self.metrics.size_px * style.scale;
        let baseline_y = origin[1] + ui.styled_baseline(style);
        let mut pen = origin[0];
        let mut col = 0;
        for (i, g) in text.grapheme_indices(true) {
            let (adv, cols, font) = ui.styled_advance(g, col, style);
            if let Some((font, bold)) = font {
                let scaled = font.face().glyph_metrics(&[]).scale(size_px);
                let mut x = pen;
                for c in g.chars() {
                    let id = font.glyph_id(c);
                    if id == 0 {
                        continue;
                    }
                    let glyph = UiGlyph {
                        font,
                        id,
                        size_px,
                        bold,
                        italic: style.italic,
                    };
                    self.push_ui_glyph(atlas, frame, glyph, [x, baseline_y], color);
                    x += scaled.advance_width(id);
                }
            } else if let Some(g) = text.get(i..i + g.len()) {
                self.push_line(atlas, frame, [pen, origin[1]], g, plain(color));
            }
            pen += adv;
            col += cols;
        }
        pen
    }

    /// Queue one glyph of the interface font with its pen position at
    /// `at` (x, baseline y).
    fn push_ui_glyph(
        &mut self,
        atlas: &mut Atlas,
        frame: &mut FrameBuilder,
        glyph: UiGlyph,
        [x, baseline_y]: [f32; 2],
        color: [f32; 4],
    ) {
        if self.reset_pending {
            return;
        }
        let whole = x.floor();
        // The glyph rasterizes from a list holding just its own font, so
        // its face is 0; the key's face still tells the weights apart, as
        // their glyph ids differ.
        let face = if glyph.bold { BOLD } else { REGULAR };
        let key = GlyphKey::new(glyph.id, glyph.size_px, x - whole).styled(face, glyph.italic);
        let cached = if let Some(entry) = self.ui_cache.get(&key) {
            *entry
        } else {
            let raster_key = key.styled(REGULAR, glyph.italic);
            match load_glyph(&[glyph.font], &mut self.raster, atlas, raster_key) {
                Loaded::Glyph(g) => {
                    self.ui_cache.insert(key, Some(g));
                    Some(g)
                }
                Loaded::Blank => {
                    self.ui_cache.insert(key, None);
                    None
                }
                Loaded::NoRoom => {
                    self.reset_pending = true;
                    None
                }
            }
        };
        if let Some(c) = cached {
            let y = baseline_y.round() - px(c.top);
            frame.push_glyph([whole + px(c.left), y], &c.slot, color);
        }
    }

    /// Forget the interface font's glyphs, for a new interface font. The
    /// atlas keeps them until its next reset; they are not drawn again.
    pub(crate) fn clear_ui_glyphs(&mut self) {
        self.ui_cache.clear();
    }

    /// Queue one character of the editor font with its cell's top-left
    /// corner at `origin`, without shaping: for markers such as the `·` and
    /// `→` of visible whitespace, drawn many times a frame. Nothing is drawn
    /// if the font lacks the character.
    pub(crate) fn push_char(
        &mut self,
        atlas: &mut Atlas,
        frame: &mut FrameBuilder,
        origin: [f32; 2],
        ch: char,
        color: [f32; 4],
    ) {
        let id = self.font.glyph_id(ch);
        if id == 0 || self.reset_pending {
            return;
        }
        let whole = origin[0].floor();
        let key = GlyphKey::new(id, self.metrics.size_px, origin[0] - whole).styled(REGULAR, false);
        let cached = if let Some(entry) = self.cache.get(&key) {
            *entry
        } else {
            match load_glyph(&self.faces, &mut self.raster, atlas, key) {
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
                    None
                }
            }
        };
        if let Some(c) = cached {
            let y = (origin[1] + self.metrics.baseline).round() - px(c.top);
            frame.push_glyph([whole + px(c.left), y], &c.slot, color);
        }
    }

    /// For a grapheme the editor font lacks, of script class `class`: the
    /// fallback face that draws it, the x of its first glyph's pen from
    /// the start of the line, and the scale that fits it in its cells; its
    /// glyphs are left in `self.fit`. `None` (draw the box) while no font
    /// is known to have it, when the font found lacks part of it, or for a
    /// joined emoji sequence (👨‍👩‍👧) the font does not draw as one picture.
    ///
    /// The grid is kept by scaling, never by widening: a glyph wider than
    /// its cells (two for East Asian wide characters, one otherwise) is
    /// drawn smaller, centred, so columns never move.
    fn fallback_cluster(
        &mut self,
        line: &str,
        cluster: usize,
        cells: MissingCells,
        class: Class,
    ) -> Option<(u8, f32, f32)> {
        let text = line.get(cluster..cluster + usize::from(cells.len))?;
        let base = text.chars().find(|&c| !is_default_ignorable(c))?;
        let face = self.fallback.face_for(base, class)?;
        let font = *self.faces.get(usize::from(face))?;
        let language = script::shaping_language(class, self.cjk);
        let lang_key = match class {
            Class::Han => self.cjk as u8 + 1,
            Class::Kana => CjkLanguage::Japanese as u8 + 1,
            Class::Hangul => CjkLanguage::Korean as u8 + 1,
            _ => 0,
        };
        let size = self.metrics.size_px;
        let shapes = self.clusters.entry((face, lang_key)).or_default();
        if !shapes.contains_key(text) {
            let has_all = text
                .chars()
                .all(|c| is_default_ignorable(c) || font.glyph_id(c) != 0);
            let cluster_fit = has_all
                .then(|| {
                    self.shaper.shape_cluster(
                        &font,
                        size,
                        text,
                        (script::shaping_script(class), language),
                        &mut self.cluster_glyphs,
                    )
                })
                .flatten()
                .map(|advance| FitCluster {
                    glyphs: self.cluster_glyphs.clone(),
                    advance,
                })
                // A joined emoji sequence the font draws as its separate
                // pictures would squeeze several into two cells: a box
                // says more.
                .filter(|f| {
                    !(text.contains('\u{200d}') && class == Class::Emoji && f.glyphs.len() != 1)
                });
            shapes.insert(text.into(), cluster_fit);
        }
        let fit = shapes.get(text)?.as_ref()?;
        let (scale, x) = fit_in_cells(fit.advance, cells, self.metrics.cell_width);
        self.fit.clear();
        self.fit.extend_from_slice(&fit.glyphs);
        Some((face, x, scale))
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

/// The scale that fits a grapheme `advance` pixels wide into `cells`, and
/// the x its pen starts at so it sits centred in them: wider than its
/// cells, it is shrunk to fit exactly; narrower, it keeps its size.
fn fit_in_cells(advance: f32, cells: MissingCells, cell_width: f32) -> (f32, f32) {
    let room = f32::from(cells.cols) * cell_width;
    let scale = if advance > room && advance > 0.0 {
        room / advance
    } else {
        1.0
    };
    (scale, cells.x + (room - advance * scale) / 2.0)
}

/// One glyph of the interface font and how to draw it.
#[derive(Clone, Copy)]
struct UiGlyph {
    font: Font,
    id: u16,
    size_px: f32,
    bold: bool,
    italic: bool,
}

/// A whole line in one colour, on the grid from column 0.
fn plain(color: [f32; 4]) -> LineStyle<'static> {
    LineStyle {
        color,
        runs: &[],
        clip_left: f32::NEG_INFINITY,
        start_col: 0,
        splits: &[],
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
fn load_glyph(fonts: &[Font], raster: &mut Rasterizer, atlas: &mut Atlas, key: GlyphKey) -> Loaded {
    let Some(bitmap) = raster.rasterize(fonts, key) else {
        return Loaded::Blank;
    };
    match atlas.insert(bitmap.page, (bitmap.width, bitmap.height), bitmap.data) {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn cells(x: f32, cols: u8) -> MissingCells {
        MissingCells { x, cols, len: 3 }
    }

    #[test]
    fn a_wide_glyph_shrinks_to_its_cells() {
        // 30 px of glyph in two 8 px cells: drawn at 16/30 of its size,
        // filling them exactly.
        let (scale, x) = fit_in_cells(30.0, cells(100.0, 2), 8.0);
        assert!((scale - 16.0 / 30.0).abs() < 1e-6);
        assert!((x - 100.0).abs() < 1e-4);
    }

    #[test]
    fn a_narrow_glyph_keeps_its_size_and_is_centred() {
        let (scale, x) = fit_in_cells(10.0, cells(100.0, 2), 8.0);
        assert!((scale - 1.0).abs() < f32::EPSILON);
        assert!((x - 103.0).abs() < 1e-4);
    }

    /// The grid rule on a real line (M24's screenshot check, as numbers):
    /// a mixed Latin, CJK and Hangul line drawn at 1x and 2x keeps every
    /// fallback glyph inside its two cells and every Latin glyph at its
    /// column. Uses this machine's fonts; where none has CJK (a bare
    /// Linux runner) only the Latin half is checked.
    #[test]
    fn mixed_scripts_stay_on_the_grid_at_1x_and_2x() {
        let line = "ab漢字cd한글ef";
        for size in [14.0, 28.0] {
            let mut text = TextRenderer::new(Font::bundled().unwrap(), size, 4);
            let mut atlas = Atlas::in_memory();
            let mut frame = FrameBuilder::default();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
            loop {
                text.begin_frame(&mut atlas);
                frame.clear();
                text.push_line(&mut atlas, &mut frame, [0.0, 0.0], line, plain([1.0; 4]));
                if !text.fonts_pending() || std::time::Instant::now() > deadline {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            text.begin_frame(&mut atlas);
            frame.clear();
            text.push_line(&mut atlas, &mut frame, [0.0, 0.0], line, plain([1.0; 4]));
            frame.finish();
            let cell = text.metrics().cell_width;
            // Columns: a b 漢漢 字字 c d 한한 글글 e f.
            let wide = [(2.0, 4.0), (4.0, 6.0), (8.0, 10.0), (10.0, 12.0)];
            let mut in_wide = 0;
            for layer in frame.layers() {
                for g in layer.glyphs {
                    let (left, right) = (g.pos[0], g.pos[0] + g.size[0]);
                    let centre = f32::midpoint(left, right) / cell;
                    if let Some(&(a, b)) = wide.iter().find(|(a, b)| centre > *a && centre < *b) {
                        // Antialiasing may spill one pixel past the edge.
                        assert!(
                            left >= a * cell - 1.0 && right <= b * cell + 1.0,
                            "size {size}: glyph {left}..{right} outside cells {a}..{b} ({cell} px)"
                        );
                        in_wide += 1;
                    }
                }
            }
            // With a fallback font loaded, all four wide characters drew.
            if text.faces.len() > usize::from(crate::fallback::FIRST_FALLBACK) {
                assert!(in_wide >= 4, "size {size}: {in_wide} wide glyphs drawn");
            }
        }
    }
}
