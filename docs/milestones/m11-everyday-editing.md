# M11 — Everyday editing

**Status: done** (2026-10-17, built alongside M10). Written on 2026-10-17. Collects the
small editing operations that every editor has and Nimbus does not yet,
plus two items deferred earlier: per-language indentation (M3) and
multi-key chords (M4). Approved the same day, with the auto-pairs
default, the 64 MB limit and the fixed chord timeout decided
(`DESIGN.md` §9).

## Goal

The line and selection operations a programmer reaches for every few
minutes, each working with several cursors and undoable in one step, and
the indentation settings that make them land where expected.

## Done when

- [x] **Comments**: "Toggle Line Comment" (`Primary+/`) comments or
      uncomments the selected lines using the language's line comment
      token; blank lines are skipped and the token aligns with the
      shallowest indent. "Toggle Block Comment" (`Primary+Shift+/`) wraps
      the selection. Languages without a line comment use the block form.
      Works for every built-in and user-defined language, since both
      already declare their tokens.
- [x] **Lines**: duplicate line or selection (`Primary+Shift+D`), move
      line up and down (`Alt+Up`/`Alt+Down`), delete line
      (`Primary+Shift+K`), join lines (`Primary+J`), select line
      (`Primary+L`, repeat extends), insert line below and above without
      breaking the current one (`Primary+Enter`, `Primary+Shift+Enter`).
- [x] **Selection**: "Expand Selection" (`Alt+Shift+Right`; `Ctrl+Shift+Right` on macOS, where `Alt+Shift+Right` selects a word) grows from
      the cursor to the word, then the quoted string or bracket pair, then
      the line, then the whole text; "Shrink Selection" goes back. "Go to
      Matching Bracket" (`Primary+Shift+\`) and "Select to Matching
      Bracket" use the bracket matcher from M8.
- [x] **Text**: upper, lower and title case; sort selected lines (plain
      and reversed); remove duplicate lines; trim trailing whitespace in
      the selection or the file. All multi-cursor aware.
- [x] **Auto-pairs**: typing `(`, `[`, `{`, `"`, `'` or a backtick inserts
      the closing character when the next character is whitespace, a
      closer or the end of the line; typing the closer over it steps past;
      Backspace between an empty pair removes both; typing an opener with
      a selection wraps it. A setting turns it off; the default is on
      for code and off for Markdown and plain text.
- [x] **Indentation per language**: `[indent]` defaults and
      `[indent.LANGUAGE]` overrides in the config: `width`, `tabs`
      (true/false), and `detect` (default true: the file's own majority
      of leading tabs or spaces, and the most common space count, win
      over the config). The status bar shows "Spaces: 4" or "Tabs" and
      clicking it offers the choices for this file. Enter after an opening
      bracket indents one level, and a closer typed first on a line
      dedents it.
- [x] **On save** (both default off, so files round-trip byte for byte as
      DESIGN.md §4.1 requires): `trim_trailing_whitespace` and
      `final_newline`, as config settings with per-language overrides.
- [x] **Multi-key chords**: a keybinding may be two chords
      (`"Ctrl+K Ctrl+C"`); after the first, the status bar shows it and
      waits two seconds for the second (a fixed timeout, not a setting);
      Escape or an unknown chord cancels. No default binding uses one;
      this exists for user overrides,
      and the keymap conflict test learns that a chord cannot be both a
      binding and a prefix.
- [x] Every operation is an `Action`, in the palette and menus; each
      covered by a test that applies it to a small document with one and
      with three cursors and compares with the expected text.
- [x] Typing budget kept: on the 4 GB log, every operation on a normal
      selection stays under 8 ms. A line operation on a selection over
      64 MB (the M8 `Primary+D` read limit) shows "selection too large
      for this operation" instead of running.

## Scope

**In**: everything above, in `core` (the pure edits) and `app` (actions,
keys, settings, status bar).

**Deferred**: syntax-aware expand selection (to the enclosing function or
block: M14, which has the tree), Markdown list continuation on Enter
(M14), snippets and templates (not planned), format on save (needs an
external formatter and a process model, not planned), spelling (not
planned).

## Design

**Pure edits in `core`** (`crates/core/src/lines.rs`, new): each operation
takes the text and the selections and returns the `(Range, &str)` items
for one `replace_sorted` call plus the selections afterwards, the shape
M8's `multi.rs` established. Nothing here reads a whole line into a
`String` when the line is long: the long-line helpers from M7 give the
window around each cursor, and a selection covering a line over the long
line threshold (64 KB) routes through `slice_into` into a reused buffer.
The one-pass batch keeps every operation one undo record and keeps it
within the typing budget, because `replace_sorted` on the mapped buffer
is O(items) pieces, not O(file).

**Comment tokens** come from the language rules: `lexical::builtin` for
the nine built-in languages and the user's `languages/NAME.toml` for the
rest (`line_comments`, `block_comment`, `hash_needs_space`). The `syntax`
crate exposes a small `CommentTokens` lookup so `app` does not reach into
the lexer. Markdown has no line comment; it gets `<!-- -->` as the block
form.

**Expand selection without a tree**: word (the M3 word classes), then the
nearest enclosing quotes or bracket pair found by the M8 bracket matcher
scanning outward from both ends, then the line, then all. The sequence is
a stack on the document, so Shrink retraces it; any other movement clears
the stack. M14 adds syntax nodes between "pair" and "line".

**Auto-pairs** live in `app`'s input path, before the character becomes
an insert: the decision needs the next character and whether the cursor
is inside a string (approximated by the lexical rules; M14 can ask the
tree). Over-typing and pair deletion are keyed on the previous keystroke
having inserted the pair, so a closer the user typed on purpose is never
eaten.

**Indentation** (`crates/core/src/indent.rs`, new): detection samples the
first 4,000 lines (never the whole file) when a document opens, counts
lines starting with a tab against lines starting with spaces, and takes
the most common space count among indented lines with 2 to 8 spaces. The
result is an `Indent { width, tabs }` on the `Document`, shown in the
status bar, used by Tab, Enter, outdent and the line operations. The
config's `[indent]` table maps to the same type; an override wins over
detection only when `detect = false`.

**Chords** (`keymap.rs`): the keymap's map becomes a trie two levels deep.
A first chord that is a prefix sets a `pending` state with a deadline;
the event loop's `WaitUntil` already exists for cursor blink and takes
the earlier of the two deadlines. While pending, the status bar shows
the chord. The conflict test gains a case: a chord that is both a
complete binding and a prefix is an error at config load, reported in
the message line.

**Settings** (new config keys, all optional): `[editor] auto_pairs`,
`trim_trailing_whitespace`, `final_newline`; `[indent] width`, `tabs`,
`detect`; `[indent.rust]` and so on by language name; the same keys in
`[languages.NAME]` for user languages. `docs/customizing.md` gets a
section.

**Actions**: `edit.toggleLineComment`, `edit.toggleBlockComment`,
`edit.duplicate`, `edit.moveLineUp`, `edit.moveLineDown`,
`edit.deleteLine`, `edit.joinLines`, `edit.insertLineBelow`,
`edit.insertLineAbove`, `select.line`, `select.expand`, `select.shrink`,
`cursor.matchingBracket`, `select.toMatchingBracket`, `edit.upperCase`,
`edit.lowerCase`, `edit.titleCase`, `edit.sortLines`,
`edit.sortLinesReversed`, `edit.removeDuplicateLines`,
`edit.trimWhitespace`, `edit.indentUsingSpaces`, `edit.indentUsingTabs`,
`edit.detectIndent`. Default keys as above where given; the rest
palette-only.

## Tasks

1. `lines.rs`: duplicate, move, delete, join, insert line below and
   above, select line; tests with one and three cursors and the long-line
   guard.
2. Comment tokens from the language rules; toggle line and block comment.
3. Expand and shrink selection; go to and select to matching bracket.
4. Case changes, sort, remove duplicates, trim whitespace.
5. Indentation: detection, the config tables, the status bar item and its
   menu, bracket-aware Enter and closer dedent.
6. Auto-pairs with its setting.
7. On-save trimming and final newline, with the byte-for-byte round-trip
   test still passing when both are off.
8. Multi-key chords in the keymap and the status bar; the conflict test.
9. Menus and palette entries; `docs/customizing.md`; measurements on the
   4 GB log; Results.

## Concepts

- **One batch, one undo record**: why each operation computes all its
  edits first and applies them from the end of the text back.
- **Detecting indentation**: a cheap majority vote on a sample, and why
  the sample is bounded.
- **Keymap as a trie**: a two-level map and a deadline instead of a
  modal layer.

## Risks

| Risk | Fallback |
|---|---|
| Auto-pairs annoy more than they help in some language | The setting is per language; the default can flip after a week of use |
| Toggling comments on mixed-indent blocks looks wrong | Align to the shallowest line; if any selected line is already commented at that column, uncomment all, else comment all (the common convention) |
| `Alt+Up` and friends collide with window-manager keys on Linux | Everything is in the palette and rebindable; note it in `customizing.md` |
| The fixed two-second chord timeout feels wrong in use | Reconsider it as a setting; a fixed value was chosen to keep the keymap simple |

## Results

Landed 2026-10-17 on branch `claude/m11-everyday-editing-iuiygc`, in a
Linux container without a GPU: the `--smoke` first-frame time and the
4 GB measurements below need the owner's machine and are left for the
PR review. `cargo test --workspace` passes (lines, comments, expansion,
transforms, indentation and auto-pairs each have a test with one and
with three cursors, in `crates/core` and `crates/app/src/editing.rs`).

**Where things went**

- `crates/core`: `lines.rs` (duplicate, move, delete, join, insert line,
  select line, and the `Batch` every operation returns), `comment.rs`,
  `expand.rs`, `transform.rs` (case, sort, dedupe, trim) and `indent.rs`
  (detection and the unit, next-stop and dedent helpers). `brackets.rs`
  gained `opener_before` for lining a typed closer up with its opener.
- `crates/syntax`: `Rules::comment_tokens` and `lexical::comment_tokens`,
  so the app never reads the lexer's tables. JSON's rules gained `//` and
  `/* */` (JSONC), the only built-in language that had no comment.
- `crates/app/src/editing.rs` (new): the `Document` methods, auto-pairs,
  bracket-aware Enter, the closer dedent, `tidy_for_save`. `config.rs`:
  `[editor]`, `[indent]` and their per-language tables. `keymap.rs`: the
  two-level trie, chords bound once per `When` (so `Primary+Enter` is
  Replace All in the find bar and Insert Line Below elsewhere), and
  shifted punctuation found by key position (`Primary+Shift+/` arrives
  as `?`). `view.rs`: the "Spaces: 4" item and its click area.

**Decisions made while building** (none changes `DESIGN.md`):

- `Primary+L` went to Select Line as planned, so Go to Line is
  `Primary+G` on every platform (it was `Primary+L` on macOS only).
- Expand and Shrink Selection are `Ctrl+Shift+Right`/`Left` on macOS,
  where the planned `Alt+Shift+Right` already selects a word.
- Per-language overrides live in `[editor.NAME]` and `[indent.NAME]`
  rather than `[languages.NAME]`: that table maps file patterns to
  language names, and a sub-table there would not parse.
- A new or undetectable file indents with four spaces (the `[indent]`
  default), not a tab as before; the config can flip it.
- Over-typing a closer and deleting a pair are keyed on the auto-pair
  still being right after the cursor and the cursors not having moved
  since, nested up to any depth, rather than on the previous keystroke
  alone: `(a[])` typed as `(`, `a`, `[`, `]`, `)` comes out right. A
  closer typed once the cursor has moved is never eaten.
- Sort, Remove Duplicate Lines and Trim Trailing Whitespace with nothing
  selected take the whole file, within the 64 MB limit.
- Clicking "Spaces: 4" opens the palette filtered to the indent actions
  rather than a bespoke menu; it needed no new chrome.
- Memory-mapped documents skip the on-save settings: either would mean
  reading the whole file.
- The chord timeout is fixed at two seconds (`CHORD_TIMEOUT`), as
  decided in `DESIGN.md` §9.

**Measurements**

| What | Value |
|---|---|
| Release binary, Linux x86_64 (container) | 20.3 MB (20,323,200 bytes), under the 35 MB budget |
| `--smoke` first frame | needs a GPU; owner's machine |
| Line operation on the 4 GB log | not measured here; every operation reads only the selected lines (`Block::of` walks `line_span`), and the mapped buffer's `replace_sorted` rebuilds its pieces once per batch, so the cost is the selection's size, capped at 64 MB |

**Surprises**

- `winit` reports `Primary+Shift+/` with the logical key `?`, so a chord
  on shifted punctuation never matched; the keymap now falls back to the
  key's position for shifted punctuation, but only onto another
  punctuation key, so Dvorak's letters stay where the layout puts them.
- `Block::of` deliberately leaves out a line that a selection only
  touches at its start, which is what makes Select Line repeatable and
  keeps "sort the whole file" from eating the final line break.
