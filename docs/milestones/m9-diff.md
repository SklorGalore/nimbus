# M9 — Compare and merge

**Status: planned**

## Goal

Compare two files and merge them, two-way or three-way, inside the
editor, with the same responsiveness as editing a single file: no
keystroke, scroll or click ever waits for diff work proportional to the
file's size.

## What it covers

| Task | Inputs | Result |
|---|---|---|
| Compare | two texts: tab and saved file, tab and file, two tabs, two files | side-by-side (or unified) view; the live side stays editable |
| Two-way merge | two texts | an editable result, built by taking changes from either side |
| Three-way merge | a base and two versions derived from it (yours and theirs) | an editable result where non-overlapping changes are applied and overlapping ones are conflicts to resolve |
| Resolve conflict markers | a file containing `<<<<<<<` / `=======` / `>>>>>>>` | the same three-way view, rebuilt from the markers |
| Command line | `nimbus --diff A B`, `nimbus --merge BASE LOCAL REMOTE -o RESULT` | usable as `git difftool` and `git mergetool`; the merge exits 0 when saved with no conflicts left, 1 otherwise |

## Performance budgets

These are the editor's existing budgets (DESIGN.md §1) applied to diff
views, plus the diff's own:

| What | Budget |
|---|---|
| Keystroke to pixels in any pane, including the merge result | under 8 ms, whatever the file size |
| Scrolling, jumping to the next change | one frame |
| Opening a compare or merge of two 10 MB files | first frame at once (text shown, "comparing…"); full diff on a worker in under 300 ms |
| Two 100 MB files | full diff on a worker in under 3 s; the view is usable meanwhile |
| Memory | about 8 bytes per line per side for the diff, plus the documents; no per-line arrays for alignment or colours |
| Files past the diff limit (mapped files from M7) | compared by streaming, or refused with a message-line note; decided by measurement in task 2 |

## Done when

- [ ] Compare: "Compare with Saved", "Compare with File…", "Compare Tabs",
      and `--diff A B`. Side-by-side, rows aligned across the sides;
      added, removed and changed lines coloured, the changed words within
      a line marked more strongly; both sides keep syntax colours. A
      unified view is one toggle away.
- [ ] Navigation: next and previous change keys; a count ("3 of 12") in the
      diff bar; long unchanged runs fold to "N unchanged lines".
- [ ] Two-way merge: "Merge Files…" opens two sides and a result column
      (starting as a copy of the left side); on each change, "Take Left",
      "Take Right" or "Take Both" edits the result as one undoable step.
- [ ] Three-way merge: `--merge BASE LOCAL REMOTE -o RESULT` and "Merge
      with Base…". Changes made on only one side are applied
      automatically; overlapping changes are conflicts, shown in the
      result and listed with a counter. Each conflict resolves by taking
      local, remote, both (in either order), or by editing the result by
      hand. Saving with conflicts left asks first.
- [ ] Opening a file with conflict markers offers "Resolve Conflicts",
      which opens the three-way view (with the base, when the markers are
      in diff3 style).
- [ ] When a file changes on disk while you have unsaved edits (M6), the
      message line also offers "Compare" and "Merge".
- [ ] Every budget in the table above, measured on the bench files and
      recorded in Results; typing in the merge result of two 10 MB files
      measured with `--bench-typing`.

## Scope

**In**: line diff with word-level refinement; compare, two-way and
three-way merge views; conflict markers in and out; command-line difftool
and mergetool modes; diff colours in themes.

**Deferred**: comparing folders; comparing against git revisions from
inside the editor (git can already call `--diff` and `--merge`); ignore
whitespace or case (diff options, easy later); merging more than three
inputs.

## Design

**Diff engine** (`crates/core/src/diff.rs`, pure, no I/O). Each line is
turned into a small integer token by an interner that maps equal lines to
the same token, read from the buffer's chunks without copying the text.
The token sequences are diffed with the histogram algorithm (readable
hunks on code, close to linear time in practice). Candidate crate:
`imara-diff`, chosen for speed on large inputs; `similar` is the fallback.
Task 2 measures both on the bench files and on binary size. Whichever is
used, the output is our own type, so views never see the crate:

```rust
pub struct Hunk { pub old: Range<usize>, pub new: Range<usize> }   // line ranges
pub struct Merge { pub chunks: Vec<MergeChunk> }                   // three-way
pub enum MergeChunk { Same(Range<usize>), Ours(..), Theirs(..), Both(..), Conflict { base, ours, theirs } }
```

A three-way merge is two diffs (base to local, base to remote) walked
together: regions changed on one side only take that side, regions
changed identically on both take either, and the rest are conflicts.

**Performance by design**:

- *Never on the main thread in proportion to size.* Tokenizing and the
  full diff run on a worker against snapshots (`Buffer::snapshot`); the
  view shows the texts unaligned with "comparing…" until the result
  arrives, as highlighting does in M5.
- *Edits re-diff locally.* An edit on the live side changes only the
  lines it touches; the hunks before and after it are unchanged. The
  re-diff runs on just the region between the nearest unchanged anchors
  either side of the edit, on the main thread when that region is small
  (it almost always is). A region over a budget goes to a worker, and the
  previous hunks, shifted by the edit, are shown until it returns. That is
  the M5 reparse pattern again.
- *Alignment without per-line arrays.* Rows on screen are worked out from
  the hunk list: a prefix sum of each hunk's row count, then a binary
  search from a row to its hunk. That costs O(log hunks) per row looked
  up, and O(hunks) memory. Folding is a flag on the hunk gaps.
- *Only what is visible.* Word-level marks are computed for the changed
  lines on screen and cached by version, like highlight spans; very long
  lines skip them.
- *Large files.* Tokens cost 4 bytes per line per side and the interner
  holds one entry per distinct line. The size limit for diffing, and
  whether mapped files (M7) stream through the tokenizer instead, is
  decided in task 2 from measurements.

**Views** (`crates/app/src/diff_view.rs`, `merge_view.rs`). A compare or
merge is a tab kind holding two or three documents, or snapshots for the
read-only sides, plus the hunks or merge chunks and their version
numbers. Each column is an ordinary editor `View` at its own origin and
width (M6 added that). The columns share one scroll position in aligned
rows, and are drawn only for the visible rows. Soft wrap is off in these
views, so one row is one line on each side. The merge result is a normal
`Document`: undo, find, multi-cursor (M8) and highlighting all work in it.
Taking a change is one `replace_many` edit.

**Conflict markers**: parsing `<<<<<<<`, `|||||||` (diff3 base),
`=======` and `>>>>>>>` gives the three versions of each conflicted
region; the rest of the file is common text. Saving a merge with conflicts
left can write markers back, so other tools still see them.

**Themes**: new `[ui]` keys `diff_added`, `diff_removed`, `diff_changed`,
`diff_word`, `diff_filler` and `merge_conflict`, set in both built-in
themes.

**Actions**: `diff.compareSaved`, `diff.compareFile`, `diff.compareTabs`,
`diff.next`, `diff.previous`, `diff.toggleUnified`, `merge.files`,
`merge.withBase`, `merge.resolveMarkers`, `merge.takeLeft`,
`merge.takeRight`, `merge.takeBoth`, `merge.nextConflict`; keys chosen when
building, checked by the keymap conflict test.

## Tasks

1. Diff engine and interner in `core`, with property tests: applying the
   hunks to the old text gives the new text, for random edits (a
   `String` oracle, as in M2).
2. Measure `imara-diff` and `similar` on the bench files (time, memory,
   binary size); pick one; set the diff size limit; record both in
   DESIGN.md.
3. Local re-diff after edits, with an oracle test: after random edits, the
   locally updated hunks describe the same change as a full re-diff.
4. Three-way merge on top of two diffs, with property tests (a side
   changed alone is taken; identical changes are taken once; the result
   with every conflict resolved one way equals that side's text).
5. Aligned rows, folding and the prefix-sum index, with unit tests.
6. Compare view: two columns, diff colours, word marks, navigation,
   unified toggle; the compare actions and `--diff`.
7. Two-way merge view and taking changes.
8. Three-way merge view, conflict list, `--merge` and its exit code.
9. Conflict markers: parse into the merge view, and write back on save.
10. The M6 "Compare"/"Merge" offers on changed-on-disk files.
11. Benchmarks for every budget; Results.

## Concepts

- **How diff algorithms work**: the longest common subsequence, Myers'
  shortest edit script, and why histogram diff reads better on code.
- **Three-way merge**: why the common ancestor turns "these differ" into
  "who changed what", and where conflicts come from.
- **Interning**: turning lines into integers so comparing two lines is
  comparing two numbers.
- **Keeping work local**: why an edit only needs the diff re-done between
  its neighbouring anchors, and the same pattern in M5's reparsing.

## Risks

| Risk | Fallback |
|---|---|
| A huge single hunk (a whole file rewritten) makes the local re-diff region the whole file | It is sent to a worker past a budget, as an over-budget reparse is; the shifted old hunks stay on screen |
| Word-level marks are slow on very long changed lines | Skip them past a length limit |
| Three columns are too narrow on a laptop screen | The merge view can hide the two sides and show the result with inline conflict blocks |
| Mapped files (M7) are too big to tokenize fully | A size limit with a message-line note; streaming tokenization if measurement shows it is cheap enough |

## Results

_Fill in when done._
