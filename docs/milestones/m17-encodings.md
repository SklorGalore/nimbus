# M17 — Encodings and line endings

**Status: done** (2026-10-18). Written on 2026-10-18. Config and data files are
the third content type `DESIGN.md` §1 names, and logs, CSV exports and
Windows-born text files are often not UTF-8. Today such a file opens
through M7's byte path with every non-UTF-8 byte shown as `�` (SUB
substitution), which is right for a binary file and wrong for a Latin-1
or UTF-16 one. Approved the same day: the Western set only, with the
East Asian tables behind a cargo feature that is off (`DESIGN.md` §9).

## Goal

Open, edit and save text in the common non-UTF-8 encodings without
corrupting it, show the encoding and line ending in the status bar, and
let the user change either.

## Done when

- [x] **Detection**: a file with a UTF-16 BOM (either order) opens as
      UTF-16; a file with a UTF-8 BOM keeps M2's handling; a file that is
      not valid UTF-8 and has no BOM is sniffed over its first 64 KB and
      opens as Windows-1252 when every byte decodes (the usual case for
      Western text), else through the byte path as today. Detection never
      reads the whole file.
- [x] **Round trip**: a file opened in an encoding is saved back in it,
      byte for byte when unedited (the M2 rule, now per encoding). A
      character that cannot be encoded (an emoji into Latin-1) blocks the
      save with a message naming the line, and offers "Save as UTF-8".
- [x] **Status bar**: shows `UTF-8`, `UTF-8 BOM`, `UTF-16 LE`,
      `Windows-1252`, … and `LF` or `CRLF`; clicking either, or the palette
      actions "Reopen with Encoding…", "Save with Encoding…" and "Change
      Line Endings to LF/CRLF", changes it. Changing line endings is one
      undoable edit.
- [x] **Explicit choice**: `nimbus --encoding latin1 FILE` and a per-pattern
      `[encodings]` table in the config (`"*.csv" = "windows-1252"`) skip
      detection.
- [x] **Huge files** are not transcoded into memory: a mapped file over
      the 256 MB threshold in a non-UTF-8 encoding opens read-only through
      the byte path with a message saying so. Under the threshold the
      decoded text goes into the rope like any file.
- [x] Budgets: opening a 10 MB Windows-1252 file costs no more than twice
      the UTF-8 open; `--smoke` unchanged; the binary grows by at most
      1 MB for the decoder tables.

## Scope

**In**: UTF-8 with and without BOM, UTF-16 LE and BE, Windows-1252 and
ISO-8859-1, plus the encodings `encoding_rs` ships that cost no extra
tables; detection, round trip, status bar, actions, config.

**Deferred**: transcoding mapped files in place (a decode layer under
the piece table); per-line mixed encodings; guessing East Asian
encodings from content (offered only through the explicit choice, since
sniffing them is unreliable and the tables are large).

## Design

**Decode at the edge, keep the buffer UTF-8.** Nothing above `core`
changes: the buffer still holds UTF-8 and positions stay byte offsets
into it. `io.rs` decodes on open with `encoding_rs` (the decoder Firefox
uses; streaming, no allocation per chunk) and encodes on save in
`saver.rs`, both in 4 MB blocks on the worker that already reads and
writes the file. A `Document` records its `Encoding` beside its `Stamp`
and line ending.

**Sniffing is bounded.** 64 KB, the first block, through three checks in
order: BOM, valid UTF-8, then Windows-1252 if every byte is in its table.
Anything else is bytes, as M7 decided. The sniff result is logged at
debug level so a wrong guess can be diagnosed.

**Unedited round trip** holds because the decoder is deterministic and
`encoding_rs` encoders are exact inverses for the encodings in scope;
a test opens and saves each fixture and compares bytes. A file with
decode errors (a stray byte in a mostly Latin-1 file) is opened with
replacement characters and marked so that the first save asks.

**Line-ending conversion** is a `replace_sorted` over every line ending,
computed from the buffer's chunks without reading lines whole, so it works
on the mapped backend too; the per-file limit from M11 (64 MB of changed
text) applies.

## Tasks

1. `Encoding` type in `app`, `encoding_rs` added with a size measurement,
   BOM and sniff logic with fixtures for each encoding; decision line.
2. Decoding on open in `io.rs` blocks; the read-only path for large
   non-UTF-8 files.
3. Encoding on save in `saver.rs`; the unencodable-character check and
   its message; round-trip tests.
4. Status bar fields and the two click targets; "Reopen with Encoding…"
   and "Save with Encoding…" pickers.
5. Line-ending conversion as one edit; `Enter` follows the new ending.
6. `--encoding` and the `[encodings]` config table; `customizing.md`.
7. Measurements; Results.

## Concepts

- **Encodings versus the buffer**: why the editor holds one encoding
  internally and converts at the file boundary, and what a BOM is.
- **Sniffing**: why detection is a bounded guess, and why the guess is
  shown rather than hidden.
- **Exact round trips**: a decode and encode that return the same bytes,
  and the cases where they cannot.

## Risks

| Risk | Fallback |
|---|---|
| Windows-1252 sniff accepts a binary file whose first 64 KB happens to decode | Also require a share of printable characters over 95 %; else bytes |
| `encoding_rs` tables push the binary past budget | The Western and UTF-16 set is about 200 KB; East Asian tables are a cargo feature left off |
| Mixed encodings inside one file | Replacement characters and the "ask on save" mark; not repaired |

## Results

Landed 2026-10-18 (nimbus 0.6.0, nimbus-core 0.5.0).

- **Open time** (`encoding_open_bench`, release, Linux container, best of
  five): a 10 MB CSV opens in 18.8 ms as UTF-8 and 26.9 ms as
  Windows-1252, 1.43x (budget 2x). Opening and saving it back: 70 ms.
- **Binary size** (Linux release, stripped): 25.43 MB against 25.13 MB on
  main before M17, +0.30 MB for `encoding_rs` and the new code (budget
  1 MB). With `--features east-asian`: 25.43 MB, 1 KB
  more. `encoding_rs` picks its decoder from a value known only at run
  time, so its East Asian tables are linked whatever is offered; the
  feature only decides whether they are offered. All of it costs 0.30 MB,
  so the feature is kept off for the reason `DESIGN.md` §9 gives
  (unreliable to guess) rather than for size.
- **`--smoke`** (release, M5 Pro, Metal; owner's run on 2026-10-18):
  132–150 ms warm (M11: 147 ms, M10: 150 ms), window and GPU ready at
  77–90 ms. Cold, the first run after `sudo purge`: 516 ms, window and
  GPU ready at 222 ms; the rest of the gap is the binary, the font and
  the system libraries coming from disk. It is the first cold run
  recorded, so there is no earlier one to compare. Nothing new runs at
  startup: the `[encodings]` table is
  compiled once with the config, and only the files on the command line
  are sniffed (64 KB each, at most).
- **Surprises**:
  - Every byte decodes in Windows-1252 as `encoding_rs` reads it, so
    "every byte decodes" (the plan's test) proves nothing. The sniff is
    instead the plan's fallback: no NUL or control bytes, none of the five
    undefined bytes, at least 95 % printable. A file whose first 64 KB are
    UTF-8 with accents but which fails later stays on the byte path, since
    reading it as Windows-1252 would garble every accent.
  - `encoding_rs` follows the web standard: it writes UTF-16 as UTF-8 and
    treats ISO-8859-1 as Windows-1252. UTF-16 output and true Latin-1 are
    a few lines in `encoding.rs`.
  - Huge files: the plan made any huge non-UTF-8 file read-only. Built
    that way, a 4 GB log with one accented byte near its start would have
    lost editing that M7 gave it. Read-only is now for huge files whose
    encoding is *known* (UTF-16 byte order mark, `--encoding`, the config
    table, Reopen with Encoding); a guessed one keeps M7's editable byte
    path. Recorded in `DESIGN.md` §10.
  - Undoing "Change Line Endings" restores the text but not the ending
    Enter inserts; run the action again to switch back.
  - Replace in Files leaves open tabs in another encoding alone (the
    search reads files' bytes, whose offsets are not the tab's) and says
    so in its summary. Files on disk are still edited as bytes, as M10
    does, except that a non-ASCII replacement into a file that is not
    UTF-8 is refused for that file ("open it to replace there") rather
    than written as UTF-8 bytes into it.
