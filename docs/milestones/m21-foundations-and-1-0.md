# M21 — Foundations and 1.0

**Status: proposed.** Written on 2026-10-18. The last milestone of the
set: bring the pinned dependencies forward, add the testing that a 1.0
promise needs, decide on accessibility, and ship 1.0.

## Goal

Call Nimbus 1.0: current dependencies, a fuzzed and soak-tested editing
path, a screen reader that can read the editor, and a release the owner
would hand to someone else.

## Done when

- [ ] **Ropey 2**: `RopeBuffer` moves to ropey 2 and the char-to-byte
      layer in `rope.rs` is deleted; the buffer oracle suite and every
      benchmark pass with numbers recorded before and after.
- [ ] **winit and wgpu current**: winit 0.31 (if stable by then) and the
      current wgpu, with the API drift noted in `CLAUDE.md` updated;
      `--smoke` within 10 % of the previous release.
- [ ] **Fuzzing**: `cargo fuzz` targets for the buffer (random edit
      scripts against the `String` oracle), the diff, the search chunker
      and the config and theme parsers, run for an hour each in a weekly
      CI job; any crash found is a regression test.
- [ ] **Soak**: `--bench-typing` extended to a 100,000-keystroke session
      with saves, undo and find on the 100 MB file, memory sampled every
      1,000 keystrokes; growth under 10 MB over the run.
- [ ] **Accessibility**: `accesskit` through winit exposes the window,
      the editor's text and the cursor position to VoiceOver, NVDA and
      Orca; the palette and pickers are lists a screen reader can walk.
      Measured against the binary budget and `--smoke`.
- [ ] **Crash record**: a panic (release builds abort) writes a one-line
      record with the version and the panic message to `crash.log` in the
      state directory before aborting, and the next start says it is
      there. No text from the user's buffers is written (the M12 autosave
      decision stands).
- [ ] **1.0**: `Cargo.toml` at 1.0.0; `CHANGELOG.md` from M0 to here;
      `DESIGN.md` §8 and this file's Results; the release workflow from
      M15 publishes it.

## Scope

**In**: the two upgrades, fuzzing and soak in CI, AccessKit, the crash
record, the 1.0 release.

**Deferred**: semantic accessibility beyond text and lists (roles for
every chrome element); crash upload; a plugin or scripting layer (still
a non-goal, §2).

## Design

**Ropey 2 is a `core` change only.** The `Buffer` trait is in byte
offsets already; the wrapper's conversions go away and the rest of the
workspace does not notice. Done first, on its own commit, because the
oracle suite makes it safe and every later task benefits from the
simpler code.

**Upgrades are measured, not assumed.** Each upgrade is one commit with
`--smoke`, the typing benchmark and binary size before and after in its
message; a regression over 10 % is reverted and the upgrade waits.

**Fuzzing reuses the oracles.** The property tests from M2, M7 and M9
already compare against a `String` oracle; fuzz targets feed them
arbitrary input instead of `proptest` strategies. The targets live in
`fuzz/` at the workspace root (cargo-fuzz's layout) and are excluded
from the normal build.

**AccessKit is a tree we already have.** winit integrates `accesskit`
through its platform adapters; the app publishes a node for the editor
with the visible text and the caret, and list nodes for the palette and
pickers, updated on the same redraw path (on demand, never a loop).

**The crash record uses a panic hook** set in `main` that writes the
message and version with `std::fs` (no allocation beyond the message)
and then lets the abort proceed.

## Tasks

1. Ropey 2 migration; delete the conversion layer; oracle suite and
   benchmarks before and after.
2. winit upgrade; wgpu upgrade; `CLAUDE.md` gotchas updated; `--smoke`.
3. Fuzz targets and the weekly CI job; first findings as tests.
4. Soak run in `--bench-typing`; memory sampling; the growth check.
5. AccessKit nodes for the editor, palette and pickers; tested with
   VoiceOver by the owner and NVDA on Windows by hand.
6. Panic hook and `crash.log`; the next-start message.
7. `CHANGELOG.md`, version 1.0.0, release; Results.

## Concepts

- **An oracle makes an upgrade safe**: why the buffer rewrite is low
  risk despite touching the lowest layer.
- **Fuzzing versus property testing**: same idea, different source of
  inputs, different time budget.
- **Accessibility trees**: what a screen reader asks the window for and
  how a GPU-drawn app answers.

## Risks

| Risk | Fallback |
|---|---|
| winit 0.31 is still beta | Stay on 0.30; record it and move the task to a later point release |
| AccessKit adds measurable startup time | Create the adapter lazily on the first accessibility request, which the platforms allow |
| The fuzzers find more than one milestone can fix | Each finding is a test and a fix on its own commit; 1.0 waits for the buffer and save targets only |

## Results

_To be filled in when the milestone lands._
