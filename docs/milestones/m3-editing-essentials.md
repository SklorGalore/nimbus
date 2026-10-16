# M3 — Editing essentials

**Status: done** (2026-10-16, merged from PR #1)

## Goal

Everything you reach for in the first minute of real editing: selection,
clipboard, undo, find and replace, soft wrap and indentation helpers.

## Done when

Ticked items were verified here: by unit or property tests, or by headless
renders (`--headless`, since the screen was locked overnight). **Owner**
items need a person at the keyboard; each lists what to try.

- [x] Shift plus any movement extends the selection; selections render behind
      the text, including the line break as one extra cell.
- [x] **Owner, confirmed:** mouse drag selects; double-click selects a word, triple-click
      a line, and dragging after them extends by words or lines; Shift+click
      extends the selection. Logic is unit-tested; needs real clicks.
- [x] Primary+A selects all.
- [x] Word movement (Option+arrows on macOS, Ctrl+arrows elsewhere) stops at
      code-style word boundaries: `.` and `(` split words.
- [x] **Owner, confirmed:** Primary+X/C/V with the system clipboard; pasting CRLF text
      into an LF file converts it (conversion is unit-tested).
- [x] Undo restores text and cursor; a typed word is one step; redo works;
      Ctrl+Y redoes off macOS. Two property suites cover undo.
- [x] Find bar: incremental search, match count, Enter/Shift+Enter, regex
      and case toggles (Option+R, Option+C), Escape, all visible matches
      highlighted, the current one brighter (headless render).
- [x] **Owner, confirmed:** use the find bar by keyboard: Primary+F, type, Enter and
      Shift+Enter, Tab to the replace field (Cmd+Option+F opens it), Enter
      to replace one, Primary+Enter to replace all, then undo it in one step.
- [x] Search in a 100 MB file runs on a background thread and streams
      results; typing stays at one display refresh per key.
- [x] Soft wrap toggles with Option+Z; rows break at word boundaries, and up
      and down move by visual row (headless render plus tests).
- [x] **Owner, confirmed:** Option+Z in a real window, then resize the window with wrap
      on: the top line should stay put and text should rewrap.
- [x] Enter keeps the indentation; Tab and Shift+Tab indent and outdent
      selected lines as one step; Shift+Tab on one line keeps the cursor.
- [x] Home toggles between the first non-blank character and column 0.
- [x] A message line shows short notes ("Saved", "Replaced 3 matches",
      "Soft wrap on") and hides itself after 4 s.

## Scope

**In**: selections, word movement, clipboard, undo and redo, find and
replace, soft wrap, indentation helpers, the message line, the first piece of
UI chrome (the find bar).

**Deferred**: multi-cursor (M8), find across files (a post-M8 idea),
configurable indentation per language (M4 config, M5 language data).

## Design

**Undo** (`core/undo.rs`): `History { undo: Vec<Group>, redo: Vec<Group> }`.
A `Group` is a list of `Edit`s plus selections before and after. A new edit
joins the current group when all hold: same kind (insert or delete), it
starts where the last one ended, and under 500 ms passed. Any movement,
paste or explicit action seals the group. Redo is cleared on a new edit.

**Find** (`core/search.rs`): a `Query { pattern, regex, case }` compiled to a
`regex::Regex`; literal patterns are escaped. Search runs on a worker thread
over a `BufferSnapshot` in 1 MB chunks with a 4 KB overlap, so matches that
cross a chunk boundary are found unless they are longer than the overlap.
Multi-line regex matches longer than that are a known limit, noted in the
UI as "matches limited to 4 KB". Each search carries the document `version`;
results for an old version are dropped. Results go back over the channel and
wake the loop with an `EventLoopProxy` user event.

**Find bar UI** (`app/widgets/text_input.rs`): a single-line text input that
reuses `RopeBuffer`, `movement.rs` and `History` from `core`, so the bar gets
cursor movement, selection, clipboard and undo for free. This is the widget
the palette and opener use in M4.

**Soft wrap** (`render/wrap.rs`): for a logical line and a width in cells,
compute break points at word boundaries, falling back to a hard break for a
word longer than the width. Cached by `(line hash, width)`. The view's scroll
position becomes `(logical line, visual row, pixel offset)` as in
`DESIGN.md` §4.3, so nothing sums rows over the whole document. The
scrollbar (when it arrives in M8) estimates wrapped height from the average.

**Clipboard**: `arboard::Clipboard` is created lazily on first use, not at
startup.

## Tasks

1. Selection rendering and Shift-extension for every movement.
2. Word movement and word and line mouse selection.
3. Undo history with coalescing; tests for each sealing rule.
4. Clipboard with line-ending conversion.
5. Message line.
6. `text_input` widget on top of `core`.
7. Find bar UI, synchronous search for small files.
8. Background chunked search with version checks; test on 100 MB.
9. Replace one and replace all.
10. Soft wrap layout and anchored scrolling; cursor movement by visual row.
11. Indentation helpers and smart Home.

## Concepts

- **Undo coalescing**: why typing a word should undo as one step, and the
  rules that decide group boundaries.
- **Chunked search with overlap**, and the honest limit it introduces.
- **Stale results**: version numbers instead of locks.
- **Reusing the editor for its own UI**: the find field is a tiny document.
- **Anchored scrolling** for wrapped text.

## Risks

| Risk | Fallback |
|---|---|
| Soft wrap and anchored scrolling take much longer than planned | Ship wrap only for files under 1 MB in M3, extend later |
| Regex search over chunks misses edge cases | Property test: chunked results equal whole-string results for random inputs under the overlap length |
| `arboard` blocks on Linux under some clipboard managers | Run clipboard reads on a worker thread with a timeout |

## Results

| Metric | Value |
|---|---|
| Tests | 148 passing, plus 1 opt-in benchmark |
| Wrapping a 1 MB line (cache miss / hit) | about 20 ms / 0.2 ms |
| Background search of the 100 MB file | 157k matches before the first frame |
| Typing in the find field on the 100 MB file | one display refresh per key |
| CI on the branch | green on macOS, Linux and Windows (first push) |

**Design changes** (logged in `DESIGN.md`):

- Wrap lives in `nimbus_core::wrap`, not the renderer, because cursor
  movement by visual row needs it. The renderer was unchanged: it already
  shapes a slice of a line with tab stops measured from the line start.
- The view works in visual rows everywhere; with wrap off each line is one
  row, so one code path serves both modes. The scroll anchor stores the
  byte where the top row starts, so a resize keeps the same text at the top.
- A cursor exactly at a row break shows at the start of the next row. Home
  and End stay logical-line based.
- The renderer draws in clipped layers (text area, then overlays), so the
  find bar and message line cover the text beneath them.
- Find fields are small `Document`s, reusing movement, selection, undo and
  the clipboard.
- Re-searching after edits waits for a 150 ms pause; stale highlights are
  hidden meanwhile.
- `--headless WxH --screenshot FILE` renders without a window into an
  off-screen texture, so rendering can be checked with the screen locked.
  This brings forward the core of M8's screenshot test.
- `--smoke` and `--screenshot` give up after 10 s without a first frame,
  instead of hanging when the screen is locked.
- `--find PATTERN` and `--wrap` exist for testing until the config file and
  command palette arrive in M4.

**Surprises**

- With the screen locked, macOS reports windows as hidden, so on-screen
  captures hang. That led to `--headless` and the 10 s limit.
- The Unicode word rules treat `bar.baz` as one word, which suits prose but
  not code, so word movement uses code-style character classes. Soft wrap
  still uses the Unicode rules plus a rule against breaking before closing
  punctuation.

**End-of-milestone review** (code-review skill, high) found ten issues, all
fixed:

- Overlapping matches across search chunks, which made replace-all delete
  text, and chunks cut mid-line near the end of a file.
- Option+C and Option+R could no longer type ç and ®.
- Shift+click then drag used the wrong anchor.
- Incremental search on large files jumped to the top of the file.
- Replace-one on large files needed two presses.
- Shift+Tab on one line moved the cursor to the line start.
- Regex replacements were computed without surrounding context.
- A match ending in a line break highlighted the next line's first cell.
- The document was re-searched on every keystroke while the find bar was
  open.

**Known limitations**

- Typing inside a wrapped line near 1 MB costs about 20 ms per keystroke,
  two or three refreshes. The fix, if needed, is lazy prefix wrapping.
- Option+Z on macOS toggles wrap, so it no longer types Ω. The owner
  accepted this until M4's keymap makes it rebindable.
- A second stacked combining mark still hangs one cell right; CJK and emoji
  draw as boxes until M8 font fallback.
- Typing latency, IME and the click/drag behaviours were not measured or
  exercised this milestone, because the screen was locked overnight.
