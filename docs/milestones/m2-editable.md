# M2 — Editable

**Status: next**

## Goal

`nimbus path/to/file` opens a file you can move around in, type into,
scroll, and save, with line numbers and a blinking cursor.

## Done when

- [ ] `nimbus file.txt` opens the file; `nimbus` with no argument opens an
      empty untitled buffer; a missing path opens an empty buffer that saves
      to that path.
- [ ] Arrow keys, Home/End, Page Up/Down, `Primary+Up/Down` (start and end
      of file) move the cursor. Left and right move by grapheme, so `é`
      written as `e` plus a combining accent is one step, and so is an emoji.
- [ ] Up and down keep the preferred column across shorter lines.
- [ ] Typing inserts; Backspace and Delete remove one grapheme; Enter inserts
      the file's line ending; Tab inserts a tab.
- [ ] IME input works for at least one composed input (macOS accent popup or
      a Japanese IME commit); the preedit text is shown at the cursor.
- [ ] Click places the cursor, including past the end of a line and on wide
      characters.
- [ ] Mouse wheel and trackpad scroll, with smooth pixel scrolling on
      trackpads.
- [ ] Line numbers in a gutter sized to the line count's digit width.
- [ ] The cursor blinks, stops blinking after 10 s idle, and the process
      uses no CPU while idle.
- [ ] `Primary+S` saves atomically; LF and CRLF files round-trip
      byte-for-byte when unedited; the window title shows the file name and
      a dirty marker.
- [ ] Closing with unsaved changes asks for confirmation (a native dialog
      via `rfd`).
- [ ] A 10 MB source file opens, scrolls and takes keystrokes with
      keystroke-to-present under 8 ms, logged at debug level.
- [ ] Files over the huge-file threshold, or not valid UTF-8, show a clear
      message and do not open (M7 handles them).
- [ ] Property tests: random edit sequences on `RopeBuffer` match a
      `String` oracle.

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

_Fill in when done._
