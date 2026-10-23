//! Shaping: turning a line of text into positioned glyphs.
//!
//! Shaping is the step between "a string" and "pictures on screen". The
//! shaper looks up which glyph the font uses for each character, applies the
//! font's rules for combining marks and substitutions, and says how far to
//! advance after each glyph. Its output is glyph ids with positions; turning
//! a glyph id into a bitmap is a separate step, rasterizing (see `raster`).
//!
//! The editor is a monospace grid, so after shaping, each *grapheme* (what a
//! reader sees as one character) is snapped to its column from
//! [`nimbus_core::column`], the same function the cursor uses. That keeps the
//! cursor and the text aligned even where the font disagrees: CJK characters
//! are two columns wide but this font has no CJK glyphs, and its combining
//! marks advance a full cell.
//!
//! **Ligatures (M16).** A programming font may draw `->` or `!=` as one
//! joined glyph. Swash applies the font's ligature features (`liga`, `calt`,
//! `clig`) by default; with ligatures off ([`Shaper::set_ligatures`]) they
//! are switched off explicitly. A ligature arrives as one glyph for several
//! characters, and the grid rule above already places it: the glyph goes at
//! the first character's column and the characters it swallowed draw
//! nothing, so every column, and with it the cursor and selections, stays
//! where it was. When the cursor or a selection edge falls inside a
//! ligature, the line is shaped again with ligatures off, so the characters
//! show apart while the cursor sits between them, as Kitty and VS Code do.

use nimbus_core::column;
use swash::shape::ShapeContext;
use unicode_segmentation::UnicodeSegmentation;

use crate::font::Font;

/// One positioned glyph in a shaped line.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ShapedGlyph {
    /// Glyph id in the font. 0 is the font's "missing glyph" box.
    pub id: u16,
    /// Horizontal position in pixels from the start of the line.
    pub x: f32,
    /// Vertical offset from the baseline in pixels, positive up. Zero for
    /// most glyphs; non-zero for marks the font positions above or below.
    pub y: f32,
    /// Byte offset in the line of the grapheme this glyph belongs to.
    pub cluster: u32,
    /// Set when this is the missing-glyph box for a grapheme the font
    /// lacks: where that grapheme's cells are, so a glyph from a fallback
    /// font can be centred in them instead.
    pub missing: Option<MissingCells>,
}

/// The cells of a grapheme the font has no glyph for.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MissingCells {
    /// Left edge of the first cell, in pixels from the start of the line.
    pub x: f32,
    /// How many cells it takes: 2 for most CJK characters and emoji.
    pub cols: u8,
    /// Its length in bytes (at most 255).
    pub len: u8,
}

/// Grid parameters for laying out one line.
#[derive(Debug, Clone, Copy)]
pub struct Grid {
    /// Width of one column in pixels.
    pub cell_width: f32,
    /// Columns per tab stop.
    pub tab_width: usize,
    /// Column at which the shaped text starts within its line. Non-zero
    /// when only the visible part of a long line is shaped; tab stops are
    /// still measured from the start of the line. Glyph positions stay
    /// relative to the start of the shaped text.
    pub start_col: usize,
}

/// A raw shaper glyph before grid placement.
#[derive(Debug, Clone, Copy)]
struct Raw {
    id: u16,
    x: f32,
    y: f32,
    cluster: u32,
    /// End of the source text its cluster covers, exclusive.
    end: u32,
    /// The cluster is a ligature: one glyph run for several characters.
    ligature: bool,
}

/// Reusable shaping state.
///
/// Holds swash's shaping cache and scratch space so that shaping a line
/// allocates nothing once the buffers have grown to fit typical lines.
pub struct Shaper {
    ctx: ShapeContext,
    scratch_text: String,
    raw: Vec<Raw>,
    /// Let the font join characters into ligatures.
    ligatures: bool,
}

/// Font features that join characters: standard ligatures, contextual
/// alternates (how Fira Code and `JetBrains Mono` build theirs) and
/// contextual ligatures. Turned off unless ligatures are wanted.
const NO_LIGATURES: [(&str, u16); 3] = [("liga", 0), ("calt", 0), ("clig", 0)];

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

#[expect(
    clippy::cast_precision_loss,
    reason = "column counts are far below 2^24, exact in f32"
)]
fn cols(n: usize) -> f32 {
    n as f32
}

impl Shaper {
    /// Empty shaping state.
    #[must_use]
    pub fn new() -> Self {
        Self {
            ctx: ShapeContext::new(),
            scratch_text: String::new(),
            raw: Vec::new(),
            ligatures: false,
        }
    }

    /// Let the font draw ligatures (`ligatures = true` in the config).
    pub fn set_ligatures(&mut self, on: bool) {
        self.ligatures = on;
    }

    /// Whether ligatures are on.
    #[must_use]
    pub fn ligatures(&self) -> bool {
        self.ligatures
    }

    /// Shape `line` with `font` at `size_px` and place each grapheme on the
    /// column grid, writing glyphs into `out` (cleared first).
    ///
    /// `line` must not contain a newline. Tabs produce no glyphs; they only
    /// move later graphemes to the next tab stop. A grapheme containing any
    /// glyph the font lacks draws as a single missing-glyph box centred in
    /// its columns.
    ///
    /// `splits` are byte offsets in `line` where a cursor or selection edge
    /// is. With ligatures on, a ligature spanning one of them is drawn as
    /// its separate characters.
    pub fn shape_line(
        &mut self,
        font: &Font,
        size_px: f32,
        line: &str,
        grid: Grid,
        splits: &[usize],
        out: &mut Vec<ShapedGlyph>,
    ) {
        out.clear();
        if line.is_empty() {
            return;
        }
        self.shape_raw(font, size_px, line, self.ligatures);
        if self.ligatures && splits.iter().any(|&at| self.joined_at(font, line, at)) {
            self.shape_raw(font, size_px, line, false);
        }
        place(&self.raw, font, line, grid, out);
    }

    /// Whether byte `at` of `line` falls inside a ligature in the last
    /// shaping: inside one cluster that swash reports as a ligature, or
    /// between two characters that both lost their own glyph to a
    /// contextual alternate (the way Fira Code builds `->`).
    fn joined_at(&self, font: &Font, line: &str, at: usize) -> bool {
        let at32 = u32::try_from(at).unwrap_or(u32::MAX);
        if self
            .raw
            .iter()
            .any(|r| r.ligature && r.cluster < at32 && at32 < r.end)
        {
            return true;
        }
        let (Some(before), Some(after)) = (
            line.get(..at).and_then(|s| s.char_indices().next_back()),
            line.get(at..).and_then(|s| s.chars().next()),
        ) else {
            return false;
        };
        let substituted = |start: usize, ch: char| {
            let start = u32::try_from(start).unwrap_or(u32::MAX);
            self.raw
                .iter()
                .find(|r| r.cluster == start)
                .is_some_and(|r| r.id != font.glyph_id(ch))
        };
        !before.1.is_whitespace()
            && !after.is_whitespace()
            && substituted(before.0, before.1)
            && substituted(at, after)
    }

    /// Run swash over `line` with tabs replaced by spaces (same byte length,
    /// so cluster offsets still match `line`), filling `self.raw`.
    fn shape_raw(&mut self, font: &Font, size_px: f32, line: &str, ligatures: bool) {
        self.raw.clear();
        let text = if line.contains('\t') {
            self.scratch_text.clear();
            self.scratch_text
                .extend(line.chars().map(|c| if c == '\t' { ' ' } else { c }));
            self.scratch_text.as_str()
        } else {
            line
        };
        let features: &[(&str, u16)] = if ligatures { &[] } else { &NO_LIGATURES };
        let mut shaper = self
            .ctx
            .builder(font.face())
            .size(size_px)
            .normalized_coords(font.coords())
            .features(features)
            .build();
        shaper.add_str(text);
        let mut pen = 0.0_f32;
        let raw = &mut self.raw;
        shaper.shape_with(|cluster| {
            for glyph in cluster.glyphs {
                raw.push(Raw {
                    id: glyph.id,
                    x: pen + glyph.x,
                    y: glyph.y,
                    cluster: cluster.source.start,
                    end: cluster.source.end,
                    ligature: cluster.is_ligature(),
                });
                pen += glyph.advance;
            }
        });
    }
}

/// One glyph of a grapheme shaped on its own with a fallback font: its id
/// and its offset from the grapheme's pen position, in pixels (y up).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ClusterGlyph {
    /// Glyph id in the fallback font.
    pub id: u16,
    /// Offset right from where the grapheme starts.
    pub dx: f32,
    /// Offset up from the baseline.
    pub dy: f32,
}

impl Shaper {
    /// Shape one grapheme the editor font lacks with the fallback `font`
    /// that has it (M24), as `script` written in `language`, writing its
    /// glyphs to `out` (cleared first). Returns its advance, or `None` if
    /// the font lacks part of it after all.
    ///
    /// Shaping matters for scripts whose letters change shape or order
    /// with their neighbours: a Devanagari grapheme such as `स्ते` is
    /// several glyphs, positioned by the font's rules, not one per
    /// character. The language picks regional forms: told Japanese, a
    /// font with a `locl` feature for Japanese draws `骨` the Japanese
    /// way. Default-ignorable characters the font has no glyph for (a
    /// zero-width joiner, a variation selector) are left out rather than
    /// drawn as boxes.
    pub fn shape_cluster(
        &mut self,
        font: &Font,
        size_px: f32,
        text: &str,
        (script, language): (swash::text::Script, Option<swash::text::Language>),
        out: &mut Vec<ClusterGlyph>,
    ) -> Option<f32> {
        out.clear();
        let mut shaper = self
            .ctx
            .builder(font.face())
            .size(size_px)
            .script(script)
            .language(language)
            .normalized_coords(font.coords())
            .build();
        shaper.add_str(text);
        let mut pen = 0.0_f32;
        let mut ok = true;
        shaper.shape_with(|cluster| {
            let source = text
                .get(cluster.source.start as usize..cluster.source.end as usize)
                .unwrap_or("");
            for glyph in cluster.glyphs {
                if glyph.id == 0 {
                    if source.chars().all(is_default_ignorable) {
                        continue;
                    }
                    ok = false;
                }
                out.push(ClusterGlyph {
                    id: glyph.id,
                    dx: pen + glyph.x,
                    dy: glyph.y,
                });
                pen += glyph.advance;
            }
        });
        (ok && !out.is_empty()).then_some(pen)
    }
}

/// Characters that draw nothing on their own and only steer their
/// neighbours: joiners and variation selectors.
#[must_use]
pub fn is_default_ignorable(c: char) -> bool {
    matches!(
        u32::from(c),
        0x200B..=0x200F | 0x2060..=0x2064 | 0xFE00..=0xFE0F | 0xE0100..=0xE01EF | 0x034F
    )
}

/// Place raw shaper glyphs on the column grid: each grapheme at its column
/// from [`nimbus_core::column`], whatever advance the font gave it.
fn place(raw: &[Raw], font: &Font, line: &str, grid: Grid, out: &mut Vec<ShapedGlyph>) {
    let cell = grid.cell_width;
    let mut col = 0usize;
    let base_col = grid.start_col;
    let mut j = 0usize;
    for (start, g) in line.grapheme_indices(true) {
        let end = start + g.len();
        let w = column::grapheme_cols(g, base_col + col, grid.tab_width);
        let first = j;
        while j < raw.len() && (raw[j].cluster as usize) < end {
            j += 1;
        }
        let glyphs = &raw[first..j];
        let x0 = cols(col) * cell;
        if g == "\u{1a}" {
            // The stand-in for a byte that is not UTF-8 (one byte for
            // one byte, so offsets stay put): draw it as the
            // replacement character, or the missing-glyph box.
            out.push(ShapedGlyph {
                id: font.glyph_id('\u{fffd}'),
                x: x0,
                y: 0.0,
                cluster: u32::try_from(start).unwrap_or(u32::MAX),
                missing: None,
            });
        } else if g == "\t" || glyphs.is_empty() {
            // Nothing to draw.
        } else if glyphs.iter().any(|r| r.id == 0) {
            // The font lacks part of this grapheme: one box, centred.
            // The missing-glyph box is one cell wide in this font.
            out.push(ShapedGlyph {
                id: 0,
                x: x0 + (cols(w) - 1.0) * cell / 2.0,
                y: 0.0,
                cluster: glyphs[0].cluster,
                missing: Some(MissingCells {
                    x: x0,
                    cols: u8::try_from(w).unwrap_or(u8::MAX),
                    len: u8::try_from(g.len()).unwrap_or(u8::MAX),
                }),
            });
        } else {
            let base = glyphs[0].x;
            for r in glyphs {
                out.push(ShapedGlyph {
                    id: r.id,
                    x: x0 + (r.x - base),
                    y: r.y,
                    cluster: r.cluster,
                    missing: None,
                });
            }
        }
        col += w;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shape(line: &str) -> (Vec<ShapedGlyph>, f32) {
        let font = Font::bundled().unwrap();
        let cell = font.metrics(14.0).cell_width;
        let mut out = Vec::new();
        Shaper::new().shape_line(
            &font,
            14.0,
            line,
            Grid {
                cell_width: cell,
                tab_width: 4,
                start_col: 0,
            },
            &[],
            &mut out,
        );
        (out, cell)
    }

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 0.01
    }

    #[test]
    fn ascii_glyphs_sit_on_the_cell_grid() {
        let (out, cell) = shape("fn main() { let x = 1; }");
        assert_eq!(out.len(), 24);
        for (i, g) in (0u16..).zip(out.iter()) {
            assert!(close(g.x, cell * f32::from(i)), "glyph {i}: {}", g.x);
            assert_eq!(g.cluster, u32::from(i));
        }
    }

    #[test]
    fn tabs_draw_nothing_and_reach_the_next_stop() {
        let (out, cell) = shape("a\tb");
        assert_eq!(out.len(), 2);
        assert!(out.iter().all(|g| g.id != 0));
        assert!(close(out[1].x, 4.0 * cell));
        let (out, cell) = shape("\tx");
        assert!(close(out[0].x, 4.0 * cell));
    }

    #[test]
    fn combining_mark_does_not_push_the_next_grapheme() {
        let (out, cell) = shape("e\u{301}x");
        let x = out.last().unwrap();
        assert!(close(x.x, cell), "x at {}", x.x);
    }

    #[test]
    fn cjk_is_one_box_in_two_cells() {
        let (out, cell) = shape("日x");
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].id, 0);
        assert!(close(out[0].x, 0.5 * cell));
        assert!(close(out[1].x, 2.0 * cell));
    }

    #[test]
    fn tab_after_wide_chars_uses_display_columns() {
        let (out, cell) = shape("日日\tx");
        assert!(close(out.last().unwrap().x, 8.0 * cell));
    }

    #[test]
    fn zwj_emoji_is_one_box() {
        let (out, cell) = shape("👨‍👩‍👧x");
        assert_eq!(out.iter().filter(|g| g.id == 0).count(), 1);
        assert!(close(out.last().unwrap().x, 2.0 * cell));
    }

    #[test]
    fn tab_stops_count_from_the_line_start_when_shaping_a_span() {
        let font = Font::bundled().unwrap();
        let cell = font.metrics(14.0).cell_width;
        let mut out = Vec::new();
        // The span starts at column 2, so its tab reaches column 4: the x
        // after it is 2 columns from the start of the span.
        let grid = Grid {
            cell_width: cell,
            tab_width: 4,
            start_col: 2,
        };
        Shaper::new().shape_line(&font, 14.0, "\tx", grid, &[], &mut out);
        assert!(close(out[0].x, 2.0 * cell));
    }

    #[test]
    fn a_cluster_the_font_has_shapes_with_its_advance() {
        let font = Font::bundled().unwrap();
        let mut out = Vec::new();
        let script = (swash::text::Script::Latin, None);
        let advance = Shaper::new()
            .shape_cluster(&font, 14.0, "e\u{301}", script, &mut out)
            .unwrap();
        assert_ne!(out, []);
        assert!(advance > 0.0);
        assert!(out.iter().all(|g| g.id != 0));
    }

    #[test]
    fn a_cluster_the_font_lacks_is_refused() {
        let font = Font::bundled().unwrap();
        let mut out = Vec::new();
        let script = (swash::text::Script::Han, None);
        assert_eq!(
            Shaper::new().shape_cluster(&font, 14.0, "漢", script, &mut out),
            None
        );
    }

    #[test]
    fn variation_selectors_are_ignorable() {
        assert!(is_default_ignorable('\u{fe0f}'));
        assert!(is_default_ignorable('\u{200d}'));
        assert!(!is_default_ignorable('a'));
    }

    #[test]
    fn reshaping_reuses_the_output_buffer() {
        let font = Font::bundled().unwrap();
        let grid = Grid {
            cell_width: font.metrics(14.0).cell_width,
            tab_width: 4,
            start_col: 0,
        };
        let mut shaper = Shaper::new();
        let mut out = Vec::new();
        shaper.shape_line(
            &font,
            14.0,
            "a long enough line of text",
            grid,
            &[],
            &mut out,
        );
        let cap = out.capacity();
        shaper.shape_line(&font, 14.0, "short", grid, &[], &mut out);
        assert_eq!(out.len(), 5);
        assert_eq!(out.capacity(), cap);
    }

    /// Raw glyphs as a ligature font would give them for `line`: one glyph
    /// per character (the bundled font's), except that each `->`, `=>` and
    /// `!=` becomes one two-cell ligature glyph (id 900) and its second
    /// character draws
    /// nothing. Lets the grid rule be tested without a ligature font.
    fn fake_raw(line: &str, ligatures: bool, cell: f32) -> Vec<Raw> {
        let font = Font::bundled().unwrap();
        let b = line.as_bytes();
        let mut raw = Vec::new();
        let mut i = 0;
        let mut pen = 0.0;
        while i < b.len() {
            let pair = b.get(i..i + 2);
            let lig = ligatures && matches!(pair, Some(b"->" | b"=>" | b"!="));
            let n = if lig { 2 } else { 1 };
            raw.push(Raw {
                id: if lig {
                    900
                } else {
                    font.glyph_id(char::from(b[i]))
                },
                x: pen,
                y: 0.0,
                cluster: u32::try_from(i).unwrap(),
                end: u32::try_from(i + n).unwrap(),
                ligature: lig,
            });
            pen += cell * f32::from(u8::try_from(n).unwrap());
            i += n;
        }
        raw
    }

    proptest::proptest! {
        /// Ligatures change glyphs, never columns: with them on or off,
        /// every glyph drawn sits at the column `nimbus_core::column` gives
        /// its first character, and the columns are the same both ways.
        #[test]
        fn ligatures_keep_every_column(line in "[-=>!a <\\t]{0,40}") {
            let font = Font::bundled().unwrap();
            let cell = font.metrics(14.0).cell_width;
            let grid = Grid { cell_width: cell, tab_width: 4, start_col: 0 };
            let mut on = Vec::new();
            let mut off = Vec::new();
            place(&fake_raw(&line, true, cell), &font, &line, grid, &mut on);
            place(&fake_raw(&line, false, cell), &font, &line, grid, &mut off);
            for g in on.iter().chain(&off) {
                let col = column::col_at_from(&line, g.cluster as usize, 0, 4);
                proptest::prop_assert!(close(g.x, cols(col) * cell), "{line:?} {g:?}");
            }
            // Every glyph with ligatures on is at a column some glyph had
            // with them off: nothing moved.
            for g in &on {
                proptest::prop_assert!(off.iter().any(|o| o.cluster == g.cluster && close(o.x, g.x)));
            }
        }
    }

    #[test]
    fn a_ligature_takes_the_first_cell_and_its_partner_draws_nothing() {
        let font = Font::bundled().unwrap();
        let cell = font.metrics(14.0).cell_width;
        let grid = Grid {
            cell_width: cell,
            tab_width: 4,
            start_col: 0,
        };
        let mut out = Vec::new();
        place(&fake_raw("a->b", true, cell), &font, "a->b", grid, &mut out);
        let ids: Vec<_> = out.iter().map(|g| (g.id, g.cluster)).collect();
        let (a, b) = (font.glyph_id('a'), font.glyph_id('b'));
        assert_eq!(ids, [(a, 0), (900, 1), (b, 3)]);
        assert!(close(out[2].x, 3.0 * cell));
    }

    #[test]
    fn a_split_inside_a_ligature_is_found() {
        let font = Font::bundled().unwrap();
        let cell = font.metrics(14.0).cell_width;
        let mut shaper = Shaper::new();
        shaper.raw = fake_raw("a->b", true, cell);
        assert!(shaper.joined_at(&font, "a->b", 2));
        assert!(!shaper.joined_at(&font, "a->b", 1));
        assert!(!shaper.joined_at(&font, "a->b", 3));
    }

    /// Shapes with a real ligature font: set `NIMBUS_LIGATURE_FONT` to a
    /// font file such as `FiraCode-Regular.ttf` and run with `--ignored`.
    #[test]
    #[ignore = "needs a ligature font named by NIMBUS_LIGATURE_FONT"]
    fn a_real_ligature_font_keeps_the_grid() {
        let path = std::env::var("NIMBUS_LIGATURE_FONT").unwrap();
        let data: &'static [u8] = Box::leak(std::fs::read(path).unwrap().into_boxed_slice());
        let font = Font::from_data(data, 0).unwrap();
        let cell = font.metrics(14.0).cell_width;
        let grid = Grid {
            cell_width: cell,
            tab_width: 4,
            start_col: 0,
        };
        let line = "a -> b != c => d";
        let mut shaper = Shaper::new();
        let mut off = Vec::new();
        shaper.shape_line(&font, 14.0, line, grid, &[], &mut off);
        shaper.set_ligatures(true);
        let mut on = Vec::new();
        shaper.shape_line(&font, 14.0, line, grid, &[], &mut on);
        println!("off {off:?}\non  {on:?}");
        assert_ne!(on, off, "the font should change something");
        for g in &on {
            let col = column::col_at_from(line, g.cluster as usize, 0, 4);
            assert!(close(g.x, cols(col) * cell) || g.x > cols(col) * cell);
        }
        // With the cursor inside `->`, the plain glyphs come back.
        let mut split = Vec::new();
        shaper.shape_line(&font, 14.0, line, grid, &[3], &mut split);
        assert_eq!(split, off);
    }
}
