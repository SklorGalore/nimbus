# M9 — Compare and merge

**Status: landed, pending the owner's look** (2026-10-17; every task is in and measured; the owner has confirmed the compare view (task 6) on screen and has yet to look at the merge views and the M6 offers (tasks 7 to 10); long-line splitting, the plan's layer 4, is deferred to the backlog)

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
| Files past the diff limit (mapped files from M7) | still compared, in layers (below); refused only for binary files |

## Done when

- [x] Compare: "Compare with Saved", "Compare with File…", "Compare with
      Tab…" (the only other tab, or a picker), and `--diff A B`.
      Side-by-side, rows aligned across the sides; added, removed and
      changed lines coloured, the changed words within a line marked more
      strongly; both sides keep syntax colours. "Toggle Unified Compare"
      stacks each change's old lines over its new ones in one column.
      **Owner confirmed on screen** (2026-10-17): built and tested
      headlessly here (no GPU in the build container), then checked on a
      real display.
- [x] Navigation: `F7` / `Shift+F7` step through the changes; the status
      bar shows "Compare with saved · change 3 of 12" (the plan's diff
      bar became part of the status bar: no layout change, and the find
      bar keeps its place); long unchanged runs fold to "··· N unchanged
      lines ···" with three lines of context, and a click on the fold
      opens it.
- [x] Two-way merge: "Merge Files…" opens the left and right files with
      the result between them (a copy of the left) in a new tab; "Take
      Left", "Take Right", "Take Both" and "Take Both, Right First" edit
      the result as one undoable step for the change under the cursor;
      the status bar says which side a change took, or that it was edited.
      **Owner to confirm on screen** (built and tested headlessly).
- [x] Three-way merge: `--merge BASE LOCAL REMOTE -o RESULT` and "Merge
      with Base…" (the active tab is ours; the base and theirs are
      picked; the result opens in a new tab). Changes made on only one
      side are applied; overlapping changes are conflicts, written into
      the result as git-style marker blocks and counted ("2 conflicts
      left"); `F8` / `Shift+F8` step through them. Each resolves by taking
      left, right, both in either order, or by editing by hand. Saving
      with conflicts left asks first; `--merge` exits 0 when the result
      was saved with none left, 1 otherwise. **Owner to confirm on
      screen.**
- [x] Opening a file with conflict markers says so and offers "Resolve
      Conflicts", which splits the text at its markers (with the base,
      when they are in diff3 style), merges the three parts again and
      puts the three-way view on the tab; the blocks come back as marker
      blocks, labelled "ours" and "theirs", until each is resolved. The
      tab's undo history starts over at that point.
- [x] When a file changes on disk while you have unsaved edits (M6), the
      message also offers "Compare with Saved" and "Merge with Disk": a
      three-way merge of the text as last loaded (a snapshot every rope
      document keeps from its last open, reload or save), the unsaved
      edits, and the file as it is now, in place of the tab's document.
- [x] Every budget in the table above, measured on the bench files and
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
The token sequences are diffed with Myers' algorithm from `imara-diff`
(**changed in task 2**: the plan said histogram, for readable hunks on
code, but histogram ignores lines that occur more than 63 times as
anchors, and in source files and logs most lines do; see Results for the
numbers, including `similar`). Hunks are then slid to indentation-friendly
positions with imara's heuristic, which recovers most of what histogram
is liked for. The output is our own type, so views never see the crate:

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
- *Large files, in layers.* A full diff costs 4 bytes per line per side
  for tokens, plus one interner entry (16 to 24 bytes) per distinct line;
  a 4 GB log with unique timestamps would need about 1 GB for the
  interner alone. So the work is layered, each layer exact or labelled:
  1. **Trim the common start and end.** Compare the two files byte by
     byte from both ends (straight from the mapping, at memory or disk
     speed, no extra memory) and diff only the middle. This alone covers
     the usual huge-file cases exactly: a log that grew, a big file with a
     few edits, a file changed on disk (M6).
  2. **Full diff of the middle** when it is under the diff limit, as for
     any file.
  3. **Anchored block diff** when the middle is over it: lines that occur
     exactly once in each file and match, in order, become anchors, and
     each gap between anchors is diffed on its own, streaming, with memory
     bounded by the largest gap. The result is always a correct edit, but
     may not be the smallest one, so the diff bar says "large file:
     changes may not be minimal". A gap still over the limit is shown as
     one changed block.
  4. **Long lines are split for diffing** at every size: a line over a
     few kilobytes is cut into pieces at content-chosen points (a rolling
     hash, so the cut points survive insertions), which gives real diffs
     of minified JSON and similar one-line files.
  5. **Refused only** for binary files (a NUL byte in the first block) and
     when the anchored diff finds no anchors at all, with a message-line
     note.

  Three-way merge uses the same layers on each of its two diffs; trimming
  usually makes merging even very large files cheap. The diff limit itself
  (where layer 3 takes over) is set from measurements in task 2.

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
2a. The large-file layers: common start and end trimming, the anchored
    block diff, and long-line splitting, each with a property test that
    the hunks turn the old text into the new; plus the 1 GB and 4 GB log
    bench files (`bench/gen.sh logs`) for the budgets.
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
| Files with few unique lines (generated data, repeated records) give the anchored diff no anchors | Fall back to one changed block per gap, labelled; trimming still finds the unchanged start and end |

## Results

_In progress; filled in as tasks land._

**Task 2: algorithm and crate** (2026-10-17, release build, Linux
container with a shared, noisy CPU; the owner's machine is likely faster
and steadier). Two inputs: the bench pairs from `bench/gen.sh diff`
(`src-10mb.rs`, the project's sources repeated, and `src-10mb-edited.rs`,
a copy with one change every thousand lines, cycling through a changed, a
deleted and an inserted line; likewise at 100 MB) and a synthetic
source-like text with a long tail of distinct lines (in `diff_bench`).
Interning is the same for every algorithm; the table shows the diff step.

| Pair | Lines | Intern | imara Myers | imara Histogram | `similar` Myers |
|---|---|---|---|---|---|
| bench 10 MB + edited | 295k | 123 ms | 34 ms, 295 hunks | 43 ms, **186 hunks** (coarse) | — |
| bench 100 MB + edited | 2.95M | 1.26 s | 348 ms, 2952 hunks | 403 ms, 2952 hunks | — |
| synthetic 6 MB + 200 edits | 250k | 78 ms | 12 ms | 114 ms | 48 ms |
| synthetic 60 MB + 2000 edits | 2.5M | 1.18 s | 144 ms | **19.3 s** | 47 ms |
| unrelated 6 MB pair (worst case) | 250k | 46 ms | 560 ms, 66k hunks | 294 ms, 9k hunks | — |

Histogram is out: it either falls apart (19 s) or gives coarse hunks on
repetitive text (186 hunks for 295 edits on the 10 MB pair), and nearly
every real input is repetitive. Between the two crates, `similar`'s Myers is faster on the synthetic pair but its
binary cost is higher (a minimal release binary grew 145 KB with
`similar` against 24 KB with `imara-diff`, both with fat LTO), it has no
token-slice entry point without copying our tokens into its types, and
`imara-diff` ships the indentation slider that makes hunks land on
function boundaries. `imara-diff` 0.2 with `Algorithm::Myers`, then.

Budgets: two 10 MB files diff in about 110 ms (budget 300 ms); two 100 MB
files in about 1.5 s (budget 3 s). The diff limit (`DIFF_LIMIT`) is
256 MB per side after trimming, set by memory, not time: tokens are
4 bytes per line per side and the interner about 40 bytes per distinct
line (more than the plan's 16 to 24: each entry keeps where a copy of the
line lives, for comparing on hash collision, so a collision can never
produce a wrong diff).

**Task 2a: layers** (`crates/core/src/compare.rs`; same machine). The
common start and end are trimmed byte by byte from storage, the middle
gets a full diff within `DIFF_LIMIT` (256 MB per side), and past that it
is walked in 32 MB windows, each diffed on its own and kept up to the
last change not touching the window's end, where the next window starts;
a window pair with nothing in common is doubled three times, then
reported as one block. Measured with `compare_bench` on the pairs from
`bench/gen.sh diff` and `bench/gen.sh difflogs` (the 1 GB log with one
edit every 100,000 lines, the 4 GB log with 100,000 lines appended);
ropes in memory, so the loaded size is in the peak:

| Pair | Compared in | Hunks | Lines through the diff | Exact | Peak RSS |
|---|---|---|---|---|---|
| bench 10 MB + edited | 110 ms | 295 | all (edits span the file) | yes | — |
| bench 100 MB + edited | 1.5 s | 2952 | all | yes | — |
| 1 GB log + 164 scattered edits | 9.7 s | 164, all found | all, in 33 windows | no (windowed) | 2.4 GB, of which 2.2 GB the two ropes |
| 4 GB log + 100,000 lines appended | 4.0 s | 1 | 100,000 (the rest trimmed) | yes | 9.2 GB, the two ropes; the app maps files this size |

The trim reads both texts once, about 2.3 GB/s through rope chunks; on a
mapped file that is the file's read speed. The windowed layer costs
memory proportional to one window pair, not the file. Long-line
splitting (the plan's layer 4) is left for task 6, where the views
decide what a split line looks like.

**Task 3: local re-diff** (`crates/core/src/live_diff.rs`). A `LiveDiff`
keeps both texts' tokens and the hunks. `edit` replaces the tokens of the
lines an edit touched, shifts the hunks after them, takes out the hunks
they meet (plus three lines of context either side, absorbing any hunk
the context reaches), and returns the region to re-diff, with the other
side's range found by counting from the texts' ends through the hunks
that remain; `rediff` diffs that region alone and splices the result in.
The two steps are separate so the app can run the diff in place when the
region is small and defer it when a rewrite makes it the whole file; a
second edit before the re-diff widens the pending region. Hunks are
always sorted, apart and in current line numbers: a pending region has
no hunks in the list, and the ones it took out ride along in
`Region::stale` for a view that wants to keep showing them. The oracle
property applies one to seven random edits to either side, some left
pending, and checks the hunks reproduce the texts after each re-diff. It
caught two design slips: stale hunks left in the list were shifted a
second time by the next edit, and the other side's region end was
computed with saturating arithmetic that silently clamped.

**Task 5: aligned rows** (`crates/core/src/align.rs`). `Rows` describes
two or three columns as segments: an equal run (one row per line, or
`2 × context + 1` rows when folded) or a change (as many rows as its
longest side, filler in the others). A prefix sum of segment rows and a
binary search take a row to its cells and a line to its row in
O(log segments); memory is O(segments); folding toggles re-sum. Change
navigation ("3 of 12", next, previous) reads the same list. Generic over
the change tag, so the merge view's three columns use the same model
with `merge::Kind`.

**Task 6: the compare view** (`crates/app/src/compare.rs`, with
`View::draw_rows`, `Layout::column` and `begin_frame` in `view.rs`). A
compare sits on an ordinary tab: the tab's document stays the editable
right side, and the tab gains a read-only left side plus the diff. The
first diff runs on a worker over snapshots ("comparing…" until it
arrives, as highlighting does); within `DIFF_LIMIT` it is a `LiveDiff`
that follows each edit through `Document::take_edits` (a log every edit
site feeds, undo and redo included; a reload marks it so the compare
starts over), and a local re-diff over the 20,000-line budget, or past
the limit, goes back to the worker after a 300 ms pause in typing. Rows
come from `Rows`; only the rows on screen are planned and drawn, and the
changed-word marks (common start and end of a changed pair trimmed, for
lines under 4 KB) are worked out for those alone. `View::draw_rows`
draws a column from a row plan (a line, a filler, or a fold label) and
shares `draw_line` with the ordinary `draw`, so selection, cursor,
matches, IME and syntax colours come for free; `Layout::column` narrows
a layout to one column, and everything that took `width` as the right
edge stops at the column's edge. Horizontal scroll is shared; soft wrap
is off while comparing. `Compare::start_sync` runs the diff in place for
`--headless` and the tests. Deviations from the plan: the diff bar's
text lives in the status bar; "Compare with Tab…" compares with a copy
of the other tab's text (edits there are not followed); drag selection
in a compare is off for now (click and keyboard selection work).

**Tasks 7 and 8: the merge views** (`crates/app/src/merge_view.rs`, on
`columns.rs`, which the compare view now shares: scrolling, folds, row
plans, drawing and clicks for two or three columns). A merge tab's
document is the result; the inputs are read-only beside it, ours on the
left and theirs on the right (the base of a three-way merge is not
shown). The result is never diffed again: `core::regions` keeps each
chunk's line range in the result through every edit the document reports
(typing, undo, redo), joining regions an edit spans and recording which
side a take put there, so typing in the result costs what typing anywhere
costs. A two-way merge is the three-way machinery with the left text as
base and ours, so every difference is a `Theirs` chunk and the result
starts as the left text; a three-way result starts as `Merge::write` with
conflicts as marker blocks. Taking a side is one `replace` of the
region's lines, so it undoes like any edit. The merge itself (two
diffs and the walk) runs on a worker, as the compare's first diff does:
the tab appears at once with the three texts unaligned and "merging…" in
the status bar, and edits, takes and saves wait until the result arrives
(`AppEvent::Merged`); a three-way result starts empty and takes the
merged text when it comes, with no undo history before it. Deviation:
"Merge with Base…" and "Merge Files…" put the result in a new untitled
tab rather than over the active one.

**Tasks 9 and 10: markers in, and the M6 offers.** `core::markers::split`
reads a conflicted file back into base, ours and theirs (the common text
into all three, each block's parts into their own); merging those again
finds the same conflicts, so "Resolve Conflicts" is the ordinary
three-way view over the tab's own file. `Document` keeps a snapshot of
the text as the file held it at the last open, reload or save (ropes
share storage, so it costs nothing until edits part them), which is the
base "Merge with Disk" needs when a file changed under unsaved edits.

Interning dominates the time: about 230 MB/s, mostly the hash-table
lookup and the byte comparison that confirms each repeated line. One
chunk lookup per line was removed in task 2 (20% faster); a faster hash
table is the next step if it ever matters.

Surprise: the two buffer backends at first produced different hunks for
the same texts, caught by the oracle's `backends_agree` property. The
indentation heuristic read each line's indent from its first storage
chunk, and the mapped backend's tiny test pieces split some indents. The
indent is now gathered across chunks, so the diff depends only on the
text.

**Task 11: budgets** (2026-10-17, release build, same container; typing
measured by `compare_typing_bench` and `merge_typing_bench`, ignored
tests in `crates/app` that open the bench pair, type 300 characters at
six places with every seventh a newline, and time each keystroke's edit,
diff or region tracking, and row planning for one screen; drawing needs a
GPU, and its cost is the ordinary editor's, measured in M3 and M8). The
three-way merge pairs the bench pair with the old file changed on every
400th line, so it has conflicts to take.

| What | Budget | 10 MB pair | 100 MB pair |
|---|---|---|---|
| Compare: diff plus rows, on the worker | 300 ms / 3 s | 149 ms (295 changes) | 1.05 s (2952 changes) |
| Three-way merge: two diffs, the walk, the result text, rows | 300 ms / 3 s | 201 ms (936 changes, 98 conflicts) | 2.0 s (9351 changes, 983 conflicts) |
| Keystroke in the compare's right side, p50 / p99 / max | 8 ms | 19 µs / 72 µs / 112 µs | 63 µs / 0.6 ms / 1.2 ms |
| Keystroke in the merge result, p50 / p99 / max | 8 ms | 32 µs / 59 µs / 98 µs | 0.3 ms / 1.1 ms / 3.0 ms |
| Taking a side in the merge (replace, regions, rows) | one frame | 38 µs p50, 86 µs max | 0.3 ms p50, 0.36 ms max |
| Scrolling, next change | one frame | rows are planned for the screen only (O(visible)); next change is a binary search over the rows | same |
| First frame of a compare or merge | at once | the texts appear unaligned with "comparing…" or "merging…" until the worker answers; no diff work on the main thread | same |
| Memory for the diff | about 8 bytes per line per side | 4-byte tokens per line per side plus the interner (task 2, above); rows are O(hunks) | same |

The merge result's keystroke on the 100 MB pair is the one number with
visible room to shrink: each edit walks the regions after it to shift
their result lines (about 20,000 regions) and rebuilds the rows' prefix
sum (77,000 rows), both linear in the number of changes rather than the
file size, and still a quarter of a frame at a hundred megabytes. A
segment tree over the regions would make it logarithmic if a merge ever
needs it.

Startup is unchanged in principle: a bare start does none of this
(`compare.rs` and `merge_view.rs` run only when a compare or merge is
opened), so `--smoke` is the owner's to re-measure; this container has no
GPU. The release binary on Linux is 20.2 MB (19.2 MiB; budget 35 MB), of
which `imara-diff` is about 60 KB.

**Deferred from the plan**: long-line splitting (layer 4). Minified JSON
and other one-line files compare as one changed line today; splitting a
line at content-chosen points for the diff needs the views to show a
split line's pieces, which is a view design question rather than a diff
one. It is in `backlog.md` with the other diff options.

