# Nimbus — Design

A fast, lightweight, modeless text editor with its own GPU rendering pipeline.
Written in Rust. Personal tool first; clean enough to share later.

Decisions come from the kickoff interview on 2026-10-15 and the approval of
the first draft the same day. Each milestone has a detailed plan in
[`docs/milestones/`](docs/milestones/). This document holds the decisions that
span milestones; a milestone doc holds the work breakdown for one milestone.
When a milestone changes a decision here, update this file in the same commit
and add a line to the decision log (§10).

## 1. Goals

| Goal | Target | How we measure | Baseline (M0) |
|---|---|---|---|
| Fast startup | First frame in < 500 ms, total time from the top of `main` | `nimbus --smoke` prints `first-frame-ms` | M1: 116–132 ms warm, of which about 3 ms is ours |
| Typing latency | Keystroke to pixels in < 1 frame (≤ 8 ms at 120 Hz) | Frame timer in debug overlay | — |
| Huge files | Open a 4 GB log, scroll and search it, under 1 s to interactive | Bench file from `bench/gen.sh` | — |
| Low footprint | Opening a file adds at most 1.5× its size over an empty window (rope backend); mapped files (M7) add roughly nothing. Syntax highlighting is not counted (§4.2). Total reported, not budgeted. Single binary < 35 MB | `vmmap` physical footprint after settling; CI size job | M2: 45 MB empty, 58 MB with a 10 MB file (1.3×); 7.5 MB binary |
| Modeless editing | Type to insert; Cmd/Ctrl shortcuts; mouse works | — | — |

Content we care about, in order: source code, prose and notes (Markdown),
config and data files (JSON, YAML, TOML, CSV, logs).

**Startup budget: under 500 ms**, set by the owner on 2026-10-15 after M1
showed that the original 100 ms was below the platform floor. M1 measured
each phase from the top of `main` (M1 doc, Results). About 120 ms goes to
winit and AppKit: creating the application, creating the window, and
showing it before the first redraw request. Our own work is about 3 ms, with
GPU setup overlapped on a background thread. The budget leaves room for
opening a file, config and highlighting setup, but later milestones should
still keep work lazy or off the main thread. Record warm, and first-run
after a rebuild, in each milestone's Results.

## 2. Non-goals (for now)

- Vim keybindings or any modal layer.
- Plugins or a scripting language.
- LSP, debugging, terminals, git integration. One exception, chosen on
  2026-10-18: M18's "Compare with HEAD" runs `git show` as a subprocess
  to open a file's committed version in the compare view. Nothing else
  runs git.
- Collaborative editing.
- Rich text, or proportional fonts in the editor area. UI chrome gets a
  proportional font setting in M20.
- Bidirectional and right-to-left text. Decided at approval; the renderer
  assumes left-to-right runs.
- Multiple windows. One window with tabs.
- Session restore, until M12 (drafted): reopening the last files is a
  nice-to-have after M9.
- Native menu bar on Linux (§4.4).

## 3. Platform

Targets macOS, Linux, Windows from day one, all through `winit` + `wgpu`.
No platform-specific windowing code. Primary development and testing happens
on macOS; CI builds, lints and tests all three on every push (verified since
M0). Linux CI needs `libwayland-dev` and `libxkbcommon-dev`.

The modifier key is "Cmd on macOS, Ctrl elsewhere". We call it `Primary` in
keybindings and resolve it per platform.

Rust edition 2024, stable toolchain (1.99 at M0). No nightly features.

## 4. Architecture

Cargo workspace, four crates. The split keeps the editor logic testable with
no GPU, and keeps compile times down when iterating on one layer.

```
nimbus/
  Cargo.toml            workspace, lint table, release profile
  clippy.toml           test-only lint relaxations
  crates/
    core/    nimbus-core     text buffers, edits, undo, cursors, search   (no I/O, no GPU)
    syntax/  nimbus-syntax   tree-sitter integration, highlight spans
    render/  nimbus-render   wgpu pipeline, glyph atlas, text layout, rect/quad batching
    app/     nimbus          winit loop, editor state, tabs, palette, file tree, keymap, config
  bench/                large test files, generated on demand
  docs/milestones/      one plan per milestone
  DESIGN.md
  CLAUDE.md             agent instructions: commands, invariants, code quality rules
```

Dependency direction is strictly downward: `app → render, syntax, core`;
`syntax → core`; `render → core` (for text slices only). `core` depends on
nothing in the workspace.

### 4.1 core: the text buffer

**Decided: two backends behind one trait.** The interview chose both ropey
and lazy memory-mapping. Ropey holds the whole text in memory, so it cannot be
the only backend. A narrow `Buffer` trait has two implementations:

1. **`RopeBuffer`** (M2). Wraps `ropey::Rope` from ropey 1.6, the stable line
   used by Helix. Used for files under a size threshold (initially 256 MB)
   that are valid UTF-8. Ropey 1.x indexes by `char`; the wrapper converts at
   the boundary so nothing above it sees char indices. Ropey 2.0, currently in
   beta, indexes by byte natively; revisit when it is stable.
2. **`MappedBuffer`** (M7). A piece table whose original piece is an `mmap`
   of the file and whose add piece is an append-only `Vec<u8>`. Lines are
   indexed in the background with a sparse checkpoint index, so a 4 GB file
   costs kilobytes of index, not hundreds of megabytes (M7 doc). Edits are
   allowed; saving streams to a temp file and renames it over the original.
   Also used for files that are not valid UTF-8, at any size: under the
   threshold the same piece table sits over the file's bytes in memory
   instead of a mapping, since the rope attempt has already read them and
   a mapping could crash if another program truncates the file.

The interface is intentionally narrow so both backends stay honest. Built
in M2 and extended in M7 (`crates/core/src/buffer.rs`), reading and editing
are split:

```rust
pub trait Text {                                   // read-only; also what snapshots offer
    fn len_bytes(&self) -> usize;
    fn len_lines(&self) -> usize;                  // a lower bound while indexing
    fn line(&self, idx: usize) -> Cow<'_, str>;    // without "\n" or "\r\n"
    fn byte_to_line(&self, b: usize) -> usize;
    fn line_to_byte(&self, l: usize) -> usize;
    fn slice(&self, range: Range<usize>) -> Cow<'_, str>; // clamped, widened to char boundaries
    fn is_char_boundary(&self, b: usize) -> bool;
    fn chunk_at(&self, b: usize) -> (&[u8], usize);  // contiguous raw bytes holding b, and their start
    // M7: walking by byte, so nothing needs a line number
    fn byte_at(&self, b: usize) -> Option<u8>;
    fn line_span(&self, b: usize) -> LineSpan;       // { start, end, next } of the line holding b
    fn line_number(&self, b: usize) -> Option<usize>; // None past the indexed part
    fn indexed_bytes(&self) -> usize;
    fn read_into(&self, range: Range<usize>, out: &mut Vec<u8>) -> io::Result<()>; // raw bytes
    fn slice_into(&self, range: Range<usize>, out: &mut String); // into a reused String
}
pub trait Snapshot: Text + Send + Sync {}          // owned by worker threads
pub trait Buffer: Text {
    fn replace(&mut self, range: Range<usize>, text: &str) -> Result<Edit, EditError>;
    fn put(&mut self, range: Range<usize>, content: &Content) -> Result<Edit, EditError>; // undo
    fn snapshot(&self) -> Box<dyn Snapshot>;       // cheap: ropes share structure
    fn is_fully_indexed(&self) -> bool { true }
    // many edits in one pass: Replace All, and undoing or redoing it
    fn replace_sorted(&mut self, items: &[(Range<usize>, &str)]) -> Result<Vec<Edit>, EditError>;
    fn put_sorted(&mut self, items: &[(Range<usize>, &Content)]) -> Result<Vec<Edit>, EditError>;
}
```

`replace` returns an error for a range outside the text or inside a
character, rather than panicking, because a panic aborts with unsaved work.
Lines end only at `\n`; a `\r` before it belongs to the line ending and a
lone `\r` is ordinary text. Both backends must agree on this.

`snapshot` was added after the first draft: background search (M3) and
initial parsing (M5) need to read the text off the main thread while the user
keeps typing. A ropey clone is O(1) because ropes share structure; a mapped
snapshot clones the piece list and shares the mmap.

Everything above the buffer (cursors, undo, search, rendering) works in byte
offsets and asks the buffer for line conversions. Nothing else may assume the
whole text is resident, or that a single line is small: minified JSON or a
log without newlines can be one line of hundreds of megabytes. Since M7 a
line over 64 KB is never read whole: the view reads the visible window of
it with `slice_into`, and movement reads a few KB around the cursor
(`crates/core/src/long_line.rs`).

Other `core` responsibilities:

- **Positions** are byte offsets into UTF-8. Char and grapheme boundaries are
  resolved with `unicode-segmentation` when the cursor moves. Display column
  is computed at render time, not stored. Vertical movement keeps a "sticky"
  preferred column so moving through a short line does not lose your place.
- **Cursors and selections**: a `Selection` is `{anchor, head}`. Since M8
  a document holds several (`Selections`: sorted, merged when they
  overlap, with one primary that the view and the input method follow).
  An edit at every cursor is worked out per cursor and applied in one
  `replace_sorted` (`crates/core/src/multi.rs`), so no offset moves before
  it is used; undo puts back each keystroke's batch in one pass.
- **Undo**: a linear history of `Edit` records (range, removed text, inserted
  text, selections before and after). Typing coalesces into one record until
  the cursor jumps, the edit kind changes, or 500 ms pass. Branching undo
  trees are a non-goal.
- **Search**: literal and regex (`regex` crate) over `BufferSnapshot` in
  chunks, so it works on mapped files and runs off the main thread. Results
  are byte ranges.
- **Line endings**: detected on open (the majority of LF or CRLF wins) and
  used for new lines typed with Enter. Saving writes the buffer's bytes
  exactly, so unedited files round-trip byte for byte, mixed endings
  included. Tabs are kept as tabs; display width
  is a setting.

### 4.2 syntax: highlighting

`tree-sitter` 0.27 with grammars compiled into the binary as crates. Starter
set, confirmed at approval: Rust, Markdown, JSON, TOML, YAML, Python,
JavaScript, TypeScript, Bash. Each grammar sits behind a cargo feature so a
grammar that costs too much binary size can be dropped. M5 measured each
(macOS release binary): TypeScript 1.42 MB, Bash 1.37 MB, Python 0.46 MB,
Markdown 0.38 MB, YAML 0.18 MB; the tree-sitter runtime plus Rust about
1.8 MB. M14 added C, Go, HTML, CSS and TSX (Linux release binary, stripped:
TSX 1.39 MB, C 0.60 MB, Go 0.22 MB, CSS 0.12 MB, HTML 0.03 MB; all five
together 2.3 MB, taking Linux from 19.1 MB to 21.4 MB of the 35 MB
budget). TSX shares the TypeScript crate, so it is a second compiled
grammar for one set of queries. If the binary budget is ever tight, TSX
goes first, then Bash, then TypeScript.

Highlighting runs our own query loop rather than `tree-sitter-highlight`:
that crate wants the whole document as one `&[u8]`, and our text lives in a
rope or a piece table. A `QueryCursor` restricted to the visible byte range,
reading text through a chunk callback, avoids copying the document.

Rules that protect the performance goals:

- Grammars are initialized lazily on first use of that language, on the
  worker that runs the first parse. Startup never pays for a parser.
- The first parse of a file runs on a worker thread against a snapshot; the
  edits made meanwhile are replayed into the tree when it returns.
- After that, parsing is incremental on the main thread: every `Edit` is fed
  to the tree (including undo and redo, which report their edits through a
  callback), and each frame reparses once with the old tree. A reparse gets
  2 ms; if it runs over (tree-sitter's progress callback stops it), it
  restarts on a worker and the edited old tree keeps colouring the view.
- Tree-sitter reads the text through `Text::chunk_at`, one rope chunk at a
  time, so neither parsing nor queries copy the document.
- Highlighting is computed only for the lines that can be on screen, and
  cached until the tree or the range changes.
- Above 16 MB the file is not parsed. It gets simple line-by-line
  highlighting instead (below), for any backend, including `MappedBuffer`.
- `nimbus-syntax` starts no threads: `SyntaxState` hands out `Job`s and the
  app runs them, so tests and headless rendering parse in place.

Memory: a tree keeps about 28 bytes per byte of Rust source (2.7 million
nodes of about 108 bytes for 10 MB, one node per 3.9 bytes), so a file at
the 16 MB limit can hold about 450 MB. Highlighting memory is not counted
in the per-file budget in §1, which is for text storage (owner decision).

**Simple highlighting for large files** (`crates/syntax/src/lexical.rs`).
Each language has a small table of rules: line and block comments, string
delimiters, keywords, and literals like `true` and `null`; Markdown has
headings and fenced code blocks. Lines are coloured one at a time, carrying
only whether the previous line ended inside a block comment, a multi-line
string or a code fence. That state is saved every 256 lines as lines are
scanned, and an edit drops the saved states after it. To reach a line far
from any saved state (a jump into the middle of a huge file), scanning
starts at most 256 KB above it and assumes plain code there, so colours can
be wrong only where a single comment or string is longer than that. The
result of that guess is kept while the view stays near it. Memory stays
near zero at any size. On a 100 MB file: a jump to the middle 3 ms, a
keystroke with 60 lines on screen 21 µs. Types, functions and the like stay
plain; only tree-sitter tells those apart.

**User additions** (`docs/customizing.md`). Without rebuilding, a user can:
map file names and `*.ext` patterns to languages (`[languages]` in the
config); define languages of their own as simple rules
(`languages/NAME.toml`, the same rules as above, used at every size); and
replace or extend a built-in language's highlight query
(`queries/NAME.scm`, with `;; extends` to append). Detection order:
config patterns, then user languages, then the built-in detection. User
rules are checked (an empty delimiter would match everywhere) and stored
for the program's lifetime, once per distinct rule set. New tree-sitter
grammars still need a rebuild: loading compiled grammars at runtime means
`unsafe` code and per-platform libraries, and WebAssembly grammars a
runtime of 10 MB or more.

**Injections** (`crates/syntax/src/inject.rs`, M14). A grammar's
`injections.scm` names regions of a tree that belong to another language:
Markdown's block grammar marks each paragraph for the inline grammar
(`tree_sitter_md::INLINE_LANGUAGE`, in the binary already) and each fenced
block for the language its fence names; HTML marks `<script>` and
`<style>`; JavaScript and TypeScript mark tagged template strings
(`html\`…\``, `css\`…\``) with the tag as the language name. Rust's query is deliberately not used: it would parse every `format!` and
`vec!` argument as a second Rust tree for no visible gain. The host tree
is queried for regions over the visible range plus one screen each side,
and each region gets a child tree parsed with `set_included_ranges` over
those bytes only, through the same `Text::chunk_at` callback as the host.
Children are kept in a map keyed by the host node's `id()` (stable across
incremental reparses), nest one level further (a code span inside a
paragraph), take `Tree::edit` for every edit the host sees, and are
dropped when scrolled out of the kept range. Parsing new or edited
children shares the frame's 2 ms budget: at least one child is parsed per
frame, the rest wait and the host's colours show meanwhile, and the app
keeps requesting frames while `injections_pending()`. An inline Markdown
parse costs about 0.3 ms per paragraph, so a screen settles over a few
frames and a frame with no edit and no scroll returns the cached spans in
under a microsecond. Each loaded grammar is a **capture table**: a span
carries the table and the capture index, the theme resolves every table
it has seen, and the inner language's capture wins over the host's where
they overlap, so a Rust keyword in a fenced block takes the keyword
colour.

**Wide trees** (`crates/syntax/src/split.rs`). A Markdown document is one
node with thousands of blocks under it, and tree-sitter's query cursor
walks the children of every node it enters, so querying the root for one
screen cost the whole file. Queries now descend to the nodes covering the
range, and a node with more than 64 children is queried child by child
over the range with the node itself queried alone, so a frame's query cost
follows what is on screen.

**Beyond colour** (M14). The tree also gives: Go to Symbol
(`symbols.rs`, a `tags.scm`-style query over the whole tree listing
functions, types, constants and Markdown headings, run on first use);
structural selection (`structure::expand`, the smallest named node
larger than the selection, which Expand Selection takes over its
bracket-counting step whenever the tree is up to date, so `{ body }`
grows to the body, the block, the function); fold regions (`structure::fold_regions`, a `folds.scm` per
language over the visible range, for M13's fold model); and bracket
matching that skips strings and comments (`SyntaxState::matching_bracket`).
Each query kind can be replaced or extended by a user file
(`docs/customizing.md`).

The `app` crate maps capture names to colours via the theme, with
longest-prefix matching (`keyword.control` falls back to `keyword`),
resolved once per language into an array indexed by capture.

Queries come from the grammar crates, except Rust's, which is vendored in
`crates/syntax/queries/rust.scm`: upstream's constant pattern has a stray
quote in its regex and sits before `@constructor`, so `FOO` never coloured
as a constant. When captures overlap, the innermost node wins, and for one
node the later pattern wins, the convention the queries are written for.

### 4.3 render: GPU text

Our own pipeline on `wgpu` 30. Four pieces:

1. **Fonts**: one monospace font is bundled in the binary with
   `include_bytes!`, so the first frame never waits for a system font scan.
   That scan (`fontdb`) is slow, tens to hundreds of milliseconds, so it runs
   on a background thread only when the config names a system font or a
   glyph is missing and needs a fallback (M8). The bundled font is Hack Nerd
   Font Mono Regular (MIT and Bitstream Vera licences, about 2.7 MB), chosen
   by the owner. Its Nerd Font icons are scaled to one cell, so file-type and
   status icons can appear in the tree, tabs and status bar without breaking
   the grid.
2. **Shaping**: `swash`, per line. Results are cached keyed on
   `(line content hash, font id, size)`. The editor area is monospace, which
   makes column math trivial.
3. **Glyph atlas**: one `R8Unorm` texture (2048², a second page is added when
   full). Our own shelf packer, about a hundred lines, written by hand
   because learning how atlases work was a goal. `etagere` is the drop-in
   fallback if the packer becomes a time sink. Glyphs are rasterized by
   `swash::scale` with grayscale antialiasing at the exact pixel size and
   scale factor. Horizontal subpixel position is quantized to quarter pixels,
   so each glyph has up to four atlas entries. The atlas is cleared and
   refilled on scale-factor or font-size change.
4. **Draw**: two instanced pipelines, one for solid rectangles (backgrounds,
   cursor, selection, gutter, UI chrome) and one for textured glyph quads.
   Each frame fills two reusable `Vec<Instance>`s, uploads them, and issues
   two draw calls. Instance structs are `#[repr(C)]` and cast to bytes with
   `bytemuck`.

**Colour and blending.** The surface uses a non-sRGB format, so blending
happens in gamma space, which is how most platforms' native text looks.
Coverage gets a small adjustable contrast curve, because light text on dark
backgrounds otherwise looks thin. Linear blending is a possible later
experiment, not a v1 goal.

**Startup path (M1).** M0 creates the GPU instance, adapter and device on the
main thread after the window exists. M1 moves that work to a thread spawned
first thing in `main`, together with font parsing and shader compilation, so
it overlaps the OS creating the window. The adapter is requested without a
surface and checked for compatibility once the surface exists. The window is
created on the main thread, which macOS requires.

**Frames are drawn on demand**, not in a loop. The app requests a redraw on
input, scroll, resize, cursor blink, and when a background task (indexing,
parsing, search) posts an update. Cursor blink uses `ControlFlow::WaitUntil`
for the next blink, and blinking stops after 10 s without input, so an idle
editor wakes nothing.

Layout is simple because everything is a monospace grid. A `View` has a
scroll position, a visible line range, and produces instances from
`Buffer::line` plus highlight spans. Soft wrap maps a logical line to N
visual rows, computed only for visible lines and cached. Scrolling is
anchored: the position is "logical line L, visual row R within it, plus a
pixel offset", so wrapped documents never need a prefix sum over every line.

### 4.4 app: the editor

- **Event loop**: `winit` 0.30, pinned below the 0.31 beta. All editor state
  lives in one `Editor` struct on the main thread. Background threads send
  messages over a `crossbeam-channel`; the loop is woken with an
  `EventLoopProxy` user event, drains the channel, and requests a redraw.
- **Actions**: every user-visible operation is an `Action` with a stable
  name, a description and default keybindings. The keymap maps chords to
  actions. The command palette lists them with fuzzy search. Nothing is
  reachable only through a hard-coded key.
- **Tabs**: a `Vec<Document>` plus the active index. `Primary+1..9` switch,
  `Primary+W` closes, `Ctrl+Tab` cycles. Dirty state shows in the tab.
- **Fuzzy file opener** (`Primary+P`): the project root is the folder passed
  on the command line, else the parent of the first opened file, else the
  current directory. Walked with `ignore`, which respects `.gitignore`, on a
  background thread, streaming into `nucleo`, which matches incrementally.
- **File tree sidebar** (`Primary+B` toggles, `Primary+Shift+E` focuses):
  an arena tree (`tree.rs`) whose folders are listed on a worker when first
  expanded, with `ignore` so `.gitignore` applies ("Show/Hide Ignored
  Files" toggles that). Rendered with the same rect and glyph pipelines; it
  is rows of text. The editor column starts at `Layout::left`.
- **Watching** (`watch.rs`): `notify`, watching only the folders listed in
  the sidebar and the folders of open files, each non-recursively. A
  recursive watch would register every folder of a large repository up front
  on Linux (startup time, and the system's watch limit). Events are
  debounced for 100 ms. Watching starts after the first frame. The OS
  watcher lives on its own thread, which the main thread only sends the
  set of folders to watch: changing watches can block inside `notify` on
  macOS and Windows, and must never block the window.
- **External changes**: an open file that changed on disk reloads if it has
  no unsaved edits, keeping the cursor's line and column; the reload clears
  undo. With unsaved edits, the tab says "changed on disk" and the message
  line offers "Reload from Disk" and "Keep My Version" (palette actions). A
  deleted file's tab says "deleted"; saving recreates it. Our own saves are
  told apart by the file's size and modification time, recorded at each
  open and save.
- **Compare and merge** (M9): side-by-side or unified comparison against
  the saved file, another file or another tab; two-way merge into an
  editable result; three-way merge from a base, with conflicts; conflict
  markers in and out; `--diff` and `--merge` so git can use Nimbus as its
  difftool and mergetool. The diff lives in `core` (pure, no I/O), works on
  interned line tokens, runs on workers, and is re-done locally around
  each edit, so diff views keep the editor's typing budget at any file
  size. Columns are ordinary `View`s on rows aligned through a prefix sum
  over the hunks. The algorithm is Myers (`imara-diff`, with its cut-offs
  for large inputs and an indentation heuristic for placing hunks), chosen
  over histogram by measurement: histogram cannot anchor on lines that
  repeat more than a few dozen times, which most lines of code and logs
  do, and took 19 s where Myers took 0.14 s on a 60 MB pair (M9 doc,
  Results). A full diff covers at most 256 MB per side (`DIFF_LIMIT`,
  the rope threshold); past that, windows. A merge's result is followed
  through edits by tracked regions (line ranges shifted per edit, no
  re-diff), so taking a side is one undoable `replace`; merges, like
  compares, are computed on a worker while the tab shows its texts.
- **Window state**: the sidebar's visibility and width are remembered in
  `state.toml` beside the config. Nimbus never writes the user's
  `config.toml`.
- **Find / replace** (`Primary+F`, `Primary+H` on Windows and Linux,
  `Primary+Alt+F` on macOS where `Cmd+H` hides the app): an inline bar at the
  top of the view, incremental, with match count and regex and case toggles.
- **Menus**: native menu bar with `muda` on macOS and Windows. On Linux,
  `muda` needs GTK, which we avoid, so Linux relies on the command palette.
- **Clipboard**: `arboard`. **File dialogs**: `rfd`.
- **Config**: one TOML file in the platform config directory (from `dirs`):
  `~/Library/Application Support/nimbus/config.toml` on macOS,
  `~/.config/nimbus/config.toml` on Linux, `%APPDATA%\nimbus\config.toml` on
  Windows. Font, size, tab width, theme, soft-wrap default, keybinding
  overrides. A missing file means defaults; a broken one is reported in the
  message line, never fatal.
- **Themes**: built-in dark and light themes as TOML compiled in with
  `include_str!` (`crates/app/themes/`); user themes use the same format in
  `themes/NAME.toml` in the config directory, selected by `theme = "NAME"`.
  A `[ui]` table of interface colours and a `[syntax]` table keyed by
  capture name, as `"#rrggbb"` or `"#rrggbbaa"`. A user theme starts from
  its `base` (dark unless given), so it lists only what it changes. Bad
  entries are warnings that keep the base colour. "Toggle Light/Dark
  Theme" switches the built-ins for the session.

## 5. Quality and performance enforcement

- **Lints**: a strict lint table in the root `Cargo.toml` (clippy pedantic,
  no `unwrap`/`expect`/`panic` outside tests, no stray prints, `missing_docs`,
  `unsafe_code` denied except where a module lifts it with a reason). CI runs
  clippy with `-D warnings` on all three platforms. Rules are in `CLAUDE.md`.
- **Startup**: `nimbus --smoke` exits after the first frame and prints
  `first-frame-ms`, timed from the top of `main`. Run with
  `RUST_LOG=nimbus=debug` to see per-phase times. Record it in each
  milestone doc's results.
- **Rendering**: `nimbus --screenshot FILE.bmp` saves the first frame read
  back from the GPU, so rendering can be checked without OS screenshot
  permission. `NIMBUS_SCALE` overrides the scale factor in debug builds.
- **Size**: the CI `release-size` job fails above 35 MB (MiB, checked on
  Linux, the largest build; 15 MB until M5). `cargo bloat`
  reviews at M5 (after grammars) and M8.
- **Runtime**: a debug overlay (`F12`, M8, with a log-only version from M2)
  shows frame time, keystroke-to-present latency, atlas occupancy, buffer
  backend and background task status.
- **Benchmarks**: `bench/gen.sh` (M2) generates 10 MB, 100 MB, 1 GB and
  4 GB files. `criterion` benches cover buffer operations and line layout.
- **Release profile**: `opt-level = 3`, `lto = "fat"`, `codegen-units = 1`,
  `panic = "abort"`, `strip = true`.

## 6. Testing

- `core` and `syntax` are pure and get unit tests plus property tests
  (`proptest`): random edit sequences on `RopeBuffer` and `MappedBuffer` must
  produce the same text as a `String` oracle.
- `render` layout code is tested headlessly: given a buffer, metrics and a
  view, assert the produced instances. The atlas packer has its own unit
  tests. GPU output gets one screenshot smoke test on macOS in M8.
- `app` gets integration tests that drive `Action`s against an `Editor`
  without a window.

## 7. Dependencies

Versions are the latest stable at planning time (2026-10-15). Each is added
in the milestone that first needs it, with `cargo add` on the crate that uses
it.

| Crate | Version | Role | Milestone |
|---|---|---|---|
| `winit` | 0.30 | window, input | M0 |
| `wgpu` | 30 | GPU | M0 |
| `pollster` | 1 | block on adapter and device setup | M0 |
| `log`, `env_logger` | 0.4, 0.11 | logging | M0 |
| `swash` | 0.2 | shaping, rasterization | M1 |
| `bytemuck` | 1 | instance structs to bytes | M1 |
| `ropey` | 1.6 | rope buffer | M2 |
| `unicode-segmentation`, `unicode-width` | 1.13, 0.2 | grapheme and width rules | M2 |
| `proptest` | 1 | property tests (dev) | M2 |
| `criterion` | 0.8 | benchmarks (dev) | M2 |
| `arboard` | 3 | clipboard | M3 |
| `regex` | 1 | search | M3 |
| `crossbeam-channel` | 0.5 | background to main thread | M3 |
| `serde`, `toml`, `dirs` | 1, 1, 7 | config | M4 |
| `muda` | 0.21 | native menus (macOS, Windows) | M4 |
| `rfd` | 0.17 | file dialogs | M4 |
| `ignore`, `nucleo` | 0.4, 0.5 | project walking, fuzzy matching | M4 |
| `tree-sitter` + grammar crates | 0.27 | syntax | M5 |
| `notify` | latest at M6 | file watching | M6 |
| `memmap2`, `memchr` | 0.9, 2 | mapped buffer, newline scanning | M7 |
| `fontdb` | 0.24 | system font discovery and fallback | M8 |
| `imara-diff` | 0.2 | line diff (Myers) over our own tokens | M9 |
| `ureq` | 3 | HTTPS for the update check; rustls with the system's certificate store | M15 |

## 8. Milestones

Each milestone ends with something you can run. Since most of the stack is
new to the owner, each milestone's commit or PR explains the concepts it
introduces and points at the code.

| # | Name | Status | You can… | Plan |
|---|---|---|---|---|
| 0 | Scaffold | **Done** | Run an empty window on all three OSes; CI builds all three | [m0](docs/milestones/m0-scaffold.md) |
| 1 | Text on screen | **Done** | See a paragraph in a monospace font at correct DPI; cold start under 100 ms | [m1](docs/milestones/m1-text-on-screen.md) |
| 2 | Editable | **Done** | Open, edit, scroll and save a file | [m2](docs/milestones/m2-editable.md) |
| 3 | Editing essentials | **Done** | Select, clipboard, undo, find and replace, soft wrap, auto-indent | [m3](docs/milestones/m3-editing-essentials.md) |
| 4 | Many files | **Done** | Tabs, fuzzy opener, command palette, config, menus | [m4](docs/milestones/m4-many-files.md) |
| 5 | Highlighting | **Done** | Syntax colours for nine languages, two themes | [m5](docs/milestones/m5-highlighting.md) |
| 6 | File tree | **Done** | Sidebar, file watching, reload on external change | [m6](docs/milestones/m6-file-tree.md) |
| 7 | Huge files | **Done** | Open, scroll, search, edit and save a 4 GB log | [m7](docs/milestones/m7-huge-files.md) |
| 8 | Polish | **Done** | Multi-cursor, brackets, status bar, overlay, fallback fonts, release builds | [m8](docs/milestones/m8-polish.md) |
| 9 | Compare and merge | **Done** | Compare files side by side; two-way and three-way merge; `--diff` and `--merge` for git | [m9](docs/milestones/m9-diff.md) |
| 10 | Find in files | **Done** | Search and replace across the project, results streaming in | [m10](docs/milestones/m10-find-in-files.md) |
| 11 | Everyday editing | **Done** | Toggle comments, line operations, expand selection, auto-pairs, per-language indentation | [m11](docs/milestones/m11-everyday-editing.md) |
| 12 | Workspace | **Next** | Reopen the last session, recent files, create and rename in the tree, follow a growing log | [m12](docs/milestones/m12-workspace.md) |
| 13 | Split views and folding | Planned | Two editor columns; fold regions at any file size | [m13](docs/milestones/m13-splits-and-folding.md) |
| 14 | Languages, second pass | In progress | Markdown inline and injected languages, symbols, structural selection, more grammars | [m14](docs/milestones/m14-languages.md) |
| 15 | Distribution | Done | Installers and packages for every platform (unsigned), an update check, signed checksums | [m15](docs/milestones/m15-distribution.md) |
| 16 | Rendering, second pass | Planned | Colour emoji, ligatures, zoom, whitespace and indent guides | [m16](docs/milestones/m16-rendering-second-pass.md) |
| 17 | Encodings and line endings | Planned | Open and save UTF-16 and Windows-1252; change encoding and line endings | [m17](docs/milestones/m17-encodings.md) |
| 18 | Compare, second pass | Planned | Ignore whitespace, compare folders, diff one-line files, compare with HEAD | [m18](docs/milestones/m18-compare-second-pass.md) |
| 19 | Workspace and search, second pass | Planned | Several roots, named workspaces, drag and drop, search history and multi-line patterns | [m19](docs/milestones/m19-workspace-and-search-second-pass.md) |
| 20 | Reading aids | Planned | Sticky scroll, minimap, a proportional UI font, Markdown preview | [m20](docs/milestones/m20-reading-aids.md) |
| 21 | Foundations and 1.0 | Planned | Ropey 2, current winit and wgpu, fuzzing, a crash record, version 1.0 | [m21](docs/milestones/m21-foundations-and-1-0.md) |

M10–M15 were drafted after M8 from the items earlier plans deferred;
`docs/milestones/backlog.md` maps each item to its plan. The owner
approved the six plans and their order on 2026-10-17 and answered their
open questions (§9, §10). M10 and M11 landed; M12 is Next, and M13–M15
follow in that order. M16–M21 were drafted on 2026-10-18 from what
M9–M15 deferred and approved the same day (§9, §10); they follow M15.

M1 and M2 carry the most risk: they are where wgpu and swash either work or
fight us. If M1's atlas takes much longer than planned, the fallback is to
swap in `glyphon` for the atlas and shader while keeping our own layout,
which is contained inside `render`.

## 9. Open questions

None open. The questions the drafts of M16–M21 raised were answered on
2026-10-18, when the owner approved the six plans and their order:

- Order: M16 (rendering) to M21 (1.0) as drafted; encodings first was
  declined.
- M16: the zoom is remembered in `state.toml` across sessions unless
  `remember_zoom = false`.
- M17: the Western encodings only (UTF-8, UTF-16, Windows-1252,
  ISO-8859-1); East Asian tables behind a cargo feature that is off.
- M18: "Compare with HEAD" is in, running git as a subprocess; the git
  non-goal in §2 carries that one exception.
- M19: workspace files live in the config directory, and also as
  `.nimbus/workspace.toml` inside a root on "Save Workspace Here".
- M20: the minimap is in, windowed to the lines around the view, off by
  default.
- M21: screen-reader support (AccessKit) waits until after 1.0; a panic
  writes a one-line `crash.log` with the version and message and no
  user text.

The questions the drafts of M10–M15 raised were answered on 2026-10-17,
when the owner approved those six plans and their order:

- Order: M10 (find in files), then M11 (everyday editing), then M12 to
  M15 as drafted. M10 is Next.
- M10: the search panel is a second mode of the sidebar, beside Files;
  results cap at 10,000 matches; undoing a replace across files keeps up
  to 64 MB of old contents.
- M11: auto-pairs on by default for code, off for Markdown and plain
  text; line operations refuse selections over 64 MB; the chord timeout
  is a fixed two seconds.
- M12: `nimbus` with no arguments reopens the last session, and
  `--no-session` skips it; deleting from the tree goes to the system
  trash through the `trash` crate, measured for binary size; autosave of
  unsaved work for crash recovery is not planned (it would write the
  user's text somewhere they did not choose) and stays unscheduled in
  `backlog.md`.
- M13: up to three editor columns, no nesting or stacking; the same file
  may be open in two columns, so the document-and-pane refactor is in
  scope.
- M14: add C, Go, HTML, CSS and TSX, each measured against the 35 MB
  budget before it stays; Go to Symbol is `Primary+Shift+R`.
- M15: no Apple Developer account and no Windows certificate, so builds
  ship unsigned and the README keeps its `xattr` step; the startup
  update check is off unless `check_updates = true`; `SHA256SUMS` is
  signed with `minisign`.

Resolved before M8: Dependabot updates GitHub Actions weekly
(`.github/dependabot.yml`); cargo crates are updated by hand at each
milestone, since wgpu, winit and swash change their APIs between versions.
Resolved at approval: the two-backend buffer (§4.1), bidi out of scope (§2),
the nine starter languages (§4.2), binary name `nimbus`. Resolved at the start
of M1: the bundled font is Hack Nerd Font Mono Regular (§4.3). Resolved after
M1: the startup budget is under 500 ms total (§1). Resolved after M2: the
memory budget is per file, at most 1.5× file size over an empty window (§1). Resolved in M7: the rope-versus-mapped threshold stays at 256 MB,
measured (M7 doc, Results). Resolved after M5: the
binary budget is under 35 MB (§1); highlighting memory is not counted in
the per-file budget, and files over 16 MB get simple line-by-line
highlighting instead of a syntax tree (§4.2).

## 10. Decision log

| Date | Decision | Where |
|---|---|---|
| 2026-10-15 | Two buffer backends behind one trait | §4.1 |
| 2026-10-15 | Bidi, multiple windows and Linux menus are non-goals | §2, §4.4 |
| 2026-10-15 | wgpu 30 and winit 0.30; 0.31 is beta | §4.3, §4.4 |
| 2026-10-15 | Strict lints enforced in CI via workspace lint table | §5 |
| 2026-10-15 | Bundled font, so first frame skips the system font scan | §4.3 |
| 2026-10-15 | GPU setup moves off the main thread in M1 to meet startup budget | §4.3 |
| 2026-10-15 | Own highlight query loop instead of tree-sitter-highlight | §4.2 |
| 2026-10-15 | `Buffer::snapshot` added for worker threads | §4.1 |
| 2026-10-15 | Invalid UTF-8 files go to `MappedBuffer` | §4.1 |
| 2026-10-15 | `notify` file watching added to M6 | §4.4 |
| 2026-10-15 | Long lines fetched by visible column range (M7), both backends | §4.1 |
| 2026-10-15 | Bundled font is Hack Nerd Font Mono Regular, about 2.7 MB of the binary | §4.3 |
| 2026-10-15 | M0 startup numbers corrected; real floor is winit and AppKit, budget reopened | §1, §9 |
| 2026-10-15 | `--screenshot` frame capture added in M1, ahead of M8 | §5 |
| 2026-10-15 | `Gpu::render` reports `FrameStatus`; GPU validation errors are logged, never panic | §4.3 |
| 2026-10-15 | Startup budget changed from 100 ms to under 500 ms total | §1 |
| 2026-10-16 | Buffer interface split into `Text`/`Buffer`/`Snapshot`; `replace` returns `Result` | §4.1 |
| 2026-10-16 | Saving writes bytes verbatim; detected line ending only governs Enter | §4.1 |
| 2026-10-16 | Shared column math in `nimbus_core::column`; renderer snaps graphemes to columns | §4.3 |
| 2026-10-16 | Only the visible column span of each line is shaped; lines over 1 MB refused until M7 | §4.1, §4.3 |
| 2026-10-16 | winit's default macOS menu disabled so Cmd+Q reaches the unsaved prompt | §4.4 |
| 2026-10-16 | Byte order mark kept out of the text, written back on save | §4.1 |
| 2026-10-16 | Memory budget: a file adds at most 1.5× its size; totals reported, not budgeted | §1 |
| 2026-10-16 | Soft wrap lives in `nimbus_core::wrap`; the view works in visual rows in both modes | §4.3 |
| 2026-10-16 | Renderer draws in clipped layers; overlays cover the text area | §4.3 |
| 2026-10-16 | Find fields are `Document`s; re-search after edits waits for a 150 ms pause | §4.4 |
| 2026-10-16 | `--headless` off-screen rendering for checks without a visible window | §5 |
| 2026-10-16 | Word movement uses code-style classes; wrap uses Unicode word bounds | §4.1 |
| 2026-10-16 | Actions have a `When` context; inapplicable chords type their text | §4.4 |
| 2026-10-16 | Physical-key fallback only for Alt/Option or non-ASCII layouts | §4.4 |
| 2026-10-16 | Config settings parsed one by one; overrides beat defaults | §4.4 |
| 2026-10-16 | Shortcut labels in words on macOS until font fallback (M8) | §4.3 |
| 2026-10-16 | Each grammar a cargo feature; TSX left out for binary size | §4.2 |
| 2026-10-16 | Rust highlights query vendored to fix its constant pattern | §4.2 |
| 2026-10-16 | `Text::chunk_at` added so tree-sitter reads rope chunks directly | §4.1, §4.2 |
| 2026-10-16 | `History::undo`/`redo` report each edit through a callback | §4.1, §4.2 |
| 2026-10-16 | Main-thread reparse budget 2 ms, then a worker; syntax crate starts no threads | §4.2 |
| 2026-10-16 | Themes are TOML, built in or in the config directory, hex colours | §4.4 |
| 2026-10-16 | Binary budget raised from 15 MB to 35 MB: grammars put Linux at 19 MB | §1, §5 |
| 2026-10-16 | Highlighting memory not counted in the per-file budget | §1, §4.2 |
| 2026-10-16 | Files over 16 MB get simple line-by-line highlighting, any backend | §4.2 |
| 2026-10-16 | Users add languages as simple rules, map patterns, and override queries; no runtime grammars | §4.2 |
| 2026-10-16 | Watch sidebar folders and open files' folders, non-recursively | §4.4 |
| 2026-10-16 | Window state in `state.toml`; the config file is never written | §4.4 |
| 2026-10-16 | Reloading a file changed on disk clears its undo history | §4.4 |
| 2026-10-16 | The file watcher runs on its own thread; the main thread never calls `notify` | §4.4 |
| 2026-10-16 | M9 diffs large files in layers: trim common ends, full diff, anchored block diff; long lines split | §8 (m9) |
| 2026-10-16 | M7 design approved: SUB for invalid bytes, walking by byte, `Content` undo, `unsafe` mapping in app, pread for bulk reads | §4.1, m7 |
| 2026-10-16 | Rope-versus-mapped threshold kept at 256 MB after measuring 64–512 MB | §4.1, §9 |
| 2026-10-16 | Compare and merge (two- and three-way) added as milestone 9, after Polish, held to the editor's budgets | §4.4, §8 |
| 2026-10-16 | Replace All and its undo and redo apply in one pass (`replace_sorted`, `put_sorted`) | §4.1 |
| 2026-10-16 | Files that are not UTF-8 and under the threshold are held in memory, not mapped | §4.1 |
| 2026-10-16 | Truncation of a dirty mapped file (SIGBUS on macOS and Linux; blocked writers on Windows) accepted as a documented limitation | §4.1, m7 |
| 2026-10-16 | M8 plan approved: sorted multi-cursor selections applied in one pass; bundle Hack Bold, slant for italic; Linux QA by CI only | m8 |
| 2026-10-16 | Font fallback copies the smallest suitable system font into memory (no `unsafe`); colour-only emoji fonts skipped | §4.3, m8 |
| 2026-10-16 | Bundled bold font subset to its text characters (no icons): 2.8 MB to 185 KB | §1, m8 |
| 2026-10-16 | Release builds: draft GitHub release on a `v*` tag; unsigned; macOS universal binary | §8, m8 |
| 2026-10-16 | App icon redrawn: white cloud and dark I-beam on an amber tile, on Apple's icon grid; solid shapes so it reads at 16 px | m8 |
| 2026-10-16 | Built-in themes take the icon's colours: amber is the one accent, selection is a shade of the ink; light theme on warm paper with a burnt-amber cursor | §4.4, m8 |
| 2026-10-17 | M10–M15 drafted from the items M1–M9 deferred; order and `Proposed` items await the owner (`backlog.md`) | §8, §9 |
| 2026-10-17 | M9 diff: Myers over histogram after measurement; full diff up to 256 MB per side, windows past that; lines interned by streaming hash, verified by bytes | §4.4, m9 |
| 2026-10-17 | M9 merge: touching hunks conflict (git's rule); the result is tracked by regions shifted per edit, not re-diffed; conflict markers read back by splitting into base, ours, theirs; a two-way merge is the three-way machinery with the left text as base | §4.4, m9 |
| 2026-10-17 | M9: merges and compares are built on workers; the tab shows its texts unaligned until the answer arrives, and edits wait on a pending merge | §4.4, m9 |
| 2026-10-17 | M9 long-line splitting (layer 4) deferred to the backlog: a view question (how a split line's pieces show) before a diff one | m9, backlog |
| 2026-10-17 | M10–M15 approved in that order; M10 (find in files) is Next | §8, §9, backlog |
| 2026-10-17 | Find in files lives in the sidebar as a Search mode; 10,000-match cap; 64 MB undo budget for replace across files | §9, m10 |
| 2026-10-17 | M10 landed: the walk and the search share `ignore`'s parallel walker threads; files are read in 4 MB blocks with a line-tail carry; hits are counted against the cap as found | m10 |
| 2026-10-17 | Auto-pairs on for code, off for Markdown and plain text; line operations refuse selections over 64 MB; chord timeout fixed at two seconds | §9, m11 |
| 2026-10-17 | `nimbus` alone reopens the last session (`--no-session` skips); tree delete uses the `trash` crate; no autosave for crash recovery | §9, m12 |
| 2026-10-17 | Up to three editor columns, no nesting; one file may be open in two columns, so documents and panes are split | §9, m13 |
| 2026-10-17 | Grammars to add: C, Go, HTML, CSS, TSX, each measured against the 35 MB budget; Go to Symbol is `Primary+Shift+R` | §9, m14 |
| 2026-10-17 | No signing accounts: builds stay unsigned and the README keeps its `xattr` step; update check off unless `check_updates = true`; checksums signed with `minisign` | §9, m15 |
| 2026-10-17 | Update check over `ureq` with rustls and the platform verifier (+1.7 MB), the one JSON field scanned by hand; packages built by shell scripts and Inno Setup in the workflow, manifests generated from the tag | §7, m15 |
| 2026-10-18 | M16–M21 drafted from what M9–M15 deferred and approved in order: rendering, encodings, compare, workspace and search, reading aids, foundations and 1.0 | §8, §9, backlog |
| 2026-10-18 | Zoom remembered across sessions unless `remember_zoom = false`; Western encodings only, East Asian behind an off feature | §9, m16, m17 |
| 2026-10-18 | "Compare with HEAD" through a git subprocess is the one exception to the git non-goal; workspace files may also live in a root as `.nimbus/workspace.toml` | §2, §9, m18, m19 |
| 2026-10-18 | Minimap in, windowed and off by default; AccessKit after 1.0; a panic writes a one-line `crash.log` with no user text | §9, m20, m21 |
| 2026-10-17 | M14 grammars all kept: C, Go, HTML, CSS and TSX add 2.3 MB on Linux (21.4 MB of 35); drop order is now TSX, Bash, TypeScript | §4.2, m14 |
| 2026-10-17 | Injections: child trees keyed by host node id, kept one screen either side of the view, parsed within the frame's 2 ms budget at one child per frame at least; Rust's injection query not used | §4.2, m14 |
| 2026-10-17 | Spans carry a capture table per grammar so the inner language's colours win inside injections; wide nodes are queried child by child | §4.2, m14 |
| 2026-10-18 | Per-language soft wrap is `[editor.NAME] wrap`, following M11's `[editor.NAME]` and `[indent.NAME]` tables rather than the planned `[languages.NAME]` (that table maps file patterns); Markdown wraps by default | §4.4, m14 |
