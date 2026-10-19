# M23 — Library upgrades

**Status: in progress (2026-10-19). Compatibility mode is built; the
three upgrades wait on stable releases (on 2026-10-19 ropey was still
2.0.0-beta.1, winit 0.31.0-beta.3, and wgpu 30.0.1 was already
current).** Planned 2026-10-18. Written on
2026-10-18 from what M21 deferred: 1.0 shipped on ropey 1.6 and winit
0.30 because ropey 2 and winit 0.31 were still betas (ropey
2.0.0-beta.1, winit 0.31.0-beta.3), and the owner chose to move both
upgrades to the first point release after they are stable. The macOS
emoji picker bug waits on winit too. Approved the same day: it follows
M22 whenever the releases arrive (`DESIGN.md` §9). Finder "Open With"
was in this plan until the owner chose to fix it in 1.0.1 with a small
`objc2` open-files listener, since winit 0.31.0-beta.3 still does not
pass those files on. Compatibility mode (drawing without a GPU, worked
under the name Cumulus) joined the plan on 2026-10-18 at the owner's
request; unlike the upgrades it waits on no release and can go first.

## Goal

Bring the text and window libraries forward once they are stable, and
fix the macOS emoji picker that needs the newer winit, with every
upgrade measured against the numbers it replaces. And keep Nimbus
running on machines with no usable GPU, instead of exiting at startup.

## Done when

- [ ] **Ropey 2**: `RopeBuffer` moves to ropey 2 and the char-to-byte
      layer in `rope.rs` is deleted; the buffer oracle suite, the fuzz
      targets from M21 and every benchmark pass, with numbers before and
      after in the commit.
- [ ] **winit 0.31**: the app moves to winit 0.31 and the matching
      `accesskit_winit`; the API drift is noted in `CLAUDE.md` beside
      the wgpu one; `--smoke` within 10 % of 1.x's number. If winit
      0.31 passes Finder's open-files message on by then, the 1.0.1
      `objc2` listener is replaced by winit's event and its `unsafe`
      removed.
- [ ] **macOS emoji picker**: text inserted from the Character Viewer
      (`Ctrl+Cmd+Space`) arrives in the buffer.
- [ ] **wgpu current**: whatever wgpu release is current when the
      milestone starts, with the same before-and-after numbers.
- [ ] **Compatibility mode** (built 2026-10-19; waiting on the owner's
      run on the Mac for the window and the 16 ms number): when the GPU
      cannot be opened, Nimbus
      starts anyway, drawing on the CPU, and says once that it is in
      compatibility mode; `--compat` (and `compat = true` in the config)
      forces it. The screenshot test also passes in compatibility mode
      within a small per-pixel tolerance, and a frame of a full
      1600x1000 window paints in under 16 ms in release on the owner's
      Mac. The painter draws every primitive the GPU renderer has when
      it lands.

## Scope

**In**: the three upgrades, one commit each; the emoji picker fix they
unlock; compatibility mode.

**Deferred**: anything the new winit offers beyond the two fixes (for
example its new window or IME features) becomes a backlog line, not
part of the upgrade. A terminal version of Nimbus, for SSH sessions
with no display, was weighed for compatibility mode and left out; the
name Cumulus is kept free for it.

**Out**: wgpu's own CPU drivers (WARP on Windows, llvmpipe on Linux) as
a middle step. The owner chose GPU, then our painter, and nothing in
between: one fewer startup path to test, and the same look on every
machine without a GPU.

## Design

**Start condition.** Each upgrade starts when its crate has a stable
release, not before; ropey and winit are independent, so whichever is
released first goes first, and the milestone may land in two point
releases. Until then the plan sits at the top of the backlog as
"Waiting".

**Ropey 2 is a `core` change only.** The `Buffer` trait is in byte
offsets already; ropey 2 indexes by byte too, so the wrapper's
conversions go away and the rest of the workspace does not notice. The
oracle suite and the fuzz targets are what make that safe.

**Upgrades are measured, not assumed.** Each one is a commit with
`--smoke`, the typing benchmark and the binary size before and after in
its message; a regression over 10 % is reverted and the upgrade waits.
This is M21's rule, kept.

**Compatibility mode is a second painter, not a second renderer.**
Every frame is already built on the CPU as two flat lists in
`FrameBuilder` (`render/src/frame.rs`): solid rectangles
(`RectInstance`) and glyph bitmaps placed on whole pixels
(`GlyphInstance`, one atlas texel per screen pixel), in clipped layers.
swash already rasterizes the glyphs on the CPU. The GPU's only work is
filling those rectangles, blending the bitmaps in the text colour and
presenting. The Cumulus painter does the same with two loops over a
`Vec<u32>` of pixels, layer by layer with the clip applied, and shows
the result with `softbuffer` (a small crate that puts a pixel buffer in
a winit window on all three systems, with no `unsafe` on our side).
Everything above `FrameBuilder`, which is all of `app`, does not know
which painter runs.

- **The atlas keeps its pages in memory.** Today glyph bitmaps go
  straight into GPU textures (`Atlas::insert` calls `write_texture`).
  In compatibility mode the same packer places them in two CPU pages (4
  MB coverage, 16 MB colour, the colour page made on the first emoji
  as now), and `GlyphInstance`'s `uv` coordinates index those pages.
- **Blending matches the shader.** The painter does what `glyph.wgsl`
  and `rect.wgsl` do: coverage shaped by the same gamma adjustment,
  premultiplied colour blended in gamma space as
  `out = src + dst × (1 − src.a)`, colour glyphs used as stored. So a
  compatibility screenshot differs from the GPU one only by rounding.
  `--linear-blend` is ignored in compatibility mode.
- **Switching is automatic.** `GpuContext::open` failing (or the surface
  failing to configure) starts the painter instead of exiting, and a
  one-time notice in the status bar says Nimbus is in compatibility
  mode and why. The F12 overlay names the painter. The name users see is
  "compatibility mode" (owner's choice); Cumulus is only the code's
  name for it.
- **It keeps up with the renderer.** The painter covers every primitive
  the GPU renderer has when it lands (today rectangles and glyph
  bitmaps). A later milestone that adds a primitive, such as the git client's
  smooth commit-graph curves (M27), adds it to the painter in the same change
  and checks it against the GPU screenshot (a standing requirement in
  `docs/milestones/README.md`).
- **Headless renders can use it.** `--headless --compat` renders with
  no GPU at all, which would let CI on Linux check pixels.
- **Cost.** A 1600x1000 window is 1.6 million pixels, so a full frame is
  a few milliseconds on one core (estimate), and a Retina window has four
  times that. Rendering is already on demand, so this is paid only on
  input. If the budget is missed, the painter redraws only the rows
  that changed, then splits the frame into bands across threads.

**The emoji picker is checked, not patched.** The bug is winit 0.30
dropping text inserted without a preceding compose. If 0.31 fixes it,
the task is a manual check and a note; if not, a report upstream and the
item stays in the backlog. A patched copy of winit stays out.

## Tasks

1. Ropey 2: migrate, delete the conversion layer, oracle suite, fuzz
   and benchmarks before and after.
2. winit 0.31 and `accesskit_winit`: migrate, `CLAUDE.md` notes,
   `--smoke`.
3. Emoji picker check on the Mac (owner).
4. wgpu current, if newer than 30.
5. Compatibility mode (does not wait on a release, so it may go first):
   CPU atlas pages, the painter and its blending tests against small
   hand-made frames, `softbuffer` presenting, the automatic switch and
   notice, `--compat` and the config key, the screenshot test in both
   modes, the frame-time benchmark.
6. Point release; Results.

## Concepts

- **Byte-indexed ropes**: what ropey 2 changes and why our code gets
  shorter.
- **Software rendering**: what a GPU does for a frame (fill, blend,
  present), and why our simple frames make doing it on the CPU cheap.
- **Alpha blending by hand**: mixing a glyph's coverage with the
  background, the same formula the shader uses.

## Risks

| Risk | Fallback |
|---|---|
| A stable release does not arrive for months | The plan waits; later milestones go ahead of it. Nothing else depends on it except M22's adapter version, which follows winit |
| winit 0.31 changes the event loop enough to touch most of `app.rs` | One commit with no behaviour change, reviewed on its own; the screenshot test and `--smoke` show nothing moved |
| Compatibility mode is too slow on a large Retina window | Redraw only changed rows, then paint in bands on several threads; scrolling may still be less smooth than on a GPU, which the notice says |
| `softbuffer` misbehaves on one system (for example Wayland) | Report upstream; on that system compatibility mode is limited to `--headless` until fixed |

## Results

Partial, 2026-10-19: compatibility mode (task 5). The upgrades (tasks
1 to 4) have not started, because no stable release exists yet: ropey
is at 2.0.0-beta.1 and winit at 0.31.0-beta.3, and wgpu 30.0.1, which
Nimbus already uses, is the newest wgpu.

**What was built.**

- `crates/render/src/soft.rs` is the painter: `draw` clears to the
  background, then for each layer fills its rectangles (`fill_rect`) and
  blends its glyphs (`blend_glyph`) inside the layer's clip, with the
  shader's formula (`over`) and gamma table (`SHAPED`). `span` picks the
  pixels whose centres are inside a shape, as a GPU does. Frames reach
  the window through `softbuffer` (`Cpu::window`).
- `Atlas` (`atlas.rs`) keeps its pages either as textures (`GpuPages`)
  or in memory (`CpuPages`), behind the same packer, so `text.rs` no
  longer passes the GPU queue around.
- `Gpu` (`gpu.rs`) holds one of two painters (`Painter::Wgpu` or
  `Painter::Cpu`); `Gpu::compat` and `Gpu::compat_headless` make the
  second. Nothing in `app` changed in how it draws.
- The switch (`App::resumed` and `start_compat` in `app.rs`): `--compat`
  never starts the GPU thread; `compat = true` ignores its result; a GPU
  that fails to open or to set up the window starts the painter, with a
  status-bar notice saying why. A GPU lost while running
  (`FrameError`) also switches, instead of exiting with unsaved work.
  Headless renders fall back the same way and say so on stderr.
- The F12 overlay has a `painter` line.

**Numbers** (Linux container, x86-64, no GPU; the owner's Mac is
still to be measured):

| | before | after |
|---|---|---|
| release binary (Linux) | 28,833,792 bytes | 28,966,496 bytes (+130 KB) |
| CPU frame, 1600x1000 (`compat_frame_bench`, p50 / worst) | – | 7.4 ms / 12.6 ms |
| CPU frame, 3200x2000 | – | 16.6 ms / 23.4 ms |

**Screenshot test.** The compatibility render of the screenshot sample
matches the Mac's GPU `reference.bmp` exactly: 0 of 230,400 pixels
differ, not merely within the tolerance. swash rasterizes the same on
every system and the painter rounds as the GPU does, so the new
`compatibility_mode_draws_the_same_frame_as_the_reference` test runs on
Linux and Windows CI too: the first pixel check there.

**Not verified here** (no GPU or display in the cloud container): the
painter in a real window on macOS, Windows, X11 and Wayland; the
switch after a real GPU failure; `--smoke`; and the 16 ms frame on the
owner's Mac. To check on the Mac: `cargo run --release -- README.md
--compat`, press F12, scroll and type; and `cargo test --release -p
nimbus-render -- --ignored --nocapture compat_frame_bench`.

**Surprises.** The match with the GPU was exact, which says the GPU
also rounds to nearest and samples pixel centres as expected. On a
Retina-sized frame the painter is at the edge of 16 ms in the
container; the plan's fallback (redraw only changed rows, then bands
on threads) is ready if the Mac's number misses.

**New dependencies.** `softbuffer` 0.4.8 (with its X11 and Wayland
backends loaded at run time, and no KMS), which brings `tiny-xlib`,
`ctor` and `dtor` on Linux. All four are exempted in
`supply-chain/config.toml` at these versions pending the owner's
review (`cargo vet diff`), as M22's AccessKit crates were.
