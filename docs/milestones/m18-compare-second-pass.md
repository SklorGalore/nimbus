# M18 — Compare, second pass

**Status: proposed.** Written on 2026-10-18 from the three items M9
deferred: diff options (ignore whitespace and case), folder compare and
long-line splitting. Each builds on machinery that exists: the interner,
M10's walker and the compare view.

## Goal

Make the compare view answer the questions M9 left: what changed apart
from whitespace, what changed across two folders, and what changed in a
file that is one long line.

## Done when

- [ ] **Diff options**: "Ignore Whitespace" (leading and trailing, then
      all), "Ignore Case" and "Ignore Blank Lines" toggle from the palette
      and the compare view's header, and re-diff in the background; the
      merge views keep exact matching (a merge must not drop a whitespace
      change). The choice is remembered in `state.toml`.
- [ ] **Folder compare**: "Compare Folders…" walks two roots with M10's
      walker (`.gitignore` respected, same include and exclude globs),
      lists files only on one side or differing on both in the sidebar's
      Compare mode, streaming as found, and opens a file pair in the
      compare view on Enter. Same-size files are compared by content in
      4 MB blocks; a tree of 100,000 files lists in under two seconds.
      `nimbus --diff DIR_A DIR_B` opens it from the command line.
- [ ] **Long lines**: a line over 4 KB is cut at content-chosen points
      (a rolling hash over 64-byte windows, so cuts survive insertions)
      before interning, so minified JSON and a one-line log show real
      changes. The view shows a split line's pieces as rows of their own
      with a continuation mark in the gutter; the pieces are never
      written back split.
- [ ] Budgets: toggling an option on the 10 MB bench pair re-diffs within
      M9's time; a 50 MB one-line file diffs against an edited copy in
      under two seconds and the view scrolls at frame rate.

## Scope

**In**: the three options, folder compare with its sidebar mode and
command-line form, long-line splitting in the diff and the compare
view.

**Deferred**: comparing against git revisions from inside the editor
(git integration is a non-goal in `DESIGN.md` §2; `--diff` already lets
git drive Nimbus); three-way folder merge; splitting in the merge views
(a merge result must stay byte-exact, so pieces would need re-joining
on every edit).

## Design

**Options are interner flags.** The interner already hashes lines
streaming from chunks; ignoring whitespace or case normalises each line
inside the hash and the byte verification, so the Myers pass is
unchanged. An option change is a new background compare, like a change of
tab.

**Folder compare is a walk feeding the view.** Two `ignore` walkers run in
parallel, sorted paths are merged like a diff of two sorted lists, and
each pair goes to a worker that compares sizes, then content. Results
stream to a `CompareTree` in the sidebar, the third sidebar mode after
Files and Search. Selecting a row opens the pair in M9's compare view
with the folder listing kept, so the user can step through files with
"Next Change" at the end of one.

**Splitting is a tokenizer step.** Before interning, a line over 4 KB is
fed through a rolling hash and cut where the low bits of the hash are
zero (about every 1 KB on average), the technique rsync and content-
defined chunking use. The pieces are tokens like lines; the diff runs
over them; the view maps a piece back to its byte range in the line and
draws it as a row flagged `continued`. Cursor movement and editing are
unchanged because the buffer never sees the split.

## Tasks

1. Interner normalisation flags; tests that the option diffs equal the
   exact diff on normalised inputs.
2. Option actions, compare header toggles, `state.toml`; background
   re-diff.
3. Folder walk and pairing; the `CompareTree`; streaming and cancel.
4. Sidebar Compare mode; opening a pair; stepping through files;
   `--diff DIR DIR`.
5. Content-defined splitting in the tokenizer with a property test that
   an insertion moves at most two cut points.
6. Continuation rows in the compare view and the gutter mark; the
   prefix-sum alignment counts pieces.
7. Benchmarks (`compare_bench` with each option and with the one-line
   file); Results.

## Concepts

- **Normalising before hashing**: how "ignore whitespace" is a change
  to equality, not to the algorithm.
- **Merging sorted walks**: a folder diff is a diff of two sorted lists
  of paths.
- **Content-defined chunking**: why cutting at fixed offsets breaks on
  insertion and cutting at hash values does not.

## Risks

| Risk | Fallback |
|---|---|
| Normalised hashes collide more (many lines become equal) | The byte verification already guards tokens; Myers handles repeated tokens |
| Folder compare of a large tree reads every same-size file | Compare mtime and size first and show "probably same"; content on demand |
| Rows for pieces confuse line numbers | The gutter shows the line number on the first piece only and a `⤶` on the rest |

## Results

_To be filled in when the milestone lands._
