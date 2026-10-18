# M16 — Rendering, second pass

**Status: done** (2026-10-18). Written on 2026-10-18 from the rendering items the
backlog kept unscheduled since M8 (colour emoji, ligatures, scrollbar
fade, the linear-blending experiment) plus the two things a user of a
GPU editor asks for first: changing the font size without editing the
config, and seeing whitespace and indentation. Approved the same day:
the zoom is remembered across sessions unless `remember_zoom = false`
(`DESIGN.md` §9).

## Goal

Make the text on screen look finished: colour emoji, optional
ligatures, zoom, visible whitespace and indent guides, with every frame
still drawn on demand and within the typing budget.

## Done when

- [x] **Colour emoji**: 🎉 in a file draws in colour from a system
      emoji font (Apple Color Emoji, Segoe UI Emoji, Noto Color Emoji)
      through M8's fallback path, on a second atlas page in `Rgba8Unorm`
      beside the `R8Unorm` one. Fonts with only outlines keep drawing as
      outlines; a font with neither still draws a box.
- [x] **Ligatures**: `ligatures = true` in the config shapes `->`, `=>`,
      `!=` and friends as the font's ligature glyphs (Hack Nerd Font has
      none; the setting applies to a configured font that does). A
      ligature occupies the cells of the characters it replaces, so
      columns, cursor movement and selections are unchanged; the cursor
      inside a ligature shows the split glyph the way Kitty and VS Code
      do. Default off.
- [x] **Zoom**: "Zoom In" (`Primary+=`), "Zoom Out" (`Primary+-`) and
      "Reset Zoom" (`Primary+0`) change the editor font size for the
      session in 1 pt steps between 6 and 72; the atlas is refilled once
      per step and the view keeps its first visible line. The zoom is
      remembered in `state.toml`, never written to `config.toml`;
      `remember_zoom = false` in the config makes every start use
      `font_size` instead.
- [x] **Whitespace and guides**: `render_whitespace = "selection" |
      "all" | "none"` draws `·` for spaces and `→` for tabs in a muted
      theme colour (default `selection`, like VS Code); `indent_guides =
      true` draws a vertical hairline at each indent level, with the
      level enclosing the cursor brighter. Both are toggles in the
      palette too.
- [x] **Scrollbar fade**: the scrollbar fades out one second after the
      last scroll with a single `WaitUntil` timer, like the cursor blink,
      so an idle editor still schedules nothing.
- [x] **Linear blending experiment**: a `--linear-blend` flag renders to
      an sRGB surface with coverage blended in linear light, and
      `--screenshot` of the same file in both modes is compared by eye.
      The result is a decision, recorded in `DESIGN.md` §4.3, not a
      shipped setting unless it wins.
- [x] Budgets: a keystroke with whitespace, guides and ligatures on stays
      under the M2 typing budget on the 10 MB file; the emoji page adds
      at most 16 MB of GPU memory (2048² RGBA) and is created only when
      the first colour glyph is drawn; `--smoke` is unchanged.

## Scope

**In**: the RGBA atlas page and the shader branch for it, the ligature
cell model, zoom actions and state, whitespace and guide rendering,
scrollbar fade, the blending experiment.

**Deferred**: proportional fonts in UI chrome (M20); font fallback per
script beyond what M8 does; subpixel (LCD) antialiasing, which needs
per-channel coverage and a surface that is not composited; variable fonts.

## Design

**Two atlas pages, one shader.** The glyph instance gains a page index
and a flag saying whether the texel is coverage (`R8`, multiplied by the
text colour) or colour (`RGBA8`, drawn as is with its own alpha). One
bind group holds both textures; the fragment shader branches on the flag.
Colour glyphs come from `swash::scale` with `Source::ColorBitmap` and
`Source::ColorOutline` ahead of `Outline` in the source list, scaled to
the cell height. The emoji font is found by M8's fallback (`fontdb`,
background thread), which stops skipping colour-only fonts once the RGBA
page exists.

**Ligatures keep the grid.** Shaping already runs per line through
swash; with the `liga` and `calt` features on, a cluster can map several
characters to one glyph. The layout keeps one cell per character for all
column math (`nimbus_core::column` is untouched) and draws the ligature
glyph across its cells. When the cursor or a selection edge falls inside
a ligature, that line is reshaped with ligatures off, the same rule
terminals use. The shaping cache key gains the ligature flag.

**Zoom is state, not config.** `Editor::font_size` starts from the config
and is changed by the actions; `Gpu::set_font_size` rebuilds metrics and
clears the atlas, as the scale-factor change path already does. The view
keeps its anchor (logical line, visual row), so zoom does not scroll.

**Whitespace and guides are rects and glyphs**, drawn from the same
visible-line pass as text: `·` and `→` are glyph instances at the cell,
guides are one-pixel rects per indent level per visual row, computed from
the line's leading whitespace (already measured for auto-indent). On long
lines (over 64 KB) only the visible window is examined, as M7 requires.

## Tasks

1. RGBA atlas page: `AtlasPage` enum, second texture and sampler in the
   bind group, page index and colour flag in `GlyphInstance`, shader
   branch; unit test that packing into the colour page never touches the
   coverage page.
2. Colour glyph rasterization through swash's colour sources; `fontdb`
   fallback admits colour fonts; measure GPU memory before and after the
   first emoji.
3. Ligature shaping behind the setting, cell-span glyphs, reshape on
   cursor-inside; property test that column math gives the same answers
   with ligatures on and off.
4. Zoom actions, `state.toml` entry, the `remember_zoom` setting, atlas
   refill, anchor kept; measure
   the refill time at 14 and 24 pt.
5. Whitespace rendering with the three modes and the theme colour
   `ui.whitespace`; palette toggle.
6. Indent guides with the active level; `ui.guide` and `ui.guide_active`
   theme colours; long-line window rule.
7. Scrollbar fade timer; confirm with the debug overlay that no frame is
   drawn after the fade ends.
8. Linear-blending experiment behind `--linear-blend`; screenshots of the
   same file both ways; decision line in `DESIGN.md`.
9. Typing benchmark with everything on; Results.

## Concepts

- **Coverage versus colour textures**: why a grayscale atlas cannot hold
  an emoji, and what a second texture format costs.
- **Ligatures and the cell grid**: shaping merges characters, layout
  keeps columns; how the two are reconciled.
- **Gamma and linear blending**: what "blending in gamma space" means,
  why text looks thin or fat depending on it, and how to compare.

## Risks

| Risk | Fallback |
|---|---|
| Apple Color Emoji bitmaps are only at fixed sizes | Pick the nearest bitmap strike and scale it on the GPU; outlines where a font has them |
| Ligature reshaping on every cursor move costs a shaping pass per keystroke | Only the cursor's line is reshaped, and only when the cursor is inside a ligature cluster |
| The linear-blend surface format is not supported on a platform | The flag logs and falls back to the current path; the experiment runs on macOS only |
| Guides and whitespace double the instance count on dense files | Instances per visible row are bounded by columns on screen; measure, and skip guides past 80 levels |

## Results

Built in a Linux container without a display. Headless renders ran on
Mesa's llvmpipe (software Vulkan), so GPU times below are a CPU
standing in for a GPU; the on-screen numbers are the owner's to fill in
on the Apple M5 Pro.

| Metric | Value |
|---|---|
| Linux release binary | 25.2 MB (budget 35 MB) |
| Typing, 10 MB file, 300 keys, everything off: edit + queue | p50 225 µs, p99 1.80 ms, max 2.97 ms |
| Same, whitespace `all` + guides + ligatures on | p50 406 µs, p99 1.70 ms, max 3.01 ms |
| Same, including the GPU frame (llvmpipe), off / on | p99 7.75 ms / 7.16 ms |
| Emoji atlas page | 16 MB, created on the first colour glyph (the debug overlay shows "emoji 0 MB" until then) |
| `--smoke` first frame on the M5 Pro | warm 131–146 ms; 620 ms on the first run after a build (window + GPU ready 303 ms against about 80 ms warm), the same as other branches built the same day (516–632 ms), so it is the new binary's first launch, not M16. Nothing new runs before the first frame: the `font` setting loads on a worker after it |
| Zoom refill, 14 pt and 24 pt | owner to measure: `RUST_LOG=nimbus=info` logs the first frame after each zoom step |

The keystroke cost with everything on is the same as with everything
off at p99; the p50 grows by about 180 µs for the whitespace and guide
instances. Both stay inside the 8 ms frame budget.

**Where the code went**

- Colour page: `crates/render/src/atlas.rs` (`AtlasPage`, the lazy
  colour texture), `raster.rs` (colour sources, premultiplying),
  `shaders/glyph.wgsl` (the `GLYPH_COLOR` flag picks the page).
- Emoji fonts: `fallback.rs` (`is_emoji`, colour fonts first, emoji
  fonts allowed up to `MAX_EMOJI_FILE`).
- Ligatures: `shape.rs` (`joined_at`, the reshape with ligatures off at
  a split, `place` keeping one glyph per cell), `view.rs` (`edges` feeds
  the cursor and selection edges as splits).
- Zoom: `app.rs` (`zoom_to`, `starting_zoom`), `state.rs`
  (`font_size`).
- Whitespace and guides: `view.rs` (`Painter::whitespace`, `guides`,
  `active_guide`).
- Scrollbar fade: `view.rs` (`scrollbar_hide_at`, `SCROLLBAR_FADE`) and
  the `WaitUntil` timer in `app.rs`.
- Linear blending: `gpu.rs` (sRGB surface format), `to_linear` and
  `space()` in both shaders.

**Surprises and design changes**

- **Apple Color Emoji is about 200 MB**, past M8's 64 MB limit on
  fallback fonts, which are copied into memory. Emoji fonts over the
  limit are now allowed up to 512 MB and copied into memory like other
  fallback fonts. Mapping the file instead would page in only the emoji
  drawn but needs new `unsafe`; the owner chose copying, so the first
  emoji costs about 200 MB of memory on macOS. Measure it on the Mac.
- **Hack has no ligatures**, so the setting would do nothing with the
  default font. A new `font` setting takes a family name or a file path
  (`.ttf`, `.otf`, `.ttc`); it loads on a worker after the first frame
  so startup is unchanged.
- **Indent guides default to off**: on by default would change the
  screenshot test's reference, which can only be remade on a Mac.
- **Compare and merge columns draw no whitespace, guides or scrollbar
  fade**: their own markers already fill the gutter. A later pass can
  add them.
- **Emoji ZWJ sequences** (👨‍👩‍👧, flags) still draw as their parts or
  boxes: they need shaping with the emoji font, which the fallback path
  does per character.
- **Linear blending was declined** (`DESIGN.md` §4.3): text got heavier
  on the dark theme and thinner on the light one, and the current gamma
  adjustment already looks right on both. `--linear-blend` stays as a
  measurement flag.
