# M8 — Polish

**Status: planned**

## Goal

Turn a working editor into one you would choose to use daily, and produce
release builds for all three platforms.

## Done when

- [ ] **Multi-cursor**: `Primary+D` adds a cursor at the next occurrence of
      the selection; `Alt+click` adds a cursor; Escape returns to one. Typing,
      deleting, movement and paste apply to every cursor; undo restores all.
- [ ] **Bracket matching**: the bracket pair around the cursor is
      highlighted, using the syntax tree where there is one and a bounded
      scan otherwise.
- [ ] **Status bar**: line and column, selection length, line ending,
      encoding note for lossy files, language, indexing progress.
- [ ] **Debug overlay** (`F12`): frame time, keystroke-to-present latency
      (last and worst of the last 100), atlas occupancy, buffer backend,
      background task status.
- [ ] **Font fallback**: a missing glyph (CJK, emoji) is found in a system
      font through `fontdb`, loaded on a background thread, and drawn when
      ready; the `.notdef` box shows until then.
- [ ] **Bold and italic** in themes, via bundled variants or synthesis.
- [ ] Shortcut labels use ⌘ ⌥ ⌃ ⌫ symbols on macOS once fallback fonts
      provide them (M4 shows `Cmd+Shift+P` because the bundled font lacks them).
- [ ] **Scrollbar** with a draggable thumb.
- [ ] **Markdown inline highlighting** (stretch; skip without blocking).
- [ ] A GPU screenshot smoke test on macOS compares a rendered frame to a
      reference image with a tolerance.
- [ ] `cargo bloat` review recorded; anything over 1 MB justified.
- [ ] Manual QA checklist run on Linux (Wayland and X11) and Windows,
      results recorded.
- [ ] A tag push builds release artifacts in GitHub Actions: a macOS `.app`
      in a zip, a Linux tarball, a Windows zip.

## Scope

**In**: everything above.

**Deferred**: code signing and notarisation, auto-update, installers,
session restore, split views, find in files.

## Design

**Multi-cursor**: `Selections` already holds a `Vec`. Edits apply to each
selection from last to first in document order, so earlier offsets stay
valid; each selection is then shifted by the net length of edits before it.
Overlapping selections merge after every operation. One keystroke across N
cursors is one undo group.

**Font fallback**: on a `.notdef` glyph, the renderer records the missing
character. A worker loads the system font database once, finds a face that
covers it, and sends back the face. The atlas key already includes a font
id, so fallback glyphs share the atlas.

**Screenshot test**: render to an offscreen texture instead of the surface,
copy it back, compare with a stored PNG allowing small per-pixel
differences. macOS only, because GPU and font rasterization differ by
platform.

**Release workflow** (`.github/workflows/release.yml`): on `v*` tags, build
release binaries on all three runners, package them, attach them to a
GitHub release. The macOS bundle is a directory with `Info.plist`, the
binary and an icon, built by a short script rather than a packaging tool.

## Tasks

1. Status bar.
2. Debug overlay.
3. Multi-cursor model, editing, rendering, undo.
4. Bracket matching.
5. Scrollbar.
6. Font fallback through `fontdb`.
7. Bold and italic.
8. Screenshot test.
9. `cargo bloat` review.
10. Release workflow and macOS bundle script.
11. Linux and Windows QA pass.
12. Markdown inline highlighting, if time allows.

## Concepts

- **Applying edits from multiple cursors** without invalidating offsets.
- **Font fallback**: why one font never covers everything.
- **Offscreen rendering** for tests.
- **Release automation** with tag-triggered workflows.

## Risks

| Risk | Fallback |
|---|---|
| Fallback fonts differ in metrics and break the grid | Scale fallback glyphs to the cell and centre them; wide characters take two cells |
| Screenshot test is flaky across macOS versions | Loosen the tolerance or compare only a text region; drop to a manual check if needed |
| macOS refuses to open an unsigned `.app` | Document `xattr -d com.apple.quarantine`; signing is deferred |

## Results

_Fill in when done._
