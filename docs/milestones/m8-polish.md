# M8 — Polish

**Status: done on `m8-wip`, waiting for owner checks**

## Goal

Turn a working editor into one you would choose to use daily, and produce
release builds for all three platforms.

## Done when

Ticked items were verified here by tests, headless renders or measured
runs. **Owner** items need a person at the keyboard; each says what to try.

- [x] **Multi-cursor**: `Primary+D` adds a cursor at the next occurrence of
      the selection; `Alt+click` adds a cursor; Escape returns to one. Typing,
      deleting, movement and paste apply to every cursor; undo restores all.
      Also `Primary+Alt+Up/Down`. Property suite `multi_oracle.rs` on both
      backends; document tests; headless renders with `--cursors`.
- [x] **Bracket matching**: the bracket pair around the cursor is
      highlighted, using the syntax tree where there is one and a bounded
      scan otherwise (syntax and core tests; headless renders).
- [x] **Status bar**: line and column, selection length, line ending,
      encoding note for lossy files, language, indexing progress.
      Messages show on its left (view test; headless renders).
- [x] **Debug overlay** (`F12`): frame time, keystroke-to-present latency
      (last and worst of the last 100), atlas occupancy, buffer backend,
      background task status. `--debug-overlay` shows it from the start,
      also in headless renders.
- [x] **Font fallback**: a missing glyph (CJK, emoji) is found in a system
      font through `fontdb`, loaded on a background thread, and drawn when
      ready; the `.notdef` box shows until then. Owner's choice: the
      smallest suitable font file is copied into memory (no `unsafe`);
      colour-only emoji fonts are skipped, so emoji draw as outlines where
      a font has them (headless renders; fallback tests).
- [x] **Bold and italic** in themes, via bundled variants or synthesis:
      Hack Bold bundled (+2.7 MB), italic by slanting (theme, raster and
      font tests; headless render with a styled theme).
- [x] Shortcut labels use ⌘ ⌥ ⌃ ⌫ symbols on macOS once fallback fonts
      provide them (M4 shows `Cmd+Shift+P` because the bundled font lacks them).
      Now `⇧⌘P`, in macOS's modifier order (headless palette render).
- [x] **Scrollbar** with a draggable thumb (view test of its geometry
      and dragging; headless renders, including the 4 GB log).
- [ ] **Markdown inline highlighting** (stretch; skip without blocking).
      Skipped: the Markdown grammar's inline part needs a second, nested
      parse in the syntax state (an injection layer), more than a stretch
      task. Left for after M8.
- [x] A GPU screenshot smoke test on macOS compares a rendered frame to a
      reference image with a tolerance (`crates/app/tests/screenshot.rs`:
      0 pixels differ on a clean run, 48 for one changed letter; at most
      20 allowed).
- [x] `cargo bloat` review recorded; anything over 1 MB justified (Results,
      Size review).
- [ ] Manual QA checklist run on Linux (Wayland and X11) and Windows,
      results recorded. Linux: CI and headless renders only (owner has no
      Linux machine; decided at approval). Windows: the owner checks below.
- [ ] **Owner, Windows:** type with three cursors (Ctrl+D twice on a
      word, then type), Alt+click to add and remove cursors, Ctrl+Alt+Up
      and Down (some graphics drivers take these for screen rotation;
      note if they do), Escape, undo and redo; copy at three cursors and
      paste at three.
- [ ] **Owner, Windows:** the status bar, F12 overlay, the scrollbar
      (drag and click the track), bracket boxes next to the cursor, and a
      file with Chinese or Japanese text (it appears after a moment).
- [ ] **Owner, Windows:** the icon shows on `nimbus.exe` in Explorer and
      in the taskbar.
- [ ] **Owner, macOS:** the same multi-cursor steps with ⌘D and ⌥-click;
      the palette shows ⇧⌘P-style shortcuts.
- [ ] A tag push builds release artifacts in GitHub Actions: a macOS `.app`
      in a zip, a Linux tarball, a Windows zip. Workflow, bundle script
      (tested locally) and icon are in; **owner**: push a tag to run it.

## Scope

**In**: everything above.

**Deferred**: code signing and notarisation, auto-update, installers,
session restore, split views, find in files.

## Design

Approved by the owner on 2026-10-16, with the recommendation taken for
each open decision (marked **Decided**). The multi-cursor part comes from
a planning pass over the current code.

### Multi-cursor (`selection.rs`, new `core/src/multi.rs`, `document.rs`)

**Selections become a sorted, merged list with a primary index.**
`Selections { items, primary }`: items sorted by start; after every
operation `normalize` merges overlapping ones (a cursor touching a
selection merges into it; two non-empty selections that only touch stay
apart). The newest cursor is the primary, as in VS Code, so the view
follows what you just added; the IME and the find bar use the primary.

**One function applies an operation at every cursor.** Each operation
says what to do at one selection (a range and its new text); `multi::
apply_each` collects those, merges deletions that overlap (two
word-deletes reaching the same start), and applies them with
`replace_sorted`. That is one pass on both backends and gives the
descending edit list that undo and `report_sorted` already handle, so the
syntax tree sees every edit correctly. Selections are then moved through
the edits in one pass (`Selections::map_through`).

**Undo.** A keystroke at N cursors is one batch; typing coalesces batches
into one undo step under the existing rules (same kind, contiguous at
every cursor, within the window). A group keeps its batches, and undo
puts each batch back with one `put_sorted`. Undo restores every
selection, not just the primary (today `Document::restore` keeps one).

**Each operation per cursor**: typing and deleting as now; Enter
auto-indents from each cursor's own line; paste puts one line per cursor
when the clipboard has exactly as many lines as there are cursors, else
the whole text at each; copy joins the selections with newlines; Tab and
Shift+Tab indent each touched line once even if two cursors share it.
Movement moves every cursor, each keeping its own column for Up and Down.

**Adding cursors**: `Primary+D` selects the word under the cursor, then
each press adds the next occurrence (wrapping around). On a huge file the
search for the next occurrence reads at most 32 MB per press, off the
search results when the find bar already has them. `Alt+click` adds or
removes a cursor; `Alt+drag` adds a selection. Escape returns to the
primary. **Decided:** `Primary+Alt+Up/Down` also adds a cursor on the
line above or below; many window managers on Linux take Alt+click for
moving windows, so a keyboard way to add cursors helps there.

**Drawing**: every selection and cursor; the gutter marks every cursor's
line. No allocation per frame (the painter walks the sorted list as it
walks search matches).

**Tests**: a new property suite, `crates/core/tests/multi_oracle.rs`,
runs random cursor sets and operations (typing, deleting, word-delete,
paste, indent, movement, undo, redo) on both backends against a plain
`String` with a naive position list, checking the text, the selections,
undo back to each step, and the syntax replay.

### Status bar (`view.rs`, `app.rs`)

A permanent last row: on the left, messages (today's message line moves
here); on the right, `Ln 12, Col 5`, the selection length (and cursor
count with several), line ending, `Bytes` for a document with bytes that
are not UTF-8, language, and `Counting lines… 40%` while indexing. The
line number is `~` past the counted part of a huge file, as in the gutter.

### Debug overlay (`F12`)

A small panel in the top right: frame time, keystroke-to-present latency
(last and worst of the last 100; the time from the key event to the frame
that shows it being presented), atlas occupancy, buffer backend, and
background work (parse, search, line count, save). Drawn only when on,
so it costs nothing otherwise.

### Bracket matching

The bracket at or just before the cursor and its partner get a box. With
a syntax tree, the partner is found from the tree, which knows that a
bracket in a string is not code. Otherwise (huge files, plain text) a
scan of at most 64 KB each way, counting nesting; brackets in strings
may then mislead it, a documented limit.

### Scrollbar

A thin bar on the right edge of the text, with a thumb showing the
visible part. Its position is by byte (`scroll_top / len`), which needs no
line count and so works on a 4 GB file at once. Dragging the thumb or
clicking the track uses `View::scroll_to_fraction` from M7. The thumb has
a minimum height, fades when idle, and widens on hover.

### Font fallback (`render`)

When a character is missing from Hack, the renderer records it and a
worker loads the system's font list once (`fontdb`, new dependency,
about 0.2 MB), finds a font that has it, and sends the font back; the
frame is redrawn when it arrives. Until then the missing-glyph box shows.
**Correction to the earlier sketch**: the glyph key does not yet carry a
font, so `GlyphKey` and the shaped glyph gain a font index. Fallback
glyphs are scaled to fit the cell grid and centred; wide characters (CJK,
emoji) take two cells, as the column code already counts them. Colour
emoji are drawn as their outline in the text colour for now (colour
bitmaps need an RGBA atlas; deferred). Once fallback fonts are in, menu
and palette shortcut labels switch to ⌘ ⌥ ⌃ ⇧ on macOS.

### Bold and italic

Themes may set `bold` and `italic` per syntax capture (for example
`keyword = { color = "#c678dd", bold = true }`). **Decided:** bundle
Hack Bold (+2.8 MB), since faked bold looks smeared at small sizes, and
make italic by slanting Regular and Bold while rasterising, which is what
Hack's italic already is (an oblique). Bundling all three would have cost
8.4 MB.

### Screenshot test

Renders a fixed document through the existing headless path with a
fixed config, theme and font size, and compares it to a stored BMP,
allowing small per-pixel differences. macOS only, since GPUs and
rasterisers differ by platform. If CI's macOS runner has no usable GPU
adapter, the test skips with a log line and runs locally; that is decided
by trying it.

### Size review

`cargo bloat --release --crates` on macOS and Linux, recorded in
Results; anything over 1 MB is justified or trimmed (the grammars and the
bundled font are the expected large items).

### Release workflow (`.github/workflows/release.yml`)

On a `v*` tag: build on all three runners and attach to a GitHub release
a macOS `Nimbus.app` in a zip (a universal binary, Apple silicon and
Intel, joined with `lipo`), a Linux `x86_64` tarball built on the oldest
supported Ubuntu runner so it runs on older systems, and a Windows zip.
The `.app` is made by a short script (`Info.plist`, binary, icon).
Unsigned: the README says how to open it the first time. **Decided:** a
simple icon drawn here (a cloud outline on a dark rounded square), kept
as SVG and rendered to the sizes macOS and Windows need.

### Manual QA

A checklist in this doc, run by the owner on Windows. **Decided:** Linux
is covered by CI and headless renders for now (no Linux machine; a VM is
possible later), so the Wayland and X11 checks are recorded as not run.

## Tasks

Each one commit, with its tests.

1. Status bar (messages move into it).
2. Debug overlay.
3. Multi-cursor: the `Selections` model, `normalize`, `map_through`.
4. Multi-cursor: `apply_each`, batched undo, and the property suite
   (agent, high, per CLAUDE.md).
5. Multi-cursor: every `Document` operation at every cursor; undo
   restores all selections.
6. Multi-cursor: movement, copy, cut, paste distribution.
7. Multi-cursor: drawing.
8. Multi-cursor: `Primary+D`, `Alt+click`, Escape (and cursor above or
   below, if approved).
9. Bracket matching.
10. Scrollbar.
11. Font fallback through `fontdb`; ⌘-style shortcut labels on macOS.
12. Bold and italic.
13. Screenshot test.
14. `cargo bloat` review.
15. Release workflow, macOS bundle script, icon.
16. Linux and Windows QA pass.
17. Markdown inline highlighting, if time allows.
18. End-of-milestone review at xhigh; Results.

## Concepts

- **Applying edits from multiple cursors** without invalidating offsets,
  and undoing them as one step.
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

| Metric | Value |
|---|---|
| Tests | 344 passing, plus 12 opt-in benchmarks or diagnostics |
| Release binary | 14.75 MB on macOS (M7: 14.45 MB) |
| `--smoke` first frame | 192 ms first run after a rebuild, 141–150 ms warm (M5 Pro) |
| Empty window | 35 MB RSS at 1400×700 (4 GB log: 35 MB too) |
| Fallback fonts for a CJK + symbols sample | about +21 MB, only once such characters appear |
| Replace All of 100,000 matches, with undo and redo | 0.21 s (debug) |

**Design changes**

- **Undo groups hold batches.** The plan merged each keystroke's N edits
  into the group's edits. Instead a group keeps one batch per keystroke
  and undo puts each back in one pass: simpler, and it never has to join
  removed text held as pieces.
- **Primary+D on a huge file** reads at most 32 MB per press and goes on
  from there on the next press.
- **Primary+Alt+Up/Down** add above the topmost or below the bottommost
  cursor, so repeating them builds a column.
- **Scrollbar** does not fade when idle (that needs timed redraws;
  rendering is on demand).
- **Font fallback** copies the smallest suitable font (owner's choice
  over mapping it), prefers complete fonts for CJK, and skips colour-only
  emoji fonts: emoji draw in the text colour where a font has outlines.
- **Bold** comes from Hack Bold without its icons (185 KB); italic is a
  12° slant. The plan's SVG icon became a Python script, since macOS has
  no SVG renderer that keeps transparency.
- **Small non-UTF-8 files** stayed in memory (from M7); the status bar
  says "Not UTF-8" for them.

**Concepts**

- *Applying edits at several cursors* (`crates/core/src/multi.rs`,
  `selection.rs`): each cursor's change is worked out in the text before
  any of them, then all are applied from the end of the text back, so no
  offset moves before it is used; the cursors are then moved through the
  edits in one pass.
- *Font fallback* (`crates/render/src/fallback.rs`): no font covers every
  script, so missing characters are looked up in system fonts on a
  worker, and drawn from whichever face has them.
- *Offscreen rendering for tests* (`crates/app/tests/screenshot.rs`): the
  headless path draws into a texture, which is compared with a stored
  image.
- *Release automation* (`.github/workflows/release.yml`): a tag push
  builds every platform and drafts a release.

**Surprises**

- The bundled bold font was 2.8 MB, almost all Nerd Font icons; subset,
  it is 185 KB.
- Choosing the smallest font file for each character mixed fonts within
  Chinese text (a symbol font happened to have 中); requiring the
  script's common characters too fixed it.
- The first scrollbar was too faint to see; the screenshot test's first
  tolerance (0.5% of pixels) let a changed letter through, so it now
  counts pixels (at most 20; one letter is 48).

**End-of-milestone review** (code-review skill, xhigh) found nine issues,
all fixed: an Alt+click that removed a cursor still started a drag that
overwrote another; copy then paste with a bare last cursor lost its line;
a failed batch during undo or redo left the text and history out of step
(now rolled back); undo kept stale per-cursor columns; removing the middle
primary chose the wrong neighbour; tree bracket matching was quadratic in
a list's length and allocated each frame; the counting fallback and the
status bar's character count re-read text each frame (both cached now);
and two copies of the size formatter allocated each frame.

**Known limitations**

- Colour emoji draw as outlines in the text colour, or as boxes where no
  font has outlines (most of macOS's emoji): colour needs an RGBA atlas.
- Markdown inline highlighting was not done (stretch).
- Bracket matching without a syntax tree can be misled by brackets in
  strings.
- Linux was not run by hand (CI and headless renders only).
- The first release needs a tag push to try the workflow.

### Size review (task 14)

macOS release binary: **14.68 MB** (M7: 14.45 MB). Code is 5.7 MiB; the
rest is read-only data, 11.2 MB of it in `__const`.

| Item | Size | Why it stays |
|---|---|---|
| Bundled regular font (Hack Nerd Font Mono) | 2.7 MB | The editor's only font until fallback; the Nerd Font icons (2.4 MB of it) are kept for file-tree icons |
| Tree-sitter grammars (tables) | about 5.5 MB | TypeScript and TSX together 2.8 MB, Bash 1.3, Rust 1.1, Markdown 0.8, the rest under 0.5 each: the nine starter languages (DESIGN.md §4.2) |
| `std` code | 1.1 MiB | The standard library |
| Everything else | each under 1 MiB | `naga` (wgpu's shader compiler) 0.9 MiB is the largest crate |

One item was trimmed: the bundled bold font. As shipped by Nerd Fonts it
is 2.8 MB, almost all icons, and bold icons are never needed, so it was
subset to its 1,881 text characters (185 KB, `pyftsubset`, recorded in
`crates/render/src/font.rs`). The release binary went from 17.19 MB to
14.68 MB. Its names keep clear of "Bitstream" and "Vera", as the font
licence requires of a modified font.
