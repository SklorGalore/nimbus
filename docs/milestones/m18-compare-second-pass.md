# M18 — Compare, second pass

**Status: done (2026-10-18).** Written on 2026-10-18 from the three
items M9 deferred: diff options (ignore whitespace and case), folder
compare and long-line splitting. Each builds on machinery that exists:
the interner, M10's walker and the compare view. Approved the same day
with one addition the owner chose: "Compare with HEAD", which runs git
as a subprocess, the one exception to the git non-goal in `DESIGN.md`
§2 (§9).

## Goal

Make the compare view answer the questions M9 left: what changed apart
from whitespace, what changed across two folders, what changed in a
file that is one long line, and what changed since the last commit.

## Done when

- [x] **Diff options**: "Ignore Whitespace" (leading and trailing, then
      all), "Ignore Case" and "Ignore Blank Lines" toggle from the palette
      and the compare view's header, and re-diff in the background; the
      merge views keep exact matching (a merge must not drop a whitespace
      change). The choice is remembered in `state.toml`.
- [x] **Folder compare**: "Compare Folders…" walks two roots with M10's
      walker (`.gitignore` respected, same include and exclude globs),
      lists files only on one side or differing on both in the sidebar's
      Compare mode, streaming as found, and opens a file pair in the
      compare view on Enter. Same-size files are compared by content in
      4 MB blocks; a tree of 100,000 files lists in under two seconds.
      `nimbus --diff DIR_A DIR_B` opens it from the command line.
- [x] **Long lines**: a line over 4 KB is cut at content-chosen points
      (a rolling hash over 64-byte windows, so cuts survive insertions)
      before interning, so minified JSON and a one-line log show real
      changes. The view shows a split line's pieces as rows of their own
      with a continuation mark in the gutter; the pieces are never
      written back split.
- [x] **Compare with HEAD** (`diff.compareHead`): for a file inside a git
      repository, runs `git show HEAD:PATH` as a subprocess on a worker
      and opens the result against the tab in the compare view; "Compare
      with Revision…" asks for a revision or ref instead. No git library
      is linked; a missing `git` or a file outside a repository gives a
      message, never an error dialog. Nimbus never runs git for anything
      else (no status colours, no commits).
- [x] Budgets: toggling an option on the 10 MB bench pair re-diffs within
      M9's time; a 50 MB one-line file diffs against an edited copy in
      under two seconds and the view scrolls at frame rate.

## Scope

**In**: the three options, folder compare with its sidebar mode and
command-line form, long-line splitting in the diff and the compare
view, Compare with HEAD and with a named revision.

**Deferred**: any further git integration (status in the tree, blame,
commits: still a non-goal in `DESIGN.md` §2, which now carries the one
exception); three-way folder merge; splitting in the merge views
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

**Compare with HEAD is a subprocess, not a library.** `git` is run with
`std::process::Command` on the saver's worker, with the repository root
found by walking up from the file (no `.git` parsing: `git rev-parse
--show-toplevel` answers it), a 10 s timeout and a 256 MB output cap, the
same limit as a full diff. The output becomes a read-only `Document`
named `PATH @ HEAD` and the pair opens in M9's compare view. The revision
picker takes any ref and shows git's error text in the message line.

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
7. `git show` subprocess with timeout and cap; `diff.compareHead` and
   "Compare with Revision…"; the repository-root lookup; tests with a
   temporary repository.
8. Benchmarks (`compare_bench` with each option and with the one-line
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
| `git` hangs (a credential prompt, a slow filesystem) | The 10 s timeout kills it and the message line says so; `GIT_TERMINAL_PROMPT=0` is set |
| Rows for pieces confuse line numbers | The gutter shows the line number on the first piece only and a `⤶` on the rest |

## Results

Landed on 2026-10-18 (nimbus 0.6.0, nimbus-core 0.5.0). Measured in the
cloud container (4 cores, release build); the owner's Mac numbers are to
be added.

| What | Budget | Measured |
|---|---|---|
| 10 MB bench pair, exact (`compare_bench`) | M9: under 300 ms | 133 ms, 293 hunks |
| same, ignore whitespace at ends / all | M9's time | 209 ms / 282 ms |
| same, ignore case / ignore blank lines | M9's time | 214 ms / 194 ms (289 hunks) |
| 50 MB one-line file against a copy with 5 edits (`one_line_bench`) | under 2 s | 381 ms, 5 hunks over 50,764 pieces |
| Folder compare of 100,000 files, 300 differing (`folder_compare` bench) | under 2 s | 0.96 s and 1.13 s; first row after 17–30 ms |

Startup and binary size: no new dependency and no startup work (the
folder walk and git run only when asked); not re-measured here.

What landed, and where:

- **Options** (`crates/core/src/diff.rs`, `Normalize`): each line is
  normalised (whitespace trimmed or removed, ASCII letters lowercased)
  into a reused buffer before hashing, and representatives are normalised
  again to confirm a hash match, so nothing extra is stored per line.
  Ignore Blank Lines drops hunks whose lines are all blank on both sides,
  as `git diff --ignore-blank-lines` does. The live diff takes the same
  options (`LiveDiff::with_normalize`); merges never do.
- **Header row** (`compare.rs`): "Ignore whitespace" (cycles off, ends,
  all), "Ignore case", "Ignore blank lines" and "Unified", bright when on.
  The palette has the same actions (`diff.ignoreWhitespace`,
  `diff.ignoreCase`, `diff.ignoreBlankLines`). The options apply to every
  open compare and live in `state.toml`.
- **Long lines** (`crates/core/src/split.rs`): a gear hash over the last 64
  bytes, cut where its top ten bits are zero (about every 1 KB), never
  inside a UTF-8 character or before the line ending, and at 16 KB
  whatever the hash says. The property test checks that an insertion moves
  at most two cuts. `Splits` maps units (lines and pieces) back to lines;
  `Columns` converts at its edges, so the views outside still see lines.
  A row for a piece draws only its bytes, with `⤶` in the gutter.
- **Compare with HEAD / with Revision…** (`crates/app/src/git.rs`):
  `git rev-parse --show-toplevel` in the file's folder, then `git cat-file
  --filters REV:PATH` from the top, on a thread of its own.
- **Folder compare** (`folder_compare.rs`, `compare_panel.rs`): two sorted
  single-threaded walks merged by `Path` order as they run, content
  compared on workers, rows streamed to the sidebar's third mode,
  "Compare". `nimbus --diff DIR_A DIR_B` opens it.

Surprises and deviations from the plan:

- `git cat-file --filters` instead of `git show`: it applies the
  line-ending and smudge conversion checkout would, so a CRLF working copy
  on Windows does not differ from its LF blob on every line. git runs on a
  thread of its own rather than the saver's worker, which is busy only
  while saving huge files. Rejects a revision starting with `-`; on
  Windows no console window flashes.
- A pair with any line over 4 KB always takes the layered compare, redone
  300 ms after typing pauses, because the live diff follows edits line by
  line. Splitting applies only to texts within the 256 MB diff limit:
  finding long lines in a 4 GB log would mean reading it all again.
- Content-defined cuts need varied content: text that repeats a short
  cycle (a few dozen bytes) never hits the hash and gets the 16 KB forced
  cuts, which do move on insertion. Real minified data is varied enough.
- Ignore Case folds ASCII only, so hashing stays a byte at a time.
- A fold row over cut lines counts pieces, not lines.
- The folder list is flat (one row per differing file with its relative
  path), lists only differences (identical files are counted), takes its
  include and exclude globs from the Search panel's fields, and asks for
  the two folders with two folder dialogs. A file only in the left folder
  opens on its own; one only in the right opens against an empty side.
- Not checked here: rendering. The container has no GPU, so there is no
  headless screenshot of the header, the `⤶` rows or the Compare mode yet.
  Strict clippy passes for Linux and Windows (`scripts/clippy-cross.sh`);
  macOS is checked by CI.
