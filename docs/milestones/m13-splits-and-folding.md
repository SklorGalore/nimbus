# M13 — Split views and folding

**Status: in progress** (everything but what needs M12's session, which waits for it to land).
Planned after M12. Written on 2026-10-17 from items
deferred by M5 (code folding) and M8 (split views); M6 prepared for
splits by giving the `View` an origin and width. Approved the same day:
up to three columns with no nesting, and the same file open in two of
them, which puts the document-and-pane refactor in scope (`DESIGN.md`
§9). The split design still has real uncertainty: run it through a
`Plan` agent (CLAUDE.md, Subagents) before writing code.

## Goal

Two places on screen at once: a second editor column, and the ability to
fold away the parts of a file you are not reading, each at the same
frame budget as a single view of any file size.

## Done when

- [x] **Split**: "Split Right" (`Primary+\`) opens a second editor group
      to the right with the current tab; up to three groups side by side,
      with no nesting and no vertical stacking, each with its own
      tab strip, scrollbar and find bar state. Dividers drag; widths are
      remembered in `state.toml` with the session (M12). *(Widths in
      `state.toml` wait for M12, which owns that file.)*
- [x] The same file can be open in two groups: edits in one show in the
      other at once, each group keeps its own cursor, selection and
      scroll, undo in either group undoes the document's last edit, and
      the dirty dot and save apply once. Closing the last tab of a group
      closes the group.
- [x] "Focus Left/Right Group" (`Primary+Alt+Left/Right`), "Move Tab to
      Next Group", "Close Group"; the sidebar and the file opener open
      into the focused group; `--diff` and merge tabs (M9) fit in a group
      as they do in the single column today.
- [x] **Folding**: fold markers in the gutter for indentation-based
      regions in any file, and for syntax regions where a tree exists
      (M14 adds the fold queries; M13 ships indentation and bracket
      regions). "Fold" (`Primary+Alt+[`), "Unfold" (`Primary+Alt+]`),
      "Fold All", "Unfold All", "Fold Level 1–3". A folded line shows its
      first row and a `…` chip with the hidden line count; clicking the
      chip unfolds.
- [x] Folds behave: an edit inside a fold unfolds it; a find match or a
      go-to-line inside one unfolds it; cursor movement steps over folded
      lines; selection through a fold selects the hidden text; copy
      includes it; soft wrap and folding work together.
- [x] Budgets: on the 4 GB log, folding and unfolding a region, scrolling
      with a thousand folds, and typing in two groups showing the same
      file all stay under 8 ms per frame; folding memory is O(folds), not
      O(lines).
- [ ] Folds are kept in the session (M12) by byte range, and dropped when
      the file changed on disk. *(Waits for M12's session; `Folds::all`
      is the list to save.)*

## Scope

**In**: editor groups, shared documents, fold model and rendering,
indentation and bracket regions, the actions and keys.

**Deferred**: nested or grid layouts; a minimap; sticky scroll (the
enclosing scope pinned at the top); syntax fold regions (M14, one task
there once the queries exist).

## Design

**Documents and panes**. Today a `Tab` owns its `Document` and `View`,
and the `Document` holds the selections. Two views of one document need
the text and history shared and the selections per view, so the two are
split:

- `Document` keeps the buffer, path, line ending, history, syntax state,
  stamp and dirty state. Documents move into a `Vec<Document>` arena on
  the app, addressed by `DocId`; a document closes when its last pane
  closes.
- `Pane { doc: DocId, view: View, selections: Selections, goals,
  preferred_column, folds: Folds }` is what a tab in a group holds. The
  `core` multi-cursor functions already take selections as a parameter
  (`multi.rs`), so moving them out of `Document` is a plumbing change,
  not a logic change.
- Undo records keep "selections before and after" (M3). On undo the
  pane that invoked it takes the recorded selections; other panes on the
  same document map their selections through the edit, as they do for
  any edit from elsewhere (the `Selections::apply_edit` path M8 uses when
  one cursor's edit moves another).
- Syntax state is per document, so both panes read one tree; the
  per-view highlight cache (M5) is already in the `View`.

**Groups** (`crates/app/src/groups.rs`). `Vec<Group>` with the focused
index; a `Group` is `Vec<Pane>` plus its active index and tab strip
state. Layout gives each group a column between `Layout::left` and the
window's right edge from the stored widths; each pane's `View` is drawn
at its group's origin, which M6 made possible. Input is routed by the
mouse position to a group, and keyboard input to the focused one. The
find bar becomes per group (it is a few small `Document`s, cheap).

**Folds** (`crates/core/src/fold.rs`). A fold is a byte range covering
whole lines, from the end of the first line to the end of the last
hidden line. `Folds` is a sorted `Vec<Range<usize>>`, merged when they
nest (the outer wins; unfolding restores the inner ones kept in a second
list). Rows map through folds the way they map through soft wrap: the
view's row walker (`wrap::step_rows`, `rows_between`) gains a step that,
on entering a fold's start line, jumps to its end; that is one binary
search per fold boundary crossed, so scrolling costs O(log folds) per
row and no per-line array exists, the M7 rule. Edits shift folds with
the same arithmetic selections use, and an edit inside one removes it.

**Fold regions**. Indentation regions are computed on demand for the
lines on screen, by scanning forward from a line to the next line with
smaller indentation, bounded by a line limit so a 4 GB file with one
indent level does not scan to the end (past the limit the marker is not
offered). Bracket regions use the M8 matcher the same way. "Fold All"
and "Fold Level N" scan the whole file on a worker over a snapshot, as
search does, and are refused past the diff-style size limit with a note.
Syntax regions (M14) will come from a `folds.scm` query over the visible
range plus a margin, the same shape as highlighting.

**Rendering**. The gutter shows a chevron on fold starts under the mouse
or when folded; the chip after a folded line's first row is text in the
UI colour (`fold_chip`, a new `[ui]` theme key), drawn by the same glyph
pipeline.

**Actions**: `view.splitRight`, `view.closeGroup`, `view.focusLeftGroup`,
`view.focusRightGroup`, `tab.moveToNextGroup`, `fold.fold`,
`fold.unfold`, `fold.foldAll`, `fold.unfoldAll`, `fold.level1` to
`fold.level3`, `fold.toggleAtCursor`.

## Tasks

1. `Plan` agent pass over the document-and-pane split, reviewed by the
   owner; then the refactor itself with no behaviour change (the
   screenshot test and all action tests still pass).
2. Groups: layout, dividers, focus, routing of mouse and keyboard, per
   group find bar; the split, focus and move actions; widths in
   `state.toml`.
3. Shared documents: the same file in two groups, undo and dirty
   semantics, close-when-last.
4. `fold.rs`: the fold set, row mapping through folds, shifting on
   edits, with property tests against a `String` oracle (rows visible
   through folds equal the lines not inside any fold).
5. Indentation and bracket regions on demand; the gutter markers and
   chip; the fold actions and keys.
6. Fold interactions: unfold on edit, find and go-to-line; movement over
   folds; copy through folds; folds with soft wrap.
7. Fold All and levels on a worker; folds in the session.
8. Measurements on the 4 GB log and a two-group session; Results.

## Concepts

- **Separating the text from the view of it**: one document, many
  panes, and why the selections had to move.
- **Row mapping as a chain**: logical line, then soft wrap, then folds,
  each step O(log n) and none needing a per-line array.
- **Undo with several cursors on one history**: whose selection comes
  back.

## Risks

| Risk | Fallback |
|---|---|
| The pane refactor touches every action and takes longer than planned | Land it alone as task 1 with no behaviour change; the milestone's features follow on top |
| Two panes editing one document at once (both with blinking cursors) confuse the IME and the blink timer | Only the focused pane shows a blinking cursor and talks to the IME; the other shows a still bar |
| Indentation folds on YAML or Python feel wrong at blank lines | Blank lines belong to the region before them, the common convention |
| Three groups on a 13-inch screen are unreadable | Allow three but default to two; widths are draggable |

## Results

Built 2026-10-18 on branch `claude/m13-splits-1x25l5` (draft PR #25), in a
Linux container without a GPU, so the per-frame numbers below are the CPU
side of a frame; drawing a folded screen costs what any screen costs. The
`--smoke` time and a look at the groups and chips need the owner's Mac.
`cargo test --workspace` passes; the fold model has property tests
against a `String` oracle (`crates/core/src/fold.rs`), and
`fold::indent_regions` is checked against `indent_region` line by line.

**Measured** (`fold_bench` in `crates/app/src/view.rs`, release, the
4 GB log mapped):

| What | p50 | p99 |
|---|---|---|
| Fold and unfold one region | 13 µs | 40 µs |
| Scroll a screen with 1000 folds (row walk and a click) | 10 µs | 28 µs |
| Keystroke in one group, the other group on the same file following | 7 µs | 19 µs |

Making the 1000 folds took 13 ms (`Folds::fold_many`); they take 16 KB,
16 bytes a fold and nothing per line.

**Where things went**

- `crates/core/src/fold.rs`: `Folds` (active and inner lists), row
  mapping through `WrapView::with_folds`, `apply_edits` and
  `apply_change` for edits, `fold_many`, the region functions
  `indent_region`, `bracket_region`, `indent_regions` (whole file, one
  pass) and `at_depth`.
- `crates/syntax/src/structure.rs`: `FoldSource`, a copy of the tree for
  Fold All's worker (M14's fold queries, now fed into the fold model).
- `crates/app/src/document.rs`: `Carets` (one pane's cursors), `Editor`
  (a document with one pane's carets, what editing methods run on),
  `Solo` (a document with its own carets, for one-line fields), and the
  `Journal` of edits other panes follow. `docs.rs`: the document list.
- `crates/app/src/groups.rs`: the columns, divider dragging, and the
  bookkeeping when a tab closes or moves. `folding.rs`: which line can
  fold, and the Fold All worker. `view.rs`: folded rows, the chip, the
  gutter marker.

**Decisions made while building**

- Tabs stay one flat list, each naming its group. Everything that works on
  every open tab (saving, file watching, results found by index) is
  unchanged; the strips, focus and layout are per group.
- Another group's cursor shows still rather than blinking, and only the
  focused group talks to the IME.
- Undo in either group undoes the document's last edit; the group that
  asked gets the recorded selections, the other moves its cursors through
  the change.
- A grammar region folds down to the line before its closing `}`, like a
  bracket region, so the closer stays in view.
- Fold All, Unfold All and the levels are `Primary+K` chords
  (`Primary+K Primary+0`, `… J`, `… 1` to `3`), as in VS Code; Toggle Fold
  is `Primary+K Primary+L`. Fold All is refused past the compare view's
  256 MB limit.
- Versions: nimbus 0.5.0, nimbus-core 0.4.0, nimbus-syntax 0.2.1.

**Surprises**

- Moving the cursors out of the document touched almost every editing
  call; doing it first with no behaviour change, as the plan said, kept
  the rest small.
- Folds needed the same journal as the second pane: a view's folds follow
  its own edits as well as another pane's, so every edit to a document in
  a tab is kept until the next command or frame.

