# M10 — Find in files

**Status: next.** Written on 2026-10-17 from the items deferred by M3 and
M8 ("find across files", "find in files"); approved the same day, with
the sidebar placement, the 10,000-match cap and the 64 MB undo budget
decided (`DESIGN.md` §9). First of M10–M15 in the approved order
(`backlog.md`).

## Goal

Search, and replace, across every file in the project folder, with results
streaming in while the search runs and the editor never waiting on it.

## Done when

- [ ] "Find in Files" (`Primary+Shift+F`) opens a search panel with the
      query, the regex, case and whole-word toggles, and include and
      exclude glob fields. Results are grouped by file, each with its line
      number and the line's text with the match marked. Enter or a click
      opens the file at that match, in the existing tab when it has one.
- [ ] Results appear while the search is still running; the first ones
      within 100 ms on this repository; the panel scrolls and the editor
      types at one frame throughout.
- [ ] "Next Result" and "Previous Result" (`F4`, `Shift+F4`) step through
      the results across files; the count ("12 matches in 4 files, still
      searching…") sits above the list.
- [ ] Open tabs with unsaved edits are searched in their buffer, not on
      disk, and marked in the results.
- [ ] `.gitignore` is respected, as the file opener and sidebar do; "Show
      Ignored Files" (M6) applies here too. Binary files and files over the
      size limit are skipped and counted ("3 files skipped").
- [ ] "Replace in Files": a replacement field, a preview of each change in
      the list (old and new text), a checkbox per file and per match, and
      "Replace All" that writes every ticked change. Open tabs get the
      change as one undoable edit; other files are written through the
      same atomic save path as `file.save`. A summary names the files
      changed and any that failed.
- [ ] "Undo Replace in Files" restores the files just written, as long as
      none has changed since (checked by size and modification time, as
      M6 does), and says which ones it could not restore.
- [ ] Memory: results are capped at 10,000 matches (the panel says "more
      than 10,000 matches, narrow the search"), and each result
      holds a file id, a line number, a byte range and at most 200 bytes
      of preview. Measured on a search for `e` over this repository and
      over a 1 GB tree from `bench/gen.sh`.
- [ ] Speed: a search of a 100,000-file tree finishes within three times
      `rg`'s wall time on the same machine, recorded in Results. (We do
      not try to beat ripgrep; we try not to embarrass ourselves.)

## Scope

**In**: the search worker pool, the results panel, opening results,
replace in files with preview and undo, include and exclude globs,
searching unsaved tabs.

**Deferred**: searching outside the project root (open a different
folder); saved searches; search history beyond the current session;
multi-line patterns (the single-line case covers nearly every use and
keeps memory bounded per line); search and replace with capture groups in
the replacement beyond `$1`-style references, which `regex` gives us for
free.

## Design

**Where the panel lives**: the sidebar column gets a second mode,
Search, beside Files; `Primary+Shift+F` switches to it and focuses the
query, `Primary+Shift+E` goes back to the tree. The sidebar already draws
rows of text at a persisted width (M6), and search results are rows of
text. A bottom panel would have cost the editor height it needs for the
file being read.

**Workers** (`crates/app/src/find_files.rs`). `ignore::WalkBuilder` has a
parallel walker (`build_parallel`) that visits files on several threads
and honours `.gitignore`; each visitor thread also searches the files it
visits, so there is no central queue to fill. Threads: available
parallelism minus one, so the main thread keeps a core. Each thread sends
batches to the event loop over the existing `crossbeam-channel`, at most
once per 16 ms or 64 results, whichever comes first, so a search with a
million hits does not wake the loop a million times. A newer search bumps
a `Generation` (M3's cancellation counter) and the old threads stop at
their next file.

**Reading files**. A file under 16 MB is read whole into a buffer the
thread reuses; the pattern runs over it with `regex::bytes`, and the
literal fast path (`search::Literal`, `memchr`) when the pattern has no
regex syntax. Files from 16 MB up to the limit (256 MB, the rope
threshold) are read in 4 MB blocks with the previous block's tail
kept, so a match across the block edge is still found; a match is never
longer than one line, which is why multi-line patterns are deferred. Files
over the limit are skipped with a count; the in-editor find (M3, M7) is
for those. A NUL byte in the first 8 KB marks a file binary. The matcher
over a block, with its line-tail carry, is a pure function in
`nimbus_core::search` with a property test (matches found block by block
equal matches found on the whole text); the file reading stays in `app`.

**Unsaved tabs**. Before the walk starts, the main thread snapshots every
open document with unsaved edits (`Buffer::snapshot`, cheap) and hands the
map of path to snapshot to the workers; a worker that reaches such a path
searches the snapshot instead of the file. A result from a snapshot is
marked with the tab's dirty dot.

**Results** (`SearchResults`): a `Vec<FileHit>` (path, interned once) and
a `Vec<Hit>` (file index, line number, byte range in the file, preview
range into one shared `String` of previews). The panel draws only the
rows on screen, as the sidebar does; rows are a prefix sum over the files
and a binary search, the M9 pattern. Previews are trimmed around the
match for long lines, since a minified file has one line of megabytes.

**Opening a result**. The hit's byte range is in the file as searched. If
the file is already open and has changed since (its version moved), the
match is looked up again by searching the line, and the cursor lands on
the first match of that line, or the line start when none remains.

**Replace in files**. The replacement runs over the ticked hits per file,
from the end of the file back, as M8's multi-cursor edits do, so earlier
offsets stay valid. For an open tab it is one `replace_sorted` call, one
undo step. For a file on disk a worker re-reads the file, checks that
every ticked range still holds the matched text (the file may have
changed since the search), builds the new content and writes it through
`saver` (temp file plus rename, M2), and records the file's new stamp.
The old content of each written file is kept in memory for "Undo Replace
in Files" until the next replace or exit, up to a budget of 64 MB (past
it, the summary says that undo is not available for the largest files,
and lists them). Undo checks each file's stamp before
writing the old content back.

**Globs**. Include and exclude fields take comma-separated patterns and
build a `globset::GlobSet`; `globset` is already in the binary as a
dependency of `ignore`, so it costs nothing.

**Actions**: `search.inFiles`, `search.replaceInFiles`, `search.next`,
`search.previous`, `search.toggleRegex`, `search.toggleCase`,
`search.toggleWord`, `search.replaceAll`, `search.undoReplace`,
`search.clear`. Keys chosen when building, checked by the keymap conflict
test.

**Themes**: `[ui]` keys `search_match` (the marked text in a preview) and
`search_file` (the file header row), in both built-in themes.

## Tasks

1. Block matcher with line-tail carry in `core`, with the property test
   against whole-text search.
2. Walker and worker pool: parallel walk, per-thread reuse buffers,
   batching, cancellation by generation. A headless test over a
   temporary folder tree.
3. Results model and the sidebar's Search mode: query and toggles, the
   grouped list, drawing only visible rows, the count line.
4. Opening a result; next and previous result across files; the
   unsaved-tab snapshots.
5. Include and exclude globs; ignored-file and binary skipping with
   counts; the size limit.
6. Replace field, previews, checkboxes; Replace All for open tabs.
7. Replace All for files on disk through `saver`, with stamp checks and
   the summary.
8. Undo Replace in Files with its memory budget.
9. Measurements: this repository, a 100,000-file tree, the 1 GB tree;
   memory of a cap-sized result set; compare with `rg`. Results.

## Concepts

- **Parallel directory walking**: why a work-stealing walk beats one
  thread listing and many searching, and what `.gitignore` handling costs.
- **Streaming and cancellation**: results arrive in batches and an old
  search is told to stop by a counter, the same way M3's find works.
- **Bounded memory for unbounded results**: a cap, previews trimmed
  around the match, and file paths stored once.
- **Edits from the end back**: the same reason M8 applies multi-cursor
  edits in reverse order.

## Risks

| Risk | Fallback |
|---|---|
| A pathological regex (nested quantifiers) runs for seconds per file | `regex` has no catastrophic backtracking; set a size limit on the compiled program and report "pattern too complex" |
| Thousands of small files make the channel chatty | Batching by time and count, measured in task 2 |
| Replace on a file that changed since the search | The pre-write check refuses that file and names it in the summary |
| The sidebar is too narrow for previews | Previews shorten to the match plus context; the width is draggable and remembered |
