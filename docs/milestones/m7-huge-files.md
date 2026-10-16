# M7 — Huge files

**Status: done**

## Goal

Open a 4 GB log in under a second, scroll anywhere, search it, edit a line
and save, without the memory cost of loading it.

## Done when

Ticked items were verified here by tests, headless renders or timed
headless runs (release build, warm cache, M5 Pro). **Owner** items need a
person at the keyboard; each says what to try.

- [x] A 4 GB log from `bench/gen.sh` shows its first screen in under 1 s
      and stays interactive while indexing continues. First screen in
      0.02 s; counting goes on in a worker that the main thread only
      receives chunks from (`crates/app/src/indexer.rs`).
- [x] Resident memory stays under 60 MB with the 4 GB file open (twice the
      normal budget, for the index and caches). 34 MB at the first screen,
      35 MB with every line counted, 36 MB after searching all of it; an
      empty window is 33 MB at the same size.
- [x] `Primary+End` and scrolling to a fraction of the file work before
      indexing finishes: the end of the file is reachable immediately, and
      line numbers past the indexed region show as `~` until known. (The
      scrollbar's drawing is M8; M7 provides the position mapping,
      `View::scroll_to_fraction`.) View test on a partly indexed buffer;
      headless render of a find at the end of the 4 GB log shows `~`.
- [x] Go-to-line (Cmd+L on macOS, Ctrl+G elsewhere, added here) works for
      any line, waiting for indexing to reach it if needed. If counting
      stops because the file was cut short, it goes to the last line
      counted.
- [x] Search streams results across the whole file without blocking typing.
      Searching all 4 GB for its last line takes 1.4 s on a worker; the
      headless render shows "1 of 1" on the last line.
- [x] Editing anywhere works; undo and redo work; saving writes a correct
      file, verified by checksum against the same edit made with `sed`
      (saver tests, including random edits compared byte for byte).
- [x] Files that are not valid UTF-8 open in this backend at any size;
      invalid bytes display as `�` and are written back unchanged unless
      edited. The bundled font has no `�`, so each invalid byte draws as
      the missing-glyph box. Files under 256 MB are held in memory rather
      than mapped (see Results).
- [x] A single line of 100 MB (generated minified JSON) opens and scrolls
      horizontally without stalling, in both backends. Opens in 0.06 s
      (rope, 146 MB) and 0.02 s (mapped, 55 MB); find scrolls to a match
      100 MB into the line.
- [x] The 256 MB threshold between backends is measured and confirmed or
      moved, with the numbers in Results. Kept at 256 MB.
- [x] **Owner, confirmed:** `cargo build --release && ./target/release/nimbus --smoke`
      twice (cold, warm), for the standing first-frame record.
- [x] **Owner, confirmed:** open `log-4gb.log` (run `bench/gen.sh logs`; 5 GB of disk)
      in a window. The text shows at once and the status line counts lines.
      While it counts: type, press `Primary+End` (the end shows, numbers as
      `~`), and go to line 60,000,000 (Cmd+L / Ctrl+G); the jump happens
      once counting reaches it.
- [x] **Owner, confirmed:** in the same file, search for `request 65352533 took` and
      keep typing in the find field while it searches; nothing stalls.
- [x] **Owner, confirmed:** edit a line near the middle, save (the status shows
      "Saving… N%"), and compare with the same edit made by `sed` on a
      copy: `shasum` of both files matches.
- [x] **Owner, Windows, confirmed:** open the 1 GB log, edit, save, then save again
      while a search runs; both saves finish. Close the window during the
      line count and during a save; it closes without freezing.
- [x] **Owner, confirmed:** open `bench/out/longline-100mb.json` (`bench/gen.sh
      longline`) and scroll sideways with the trackpad, and with End and
      Home; nothing stalls.

## Scope

**In**: `MappedBuffer`, sparse line index, backend selection, long-line
handling, go-to-line, save for mapped files, `unsafe` for the mapping.

**Deferred**: following a growing log like `tail -f` (a post-M8 idea),
tree-sitter highlighting for mapped files (they get the simple
line-by-line highlighting every file over 16 MB gets; M7 checks it works
on `MappedBuffer` through `Text::chunk_at` and `Text::line`).

## Design

Approved by the owner on 2026-10-16, from a plan that changed the first
sketch in twelve places (listed under "Changes from the first sketch").

**Invalid bytes keep their size.** `Text::line` and `slice` stay `Cow<str>`,
but each invalid byte becomes the one-byte control character SUB (`\x1A`),
so a decoded line has exactly the raw byte length and every
`line_start + offset` stays a file offset: movement, column math and wrap
need no translation. The renderer draws SUB as `�`. A unit of text is a
valid UTF-8 scalar or one invalid byte; `is_char_boundary` follows that.
Text leaving the buffer (clipboard, search, save, undo) reads raw bytes
(`read_into`), never the substituted string. A genuine SUB byte also shows
as `�`: harmless, documented.

**Trait changes** (`crates/core/src/buffer.rs`), all backends:
`chunk_at` returns `(&[u8], usize)`; new `line_span(byte) -> LineSpan
{ start, end, next }` walks lines without line numbers; `line_number(byte)
-> Option<usize>` is `None` past the indexed region; `indexed_bytes`,
`byte_at`, `read_into(range, &mut Vec<u8>)` and `slice_into(range, &mut
String)`. `Buffer::put(range, &Content)` replays undo and redo (bounds
checked only, since a deletion can join invalid bytes into a valid
character), and `replace_sorted` applies many edits in one pass.

**Walking by byte, not line number.** The view's scroll anchor becomes one
absolute byte (`scroll_top`), rows are `RowPos { start, row }` with `start` a
line-start byte, and stepping uses `line_span`. Line numbers are only for
the gutter (`~` when unknown), go-to-line and highlight checkpoints. Unknown
regions need no special scrolling, and `Primary+End` never waits.

**Long lines** (both backends): nothing reads a line over 64 KB whole. Such
lines are read in screen-sized windows (`slice_into` into a reused
`String`), never soft-wrap, and scroll sideways with `scroll_col` plus a
fraction (an `f32` pixel offset loses precision on a 100 MB line). Their
columns are exact within 4 KB blocks and assume one column per byte at
each block start (`crates/core/src/long_line.rs`). The M3 1 MB line limit
goes.

**Sparse line index** (`crates/core/src/line_index.rs`): a checkpoint at
every 1024th line start and at least every 1 MB of bytes, so any lookup
scans at most 1 MB; about 1.7 MB of index for a 4 GB log. The first 4 MB is
indexed at open, so the first screen has line numbers; a worker reads the
rest in 4 MB blocks and publishes every 64 MB. In the region not yet
indexed, lines are found by scanning for newlines, with a pseudo-line
boundary every 1 MB so a file without newlines never needs a full scan.

**Piece table** (`crates/core/src/mapped.rs`): pieces over the original
bytes (a `Source`) or an append-only add buffer (sealed `Arc` segments plus
a small open one), with byte and line prefix sums; pieces carry newline
counts so edits ahead of the indexer stay correct. Snapshots share the
segments and the piece list (`Arc`, copy on write).

**Undo**: an `Edit` holds `Content`, either bytes or, for removals over 256
KB in a mapped document, a run of pieces, so deleting 1 GB copies nothing.

**Memory**: bulk work (indexing, search, save) reads with `pread` into
reused 4 MB buffers instead of through the mapping, so the file's pages are
not pulled into memory; only what is on screen is read through the mapping.
The budget is measured as macOS physical footprint, Linux `RssAnon` and
Windows private bytes, with RSS reported alongside.

**The one `unsafe`** is `crates/app/src/mapping.rs` (memmap2), behind the
core `Source` trait, so `core` stays free of I/O and `unsafe`. Its `SAFETY`
comment states the risk: another process can change the mapped file, and on
macOS and Linux a truncation makes reading a lost page raise `SIGBUS`. Bulk
readers use `pread`, which fails cleanly instead, and the watcher reacts to
size changes.

**Backend selection** (`crates/app/src/io.rs`): files at or above the rope
limit, and smaller files that are not valid UTF-8, open as `MappedBuffer`;
`Document.buffer` becomes an `AnyBuffer` enum. Mapped files always use the
simple lexical highlighting.

**Search** works on bytes (`regex::bytes`) with context before each chunk,
so it needs no line index and handles 100 MB lines. Matches are capped at
250,000 (counted past that); Replace All on a mapped file waits for the
search to finish.

**Save** for mapped files: a worker streams the pieces to a temp file and
indexes it as it writes; the document switches to the temp file's mapping;
the old mapping is dropped (on Windows, once no worker still holds it);
then the temp file is renamed over the original. Edits pause while saving.
Undo steps that refer to the old file are dropped. If the rename fails, the
document stays on the temp file, still dirty, and says so.

**Changes on disk**: mapped files never reload by themselves. A clean file
that got shorter is re-mapped (the safe response to truncation); otherwise
the tab says "changed on disk" with the usual Reload and Keep My Version.

**Changes from the first sketch** (approved): SUB substitution instead of
lossy decoding; walking by byte instead of `line_slice(idx, range)`; undo
`Content` instead of piece lists per group; `unsafe` in app, not core;
`pread` in 4 MB blocks and 1 MB checkpoints; no separate 1 MB UTF-8 sample;
the save order above; replace-all capped instead of chunk-rewritten;
scrollbar drawing left to M8; mapped files never auto-reload; Cmd+L/Ctrl+G
for go-to-line; long-line rows scroll sideways even with soft wrap on.

## Tasks

Each one commit, with its tests. "Agent" marks tasks done by a
general-purpose subagent at the effort CLAUDE.md sets for them.

1. Byte groundwork, no behaviour change: `chunk_at` as bytes, the new
   `Text` methods, `Content`, `Buffer::put`, undo through `put`; the syntax
   crate and app follow.
2. Search over bytes, with context, a match cap, and no line index needed.
3. Core walking by byte: `wrap.rs` (`RowPos { start, row }`) and
   `movement.rs` on `line_span`.
4. App walking by byte: `View::scroll_top`, gutter `~`, document line
   operations.
5. Long lines in both backends (`long_line.rs`, windowed rows,
   `scroll_col`); remove the 1 MB limit.
6. Line index, with a property test against a naive index (agent, high).
7. Piece table and `AnyBuffer`, sharing the buffer and undo oracles with
   `RopeBuffer` (agent, xhigh).
8. Invalid bytes and the partially indexed piece table (agent, xhigh).
9. The mapping (`unsafe`), backend selection, `--backend` (agent, xhigh).
10. Background indexer, progress, `~`, `--wait-index`.
11. Watcher policy for mapped files.
12. Save for mapped files, checksum-tested against `sed` (agent, xhigh).
13. Go-to-line.
14. Measure the rope limit at 64, 128, 256 and 512 MB; set it.
15. End-of-milestone review at xhigh; Results.

## Concepts

- **Memory mapping**: letting the OS page the file in on demand, and why
  that makes "opening" a 4 GB file nearly free.
- **Piece tables**: editing without ever modifying the original bytes.
- **Sparse indexes**: trading a little scan time for a lot of memory.
- **Why this is the one `unsafe` in core**, and what `SIGBUS` is.

## Risks

| Risk | Fallback |
|---|---|
| Indexing 4 GB takes longer than expected on slower disks | It is in the background by design; only "jump to line N" waits |
| `SIGBUS` from a truncated log | Documented limit; watcher warning; revisit if it bites in practice |
| Piece list grows large under a replace-all on a huge file | Replace-all on mapped files writes one new add-buffer region per 1 MB chunk instead of one piece per match |

## Results

| Metric | Value |
|---|---|
| Tests | 305 passing, plus 11 opt-in benchmarks or diagnostics |
| Release binary | 14.45 MB on macOS (M6: 15.02 MB) |
| 4 GB log, first screen | 0.02 s, 34 MB RSS (empty window: 33 MB) |
| 4 GB log, every line counted | 0.26 s in the background, 35 MB |
| 4 GB log, search to the last line | 1.4 s on a worker, 36 MB |
| 4 GB log, save | about 0.9 s on a worker (1 GB: 0.15–0.25 s, 0.3 ms on the main thread) |
| 100 MB single line | rope 0.06 s, 146 MB; mapped 0.02 s, 55 MB |
| Typing in a mapped buffer | 1.2 µs per key (64 MB, 4,000 pieces) |
| Replace All, 100,000 matches, with undo and redo | 0.21 s in a debug build |

All timed headless at 1400×700, release build, warm cache.

**Rope versus mapped threshold** (`bench/threshold.sh`, task 14):

| Size | Rope | Mapped |
|---|---|---|
| 64 MB | 0.06 s, 105 MB | 0.02 s, 33 MB |
| 128 MB | 0.11 s, 178 MB | 0.02 s, 33 MB |
| 256 MB | 0.20 s, 322 MB | 0.02 s, 33 MB |
| 512 MB | 0.38 s, 612 MB | 0.02 s, 33 MB |

The rope meets the budgets up to 512 MB, but above 256 MB it costs a
third to two thirds of a gigabyte where the mapped buffer costs nothing
extra. Below it, the rope keeps undo across saves and cannot be broken by
another program truncating the file. Kept at 256 MB.

**Design changes**

- **Small files that are not UTF-8 are held in memory, not mapped.** The
  plan mapped them at any size. The rope attempt has already read such a
  file, and a mapping exposes it to the crash described under Known
  limitations. They use the same piece table over in-memory bytes and save
  at once, like a rope.
- **Replace All is applied, undone and redone in one pass.** The plan
  capped Replace All on mapped files. Undoing 100,000 replacements one at
  a time took 79 s, because each put splices the piece list. `Buffer::
  put_sorted` puts a whole group back in one rebuild, and `report_sorted`
  tells the syntax tree about such a batch in an order where the text after
  the batch is right for every edit.
- `Text::chunk_at` returns bytes, and lines are walked by byte
  (`line_span`) rather than number, as approved; `line_slice` from the
  first sketch was not needed.
- The line counter reports a failed read, so a cut-short file stops the
  count with a message instead of leaving it at "Counting lines… N%".

**Concepts**

- *Memory mapping* (`crates/app/src/mapping.rs`): the file appears in
  memory without being read; pages load when touched. Bulk work (counting,
  search, save) reads with `pread` instead, so it neither keeps pages nor
  crashes on a cut-short file.
- *Piece tables* (`crates/core/src/mapped.rs`): the original file is never
  changed; the document is a list of pieces of it and of an append-only
  buffer of typed text.
- *Sparse indexes* (`crates/core/src/line_index.rs`): one checkpoint per
  1024 lines or 1 MiB, so 4 GB costs about 1 MB of index, at the price of
  scanning up to one spacing per lookup.
- *Why `unsafe`, and `SIGBUS`*: the `SAFETY` comment in
  `MappedFile::new` explains what another program can do to a mapped file.
- *Long lines* (`crates/core/src/long_line.rs`): columns are exact within
  4 KB blocks, so drawing and moving on a 100 MB line read a few KB.

**Surprises**

- One wide highlight range on a long line spanned everything to the next
  visible line: 450 MB on the 100 MB JSON. Highlighting now takes a list
  of visible ranges.
- Helper agents' worktrees were committed once by `git add -A`; they were
  removed in a follow-up commit and `.claude/worktrees/` is ignored.
- **Owner testing:** a Save As to a new name left the file readable only
  by its owner (`0600`): the temporary file is created owner-only so a
  replaced file's contents never leak mid-save, and a new file had no
  permissions to copy. A new file now gets the usual mode (`0666` less
  the umask). The saved bytes themselves matched the original plus the
  edit exactly.
- Counting 4 GB of lines first took 1.06 s and 5.4 GB with the temporary
  full index; with the sparse index and `pread` it takes 0.26 s and leaves
  RSS where it started.

**End-of-milestone review** (code-review skill, xhigh) found ten issues;
seven are fixed: small non-UTF-8 files were mapped; an extra empty regex
match was reported right after a match ending on a 1 MiB chunk boundary
(a proptest now covers patterns that can match nothing); a failed
read left the line count and a waiting go-to-line hanging; Replace All
could finish a search of gigabytes on the main thread; one match inside a
character made Replace All fail entirely; Save As to another file waited on
Windows for readers of the old one; the indexing setup was copied three
times. Two were left as they are: the first megabyte is read twice at open
(about 50 µs), and Replace All allocates one replacement string per match
(bounded by the match cap). The tenth, a dirty mapped file being
truncated, is below.

**Known limitations**

- **A mapped file with unsaved edits that another program truncates** can
  crash Nimbus on macOS and Linux the next time a lost page is drawn
  (`SIGBUS`), and its edits cannot be saved, since the bytes they sit
  between are gone. Clean documents re-map at once and the watcher warns
  about dirty ones, but there is a window of up to 100 ms. Windows refuses
  such a truncation. The fix is to map zero pages over the lost range when
  the watcher notices, which is new `unsafe` code; not done in M7.
- Undo history is cleared when a mapped document is saved.
- Columns on lines over 64 KB are approximate outside ASCII (exact within
  4 KB blocks).
- The scrollbar is not drawn yet (M8); `View::scroll_to_fraction` is the
  mapping it will use.
