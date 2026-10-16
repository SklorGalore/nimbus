# M3 — Editing essentials

**Status: next**

## Goal

Everything you reach for in the first minute of real editing: selection,
clipboard, undo, find and replace, soft wrap and indentation helpers.

## Done when

- [ ] Shift plus any movement extends the selection; selections render as
      rects behind the text.
- [ ] Mouse drag selects; double-click selects a word; triple-click selects
      a line; dragging after a double-click extends by words.
- [ ] `Primary+A` selects all.
- [ ] Word movement: `Alt+Left/Right` on macOS, `Ctrl+Left/Right` elsewhere,
      stopping at word boundaries from `unicode-segmentation`.
- [ ] `Primary+X/C/V` cut, copy and paste through the system clipboard.
      Pasting CRLF text into an LF file converts it.
- [ ] `Primary+Z` undoes and `Primary+Shift+Z` (and `Ctrl+Y` on Windows and
      Linux) redoes. Typing a word undoes as one step; cursor position and
      selection are restored on undo.
- [ ] Find bar: incremental search as you type, match count, next and
      previous with Enter and Shift+Enter, regex and case toggles, Escape to
      close. All visible matches are highlighted.
- [ ] Replace: replace one, replace all (as one undo step).
- [ ] Search in a 100 MB file does not freeze typing; results stream in.
- [ ] Soft wrap toggles (`Alt+Z`); wrapped lines break at word boundaries
      and the cursor moves by visual row with up and down.
- [ ] Enter copies the current line's leading whitespace. Tab with a
      multi-line selection indents those lines; Shift+Tab dedents.
- [ ] Home toggles between the first non-blank character and column 0.
- [ ] A minimal message line at the bottom of the window shows transient
      messages (save errors, "no matches").

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

_Fill in when done._
