# M16 — Rendering, second pass

**Status: proposed.** Written on 2026-10-18 from the rendering items the
backlog kept unscheduled since M8 (colour emoji, ligatures, scrollbar
fade, the linear-blending experiment) plus the two things a user of a
GPU editor asks for first: changing the font size without editing the
config, and seeing whitespace and indentation.

## Goal

Make the text on screen look finished: colour emoji, optional
ligatures, zoom, visible whitespace and indent guides, with every frame
still drawn on demand and within the typing budget.

## Done when

- [ ] **Colour emoji**: 🎉 in a file draws in colour from a system
      emoji font (Apple Color Emoji, Segoe UI Emoji, Noto Color Emoji)
      through M8's fallback path, on a second atlas page in `Rgba8Unorm`
      beside the `R8Unorm` one. Fonts with only outlines keep drawing as
      outlines; a font with neither still draws a box.
- [ ] **Ligatures**: `ligatures = true` in the config shapes `->`, `=>`,
      `!=` and friends as the font's ligature glyphs (Hack Nerd Font has
      none; the setting applies to a configured font that does). A
      ligature occupies the cells of the characters it replaces, so
      columns, cursor movement and selections are unchanged; the cursor
      inside a ligature shows the split glyph the way Kitty and VS Code
      do. Default off.
- [ ] **Zoom**: "Zoom In" (`Primary+=`), "Zoom Out" (`Primary+-`) and
      "Reset Zoom" (`Primary+0`) change the editor font size for the
      session in 1 pt steps between 6 and 72; the atlas is refilled once
      per step and the view keeps its first visible line. The zoom is
      remembered in `state.toml`, never written to `config.toml`.
- [ ] **Whitespace and guides**: `render_whitespace = "selection" |
      "all" | "none"` draws `·` for spaces and `→` for tabs in a muted
      theme colour (default `selection`, like VS Code); `indent_guides =
      true` draws a vertical hairline at each indent level, with the
      level enclosing the cursor brighter. Both are toggles in the
      palette too.
- [ ] **Scrollbar fade**: the scrollbar fades out one second after the
      last scroll with a single `WaitUntil` timer, like the cursor blink,
      so an idle editor still schedules nothing.
- [ ] **Linear blending experiment**: a `--linear-blend` flag renders to
      an sRGB surface with coverage blended in linear light, and
      `--screenshot` of the same file in both modes is compared by eye.
      The result is a decision, recorded in `DESIGN.md` §4.3, not a
      shipped setting unless it wins.
- [ ] Budgets: a keystroke with whitespace, guides and ligatures on stays
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
4. Zoom actions, `state.toml` entry, atlas refill, anchor kept; measure
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

_To be filled in when the milestone lands._
