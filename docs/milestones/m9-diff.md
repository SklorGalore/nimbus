# M9 — Compare and merge

**Status: in progress** (started 2026-10-17; tasks 1, 2, 2a and 3 landed; 2a's long-line splitting waits for task 6)

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
container; the owner's machine is likely faster). Two inputs: the
owner's bench pair (`bench/out/src-10mb.rs`, the project's sources
repeated, and a copy with one edit per thousand lines) and a synthetic
source-like text with a long tail of distinct lines. Interning is the
same for every algorithm; the table shows the diff step.

| Pair | Lines | Intern | imara Myers | imara Histogram | `similar` Myers |
|---|---|---|---|---|---|
| bench 10 MB + edited | 295k | 84 ms | 10 ms, 291 hunks | 16 ms, **83 hunks** (coarse) | — |
| bench 100 MB + edited | 2.95M | 848 ms | 115 ms, 3146 hunks | 229 ms, 3146 hunks | — |
| synthetic 6 MB + 200 edits | 250k | 78 ms | 12 ms | 114 ms | 48 ms |
| synthetic 60 MB + 2000 edits | 2.5M | 1.18 s | 144 ms | **19.3 s** | 47 ms |
| unrelated 6 MB pair (worst case) | 250k | 46 ms | 560 ms, 66k hunks | 294 ms, 9k hunks | — |

Histogram is out: it either falls apart (19 s) or gives coarse hunks on
repetitive text, and nearly every real input is repetitive. Between the
two crates, `similar`'s Myers is faster on the synthetic pair but its
binary cost is higher (a minimal release binary grew 145 KB with
`similar` against 24 KB with `imara-diff`, both with fat LTO), it has no
token-slice entry point without copying our tokens into its types, and
`imara-diff` ships the indentation slider that makes hunks land on
function boundaries. `imara-diff` 0.2 with `Algorithm::Myers`, then.

Budgets: two 10 MB files diff in about 95 ms (budget 300 ms); two 100 MB
files in about 0.96 s (budget 3 s). The diff limit (`DIFF_LIMIT`) is
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
reported as one block. Measured with `compare_bench` (ropes in memory,
so the loaded size is in the peak):

| Pair | Compared in | Hunks | Lines through the diff | Exact | Peak RSS |
|---|---|---|---|---|---|
| bench 10 MB + edited | 100 ms | 291 | all (edits span the file) | yes | — |
| bench 100 MB + edited | 0.99 s | 3146 | all | yes | — |
| 1 GB log + 164 scattered edits | 8.4 s | 164, all found | all, in 33 windows | no (windowed) | 2.4 GB, of which 2.2 GB the two ropes |
| 4 GB log + 100,000 lines appended | 3.65 s | 1 | 100,000 (the rest trimmed) | yes | 9.2 GB, the two ropes; the app maps files this size |

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
