# M7 — Huge files

**Status: planned**

## Goal

Open a 4 GB log in under a second, scroll anywhere, search it, edit a line
and save, without the memory cost of loading it.

## Done when

- [ ] A 4 GB log from `bench/gen.sh` shows its first screen in under 1 s
      and stays interactive while indexing continues.
- [ ] Resident memory stays under 60 MB with the 4 GB file open (twice the
      normal budget, for the index and caches).
- [ ] The scrollbar and `Primary+End` work before indexing finishes: the end
      of the file is reachable immediately, and line numbers past the indexed
      region show as `~` until known.
- [ ] Go-to-line (`Primary+G`, added here) works for any indexed line.
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
highlighting for mapped files (off by design).

## Design

**Piece table** (`core/mapped.rs`): the document is a list of pieces, each a
byte range in either the original file (the mmap) or the add buffer (a
`Vec<u8>` that only grows). `Vec<Piece>` with cached prefix sums of byte
length; a lookup is a binary search; an edit splits at most two pieces and
inserts one. Human editing produces at most thousands of pieces, so a `Vec`
is enough; a tree is a later optimisation if profiling asks for it. Undo
stores the piece list before and after each group; pieces are tiny, so this
is cheap and needs no byte copies.

**Sparse line index** (`core/line_index.rs`): a full index of a 4 GB file
with 100-byte lines is 40 million offsets, 320 MB. Instead, store the byte
offset of every 1024th line start: about 40k entries, 320 KB. Finding line
`n` jumps to checkpoint `n / 1024` and scans forward with `memchr` for at
most 1023 newlines, about 100 KB, which takes microseconds. A worker builds
checkpoints in 64 MB chunks from the start of the file and publishes
progress; `len_lines` returns the count so far until done. Edited pieces
carry their own newline counts so the index stays correct after edits.

**Unknown regions**: before indexing reaches a byte offset, the view can
still show it by scrolling by bytes. Jumping to the end seeks to a byte
offset, backs up to the previous newline, and renders from there with `~`
line numbers. The scrollbar maps to bytes until indexing finishes, then to
lines.

**Long lines** (both backends): the `Buffer` trait gains
`line_slice(idx, byte_range) -> Cow<str>`, and the view requests only the
visible columns plus a margin for any line over 64 KB. Column-to-byte mapping
for those lines is approximate (assumes one column per byte, corrected by
scanning the visible window). Soft wrap is disabled for lines over 64 KB.

**Invalid UTF-8**: `line` decodes lossily. Cursor movement treats each
invalid byte as one grapheme of width 1. Edits are byte ranges, so untouched
invalid bytes survive saving.

**Save**: stream every piece in order into a temp file in the same
directory, `sync_all`, then drop the mapping, rename over the original, and
map the new file. Windows refuses to replace a file that is mapped, which is
why the mapping is dropped first. After saving, the document is a single
original piece over the new file.

**Safety**: `memmap2::Mmap::map` is `unsafe` because another process can
change the file underneath us. This module is the one place in `core` that
lifts the workspace `unsafe_code` deny, with a `reason`. The `SAFETY`
comment states the risk plainly: on macOS and Linux, if another process
truncates the file, touching the lost pages raises `SIGBUS` and the process
dies. Mitigations: the M6 watcher warns as soon as the file's size changes
and offers to reload; Windows prevents truncation of mapped files. A signal
handler that recovers from `SIGBUS` is out of scope.

**Backend selection** (`app/io.rs`): at open, files over the threshold or
failing a UTF-8 check go to `MappedBuffer`. The UTF-8 check on a large file
reads only the first 1 MB; a later invalid byte in a `RopeBuffer`-sized file
is caught by the full read and reroutes to `MappedBuffer`.

## Tasks

1. `line_slice` in the trait and long-line rendering in both backends, with
   the 100 MB single-line test file.
2. Piece table with the `String`-oracle property tests shared with
   `RopeBuffer`.
3. Sparse line index with tests against a naive full index.
4. Background indexing with progress; byte-based scrolling before it ends.
5. Backend selection at open; invalid UTF-8 handling.
6. Search over mapped snapshots with `regex::bytes`.
7. Save with unmap, rename and remap; checksum test against `sed`.
8. Go-to-line action.
9. Measure ropey load time and memory at 64, 128, 256 and 512 MB; set the
   threshold.

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
