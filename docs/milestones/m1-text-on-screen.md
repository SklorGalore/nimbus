# M1 — Text on screen

**Status: done** (2026-10-15)

## Goal

Render a hard-coded paragraph of monospace text, crisp at any scale factor,
through our own shaping, glyph atlas and instanced pipelines, and bring cold
start back under 100 ms.

## Done when

- [x] The window shows a built-in sample: ASCII code, accented Latin
      (`naïve café`), a tab-indented line, box-drawing characters, a few Nerd
      Font icons, and one glyph missing from the font, which draws as the
      font's `.notdef` box.
- [x] Text is crisp on a Retina display, checked from a `--screenshot` capture.
- [x] **Manual, owner confirmed:** text is crisp and scale changes
      re-rasterize cleanly. The 1x path also renders correctly with
      `NIMBUS_SCALE=1`.
- [x] **Manual, owner confirmed:** resizing does not blur, stretch or reflow
      the text, including past 2048 physical pixels.
- [x] A filled rectangle (a stand-in cursor) draws behind the glyphs on line 1,
      proving the rect pipeline and draw order.
- [x] `nimbus --smoke` first frame is within the startup budget in release.
      The original 100 ms target was below the platform floor (see Results);
      the owner set the budget to under 500 ms. Measured 116–132 ms warm.
- [x] Layout and atlas packer have headless unit tests.
- [x] A debug log line reports atlas occupancy and glyph count per frame.

## Scope

**In**: one bundled font at one size and weight; shaping per line; glyph
atlas with one page; glyph and rect pipelines; viewport uniform; scale-factor
handling; startup work moved off the main thread.

**Deferred**: editing and scrolling (M2), fonts from config (M4), font
fallback for missing glyphs (M8), bold and italic, ligatures (the
monospace grid assumes one glyph per cell; ligatures are a later setting),
a second atlas page (only needed with many sizes or a huge glyph set; an
`AtlasFull` error is logged and the glyph skipped).

## Design

New modules in `crates/render/src`:

| Module | Owns |
|---|---|
| `font.rs` | `Font`: the bundled Hack Nerd Font Mono bytes (`include_bytes!`) and the swash `FontRef`, and `Metrics` at a given pixel size: ascent, descent, line height (rounded to whole pixels), cell width (advance of `0`). |
| `shape.rs` | `shape_line(&Font, size, &str) -> ShapedLine`: a reused swash `ShapeContext`; output is `Vec<ShapedGlyph { id, x, cluster }>` in pixels. Tabs expand to the next multiple of `tab_width` cells before shaping. |
| `atlas.rs` | `Atlas`: an `R8Unorm` 2048² texture, a shelf packer, and `HashMap<GlyphKey, Slot>`. `GlyphKey { font, glyph_id, size_px: u32 (×64 fixed point), subpx_x: u8 (0..4) }`. Rasterizes on miss with a reused swash `ScaleContext`, uploads with `queue.write_texture`. |
| `pipeline.rs` | `GlyphPipeline` and `RectPipeline`: WGSL shaders, bind group layouts, a growable instance buffer each. |
| `frame.rs` | `FrameBuilder`: two reusable `Vec`s of instances; `push_rect`, `push_text_line`; `finish` uploads and records two draws. |
| `gpu.rs` | Today's `Gpu`, moved out of `lib.rs`, plus `GpuInit`, the off-thread setup. |

**Instances**, all `#[repr(C)]` with `bytemuck::Pod`:

```rust
struct RectInstance  { pos: [f32; 2], size: [f32; 2], color: [f32; 4] }
struct GlyphInstance { pos: [f32; 2], size: [f32; 2], uv_min: [f32; 2], uv_max: [f32; 2], color: [f32; 4] }
```

**Shaders**: one uniform, the viewport size in physical pixels. The vertex
shader builds a quad from `vertex_index` (0..4, triangle strip), so there is
no vertex buffer, only the instance buffer. Positions are converted from
pixels (origin top left) to clip space in the shader. The glyph fragment
shader samples coverage from the atlas with a nearest-neighbour sampler,
because glyphs are drawn at exactly their rasterized size, applies the
contrast curve, and outputs premultiplied colour with standard alpha
blending.

**Pixel snapping**: baselines land on whole pixels. Glyph x positions are
split into whole pixels plus a quarter-pixel bucket, which selects the
`subpx_x` variant from the atlas. With a monospace font the same glyph
appears in at most four variants.

**Scale factor**: logical font size (points, from a constant in M1) times the
window scale factor gives pixel size. On `ScaleFactorChanged` the atlas is
cleared, metrics recomputed and a redraw requested.

**Startup path**: `main` spawns a thread before creating the event loop. It
creates the wgpu instance, requests an adapter with no surface, opens the
device, parses the font, and compiles both shader modules. The main thread
creates the window in `resumed`, then joins the thread, creates the surface
from the shared instance, checks `adapter.is_surface_supported`, and
configures it. If the adapter turns out incompatible, fall back to the M0
path: request a new adapter with `compatible_surface` set. Log the time each
phase takes so later regressions are visible.

## Tasks

1. Add the bundled font, Hack Nerd Font Mono Regular, with its licence file
   in `crates/render/assets/`.
2. Move `Gpu` into `gpu.rs`. No behaviour change.
3. `font.rs`: load the font, compute metrics, unit-test metrics are positive
   and line height is a whole number of pixels.
4. `atlas.rs` packer only, no GPU: shelf packing with tests for fill order,
   rejection when full, and no overlapping slots.
5. `atlas.rs` GPU side: texture, rasterize with swash, upload, cache lookup.
6. `pipeline.rs` rect pipeline and `frame.rs`; draw the stand-in cursor.
7. `pipeline.rs` glyph pipeline; draw one hard-coded glyph.
8. `shape.rs` and `push_text_line`; draw the sample paragraph. Headless test:
   for a fixed string, glyph x positions are multiples of the cell width.
9. Scale-factor handling. Test by dragging between a Retina and a 1x
   display, and add a `NIMBUS_SCALE` environment variable that overrides the
   scale factor in debug builds. winit's own override, `WINIT_X11_SCALE_FACTOR`,
   only works on X11.
10. Off-thread GPU and font setup; record phase timings; confirm cold start.
11. Fill in Results; update `DESIGN.md` if anything changed.

## Concepts

- **Shaping versus rasterizing.** Shaping turns text into positioned glyph
  ids. Rasterizing turns one glyph id into a coverage bitmap.
- **Glyph atlas.** Why many small bitmaps live in one texture, and how shelf
  packing works.
- **Instanced drawing.** One draw call, one small struct per glyph, a quad
  built in the vertex shader.
- **Coordinate spaces.** Logical points, physical pixels, clip space, and UV
  coordinates in the atlas.
- **Premultiplied alpha** and why text looks thinner on dark backgrounds.
- **Why startup moved to a thread**, and why the window itself cannot.

## Risks

| Risk | Fallback |
|---|---|
| swash API differs from docs or examples, as wgpu's did | Read the crate source under `~/.cargo/registry/src`, as with wgpu |
| Text looks blurry or too thin | Check pixel snapping first, then tune the contrast curve; compare against TextEdit at the same size |
| Atlas work balloons | Swap the packer for `etagere`; if the whole atlas fights us, use `glyphon` for atlas and shader and keep our own layout (`DESIGN.md` §8) |
| Off-thread adapter is incompatible with the surface on some platform | Detect and fall back to the surface-aware request; cold start is worse only there |
| Cold start still over 100 ms | Profile with Instruments; candidates are shader compile (cache the pipeline), font parse (smaller font), window show (outside our control, record it) |

## Results

| Metric | Value |
|---|---|
| First frame, release, warm | 116–132 ms (timer from the top of `main`) |
| First frame, release, first run after a rebuild | about 225 ms (macOS compiles the shaders once per binary) |
| Our own work on the startup path | about 3 ms: GPU context 10 ms on a background thread, fully overlapped; pipelines and atlas 1.5 ms; first frame 1.5 ms |
| Release binary | 7.3 MB (the font is 2.7 MB) |
| Tests | 26 across the workspace |

**Startup finding.** The 100 ms budget cannot be met by optimising our code
on this machine. Warm phases, measured from the top of `main`:

| Phase | Time |
|---|---|
| `EventLoop::new` (AppKit application setup) | about 32 ms |
| until winit calls `resumed` | about 20 ms |
| `create_window` | about 25 ms |
| surface, pipelines, atlas | about 1.5 ms |
| until AppKit asks for the first redraw (window shown) | about 43 ms |
| acquire, draw, present | about 1.5 ms |

Drawing earlier is not possible: a frame rendered right after window creation
is reported as occluded and skipped, because the window is not visible yet.

**M0's numbers were wrong.** M0 started its timer in `App::new`, after
`EventLoop::new`, so it left out about 30 ms. Measured from `main`, M0's real
warm first frame was about 125 ms. The GPU was never the slow part either: M0
spent about 3 ms on GPU setup.

**Surprises**

- The bundled font changed at the owner's request from JetBrains Mono to Hack
  Nerd Font Mono, which is 2.7 MB instead of 270 KB.
- macOS refused external screenshots without Screen Recording permission, so
  a `--screenshot FILE.bmp` debug option was added. It reads the frame back
  from the GPU. It brings forward part of M8's screenshot test.
- wgpu panics on validation errors by default, which aborts a release build;
  `Gpu::new` now installs a logging error handler.
- Resizing the window past 2048 physical pixels failed with a wgpu
  validation error. `Limits::downlevel_defaults()` caps textures, including
  the surface, at 2048 px, smaller than a large Retina window. Found by the
  owner while resizing. The device now takes texture size limits from the
  adapter (16384 on the M5 Pro), and the surface size is clamped to the
  device limit as a backstop. Verified with a 2800×1600 surface.
- The app handled no keys in M0, so Cmd+W did nothing. Primary+W and
  Primary+Q now close the window until M4's action registry takes over.

**End-of-milestone review** (code-review skill, high) found ten issues, nine
fixed:

- Reconfiguring the surface while holding a Suboptimal frame.
- Skipped frames never scheduling a redraw. `render` now returns a `FrameStatus`.
- A full atlas re-rasterizing every frame. It now resets at the start of the next frame.
- Surface format picking that could choose an HDR float format.
- `--screenshot` exiting 0 on failure.
- GPU-thread log lines lost before logger init.
- Shaper y offsets dropped.
- Stale texels left after an atlas clear.
- A misplaced doc comment.

Not fixed: moving shader compilation onto the GPU thread. Measured pipeline
creation is 1.5 ms warm, so it would not help the budget.

**Not covered by tests**: the atlas-full reset path, which needs a GPU, and
vertical offsets for combining marks.

**Design changes**: `Gpu::render` returns `FrameStatus`; frame capture added;
startup budget changed from 100 ms to under 500 ms total.
