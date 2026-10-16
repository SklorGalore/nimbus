# M2 — Editable

**Status: done** (2026-10-16)

## Goal

`nimbus path/to/file` opens a file you can move around in, type into,
scroll, and save, with line numbers and a blinking cursor.

## Done when

Ticked items were verified here: by tests, by `--screenshot` captures, or
by `--bench-typing`. **Owner** items need a person at the keyboard.

- [x] `nimbus file.txt` opens the file; `nimbus` alone opens an untitled
      buffer; a missing path opens an empty buffer that saves to that path.
- [x] Arrows, Home/End, Page Up/Down and Primary+Up/Down move the cursor;
      Cmd+Left/Right on macOS and Ctrl+Home/End elsewhere also work. Left and
      right move by grapheme (tests cover combining marks, ZWJ emoji, flags,
      skin tones, CRLF).
- [x] Up and down keep the preferred column across shorter lines, tabs and
      wide characters.
- [x] Typing inserts; Backspace and Delete remove one grapheme (a CRLF as one
      unit); Enter inserts the file's line ending; Tab inserts a tab.
- [x] **Owner, confirmed:** IME composition, e.g. Option+E then E, and a Japanese IME
      commit. Preedit is drawn underlined at the cursor and the candidate
      window is positioned from the cursor; not yet exercised by hand.
- [x] **Owner, confirmed:** clicking places the cursor, including past the end of a line
      and on wide characters. Hit testing is unit-tested.
- [x] **Owner, confirmed:** mouse wheel and trackpad scroll, smooth on trackpads,
      including horizontal scrolling of long lines.
- [x] Line numbers in a gutter sized to the line count.
- [x] The cursor blinks, stops after 10 s idle, and an idle editor uses no
      CPU (0 frames, 0 CPU time over 8 s, measured).
- [x] Primary+S saves atomically; LF, CRLF, mixed and BOM files round-trip
      byte for byte; the title shows the name and a ● when dirty.
- [x] **Owner, confirmed:** closing with unsaved changes asks to Save, Don't Save or
      Cancel, for the close button, Cmd+W and Cmd+Q.
- [x] A 10 MB file opens and takes keystrokes with keystroke-to-present of
      one display refresh: p50 8.2 ms at 120 Hz, of which our work is about
      0.7 ms (see Results).
- [x] Files over 256 MB, not UTF-8, with a line over 1 MB, directories and
      devices are refused with a clear message on stderr and in a dialog.
- [x] Property tests: random edit sequences on `RopeBuffer` match a
      `String` oracle (`crates/core/tests/buffer_oracle.rs`).

## Scope

**In**: `Buffer` trait and `RopeBuffer`; selections with one cursor;
movement; insertion and deletion; keyboard, IME and mouse input; scrolling;
gutter; blink; open and save; keystroke latency logging; `bench/gen.sh`.

**Deferred**: selection extension, clipboard and undo (M3), soft wrap (M3),
tabs and dialogs to open files (M4), highlighting (M5), huge or non-UTF-8
files (M7).

## Design

**`core` modules**

| Module | Owns |
|---|---|
| `buffer.rs` | The `Buffer` and `BufferSnapshot` traits from `DESIGN.md` §4.1, and `Edit { range, removed, inserted }`. |
| `rope.rs` | `RopeBuffer` over `ropey::Rope`. Converts byte to char offsets at the boundary with `byte_to_char` and `char_to_byte`. Debug-asserts every incoming offset is on a char boundary. |
| `selection.rs` | `Selection { anchor, head }` and `Selections(Vec<Selection>)` with one entry in M2. |
| `movement.rs` | Pure functions from `(&dyn Buffer, position, tab_width) -> position`: grapheme left and right, line start and end, up and down with a sticky column, document start and end. Display column is computed with `unicode-width`, with tabs expanded. |
| `line_ending.rs` | Detect LF, CRLF or mixed on open; the majority wins; tell save what to write. |

**`app` modules**

| Module | Owns |
|---|---|
| `document.rs` | `Document { buffer, path, line_ending, selections, preferred_col, dirty, version }`. `version` increments per edit; later milestones use it to discard stale background results. |
| `view.rs` | `View { scroll_line, scroll_px, gutter_cols }`; turns a document and metrics into a `FrameBuilder` call. Converts a click point to a byte offset. Keeps the cursor visible after movement. |
| `input.rs` | Maps winit `KeyEvent` and `Ime` events to editor operations. Text comes from `KeyEvent::text`; named keys and `Primary` chords are matched explicitly in M2 and become actions in M4. |
| `io.rs` | Open: size and UTF-8 check, read, detect line endings. Save: write to a temp file in the same directory, `sync_all`, rename over the target, so a crash never leaves half a file. |

**Render additions**: `View` produces text lines only for the visible range,
plus gutter numbers right-aligned in muted colour, a cursor rect, and an
underlined preedit string while composing. The shaping cache from M1 starts
earning its keep: lines are keyed by content hash, so scrolling one line
reshapes one line.

**Blink**: on any input the cursor shows solid and the next toggle is
scheduled 530 ms out with `ControlFlow::WaitUntil`. After 10 s without input
the cursor stays solid and the loop returns to `Wait`.

**Latency measurement**: the time between receiving a `KeyboardInput` event
and `queue.present` returning is logged at debug level, with a rolling
maximum. This becomes the overlay value in M8.

## Tasks

1. `buffer.rs` traits and `rope.rs`, with unit tests and a proptest oracle.
2. `movement.rs` with tests for combining marks, emoji, tabs, CJK width, and
   sticky column.
3. `line_ending.rs` and `io.rs`, with round-trip tests in a temp directory.
4. `bench/gen.sh` producing 10 MB and 100 MB source-like files; 1 GB and
   4 GB logs for M7.
5. `Document` and `View`; render the opened file read-only with gutter.
6. Scrolling: wheel lines and trackpad pixels.
7. Cursor rendering, keyboard movement, keep-cursor-visible.
8. Insertion, deletion, Enter, Tab; dirty marker in the title.
9. Mouse click to position.
10. IME: enable with `window.set_ime_allowed(true)`, position the candidate
    window with `set_ime_cursor_area`, render preedit, commit on `Ime::Commit`.
11. Blink and idle behaviour; confirm zero CPU idle in Activity Monitor.
12. Save, unsaved-changes prompt on close.
13. Latency logging; measure on the 10 MB file; fill in Results.

## Concepts

- **What a rope is**, and why inserting in the middle of a 10 MB file is
  cheap.
- **Bytes, chars, graphemes and columns**: four different ways to count
  "one character", and which one each operation uses.
- **Sticky column** for vertical movement.
- **IME composition**: preedit versus commit, and why the OS needs to know
  where the cursor is.
- **Atomic save** with temp file plus rename.
- **Redraw scheduling** with `Wait` and `WaitUntil`.

## Risks

| Risk | Fallback |
|---|---|
| Ropey 1.x char indexing leaks into higher layers | Keep all conversion in `rope.rs`; the debug assertions catch misuse |
| IME behaves differently on each platform | macOS first; record Linux and Windows status in Results; do not block the milestone on Linux IME |
| Trackpad scrolling feels wrong | Use pixel deltas directly with no acceleration of our own; momentum comes from the OS |
| Rename-over fails on Windows if another process holds the file open | Retry once, then report the error in the title and keep the buffer dirty |

## Results

| Metric | Value |
|---|---|
| First frame, release: empty / 10 MB / 100 MB file | 130 / 133 / 228 ms |
| Typing on the 10 MB file (`--bench-typing 300`) | p50 8.2 ms, p99 8.9 ms, max 16.7 ms |
| Of which: waiting for the display / our work | about 7.6 ms / about 0.7 ms |
| Idle after blink stops | 0 frames, 0 CPU time |
| Settled memory: empty / 10 MB / 100 MB file | 45 / 58 / 163 MB |
| Release binary | 7.5 MB |
| Tests | 90 across the workspace |

**Typing latency is display-bound.** The benchmark types each key right
after the previous frame, so every keystroke waits almost a full refresh for
a free drawable. Real typing lands at random points in the refresh and waits
about half as long on average. Our own work per keystroke is under 1 ms.

**Memory goal changed.** The 30 MB goal was missed even for an empty window: 45 MB settled,
of which about 19 MB is the window's Retina frame buffers. For several
seconds after launch the footprint is much higher, up to about 175 MB,
mostly GPU driver memory that macOS releases within about 15 s. Files cost
about 1.2 times their size once loading streams into the rope (the first
version read the whole file into a string first and peaked at twice the
size). The owner set the budget per file instead: opening a file may add at most
1.5× its size over an empty window. M2 adds 1.3× for a 10 MB file.

**Design changes** (all in `DESIGN.md`):

- Column math lives in `nimbus_core::column`, shared by movement and the
  renderer. The renderer snaps each grapheme to its column, so the cursor
  and text always agree.
- Movement takes `Text`, so it works on snapshots.
- Tabs draw nothing; missing glyphs draw as one box centred in the
  grapheme's columns.
- Only the visible column span of each line is shaped and drawn.
- Lines over 1 MB are refused until M7.
- Horizontal scrolling was added; the plan had missed it.
- winit's default macOS menu is off, so Cmd+Q reaches the unsaved-changes
  prompt; menus return in M4.
- A UTF-8 byte order mark is kept out of the text and written back on save.

**Surprises**

- A new window can report itself hidden for its first frames without ever
  reporting "visible" afterwards. The first frame was skipped and the
  window stayed blank, which hung `--screenshot` runs. Skipped frames now
  retry on a timer: 100 ms, doubling, giving up after about 4 s.
- The plan said a line-shaping cache existed; it did not. Shaping only the
  visible span made it unnecessary for now.
- Test windows opened on the owner's screen during measurements, and their
  mouse and keyboard input skewed one idle measurement. Idle checks now
  count window events alongside frames.

**End-of-milestone review** (code-review skill, xhigh) found fifteen issues.
Twelve are fixed:

- AltGr typing swallowed on Windows and Linux, which would have failed CI.
- Unbounded per-frame work on very long lines.
- The temp file followed planted symlinks (now `create_new`) and was briefly
  world-readable (now created 0600, with permissions copied before writing).
- BOM handling.
- Devices and growing files read without limit.
- A hidden-window retry that never stopped.
- Open errors visible only on stderr.
- A cursor that could vanish when the window lost focus.
- Save scrolling the view.
- Title rebuilt on every keystroke.
- The test-location rule (CLAUDE.md now allows cross-module property suites
  in `tests/`).

**Known limitations**

- Quitting from the macOS Dock, or at logout, skips the unsaved-changes
  prompt. winit 0.30 exposes no `applicationShouldTerminate` hook; revisit
  with M4's menus.
- On Linux without `zenity`, rfd cannot show the close prompt. A dirty
  untitled buffer then cannot be closed without killing the process. An
  in-window prompt (M3's message line) would remove the dependency.
- CJK, kana, Hangul and emoji draw as boxes until M8 font fallback; their
  positions and the cursor are correct. A second stacked combining mark hangs
  one cell to the right.
- IME suppression of duplicate key events was traced in winit's macOS code
  only; Windows and Linux follow winit's documentation.
- The AltGr fix can only run in CI (its test is compiled for Windows and
  Linux only).
