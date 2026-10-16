# M7 — Huge files

**Status: in progress on `m7-wip`**

## Goal

Open a 4 GB log in under a second, scroll anywhere, search it, edit a line
and save, without the memory cost of loading it.

## Done when

- [ ] A 4 GB log from `bench/gen.sh` shows its first screen in under 1 s
      and stays interactive while indexing continues.
- [ ] Resident memory stays under 60 MB with the 4 GB file open (twice the
      normal budget, for the index and caches).
- [ ] `Primary+End` and scrolling to a fraction of the file work before
      indexing finishes: the end of the file is reachable immediately, and
      line numbers past the indexed region show as `~` until known. (The
      scrollbar's drawing is M8; M7 provides the position mapping.)
- [ ] Go-to-line (Cmd+L on macOS, Ctrl+G elsewhere, added here) works for
      any line, waiting for indexing to reach it if needed.
- [ ] Search streams results across the whole file without blocking typing.
- [ ] Editing anywhere works; undo and redo work; saving writes a correct
      file, verified by checksum against the same edit made with `sed`.
- [ ] Files that are not valid UTF-8 open in this backend at any size;
      invalid bytes display as `�` and are written back unchanged unless
      edited.
- [ ] A single line of 100 MB (generated minified JSON) opens and scrolls
      horizontally without stalling, in both backends.
- [ ] The 256 MB threshold between backends is measured and confirmed or
      moved, with the numbers in Results.

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

_Fill in when done._
