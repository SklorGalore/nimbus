# M9 — File diffing

**Status: planned**

## Goal

See what changed between two versions of a file, side by side, and take
changes from one side to the other, without leaving the editor.

## Done when

- [ ] "Compare with Saved" opens a diff of the active tab against the file
      on disk: your unsaved edits, highlighted. The right side is the live
      document; editing it updates the diff.
- [ ] "Compare with File…" diffs the active tab against any file, chosen
      with the file opener or a dialog. "Compare Tabs" diffs two open tabs.
- [ ] When a file changes on disk while you have unsaved edits (M6), the
      message line also offers "Compare", which opens the same view.
- [ ] Side-by-side view with rows aligned across the two sides: added,
      removed and changed lines have their own background colours, and
      the changed words within a changed line are marked more strongly.
      Both sides keep their syntax colours.
- [ ] A unified view (one column, `-` and `+` lines) is one toggle away.
- [ ] Next/previous change keys jump between hunks; a count ("3 of 12
      changes") shows in the diff bar.
- [ ] On a hunk, "Take Left" / "Take Right" copies that change into the
      editable side as one undoable edit; "Revert Change" in Compare with
      Saved puts the saved text back.
- [ ] Unchanged runs longer than a few screens fold to a "N unchanged
      lines" row that expands on click.
- [ ] Diffing two 10 MB files takes under 200 ms on a worker, with the view
      usable (plain, then coloured) meanwhile; keystrokes on the live side
      never wait for a re-diff.
- [ ] Optional: in a normal tab, gutter marks show lines changed since the
      last save (the same diff, drawn as thin bars).

## Scope

**In**: line diff with word-level refinement, side-by-side and unified
views, hunk navigation, taking hunks, compare with saved, file or tab;
diff colours in themes.

**Deferred**: three-way merge and conflict-marker resolution; comparing
folders; comparing against git revisions (it would need `git` on the path
or a git library; revisit once the basic view exists); ignoring whitespace
or case (easy to add later as diff options).

## Design

**Diff engine** (`crates/core/src/diff.rs`, pure, no I/O): lines are hashed
to integers, then diffed with a histogram or Myers algorithm. Candidate
crates are `imara-diff` (fast on large inputs, histogram algorithm) and
`similar` (also does inline word diffs). Measure both on the 10 MB bench
file and on binary size, and pick one; whichever is chosen, the result is
our own `Hunk { old: Range<usize>, new: Range<usize> }` list in line
numbers, so the view does not depend on the crate. Changed lines in a hunk
are paired and diffed again by words for the inline marks.

**Running it**: the first diff runs on a worker against two snapshots
(`Buffer::snapshot`). Edits to the live side bump its version; a re-diff
is scheduled after a short pause in typing (like find's re-search in M3),
and a result for an old version is dropped. While a diff is pending the
previous hunks are shown, shifted by the edit, as the syntax tree is in M5.

**Diff view** (`crates/app/src/diff_view.rs`): a tab kind holding two
documents (or one document and a read-only snapshot) and the hunk list. The
two columns share one scroll position measured in *aligned rows*: a row
pairs a left line with a right line, or with a filler where one side has
nothing. The editor `View` already draws a column at an origin and width
(M6), so each side is a `View` driven from the aligned rows. Soft wrap is
off in the diff view, so a row is always one line on each side.

**Themes**: new `[ui]` keys `diff_added`, `diff_removed`, `diff_changed`,
`diff_word` and `diff_filler`, with values in both built-in themes.

**Actions**: `diff.compareSaved`, `diff.compareFile`, `diff.compareTabs`,
`diff.next`, `diff.previous`, `diff.takeLeft`, `diff.takeRight`,
`diff.revert`, `diff.toggleUnified`; keys chosen when building, checked by
the keymap conflict test.

## Tasks

1. Diff engine in `core` with property tests: applying the hunks to the
   old text gives the new text, for random edits (an oracle as in M2).
2. Pick the crate by measurement; record it in DESIGN.md.
3. Aligned rows and folding of unchanged runs, unit-tested on small diffs.
4. Side-by-side rendering with diff colours and inline word marks.
5. Compare with Saved, with File, and Tabs; the M6 "Compare" offer.
6. Live re-diff after edits on a worker, dropping stale results.
7. Hunk navigation and taking hunks as undoable edits.
8. Unified view toggle.
9. Optional: changed-line gutter marks in normal tabs.

## Concepts

- **How diff algorithms work**: the longest common subsequence, why Myers
  finds the shortest edit script, and why histogram diff gives more
  readable hunks on code.
- **Alignment**: turning a list of hunks into rows that line up across two
  columns.
- **Versioned background work**: diffing on a worker against snapshots,
  and recognising stale results (the same idea as search and parsing).

## Risks

| Risk | Fallback |
|---|---|
| Diffing huge files (M7 mapped buffers) is too slow or too large | Diff only files under a size limit, as highlighting does; say so in the message line |
| Word-level refinement is slow on very long changed lines | Skip inline marks for lines over a length limit |
| Two editable sides make undo confusing | Only the live document is editable; the other side is a read-only snapshot |

## Results

_Fill in when done._
