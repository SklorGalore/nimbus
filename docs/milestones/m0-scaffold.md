# M0 — Scaffold

**Status: done** (2026-10-15, commits `68bd933`, `064c691`, `7271891`)

## Goal

A workspace that builds on three platforms and opens an empty window that
resizes and closes cleanly.

## Done when

- [x] `cargo run` opens a window cleared to the editor background colour.
- [x] Resizing redraws; minimising does not crash; closing exits with code 0.
- [x] `nimbus --smoke` exits after the first frame and prints `first-frame-ms`.
- [x] `nimbus --version` and `--help` work; unknown arguments exit with code 2.
- [x] CI builds, lints and tests on macOS, Linux and Windows.
- [x] CI fails if the release binary exceeds 15 MB.

## What was built

- Four crates: `nimbus-core`, `nimbus-syntax`, `nimbus-render`, and the
  `nimbus` binary in `crates/app`.
- `render::Gpu` owns the wgpu instance, adapter, device, queue and surface,
  reconfigures on resize, and clears the frame.
- `app` implements winit's `ApplicationHandler`, creates the window in
  `resumed`, and uses `ControlFlow::Wait` so an idle window costs no CPU.
- Strict lint table in the root `Cargo.toml`, `clippy.toml` relaxations for
  tests, and `CLAUDE.md` with the rules.

## Concepts introduced

- The winit event loop and why windows are created in `resumed`.
- The five wgpu objects: instance, adapter, device, queue, surface.
- What a frame is: acquire a surface texture, record commands, submit, present.
- On-demand redraw versus a render loop.

## Results

| Metric | Value |
|---|---|
| First frame, release, warm | 93–99 ms as measured; about 125 ms in truth (see note) |
| First frame, release, cold | 129 ms as measured |
| Release binary | 3.4 MB |
| GPU setup share of first frame | about 3 ms (originally misjudged as half) |

**Correction, found in M1**: M0's timer started in `App::new`, after
`EventLoop::new`, so these numbers leave out about 30 ms of AppKit setup.
M1 measures from the top of `main`.

**Surprises**

- wgpu 30 changed the frame API: `get_current_texture` returns a
  `CurrentSurfaceTexture` enum, frames are presented with `queue.present`,
  `InstanceDescriptor` has no `Default`, render passes need `multiview_mask`.
  Recorded in `CLAUDE.md`.
- winit's newest release is a 0.31 beta; pinned to 0.30.
- `actions/checkout@v4` triggered the Node 20 deprecation warning; moved to v7.

**Design changes**: startup looked tight, so M1 moved GPU setup off the
main thread (`DESIGN.md` §4.3). M1 then found the real cost is in winit and
AppKit, not the GPU.
